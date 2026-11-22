//! Toggleable Windows Services (SCM) view rendered inside the `proc` panel (`S` or tab click),
//! joining linked service PIDs with live CPU and private working-set metrics from `Snapshot`.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use btop_config::Config;
use btop_model::{DataState, ProcessEntry, ServiceEntry, Snapshot};
use btop_term::{Color, Grid, HitAction, Rect};

use crate::mem_box::format_mem_bytes;
use crate::proc_box::clip_text;
use crate::proc_tree::{CompiledFilter, compile_proc_filter};
use crate::theme::ThemePalette;
use crate::widgets::{draw_border_tag, draw_box_frame, fill_rect};

/// Supported sort columns for the Windows Services view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ServiceSort {
    Name,
    DisplayName,
    #[default]
    State,
    StartType,
    Pid,
    Cpu,
    Memory,
}

impl ServiceSort {
    pub const ALL: [Self; 7] = [
        Self::Name,
        Self::DisplayName,
        Self::State,
        Self::StartType,
        Self::Pid,
        Self::Cpu,
        Self::Memory,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::DisplayName => "display",
            Self::State => "state",
            Self::StartType => "start",
            Self::Pid => "pid",
            Self::Cpu => "cpu",
            Self::Memory => "memory",
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

/// Pure UI interaction state for the toggleable Windows Services view.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServiceViewState {
    pub active: bool,
    pub is_elevated: bool,
    pub selected: usize,
    pub scroll_offset: usize,
    pub sort: ServiceSort,
    pub reversed: bool,
    pub filter: String,
    pub filter_editing: bool,
    pub action_status: Option<String>,
}

impl ServiceViewState {
    /// Number of extra status/error rows rendered above the column header (always includes the
    /// elevation Start/Stop indicator bar).
    pub fn extra_header_rows(&self) -> usize {
        let has_regex_err = compile_proc_filter(&self.filter).error_message().is_some();
        let has_action = self.action_status.is_some();
        1 + usize::from(has_regex_err) + usize::from(has_action)
    }

