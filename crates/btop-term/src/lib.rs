//! Terminal cell grid, hit-test map, DEC 2026 synchronized VT diff encoder,
//! input abstraction, and Windows Terminal console backend.

pub mod backend;
pub mod input;

use std::fmt::Write as _;

use unicode_width::UnicodeWidthChar;

pub use backend::{ENTER_SEQUENCE, LEAVE_SEQUENCE, TermError, TerminalGuard, restore_terminal_now};
pub use input::{
    CrosstermInput, InputSource, KeyCode, KeyEvent, MouseButton, MouseEvent, MouseKind, TermEvent,
    VecInputSource,
};

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
    CyclePreset,
    AdjustUpdateMs(i32),
    ToggleDisks,
    ToggleIoMode,
    NetPrevIface,
    NetNextIface,
    NetToggleAuto,
    NetToggleSync,
    NetResetTotals,
    ToggleServicesView,
    ProcPrevSort,
    ProcNextSort,
    ProcToggleReverse,
    ProcSortColumn(String),
    ProcBeginFilter,
    SelectProcess(u32),
    SelectService(usize),
    OpenMenu(String),
    MenuItem(usize),
    MenuCategory(usize),
    MenuClose,
    MessageBoxButton(bool),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HitMap {
    regions: Vec<(Rect, HitAction)>,
}

impl HitMap {
    pub fn clear(&mut self) {
        self.regions.clear();
    }

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

