//! Live Windows 11 x64 CPU telemetry collector backed by `NtQuerySystemInformation`,
//! `GetLogicalProcessorInformationEx`, Registry `ProcessorNameString`/`~MHz`,
//! shared English PDH (`% Processor Performance` + `% Processor Utility`), `GetTickCount64`,
//! and 1/5/15-minute `RunQ` EWMA.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use btop_model::{CoreKind, CoreSample, CpuSnapshot, DataState, LogicalCpuId, Snapshot};
use btop_sys::{
    CpuPdhQuery, SystemProcessorPerformanceInformation, query_core_efficiency_classes,
    query_processor_performance, read_cpu_model_name, read_cpu_nominal_mhz, system_uptime_secs,
    total_physical_memory_bytes,
};

use crate::disk::DiskCollector;
use crate::math::cpu_busy_percent;
use crate::mem::MemoryCollector;
use crate::net::NetCollector;
use crate::proc::ProcessCollector;
use crate::{CollectError, SnapshotSource};

/// Aggregation mode for live CPU frequency derived from PDH `% Processor Performance`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FreqMode {
    First,
    Range,
    Lowest,
    Highest,
    #[default]
    Average,
}

impl FreqMode {
    pub fn from_config_str(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "first" => Self::First,
            "range" => Self::Range,
            "lowest" => Self::Lowest,
            "highest" => Self::Highest,
            _ => Self::Average,
        }
    }

    pub const fn to_u8(self) -> u8 {
        match self {
            Self::First => 0,
            Self::Range => 1,
            Self::Lowest => 2,
            Self::Highest => 3,
            Self::Average => 4,
        }
    }

    pub const fn from_u8(raw: u8) -> Self {
        match raw {
            0 => Self::First,
            1 => Self::Range,
            2 => Self::Lowest,
            3 => Self::Highest,
            _ => Self::Average,
        }
    }
}

/// Maps Windows `EfficiencyClass` bytes to [`CoreKind`].
///
/// On hybrid CPUs with multiple distinct efficiency classes, higher values map to
/// [`CoreKind::Performance`] and lower values to [`CoreKind::Efficiency`].
/// On homogeneous CPUs, all logical processors map to [`CoreKind::Uniform`].
pub fn classify_core_kinds(efficiency_classes: &[u8]) -> Vec<CoreKind> {
    if efficiency_classes.is_empty() {
        return Vec::new();
    }
    let min = efficiency_classes.iter().copied().min().unwrap_or(0);
    let max = efficiency_classes.iter().copied().max().unwrap_or(0);
    if min == max {
        return vec![CoreKind::Uniform; efficiency_classes.len()];
    }
    efficiency_classes
        .iter()
        .map(|&cls| {
            if cls == max {
                CoreKind::Performance
            } else {
                CoreKind::Efficiency
            }
        })
        .collect()
}

/// Computes live frequency in MHz from nominal base MHz and per-core PDH
/// `% Processor Performance` readings (`nominal_mhz * perf_pct / 100.0`).
///
/// Returns `(primary_mhz, optional_range_mhz)` where `optional_range_mhz` is populated
/// as `Some((min, max))` when `mode == FreqMode::Range`.
pub fn compute_frequency_mhz(
    nominal_mhz: u32,
    perf_pcts: &[f64],
    mode: FreqMode,
) -> (Option<u32>, Option<(u32, u32)>) {
    if nominal_mhz == 0 {
        return (None, None);
    }
    let freqs: Vec<u32> = perf_pcts
        .iter()
        .copied()
        .filter(|v| v.is_finite() && *v > 0.0)
        .map(|pct| {
            let scaled = (f64::from(nominal_mhz) * (pct / 100.0)).round();
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let mhz = scaled.clamp(1.0, f64::from(u32::MAX)) as u32;
            mhz
        })
        .collect();

    if freqs.is_empty() {
        return (None, None);
    }

    let min_mhz = freqs.iter().copied().min().unwrap_or(freqs[0]);
    let max_mhz = freqs.iter().copied().max().unwrap_or(freqs[0]);
    let sum: u64 = freqs.iter().map(|&v| u64::from(v)).sum();
    let avg_mhz = u32::try_from(sum / (freqs.len() as u64)).unwrap_or(freqs[0]);

    match mode {
        FreqMode::First => (Some(freqs[0]), None),
        FreqMode::Lowest => (Some(min_mhz), None),
        FreqMode::Highest => (Some(max_mhz), None),
        FreqMode::Average => (Some(avg_mhz), None),
        FreqMode::Range => (Some(avg_mhz), Some((min_mhz, max_mhz))),
    }
}

/// Exponentially weighted moving average (1, 5, and 15 minute windows) of Ready/Running threads.
#[derive(Debug, Clone, Default)]
pub struct RunQueueEwma {
    initialized: bool,
    values: [f64; 3],
}

