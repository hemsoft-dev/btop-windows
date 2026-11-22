//! Runtime session loop shared by the `btop-windows` binary and integration tests.

use std::path::{Path, PathBuf};
use std::time::Duration;

use btop_collect::{CollectError, FreqMode, Sampler, SnapshotSource};
use btop_config::{Config, ConfigError};
use btop_model::{DataState, RecordStats, SnapshotRecorder};
use btop_term::{
    DiffEncoder, Grid, InputSource, KeyCode, Size, TermError, TermEvent, TerminalGuard,
};
use btop_ui::{BoxId, UiHistory, UiState, build_visible_rows, visible_proc_rows};
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
    pub preset: Option<u8>,
    pub filter: Option<String>,
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

fn sync_proc_selection(snapshot: &btop_model::Snapshot, ui_state: &mut UiState, proc_rows: usize) {
    if let Some(active_procs) = ui_state.proc.active_processes(snapshot) {
        let (built_rows, _) = build_visible_rows(active_procs, &ui_state.proc);
        let visible_count = built_rows.len();
        let target_idx = ui_state
            .proc
            .followed
            .and_then(|target| built_rows.iter().position(|r| r.entry.identity == target));
        if let Some(idx) = target_idx {
            ui_state.proc.selected = idx;
        }
        ui_state.proc.clamp(visible_count, proc_rows);
    } else {
        ui_state.proc.clamp(0, proc_rows);
    }
}

