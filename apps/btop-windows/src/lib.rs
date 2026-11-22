//! Runtime session loop shared by the `btop-windows` binary and integration tests.

use std::path::{Path, PathBuf};
use std::time::Duration;

use btop_collect::{CollectError, FreqMode, Sampler, SnapshotSource};
use btop_config::{Config, ConfigError};
use btop_model::{DataState, RecordStats, SnapshotRecorder};
use btop_sys::{
    PriorityClass, graceful_close_process, is_process_elevated, query_process_priority,
    set_process_priority, start_windows_service, stop_windows_service, terminate_process,
};
use btop_term::{
    DiffEncoder, Grid, HitAction, InputSource, KeyCode, MouseButton, MouseKind, Size, TermError,
    TermEvent, TerminalGuard,
};
use btop_ui::{
    BoxId, ProcActionModal, UiHistory, UiPriorityClass, UiState, build_visible_rows,
    build_visible_service_rows, compute_layout, is_modal_visible, modal_rect, visible_proc_rows,
    visible_service_rows,
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

fn sync_services_selection(
    snapshot: &btop_model::Snapshot,
    ui_state: &mut UiState,
    service_rows: usize,
) {
    let services = match &snapshot.services {
        DataState::Available(list) | DataState::Stale { value: list, .. } => Some(list.as_slice()),
        _ => None,
    };
    let procs = match &snapshot.processes {
        DataState::Available(list) | DataState::Stale { value: list, .. } => Some(list.as_slice()),
        _ => None,
    };
    if let Some(svcs) = services {
        let (built_rows, _) = build_visible_service_rows(svcs, procs, &ui_state.services);
        ui_state.services.clamp(built_rows.len(), service_rows);
    } else {
        ui_state.services.clamp(0, service_rows);
    }
}

fn has_proc_panel(size: Size, ui_state: &UiState) -> bool {
    compute_layout(size, &ui_state.layout)
        .ok()
        .and_then(|layout| layout.proc())
        .is_some()
}

fn should_poll_services(size: Size, ui_state: &UiState) -> bool {
    ui_state.services.active && has_proc_panel(size, ui_state)
}

fn build_ui_state(config: &Config, overrides: &ConfigOverrides) -> UiState {
    let mut ui_state = UiState::from_config(config);
    ui_state.tz_offset_secs = btop_sys::local_utc_offset_secs();
    ui_state.services.is_elevated = is_process_elevated();
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
    let allow_live_actions = source.allows_live_actions();
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
            let service_rows = visible_service_rows(size, &ui_state);
            sync_proc_selection(&current_snapshot, &mut ui_state, proc_rows);
            if ui_state.services.active {
                sync_services_selection(&current_snapshot, &mut ui_state, service_rows);
            }
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
                            && !ui_state.services.filter_editing
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
                        service_rows,
                        allow_live_actions,
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
                        sampler.set_services_active(should_poll_services(size, &ui_state));
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
    service_rows: usize,
    allow_live_actions: bool,
    prev_grid: &mut Option<Grid>,
) -> bool {
    match event {
        TermEvent::Key(k) if ui_state.modal.is_some() => handle_modal_key(
            k.code,
            k.ctrl,
            k.alt,
            *size,
            allow_live_actions,
            Some(sampler),
            ui_state,
            prev_grid,
        ),
        TermEvent::Key(k)
            if ui_state.proc.filter_editing
                || (ui_state.services.active && ui_state.services.filter_editing) =>
        {
            handle_filter_edit_key(k.code, k.ctrl, k.alt, ui_state)
        }
        TermEvent::Key(k) if k.ctrl && matches!(k.code, KeyCode::Char('r' | 'R')) => {
            if let Ok(reloaded) = load_config(config_path, overrides) {
                *config = reloaded;
                config.update_ms = sampler.set_update_ms(config.update_ms);
                sampler.set_freq_mode(FreqMode::from_config_str(&config.freq_mode));
                *ui_state = build_ui_state(config, overrides);
                ui_state.net.observe_snapshot(snapshot);
                sampler.set_services_active(should_poll_services(*size, ui_state));
                *prev_grid = None;
                return true;
            }
            false
        }
        TermEvent::Key(k) if !k.ctrl && !k.alt => {
            if handle_layout_key(k.code, ui_state, prev_grid) {
                sampler.set_services_active(should_poll_services(*size, ui_state));
                return true;
            }
            if k.code == KeyCode::Char('S') {
                return toggle_services_view(*size, ui_state, Some(sampler), prev_grid);
            }
            handle_panel_key(
                k.code,
                sampler,
                snapshot,
                history,
                config,
                *size,
                ui_state,
                proc_rows,
                service_rows,
                allow_live_actions,
            )
        }
        TermEvent::Key(k) => handle_unmodified_proc_key(
            k.code,
            k.ctrl,
            k.alt,
            snapshot,
            history,
            *size,
            ui_state,
            proc_rows,
            allow_live_actions,
        ),
        TermEvent::Resize(new_size) => {
            *size = guard.size().unwrap_or(new_size);
            sampler.set_services_active(should_poll_services(*size, ui_state));
            let _ = cancel_invisible_modal(*size, ui_state, prev_grid);
            *prev_grid = None;
            true
        }
        TermEvent::Mouse(mouse) => {
            handle_mouse_event(mouse, *size, ui_state, Some(sampler), prev_grid)
        }
    }
}

fn toggle_services_view(
    size: Size,
    ui_state: &mut UiState,
    sampler: Option<&Sampler>,
    prev_grid: &mut Option<Grid>,
) -> bool {
    if !has_proc_panel(size, ui_state) {
        return false;
    }
    // Always exit any active filter editor on either side so a hidden panel never traps keys.
    ui_state.proc.end_filter_edit();
    ui_state.services.end_filter_edit();
    let active = ui_state.services.toggle_active();
    if let Some(s) = sampler {
        s.set_services_active(active && has_proc_panel(size, ui_state));
    }
    *prev_grid = None;
    true
}

fn handle_mouse_event(
    mouse: btop_term::MouseEvent,
    size: Size,
    ui_state: &mut UiState,
    sampler: Option<&Sampler>,
    prev_grid: &mut Option<Grid>,
) -> bool {
    if ui_state.modal.is_some() || mouse.kind != MouseKind::Down(MouseButton::Left) {
        return false;
    }
    let hit = prev_grid
        .as_ref()
        .and_then(|g| g.hit_map.hit_test(mouse.col, mouse.row))
        .cloned();
    if hit == Some(HitAction::ToggleServicesView) {
        return toggle_services_view(size, ui_state, sampler, prev_grid);
    }
    false
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
    let msg = "Terminal too small to display action dialog; resize larger".to_owned();
    if ui_state.services.active {
        ui_state.services.action_status = Some(msg);
    } else {
        ui_state.proc.action_status = Some(msg);
    }
    false
}

#[allow(clippy::too_many_arguments)]
fn handle_modal_key(
    code: KeyCode,
    ctrl: bool,
    alt: bool,
    size: Size,
    allow_live_actions: bool,
    sampler: Option<&Sampler>,
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
                if !allow_live_actions {
                    ui_state.modal = None;
                    ui_state.proc.action_status =
                        Some("Process actions are disabled in replay mode".to_owned());
                    *prev_grid = None;
                    return true;
                }
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
        ProcActionModal::ConfirmServiceAction {
            service_name,
            display_name,
            start,
        } => handle_service_confirm_modal(
            code,
            &service_name,
            &display_name,
            start,
            allow_live_actions,
            sampler,
            ui_state,
            prev_grid,
        ),
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
                apply_priority_modal_selection(&modal, allow_live_actions, ui_state, prev_grid)
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

#[allow(clippy::too_many_arguments)]
fn handle_service_confirm_modal(
    code: KeyCode,
    service_name: &str,
    display_name: &str,
    start: bool,
    allow_live_actions: bool,
    sampler: Option<&Sampler>,
    ui_state: &mut UiState,
    prev_grid: &mut Option<Grid>,
) -> bool {
    match code {
        KeyCode::Enter | KeyCode::Char('y' | 'Y') => {
            if !allow_live_actions {
                ui_state.modal = None;
                ui_state.services.action_status =
                    Some("Service actions are disabled in replay mode".to_owned());
                *prev_grid = None;
                return true;
            }
            let result = if start {
                start_windows_service(service_name)
            } else {
                stop_windows_service(service_name)
            };
            match result {
                Ok(()) => {
                    ui_state.modal = None;
                    let verb = if start {
                        "Requested start for"
                    } else {
                        "Requested stop for"
                    };
                    ui_state.services.action_status =
                        Some(format!("{verb} '{service_name}' ({display_name})"));
                    if let Some(s) = sampler {
                        s.refresh_services();
                    }
                }
                Err(err) => {
                    ui_state.modal = Some(ProcActionModal::ActionNotice {
                        title: if start {
                            "Service Start Refused".to_owned()
                        } else {
                            "Service Stop Refused".to_owned()
                        },
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
    }
}

fn apply_priority_modal_selection(
    modal: &ProcActionModal,
    allow_live_actions: bool,
    ui_state: &mut UiState,
    prev_grid: &mut Option<Grid>,
) -> bool {
    if !allow_live_actions {
        ui_state.modal = None;
        ui_state.proc.action_status =
            Some("Process actions are disabled in replay mode".to_owned());
        *prev_grid = None;
        return true;
    }
    let Some((target, name, ui_class)) = modal.selected_priority() else {
        return false;
    };
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
    true
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
    if ui_state.services.active && ui_state.services.filter_editing {
        return match code {
            KeyCode::Enter | KeyCode::Esc => {
                ui_state.services.end_filter_edit();
                true
            }
            KeyCode::Backspace => {
                ui_state.services.pop_filter_char();
                true
            }
            KeyCode::Delete => ui_state.services.clear_filter(),
            KeyCode::Char(ch) => {
                ui_state.services.push_filter_char(ch);
                true
            }
            _ => false,
        };
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
    service_rows: usize,
    allow_live_actions: bool,
) -> bool {
    match code {
        KeyCode::Char('+') => {
            if !ui_state.services.active
                && let Some(procs) = ui_state.proc.active_processes(snapshot).map(<[_]>::to_vec)
            {
                let (rows, _) = build_visible_rows(&procs, &ui_state.proc);
                if ui_state.proc.expand_selected(&rows) {
                    return true;
                }
            }
            config.update_ms = sampler.adjust_update_ms(100);
            true
        }
        KeyCode::Char('-') => {
            if !ui_state.services.active
                && let Some(procs) = ui_state.proc.active_processes(snapshot).map(<[_]>::to_vec)
            {
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
        other if ui_state.services.active => handle_services_key(
            other,
            snapshot,
            size,
            ui_state,
            service_rows,
            allow_live_actions,
        ),
        other => handle_proc_key(
            other,
            snapshot,
            history,
            size,
            ui_state,
            proc_rows,
            allow_live_actions,
        ),
    }
}

fn handle_services_key(
    code: KeyCode,
    snapshot: &btop_model::Snapshot,
    size: Size,
    state: &mut UiState,
    service_rows: usize,
    allow_live_actions: bool,
) -> bool {
    let services = match &snapshot.services {
        DataState::Available(list) | DataState::Stale { value: list, .. } => Some(list.as_slice()),
        _ => None,
    };
    let procs = match &snapshot.processes {
        DataState::Available(list) | DataState::Stale { value: list, .. } => Some(list.as_slice()),
        _ => None,
    };
    let rows_built =
        services.map(|svcs| build_visible_service_rows(svcs, procs, &state.services).0);
    let count = rows_built.as_ref().map_or(0, Vec::len);

    match code {
        KeyCode::Up => state.services.move_up(count, service_rows),
        KeyCode::Down => state.services.move_down(count, service_rows),
        KeyCode::PageUp => state.services.page_up(count, service_rows),
        KeyCode::PageDown => state.services.page_down(count, service_rows),
        KeyCode::Home => state.services.home(count, service_rows),
        KeyCode::End => state.services.end(count, service_rows),
        KeyCode::Left => state.services.prev_sort(),
        KeyCode::Right => state.services.next_sort(),
        KeyCode::Char('r') => state.services.toggle_reverse(),
        KeyCode::Char('f' | '/') => state.services.begin_filter_edit(),
        KeyCode::Delete => return state.services.clear_filter(),
        KeyCode::Esc => return state.services.action_status.take().is_some(),
        KeyCode::Char('s' | 'x') => {
            return handle_service_action_shortcut(
                code,
                rows_built.as_deref(),
                size,
                service_rows,
                allow_live_actions,
                state,
            );
        }
        _ => return false,
    }
    true
}

fn handle_service_action_shortcut(
    code: KeyCode,
    rows_built: Option<&[btop_ui::ServiceRow<'_>]>,
    size: Size,
    service_rows: usize,
    allow_live_actions: bool,
    state: &mut UiState,
) -> bool {
    if service_rows == 0 {
        return false;
    }
    let selected = state.services.selected;
    let visible_end = state.services.scroll_offset.saturating_add(service_rows);
    if selected < state.services.scroll_offset || selected >= visible_end {
        return false;
    }
    if !allow_live_actions {
        let notice = ProcActionModal::ActionNotice {
            title: "Live Actions Disabled".to_owned(),
            message: "Service Start/Stop actions (s, x) are disabled in --replay mode.".to_owned(),
            is_error: false,
        };
        if !try_open_modal(size, state, notice) {
            state.services.action_status =
                Some("Service Start/Stop actions are disabled in --replay mode".to_owned());
        }
        return true;
    }
    if !state.services.is_elevated {
        let notice = ProcActionModal::ActionNotice {
            title: "Administrator Privileges Required".to_owned(),
            message: "Service Start (s) and Stop (x) are disabled when non-elevated. Restart btop-windows as Administrator.".to_owned(),
            is_error: false,
        };
        if !try_open_modal(size, state, notice) {
            state.services.action_status = Some(
                "Administrator privileges required: run btop-windows elevated to start/stop services"
                    .to_owned(),
            );
        }
        return true;
    }
    let Some(row) = rows_built.and_then(|rows| rows.get(selected)) else {
        return false;
    };
    let modal = ProcActionModal::ConfirmServiceAction {
        service_name: row.entry.name.clone(),
        display_name: row.entry.display_name.clone(),
        start: matches!(code, KeyCode::Char('s')),
    };
    let _ = try_open_modal(size, state, modal);
    true
}

#[allow(clippy::too_many_arguments)]
fn handle_unmodified_proc_key(
    code: KeyCode,
    ctrl: bool,
    alt: bool,
    snapshot: &btop_model::Snapshot,
    history: &UiHistory,
    size: Size,
    state: &mut UiState,
    proc_rows: usize,
    allow_live_actions: bool,
) -> bool {
    if ctrl || alt {
        return false;
    }
    handle_proc_key(
        code,
        snapshot,
        history,
        size,
        state,
        proc_rows,
        allow_live_actions,
    )
}

fn handle_proc_key(
    code: KeyCode,
    snapshot: &btop_model::Snapshot,
    history: &UiHistory,
    size: Size,
    state: &mut UiState,
    proc_rows: usize,
    allow_live_actions: bool,
) -> bool {
    let active_owned = state.proc.active_processes(snapshot).map(<[_]>::to_vec);
    let rows_built = active_owned
        .as_deref()
        .map(|procs| build_visible_rows(procs, &state.proc).0);
    let proc_count = rows_built.as_ref().map_or(0, Vec::len);

    if handle_proc_nav_key(code, rows_built.as_deref(), proc_count, proc_rows, state) {
        return true;
    }
    match code {
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
                allow_live_actions,
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

fn handle_proc_nav_key(
    code: KeyCode,
    rows_built: Option<&[btop_ui::DisplayProcRow<'_>]>,
    proc_count: usize,
    proc_rows: usize,
    state: &mut UiState,
) -> bool {
    match code {
        KeyCode::Up => state.proc.move_up(proc_count, proc_rows),
        KeyCode::Down => state.proc.move_down(proc_count, proc_rows),
        KeyCode::PageUp => state.proc.page_up(proc_count, proc_rows),
        KeyCode::PageDown => state.proc.page_down(proc_count, proc_rows),
        KeyCode::Home => state.proc.home(proc_count, proc_rows),
        KeyCode::End => state.proc.end(proc_count, proc_rows),
        _ => return false,
    }
    if let Some(rows) = rows_built {
        state.proc.sync_detail_after_navigation(rows);
    }
    true
}

fn handle_proc_action_shortcut(
    code: KeyCode,
    rows_built: Option<&[btop_ui::DisplayProcRow<'_>]>,
    size: Size,
    proc_rows: usize,
    allow_live_actions: bool,
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
    if !allow_live_actions {
        let notice = ProcActionModal::ActionNotice {
            title: "Live Actions Disabled".to_owned(),
            message: "Process actions (t, k, N) are disabled in --replay mode.".to_owned(),
            is_error: false,
        };
        if !try_open_modal(size, state, notice) {
            state.proc.action_status =
                Some("Process actions (t, k, N) are disabled in --replay mode".to_owned());
        }
        return true;
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
            proc_rows,
            true,
        ));
        assert!(!handle_proc_key(
            KeyCode::Char('k'),
            &snap,
            &history,
            size,
            &mut state,
            proc_rows,
            true,
        ));
        assert!(!handle_proc_key(
            KeyCode::Char('N'),
            &snap,
            &history,
            size,
            &mut state,
            proc_rows,
            true,
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
            true,
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
            true,
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
    fn modified_ctrl_or_alt_shortcuts_do_not_dispatch_process_actions() {
        let snap = sample_snapshot_with_proc();
        let history = UiHistory::default();
        let mut state = UiState::default();
        let size = Size {
            width: 120,
            height: 40,
        };
        let proc_rows = visible_proc_rows(size, &state);
        assert!(proc_rows > 0);

        for (code, ctrl, alt) in [
            (KeyCode::Char('t'), true, false),
            (KeyCode::Char('t'), false, true),
            (KeyCode::Char('k'), true, false),
            (KeyCode::Char('N'), false, true),
        ] {
            let handled = handle_unmodified_proc_key(
                code, ctrl, alt, &snap, &history, size, &mut state, proc_rows, true,
            );
            assert!(!handled);
            assert!(state.modal.is_none());
            assert!(state.proc.action_status.is_none());
        }
    }

    #[test]
    fn replay_mode_blocks_mutating_process_actions() {
        let snap = sample_snapshot_with_proc();
        let replay =
            btop_collect::ReplaySource::from_snapshots(vec![snap.clone()]).expect("replay source");
        assert!(
            !replay.allows_live_actions(),
            "ReplaySource must be fail-closed for live process actions"
        );

        let history = UiHistory::default();
        let mut state = UiState::default();
        let size = Size {
            width: 120,
            height: 40,
        };
        let proc_rows = visible_proc_rows(size, &state);
        for key in [KeyCode::Char('t'), KeyCode::Char('k'), KeyCode::Char('N')] {
            state.modal = None;
            assert!(handle_proc_key(
                key, &snap, &history, size, &mut state, proc_rows, false,
            ));
            assert!(matches!(
                state.modal,
                Some(ProcActionModal::ActionNotice { ref title, .. }) if title == "Live Actions Disabled"
            ));
        }
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

    #[test]
    fn services_actions_require_elevation_live_mode_and_visible_panel() {
        let mut snap = sample_snapshot_with_proc();
        snap.services = DataState::Available(vec![btop_model::ServiceEntry {
            name: "W32Time".to_owned(),
            display_name: "Windows Time".to_owned(),
            state: "Running".to_owned(),
            start_type: "Auto".to_owned(),
            pid: Some(4216),
        }]);
        let size = Size {
            width: 120,
            height: 40,
        };
        let mut state = UiState::default();
        state.services.active = true;
        let rows = visible_service_rows(size, &state);
        assert!(rows > 0);

        // Non-elevated live mode shows Administrator notice instead of Start/Stop confirmation.
        state.services.is_elevated = false;
        assert!(handle_services_key(
            KeyCode::Char('x'),
            &snap,
            size,
            &mut state,
            rows,
            true,
        ));
        assert!(matches!(
            state.modal,
            Some(ProcActionModal::ActionNotice { ref title, .. })
                if title == "Administrator Privileges Required"
        ));

        // Elevated in replay mode is still blocked fail-closed.
        state.modal = None;
        state.services.is_elevated = true;
        assert!(handle_services_key(
            KeyCode::Char('x'),
            &snap,
            size,
            &mut state,
            rows,
            false,
        ));
        assert!(matches!(
            state.modal,
            Some(ProcActionModal::ActionNotice { ref title, .. })
                if title == "Live Actions Disabled"
        ));

        // Elevated in live mode opens the confirmation dialog for the selected service.
        state.modal = None;
        assert!(handle_services_key(
            KeyCode::Char('x'),
            &snap,
            size,
            &mut state,
            rows,
            true,
        ));
        assert!(matches!(
            state.modal,
            Some(ProcActionModal::ConfirmServiceAction {
                ref service_name,
                start: false,
                ..
            }) if service_name == "W32Time"
        ));

        // Hidden panel blocks service actions completely.
        state.modal = None;
        let _ = state.layout.toggle_box(BoxId::Proc);
        let hidden_rows = visible_service_rows(size, &state);
        assert_eq!(hidden_rows, 0);
        assert!(!handle_services_key(
            KeyCode::Char('s'),
            &snap,
            size,
            &mut state,
            hidden_rows,
            true,
        ));
        assert!(state.modal.is_none());
    }

    #[test]
    fn services_tab_click_toggles_view_and_filter_edit_updates_services_filter() {
        let snap = sample_snapshot_with_proc();
        let history = UiHistory::default();
        let cfg = Config::default();
        let size = Size {
            width: 120,
            height: 40,
        };
        let mut state = UiState::from_config(&cfg);
        let grid = btop_ui::render_frame(&snap, &history, &state, &cfg, size);
        let (col, row) = (0..size.height)
            .find_map(|y| {
                (0..size.width).find_map(|x| {
                    (grid.hit_map.hit_test(x, y) == Some(&HitAction::ToggleServicesView))
                        .then_some((x, y))
                })
            })
            .expect("proc panel must render clickable S:svc tab");
        let mut prev_grid = Some(grid);

        // Start editing process filter before clicking S:svc; tab switch must exit the hidden editor.
        state.proc.begin_filter_edit();
        assert!(state.proc.filter_editing);

        assert!(handle_mouse_event(
            btop_term::MouseEvent {
                col,
                row,
                kind: MouseKind::Down(MouseButton::Left),
            },
            size,
            &mut state,
            None,
            &mut prev_grid,
        ));
        assert!(state.services.active);
        assert!(
            !state.proc.filter_editing,
            "switching to Services must exit hidden process filter editing"
        );

        // Typing into Services filter updates `state.services.filter`, not `state.proc.filter`.
        state.services.begin_filter_edit();
        assert!(handle_filter_edit_key(
            KeyCode::Char('q'),
            false,
            false,
            &mut state,
        ));
        assert_eq!(state.services.filter, "q");
        assert_eq!(state.proc.filter, "");
    }
}
