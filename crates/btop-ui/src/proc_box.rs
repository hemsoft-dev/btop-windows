//! Process list panel (`4` / `proc`) with flat sorting, cycle-free tree view (`e`),
//! live substring/`!regex` filtering (`f`/`/`/`Delete`), pause (`u`), follow (`F`), and
//! per-row CPU sparklines (`proc_cpu_graphs`).

use std::collections::BTreeSet;

use btop_config::Config;
use btop_model::{DataState, ProcessEntry, ProcessIdentity, Snapshot};
use btop_term::{Cell, Grid, HitAction, Rect};
use unicode_width::UnicodeWidthChar;

use crate::UiHistory;
use crate::proc_tree::{
    DisplayProcRow, build_visible_rows, compile_proc_filter, expandable_parents,
};
use crate::theme::ThemePalette;
use crate::widgets::{draw_border_tag, draw_box_frame, fill_rect};

const SPARK_GLYPHS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

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

/// Pure view state for the process list panel (selection, scroll, sort, tree, filter, pause, follow).
#[derive(Debug, Clone, PartialEq)]
pub struct ProcViewState {
    pub selected: usize,
    pub scroll_offset: usize,
    pub sort: ProcSort,
    pub reversed: bool,
    pub tree_mode: bool,
    pub aggregate: bool,
    pub collapsed: BTreeSet<ProcessIdentity>,
    pub filter: String,
    pub filter_editing: bool,
    pub paused: bool,
    pub frozen_processes: Option<Vec<ProcessEntry>>,
    pub followed: Option<ProcessIdentity>,
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
            tree_mode: false,
            aggregate: false,
            collapsed: BTreeSet::new(),
            filter: String::new(),
            filter_editing: false,
            paused: false,
            frozen_processes: None,
            followed: None,
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
            tree_mode: config.proc_tree,
            aggregate: config.proc_aggregate,
            collapsed: BTreeSet::new(),
            filter: String::new(),
            filter_editing: false,
            paused: false,
            frozen_processes: None,
            followed: None,
            per_core_cpu: config.proc_per_core,
            mem_bytes: config.proc_mem_bytes,
        }
    }

    /// Returns the active process slice (frozen list when paused, otherwise live snapshot list).
    pub fn active_processes<'a>(&'a self, snapshot: &'a Snapshot) -> Option<&'a [ProcessEntry]> {
        if self.paused
            && let Some(ref frozen) = self.frozen_processes
        {
            return Some(frozen.as_slice());
        }
        match &snapshot.processes {
            DataState::Available(list) | DataState::Stale { value: list, .. } => {
                Some(list.as_slice())
            }
            _ => None,
        }
    }

    /// Number of extra status/error rows rendered above the column header.
    pub fn extra_header_rows(&self) -> usize {
        let has_banner = self.paused || self.followed.is_some();
        let has_regex_err = compile_proc_filter(&self.filter).error_message().is_some();
        usize::from(has_banner) + usize::from(has_regex_err)
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

    /// Toggles cycle-free tree view (`e`).
    pub fn toggle_tree(&mut self) {
        self.tree_mode = !self.tree_mode;
    }

    /// Toggles collapse/expand on the selected tree branch (`Space`).
    pub fn toggle_collapse_selected(&mut self, rows: &[DisplayProcRow<'_>]) -> bool {
        if !self.tree_mode {
            return false;
        }
        let Some(row) = rows.get(self.selected) else {
            return false;
        };
        if !row.has_children {
            return false;
        }
        let id = row.entry.identity;
        if !self.collapsed.remove(&id) {
            self.collapsed.insert(id);
        }
        true
    }

    /// Expands the selected tree branch (`+`). Returns `true` when the key was handled by a branch.
    pub fn expand_selected(&mut self, rows: &[DisplayProcRow<'_>]) -> bool {
        if !self.tree_mode {
            return false;
        }
        let Some(row) = rows.get(self.selected) else {
            return false;
        };
        if !row.has_children {
            return false;
        }
        self.collapsed.remove(&row.entry.identity);
        true
    }

    /// Collapses the selected tree branch (`-`). Returns `true` when the key was handled by a branch.
    pub fn collapse_selected(&mut self, rows: &[DisplayProcRow<'_>]) -> bool {
        if !self.tree_mode {
            return false;
        }
        let Some(row) = rows.get(self.selected) else {
            return false;
        };
        if !row.has_children {
            return false;
        }
        self.collapsed.insert(row.entry.identity);
        true
    }

    /// Collapses all branches when any are expanded, or expands all when all are collapsed (`E`).
    pub fn toggle_collapse_all(&mut self, procs: &[ProcessEntry]) -> bool {
        if !self.tree_mode {
            return false;
        }
        let parents = expandable_parents(procs);
        if parents.is_empty() {
            return false;
        }
        let all_collapsed = parents.iter().all(|id| self.collapsed.contains(id));
        if all_collapsed {
            self.collapsed.clear();
        } else {
            for id in parents {
                self.collapsed.insert(id);
            }
        }
        true
    }

    pub fn begin_filter_edit(&mut self) {
        self.filter_editing = true;
    }

    pub fn end_filter_edit(&mut self) {
        self.filter_editing = false;
    }

    pub fn set_filter(&mut self, value: impl Into<String>) {
        self.filter = value.into();
        self.selected = 0;
        self.scroll_offset = 0;
    }

    pub fn push_filter_char(&mut self, ch: char) {
        if !ch.is_control() {
            self.filter.push(ch);
            self.selected = 0;
            self.scroll_offset = 0;
        }
    }

    pub fn pop_filter_char(&mut self) {
        self.filter.pop();
        self.selected = 0;
        self.scroll_offset = 0;
    }

    pub fn clear_filter(&mut self) -> bool {
        if self.filter.is_empty() && !self.filter_editing {
            return false;
        }
        self.filter.clear();
        self.selected = 0;
        self.scroll_offset = 0;
        true
    }

    /// Toggles pause (`u`), freezing the current process list so live ticks don't shift rows.
    pub fn toggle_pause(&mut self, live_procs: Option<&[ProcessEntry]>) {
        if self.paused {
            self.paused = false;
            self.frozen_processes = None;
        } else if let Some(procs) = live_procs {
            self.paused = true;
            self.frozen_processes = Some(procs.to_vec());
        }
    }

    /// Toggles process following (`F`) on the currently selected row by exact `(PID, CreateTime)`.
    pub fn toggle_follow(&mut self, rows: &[DisplayProcRow<'_>]) -> bool {
        if self.followed.is_some() {
            self.followed = None;
            return true;
        }
        if let Some(row) = rows.get(self.selected) {
            self.followed = Some(row.entry.identity);
            return true;
        }
        false
    }

    /// Updates selection to track the followed `(PID, CreateTime)` row and clamps scroll bounds.
    pub fn sync_follow_and_clamp(&mut self, rows: &[DisplayProcRow<'_>], visible_rows: usize) {
        if let Some(target) = self.followed
            && let Some(idx) = rows.iter().position(|r| r.entry.identity == target)
        {
            self.selected = idx;
        }
        self.clamp(rows.len(), visible_rows);
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
    let view = ProcViewState {
        sort,
        reversed,
        ..ProcViewState::default()
    };
    build_visible_rows(procs, &view)
        .0
        .into_iter()
        .map(|row| row.entry)
        .collect()
}

/// Renders the Process box (`⁴proc`) inside `rect`.
pub fn render_proc_box(
    grid: &mut Grid,
    rect: Rect,
    snapshot: &Snapshot,
    history: &UiHistory,
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

    let active = view.active_processes(snapshot);
    let built = active.map(|procs| build_visible_rows(procs, view));
    let total_visible = built.as_ref().map_or(0, |(rows, _)| rows.len());
    render_proc_chrome(grid, rect, view, total_visible, theme);

    let inner = Rect {
        x: rect.x.saturating_add(1),
        y: rect.y.saturating_add(1),
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    };

    if let Some((rows, filter_error)) = built {
        render_proc_table(
            grid,
            inner,
            snapshot,
            history,
            &rows,
            filter_error.as_deref(),
            view,
            config,
            theme,
        );
        return;
    }

    match &snapshot.processes {
        DataState::WarmingUp => {
            let msg = clip_text(
                "Processes warming up...",
                usize::from(inner.width.saturating_sub(2)),
            );
            grid.write_str(
                inner.x + 1,
                inner.y + 1,
                &msg,
                theme.inactive_fg,
                theme.main_bg,
                false,
            );
        }
        DataState::Denied => {
            let msg = clip_text(
                "Process enumeration access denied",
                usize::from(inner.width.saturating_sub(2)),
            );
            grid.write_str(
                inner.x + 1,
                inner.y + 1,
                &msg,
                theme.hi_fg,
                theme.main_bg,
                true,
            );
        }
        DataState::Unavailable(reason) => {
            let msg = clip_text(
                &format!("Processes unavailable: {reason}"),
                usize::from(inner.width.saturating_sub(2)),
            );
            grid.write_str(
                inner.x + 1,
                inner.y + 1,
                &msg,
                theme.inactive_fg,
                theme.main_bg,
                false,
            );
        }
        DataState::Available(_) | DataState::Stale { .. } => {}
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

    render_proc_filter_and_badges(grid, rect, sort_x, view, theme);

    let bottom_y = rect.y.saturating_add(rect.height - 1);
    let footer = if view.filter_editing {
        "Filter mode: type text or !regex | Enter done | Del clear | Esc exit"
    } else if view.tree_mode {
        "↑↓ sel | e tree | Space/+/- fold | E all | f filter | u pause | F follow"
    } else {
        "↑↓ sel | e tree | f filter | u pause | F follow | ←→ sort | r rev"
    };
    let _ = draw_border_tag(
        grid,
        rect.x.saturating_add(2),
        bottom_y,
        max_top_x,
        None,
        footer,
        theme.proc_box,
        theme.inactive_fg,
        theme.hi_fg,
        theme.main_bg,
    );
}

fn render_proc_filter_and_badges(
    grid: &mut Grid,
    rect: Rect,
    sort_x: u16,
    view: &ProcViewState,
    theme: &ThemePalette,
) {
    let mut right_cursor = sort_x;
    if view.filter_editing || !view.filter.is_empty() {
        let cursor_glyph = if view.filter_editing { "_" } else { "" };
        let filter_tag = format!("f:{}{cursor_glyph}", view.filter);
        let max_filter_w = right_cursor.saturating_sub(rect.x.saturating_add(12));
        if max_filter_w >= 6 {
            let clipped = clip_text(&filter_tag, usize::from(max_filter_w.saturating_sub(2)));
            let tag_w = u16::try_from(clipped.chars().count() + 2).unwrap_or(6);
            let tag_x = right_cursor.saturating_sub(tag_w + 1);
            if tag_x > rect.x.saturating_add(10) {
                let _ = draw_border_tag(
                    grid,
                    tag_x,
                    rect.y,
                    right_cursor.saturating_sub(1),
                    None,
                    &clipped,
                    theme.proc_box,
                    theme.hi_fg,
                    theme.hi_fg,
                    theme.main_bg,
                );
                right_cursor = tag_x;
            }
        }
    }

    let mut badges: Vec<&str> = Vec::new();
    if view.tree_mode {
        badges.push("tree");
    }
    if view.per_core_cpu {
        badges.push("per-core");
    }
    if !view.mem_bytes {
        badges.push("mem%");
    }
    if !badges.is_empty() {
        let badge = badges.join(" | ");
        let badge_w = u16::try_from(badge.chars().count() + 2).unwrap_or(10);
        let badge_x = right_cursor.saturating_sub(badge_w + 1);
        if badge_x > rect.x.saturating_add(11) {
            let _ = draw_border_tag(
                grid,
                badge_x,
                rect.y,
                right_cursor.saturating_sub(1),
                None,
                &badge,
                theme.proc_box,
                theme.title,
                theme.hi_fg,
                theme.main_bg,
            );
        }
    }
}

fn render_status_banners(
    grid: &mut Grid,
    inner: Rect,
    rows: &[DisplayProcRow<'_>],
    filter_error: Option<&str>,
    view: &ProcViewState,
    theme: &ThemePalette,
) -> u16 {
    let mut y = inner.y;
    let max_y = inner.y.saturating_add(inner.height);

    if (view.paused || view.followed.is_some()) && y < max_y {
        let (bg, text) = match (view.paused, view.followed) {
            (true, Some(target)) => {
                let label = format_follow_target(target, rows);
                (
                    theme.proc_banner_bg,
                    format!(" PAUSED | FOLLOWING {label} (u resume, F unfollow)"),
                )
            }
            (true, None) => (
                theme.proc_pause_bg,
                " PAUSED - process list frozen (press u to resume)".to_owned(),
            ),
            (false, Some(target)) => {
                let label = format_follow_target(target, rows);
                (
                    theme.proc_follow_bg,
                    format!(" FOLLOWING {label} (press F to stop)"),
                )
            }
            (false, None) => unreachable!(),
        };
        fill_rect(
            grid,
            Rect {
                x: inner.x,
                y,
                width: inner.width,
                height: 1,
            },
            theme.proc_banner_fg,
            bg,
        );
        let clipped = clip_text(&text, usize::from(inner.width));
        grid.write_str(inner.x, y, &clipped, theme.proc_banner_fg, bg, true);
        y = y.saturating_add(1);
    }

    if let Some(err) = filter_error
        && y < max_y
    {
        let text = clip_text(&format!(" Regex error: {err}"), usize::from(inner.width));
        grid.write_str(inner.x, y, &text, theme.hi_fg, theme.main_bg, true);
        y = y.saturating_add(1);
    }

    y.saturating_sub(inner.y)
}

fn format_follow_target(target: ProcessIdentity, rows: &[DisplayProcRow<'_>]) -> String {
    if let Some(row) = rows.iter().find(|r| r.entry.identity == target) {
        format!("{} (PID {})", row.entry.name, target.pid)
    } else {
        format!("PID {} (exited)", target.pid)
    }
}

struct TableColumns {
    content_w: u16,
    has_scrollbar: bool,
    pid_w: u16,
    prog_w: u16,
    thr_w: u16,
    mem_w: u16,
    spark_w: u16,
    cpu_w: u16,
    col_pid: u16,
    col_prog: u16,
    col_thr: u16,
    col_mem: u16,
    col_spark: u16,
    col_cpu: u16,
}

fn compute_columns(
    inner: Rect,
    row_count: usize,
    visible_rows: usize,
    show_spark: bool,
) -> TableColumns {
    let has_scrollbar = row_count > visible_rows;
    let content_w = inner.width.saturating_sub(u16::from(has_scrollbar));

    let pid_w: u16 = 7;
    let thr_w: u16 = if content_w >= 48 { 6 } else { 0 };
    let mem_w: u16 = 8;
    let spark_w: u16 = if show_spark && content_w >= 62 { 8 } else { 0 };
    let cpu_w: u16 = 7;

    let thr_gap = u16::from(thr_w > 0);
    let spark_gap = u16::from(spark_w > 0);
    let fixed_w = pid_w + thr_w + mem_w + spark_w + cpu_w + 5 + thr_gap + spark_gap;
    let prog_w = content_w.saturating_sub(fixed_w).max(4);

    let col_pid = inner.x + 1;
    let col_prog = col_pid + pid_w + 1;
    let col_thr = col_prog + prog_w + 1;
    let col_mem = if thr_w > 0 {
        col_thr + thr_w + 1
    } else {
        col_prog + prog_w + 1
    };
    let col_spark = col_mem + mem_w + 1;
    let col_cpu = if spark_w > 0 {
        col_spark + spark_w + 1
    } else {
        col_mem + mem_w + 1
    };

    TableColumns {
        content_w,
        has_scrollbar,
        pid_w,
        prog_w,
        thr_w,
        mem_w,
        spark_w,
        cpu_w,
        col_pid,
        col_prog,
        col_thr,
        col_mem,
        col_spark,
        col_cpu,
    }
}

fn render_table_header(
    grid: &mut Grid,
    y: u16,
    cols: &TableColumns,
    view: &ProcViewState,
    theme: &ThemePalette,
) {
    let hdr_fg = |active: bool| if active { theme.hi_fg } else { theme.title };
    grid.write_str(
        cols.col_pid,
        y,
        &format!("{:>w$}", "Pid", w = usize::from(cols.pid_w)),
        hdr_fg(view.sort == ProcSort::Pid),
        theme.main_bg,
        true,
    );
    grid.write_str(
        cols.col_prog,
        y,
        "Program",
        hdr_fg(view.sort == ProcSort::Program),
        theme.main_bg,
        true,
    );
    if cols.thr_w > 0 {
        grid.write_str(
            cols.col_thr,
            y,
            &format!("{:>w$}", "Thr", w = usize::from(cols.thr_w)),
            hdr_fg(view.sort == ProcSort::Threads),
            theme.main_bg,
            true,
        );
    }
    let mem_hdr = if view.mem_bytes { "MemB" } else { "Mem%" };
    grid.write_str(
        cols.col_mem,
        y,
        &format!("{mem_hdr:>w$}", w = usize::from(cols.mem_w)),
        hdr_fg(view.sort == ProcSort::Memory),
        theme.main_bg,
        true,
    );
    if cols.spark_w > 0 {
        grid.write_str(
            cols.col_spark,
            y,
            &format!("{:>w$}", "Graph", w = usize::from(cols.spark_w)),
            theme.inactive_fg,
            theme.main_bg,
            false,
        );
    }
    let cpu_hdr = if view.per_core_cpu { "Core%" } else { "Cpu%" };
    grid.write_str(
        cols.col_cpu,
        y,
        &format!("{cpu_hdr:>w$}", w = usize::from(cols.cpu_w)),
        hdr_fg(matches!(view.sort, ProcSort::CpuLazy | ProcSort::CpuDirect)),
        theme.main_bg,
        true,
    );
}

#[allow(clippy::too_many_arguments)]
fn render_proc_table(
    grid: &mut Grid,
    inner: Rect,
    snapshot: &Snapshot,
    history: &UiHistory,
    rows: &[DisplayProcRow<'_>],
    filter_error: Option<&str>,
    view: &ProcViewState,
    config: &Config,
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

    let banner_rows = render_status_banners(grid, inner, rows, filter_error, view, theme);
    let header_y = inner.y.saturating_add(banner_rows);
    let table_h = inner.height.saturating_sub(banner_rows);
    if table_h < 2 {
        return;
    }

    let visible_rows = usize::from(table_h.saturating_sub(1));
    let mut clamped = view.clone();
    clamped.sync_follow_and_clamp(rows, visible_rows);

    let logical_cpus = match &snapshot.cpu {
        DataState::Available(cpu) | DataState::Stale { value: cpu, .. } => cpu.cores.len().max(1),
        _ => 1,
    };
    let total_mem_bytes = match &snapshot.mem {
        DataState::Available(mem) | DataState::Stale { value: mem, .. } => mem.total_bytes,
        _ => snapshot.total_phys_mem_bytes.unwrap_or(0),
    };

    let cols = compute_columns(inner, rows.len(), visible_rows, config.proc_cpu_graphs);
    render_table_header(grid, header_y, &cols, &clamped, theme);

    for row_idx in 0..visible_rows {
        let abs_idx = clamped.scroll_offset + row_idx;
        let Some(row) = rows.get(abs_idx) else {
            break;
        };
        let y = header_y + 1 + u16::try_from(row_idx).unwrap_or(0);
        render_proc_row(
            grid,
            inner,
            y,
            &cols,
            row,
            history,
            &clamped,
            abs_idx == clamped.selected,
            clamped.followed == Some(row.entry.identity),
            logical_cpus,
            total_mem_bytes,
            theme,
        );
    }

    if cols.has_scrollbar && visible_rows > 0 {
        render_scrollbar(
            grid,
            inner,
            header_y + 1,
            visible_rows,
            rows.len(),
            clamped.scroll_offset,
            theme,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn render_proc_row(
    grid: &mut Grid,
    inner: Rect,
    y: u16,
    cols: &TableColumns,
    row: &DisplayProcRow<'_>,
    history: &UiHistory,
    view: &ProcViewState,
    is_selected: bool,
    is_followed: bool,
    logical_cpus: usize,
    total_mem_bytes: u64,
    theme: &ThemePalette,
) {
    let entry = row.entry;
    let highlighted = is_selected || is_followed;
    let row_bg = if is_followed {
        theme.followed_bg
    } else if is_selected {
        theme.selected_bg
    } else {
        theme.main_bg
    };
    let base_fg = if is_followed {
        theme.followed_fg
    } else if is_selected {
        theme.selected_fg
    } else if entry.is_protected {
        theme.inactive_fg
    } else {
        theme.main_fg
    };

    if highlighted {
        fill_rect(
            grid,
            Rect {
                x: inner.x,
                y,
                width: cols.content_w,
                height: 1,
            },
            base_fg,
            row_bg,
        );
    }

    let pid_str = format!("{:>w$}", entry.identity.pid, w = usize::from(cols.pid_w));
    let full_prog = if row.tree_prefix.is_empty() {
        entry.name.clone()
    } else {
        format!("{}{}", row.tree_prefix, entry.name)
    };
    let prog_str = clip_text(&full_prog, usize::from(cols.prog_w));
    let mem_str = format_mem_column(
        row.display_mem_bytes,
        total_mem_bytes,
        view.mem_bytes,
        usize::from(cols.mem_w),
    );
    #[allow(clippy::cast_precision_loss)]
    let cpu_display = if view.per_core_cpu {
        row.display_cpu_pct * (logical_cpus as f32)
    } else {
        row.display_cpu_pct
    };
    let cpu_str = format!("{cpu_display:>w$.1}", w = usize::from(cols.cpu_w));
    let cpu_fg = if highlighted {
        base_fg
    } else {
        theme.cpu_gradient(cpu_display.min(100.0))
    };

    grid.write_str(cols.col_pid, y, &pid_str, base_fg, row_bg, highlighted);
    grid.write_str(cols.col_prog, y, &prog_str, base_fg, row_bg, highlighted);
    if cols.thr_w > 0 {
        let thr_str = format!("{:>w$}", entry.threads, w = usize::from(cols.thr_w));
        grid.write_str(cols.col_thr, y, &thr_str, base_fg, row_bg, highlighted);
    }
    grid.write_str(cols.col_mem, y, &mem_str, base_fg, row_bg, highlighted);

    if cols.spark_w > 0 {
        let series = history.proc_cpu_slice(entry.identity);
        draw_proc_sparkline(
            grid,
            cols.col_spark,
            y,
            cols.spark_w,
            series,
            row.display_cpu_pct,
            row_bg,
            theme,
        );
    }

    grid.write_str(cols.col_cpu, y, &cpu_str, cpu_fg, row_bg, highlighted);

    grid.hit_map.push(
        Rect {
            x: inner.x,
            y,
            width: cols.content_w,
            height: 1,
        },
        HitAction::SelectProcess(entry.identity.pid),
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_proc_sparkline(
    grid: &mut Grid,
    x: u16,
    y: u16,
    width: u16,
    series: &[f32],
    fallback_pct: f32,
    bg: btop_term::Color,
    theme: &ThemePalette,
) {
    let w = usize::from(width);
    let fallback = [fallback_pct];
    let samples = if series.is_empty() { &fallback } else { series };
    let peak = samples.iter().copied().fold(10.0_f32, f32::max).min(100.0);

    for col in 0..w {
        let from_right = w - 1 - col;
        let val = if from_right < samples.len() {
            samples[samples.len() - 1 - from_right].clamp(0.0, 100.0)
        } else {
            0.0
        };
        if val <= 0.05 {
            continue;
        }
        let ratio = (val / peak).clamp(0.0, 1.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let level = ((ratio * 7.0).round() as usize).min(7);
        let fg = theme.cpu_gradient(val.min(100.0));
        grid.set(
            x.saturating_add(u16::try_from(col).unwrap_or(0)),
            y,
            Cell {
                ch: SPARK_GLYPHS[level],
                fg,
                bg,
                bold: false,
                continuation: false,
            },
        );
    }
}

fn render_scrollbar(
    grid: &mut Grid,
    inner: Rect,
    start_y: u16,
    visible_rows: usize,
    total_rows: usize,
    scroll_offset: usize,
    theme: &ThemePalette,
) {
    let bar_x = inner.x.saturating_add(inner.width - 1);
    let max_scroll = total_rows.saturating_sub(visible_rows).max(1);
    let thumb_row = (scroll_offset * (visible_rows - 1)) / max_scroll;
    for row_idx in 0..visible_rows {
        let y = start_y + u16::try_from(row_idx).unwrap_or(0);
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
    fn pause_freezes_snapshot_and_follow_tracks_moving_process() {
        let tick1 = vec![
            entry(10, "a.exe", 4, 10_000, 5.0),
            entry(20, "target.exe", 4, 20_000, 10.0),
        ];
        let mut view = ProcViewState {
            sort: ProcSort::CpuDirect,
            ..ProcViewState::default()
        };
        let (rows1, _) = build_visible_rows(&tick1, &view);
        assert_eq!(rows1[0].entry.identity.pid, 20);
        assert!(view.toggle_follow(&rows1));
        assert_eq!(view.followed, Some(tick1[1].identity));

        // On tick 2, target.exe drops to index 1; follow keeps selection locked on target.exe.
        let tick2 = vec![
            entry(10, "a.exe", 4, 10_000, 40.0),
            entry(20, "target.exe", 4, 20_000, 2.0),
        ];
        let (rows2, _) = build_visible_rows(&tick2, &view);
        view.sync_follow_and_clamp(&rows2, 10);
        assert_eq!(view.selected, 1);
        assert_eq!(rows2[view.selected].entry.identity.pid, 20);

        // Pausing captures tick2; unpausing drops the frozen copy.
        view.toggle_pause(Some(&tick2));
        assert!(view.paused);
        assert_eq!(view.frozen_processes.as_ref().map(Vec::len), Some(2));
        view.toggle_pause(Some(&tick2));
        assert!(!view.paused);
        assert!(view.frozen_processes.is_none());

        // Second `F` clears follow.
        assert!(view.toggle_follow(&rows2));
        assert_eq!(view.followed, None);
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
        assert_eq!(clip_text("hello", 5), "hello");
        assert_eq!(clip_text("hello world", 5), "hell…");
        assert_eq!(clip_text("🔥test", 3), "🔥…");
        assert_eq!(clip_text("🔥test", 2), "…");
    }
}
