//! Dedicated GPU boxes (`5`..`0` -> `gpu0`..`gpu5`) and compact CPU-box GPU summary
//! honoring `custom_gpu_name0..5`, `gpu_mirror_graph`, `graph_symbol_gpu`, and `show_gpu_info`.

use std::fmt::Write as _;

use btop_config::{Config, ShowGpuInfo};
use btop_model::{DataState, GpuSnapshot, Snapshot};
use btop_term::{Cell, Grid, HitAction, Rect};

use crate::UiHistory;
use crate::mem_box::{format_mem_bytes, ratio_percent};
use crate::proc_box::clip_text;
use crate::theme::ThemePalette;
use crate::widgets::{draw_border_tag, draw_box_frame, draw_graph, draw_meter, fill_rect};

/// Formats the compact GPU utilization + VRAM summary line rendered inside the CPU box when
/// `config.show_gpu_info` is `On`, or `Auto` with no dedicated GPU box currently visible.
///
/// Unavailable optional fields (temperature, clocks, power) are omitted rather than shown as zero.
pub fn format_compact_gpu_summary(
    snapshot: &Snapshot,
    config: &Config,
    has_dedicated_gpu_boxes: bool,
) -> Option<String> {
    let enabled = match config.show_gpu_info {
        ShowGpuInfo::Off => false,
        ShowGpuInfo::On => true,
        ShowGpuInfo::Auto => !has_dedicated_gpu_boxes,
    };
    if !enabled {
        return None;
    }

    let available: Vec<&GpuSnapshot> = snapshot
        .gpus
        .iter()
        .filter_map(|state| match state {
            DataState::Available(g) | DataState::Stale { value: g, .. } => Some(g),
            _ => None,
        })
        .collect();
    if available.is_empty() {
        return None;
    }

    if available.len() == 1 {
        let g = available[0];
        let mut out = format!("GPU{}", g.index);
        if let Some(pct) = g.utilization_pct {
            let _ = write!(out, " {:>3.0}%", pct.clamp(0.0, 100.0));
        }
        if let (Some(used), Some(total)) = (g.vram_used_bytes, g.vram_total_bytes) {
            let _ = write!(
                out,
                "  VRAM {}/{}",
                format_mem_bytes(used, config.base_10_sizes),
                format_mem_bytes(total, config.base_10_sizes),
            );
        } else if let Some(used) = g.vram_used_bytes {
            let _ = write!(
                out,
                "  VRAM {}",
                format_mem_bytes(used, config.base_10_sizes)
            );
        }
        if let Some(temp) = g.temp_celsius {
            let _ = write!(out, "  {temp:.0}°C");
        }
        return Some(out);
    }

    let parts: Vec<String> = available
        .iter()
        .take(2)
        .map(|g| {
            let mut seg = format!("G{}", g.index);
            if let Some(pct) = g.utilization_pct {
                let _ = write!(seg, " {:.0}%", pct.clamp(0.0, 100.0));
            }
            if let (Some(used), Some(total)) = (g.vram_used_bytes, g.vram_total_bytes) {
                let _ = write!(
                    seg,
                    " {}/{}",
                    format_compact_bytes(used, config.base_10_sizes),
                    format_compact_bytes(total, config.base_10_sizes),
                );
            }
            seg
        })
        .collect();
    Some(parts.join(" | "))
}

