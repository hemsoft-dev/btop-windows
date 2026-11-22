//! Keyboard, mouse, and live configuration synchronization for the overlay menu stack
//! (Main, Options, Help, and `MessageBox`).

use btop_collect::{FreqMode, Sampler};
use btop_config::Config;
use btop_model::Snapshot;
use btop_term::{Grid, HitAction, KeyCode, KeyEvent, MouseButton, MouseEvent, MouseKind, Size};
use btop_ui::{
    LayoutState, MainMenuChoice, MessageBoxAction, MessageBoxResult, MessageBoxState, NetViewState,
    OptionCategory, OptionKind, OverlayMenu, ProcViewState, UiState, help_visible_rows,
    options_visible_rows,
};

/// Result of handling an input event inside the main TUI event loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopAction {
    None,
    Redraw,
    Quit,
}

/// Checks if an unmodified shortcut opens an overlay menu (`m`, `F2`/`o`, `F1`/`?`/`h` or `H`).
pub fn try_open_menu_shortcut(
    code: KeyCode,
    config: &Config,
    ui_state: &mut UiState,
    prev_grid: &mut Option<Grid>,
) -> bool {
    let opened = match code {
        KeyCode::Char('m') => Some(OverlayMenu::main()),
        KeyCode::F(2) | KeyCode::Char('o') => Some(OverlayMenu::options()),
        KeyCode::F(1) | KeyCode::Char('?') => Some(OverlayMenu::help()),
        KeyCode::Char('h') if !config.vim_keys => Some(OverlayMenu::help()),
        KeyCode::Char('H') if config.vim_keys => Some(OverlayMenu::help()),
        _ => None,
    };
    if let Some(menu) = opened {
        ui_state.menu = Some(menu);
        *prev_grid = None;
        true
    } else {
        false
    }
}

/// Handles keyboard input while an [`OverlayMenu`] is active.
pub fn handle_menu_key(
    key: KeyEvent,
    size: Size,
    sampler: Option<&Sampler>,
    snapshot: &Snapshot,
    config: &mut Config,
    ui_state: &mut UiState,
    prev_grid: &mut Option<Grid>,
) -> LoopAction {
    if key.ctrl || key.alt {
        return LoopAction::None;
    }
    let Some(mut menu) = ui_state.menu.take() else {
        return LoopAction::None;
    };

    let mut config_changed = false;
    let action = match &mut menu {
        OverlayMenu::Main(state) => handle_main_menu_key(key.code, config.vim_keys, state),
        OverlayMenu::Options(state) => handle_options_key(
            key.code,
            key.shift,
            size,
            config,
            state,
            &mut config_changed,
        ),
        OverlayMenu::Help(state) => handle_help_key(key.code, config.vim_keys, size, state),
        OverlayMenu::MessageBox { dialog, .. } => {
            let result = dialog.handle_key(key.code);
            return apply_messagebox_result(
                result, sampler, snapshot, config, ui_state, prev_grid, menu,
            );
        }
    };

    if config_changed {
        sync_runtime_from_config(config, sampler, snapshot, ui_state);
    }

    match action {
        MenuKeyOutcome::KeepOpen(changed) => {
            ui_state.menu = Some(menu);
            if changed || config_changed {
                *prev_grid = None;
                LoopAction::Redraw
            } else {
                LoopAction::None
            }
        }
        MenuKeyOutcome::Replace(next_menu) => {
            ui_state.menu = Some(next_menu);
            *prev_grid = None;
            LoopAction::Redraw
        }
        MenuKeyOutcome::Close => {
            ui_state.menu = None;
            *prev_grid = None;
            LoopAction::Redraw
        }
        MenuKeyOutcome::Quit => LoopAction::Quit,
    }
}