    pub fn toggle_active(&mut self) -> bool {
        self.active = !self.active;
        if !self.active {
            self.filter_editing = false;
        }
        self.active
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

/// Service row joined with live CPU% and private working-set memory from its hosting PID.
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceRow<'a> {
    pub entry: &'a ServiceEntry,
    pub cpu_pct: Option<f32>,
    pub memory_bytes: Option<u64>,
    pub is_pending: bool,
}

/// Returns `true` when `state` is a transitional SCM pending state (`Start Pending`,
/// `Stop Pending`, `Continue Pending`, `Pause Pending`).
pub fn is_pending_service_state(state: &str) -> bool {
    state.to_ascii_lowercase().contains("pending")
}

/// Joins `services` with live `processes` by PID, applies `view.filter`, and sorts deterministically.
pub fn build_visible_service_rows<'a>(
    services: &'a [ServiceEntry],
    processes: Option<&[ProcessEntry]>,
    view: &ServiceViewState,
) -> (Vec<ServiceRow<'a>>, Option<String>) {
    let mut pid_metrics: BTreeMap<u32, (f32, u64)> = BTreeMap::new();
    if let Some(procs) = processes {
        for proc_entry in procs {
            pid_metrics.insert(
                proc_entry.identity.pid,
                (proc_entry.cpu_pct, proc_entry.working_set_private_bytes),
            );
        }
    }

    let compiled = compile_proc_filter(&view.filter);
    let filter_error = compiled.error_message().map(str::to_owned);

    let mut rows: Vec<ServiceRow<'a>> = services
        .iter()
        .filter(|svc| matches_service_filter(svc, &compiled))
        .map(|entry| {
            let metrics = entry.pid.and_then(|pid| pid_metrics.get(&pid).copied());
            ServiceRow {
                entry,
                cpu_pct: metrics.map(|(cpu, _)| cpu),
                memory_bytes: metrics.map(|(_, mem)| mem),
                is_pending: is_pending_service_state(&entry.state),
            }
        })
        .collect();

    rows.sort_by(|a, b| compare_service_rows(a, b, view.sort, view.reversed));
    (rows, filter_error)
}

fn matches_service_filter(svc: &ServiceEntry, filter: &CompiledFilter) -> bool {
    match filter {
        CompiledFilter::None | CompiledFilter::InvalidRegex(_) => true,
        CompiledFilter::Substring(needle) => {
            svc.name.to_ascii_lowercase().contains(needle)
                || svc.display_name.to_ascii_lowercase().contains(needle)
                || svc.state.to_ascii_lowercase().contains(needle)
                || svc.start_type.to_ascii_lowercase().contains(needle)
                || svc.pid.is_some_and(|p| p.to_string().contains(needle))
        }
        CompiledFilter::Regex(re) => {
            re.is_match(&svc.name)
                || re.is_match(&svc.display_name)
                || re.is_match(&svc.state)
                || re.is_match(&svc.start_type)
                || svc.pid.is_some_and(|p| re.is_match(&p.to_string()))
        }
    }
}

fn state_rank(state: &str) -> u8 {
    if is_pending_service_state(state) {
        0
    } else if state.eq_ignore_ascii_case("running") {
        1
    } else if state.eq_ignore_ascii_case("paused") {
        2
    } else if state.eq_ignore_ascii_case("stopped") {
        3
    } else {
        4
    }
}

fn compare_service_rows(
    a: &ServiceRow<'_>,
    b: &ServiceRow<'_>,
    sort: ServiceSort,
    reversed: bool,
) -> Ordering {
    let ord = match sort {
        ServiceSort::Name => a
            .entry
            .name
            .to_ascii_lowercase()
            .cmp(&b.entry.name.to_ascii_lowercase()),
        ServiceSort::DisplayName => a
            .entry
            .display_name
            .to_ascii_lowercase()
            .cmp(&b.entry.display_name.to_ascii_lowercase()),
        ServiceSort::State => state_rank(&a.entry.state)
            .cmp(&state_rank(&b.entry.state))
            .then_with(|| a.entry.state.cmp(&b.entry.state)),
        ServiceSort::StartType => a
            .entry
            .start_type
            .to_ascii_lowercase()
            .cmp(&b.entry.start_type.to_ascii_lowercase()),
        ServiceSort::Pid => match (a.entry.pid, b.entry.pid) {
            (Some(pa), Some(pb)) => pa.cmp(&pb),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        },
        ServiceSort::Cpu => b
            .cpu_pct
            .unwrap_or(-1.0)
            .total_cmp(&a.cpu_pct.unwrap_or(-1.0)),
        ServiceSort::Memory => b
            .memory_bytes
            .unwrap_or(0)
            .cmp(&a.memory_bytes.unwrap_or(0)),
    };
    let directed = if reversed { ord.reverse() } else { ord };
    directed.then_with(|| {
        a.entry
            .name
            .to_ascii_lowercase()
            .cmp(&b.entry.name.to_ascii_lowercase())
    })
}

/// Renders the Windows Services panel inside `rect` (replacing the `⁴proc` list when toggled on).
pub fn render_services_view(
    grid: &mut Grid,
    rect: Rect,
    snapshot: &Snapshot,
    view: &ServiceViewState,
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

    let active_services = match &snapshot.services {
        DataState::Available(list) | DataState::Stale { value: list, .. } => Some(list.as_slice()),
        _ => None,
    };
    let active_procs = match &snapshot.processes {
        DataState::Available(list) | DataState::Stale { value: list, .. } => Some(list.as_slice()),
        _ => None,
    };
    let built = active_services.map(|svcs| build_visible_service_rows(svcs, active_procs, view));
    let total_visible = built.as_ref().map_or(0, |(rows, _)| rows.len());

    render_services_chrome(grid, rect, view, total_visible, theme);

    let inner = Rect {
        x: rect.x.saturating_add(1),
        y: rect.y.saturating_add(1),
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    };

    if let Some((rows, filter_error)) = built {
        render_services_table(grid, inner, &rows, filter_error.as_deref(), view, theme);
        return;
    }

    render_elevation_bar(grid, inner.x, inner.y, inner.width, view.is_elevated, theme);
    let msg = match &snapshot.services {
        DataState::WarmingUp => "Querying Windows Service Control Manager (SCM)...".to_owned(),
        DataState::Denied => "Windows SCM access denied".to_owned(),
        DataState::Unavailable(reason) => format!("Services unavailable: {reason}"),
        DataState::Available(_) | DataState::Stale { .. } => String::new(),
    };
    if !msg.is_empty() && inner.height > 2 {
        let clipped = clip_text(&msg, usize::from(inner.width.saturating_sub(2)));
        grid.write_str(
            inner.x + 1,
            inner.y + 2,
            &clipped,
            theme.inactive_fg,
            theme.main_bg,
            false,
        );
    }
}

fn render_services_chrome(
    grid: &mut Grid,
    rect: Rect,
    view: &ServiceViewState,
    total_services: usize,
    theme: &ThemePalette,
) {
    let max_top_x = rect.x.saturating_add(rect.width - 1);

    if let Some(tag_rect) = draw_border_tag(
        grid,
        rect.x.saturating_add(2),
        rect.y,
        max_top_x,
        Some('⁴'),
        "svc",
        theme.proc_box,
        theme.title,
        theme.hi_fg,
        theme.main_bg,
    ) {
        grid.hit_map
            .push(tag_rect, HitAction::ToggleBox("proc".to_owned()));
    }

    let tab_x = rect.x.saturating_add(9);
    let mut left_bound = tab_x;
    if let Some(tab_rect) = draw_border_tag(
        grid,
        tab_x,
        rect.y,
        max_top_x,
        None,
        "S:proc",
        theme.proc_box,
        theme.hi_fg,
        theme.hi_fg,
        theme.main_bg,
    ) {
        grid.hit_map.push(tab_rect, HitAction::ToggleServicesView);
        left_bound = tab_rect.x.saturating_add(tab_rect.width);
    }

    let pos_label = if total_services == 0 {
        "0/0".to_owned()
    } else {
        format!(
            "{}/{total_services}",
            view.selected.min(total_services - 1) + 1
        )
    };
    let pos_w = u16::try_from(pos_label.chars().count() + 2).unwrap_or(6);
    let pos_x = max_top_x.saturating_sub(pos_w + 1);
    if pos_x > left_bound.saturating_add(1) {
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
    if sort_x > left_bound.saturating_add(1) {
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

    render_services_filter_and_footer(grid, rect, left_bound, sort_x, max_top_x, view, theme);
}

fn render_services_filter_and_footer(
    grid: &mut Grid,
    rect: Rect,
    left_bound: u16,
    sort_x: u16,
    max_top_x: u16,
    view: &ServiceViewState,
    theme: &ThemePalette,
) {
    if view.filter_editing || !view.filter.is_empty() {
        let cursor_glyph = if view.filter_editing { "_" } else { "" };
        let filter_tag = format!("f:{}{cursor_glyph}", view.filter);
        let max_filter_w = sort_x.saturating_sub(left_bound.saturating_add(2));
        if max_filter_w >= 6 {
            let clipped = clip_text(&filter_tag, usize::from(max_filter_w.saturating_sub(2)));
            let filter_w = u16::try_from(clipped.chars().count() + 2).unwrap_or(6);
            let filter_x = sort_x.saturating_sub(filter_w + 1);
            if filter_x > left_bound {
                let _ = draw_border_tag(
                    grid,
                    filter_x,
                    rect.y,
                    sort_x.saturating_sub(1),
                    None,
                    &clipped,
                    theme.proc_box,
                    theme.hi_fg,
                    theme.hi_fg,
                    theme.main_bg,
                );
            }
        }
    }

    let bottom_y = rect.y.saturating_add(rect.height - 1);
    let footer = if view.filter_editing {
        "Filter mode: type text or !regex | Enter done | Del clear | Esc exit"
    } else if view.is_elevated {
        "S proc | ↑↓ sel | f filter | ←→ sort | r rev | s start | x stop"
    } else {
        "S proc | ↑↓ sel | f filter | ←→ sort | r rev | s/x Admin only"
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

fn render_elevation_bar(
    grid: &mut Grid,
    x: u16,
    y: u16,
    width: u16,
    is_elevated: bool,
    theme: &ThemePalette,
) {
    let w = usize::from(width);
    if is_elevated {
        let text = clip_text(
            " SCM Controls: [s] Start [x] Stop ENABLED (Administrator)",
            w,
        );
        grid.write_str(x, y, &text, Color::Rgb(theme.cpu_mid), theme.main_bg, true);
    } else {
        let text = clip_text(
            " SCM Controls: [s] Start [x] Stop DISABLED (run as Administrator)",
            w,
        );
        grid.write_str(x, y, &text, theme.inactive_fg, theme.main_bg, false);
    }
}

fn render_services_table(
    grid: &mut Grid,
    inner: Rect,
    rows: &[ServiceRow<'_>],
    filter_error: Option<&str>,
    view: &ServiceViewState,
    theme: &ThemePalette,
) {
    if inner.height < 2 {
        return;
    }

    let mut cur_y = inner.y;
    render_elevation_bar(grid, inner.x, cur_y, inner.width, view.is_elevated, theme);
    cur_y = cur_y.saturating_add(1);

    if let Some(err) = filter_error
        && cur_y < inner.y.saturating_add(inner.height)
    {
        let text = clip_text(&format!(" {err}"), usize::from(inner.width));
        grid.write_str(inner.x, cur_y, &text, theme.hi_fg, theme.main_bg, true);
        cur_y = cur_y.saturating_add(1);
    }

    if let Some(ref status) = view.action_status
        && cur_y < inner.y.saturating_add(inner.height)
    {
        let text = clip_text(&format!(" ACTION: {status}"), usize::from(inner.width));
        grid.write_str(inner.x, cur_y, &text, theme.title, theme.main_bg, true);
        cur_y = cur_y.saturating_add(1);
    }

    if cur_y >= inner.y.saturating_add(inner.height) {
        return;
    }

    let cols = ServiceColumns::for_width(usize::from(inner.width));
    render_header_row(grid, inner.x, cur_y, cols, view, theme);
    cur_y = cur_y.saturating_add(1);

    let body_rows = usize::from(inner.y.saturating_add(inner.height).saturating_sub(cur_y));
    if body_rows == 0 {
        return;
    }

    if rows.is_empty() {
        let msg = if view.filter.is_empty() {
            "No Windows services reported by SCM"
        } else {
            "No services match current filter (press Delete to clear)"
        };
        let clipped = clip_text(msg, usize::from(inner.width.saturating_sub(2)));
        grid.write_str(
            inner.x + 1,
            cur_y,
            &clipped,
            theme.inactive_fg,
            theme.main_bg,
            false,
        );
        return;
    }

    let selected = view.selected.min(rows.len() - 1);
    let max_scroll = rows.len().saturating_sub(body_rows);
    let scroll = view.scroll_offset.min(max_scroll);

    for (slot, row) in rows.iter().skip(scroll).take(body_rows).enumerate() {
        let row_idx = scroll + slot;
        let y = cur_y.saturating_add(u16::try_from(slot).unwrap_or(0));
        let is_selected = row_idx == selected;
        render_service_row(grid, inner.x, y, inner.width, cols, row, is_selected, theme);
    }
}

#[derive(Debug, Clone, Copy)]
struct ServiceColumns {
    name: usize,
    display: usize,
    state: usize,
    start: usize,
    pid: usize,
    mem: usize,
    cpu: usize,
}

impl ServiceColumns {
    fn for_width(total_w: usize) -> Self {
        let state = 14;
        let start = 8;
        let pid = 6;
        let cpu = 6;
        let mem = if total_w >= 58 { 8 } else { 0 };

        let fixed = state + 1 + start + 1 + pid + 1 + if mem > 0 { mem + 1 } else { 0 } + cpu + 2; // leading + trailing padding
        let flex = total_w.saturating_sub(fixed);
        if flex >= 28 {
            let name = (flex * 38 / 100).clamp(12, 24);
            let display = flex.saturating_sub(name + 1);
            Self {
                name,
                display,
                state,
                start,
                pid,
                mem,
                cpu,
            }
        } else {
            Self {
                name: flex.max(8),
                display: 0,
                state,
                start,
                pid,
                mem,
                cpu,
            }
        }
    }
}

fn render_header_row(
    grid: &mut Grid,
    x: u16,
    y: u16,
    cols: ServiceColumns,
    view: &ServiceViewState,
    theme: &ThemePalette,
) {
    let mut cx = x.saturating_add(1);
    let col_fg = |sort: ServiceSort| {
        if view.sort == sort {
            theme.hi_fg
        } else {
            theme.title
        }
    };

    let name_hdr = format!("{:<w$}", clip_text("Service", cols.name), w = cols.name);
    grid.write_str(
        cx,
        y,
        &name_hdr,
        col_fg(ServiceSort::Name),
        theme.main_bg,
        true,
    );
    cx = cx.saturating_add(u16::try_from(cols.name + 1).unwrap_or(0));

    if cols.display > 0 {
        let disp_hdr = format!(
            "{:<w$}",
            clip_text("Display Name", cols.display),
            w = cols.display
        );
        grid.write_str(
            cx,
            y,
            &disp_hdr,
            col_fg(ServiceSort::DisplayName),
            theme.main_bg,
            true,
        );
        cx = cx.saturating_add(u16::try_from(cols.display + 1).unwrap_or(0));
    }

    let state_hdr = format!("{:<w$}", "State", w = cols.state);
    grid.write_str(
        cx,
        y,
        &state_hdr,
        col_fg(ServiceSort::State),
        theme.main_bg,
        true,
    );
    cx = cx.saturating_add(u16::try_from(cols.state + 1).unwrap_or(0));

    let start_hdr = format!("{:<w$}", "Start", w = cols.start);
    grid.write_str(
        cx,
        y,
        &start_hdr,
        col_fg(ServiceSort::StartType),
        theme.main_bg,
        true,
    );
    cx = cx.saturating_add(u16::try_from(cols.start + 1).unwrap_or(0));

    let pid_hdr = format!("{:>w$}", "PID", w = cols.pid);
    grid.write_str(
        cx,
        y,
        &pid_hdr,
        col_fg(ServiceSort::Pid),
        theme.main_bg,
        true,
    );
    cx = cx.saturating_add(u16::try_from(cols.pid + 1).unwrap_or(0));

    if cols.mem > 0 {
        let mem_hdr = format!("{:>w$}", "Mem", w = cols.mem);
        grid.write_str(
            cx,
            y,
            &mem_hdr,
            col_fg(ServiceSort::Memory),
            theme.main_bg,
            true,
        );
        cx = cx.saturating_add(u16::try_from(cols.mem + 1).unwrap_or(0));
    }

    let cpu_hdr = format!("{:>w$}", "CPU%", w = cols.cpu);
    grid.write_str(
        cx,
        y,
        &cpu_hdr,
        col_fg(ServiceSort::Cpu),
        theme.main_bg,
        true,
    );
}

fn service_state_fg(row: &ServiceRow<'_>, is_selected: bool, theme: &ThemePalette) -> Color {
    if is_selected {
        theme.selected_fg
    } else if row.is_pending {
        theme.hi_fg
    } else if row.entry.state.eq_ignore_ascii_case("running") {
        Color::Rgb(theme.cpu_mid)
    } else if row.entry.state.eq_ignore_ascii_case("paused") {
        theme.title
    } else {
        theme.inactive_fg
    }
}

#[allow(clippy::too_many_arguments)]
fn render_service_row(
    grid: &mut Grid,
    x: u16,
    y: u16,
    width: u16,
    cols: ServiceColumns,
    row: &ServiceRow<'_>,
    is_selected: bool,
    theme: &ThemePalette,
) {
    let row_bg = if is_selected {
        theme.selected_bg
    } else {
        theme.main_bg
    };
    let base_fg = if is_selected {
        theme.selected_fg
    } else {
        theme.main_fg
    };
    if is_selected {
        fill_rect(
            grid,
            Rect {
                x,
                y,
                width,
                height: 1,
            },
            base_fg,
            row_bg,
        );
    }

    let state_fg = service_state_fg(row, is_selected, theme);
    let state_text = if row.is_pending {
        format!("~{}", row.entry.state)
    } else {
        row.entry.state.clone()
    };
    let pid_str = row
        .entry
        .pid
        .map_or_else(|| "-".to_owned(), |p| p.to_string());
    let mem_str = row
        .memory_bytes
        .map_or_else(|| "-".to_owned(), |b| format_mem_bytes(b, false));
    let cpu_str = row
        .cpu_pct
        .map_or_else(|| "-".to_owned(), |c| format!("{c:.1}"));

    let mut cx = x.saturating_add(1);
    let name_cell = format!(
        "{:<w$}",
        clip_text(&row.entry.name, cols.name),
        w = cols.name
    );
    grid.write_str(
        cx,
        y,
        &name_cell,
        base_fg,
        row_bg,
        is_selected || row.is_pending,
    );
    cx = cx.saturating_add(u16::try_from(cols.name + 1).unwrap_or(0));

    if cols.display > 0 {
        let disp_cell = format!(
            "{:<w$}",
            clip_text(&row.entry.display_name, cols.display),
            w = cols.display
        );
        let disp_fg = if is_selected {
            theme.selected_fg
        } else {
            theme.inactive_fg
        };
        grid.write_str(cx, y, &disp_cell, disp_fg, row_bg, is_selected);
        cx = cx.saturating_add(u16::try_from(cols.display + 1).unwrap_or(0));
    }

    let state_cell = format!("{:<w$}", clip_text(&state_text, cols.state), w = cols.state);
    grid.write_str(
        cx,
        y,
        &state_cell,
        state_fg,
        row_bg,
        is_selected || row.is_pending,
    );
    cx = cx.saturating_add(u16::try_from(cols.state + 1).unwrap_or(0));

    let start_cell = format!(
        "{:<w$}",
        clip_text(&row.entry.start_type, cols.start),
        w = cols.start
    );
    grid.write_str(cx, y, &start_cell, base_fg, row_bg, is_selected);
    cx = cx.saturating_add(u16::try_from(cols.start + 1).unwrap_or(0));

    let pid_cell = format!("{:>w$}", clip_text(&pid_str, cols.pid), w = cols.pid);
    grid.write_str(cx, y, &pid_cell, base_fg, row_bg, is_selected);
    cx = cx.saturating_add(u16::try_from(cols.pid + 1).unwrap_or(0));

    if cols.mem > 0 {
        let mem_cell = format!("{:>w$}", clip_text(&mem_str, cols.mem), w = cols.mem);
        grid.write_str(cx, y, &mem_cell, base_fg, row_bg, is_selected);
        cx = cx.saturating_add(u16::try_from(cols.mem + 1).unwrap_or(0));
    }

    let cpu_cell = format!("{:>w$}", clip_text(&cpu_str, cols.cpu), w = cols.cpu);
    grid.write_str(cx, y, &cpu_cell, base_fg, row_bg, is_selected);
}

#[cfg(test)]
mod tests {
    use btop_model::ProcessIdentity;
    use btop_term::Size;

    use super::*;

    fn sample_services_and_procs() -> (Vec<ServiceEntry>, Vec<ProcessEntry>) {
        let services = vec![
            ServiceEntry {
                name: "W32Time".to_owned(),
                display_name: "Windows Time".to_owned(),
                state: "Running".to_owned(),
                start_type: "Auto".to_owned(),
                pid: Some(1204),
            },
            ServiceEntry {
                name: "Spooler".to_owned(),
                display_name: "Print Spooler".to_owned(),
                state: "Start Pending".to_owned(),
                start_type: "Auto".to_owned(),
                pid: Some(2200),
            },
            ServiceEntry {
                name: "wuauserv".to_owned(),
                display_name: "Windows Update".to_owned(),
                state: "Stop Pending".to_owned(),
                start_type: "Manual".to_owned(),
                pid: Some(1204),
            },
            ServiceEntry {
                name: "Fax".to_owned(),
                display_name: "Fax Service".to_owned(),
                state: "Stopped".to_owned(),
                start_type: "Disabled".to_owned(),
                pid: None,
            },
        ];
        let procs = vec![
            ProcessEntry {
                identity: ProcessIdentity {
                    pid: 1204,
                    create_time_100ns: 100,
                },
                parent_pid: 640,
                name: "svchost.exe".to_owned(),
                image_path: None,
                command_line: None,
                command_line_denied: false,
                user: Some("NT AUTHORITY\\LOCAL SERVICE".to_owned()),
                user_denied: false,
                threads: 12,
                cpu_pct: 3.4,
                working_set_private_bytes: 48 * 1024 * 1024,
                io_read_bytes: 0,
                io_write_bytes: 0,
                is_protected: false,
            },
            ProcessEntry {
                identity: ProcessIdentity {
                    pid: 2200,
                    create_time_100ns: 200,
                },
                parent_pid: 640,
                name: "spoolsv.exe".to_owned(),
                image_path: None,
                command_line: None,
                command_line_denied: false,
                user: Some("NT AUTHORITY\\SYSTEM".to_owned()),
                user_denied: false,
                threads: 8,
                cpu_pct: 12.6,
                working_set_private_bytes: 96 * 1024 * 1024,
                io_read_bytes: 0,
                io_write_bytes: 0,
                is_protected: false,
            },
        ];
        (services, procs)
    }

    #[test]
    fn joins_live_pid_cpu_and_memory_metrics_onto_services() {
        let (services, procs) = sample_services_and_procs();
        let view = ServiceViewState {
            sort: ServiceSort::Name,
            ..ServiceViewState::default()
        };
        let (rows, err) = build_visible_service_rows(&services, Some(&procs), &view);
        assert!(err.is_none());
        assert_eq!(rows.len(), 4);

        let w32 = rows
            .iter()
            .find(|r| r.entry.name == "W32Time")
            .expect("w32");
        assert_eq!(w32.cpu_pct, Some(3.4));
        assert_eq!(w32.memory_bytes, Some(48 * 1024 * 1024));
        assert!(!w32.is_pending);

        let spooler = rows
            .iter()
            .find(|r| r.entry.name == "Spooler")
            .expect("spool");
        assert_eq!(spooler.cpu_pct, Some(12.6));
        assert_eq!(spooler.memory_bytes, Some(96 * 1024 * 1024));
        assert!(spooler.is_pending);

        let fax = rows.iter().find(|r| r.entry.name == "Fax").expect("fax");
        assert_eq!(fax.cpu_pct, None);
        assert_eq!(fax.memory_bytes, None);
    }

    #[test]
    fn supports_sorting_reversing_and_substring_or_regex_filtering() {
        let (services, procs) = sample_services_and_procs();

        let mut view = ServiceViewState {
            sort: ServiceSort::Cpu,
            ..ServiceViewState::default()
        };
        let (by_cpu, _) = build_visible_service_rows(&services, Some(&procs), &view);
        assert_eq!(by_cpu[0].entry.name, "Spooler");
        assert_eq!(by_cpu.last().expect("last").entry.name, "Fax");

        view.toggle_reverse();
        let (by_cpu_rev, _) = build_visible_service_rows(&services, Some(&procs), &view);
        assert_eq!(by_cpu_rev[0].entry.name, "Fax");

        view.set_filter("spool");
        let (sub_rows, err) = build_visible_service_rows(&services, Some(&procs), &view);
        assert!(err.is_none());
        assert_eq!(sub_rows.len(), 1);
        assert_eq!(sub_rows[0].entry.name, "Spooler");

        view.set_filter("!^w(32|ua)");
        let (re_rows, err) = build_visible_service_rows(&services, Some(&procs), &view);
        assert!(err.is_none());
        assert_eq!(re_rows.len(), 2);

        view.set_filter("![unterminated");
        let (_, bad_err) = build_visible_service_rows(&services, Some(&procs), &view);
        assert!(bad_err.is_some());
    }

    #[test]
    fn renders_pending_states_and_disabled_vs_enabled_elevation_indicators() {
        let (services, procs) = sample_services_and_procs();
        let snap = Snapshot {
            processes: DataState::Available(procs),
            services: DataState::Available(services),
            ..Snapshot::default()
        };
        let cfg = Config::default();
        let theme = ThemePalette::default_dark(true);
        let rect = Rect {
            x: 0,
            y: 0,
            width: 110,
            height: 16,
        };

        let mut non_elevated = Grid::new(Size {
            width: 110,
            height: 16,
        });
        let view_disabled = ServiceViewState {
            active: true,
            is_elevated: false,
            ..ServiceViewState::default()
        };
        render_services_view(&mut non_elevated, rect, &snap, &view_disabled, &cfg, &theme);
        let plain_disabled = non_elevated.to_plain_text();
        assert!(plain_disabled.contains("DISABLED (run as Administrator)"));
        assert!(plain_disabled.contains("~Start Pending"));
        assert!(plain_disabled.contains("~Stop Pending"));
        assert!(plain_disabled.contains("12.6"));
        assert_eq!(
            non_elevated.hit_map.hit_test(11, 0),
            Some(&HitAction::ToggleServicesView)
        );

        let mut elevated = Grid::new(Size {
            width: 110,
            height: 16,
        });
        let view_enabled = ServiceViewState {
            active: true,
            is_elevated: true,
            ..ServiceViewState::default()
        };
        render_services_view(&mut elevated, rect, &snap, &view_enabled, &cfg, &theme);
        let plain_enabled = elevated.to_plain_text();
        assert!(plain_enabled.contains("ENABLED (Administrator)"));
        assert!(plain_enabled.contains("s start | x stop"));
    }
}
