//! Local fixed disk capacity sub-panel (`d`) and Disk I/O throughput graph view (`i`)
//! rendered inside the `²mem` panel.

use std::collections::BTreeMap;

use btop_config::Config;
use btop_model::{DataState, DiskSnapshot, Snapshot};
use btop_term::{Grid, Rect};

use crate::UiHistory;
use crate::mem_box::{format_mem_bytes, ratio_percent};
use crate::theme::ThemePalette;
use crate::widgets::{draw_graph_with_gradient, draw_meter_with_gradient};

const MIN_AUTO_SCALE_BPS: u64 = 1024 * 1024; // 1 MiB/s floor so idle graphs stay readable.

/// Formats a byte-per-second throughput rate cleanly (e.g. `85.0 MiB/s` or `89.1 MB/s`).
pub fn format_rate_bytes_per_sec(bps: u64, base_10: bool) -> String {
    format!("{}/s", format_mem_bytes(bps, base_10))
}

/// Parses `io_graph_speeds` entries such as `"C:500 D:1000"` or `"C:\\:250,D:500"` (values in
/// MiB/s when `base_10 = false` or MB/s when `base_10 = true`) into per-drive bytes/sec ceilings.
pub fn parse_io_graph_speeds(spec: &str, base_10: bool) -> BTreeMap<char, u64> {
    let unit: u64 = if base_10 { 1_000_000 } else { 1_048_576 };
    let mut map = BTreeMap::new();
    for token in spec.split(|c: char| c.is_ascii_whitespace() || c == ',' || c == ';') {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some((drive_part, speed_part)) = trimmed.rsplit_once(':') else {
            continue;
        };
        let Some(letter) = extract_drive_letter(drive_part) else {
            continue;
        };
        if let Ok(mib_per_sec) = speed_part.trim().parse::<u64>()
            && mib_per_sec > 0
        {
            map.insert(letter, mib_per_sec.saturating_mul(unit));
        }
    }
    map
}

/// Evaluates whether a drive mount (`C:\`, `D:\`, ...) passes `disks_filter`.
pub fn matches_disks_filter(mount: &str, filter: &str) -> bool {
    let trimmed = filter.trim();
    if trimmed.is_empty() {
        return true;
    }
    let Some(drive_letter) = extract_drive_letter(mount) else {
        return false;
    };
    let (is_exclude, body) = if trimmed.len() >= 8 && trimmed[..8].eq_ignore_ascii_case("exclude=")
    {
        (true, &trimmed[8..])
    } else {
        (false, trimmed)
    };
    let letters: Vec<char> = body
        .split(|c: char| c.is_ascii_whitespace() || c == ',' || c == ';')
        .filter_map(extract_drive_letter)
        .collect();
    if letters.is_empty() {
        return true;
    }
    let listed = letters.contains(&drive_letter);
    if is_exclude { !listed } else { listed }
}

fn extract_drive_letter(token: &str) -> Option<char> {
    let trimmed = token.trim().trim_start_matches('/');
    let ch = trimmed.chars().next()?;
    if !ch.is_ascii_alphabetic() {
        return None;
    }
    let rest = &trimmed[ch.len_utf8()..];
    if rest.is_empty() || rest == ":" || rest == ":\\" || rest == ":/" {
        Some(ch.to_ascii_uppercase())
    } else {
        None
    }
}

/// Filters `snapshot.disks` using `config.disks_filter`.
pub fn filtered_disks<'a>(
    snapshot: &'a Snapshot,
    config: &Config,
) -> Option<Vec<&'a DiskSnapshot>> {
    match &snapshot.disks {
        DataState::Available(list) | DataState::Stale { value: list, .. } => Some(
            list.iter()
                .filter(|d| matches_disks_filter(&d.mount, &config.disks_filter))
                .collect(),
        ),
        _ => None,
    }
}