/// Handles mouse click and scroll events while an [`OverlayMenu`] is active.
pub fn handle_menu_mouse(
    mouse: MouseEvent,
    size: Size,
    sampler: Option<&Sampler>,
    snapshot: &Snapshot,
    config: &mut Config,
    ui_state: &mut UiState,
    prev_grid: &mut Option<Grid>,
) -> LoopAction {
    if ui_state.menu.is_none() {
        return LoopAction::None;
    }

    if matches!(mouse.kind, MouseKind::ScrollUp | MouseKind::ScrollDown) {
        return handle_menu_wheel(mouse.kind == MouseKind::ScrollUp, size, ui_state, prev_grid);
    }

    if mouse.kind != MouseKind::Down(MouseButton::Left) {
        return LoopAction::None;
    }

    let hit = prev_grid
        .as_ref()
        .and_then(|g| g.hit_map.hit_test(mouse.col, mouse.row))
        .cloned();
    let Some(hit) = hit else {
        return LoopAction::None;
    };

    if hit == HitAction::MenuClose {
        ui_state.menu = None;
        *prev_grid = None;
        return LoopAction::Redraw;
    }

    let Some(mut menu) = ui_state.menu.take() else {
        return LoopAction::None;
    };
    let mut config_changed = false;

    match (&mut menu, hit) {
        (OverlayMenu::Main(_), HitAction::MenuItem(idx)) => match MainMenuChoice::ALL.get(idx) {
            Some(MainMenuChoice::Options) => {
                ui_state.menu = Some(OverlayMenu::options());
                *prev_grid = None;
                return LoopAction::Redraw;
            }
            Some(MainMenuChoice::Help) => {
                ui_state.menu = Some(OverlayMenu::help());
                *prev_grid = None;
                return LoopAction::Redraw;
            }
            Some(MainMenuChoice::Quit) => return LoopAction::Quit,
            None => {}
        },
        (OverlayMenu::Options(opts), HitAction::MenuCategory(idx)) => {
            if let Some(&cat) = OptionCategory::ALL.get(idx) {
                opts.set_category(cat);
                ui_state.menu = Some(menu);
                *prev_grid = None;
                return LoopAction::Redraw;
            }
        }
        (OverlayMenu::Options(opts), HitAction::MenuItem(idx)) => {
            let vis = options_visible_rows(size);
            if opts.selected == idx && opts.editing.is_none() {
                if let Some(opt) = opts.selected_option() {
                    match opt.kind() {
                        OptionKind::Bool | OptionKind::Enum => {
                            config_changed = opt.step(config, 1);
                        }
                        OptionKind::Text | OptionKind::Int => {
                            let _ = opts.begin_edit(config);
                        }
                    }
                }
            } else {
                opts.cancel_edit();
                opts.select_index(idx, vis);
            }
            if config_changed {
                sync_runtime_from_config(config, sampler, snapshot, ui_state);
            }
            ui_state.menu = Some(menu);
            *prev_grid = None;
            return LoopAction::Redraw;
        }
        (OverlayMenu::MessageBox { dialog, .. }, HitAction::MessageBoxButton(confirm)) => {
            let result = dialog.handle_button_click(confirm);
            return apply_messagebox_result(
                result, sampler, snapshot, config, ui_state, prev_grid, menu,
            );
        }
        _ => {}
    }

    ui_state.menu = Some(menu);
    LoopAction::None
}

fn handle_menu_wheel(
    up: bool,
    size: Size,
    ui_state: &mut UiState,
    prev_grid: &mut Option<Grid>,
) -> LoopAction {
    let Some(ref mut menu) = ui_state.menu else {
        return LoopAction::None;
    };
    match menu {
        OverlayMenu::Options(opts) if opts.editing.is_none() => {
            let vis = options_visible_rows(size);
            if up {
                opts.move_up(vis);
            } else {
                opts.move_down(vis);
            }
            *prev_grid = None;
            LoopAction::Redraw
        }
        OverlayMenu::Help(help) => {
            let vis = help_visible_rows(size);
            if up {
                help.scroll_up(3);
            } else {
                help.scroll_down(3, vis);
            }
            *prev_grid = None;
            LoopAction::Redraw
        }
        _ => LoopAction::None,
    }
}

enum MenuKeyOutcome {
    KeepOpen(bool),
    Replace(OverlayMenu),
    Close,
    Quit,
}

fn handle_main_menu_key(
    code: KeyCode,
    vim_keys: bool,
    state: &mut btop_ui::MainMenuState,
) -> MenuKeyOutcome {
    match code {
        KeyCode::Esc | KeyCode::Char('m') => MenuKeyOutcome::Close,
        KeyCode::Up => {
            state.move_up();
            MenuKeyOutcome::KeepOpen(true)
        }
        KeyCode::Down => {
            state.move_down();
            MenuKeyOutcome::KeepOpen(true)
        }
        KeyCode::Char('k') if vim_keys => {
            state.move_up();
            MenuKeyOutcome::KeepOpen(true)
        }
        KeyCode::Char('j') if vim_keys => {
            state.move_down();
            MenuKeyOutcome::KeepOpen(true)
        }
        KeyCode::F(2) | KeyCode::Char('o') => MenuKeyOutcome::Replace(OverlayMenu::options()),
        KeyCode::F(1) | KeyCode::Char('?' | 'h') => MenuKeyOutcome::Replace(OverlayMenu::help()),
        KeyCode::Char('q') => MenuKeyOutcome::Quit,
        KeyCode::Enter | KeyCode::Char(' ') => match state.selected_choice() {
            MainMenuChoice::Options => MenuKeyOutcome::Replace(OverlayMenu::options()),
            MainMenuChoice::Help => MenuKeyOutcome::Replace(OverlayMenu::help()),
            MainMenuChoice::Quit => MenuKeyOutcome::Quit,
        },
        _ => MenuKeyOutcome::KeepOpen(false),
    }
}

