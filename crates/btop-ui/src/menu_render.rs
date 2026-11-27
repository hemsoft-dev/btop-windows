//! Pure rendering for the dimmed overlay menu stack: Main menu (`Esc`/`m`),
//! categorized Options menu (`F2`/`o`), scrollable Help reference (`F1`/`?`/`h`),
//! and centered `MessageBox` (`OK` / `YES_NO`).

use btop_config::Config;
use btop_term::{Cell, Color, Grid, HitAction, Rect, Rgb, Size};

use crate::menu::{
    HELP_BINDINGS, HelpState, MainMenuChoice, MainMenuState, MessageBoxKind, MessageBoxState,
    OptionCategory, OptionId, OptionKind, OptionsState, OverlayMenu, UNIX_HELP_BINDINGS,
};
use crate::proc_box::clip_text;
use crate::theme::ThemePalette;
use crate::widgets::{draw_border_tag, draw_box_frame, fill_rect};

const ART_OPTIONS: [&str; 3] = [
    "█▀▀█ █▀▀█ ▀▀█▀▀ ▀█▀ █▀▀█ █▄  █ █▀▀▀",
    "█  █ █▀▀▀   █    █  █  █ █ ▀▄█ ▀▀▀█",
    "▀▀▀▀ ▀      ▀   ▀▀▀ ▀▀▀▀ ▀   ▀ ▀▀▀▀",
];

const ART_HELP: [&str; 3] = [
    "█  █ █▀▀▀ █    █▀▀█",
    "█▀▀█ █▀▀  █    █▀▀▀",
    "▀  ▀ ▀▀▀▀ ▀▀▀▀ ▀   ",
];

const ART_QUIT: [&str; 3] = [
    "█▀▀█ █  █ ▀█▀ ▀▀█▀▀",
    "█  █ █  █  █    █  ",
    "▀▀▀█ ▀▀▀▀ ▀▀▀   ▀  ",
];

/// Returns the number of visible option rows inside the Options menu for `size`.
pub fn options_visible_rows(size: Size) -> usize {
    let rect = options_rect(size);
    usize::from(rect.height.saturating_sub(6)).max(1)
}

/// Returns the number of visible keybinding rows inside the Help menu for `size`.
pub fn help_visible_rows(size: Size) -> usize {
    let rect = help_rect(size);
    usize::from(rect.height.saturating_sub(5)).max(1)
}

/// Dims all cells in `grid` so overlay menus pop cleanly over the live dashboard, and clears
/// underlying `HitMap` regions so clicks behind the menu never trigger background controls.
pub fn dim_grid(grid: &mut Grid, theme: &ThemePalette) {
    grid.hit_map.clear();
    let bg_rgb = match theme.main_bg {
        Color::Rgb(rgb) => rgb,
        _ => Rgb {
            r: 14,
            g: 16,
            b: 22,
        },
    };
    let size = grid.size();
    for y in 0..size.height {
        for x in 0..size.width {
            if let Some(cell) = grid.get(x, y).copied() {
                let dimmed = Cell {
                    ch: cell.ch,
                    fg: dim_color(cell.fg, bg_rgb),
                    bg: dim_bg(cell.bg, bg_rgb),
                    bold: false,
                    continuation: cell.continuation,
                };
                grid.set(x, y, dimmed);
            }
        }
    }
}

fn dim_color(color: Color, base: Rgb) -> Color {
    match color {
        Color::Rgb(rgb) => Color::Rgb(blend_rgb(rgb, base, 38)),
        Color::Reset | Color::Indexed(_) => Color::Rgb(Rgb {
            r: 72,
            g: 78,
            b: 92,
        }),
    }
}

fn dim_bg(color: Color, base: Rgb) -> Color {
    match color {
        Color::Rgb(rgb) => Color::Rgb(blend_rgb(rgb, base, 50)),
        Color::Reset => Color::Reset,
        Color::Indexed(_) => Color::Rgb(base),
    }
}

fn blend_rgb(fg: Rgb, bg: Rgb, fg_pct: u16) -> Rgb {
    let bg_pct = 100u16.saturating_sub(fg_pct);
    let mix = |a: u8, b: u8| -> u8 {
        let val = (u16::from(a) * fg_pct + u16::from(b) * bg_pct) / 100;
        u8::try_from(val.min(255)).unwrap_or(255)
    };
    Rgb {
        r: mix(fg.r, bg.r),
        g: mix(fg.g, bg.g),
        b: mix(fg.b, bg.b),
    }
}