/// Renders the regular Disks sub-panel (`show_disks = true`, `io_mode = false`) inside `rect`.
pub fn render_disks_subpanel(
    grid: &mut Grid,
    rect: Rect,
    snapshot: &Snapshot,
    config: &Config,
    theme: &ThemePalette,
) {
    if rect.width < 16 || rect.height < 2 {
        return;
    }

    match &snapshot.disks {
        DataState::Available(list) => {
            let disks: Vec<&DiskSnapshot> = list
                .iter()
                .filter(|d| matches_disks_filter(&d.mount, &config.disks_filter))
                .collect();
            render_disks_list(grid, rect, &disks, config, theme, None);
        }
        DataState::Stale { value, age_ms } => {
            let note = format!("stale {age_ms}ms");
            let disks: Vec<&DiskSnapshot> = value
                .iter()
                .filter(|d| matches_disks_filter(&d.mount, &config.disks_filter))
                .collect();
            render_disks_list(grid, rect, &disks, config, theme, Some(&note));
        }
        DataState::WarmingUp => {
            grid.write_str(
                rect.x.saturating_add(1),
                rect.y,
                "Disks warming up...",
                theme.inactive_fg,
                theme.main_bg,
                false,
            );
        }
        DataState::Denied => {
            grid.write_str(
                rect.x.saturating_add(1),
                rect.y,
                "Disk telemetry access denied",
                theme.hi_fg,
                theme.main_bg,
                true,
            );
        }
        DataState::Unavailable(reason) => {
            let msg = format!("Disks unavailable: {reason}");
            let clipped: String = msg
                .chars()
                .take(usize::from(rect.width.saturating_sub(2)))
                .collect();
            grid.write_str(
                rect.x.saturating_add(1),
                rect.y,
                &clipped,
                theme.inactive_fg,
                theme.main_bg,
                false,
            );
        }
    }
}

fn render_disks_list(
    grid: &mut Grid,
    rect: Rect,
    disks: &[&DiskSnapshot],
    config: &Config,
    theme: &ThemePalette,
    stale_note: Option<&str>,
) {
    let mut y = rect.y;
    let y_end = rect.y.saturating_add(rect.height);

    let header = if let Some(note) = stale_note {
        format!(" Disks ({}) [{note}]", disks.len())
    } else {
        format!(" Disks ({})", disks.len())
    };
    let clipped_hdr: String = header.chars().take(usize::from(rect.width)).collect();
    grid.write_str(rect.x, y, &clipped_hdr, theme.title, theme.main_bg, true);
    y = y.saturating_add(1);

    if disks.is_empty() {
        if y < y_end {
            grid.write_str(
                rect.x.saturating_add(1),
                y,
                "No matching local fixed drives",
                theme.inactive_fg,
                theme.main_bg,
                false,
            );
        }
        return;
    }

    let rows_per_disk: u16 =
        if config.show_io_stat && rect.height > u16::try_from(disks.len()).unwrap_or(1) * 3 {
            3
        } else {
            2
        };

    for disk in disks {
        if y >= y_end {
            break;
        }
        render_disk_entry(grid, rect, y, y_end, rows_per_disk, disk, config, theme);
        y = y.saturating_add(rows_per_disk);
    }
}