impl RunQueueEwma {
    const WINDOWS_SECS: [f64; 3] = [60.0, 300.0, 900.0];

    /// Updates the 1/5/15m EWMA buckets with `runnable_threads` observed after `dt_secs`.
    #[allow(clippy::cast_possible_truncation)]
    pub fn update(&mut self, runnable_threads: u32, dt_secs: f64) -> [f32; 3] {
        let sample = f64::from(runnable_threads);
        if !self.initialized {
            self.values = [sample; 3];
            self.initialized = true;
        } else if dt_secs.is_finite() && dt_secs > 0.0 {
            for (val, &window) in self.values.iter_mut().zip(&Self::WINDOWS_SECS) {
                let alpha = 1.0 - (-dt_secs / window).exp();
                *val += alpha * (sample - *val);
            }
        }
        [
            self.values[0] as f32,
            self.values[1] as f32,
            self.values[2] as f32,
        ]
    }

    /// Returns the last computed EWMA values, if at least one sample has succeeded.
    #[allow(clippy::cast_possible_truncation)]
    pub fn current(&self) -> Option<[f32; 3]> {
        self.initialized.then_some([
            self.values[0] as f32,
            self.values[1] as f32,
            self.values[2] as f32,
        ])
    }
}

/// Computes per-core and aggregate CPU percentages from two consecutive
/// `SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION` samples, applying hybrid P/E core kinds.
pub fn compute_cpu_deltas_with_kinds(
    prev: &[SystemProcessorPerformanceInformation],
    curr: &[SystemProcessorPerformanceInformation],
    core_kinds: &[CoreKind],
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
            kind: core_kinds.get(idx).copied().unwrap_or(CoreKind::Uniform),
            temp_celsius: None,
        });
    }

    let total_busy = cpu_busy_percent(sum_kernel, sum_user, sum_idle);
    (total_busy, cores)
}

/// Computes per-core and aggregate CPU percentages with default [`CoreKind::Uniform`] kinds.
pub fn compute_cpu_deltas(
    prev: &[SystemProcessorPerformanceInformation],
    curr: &[SystemProcessorPerformanceInformation],
) -> (f32, Vec<CoreSample>) {
    compute_cpu_deltas_with_kinds(prev, curr, &[])
}

fn non_negative_delta(prev: i64, curr: i64) -> u64 {
    if curr <= prev {
        0
    } else {
        u64::try_from(curr.saturating_sub(prev)).unwrap_or(0)
    }
}

/// Stateful Windows 11 x64 CPU collector holding the previous processor-time baseline,
/// hybrid P/E-core topology, nominal MHz, and optional shared English PDH query.
#[derive(Debug)]
pub struct CpuCollector {
    model_name: String,
    nominal_mhz: Option<u32>,
    freq_mode: FreqMode,
    core_kinds: Vec<CoreKind>,
    pdh: Option<CpuPdhQuery>,
    prev: Vec<SystemProcessorPerformanceInformation>,
}

impl CpuCollector {
    /// Captures the initial baseline sample, hybrid P/E topology, nominal MHz, and shared PDH query.
    pub fn new() -> Result<Self, CollectError> {
        Self::with_freq_mode(FreqMode::Average)
    }

    /// Captures the initial baseline sample using the specified [`FreqMode`].
    pub fn with_freq_mode(freq_mode: FreqMode) -> Result<Self, CollectError> {
        let prev = query_processor_performance().map_err(CollectError::Platform)?;
        let model_name = read_cpu_model_name().unwrap_or_else(|| "Windows x64 CPU".to_owned());
        let nominal_mhz = read_cpu_nominal_mhz();
        let core_kinds = classify_core_kinds(&query_core_efficiency_classes());
        let pdh = CpuPdhQuery::open();
        Ok(Self {
            model_name,
            nominal_mhz,
            freq_mode,
            core_kinds,
            pdh,
            prev,
        })
    }

    /// Updates the frequency aggregation mode used on subsequent samples.
    pub fn set_freq_mode(&mut self, mode: FreqMode) {
        self.freq_mode = mode;
    }