/// Renders the active [`OverlayMenu`] on top of the dimmed `grid`.
pub fn render_overlay_menu(
    grid: &mut Grid,
    menu: &OverlayMenu,
    config: &Config,
    theme: &ThemePalette,
) {
    render_overlay_menu_for_platform(grid, menu, false, config, theme);
}

/// Renders the active [`OverlayMenu`] on top of the dimmed `grid`, selecting Unix or Windows
/// keybinding descriptions for the Help overlay.
pub fn render_overlay_menu_for_platform(
    grid: &mut Grid,
    menu: &OverlayMenu,
    unix_help: bool,
    config: &Config,
    theme: &ThemePalette,
) {
    dim_grid(grid, theme);
    match menu {
        OverlayMenu::Main(state) => render_main_menu(grid, state, config, theme),
        OverlayMenu::Options(state) => render_options_menu(grid, state, config, theme),
        OverlayMenu::Help(state) => render_help_menu(grid, state, unix_help, config, theme),
        OverlayMenu::MessageBox { dialog, return_to } => {
            if let Some(parent) = return_to.as_deref() {
                match parent {
                    OverlayMenu::Main(s) => render_main_menu(grid, s, config, theme),
                    OverlayMenu::Options(s) => render_options_menu(grid, s, config, theme),
                    OverlayMenu::Help(s) => render_help_menu(grid, s, unix_help, config, theme),
                    OverlayMenu::MessageBox { .. } => {}
                }
                dim_grid(grid, theme);
            }
            render_message_box(grid, dialog, config, theme);
        }
    }
}

fn centered_rect(size: Size, width: u16, height: u16) -> Rect {
    let max_w = size.width.saturating_sub(2).max(size.width.min(1));
    let max_h = size.height.saturating_sub(2).max(size.height.min(1));
    let w = width.min(max_w);
    let h = height.min(max_h);
    Rect {
        x: size.width.saturating_sub(w) / 2,
        y: size.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    }
}

fn options_rect(size: Size) -> Rect {
    let w = size.width.saturating_sub(4).clamp(60, 104);
    let h = size.height.saturating_sub(2).clamp(18, 34);
    centered_rect(size, w, h)
}

fn help_rect(size: Size) -> Rect {
    let w = size.width.saturating_sub(4).clamp(60, 96);
    let h = size.height.saturating_sub(2).clamp(18, 34);
    centered_rect(size, w, h)
}

fn render_main_menu(grid: &mut Grid, state: &MainMenuState, config: &Config, theme: &ThemePalette) {
    let size = grid.size();
    let show_banner = size.height >= 28;
    let rect = centered_rect(size, 52, if show_banner { 22 } else { 19 });
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
        "Main Menu",
        theme.hi_fg,
        theme.title,
        theme.hi_fg,
        theme.main_bg,
    );
    draw_close_tag(grid, rect, theme);

    let mut cur_y = rect.y.saturating_add(2);
    if show_banner {
        let banner = format!("btop-windows  •  theme: {}", config.color_theme);
        let clipped = clip_text(&banner, usize::from(rect.width.saturating_sub(4)));
        let bx = center_x(rect, clipped.chars().count());
        grid.write_str(bx, cur_y, &clipped, theme.inactive_fg, theme.main_bg, false);
        cur_y = cur_y.saturating_add(2);
    }

    let items: [(&[&str; 3], MainMenuChoice, &str); 3] = [
        (&ART_OPTIONS, MainMenuChoice::Options, "F2 / o"),
        (&ART_HELP, MainMenuChoice::Help, "F1 / ?"),
        (&ART_QUIT, MainMenuChoice::Quit, "q"),
    ];

    for (idx, (art, _, hotkey)) in items.iter().enumerate() {
        let selected = idx == state.selected;
        let btn_rect = Rect {
            x: rect.x.saturating_add(2),
            y: cur_y,
            width: rect.width.saturating_sub(4),
            height: 3,
        };
        let bg = if selected {
            theme.selected_bg
        } else {
            theme.main_bg
        };
        let fg = if selected { theme.hi_fg } else { theme.main_fg };
        fill_rect(grid, btn_rect, fg, bg);

        for (line_idx, art_line) in art.iter().enumerate() {
            let y = cur_y.saturating_add(u16::try_from(line_idx).unwrap_or(0));
            if y >= rect.y.saturating_add(rect.height.saturating_sub(2)) {
                break;
            }
            let clipped = clip_text(art_line, usize::from(btn_rect.width.saturating_sub(4)));
            let lx = center_x(btn_rect, clipped.chars().count());
            grid.write_str(lx, y, &clipped, fg, bg, selected);
        }
        if selected {
            let marker_y = cur_y.saturating_add(1);
            grid.write_str(btn_rect.x + 1, marker_y, "▶", theme.hi_fg, bg, true);
            let right_marker = btn_rect.x.saturating_add(btn_rect.width.saturating_sub(2));
            grid.write_str(right_marker, marker_y, "◀", theme.hi_fg, bg, true);
        }
        let hk = format!("[{hotkey}]");
        let hk_x = btn_rect.x.saturating_add(
            btn_rect
                .width
                .saturating_sub(u16::try_from(hk.len()).unwrap_or(4) + 1),
        );
        if hk_x > btn_rect.x + 2 && !selected {
            grid.write_str(hk_x, cur_y + 1, &hk, theme.inactive_fg, bg, false);
        }

        grid.hit_map.push(btn_rect, HitAction::MenuItem(idx));
        cur_y = cur_y.saturating_add(4);
    }

    let hint_y = rect.y.saturating_add(rect.height.saturating_sub(2));
    let hint = clip_text(
        "↑↓ select  |  Enter open  |  Esc / m close",
        usize::from(rect.width.saturating_sub(4)),
    );
    let hx = center_x(rect, hint.chars().count());
    grid.write_str(hx, hint_y, &hint, theme.inactive_fg, theme.main_bg, false);
}

