//! Memory, cache breakdown, commit charge, and pagefile panel (`2` / `mem`).

use btop_config::Config;
use btop_model::{DataState, MemSnapshot, Snapshot};
use btop_term::{Cell, Grid, HitAction, Rect};

use crate::UiHistory;
use crate::theme::{GradientStops, ThemePalette};
use crate::widgets::{
    draw_border_tag, draw_box_frame, draw_graph_with_gradient, draw_meter_with_gradient, fill_rect,
};

/// Formats a byte count using either binary IEC units (`base_10 = false`: KiB/MiB/GiB/TiB)
/// or decimal SI units (`base_10 = true`: KB/MB/GB/TB).
#[allow(clippy::cast_precision_loss)]
pub fn format_mem_bytes(bytes: u64, base_10: bool) -> String {
    const BINARY_UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    const DECIMAL_UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];

    let (divisor, units) = if base_10 {
        (1000.0_f64, &DECIMAL_UNITS)
    } else {
        (1024.0_f64, &BINARY_UNITS)
    };

    if bytes < if base_10 { 1000 } else { 1024 } {
        return format!("{bytes} B");
    }

    let mut value = bytes as f64;
    let mut unit_idx = 0usize;
    while value >= divisor && unit_idx + 1 < units.len() {
        value /= divisor;
        unit_idx += 1;
    }

    let unit = units[unit_idx];
    if value >= 100.0 {
        format!("{value:.0} {unit}")
    } else if value >= 10.0 {
        format!("{value:.1} {unit}")
    } else {
        format!("{value:.2} {unit}")
    }
}

/// Computes non-overlapping physical RAM segments `(used, standby_reclaimable, free)` whose
/// sum always equals `mem.total_bytes`.
///
/// On Windows, Standby cache pages are counted inside `available_bytes`. Partitioning
/// `available_bytes` into `free` (`free_bytes.min(available_bytes)`) and
/// `standby_reclaimable` (`available_bytes - free`) guarantees stacked bars never
/// double-count Standby memory.
pub fn stacked_physical_segments(mem: &MemSnapshot) -> (u64, u64, u64) {
    let total = mem.total_bytes;
    let available = mem.available_bytes.min(total);
    let used = total.saturating_sub(available);
    let free = mem.free_bytes.min(available);
    let standby_reclaimable = available.saturating_sub(free);
    (used, standby_reclaimable, free)
}

/// Computes percentage `part / total * 100.0` safely clamped to `0.0..=100.0`.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
pub fn ratio_percent(part: u64, total: u64) -> f32 {
    if total == 0 {
        return 0.0;
    }
    ((part as f64 / total as f64) * 100.0).clamp(0.0, 100.0) as f32
}

