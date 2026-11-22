//! Centered process action modal overlays (`k` Terminate confirmation, `N` Priority Class
//! selector, and refusal/error notice dialog) for `btop-ui`.

use btop_config::Config;
use btop_model::ProcessIdentity;
use btop_term::{Grid, Rect, Size};
use unicode_width::UnicodeWidthStr;

use crate::proc_box::clip_text;
use crate::theme::ThemePalette;
use crate::widgets::{draw_border_tag, draw_box_frame, fill_rect};

/// Supported Windows process priority levels mirroring `btop_sys::PriorityClass`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiPriorityClass {
    Idle,
    BelowNormal,
    Normal,
    AboveNormal,
    High,
    Realtime,
}

impl UiPriorityClass {
    pub const ALL: [Self; 6] = [
        Self::Idle,
        Self::BelowNormal,
        Self::Normal,
        Self::AboveNormal,
        Self::High,
        Self::Realtime,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::BelowNormal => "Below Normal",
            Self::Normal => "Normal",
            Self::AboveNormal => "Above Normal",
            Self::High => "High",
            Self::Realtime => "Realtime",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|&c| c == self).unwrap_or(2)
    }
}

/// Active process-action modal rendered centered on top of the current frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcActionModal {
    /// `k` confirmation dialog before calling guarded `TerminateProcess`.
    ConfirmTerminate {
        target: ProcessIdentity,
        name: String,
    },
    /// `N` priority class picker backed by `SetPriorityClass`.
    PrioritySelector {
        target: ProcessIdentity,
        name: String,
        current: Option<UiPriorityClass>,
        selected: usize,
    },
    /// `s` / `x` confirmation dialog before calling elevated `StartServiceW` / `ControlService(STOP)`.
    ConfirmServiceAction {
        service_name: String,
        display_name: String,
        start: bool,
    },
    /// Explicit refusal or error notice (e.g., critical system process BSOD guard, access denied,
    /// `CreateTime` mismatch, or missing window on `WM_CLOSE`).
    ActionNotice {
        title: String,
        message: String,
        is_error: bool,
    },
}

impl ProcActionModal {
    pub fn move_priority_up(&mut self) {
        if let Self::PrioritySelector { selected, .. } = self {
            *selected = selected.saturating_sub(1);
        }
    }

    pub fn move_priority_down(&mut self) {
        if let Self::PrioritySelector { selected, .. } = self {
            *selected = (*selected + 1).min(UiPriorityClass::ALL.len() - 1);
        }
    }

    pub fn selected_priority(&self) -> Option<(ProcessIdentity, String, UiPriorityClass)> {
        if let Self::PrioritySelector {
            target,
            name,
            selected,
            ..
        } = self
        {
            UiPriorityClass::ALL
                .get(*selected)
                .copied()
                .map(|class| (*target, name.clone(), class))
        } else {
            None
        }
    }
}

/// Computes the centered [`Rect`] for `modal` inside `size`, or `None` when the terminal
/// viewport is too small to display the dialog safely.
pub fn modal_rect(size: Size, modal: &ProcActionModal) -> Option<Rect> {
    match modal {
        ProcActionModal::ConfirmTerminate { .. } => centered_rect_for_size(size, 62, 8, 8),
        ProcActionModal::PrioritySelector { .. } => centered_rect_for_size(size, 52, 12, 12),
        ProcActionModal::ConfirmServiceAction { .. } => centered_rect_for_size(size, 64, 8, 8),
        ProcActionModal::ActionNotice { .. } => centered_rect_for_size(size, 66, 8, 8),
    }
}

/// Renders the active [`ProcActionModal`] centered inside `grid`.
pub fn render_proc_action_modal(
    grid: &mut Grid,
    modal: &ProcActionModal,
    config: &Config,
    theme: &ThemePalette,
) {
    let Some(rect) = modal_rect(grid.size(), modal) else {
        return;
    };
    match modal {
        ProcActionModal::ConfirmTerminate { target, name } => {
            render_confirm_terminate(grid, rect, *target, name, config, theme);
        }
        ProcActionModal::PrioritySelector {
            target,
            name,
            current,
            selected,
        } => {
            render_priority_selector(
                grid, rect, *target, name, *current, *selected, config, theme,
            );
        }
        ProcActionModal::ConfirmServiceAction {
            service_name,
            display_name,
            start,
        } => {
            render_confirm_service_action(
                grid,
                rect,
                service_name,
                display_name,
                *start,
                config,
                theme,
            );
        }
        ProcActionModal::ActionNotice {
            title,
            message,
            is_error,
        } => {
            render_action_notice(grid, rect, title, message, *is_error, config, theme);
        }
    }
}

