//! Runtime session loop shared by the `btop-windows` binary and integration tests.

use std::time::{Duration, Instant};

use btop_collect::{CollectError, SnapshotSource};
use btop_config::Config;
use btop_model::{RecordStats, SnapshotRecorder};
use btop_term::{
    DiffEncoder, Grid, InputSource, KeyCode, Size, TermError, TermEvent, TerminalGuard,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error(transparent)]
    Collect(#[from] CollectError),
    #[error(transparent)]
    Term(#[from] TermError),
    #[error(transparent)]
    Jsonl(#[from] btop_model::JsonlError),
    #[error(
        "recording incomplete: wrote {written} snapshots and dropped {dropped} due to slow disk I/O"
    )]
    IncompleteRecording { written: u64, dropped: u64 },
}

/// Validates recorder completion statistics and fails clearly if any snapshots were dropped.
pub fn check_record_stats(stats: RecordStats) -> Result<RecordStats, AppError> {
    if stats.dropped > 0 {
        return Err(AppError::IncompleteRecording {
            written: stats.written,
            dropped: stats.dropped,
        });
    }
    Ok(stats)
}

/// Runs a deterministic headless render session for `max_ticks` snapshots, optionally
/// recording each snapshot via `recorder`, and returns every rendered [`Grid`].
pub fn run_headless_frames(
    source: &mut dyn SnapshotSource,
    recorder: Option<&SnapshotRecorder>,
    config: &Config,
    size: Size,
    max_ticks: usize,
) -> Result<Vec<Grid>, AppError> {
    let mut frames = Vec::with_capacity(max_ticks);
    for _ in 0..max_ticks {
        let snapshot = match source.next_snapshot() {
            Ok(s) => s,
            Err(CollectError::EndOfStream) => break,
            Err(err) => return Err(AppError::Collect(err)),
        };
        if let Some(rec) = recorder {
            rec.record(snapshot.clone());
        }
        frames.push(btop_ui::render(&snapshot, config, size));
    }
    Ok(frames)
}

/// Runs the interactive TUI loop in Windows Terminal at `config.update_ms`, exiting cleanly
/// on `Ctrl+C` or `q` and flushing any active [`SnapshotRecorder`].
pub fn run_tui_loop(
    source: &mut dyn SnapshotSource,
    recorder: Option<SnapshotRecorder>,
    input: &mut dyn InputSource,
    config: &Config,
) -> Result<u64, AppError> {
    let mut guard = TerminalGuard::enter()?;
    let encoder = DiffEncoder {
        sync_output: config.terminal_sync,
    };
    let tick_interval = Duration::from_millis(u64::from(config.update_ms.max(100)));

    let mut size = guard.size().unwrap_or(Size {
        width: 80,
        height: 24,
    });
    let mut prev_grid: Option<Grid> = None;
    let mut frames_rendered = 0u64;

    let run_result: Result<(), AppError> = (|| {
        let mut current_snapshot = source.next_snapshot()?;
        if let Some(rec) = recorder.as_ref() {
            rec.record(current_snapshot.clone());
        }
        let mut next_tick = Instant::now() + tick_interval;

        loop {
            let next_grid = btop_ui::render(&current_snapshot, config, size);
            let payload = encoder.encode(prev_grid.as_ref(), &next_grid);
            guard.write_frame(&payload)?;
            prev_grid = Some(next_grid);
            frames_rendered += 1;

            let now = Instant::now();
            let timeout = next_tick
                .saturating_duration_since(now)
                .min(Duration::from_millis(100));

            match input.poll(timeout)? {
                Some(TermEvent::Key(k)) if k.is_ctrl_c() || k.code == KeyCode::Char('q') => {
                    break;
                }
                Some(TermEvent::Resize(new_size)) => {
                    size = guard.size().unwrap_or(new_size);
                    prev_grid = None;
                    continue;
                }
                Some(TermEvent::Key(_) | TermEvent::Mouse(_)) => {}
                None => {
                    if let Ok(live) = guard.size()
                        && live != size
                    {
                        size = live;
                        prev_grid = None;
                        continue;
                    }
                }
            }

            if Instant::now() >= next_tick {
                current_snapshot = match source.next_snapshot() {
                    Ok(snap) => snap,
                    Err(CollectError::EndOfStream) => break,
                    Err(err) => return Err(AppError::Collect(err)),
                };
                if let Some(rec) = recorder.as_ref() {
                    rec.record(current_snapshot.clone());
                }
                next_tick = Instant::now() + tick_interval;
            }
        }
        Ok(())
    })();

    drop(guard);

    let finish_result = recorder.map(SnapshotRecorder::finish).transpose();
    run_result?;
    if let Some(stats) = finish_result? {
        check_record_stats(stats)?;
    }

    Ok(frames_rendered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_record_stats_fails_when_snapshots_were_dropped() {
        assert!(
            check_record_stats(RecordStats {
                written: 5,
                dropped: 0,
            })
            .is_ok()
        );

        let err = check_record_stats(RecordStats {
            written: 64,
            dropped: 3,
        })
        .expect_err("dropped snapshots must fail");
        assert!(matches!(
            err,
            AppError::IncompleteRecording {
                written: 64,
                dropped: 3
            }
        ));
    }
}
