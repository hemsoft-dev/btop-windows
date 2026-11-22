//! CPU monitoring panel (`1` / `cpu`) with dual-series split graph and per-core meters.

use std::fmt::Write as _;

use btop_config::Config;
use btop_model::{CoreSample, CpuSnapshot, DataState, Snapshot};
use btop_term::{Cell, Grid, HitAction, Rect};

use crate::UiHistory;
use crate::theme::ThemePalette;
use crate::widgets::{draw_border_tag, draw_box_frame, draw_graph, draw_meter, fill_rect};

/// Renders the full CPU box into `rect`.
pub fn render_cpu_box(
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
        theme.cpu_box,
        theme.main_bg,
        config.rounded_corners,
    );
    render_cpu_chrome(grid, rect, snapshot, config, theme);

    let inner = Rect {
        x: rect.x.saturating_add(1),
        y: rect.y.saturating_add(1),
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    };

    match &snapshot.cpu {
        DataState::Available(cpu) => {
            render_cpu_body(grid, inner, cpu, history, config, theme, None);
        }
        DataState::Stale { value, age_ms } => {
            let note = format!("stale {age_ms}ms");
            render_cpu_body(grid, inner, value, history, config, theme, Some(&note));
        }
        DataState::WarmingUp => {
            grid.write_str(
                inner.x + 1,
                inner.y + 1,
                "CPU warming up...",
                theme.inactive_fg,
                theme.main_bg,
                false,
            );
        }
        DataState::Denied => {
            grid.write_str(
                inner.x + 1,
                inner.y + 1,
                "CPU telemetry access denied",
                theme.hi_fg,
                theme.main_bg,
                true,
            );
        }
        DataState::Unavailable(reason) => {
            let msg = format!("CPU unavailable: {reason}");
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

fn render_cpu_chrome(
    grid: &mut Grid,
    rect: Rect,
    snapshot: &Snapshot,
    config: &Config,
    theme: &ThemePalette,
) {
    let max_top_x = rect.x.saturating_add(rect.width - 1);

    // Top-left hotkey title tag: `┐¹cpu┌`
    if let Some(tag_rect) = draw_border_tag(
        grid,
        rect.x.saturating_add(2),
        rect.y,
        max_top_x,
        Some('¹'),
        "cpu",
        theme.cpu_box,
        theme.title,
        theme.hi_fg,
        theme.main_bg,
    ) {
        grid.hit_map
            .push(tag_rect, HitAction::ToggleBox("cpu".to_owned()));
    }

    // Top-right cadence tag: `┐- 2000ms +┌`
    let cadence_label = format!("- {}ms +", config.update_ms);
    let cadence_w = u16::try_from(cadence_label.chars().count() + 2).unwrap_or(10);
    let cadence_x = max_top_x.saturating_sub(cadence_w + 1);
    if cadence_x > rect.x.saturating_add(10) {
        let _ = draw_border_tag(
            grid,
            cadence_x,
            rect.y,
            max_top_x,
            None,
            &cadence_label,
            theme.cpu_box,
            theme.title,
            theme.hi_fg,
            theme.main_bg,
        );
    }

    // Embedded CPU model name + uptime tag when available
    if let DataState::Available(cpu) | DataState::Stale { value: cpu, .. } = &snapshot.cpu {
        let uptime_label = format!("up {}", format_uptime(cpu.uptime_secs));
        let uptime_w = u16::try_from(uptime_label.chars().count() + 2).unwrap_or(10);
        let uptime_x = cadence_x.saturating_sub(uptime_w + 2);
        if uptime_x > rect.x.saturating_add(14) {
            let _ = draw_border_tag(
                grid,
                uptime_x,
                rect.y,
                cadence_x.saturating_sub(1),
                None,
                &uptime_label,
                theme.cpu_box,
                theme.main_fg,
                theme.hi_fg,
                theme.main_bg,
            );
        }

        let model_start = rect.x.saturating_add(10);
        let model_limit = uptime_x.saturating_sub(1);
        if model_limit > model_start.saturating_add(6) {
            let _ = draw_border_tag(
                grid,
                model_start,
                rect.y,
                model_limit,
                None,
                &cpu.model_name,
                theme.cpu_box,
                theme.title,
                theme.hi_fg,
                theme.main_bg,
            );
        }
    }

    // Bottom border hotkey hint tag
    let bottom_y = rect.y.saturating_add(rect.height - 1);
    let hint = format!(
        "{} | +/- cadence | Ctrl+R reload | q quit",
        snapshot.hostname
    );
    let _ = draw_border_tag(
        grid,
        rect.x.saturating_add(2),
        bottom_y,
        max_top_x,
        None,
        &hint,
        theme.cpu_box,
        theme.inactive_fg,
        theme.hi_fg,
        theme.main_bg,
    );
}

fn render_cpu_body(
    grid: &mut Grid,
    inner: Rect,
    cpu: &CpuSnapshot,
    history: &UiHistory,
    config: &Config,
    theme: &ThemePalette,
    stale_note: Option<&str>,
) {
    // Split interior into left history graph (~60%) and right stats/per-core panel (~40%).
    let right_w = if inner.width >= 90 {
        (inner.width / 3).clamp(34, 58)
    } else if inner.width >= 60 {
        (inner.width * 2 / 5).clamp(26, 38)
    } else {
        (inner.width / 2).max(16)
    };
    let graph_w = inner.width.saturating_sub(right_w + 1);

    let graph_rect = Rect {
        x: inner.x,
        y: inner.y,
        width: graph_w,
        height: inner.height,
    };
    let divider_x = inner.x.saturating_add(graph_w);
    let stats_rect = Rect {
        x: divider_x.saturating_add(1),
        y: inner.y,
        width: inner.width.saturating_sub(graph_w + 1),
        height: inner.height,
    };

    // Vertical divider between graph and core meters
    for y in inner.y..inner.y.saturating_add(inner.height) {
        grid.set(
            divider_x,
            y,
            Cell {
                ch: '│',
                fg: theme.div_line,
                bg: theme.main_bg,
                bold: false,
                continuation: false,
            },
        );
    }

    render_split_graph(grid, graph_rect, history, config, theme);
    render_stats_pane(grid, stats_rect, cpu, theme, stale_note);
}

fn render_split_graph(
    grid: &mut Grid,
    rect: Rect,
    history: &UiHistory,
    config: &Config,
    theme: &ThemePalette,
) {
    if rect.width < 4 || rect.height < 3 {
        return;
    }

    let upper_h = ((rect.height * 2) / 3).max(1);
    let lower_h = rect.height.saturating_sub(upper_h);

    let upper_rect = Rect {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: upper_h,
    };
    draw_graph(
        grid,
        upper_rect,
        history.cpu_total_slice(),
        config.graph_symbol,
        false,
        theme,
    );

    if lower_h > 0 {
        let lower_rect = Rect {
            x: rect.x,
            y: rect.y.saturating_add(upper_h),
            width: rect.width,
            height: lower_h,
        };
        draw_graph(
            grid,
            lower_rect,
            history.cpu_peak_slice(),
            config.graph_symbol,
            true,
            theme,
        );
    }

    // Crisp axis markers in the left edge of the graph pane
    grid.write_str(
        rect.x,
        rect.y,
        "100%",
        theme.inactive_fg,
        theme.main_bg,
        false,
    );
    if upper_h >= 2 {
        grid.write_str(
            rect.x,
            rect.y.saturating_add(upper_h - 1),
            "0%",
            theme.inactive_fg,
            theme.main_bg,
            false,
        );
    }
    if lower_h >= 2 {
        grid.write_str(
            rect.x,
            rect.y.saturating_add(rect.height - 1),
            "pk",
            theme.inactive_fg,
            theme.main_bg,
            false,
        );
    }
}

fn render_stats_pane(
    grid: &mut Grid,
    rect: Rect,
    cpu: &CpuSnapshot,
    theme: &ThemePalette,
    stale_note: Option<&str>,
) {
    if rect.width < 12 || rect.height < 3 {
        return;
    }

    let mut y = rect.y;
    let max_y = rect.y.saturating_add(rect.height);

    // Row 0: Total CPU meter + percentage
    let total_pct_str = format!("{:>5.1}%", cpu.total_busy_pct);
    let label = "CPU ";
    let reserved = u16::try_from(label.len() + total_pct_str.len() + 3).unwrap_or(13);
    grid.write_str(rect.x + 1, y, label, theme.title, theme.main_bg, true);
    let meter_w = rect.width.saturating_sub(reserved);
    let meter_x = rect
        .x
        .saturating_add(1 + u16::try_from(label.len()).unwrap_or(4));
    draw_meter(grid, meter_x, y, meter_w, cpu.total_busy_pct, theme);
    grid.write_str(
        meter_x.saturating_add(meter_w + 1),
        y,
        &total_pct_str,
        theme.cpu_gradient(cpu.total_busy_pct),
        theme.main_bg,
        true,
    );
    y = y.saturating_add(1);

    // Horizontal divider
    if y < max_y {
        let div = "─".repeat(usize::from(rect.width));
        grid.write_str(rect.x, y, &div, theme.div_line, theme.main_bg, false);
        y = y.saturating_add(1);
    }

    // Reserve footer rows for metadata (uptime, freq, RunQ / stale status)
    let footer_rows: u16 = if rect.height >= 8 { 2 } else { 1 };
    let core_area_end = max_y.saturating_sub(footer_rows);
    if y < core_area_end && !cpu.cores.is_empty() {
        render_core_meters(
            grid,
            Rect {
                x: rect.x + 1,
                y,
                width: rect.width.saturating_sub(2),
                height: core_area_end.saturating_sub(y),
            },
            &cpu.cores,
            theme,
        );
    }

    // Metadata footer inside right pane
    if core_area_end < max_y {
        let mut meta = format!("Up {}", format_uptime(cpu.uptime_secs));
        if let Some(freq) = cpu.freq_mhz {
            #[allow(clippy::cast_precision_loss)]
            let ghz = (freq as f32) / 1000.0;
            let _ = write!(meta, "  {ghz:.2} GHz");
        }
        if let Some(note) = stale_note {
            let _ = write!(meta, "  [{note}]");
        }
        let clipped: String = meta
            .chars()
            .take(usize::from(rect.width.saturating_sub(2)))
            .collect();
        grid.write_str(
            rect.x + 1,
            core_area_end,
            &clipped,
            theme.main_fg,
            theme.main_bg,
            false,
        );
    }

    if core_area_end + 1 < max_y {
        let runq = format!(
            "{} LP  RunQ {:.2} {:.2} {:.2}",
            cpu.cores.len(),
            cpu.run_queue_ewma[0],
            cpu.run_queue_ewma[1],
            cpu.run_queue_ewma[2]
        );
        let clipped: String = runq
            .chars()
            .take(usize::from(rect.width.saturating_sub(2)))
            .collect();
        grid.write_str(
            rect.x + 1,
            core_area_end + 1,
            &clipped,
            theme.inactive_fg,
            theme.main_bg,
            false,
        );
    }
}

fn render_core_meters(grid: &mut Grid, rect: Rect, cores: &[CoreSample], theme: &ThemePalette) {
    if rect.width < 10 || rect.height == 0 {
        return;
    }

    let cols: usize = if rect.width >= 30 && cores.len() > usize::from(rect.height) {
        2
    } else {
        1
    };
    let col_gap: u16 = if cols == 2 { 2 } else { 0 };
    let col_w = rect.width.saturating_sub(col_gap) / u16::try_from(cols).unwrap_or(1);
    let max_rows = usize::from(rect.height);

    for (idx, core) in cores.iter().enumerate() {
        let col = idx / max_rows;
        let row = idx % max_rows;
        if col >= cols {
            break;
        }
        let cell_x = rect
            .x
            .saturating_add(u16::try_from(col).unwrap_or(0) * (col_w + col_gap));
        let cell_y = rect.y.saturating_add(u16::try_from(row).unwrap_or(0));

        let label = format!("C{:02} ", core.id.number);
        let pct_str = format!("{:>3.0}%", core.busy_pct);
        let label_w = u16::try_from(label.len()).unwrap_or(4);
        let pct_w = u16::try_from(pct_str.len()).unwrap_or(4);

        grid.write_str(
            cell_x,
            cell_y,
            &label,
            theme.inactive_fg,
            theme.main_bg,
            false,
        );
        let meter_w = col_w.saturating_sub(label_w + pct_w + 1);
        let meter_x = cell_x.saturating_add(label_w);
        if meter_w >= 2 {
            draw_meter(grid, meter_x, cell_y, meter_w, core.busy_pct, theme);
            grid.write_str(
                meter_x.saturating_add(meter_w + 1),
                cell_y,
                &pct_str,
                theme.cpu_gradient(core.busy_pct),
                theme.main_bg,
                false,
            );
        } else {
            grid.write_str(
                meter_x,
                cell_y,
                &pct_str,
                theme.cpu_gradient(core.busy_pct),
                theme.main_bg,
                false,
            );
        }
    }
}

/// Formats uptime seconds as `HH:MM:SS` or `Xd HH:MM:SS`.
pub fn format_uptime(total_secs: u64) -> String {
    let days = total_secs / 86_400;
    let rem = total_secs % 86_400;
    let hours = rem / 3600;
    let mins = (rem % 3600) / 60;
    let secs = rem % 60;
    if days > 0 {
        format!("{days}d {hours:02}:{mins:02}:{secs:02}")
    } else {
        format!("{hours:02}:{mins:02}:{secs:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::format_uptime;

    #[test]
    fn formats_uptime_below_and_above_one_day() {
        assert_eq!(format_uptime(3661), "01:01:01");
        assert_eq!(format_uptime(90_061), "1d 01:01:01");
    }
}
