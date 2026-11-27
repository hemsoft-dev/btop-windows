//! Runtime session loop shared by the `btop-windows` binary and integration tests.

pub mod cli;
pub mod headless;
mod menu_input;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

/// Interval after which the TUI forces a full-grid cell repaint (`prev_grid = None`) so terminals
/// or web/PWA panes that dropped earlier incremental diff frames recover static box borders and
/// labels even when no focus or resize event arrives.
const FULL_REDRAW_INTERVAL: Duration = Duration::from_secs(5);

use btop_collect::{CollectError, FreqMode, Sampler, SnapshotSource};
use btop_config::{Config, ConfigError, ConfigLocation, ThemeSearchPaths, resolve_config_location};
use btop_model::{DataState, RecordStats, SnapshotRecorder};
use btop_sys::{
    PriorityClass, ServiceActionError, graceful_close_process, is_process_elevated,
    query_process_priority, set_process_priority, start_windows_service, stop_windows_service,
    terminate_process,
};
use btop_term::{
    DiffEncoder, Grid, HitAction, InputSource, KeyCode, MouseButton, MouseKind, Size, TermError,
    TermEvent, TerminalGuard,
};
use btop_ui::{
    BoxId, OverlayMenu, ProcActionModal, ProcSort, ServiceSort, UiHistory, UiPriorityClass,
    UiState, build_visible_rows, build_visible_service_rows, compute_layout, is_modal_visible,
    modal_rect, theme::ThemePalette, visible_proc_rows, visible_service_rows,
};
use menu_input::{
    LoopAction, handle_menu_key, handle_menu_mouse, sync_config_from_ui_state,
    try_open_menu_shortcut,
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
    pub themes_dir: Option<PathBuf>,
    pub force_tty: bool,
    pub low_color: bool,
}

