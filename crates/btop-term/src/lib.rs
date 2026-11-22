//! Terminal cell grid, hit-test map, and DEC 2026 synchronized VT diff encoder.

use std::fmt::Write as _;

use unicode_width::UnicodeWidthChar;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Size {
    pub width: u16,
    pub height: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub fn contains(self, col: u16, row: u16) -> bool {
        col >= self.x
            && row >= self.y
            && col < self.x.saturating_add(self.width)
            && row < self.y.saturating_add(self.height)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    #[default]
    Reset,
    Rgb(Rgb),
    Indexed(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub continuation: bool,
}

impl Cell {
    pub const BLANK: Self = Self {
        ch: ' ',
        fg: Color::Reset,
        bg: Color::Reset,
        bold: false,
        continuation: false,
    };
}

/// Action mapped to a clickable cell region during the same render pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HitAction {
    ToggleBox(String),
    SelectProcess(u32),
    OpenMenu(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HitMap {
    regions: Vec<(Rect, HitAction)>,
}

impl HitMap {
    pub fn push(&mut self, rect: Rect, action: HitAction) {
        self.regions.push((rect, action));
    }

    pub fn hit_test(&self, col: u16, row: u16) -> Option<&HitAction> {
        self.regions
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(col, row))
            .map(|(_, action)| action)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grid {
    size: Size,
    cells: Vec<Cell>,
    pub hit_map: HitMap,
}

impl Grid {
    pub fn new(size: Size) -> Self {
        let len = usize::from(size.width) * usize::from(size.height);
        Self {
            size,
            cells: vec![Cell::BLANK; len],
            hit_map: HitMap::default(),
        }
    }

    pub fn size(&self) -> Size {
        self.size
    }

    pub fn get(&self, x: u16, y: u16) -> Option<&Cell> {
        if x >= self.size.width || y >= self.size.height {
            return None;
        }
        let idx = usize::from(y) * usize::from(self.size.width) + usize::from(x);
        self.cells.get(idx)
    }

    pub fn set(&mut self, x: u16, y: u16, cell: Cell) {
        if x >= self.size.width || y >= self.size.height {
            return;
        }
        let idx = usize::from(y) * usize::from(self.size.width) + usize::from(x);
        if let Some(slot) = self.cells.get_mut(idx) {
            *slot = cell;
        }
    }

    /// Writes a UTF-8 string respecting Unicode display width (`unicode-width`),
    /// never overflowing the row width.
    pub fn write_str(&mut self, mut x: u16, y: u16, text: &str, fg: Color, bg: Color, bold: bool) {
        if y >= self.size.height {
            return;
        }
        for ch in text.chars() {
            let width = u16::try_from(ch.width().unwrap_or(0)).unwrap_or(0);
            if width == 0 {
                continue;
            }
            if x.saturating_add(width) > self.size.width {
                break;
            }
            self.set(
                x,
                y,
                Cell {
                    ch,
                    fg,
                    bg,
                    bold,
                    continuation: false,
                },
            );
            if width == 2 {
                self.set(
                    x + 1,
                    y,
                    Cell {
                        ch: ' ',
                        fg,
                        bg,
                        bold,
                        continuation: true,
                    },
                );
            }
            x = x.saturating_add(width);
        }
    }
}

/// Encodes incremental cell diffs into a single ANSI/VT frame buffer, optionally
/// wrapped in DEC 2026 synchronized output sequences (`CSI ?2026h ... CSI ?2026l`).
#[derive(Debug, Default)]
pub struct DiffEncoder {
    pub sync_output: bool,
}

impl DiffEncoder {
    pub fn encode(&self, prev: Option<&Grid>, next: &Grid) -> String {
        let mut out = String::with_capacity(4096);
        if self.sync_output {
            out.push_str("\x1b[?2026h");
        }

        let mut active_foreground = Color::Reset;
        let mut active_background = Color::Reset;
        let mut active_bold = false;
        let mut cursor: Option<(u16, u16)> = None;

        for y in 0..next.size.height {
            for x in 0..next.size.width {
                let Some(cell) = next.get(x, y) else {
                    continue;
                };
                if cell.continuation {
                    continue;
                }
                if let Some(prev_grid) = prev
                    && prev_grid.size == next.size
                    && prev_grid.get(x, y) == Some(cell)
                {
                    continue;
                }

                if cursor != Some((x, y)) {
                    let _ = write!(out, "\x1b[{};{}H", y + 1, x + 1);
                }

                if cell.bold != active_bold {
                    out.push_str(if cell.bold { "\x1b[1m" } else { "\x1b[22m" });
                    active_bold = cell.bold;
                }
                if cell.fg != active_foreground {
                    write_color(&mut out, cell.fg, true);
                    active_foreground = cell.fg;
                }
                if cell.bg != active_background {
                    write_color(&mut out, cell.bg, false);
                    active_background = cell.bg;
                }
                out.push(cell.ch);
                let step = u16::try_from(cell.ch.width().unwrap_or(1)).unwrap_or(1);
                cursor = Some((x.saturating_add(step), y));
            }
        }

        if self.sync_output {
            out.push_str("\x1b[?2026l");
        }
        out
    }
}

fn write_color(out: &mut String, color: Color, foreground: bool) {
    match color {
        Color::Reset => out.push_str(if foreground { "\x1b[39m" } else { "\x1b[49m" }),
        Color::Rgb(Rgb { r, g, b }) => {
            let code = if foreground { 38 } else { 48 };
            let _ = write!(out, "\x1b[{code};2;{r};{g};{b}m");
        }
        Color::Indexed(idx) => {
            let code = if foreground { 38 } else { 48 };
            let _ = write!(out, "\x1b[{code};5;{idx}m");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_encoder_emits_only_changed_cells_and_dec_2026() {
        let size = Size {
            width: 10,
            height: 2,
        };
        let mut a = Grid::new(size);
        a.write_str(0, 0, "cpu", Color::Reset, Color::Reset, false);

        let mut b = a.clone();
        b.write_str(
            0,
            1,
            "mem",
            Color::Rgb(Rgb {
                r: 10,
                g: 20,
                b: 30,
            }),
            Color::Reset,
            true,
        );

        let encoder = DiffEncoder { sync_output: true };
        let frame = encoder.encode(Some(&a), &b);
        assert!(frame.starts_with("\x1b[?2026h"));
        assert!(frame.ends_with("\x1b[?2026l"));
        assert!(!frame.contains("cpu"));
        assert!(frame.contains("mem"));
    }

    #[test]
    fn hit_map_prefers_topmost_region() {
        let mut map = HitMap::default();
        map.push(
            Rect {
                x: 0,
                y: 0,
                width: 20,
                height: 10,
            },
            HitAction::ToggleBox("cpu".to_owned()),
        );
        map.push(
            Rect {
                x: 2,
                y: 2,
                width: 5,
                height: 1,
            },
            HitAction::OpenMenu("options".to_owned()),
        );
        assert_eq!(
            map.hit_test(3, 2),
            Some(&HitAction::OpenMenu("options".to_owned()))
        );
        assert_eq!(
            map.hit_test(0, 0),
            Some(&HitAction::ToggleBox("cpu".to_owned()))
        );
    }
}
