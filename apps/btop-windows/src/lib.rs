//! Runtime session loop shared by the `btop-windows` binary and integration tests.

use std::path::{Path, PathBuf};
use std::time::Duration;

use btop_collect::{CollectError, FreqMode, Sampler, SnapshotSource};
use btop_config::{Config, ConfigError};
use btop_model::{DataState, RecordStats, SnapshotRecorder};
use btop_sys::{
    PriorityClass, graceful_close_process, query_process_priority, set_process_priority,
    terminate_process,
};
use btop_term::{
    DiffEncoder, Grid, InputSource, KeyCode, Size, TermError, TermEvent, TerminalGuard,
};
use btop_ui::{
    BoxId, ProcActionModal, UiHistory, UiPriorityClass, UiState, build_visible_rows,
    is_modal_visible, modal_rect, visible_proc_rows,
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
    if let Some(active_procs) = ui_state.proc.active_processes(snapshot).map(<[_]>::to_vec) {
        let (built_rows, _) = build_visible_rows(&active_procs, &ui_state.proc);
        ui_state
            .proc
            .sync_detail_and_follow(&active_procs, &built_rows, proc_rows);
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
            if cancel_invisible_modal(size, &mut ui_state, &mut prev_grid) {
                dirty = true;
            }

            if dirty {
                render_dirty_frame(
                    &mut guard,
                    &current_snapshot,
                    &history,
                    &ui_state,
                    &config,
                    size,
                    &mut prev_grid,
                )?;
                frames_rendered += 1;
                dirty = false;
            }

            if sampler.is_finished() {
                break;
            }

            match input.poll(Duration::from_millis(50))? {
                Some(TermEvent::Key(k))
                    if k.is_ctrl_c()
                        || (!ui_state.proc.filter_editing
                            && ui_state.modal.is_none()
                            && k.code == KeyCode::Char('q')) =>
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
    finish_sampler_and_recording(sampler, run_result)?;
    Ok(frames_rendered)
}

fn finish_sampler_and_recording(
    sampler: Sampler,
    run_result: Result<(), AppError>,
) -> Result<(), AppError> {
    let finish_result = match sampler.stop() {
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
    Ok(())
}

fn render_dirty_frame(
    guard: &mut TerminalGuard,
    snapshot: &btop_model::Snapshot,
    history: &UiHistory,
    ui_state: &UiState,
    config: &Config,
    size: Size,
    prev_grid: &mut Option<Grid>,
) -> Result<(), AppError> {
    let encoder = DiffEncoder {
        sync_output: config.terminal_sync,
    };
    let next_grid = btop_ui::render_frame(snapshot, history, ui_state, config, size);
    let payload = encoder.encode(prev_grid.as_ref(), &next_grid);
    guard.write_frame(&payload)?;
    *prev_grid = Some(next_grid);
    Ok(())
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
        TermEvent::Key(k) if ui_state.modal.is_some() => {
            handle_modal_key(k.code, k.ctrl, k.alt, *size, ui_state, prev_grid)
        }
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
                k.code, sampler, snapshot, history, config, *size, ui_state, proc_rows,
            )
        }
        TermEvent::Key(k) => handle_proc_key(k.code, snapshot, history, *size, ui_state, proc_rows),
        TermEvent::Resize(new_size) => {
            *size = guard.size().unwrap_or(new_size);
            let _ = cancel_invisible_modal(*size, ui_state, prev_grid);
            *prev_grid = None;
            true
        }
        TermEvent::Mouse(_) => false,
    }
}

fn cancel_invisible_modal(
    size: Size,
    ui_state: &mut UiState,
    prev_grid: &mut Option<Grid>,
) -> bool {
    if ui_state.modal.is_some() && !is_modal_visible(size, ui_state) {
        ui_state.modal = None;
        *prev_grid = None;
        return true;
    }
    false
}

fn try_open_modal(size: Size, ui_state: &mut UiState, modal: ProcActionModal) -> bool {
    if modal_rect(size, &modal).is_some() {
        let mut probe = ui_state.clone();
        probe.modal = Some(modal.clone());
        if is_modal_visible(size, &probe) {
            ui_state.modal = Some(modal);
            return true;
        }
    }
    ui_state.proc.action_status =
        Some("Terminal too small to display action dialog; resize larger".to_owned());
    false
}

