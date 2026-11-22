//! Reusable pure UI widgets: `BoxFrame`, `Meter`, and `Graph` (`braille`, `block`, `tty`).

use btop_config::GraphSymbol;
use btop_term::{Cell, Color, Grid, Rect};

use crate::theme::{GradientStops, ThemePalette};

const BLOCK_UP: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Fills every cell inside `rect` with spaces using `fg` and `bg`.
pub fn fill_rect(grid: &mut Grid, rect: Rect, fg: Color, bg: Color) {
    let x_end = rect.x.saturating_add(rect.width).min(grid.size().width);
    let y_end = rect.y.saturating_add(rect.height).min(grid.size().height);
    for y in rect.y..y_end {
        for x in rect.x..x_end {
            grid.set(
                x,
                y,
                Cell {
                    ch: ' ',
                    fg,
                    bg,
                    bold: false,
                    continuation: false,
                },
            );
        }
    }
}

/// Draws a rounded (`╭─╮│╰─╯`) or square (`┌─┐│└─┘`) box frame inside `rect`.
pub fn draw_box_frame(grid: &mut Grid, rect: Rect, border_fg: Color, bg: Color, rounded: bool) {
    if rect.width < 2 || rect.height < 2 {
        return;
    }
    let (tl, tr, bl, br) = if rounded {
        ('╭', '╮', '╰', '╯')
    } else {
        ('┌', '┐', '└', '┘')
    };

    let right = rect.x.saturating_add(rect.width - 1);
    let bottom = rect.y.saturating_add(rect.height - 1);

    let set_ch = |g: &mut Grid, x: u16, y: u16, ch: char| {
        g.set(
            x,
            y,
            Cell {
                ch,
                fg: border_fg,
                bg,
                bold: false,
                continuation: false,
            },
        );
    };

    set_ch(grid, rect.x, rect.y, tl);
    set_ch(grid, right, rect.y, tr);
    set_ch(grid, rect.x, bottom, bl);
    set_ch(grid, right, bottom, br);

    for x in (rect.x + 1)..right {
        set_ch(grid, x, rect.y, '─');
        set_ch(grid, x, bottom, '─');
    }
    for y in (rect.y + 1)..bottom {
        set_ch(grid, rect.x, y, '│');
        set_ch(grid, right, y, '│');
    }
}

/// Draws an embedded `btop`-style border tag (`┐<hotkey><label>┌`) clipped before `max_x`.
/// Returns the rendered [`Rect`] for optional `HitMap` registration.
#[allow(clippy::too_many_arguments)]
pub fn draw_border_tag(
    grid: &mut Grid,
    x: u16,
    y: u16,
    max_x: u16,
    hotkey: Option<char>,
    label: &str,
    border_fg: Color,
    title_fg: Color,
    hi_fg: Color,
    bg: Color,
) -> Option<Rect> {
    if max_x <= x.saturating_add(3) {
        return None;
    }
    let available_inner = usize::from(max_x.saturating_sub(x).saturating_sub(2));
    let mut cursor = x;

    grid.write_str(cursor, y, "┐", border_fg, bg, false);
    cursor = cursor.saturating_add(1);

    let mut used = 0usize;
    if let Some(hk) = hotkey
        && used < available_inner
    {
        let mut buf = [0u8; 4];
        grid.write_str(cursor, y, hk.encode_utf8(&mut buf), hi_fg, bg, true);
        cursor = cursor.saturating_add(1);
        used += 1;
    }

    let remaining = available_inner.saturating_sub(used);
    if remaining > 0 {
        let clipped: String = label.chars().take(remaining).collect();
        let len = u16::try_from(clipped.chars().count()).unwrap_or(0);
        grid.write_str(cursor, y, &clipped, title_fg, bg, true);
        cursor = cursor.saturating_add(len);
    }

    grid.write_str(cursor, y, "┌", border_fg, bg, false);
    cursor = cursor.saturating_add(1);

    Some(Rect {
        x,
        y,
        width: cursor.saturating_sub(x),
        height: 1,
    })
}

