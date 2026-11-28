//! Bounded in-memory `Ring`, monotonic `AgentClock`, and sleep-gap delta guard for the Fleet
//! Agent. Writes nothing to disk.

use std::collections::VecDeque;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use btop_model::{
    DEFAULT_AGENT_INTERVAL_SECS, DEFAULT_AGENT_RETENTION_MINS, DataState, MAX_AGENT_INTERVAL_SECS,
    MAX_AGENT_RETENTION_MINS, MIN_AGENT_INTERVAL_SECS, MIN_AGENT_RETENTION_MINS, NodeSample,
};

use crate::FleetError;

/// Validated sampling cadence and retention window for a Fleet Agent `Ring`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingConfig {
    interval_secs: u64,
    retention_mins: u64,
}

impl Default for RingConfig {
    fn default() -> Self {
        Self {
            interval_secs: DEFAULT_AGENT_INTERVAL_SECS,
            retention_mins: DEFAULT_AGENT_RETENTION_MINS,
        }
    }
}

impl RingConfig {
    /// Validates and constructs a [`RingConfig`].
    ///
    /// - `interval_secs` must be in `1..=60` (default `5`).
    /// - `retention_mins` must be in `1..=60` (default `10`).
    pub fn new(interval_secs: u64, retention_mins: u64) -> Result<Self, FleetError> {
        if !(MIN_AGENT_INTERVAL_SECS..=MAX_AGENT_INTERVAL_SECS).contains(&interval_secs) {
            return Err(FleetError::InvalidConfig(format!(
                "interval must be between {MIN_AGENT_INTERVAL_SECS}s and {MAX_AGENT_INTERVAL_SECS}s, got {interval_secs}s"
            )));
        }
        if !(MIN_AGENT_RETENTION_MINS..=MAX_AGENT_RETENTION_MINS).contains(&retention_mins) {
            return Err(FleetError::InvalidConfig(format!(
                "retention must be between {MIN_AGENT_RETENTION_MINS}m and {MAX_AGENT_RETENTION_MINS}m, got {retention_mins}m"
            )));
        }
        Ok(Self {
            interval_secs,
            retention_mins,
        })
    }

    pub const fn interval_secs(self) -> u64 {
        self.interval_secs
    }

    pub const fn retention_mins(self) -> u64 {
        self.retention_mins
    }

    pub const fn interval_ms(self) -> u64 {
        self.interval_secs.saturating_mul(1_000)
    }

    pub const fn retention_ms(self) -> u64 {
        self.retention_mins.saturating_mul(60_000)
    }

    /// Maximum number of `NodeSample` entries retained in memory (`retention / interval`, e.g.
    /// 120 samples for the default 10m window at 5s intervals).
    #[allow(clippy::cast_possible_truncation)]
    pub const fn capacity(self) -> usize {
        let count = (self.retention_mins * 60) / self.interval_secs;
        if count == 0 { 1 } else { count as usize }
    }
}

/// Returns `true` when `elapsed_ms` spans more than `3x interval_ms` (indicating host suspend or
/// scheduler stall where cumulative rate deltas must be dropped per ADR-0004).
pub const fn is_sleep_gap(elapsed_ms: u64, interval_ms: u64) -> bool {
    elapsed_ms > interval_ms.saturating_mul(3)
}

/// Drops rate/delta fields on `sample` after a sleep gap (`> 3x interval`) while preserving
/// instantaneous gauges (`mem_*`, `disk_max_used_pct`, `uptime_secs`) and `DataState::Unavailable`
/// markers for permanently unsupported host subsystems.
pub fn apply_sleep_gap_guard(sample: &mut NodeSample) {
    if matches!(sample.cpu_busy_pct, DataState::Available(_)) {
        sample.cpu_busy_pct = DataState::WarmingUp;
    }
    if matches!(sample.disk_io, DataState::Available(_)) {
        sample.disk_io = DataState::WarmingUp;
    }
    if matches!(sample.net_io, DataState::Available(_)) {
        sample.net_io = DataState::WarmingUp;
    }
}