#[allow(clippy::too_many_arguments)]
fn render_disk_entry(
    grid: &mut Grid,
    rect: Rect,
    y: u16,
    y_end: u16,
    rows_per_disk: u16,
    disk: &DiskSnapshot,
    config: &Config,
    theme: &ThemePalette,
) {
    let used_pct = ratio_percent(disk.used_bytes, disk.total_bytes);
    let right = format!(
        "{}/{} {:>3.0}% ",
        format_mem_bytes(disk.used_bytes, config.base_10_sizes),
        format_mem_bytes(disk.total_bytes, config.base_10_sizes),
        used_pct
    );

    let right_w = u16::try_from(right.chars().count()).unwrap_or(0);
    let max_left = usize::from(rect.width.saturating_sub(right_w).saturating_sub(1));
    let full_left = match (disk.label.is_empty(), disk.fs_type.is_empty()) {
        (false, false) => format!(" {} {} ({})", disk.mount, disk.label, disk.fs_type),
        (false, true) => format!(" {} {}", disk.mount, disk.label),
        (true, false) => format!(" {} ({})", disk.mount, disk.fs_type),
        (true, true) => format!(" {}", disk.mount),
    };
    let left_clipped = if full_left.chars().count() <= max_left {
        full_left
    } else if !disk.label.is_empty()
        && format!(" {} {}", disk.mount, disk.label).chars().count() <= max_left
    {
        format!(" {} {}", disk.mount, disk.label)
    } else {
        format!(" {}", disk.mount)
            .chars()
            .take(max_left.max(4))
            .collect()
    };
    grid.write_str(rect.x, y, &left_clipped, theme.hi_fg, theme.main_bg, true);
    if rect.width > right_w.saturating_add(4) {
        let rx = rect.x.saturating_add(rect.width.saturating_sub(right_w));
        grid.write_str(rx, y, &right, theme.main_fg, theme.main_bg, false);
    }

    if y.saturating_add(1) < y_end {
        let meter_x = rect.x.saturating_add(1);
        let meter_w = rect.width.saturating_sub(2);
        draw_meter_with_gradient(
            grid,
            meter_x,
            y.saturating_add(1),
            meter_w,
            used_pct,
            theme.cpu_stops(),
            theme,
        );
    }

    if rows_per_disk >= 3 && config.show_io_stat && y.saturating_add(2) < y_end {
        let io_line = format_disk_io_summary(disk, config.base_10_sizes);
        let clipped: String = io_line.chars().take(usize::from(rect.width)).collect();
        grid.write_str(
            rect.x,
            y.saturating_add(2),
            &clipped,
            theme.inactive_fg,
            theme.main_bg,
            false,
        );
    }
}

/// Renders the full Disk I/O graph mode (`io_mode = true`) inside the `²mem` box interior.
pub fn render_disk_io_mode(
    grid: &mut Grid,
    inner: Rect,
    snapshot: &Snapshot,
    history: &UiHistory,
    config: &Config,
    theme: &ThemePalette,
) {
    if inner.width < 18 || inner.height < 4 {
        return;
    }

    let Some((disks, stale_note)) = resolve_io_mode_disks(grid, inner, snapshot, config, theme)
    else {
        return;
    };

    if disks.is_empty() {
        grid.write_str(
            inner.x.saturating_add(1),
            inner.y.saturating_add(1),
            "No matching local fixed drives",
            theme.inactive_fg,
            theme.main_bg,
            false,
        );
        return;
    }

    let speed_overrides = parse_io_graph_speeds(&config.io_graph_speeds, config.base_10_sizes);
    let mut y = inner.y;
    let y_end = inner.y.saturating_add(inner.height);

    let mode_label = if config.io_graph_combined {
        "combined"
    } else {
        "split R/W"
    };
    let header = if let Some(note) = stale_note {
        format!(" Disk I/O ({mode_label}) [{note}]  d:disks  i:mem")
    } else {
        format!(" Disk I/O ({mode_label})  d:disks  i:mem")
    };
    let clipped_hdr: String = header.chars().take(usize::from(inner.width)).collect();
    grid.write_str(inner.x, y, &clipped_hdr, theme.title, theme.main_bg, true);
    y = y.saturating_add(1);

    let avail_h = y_end.saturating_sub(y);
    let drive_count = u16::try_from(disks.len()).unwrap_or(1).max(1);
    let band_h = (avail_h / drive_count).max(3);

    for (idx, disk) in disks.iter().enumerate() {
        if y.saturating_add(2) > y_end {
            break;
        }
        let is_last = idx + 1 == disks.len();
        let this_band_h = if is_last {
            y_end.saturating_sub(y)
        } else {
            band_h.min(y_end.saturating_sub(y))
        };
        render_drive_io_band(
            grid,
            Rect {
                x: inner.x,
                y,
                width: inner.width,
                height: this_band_h,
            },
            disk,
            history,
            &speed_overrides,
            config,
            theme,
        );
        y = y.saturating_add(this_band_h);
    }
}