fn render_options_menu(
    grid: &mut Grid,
    state: &OptionsState,
    config: &Config,
    theme: &ThemePalette,
) {
    let rect = options_rect(grid.size());
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
        "Options (F2)",
        theme.proc_box,
        theme.title,
        theme.hi_fg,
        theme.main_bg,
    );
    draw_close_tag(grid, rect, theme);

    render_options_tabs(grid, rect, state.category, theme);

    render_options_body(grid, rect, state, config, theme);

    let footer_y = rect.y.saturating_add(rect.height.saturating_sub(2));
    let footer = if state.editing.is_some() {
        "Type value  |  ←→ cursor  |  Enter save  |  Esc cancel"
    } else {
        "↑↓ select  |  ←→/Enter edit  |  Tab category  |  R reset  |  Esc close"
    };
    let clipped_footer = clip_text(footer, usize::from(rect.width.saturating_sub(4)));
    grid.write_str(
        rect.x + 2,
        footer_y,
        &clipped_footer,
        theme.inactive_fg,
        theme.main_bg,
        false,
    );
}

fn render_options_body(
    grid: &mut Grid,
    rect: Rect,
    state: &OptionsState,
    config: &Config,
    theme: &ThemePalette,
) {
    let body_top = rect.y.saturating_add(3);
    let body_h = rect.height.saturating_sub(5);
    let inner_w = rect.width.saturating_sub(2);

    let hdiv = "─".repeat(usize::from(inner_w));
    grid.write_str(
        rect.x + 1,
        rect.y + 2,
        &hdiv,
        theme.div_line,
        theme.main_bg,
        false,
    );

    if inner_w >= 54 {
        let left_w = (inner_w * 58 / 100).clamp(24, inner_w.saturating_sub(20));
        let div_x = rect.x.saturating_add(1).saturating_add(left_w);
        let right_x = div_x.saturating_add(1);
        let right_w = rect
            .x
            .saturating_add(rect.width.saturating_sub(1))
            .saturating_sub(right_x);

        for dy in 0..body_h {
            grid.set(
                div_x,
                body_top.saturating_add(dy),
                Cell {
                    ch: '│',
                    fg: theme.div_line,
                    bg: theme.main_bg,
                    bold: false,
                    continuation: false,
                },
            );
        }

        let list_rect = Rect {
            x: rect.x.saturating_add(1),
            y: body_top,
            width: left_w,
            height: body_h,
        };
        render_options_list(grid, list_rect, state, config, theme);

        let detail_rect = Rect {
            x: right_x,
            y: body_top,
            width: right_w,
            height: body_h,
        };
        render_options_detail(grid, detail_rect, state, config, theme);
    } else if inner_w > 0 && body_h > 0 {
        let list_h = if state.validation_error.is_some() && body_h > 2 {
            body_h - 1
        } else {
            body_h
        };
        let list_rect = Rect {
            x: rect.x.saturating_add(1),
            y: body_top,
            width: inner_w,
            height: list_h,
        };
        render_options_list(grid, list_rect, state, config, theme);
        if let Some(ref err) = state.validation_error
            && body_h > 2
        {
            let msg = clip_text(&format!("Error: {err}"), usize::from(inner_w));
            grid.write_str(
                rect.x + 1,
                body_top.saturating_add(list_h),
                &msg,
                theme.hi_fg,
                theme.main_bg,
                true,
            );
        }
    }
}