fn handle_options_text_edit_key(
    code: KeyCode,
    config: &mut Config,
    state: &mut btop_ui::OptionsState,
    config_changed: &mut bool,
) -> Option<MenuKeyOutcome> {
    let editor = state.editing.as_mut()?;
    let outcome = match code {
        KeyCode::Esc => {
            state.cancel_edit();
            MenuKeyOutcome::KeepOpen(true)
        }
        KeyCode::Enter => {
            if state.commit_edit(config).is_ok() {
                *config_changed = true;
            }
            MenuKeyOutcome::KeepOpen(true)
        }
        other => {
            let changed = editor.handle_key(other);
            if changed {
                state.validation_error = None;
            }
            MenuKeyOutcome::KeepOpen(changed)
        }
    };
    Some(outcome)
}

fn handle_options_key(
    code: KeyCode,
    shift: bool,
    size: Size,
    config: &mut Config,
    state: &mut btop_ui::OptionsState,
    config_changed: &mut bool,
) -> MenuKeyOutcome {
    if let Some(outcome) = handle_options_text_edit_key(code, config, state, config_changed) {
        return outcome;
    }

    let vis = options_visible_rows(size);
    match code {
        KeyCode::Esc | KeyCode::F(2) | KeyCode::Char('o') => MenuKeyOutcome::Close,
        KeyCode::Tab => {
            if shift {
                state.prev_category();
            } else {
                state.next_category();
            }
            MenuKeyOutcome::KeepOpen(true)
        }
        KeyCode::Char('[') => {
            state.prev_category();
            MenuKeyOutcome::KeepOpen(true)
        }
        KeyCode::Char(']') => {
            state.next_category();
            MenuKeyOutcome::KeepOpen(true)
        }
        KeyCode::Up => {
            state.move_up(vis);
            MenuKeyOutcome::KeepOpen(true)
        }
        KeyCode::Down => {
            state.move_down(vis);
            MenuKeyOutcome::KeepOpen(true)
        }
        KeyCode::Char('k') if config.vim_keys => {
            state.move_up(vis);
            MenuKeyOutcome::KeepOpen(true)
        }
        KeyCode::Char('j') if config.vim_keys => {
            state.move_down(vis);
            MenuKeyOutcome::KeepOpen(true)
        }
        KeyCode::PageUp => {
            state.select_index(state.selected.saturating_sub(vis), vis);
            MenuKeyOutcome::KeepOpen(true)
        }
        KeyCode::PageDown => {
            state.select_index(state.selected.saturating_add(vis), vis);
            MenuKeyOutcome::KeepOpen(true)
        }
        KeyCode::Home => {
            state.select_index(0, vis);
            MenuKeyOutcome::KeepOpen(true)
        }
        KeyCode::End => {
            state.select_index(state.options().len().saturating_sub(1), vis);
            MenuKeyOutcome::KeepOpen(true)
        }
        KeyCode::Left | KeyCode::Right => {
            let dir = if code == KeyCode::Right { 1 } else { -1 };
            let changed = state.selected_option().is_some_and(|o| o.step(config, dir));
            if changed {
                state.validation_error = None;
                *config_changed = true;
            }
            MenuKeyOutcome::KeepOpen(changed)
        }
        KeyCode::Enter | KeyCode::Char(' ') => {
            activate_selected_option(config, state, config_changed)
        }
        KeyCode::Char('r' | 'R') => {
            let dialog = MessageBoxState::yes_no(
                "Reset Options",
                vec![
                    "Restore all btop-windows options to their default values?".to_owned(),
                    "Live theme and panel settings will update immediately.".to_owned(),
                ],
                MessageBoxAction::ResetOptionsToDefaults,
                false,
            );
            MenuKeyOutcome::Replace(OverlayMenu::message_box(
                dialog,
                Some(OverlayMenu::Options(state.clone())),
            ))
        }
        _ => MenuKeyOutcome::KeepOpen(false),
    }
}