fn resolve_io_mode_disks<'a>(
    grid: &mut Grid,
    inner: Rect,
    snapshot: &'a Snapshot,
    config: &Config,
    theme: &ThemePalette,
) -> Option<(Vec<&'a DiskSnapshot>, Option<String>)> {
    match &snapshot.disks {
        DataState::Available(list) => Some((
            list.iter()
                .filter(|d| matches_disks_filter(&d.mount, &config.disks_filter))
                .collect(),
            None,
        )),
        DataState::Stale { value, age_ms } => Some((
            value
                .iter()
                .filter(|d| matches_disks_filter(&d.mount, &config.disks_filter))
                .collect(),
            Some(format!("stale {age_ms}ms")),
        )),
        DataState::WarmingUp => {
            grid.write_str(
                inner.x.saturating_add(1),
                inner.y.saturating_add(1),
                "Disk I/O warming up...",
                theme.inactive_fg,
                theme.main_bg,
                false,
            );
            None
        }
        DataState::Denied => {
            grid.write_str(
                inner.x.saturating_add(1),
                inner.y.saturating_add(1),
                "Disk I/O telemetry access denied",
                theme.hi_fg,
                theme.main_bg,
                true,
            );
            None
        }
        DataState::Unavailable(reason) => {
            let msg = format!("Disk I/O unavailable: {reason}");
            let clipped: String = msg
                .chars()
                .take(usize::from(inner.width.saturating_sub(2)))
                .collect();
            grid.write_str(
                inner.x.saturating_add(1),
                inner.y.saturating_add(1),
                &clipped,
                theme.inactive_fg,
                theme.main_bg,
                false,
            );
            None
        }
    }
}

fn compute_drive_scale_max(
    disk: &DiskSnapshot,
    read_raw: &[u64],
    write_raw: &[u64],
    speed_overrides: &BTreeMap<char, u64>,
    combined: bool,
) -> u64 {
    let configured_max =
        extract_drive_letter(&disk.mount).and_then(|c| speed_overrides.get(&c).copied());
    let observed_peak = if combined {
        read_raw
            .iter()
            .zip(write_raw.iter())
            .map(|(&r, &w)| r.saturating_add(w))
            .max()
            .unwrap_or(0)
            .max(
                disk.read_bytes_per_sec
                    .unwrap_or(0)
                    .saturating_add(disk.write_bytes_per_sec.unwrap_or(0)),
            )
    } else {
        read_raw
            .iter()
            .chain(write_raw.iter())
            .copied()
            .max()
            .unwrap_or(0)
            .max(disk.read_bytes_per_sec.unwrap_or(0))
            .max(disk.write_bytes_per_sec.unwrap_or(0))
    };
    configured_max
        .unwrap_or_else(|| observed_peak.max(MIN_AUTO_SCALE_BPS))
        .max(1)
}