fn render_options_tabs(grid: &mut Grid, rect: Rect, active: OptionCategory, theme: &ThemePalette) {
    let tab_y = rect.y.saturating_add(1);
    let mut x = rect.x.saturating_add(2);
    let right_limit = rect.x.saturating_add(rect.width.saturating_sub(2));

    for (idx, cat) in OptionCategory::ALL.iter().enumerate() {
        let label = format!(" {} ", cat.label());
        let width = u16::try_from(label.len()).unwrap_or(8);
        if x.saturating_add(width) > right_limit {
            break;
        }
        let is_active = *cat == active;
        let fg = if is_active {
            theme.selected_fg
        } else {
            theme.inactive_fg
        };
        let bg = if is_active {
            theme.selected_bg
        } else {
            theme.main_bg
        };
        grid.write_str(x, tab_y, &label, fg, bg, is_active);
        grid.hit_map.push(
            Rect {
                x,
                y: tab_y,
                width,
                height: 1,
            },
            HitAction::MenuCategory(idx),
        );
        x = x.saturating_add(width + 1);
    }
}

fn render_options_list(
    grid: &mut Grid,
    rect: Rect,
    state: &OptionsState,
    config: &Config,
    theme: &ThemePalette,
) {
    let opts = state.options();
    let visible = usize::from(rect.height).max(1);
    let start = state.scroll_offset.min(opts.len().saturating_sub(1));
    let key_col_w = usize::from(rect.width.saturating_sub(4)).min(24);

    for row_idx in 0..visible {
        let opt_idx = start + row_idx;
        let Some(&opt) = opts.get(opt_idx) else {
            break;
        };
        let y = rect.y.saturating_add(u16::try_from(row_idx).unwrap_or(0));
        let is_sel = opt_idx == state.selected;
        let row_bg = if is_sel {
            theme.selected_bg
        } else {
            theme.main_bg
        };
        let key_fg = if is_sel {
            theme.selected_fg
        } else {
            theme.main_fg
        };
        fill_rect(
            grid,
            Rect {
                x: rect.x,
                y,
                width: rect.width,
                height: 1,
            },
            key_fg,
            row_bg,
        );

        let prefix = if is_sel { "▸ " } else { "  " };
        let key_text = clip_text(&format!("{prefix}{}", opt.key_name()), key_col_w);
        grid.write_str(rect.x, y, &key_text, key_fg, row_bg, is_sel);

        let val_x = rect
            .x
            .saturating_add(u16::try_from(key_col_w).unwrap_or(20) + 1);
        let val_w = usize::from(rect.x.saturating_add(rect.width).saturating_sub(val_x + 1));
        if val_w > 0 {
            let (display_val, val_fg) = format_option_value(opt, is_sel, state, config, theme);
            let clipped_val = clip_text(&display_val, val_w);
            grid.write_str(val_x, y, &clipped_val, val_fg, row_bg, is_sel);
        }

        grid.hit_map.push(
            Rect {
                x: rect.x,
                y,
                width: rect.width,
                height: 1,
            },
            HitAction::MenuItem(opt_idx),
        );
    }
}

