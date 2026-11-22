//! Top header clock (`clock_format` with `strftime` + `/host`, `/user`, `/uptime`),
//! preset indicator badge (`p0`..`p9`), and simple laptop battery indicator (`GetSystemPowerStatus`).

use std::fmt::Write as _;

use btop_config::{Config, DisablePresets};
use btop_model::{BatterySnapshot, DataState, Snapshot};
use btop_term::{Grid, Rect};

use crate::UiState;
use crate::cpu_box::format_uptime;
use crate::layout::{BoxId, ComputedLayout};
use crate::theme::ThemePalette;
use crate::widgets::draw_border_tag;

const WEEKDAYS_SHORT: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const WEEKDAYS_LONG: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const MONTHS_SHORT: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const MONTHS_LONG: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CivilDateTime {
    year: i64,
    month: u32,       // 1..=12
    day: u32,         // 1..=31
    day_of_year: u32, // 1..=366
    weekday: usize,   // 0 = Sun .. 6 = Sat
    hour: u32,        // 0..=23
    minute: u32,      // 0..=59
    second: u32,      // 0..=59
}

fn civil_from_unix_secs(unix_secs: i64) -> CivilDateTime {
    let days = unix_secs.div_euclid(86_400);
    let secs_of_day = u32::try_from(unix_secs.rem_euclid(86_400)).unwrap_or(0);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;

    // Howard Hinnant's civil-from-days algorithm (proleptic Gregorian calendar).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = u64::try_from(z - era * 146_097).unwrap_or(0);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = i64::try_from(yoe).unwrap_or(0) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };

    let month = u32::try_from(m).unwrap_or(1);
    let day = u32::try_from(d).unwrap_or(1);
    let is_leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
    let month_offsets: [u32; 12] = if is_leap {
        [0, 31, 60, 91, 121, 152, 182, 213, 244, 274, 305, 335]
    } else {
        [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334]
    };
    let day_of_year = month_offsets[usize::try_from(month - 1).unwrap_or(0)] + day;
    // 1970-01-01 was Thursday (index 4).
    let weekday = usize::try_from((days + 4).rem_euclid(7)).unwrap_or(0);

    CivilDateTime {
        year,
        month,
        day,
        day_of_year,
        weekday,
        hour,
        minute,
        second,
    }
}

fn sanitize_token(raw: &str) -> String {
    raw.chars().filter(|ch| !ch.is_control()).collect()
}

/// Formats `clock_format` using `strftime` tokens plus upstream `/host`, `/user`, and `/uptime`.
///
/// Uses `snapshot.timestamp_ms` shifted by `tz_offset_secs` so rendering stays 100% pure
/// and deterministic in replay and golden tests while showing local wall-clock time live.
pub fn format_clock(format: &str, snapshot: &Snapshot, tz_offset_secs: i32) -> String {
    let trimmed = format.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let base_secs = i64::try_from(snapshot.timestamp_ms / 1000).unwrap_or(0);
    let dt = civil_from_unix_secs(base_secs.saturating_add(i64::from(tz_offset_secs)));
    let uptime_secs = match &snapshot.cpu {
        DataState::Available(cpu) | DataState::Stale { value: cpu, .. } => cpu.uptime_secs,
        _ => 0,
    };

    let mut out = String::with_capacity(trimmed.len() + 16);
    let mut rest = trimmed;

    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("/host") {
            out.push_str(&sanitize_token(&snapshot.hostname));
            rest = after;
            continue;
        }
        if let Some(after) = rest.strip_prefix("/user") {
            out.push_str(&sanitize_token(&snapshot.username));
            rest = after;
            continue;
        }
        if let Some(after) = rest.strip_prefix("/uptime") {
            out.push_str(&format_uptime(uptime_secs));
            rest = after;
            continue;
        }
        if let Some(after_pct) = rest.strip_prefix('%') {
            let mut chars = after_pct.chars();
            if let Some(first) = chars.next() {
                // Handle POSIX `%E` / `%O` alternative-format modifiers cleanly.
                let (spec, next_rest) = if matches!(first, 'E' | 'O') {
                    let mut mod_chars = chars.clone();
                    if let Some(modified) = mod_chars.next() {
                        (modified, mod_chars.as_str())
                    } else {
                        (first, chars.as_str())
                    }
                } else {
                    (first, chars.as_str())
                };
                append_strftime_token(&mut out, spec, dt, base_secs, tz_offset_secs);
                rest = next_rest;
                continue;
            }
            out.push('%');
            break;
        }

        let mut chars = rest.chars();
        if let Some(ch) = chars.next() {
            if !ch.is_control() {
                out.push(ch);
            }
            rest = chars.as_str();
        }
    }

    out
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