/// Draws a horizontal `btop` meter bar (`■`) colored along the CPU gradient.
pub fn draw_meter(grid: &mut Grid, x: u16, y: u16, width: u16, percent: f32, theme: &ThemePalette) {
    draw_meter_with_gradient(grid, x, y, width, percent, theme.cpu_stops(), theme);
}

/// Draws a horizontal `btop` meter bar (`■`) colored along custom [`GradientStops`].
pub fn draw_meter_with_gradient(
    grid: &mut Grid,
    x: u16,
    y: u16,
    width: u16,
    percent: f32,
    stops: GradientStops,
    theme: &ThemePalette,
) {
    if width == 0 {
        return;
    }
    let clamped = percent.clamp(0.0, 100.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let filled = ((clamped / 100.0) * f32::from(width)).round() as u16;

    for col in 0..width {
        let pos_pct = if width <= 1 {
            clamped
        } else {
            (f32::from(col) / f32::from(width - 1)) * 100.0
        };
        let fg = if col < filled {
            stops.sample(pos_pct)
        } else {
            theme.meter_bg
        };
        grid.set(
            x.saturating_add(col),
            y,
            Cell {
                ch: '■',
                fg,
                bg: theme.main_bg,
                bold: false,
                continuation: false,
            },
        );
    }
}

/// Draws a time-series graph inside `rect` using `braille`, `block`, or `tty` symbols
/// colored with the CPU gradient.
pub fn draw_graph(
    grid: &mut Grid,
    rect: Rect,
    values: &[f32],
    symbol: GraphSymbol,
    inverted: bool,
    theme: &ThemePalette,
) {
    draw_graph_with_gradient(
        grid,
        rect,
        values,
        symbol,
        inverted,
        theme.cpu_stops(),
        theme,
    );
}

/// Draws a time-series graph inside `rect` using custom [`GradientStops`].
///
/// Values are right-aligned so the newest sample sits on the right edge. When `inverted`
/// is `true`, bars grow downward from the top edge (used for the lower split CPU graph).
pub fn draw_graph_with_gradient(
    grid: &mut Grid,
    rect: Rect,
    values: &[f32],
    symbol: GraphSymbol,
    inverted: bool,
    stops: GradientStops,
    theme: &ThemePalette,
) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }

    match symbol {
        GraphSymbol::Braille => draw_braille_graph(grid, rect, values, inverted, stops, theme),
        GraphSymbol::Block => draw_block_graph(grid, rect, values, inverted, stops, theme),
        GraphSymbol::Tty => draw_tty_graph(grid, rect, values, inverted, stops, theme),
    }
}

