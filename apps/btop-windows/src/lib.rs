//! Runtime session loop shared by the `btop-windows` binary and integration tests.

use std::path::{Path, PathBuf};
use std::time::Duration;

use btop_collect::{CollectError, Sampler, SnapshotSource};
use btop_config::{Config, ConfigError};
use btop_model::{RecordStats, SnapshotRecorder};
use btop_term::{
    DiffEncoder, Grid, InputSource, KeyCode, Size, TermError, TermEvent, TerminalGuard,
};
use btop_ui::UiHistory;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error(transparent)]
    Collect(#[from] CollectError),
    #[error(transparent)]
    Term(#[from] TermError),
    #[error(transparent)]
    Jsonl(#[from] btop_model::JsonlError),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error(
        "recording incomplete: wrote {written} snapshots and dropped {dropped} due to slow disk I/O"
    )]
    IncompleteRecording { written: u64, dropped: u64 },
}

/// CLI overrides applied on top of loaded `btop.conf` at startup and on `Ctrl+R` reload.
#[derive(Debug, Clone, Default)]
pub struct ConfigOverrides {
    pub update_ms: Option<u32>,
    pub force_tty: bool,
    pub low_color: bool,
}

/// Loads `btop.conf` from an explicit path (erroring if missing/invalid) or from `./btop.conf`
/// when present, then applies CLI overrides.
pub fn load_config(
    explicit_path: Option<&Path>,
    overrides: &ConfigOverrides,
) -> Result<Config, AppError> {
    let mut cfg = if let Some(path) = explicit_path {
        let content = std::fs::read_to_string(path)?;
        Config::parse(&content)?
    } else {
        let default_path = PathBuf::from("btop.conf");
        if default_path.exists() {
            let content = std::fs::read_to_string(&default_path)?;
            Config::parse(&content)?
        } else {
            Config::default()
        }
    };

    if let Some(ms) = overrides.update_ms {
        cfg.update_ms = ms.max(100);
    }
    if overrides.force_tty {
        cfg.force_tty = true;
    }
    if overrides.low_color {
        cfg.truecolor = false;
    }
    Ok(cfg)
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
    let mut history = UiHistory::default();
    for _ in 0..max_ticks {
        let snapshot = match source.next_snapshot() {
            Ok(s) => s,
            Err(CollectError::EndOfStream) => break,
            Err(err) => return Err(AppError::Collect(err)),
        };
        if let Some(rec) = recorder {
            rec.record(snapshot.clone());
        }
        history.push(&snapshot);
        frames.push(btop_ui::render_with_history(
            &snapshot, &history, config, size,
        ));
    }
    Ok(frames)
}

/// Runs the live Windows Terminal TUI loop backed by a waitable-timer [`Sampler`] and
/// `ArcSwap<Snapshot>`, supporting `+`/`-` cadence control, `Ctrl+R` config reload, and
/// clean exit on `q`/`Ctrl+C`.
pub fn run_tui_loop(
    source: Box<dyn SnapshotSource>,
    recorder: Option<SnapshotRecorder>,
    input: &mut dyn InputSource,
    mut config: Config,
    config_path: Option<&Path>,
    overrides: &ConfigOverrides,
) -> Result<u64, AppError> {
    let sampler = Sampler::spawn(source, config.update_ms, recorder)?;
    let mut guard = TerminalGuard::enter()?;

    let mut size = guard.size().unwrap_or(Size {
        width: 80,
        height: 24,
    });
    let mut history = UiHistory::default();
    let mut last_generation = sampler.generation();
    let mut current_snapshot = sampler.latest();
    history.push(&current_snapshot);

    let mut prev_grid: Option<Grid> = None;
    let mut dirty = true;
    let mut frames_rendered = 0u64;

    let run_result: Result<(), AppError> = (|| {
        loop {
            let gen_now = sampler.generation();
            if gen_now != last_generation {
                last_generation = gen_now;
                current_snapshot = sampler.latest();
                history.push(&current_snapshot);
                dirty = true;
            }

            if dirty {
                let encoder = DiffEncoder {
                    sync_output: config.terminal_sync,
                };
                let next_grid =
                    btop_ui::render_with_history(&current_snapshot, &history, &config, size);
                let payload = encoder.encode(prev_grid.as_ref(), &next_grid);
                guard.write_frame(&payload)?;
                prev_grid = Some(next_grid);
                frames_rendered += 1;
                dirty = false;
            }

            if sampler.is_finished() {
                break;
            }

            match input.poll(Duration::from_millis(50))? {
                Some(TermEvent::Key(k)) if k.is_ctrl_c() || k.code == KeyCode::Char('q') => {
                    break;
                }
                Some(TermEvent::Key(k)) if k.ctrl && matches!(k.code, KeyCode::Char('r' | 'R')) => {
                    if let Ok(reloaded) = load_config(config_path, overrides) {
                        config = reloaded;
                        config.update_ms = sampler.set_update_ms(config.update_ms);
                        prev_grid = None;
                        dirty = true;
                    }
                }
                Some(TermEvent::Key(k)) if matches!(k.code, KeyCode::Char('+' | '=')) => {
                    config.update_ms = sampler.adjust_update_ms(100);
                    dirty = true;
                }
                Some(TermEvent::Key(k)) if matches!(k.code, KeyCode::Char('-' | '_')) => {
                    config.update_ms = sampler.adjust_update_ms(-100);
                    dirty = true;
                }
                Some(TermEvent::Resize(new_size)) => {
                    size = guard.size().unwrap_or(new_size);
                    prev_grid = None;
                    dirty = true;
                }
                Some(TermEvent::Key(_) | TermEvent::Mouse(_)) => {}
                None => {
                    if let Ok(live) = guard.size()
                        && live != size
                    {
                        size = live;
                        prev_grid = None;
                        dirty = true;
                    }
                }
            }
        }
        Ok(())
    })();

    drop(guard);

    let stop_result = sampler.stop();
    let finish_result = match stop_result {
        Ok(Some(rec)) => Some(rec.finish()),
        Ok(None) => None,
        Err(err) => {
            run_result?;
            return Err(AppError::Collect(err));
        }
    };

    run_result?;
    if let Some(res) = finish_result {
        let stats = res?;
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

    #[test]
    fn load_config_reads_file_and_applies_cli_overrides() {
        let unique = format!(
            "btop-windows-cfg-{}-{}.conf",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        );
        let path = std::env::temp_dir().join(unique);
        std::fs::write(&path, "update_ms = 1500\nrounded_corners = False\n")
            .expect("write temp config");

        let overrides = ConfigOverrides {
            update_ms: Some(500),
            force_tty: false,
            low_color: false,
        };
        let cfg = load_config(Some(&path), &overrides).expect("load config");
        let _ = std::fs::remove_file(&path);

        assert_eq!(cfg.update_ms, 500);
        assert!(!cfg.rounded_corners);
    }
}