/// Renders the full memory box (`2` / `mem`) into `rect`.
pub fn render_mem_box(
    grid: &mut Grid,
    rect: Rect,
    snapshot: &Snapshot,
    history: &UiHistory,
    config: &Config,
    theme: &ThemePalette,
) {
    if rect.width < 20 || rect.height < 6 {
        return;
    }

    fill_rect(grid, rect, theme.main_fg, theme.main_bg);
    draw_box_frame(
        grid,
        rect,
        theme.mem_box,
        theme.main_bg,
        config.rounded_corners,
    );
    render_mem_chrome(grid, rect, snapshot, config, theme);

    let inner = Rect {
        x: rect.x.saturating_add(1),
        y: rect.y.saturating_add(1),
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    };

    match &snapshot.mem {
        DataState::Available(mem) => {
            render_mem_body(grid, inner, mem, history, config, theme, None);
        }
        DataState::Stale { value, age_ms } => {
            let note = format!("stale {age_ms}ms");
            render_mem_body(grid, inner, value, history, config, theme, Some(&note));
        }
        DataState::WarmingUp => {
            grid.write_str(
                inner.x + 1,
                inner.y + 1,
                "Memory warming up...",
                theme.inactive_fg,
                theme.main_bg,
                false,
            );
        }
        DataState::Denied => {
            grid.write_str(
                inner.x + 1,
                inner.y + 1,
                "Memory telemetry access denied",
                theme.hi_fg,
                theme.main_bg,
                true,
            );
        }
        DataState::Unavailable(reason) => {
            let msg = format!("Memory unavailable: {reason}");
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

fn render_mem_chrome(
    grid: &mut Grid,
    rect: Rect,
    snapshot: &Snapshot,
    config: &Config,
    theme: &ThemePalette,
) {
    let right_limit = rect.x.saturating_add(rect.width.saturating_sub(1));
    if let Some(tag_rect) = draw_border_tag(
        grid,
        rect.x.saturating_add(2),
        rect.y,
        right_limit,
        Some('²'),
        "mem",
        theme.mem_box,
        theme.title,
        theme.hi_fg,
        theme.main_bg,
    ) {
        grid.hit_map
            .push(tag_rect, HitAction::ToggleBox("mem".to_owned()));
    }

    let total_label = match &snapshot.mem {
        DataState::Available(mem) => {
            format!(
                "Total: {}",
                format_mem_bytes(mem.total_bytes, config.base_10_sizes)
            )
        }
        DataState::Stale { value, age_ms } => format!(
            "Total: {} [stale {}ms]",
            format_mem_bytes(value.total_bytes, config.base_10_sizes),
            age_ms
        ),
        _ => String::new(),
    };
    if !total_label.is_empty() {
        let tag_width = u16::try_from(total_label.chars().count() + 2).unwrap_or(0);
        let min_x = rect.x.saturating_add(9);
        if right_limit > min_x.saturating_add(tag_width) {
            let tag_x = right_limit.saturating_sub(tag_width);
            let _ = draw_border_tag(
                grid,
                tag_x,
                rect.y,
                right_limit,
                None,
                &total_label,
                theme.mem_box,
                theme.title,
                theme.hi_fg,
                theme.main_bg,
            );
        }
    }
}

struct MemSeries<'a> {
    label: &'static str,
    bytes: u64,
    percent: f32,
    history: &'a [f32],
    stops: GradientStops,
}

fn render_mem_body(
    grid: &mut Grid,
    inner: Rect,
    mem: &MemSnapshot,
    history: &UiHistory,
    config: &Config,
    theme: &ThemePalette,
    stale_note: Option<&str>,
) {
    if inner.width < 16 || inner.height < 4 {
        return;
    }

    let mut y = inner.y;
    let y_end = inner.y.saturating_add(inner.height);

    // Top row: non-double-counting stacked physical RAM composition bar [Used | Standby | Free].
    render_stacked_bar_row(grid, inner.x, y, inner.width, mem, stale_note, theme);
    y = y.saturating_add(1);

    // Reserve footer rows for Commit, optional Pagefile (`show_swap`), and optional Compressed.
    let show_pagefile = config.show_swap;
    let base_footer_rows: u16 = 1 + u16::from(show_pagefile);
    let has_compressed = mem.compressed_bytes.is_some();
    let remaining_after_bar = y_end.saturating_sub(y);

    let show_compressed = has_compressed && remaining_after_bar >= base_footer_rows + 6;
    let show_divider = remaining_after_bar >= base_footer_rows + u16::from(show_compressed) + 5;
    let footer_rows = base_footer_rows + u16::from(show_compressed) + u16::from(show_divider);
    let metrics_height = remaining_after_bar.saturating_sub(footer_rows);

    let series = [
        MemSeries {
            label: "Used",
            bytes: mem.used_bytes,
            percent: ratio_percent(mem.used_bytes, mem.total_bytes),
            history: history.mem_used_slice(),
            stops: theme.used_stops(),
        },
        MemSeries {
            label: "Available",
            bytes: mem.available_bytes,
            percent: ratio_percent(mem.available_bytes, mem.total_bytes),
            history: history.mem_available_slice(),
            stops: theme.available_stops(),
        },
        MemSeries {
            label: "Cached",
            bytes: mem.cached_bytes,
            percent: ratio_percent(mem.cached_bytes, mem.total_bytes),
            history: history.mem_cached_slice(),
            stops: theme.cached_stops(),
        },
        MemSeries {
            label: "Free",
            bytes: mem.free_bytes,
            percent: ratio_percent(mem.free_bytes, mem.total_bytes),
            history: history.mem_free_slice(),
            stops: theme.free_stops(),
        },
    ];

    let footer_y = y_end.saturating_sub(footer_rows);
    render_mem_series(
        grid,
        inner,
        y,
        footer_y,
        metrics_height,
        &series,
        config,
        theme,
    );
    render_mem_footer(
        grid,
        inner,
        footer_y,
        y_end,
        mem,
        config,
        theme,
        show_divider,
        show_pagefile,
        show_compressed,
    );
}

#[allow(clippy::too_many_arguments)]
fn render_mem_series(
    grid: &mut Grid,
    inner: Rect,
    start_y: u16,
    limit_y: u16,
    metrics_height: u16,
    series: &[MemSeries<'_>],
    config: &Config,
    theme: &ThemePalette,
) {
    let mut y = start_y;
    if config.mem_graphs && metrics_height >= 8 {
        let band_h = metrics_height / 4;
        for item in series {
            if y.saturating_add(2) > limit_y {
                break;
            }
            render_metric_header(
                grid,
                inner.x,
                y,
                inner.width,
                item.label,
                item.bytes,
                item.percent,
                item.stops,
                config.base_10_sizes,
                theme,
            );
            let graph_h = band_h.saturating_sub(1).max(1);
            let graph_rect = Rect {
                x: inner.x.saturating_add(1),
                y: y.saturating_add(1),
                width: inner.width.saturating_sub(2),
                height: graph_h,
            };
            draw_graph_with_gradient(
                grid,
                graph_rect,
                item.history,
                config.graph_symbol,
                false,
                item.stops,
                theme,
            );
            y = y.saturating_add(band_h);
        }
    } else if metrics_height >= 8 {
        // Spacious meter mode: header line + dedicated full-width gradient meter per series.
        let band_h = (metrics_height / 4).max(2);
        for item in series {
            if y.saturating_add(2) > limit_y {
                break;
            }
            render_metric_header(
                grid,
                inner.x,
                y,
                inner.width,
                item.label,
                item.bytes,
                item.percent,
                item.stops,
                config.base_10_sizes,
                theme,
            );
            draw_meter_with_gradient(
                grid,
                inner.x.saturating_add(1),
                y.saturating_add(1),
                inner.width.saturating_sub(2),
                item.percent,
                item.stops,
                theme,
            );
            y = y.saturating_add(band_h);
        }
    } else {
        // Compact 1-row-per-metric meter fallback for short boxes.
        for item in series {
            if y >= limit_y {
                break;
            }
            render_compact_meter_row(
                grid,
                inner.x,
                y,
                inner.width,
                item.label,
                item.bytes,
                item.percent,
                item.stops,
                config.base_10_sizes,
                theme,
            );
            y = y.saturating_add(1);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_mem_footer(
    grid: &mut Grid,
    inner: Rect,
    start_y: u16,
    y_end: u16,
    mem: &MemSnapshot,
    config: &Config,
    theme: &ThemePalette,
    show_divider: bool,
    show_pagefile: bool,
    show_compressed: bool,
) {
    let mut fy = start_y;
    if show_divider && fy < y_end {
        for dx in 0..inner.width {
            grid.set(
                inner.x.saturating_add(dx),
                fy,
                Cell {
                    ch: '─',
                    fg: theme.div_line,
                    bg: theme.main_bg,
                    bold: false,
                    continuation: false,
                },
            );
        }
        fy = fy.saturating_add(1);
    }

    if fy < y_end {
        let commit_pct = ratio_percent(mem.commit_used_bytes, mem.commit_limit_bytes);
        let right = format!(
            "{}/{} {:>4.0}%",
            format_mem_bytes(mem.commit_used_bytes, config.base_10_sizes),
            format_mem_bytes(mem.commit_limit_bytes, config.base_10_sizes),
            commit_pct
        );
        render_footer_meter_row(
            grid,
            inner.x,
            fy,
            inner.width,
            "Commit",
            &right,
            commit_pct,
            theme.used_stops(),
            theme,
        );
        fy = fy.saturating_add(1);
    }

    if show_pagefile && fy < y_end {
        if mem.pagefile_total_bytes == 0 {
            render_footer_meter_row(
                grid,
                inner.x,
                fy,
                inner.width,
                "Swap",
                "none",
                0.0,
                theme.available_stops(),
                theme,
            );
        } else {
            let swap_pct = ratio_percent(mem.pagefile_used_bytes, mem.pagefile_total_bytes);
            let right = format!(
                "{}/{} {:>4.0}%",
                format_mem_bytes(mem.pagefile_used_bytes, config.base_10_sizes),
                format_mem_bytes(mem.pagefile_total_bytes, config.base_10_sizes),
                swap_pct
            );
            render_footer_meter_row(
                grid,
                inner.x,
                fy,
                inner.width,
                "Swap",
                &right,
                swap_pct,
                theme.available_stops(),
                theme,
            );
        }
        fy = fy.saturating_add(1);
    }

    if show_compressed
        && fy < y_end
        && let Some(compressed) = mem.compressed_bytes
    {
        let label = format!(
            " Compressed: {}",
            format_mem_bytes(compressed, config.base_10_sizes)
        );
        let clipped: String = label.chars().take(usize::from(inner.width)).collect();
        grid.write_str(
            inner.x,
            fy,
            &clipped,
            theme.inactive_fg,
            theme.main_bg,
            false,
        );
    }
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn render_stacked_bar_row(
    grid: &mut Grid,
    x: u16,
    y: u16,
    width: u16,
    mem: &MemSnapshot,
    stale_note: Option<&str>,
    theme: &ThemePalette,
) {
    let prefix = if let Some(note) = stale_note {
        format!(" RAM [{note}] ")
    } else {
        " RAM ".to_owned()
    };
    let prefix_w = u16::try_from(prefix.chars().count()).unwrap_or(0);
    if width <= prefix_w.saturating_add(4) {
        let clipped: String = prefix.chars().take(usize::from(width)).collect();
        grid.write_str(x, y, &clipped, theme.title, theme.main_bg, true);
        return;
    }

    grid.write_str(x, y, &prefix, theme.title, theme.main_bg, true);
    let bar_x = x.saturating_add(prefix_w);
    let bar_w = width.saturating_sub(prefix_w).saturating_sub(1);

    let (used, standby, _free) = stacked_physical_segments(mem);
    let total = mem.total_bytes;
    let (used_cols, standby_cols) = if total == 0 {
        (0u16, 0u16)
    } else {
        let bar_f = f64::from(bar_w);
        let used_c = (((used as f64 / total as f64) * bar_f).round() as u16).min(bar_w);
        let standby_c = (((standby as f64 / total as f64) * bar_f).round() as u16)
            .min(bar_w.saturating_sub(used_c));
        (used_c, standby_c)
    };

    for col in 0..bar_w {
        let (ch, fg) = if col < used_cols {
            ('■', theme.used_gradient(70.0))
        } else if col < used_cols.saturating_add(standby_cols) {
            ('■', theme.cached_gradient(55.0))
        } else {
            ('■', theme.meter_bg)
        };
        grid.set(
            bar_x.saturating_add(col),
            y,
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

#[allow(clippy::too_many_arguments)]
fn render_metric_header(
    grid: &mut Grid,
    x: u16,
    y: u16,
    width: u16,
    label: &str,
    bytes: u64,
    percent: f32,
    stops: GradientStops,
    base_10: bool,
    theme: &ThemePalette,
) {
    let left = format!(" {label}");
    let right = format!("{} {:>5.1}% ", format_mem_bytes(bytes, base_10), percent);
    let left_w = u16::try_from(left.chars().count()).unwrap_or(0);
    let right_w = u16::try_from(right.chars().count()).unwrap_or(0);

    grid.write_str(x, y, &left, stops.sample(50.0), theme.main_bg, true);
    if width > left_w.saturating_add(right_w) {
        let rx = x.saturating_add(width.saturating_sub(right_w));
        grid.write_str(rx, y, &right, theme.main_fg, theme.main_bg, false);
    } else if width > left_w.saturating_add(1) {
        let avail = usize::from(width.saturating_sub(left_w).saturating_sub(1));
        let clipped: String = right.chars().take(avail).collect();
        grid.write_str(
            x.saturating_add(left_w).saturating_add(1),
            y,
            &clipped,
            theme.main_fg,
            theme.main_bg,
            false,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn render_compact_meter_row(
    grid: &mut Grid,
    x: u16,
    y: u16,
    width: u16,
    label: &str,
    bytes: u64,
    percent: f32,
    stops: GradientStops,
    base_10: bool,
    theme: &ThemePalette,
) {
    let short_label = match label {
        "Available" => "Avail ",
        "Cached" => "Cache ",
        "Used" => "Used  ",
        "Free" => "Free  ",
        other => other,
    };
    let left = format!(" {short_label}");
    let right = format!(" {} {:>3.0}% ", format_mem_bytes(bytes, base_10), percent);
    let left_w = u16::try_from(left.chars().count()).unwrap_or(0);
    let right_w = u16::try_from(right.chars().count()).unwrap_or(0);

    grid.write_str(x, y, &left, stops.sample(50.0), theme.main_bg, true);
    if width >= left_w.saturating_add(right_w).saturating_add(4) {
        let meter_w = width.saturating_sub(left_w).saturating_sub(right_w);
        draw_meter_with_gradient(
            grid,
            x.saturating_add(left_w),
            y,
            meter_w,
            percent,
            stops,
            theme,
        );
        let rx = x.saturating_add(width.saturating_sub(right_w));
        grid.write_str(rx, y, &right, theme.main_fg, theme.main_bg, false);
    } else if width > left_w {
        let clipped: String = right
            .chars()
            .take(usize::from(width.saturating_sub(left_w)))
            .collect();
        grid.write_str(
            x.saturating_add(left_w),
            y,
            &clipped,
            theme.main_fg,
            theme.main_bg,
            false,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn render_footer_meter_row(
    grid: &mut Grid,
    x: u16,
    y: u16,
    width: u16,
    label: &str,
    right_text: &str,
    percent: f32,
    stops: GradientStops,
    theme: &ThemePalette,
) {
    let left = format!(" {label:<6} ");
    let right = format!(" {right_text} ");
    let left_w = u16::try_from(left.chars().count()).unwrap_or(0);
    let right_w = u16::try_from(right.chars().count()).unwrap_or(0);

    grid.write_str(x, y, &left, theme.title, theme.main_bg, true);
    if width >= left_w.saturating_add(right_w).saturating_add(4) {
        let meter_w = width.saturating_sub(left_w).saturating_sub(right_w);
        draw_meter_with_gradient(
            grid,
            x.saturating_add(left_w),
            y,
            meter_w,
            percent,
            stops,
            theme,
        );
        let rx = x.saturating_add(width.saturating_sub(right_w));
        grid.write_str(rx, y, &right, theme.main_fg, theme.main_bg, false);
    } else if width > left_w {
        let clipped: String = right
            .chars()
            .take(usize::from(width.saturating_sub(left_w)))
            .collect();
        grid.write_str(
            x.saturating_add(left_w),
            y,
            &clipped,
            theme.main_fg,
            theme.main_bg,
            false,
        );
    }
}

#[cfg(test)]
mod tests {
    use btop_term::Size;

    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;

    #[test]
    fn format_mem_bytes_distinguishes_binary_and_base_10_units() {
        assert_eq!(format_mem_bytes(512, false), "512 B");
        assert_eq!(format_mem_bytes(512, true), "512 B");

        assert_eq!(format_mem_bytes(1024, false), "1.00 KiB");
        assert_eq!(format_mem_bytes(1000, true), "1.00 KB");

        assert_eq!(format_mem_bytes(16 * GIB, false), "16.0 GiB");
        assert_eq!(format_mem_bytes(16_000_000_000, true), "16.0 GB");

        assert_eq!(format_mem_bytes(64 * GIB, false), "64.0 GiB");
        assert_eq!(format_mem_bytes(64 * GIB, true), "68.7 GB");

        assert_eq!(format_mem_bytes(512 * MIB, false), "512 MiB");
        assert_eq!(format_mem_bytes(512 * MIB, true), "537 MB");
    }

    #[test]
    fn stacked_physical_segments_never_double_count_standby() {
        let mem = MemSnapshot {
            total_bytes: 64 * GIB,
            used_bytes: 26 * GIB,
            available_bytes: 38 * GIB,
            cached_bytes: 11 * GIB, // 10 GiB Standby + 1 GiB Modified
            free_bytes: 28 * GIB,
            ..MemSnapshot::default()
        };
        let (used, standby, free) = stacked_physical_segments(&mem);
        assert_eq!(used, 26 * GIB);
        assert_eq!(standby, 10 * GIB);
        assert_eq!(free, 28 * GIB);
        assert_eq!(used + standby + free, mem.total_bytes);
    }

    #[test]
    fn mem_box_renders_graph_meter_show_swap_and_base_10_modes() {
        let mem = MemSnapshot {
            total_bytes: 64 * GIB,
            used_bytes: 26 * GIB,
            available_bytes: 38 * GIB,
            cached_bytes: 11 * GIB,
            free_bytes: 27 * GIB,
            commit_used_bytes: 31 * GIB,
            commit_limit_bytes: 72 * GIB,
            pagefile_used_bytes: GIB,
            pagefile_total_bytes: 8 * GIB,
            compressed_bytes: Some(512 * MIB),
        };
        let snap = Snapshot {
            mem: DataState::Available(mem),
            ..Snapshot::default()
        };
        let mut hist = UiHistory::default();
        hist.push(&snap);
        hist.push(&snap);

        let graph_cfg = Config {
            shown_boxes: "mem".to_owned(),
            mem_graphs: true,
            show_swap: true,
            base_10_sizes: false,
            ..Config::default()
        };
        let graph_text = crate::render_with_history(
            &snap,
            &hist,
            &graph_cfg,
            Size {
                width: 60,
                height: 22,
            },
        )
        .to_plain_text();
        assert!(graph_text.contains("²mem"));
        assert!(graph_text.contains("64.0 GiB"));
        assert!(graph_text.contains("Commit"));
        assert!(graph_text.contains("Swap"));

        let meter_no_swap_base10 = Config {
            shown_boxes: "mem".to_owned(),
            mem_graphs: false,
            show_swap: false,
            base_10_sizes: true,
            ..Config::default()
        };
        let meter_text = crate::render_with_history(
            &snap,
            &hist,
            &meter_no_swap_base10,
            Size {
                width: 60,
                height: 22,
            },
        )
        .to_plain_text();
        assert!(meter_text.contains("68.7 GB"));
        assert!(meter_text.contains("Commit"));
        assert!(!meter_text.contains("Swap"));
    }
}
