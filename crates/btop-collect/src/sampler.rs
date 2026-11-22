//! Background sampler thread waking on a Windows waitable timer and publishing
//! immutable `Arc<Snapshot>` values through `arc-swap`.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use btop_model::{Snapshot, SnapshotRecorder};
use btop_sys::{TimerWake, TimerWaker, WaitableWakeTimer};

use crate::{CollectError, FreqMode, SERVICE_POLL_INTERVAL, SnapshotSource};

/// Minimum allowed sampling interval in milliseconds.
pub const MIN_UPDATE_MS: u32 = 100;

/// Maximum allowed sampling interval in milliseconds.
pub const MAX_UPDATE_MS: u32 = 60_000;

struct SamplerShared {
    latest: ArcSwap<Snapshot>,
    generation: AtomicU64,
    update_ms: AtomicU32,
    update_ms_dirty: AtomicBool,
    freq_mode: AtomicU8,
    freq_mode_dirty: AtomicBool,
    services_active: AtomicBool,
    services_dirty: AtomicBool,
    services_refresh: AtomicBool,
    stop: AtomicBool,
    finished: AtomicBool,
    waker: TimerWaker,
}

/// Handle to the running waitable-timer sampler thread.
pub struct Sampler {
    shared: Arc<SamplerShared>,
    handle: Option<JoinHandle<Result<Option<SnapshotRecorder>, CollectError>>>,
}

impl fmt::Debug for Sampler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sampler")
            .field("update_ms", &self.update_ms())
            .field("generation", &self.generation())
            .field("running", &self.handle.is_some())
            .finish_non_exhaustive()
    }
}

impl Sampler {
    /// Captures the initial snapshot synchronously, starts the waitable-timer background
    /// thread, and publishes every tick into an [`ArcSwap<Snapshot>`].
    pub fn spawn(
        source: Box<dyn SnapshotSource>,
        initial_update_ms: u32,
        recorder: Option<SnapshotRecorder>,
    ) -> Result<Self, CollectError> {
        Self::spawn_with_service_interval(
            source,
            initial_update_ms,
            recorder,
            SERVICE_POLL_INTERVAL,
        )
    }

    fn spawn_with_service_interval(
        mut source: Box<dyn SnapshotSource>,
        initial_update_ms: u32,
        recorder: Option<SnapshotRecorder>,
        service_interval: Duration,
    ) -> Result<Self, CollectError> {
        let initial_snapshot = source.next_snapshot()?;
        if let Some(rec) = recorder.as_ref() {
            rec.record(initial_snapshot.clone());
        }

        let (timer, waker) = WaitableWakeTimer::new().map_err(CollectError::Platform)?;
        let shared = Arc::new(SamplerShared {
            latest: ArcSwap::from_pointee(initial_snapshot),
            generation: AtomicU64::new(1),
            update_ms: AtomicU32::new(initial_update_ms.clamp(MIN_UPDATE_MS, MAX_UPDATE_MS)),
            update_ms_dirty: AtomicBool::new(false),
            freq_mode: AtomicU8::new(FreqMode::Average.to_u8()),
            freq_mode_dirty: AtomicBool::new(false),
            services_active: AtomicBool::new(false),
            services_dirty: AtomicBool::new(false),
            services_refresh: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            waker,
        });

        let thread_shared = Arc::clone(&shared);
        let handle = thread::Builder::new()
            .name("btop-sampler".to_owned())
            .spawn(move || {
                run_sampler_loop(source, recorder, &timer, &thread_shared, service_interval)
            })
            .map_err(|err| CollectError::Platform(format!("failed to spawn sampler: {err}")))?;

        Ok(Self {
            shared,
            handle: Some(handle),
        })
    }

    /// Loads the latest published immutable [`Snapshot`] lock-free via `ArcSwap`.
    pub fn latest(&self) -> Arc<Snapshot> {
        self.shared.latest.load_full()
    }