fn format_option_value(
    opt: OptionId,
    is_sel: bool,
    state: &OptionsState,
    config: &Config,
    theme: &ThemePalette,
) -> (String, Color) {
    if is_sel && let Some(ref editor) = state.editing {
        let raw = editor.as_str();
        let cur = editor.cursor_char_idx();
        let mut with_cursor = String::with_capacity(raw.len() + 3);
        with_cursor.push('[');
        for (i, ch) in raw.chars().enumerate() {
            if i == cur {
                with_cursor.push('▏');
            }
            with_cursor.push(ch);
        }
        if cur >= raw.chars().count() {
            with_cursor.push('▏');
        }
        with_cursor.push(']');
        return (with_cursor, theme.hi_fg);
    }
    let val = opt.value_string(config);
    let formatted = match opt.kind() {
        OptionKind::Bool | OptionKind::Enum => format!("◀ {val} ▶"),
        OptionKind::Text | OptionKind::Int => {
            if val.is_empty() {
                "\"\"".to_owned()
            } else {
                val
            }
        }
    };
    let fg = if is_sel { theme.hi_fg } else { theme.title };
    (formatted, fg)
}

fn render_options_detail(
    grid: &mut Grid,
    rect: Rect,
    state: &OptionsState,
    config: &Config,
    theme: &ThemePalette,
) {
    if rect.width < 10 || rect.height < 4 {
        return;
    }
    let inner_x = rect.x.saturating_add(1);
    let inner_w = usize::from(rect.width.saturating_sub(2));
    let mut y = rect.y;
    let max_y = rect.y.saturating_add(rect.height);

    if let Some(opt) = state.selected_option() {
        let heading = clip_text(opt.key_name(), inner_w);
        grid.write_str(inner_x, y, &heading, theme.title, theme.main_bg, true);
        y = y.saturating_add(1);

        let kind_label = match opt.kind() {
            OptionKind::Bool => "Type: Boolean (← / → / Enter)",
            OptionKind::Enum => "Type: Choice (← / → / Enter)",
            OptionKind::Int => "Type: Integer (← / → or Enter to type)",
            OptionKind::Text => "Type: String (Enter to edit)",
        };
        if y < max_y {
            let clipped = clip_text(kind_label, inner_w);
            grid.write_str(
                inner_x,
                y,
                &clipped,
                theme.inactive_fg,
                theme.main_bg,
                false,
            );
            y = y.saturating_add(2);
        }

        for line in wrap_lines(opt.description(), inner_w, 4) {
            if y >= max_y {
                break;
            }
            grid.write_str(inner_x, y, &line, theme.main_fg, theme.main_bg, false);
            y = y.saturating_add(1);
        }
    }

    // Live theme palette preview strip.
    if y.saturating_add(2) < max_y {
        y = y.saturating_add(1);
        let label = clip_text(&format!("Theme preview: {}", config.color_theme), inner_w);
        grid.write_str(inner_x, y, &label, theme.title, theme.main_bg, false);
        y = y.saturating_add(1);
        let swatches = [
            theme.cpu_box,
            theme.mem_box,
            theme.net_box,
            theme.proc_box,
            theme.hi_fg,
            Color::Rgb(theme.cpu_start),
            Color::Rgb(theme.cpu_mid),
            Color::Rgb(theme.cpu_end),
        ];
        let mut sx = inner_x;
        let limit = rect.x.saturating_add(rect.width.saturating_sub(1));
        for swatch in swatches {
            if sx.saturating_add(2) > limit {
                break;
            }
            grid.write_str(sx, y, "██", swatch, theme.main_bg, false);
            sx = sx.saturating_add(2);
        }
        y = y.saturating_add(2);
    }

    if let Some(ref err) = state.validation_error
        && y < max_y
    {
        for line in wrap_lines(&format!("Error: {err}"), inner_w, 3) {
            if y >= max_y {
                break;
            }
            grid.write_str(inner_x, y, &line, theme.hi_fg, theme.main_bg, true);
            y = y.saturating_add(1);
        }
    }
}