/// Renders one dedicated GPU panel (`gpu0..gpu5`) inside `rect`.
pub fn render_gpu_box(
    grid: &mut Grid,
    rect: Rect,
    gpu_idx: u8,
    snapshot: &Snapshot,
    history: &UiHistory,
    config: &Config,
    theme: &ThemePalette,
) {
    if rect.width < 16 || rect.height < 5 {
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

    let gpu_state = snapshot.gpus.get(usize::from(gpu_idx));
    render_gpu_chrome(grid, rect, gpu_idx, gpu_state, config, theme);

    let inner = Rect {
        x: rect.x.saturating_add(1),
        y: rect.y.saturating_add(1),
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    };

    match gpu_state {
        Some(DataState::Available(gpu)) => {
            render_gpu_body(grid, inner, gpu_idx, gpu, history, config, theme, None);
        }
        Some(DataState::Stale { value, age_ms }) => {
            let note = format!("stale {age_ms}ms");
            render_gpu_body(
                grid,
                inner,
                gpu_idx,
                value,
                history,
                config,
                theme,
                Some(&note),
            );
        }
        Some(DataState::WarmingUp) => {
            let msg = clip_text(
                "GPU telemetry warming up...",
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
        Some(DataState::Denied) => {
            let msg = clip_text(
                "GPU telemetry access denied",
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
        Some(DataState::Unavailable(reason)) => {
            let msg = clip_text(
                &format!("GPU unavailable: {reason}"),
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
        None => {
            let msg = clip_text(
                &format!("GPU {gpu_idx} not detected"),
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
    }
}

fn gpu_hotkey_char(idx: u8) -> char {
    match idx {
        0 => '⁵',
        1 => '⁶',
        2 => '⁷',
        3 => '⁸',
        4 => '⁹',
        _ => '⁰',
    }
}

fn render_gpu_chrome(
    grid: &mut Grid,
    rect: Rect,
    gpu_idx: u8,
    gpu_state: Option<&DataState<GpuSnapshot>>,
    config: &Config,
    theme: &ThemePalette,
) {
    let max_x = rect.x.saturating_add(rect.width - 1);
    let label = format!("gpu{gpu_idx}");
    let mut left_end = rect.x.saturating_add(10);
    if let Some(tag_rect) = draw_border_tag(
        grid,
        rect.x.saturating_add(2),
        rect.y,
        max_x,
        Some(gpu_hotkey_char(gpu_idx)),
        &label,
        theme.cpu_box,
        theme.title,
        theme.hi_fg,
        theme.main_bg,
    ) {
        grid.hit_map.push(tag_rect, HitAction::ToggleBox(label));
        left_end = tag_rect.x.saturating_add(tag_rect.width);
    }

    let display_name = config.custom_gpu_name(gpu_idx).or(match gpu_state {
        Some(DataState::Available(g) | DataState::Stale { value: g, .. }) => Some(g.name.as_str()),
        _ => None,
    });
    if let Some(name) = display_name {
        let (name_start, name_limit) = if rect.y == 0 {
            let term_mid = grid.size().width / 2;
            let clock_left = term_mid.saturating_sub(9);
            let clock_right = term_mid.saturating_add(9);
            let start = if rect.x == 0 {
                left_end.saturating_add(1).max(rect.x.saturating_add(16))
            } else {
                left_end.saturating_add(1)
            };
            if start.saturating_add(6) < clock_left.min(max_x.saturating_sub(1)) {
                (start, clock_left.min(max_x.saturating_sub(1)))
            } else {
                (
                    start.max(clock_right.saturating_add(1)),
                    max_x.saturating_sub(1),
                )
            }
        } else {
            (left_end.saturating_add(1), max_x.saturating_sub(1))
        };
        if name_limit > name_start.saturating_add(6) {
            let max_chars = usize::from(name_limit.saturating_sub(name_start).saturating_sub(2));
            let clipped = clip_text(name, max_chars);
            let _ = draw_border_tag(
                grid,
                name_start,
                rect.y,
                name_limit,
                None,
                &clipped,
                theme.cpu_box,
                theme.title,
                theme.hi_fg,
                theme.main_bg,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_gpu_body(
    grid: &mut Grid,
    inner: Rect,
    gpu_idx: u8,
    gpu: &GpuSnapshot,
    history: &UiHistory,
    config: &Config,
    theme: &ThemePalette,
    stale_note: Option<&str>,
) {
    if inner.width < 12 || inner.height < 3 {
        return;
    }

    let right_w = if inner.width >= 84 {
        (inner.width * 38 / 100).clamp(32, 52)
    } else if inner.width >= 48 {
        (inner.width * 48 / 100).clamp(24, 36)
    } else {
        inner.width
    };

    if inner.width < 48 {
        render_gpu_stats_pane(grid, inner, gpu, config, theme, stale_note);
        return;
    }

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

    render_gpu_graph(grid, graph_rect, gpu_idx, gpu, history, config, theme);
    render_gpu_stats_pane(grid, stats_rect, gpu, config, theme, stale_note);
}

fn render_gpu_graph(
    grid: &mut Grid,
    rect: Rect,
    gpu_idx: u8,
    gpu: &GpuSnapshot,
    history: &UiHistory,
    config: &Config,
    theme: &ThemePalette,
) {
    if rect.width < 4 || rect.height < 2 {
        return;
    }
    let series = history.gpu_util_slice_for(gpu_idx, gpu);
    if config.gpu_mirror_graph && rect.height >= 4 {
        let upper_h = rect.height.div_ceil(2);
        let lower_h = rect.height.saturating_sub(upper_h);
        draw_graph(
            grid,
            Rect {
                height: upper_h,
                ..rect
            },
            series,
            config.graph_symbol,
            false,
            theme,
        );
        if lower_h > 0 {
            draw_graph(
                grid,
                Rect {
                    y: rect.y.saturating_add(upper_h),
                    height: lower_h,
                    ..rect
                },
                series,
                config.graph_symbol,
                true,
                theme,
            );
        }
    } else {
        draw_graph(grid, rect, series, config.graph_symbol, false, theme);
    }

    grid.write_str(
        rect.x,
        rect.y,
        "100%",
        theme.inactive_fg,
        theme.main_bg,
        false,
    );
    if rect.height >= 2 {
        grid.write_str(
            rect.x,
            rect.y.saturating_add(rect.height - 1),
            "0%",
            theme.inactive_fg,
            theme.main_bg,
            false,
        );
    }
}

fn render_gpu_stats_pane(
    grid: &mut Grid,
    rect: Rect,
    gpu: &GpuSnapshot,
    config: &Config,
    theme: &ThemePalette,
    stale_note: Option<&str>,
) {
    if rect.width < 12 || rect.height < 2 {
        return;
    }
    let mut y = rect.y;
    let max_y = rect.y.saturating_add(rect.height);
    let inner_w = usize::from(rect.width.saturating_sub(2));

    // Row 0: GPU headline utilization meter + percentage (or N/A when unavailable).
    if let Some(pct) = gpu.utilization_pct {
        let clamped = pct.clamp(0.0, 100.0);
        let pct_str = format!("{clamped:>5.1}%");
        grid.write_str(rect.x + 1, y, "GPU ", theme.title, theme.main_bg, true);
        let meter_x = rect.x.saturating_add(5);
        let reserved = u16::try_from(4 + pct_str.len() + 3).unwrap_or(13);
        let meter_w = rect.width.saturating_sub(reserved);
        if meter_w >= 2 {
            draw_meter(grid, meter_x, y, meter_w, clamped, theme);
            grid.write_str(
                meter_x.saturating_add(meter_w + 1),
                y,
                &pct_str,
                theme.cpu_gradient(clamped),
                theme.main_bg,
                true,
            );
        } else {
            let text = clip_text(&format!("GPU {pct_str}"), inner_w);
            grid.write_str(
                rect.x + 1,
                y,
                &text,
                theme.cpu_gradient(clamped),
                theme.main_bg,
                true,
            );
        }
    } else {
        let text = clip_text("GPU  utilization N/A", inner_w);
        grid.write_str(
            rect.x + 1,
            y,
            &text,
            theme.inactive_fg,
            theme.main_bg,
            false,
        );
    }
    y = y.saturating_add(1);

    // Optional horizontal divider when we have vertical headroom.
    if y < max_y && rect.height >= 5 {
        let div = "─".repeat(usize::from(rect.width));
        grid.write_str(rect.x, y, &div, theme.div_line, theme.main_bg, false);
        y = y.saturating_add(1);
    }

    // VRAM row (dedicated) when available.
    if y < max_y && (gpu.vram_used_bytes.is_some() || gpu.vram_total_bytes.is_some()) {
        render_vram_row(grid, rect, y, gpu, config, theme);
        y = y.saturating_add(1);
    }

    // Shared GPU memory row when reported by WDDM PDH.
    if y < max_y
        && let Some(shared_used) = gpu.shared_vram_used_bytes
    {
        let mut line = format!(
            "SHRD {}",
            format_mem_bytes(shared_used, config.base_10_sizes)
        );
        if let Some(shared_total) = gpu.shared_vram_total_bytes {
            let _ = write!(
                line,
                " / {}",
                format_mem_bytes(shared_total, config.base_10_sizes)
            );
        }
        let clipped = clip_text(&line, inner_w);
        grid.write_str(rect.x + 1, y, &clipped, theme.main_fg, theme.main_bg, false);
        y = y.saturating_add(1);
    }

    // Optional vendor-enriched lines: strictly omitted when `None` (never render placeholder zeros).
    for line in optional_vendor_lines(gpu, stale_note) {
        if y >= max_y {
            break;
        }
        let clipped = clip_text(&line, inner_w);
        grid.write_str(rect.x + 1, y, &clipped, theme.main_fg, theme.main_bg, false);
        y = y.saturating_add(1);
    }

    // Vendor / PCI BDF metadata footer when space permits.
    if y < max_y {
        let mut meta = gpu.vendor.clone();
        if let Some(ref bdf) = gpu.pci_bdf {
            let _ = write!(meta, "  {bdf}");
        }
        let clipped = clip_text(&meta, inner_w);
        grid.write_str(
            rect.x + 1,
            max_y - 1,
            &clipped,
            theme.inactive_fg,
            theme.main_bg,
            false,
        );
    }
}

fn render_vram_row(
    grid: &mut Grid,
    rect: Rect,
    y: u16,
    gpu: &GpuSnapshot,
    config: &Config,
    theme: &ThemePalette,
) {
    let inner_w = usize::from(rect.width.saturating_sub(2));
    match (gpu.vram_used_bytes, gpu.vram_total_bytes) {
        (Some(used), Some(total)) if total > 0 => {
            let pct = ratio_percent(used, total);
            let label = format!(
                "{}/{}",
                format_mem_bytes(used, config.base_10_sizes),
                format_mem_bytes(total, config.base_10_sizes)
            );
            grid.write_str(rect.x + 1, y, "VRAM ", theme.title, theme.main_bg, false);
            let meter_x = rect.x.saturating_add(6);
            let reserved = u16::try_from(5 + label.len() + 3).unwrap_or(20);
            let meter_w = rect.width.saturating_sub(reserved);
            if meter_w >= 4 {
                draw_meter(grid, meter_x, y, meter_w, pct, theme);
                grid.write_str(
                    meter_x.saturating_add(meter_w + 1),
                    y,
                    &label,
                    theme.used_gradient(pct),
                    theme.main_bg,
                    false,
                );
            } else {
                let compact = format!(
                    "{}/{}",
                    format_compact_bytes(used, config.base_10_sizes),
                    format_compact_bytes(total, config.base_10_sizes)
                );
                let text = clip_text(&format!("VRAM {compact} ({pct:.0}%)"), inner_w);
                grid.write_str(
                    rect.x + 1,
                    y,
                    &text,
                    theme.used_gradient(pct),
                    theme.main_bg,
                    false,
                );
            }
        }
        (Some(used), _) => {
            let text = clip_text(
                &format!("VRAM {}", format_mem_bytes(used, config.base_10_sizes)),
                inner_w,
            );
            grid.write_str(rect.x + 1, y, &text, theme.main_fg, theme.main_bg, false);
        }
        (None, Some(total)) => {
            let text = clip_text(
                &format!(
                    "VRAM total {}",
                    format_mem_bytes(total, config.base_10_sizes)
                ),
                inner_w,
            );
            grid.write_str(
                rect.x + 1,
                y,
                &text,
                theme.inactive_fg,
                theme.main_bg,
                false,
            );
        }
        (None, None) => {}
    }
}

fn optional_vendor_lines(gpu: &GpuSnapshot, stale_note: Option<&str>) -> Vec<String> {
    let mut lines = Vec::new();

    let mut thermal_parts = Vec::new();
    if let Some(temp) = gpu.temp_celsius {
        if let Some(max_temp) = gpu.temp_max_celsius {
            thermal_parts.push(format!("TEMP {temp:.0}/{max_temp:.0}°C"));
        } else {
            thermal_parts.push(format!("TEMP {temp:.0}°C"));
        }
    }
    if let Some(power) = gpu.power_watts {
        if let Some(limit) = gpu.power_limit_watts {
            thermal_parts.push(format!("PWR {power:.0}/{limit:.0}W"));
        } else {
            thermal_parts.push(format!("PWR {power:.0}W"));
        }
    }
    if !thermal_parts.is_empty() {
        lines.push(thermal_parts.join("  "));
    }

    let mut clk_parts = Vec::new();
    if let Some(core) = gpu.core_clock_mhz {
        clk_parts.push(format!("CLK {core} MHz"));
    }
    if let Some(mem) = gpu.mem_clock_mhz {
        clk_parts.push(format!("MEM {mem} MHz"));
    }
    if let Some(ref ps) = gpu.pstate {
        clk_parts.push(ps.clone());
    }
    if !clk_parts.is_empty() {
        lines.push(clk_parts.join("  "));
    }

    let mut media_parts = Vec::new();
    if let Some(enc) = gpu.encoder_pct {
        media_parts.push(format!("ENC {enc:.0}%"));
    }
    if let Some(dec) = gpu.decoder_pct {
        media_parts.push(format!("DEC {dec:.0}%"));
    }
    if !media_parts.is_empty() {
        lines.push(media_parts.join("  "));
    }

    if gpu.pcie_tx_kbs.is_some() || gpu.pcie_rx_kbs.is_some() {
        let mut pcie_parts = Vec::new();
        if let Some(tx) = gpu.pcie_tx_kbs {
            pcie_parts.push(format!("TX {tx} KB/s"));
        }
        if let Some(rx) = gpu.pcie_rx_kbs {
            pcie_parts.push(format!("RX {rx} KB/s"));
        }
        lines.push(format!("PCIe {}", pcie_parts.join("  ")));
    }

    if let Some(note) = stale_note {
        lines.push(format!("[{note}]"));
    }

    lines
}

fn format_compact_bytes(bytes: u64, base_10: bool) -> String {
    let unit = if base_10 { 1000.0_f64 } else { 1024.0_f64 };
    #[allow(clippy::cast_precision_loss)]
    let b = bytes as f64;
    let gib = unit * unit * unit;
    let mib = unit * unit;
    if b >= gib {
        format!("{:.1}G", b / gib)
    } else if b >= mib {
        format!("{:.0}M", b / mib)
    } else {
        format!("{:.0}K", b / unit)
    }
}

#[cfg(test)]
mod tests {
    use btop_config::GraphSymbol;
    use btop_model::fixtures::{win11_8core_single_gpu_desktop, win11_16core_dual_gpu_desktop};
    use btop_term::Size;

    use super::*;
    use crate::{UiState, render_frame};

    #[test]
    fn gpu_box_honors_custom_name_mirror_graph_and_hides_missing_vendor_fields() {
        let mut snap = win11_16core_dual_gpu_desktop()[0].clone();
        // Verify GPU 1 (AMD iGPU with None temp/power/clocks) hides unavailable fields cleanly.
        let mut cfg = Config {
            shown_boxes: "gpu0 gpu1".to_owned(),
            gpu_mirror_graph: false,
            graph_symbol_gpu: Some(GraphSymbol::Block),
            ..Config::default()
        };
        cfg.custom_gpu_names[0] = "Workstation 5090".to_owned();
        let hist = UiHistory::from_snapshot(&snap);
        let state = UiState::from_config(&cfg);
        let grid = render_frame(
            &snap,
            &hist,
            &state,
            &cfg,
            Size {
                width: 120,
                height: 24,
            },
        );
        let plain = grid.to_plain_text();
        assert!(
            plain.contains("Workstation 5090"),
            "expected custom GPU name:\n{plain}"
        );
        assert!(plain.contains("⁵gpu0"));
        assert!(plain.contains("⁶gpu1"));
        assert!(plain.contains("AMD Radeon(TM) Graphics"));

        // Strip vendor fields from gpu0 too and confirm no "0°C" / "0W" / "0 MHz" placeholders appear.
        if let DataState::Available(ref mut g0) = snap.gpus[0] {
            g0.temp_celsius = None;
            g0.temp_max_celsius = None;
            g0.power_watts = None;
            g0.power_limit_watts = None;
            g0.core_clock_mhz = None;
            g0.mem_clock_mhz = None;
            g0.pstate = None;
            g0.encoder_pct = None;
            g0.decoder_pct = None;
            g0.pcie_tx_kbs = None;
            g0.pcie_rx_kbs = None;
        }
        let baseline_only = render_frame(
            &snap,
            &hist,
            &state,
            &cfg,
            Size {
                width: 120,
                height: 24,
            },
        )
        .to_plain_text();
        assert!(!baseline_only.contains("TEMP"));
        assert!(!baseline_only.contains("PWR"));
        assert!(!baseline_only.contains("CLK"));
        assert!(!baseline_only.contains("ENC"));
        assert!(!baseline_only.contains("PCIe"));

        // Verify optional NVML temp_max_celsius and pstate render cleanly when populated.
        if let DataState::Available(ref mut g0) = snap.gpus[0] {
            g0.temp_celsius = Some(47.0);
            g0.temp_max_celsius = Some(90.0);
            g0.core_clock_mhz = Some(2782);
            g0.pstate = Some("P0".to_owned());
        }
        let enriched = render_frame(
            &snap,
            &hist,
            &state,
            &cfg,
            Size {
                width: 120,
                height: 24,
            },
        )
        .to_plain_text();
        assert!(
            enriched.contains("TEMP 47/90°C"),
            "expected max temp:\n{enriched}"
        );
        assert!(enriched.contains("P0"), "expected pstate:\n{enriched}");
    }

    #[test]
    fn compact_gpu_summary_respects_auto_on_and_off_modes() {
        let single = win11_8core_single_gpu_desktop()[0].clone();
        let dual = win11_16core_dual_gpu_desktop()[0].clone();

        let auto_cfg = Config {
            show_gpu_info: ShowGpuInfo::Auto,
            ..Config::default()
        };
        assert!(format_compact_gpu_summary(&single, &auto_cfg, false).is_some());
        assert_eq!(format_compact_gpu_summary(&single, &auto_cfg, true), None);

        let on_cfg = Config {
            show_gpu_info: ShowGpuInfo::On,
            ..Config::default()
        };
        let dual_text = format_compact_gpu_summary(&dual, &on_cfg, true).expect("dual summary");
        assert!(dual_text.contains("G0 "));
        assert!(dual_text.contains("G1 "));

        let off_cfg = Config {
            show_gpu_info: ShowGpuInfo::Off,
            ..Config::default()
        };
        assert_eq!(format_compact_gpu_summary(&single, &off_cfg, false), None);
    }
}
