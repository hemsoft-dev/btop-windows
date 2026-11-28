//! System telemetry collectors, waitable-timer sampler, and pure delta mathematics.

pub mod agent;
pub mod battery;
pub mod cpu;
pub mod disk;
pub mod gpu;
pub mod mem;
pub mod net;
pub mod nvml;
pub mod proc;
pub mod replay;
pub mod sampler;
pub mod services;
pub mod unix;

use btop_model::Snapshot;
use thiserror::Error;

pub use agent::{
    DISK_IO_UNAVAILABLE_REASON, LiveNodeCollector, NodeSampleSource,
    summarize_disks_for_node_sample, summarize_net_for_node_sample,
};
#[cfg(unix)]
pub use unix::{UnixAgentCollector, UnixLiveCollector};

pub use battery::{
    BATTERY_FLAG_CHARGING, BATTERY_FLAG_NO_BATTERY, BatteryCollector, decode_power_status,
};

pub use cpu::{
    CpuCollector, FreqMode, LiveCollector, RunQueueEwma, classify_core_kinds, compute_cpu_deltas,
    compute_cpu_deltas_with_kinds, compute_frequency_mhz,
};
pub use disk::{
    DiskCollector, build_disk_snapshots, compute_disk_busy_pct, compute_disk_io_rates,
    extract_drive_letter, matches_disks_filter,
};
pub use gpu::{GpuCollector, build_gpu_snapshots};
pub use mem::{MemoryCollector, compute_mem_snapshot, pagefile_pages_to_bytes};
pub use net::{NetCollector, compute_net_rates};
pub use nvml::{
    NVML_POLL_INTERVAL, NVML_STALE_TIMEOUT, NvmlSnapshotState, NvmlWorker, apply_nvml_enrichment,
    match_nvml_devices, normalize_pci_bdf,
};
pub use proc::{ProcessCollector, ProcessDeltaTracker};
pub use replay::{ReplaySource, StubLiveSource};
pub use sampler::{MAX_UPDATE_MS, MIN_UPDATE_MS, Sampler};
pub use services::{SERVICE_POLL_INTERVAL, ServiceCollector};

#[derive(Debug, Error)]
pub enum CollectError {
    #[error("collector failed: {0}")]
    Platform(String),
    #[error("replay stream error: {0}")]
    Replay(String),
    #[error("replay stream reached end of file")]
    EndOfStream,
}

/// Trait implemented by live Windows collectors and deterministic replay fixtures.
pub trait SnapshotSource: Send {
    fn next_snapshot(&mut self) -> Result<Snapshot, CollectError>;

    /// Updates the live frequency aggregation mode (e.g., on `Ctrl+R` config reload).
    fn set_freq_mode(&mut self, _mode: FreqMode) {}

    /// Updates optional NVIDIA NVML `PCIe` Tx/Rx measurement (e.g., on `Ctrl+R` config reload).
    fn set_nvml_measure_pcie_speeds(&mut self, _enabled: bool) {}

    /// Enables or disables on-demand Windows Services (SCM) polling while the Services view is open.
    fn set_services_active(&mut self, _active: bool) {}

    /// Requests an immediate SCM re-poll on the next tick (e.g., after Start or Stop).
    fn request_services_refresh(&mut self) {}

    /// Polls Windows Services on demand without running a full telemetry tick so the Services
    /// view stays on its 2.5s cadence regardless of `update_ms`.
    fn poll_services_only(
        &mut self,
    ) -> Option<btop_model::DataState<Vec<btop_model::ServiceEntry>>> {
        None
    }

    /// Returns `true` only for live host telemetry sources where mutating OS process actions
    /// (`WM_CLOSE`, `TerminateProcess`, `SetPriorityClass`) are authorized. Replay and stub
    /// sources default to `false` (fail-closed).
    fn allows_live_actions(&self) -> bool {
        false
    }
}

/// Pure, platform-independent delta math tested on every CI target.
pub mod math {
    /// Computes CPU busy percentage from Windows `KernelTime`, `UserTime`, and `IdleTime` deltas.
    ///
    /// On Windows, `KernelTime` **includes** `IdleTime`, so total elapsed CPU time is
    /// `delta_kernel + delta_user` and busy time is `(delta_kernel + delta_user) - delta_idle`.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    pub fn cpu_busy_percent(delta_kernel: u64, delta_user: u64, delta_idle: u64) -> f32 {
        let total = delta_kernel.saturating_add(delta_user);
        if total == 0 {
            return 0.0;
        }
        let busy = total.saturating_sub(delta_idle);
        ((busy as f64 / total as f64) * 100.0).clamp(0.0, 100.0) as f32
    }

    /// Validates whether `parent_create_time_100ns` can legally be the parent of
    /// `child_create_time_100ns`, preventing false trees caused by Windows PID reuse.
    pub fn is_valid_parent_child(
        parent_create_time_100ns: u64,
        child_create_time_100ns: u64,
    ) -> bool {
        parent_create_time_100ns > 0 && parent_create_time_100ns <= child_create_time_100ns
    }

    /// Computes non-negative rate in units/second, clamping counter rollbacks or adapter resets.
    #[allow(clippy::cast_precision_loss)]
    pub fn clamped_rate_per_sec(prev: u64, curr: u64, elapsed_ms: u64) -> f64 {
        if elapsed_ms == 0 || curr < prev {
            return 0.0;
        }
        let delta = curr - prev;
        (delta as f64) * 1000.0 / (elapsed_ms as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::math::*;

    #[test]
    fn cpu_busy_subtracts_idle_from_kernel() {
        // 800 kernel (of which 600 idle) + 200 user => total 1000, busy 400 => 40%
        let pct = cpu_busy_percent(800, 200, 600);
        assert!((pct - 40.0).abs() < 0.01);
        assert!((cpu_busy_percent(0, 0, 0) - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn pid_reuse_rejects_younger_parent() {
        assert!(is_valid_parent_child(1000, 2000));
        assert!(!is_valid_parent_child(3000, 2000));
        assert!(!is_valid_parent_child(0, 2000));
    }

    #[test]
    fn rate_clamps_counter_reset() {
        assert!((clamped_rate_per_sec(500, 200, 1000) - 0.0).abs() < f64::EPSILON);
        assert!((clamped_rate_per_sec(100, 600, 500) - 1000.0).abs() < 0.01);
    }
}