    /// Renders the grid to newline-separated plain text with trailing spaces trimmed per row,
    /// suitable for deterministic golden snapshot tests.
    pub fn to_plain_text(&self) -> String {
        let mut out = String::with_capacity(
            usize::from(self.size.height) * (usize::from(self.size.width) + 1),
        );
        for y in 0..self.size.height {
            let mut row = String::with_capacity(usize::from(self.size.width));
            for x in 0..self.size.width {
                if let Some(cell) = self.get(x, y)
                    && !cell.continuation
                {
                    row.push(cell.ch);
                }
            }
            out.push_str(row.trim_end_matches(' '));
            if y + 1 < self.size.height {
                out.push('\n');
            }
        }
        out
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

        let same_size = prev.is_some_and(|p| p.size == next.size);
        if !same_size {
            // Re-hide cursor and reset SGR on full redraws so web/PWA terminals that
            // reconnected or dropped frames recover cleanly without stale attributes.
            out.push_str("\x1b[?25l\x1b[0m");
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
                if same_size
                    && let Some(prev_grid) = prev
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

        if active_bold || active_foreground != Color::Reset || active_background != Color::Reset {
            out.push_str("\x1b[0m");
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

    /// Minimal ANSI/VT terminal emulator verifying that incremental `DiffEncoder`
    /// output reproduces exact cell and style parity with full redraws.
    struct VtScreen {
        grid: Grid,
        cur_x: u16,
        cur_y: u16,
        fg: Color,
        bg: Color,
        bold: bool,
        sync_depth: i32,
    }

    impl VtScreen {
        fn new(size: Size) -> Self {
            Self {
                grid: Grid::new(size),
                cur_x: 0,
                cur_y: 0,
                fg: Color::Reset,
                bg: Color::Reset,
                bold: false,
                sync_depth: 0,
            }
        }

        fn apply(&mut self, stream: &str) {
            let bytes = stream.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
                    i += 2;
                    let start = i;
                    while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                        i += 1;
                    }
                    assert!(i < bytes.len(), "unterminated CSI sequence");
                    let params = &stream[start..i];
                    let final_byte = bytes[i] as char;
                    i += 1;
                    self.handle_csi(params, final_byte);
                } else {
                    let ch = stream[i..].chars().next().expect("valid utf-8");
                    i += ch.len_utf8();
                    let w = u16::try_from(ch.width().unwrap_or(1)).unwrap_or(1);
                    self.grid.set(
                        self.cur_x,
                        self.cur_y,
                        Cell {
                            ch,
                            fg: self.fg,
                            bg: self.bg,
                            bold: self.bold,
                            continuation: false,
                        },
                    );
                    if w == 2 {
                        self.grid.set(
                            self.cur_x + 1,
                            self.cur_y,
                            Cell {
                                ch: ' ',
                                fg: self.fg,
                                bg: self.bg,
                                bold: self.bold,
                                continuation: true,
                            },
                        );
                    }
                    self.cur_x = self.cur_x.saturating_add(w);
                }
            }
        }

        fn handle_csi(&mut self, params: &str, final_byte: char) {
            match (params, final_byte) {
                ("?2026", 'h') => self.sync_depth += 1,
                ("?2026", 'l') => self.sync_depth -= 1,
                ("?25", 'l') => {}
                (_, 'H') => {
                    let mut parts = params.split(';');
                    let row: u16 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1);
                    let col: u16 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1);
                    self.cur_y = row.saturating_sub(1);
                    self.cur_x = col.saturating_sub(1);
                }
                (_, 'm') => {
                    let nums: Vec<u8> = params
                        .split(';')
                        .filter(|s| !s.is_empty())
                        .map(|s| s.parse().expect("valid sgr number"))
                        .collect();
                    let mut idx = 0;
                    while idx < nums.len() {
                        match nums[idx] {
                            0 => {
                                self.fg = Color::Reset;
                                self.bg = Color::Reset;
                                self.bold = false;
                            }
                            1 => self.bold = true,
                            22 => self.bold = false,
                            39 => self.fg = Color::Reset,
                            49 => self.bg = Color::Reset,
                            38 if nums.get(idx + 1) == Some(&2) => {
                                self.fg = Color::Rgb(Rgb {
                                    r: nums[idx + 2],
                                    g: nums[idx + 3],
                                    b: nums[idx + 4],
                                });
                                idx += 4;
                            }
                            48 if nums.get(idx + 1) == Some(&2) => {
                                self.bg = Color::Rgb(Rgb {
                                    r: nums[idx + 2],
                                    g: nums[idx + 3],
                                    b: nums[idx + 4],
                                });
                                idx += 4;
                            }
                            38 if nums.get(idx + 1) == Some(&5) => {
                                self.fg = Color::Indexed(nums[idx + 2]);
                                idx += 2;
                            }
                            48 if nums.get(idx + 1) == Some(&5) => {
                                self.bg = Color::Indexed(nums[idx + 2]);
                                idx += 2;
                            }
                            other => panic!("unexpected SGR code {other}"),
                        }
                        idx += 1;
                    }
                }
                _ => panic!("unexpected CSI {params}{final_byte}"),
            }
        }
    }

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
        assert!(!frame.contains("\x1b[?25l"));
        assert!(!frame.contains("cpu"));
        assert!(frame.contains("mem"));

        let full_frame = encoder.encode(None, &b);
        assert!(full_frame.starts_with("\x1b[?2026h\x1b[?25l\x1b[0m"));
        assert!(full_frame.ends_with("\x1b[?2026l"));
        assert!(full_frame.contains("cpu"));
        assert!(full_frame.contains("mem"));
    }

    #[test]
    fn incremental_diffs_match_full_redraws_across_frames() {
        let size = Size {
            width: 24,
            height: 6,
        };
        let encoder = DiffEncoder { sync_output: true };

        let mut f1 = Grid::new(size);
        f1.write_str(
            0,
            0,
            "CPU 42.5% ⣿⣶⣤⣀",
            Color::Rgb(Rgb {
                r: 119,
                g: 202,
                b: 155,
            }),
            Color::Rgb(Rgb {
                r: 16,
                g: 18,
                b: 24,
            }),
            true,
        );
        f1.write_str(2, 2, "proc-name", Color::Indexed(208), Color::Reset, false);

        let mut f2 = f1.clone();
        f2.write_str(
            4,
            0,
            "99.1% ⣿⣿⣿⣿",
            Color::Rgb(Rgb {
                r: 220,
                g: 76,
                b: 76,
            }),
            Color::Rgb(Rgb {
                r: 16,
                g: 18,
                b: 24,
            }),
            false,
        );
        f2.write_str(0, 5, "Footer status", Color::Reset, Color::Reset, true);

        let mut f3 = Grid::new(size);
        f3.write_str(0, 0, "Cleared & redrawn", Color::Reset, Color::Reset, false);

        let mut incremental = VtScreen::new(size);
        let frames = [&f1, &f2, &f3];
        let mut prev: Option<&Grid> = None;

        for target in frames {
            let diff_bytes = encoder.encode(prev, target);
            assert!(diff_bytes.starts_with("\x1b[?2026h"));
            assert!(diff_bytes.ends_with("\x1b[?2026l"));
            incremental.apply(&diff_bytes);
            assert_eq!(incremental.sync_depth, 0);

            let mut full = VtScreen::new(size);
            let full_bytes = encoder.encode(None, target);
            full.apply(&full_bytes);
            assert_eq!(incremental.grid, full.grid);
            assert_eq!(incremental.grid, *target);
            prev = Some(target);
        }
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
