//! Selected-process detail overlay rendered above the `⁴proc` list when `Enter` is pressed.
//!
//! Shows historical CPU and memory graphs, PID, create-time-verified parent process name/PID,
//! elapsed uptime, thread count, private working set, I/O read/write totals, locally SID-cached
//! owner user, full image path (`SystemProcessIdInformation`), and command line
//! (`ProcessCommandLineInformation`, degrading cleanly to `-` when PPL denies access).

use btop_config::Config;
use btop_model::{DataState, ProcessEntry, ProcessIdentity, Snapshot};
use btop_term::{Cell, Grid, Rect};

use crate::UiHistory;
use crate::proc_box::{ProcViewState, clip_text, format_bytes_compact};
use crate::theme::ThemePalette;
use crate::widgets::{draw_border_tag, draw_box_frame, draw_graph_with_gradient, fill_rect};

/// Windows FILETIME epoch (`1601-01-01`) to Unix epoch (`1970-01-01`) offset in 100ns ticks.
pub const WINDOWS_TO_UNIX_EPOCH_100NS: u64 = 116_444_736_000_000_000;

/// Returns how many inner rows inside `⁴proc` are reserved for the detail overlay.
pub const fn detail_overlay_height(inner_height: u16, detail_open: bool) -> u16 {
    if !detail_open {
        return 0;
    }
    if inner_height >= 13 {
        9
    } else if inner_height >= 10 {
        8
    } else {
        0
    }
}

/// Formats an optional/restricted detail string field, rendering `DataState::Denied` (e.g., PPL)
/// and unavailable/empty states cleanly as `"-"`.
pub fn format_detail_field<'a>(state: &DataState<&'a str>) -> &'a str {
    match state {
        DataState::Available(val) | DataState::Stale { value: val, .. }
            if !val.trim().is_empty() =>
        {
            val
        }
        DataState::Denied
        | DataState::WarmingUp
        | DataState::Unavailable(_)
        | DataState::Available(_)
        | DataState::Stale { .. } => "-",
    }
}

/// Formats elapsed process uptime from a snapshot Unix timestamp (`ms`) and Windows `FILETIME`
/// `create_time_100ns`. Returns `"-"` when `create_time_100ns == 0` or ahead of `now`.
pub fn format_elapsed_uptime(snapshot_timestamp_ms: u64, create_time_100ns: u64) -> String {
    if create_time_100ns == 0 || snapshot_timestamp_ms == 0 {
        return "-".to_owned();
    }
    let now_100ns = snapshot_timestamp_ms
        .saturating_mul(10_000)
        .saturating_add(WINDOWS_TO_UNIX_EPOCH_100NS);
    let Some(elapsed_100ns) = now_100ns.checked_sub(create_time_100ns) else {
        return "-".to_owned();
    };
    let total_secs = elapsed_100ns / 10_000_000;
    let days = total_secs / 86_400;
    let hours = (total_secs % 86_400) / 3600;
    let mins = (total_secs % 3600) / 60;
    let secs = total_secs % 60;
    if days > 0 {
        format!("{days}d {hours:02}:{mins:02}:{secs:02}")
    } else {
        format!("{hours:02}:{mins:02}:{secs:02}")
    }
}

/// Resolves the create-time-verified parent label (`name (ppid)`) for `entry`.
pub fn format_parent_label(entry: &ProcessEntry, active_procs: &[ProcessEntry]) -> String {
    if entry.parent_pid == 0 {
        return "-".to_owned();
    }
    if let Some(parent) = active_procs.iter().find(|p| {
        p.identity.pid == entry.parent_pid
            && p.identity != entry.identity
            && (p.identity.create_time_100ns == 0
                || entry.identity.create_time_100ns == 0
                || p.identity.create_time_100ns <= entry.identity.create_time_100ns)
    }) {
        format!("{} ({})", parent.name, entry.parent_pid)
    } else {
        format!("PID {} (exited)", entry.parent_pid)
    }
}

/// Normalizes a raw CPU percentage series into `0.0..=100.0` graph coordinates and returns
/// `(scaled_samples, scale_ceiling_pct)`.
pub fn scale_cpu_series(series: &[f32], current_pct: f32) -> (Vec<f32>, f32) {
    let fallback = [current_pct.clamp(0.0, 100.0)];
    let source = if series.is_empty() { &fallback } else { series };
    let max_val = source
        .iter()
        .copied()
        .fold(current_pct.clamp(0.0, 100.0), f32::max);
    let ceiling = (max_val * 1.2).clamp(10.0, 100.0);
    let scaled = source
        .iter()
        .map(|&v| ((v.clamp(0.0, 100.0) / ceiling) * 100.0).clamp(0.0, 100.0))
        .collect();
    (scaled, ceiling)
}