fn centered_rect_for_size(size: Size, desired_w: u16, desired_h: u16, min_h: u16) -> Option<Rect> {
    let width = desired_w.min(size.width.saturating_sub(2));
    let height = desired_h.min(size.height.saturating_sub(2));
    if width < 28 || height < min_h {
        return None;
    }
    Some(Rect {
        x: (size.width.saturating_sub(width)) / 2,
        y: (size.height.saturating_sub(height)) / 2,
        width,
        height,
    })
}

fn render_confirm_terminate(
    grid: &mut Grid,
    rect: Rect,
    target: ProcessIdentity,
    name: &str,
    config: &Config,
    theme: &ThemePalette,
) {
    fill_rect(grid, rect, theme.main_fg, theme.main_bg);
    draw_box_frame(
        grid,
        rect,
        theme.hi_fg,
        theme.main_bg,
        config.rounded_corners,
    );
    let max_x = rect.x.saturating_add(rect.width - 1);
    let _ = draw_border_tag(
        grid,
        rect.x.saturating_add(2),
        rect.y,
        max_x,
        None,
        "Terminate Process (k)",
        theme.hi_fg,
        theme.title,
        theme.hi_fg,
        theme.main_bg,
    );

    let inner_x = rect.x.saturating_add(2);
    let inner_w = usize::from(rect.width.saturating_sub(4));
    let line1 = clip_text(&format!("Terminate {name} (PID {})?", target.pid), inner_w);
    let line2 = clip_text(
        "Safety: verifies CreateTime on handle and blocks critical processes.",
        inner_w,
    );
    let hint = clip_text("[Enter / y] Terminate    [Esc / n] Cancel", inner_w);

    grid.write_str(
        inner_x,
        rect.y + 2,
        &line1,
        theme.title,
        theme.main_bg,
        true,
    );
    grid.write_str(
        inner_x,
        rect.y + 3,
        &line2,
        theme.inactive_fg,
        theme.main_bg,
        false,
    );
    grid.write_str(inner_x, rect.y + 5, &hint, theme.hi_fg, theme.main_bg, true);
}

#[allow(clippy::too_many_arguments)]
fn render_confirm_service_action(
    grid: &mut Grid,
    rect: Rect,
    service_name: &str,
    display_name: &str,
    start: bool,
    config: &Config,
    theme: &ThemePalette,
) {
    let border_fg = if start {
        btop_term::Color::Rgb(theme.cpu_mid)
    } else {
        theme.hi_fg
    };
    fill_rect(grid, rect, theme.main_fg, theme.main_bg);
    draw_box_frame(grid, rect, border_fg, theme.main_bg, config.rounded_corners);
    let max_x = rect.x.saturating_add(rect.width - 1);
    let title = if start {
        "Start Service (s)"
    } else {
        "Stop Service (x)"
    };
    let _ = draw_border_tag(
        grid,
        rect.x.saturating_add(2),
        rect.y,
        max_x,
        None,
        title,
        border_fg,
        theme.title,
        theme.hi_fg,
        theme.main_bg,
    );

    let inner_x = rect.x.saturating_add(2);
    let inner_w = usize::from(rect.width.saturating_sub(4));
    let verb = if start { "Start" } else { "Stop" };
    let line1 = clip_text(
        &format!("{verb} service '{service_name}' ({display_name})?"),
        inner_w,
    );
    let line2 = clip_text(
        if start {
            "Calls StartServiceW via Windows Service Control Manager (SCM)."
        } else {
            "Calls ControlService(STOP) via Windows Service Control Manager (SCM)."
        },
        inner_w,
    );
    let hint = clip_text(&format!("[Enter / y] {verb}    [Esc / n] Cancel"), inner_w);

    grid.write_str(
        inner_x,
        rect.y + 2,
        &line1,
        theme.title,
        theme.main_bg,
        true,
    );
    grid.write_str(
        inner_x,
        rect.y + 3,
        &line2,
        theme.inactive_fg,
        theme.main_bg,
        false,
    );
    grid.write_str(inner_x, rect.y + 5, &hint, border_fg, theme.main_bg, true);
}

