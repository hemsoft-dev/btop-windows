//! Lightweight Fleet Agent telemetry collector producing OS-neutral [`NodeSample`] readings.
//!
//! On Windows, polls strictly CPU (`NtQuerySystemInformation`), memory (`GlobalMemoryStatusEx`),
//! local fixed disks (`DiskCollector`), and physical network interfaces (`NetCollector`) directly,
//! without ever initializing process, GPU, NVML, battery, or SCM polling.
//!
//! On Linux and macOS, polls strictly CPU usage, memory, disks, and physical network interfaces
//! without ever reading `/proc/<pid>`, process lists, command lines, or users.

use std::collections::HashSet;

use btop_model::{
    DataState, DiskIoRate, DiskSnapshot, NetInterfaceSnapshot, NetIoRate, NodeSample,
};

use crate::CollectError;

/// Canonical reason reported when an OS/device exposes no local disk I/O byte counters.
pub const DISK_IO_UNAVAILABLE_REASON: &str = "disk I/O rates unavailable";

/// Source of OS-neutral [`NodeSample`] telemetry for the Fleet Agent `Ring`.
pub trait NodeSampleSource: Send {
    fn next_node_sample(&mut self, timestamp_ms: u64) -> Result<NodeSample, CollectError>;
}

/// Summarizes local [`DiskSnapshot`] volumes into `(disk_max_used_pct, disk_io)` for a
/// [`NodeSample`].
///
/// Deduplicates `physical_drive` indices so multiple partitions sharing one physical disk on
/// Windows are never double-counted in aggregate read/write throughput.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
pub fn summarize_disks_for_node_sample(
    disks: &[DiskSnapshot],
    has_baseline: bool,
) -> (DataState<f32>, DataState<DiskIoRate>) {
    let mut worst_pct: Option<f32> = None;
    let mut seen_physical_drives: HashSet<u32> = HashSet::new();
    let mut any_io = false;
    let mut total_read = 0u64;
    let mut total_write = 0u64;

    for disk in disks {
        if disk.total_bytes > 0 {
            let pct = ((disk.used_bytes as f64 / disk.total_bytes as f64) * 100.0).clamp(0.0, 100.0)
                as f32;
            worst_pct = Some(worst_pct.map_or(pct, |cur| cur.max(pct)));
        }

        let include_io = match disk.physical_drive {
            Some(drive_idx) => seen_physical_drives.insert(drive_idx),
            None => true,
        };
        if include_io && (disk.read_bytes_per_sec.is_some() || disk.write_bytes_per_sec.is_some()) {
            any_io = true;
            total_read = total_read.saturating_add(disk.read_bytes_per_sec.unwrap_or(0));
            total_write = total_write.saturating_add(disk.write_bytes_per_sec.unwrap_or(0));
        }
    }

    let fullness = match worst_pct {
        Some(pct) => DataState::Available(pct),
        None => DataState::Unavailable("no local storage volumes".to_owned()),
    };

    let io_state = if any_io {
        DataState::Available(DiskIoRate {
            read_bytes_per_sec: total_read,
            write_bytes_per_sec: total_write,
        })
    } else if !has_baseline {
        DataState::WarmingUp
    } else {
        DataState::Unavailable(DISK_IO_UNAVAILABLE_REASON.to_owned())
    };

    (fullness, io_state)
}

/// Aggregates physical [`NetInterfaceSnapshot`] entries into [`NetIoRate`] for a [`NodeSample`].
pub fn summarize_net_for_node_sample(
    ifaces: &[NetInterfaceSnapshot],
    has_baseline: bool,
) -> DataState<NetIoRate> {
    if !has_baseline {
        return DataState::WarmingUp;
    }
    let mut rx = 0u64;
    let mut tx = 0u64;
    for iface in ifaces {
        rx = rx.saturating_add(iface.rx_bytes_per_sec);
        tx = tx.saturating_add(iface.tx_bytes_per_sec);
    }
    DataState::Available(NetIoRate {
        rx_bytes_per_sec: rx,
        tx_bytes_per_sec: tx,
    })
}

#[cfg(windows)]
mod platform {
    use btop_model::{DataState, NodeSample};
    use btop_sys::{
        SystemProcessorPerformanceInformation, query_memory_status, query_processor_performance,
        system_uptime_secs,
    };

