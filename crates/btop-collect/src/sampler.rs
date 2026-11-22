//! Background sampler thread waking on a Windows waitable timer and publishing
//! immutable `Arc<Snapshot>` values through `arc-swap`.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use arc_swap::ArcSwap;
use btop_model::{Snapshot, SnapshotRecorder};
use btop_sys::{TimerWake, TimerWaker, WaitableWakeTimer};

use crate::{CollectError, FreqMode, SnapshotSource};

/// Minimum allowed sampling interval in milliseconds.
pub const MIN_UPDATE_MS: u32 = 100;

/// Maximum allowed sampling interval in milliseconds.
pub const MAX_UPDATE_MS: u32 = 60_000;

struct SamplerShared {
    latest: ArcSwap<Snapshot>,
    generation: AtomicU64,
    update_ms: AtomicU32,
    freq_mode: AtomicU8,
    freq_mode_dirty: AtomicBool,
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
        mut source: Box<dyn SnapshotSource>,
        initial_update_ms: u32,
        recorder: Option<SnapshotRecorder>,
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
            freq_mode: AtomicU8::new(FreqMode::Average.to_u8()),
            freq_mode_dirty: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            waker,
        });

        let thread_shared = Arc::clone(&shared);
        let handle = thread::Builder::new()
            .name("btop-sampler".to_owned())
            .spawn(move || run_sampler_loop(source, recorder, &timer, &thread_shared))
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
        self.shared.waker.wake();
        clamped
    }

    /// Propagates a new [`FreqMode`] to the background collector (e.g., on `Ctrl+R` reload).
    pub fn set_freq_mode(&self, mode: FreqMode) {
        self.shared.freq_mode.store(mode.to_u8(), Ordering::Relaxed);
        self.shared.freq_mode_dirty.store(true, Ordering::Release);
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
) -> Result<Option<SnapshotRecorder>, CollectError> {
    let mut exit_result = Ok(());

    while !shared.stop.load(Ordering::Acquire) {
        if shared.freq_mode_dirty.swap(false, Ordering::AcqRel) {
            source.set_freq_mode(FreqMode::from_u8(shared.freq_mode.load(Ordering::Relaxed)));
        }
        let interval = Duration::from_millis(u64::from(shared.update_ms.load(Ordering::Relaxed)));
        match timer.wait(interval) {
            Ok(TimerWake::Woken) => {
                // Either shutdown requested, cadence changed, or freq_mode changed.
                continue;
            }
            Ok(TimerWake::Elapsed) => {}
            Err(err) => {
                exit_result = Err(CollectError::Platform(err));
                break;
            }
        }

        if shared.stop.load(Ordering::Acquire) {
            break;
        }
        if shared.freq_mode_dirty.swap(false, Ordering::AcqRel) {
            source.set_freq_mode(FreqMode::from_u8(shared.freq_mode.load(Ordering::Relaxed)));
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
    }

    shared.finished.store(true, Ordering::Release);
    exit_result?;
    Ok(recorder)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StubLiveSource;

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
}
