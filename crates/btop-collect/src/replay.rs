//! Deterministic JSONL replay and lightweight placeholder snapshot sources.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use btop_model::{CpuSnapshot, DataState, Snapshot, parse_jsonl_file};

use crate::{CollectError, SnapshotSource};

/// Replays pre-recorded [`Snapshot`] streams without calling any Windows APIs.
#[derive(Debug, Clone)]
pub struct ReplaySource {
    snapshots: Vec<Snapshot>,
    cursor: usize,
    looping: bool,
}

impl ReplaySource {
    /// Loads and validates a JSONL snapshot stream from `path`.
    pub fn from_path(path: &Path) -> Result<Self, CollectError> {
        let snapshots =
            parse_jsonl_file(path).map_err(|err| CollectError::Replay(err.to_string()))?;
        Ok(Self {
            snapshots,
            cursor: 0,
            looping: true,
        })
    }

    /// Builds a replay source directly from an in-memory sequence of snapshots.
    pub fn from_snapshots(snapshots: Vec<Snapshot>) -> Result<Self, CollectError> {
        if snapshots.is_empty() {
            return Err(CollectError::Replay(
                "snapshot JSONL stream contains no snapshots".to_owned(),
            ));
        }
        Ok(Self {
            snapshots,
            cursor: 0,
            looping: true,
        })
    }

    /// Controls whether replay wraps around to the first snapshot after reaching the end.
    #[must_use]
    pub fn with_looping(mut self, looping: bool) -> Self {
        self.looping = looping;
        self
    }

    pub fn len(&self) -> usize {
        self.snapshots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }
}

impl SnapshotSource for ReplaySource {
    fn next_snapshot(&mut self) -> Result<Snapshot, CollectError> {
        if self.cursor >= self.snapshots.len() {
            if self.looping {
                self.cursor = 0;
            } else {
                return Err(CollectError::EndOfStream);
            }
        }
        let snapshot = self.snapshots[self.cursor].clone();
        self.cursor += 1;
        Ok(snapshot)
    }
}

/// Portable placeholder live source used until live Windows collectors land in M1.
#[derive(Debug, Clone)]
pub struct StubLiveSource {
    hostname: String,
    username: String,
    tick: u64,
    start_ms: u64,
    step_ms: u64,
}

impl StubLiveSource {
    pub fn new(step_ms: u32) -> Self {
        let hostname = std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "WINDOWS-HOST".to_owned());
        let username = std::env::var("USERNAME")
            .or_else(|_| std::env::var("USER"))
            .unwrap_or_else(|_| "user".to_owned());
        let start_ms = u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis()),
        )
        .unwrap_or(u64::MAX);
        Self {
            hostname,
            username,
            tick: 0,
            start_ms,
            step_ms: u64::from(step_ms.max(100)),
        }
    }
}

impl SnapshotSource for StubLiveSource {
    fn next_snapshot(&mut self) -> Result<Snapshot, CollectError> {
        let tick = self.tick;
        self.tick += 1;
        Ok(Snapshot {
            timestamp_ms: self.start_ms.saturating_add(tick * self.step_ms),
            hostname: self.hostname.clone(),
            username: self.username.clone(),
            cpu: DataState::Available(CpuSnapshot {
                model_name: "Pending M1 live CPU collector".to_owned(),
                total_busy_pct: 0.0,
                uptime_secs: (tick * self.step_ms) / 1000,
                ..CpuSnapshot::default()
            }),
            ..Snapshot::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use btop_model::fixtures::win11_8core_single_gpu_desktop;

    use super::*;

    #[test]
    fn replay_source_loops_and_stops_when_configured() {
        let fixtures = win11_8core_single_gpu_desktop();
        let mut looping = ReplaySource::from_snapshots(fixtures.clone()).expect("create replay");
        assert_eq!(looping.len(), 3);
        assert_eq!(looping.next_snapshot().expect("t0"), fixtures[0]);
        assert_eq!(looping.next_snapshot().expect("t1"), fixtures[1]);
        assert_eq!(looping.next_snapshot().expect("t2"), fixtures[2]);
        assert_eq!(looping.next_snapshot().expect("wrap t0"), fixtures[0]);

        let mut finite = ReplaySource::from_snapshots(fixtures.clone())
            .expect("create finite")
            .with_looping(false);
        for expected in &fixtures {
            assert_eq!(finite.next_snapshot().expect("tick"), *expected);
        }
        assert!(matches!(
            finite.next_snapshot(),
            Err(CollectError::EndOfStream)
        ));
    }

    #[test]
    fn replay_source_rejects_empty_snapshots() {
        let err = ReplaySource::from_snapshots(Vec::new()).expect_err("empty must fail");
        assert!(matches!(err, CollectError::Replay(_)));
    }
}