fn render_help_menu(
    grid: &mut Grid,
    state: &HelpState,
    unix_help: bool,
    config: &Config,
    theme: &ThemePalette,
) {
    let bindings = if unix_help {
        UNIX_HELP_BINDINGS
    } else {
        HELP_BINDINGS
    };
    let rect = help_rect(grid.size());
    fill_rect(grid, rect, theme.main_fg, theme.main_bg);
    draw_box_frame(
        grid,
        rect,
        theme.net_box,
        theme.main_bg,
        config.rounded_corners,
    );

    let max_x = rect.x.saturating_add(rect.width - 1);
    let title = if unix_help {
        "Help & Unix Keybindings (F1)"
    } else {
        "Help & Windows 11 Keybindings (F1)"
    };
    let _ = draw_border_tag(
        grid,
        rect.x.saturating_add(2),
        rect.y,
        max_x,
        None,
        title,
        theme.net_box,
        theme.title,
        theme.hi_fg,
        theme.main_bg,
    );
    draw_close_tag(grid, rect, theme);

    let header_y = rect.y.saturating_add(1);
    let key_w = 24usize.min(usize::from(rect.width / 3));
    let key_x = rect.x.saturating_add(2);
    let desc_x = key_x.saturating_add(u16::try_from(key_w).unwrap_or(20) + 1);
    let desc_w = usize::from(rect.x.saturating_add(rect.width - 2).saturating_sub(desc_x));

    grid.write_str(key_x, header_y, "Key", theme.title, theme.main_bg, true);
    grid.write_str(desc_x, header_y, "Action", theme.title, theme.main_bg, true);

    let div = "─".repeat(usize::from(rect.width.saturating_sub(2)));
    grid.write_str(
        rect.x + 1,
        rect.y + 2,
        &div,
        theme.div_line,
        theme.main_bg,
        false,
    );

    let visible = usize::from(rect.height.saturating_sub(5)).max(1);
    let max_scroll = bindings.len().saturating_sub(visible);
    let start = state.scroll_offset.min(max_scroll);

    for row in 0..visible {
        let Some(entry) = bindings.get(start + row) else {
            break;
        };
        let y = rect.y.saturating_add(3 + u16::try_from(row).unwrap_or(0));
        if entry.is_section {
            let section_title = clip_text(
                &format!("── {} ──", entry.key),
                usize::from(rect.width.saturating_sub(4)),
            );
            grid.write_str(key_x, y, &section_title, theme.net_box, theme.main_bg, true);
        } else {
            let k = clip_text(entry.key, key_w);
            let d = clip_text(entry.description, desc_w);
            grid.write_str(key_x, y, &k, theme.hi_fg, theme.main_bg, true);
            grid.write_str(desc_x, y, &d, theme.main_fg, theme.main_bg, false);
        }
    }

    let end_idx = (start + visible).min(bindings.len());
    let footer_y = rect.y.saturating_add(rect.height.saturating_sub(2));
    let footer = clip_text(
        &format!(
            "↑↓ / PgUp / PgDn scroll ({}-{} of {})  |  Esc / q close",
            start + 1,
            end_idx,
            bindings.len()
        ),
        usize::from(rect.width.saturating_sub(4)),
    );
    grid.write_str(
        key_x,
        footer_y,
        &footer,
        theme.inactive_fg,
        theme.main_bg,
        false,
    );
}

fn render_message_box(
    grid: &mut Grid,
    dialog: &MessageBoxState,
    config: &Config,
    theme: &ThemePalette,
) {
    let size = grid.size();
    let max_body = dialog.lines.len().clamp(1, 6);
    let desired_h = u16::try_from(max_body + 6).unwrap_or(9);
    let rect = centered_rect(size, 64, desired_h);
    fill_rect(grid, rect, theme.main_fg, theme.main_bg);
    draw_box_frame(
        grid,
        rect,
        theme.hi_fg,
        theme.main_bg,
        config.rounded_corners,
    );

    let max_x = rect.x.saturating_add(rect.width - 1);
    let title = clip_text(&dialog.title, usize::from(rect.width.saturating_sub(8)));
    let _ = draw_border_tag(
        grid,
        rect.x.saturating_add(2),
        rect.y,
        max_x,
        None,
        &title,
        theme.hi_fg,
        theme.title,
        theme.hi_fg,
        theme.main_bg,
    );

    let inner_x = rect.x.saturating_add(2);
    let inner_w = usize::from(rect.width.saturating_sub(4));
    for (idx, line) in dialog.lines.iter().take(max_body).enumerate() {
        let y = rect.y.saturating_add(2 + u16::try_from(idx).unwrap_or(0));
        if y >= rect.y.saturating_add(rect.height.saturating_sub(3)) {
            break;
        }
        let clipped = clip_text(line, inner_w);
        grid.write_str(inner_x, y, &clipped, theme.main_fg, theme.main_bg, false);
    }

    let btn_y = rect.y.saturating_add(rect.height.saturating_sub(2));
    render_message_box_buttons(grid, rect, btn_y, dialog, theme);
}