fn handle_modal_key(
    code: KeyCode,
    ctrl: bool,
    alt: bool,
    size: Size,
    ui_state: &mut UiState,
    prev_grid: &mut Option<Grid>,
) -> bool {
    if cancel_invisible_modal(size, ui_state, prev_grid) {
        return true;
    }
    if ctrl || alt {
        return false;
    }
    let Some(modal) = ui_state.modal.clone() else {
        return false;
    };
    match modal {
        ProcActionModal::ConfirmTerminate { target, name } => match code {
            KeyCode::Enter | KeyCode::Char('y' | 'Y') => {
                match terminate_process(target.pid, target.create_time_100ns) {
                    Ok(()) => {
                        ui_state.modal = None;
                        ui_state.proc.action_status =
                            Some(format!("Terminated {name} (PID {})", target.pid));
                    }
                    Err(err) => {
                        ui_state.modal = Some(ProcActionModal::ActionNotice {
                            title: "Terminate Refused".to_owned(),
                            message: err.to_string(),
                            is_error: true,
                        });
                    }
                }
                *prev_grid = None;
                true
            }
            KeyCode::Esc | KeyCode::Char('n' | 'N') => {
                ui_state.modal = None;
                *prev_grid = None;
                true
            }
            _ => false,
        },
        ProcActionModal::PrioritySelector { .. } => match code {
            KeyCode::Up => {
                if let Some(ref mut m) = ui_state.modal {
                    m.move_priority_up();
                }
                true
            }
            KeyCode::Down => {
                if let Some(ref mut m) = ui_state.modal {
                    m.move_priority_down();
                }
                true
            }
            KeyCode::Enter => {
                if let Some((target, name, ui_class)) = modal.selected_priority() {
                    let sys_class = ui_to_sys_priority(ui_class);
                    match set_process_priority(target.pid, target.create_time_100ns, sys_class) {
                        Ok(outcome) => {
                            ui_state.modal = None;
                            let status = if outcome.applied == outcome.requested {
                                format!(
                                    "Set priority for {name} (PID {}) to {}",
                                    target.pid,
                                    outcome.applied.label()
                                )
                            } else {
                                format!(
                                    "Priority for {name} (PID {}) clamped to {} ({} requires Administrator)",
                                    target.pid,
                                    outcome.applied.label(),
                                    outcome.requested.label()
                                )
                            };
                            ui_state.proc.action_status = Some(status);
                        }
                        Err(err) => {
                            ui_state.modal = Some(ProcActionModal::ActionNotice {
                                title: "Priority Change Refused".to_owned(),
                                message: err.to_string(),
                                is_error: true,
                            });
                        }
                    }
                    *prev_grid = None;
                    return true;
                }
                false
            }
            KeyCode::Esc => {
                ui_state.modal = None;
                *prev_grid = None;
                true
            }
            _ => false,
        },
        ProcActionModal::ActionNotice { .. } => match code {
            KeyCode::Enter | KeyCode::Esc | KeyCode::Char(' ') => {
                ui_state.modal = None;
                *prev_grid = None;
                true
            }
            _ => false,
        },
    }
}

const fn ui_to_sys_priority(class: UiPriorityClass) -> PriorityClass {
    match class {
        UiPriorityClass::Idle => PriorityClass::Idle,
        UiPriorityClass::BelowNormal => PriorityClass::BelowNormal,
        UiPriorityClass::Normal => PriorityClass::Normal,
        UiPriorityClass::AboveNormal => PriorityClass::AboveNormal,
        UiPriorityClass::High => PriorityClass::High,
        UiPriorityClass::Realtime => PriorityClass::Realtime,
    }
}