#[allow(clippy::too_many_arguments)]
fn render_priority_selector(
    grid: &mut Grid,
    rect: Rect,
    target: ProcessIdentity,
    name: &str,
    current: Option<UiPriorityClass>,
    selected: usize,
    config: &Config,
    theme: &ThemePalette,
) {
    fill_rect(grid, rect, theme.main_fg, theme.main_bg);
    draw_box_frame(
        grid,
        rect,
        theme.proc_box,
        theme.main_bg,
        config.rounded_corners,
    );
    let max_x = rect.x.saturating_add(rect.width - 1);
    let _ = draw_border_tag(
        grid,
        rect.x.saturating_add(2),
        rect.y,
        max_x,
        None,
        "Priority Class (N)",
        theme.proc_box,
        theme.title,
        theme.hi_fg,
        theme.main_bg,
    );

    let inner_x = rect.x.saturating_add(2);
    let inner_w = usize::from(rect.width.saturating_sub(4));
    let header = clip_text(&format!("Process: {name} (PID {})", target.pid), inner_w);
    grid.write_str(
        inner_x,
        rect.y + 2,
        &header,
        theme.title,
        theme.main_bg,
        true,
    );

    let sel_clamped = selected.min(UiPriorityClass::ALL.len() - 1);
    for (idx, class) in UiPriorityClass::ALL.iter().enumerate() {
        let y = rect.y + 4 + u16::try_from(idx).unwrap_or(0);
        if y >= rect.y.saturating_add(rect.height - 2) {
            break;
        }
        let is_sel = idx == sel_clamped;
        let is_cur = current == Some(*class);
        let row_bg = if is_sel {
            theme.selected_bg
        } else {
            theme.main_bg
        };
        let text_fg = if is_sel {
            theme.selected_fg
        } else if is_cur {
            theme.hi_fg
        } else {
            theme.main_fg
        };
        if is_sel {
            fill_rect(
                grid,
                Rect {
                    x: rect.x + 1,
                    y,
                    width: rect.width.saturating_sub(2),
                    height: 1,
                },
                text_fg,
                row_bg,
            );
        }
        let marker = if is_sel { ">" } else { " " };
        let suffix = if is_cur { " (current)" } else { "" };
        let row_text = clip_text(&format!("{marker} {}{suffix}", class.label()), inner_w);
        grid.write_str(inner_x, y, &row_text, text_fg, row_bg, is_sel || is_cur);
    }

    let hint = clip_text("↑↓ select  |  Enter apply  |  Esc cancel", inner_w);
    grid.write_str(
        inner_x,
        rect.y.saturating_add(rect.height - 2),
        &hint,
        theme.inactive_fg,
        theme.main_bg,
        false,
    );
}

fn render_action_notice(
    grid: &mut Grid,
    rect: Rect,
    title: &str,
    message: &str,
    is_error: bool,
    config: &Config,
    theme: &ThemePalette,
) {
    let border_fg = if is_error {
        theme.hi_fg
    } else {
        theme.proc_box
    };
    fill_rect(grid, rect, theme.main_fg, theme.main_bg);
    draw_box_frame(grid, rect, border_fg, theme.main_bg, config.rounded_corners);
    let max_x = rect.x.saturating_add(rect.width - 1);
    let title_clipped = clip_text(title, usize::from(rect.width.saturating_sub(6)));
    let _ = draw_border_tag(
        grid,
        rect.x.saturating_add(2),
        rect.y,
        max_x,
        None,
        &title_clipped,
        border_fg,
        theme.title,
        theme.hi_fg,
        theme.main_bg,
    );

    let inner_x = rect.x.saturating_add(2);
    let inner_w = usize::from(rect.width.saturating_sub(4));
    let wrapped = wrap_two_lines(message, inner_w);
    let msg_fg = if is_error { theme.hi_fg } else { theme.main_fg };
    grid.write_str(
        inner_x,
        rect.y + 2,
        &wrapped.0,
        msg_fg,
        theme.main_bg,
        is_error,
    );
    if !wrapped.1.is_empty() {
        grid.write_str(
            inner_x,
            rect.y + 3,
            &wrapped.1,
            msg_fg,
            theme.main_bg,
            is_error,
        );
    }

    let hint = clip_text("[Enter / Esc] Dismiss", inner_w);
    grid.write_str(
        inner_x,
        rect.y + 5,
        &hint,
        theme.inactive_fg,
        theme.main_bg,
        false,
    );
}

fn wrap_two_lines(text: &str, max_cells: usize) -> (String, String) {
    if max_cells == 0 {
        return (String::new(), String::new());
    }
    if text.width() <= max_cells {
        return (text.to_owned(), String::new());
    }
    let mut line1 = String::new();
    let mut line2 = String::new();
    for word in text.split_whitespace() {
        let sep = usize::from(!line1.is_empty());
        if line2.is_empty() && line1.width() + sep + word.width() <= max_cells {
            if sep == 1 {
                line1.push(' ');
            }
            line1.push_str(word);
        } else {
            if !line2.is_empty() {
                line2.push(' ');
            }
            line2.push_str(word);
        }
    }
    if line1.is_empty() {
        return (clip_text(text, max_cells), String::new());
    }
    (line1, clip_text(&line2, max_cells))
}

