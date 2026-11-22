//! Platform-independent telemetry snapshot models and JSONL serialization for
//! deterministic `--record` / `--replay` testing.

#![forbid(unsafe_code)]

pub mod fixtures;
pub mod jsonl;

pub use jsonl::{
    JsonlError, RecordStats, SnapshotRecorder, parse_jsonl_file, parse_jsonl_reader,
    write_snapshot_line,
};

use serde::{Deserialize, Serialize};

/// Explicit availability and freshness state for any optional or async subsystem.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "state", content = "data", rename_all = "snake_case")]
pub enum DataState<T> {
    /// Waiting for an initial pair of samples to compute deltas or rates.
    #[default]
    WarmingUp,
    /// Fresh telemetry sample within expected cadence.
    Available(T),
    /// Last known sample when a slow worker timed out or paused.
    Stale { value: T, age_ms: u64 },
    /// Caller lacks sufficient Windows privileges (e.g., PPL or non-elevated SCM).
    Denied,
    /// Hardware, sensor, or provider is absent or unsupported on this machine.
    Unavailable(String),
}

/// Identifies a logical CPU across Windows processor groups (>64 logical processors).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LogicalCpuId {
    pub group: u16,
    pub number: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreKind {
    Uniform,
    Performance,
    Efficiency,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoreSample {
    pub id: LogicalCpuId,
    pub busy_pct: f32,
    pub kind: CoreKind,
    pub temp_celsius: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CpuSnapshot {
    pub model_name: String,
    pub total_busy_pct: f32,
    pub utility_pct: Option<f32>,
    pub freq_mhz: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freq_range_mhz: Option<(u32, u32)>,
    pub package_temp_celsius: Option<f32>,
    pub package_watts: Option<f32>,
    /// Emulated 1/5/15m Ready/Running thread EWMA (displayed as `RunQ`).
    pub run_queue_ewma: [f32; 3],
    pub uptime_secs: u64,
    pub cores: Vec<CoreSample>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MemSnapshot {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub cached_bytes: u64,
    pub free_bytes: u64,
    pub commit_used_bytes: u64,
    pub commit_limit_bytes: u64,
    pub pagefile_used_bytes: u64,
    pub pagefile_total_bytes: u64,
    pub compressed_bytes: Option<u64>,
}

/// Capacity and optional live throughput metrics for one local fixed Windows volume (`DRIVE_FIXED`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DiskSnapshot {
    pub mount: String,
    pub label: String,
    pub fs_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub physical_drive: Option<u32>,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub free_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_bytes_per_sec: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_bytes_per_sec: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub busy_pct: Option<f32>,
}

/// Live 64-bit IP Helper telemetry for one hardware Windows network interface keyed by `NET_LUID`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NetInterfaceSnapshot {
    pub luid: u64,
    pub name: String,
    pub description: String,
    pub is_up: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ipv4: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ipv6: Option<String>,
    pub rx_bytes_per_sec: u64,
    pub tx_bytes_per_sec: u64,
    pub rx_total_bytes: u64,
    pub tx_total_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_speed_bps: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: u32,
    /// 100-nanosecond intervals since 1601-01-01 UTC (`FILETIME`), preventing PID reuse bugs.
    pub create_time_100ns: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessEntry {
    pub identity: ProcessIdentity,
    pub parent_pid: u32,
    pub name: String,
    pub command_line: Option<String>,
    pub user: Option<String>,
    pub threads: u32,
    pub cpu_pct: f32,
    pub working_set_private_bytes: u64,
    pub io_read_bytes: u64,
    pub io_write_bytes: u64,
    pub is_protected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceEntry {
    pub name: String,
    pub display_name: String,
    pub state: String,
    pub start_type: String,
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuSnapshot {
    pub index: u8,
    pub name: String,
    pub vendor: String,
    pub pci_bdf: Option<String>,
    pub utilization_pct: Option<f32>,
    pub vram_used_bytes: Option<u64>,
    pub vram_total_bytes: Option<u64>,
    pub core_clock_mhz: Option<u32>,
    pub mem_clock_mhz: Option<u32>,
    pub temp_celsius: Option<f32>,
    pub power_watts: Option<f32>,
    pub power_limit_watts: Option<f32>,
    pub pcie_tx_kbs: Option<u64>,
    pub pcie_rx_kbs: Option<u64>,
    pub encoder_pct: Option<f32>,
    pub decoder_pct: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatterySnapshot {
    pub charge_pct: u8,
    pub status: String,
    pub watts: Option<f32>,
    pub remaining_secs: Option<u64>,
}

/// Complete immutable telemetry snapshot published per collection tick.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Snapshot {
    pub timestamp_ms: u64,
    pub hostname: String,
    pub username: String,
    /// Total physical RAM in bytes when known independently of full memory counter collection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_phys_mem_bytes: Option<u64>,
    pub cpu: DataState<CpuSnapshot>,
    pub mem: DataState<MemSnapshot>,
    #[serde(default)]
    pub disks: DataState<Vec<DiskSnapshot>>,
    #[serde(default)]
    pub net: DataState<Vec<NetInterfaceSnapshot>>,
    pub processes: DataState<Vec<ProcessEntry>>,
    pub services: DataState<Vec<ServiceEntry>>,
    pub gpus: Vec<DataState<GpuSnapshot>>,
    pub battery: DataState<BatterySnapshot>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_roundtrips_jsonl() {
        let snap = Snapshot {
            timestamp_ms: 1_700_000_000_000,
            hostname: "HOME".to_owned(),
            username: "franz".to_owned(),
            cpu: DataState::Available(CpuSnapshot {
                model_name: "AMD Ryzen 9".to_owned(),
                total_busy_pct: 42.5,
                uptime_secs: 3600,
                ..CpuSnapshot::default()
            }),
            ..Snapshot::default()
        };

        let line = serde_json::to_string(&snap).expect("serialize snapshot");
        assert!(!line.contains('\n'));
        let decoded: Snapshot = serde_json::from_str(&line).expect("deserialize snapshot");
        assert_eq!(snap, decoded);
    }
}