    use super::{NodeSampleSource, summarize_disks_for_node_sample, summarize_net_for_node_sample};
    use crate::CollectError;
    use crate::cpu::compute_cpu_deltas;
    use crate::disk::DiskCollector;
    use crate::net::NetCollector;

    /// Windows 11 Fleet Agent collector invoking strictly CPU, memory, disk, and network
    /// collectors without initializing process, GPU, NVML, battery, or SCM polling.
    #[derive(Debug)]
    pub struct LiveNodeCollector {
        prev_cpu: Vec<SystemProcessorPerformanceInformation>,
        disks: DiskCollector,
        net: NetCollector,
        disks_has_baseline: bool,
        net_has_baseline: bool,
    }

    impl LiveNodeCollector {
        pub fn new() -> Result<Self, CollectError> {
            let prev_cpu = query_processor_performance().map_err(CollectError::Platform)?;
            let mut disks = DiskCollector::new();
            let mut net = NetCollector::new();
            // Prime disk and network baselines once so the first post-warmup sample reports live
            // rates without requiring two full intervals.
            let disks_has_baseline = disks.sample().is_ok();
            let net_has_baseline = net.sample().is_ok();
            Ok(Self {
                prev_cpu,
                disks,
                net,
                disks_has_baseline,
                net_has_baseline,
            })
        }
    }

    impl NodeSampleSource for LiveNodeCollector {
        fn next_node_sample(&mut self, timestamp_ms: u64) -> Result<NodeSample, CollectError> {
            let cpu_busy_pct = match query_processor_performance() {
                Ok(curr) => {
                    let state = if self.prev_cpu.len() == curr.len() {
                        DataState::Available(compute_cpu_deltas(&self.prev_cpu, &curr).0)
                    } else {
                        DataState::WarmingUp
                    };
                    self.prev_cpu = curr;
                    state
                }
                Err(err) => DataState::Unavailable(err),
            };

            let (mem_used_bytes, mem_total_bytes) = match query_memory_status() {
                Ok(status) => {
                    let total = status.total_phys_bytes;
                    let avail = status.avail_phys_bytes.min(total);
                    (total.saturating_sub(avail), total)
                }
                Err(_) => (0, 0),
            };

            let (disk_max_used_pct, disk_io) = match self.disks.sample() {
                Ok(disks) => {
                    let summary = summarize_disks_for_node_sample(&disks, self.disks_has_baseline);
                    self.disks_has_baseline = true;
                    summary
                }
                Err(err) => (
                    DataState::Unavailable(err.to_string()),
                    DataState::Unavailable(err.to_string()),
                ),
            };

            let net_io = match self.net.sample() {
                Ok(ifaces) => {
                    let summary = summarize_net_for_node_sample(&ifaces, self.net_has_baseline);
                    self.net_has_baseline = true;
                    summary
                }
                Err(err) => DataState::Unavailable(err.to_string()),
            };

            Ok(NodeSample {
                timestamp_ms,
                cpu_busy_pct,
                mem_used_bytes,
                mem_total_bytes,
                disk_max_used_pct,
                disk_io,
                net_io,
                uptime_secs: system_uptime_secs(),
            })
        }
    }
}

#[cfg(unix)]
mod platform {
    use btop_model::NodeSample;

    use super::NodeSampleSource;
    use crate::CollectError;
    use crate::unix::UnixAgentCollector;

    /// Linux and macOS Fleet Agent collector polling strictly CPU, memory, disks, and physical
    /// network interfaces without reading processes or users.
    #[derive(Debug)]
    pub struct LiveNodeCollector {
        inner: UnixAgentCollector,
    }

    impl LiveNodeCollector {
        pub fn new() -> Result<Self, CollectError> {
            Ok(Self {
                inner: UnixAgentCollector::new()?,
            })
        }
    }

    impl NodeSampleSource for LiveNodeCollector {
        fn next_node_sample(&mut self, timestamp_ms: u64) -> Result<NodeSample, CollectError> {
            self.inner.sample_node(timestamp_ms)
        }
    }
}