fn draw_braille_graph(
    grid: &mut Grid,
    rect: Rect,
    values: &[f32],
    inverted: bool,
    stops: GradientStops,
    theme: &ThemePalette,
) {
    const LEFT_BITS: [u8; 4] = [0x01, 0x02, 0x04, 0x40];
    const RIGHT_BITS: [u8; 4] = [0x08, 0x10, 0x20, 0x80];

    let width = usize::from(rect.width);
    let height = usize::from(rect.height);
    let total_sub_cols = width * 2;
    let total_sub_rows = height * 4;

    // Right-align samples to the rightmost sub-column.
    let sample_at = |sub_col: usize| -> f32 {
        let from_right = total_sub_cols - 1 - sub_col;
        if from_right < values.len() {
            values[values.len() - 1 - from_right].clamp(0.0, 100.0)
        } else {
            0.0
        }
    };

    for col in 0..width {
        let v_left = sample_at(col * 2);
        let v_right = sample_at(col * 2 + 1);
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let fill_left = ((v_left / 100.0) * (total_sub_rows as f32)).round() as usize;
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let fill_right = ((v_right / 100.0) * (total_sub_rows as f32)).round() as usize;

        for row in 0..height {
            let mut mask = 0u8;
            for sub_r in 0..4 {
                let global_r = row * 4 + sub_r;
                let left_lit = if inverted {
                    global_r < fill_left
                } else {
                    (total_sub_rows - 1 - global_r) < fill_left
                };
                let right_lit = if inverted {
                    global_r < fill_right
                } else {
                    (total_sub_rows - 1 - global_r) < fill_right
                };
                if left_lit {
                    mask |= LEFT_BITS[sub_r];
                }
                if right_lit {
                    mask |= RIGHT_BITS[sub_r];
                }
            }

            if mask == 0 {
                continue;
            }
            let ch = char::from_u32(0x2800 + u32::from(mask)).unwrap_or(' ');
            let fg = stops.sample(row_gradient_percent(row, height, inverted));
            grid.set(
                rect.x.saturating_add(u16::try_from(col).unwrap_or(0)),
                rect.y.saturating_add(u16::try_from(row).unwrap_or(0)),
                Cell {
                    ch,
                    fg,
                    bg: theme.main_bg,
                    bold: false,
                    continuation: false,
                },
            );
        }
    }
}

fn draw_block_graph(
    grid: &mut Grid,
    rect: Rect,
    values: &[f32],
    inverted: bool,
    stops: GradientStops,
    theme: &ThemePalette,
) {
    let width = usize::from(rect.width);
    let height = usize::from(rect.height);
    let total_levels = height * 8;

    for col in 0..width {
        let from_right = width - 1 - col;
        let val = if from_right < values.len() {
            values[values.len() - 1 - from_right].clamp(0.0, 100.0)
        } else {
            0.0
        };
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let filled = ((val / 100.0) * (total_levels as f32)).round() as usize;

        for row in 0..height {
            let ch = if inverted {
                let row_start = row * 8;
                let part = filled.saturating_sub(row_start).min(8);
                match part {
                    0 => ' ',
                    1..=2 => '▔',
                    3..=5 => '▀',
                    _ => '█',
                }
            } else {
                let row_from_bottom = height - 1 - row;
                let row_start = row_from_bottom * 8;
                let part = filled.saturating_sub(row_start).min(8);
                BLOCK_UP[part]
            };

            if ch == ' ' {
                continue;
            }
            let fg = stops.sample(row_gradient_percent(row, height, inverted));
            grid.set(
                rect.x.saturating_add(u16::try_from(col).unwrap_or(0)),
                rect.y.saturating_add(u16::try_from(row).unwrap_or(0)),
                Cell {
                    ch,
                    fg,
                    bg: theme.main_bg,
                    bold: false,
                    continuation: false,
                },
            );
        }
    }
}

fn draw_tty_graph(
    grid: &mut Grid,
    rect: Rect,
    values: &[f32],
    inverted: bool,
    stops: GradientStops,
    theme: &ThemePalette,
) {
    let width = usize::from(rect.width);
    let height = usize::from(rect.height);

    for col in 0..width {
        let from_right = width - 1 - col;
        let val = if from_right < values.len() {
            values[values.len() - 1 - from_right].clamp(0.0, 100.0)
        } else {
            0.0
        };
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let filled_rows = ((val / 100.0) * (height as f32)).round() as usize;

        for row in 0..height {
            let lit = if inverted {
                row < filled_rows
            } else {
                (height - 1 - row) < filled_rows
            };
            if !lit {
                continue;
            }
            let fg = stops.sample(row_gradient_percent(row, height, inverted));
            grid.set(
                rect.x.saturating_add(u16::try_from(col).unwrap_or(0)),
                rect.y.saturating_add(u16::try_from(row).unwrap_or(0)),
                Cell {
                    ch: '#',
                    fg,
                    bg: theme.main_bg,
                    bold: false,
                    continuation: false,
                },
            );
        }
    }
}