fn activate_selected_option(
    config: &mut Config,
    state: &mut btop_ui::OptionsState,
    config_changed: &mut bool,
) -> MenuKeyOutcome {
    let Some(opt) = state.selected_option() else {
        return MenuKeyOutcome::KeepOpen(false);
    };
    match opt.kind() {
        OptionKind::Bool | OptionKind::Enum => {
            let changed = opt.step(config, 1);
            if changed {
                state.validation_error = None;
                *config_changed = true;
            }
            MenuKeyOutcome::KeepOpen(changed)
        }
        OptionKind::Text | OptionKind::Int => {
            let _ = state.begin_edit(config);
            MenuKeyOutcome::KeepOpen(true)
        }
    }
}

fn handle_help_key(
    code: KeyCode,
    vim_keys: bool,
    size: Size,
    state: &mut btop_ui::HelpState,
) -> MenuKeyOutcome {
    let vis = help_visible_rows(size);
    match code {
        KeyCode::Esc | KeyCode::F(1) | KeyCode::Char('?' | 'q') => MenuKeyOutcome::Close,
        KeyCode::Char('h') if !vim_keys => MenuKeyOutcome::Close,
        KeyCode::Char('H') if vim_keys => MenuKeyOutcome::Close,
        KeyCode::Up => {
            state.scroll_up(1);
            MenuKeyOutcome::KeepOpen(true)
        }
        KeyCode::Down => {
            state.scroll_down(1, vis);
            MenuKeyOutcome::KeepOpen(true)
        }
        KeyCode::Char('k') if vim_keys => {
            state.scroll_up(1);
            MenuKeyOutcome::KeepOpen(true)
        }
        KeyCode::Char('j') if vim_keys => {
            state.scroll_down(1, vis);
            MenuKeyOutcome::KeepOpen(true)
        }
        KeyCode::PageUp => {
            state.scroll_up(vis);
            MenuKeyOutcome::KeepOpen(true)
        }
        KeyCode::PageDown => {
            state.scroll_down(vis, vis);
            MenuKeyOutcome::KeepOpen(true)
        }
        KeyCode::Home => {
            state.home();
            MenuKeyOutcome::KeepOpen(true)
        }
        KeyCode::End => {
            state.end(vis);
            MenuKeyOutcome::KeepOpen(true)
        }
        _ => MenuKeyOutcome::KeepOpen(false),
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_messagebox_result(
    result: MessageBoxResult,
    sampler: Option<&Sampler>,
    snapshot: &Snapshot,
    config: &mut Config,
    ui_state: &mut UiState,
    prev_grid: &mut Option<Grid>,
    current_menu: OverlayMenu,
) -> LoopAction {
    if matches!(result, MessageBoxResult::Pending) {
        ui_state.menu = Some(current_menu);
        *prev_grid = None;
        return LoopAction::Redraw;
    }
    let parent = match current_menu {
        OverlayMenu::MessageBox { return_to, .. } => return_to.map(|b| *b),
        _ => None,
    };
    match result {
        MessageBoxResult::Pending => unreachable!(),
        MessageBoxResult::Confirmed(MessageBoxAction::QuitApp) => LoopAction::Quit,
        MessageBoxResult::Confirmed(MessageBoxAction::ResetOptionsToDefaults) => {
            *config = Config::default();
            sync_runtime_from_config(config, sampler, snapshot, ui_state);
            ui_state.menu = parent;
            *prev_grid = None;
            LoopAction::Redraw
        }
        MessageBoxResult::Confirmed(MessageBoxAction::DismissOnly)
        | MessageBoxResult::Cancelled => {
            ui_state.menu = parent;
            *prev_grid = None;
            LoopAction::Redraw
        }
    }
}

/// Synchronizes live sampler parameters and UI panel state from an edited [`Config`] without
/// discarding the open overlay menu or runtime metadata.
pub fn sync_runtime_from_config(
    config: &mut Config,
    sampler: Option<&Sampler>,
    snapshot: &Snapshot,
    ui_state: &mut UiState,
) {
    if let Some(s) = sampler {
        config.update_ms = s.set_update_ms(config.update_ms);
        s.set_freq_mode(FreqMode::from_config_str(&config.freq_mode));
        s.set_nvml_measure_pcie_speeds(config.nvml_measure_pcie_speeds);
    }
    ui_state.layout = LayoutState::from_config(config);
    ui_state.show_disks = config.show_disks;
    ui_state.io_mode = config.io_mode;

    let mut proc_next = ProcViewState::from_config(config);
    proc_next.selected = ui_state.proc.selected;
    proc_next.scroll_offset = ui_state.proc.scroll_offset;
    proc_next.filter.clone_from(&ui_state.proc.filter);
    ui_state.proc = proc_next;

    let mut net_next = NetViewState::from_config(config);
    net_next.observe_snapshot(snapshot);
    ui_state.net = net_next;
}