fn unix_epoch_ms() -> u64 {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    u64::try_from(dur.as_millis()).unwrap_or(u64::MAX)
}

/// Monotonic Agent clock anchored at Unix epoch milliseconds.
///
/// Advances by `max(monotonic_delta, positive_wall_delta)` so:
/// 1. Backward NTP clock steps never move the `Cursor` backward or break incremental polling.
/// 2. Host S3 suspend on Linux (where `CLOCK_MONOTONIC` pauses) still advances Agent time via the
///    post-wake wall-clock jump, ensuring sleep-gap detection fires reliably on resume.
#[derive(Debug, Clone)]
pub struct AgentClock {
    current_ms: u64,
    last_mono: Instant,
    last_wall_ms: u64,
}

impl Default for AgentClock {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentClock {
    pub fn new() -> Self {
        let wall = unix_epoch_ms().max(1);
        Self {
            current_ms: wall,
            last_mono: Instant::now(),
            last_wall_ms: wall,
        }
    }

    #[cfg(test)]
    fn from_parts(initial_ms: u64) -> Self {
        Self {
            current_ms: initial_ms,
            last_mono: Instant::now(),
            last_wall_ms: initial_ms,
        }
    }

    /// Advances and returns the current Agent clock reading in milliseconds.
    pub fn now_ms(&mut self) -> u64 {
        let now_mono = Instant::now();
        let mono_delta = u64::try_from(
            now_mono
                .saturating_duration_since(self.last_mono)
                .as_millis(),
        )
        .unwrap_or(u64::MAX);
        let wall_now = unix_epoch_ms();
        let step = self.advance_deltas(mono_delta, wall_now);
        self.last_mono = now_mono;
        step
    }

    fn advance_deltas(&mut self, mono_delta_ms: u64, wall_now_ms: u64) -> u64 {
        let wall_delta_ms = wall_now_ms.saturating_sub(self.last_wall_ms);
        let step = mono_delta_ms.max(wall_delta_ms);
        self.current_ms = self.current_ms.saturating_add(step);
        self.last_wall_ms = wall_now_ms;
        self.current_ms
    }

    /// Returns a strictly increasing sample timestamp (`> last_sample_ms` when known) so two
    /// samples never collide on the same `Cursor` value.
    pub fn next_sample_timestamp_ms(&mut self, last_sample_ms: Option<u64>) -> u64 {
        let now = self.now_ms();
        match last_sample_ms {
            Some(prev) if now <= prev => {
                self.current_ms = prev.saturating_add(1);
                self.current_ms
            }
            _ => now,
        }
    }
}

/// Bounded in-memory sliding window of chronological `NodeSample` values held by a Fleet Agent.
#[derive(Debug, Clone)]
pub struct Ring {
    config: RingConfig,
    samples: VecDeque<NodeSample>,
    last_sample_ts_ms: Option<u64>,
}

impl Ring {
    pub fn new(config: RingConfig) -> Self {
        Self {
            config,
            samples: VecDeque::with_capacity(config.capacity()),
            last_sample_ts_ms: None,
        }
    }