    /// Queries the next CPU delta snapshot (falling back cleanly to `None` if PDH is unavailable).
    pub fn sample(&mut self) -> Result<CpuSnapshot, CollectError> {
        let curr = query_processor_performance().map_err(CollectError::Platform)?;
        let (total_busy_pct, cores) = if self.prev.len() == curr.len() {
            compute_cpu_deltas_with_kinds(&self.prev, &curr, &self.core_kinds)
        } else {
            compute_cpu_deltas_with_kinds(&curr, &curr, &self.core_kinds)
        };
        self.prev = curr;

        let pdh_sample = self.pdh.as_mut().and_then(CpuPdhQuery::sample);
        let (freq_mhz, freq_range_mhz) = match (self.nominal_mhz, pdh_sample.as_ref()) {
            (Some(nom), Some(sample)) => {
                compute_frequency_mhz(nom, &sample.per_core_perf_pct, self.freq_mode)
            }
            _ => (None, None),
        };
        #[allow(clippy::cast_possible_truncation)]
        let utility_pct = pdh_sample
            .as_ref()
            .and_then(|s| s.total_utility_pct)
            .map(|v| v.clamp(0.0, 100.0) as f32);

        Ok(CpuSnapshot {
            model_name: self.model_name.clone(),
            total_busy_pct,
            utility_pct,
            freq_mhz,
            freq_range_mhz,
            package_temp_celsius: None,
            package_watts: None,
            run_queue_ewma: [0.0; 3],
            uptime_secs: system_uptime_secs(),
            cores,
        })
    }
}

/// Live Windows 11 snapshot source producing CPU (with P/E topology, PDH frequency/utility,
/// and `RunQ` EWMA), memory/commit/pagefile/cache, local fixed disks/I/O, and process snapshots.
#[derive(Debug)]
pub struct LiveCollector {
    hostname: String,
    username: String,
    total_phys_mem: Option<u64>,
    cpu: CpuCollector,
    mem: MemoryCollector,
    disks: DiskCollector,
    net: NetCollector,
    procs: ProcessCollector,
    runq: RunQueueEwma,
    last_runq_instant: Option<Instant>,
}

impl LiveCollector {
    pub fn new() -> Result<Self, CollectError> {
        Self::with_freq_mode(FreqMode::Average)
    }

    pub fn with_freq_mode(freq_mode: FreqMode) -> Result<Self, CollectError> {
        let hostname = std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "WINDOWS-HOST".to_owned());
        let username = std::env::var("USERNAME")
            .or_else(|_| std::env::var("USER"))
            .unwrap_or_else(|_| "user".to_owned());
        let cpu = CpuCollector::with_freq_mode(freq_mode)?;
        let mem = MemoryCollector::new();
        let mut disks = DiskCollector::new();
        // Prime physical disk I/O baseline so the first displayed tick has live rates.
        let _ = disks.sample();
        let mut net = NetCollector::new();
        // Prime 64-bit network counters so the first displayed tick has live deltas.
        let _ = net.sample();
        let mut procs = ProcessCollector::new();
        // Prime baseline process CPU timestamps so the first displayed tick has real deltas.
        let _ = procs.sample_with_runnable(cpu.prev.len().max(1));
        Ok(Self {
            hostname,
            username,
            total_phys_mem: total_physical_memory_bytes(),
            cpu,
            mem,
            disks,
            net,
            procs,
            runq: RunQueueEwma::default(),
            last_runq_instant: None,
        })
    }
}