fn build_ui_state(config: &Config, overrides: &ConfigOverrides) -> UiState {
    let mut ui_state = UiState::from_config(config);
    ui_state.tz_offset_secs = btop_sys::local_utc_offset_secs();
    if let Some(preset) = overrides.preset {
        let _ = ui_state.layout.apply_preset(preset);
    }
    if let Some(ref filter) = overrides.filter {
        ui_state.proc.set_filter(filter.clone());
    }
    ui_state
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
    let mut ui_state = build_ui_state(&config, overrides);
    let mut last_generation = sampler.generation();
    let mut current_snapshot = sampler.latest();
    history.push_with_util_mode(&current_snapshot, is_utility_mode(&config));
    ui_state.net.observe_snapshot(&current_snapshot);

    let mut prev_grid: Option<Grid> = None;
    let mut dirty = true;
    let mut frames_rendered = 0u64;

    let run_result: Result<(), AppError> = (|| {
        loop {
            let gen_now = sampler.generation();
            if gen_now != last_generation {
                last_generation = gen_now;
                current_snapshot = sampler.latest();
                ui_state.tz_offset_secs = btop_sys::local_utc_offset_secs();
                history.push_with_util_mode(&current_snapshot, is_utility_mode(&config));
                ui_state.net.observe_snapshot(&current_snapshot);
                dirty = true;
            }

            let proc_rows = visible_proc_rows(size, &ui_state);
            sync_proc_selection(&current_snapshot, &mut ui_state, proc_rows);

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
                Some(TermEvent::Key(k))
                    if k.is_ctrl_c()
                        || (!ui_state.proc.filter_editing && k.code == KeyCode::Char('q')) =>
                {
                    break;
                }
                Some(event) => {
                    if handle_tui_event(
                        event,
                        &sampler,
                        &guard,
                        &current_snapshot,
                        &mut history,
                        &mut size,
                        &mut config,
                        config_path,
                        overrides,
                        &mut ui_state,
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
    snapshot: &btop_model::Snapshot,
    history: &mut UiHistory,
    size: &mut Size,
    config: &mut Config,
    config_path: Option<&Path>,
    overrides: &ConfigOverrides,
    ui_state: &mut UiState,
    proc_rows: usize,
    prev_grid: &mut Option<Grid>,
) -> bool {
    match event {
        TermEvent::Key(k) if ui_state.proc.filter_editing => {
            handle_filter_edit_key(k.code, k.ctrl, k.alt, ui_state)
        }
        TermEvent::Key(k) if k.ctrl && matches!(k.code, KeyCode::Char('r' | 'R')) => {
            if let Ok(reloaded) = load_config(config_path, overrides) {
                *config = reloaded;
                config.update_ms = sampler.set_update_ms(config.update_ms);
                sampler.set_freq_mode(FreqMode::from_config_str(&config.freq_mode));
                *ui_state = build_ui_state(config, overrides);
                ui_state.net.observe_snapshot(snapshot);
                *prev_grid = None;
                return true;
            }
            false
        }
        TermEvent::Key(k) if !k.ctrl && !k.alt => {
            if handle_layout_key(k.code, ui_state, prev_grid) {
                return true;
            }
            handle_panel_key(
                k.code, sampler, snapshot, history, config, ui_state, proc_rows,
            )
        }
        TermEvent::Key(k) => handle_proc_key(k.code, snapshot, history, ui_state, proc_rows),
        TermEvent::Resize(new_size) => {
            *size = guard.size().unwrap_or(new_size);
            *prev_grid = None;
            true
        }
        TermEvent::Mouse(_) => false,
    }
}

fn handle_filter_edit_key(code: KeyCode, ctrl: bool, alt: bool, ui_state: &mut UiState) -> bool {
    if ctrl || alt {
        return false;
    }
    match code {
        KeyCode::Enter | KeyCode::Esc => {
            ui_state.proc.end_filter_edit();
            true
        }
        KeyCode::Backspace => {
            ui_state.proc.pop_filter_char();
            true
        }
        KeyCode::Delete => ui_state.proc.clear_filter(),
        KeyCode::Char(ch) => {
            ui_state.proc.push_filter_char(ch);
            true
        }
        _ => false,
    }
}

fn handle_layout_key(code: KeyCode, ui_state: &mut UiState, prev_grid: &mut Option<Grid>) -> bool {
    let changed = match code {
        KeyCode::Char('p') => ui_state.layout.cycle_preset_next(),
        KeyCode::Char('P') => ui_state.layout.cycle_preset_prev(),
        KeyCode::Char('1') => ui_state.layout.toggle_box(BoxId::Cpu),
        KeyCode::Char('2') => ui_state.layout.toggle_box(BoxId::Mem),
        KeyCode::Char('3') => ui_state.layout.toggle_box(BoxId::Net),
        KeyCode::Char('4') => ui_state.layout.toggle_box(BoxId::Proc),
        _ => false,
    };
    if changed {
        *prev_grid = None;
    }
    changed
}

fn handle_panel_key(
    code: KeyCode,
    sampler: &Sampler,
    snapshot: &btop_model::Snapshot,
    history: &mut UiHistory,
    config: &mut Config,
    ui_state: &mut UiState,
    proc_rows: usize,
) -> bool {
    match code {
        KeyCode::Char('+') => {
            if let Some(procs) = ui_state.proc.active_processes(snapshot).map(<[_]>::to_vec) {
                let (rows, _) = build_visible_rows(&procs, &ui_state.proc);
                if ui_state.proc.expand_selected(&rows) {
                    return true;
                }
            }
            config.update_ms = sampler.adjust_update_ms(100);
            true
        }
        KeyCode::Char('-') => {
            if let Some(procs) = ui_state.proc.active_processes(snapshot).map(<[_]>::to_vec) {
                let (rows, _) = build_visible_rows(&procs, &ui_state.proc);
                if ui_state.proc.collapse_selected(&rows) {
                    return true;
                }
            }
            config.update_ms = sampler.adjust_update_ms(-100);
            true
        }
        KeyCode::Char('=') => {
            config.update_ms = sampler.adjust_update_ms(100);
            true
        }
        KeyCode::Char('_') => {
            config.update_ms = sampler.adjust_update_ms(-100);
            true
        }
        KeyCode::Char('d') => {
            ui_state.toggle_disks();
            true
        }
        KeyCode::Char('i') => {
            ui_state.toggle_io_mode();
            true
        }
        KeyCode::Char('b') => {
            ui_state.net.select_prev(snapshot);
            true
        }
        KeyCode::Char('n') => {
            ui_state.net.select_next(snapshot);
            true
        }
        KeyCode::Char('a') => {
            ui_state.net.toggle_auto();
            true
        }
        KeyCode::Char('y') => {
            ui_state.net.toggle_sync();
            true
        }
        KeyCode::Char('z') => {
            ui_state.net.reset_session_totals(snapshot, history);
            true
        }
        other => handle_proc_key(other, snapshot, history, ui_state, proc_rows),
    }
}

fn handle_proc_key(
    code: KeyCode,
    snapshot: &btop_model::Snapshot,
    history: &UiHistory,
    state: &mut UiState,
    proc_rows: usize,
) -> bool {
    let active_owned = state.proc.active_processes(snapshot).map(<[_]>::to_vec);
    let rows_built = active_owned
        .as_deref()
        .map(|procs| build_visible_rows(procs, &state.proc).0);
    let proc_count = rows_built.as_ref().map_or(0, Vec::len);

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
        KeyCode::Char('e') => state.proc.toggle_tree(),
        KeyCode::Char(' ') => {
            return rows_built
                .as_deref()
                .is_some_and(|rows| state.proc.toggle_collapse_selected(rows));
        }
        KeyCode::Char('E') => {
            return active_owned
                .as_deref()
                .is_some_and(|procs| state.proc.toggle_collapse_all(procs));
        }
        KeyCode::Char('f' | '/') => state.proc.begin_filter_edit(),
        KeyCode::Delete => return state.proc.clear_filter(),
        KeyCode::Char('u') => {
            let live = match &snapshot.processes {
                DataState::Available(list) | DataState::Stale { value: list, .. } => {
                    Some(list.as_slice())
                }
                _ => None,
            };
            state.proc.toggle_pause(live, Some(history));
        }
        KeyCode::Char('F') => {
            return rows_built
                .as_deref()
                .is_some_and(|rows| state.proc.toggle_follow(rows));
        }
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
            preset: Some(2),
            filter: Some("!rust".to_owned()),
            force_tty: false,
            low_color: false,
        };
        let cfg = load_config(Some(&path), &overrides).expect("load config");
        let _ = std::fs::remove_file(&path);

        assert_eq!(cfg.update_ms, 500);
        assert!(!cfg.rounded_corners);
    }
}