/// Computes ISO 8601 `(iso_year, iso_week)` (`1..=53`) for `dt`.
fn iso_week_date(dt: CivilDateTime) -> (i64, u32) {
    let iso_wd = if dt.weekday == 0 {
        7
    } else {
        u32::try_from(dt.weekday).unwrap_or(1)
    };
    let doy_i32 = i32::try_from(dt.day_of_year).unwrap_or(1);
    let wd_i32 = i32::try_from(iso_wd).unwrap_or(1);
    let raw_week = (doy_i32 - wd_i32 + 10) / 7;

    if raw_week < 1 {
        let prev_year = dt.year - 1;
        let dec31 = civil_from_unix_secs(days_from_civil(prev_year, 12, 31) * 86_400);
        return iso_week_date(dec31);
    }
    let max_weeks = iso_weeks_in_year(dt.year);
    if raw_week > i32::try_from(max_weeks).unwrap_or(52) {
        return (dt.year + 1, 1);
    }
    (dt.year, u32::try_from(raw_week).unwrap_or(1))
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(month);
    let d = i64::from(day);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn iso_weeks_in_year(year: i64) -> u32 {
    let jan1_days = days_from_civil(year, 1, 1);
    let jan1_wd = (jan1_days + 4).rem_euclid(7); // 0 = Sun, 4 = Thu
    if jan1_wd == 4 || (jan1_wd == 3 && is_leap_year(year)) {
        53
    } else {
        52
    }
}

fn append_tz_offset(out: &mut String, tz_offset_secs: i32, with_prefix_colon: bool) {
    let sign = if tz_offset_secs < 0 { '-' } else { '+' };
    let abs_mins = tz_offset_secs.unsigned_abs() / 60;
    let hours = abs_mins / 60;
    let mins = abs_mins % 60;
    if with_prefix_colon {
        if tz_offset_secs == 0 {
            out.push_str("UTC");
        } else {
            let _ = write!(out, "UTC{sign}{hours:02}:{mins:02}");
        }
    } else {
        let _ = write!(out, "{sign}{hours:02}{mins:02}");
    }
}

#[allow(clippy::too_many_lines)]
fn append_strftime_token(
    out: &mut String,
    spec: char,
    dt: CivilDateTime,
    base_unix_secs: i64,
    tz_offset_secs: i32,
) {
    let hour_12 = match dt.hour % 12 {
        0 => 12,
        h => h,
    };
    let am_pm = if dt.hour < 12 { "AM" } else { "PM" };
    let am_pm_lower = if dt.hour < 12 { "am" } else { "pm" };
    let month_idx = usize::try_from(dt.month.saturating_sub(1))
        .unwrap_or(0)
        .min(11);
    let iso_weekday = if dt.weekday == 0 { 7 } else { dt.weekday };
    let sun_week = (dt.day_of_year + 6 - u32::try_from(dt.weekday).unwrap_or(0)) / 7;
    let mon_wd = u32::try_from((dt.weekday + 6) % 7).unwrap_or(0);
    let mon_week = (dt.day_of_year + 6 - mon_wd) / 7;

    match spec {
        '%' => out.push('%'),
        'n' | 't' => out.push(' '),
        'X' | 'T' => {
            let _ = write!(out, "{:02}:{:02}:{:02}", dt.hour, dt.minute, dt.second);
        }
        'r' => {
            let _ = write!(
                out,
                "{hour_12:02}:{:02}:{:02} {am_pm}",
                dt.minute, dt.second
            );
        }
        'R' => {
            let _ = write!(out, "{:02}:{:02}", dt.hour, dt.minute);
        }
        'H' => {
            let _ = write!(out, "{:02}", dt.hour);
        }
        'k' => {
            let _ = write!(out, "{:>2}", dt.hour);
        }
        'I' => {
            let _ = write!(out, "{hour_12:02}");
        }
        'l' => {
            let _ = write!(out, "{hour_12:>2}");
        }
        'M' => {
            let _ = write!(out, "{:02}", dt.minute);
        }
        'S' => {
            let _ = write!(out, "{:02}", dt.second);
        }
        'p' => out.push_str(am_pm),
        'P' => out.push_str(am_pm_lower),
        'C' => {
            let _ = write!(out, "{:02}", dt.year.div_euclid(100));
        }
        'Y' => {
            let _ = write!(out, "{:04}", dt.year);
        }
        'y' => {
            let _ = write!(out, "{:02}", dt.year.rem_euclid(100));
        }
        'm' => {
            let _ = write!(out, "{:02}", dt.month);
        }
        'd' => {
            let _ = write!(out, "{:02}", dt.day);
        }
        'e' => {
            let _ = write!(out, "{:>2}", dt.day);
        }
        'F' => {
            let _ = write!(out, "{:04}-{:02}-{:02}", dt.year, dt.month, dt.day);
        }
        'D' => {
            let _ = write!(
                out,
                "{:02}/{:02}/{:02}",
                dt.month,
                dt.day,
                dt.year.rem_euclid(100)
            );
        }
        'x' => {
            let _ = write!(out, "{:02}/{:02}/{:04}", dt.month, dt.day, dt.year);
        }
        'a' => out.push_str(WEEKDAYS_SHORT[dt.weekday]),
        'A' => out.push_str(WEEKDAYS_LONG[dt.weekday]),
        'b' | 'h' => out.push_str(MONTHS_SHORT[month_idx]),
        'B' => out.push_str(MONTHS_LONG[month_idx]),
        'j' => {
            let _ = write!(out, "{:03}", dt.day_of_year);
        }
        'U' => {
            let _ = write!(out, "{sun_week:02}");
        }
        'W' => {
            let _ = write!(out, "{mon_week:02}");
        }
        'V' => {
            let (_, iso_week) = iso_week_date(dt);
            let _ = write!(out, "{iso_week:02}");
        }
        'G' => {
            let (iso_year, _) = iso_week_date(dt);
            let _ = write!(out, "{iso_year:04}");
        }
        'g' => {
            let (iso_year, _) = iso_week_date(dt);
            let _ = write!(out, "{:02}", iso_year.rem_euclid(100));
        }
        'u' => {
            let _ = write!(out, "{iso_weekday}");
        }
        'w' => {
            let _ = write!(out, "{}", dt.weekday);
        }
        's' => {
            let _ = write!(out, "{base_unix_secs}");
        }
        'z' => append_tz_offset(out, tz_offset_secs, false),
        'Z' => append_tz_offset(out, tz_offset_secs, true),
        'c' => {
            let _ = write!(
                out,
                "{} {} {:>2} {:02}:{:02}:{:02} {:04}",
                WEEKDAYS_SHORT[dt.weekday],
                MONTHS_SHORT[month_idx],
                dt.day,
                dt.hour,
                dt.minute,
                dt.second,
                dt.year
            );
        }
        other => {
            out.push('%');
            out.push(other);
        }
    }
}

/// Formats battery indicator text (`BAT 82% - 1h30m`, `BAT 95% +`, `BAT 100% AC`),
/// returning `None` when disabled or when no battery is present.
pub fn format_battery(battery: &DataState<BatterySnapshot>, show_battery: bool) -> Option<String> {
    if !show_battery {
        return None;
    }
    let bat = match battery {
        DataState::Available(b) | DataState::Stale { value: b, .. } => b,
        DataState::WarmingUp | DataState::Denied | DataState::Unavailable(_) => return None,
    };

    let status_tag = match bat.status.as_str() {
        "charging" => "+",
        "discharging" => "-",
        "full" | "ac" => "AC",
        _ => "",
    };

    let mut out = if status_tag.is_empty() {
        format!("BAT {}%", bat.charge_pct)
    } else {
        format!("BAT {}% {status_tag}", bat.charge_pct)
    };

    if let Some(secs) = bat.remaining_secs
        && secs > 0
        && bat.status != "full"
    {
        let hours = secs / 3600;
        let mins = (secs % 3600) / 60;
        let _ = write!(out, " {hours}h{mins:02}m");
    }

    Some(out)
}

/// Returns all maximal contiguous spans `(start_x, end_x)` of untouched horizontal border
/// glyphs (`'─'`) on row `0` strictly inside `rect`'s corners.
fn free_top_border_spans(grid: &Grid, rect: Rect) -> Vec<(u16, u16)> {
    if rect.width < 4 {
        return Vec::new();
    }
    let start = rect.x.saturating_add(1);
    let end = rect.x.saturating_add(rect.width - 1);
    let mut spans = Vec::new();
    let mut cur_start: Option<u16> = None;

    for x in start..end {
        let is_free = grid.get(x, 0).is_some_and(|c| c.ch == '─');
        match (cur_start, is_free) {
            (None, true) => cur_start = Some(x),
            (Some(s), false) => {
                spans.push((s, x));
                cur_start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = cur_start {
        spans.push((s, end));
    }
    spans
}

fn render_preset_badge(
    grid: &mut Grid,
    left_id: BoxId,
    left_rect: Rect,
    state: &UiState,
    theme: &ThemePalette,
) {
    if state.layout.disable_presets == DisablePresets::All || left_rect.width < 28 {
        return;
    }
    let label = state.layout.preset_label();
    let tag_w = u16::try_from(label.chars().count() + 2).unwrap_or(4);
    let min_span = tag_w.saturating_add(2);
    let spans = free_top_border_spans(grid, left_rect);
    let Some((span_start, span_end)) = spans
        .into_iter()
        .find(|&(s, e)| e.saturating_sub(s) >= min_span)
    else {
        return;
    };
    let preset_x = span_start.saturating_add(1);
    let preset_max = preset_x
        .saturating_add(tag_w)
        .min(span_end.saturating_sub(1));
    let _ = draw_border_tag(
        grid,
        preset_x,
        0,
        preset_max,
        None,
        &label,
        box_border_color(left_id, theme),
        theme.hi_fg,
        theme.hi_fg,
        theme.main_bg,
    );
}

/// Renders top header elements on row `0`: preset badge (`p0`), centered clock (`clock_format`),
/// and optional battery indicator (`BAT ...`), placed strictly inside untouched top-border spans
/// so they never overwrite panel titles or right-edge tags.
pub fn render_top_header(
    grid: &mut Grid,
    layout: &ComputedLayout,
    snapshot: &Snapshot,
    state: &UiState,
    config: &Config,
    theme: &ThemePalette,
) {
    let mut top_boxes: Vec<(BoxId, Rect)> = layout
        .rects
        .iter()
        .filter_map(|(&id, &rect)| (rect.y == 0).then_some((id, rect)))
        .collect();
    if top_boxes.is_empty() {
        return;
    }
    top_boxes.sort_by_key(|(_, rect)| rect.x);

    let (left_id, left_rect) = top_boxes[0];
    render_preset_badge(grid, left_id, left_rect, state, theme);

    // Choose the widest top-row box as the host for the centered clock and optional battery.
    let (host_id, host_rect) = top_boxes
        .iter()
        .copied()
        .max_by_key(|(_, r)| r.width)
        .unwrap_or(top_boxes[0]);

    let border_fg = box_border_color(host_id, theme);
    let clock_text = format_clock(&config.clock_format, snapshot, state.tz_offset_secs);

    if !clock_text.is_empty()
        && let Some((span_start, span_end)) = free_top_border_spans(grid, host_rect)
            .into_iter()
            .max_by_key(|&(s, e)| e.saturating_sub(s))
    {
        let usable_start = span_start.saturating_add(1);
        let usable_end = span_end.saturating_sub(1);
        if usable_end > usable_start.saturating_add(4) {
            let span = usable_end.saturating_sub(usable_start);
            let desired_w = u16::try_from(clock_text.chars().count() + 2).unwrap_or(span);
            let actual_w = desired_w.min(span);
            let ideal_x = host_rect
                .x
                .saturating_add(host_rect.width.saturating_sub(actual_w) / 2);
            let clock_x = ideal_x.clamp(usable_start, usable_end.saturating_sub(actual_w));
            let max_x = clock_x.saturating_add(actual_w).min(usable_end);

            let _ = draw_border_tag(
                grid,
                clock_x,
                0,
                max_x,
                None,
                &clock_text,
                border_fg,
                theme.title,
                theme.hi_fg,
                theme.main_bg,
            );
        }
    }

    if let Some(bat_text) = format_battery(&snapshot.battery, config.show_battery) {
        let bat_w = u16::try_from(bat_text.chars().count() + 2).unwrap_or(14);
        let min_span = bat_w.saturating_add(2);
        // Rightmost free span on the host box's top border after clock placement.
        if let Some((_, span_end)) = free_top_border_spans(grid, host_rect)
            .into_iter()
            .rev()
            .find(|&(s, e)| e.saturating_sub(s) >= min_span)
        {
            let usable_end = span_end.saturating_sub(1);
            let bat_x = usable_end.saturating_sub(bat_w);
            let bat_fg = battery_color(&snapshot.battery, theme);
            let _ = draw_border_tag(
                grid,
                bat_x,
                0,
                usable_end,
                None,
                &bat_text,
                border_fg,
                bat_fg,
                theme.hi_fg,
                theme.main_bg,
            );
        }
    }
}

fn box_border_color(id: BoxId, theme: &ThemePalette) -> btop_term::Color {
    match id {
        BoxId::Cpu | BoxId::Gpu(_) => theme.cpu_box,
        BoxId::Mem => theme.mem_box,
        BoxId::Net => theme.net_box,
        BoxId::Proc => theme.proc_box,
    }
}

fn battery_color(battery: &DataState<BatterySnapshot>, theme: &ThemePalette) -> btop_term::Color {
    let pct = match battery {
        DataState::Available(b) | DataState::Stale { value: b, .. } => f32::from(b.charge_pct),
        _ => 100.0,
    };
    // High battery -> green (low CPU gradient end), low battery -> red (high CPU gradient end).
    theme.cpu_gradient(100.0 - pct)
}

#[cfg(test)]
mod tests {
    use btop_model::CpuSnapshot;

    use super::*;

    fn sample_snapshot() -> Snapshot {
        // 2026-11-22 19:30:45 UTC (Sunday)
        let ts_secs: u64 = 1_795_375_845;
        Snapshot {
            timestamp_ms: ts_secs * 1000,
            hostname: "WIN11-RIG".to_owned(),
            username: "franz".to_owned(),
            cpu: DataState::Available(CpuSnapshot {
                uptime_secs: 90_061, // 1d 01:01:01
                ..CpuSnapshot::default()
            }),
            ..Snapshot::default()
        }
    }

    #[test]
    fn clock_formats_strftime_and_custom_tokens() {
        let snap = sample_snapshot();
        assert_eq!(format_clock("%X", &snap, 0), "19:30:45");
        assert_eq!(format_clock("%X", &snap, -5 * 3600), "14:30:45");
        assert_eq!(
            format_clock("%F %I:%M %p %a %b", &snap, 0),
            "2026-11-22 07:30 PM Sun Nov"
        );
        assert_eq!(
            format_clock("/host /user up /uptime %%", &snap, 0),
            "WIN11-RIG franz up 1d 01:01:01 %"
        );
        assert_eq!(
            format_clock("%r %D %C %U %W %G-W%V %z %Z", &snap, -5 * 3600),
            "02:30:45 PM 11/22/26 20 47 46 2026-W47 -0500 UTC-05:00"
        );
        assert_eq!(format_clock("%k:%M %l%P %Z", &snap, 0), "19:30  7pm UTC");
        assert_eq!(format_clock("%s", &snap, 0), "1795375845");
        assert_eq!(format_clock("", &snap, 0), "");
    }

    #[test]
    fn civil_date_handles_epoch_and_leap_day() {
        let epoch = civil_from_unix_secs(0);
        assert_eq!(
            (epoch.year, epoch.month, epoch.day, epoch.weekday),
            (1970, 1, 1, 4)
        );

        // 2000-02-29 12:00:00 UTC (Tuesday = 2)
        let leap = civil_from_unix_secs(951_825_600);
        assert_eq!(
            (leap.year, leap.month, leap.day, leap.weekday),
            (2000, 2, 29, 2)
        );
    }

    #[test]
    fn battery_formatting_hides_when_absent_or_disabled() {
        let absent = DataState::Unavailable("no battery".to_owned());
        assert_eq!(format_battery(&absent, true), None);

        let discharging = DataState::Available(BatterySnapshot {
            charge_pct: 64,
            status: "discharging".to_owned(),
            watts: None,
            remaining_secs: Some(5400),
        });
        assert_eq!(
            format_battery(&discharging, true).as_deref(),
            Some("BAT 64% - 1h30m")
        );
        assert_eq!(format_battery(&discharging, false), None);

        let full = DataState::Available(BatterySnapshot {
            charge_pct: 100,
            status: "full".to_owned(),
            watts: None,
            remaining_secs: None,
        });
        assert_eq!(format_battery(&full, true).as_deref(), Some("BAT 100% AC"));
    }
}