fn render_message_box_buttons(
    grid: &mut Grid,
    rect: Rect,
    btn_y: u16,
    dialog: &MessageBoxState,
    theme: &ThemePalette,
) {
    match dialog.kind {
        MessageBoxKind::Ok => {
            let label = "[  OK  ]";
            let w = u16::try_from(label.len()).unwrap_or(8);
            let x = center_x(rect, label.len());
            grid.write_str(x, btn_y, label, theme.selected_fg, theme.selected_bg, true);
            grid.hit_map.push(
                Rect {
                    x,
                    y: btn_y,
                    width: w,
                    height: 1,
                },
                HitAction::MessageBoxButton(true),
            );
        }
        MessageBoxKind::YesNo => {
            let yes_label = "[  Yes  ]";
            let no_label = "[  No  ]";
            let gap = 4usize;
            let total = yes_label.len() + gap + no_label.len();
            let start_x = center_x(rect, total);
            let yes_w = u16::try_from(yes_label.len()).unwrap_or(9);
            let no_x = start_x.saturating_add(yes_w + u16::try_from(gap).unwrap_or(4));
            let no_w = u16::try_from(no_label.len()).unwrap_or(8);

            let (confirm_text, confirm_fill) = if dialog.selected_confirm {
                (theme.selected_fg, theme.selected_bg)
            } else {
                (theme.main_fg, theme.main_bg)
            };
            let (cancel_text, cancel_fill) = if dialog.selected_confirm {
                (theme.main_fg, theme.main_bg)
            } else {
                (theme.selected_fg, theme.selected_bg)
            };

            grid.write_str(
                start_x,
                btn_y,
                yes_label,
                confirm_text,
                confirm_fill,
                dialog.selected_confirm,
            );
            grid.write_str(
                no_x,
                btn_y,
                no_label,
                cancel_text,
                cancel_fill,
                !dialog.selected_confirm,
            );
            grid.hit_map.push(
                Rect {
                    x: start_x,
                    y: btn_y,
                    width: yes_w,
                    height: 1,
                },
                HitAction::MessageBoxButton(true),
            );
            grid.hit_map.push(
                Rect {
                    x: no_x,
                    y: btn_y,
                    width: no_w,
                    height: 1,
                },
                HitAction::MessageBoxButton(false),
            );
        }
    }
}

fn draw_close_tag(grid: &mut Grid, rect: Rect, theme: &ThemePalette) {
    let tag = "✕ Esc";
    let tag_w = u16::try_from(tag.chars().count() + 2).unwrap_or(7);
    let start_x = rect.x.saturating_add(rect.width.saturating_sub(tag_w + 2));
    if start_x <= rect.x + 14 {
        return;
    }
    if let Some(tag_rect) = draw_border_tag(
        grid,
        start_x,
        rect.y,
        rect.x.saturating_add(rect.width - 1),
        None,
        tag,
        theme.hi_fg,
        theme.inactive_fg,
        theme.hi_fg,
        theme.main_bg,
    ) {
        grid.hit_map.push(tag_rect, HitAction::MenuClose);
    }
}

fn center_x(rect: Rect, content_len: usize) -> u16 {
    let w = u16::try_from(content_len).unwrap_or(rect.width);
    rect.x.saturating_add(rect.width.saturating_sub(w) / 2)
}