/// Normalizes a private working-set byte series into `0.0..=100.0` graph coordinates so
/// memory trends and leaks remain clearly visible even on high-RAM hosts.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
pub fn scale_mem_series(series: &[u64], current_bytes: u64) -> Vec<f32> {
    const MIN_CEILING: f64 = 1024.0 * 1024.0; // 1 MiB floor
    let fallback = [current_bytes];
    let source = if series.is_empty() { &fallback } else { series };
    let peak = source.iter().copied().fold(current_bytes, u64::max) as f64;
    let ceiling = (peak * 1.15).max(MIN_CEILING);
    source
        .iter()
        .map(|&b| (((b as f64) / ceiling) * 100.0).clamp(0.0, 100.0) as f32)
        .collect()
}

/// Renders the selected-process detail overlay inside `rect` (top of `⁴proc` inner area).
#[allow(clippy::too_many_arguments)]
pub fn render_proc_detail(
    grid: &mut Grid,
    rect: Rect,
    target: ProcessIdentity,
    snapshot: &Snapshot,
    history: &UiHistory,
    active_procs: &[ProcessEntry],
    view: &ProcViewState,
    config: &Config,
    theme: &ThemePalette,
) {
    if rect.width < 32 || rect.height < 8 {
        return;
    }

    let (entry, exited) = if let Some(live) = active_procs.iter().find(|p| p.identity == target) {
        (live, false)
    } else if let Some(ref cached) = view.detailed_snapshot
        && cached.identity == target
    {
        (cached, true)
    } else {
        return;
    };

    fill_rect(grid, rect, theme.main_fg, theme.main_bg);
    draw_box_frame(
        grid,
        rect,
        theme.div_line,
        theme.main_bg,
        config.rounded_corners,
    );

    render_detail_chrome(grid, rect, entry, exited, theme);

    let inner = Rect {
        x: rect.x.saturating_add(1),
        y: rect.y.saturating_add(1),
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    };
    if inner.height < 6 || inner.width < 28 {
        return;
    }

    let path_y = inner.y.saturating_add(inner.height - 2);
    let cmd_y = inner.y.saturating_add(inner.height - 1);
    let top_h = inner.height.saturating_sub(2);
    let top_rect = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: top_h,
    };

    render_detail_top_band(
        grid,
        top_rect,
        entry,
        exited,
        snapshot,
        history,
        active_procs,
        view,
        config,
        theme,
    );
    render_detail_path_and_cmd(grid, inner, path_y, cmd_y, entry, theme);
}

fn render_detail_chrome(
    grid: &mut Grid,
    rect: Rect,
    entry: &ProcessEntry,
    exited: bool,
    theme: &ThemePalette,
) {
    let max_x = rect.x.saturating_add(rect.width - 1);
    let close_label = "Esc/Enter close";
    let close_w = u16::try_from(close_label.len() + 2).unwrap_or(17);
    let close_x = max_x.saturating_sub(close_w + 1);
    let mut right_limit = max_x;

    if close_x > rect.x.saturating_add(16) {
        let _ = draw_border_tag(
            grid,
            close_x,
            rect.y,
            max_x,
            None,
            close_label,
            theme.div_line,
            theme.inactive_fg,
            theme.hi_fg,
            theme.main_bg,
        );
        right_limit = close_x.saturating_sub(1);
    }

    let badge = if exited {
        Some(("EXITED", theme.hi_fg))
    } else if entry.is_protected {
        Some(("PPL", theme.hi_fg))
    } else if entry.command_line_denied || entry.user_denied {
        Some(("RESTRICTED", theme.inactive_fg))
    } else {
        None
    };
    if let Some((badge_text, badge_fg)) = badge {
        let badge_w = u16::try_from(badge_text.len() + 2).unwrap_or(6);
        let badge_x = right_limit.saturating_sub(badge_w + 1);
        if badge_x > rect.x.saturating_add(14) {
            let _ = draw_border_tag(
                grid,
                badge_x,
                rect.y,
                right_limit,
                None,
                badge_text,
                theme.div_line,
                badge_fg,
                theme.hi_fg,
                theme.main_bg,
            );
            right_limit = badge_x.saturating_sub(1);
        }
    }

    let title_budget = usize::from(right_limit.saturating_sub(rect.x.saturating_add(4)));
    let raw_title = format!("detail: {} ({})", entry.name, entry.identity.pid);
    let title = clip_text(&raw_title, title_budget);
    let _ = draw_border_tag(
        grid,
        rect.x.saturating_add(2),
        rect.y,
        right_limit,
        None,
        &title,
        theme.div_line,
        theme.title,
        theme.hi_fg,
        theme.main_bg,
    );
}

