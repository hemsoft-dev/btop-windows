//! Read-only scrollable, sortable Process list panel (`4` / `proc`).

use btop_config::Config;
use btop_model::{DataState, ProcessEntry, Snapshot};
use btop_term::{Cell, Grid, HitAction, Rect};
use unicode_width::UnicodeWidthChar;

use crate::theme::ThemePalette;
use crate::widgets::{draw_border_tag, draw_box_frame, fill_rect};

/// Supported sort columns for the process list panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProcSort {
    Pid,
    Program,
    Threads,
    Memory,
    #[default]
    CpuLazy,
    CpuDirect,
}

impl ProcSort {
    pub const ALL: [Self; 6] = [
        Self::Pid,
        Self::Program,
        Self::Threads,
        Self::Memory,
        Self::CpuLazy,
        Self::CpuDirect,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pid => "pid",
            Self::Program => "program",
            Self::Threads => "threads",
            Self::Memory => "memory",
            Self::CpuLazy => "cpu lazy",
            Self::CpuDirect => "cpu direct",
        }
    }

    pub fn from_config_str(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "pid" => Self::Pid,
            "program" => Self::Program,
            "threads" => Self::Threads,
            "memory" => Self::Memory,
            "cpu direct" => Self::CpuDirect,
            _ => Self::CpuLazy,
        }
    }

    #[must_use]
    pub fn next(self) -> Self {
        let idx = Self::ALL.iter().position(|&s| s == self).unwrap_or(0);
        Self::ALL[(idx + 1) % Self::ALL.len()]
    }

    #[must_use]
    pub fn prev(self) -> Self {
        let idx = Self::ALL.iter().position(|&s| s == self).unwrap_or(0);
        Self::ALL[(idx + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

/// Pure view state for the process list panel (selection, scroll, sort, and display modes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcViewState {
    pub selected: usize,
    pub scroll_offset: usize,
    pub sort: ProcSort,
    pub reversed: bool,
    pub per_core_cpu: bool,
    pub mem_bytes: bool,
}

impl Default for ProcViewState {
    fn default() -> Self {
        Self {
            selected: 0,
            scroll_offset: 0,
            sort: ProcSort::CpuLazy,
            reversed: false,
            per_core_cpu: false,
            mem_bytes: true,
        }
    }
}

impl ProcViewState {
    pub fn from_config(config: &Config) -> Self {
        Self {
            selected: 0,
            scroll_offset: 0,
            sort: ProcSort::from_config_str(&config.proc_sorting),
            reversed: config.proc_reversed,
            per_core_cpu: config.proc_per_core,
            mem_bytes: config.proc_mem_bytes,
        }
    }

    pub fn move_up(&mut self, total: usize, visible_rows: usize) {
        self.selected = self.selected.saturating_sub(1);
        self.clamp(total, visible_rows);
    }

    pub fn move_down(&mut self, total: usize, visible_rows: usize) {
        if total > 0 {
            self.selected = self.selected.saturating_add(1).min(total - 1);
        }
        self.clamp(total, visible_rows);
    }

    pub fn page_up(&mut self, total: usize, visible_rows: usize) {
        let step = visible_rows.max(1);
        self.selected = self.selected.saturating_sub(step);
        self.clamp(total, visible_rows);
    }

    pub fn page_down(&mut self, total: usize, visible_rows: usize) {
        if total > 0 {
            let step = visible_rows.max(1);
            self.selected = self.selected.saturating_add(step).min(total - 1);
        }
        self.clamp(total, visible_rows);
    }

    pub fn home(&mut self, total: usize, visible_rows: usize) {
        self.selected = 0;
        self.scroll_offset = 0;
        self.clamp(total, visible_rows);
    }

    pub fn end(&mut self, total: usize, visible_rows: usize) {
        self.selected = total.saturating_sub(1);
        self.clamp(total, visible_rows);
    }

    pub fn next_sort(&mut self) {
        self.sort = self.sort.next();
    }

    pub fn prev_sort(&mut self) {
        self.sort = self.sort.prev();
    }

    pub fn toggle_reverse(&mut self) {
        self.reversed = !self.reversed;
    }

    pub fn toggle_per_core(&mut self) {
        self.per_core_cpu = !self.per_core_cpu;
    }

    pub fn toggle_mem_bytes(&mut self) {
        self.mem_bytes = !self.mem_bytes;
    }

    pub fn clamp(&mut self, total: usize, visible_rows: usize) {
        if total == 0 {
            self.selected = 0;
            self.scroll_offset = 0;
            return;
        }
        self.selected = self.selected.min(total - 1);
        let rows = visible_rows.max(1);
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset.saturating_add(rows) {
            self.scroll_offset = self.selected.saturating_sub(rows - 1);
        }
        let max_scroll = total.saturating_sub(rows);
        self.scroll_offset = self.scroll_offset.min(max_scroll);
    }
}

/// Returns references to `procs` sorted deterministically according to `sort` and `reversed`.
pub fn sort_processes(
    procs: &[ProcessEntry],
    sort: ProcSort,
    reversed: bool,
) -> Vec<&ProcessEntry> {
    let mut rows: Vec<&ProcessEntry> = procs.iter().collect();
    rows.sort_by(|a, b| {
        let ord = match sort {
            ProcSort::Pid => a.identity.pid.cmp(&b.identity.pid),
            ProcSort::Program => a
                .name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase())
                .then_with(|| a.identity.pid.cmp(&b.identity.pid)),
            ProcSort::Threads => b
                .threads
                .cmp(&a.threads)
                .then_with(|| a.identity.pid.cmp(&b.identity.pid)),
            ProcSort::Memory => b
                .working_set_private_bytes
                .cmp(&a.working_set_private_bytes)
                .then_with(|| a.identity.pid.cmp(&b.identity.pid)),
            ProcSort::CpuDirect => b
                .cpu_pct
                .total_cmp(&a.cpu_pct)
                .then_with(|| {
                    b.working_set_private_bytes
                        .cmp(&a.working_set_private_bytes)
                })
                .then_with(|| a.identity.pid.cmp(&b.identity.pid)),
            ProcSort::CpuLazy => {
                // Group sub-0.1% differences together and break ties by private working set
                // so idle processes stay steady instead of jittering every tick.
                let a_bucket = cpu_lazy_bucket(a.cpu_pct);
                let b_bucket = cpu_lazy_bucket(b.cpu_pct);
                b_bucket
                    .cmp(&a_bucket)
                    .then_with(|| {
                        b.working_set_private_bytes
                            .cmp(&a.working_set_private_bytes)
                    })
                    .then_with(|| a.identity.pid.cmp(&b.identity.pid))
            }
        };
        if reversed { ord.reverse() } else { ord }
    });
    rows
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn cpu_lazy_bucket(cpu_pct: f32) -> u32 {
    (cpu_pct.clamp(0.0, 100.0) * 10.0).round() as u32
}

/// Renders the read-only scrollable, sortable Process box (`⁴proc`) inside `rect`.
pub fn render_proc_box(
    grid: &mut Grid,
    rect: Rect,
    snapshot: &Snapshot,
    view: &ProcViewState,
    config: &Config,
    theme: &ThemePalette,
) {
    if rect.width < 24 || rect.height < 6 {
        return;
    }

    fill_rect(grid, rect, theme.main_fg, theme.main_bg);
    draw_box_frame(
        grid,
        rect,
        theme.proc_box,
        theme.main_bg,
        config.rounded_corners,
    );

    let total_procs = match &snapshot.processes {
        DataState::Available(list) | DataState::Stale { value: list, .. } => list.len(),
        _ => 0,
    };
    render_proc_chrome(grid, rect, view, total_procs, theme);

    let inner = Rect {
        x: rect.x.saturating_add(1),
        y: rect.y.saturating_add(1),
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    };

    match &snapshot.processes {
        DataState::Available(procs) | DataState::Stale { value: procs, .. } => {
            render_proc_table(grid, inner, snapshot, procs, view, theme);
        }
        DataState::WarmingUp => {
            grid.write_str(
                inner.x + 1,
                inner.y + 1,
                "Processes warming up...",
                theme.inactive_fg,
                theme.main_bg,
                false,
            );
        }
        DataState::Denied => {
            grid.write_str(
                inner.x + 1,
                inner.y + 1,
                "Process enumeration access denied",
                theme.hi_fg,
                theme.main_bg,
                true,
            );
        }
        DataState::Unavailable(reason) => {
            let msg = format!("Processes unavailable: {reason}");
            let clipped: String = msg
                .chars()
                .take(usize::from(inner.width.saturating_sub(2)))
                .collect();
            grid.write_str(
                inner.x + 1,
                inner.y + 1,
                &clipped,
                theme.inactive_fg,
                theme.main_bg,
                false,
            );
        }
    }
}

fn render_proc_chrome(
    grid: &mut Grid,
    rect: Rect,
    view: &ProcViewState,
    total_procs: usize,
    theme: &ThemePalette,
) {
    let max_top_x = rect.x.saturating_add(rect.width - 1);

    if let Some(tag_rect) = draw_border_tag(
        grid,
        rect.x.saturating_add(2),
        rect.y,
        max_top_x,
        Some('⁴'),
        "proc",
        theme.proc_box,
        theme.title,
        theme.hi_fg,
        theme.main_bg,
    ) {
        grid.hit_map
            .push(tag_rect, HitAction::ToggleBox("proc".to_owned()));
    }

    // Right-aligned counter tag: `┐1/50┌`
    let pos_label = if total_procs == 0 {
        "0/0".to_owned()
    } else {
        format!("{}/{total_procs}", view.selected.min(total_procs - 1) + 1)
    };
    let pos_w = u16::try_from(pos_label.chars().count() + 2).unwrap_or(6);
    let pos_x = max_top_x.saturating_sub(pos_w + 1);
    if pos_x > rect.x.saturating_add(12) {
        let _ = draw_border_tag(
            grid,
            pos_x,
            rect.y,
            max_top_x,
            None,
            &pos_label,
            theme.proc_box,
            theme.main_fg,
            theme.hi_fg,
            theme.main_bg,
        );
    }

    // Sort indicator tag: `┐< cpu lazy >┌` or `┐< cpu lazy ↑ >┌`
    let arrow = if view.reversed { "↑" } else { "↓" };
    let sort_label = format!("< {} {arrow} >", view.sort.as_str());
    let sort_w = u16::try_from(sort_label.chars().count() + 2).unwrap_or(14);
    let sort_x = pos_x.saturating_sub(sort_w + 2);
    if sort_x > rect.x.saturating_add(11) {
        let _ = draw_border_tag(
            grid,
            sort_x,
            rect.y,
            pos_x.saturating_sub(1),
            None,
            &sort_label,
            theme.proc_box,
            theme.hi_fg,
            theme.hi_fg,
            theme.main_bg,
        );
    }

    // Optional mode badges (`per-core`, `mem%`) when space allows
    let mode_label = match (view.per_core_cpu, view.mem_bytes) {
        (true, false) => Some("per-core | mem%"),
        (true, true) => Some("per-core"),
        (false, false) => Some("mem%"),
        (false, true) => None,
    };
    if let Some(badge) = mode_label {
        let badge_w = u16::try_from(badge.chars().count() + 2).unwrap_or(10);
        let badge_x = sort_x.saturating_sub(badge_w + 2);
        if badge_x > rect.x.saturating_add(11) {
            let _ = draw_border_tag(
                grid,
                badge_x,
                rect.y,
                sort_x.saturating_sub(1),
                None,
                badge,
                theme.proc_box,
                theme.title,
                theme.hi_fg,
                theme.main_bg,
            );
        }
    }

    let bottom_y = rect.y.saturating_add(rect.height - 1);
    let _ = draw_border_tag(
        grid,
        rect.x.saturating_add(2),
        bottom_y,
        max_top_x,
        None,
        "↑↓ select | ←→ sort | r rev | c core | % mem | q quit",
        theme.proc_box,
        theme.inactive_fg,
        theme.hi_fg,
        theme.main_bg,
    );
}

#[allow(clippy::too_many_lines)]
fn render_proc_table(
    grid: &mut Grid,
    inner: Rect,
    snapshot: &Snapshot,
    procs: &[ProcessEntry],
    view: &ProcViewState,
    theme: &ThemePalette,
) {
    if inner.height < 2 || inner.width < 34 {
        let msg = clip_text("Widen terminal", usize::from(inner.width.saturating_sub(2)));
        grid.write_str(
            inner.x + 1,
            inner.y,
            &msg,
            theme.inactive_fg,
            theme.main_bg,
            false,
        );
        return;
    }

    let sorted = sort_processes(procs, view.sort, view.reversed);
    let visible_rows = usize::from(inner.height.saturating_sub(1));
    let mut clamped_view = view.clone();
    clamped_view.clamp(sorted.len(), visible_rows);

    let logical_cpus = match &snapshot.cpu {
        DataState::Available(cpu) | DataState::Stale { value: cpu, .. } => cpu.cores.len().max(1),
        _ => 1,
    };
    let total_mem_bytes = match &snapshot.mem {
        DataState::Available(mem) | DataState::Stale { value: mem, .. } => mem.total_bytes,
        _ => snapshot.total_phys_mem_bytes.unwrap_or(0),
    };

    let has_scrollbar = sorted.len() > visible_rows;
    let content_w = inner.width.saturating_sub(u16::from(has_scrollbar));

    let pid_w: u16 = 7;
    let thr_w: u16 = if content_w >= 48 { 6 } else { 0 };
    let mem_w: u16 = 8;
    let cpu_w: u16 = 7;
    let thr_gap = u16::from(thr_w > 0);
    let fixed_w = pid_w + thr_w + mem_w + cpu_w + 5 + thr_gap;
    let prog_w = content_w.saturating_sub(fixed_w).max(4);

    let col_pid = inner.x + 1;
    let col_prog = col_pid + pid_w + 1;
    let col_thr = col_prog + prog_w + 1;
    let col_mem = if thr_w > 0 {
        col_thr + thr_w + 1
    } else {
        col_prog + prog_w + 1
    };
    let col_cpu = col_mem + mem_w + 1;

    // Header row
    let header_y = inner.y;
    let hdr_fg = |active: bool| if active { theme.hi_fg } else { theme.title };
    grid.write_str(
        col_pid,
        header_y,
        &format!("{:>w$}", "Pid", w = usize::from(pid_w)),
        hdr_fg(clamped_view.sort == ProcSort::Pid),
        theme.main_bg,
        true,
    );
    grid.write_str(
        col_prog,
        header_y,
        "Program",
        hdr_fg(clamped_view.sort == ProcSort::Program),
        theme.main_bg,
        true,
    );
    if thr_w > 0 {
        grid.write_str(
            col_thr,
            header_y,
            &format!("{:>w$}", "Thr", w = usize::from(thr_w)),
            hdr_fg(clamped_view.sort == ProcSort::Threads),
            theme.main_bg,
            true,
        );
    }
    let mem_hdr = if clamped_view.mem_bytes {
        "MemB"
    } else {
        "Mem%"
    };
    grid.write_str(
        col_mem,
        header_y,
        &format!("{mem_hdr:>w$}", w = usize::from(mem_w)),
        hdr_fg(clamped_view.sort == ProcSort::Memory),
        theme.main_bg,
        true,
    );
    let cpu_hdr = if clamped_view.per_core_cpu {
        "Core%"
    } else {
        "Cpu%"
    };
    grid.write_str(
        col_cpu,
        header_y,
        &format!("{cpu_hdr:>w$}", w = usize::from(cpu_w)),
        hdr_fg(matches!(
            clamped_view.sort,
            ProcSort::CpuLazy | ProcSort::CpuDirect
        )),
        theme.main_bg,
        true,
    );

    // Visible process rows
    for row_idx in 0..visible_rows {
        let abs_idx = clamped_view.scroll_offset + row_idx;
        let Some(entry) = sorted.get(abs_idx) else {
            break;
        };
        let y = inner.y + 1 + u16::try_from(row_idx).unwrap_or(0);
        let is_selected = abs_idx == clamped_view.selected;
        let row_bg = if is_selected {
            theme.selected_bg
        } else {
            theme.main_bg
        };
        let base_fg = if is_selected {
            theme.selected_fg
        } else if entry.is_protected {
            theme.inactive_fg
        } else {
            theme.main_fg
        };

        if is_selected {
            fill_rect(
                grid,
                Rect {
                    x: inner.x,
                    y,
                    width: content_w,
                    height: 1,
                },
                base_fg,
                row_bg,
            );
        }

        let pid_str = format!("{:>w$}", entry.identity.pid, w = usize::from(pid_w));
        let prog_str = clip_text(&entry.name, usize::from(prog_w));
        let mem_str = format_mem_column(
            entry.working_set_private_bytes,
            total_mem_bytes,
            clamped_view.mem_bytes,
            usize::from(mem_w),
        );
        #[allow(clippy::cast_precision_loss)]
        let cpu_display = if clamped_view.per_core_cpu {
            entry.cpu_pct * (logical_cpus as f32)
        } else {
            entry.cpu_pct
        };
        let cpu_str = format!("{cpu_display:>w$.1}", w = usize::from(cpu_w));
        let cpu_fg = if is_selected {
            theme.selected_fg
        } else {
            theme.cpu_gradient(cpu_display.min(100.0))
        };

        grid.write_str(col_pid, y, &pid_str, base_fg, row_bg, is_selected);
        grid.write_str(col_prog, y, &prog_str, base_fg, row_bg, is_selected);
        if thr_w > 0 {
            let thr_str = format!("{:>w$}", entry.threads, w = usize::from(thr_w));
            grid.write_str(col_thr, y, &thr_str, base_fg, row_bg, is_selected);
        }
        grid.write_str(col_mem, y, &mem_str, base_fg, row_bg, is_selected);
        grid.write_str(col_cpu, y, &cpu_str, cpu_fg, row_bg, is_selected);

        grid.hit_map.push(
            Rect {
                x: inner.x,
                y,
                width: content_w,
                height: 1,
            },
            HitAction::SelectProcess(entry.identity.pid),
        );
    }

    if has_scrollbar && visible_rows > 0 {
        let bar_x = inner.x.saturating_add(inner.width - 1);
        let max_scroll = sorted.len().saturating_sub(visible_rows).max(1);
        let thumb_row = (clamped_view.scroll_offset * (visible_rows - 1)) / max_scroll;
        for row_idx in 0..visible_rows {
            let y = inner.y + 1 + u16::try_from(row_idx).unwrap_or(0);
            let is_thumb = row_idx == thumb_row;
            grid.set(
                bar_x,
                y,
                Cell {
                    ch: if is_thumb { '█' } else { '│' },
                    fg: if is_thumb {
                        theme.proc_box
                    } else {
                        theme.div_line
                    },
                    bg: theme.main_bg,
                    bold: false,
                    continuation: false,
                },
            );
        }
    }
}

fn clip_text(text: &str, max_cells: usize) -> String {
    if max_cells == 0 {
        return String::new();
    }
    let total_width: usize = text.chars().map(|ch| ch.width().unwrap_or(0)).sum();
    if total_width <= max_cells {
        return text.to_owned();
    }
    if max_cells == 1 {
        return "…".to_owned();
    }
    let limit = max_cells - 1;
    let mut used = 0usize;
    let mut out = String::new();
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        if w == 0 {
            continue;
        }
        if used + w > limit {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

/// Formats bytes concisely (`0B`, `512K`, `142M`, `1.4G`) or percentage of physical RAM.
pub fn format_mem_column(bytes: u64, total_bytes: u64, as_bytes: bool, width: usize) -> String {
    if !as_bytes {
        let pct = if total_bytes == 0 {
            0.0
        } else {
            #[allow(clippy::cast_precision_loss)]
            let ratio = (bytes as f64 / total_bytes as f64) * 100.0;
            ratio.clamp(0.0, 100.0)
        };
        return format!("{pct:>w$.1}%", w = width.saturating_sub(1));
    }
    let formatted = format_bytes_compact(bytes);
    format!("{formatted:>width$}")
}

/// Formats byte sizes compactly to match `btop`'s process memory column.
#[allow(clippy::cast_precision_loss)]
pub fn format_bytes_compact(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const TIB: f64 = 1024.0 * 1024.0 * 1024.0 * 1024.0;

    let b = bytes as f64;
    if b >= TIB {
        format!("{:.1}T", b / TIB)
    } else if b >= GIB {
        format!("{:.1}G", b / GIB)
    } else if b >= MIB {
        if b >= 100.0 * MIB {
            format!("{:.0}M", b / MIB)
        } else {
            format!("{:.1}M", b / MIB)
        }
    } else if b >= KIB {
        format!("{:.0}K", b / KIB)
    } else {
        format!("{bytes}B")
    }
}

#[cfg(test)]
mod tests {
    use btop_model::ProcessIdentity;

    use super::*;

    fn entry(pid: u32, name: &str, threads: u32, mem: u64, cpu: f32) -> ProcessEntry {
        ProcessEntry {
            identity: ProcessIdentity {
                pid,
                create_time_100ns: 1000,
            },
            parent_pid: 4,
            name: name.to_owned(),
            command_line: None,
            user: None,
            threads,
            cpu_pct: cpu,
            working_set_private_bytes: mem,
            io_read_bytes: 0,
            io_write_bytes: 0,
            is_protected: pid == 4,
        }
    }

    #[test]
    fn sort_columns_and_reverse_behave_deterministically() {
        let procs = vec![
            entry(4, "System", 120, 10_000_000, 0.5),
            entry(200, "alpha.exe", 8, 500_000_000, 12.0),
            entry(100, "zeta.exe", 32, 50_000_000, 3.0),
        ];

        assert_eq!(
            sort_processes(&procs, ProcSort::Pid, false)
                .iter()
                .map(|p| p.identity.pid)
                .collect::<Vec<_>>(),
            vec![4, 100, 200]
        );
        assert_eq!(
            sort_processes(&procs, ProcSort::Program, false)
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha.exe", "System", "zeta.exe"]
        );
        assert_eq!(
            sort_processes(&procs, ProcSort::Memory, false)
                .iter()
                .map(|p| p.identity.pid)
                .collect::<Vec<_>>(),
            vec![200, 100, 4]
        );
        assert_eq!(
            sort_processes(&procs, ProcSort::Threads, true)
                .iter()
                .map(|p| p.identity.pid)
                .collect::<Vec<_>>(),
            vec![200, 100, 4]
        );
        assert_eq!(
            sort_processes(&procs, ProcSort::CpuDirect, false)
                .iter()
                .map(|p| p.identity.pid)
                .collect::<Vec<_>>(),
            vec![200, 100, 4]
        );
    }

    #[test]
    fn view_state_clamps_navigation_and_scroll_window() {
        let mut view = ProcViewState::default();
        view.page_down(50, 10);
        assert_eq!(view.selected, 10);
        assert_eq!(view.scroll_offset, 1);
        view.end(50, 10);
        assert_eq!(view.selected, 49);
        assert_eq!(view.scroll_offset, 40);
        view.home(50, 10);
        assert_eq!(view.selected, 0);
        assert_eq!(view.scroll_offset, 0);
    }

    #[test]
    fn formats_memory_bytes_and_percent() {
        assert_eq!(format_bytes_compact(0), "0B");
        assert_eq!(format_bytes_compact(512 * 1024), "512K");
        assert_eq!(format_bytes_compact(142 * 1024 * 1024), "142M");
        assert_eq!(
            format_mem_column(16 * 1024 * 1024 * 1024, 32 * 1024 * 1024 * 1024, false, 8),
            "   50.0%"
        );
    }

    #[test]
    fn clip_text_respects_wide_display_cells() {
        let clipped = clip_text("日本語プロセス.exe", 7);
        let display_w: usize = clipped.chars().map(|c| c.width().unwrap_or(0)).sum();
        assert!(
            display_w <= 7,
            "clipped width {display_w} exceeded 7: {clipped}"
        );
        assert!(clipped.ends_with('…'));
    }
}