#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn render_drive_io_band(
    grid: &mut Grid,
    band: Rect,
    disk: &DiskSnapshot,
    history: &UiHistory,
    speed_overrides: &BTreeMap<char, u64>,
    config: &Config,
    theme: &ThemePalette,
) {
    let series = history.disk_series(&disk.mount);
    let read_raw = series.map_or(&[][..], crate::DiskIoSeries::read_slice);
    let write_raw = series.map_or(&[][..], crate::DiskIoSeries::write_slice);
    let scale_max = compute_drive_scale_max(
        disk,
        read_raw,
        write_raw,
        speed_overrides,
        config.io_graph_combined,
    );

    let label = if disk.label.is_empty() {
        format!(" {}", disk.mount)
    } else {
        format!(" {} {}", disk.mount, disk.label)
    };
    let full_stats = if config.show_io_stat {
        format!(
            "{} [max {}] ",
            format_disk_io_summary(disk, config.base_10_sizes).trim(),
            format_rate_bytes_per_sec(scale_max, config.base_10_sizes)
        )
    } else {
        format!(
            "[max {}] ",
            format_rate_bytes_per_sec(scale_max, config.base_10_sizes)
        )
    };
    let stats = if u16::try_from(full_stats.chars().count())
        .unwrap_or(u16::MAX)
        .saturating_add(5)
        <= band.width
    {
        full_stats
    } else if config.show_io_stat {
        let short_io = format!(
            "{} ",
            format_disk_io_summary(disk, config.base_10_sizes).trim()
        );
        if u16::try_from(short_io.chars().count())
            .unwrap_or(u16::MAX)
            .saturating_add(5)
            <= band.width
        {
            short_io
        } else {
            short_io
                .replace(" MiB", "M")
                .replace(" KiB", "K")
                .replace(" GiB", "G")
                .replace(" MB", "M")
                .replace(" KB", "K")
                .replace(" GB", "G")
        }
    } else {
        full_stats
    };

    let stats_w = u16::try_from(stats.chars().count()).unwrap_or(0);
    let max_left = usize::from(band.width.saturating_sub(stats_w).saturating_sub(1));
    let clipped_left: String = if label.chars().count() <= max_left {
        label
    } else {
        format!(" {}", disk.mount)
            .chars()
            .take(max_left.max(4))
            .collect()
    };
    grid.write_str(
        band.x,
        band.y,
        &clipped_left,
        theme.hi_fg,
        theme.main_bg,
        true,
    );
    if band.width > stats_w.saturating_add(4) {
        let rx = band.x.saturating_add(band.width.saturating_sub(stats_w));
        grid.write_str(rx, band.y, &stats, theme.main_fg, theme.main_bg, false);
    }

    let graph_h = band.height.saturating_sub(1);
    if graph_h == 0 || band.width < 4 {
        return;
    }

    let graph_rect = Rect {
        x: band.x.saturating_add(1),
        y: band.y.saturating_add(1),
        width: band.width.saturating_sub(2),
        height: graph_h,
    };
    draw_drive_io_graphs(
        grid, graph_rect, read_raw, write_raw, scale_max, config, theme,
    );
}

#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn draw_drive_io_graphs(
    grid: &mut Grid,
    graph_rect: Rect,
    read_raw: &[u64],
    write_raw: &[u64],
    scale_max: u64,
    config: &Config,
    theme: &ThemePalette,
) {
    let to_pct =
        |bps: u64| -> f32 { ((bps as f64 / scale_max as f64) * 100.0).clamp(0.0, 100.0) as f32 };

    if config.io_graph_combined || graph_rect.height < 2 {
        let combined: Vec<f32> = read_raw
            .iter()
            .zip(write_raw.iter())
            .map(|(&r, &w)| to_pct(r.saturating_add(w)))
            .collect();
        draw_graph_with_gradient(
            grid,
            graph_rect,
            &combined,
            config.graph_symbol,
            false,
            theme.cached_stops(),
            theme,
        );
    } else {
        let read_pcts: Vec<f32> = read_raw.iter().map(|&r| to_pct(r)).collect();
        let write_pcts: Vec<f32> = write_raw.iter().map(|&w| to_pct(w)).collect();
        let top_h = graph_rect.height.div_ceil(2);
        let bottom_h = graph_rect.height.saturating_sub(top_h);
        draw_graph_with_gradient(
            grid,
            Rect {
                height: top_h,
                ..graph_rect
            },
            &read_pcts,
            config.graph_symbol,
            false,
            theme.cached_stops(),
            theme,
        );
        if bottom_h > 0 {
            draw_graph_with_gradient(
                grid,
                Rect {
                    y: graph_rect.y.saturating_add(top_h),
                    height: bottom_h,
                    ..graph_rect
                },
                &write_pcts,
                config.graph_symbol,
                true,
                theme.used_stops(),
                theme,
            );
        }
    }
}

