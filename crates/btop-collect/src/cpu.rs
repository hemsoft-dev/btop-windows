//! Live Windows 11 x64 CPU telemetry collector backed by `NtQuerySystemInformation`,
//! Registry `ProcessorNameString`, and `GetTickCount64`.

use std::time::{SystemTime, UNIX_EPOCH};

use btop_model::{CoreKind, CoreSample, CpuSnapshot, DataState, LogicalCpuId, Snapshot};
use btop_sys::{
    SystemProcessorPerformanceInformation, query_processor_performance, read_cpu_model_name,
    system_uptime_secs,
};

use crate::math::cpu_busy_percent;
use crate::{CollectError, SnapshotSource};

/// Computes per-core and aggregate CPU percentages from two consecutive
/// `SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION` samples.
pub fn compute_cpu_deltas(
    prev: &[SystemProcessorPerformanceInformation],
    curr: &[SystemProcessorPerformanceInformation],
) -> (f32, Vec<CoreSample>) {
    let count = prev.len().min(curr.len());
    let mut cores = Vec::with_capacity(count);
    let mut sum_kernel = 0u64;
    let mut sum_user = 0u64;
    let mut sum_idle = 0u64;

    for idx in 0..count {
        let kernel = non_negative_delta(prev[idx].kernel_time, curr[idx].kernel_time);
        let user = non_negative_delta(prev[idx].user_time, curr[idx].user_time);
        let idle = non_negative_delta(prev[idx].idle_time, curr[idx].idle_time);
        sum_kernel = sum_kernel.saturating_add(kernel);
        sum_user = sum_user.saturating_add(user);
        sum_idle = sum_idle.saturating_add(idle);

        let busy_pct = cpu_busy_percent(kernel, user, idle);
        cores.push(CoreSample {
            id: LogicalCpuId {
                group: 0,
                number: u8::try_from(idx).unwrap_or(u8::MAX),
            },
            busy_pct,
            kind: CoreKind::Uniform,
            temp_celsius: None,
        });
    }

    let total_busy = cpu_busy_percent(sum_kernel, sum_user, sum_idle);
    (total_busy, cores)
}

fn non_negative_delta(prev: i64, curr: i64) -> u64 {
    if curr <= prev {
        0
    } else {
        u64::try_from(curr.saturating_sub(prev)).unwrap_or(0)
    }
}

/// Stateful Windows 11 x64 CPU collector holding the previous processor-time baseline.
#[derive(Debug)]
pub struct CpuCollector {
    model_name: String,
    prev: Vec<SystemProcessorPerformanceInformation>,
}

impl CpuCollector {
    /// Captures the initial baseline sample and CPU brand string from the Registry.
    pub fn new() -> Result<Self, CollectError> {
        let prev = query_processor_performance().map_err(CollectError::Platform)?;
        let model_name = read_cpu_model_name().unwrap_or_else(|| "Windows x64 CPU".to_owned());
        Ok(Self { model_name, prev })
    }

    /// Queries the next CPU delta snapshot.
    pub fn sample(&mut self) -> Result<CpuSnapshot, CollectError> {
        let curr = query_processor_performance().map_err(CollectError::Platform)?;
        let (total_busy_pct, cores) = if self.prev.len() == curr.len() {
            compute_cpu_deltas(&self.prev, &curr)
        } else {
            compute_cpu_deltas(&curr, &curr)
        };
        self.prev = curr;

        Ok(CpuSnapshot {
            model_name: self.model_name.clone(),
            total_busy_pct,
            utility_pct: None,
            freq_mhz: None,
            package_temp_celsius: None,
            package_watts: None,
            run_queue_ewma: [0.0; 3],
            uptime_secs: system_uptime_secs(),
            cores,
        })
    }
}

/// Live Windows 11 snapshot source producing CPU snapshots on each tick.
#[derive(Debug)]
pub struct LiveCollector {
    hostname: String,
    username: String,
    cpu: CpuCollector,
}

impl LiveCollector {
    pub fn new() -> Result<Self, CollectError> {
        let hostname = std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "WINDOWS-HOST".to_owned());
        let username = std::env::var("USERNAME")
            .or_else(|_| std::env::var("USER"))
            .unwrap_or_else(|_| "user".to_owned());
        let cpu = CpuCollector::new()?;
        Ok(Self {
            hostname,
            username,
            cpu,
        })
    }
}

impl SnapshotSource for LiveCollector {
    fn next_snapshot(&mut self) -> Result<Snapshot, CollectError> {
        let timestamp_ms = u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis()),
        )
        .unwrap_or(u64::MAX);
        let cpu_snapshot = self.cpu.sample()?;

        Ok(Snapshot {
            timestamp_ms,
            hostname: self.hostname.clone(),
            username: self.username.clone(),
            cpu: DataState::Available(cpu_snapshot),
            ..Snapshot::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_cpu_deltas_aggregates_cores_accurately() {
        let prev = [
            SystemProcessorPerformanceInformation {
                idle_time: 1000,
                kernel_time: 1200,
                user_time: 300,
                ..Default::default()
            },
            SystemProcessorPerformanceInformation {
                idle_time: 1000,
                kernel_time: 1500,
                user_time: 500,
                ..Default::default()
            },
        ];
        let curr = [
            SystemProcessorPerformanceInformation {
                idle_time: 1500,   // di = 500
                kernel_time: 1900, // dk = 700
                user_time: 600,    // du = 300 -> total 1000, busy 500 -> 50%
                ..Default::default()
            },
            SystemProcessorPerformanceInformation {
                idle_time: 1250,   // di = 250
                kernel_time: 2000, // dk = 500
                user_time: 1000,   // du = 500 -> total 1000, busy 750 -> 75%
                ..Default::default()
            },
        ];

        let (total, cores) = compute_cpu_deltas(&prev, &curr);
        assert_eq!(cores.len(), 2);
        assert!((cores[0].busy_pct - 50.0).abs() < 0.01);
        assert!((cores[1].busy_pct - 75.0).abs() < 0.01);
        assert!((total - 62.5).abs() < 0.01);
    }

    #[cfg(windows)]
    #[test]
    fn live_windows_cpu_collector_returns_valid_telemetry() {
        let mut collector = CpuCollector::new().expect("initialize Windows CPU collector");
        std::thread::sleep(std::time::Duration::from_millis(60));
        let snap = collector.sample().expect("sample live Windows CPU");
        assert!(
            (0.0..=100.0).contains(&snap.total_busy_pct),
            "total_busy_pct out of range: {}",
            snap.total_busy_pct
        );
        assert!(!snap.model_name.is_empty(), "model_name must not be empty");
        assert!(snap.uptime_secs > 0, "uptime_secs must be positive");
        assert!(!snap.cores.is_empty(), "cores must not be empty");
    }
}