#[cfg(test)]
mod tests {
    use btop_term::Size;

    use super::*;

    #[test]
    fn renders_confirm_terminate_priority_and_refusal_modals_cleanly() {
        let cfg = Config::default();
        let theme = ThemePalette::default_dark(true);
        let target = ProcessIdentity {
            pid: 4216,
            create_time_100ns: 1000,
        };

        let mut g1 = Grid::new(Size {
            width: 100,
            height: 30,
        });
        render_proc_action_modal(
            &mut g1,
            &ProcActionModal::ConfirmTerminate {
                target,
                name: "worker.exe".to_owned(),
            },
            &cfg,
            &theme,
        );
        let t1 = g1.to_plain_text();
        assert!(t1.contains("Terminate Process (k)"));
        assert!(t1.contains("Terminate worker.exe (PID 4216)?"));
        assert!(t1.contains("[Enter / y] Terminate"));

        let mut modal = ProcActionModal::PrioritySelector {
            target,
            name: "worker.exe".to_owned(),
            current: Some(UiPriorityClass::Normal),
            selected: UiPriorityClass::Normal.index(),
        };
        modal.move_priority_down();
        assert_eq!(
            modal.selected_priority().map(|(_, _, c)| c),
            Some(UiPriorityClass::AboveNormal)
        );
        let mut g2 = Grid::new(Size {
            width: 100,
            height: 30,
        });
        render_proc_action_modal(&mut g2, &modal, &cfg, &theme);
        let t2 = g2.to_plain_text();
        assert!(t2.contains("Priority Class (N)"));
        assert!(t2.contains("Normal (current)"));
        assert!(t2.contains("> Above Normal"));

        let mut g3 = Grid::new(Size {
            width: 100,
            height: 30,
        });
        render_proc_action_modal(
            &mut g3,
            &ProcActionModal::ActionNotice {
                title: "Action Refused".to_owned(),
                message: "Refused: PID 720 is a critical Windows system process (terminating would cause a BSOD)".to_owned(),
                is_error: true,
            },
            &cfg,
            &theme,
        );
        let t3 = g3.to_plain_text();
        assert!(t3.contains("Action Refused"));
        assert!(t3.contains("critical Windows system process"));
        assert!(t3.contains("[Enter / Esc] Dismiss"));

        let mut g4 = Grid::new(Size {
            width: 100,
            height: 30,
        });
        render_proc_action_modal(
            &mut g4,
            &ProcActionModal::ConfirmServiceAction {
                service_name: "W32Time".to_owned(),
                display_name: "Windows Time".to_owned(),
                start: false,
            },
            &cfg,
            &theme,
        );
        let t4 = g4.to_plain_text();
        assert!(t4.contains("Stop Service (x)"));
        assert!(t4.contains("Stop service 'W32Time' (Windows Time)?"));
        assert!(t4.contains("[Enter / y] Stop"));
    }

    #[test]
    fn modal_rect_rejects_short_viewports_so_modals_never_arm_invisibly() {
        let confirm = ProcActionModal::ConfirmTerminate {
            target: ProcessIdentity {
                pid: 4216,
                create_time_100ns: 1000,
            },
            name: "worker.exe".to_owned(),
        };
        assert_eq!(
            modal_rect(
                Size {
                    width: 80,
                    height: 7
                },
                &confirm
            ),
            None
        );
        assert!(
            modal_rect(
                Size {
                    width: 80,
                    height: 24
                },
                &confirm
            )
            .is_some()
        );

        let prio = ProcActionModal::PrioritySelector {
            target: ProcessIdentity {
                pid: 4216,
                create_time_100ns: 1000,
            },
            name: "worker.exe".to_owned(),
            current: Some(UiPriorityClass::Normal),
            selected: UiPriorityClass::Realtime.index(),
        };
        assert_eq!(
            modal_rect(
                Size {
                    width: 80,
                    height: 13
                },
                &prio
            ),
            None,
            "80x13 cannot display all 6 priority options without hiding Realtime"
        );
        let min_fit = Size {
            width: 80,
            height: 14,
        };
        assert!(modal_rect(min_fit, &prio).is_some());
        let mut g = Grid::new(min_fit);
        render_proc_action_modal(
            &mut g,
            &prio,
            &Config::default(),
            &ThemePalette::default_dark(true),
        );
        assert!(g.to_plain_text().contains("> Realtime"));
    }
}