fn wrap_lines(text: &str, max_cells: usize, max_lines: usize) -> Vec<String> {
    if max_cells == 0 || max_lines == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let sep = usize::from(!current.is_empty());
        if current.chars().count() + sep + word.chars().count() <= max_cells {
            if sep == 1 {
                current.push(' ');
            }
            current.push_str(word);
        } else {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
                if out.len() >= max_lines {
                    return out;
                }
            }
            current.push_str(&clip_text(word, max_cells));
        }
    }
    if !current.is_empty() && out.len() < max_lines {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use btop_model::fixtures::win11_8core_single_gpu_desktop;

    use super::*;
    use crate::menu::MessageBoxAction;
    use crate::{UiHistory, UiState, render_frame};

    #[test]
    fn overlay_menus_dim_background_and_register_clickable_hit_regions() {
        let snap = win11_8core_single_gpu_desktop()[0].clone();
        let cfg = Config::default();
        let hist = UiHistory::from_snapshot(&snap);
        let size = Size {
            width: 120,
            height: 40,
        };

        // Main menu renders ASCII-art buttons and registers MenuItem(0..=2).
        let mut state = UiState::from_config(&cfg);
        state.menu = Some(OverlayMenu::main());
        let g_main = render_frame(&snap, &hist, &state, &cfg, size);
        let t_main = g_main.to_plain_text();
        assert!(t_main.contains("Main Menu"));
        assert!(t_main.contains("█▀▀█ █▀▀█ ▀▀█▀▀"));
        // Background corner cell should be dimmed (not bold, lower luminance than original).
        let base_grid = render_frame(&snap, &hist, &UiState::from_config(&cfg), &cfg, size);
        assert_ne!(
            g_main.get(0, 0).map(|c| c.fg),
            base_grid.get(0, 0).map(|c| c.fg),
            "background should be dimmed behind Main menu"
        );

        // MessageBox registers clickable Yes / No buttons.
        state.menu = Some(OverlayMenu::message_box(
            MessageBoxState::yes_no(
                "Reset Options",
                vec!["Restore all defaults?".to_owned()],
                MessageBoxAction::ResetOptionsToDefaults,
                true,
            ),
            None,
        ));
        let g_msg = render_frame(&snap, &hist, &state, &cfg, size);
        let t_msg = g_msg.to_plain_text();
        assert!(t_msg.contains("Reset Options"));
        assert!(t_msg.contains("[  Yes  ]"));
        assert!(t_msg.contains("[  No  ]"));
    }

    #[test]
    fn live_theme_preview_changes_palette_colors_immediately() {
        let snap = win11_8core_single_gpu_desktop()[0].clone();
        let hist = UiHistory::from_snapshot(&snap);
        let size = Size {
            width: 80,
            height: 24,
        };

        let cfg_default = Config::default();
        let cfg_dracula = Config {
            color_theme: "Dracula".to_owned(),
            ..Config::default()
        };

        let g1 = render_frame(
            &snap,
            &hist,
            &UiState::from_config(&cfg_default),
            &cfg_default,
            size,
        );
        let g2 = render_frame(
            &snap,
            &hist,
            &UiState::from_config(&cfg_dracula),
            &cfg_dracula,
            size,
        );
        assert_ne!(
            g1.get(0, 0).map(|c| c.fg),
            g2.get(0, 0).map(|c| c.fg),
            "switching color_theme must immediately update rendered TrueColor palette"
        );
    }

    #[test]
    fn narrow_and_oversized_layouts_render_all_menus_without_panicking() {
        let snap = win11_8core_single_gpu_desktop()[0].clone();
        let hist = UiHistory::from_snapshot(&snap);

        // Narrow valid CPU-only dashboard (40x24) and tiny size-error viewports (40x12, 24x10).
        let narrow_cfg = Config {
            shown_boxes: "cpu".to_owned(),
            ..Config::default()
        };
        let menus = [
            OverlayMenu::main(),
            OverlayMenu::options(),
            OverlayMenu::help(),
            OverlayMenu::message_box(
                MessageBoxState::yes_no(
                    "Confirm",
                    vec!["Proceed?".to_owned()],
                    MessageBoxAction::DismissOnly,
                    true,
                ),
                None,
            ),
        ];
        for (w, h) in [(40, 24), (40, 12), (24, 10)] {
            for menu in &menus {
                let mut st = UiState::from_config(&narrow_cfg);
                st.menu = Some(menu.clone());
                let g = render_frame(
                    &snap,
                    &hist,
                    &st,
                    &narrow_cfg,
                    Size {
                        width: w,
                        height: h,
                    },
                );
                assert_eq!(g.size().width, w);
                assert_eq!(g.size().height, h);
            }
        }

        // At 80x24 with `cpu mem net proc gpu0` (needs 25 rows -> SizeError), Options must
        // STILL render visibly over the size-error screen so the user can edit `shown_boxes`.
        let too_tall_cfg = Config {
            shown_boxes: "cpu mem net proc gpu0".to_owned(),
            ..Config::default()
        };
        let mut st = UiState::from_config(&too_tall_cfg);
        st.menu = Some(OverlayMenu::options());
        let rendered = render_frame(
            &snap,
            &hist,
            &st,
            &too_tall_cfg,
            Size {
                width: 80,
                height: 24,
            },
        )
        .to_plain_text();
        assert!(
            rendered.contains("Options (F2)"),
            "Options menu must remain visible even when dashboard layout triggers SizeError:\n{rendered}"
        );
        assert!(rendered.contains("color_theme"));
    }
}
