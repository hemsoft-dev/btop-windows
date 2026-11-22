//! System telemetry collectors, waitable-timer sampler, and pure delta mathematics.

pub mod cpu;
pub mod replay;
pub mod sampler;

use btop_model::Snapshot;
use thiserror::Error;

pub use cpu::{CpuCollector, LiveCollector, compute_cpu_deltas};
pub use replay::{ReplaySource, StubLiveSource};
pub use sampler::{MAX_UPDATE_MS, MIN_UPDATE_MS, Sampler};

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