#[allow(clippy::cast_precision_loss)]
fn row_gradient_percent(row: usize, height: usize, inverted: bool) -> f32 {
    if height <= 1 {
        return 50.0;
    }
    let ratio = if inverted {
        (row as f32) / ((height - 1) as f32)
    } else {
        ((height - 1 - row) as f32) / ((height - 1) as f32)
    };
    (ratio * 100.0).clamp(0.0, 100.0)
}

#[cfg(test)]
mod tests {
    use btop_term::Size;

    use super::*;

    #[test]
    fn braille_graph_renders_upward_and_inverted_accurately() {
        let theme = ThemePalette::default_dark(true);
        let size = Size {
            width: 4,
            height: 2,
        };
        let rect = Rect {
            x: 0,
            y: 0,
            width: 4,
            height: 2,
        };

        // Rightmost column gets [100.0, 100.0] -> full braille `⣿` in both rows.
        let mut up = Grid::new(size);
        draw_graph(
            &mut up,
            rect,
            &[0.0, 0.0, 50.0, 50.0, 100.0, 100.0],
            GraphSymbol::Braille,
            false,
            &theme,
        );
        assert_eq!(up.get(3, 0).map(|c| c.ch), Some('⣿'));
        assert_eq!(up.get(3, 1).map(|c| c.ch), Some('⣿'));
        assert_eq!(up.get(2, 0).map(|c| c.ch), Some(' '));
        assert_eq!(up.get(2, 1).map(|c| c.ch), Some('⣿'));

        // Inverted 50% fills top row and leaves bottom row empty.
        let mut down = Grid::new(size);
        draw_graph(
            &mut down,
            rect,
            &[50.0, 50.0],
            GraphSymbol::Braille,
            true,
            &theme,
        );
        assert_eq!(down.get(3, 0).map(|c| c.ch), Some('⣿'));
        assert_eq!(down.get(3, 1).map(|c| c.ch), Some(' '));
    }

    #[test]
    fn block_graph_renders_upward_and_inverted() {
        let theme = ThemePalette::default_dark(true);
        let size = Size {
            width: 3,
            height: 2,
        };
        let rect = Rect {
            x: 0,
            y: 0,
            width: 3,
            height: 2,
        };

        let mut up = Grid::new(size);
        draw_graph(
            &mut up,
            rect,
            &[0.0, 50.0, 100.0],
            GraphSymbol::Block,
            false,
            &theme,
        );
        assert_eq!(up.get(2, 0).map(|c| c.ch), Some('█'));
        assert_eq!(up.get(2, 1).map(|c| c.ch), Some('█'));
        assert_eq!(up.get(1, 0).map(|c| c.ch), Some(' '));
        assert_eq!(up.get(1, 1).map(|c| c.ch), Some('█'));

        let mut down = Grid::new(size);
        draw_graph(&mut down, rect, &[50.0], GraphSymbol::Block, true, &theme);
        assert_eq!(down.get(2, 0).map(|c| c.ch), Some('█'));
        assert_eq!(down.get(2, 1).map(|c| c.ch), Some(' '));
    }

    #[test]
    fn box_frame_respects_rounded_and_square_corners() {
        let theme = ThemePalette::default_dark(true);
        let mut grid = Grid::new(Size {
            width: 10,
            height: 4,
        });
        let rect = Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 4,
        };
        draw_box_frame(&mut grid, rect, theme.cpu_box, theme.main_bg, true);
        assert_eq!(grid.get(0, 0).map(|c| c.ch), Some('╭'));
        assert_eq!(grid.get(9, 3).map(|c| c.ch), Some('╯'));

        draw_box_frame(&mut grid, rect, theme.cpu_box, theme.main_bg, false);
        assert_eq!(grid.get(0, 0).map(|c| c.ch), Some('┌'));
        assert_eq!(grid.get(9, 3).map(|c| c.ch), Some('┘'));
    }
}