#[allow(clippy::too_many_arguments)]
fn render_detail_top_band(
    grid: &mut Grid,
    top: Rect,
    entry: &ProcessEntry,
    exited: bool,
    snapshot: &Snapshot,
    history: &UiHistory,
    active_procs: &[ProcessEntry],
    view: &ProcViewState,
    config: &Config,
    theme: &ThemePalette,
) {
    let total_mem_bytes = match &snapshot.mem {
        DataState::Available(mem) | DataState::Stale { value: mem, .. } => mem.total_bytes,
        _ => snapshot.total_phys_mem_bytes.unwrap_or(0),
    };
    let cpu_series = view.cpu_sparkline_series(entry.identity, history);
    let mem_series = view.mem_history_series(entry.identity, history);
    let (scaled_cpu, _cpu_cap) = scale_cpu_series(cpu_series, entry.cpu_pct);
    let scaled_mem = scale_mem_series(mem_series, entry.working_set_private_bytes);

    if top.width < 58 {
        // Compact single-column metadata layout for narrow viewports (e.g., 80x24 right half).
        render_detail_metadata(
            grid,
            top,
            entry,
            exited,
            snapshot.timestamp_ms,
            active_procs,
            total_mem_bytes,
            true,
            theme,
        );
        return;
    }

    let (cpu_w, mem_w) = if top.width >= 88 {
        let g = (top.width * 24 / 100).clamp(16, 28);
        (g, g)
    } else {
        (13u16, 13u16)
    };

    let cpu_rect = Rect {
        x: top.x.saturating_add(1),
        y: top.y,
        width: cpu_w,
        height: top.height,
    };
    let div1_x = cpu_rect.x.saturating_add(cpu_rect.width + 1);
    let mem_rect = Rect {
        x: div1_x.saturating_add(2),
        y: top.y,
        width: mem_w,
        height: top.height,
    };
    let div2_x = mem_rect.x.saturating_add(mem_rect.width + 1);
    let meta_x = div2_x.saturating_add(2);
    let meta_w = top.x.saturating_add(top.width).saturating_sub(meta_x + 1);

    // CPU graph header + graph
    let cpu_hdr = format!("CPU {:>5.1}%", entry.cpu_pct);
    render_labeled_graph(
        grid,
        cpu_rect,
        &cpu_hdr,
        theme.process_gradient(entry.cpu_pct.min(100.0)),
        &scaled_cpu,
        config.graph_symbol,
        theme.cpu_stops(),
        theme,
    );

    // Memory graph header + graph
    let mem_pct = mem_percent(entry.working_set_private_bytes, total_mem_bytes);
    let mem_hdr = format!(
        "MEM {} ({mem_pct:.1}%)",
        format_bytes_compact(entry.working_set_private_bytes)
    );
    render_labeled_graph(
        grid,
        mem_rect,
        &mem_hdr,
        theme.used_gradient(mem_pct),
        &scaled_mem,
        config.graph_symbol,
        theme.used_stops(),
        theme,
    );

    draw_vertical_divider(grid, div1_x, top.y, top.height, theme);
    draw_vertical_divider(grid, div2_x, top.y, top.height, theme);

    render_detail_metadata(
        grid,
        Rect {
            x: meta_x,
            y: top.y,
            width: meta_w,
            height: top.height,
        },
        entry,
        exited,
        snapshot.timestamp_ms,
        active_procs,
        total_mem_bytes,
        false,
        theme,
    );
}

#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn mem_percent(bytes: u64, total_bytes: u64) -> f32 {
    if total_bytes == 0 {
        0.0
    } else {
        ((bytes as f64 / total_bytes as f64) * 100.0).clamp(0.0, 100.0) as f32
    }
}