fn format_disk_io_summary(disk: &DiskSnapshot, base_10: bool) -> String {
    match (
        disk.read_bytes_per_sec,
        disk.write_bytes_per_sec,
        disk.busy_pct,
    ) {
        (Some(r), Some(w), Some(busy)) => format!(
            " R:{} W:{} {:>3.0}%",
            format_rate_bytes_per_sec(r, base_10),
            format_rate_bytes_per_sec(w, base_10),
            busy
        ),
        (Some(r), Some(w), None) => format!(
            " R:{} W:{}",
            format_rate_bytes_per_sec(r, base_10),
            format_rate_bytes_per_sec(w, base_10)
        ),
        _ => " I/O: warming...".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_io_graph_speeds_parses_binary_and_base_10_ceilings() {
        let bin = parse_io_graph_speeds("C:500 D:\\:1000", false);
        assert_eq!(bin.get(&'C'), Some(&(500 * 1_048_576)));
        assert_eq!(bin.get(&'D'), Some(&(1000 * 1_048_576)));

        let dec = parse_io_graph_speeds("c:250,e:0", true);
        assert_eq!(dec.get(&'C'), Some(&(250 * 1_000_000)));
        assert!(!dec.contains_key(&'E'));
    }

    #[test]
    fn format_rate_uses_binary_or_base_10() {
        assert_eq!(format_rate_bytes_per_sec(1_048_576, false), "1.00 MiB/s");
        assert_eq!(format_rate_bytes_per_sec(1_000_000, true), "1.00 MB/s");
    }

    #[test]
    fn ui_disks_filter_and_combined_io_mode_render_cleanly() {
        use btop_term::Size;

        assert!(matches_disks_filter("C:\\", "C D"));
        assert!(!matches_disks_filter("E:\\", "C D"));
        assert!(!matches_disks_filter("E:\\", "exclude=E"));

        let snap = Snapshot {
            disks: DataState::Available(vec![
                DiskSnapshot {
                    mount: "C:\\".to_owned(),
                    label: "OS".to_owned(),
                    fs_type: "NTFS".to_owned(),
                    physical_drive: Some(0),
                    total_bytes: 500 * 1024 * 1024 * 1024,
                    used_bytes: 200 * 1024 * 1024 * 1024,
                    free_bytes: 300 * 1024 * 1024 * 1024,
                    read_bytes_per_sec: Some(120 * 1024 * 1024),
                    write_bytes_per_sec: Some(60 * 1024 * 1024),
                    busy_pct: Some(25.0),
                },
                DiskSnapshot {
                    mount: "E:\\".to_owned(),
                    label: "Scratch".to_owned(),
                    fs_type: "NTFS".to_owned(),
                    physical_drive: Some(1),
                    total_bytes: 500 * 1024 * 1024 * 1024,
                    used_bytes: 100 * 1024 * 1024 * 1024,
                    free_bytes: 400 * 1024 * 1024 * 1024,
                    read_bytes_per_sec: Some(10 * 1024 * 1024),
                    write_bytes_per_sec: Some(5 * 1024 * 1024),
                    busy_pct: Some(4.0),
                },
            ]),
            ..Snapshot::default()
        };
        let mut hist = UiHistory::default();
        hist.push(&snap);
        hist.push(&snap);

        let cfg = Config {
            shown_boxes: "mem".to_owned(),
            io_mode: true,
            io_graph_combined: true,
            disks_filter: "exclude=E".to_owned(),
            io_graph_speeds: "C:500".to_owned(),
            ..Config::default()
        };
        let text = crate::render_with_history(
            &snap,
            &hist,
            &cfg,
            Size {
                width: 72,
                height: 20,
            },
        )
        .to_plain_text();
        assert!(text.contains("combined"));
        assert!(text.contains("C:\\"));
        assert!(text.contains("500 MiB/s"));
        assert!(!text.contains("E:\\"));
    }
}