    pub const fn config(&self) -> RingConfig {
        self.config
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub const fn latest_timestamp_ms(&self) -> Option<u64> {
        self.last_sample_ts_ms
    }

    /// Appends `sample` to the bounded `Ring`, enforcing:
    /// 1. Strictly increasing timestamps (non-monotonic stale samples are ignored and return
    ///    `false`).
    /// 2. Sleep-gap delta dropping whenever the timestamp gap OR optional `mono_elapsed_ms` from
    ///    the previous sample exceeds `3x interval_ms`.
    /// 3. Eviction of entries older than `retention_ms` or exceeding `capacity()`.
    pub fn push(&mut self, mut sample: NodeSample, mono_elapsed_ms: Option<u64>) -> bool {
        if matches!(sample.cpu_busy_pct, DataState::Available(v) if !v.is_finite()) {
            sample.cpu_busy_pct = DataState::Unavailable("non-finite CPU reading".to_owned());
        }
        if matches!(sample.disk_max_used_pct, DataState::Available(v) if !v.is_finite()) {
            sample.disk_max_used_pct =
                DataState::Unavailable("non-finite disk usage reading".to_owned());
        }

        if let Some(prev_ts) = self.last_sample_ts_ms {
            if sample.timestamp_ms <= prev_ts {
                return false;
            }
            let ts_gap = sample.timestamp_ms.saturating_sub(prev_ts);
            let effective_gap = mono_elapsed_ms.map_or(ts_gap, |m| ts_gap.max(m));
            if is_sleep_gap(effective_gap, self.config.interval_ms()) {
                apply_sleep_gap_guard(&mut sample);
            }
        } else if let Some(mono_ms) = mono_elapsed_ms
            && is_sleep_gap(mono_ms, self.config.interval_ms())
        {
            apply_sleep_gap_guard(&mut sample);
        }

        let retention_ms = self.config.retention_ms();
        let newest_ts = sample.timestamp_ms;
        while let Some(front) = self.samples.front() {
            if newest_ts.saturating_sub(front.timestamp_ms) > retention_ms {
                self.samples.pop_front();
            } else {
                break;
            }
        }

        let cap = self.config.capacity();
        while self.samples.len() >= cap {
            self.samples.pop_front();
        }

        self.last_sample_ts_ms = Some(newest_ts);
        self.samples.push_back(sample);
        true
    }

    /// Returns all samples in chronological order when `since_ms == 0`, or only samples strictly
    /// newer than `since_ms` otherwise (using the Agent's own clock).
    pub fn samples_since(&self, since_ms: u64) -> Vec<NodeSample> {
        if since_ms == 0 {
            return self.samples.iter().cloned().collect();
        }
        self.samples
            .iter()
            .filter(|s| s.timestamp_ms > since_ms)
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use btop_model::{DiskIoRate, NetIoRate};

    use super::*;

    fn mk_sample(ts_ms: u64, cpu: f32) -> NodeSample {
        NodeSample {
            timestamp_ms: ts_ms,
            cpu_busy_pct: DataState::Available(cpu),
            mem_used_bytes: 8_000,
            mem_total_bytes: 16_000,
            disk_max_used_pct: DataState::Available(55.0),
            disk_io: DataState::Available(DiskIoRate {
                read_bytes_per_sec: 100,
                write_bytes_per_sec: 200,
            }),
            net_io: DataState::Available(NetIoRate {
                rx_bytes_per_sec: 300,
                tx_bytes_per_sec: 400,
            }),
            uptime_secs: 1_000,
        }
    }

    #[test]
    fn ring_config_validates_bounds_and_computes_capacity() {
        assert!(RingConfig::new(0, 10).is_err());
        assert!(RingConfig::new(61, 10).is_err());
        assert!(RingConfig::new(5, 0).is_err());
        assert!(RingConfig::new(5, 61).is_err());

        let def = RingConfig::default();
        assert_eq!(def.interval_secs(), 5);
        assert_eq!(def.retention_mins(), 10);
        assert_eq!(def.interval_ms(), 5_000);
        assert_eq!(def.retention_ms(), 600_000);
        assert_eq!(def.capacity(), 120);

        let min_cfg = RingConfig::new(60, 1).expect("1m at 60s");
        assert_eq!(min_cfg.capacity(), 1);
    }

    #[test]
    fn ring_evicts_by_capacity_and_retention_and_slices_by_cursor() {
        // 1m retention at 15s interval => capacity 4
        let cfg = RingConfig::new(15, 1).expect("valid config");
        assert_eq!(cfg.capacity(), 4);
        let mut ring = Ring::new(cfg);

        for i in 1..=6u16 {
            assert!(ring.push(mk_sample(u64::from(i) * 15_000, f32::from(i)), Some(15_000)));
        }

        // Only the newest 4 samples remain (t = 45s, 60s, 75s, 90s)
        let all = ring.samples_since(0);
        let ts: Vec<u64> = all.iter().map(|s| s.timestamp_ms).collect();
        assert_eq!(ts, vec![45_000, 60_000, 75_000, 90_000]);

        // Incremental cursor returns strictly newer entries
        let delta = ring.samples_since(60_000);
        assert_eq!(
            delta.iter().map(|s| s.timestamp_ms).collect::<Vec<_>>(),
            vec![75_000, 90_000]
        );
        assert_eq!(ring.samples_since(90_000).len(), 0);
        assert_eq!(ring.samples_since(999_999).len(), 0);

        // Non-monotonic timestamp is rejected without mutating ring
        assert!(!ring.push(mk_sample(90_000, 1.0), Some(1_000)));
        assert!(!ring.push(mk_sample(80_000, 1.0), Some(1_000)));
        assert_eq!(ring.len(), 4);
    }

    #[test]
    fn sleep_gap_drops_rate_deltas_but_preserves_gauges_and_unavailable_state() {
        let cfg = RingConfig::new(5, 10).expect("5s / 10m");
        let mut ring = Ring::new(cfg);

        // First sample at t = 10_000
        assert!(ring.push(mk_sample(10_000, 20.0), Some(5_000)));
        // Boundary: exactly 3x interval (15_000ms) is NOT a sleep gap
        assert!(ring.push(mk_sample(25_000, 35.0), Some(15_000)));
        let at_boundary = ring.samples_since(10_000);
        assert_eq!(at_boundary.len(), 1);
        assert_eq!(at_boundary[0].cpu_busy_pct, DataState::Available(35.0));

        // Sleep gap: 15_001ms (> 3x interval), with Unsupported disk I/O preserved
        let mut post_wake = mk_sample(40_001, 99.9);
        post_wake.disk_io = DataState::Unavailable("unsupported".to_owned());
        assert!(ring.push(post_wake, Some(15_001)));

        let wake_samples = ring.samples_since(25_000);
        assert_eq!(wake_samples.len(), 1);
        let s = &wake_samples[0];
        assert_eq!(s.cpu_busy_pct, DataState::WarmingUp);
        assert_eq!(s.net_io, DataState::WarmingUp);
        // Unavailable stays Unavailable (never overwritten with WarmingUp)
        assert_eq!(s.disk_io, DataState::Unavailable("unsupported".to_owned()));
        // Instantaneous gauges stay intact
        assert_eq!(s.mem_used_bytes, 8_000);
        assert_eq!(s.mem_total_bytes, 16_000);
        assert_eq!(s.disk_max_used_pct, DataState::Available(55.0));
        assert_eq!(s.uptime_secs, 1_000);

        // Next normal tick resumes full Available rates
        assert!(ring.push(mk_sample(45_001, 18.0), Some(5_000)));
        let resumed = ring.samples_since(40_001);
        assert_eq!(resumed[0].cpu_busy_pct, DataState::Available(18.0));
        assert!(matches!(resumed[0].net_io, DataState::Available(_)));
    }

    #[test]
    fn retention_window_evicts_pre_sleep_samples_on_wake() {
        let cfg = RingConfig::new(5, 1).expect("5s / 1m");
        let mut ring = Ring::new(cfg);

        assert!(ring.push(mk_sample(100_000, 10.0), Some(5_000)));
        assert!(ring.push(mk_sample(105_000, 12.0), Some(5_000)));
        // Host slept 5 minutes (300_000 ms > 60_000 ms retention)
        assert!(ring.push(mk_sample(405_000, 88.0), Some(300_000)));

        let all = ring.samples_since(0);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].timestamp_ms, 405_000);
        assert_eq!(all[0].cpu_busy_pct, DataState::WarmingUp);
    }

    #[test]
    fn agent_clock_never_moves_backward_on_ntp_step_and_advances_on_suspend_wake() {
        let mut clock = AgentClock::from_parts(1_000_000);
        // Backward wall-clock jump of 60s while monotonic advanced 5s => advances +5s
        let t1 = clock.advance_deltas(5_000, 940_000);
        assert_eq!(t1, 1_005_000);

        // Linux S3 suspend: monotonic paused (+100ms), wall clock jumped +300s => advances +300s
        let t2 = clock.advance_deltas(100, 1_240_000);
        assert_eq!(t2, 1_305_000);
    }
}
