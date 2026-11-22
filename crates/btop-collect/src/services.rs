//! On-demand Windows Service Control Manager (SCM) collector that polls every 2.5 seconds
//! **only** while the Services view (`S`) is active.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use btop_model::{DataState, ServiceEntry};
use btop_sys::enumerate_windows_services;

/// SCM polling cadence while the Services view is active (within the 2-3s target window).
pub const SERVICE_POLL_INTERVAL: Duration = Duration::from_millis(2500);

/// Stateful on-demand SCM collector that remains idle when the Services view is closed.
#[derive(Debug, Default)]
pub struct ServiceCollector {
    active: bool,
    force_poll: bool,
    last_poll: Option<Instant>,
    cached: DataState<Vec<ServiceEntry>>,
    start_type_cache: BTreeMap<String, String>,
    poll_count: u64,
}

impl ServiceCollector {
    pub fn new() -> Self {
        Self::default()
    }

    pub const fn is_active(&self) -> bool {
        self.active
    }

    pub const fn poll_count(&self) -> u64 {
        self.poll_count
    }

    /// Enables or disables SCM polling. Enabling schedules an immediate refresh on the next tick.
    pub fn set_active(&mut self, active: bool) {
        if active && !self.active {
            self.force_poll = true;
        }
        self.active = active;
    }

    /// Forces an immediate SCM re-poll on the next tick (e.g., right after Start or Stop).
    pub fn request_refresh(&mut self) {
        if self.active {
            self.force_poll = true;
        }
    }

    /// Returns cached service entries when inactive (without touching SCM) or polls SCM at
    /// [`SERVICE_POLL_INTERVAL`] when active.
    pub fn sample(&mut self) -> DataState<Vec<ServiceEntry>> {
        self.sample_with(Instant::now(), |cache| {
            enumerate_windows_services(cache).map(|rows| {
                rows.into_iter()
                    .map(|r| ServiceEntry {
                        name: r.name,
                        display_name: r.display_name,
                        state: r.state,
                        start_type: r.start_type,
                        pid: r.pid,
                    })
                    .collect()
            })
        })
    }

    /// Testable core polling gate enforcing inactive-idle and 2.5s cadence invariants.
    pub fn sample_with<F>(&mut self, now: Instant, mut poll_fn: F) -> DataState<Vec<ServiceEntry>>
    where
        F: FnMut(&mut BTreeMap<String, String>) -> Result<Vec<ServiceEntry>, String>,
    {
        if !self.active {
            return DataState::Unavailable("services view inactive".to_owned());
        }

        let due = self.force_poll
            || self
                .last_poll
                .is_none_or(|prev| now.saturating_duration_since(prev) >= SERVICE_POLL_INTERVAL);
        if due {
            self.force_poll = false;
            self.last_poll = Some(now);
            self.poll_count = self.poll_count.saturating_add(1);
            // Periodically clear start_type_cache every 12 polls (~30s) so external changes show up.
            if self.poll_count.is_multiple_of(12) {
                self.start_type_cache.clear();
            }
            self.cached = match poll_fn(&mut self.start_type_cache) {
                Ok(entries) => DataState::Available(entries),
                Err(reason) => DataState::Unavailable(reason),
            };
        }

        self.cached.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_collector_never_polls_when_inactive_and_throttles_when_active() {
        let mut collector = ServiceCollector::new();
        let t0 = Instant::now();
        let mut calls = 0u32;

        // Inactive: zero SCM calls.
        let inactive = collector.sample_with(t0, |_| {
            calls += 1;
            Ok(Vec::new())
        });
        assert!(matches!(inactive, DataState::Unavailable(_)));
        assert_eq!(calls, 0);

        // Activate -> polls immediately once.
        collector.set_active(true);
        let first = collector.sample_with(t0, |_| {
            calls += 1;
            Ok(vec![ServiceEntry {
                name: "W32Time".to_owned(),
                display_name: "Windows Time".to_owned(),
                state: "Running".to_owned(),
                start_type: "Auto".to_owned(),
                pid: Some(1204),
            }])
        });
        assert_eq!(calls, 1);
        assert!(matches!(first, DataState::Available(ref list) if list.len() == 1));

        // 1s later (< 2.5s): serves cached result without polling SCM again.
        let cached = collector.sample_with(t0 + Duration::from_secs(1), |_| {
            calls += 1;
            Ok(Vec::new())
        });
        assert_eq!(calls, 1);
        assert!(matches!(cached, DataState::Available(ref list) if list.len() == 1));

        // 3s later (>= 2.5s): polls SCM again.
        let refreshed = collector.sample_with(t0 + Duration::from_secs(3), |_| {
            calls += 1;
            Ok(Vec::new())
        });
        assert_eq!(calls, 2);
        assert!(matches!(refreshed, DataState::Available(ref list) if list.is_empty()));

        // Deactivate -> stops polling again.
        collector.set_active(false);
        let off = collector.sample_with(t0 + Duration::from_secs(10), |_| {
            calls += 1;
            Ok(Vec::new())
        });
        assert_eq!(calls, 2);
        assert!(matches!(off, DataState::Unavailable(_)));
    }
}