pub use platform::LiveNodeCollector;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_disks_deduplicates_shared_physical_drives_and_picks_worst_fullness() {
        let disks = vec![
            DiskSnapshot {
                mount: "C:\\".to_owned(),
                physical_drive: Some(0),
                total_bytes: 1_000,
                used_bytes: 400, // 40%
                free_bytes: 600,
                read_bytes_per_sec: Some(1_000),
                write_bytes_per_sec: Some(2_000),
                ..DiskSnapshot::default()
            },
            DiskSnapshot {
                mount: "D:\\".to_owned(),
                physical_drive: Some(0), // Same PhysicalDrive0 as C:\ -> must NOT double-count I/O
                total_bytes: 1_000,
                used_bytes: 850, // 85%
                free_bytes: 150,
                read_bytes_per_sec: Some(1_000),
                write_bytes_per_sec: Some(2_000),
                ..DiskSnapshot::default()
            },
            DiskSnapshot {
                mount: "E:\\".to_owned(),
                physical_drive: Some(1),
                total_bytes: 2_000,
                used_bytes: 500, // 25%
                free_bytes: 1_500,
                read_bytes_per_sec: Some(500),
                write_bytes_per_sec: Some(300),
                ..DiskSnapshot::default()
            },
        ];

        let (fullness, io) = summarize_disks_for_node_sample(&disks, true);
        assert_eq!(fullness, DataState::Available(85.0));
        assert_eq!(
            io,
            DataState::Available(DiskIoRate {
                read_bytes_per_sec: 1_500,
                write_bytes_per_sec: 2_300,
            })
        );
    }

    #[test]
    fn summarize_disks_reports_unavailable_when_os_has_no_io_counters_after_baseline() {
        let disks = vec![DiskSnapshot {
            mount: "/".to_owned(),
            physical_drive: None,
            total_bytes: 500,
            used_bytes: 250,
            free_bytes: 250,
            read_bytes_per_sec: None,
            write_bytes_per_sec: None,
            ..DiskSnapshot::default()
        }];

        let (fullness_warm, io_warm) = summarize_disks_for_node_sample(&disks, false);
        assert_eq!(fullness_warm, DataState::Available(50.0));
        assert_eq!(io_warm, DataState::WarmingUp);

        let (fullness, io) = summarize_disks_for_node_sample(&disks, true);
        assert_eq!(fullness, DataState::Available(50.0));
        assert_eq!(
            io,
            DataState::Unavailable(DISK_IO_UNAVAILABLE_REASON.to_owned())
        );
    }

    #[test]
    fn summarize_net_sums_physical_nics_and_handles_warmup() {
        let ifaces = vec![
            NetInterfaceSnapshot {
                name: "eth0".to_owned(),
                is_up: true,
                rx_bytes_per_sec: 1_200,
                tx_bytes_per_sec: 300,
                ..NetInterfaceSnapshot::default()
            },
            NetInterfaceSnapshot {
                name: "wlan0".to_owned(),
                is_up: true,
                rx_bytes_per_sec: 800,
                tx_bytes_per_sec: 200,
                ..NetInterfaceSnapshot::default()
            },
        ];

        assert_eq!(
            summarize_net_for_node_sample(&ifaces, false),
            DataState::WarmingUp
        );
        assert_eq!(
            summarize_net_for_node_sample(&ifaces, true),
            DataState::Available(NetIoRate {
                rx_bytes_per_sec: 2_000,
                tx_bytes_per_sec: 500,
            })
        );
    }

    #[test]
    fn live_node_collector_produces_valid_os_neutral_sample() {
        let mut collector = LiveNodeCollector::new().expect("init live node collector");
        std::thread::sleep(std::time::Duration::from_millis(220));
        let sample = collector
            .next_node_sample(1_732_800_000_000)
            .expect("collect live node sample");
        assert_eq!(sample.timestamp_ms, 1_732_800_000_000);
        assert!(sample.mem_total_bytes > 0);
        assert!(sample.mem_used_bytes <= sample.mem_total_bytes);
        assert!(matches!(sample.cpu_busy_pct, DataState::Available(_)));
        assert!(matches!(sample.disk_max_used_pct, DataState::Available(_)));
        assert!(matches!(sample.net_io, DataState::Available(_)));
    }
}