impl SnapshotSource for LiveCollector {
    fn next_snapshot(&mut self) -> Result<Snapshot, CollectError> {
        let now = Instant::now();
        let timestamp_ms = u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis()),
        )
        .unwrap_or(u64::MAX);
        let mut cpu_snapshot = self.cpu.sample()?;
        let logical_cpus = cpu_snapshot.cores.len().max(1);

        let mem = match self.mem.sample() {
            Ok(mem_snap) => {
                self.total_phys_mem = Some(mem_snap.total_bytes);
                DataState::Available(mem_snap)
            }
            Err(err) => DataState::Unavailable(err.to_string()),
        };

        let disks = match self.disks.sample() {
            Ok(list) => DataState::Available(list),
            Err(err) => DataState::Unavailable(err.to_string()),
        };

        let net = match self.net.sample() {
            Ok(list) => DataState::Available(list),
            Err(err) => DataState::Unavailable(err.to_string()),
        };

        let processes = match self.procs.sample_with_runnable(logical_cpus) {
            Ok((list, runnable)) => {
                let dt_secs = self.last_runq_instant.map_or(0.0, |prev| {
                    now.saturating_duration_since(prev).as_secs_f64()
                });
                self.last_runq_instant = Some(now);
                cpu_snapshot.run_queue_ewma = self.runq.update(runnable, dt_secs);
                DataState::Available(list)
            }
            Err(err) => {
                // Preserve retained RunQ EWMA across transient process-query failures and do not
                // advance `last_runq_instant`, so recovery measures true elapsed time since last update.
                if let Some(retained) = self.runq.current() {
                    cpu_snapshot.run_queue_ewma = retained;
                }
                DataState::Unavailable(err.to_string())
            }
        };

        Ok(Snapshot {
            timestamp_ms,
            hostname: self.hostname.clone(),
            username: self.username.clone(),
            total_phys_mem_bytes: self.total_phys_mem,
            cpu: DataState::Available(cpu_snapshot),
            mem,
            disks,
            net,
            processes,
            ..Snapshot::default()
        })
    }

    fn set_freq_mode(&mut self, mode: FreqMode) {
        self.cpu.set_freq_mode(mode);
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
                idle_time: 1500,
                kernel_time: 1900,
                user_time: 600,
                ..Default::default()
            },
            SystemProcessorPerformanceInformation {
                idle_time: 1250,
                kernel_time: 2000,
                user_time: 1000,
                ..Default::default()
            },
        ];

        let (total, cores) = compute_cpu_deltas(&prev, &curr);
        assert_eq!(cores.len(), 2);
        assert!((cores[0].busy_pct - 50.0).abs() < 0.01);
        assert!((cores[1].busy_pct - 75.0).abs() < 0.01);
        assert!((total - 62.5).abs() < 0.01);
    }

    #[test]
    fn classify_core_kinds_handles_uniform_and_hybrid_cpus() {
        assert_eq!(
            classify_core_kinds(&[0, 0, 0, 0]),
            vec![CoreKind::Uniform; 4]
        );
        assert_eq!(
            classify_core_kinds(&[1, 1, 0, 0]),
            vec![
                CoreKind::Performance,
                CoreKind::Performance,
                CoreKind::Efficiency,
                CoreKind::Efficiency,
            ]
        );
        assert_eq!(classify_core_kinds(&[]), Vec::<CoreKind>::new());
    }

    #[test]
    fn compute_frequency_mhz_supports_all_modes_and_graceful_fallback() {
        let perf = [120.0, 100.0, 80.0, f64::NAN, -5.0];
        assert_eq!(
            compute_frequency_mhz(4000, &perf, FreqMode::First),
            (Some(4800), None)
        );
        assert_eq!(
            compute_frequency_mhz(4000, &perf, FreqMode::Lowest),
            (Some(3200), None)
        );
        assert_eq!(
            compute_frequency_mhz(4000, &perf, FreqMode::Highest),
            (Some(4800), None)
        );
        assert_eq!(
            compute_frequency_mhz(4000, &perf, FreqMode::Average),
            (Some(4000), None)
        );
        assert_eq!(
            compute_frequency_mhz(4000, &perf, FreqMode::Range),
            (Some(4000), Some((3200, 4800)))
        );
        assert_eq!(
            compute_frequency_mhz(0, &perf, FreqMode::Average),
            (None, None)
        );
        assert_eq!(
            compute_frequency_mhz(4000, &[], FreqMode::Average),
            (None, None)
        );
    }

    #[test]
    fn run_queue_ewma_initializes_and_decays_exponentially() {
        let mut ewma = RunQueueEwma::default();
        let first = ewma.update(4, 0.0);
        assert!((first[0] - 4.0).abs() < 1e-4);
        assert!((first[1] - 4.0).abs() < 1e-4);
        assert!((first[2] - 4.0).abs() < 1e-4);

        // After 60 seconds at 10 runnable threads:
        // 1m window moves by 1 - e^-1 (~63.2%) => 4 + 6 * 0.6321 = ~7.79
        // 5m moves slower (~18.1%) => ~5.09; 15m moves slowest (~6.4%) => ~4.39
        let next = ewma.update(10, 60.0);
        assert!((next[0] - 7.7927).abs() < 0.02, "1m ewma: {}", next[0]);
        assert!(next[0] > next[1] && next[1] > next[2] && next[2] > 4.0);
    }

    #[cfg(windows)]
    #[test]
    fn live_windows_cpu_collector_returns_valid_telemetry() {
        let mut collector = CpuCollector::new().expect("initialize Windows CPU collector");
        std::thread::sleep(std::time::Duration::from_millis(120));
        let snap = collector.sample().expect("sample live Windows CPU");
        assert!(
            (0.0..=100.0).contains(&snap.total_busy_pct),
            "total_busy_pct out of range: {}",
            snap.total_busy_pct
        );
        assert_ne!(snap.model_name, "");
        assert!(snap.uptime_secs > 0, "uptime_secs must be positive");
        assert!(!snap.cores.is_empty(), "cores must not be empty");
        if let Some(u) = snap.utility_pct {
            assert!((0.0..=100.0).contains(&u), "utility_pct out of range: {u}");
        }
        if let Some(mhz) = snap.freq_mhz {
            assert!(mhz > 100, "unexpectedly low live freq_mhz: {mhz}");
        }
        // Verify variable-sized RelationProcessorCore records actually populated every logical processor.
        let classes = query_core_efficiency_classes();
        assert_eq!(
            classes.len(),
            snap.cores.len(),
            "RelationProcessorCore must cover all logical processors"
        );
    }
}