#[allow(clippy::too_many_arguments)]
fn render_labeled_graph(
    grid: &mut Grid,
    rect: Rect,
    header: &str,
    header_fg: btop_term::Color,
    samples: &[f32],
    symbol: btop_config::GraphSymbol,
    stops: crate::theme::GradientStops,
    theme: &ThemePalette,
) {
    let clipped = clip_text(header, usize::from(rect.width));
    grid.write_str(rect.x, rect.y, &clipped, header_fg, theme.main_bg, true);
    if rect.height > 1 {
        draw_graph_with_gradient(
            grid,
            Rect {
                x: rect.x,
                y: rect.y + 1,
                width: rect.width,
                height: rect.height - 1,
            },
            samples,
            symbol,
            false,
            stops,
            theme,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn render_detail_metadata(
    grid: &mut Grid,
    rect: Rect,
    entry: &ProcessEntry,
    exited: bool,
    timestamp_ms: u64,
    active_procs: &[ProcessEntry],
    total_mem_bytes: u64,
    include_cpu_mem: bool,
    theme: &ThemePalette,
) {
    if rect.width < 12 || rect.height == 0 {
        return;
    }

    let parent_label = format_parent_label(entry, active_procs);
    let uptime = format_elapsed_uptime(timestamp_ms, entry.identity.create_time_100ns);
    let user_state = entry.user_state();
    let user_text = format_detail_field(&user_state);
    let priv_ws = format_bytes_compact(entry.working_set_private_bytes);
    let io_read = format_bytes_compact(entry.io_read_bytes);
    let io_write = format_bytes_compact(entry.io_write_bytes);
    let status = if exited {
        "Exited"
    } else if entry.is_protected {
        "Protected (PPL)"
    } else if entry.command_line_denied || entry.user_denied {
        "Access Restricted"
    } else {
        "Running"
    };

    let w = usize::from(rect.width);
    let mut lines: Vec<(String, bool)> = Vec::with_capacity(5);
    if include_cpu_mem {
        let mem_pct = if total_mem_bytes == 0 {
            0.0
        } else {
            #[allow(clippy::cast_precision_loss)]
            let pct = (entry.working_set_private_bytes as f64 / total_mem_bytes as f64) * 100.0;
            pct.clamp(0.0, 100.0)
        };
        lines.push((
            format!(
                "PID: {}  CPU: {:.1}%  MEM: {} ({:.1}%)",
                entry.identity.pid, entry.cpu_pct, priv_ws, mem_pct
            ),
            false,
        ));
        lines.push((
            format!("Parent: {parent_label}  Thr: {}", entry.threads),
            false,
        ));
        lines.push((
            format!("Uptime: {uptime}  User: {user_text}"),
            matches!(user_state, DataState::Denied),
        ));
        lines.push((format!("I/O Read: {io_read}  Write: {io_write}"), false));
    } else {
        lines.push((
            format!("PID: {:<7} Parent: {parent_label}", entry.identity.pid),
            false,
        ));
        lines.push((
            format!("Uptime: {uptime:<12} Threads: {}", entry.threads),
            false,
        ));
        lines.push((
            format!("Priv WS: {priv_ws:<11} User: {user_text}"),
            matches!(user_state, DataState::Denied),
        ));
        lines.push((format!("I/O Read: {io_read:<10} Write: {io_write}"), false));
        lines.push((format!("Status: {status}"), exited || entry.is_protected));
    }

    for (idx, (line, muted_accent)) in lines.into_iter().take(usize::from(rect.height)).enumerate()
    {
        let y = rect.y.saturating_add(u16::try_from(idx).unwrap_or(0));
        let clipped = clip_text(&line, w);
        let fg = if muted_accent && exited {
            theme.hi_fg
        } else {
            theme.main_fg
        };
        grid.write_str(rect.x, y, &clipped, fg, theme.main_bg, false);
    }
}

fn render_detail_path_and_cmd(
    grid: &mut Grid,
    inner: Rect,
    path_y: u16,
    cmd_y: u16,
    entry: &ProcessEntry,
    theme: &ThemePalette,
) {
    let x = inner.x.saturating_add(1);
    let avail = usize::from(inner.width.saturating_sub(2));
    if avail < 8 {
        return;
    }

    let path_state = entry.image_path_state();
    let path_val = format_detail_field(&path_state);
    let path_fg = if matches!(
        path_state,
        DataState::Available(_) | DataState::Stale { .. }
    ) {
        theme.main_fg
    } else {
        theme.inactive_fg
    };
    grid.write_str(x, path_y, "Path: ", theme.title, theme.main_bg, true);
    let clipped_path = clip_text(path_val, avail.saturating_sub(6));
    grid.write_str(
        x.saturating_add(6),
        path_y,
        &clipped_path,
        path_fg,
        theme.main_bg,
        false,
    );

    let cmd_state = entry.command_line_state();
    let cmd_val = format_detail_field(&cmd_state);
    let cmd_fg = if matches!(cmd_state, DataState::Available(_) | DataState::Stale { .. }) {
        theme.main_fg
    } else {
        theme.inactive_fg
    };
    grid.write_str(x, cmd_y, "Cmd:  ", theme.title, theme.main_bg, true);
    let clipped_cmd = clip_text(cmd_val, avail.saturating_sub(6));
    grid.write_str(
        x.saturating_add(6),
        cmd_y,
        &clipped_cmd,
        cmd_fg,
        theme.main_bg,
        false,
    );
}

fn draw_vertical_divider(grid: &mut Grid, x: u16, y: u16, height: u16, theme: &ThemePalette) {
    for row in 0..height {
        grid.set(
            x,
            y.saturating_add(row),
            Cell {
                ch: '│',
                fg: theme.div_line,
                bg: theme.main_bg,
                bold: false,
                continuation: false,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use btop_term::Size;

    use super::*;

    fn sample_proc(pid: u32, parent_pid: u32, name: &str, protected: bool) -> ProcessEntry {
        ProcessEntry {
            identity: ProcessIdentity {
                pid,
                create_time_100ns: 133_801_632_000_000_000,
            },
            parent_pid,
            name: name.to_owned(),
            image_path: Some(format!("C:\\Windows\\System32\\{name}")),
            command_line: if protected {
                None
            } else {
                Some(format!("C:\\Windows\\System32\\{name} --serve"))
            },
            command_line_denied: protected,
            user: if protected {
                None
            } else {
                Some("HOME\\franz".to_owned())
            },
            user_denied: protected,
            threads: 16,
            cpu_pct: 14.5,
            working_set_private_bytes: 256 * 1024 * 1024,
            io_read_bytes: 32 * 1024 * 1024,
            io_write_bytes: 8 * 1024 * 1024,
            is_protected: protected,
        }
    }

    #[test]
    fn formats_ppl_denied_and_unavailable_fields_as_dash() {
        assert_eq!(format_detail_field(&DataState::Denied), "-");
        assert_eq!(format_detail_field(&DataState::WarmingUp), "-");
        assert_eq!(
            format_detail_field(&DataState::Unavailable("missing".to_owned())),
            "-"
        );
        assert_eq!(format_detail_field(&DataState::Available("")), "-");
        assert_eq!(
            format_detail_field(&DataState::Available("C:\\App\\tool.exe --flag")),
            "C:\\App\\tool.exe --flag"
        );

        let ppl = sample_proc(720, 512, "csrss.exe", true);
        assert_eq!(ppl.command_line_state(), DataState::Denied);
        assert_eq!(ppl.user_state(), DataState::Denied);
        assert_eq!(format_detail_field(&ppl.command_line_state()), "-");
        assert_eq!(format_detail_field(&ppl.user_state()), "-");
        assert_eq!(
            format_detail_field(&ppl.image_path_state()),
            "C:\\Windows\\System32\\csrss.exe"
        );
    }

    #[test]
    fn formats_uptime_and_parent_with_create_time_guard() {
        // 2025-01-01 01:05:10 UTC (3910s after boot_time 133_801_632_000_000_000)
        let ts_ms = 1_735_689_600_000 + 3_910_000;
        assert_eq!(
            format_elapsed_uptime(ts_ms, 133_801_632_000_000_000),
            "01:05:10"
        );
        assert_eq!(format_elapsed_uptime(ts_ms, 0), "-");

        let mut parent = sample_proc(4100, 2048, "Code.exe", false);
        parent.identity.create_time_100ns = 1000;
        let mut child = sample_proc(4216, 4100, "rust-analyzer.exe", false);
        child.identity.create_time_100ns = 2000;
        assert_eq!(
            format_parent_label(&child, &[parent.clone(), child.clone()]),
            "Code.exe (4100)"
        );

        // If parent PID was reused AFTER child started, do not misattribute parent name.
        parent.identity.create_time_100ns = 5000;
        let snapshot_procs = [parent, child.clone()];
        assert_eq!(
            format_parent_label(&child, &snapshot_procs),
            "PID 4100 (exited)"
        );
    }

    #[test]
    fn detail_overlay_renders_standard_and_ppl_layouts_cleanly() {
        let parent = sample_proc(4100, 0, "Code.exe", false);
        let std_proc = sample_proc(4216, 4100, "rust-analyzer.exe", false);
        let ppl_proc = sample_proc(720, 4100, "csrss.exe", true);
        let procs = vec![parent, std_proc.clone(), ppl_proc.clone()];

        let snap = Snapshot {
            timestamp_ms: 1_735_693_200_000,
            total_phys_mem_bytes: Some(64 * 1024 * 1024 * 1024),
            processes: DataState::Available(procs.clone()),
            ..Snapshot::default()
        };
        let mut history = UiHistory::default();
        history.push(&snap);

        let cfg = Config::default();
        let theme = ThemePalette::default_dark(true);
        let rect = Rect {
            x: 0,
            y: 0,
            width: 118,
            height: 9,
        };

        let mut grid_std = Grid::new(Size {
            width: 118,
            height: 9,
        });
        let view_std = ProcViewState {
            detailed: Some(std_proc.identity),
            detailed_snapshot: Some(std_proc.clone()),
            ..ProcViewState::default()
        };
        render_proc_detail(
            &mut grid_std,
            rect,
            std_proc.identity,
            &snap,
            &history,
            &procs,
            &view_std,
            &cfg,
            &theme,
        );
        let text_std = grid_std.to_plain_text();
        assert!(text_std.contains("detail: rust-analyzer.exe (4216)"));
        assert!(text_std.contains("Parent: Code.exe (4100)"));
        assert!(text_std.contains("User: HOME\\franz"));
        assert!(text_std.contains("Path: C:\\Windows\\System32\\rust-analyzer.exe"));
        assert!(text_std.contains("Cmd:  C:\\Windows\\System32\\rust-analyzer.exe --serve"));
        assert!(text_std.contains("I/O Read: 32.0M"));

        let mut grid_ppl = Grid::new(Size {
            width: 118,
            height: 9,
        });
        let view_ppl = ProcViewState {
            detailed: Some(ppl_proc.identity),
            detailed_snapshot: Some(ppl_proc.clone()),
            ..ProcViewState::default()
        };
        render_proc_detail(
            &mut grid_ppl,
            rect,
            ppl_proc.identity,
            &snap,
            &history,
            &procs,
            &view_ppl,
            &cfg,
            &theme,
        );
        let text_ppl = grid_ppl.to_plain_text();
        assert!(text_ppl.contains("detail: csrss.exe (720)"));
        assert!(text_ppl.contains("PPL"));
        assert!(text_ppl.contains("User: -"));
        assert!(text_ppl.contains("Path: C:\\Windows\\System32\\csrss.exe"));
        assert!(text_ppl.contains("Cmd:  -"));
    }

    #[test]
    fn detail_overlay_distinguishes_restricted_acl_from_ppl() {
        let mut restricted_proc = sample_proc(3300, 4100, "svc_acl.exe", false);
        restricted_proc.command_line = None;
        restricted_proc.command_line_denied = true;
        restricted_proc.user = None;
        restricted_proc.user_denied = true;
        let acl_procs = vec![restricted_proc.clone()];
        let snap = Snapshot {
            timestamp_ms: 1_735_693_200_000,
            total_phys_mem_bytes: Some(64 * 1024 * 1024 * 1024),
            processes: DataState::Available(acl_procs.clone()),
            ..Snapshot::default()
        };
        let history = UiHistory::from_snapshot(&snap);
        let mut grid_acl = Grid::new(Size {
            width: 118,
            height: 9,
        });
        let view_acl = ProcViewState {
            detailed: Some(restricted_proc.identity),
            detailed_snapshot: Some(restricted_proc.clone()),
            ..ProcViewState::default()
        };
        render_proc_detail(
            &mut grid_acl,
            Rect {
                x: 0,
                y: 0,
                width: 118,
                height: 9,
            },
            restricted_proc.identity,
            &snap,
            &history,
            &acl_procs,
            &view_acl,
            &Config::default(),
            &ThemePalette::default_dark(true),
        );
        let text_acl = grid_acl.to_plain_text();
        assert!(text_acl.contains("RESTRICTED"));
        assert!(text_acl.contains("Status: Access Restricted"));
        assert!(!text_acl.contains("Protected (PPL)"));
        assert!(text_acl.contains("User: -"));
        assert!(text_acl.contains("Cmd:  -"));
    }
}