/// Loads `btop.conf` using strict precedence (`-c` explicit -> portable exe-adjacent
/// `btop.conf` -> `%APPDATA%\btop\btop.conf`), then applies CLI overrides.
pub fn load_config(
    explicit_path: Option<&Path>,
    overrides: &ConfigOverrides,
) -> Result<Config, AppError> {
    let mut cfg = match resolve_config_location(explicit_path) {
        Some(ConfigLocation::Explicit(path)) => {
            let content = std::fs::read_to_string(&path)?;
            Config::parse(&content)?
        }
        Some(ConfigLocation::Portable(path) | ConfigLocation::AppData(path)) => {
            if path.is_file() {
                let content = std::fs::read_to_string(&path)?;
                Config::parse(&content)?
            } else {
                Config::default()
            }
        }
        None => Config::default(),
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

/// Persists `config` (synchronized with live `ui_state`) to the resolved `btop.conf` path
/// when `allow_live_actions && config.save_config_on_exit` are both `true`.
pub fn save_config_if_enabled(
    config: &mut Config,
    ui_state: &UiState,
    explicit_path: Option<&Path>,
    allow_live_actions: bool,
) -> Result<Option<PathBuf>, AppError> {
    if !allow_live_actions || !config.save_config_on_exit {
        return Ok(None);
    }
    let Some(location) = resolve_config_location(explicit_path) else {
        return Ok(None);
    };
    sync_config_from_ui_state(config, ui_state);
    config.save_to_path(location.path())?;
    Ok(Some(location.path().to_path_buf()))
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

struct ServiceActionOutcome {
    service_name: String,
    display_name: String,
    start: bool,
    result: Result<(), ServiceActionError>,
}

struct ServiceDispatch<'a> {
    tx: &'a Sender<ServiceActionOutcome>,
    in_flight: &'a Arc<AtomicBool>,
}

struct ResetBoolOnDrop(Arc<AtomicBool>);

impl Drop for ResetBoolOnDrop {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn build_ui_state(config: &Config, overrides: &ConfigOverrides) -> UiState {
    let mut ui_state = UiState::from_config(config);
    let theme_paths = ThemeSearchPaths::resolve(overrides.themes_dir.as_deref());
    ui_state.available_themes = theme_paths.discover_themes();
    ui_state.resolved_theme = Some(ThemePalette::resolve(
        &config.color_theme,
        config.theme_background,
        &theme_paths,
    ));
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
    let (mut history, mut ui_state) = (UiHistory::default(), build_ui_state(&config, overrides));
    let (mut last_generation, mut last_telemetry_generation) =
        (sampler.generation(), sampler.telemetry_generation());
    let mut current_snapshot = sampler.latest();
    history.push_with_util_mode(&current_snapshot, is_utility_mode(&config));
    ui_state.net.observe_snapshot(&current_snapshot);

    let (svc_tx, svc_rx) = mpsc::channel::<ServiceActionOutcome>();
    let svc_in_flight = Arc::new(AtomicBool::new(false));
    let mut prev_grid: Option<Grid> = None;
    let mut last_full_redraw = Instant::now();
    let mut dirty = true;
    let mut frames_rendered = 0u64;

    let run_result: Result<(), AppError> = (|| {
        loop {
            let (proc_rows, service_rows, state_dirty) = sync_loop_state(
                &sampler,
                &svc_rx,
                size,
                &config,
                &mut last_generation,
                &mut last_telemetry_generation,
                &mut current_snapshot,
                &mut history,
                &mut ui_state,
                &mut prev_grid,
                last_full_redraw.elapsed(),
            );
            dirty |= state_dirty;

            if dirty {
                render_dirty_frame(
                    &mut guard,
                    &current_snapshot,
                    &history,
                    &ui_state,
                    &config,
                    size,
                    &mut prev_grid,
                    &mut last_full_redraw,
                )?;
                frames_rendered += 1;
                dirty = false;
            }

            if sampler.is_finished() {
                break;
            }

            match input.poll(Duration::from_millis(50))? {
                Some(TermEvent::Key(k)) if is_quit_key(k, &ui_state) => break,
                Some(event) => {
                    let dispatch = ServiceDispatch {
                        tx: &svc_tx,
                        in_flight: &svc_in_flight,
                    };
                    match handle_tui_event(
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
                        Some(&dispatch),
                        &mut prev_grid,
                    ) {
                        LoopAction::None => {}
                        LoopAction::Redraw => dirty = true,
                        LoopAction::Quit => break,
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
    warn_on_exit_save(&mut config, &ui_state, config_path, allow_live_actions);
    Ok(frames_rendered)
}

fn warn_on_exit_save(
    config: &mut Config,
    ui_state: &UiState,
    config_path: Option<&Path>,
    allow_live_actions: bool,
) {
    if let Err(err) = save_config_if_enabled(config, ui_state, config_path, allow_live_actions) {
        eprintln!("warning: failed to save btop.conf on exit: {err}");
    }
}

fn is_quit_key(k: btop_term::KeyEvent, ui_state: &UiState) -> bool {
    k.is_ctrl_c()
        || (!ui_state.proc.filter_editing
            && !ui_state.services.filter_editing
            && ui_state.modal.is_none()
            && ui_state.menu.is_none()
            && k.code == KeyCode::Char('q'))
}

#[allow(clippy::too_many_arguments)]
fn sync_loop_state(
    sampler: &Sampler,
    svc_rx: &Receiver<ServiceActionOutcome>,
    size: Size,
    config: &Config,
    last_generation: &mut u64,
    last_telemetry_generation: &mut u64,
    current_snapshot: &mut Arc<btop_model::Snapshot>,
    history: &mut UiHistory,
    ui_state: &mut UiState,
    prev_grid: &mut Option<Grid>,
    since_full_redraw: Duration,
) -> (usize, usize, bool) {
    let mut dirty = poll_sampler_updates(
        sampler,
        last_generation,
        last_telemetry_generation,
        current_snapshot,
        history,
        ui_state,
        config,
    );
    dirty |= drain_service_action_outcomes(svc_rx, size, Some(sampler), ui_state, prev_grid);
    let proc_rows = visible_proc_rows(size, ui_state);
    let service_rows = visible_service_rows(size, ui_state);
    sync_proc_selection(current_snapshot, ui_state, proc_rows);
    if ui_state.services.active {
        sync_services_selection(current_snapshot, ui_state, service_rows);
    }
    dirty |= cancel_invisible_modal(size, ui_state, prev_grid);
    dirty |= should_force_periodic_full_redraw(since_full_redraw, FULL_REDRAW_INTERVAL, prev_grid);
    (proc_rows, service_rows, dirty)
}

fn poll_sampler_updates(
    sampler: &Sampler,
    last_generation: &mut u64,
    last_telemetry_generation: &mut u64,
    current_snapshot: &mut Arc<btop_model::Snapshot>,
    history: &mut UiHistory,
    ui_state: &mut UiState,
    config: &Config,
) -> bool {
    if ui_state.menu.is_some() && !config.background_update {
        return false;
    }
    let gen_now = sampler.generation();
    if gen_now == *last_generation {
        return false;
    }
    *last_generation = gen_now;
    *current_snapshot = sampler.latest();
    let telemetry_gen_now = sampler.telemetry_generation();
    if telemetry_gen_now != *last_telemetry_generation {
        *last_telemetry_generation = telemetry_gen_now;
        ui_state.tz_offset_secs = btop_sys::local_utc_offset_secs();
        history.push_with_util_mode(current_snapshot, is_utility_mode(config));
        ui_state.net.observe_snapshot(current_snapshot);
    }
    true
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

#[allow(clippy::too_many_arguments)]
fn render_dirty_frame(
    guard: &mut TerminalGuard,
    snapshot: &btop_model::Snapshot,
    history: &UiHistory,
    ui_state: &UiState,
    config: &Config,
    size: Size,
    prev_grid: &mut Option<Grid>,
    last_full_redraw: &mut Instant,
) -> Result<(), AppError> {
    let is_full_redraw = prev_grid.is_none();
    let encoder = DiffEncoder {
        sync_output: config.terminal_sync,
    };
    let next_grid = btop_ui::render_frame(snapshot, history, ui_state, config, size);
    let payload = encoder.encode(prev_grid.as_ref(), &next_grid);
    guard.write_frame(&payload)?;
    if is_full_redraw {
        *last_full_redraw = Instant::now();
    }
    *prev_grid = Some(next_grid);
    Ok(())
}

fn is_utility_mode(config: &Config) -> bool {
    config.cpu_util_mode.eq_ignore_ascii_case("utility")
}

fn should_force_periodic_full_redraw(
    elapsed: Duration,
    interval: Duration,
    prev_grid: &mut Option<Grid>,
) -> bool {
    if elapsed >= interval {
        *prev_grid = None;
        true
    } else {
        false
    }
}

fn try_handle_force_redraw(event: TermEvent, prev_grid: &mut Option<Grid>) -> bool {
    let force = matches!(
        event,
        TermEvent::FocusGained
            | TermEvent::Key(btop_term::KeyEvent {
                code: KeyCode::Char('l' | 'L'),
                ctrl: true,
                alt: false,
                ..
            })
    );
    if force {
        *prev_grid = None;
    }
    force
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
    service_dispatch: Option<&ServiceDispatch<'_>>,
    prev_grid: &mut Option<Grid>,
) -> LoopAction {
    if try_handle_force_redraw(event, prev_grid) {
        return LoopAction::Redraw;
    }
    match event {
        TermEvent::FocusGained => unreachable!("handled by try_handle_force_redraw"),
        TermEvent::Key(k) if ui_state.menu.is_some() => {
            let outcome = handle_menu_key(
                k,
                *size,
                Some(sampler),
                snapshot,
                config,
                overrides.themes_dir.as_deref(),
                ui_state,
                prev_grid,
            );
            sampler.set_services_active(should_poll_services(*size, ui_state));
            outcome
        }
        TermEvent::Key(k) if ui_state.modal.is_some() => bool_to_loop_action(handle_modal_key(
            k,
            config.vim_keys,
            *size,
            allow_live_actions,
            service_dispatch,
            ui_state,
            prev_grid,
        )),
        TermEvent::Key(k)
            if ui_state.proc.filter_editing
                || (ui_state.services.active && ui_state.services.filter_editing) =>
        {
            bool_to_loop_action(handle_filter_edit_key(k.code, k.ctrl, k.alt, ui_state))
        }
        TermEvent::Key(k) if k.ctrl && matches!(k.code, KeyCode::Char('r' | 'R')) => {
            handle_ctrl_r_reload(
                sampler,
                snapshot,
                *size,
                config,
                config_path,
                overrides,
                ui_state,
                prev_grid,
            )
        }
        TermEvent::Key(k) if !k.ctrl && !k.alt => handle_unmodified_global_key(
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
            prev_grid,
        ),
        TermEvent::Key(k) => bool_to_loop_action(handle_unmodified_proc_key(
            k,
            config.vim_keys,
            snapshot,
            history,
            *size,
            ui_state,
            proc_rows,
            allow_live_actions,
        )),
        TermEvent::Resize(new_size) => {
            *size = guard.size().unwrap_or(new_size);
            sampler.set_services_active(should_poll_services(*size, ui_state));
            let _ = cancel_invisible_modal(*size, ui_state, prev_grid);
            *prev_grid = None;
            LoopAction::Redraw
        }
        TermEvent::Mouse(mouse) => handle_tui_mouse(
            mouse,
            *size,
            sampler,
            snapshot,
            history,
            config,
            overrides,
            ui_state,
            proc_rows,
            service_rows,
            prev_grid,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_tui_mouse(
    mouse: btop_term::MouseEvent,
    size: Size,
    sampler: &Sampler,
    snapshot: &btop_model::Snapshot,
    history: &mut UiHistory,
    config: &mut Config,
    overrides: &ConfigOverrides,
    ui_state: &mut UiState,
    proc_rows: usize,
    service_rows: usize,
    prev_grid: &mut Option<Grid>,
) -> LoopAction {
    if config.disable_mouse {
        return LoopAction::None;
    }
    if ui_state.menu.is_some() {
        let outcome = handle_menu_mouse(
            mouse,
            size,
            Some(sampler),
            snapshot,
            config,
            overrides.themes_dir.as_deref(),
            ui_state,
            prev_grid,
        );
        sampler.set_services_active(should_poll_services(size, ui_state));
        return outcome;
    }
    bool_to_loop_action(handle_mouse_event(
        mouse,
        size,
        snapshot,
        history,
        config,
        ui_state,
        Some(sampler),
        proc_rows,
        service_rows,
        prev_grid,
    ))
}

const fn bool_to_loop_action(changed: bool) -> LoopAction {
    if changed {
        LoopAction::Redraw
    } else {
        LoopAction::None
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_ctrl_r_reload(
    sampler: &Sampler,
    snapshot: &btop_model::Snapshot,
    size: Size,
    config: &mut Config,
    config_path: Option<&Path>,
    overrides: &ConfigOverrides,
    ui_state: &mut UiState,
    prev_grid: &mut Option<Grid>,
) -> LoopAction {
    if let Ok(reloaded) = load_config(config_path, overrides) {
        *config = reloaded;
        config.update_ms = sampler.set_update_ms(config.update_ms);
        sampler.set_freq_mode(FreqMode::from_config_str(&config.freq_mode));
        sampler.set_nvml_measure_pcie_speeds(config.nvml_measure_pcie_speeds);
        *ui_state = build_ui_state(config, overrides);
        ui_state.net.observe_snapshot(snapshot);
        sampler.set_services_active(should_poll_services(size, ui_state));
        *prev_grid = None;
        return LoopAction::Redraw;
    }
    LoopAction::None
}

#[allow(clippy::too_many_arguments)]
fn handle_unmodified_global_key(
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
    prev_grid: &mut Option<Grid>,
) -> LoopAction {
    if try_open_menu_shortcut(code, config, ui_state, prev_grid) {
        return LoopAction::Redraw;
    }
    if handle_layout_key(code, ui_state, prev_grid) {
        sampler.set_services_active(should_poll_services(size, ui_state));
        return LoopAction::Redraw;
    }
    if code == KeyCode::Char('S') {
        return if toggle_services_view(size, ui_state, Some(sampler), prev_grid) {
            LoopAction::Redraw
        } else {
            LoopAction::None
        };
    }
    let handled = handle_panel_key(
        code,
        sampler,
        snapshot,
        history,
        config,
        size,
        ui_state,
        proc_rows,
        service_rows,
        allow_live_actions,
    );
    if handled {
        return LoopAction::Redraw;
    }
    if code == KeyCode::Esc {
        ui_state.menu = Some(OverlayMenu::main());
        *prev_grid = None;
        return LoopAction::Redraw;
    }
    LoopAction::None
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

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn handle_mouse_event(
    mouse: btop_term::MouseEvent,
    size: Size,
    snapshot: &btop_model::Snapshot,
    history: &mut UiHistory,
    config: &mut Config,
    ui_state: &mut UiState,
    sampler: Option<&Sampler>,
    proc_rows: usize,
    service_rows: usize,
    prev_grid: &mut Option<Grid>,
) -> bool {
    if config.disable_mouse || ui_state.modal.is_some() {
        return false;
    }

    if matches!(mouse.kind, MouseKind::ScrollUp | MouseKind::ScrollDown) {
        return handle_dashboard_wheel(mouse, size, snapshot, ui_state, proc_rows, service_rows);
    }

    let is_left = mouse.kind == MouseKind::Down(MouseButton::Left);
    let is_right = mouse.kind == MouseKind::Down(MouseButton::Right);
    if !is_left && !is_right {
        return false;
    }

    let hit = prev_grid
        .as_ref()
        .and_then(|g| g.hit_map.hit_test(mouse.col, mouse.row))
        .cloned();
    let Some(hit) = hit else {
        return false;
    };

    if is_right {
        return match hit {
            HitAction::CyclePreset if ui_state.layout.cycle_preset_prev() => {
                if let Some(s) = sampler {
                    s.set_services_active(should_poll_services(size, ui_state));
                }
                *prev_grid = None;
                true
            }
            _ => false,
        };
    }

    match hit {
        HitAction::ToggleBox(name) => {
            let Some(box_id) = BoxId::parse(&name) else {
                return false;
            };
            if ui_state.layout.toggle_box(box_id) {
                if let Some(s) = sampler {
                    s.set_services_active(should_poll_services(size, ui_state));
                }
                *prev_grid = None;
                true
            } else {
                false
            }
        }
        HitAction::CyclePreset => {
            if ui_state.layout.cycle_preset_next() {
                if let Some(s) = sampler {
                    s.set_services_active(should_poll_services(size, ui_state));
                }
                *prev_grid = None;
                true
            } else {
                false
            }
        }
        HitAction::AdjustUpdateMs(delta) => {
            if let Some(s) = sampler {
                config.update_ms = s.adjust_update_ms(delta);
            } else {
                let next = i64::from(config.update_ms)
                    .saturating_add(i64::from(delta))
                    .clamp(100, 60_000);
                config.update_ms = u32::try_from(next).unwrap_or(100);
            }
            true
        }
        HitAction::ToggleDisks => {
            ui_state.toggle_disks();
            *prev_grid = None;
            true
        }
        HitAction::ToggleIoMode => {
            ui_state.toggle_io_mode();
            *prev_grid = None;
            true
        }
        HitAction::NetPrevIface => {
            ui_state.net.select_prev(snapshot);
            true
        }
        HitAction::NetNextIface => {
            ui_state.net.select_next(snapshot);
            true
        }
        HitAction::NetToggleAuto => {
            ui_state.net.toggle_auto();
            true
        }
        HitAction::NetToggleSync => {
            ui_state.net.toggle_sync();
            true
        }
        HitAction::NetResetTotals => {
            ui_state.net.reset_session_totals(snapshot, history);
            true
        }
        HitAction::ToggleServicesView => toggle_services_view(size, ui_state, sampler, prev_grid),
        HitAction::ProcPrevSort => {
            if ui_state.services.active {
                ui_state.services.prev_sort();
            } else {
                ui_state.proc.prev_sort();
            }
            true
        }
        HitAction::ProcNextSort => {
            if ui_state.services.active {
                ui_state.services.next_sort();
            } else {
                ui_state.proc.next_sort();
            }
            true
        }
        HitAction::ProcToggleReverse => {
            if ui_state.services.active {
                ui_state.services.toggle_reverse();
            } else {
                ui_state.proc.toggle_reverse();
            }
            true
        }
        HitAction::ProcSortColumn(col) => handle_mouse_sort_column(&col, ui_state),
        HitAction::ProcBeginFilter => {
            if ui_state.services.active {
                ui_state.services.begin_filter_edit();
            } else {
                ui_state.proc.begin_filter_edit();
            }
            true
        }
        HitAction::SelectProcess(pid) => {
            handle_select_process_click(pid, snapshot, ui_state, proc_rows)
        }
        HitAction::SelectService(row_idx) => {
            handle_select_service_click(row_idx, snapshot, ui_state, service_rows)
        }
        HitAction::OpenMenu(name) => {
            let menu = match name.as_str() {
                "options" => {
                    sync_config_from_ui_state(config, ui_state);
                    OverlayMenu::options()
                }
                "help" => OverlayMenu::help(),
                _ => OverlayMenu::main(),
            };
            ui_state.menu = Some(menu);
            *prev_grid = None;
            true
        }
        HitAction::MenuItem(_)
        | HitAction::MenuCategory(_)
        | HitAction::MenuClose
        | HitAction::MessageBoxButton(_) => false,
    }
}

fn handle_dashboard_wheel(
    mouse: btop_term::MouseEvent,
    size: Size,
    snapshot: &btop_model::Snapshot,
    ui_state: &mut UiState,
    proc_rows: usize,
    service_rows: usize,
) -> bool {
    let Some(proc_rect) = compute_layout(size, &ui_state.layout)
        .ok()
        .and_then(|layout| layout.proc())
    else {
        return false;
    };
    if !proc_rect.contains(mouse.col, mouse.row) {
        return false;
    }
    let up = mouse.kind == MouseKind::ScrollUp;
    if ui_state.services.active {
        let services = match &snapshot.services {
            DataState::Available(list) | DataState::Stale { value: list, .. } => {
                Some(list.as_slice())
            }
            _ => None,
        };
        let procs = match &snapshot.processes {
            DataState::Available(list) | DataState::Stale { value: list, .. } => {
                Some(list.as_slice())
            }
            _ => None,
        };
        let count = services.map_or(0, |svcs| {
            build_visible_service_rows(svcs, procs, &ui_state.services)
                .0
                .len()
        });
        if count == 0 {
            return false;
        }
        if up {
            ui_state.services.move_up(count, service_rows);
        } else {
            ui_state.services.move_down(count, service_rows);
        }
        return true;
    }

    let Some(procs) = ui_state.proc.active_processes(snapshot).map(<[_]>::to_vec) else {
        return false;
    };
    let (rows, _) = build_visible_rows(&procs, &ui_state.proc);
    if rows.is_empty() {
        return false;
    }
    if up {
        ui_state.proc.move_up(rows.len(), proc_rows);
    } else {
        ui_state.proc.move_down(rows.len(), proc_rows);
    }
    ui_state.proc.sync_detail_after_navigation(&rows);
    true
}

fn handle_mouse_sort_column(col: &str, ui_state: &mut UiState) -> bool {
    if ui_state.services.active {
        let target = match col {
            "name" => ServiceSort::Name,
            "display" => ServiceSort::DisplayName,
            "state" => ServiceSort::State,
            "start" => ServiceSort::StartType,
            "pid" => ServiceSort::Pid,
            "memory" => ServiceSort::Memory,
            "cpu" => ServiceSort::Cpu,
            _ => return false,
        };
        if ui_state.services.sort == target {
            ui_state.services.toggle_reverse();
        } else {
            ui_state.services.sort = target;
            ui_state.services.reversed = false;
        }
        return true;
    }

    match col {
        "pid" => apply_proc_sort_click(ProcSort::Pid, ui_state),
        "program" => apply_proc_sort_click(ProcSort::Program, ui_state),
        "threads" => apply_proc_sort_click(ProcSort::Threads, ui_state),
        "memory" => apply_proc_sort_click(ProcSort::Memory, ui_state),
        "cpu" => {
            if matches!(ui_state.proc.sort, ProcSort::CpuLazy | ProcSort::CpuDirect) {
                ui_state.proc.toggle_reverse();
            } else {
                ui_state.proc.sort = ProcSort::CpuLazy;
                ui_state.proc.reversed = false;
            }
            true
        }
        _ => false,
    }
}

fn apply_proc_sort_click(target: ProcSort, ui_state: &mut UiState) -> bool {
    if ui_state.proc.sort == target {
        ui_state.proc.toggle_reverse();
    } else {
        ui_state.proc.sort = target;
        ui_state.proc.reversed = false;
    }
    true
}

fn handle_select_process_click(
    pid: u32,
    snapshot: &btop_model::Snapshot,
    ui_state: &mut UiState,
    proc_rows: usize,
) -> bool {
    let Some(procs) = ui_state.proc.active_processes(snapshot).map(<[_]>::to_vec) else {
        return false;
    };
    let (rows, _) = build_visible_rows(&procs, &ui_state.proc);
    let Some(idx) = rows.iter().position(|r| r.entry.identity.pid == pid) else {
        return false;
    };
    if ui_state.proc.selected == idx {
        return ui_state.proc.toggle_detail_selected(&rows, proc_rows);
    }
    ui_state.proc.selected = idx;
    ui_state.proc.clamp(rows.len(), proc_rows);
    ui_state.proc.sync_detail_after_navigation(&rows);
    true
}

fn handle_select_service_click(
    row_idx: usize,
    snapshot: &btop_model::Snapshot,
    ui_state: &mut UiState,
    service_rows: usize,
) -> bool {
    let services = match &snapshot.services {
        DataState::Available(list) | DataState::Stale { value: list, .. } => Some(list.as_slice()),
        _ => None,
    };
    let procs = match &snapshot.processes {
        DataState::Available(list) | DataState::Stale { value: list, .. } => Some(list.as_slice()),
        _ => None,
    };
    let count = services.map_or(0, |svcs| {
        build_visible_service_rows(svcs, procs, &ui_state.services)
            .0
            .len()
    });
    if count == 0 || row_idx >= count {
        return false;
    }
    ui_state.services.selected = row_idx;
    ui_state.services.clamp(count, service_rows);
    true
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
    key: btop_term::KeyEvent,
    vim_keys: bool,
    size: Size,
    allow_live_actions: bool,
    service_dispatch: Option<&ServiceDispatch<'_>>,
    ui_state: &mut UiState,
    prev_grid: &mut Option<Grid>,
) -> bool {
    if cancel_invisible_modal(size, ui_state, prev_grid) {
        return true;
    }
    if key.ctrl || key.alt {
        return false;
    }
    let code = key.code;
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
            size,
            allow_live_actions,
            service_dispatch,
            ui_state,
            prev_grid,
        ),
        ProcActionModal::PrioritySelector { .. } => handle_priority_modal_key(
            code,
            vim_keys,
            &modal,
            allow_live_actions,
            ui_state,
            prev_grid,
        ),
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

fn handle_priority_modal_key(
    code: KeyCode,
    vim_keys: bool,
    modal: &ProcActionModal,
    allow_live_actions: bool,
    ui_state: &mut UiState,
    prev_grid: &mut Option<Grid>,
) -> bool {
    match code {
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
        KeyCode::Char('k') if vim_keys => {
            if let Some(ref mut m) = ui_state.modal {
                m.move_priority_up();
            }
            true
        }
        KeyCode::Char('j') if vim_keys => {
            if let Some(ref mut m) = ui_state.modal {
                m.move_priority_down();
            }
            true
        }
        KeyCode::Enter => {
            apply_priority_modal_selection(modal, allow_live_actions, ui_state, prev_grid)
        }
        KeyCode::Esc => {
            ui_state.modal = None;
            *prev_grid = None;
            true
        }
        _ => false,
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_service_confirm_modal(
    code: KeyCode,
    service_name: &str,
    display_name: &str,
    start: bool,
    size: Size,
    allow_live_actions: bool,
    service_dispatch: Option<&ServiceDispatch<'_>>,
    ui_state: &mut UiState,
    prev_grid: &mut Option<Grid>,
) -> bool {
    match code {
        KeyCode::Enter | KeyCode::Char('y' | 'Y') => {
            ui_state.modal = None;
            *prev_grid = None;
            if !allow_live_actions {
                ui_state.services.action_status =
                    Some("Service actions are disabled in replay mode".to_owned());
                return true;
            }
            let Some(dispatch) = service_dispatch else {
                ui_state.services.action_status =
                    Some("Service action dispatcher unavailable".to_owned());
                return true;
            };
            if dispatch.in_flight.swap(true, Ordering::AcqRel) {
                ui_state.services.action_status =
                    Some("Another service Start/Stop request is still in progress".to_owned());
                return true;
            }
            let svc_name = service_name.to_owned();
            let disp_name = display_name.to_owned();
            let tx = dispatch.tx.clone();
            let in_flight = Arc::clone(dispatch.in_flight);
            let verb = if start { "start" } else { "stop" };
            ui_state.services.action_status = Some(format!(
                "Sending {verb} request to '{svc_name}' ({disp_name})..."
            ));

            // Dispatch SCM `StartServiceW` / `ControlService` off the UI thread because Windows
            // `ControlService` may block for up to 30 seconds while another control is pending.
            let spawn_res = std::thread::Builder::new()
                .name("btop-svc-action".to_owned())
                .spawn(move || {
                    let _guard = ResetBoolOnDrop(in_flight);
                    let result = if start {
                        start_windows_service(&svc_name)
                    } else {
                        stop_windows_service(&svc_name)
                    };
                    let _ = tx.send(ServiceActionOutcome {
                        service_name: svc_name,
                        display_name: disp_name,
                        start,
                        result,
                    });
                });
            if let Err(err) = spawn_res {
                dispatch.in_flight.store(false, Ordering::Release);
                let notice = ProcActionModal::ActionNotice {
                    title: if start {
                        "Service Start Failed".to_owned()
                    } else {
                        "Service Stop Failed".to_owned()
                    },
                    message: format!("Failed to spawn service control worker: {err}"),
                    is_error: true,
                };
                if !try_open_modal(size, ui_state, notice) {
                    ui_state.services.action_status =
                        Some(format!("Failed to spawn service control worker: {err}"));
                }
            }
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

fn drain_service_action_outcomes(
    rx: &Receiver<ServiceActionOutcome>,
    size: Size,
    sampler: Option<&Sampler>,
    ui_state: &mut UiState,
    prev_grid: &mut Option<Grid>,
) -> bool {
    let mut updated = false;
    while let Ok(outcome) = rx.try_recv() {
        updated = true;
        *prev_grid = None;
        match outcome.result {
            Ok(()) => {
                let verb = if outcome.start {
                    "Requested start for"
                } else {
                    "Requested stop for"
                };
                ui_state.services.action_status = Some(format!(
                    "{verb} '{}' ({})",
                    outcome.service_name, outcome.display_name
                ));
                if let Some(s) = sampler {
                    s.refresh_services();
                }
            }
            Err(err) => {
                let title = if outcome.start {
                    "Service Start Refused".to_owned()
                } else {
                    "Service Stop Refused".to_owned()
                };
                let msg = err.to_string();
                let notice = ProcActionModal::ActionNotice {
                    title,
                    message: msg.clone(),
                    is_error: true,
                };
                if !try_open_modal(size, ui_state, notice) {
                    ui_state.services.action_status = Some(msg);
                }
            }
        }
    }
    updated
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
        KeyCode::Char('5') => ui_state.layout.toggle_box(BoxId::Gpu(0)),
        KeyCode::Char('6') => ui_state.layout.toggle_box(BoxId::Gpu(1)),
        KeyCode::Char('7') => ui_state.layout.toggle_box(BoxId::Gpu(2)),
        KeyCode::Char('8') => ui_state.layout.toggle_box(BoxId::Gpu(3)),
        KeyCode::Char('9') => ui_state.layout.toggle_box(BoxId::Gpu(4)),
        KeyCode::Char('0') => ui_state.layout.toggle_box(BoxId::Gpu(5)),
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
            config.vim_keys,
            snapshot,
            size,
            ui_state,
            service_rows,
            allow_live_actions,
        ),
        other => handle_proc_key(
            other,
            config.vim_keys,
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
    vim_keys: bool,
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
        KeyCode::Char('k') if vim_keys => state.services.move_up(count, service_rows),
        KeyCode::Char('j') if vim_keys => state.services.move_down(count, service_rows),
        KeyCode::PageUp => state.services.page_up(count, service_rows),
        KeyCode::PageDown => state.services.page_down(count, service_rows),
        KeyCode::Home => state.services.home(count, service_rows),
        KeyCode::End => state.services.end(count, service_rows),
        KeyCode::Char('g') if vim_keys => state.services.home(count, service_rows),
        KeyCode::Char('G') if vim_keys => state.services.end(count, service_rows),
        KeyCode::Left => state.services.prev_sort(),
        KeyCode::Right => state.services.next_sort(),
        KeyCode::Char('h') if vim_keys => state.services.prev_sort(),
        KeyCode::Char('l') if vim_keys => state.services.next_sort(),
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
    key: btop_term::KeyEvent,
    vim_keys: bool,
    snapshot: &btop_model::Snapshot,
    history: &UiHistory,
    size: Size,
    state: &mut UiState,
    proc_rows: usize,
    allow_live_actions: bool,
) -> bool {
    if key.ctrl || key.alt {
        return false;
    }
    handle_proc_key(
        key.code,
        vim_keys,
        snapshot,
        history,
        size,
        state,
        proc_rows,
        allow_live_actions,
    )
}

#[allow(clippy::too_many_arguments)]
fn handle_proc_key(
    code: KeyCode,
    vim_keys: bool,
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

    if handle_proc_nav_key(
        code,
        vim_keys,
        rows_built.as_deref(),
        proc_count,
        proc_rows,
        state,
    ) {
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
        KeyCode::Char('t' | 'N') => {
            return handle_proc_action_shortcut(
                code,
                rows_built.as_deref(),
                size,
                proc_rows,
                allow_live_actions,
                state,
            );
        }
        KeyCode::Char('k') if !vim_keys => {
            return handle_proc_action_shortcut(
                KeyCode::Char('k'),
                rows_built.as_deref(),
                size,
                proc_rows,
                allow_live_actions,
                state,
            );
        }
        KeyCode::Char('K') if vim_keys => {
            return handle_proc_action_shortcut(
                KeyCode::Char('k'),
                rows_built.as_deref(),
                size,
                proc_rows,
                allow_live_actions,
                state,
            );
        }
        KeyCode::Left => state.proc.prev_sort(),
        KeyCode::Right => state.proc.next_sort(),
        KeyCode::Char('h') if vim_keys => state.proc.prev_sort(),
        KeyCode::Char('l') if vim_keys => state.proc.next_sort(),
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
    vim_keys: bool,
    rows_built: Option<&[btop_ui::DisplayProcRow<'_>]>,
    proc_count: usize,
    proc_rows: usize,
    state: &mut UiState,
) -> bool {
    match code {
        KeyCode::Up => state.proc.move_up(proc_count, proc_rows),
        KeyCode::Down => state.proc.move_down(proc_count, proc_rows),
        KeyCode::Char('k') if vim_keys => state.proc.move_up(proc_count, proc_rows),
        KeyCode::Char('j') if vim_keys => state.proc.move_down(proc_count, proc_rows),
        KeyCode::PageUp => state.proc.page_up(proc_count, proc_rows),
        KeyCode::PageDown => state.proc.page_down(proc_count, proc_rows),
        KeyCode::Home => state.proc.home(proc_count, proc_rows),
        KeyCode::End => state.proc.end(proc_count, proc_rows),
        KeyCode::Char('g') if vim_keys => state.proc.home(proc_count, proc_rows),
        KeyCode::Char('G') if vim_keys => state.proc.end(proc_count, proc_rows),
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
            false,
            &snap,
            &history,
            size,
            &mut state,
            proc_rows,
            true,
        ));
        assert!(!handle_proc_key(
            KeyCode::Char('k'),
            false,
            &snap,
            &history,
            size,
            &mut state,
            proc_rows,
            true,
        ));
        assert!(!handle_proc_key(
            KeyCode::Char('N'),
            false,
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
            false,
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
            false,
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
                btop_term::KeyEvent {
                    code,
                    ctrl,
                    alt,
                    shift: false,
                },
                false,
                &snap,
                &history,
                size,
                &mut state,
                proc_rows,
                true,
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
                key, false, &snap, &history, size, &mut state, proc_rows, false,
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
            themes_dir: None,
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
            false,
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
            false,
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
            false,
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
            false,
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
        let mut history = UiHistory::default();
        let mut cfg = Config::default();
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
            &snap,
            &mut history,
            &mut cfg,
            &mut state,
            None,
            10,
            10,
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

    #[test]
    fn service_control_outcomes_apply_asynchronously_and_guard_overlap() {
        let size = Size {
            width: 120,
            height: 40,
        };
        let mut state = UiState::default();
        state.services.active = true;
        let mut prev_grid = None;

        // Overlapping confirm while another control call is already in flight returns immediately.
        let (tx, rx) = mpsc::channel();
        let in_flight = Arc::new(AtomicBool::new(true));
        let dispatch = ServiceDispatch {
            tx: &tx,
            in_flight: &in_flight,
        };
        state.modal = Some(ProcActionModal::ConfirmServiceAction {
            service_name: "W32Time".to_owned(),
            display_name: "Windows Time".to_owned(),
            start: false,
        });
        assert!(handle_modal_key(
            btop_term::KeyEvent {
                code: KeyCode::Enter,
                ctrl: false,
                alt: false,
                shift: false,
            },
            false,
            size,
            true,
            Some(&dispatch),
            &mut state,
            &mut prev_grid,
        ));
        assert!(state.modal.is_none());
        assert!(
            state
                .services
                .action_status
                .as_deref()
                .is_some_and(|s| s.contains("still in progress"))
        );

        // Draining a completed success outcome updates `services.action_status`.
        tx.send(ServiceActionOutcome {
            service_name: "W32Time".to_owned(),
            display_name: "Windows Time".to_owned(),
            start: true,
            result: Ok(()),
        })
        .expect("send ok outcome");
        assert!(drain_service_action_outcomes(
            &rx,
            size,
            None,
            &mut state,
            &mut prev_grid,
        ));
        assert!(
            state
                .services
                .action_status
                .as_deref()
                .is_some_and(|s| s.contains("Requested start for 'W32Time'"))
        );

        // Draining an error outcome opens a refusal notice modal cleanly.
        tx.send(ServiceActionOutcome {
            service_name: "W32Time".to_owned(),
            display_name: "Windows Time".to_owned(),
            start: false,
            result: Err(ServiceActionError::AccessDenied("W32Time".to_owned())),
        })
        .expect("send err outcome");
        assert!(drain_service_action_outcomes(
            &rx,
            size,
            None,
            &mut state,
            &mut prev_grid,
        ));
        assert!(matches!(
            state.modal,
            Some(ProcActionModal::ActionNotice { ref title, is_error: true, .. })
                if title == "Service Stop Refused"
        ));
    }

    #[test]
    fn gpu_hotkeys_5_through_0_toggle_gpu0_through_gpu5() {
        let mut state = UiState::default();
        let mut prev_grid = None;
        for (ch, idx) in [('5', 0u8), ('6', 1), ('7', 2), ('8', 3), ('9', 4), ('0', 5)] {
            assert!(handle_layout_key(
                KeyCode::Char(ch),
                &mut state,
                &mut prev_grid
            ));
            assert!(state.layout.shown_boxes.contains(&BoxId::Gpu(idx)));
            assert!(handle_layout_key(
                KeyCode::Char(ch),
                &mut state,
                &mut prev_grid
            ));
            assert!(!state.layout.shown_boxes.contains(&BoxId::Gpu(idx)));
        }
    }

    #[test]
    fn overlay_menus_honor_background_update_and_vim_keys() {
        let mut cfg = Config {
            background_update: false,
            vim_keys: false,
            ..Config::default()
        };
        let mut state = UiState::from_config(&cfg);
        let mut prev_grid = None;

        // `h` opens Help when vim_keys=False; `H` opens Help when vim_keys=True.
        assert!(try_open_menu_shortcut(
            KeyCode::Char('h'),
            &mut cfg,
            &mut state,
            &mut prev_grid
        ));
        assert!(matches!(state.menu, Some(OverlayMenu::Help(_))));
        state.menu = None;
        cfg.vim_keys = true;
        assert!(!try_open_menu_shortcut(
            KeyCode::Char('h'),
            &mut cfg,
            &mut state,
            &mut prev_grid
        ));
        assert!(try_open_menu_shortcut(
            KeyCode::Char('H'),
            &mut cfg,
            &mut state,
            &mut prev_grid
        ));
        assert!(matches!(state.menu, Some(OverlayMenu::Help(_))));

        // Background updates pause while menu is open and `background_update = false`.
        let source = Box::new(btop_collect::StubLiveSource::new(100));
        let sampler = Sampler::spawn(source, 100, None).expect("spawn sampler");
        let mut last_gen = sampler.generation();
        let mut last_tel_gen = sampler.telemetry_generation();
        let mut current = sampler.latest();
        let mut history = UiHistory::default();
        std::thread::sleep(Duration::from_millis(240));
        assert!(
            sampler.generation() > last_gen,
            "sampler should advance in background"
        );
        assert!(
            !poll_sampler_updates(
                &sampler,
                &mut last_gen,
                &mut last_tel_gen,
                &mut current,
                &mut history,
                &mut state,
                &cfg,
            ),
            "background_update=false must freeze UI snapshot updates while a menu is open"
        );
        state.menu = None;
        assert!(
            poll_sampler_updates(
                &sampler,
                &mut last_gen,
                &mut last_tel_gen,
                &mut current,
                &mut history,
                &mut state,
                &cfg,
            ),
            "closing menu must resume snapshot updates immediately"
        );
        let _ = sampler.stop();
    }

    #[test]
    fn overlay_menus_mouse_and_messagebox_reset_flow() {
        let snap = sample_snapshot_with_proc();
        let history = UiHistory::default();
        let size = Size {
            width: 120,
            height: 40,
        };
        let mut cfg = Config::default();
        let mut state = UiState::from_config(&cfg);

        // Main menu mouse click on OPTIONS opens Options.
        state.menu = Some(OverlayMenu::main());
        let main_grid = btop_ui::render_frame(&snap, &history, &state, &cfg, size);
        let (opt_x, opt_y) = find_hit(&main_grid, &HitAction::MenuItem(0));
        let mut prev_grid = Some(main_grid);
        assert_eq!(
            handle_menu_mouse(
                btop_term::MouseEvent {
                    col: opt_x,
                    row: opt_y,
                    kind: MouseKind::Down(MouseButton::Left),
                },
                size,
                None,
                &snap,
                &mut cfg,
                None,
                &mut state,
                &mut prev_grid,
            ),
            LoopAction::Redraw
        );
        assert!(matches!(state.menu, Some(OverlayMenu::Options(_))));

        // Right arrow cycles color_theme live to dracula.
        let plain_key = |code| btop_term::KeyEvent {
            code,
            ctrl: false,
            alt: false,
            shift: false,
        };
        assert_eq!(
            handle_menu_key(
                plain_key(KeyCode::Right),
                size,
                None,
                &snap,
                &mut cfg,
                None,
                &mut state,
                &mut prev_grid,
            ),
            LoopAction::Redraw
        );
        assert_eq!(cfg.color_theme, "dracula");

        // Pressing 'r' opens Reset Options MessageBox; toggling Left keeps return_to intact;
        // clicking Yes resets color_theme to Default and returns cleanly to Options.
        assert_eq!(
            handle_menu_key(
                plain_key(KeyCode::Char('r')),
                size,
                None,
                &snap,
                &mut cfg,
                None,
                &mut state,
                &mut prev_grid,
            ),
            LoopAction::Redraw
        );
        assert_eq!(
            handle_menu_key(
                plain_key(KeyCode::Left),
                size,
                None,
                &snap,
                &mut cfg,
                None,
                &mut state,
                &mut prev_grid,
            ),
            LoopAction::Redraw
        );
        let msg_grid = btop_ui::render_frame(&snap, &history, &state, &cfg, size);
        let (yes_x, yes_y) = find_hit(&msg_grid, &HitAction::MessageBoxButton(true));
        prev_grid = Some(msg_grid);
        assert_eq!(
            handle_menu_mouse(
                btop_term::MouseEvent {
                    col: yes_x,
                    row: yes_y,
                    kind: MouseKind::Down(MouseButton::Left),
                },
                size,
                None,
                &snap,
                &mut cfg,
                None,
                &mut state,
                &mut prev_grid,
            ),
            LoopAction::Redraw
        );
        assert_eq!(cfg.color_theme, "Default");
        assert!(matches!(state.menu, Some(OverlayMenu::Options(_))));
    }

    fn find_hit(grid: &Grid, target: &HitAction) -> (u16, u16) {
        let size = grid.size();
        (0..size.height)
            .find_map(|y| {
                (0..size.width)
                    .find_map(|x| (grid.hit_map.hit_test(x, y) == Some(target)).then_some((x, y)))
            })
            .expect("expected hit region")
    }

    #[test]
    fn cosmetic_option_edit_preserves_paused_proc_net_baselines_and_manual_layout() {
        let snap = sample_snapshot_with_proc();
        let history = UiHistory::default();
        let size = Size {
            width: 120,
            height: 40,
        };
        let mut cfg = Config::default();
        let mut state = UiState::from_config(&cfg);
        let target_id = btop_model::ProcessIdentity {
            pid: 4216,
            create_time_100ns: 1000,
        };

        // Establish rich session state: paused process list, followed/detailed process,
        // collapsed tree node, selected net adapter + session baseline, and manual layout toggle.
        if let DataState::Available(ref procs) = snap.processes {
            state.proc.toggle_pause(Some(procs), Some(&history));
        }
        state.proc.followed = Some(target_id);
        state.proc.detailed = Some(target_id);
        state.proc.collapsed.insert(target_id);
        state.net.selected_luid = Some(0xDEAD_BEEF);
        state.net.session_baselines.insert(0xDEAD_BEEF, (1000, 500));
        assert!(state.layout.toggle_box(BoxId::Gpu(0)));
        assert!(state.layout.shown_boxes.contains(&BoxId::Gpu(0)));

        // Open Options and cycle `color_theme` (a cosmetic edit).
        state.menu = Some(OverlayMenu::options());
        let mut prev_grid = None;
        assert_eq!(
            handle_menu_key(
                btop_term::KeyEvent {
                    code: KeyCode::Right,
                    ctrl: false,
                    alt: false,
                    shift: false,
                },
                size,
                None,
                &snap,
                &mut cfg,
                None,
                &mut state,
                &mut prev_grid,
            ),
            LoopAction::Redraw
        );
        assert_eq!(cfg.color_theme, "dracula");

        // All unrelated session state must remain completely intact!
        assert!(state.proc.paused);
        assert!(state.proc.frozen_processes.is_some());
        assert_eq!(state.proc.followed, Some(target_id));
        assert_eq!(state.proc.detailed, Some(target_id));
        assert!(state.proc.collapsed.contains(&target_id));
        assert_eq!(state.net.selected_luid, Some(0xDEAD_BEEF));
        assert_eq!(
            state.net.session_baselines.get(&0xDEAD_BEEF),
            Some(&(1000, 500))
        );
        assert!(state.layout.shown_boxes.contains(&BoxId::Gpu(0)));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn mouse_hit_testing_covers_all_panel_hotkeys_rows_headers_wheel_and_disable_mouse() {
        let mut snap = btop_model::fixtures::win11_16core_dual_gpu_desktop()[0].clone();
        snap.services = DataState::Available(vec![
            btop_model::ServiceEntry {
                name: "W32Time".to_owned(),
                display_name: "Windows Time".to_owned(),
                state: "Running".to_owned(),
                start_type: "Auto".to_owned(),
                pid: Some(1204),
            },
            btop_model::ServiceEntry {
                name: "Spooler".to_owned(),
                display_name: "Print Spooler".to_owned(),
                state: "Stopped".to_owned(),
                start_type: "Manual".to_owned(),
                pid: None,
            },
        ]);
        let mut history = UiHistory::from_snapshot(&snap);
        let size = Size {
            width: 140,
            height: 44,
        };
        let mut cfg = Config {
            shown_boxes: "cpu mem net proc".to_owned(),
            update_ms: 2000,
            ..Config::default()
        };
        let mut state = UiState::from_config(&cfg);
        state.net.observe_snapshot(&snap);

        let click_action = |action: &HitAction,
                            button: MouseButton,
                            cfg: &mut Config,
                            state: &mut UiState,
                            history: &mut UiHistory| {
            let grid = btop_ui::render_frame(&snap, history, state, cfg, size);
            let (col, row) = find_hit(&grid, action);
            let mut prev = Some(grid);
            let proc_rows = visible_proc_rows(size, state);
            let service_rows = visible_service_rows(size, state);
            assert!(
                handle_mouse_event(
                    btop_term::MouseEvent {
                        col,
                        row,
                        kind: MouseKind::Down(button),
                    },
                    size,
                    &snap,
                    history,
                    cfg,
                    state,
                    None,
                    proc_rows,
                    service_rows,
                    &mut prev,
                ),
                "expected click on {action:?} to be handled"
            );
        };

        // CPU cadence `-` and `+` buttons.
        click_action(
            &HitAction::AdjustUpdateMs(-100),
            MouseButton::Left,
            &mut cfg,
            &mut state,
            &mut history,
        );
        assert_eq!(cfg.update_ms, 1900);
        click_action(
            &HitAction::AdjustUpdateMs(100),
            MouseButton::Left,
            &mut cfg,
            &mut state,
            &mut history,
        );
        assert_eq!(cfg.update_ms, 2000);

        // MEM `disks` and `io` hotkeys.
        assert!(state.show_disks);
        click_action(
            &HitAction::ToggleIoMode,
            MouseButton::Left,
            &mut cfg,
            &mut state,
            &mut history,
        );
        assert!(state.io_mode);
        click_action(
            &HitAction::ToggleDisks,
            MouseButton::Left,
            &mut cfg,
            &mut state,
            &mut history,
        );
        assert!(!state.show_disks);
        assert!(!state.io_mode);

        // NET `auto`, `ysync`, `zero`, `<b`, `n>` controls.
        let initial_net_idx = state.net.selected_index;
        click_action(
            &HitAction::NetToggleAuto,
            MouseButton::Left,
            &mut cfg,
            &mut state,
            &mut history,
        );
        assert!(!state.net.auto_scale);
        click_action(
            &HitAction::NetToggleSync,
            MouseButton::Left,
            &mut cfg,
            &mut state,
            &mut history,
        );
        assert!(!state.net.synced_scale);
        click_action(
            &HitAction::NetResetTotals,
            MouseButton::Left,
            &mut cfg,
            &mut state,
            &mut history,
        );
        click_action(
            &HitAction::NetNextIface,
            MouseButton::Left,
            &mut cfg,
            &mut state,
            &mut history,
        );
        assert_ne!(state.net.selected_index, initial_net_idx);
        click_action(
            &HitAction::NetPrevIface,
            MouseButton::Left,
            &mut cfg,
            &mut state,
            &mut history,
        );
        assert_eq!(state.net.selected_index, initial_net_idx);

        // PROC sort tag (`<`, middle reverse, `>`) and column headers (`Pid`, `Memory`).
        let before_sort = state.proc.sort;
        click_action(
            &HitAction::ProcNextSort,
            MouseButton::Left,
            &mut cfg,
            &mut state,
            &mut history,
        );
        assert_eq!(state.proc.sort, before_sort.next());
        click_action(
            &HitAction::ProcPrevSort,
            MouseButton::Left,
            &mut cfg,
            &mut state,
            &mut history,
        );
        assert_eq!(state.proc.sort, before_sort);
        click_action(
            &HitAction::ProcToggleReverse,
            MouseButton::Left,
            &mut cfg,
            &mut state,
            &mut history,
        );
        assert!(state.proc.reversed);
        click_action(
            &HitAction::ProcSortColumn("memory".to_owned()),
            MouseButton::Left,
            &mut cfg,
            &mut state,
            &mut history,
        );
        assert_eq!(state.proc.sort, ProcSort::Memory);
        assert!(!state.proc.reversed);
        click_action(
            &HitAction::ProcSortColumn("memory".to_owned()),
            MouseButton::Left,
            &mut cfg,
            &mut state,
            &mut history,
        );
        assert!(state.proc.reversed);

        // Process row click selects a row; clicking an already-selected row toggles detail overlay.
        let procs = state
            .proc
            .active_processes(&snap)
            .expect("processes")
            .to_vec();
        let (rows, _) = build_visible_rows(&procs, &state.proc);
        assert!(rows.len() >= 2);
        let second_pid = rows[1].entry.identity.pid;
        click_action(
            &HitAction::SelectProcess(second_pid),
            MouseButton::Left,
            &mut cfg,
            &mut state,
            &mut history,
        );
        assert_eq!(state.proc.selected, 1);
        assert!(state.proc.detailed.is_none());
        click_action(
            &HitAction::SelectProcess(second_pid),
            MouseButton::Left,
            &mut cfg,
            &mut state,
            &mut history,
        );
        assert_eq!(state.proc.detailed, Some(rows[1].entry.identity));
        click_action(
            &HitAction::SelectProcess(second_pid),
            MouseButton::Left,
            &mut cfg,
            &mut state,
            &mut history,
        );
        assert!(state.proc.detailed.is_none());

        // Mouse wheel scrolls inside the PROC panel.
        let proc_rect = compute_layout(size, &state.layout)
            .expect("layout")
            .proc()
            .expect("proc rect");
        let proc_rows = visible_proc_rows(size, &state);
        let mut prev = None;
        assert!(handle_mouse_event(
            btop_term::MouseEvent {
                col: proc_rect.x + 2,
                row: proc_rect.y + 3,
                kind: MouseKind::ScrollDown,
            },
            size,
            &snap,
            &mut history,
            &mut cfg,
            &mut state,
            None,
            proc_rows,
            10,
            &mut prev,
        ));
        assert_eq!(state.proc.selected, 2);
        assert!(handle_mouse_event(
            btop_term::MouseEvent {
                col: proc_rect.x + 2,
                row: proc_rect.y + 3,
                kind: MouseKind::ScrollUp,
            },
            size,
            &snap,
            &mut history,
            &mut cfg,
            &mut state,
            None,
            proc_rows,
            10,
            &mut prev,
        ));
        assert_eq!(state.proc.selected, 1);

        // Toggle to Services via click, click column sort & service row, and wheel scroll.
        click_action(
            &HitAction::ToggleServicesView,
            MouseButton::Left,
            &mut cfg,
            &mut state,
            &mut history,
        );
        assert!(state.services.active);
        click_action(
            &HitAction::ProcSortColumn("name".to_owned()),
            MouseButton::Left,
            &mut cfg,
            &mut state,
            &mut history,
        );
        assert_eq!(state.services.sort, ServiceSort::Name);
        click_action(
            &HitAction::SelectService(1),
            MouseButton::Left,
            &mut cfg,
            &mut state,
            &mut history,
        );
        assert_eq!(state.services.selected, 1);

        // ToggleBox (`net`) sets manual layout (`None`); CyclePreset clicks cycle None -> 0 -> 1 -> 0.
        click_action(
            &HitAction::ToggleBox("net".to_owned()),
            MouseButton::Left,
            &mut cfg,
            &mut state,
            &mut history,
        );
        assert!(!state.layout.shown_boxes.contains(&BoxId::Net));
        assert_eq!(state.layout.active_preset, None);
        click_action(
            &HitAction::CyclePreset,
            MouseButton::Left,
            &mut cfg,
            &mut state,
            &mut history,
        );
        assert_eq!(state.layout.active_preset, Some(0));
        click_action(
            &HitAction::CyclePreset,
            MouseButton::Left,
            &mut cfg,
            &mut state,
            &mut history,
        );
        assert_eq!(state.layout.active_preset, Some(1));
        click_action(
            &HitAction::CyclePreset,
            MouseButton::Right,
            &mut cfg,
            &mut state,
            &mut history,
        );
        assert_eq!(state.layout.active_preset, Some(0));

        // `disable_mouse = true` blocks clicks and scroll events completely.
        cfg.disable_mouse = true;
        let grid = btop_ui::render_frame(&snap, &history, &state, &cfg, size);
        let (col, row) = find_hit(&grid, &HitAction::ToggleBox("cpu".to_owned()));
        let mut prev_disabled = Some(grid);
        assert!(!handle_mouse_event(
            btop_term::MouseEvent {
                col,
                row,
                kind: MouseKind::Down(MouseButton::Left),
            },
            size,
            &snap,
            &mut history,
            &mut cfg,
            &mut state,
            None,
            proc_rows,
            10,
            &mut prev_disabled,
        ));
        assert!(state.layout.shown_boxes.contains(&BoxId::Cpu));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn vim_keys_enables_hjkl_g_and_remaps_help_and_kill() {
        let snap = btop_model::fixtures::win11_16core_dual_gpu_desktop()[0].clone();
        let history = UiHistory::from_snapshot(&snap);
        let size = Size {
            width: 120,
            height: 40,
        };
        let mut cfg = Config {
            vim_keys: true,
            ..Config::default()
        };
        let mut state = UiState::from_config(&cfg);
        let proc_rows = visible_proc_rows(size, &state);
        let total = state.proc.active_processes(&snap).expect("procs").len();
        assert!(total >= 3);

        // `j` moves down, `k` moves up and MUST NOT open ConfirmTerminate!
        assert!(handle_proc_key(
            KeyCode::Char('j'),
            true,
            &snap,
            &history,
            size,
            &mut state,
            proc_rows,
            true,
        ));
        assert_eq!(state.proc.selected, 1);
        assert!(handle_proc_key(
            KeyCode::Char('k'),
            true,
            &snap,
            &history,
            size,
            &mut state,
            proc_rows,
            true,
        ));
        assert_eq!(state.proc.selected, 0);
        assert!(
            state.modal.is_none(),
            "k must move up rather than arming Terminate when vim_keys = true"
        );

        // `G` jumps to end, `g` jumps to top.
        assert!(handle_proc_key(
            KeyCode::Char('G'),
            true,
            &snap,
            &history,
            size,
            &mut state,
            proc_rows,
            true,
        ));
        assert_eq!(state.proc.selected, total - 1);
        assert!(handle_proc_key(
            KeyCode::Char('g'),
            true,
            &snap,
            &history,
            size,
            &mut state,
            proc_rows,
            true,
        ));
        assert_eq!(state.proc.selected, 0);

        // `l` / `h` cycle sort column forward / backward.
        let orig_sort = state.proc.sort;
        assert!(handle_proc_key(
            KeyCode::Char('l'),
            true,
            &snap,
            &history,
            size,
            &mut state,
            proc_rows,
            true,
        ));
        assert_eq!(state.proc.sort, orig_sort.next());
        assert!(handle_proc_key(
            KeyCode::Char('h'),
            true,
            &snap,
            &history,
            size,
            &mut state,
            proc_rows,
            true,
        ));
        assert_eq!(state.proc.sort, orig_sort);

        // `K` triggers Terminate confirmation when vim_keys = true.
        assert!(handle_proc_key(
            KeyCode::Char('K'),
            true,
            &snap,
            &history,
            size,
            &mut state,
            proc_rows,
            true,
        ));
        assert!(matches!(
            state.modal,
            Some(ProcActionModal::ConfirmTerminate { .. })
        ));
        state.modal = None;

        // `h` / `l` / `g` / `G` also work in Options and Help menus under vim_keys.
        let mut prev_grid = None;
        state.menu = Some(OverlayMenu::options());
        let plain = |code| btop_term::KeyEvent {
            code,
            ctrl: false,
            alt: false,
            shift: false,
        };
        assert_eq!(
            handle_menu_key(
                plain(KeyCode::Char('l')),
                size,
                None,
                &snap,
                &mut cfg,
                None,
                &mut state,
                &mut prev_grid,
            ),
            LoopAction::Redraw
        );
        assert_eq!(cfg.color_theme, "dracula");
        assert_eq!(
            handle_menu_key(
                plain(KeyCode::Char('h')),
                size,
                None,
                &snap,
                &mut cfg,
                None,
                &mut state,
                &mut prev_grid,
            ),
            LoopAction::Redraw
        );
        assert_eq!(cfg.color_theme, "Default");

        let compact_help = Size {
            width: 80,
            height: 24,
        };
        state.menu = Some(OverlayMenu::help());
        assert_eq!(
            handle_menu_key(
                plain(KeyCode::Char('G')),
                compact_help,
                None,
                &snap,
                &mut cfg,
                None,
                &mut state,
                &mut prev_grid,
            ),
            LoopAction::Redraw
        );
        assert!(matches!(state.menu, Some(OverlayMenu::Help(ref h)) if h.scroll_offset > 0));
        assert_eq!(
            handle_menu_key(
                plain(KeyCode::Char('g')),
                compact_help,
                None,
                &snap,
                &mut cfg,
                None,
                &mut state,
                &mut prev_grid,
            ),
            LoopAction::Redraw
        );
        assert!(matches!(state.menu, Some(OverlayMenu::Help(ref h)) if h.scroll_offset == 0));
    }

    #[test]
    fn save_config_if_enabled_syncs_ui_state_and_respects_flags() {
        let path = std::env::temp_dir().join(format!(
            "btop-save-exit-{}-{}.conf",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let snap = btop_model::fixtures::win11_16core_dual_gpu_desktop()[0].clone();
        let mut cfg = Config::parse("use_fstab = True\nsave_config_on_exit = True\n")
            .expect("parse initial conf");
        let mut state = UiState::from_config(&cfg);
        state.net.observe_snapshot(&snap);
        state.net.select_next(&snap);
        let expected_iface = state
            .net
            .selected_iface_name
            .clone()
            .expect("selected adapter name after cycling");
        state.proc.toggle_tree();
        state.proc.toggle_reverse();
        state.net.toggle_auto();

        // Replay mode (`allow_live_actions = false`) never writes to disk.
        assert_eq!(
            save_config_if_enabled(&mut cfg, &state, Some(&path), false).expect("replay no-op"),
            None
        );
        assert!(!path.exists());

        // `save_config_on_exit = false` never writes to disk.
        cfg.save_config_on_exit = false;
        assert_eq!(
            save_config_if_enabled(&mut cfg, &state, Some(&path), true).expect("disabled no-op"),
            None
        );
        assert!(!path.exists());

        // Enabled live exit persists synchronized UI state and unknown Unix keys cleanly.
        cfg.save_config_on_exit = true;
        let saved = save_config_if_enabled(&mut cfg, &state, Some(&path), true)
            .expect("save live conf")
            .expect("saved path");
        assert_eq!(saved, path);

        let reloaded = load_config(Some(&path), &ConfigOverrides::default()).expect("reload conf");
        let _ = std::fs::remove_file(&path);
        assert!(reloaded.proc_tree);
        assert!(reloaded.proc_reversed);
        assert!(!reloaded.net_auto);
        assert_eq!(reloaded.net_iface, expected_iface);
        assert_eq!(
            reloaded
                .preserved_unknown
                .get("use_fstab")
                .map(String::as_str),
            Some("True")
        );
    }

    #[test]
    fn focus_gained_ctrl_l_and_periodic_interval_force_full_redraw() {
        let size = Size {
            width: 80,
            height: 24,
        };
        let mut prev_grid = Some(Grid::new(size));
        assert!(try_handle_force_redraw(
            TermEvent::FocusGained,
            &mut prev_grid
        ));
        assert!(prev_grid.is_none());

        prev_grid = Some(Grid::new(size));
        let ctrl_l = btop_term::KeyEvent {
            code: KeyCode::Char('l'),
            ctrl: true,
            alt: false,
            shift: false,
        };
        assert!(try_handle_force_redraw(
            TermEvent::Key(ctrl_l),
            &mut prev_grid
        ));
        assert!(prev_grid.is_none());

        // Plain 'l' and Alt+Ctrl+L do not hijack navigation or AltGr combinations.
        prev_grid = Some(Grid::new(size));
        let plain_l = btop_term::KeyEvent {
            code: KeyCode::Char('l'),
            ctrl: false,
            alt: false,
            shift: false,
        };
        assert!(!try_handle_force_redraw(
            TermEvent::Key(plain_l),
            &mut prev_grid
        ));
        assert!(prev_grid.is_some());

        let alt_ctrl_l = btop_term::KeyEvent {
            code: KeyCode::Char('l'),
            ctrl: true,
            alt: true,
            shift: false,
        };
        assert!(!try_handle_force_redraw(
            TermEvent::Key(alt_ctrl_l),
            &mut prev_grid
        ));
        assert!(prev_grid.is_some());

        prev_grid = Some(Grid::new(size));
        assert!(!should_force_periodic_full_redraw(
            Duration::from_millis(4999),
            FULL_REDRAW_INTERVAL,
            &mut prev_grid,
        ));
        assert!(prev_grid.is_some());
        assert!(should_force_periodic_full_redraw(
            Duration::from_secs(5),
            FULL_REDRAW_INTERVAL,
            &mut prev_grid,
        ));
        assert!(prev_grid.is_none());
        prev_grid = Some(Grid::new(size));
        assert!(should_force_periodic_full_redraw(
            Duration::from_secs(8),
            FULL_REDRAW_INTERVAL,
            &mut prev_grid,
        ));
        assert!(prev_grid.is_none());
    }
}
