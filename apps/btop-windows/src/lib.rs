//! Runtime session loop shared by the `btop-windows` binary and integration tests.

use std::path::{Path, PathBuf};
use std::time::Duration;

use btop_collect::{CollectError, FreqMode, Sampler, SnapshotSource};
use btop_config::{Config, ConfigError};
use btop_model::{DataState, RecordStats, SnapshotRecorder};
use btop_term::{
    DiffEncoder, Grid, InputSource, KeyCode, Size, TermError, TermEvent, TerminalGuard,
};
use btop_ui::{UiHistory, UiState, visible_proc_rows};
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
        history.push_with_util_mode(&snapshot, is_utility_mode(config));
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
    config.update_ms = sampler.update_ms();
    let mut guard = TerminalGuard::enter()?;

    let mut size = guard.size().unwrap_or(Size {
        width: 80,
        height: 24,
    });
    let mut history = UiHistory::default();
    let mut ui_state = UiState::from_config(&config);
    let mut last_generation = sampler.generation();
    let mut current_snapshot = sampler.latest();
    history.push_with_util_mode(&current_snapshot, is_utility_mode(&config));

    let mut prev_grid: Option<Grid> = None;
    let mut dirty = true;
    let mut frames_rendered = 0u64;

    let run_result: Result<(), AppError> = (|| {
        loop {
            let gen_now = sampler.generation();
            if gen_now != last_generation {
                last_generation = gen_now;
                current_snapshot = sampler.latest();
                history.push_with_util_mode(&current_snapshot, is_utility_mode(&config));
                dirty = true;
            }

            let proc_count = match &current_snapshot.processes {
                DataState::Available(list) | DataState::Stale { value: list, .. } => list.len(),
                _ => 0,
            };
            let proc_rows = visible_proc_rows(size, &config);
            ui_state.proc.clamp(proc_count, proc_rows);

            if dirty {
                let encoder = DiffEncoder {
                    sync_output: config.terminal_sync,
                };
                let next_grid =
                    btop_ui::render_frame(&current_snapshot, &history, &ui_state, &config, size);
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
                Some(event) => {
                    if handle_tui_event(
                        event,
                        &sampler,
                        &guard,
                        &mut size,
                        &mut config,
                        config_path,
                        overrides,
                        &mut ui_state,
                        proc_count,
                        proc_rows,
                        &mut prev_grid,
                    ) {
                        dirty = true;
                    }
                }
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

fn is_utility_mode(config: &Config) -> bool {
    config.cpu_util_mode.eq_ignore_ascii_case("utility")
}

#[allow(clippy::too_many_arguments)]
fn handle_tui_event(
    event: TermEvent,
    sampler: &Sampler,
    guard: &TerminalGuard,
    size: &mut Size,
    config: &mut Config,
    config_path: Option<&Path>,
    overrides: &ConfigOverrides,
    ui_state: &mut UiState,
    proc_count: usize,
    proc_rows: usize,
    prev_grid: &mut Option<Grid>,
) -> bool {
    match event {
        TermEvent::Key(k) if k.ctrl && matches!(k.code, KeyCode::Char('r' | 'R')) => {
            if let Ok(reloaded) = load_config(config_path, overrides) {
                *config = reloaded;
                config.update_ms = sampler.set_update_ms(config.update_ms);
                sampler.set_freq_mode(FreqMode::from_config_str(&config.freq_mode));
                *ui_state = UiState::from_config(config);
                *prev_grid = None;
                return true;
            }
            false
        }
        TermEvent::Key(k) if matches!(k.code, KeyCode::Char('+' | '=')) => {
            config.update_ms = sampler.adjust_update_ms(100);
            true
        }
        TermEvent::Key(k) if matches!(k.code, KeyCode::Char('-' | '_')) => {
            config.update_ms = sampler.adjust_update_ms(-100);
            true
        }
        TermEvent::Key(k) if !k.ctrl && !k.alt && k.code == KeyCode::Char('d') => {
            ui_state.toggle_disks();
            true
        }
        TermEvent::Key(k) if !k.ctrl && !k.alt && k.code == KeyCode::Char('i') => {
            ui_state.toggle_io_mode();
            true
        }
        TermEvent::Key(k) => handle_proc_key(k.code, ui_state, proc_count, proc_rows),
        TermEvent::Resize(new_size) => {
            *size = guard.size().unwrap_or(new_size);
            *prev_grid = None;
            true
        }
        TermEvent::Mouse(_) => false,
    }
}

fn handle_proc_key(
    code: KeyCode,
    state: &mut UiState,
    proc_count: usize,
    proc_rows: usize,
) -> bool {
    match code {
        KeyCode::Up => state.proc.move_up(proc_count, proc_rows),
        KeyCode::Down => state.proc.move_down(proc_count, proc_rows),
        KeyCode::PageUp => state.proc.page_up(proc_count, proc_rows),
        KeyCode::PageDown => state.proc.page_down(proc_count, proc_rows),
        KeyCode::Home => state.proc.home(proc_count, proc_rows),
        KeyCode::End => state.proc.end(proc_count, proc_rows),
        KeyCode::Left => state.proc.prev_sort(),
        KeyCode::Right => state.proc.next_sort(),
        KeyCode::Char('r') => state.proc.toggle_reverse(),
        KeyCode::Char('c') => state.proc.toggle_per_core(),
        KeyCode::Char('%') => state.proc.toggle_mem_bytes(),
        _ => return false,
    }
    true
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