const fn sys_to_ui_priority(class: PriorityClass) -> UiPriorityClass {
    match class {
        PriorityClass::Idle => UiPriorityClass::Idle,
        PriorityClass::BelowNormal => UiPriorityClass::BelowNormal,
        PriorityClass::Normal => UiPriorityClass::Normal,
        PriorityClass::AboveNormal => UiPriorityClass::AboveNormal,
        PriorityClass::High => UiPriorityClass::High,
        PriorityClass::Realtime => UiPriorityClass::Realtime,
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

#[allow(clippy::too_many_arguments)]
fn handle_panel_key(
    code: KeyCode,
    sampler: &Sampler,
    snapshot: &btop_model::Snapshot,
    history: &mut UiHistory,
    config: &mut Config,
    size: Size,
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
        other => handle_proc_key(other, snapshot, history, size, ui_state, proc_rows),
    }
}

fn handle_proc_key(
    code: KeyCode,
    snapshot: &btop_model::Snapshot,
    history: &UiHistory,
    size: Size,
    state: &mut UiState,
    proc_rows: usize,
) -> bool {
    let active_owned = state.proc.active_processes(snapshot).map(<[_]>::to_vec);
    let rows_built = active_owned
        .as_deref()
        .map(|procs| build_visible_rows(procs, &state.proc).0);
    let proc_count = rows_built.as_ref().map_or(0, Vec::len);

    match code {
        KeyCode::Up => {
            state.proc.move_up(proc_count, proc_rows);
            if let Some(rows) = rows_built.as_deref() {
                state.proc.sync_detail_after_navigation(rows);
            }
        }
        KeyCode::Down => {
            state.proc.move_down(proc_count, proc_rows);
            if let Some(rows) = rows_built.as_deref() {
                state.proc.sync_detail_after_navigation(rows);
            }
        }
        KeyCode::PageUp => {
            state.proc.page_up(proc_count, proc_rows);
            if let Some(rows) = rows_built.as_deref() {
                state.proc.sync_detail_after_navigation(rows);
            }
        }
        KeyCode::PageDown => {
            state.proc.page_down(proc_count, proc_rows);
            if let Some(rows) = rows_built.as_deref() {
                state.proc.sync_detail_after_navigation(rows);
            }
        }
        KeyCode::Home => {
            state.proc.home(proc_count, proc_rows);
            if let Some(rows) = rows_built.as_deref() {
                state.proc.sync_detail_after_navigation(rows);
            }
        }
        KeyCode::End => {
            state.proc.end(proc_count, proc_rows);
            if let Some(rows) = rows_built.as_deref() {
                state.proc.sync_detail_after_navigation(rows);
            }
        }
        KeyCode::Enter => {
            return rows_built
                .as_deref()
                .is_some_and(|rows| state.proc.toggle_detail_selected(rows, proc_rows));
        }
        KeyCode::Esc => {
            if rows_built
                .as_deref()
                .is_some_and(|rows| state.proc.close_detail(rows, proc_rows))
            {
                return true;
            }
            return state.proc.action_status.take().is_some();
        }
        KeyCode::Char('t' | 'k' | 'N') => {
            return handle_proc_action_shortcut(
                code,
                rows_built.as_deref(),
                size,
                proc_rows,
                state,
            );
        }
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

fn handle_proc_action_shortcut(
    code: KeyCode,
    rows_built: Option<&[btop_ui::DisplayProcRow<'_>]>,
    size: Size,
    proc_rows: usize,
    state: &mut UiState,
) -> bool {
    // Never dispatch process actions when the `proc` panel is hidden or no rows are visible.
    if proc_rows == 0 {
        return false;
    }
    let selected = state.proc.selected;
    let visible_end = state.proc.scroll_offset.saturating_add(proc_rows);
    if selected < state.proc.scroll_offset || selected >= visible_end {
        return false;
    }
    let Some(row) = rows_built.and_then(|rows| rows.get(selected)) else {
        return false;
    };
    let id = row.entry.identity;
    let name = row.entry.name.clone();
    match code {
        KeyCode::Char('t') => match graceful_close_process(id.pid, id.create_time_100ns) {
            Ok(count) => {
                let plural = if count == 1 { "" } else { "s" };
                state.proc.action_status = Some(format!(
                    "Sent WM_CLOSE to {name} (PID {}, {count} window{plural})",
                    id.pid
                ));
            }
            Err(err) => {
                let notice = ProcActionModal::ActionNotice {
                    title: "Graceful Close Refused".to_owned(),
                    message: err.to_string(),
                    is_error: true,
                };
                if !try_open_modal(size, state, notice) {
                    state.proc.action_status = Some(err.to_string());
                }
            }
        },
        KeyCode::Char('k') => {
            let _ = try_open_modal(
                size,
                state,
                ProcActionModal::ConfirmTerminate { target: id, name },
            );
            return true;
        }
        KeyCode::Char('N') => match query_process_priority(id.pid, id.create_time_100ns) {
            Ok(current) => {
                let ui_cur = sys_to_ui_priority(current);
                let _ = try_open_modal(
                    size,
                    state,
                    ProcActionModal::PrioritySelector {
                        target: id,
                        name,
                        current: Some(ui_cur),
                        selected: ui_cur.index(),
                    },
                );
                return true;
            }
            Err(err) => {
                let notice = ProcActionModal::ActionNotice {
                    title: "Priority Change Refused".to_owned(),
                    message: err.to_string(),
                    is_error: true,
                };
                if !try_open_modal(size, state, notice) {
                    state.proc.action_status = Some(err.to_string());
                }
            }
        },
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_snapshot_with_proc() -> btop_model::Snapshot {
        btop_model::Snapshot {
            processes: DataState::Available(vec![btop_model::ProcessEntry {
                identity: btop_model::ProcessIdentity {
                    pid: 4216,
                    create_time_100ns: 1000,
                },
                parent_pid: 4,
                name: "worker.exe".to_owned(),
                image_path: None,
                command_line: None,
                command_line_denied: false,
                user: None,
                user_denied: false,
                threads: 4,
                cpu_pct: 5.0,
                working_set_private_bytes: 1024,
                io_read_bytes: 0,
                io_write_bytes: 0,
                is_protected: false,
            }]),
            ..btop_model::Snapshot::default()
        }
    }

    #[test]
    fn process_actions_are_disabled_when_proc_panel_is_hidden() {
        let snap = sample_snapshot_with_proc();
        let history = UiHistory::default();
        let mut state = UiState::default();
        // Hide the `proc` panel (equivalent to pressing `4`).
        let _ = state.layout.toggle_box(BoxId::Proc);
        let size = Size {
            width: 120,
            height: 40,
        };
        let proc_rows = visible_proc_rows(size, &state);
        assert_eq!(proc_rows, 0);

        assert!(!handle_proc_key(
            KeyCode::Char('t'),
            &snap,
            &history,
            size,
            &mut state,
            proc_rows
        ));
        assert!(!handle_proc_key(
            KeyCode::Char('k'),
            &snap,
            &history,
            size,
            &mut state,
            proc_rows
        ));
        assert!(!handle_proc_key(
            KeyCode::Char('N'),
            &snap,
            &history,
            size,
            &mut state,
            proc_rows
        ));
        assert!(state.modal.is_none());
        assert!(state.proc.action_status.is_none());
    }

    #[test]
    fn confirm_terminate_refuses_to_arm_or_fire_when_viewport_hides_modal() {
        let snap = sample_snapshot_with_proc();
        let history = UiHistory::default();
        let cfg = Config {
            shown_boxes: "proc".to_owned(),
            ..Config::default()
        };
        let mut state = UiState::from_config(&cfg);
        let tiny = Size {
            width: 80,
            height: 7,
        };
        let tiny_rows = visible_proc_rows(tiny, &state);
        assert!(tiny_rows > 0, "80x7 proc-only layout still renders rows");

        // Pressing `k` at 80x7 must NOT arm an invisible ConfirmTerminate modal.
        let _ = handle_proc_key(
            KeyCode::Char('k'),
            &snap,
            &history,
            tiny,
            &mut state,
            tiny_rows,
        );
        assert!(state.modal.is_none());
        assert!(
            state
                .proc
                .action_status
                .as_deref()
                .is_some_and(|s| s.contains("too small"))
        );

        // If armed at 120x40 and then resized to 80x7, resizing cancels the modal before `Enter` can fire.
        let normal = Size {
            width: 120,
            height: 40,
        };
        let normal_rows = visible_proc_rows(normal, &state);
        let _ = handle_proc_key(
            KeyCode::Char('k'),
            &snap,
            &history,
            normal,
            &mut state,
            normal_rows,
        );
        assert!(matches!(
            state.modal,
            Some(ProcActionModal::ConfirmTerminate { .. })
        ));

        let mut prev = None;
        assert!(cancel_invisible_modal(tiny, &mut state, &mut prev));
        assert!(state.modal.is_none());
    }

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