    /// Monotonically increasing tick counter incremented on every newly published snapshot.
    pub fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::Acquire)
    }

    /// Returns the currently configured update cadence in milliseconds.
    pub fn update_ms(&self) -> u32 {
        self.shared.update_ms.load(Ordering::Relaxed)
    }

    /// Updates the sampling cadence (clamped to `100..=60_000` ms) and immediately
    /// wakes the waitable timer to reschedule at the new interval.
    pub fn set_update_ms(&self, ms: u32) -> u32 {
        let clamped = ms.clamp(MIN_UPDATE_MS, MAX_UPDATE_MS);
        self.shared.update_ms.store(clamped, Ordering::Relaxed);
        self.shared.update_ms_dirty.store(true, Ordering::Release);
        self.shared.waker.wake();
        clamped
    }

    /// Propagates a new [`FreqMode`] to the background collector (e.g., on `Ctrl+R` reload).
    pub fn set_freq_mode(&self, mode: FreqMode) {
        self.shared.freq_mode.store(mode.to_u8(), Ordering::Relaxed);
        self.shared.freq_mode_dirty.store(true, Ordering::Release);
        self.shared.waker.wake();
    }

    /// Enables or disables on-demand SCM service polling; enabling wakes the sampler immediately
    /// so the Services view populates without waiting for the next timer interval.
    pub fn set_services_active(&self, active: bool) {
        self.shared.services_active.store(active, Ordering::Release);
        self.shared.services_dirty.store(true, Ordering::Release);
        if active {
            self.shared.services_refresh.store(true, Ordering::Release);
        }
        self.shared.waker.wake();
    }

    /// Requests an immediate SCM refresh on the sampler thread (e.g., right after Start or Stop).
    pub fn refresh_services(&self) {
        self.shared.services_refresh.store(true, Ordering::Release);
        self.shared.waker.wake();
    }

    /// Adjusts `update_ms` by `delta_ms` (e.g. `+100` or `-100`), clamping at [`MIN_UPDATE_MS`].
    pub fn adjust_update_ms(&self, delta_ms: i32) -> u32 {
        let current = i64::from(self.update_ms());
        let next = current.saturating_add(i64::from(delta_ms));
        let clamped = u32::try_from(next.clamp(i64::from(MIN_UPDATE_MS), i64::from(MAX_UPDATE_MS)))
            .unwrap_or(MIN_UPDATE_MS);
        self.set_update_ms(clamped)
    }

    /// Returns `true` if the sampler thread has stopped (e.g. finite replay EOF or error).
    pub fn is_finished(&self) -> bool {
        self.shared.finished.load(Ordering::Acquire)
    }

    /// Signals the sampler thread to exit, joins it cleanly, and returns the optional
    /// [`SnapshotRecorder`] so the caller can flush and check recording stats.
    pub fn stop(mut self) -> Result<Option<SnapshotRecorder>, CollectError> {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.waker.wake();
        if let Some(handle) = self.handle.take() {
            match handle.join() {
                Ok(res) => res,
                Err(_) => Err(CollectError::Platform("sampler thread panicked".to_owned())),
            }
        } else {
            Ok(None)
        }
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.waker.wake();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn run_sampler_loop(
    mut source: Box<dyn SnapshotSource>,
    recorder: Option<SnapshotRecorder>,
    timer: &WaitableWakeTimer,
    shared: &SamplerShared,
    service_interval: Duration,
) -> Result<Option<SnapshotRecorder>, CollectError> {
    let mut exit_result = Ok(());
    let mut main_interval =
        Duration::from_millis(u64::from(shared.update_ms.load(Ordering::Relaxed)));
    let mut next_main = Instant::now() + main_interval;
    let mut svc_active = false;
    let mut next_svc: Option<Instant> = None;

    while !shared.stop.load(Ordering::Acquire) {
        let now = Instant::now();
        if shared.freq_mode_dirty.swap(false, Ordering::AcqRel) {
            source.set_freq_mode(FreqMode::from_u8(shared.freq_mode.load(Ordering::Relaxed)));
        }
        if shared.update_ms_dirty.swap(false, Ordering::AcqRel) {
            main_interval =
                Duration::from_millis(u64::from(shared.update_ms.load(Ordering::Relaxed)));
            next_main = now + main_interval;
        }
        if shared.services_dirty.swap(false, Ordering::AcqRel) {
            svc_active = shared.services_active.load(Ordering::Acquire);
            source.set_services_active(svc_active);
            next_svc = if svc_active { Some(now) } else { None };
        }
        if shared.services_refresh.swap(false, Ordering::AcqRel) {
            source.request_services_refresh();
            if svc_active {
                next_svc = Some(now);
            }
        }

        let main_due = now >= next_main;
        let svc_due = svc_active && next_svc.is_some_and(|t| now >= t);

        if main_due {
            if svc_due {
                source.request_services_refresh();
            }
            match source.next_snapshot() {
                Ok(snapshot) => {
                    if let Some(rec) = recorder.as_ref() {
                        rec.record(snapshot.clone());
                    }
                    shared.latest.store(Arc::new(snapshot));
                    shared.generation.fetch_add(1, Ordering::Release);
                }
                Err(CollectError::EndOfStream) => break,
                Err(err) => {
                    exit_result = Err(err);
                    break;
                }
            }
            let after = Instant::now();
            next_main = after + main_interval;
            if svc_due {
                next_svc = Some(after + service_interval);
            }
            continue;
        }

        if svc_due {
            source.request_services_refresh();
            if let Some(services_state) = source.poll_services_only() {
                let mut updated = (**shared.latest.load()).clone();
                updated.services = services_state;
                shared.latest.store(Arc::new(updated));
                shared.generation.fetch_add(1, Ordering::Release);
            }
            next_svc = Some(Instant::now() + service_interval);
            continue;
        }

        let next_deadline = match next_svc {
            Some(svc_t) if svc_active => next_main.min(svc_t),
            _ => next_main,
        };
        let wait_for = next_deadline
            .saturating_duration_since(now)
            .max(Duration::from_millis(1));
        match timer.wait(wait_for) {
            Ok(TimerWake::Woken | TimerWake::Elapsed) => {}
            Err(err) => {
                exit_result = Err(CollectError::Platform(err));
                break;
            }
        }
    }

    shared.finished.store(true, Ordering::Release);
    exit_result?;
    Ok(recorder)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use btop_model::{DataState, ServiceEntry};

    use super::*;
    use crate::StubLiveSource;

    struct CountingServicesSource {
        active: bool,
        svc_polls: Arc<AtomicUsize>,
        main_polls: Arc<AtomicUsize>,
    }

    impl SnapshotSource for CountingServicesSource {
        fn next_snapshot(&mut self) -> Result<Snapshot, CollectError> {
            self.main_polls.fetch_add(1, Ordering::SeqCst);
            Ok(Snapshot::default())
        }

        fn set_services_active(&mut self, active: bool) {
            self.active = active;
        }

        fn poll_services_only(&mut self) -> Option<DataState<Vec<ServiceEntry>>> {
            if !self.active {
                return Some(DataState::Unavailable("inactive".to_owned()));
            }
            let n = self.svc_polls.fetch_add(1, Ordering::SeqCst) + 1;
            Some(DataState::Available(vec![ServiceEntry {
                name: format!("Svc{n}"),
                display_name: format!("Service {n}"),
                state: "Running".to_owned(),
                start_type: "Auto".to_owned(),
                pid: Some(1000),
            }]))
        }
    }

    #[test]
    fn sampler_publishes_via_arcswap_and_adjusts_cadence() {
        let source = Box::new(StubLiveSource::new(100));
        let sampler = Sampler::spawn(source, 100, None).expect("spawn sampler");
        assert_eq!(sampler.update_ms(), 100);
        assert_eq!(sampler.generation(), 1);

        // Wait briefly for at least one timer-driven tick.
        thread::sleep(Duration::from_millis(260));
        assert!(sampler.generation() >= 2, "expected sampler ticks");
        let snap = sampler.latest();
        assert_ne!(snap.hostname, "");

        // Adjust cadence up and clamp at floor 100ms.
        assert_eq!(sampler.adjust_update_ms(100), 200);
        assert_eq!(sampler.adjust_update_ms(-500), 100);

        let rec = sampler.stop().expect("stop sampler cleanly");
        assert!(rec.is_none());
    }

    #[test]
    fn sampler_polls_services_on_independent_cadence_even_with_60s_update_ms() {
        let svc_polls = Arc::new(AtomicUsize::new(0));
        let main_polls = Arc::new(AtomicUsize::new(0));
        let source = Box::new(CountingServicesSource {
            active: false,
            svc_polls: Arc::clone(&svc_polls),
            main_polls: Arc::clone(&main_polls),
        });
        let sampler = Sampler::spawn_with_service_interval(
            source,
            MAX_UPDATE_MS,
            None,
            Duration::from_millis(60),
        )
        .expect("spawn sampler");

        // Inactive: zero service polls even after >2 service intervals.
        thread::sleep(Duration::from_millis(150));
        assert_eq!(svc_polls.load(Ordering::SeqCst), 0);
        assert_eq!(main_polls.load(Ordering::SeqCst), 1);

        // Activate Services: polls independently at ~60ms without waiting 60_000ms.
        sampler.set_services_active(true);
        thread::sleep(Duration::from_millis(220));
        let active_polls = svc_polls.load(Ordering::SeqCst);
        assert!(
            active_polls >= 2,
            "expected independent service polling >= 2 times under 60s update_ms, got {active_polls}"
        );
        assert_eq!(
            main_polls.load(Ordering::SeqCst),
            1,
            "service-only ticks must not run extra full telemetry snapshots"
        );
        assert!(matches!(
            sampler.latest().services,
            DataState::Available(ref list) if !list.is_empty()
        ));

        // Deactivate Services: stops service polls completely.
        sampler.set_services_active(false);
        thread::sleep(Duration::from_millis(50));
        let after_disable = svc_polls.load(Ordering::SeqCst);
        thread::sleep(Duration::from_millis(160));
        assert_eq!(svc_polls.load(Ordering::SeqCst), after_disable);

        let _ = sampler.stop().expect("stop sampler cleanly");
    }
}
