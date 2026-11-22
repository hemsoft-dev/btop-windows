//! Auto-scaling network throughput panel (`3` / `net`) with `b`/`n` interface cycling,
//! `a` auto-scale toggle (10 KiB/s floor), `y` synced scale toggle, and `z` session total reset.

use std::collections::BTreeMap;

use btop_config::Config;
use btop_model::{DataState, NetInterfaceSnapshot, Snapshot};
use btop_term::{Cell, Grid, HitAction, Rect};

use crate::mem_box::format_mem_bytes;
use crate::theme::ThemePalette;
use crate::widgets::{draw_border_tag, draw_box_frame, draw_graph_with_gradient, fill_rect};
use crate::{NetIoSeries, UiHistory};

/// Minimum auto-scale ceiling in bytes/sec (10 KiB/s) per Issue #12.
pub const AUTO_SCALE_FLOOR_BPS: u64 = 10 * 1024;

/// Interactive view state for the `³net` box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetViewState {
    pub selected_index: usize,
    /// Stable `NET_LUID` of the selected adapter so enumeration reordering never switches adapters.
    pub selected_luid: Option<u64>,
    pub preferred_iface: Option<String>,
    pub auto_scale: bool,
    pub synced_scale: bool,
    /// Per-interface `(rx_total_bytes, tx_total_bytes)` session baselines keyed by `NET_LUID`.
    pub session_baselines: BTreeMap<u64, (u64, u64)>,
}

impl Default for NetViewState {
    fn default() -> Self {
        Self {
            selected_index: 0,
            selected_luid: None,
            preferred_iface: None,
            auto_scale: true,
            synced_scale: true,
            session_baselines: BTreeMap::new(),
        }
    }
}

/// Matches a `net_iface` selector against either the adapter friendly name (case-insensitive)
/// or its 64-bit `NET_LUID` in decimal, `0x`-prefixed hex, or `luid:`-prefixed form.
pub fn matches_net_iface(iface: &NetInterfaceSnapshot, selector: &str) -> bool {
    let trimmed = selector.trim();
    if trimmed.is_empty() {
        return false;
    }
    if iface.name.eq_ignore_ascii_case(trimmed) {
        return true;
    }
    parse_luid_selector(trimmed).is_some_and(|luid| iface.luid == luid)
}

fn parse_luid_selector(selector: &str) -> Option<u64> {
    let raw = if let Some(prefix) = selector.get(..5)
        && prefix.eq_ignore_ascii_case("luid:")
    {
        selector[5..].trim()
    } else {
        selector
    };
    if let Some(hex) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else {
        raw.parse::<u64>().ok()
    }
}

impl NetViewState {
    pub fn from_config(config: &Config) -> Self {
        let preferred_iface =
            (!config.net_iface.trim().is_empty()).then(|| config.net_iface.trim().to_owned());
        Self {
            selected_index: 0,
            selected_luid: None,
            preferred_iface,
            auto_scale: config.net_auto,
            synced_scale: config.net_sync,
            session_baselines: BTreeMap::new(),
        }
    }

    /// Updates initial session baselines and preserves selected adapter by `NET_LUID`.
    pub fn observe_snapshot(&mut self, snapshot: &Snapshot) {
        let Some(ifaces) = active_interfaces(snapshot) else {
            return;
        };
        for iface in ifaces {
            let entry = self
                .session_baselines
                .entry(iface.luid)
                .or_insert((iface.rx_total_bytes, iface.tx_total_bytes));
            // Re-anchor cleanly to 0 if OS counters rolled back below our baseline.
            if iface.rx_total_bytes < entry.0 {
                entry.0 = 0;
            }
            if iface.tx_total_bytes < entry.1 {
                entry.1 = 0;
            }
        }
        if ifaces.is_empty() {
            return;
        }
        if let Some(ref pref) = self.preferred_iface
            && let Some(idx) = ifaces.iter().position(|i| matches_net_iface(i, pref))
        {
            self.selected_index = idx;
            self.selected_luid = Some(ifaces[idx].luid);
            self.preferred_iface = None;
            return;
        }
        if let Some(luid) = self.selected_luid
            && let Some(idx) = ifaces.iter().position(|i| i.luid == luid)
        {
            self.selected_index = idx;
            return;
        }
        if self.selected_index >= ifaces.len() {
            self.selected_index = 0;
        }
        self.selected_luid = Some(ifaces[self.selected_index].luid);
    }

    /// Cycles to the previous hardware interface (`b`).
    pub fn select_prev(&mut self, snapshot: &Snapshot) {
        let Some(ifaces) = active_interfaces(snapshot) else {
            return;
        };
        if ifaces.is_empty() {
            return;
        }
        let (cur, _) = self.resolve_index(ifaces);
        let next = if cur == 0 { ifaces.len() - 1 } else { cur - 1 };
        self.preferred_iface = None;
        self.selected_index = next;
        self.selected_luid = Some(ifaces[next].luid);
    }

    /// Cycles to the next hardware interface (`n`).
    pub fn select_next(&mut self, snapshot: &Snapshot) {
        let Some(ifaces) = active_interfaces(snapshot) else {
            return;
        };
        if ifaces.is_empty() {
            return;
        }
        let (cur, _) = self.resolve_index(ifaces);
        let next = (cur + 1) % ifaces.len();
        self.preferred_iface = None;
        self.selected_index = next;
        self.selected_luid = Some(ifaces[next].luid);
    }

    /// Toggles auto-scaling (`a`).
    pub fn toggle_auto(&mut self) {
        self.auto_scale = !self.auto_scale;
    }

    /// Toggles synced download/upload graph ceilings (`y`).
    pub fn toggle_sync(&mut self) {
        self.synced_scale = !self.synced_scale;
    }

    /// Resets session RX/TX totals and peak speeds for the selected interface (`z`).
    pub fn reset_session_totals(&mut self, snapshot: &Snapshot, history: &mut UiHistory) {
        let Some(ifaces) = active_interfaces(snapshot) else {
            return;
        };
        if let Some((_, iface)) = self.resolve_selected(ifaces) {
            self.session_baselines
                .insert(iface.luid, (iface.rx_total_bytes, iface.tx_total_bytes));
            history.reset_net_peaks(iface.luid);
        }
    }

    /// Resolves `(index, &NetInterfaceSnapshot)` for the currently selected interface.
    pub fn resolve_selected<'a>(
        &self,
        ifaces: &'a [NetInterfaceSnapshot],
    ) -> Option<(usize, &'a NetInterfaceSnapshot)> {
        if ifaces.is_empty() {
            return None;
        }
        let (idx, _) = self.resolve_index(ifaces);
        Some((idx, &ifaces[idx]))
    }

    fn resolve_index(&self, ifaces: &[NetInterfaceSnapshot]) -> (usize, usize) {
        if let Some(ref pref) = self.preferred_iface
            && let Some(idx) = ifaces.iter().position(|i| matches_net_iface(i, pref))
        {
            return (idx, ifaces.len());
        }
        if let Some(luid) = self.selected_luid
            && let Some(idx) = ifaces.iter().position(|i| i.luid == luid)
        {
            return (idx, ifaces.len());
        }
        (self.selected_index.min(ifaces.len() - 1), ifaces.len())
    }

    /// Computes `(session_rx_bytes, session_tx_bytes)` for `iface`.
    pub fn session_totals(&self, iface: &NetInterfaceSnapshot) -> (u64, u64) {
        if let Some(&(base_rx, base_tx)) = self.session_baselines.get(&iface.luid) {
            (
                iface.rx_total_bytes.saturating_sub(base_rx),
                iface.tx_total_bytes.saturating_sub(base_tx),
            )
        } else {
            (iface.rx_total_bytes, iface.tx_total_bytes)
        }
    }
}

fn active_interfaces(snapshot: &Snapshot) -> Option<&[NetInterfaceSnapshot]> {
    match &snapshot.net {
        DataState::Available(list) | DataState::Stale { value: list, .. } => Some(list.as_slice()),
        _ => None,
    }
}

/// Formats network throughput as either base-10 bits/sec (`base_10_bitrate = true`:
/// `bps`, `Kbps`, `Mbps`, `Gbps`, `Tbps`) or binary bytes/sec (`base_10_bitrate = false`:
/// `B/s`, `KiB/s`, `MiB/s`, `GiB/s`, `TiB/s`).
#[allow(clippy::cast_precision_loss)]
pub fn format_bitrate(bytes_per_sec: u64, base_10_bitrate: bool) -> String {
    const BIT_UNITS: [&str; 5] = ["bps", "Kbps", "Mbps", "Gbps", "Tbps"];

    if !base_10_bitrate {
        return format!("{}/s", format_mem_bytes(bytes_per_sec, false));
    }

    let bits_per_sec = bytes_per_sec.saturating_mul(8);
    if bits_per_sec < 1000 {
        return format!("{bits_per_sec} bps");
    }

    let mut value = bits_per_sec as f64;
    let mut idx = 0usize;
    while value >= 1000.0 && idx + 1 < BIT_UNITS.len() {
        value /= 1000.0;
        idx += 1;
    }
    let unit = BIT_UNITS[idx];
    if value >= 100.0 {
        format!("{value:.0} {unit}")
    } else if value >= 10.0 {
        format!("{value:.1} {unit}")
    } else {
        format!("{value:.2} {unit}")
    }
}

/// Computes `(download_ceiling_bps, upload_ceiling_bps)` in bytes/sec.
///
/// - In auto-scale mode (`auto_scale = true`), each ceiling tracks the history/current peak
///   floored at [`AUTO_SCALE_FLOOR_BPS`] (10 KiB/s).
/// - In fixed-scale mode (`auto_scale = false`), ceilings convert `net_download_mbit` and
///   `net_upload_mbit` (Mbit/s) to bytes/sec (`mbit * 125_000`).
/// - When `synced_scale = true`, both ceilings take `rx_ceil.max(tx_ceil)`.
#[allow(clippy::too_many_arguments)]
pub fn compute_net_scale_ceilings(
    rx_hist: &[u64],
    tx_hist: &[u64],
    current_rx: u64,
    current_tx: u64,
    auto_scale: bool,
    synced_scale: bool,
    net_download_mbit: u64,
    net_upload_mbit: u64,
) -> (u64, u64) {
    let (rx_ceil, tx_ceil) = if auto_scale {
        let rx_peak = rx_hist
            .iter()
            .copied()
            .max()
            .unwrap_or(0)
            .max(current_rx)
            .max(AUTO_SCALE_FLOOR_BPS);
        let tx_peak = tx_hist
            .iter()
            .copied()
            .max()
            .unwrap_or(0)
            .max(current_tx)
            .max(AUTO_SCALE_FLOOR_BPS);
        (rx_peak, tx_peak)
    } else {
        (
            net_download_mbit.max(1).saturating_mul(125_000),
            net_upload_mbit.max(1).saturating_mul(125_000),
        )
    };

    if synced_scale {
        let synced = rx_ceil.max(tx_ceil);
        (synced, synced)
    } else {
        (rx_ceil, tx_ceil)
    }
}

/// Renders the `³net` box into `rect`.
pub fn render_net_box(
    grid: &mut Grid,
    rect: Rect,
    snapshot: &Snapshot,
    history: &UiHistory,
    state: &NetViewState,
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
        theme.net_box,
        theme.main_bg,
        config.rounded_corners,
    );
    render_net_chrome(grid, rect, snapshot, state, theme);

    let inner = Rect {
        x: rect.x.saturating_add(1),
        y: rect.y.saturating_add(1),
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    };

    match &snapshot.net {
        DataState::Available(list) => {
            render_net_body(grid, inner, list, history, state, config, theme, None);
        }
        DataState::Stale { value, age_ms } => {
            let note = format!("stale {age_ms}ms");
            render_net_body(
                grid,
                inner,
                value,
                history,
                state,
                config,
                theme,
                Some(&note),
            );
        }
        DataState::WarmingUp => {
            let msg: String = "Network warming up..."
                .chars()
                .take(usize::from(inner.width.saturating_sub(2)))
                .collect();
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
            let msg: String = "Network telemetry access denied"
                .chars()
                .take(usize::from(inner.width.saturating_sub(2)))
                .collect();
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
            let msg = format!("Network unavailable: {reason}");
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

fn render_net_chrome(
    grid: &mut Grid,
    rect: Rect,
    snapshot: &Snapshot,
    state: &NetViewState,
    theme: &ThemePalette,
) {
    let right_limit = rect.x.saturating_add(rect.width.saturating_sub(1));
    let mut cursor = rect.x.saturating_add(2);
    if let Some(tag_rect) = draw_border_tag(
        grid,
        cursor,
        rect.y,
        right_limit,
        Some('³'),
        "net",
        theme.net_box,
        theme.title,
        theme.hi_fg,
        theme.main_bg,
    ) {
        grid.hit_map
            .push(tag_rect, HitAction::ToggleBox("net".to_owned()));
        cursor = tag_rect.x.saturating_add(tag_rect.width).saturating_add(1);
    }

    let iface_tag = active_interfaces(snapshot)
        .and_then(|list| state.resolve_selected(list))
        .map(|(_, iface)| {
            let clipped: String = iface.name.chars().take(16).collect();
            format!("<b {clipped} n>")
        })
        .unwrap_or_default();
    let right_reserved = if iface_tag.is_empty() {
        0
    } else {
        u16::try_from(iface_tag.chars().count() + 3).unwrap_or(0)
    };
    let mid_limit = right_limit.saturating_sub(right_reserved);

    // Optional `[a]uto`, `[y]sync`, `[z]ero` mode tags only when they fit intact before interface tag.
    let tags: [(char, &str, bool); 3] = [
        ('a', "uto", state.auto_scale),
        ('y', "sync", state.synced_scale),
        ('z', "ero", false),
    ];
    for (hk, suffix, active) in tags {
        let needed = u16::try_from(suffix.chars().count() + 3).unwrap_or(8);
        if cursor.saturating_add(needed) > mid_limit {
            break;
        }
        let fg = if active {
            theme.hi_fg
        } else {
            theme.inactive_fg
        };
        if let Some(t_rect) = draw_border_tag(
            grid,
            cursor,
            rect.y,
            mid_limit,
            Some(hk),
            suffix,
            theme.net_box,
            fg,
            theme.hi_fg,
            theme.main_bg,
        ) {
            cursor = t_rect.x.saturating_add(t_rect.width).saturating_add(1);
        }
    }

    if !iface_tag.is_empty() {
        let tag_width = u16::try_from(iface_tag.chars().count() + 2).unwrap_or(0);
        if right_limit > cursor.saturating_add(tag_width) {
            let tag_x = right_limit.saturating_sub(tag_width);
            let _ = draw_border_tag(
                grid,
                tag_x,
                rect.y,
                right_limit,
                None,
                &iface_tag,
                theme.net_box,
                theme.title,
                theme.hi_fg,
                theme.main_bg,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_net_body(
    grid: &mut Grid,
    inner: Rect,
    ifaces: &[NetInterfaceSnapshot],
    history: &UiHistory,
    state: &NetViewState,
    config: &Config,
    theme: &ThemePalette,
    stale_note: Option<&str>,
) {
    let Some((_, iface)) = state.resolve_selected(ifaces) else {
        let msg: String = "No hardware network interfaces"
            .chars()
            .take(usize::from(inner.width.saturating_sub(2)))
            .collect();
        grid.write_str(
            inner.x.saturating_add(1),
            inner.y.saturating_add(1),
            &msg,
            theme.inactive_fg,
            theme.main_bg,
            false,
        );
        return;
    };

    let series = history.net_series(iface.luid);
    let rx_raw = series.map_or(&[][..], NetIoSeries::rx_slice);
    let tx_raw = series.map_or(&[][..], NetIoSeries::tx_slice);
    let (rx_ceil, tx_ceil) = compute_net_scale_ceilings(
        rx_raw,
        tx_raw,
        iface.rx_bytes_per_sec,
        iface.tx_bytes_per_sec,
        state.auto_scale,
        state.synced_scale,
        config.net_download,
        config.net_upload,
    );

    let (graph_area, stats_area) = if inner.width >= 44 {
        let stats_w =
            u16::try_from((u32::from(inner.width) * 44 / 100).clamp(21, 30)).unwrap_or(24);
        let graph_w = inner.width.saturating_sub(stats_w).saturating_sub(1);
        let div_x = inner.x.saturating_add(graph_w);
        for dy in 0..inner.height {
            grid.set(
                div_x,
                inner.y.saturating_add(dy),
                Cell {
                    ch: '│',
                    fg: theme.div_line,
                    bg: theme.main_bg,
                    bold: false,
                    continuation: false,
                },
            );
        }
        (
            Some(Rect {
                width: graph_w,
                ..inner
            }),
            Rect {
                x: div_x.saturating_add(1),
                width: stats_w,
                ..inner
            },
        )
    } else {
        let stats_h = inner.height.min(4);
        let rem_h = inner.height.saturating_sub(stats_h);
        let graph = (rem_h >= 2).then_some(Rect {
            y: inner.y.saturating_add(stats_h),
            height: rem_h,
            ..inner
        });
        (
            graph,
            Rect {
                height: stats_h,
                ..inner
            },
        )
    };

    if let Some(area) = graph_area {
        render_net_graphs(grid, area, rx_raw, tx_raw, rx_ceil, tx_ceil, config, theme);
    }
    render_net_stats(
        grid, stats_area, iface, series, state, rx_ceil, tx_ceil, stale_note, config, theme,
    );
}

#[allow(
    clippy::too_many_arguments,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation
)]
fn render_net_graphs(
    grid: &mut Grid,
    area: Rect,
    rx_raw: &[u64],
    tx_raw: &[u64],
    rx_ceil: u64,
    tx_ceil: u64,
    config: &Config,
    theme: &ThemePalette,
) {
    if area.width < 4 || area.height < 2 {
        return;
    }

    let to_pct = |vals: &[u64], ceil: u64| -> Vec<f32> {
        let denom = ceil.max(1) as f64;
        vals.iter()
            .map(|&v| ((v as f64 / denom) * 100.0).clamp(0.0, 100.0) as f32)
            .collect()
    };

    let rx_pcts = to_pct(rx_raw, rx_ceil);
    let tx_pcts = to_pct(tx_raw, tx_ceil);

    let (top_vals, top_stops, top_ceil, bot_vals, bot_stops, bot_ceil) =
        if config.swap_upload_download {
            (
                tx_pcts,
                theme.upload_stops(),
                tx_ceil,
                rx_pcts,
                theme.download_stops(),
                rx_ceil,
            )
        } else {
            (
                rx_pcts,
                theme.download_stops(),
                rx_ceil,
                tx_pcts,
                theme.upload_stops(),
                tx_ceil,
            )
        };

    let top_h = area.height.div_ceil(2);
    let bot_h = area.height.saturating_sub(top_h);

    draw_graph_with_gradient(
        grid,
        Rect {
            height: top_h,
            ..area
        },
        &top_vals,
        config.graph_symbol,
        false,
        top_stops,
        theme,
    );
    if bot_h > 0 {
        draw_graph_with_gradient(
            grid,
            Rect {
                y: area.y.saturating_add(top_h),
                height: bot_h,
                ..area
            },
            &bot_vals,
            config.graph_symbol,
            true,
            bot_stops,
            theme,
        );
    }

    // Subtle scale labels on the left margin of top & bottom graphs.
    let top_lbl: String = format_bitrate(top_ceil, config.base_10_bitrate)
        .chars()
        .take(usize::from(area.width))
        .collect();
    grid.write_str(
        area.x,
        area.y,
        &top_lbl,
        theme.inactive_fg,
        theme.main_bg,
        false,
    );
    if bot_h > 0 {
        let bot_lbl: String = format_bitrate(bot_ceil, config.base_10_bitrate)
            .chars()
            .take(usize::from(area.width))
            .collect();
        grid.write_str(
            area.x,
            area.y.saturating_add(area.height.saturating_sub(1)),
            &bot_lbl,
            theme.inactive_fg,
            theme.main_bg,
            false,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn render_net_stats(
    grid: &mut Grid,
    area: Rect,
    iface: &NetInterfaceSnapshot,
    series: Option<&NetIoSeries>,
    state: &NetViewState,
    rx_ceil: u64,
    tx_ceil: u64,
    stale_note: Option<&str>,
    config: &Config,
    theme: &ThemePalette,
) {
    if area.width < 8 || area.height == 0 {
        return;
    }

    let (sess_rx, sess_tx) = state.session_totals(iface);
    let peak_rx = series.map_or(iface.rx_bytes_per_sec, |s| {
        s.peak_rx().max(iface.rx_bytes_per_sec)
    });
    let peak_tx = series.map_or(iface.tx_bytes_per_sec, |s| {
        s.peak_tx().max(iface.tx_bytes_per_sec)
    });

    let dl_block = (
        "▼ Down",
        iface.rx_bytes_per_sec,
        peak_rx,
        sess_rx,
        theme.download_stops(),
    );
    let ul_block = (
        "▲ Up",
        iface.tx_bytes_per_sec,
        peak_tx,
        sess_tx,
        theme.upload_stops(),
    );
    let ordered = if config.swap_upload_download {
        [ul_block, dl_block]
    } else {
        [dl_block, ul_block]
    };

    let mut y = area.y;
    let y_end = area.y.saturating_add(area.height);
    let max_w = usize::from(area.width);

    for (title, live, peak, total, stops) in ordered {
        if y >= y_end {
            break;
        }
        let line = format!(
            " {title:<6} {}",
            format_bitrate(live, config.base_10_bitrate)
        );
        let clipped: String = line.chars().take(max_w).collect();
        grid.write_str(area.x, y, &clipped, stops.sample(65.0), theme.main_bg, true);
        y = y.saturating_add(1);

        if area.height >= 7 && y < y_end {
            let detail = format!("  Top: {}", format_bitrate(peak, config.base_10_bitrate));
            let clipped: String = detail.chars().take(max_w).collect();
            grid.write_str(area.x, y, &clipped, theme.main_fg, theme.main_bg, false);
            y = y.saturating_add(1);
        }

        if area.height >= 5 && y < y_end {
            let tot = format!("  Tot: {}", format_mem_bytes(total, config.base_10_sizes));
            let clipped: String = tot.chars().take(max_w).collect();
            grid.write_str(area.x, y, &clipped, theme.main_fg, theme.main_bg, false);
            y = y.saturating_add(1);
        }
    }

    if y < y_end {
        let ip4 = iface.ipv4.as_deref().unwrap_or("none");
        let line = format!(" IP4: {ip4}");
        let clipped: String = line.chars().take(max_w).collect();
        grid.write_str(area.x, y, &clipped, theme.inactive_fg, theme.main_bg, false);
        y = y.saturating_add(1);
    }

    if y < y_end
        && let Some(ip6) = iface.ipv6.as_deref()
    {
        let line = format!(" IP6: {ip6}");
        let clipped: String = line.chars().take(max_w).collect();
        grid.write_str(area.x, y, &clipped, theme.inactive_fg, theme.main_bg, false);
        y = y.saturating_add(1);
    }

    if y < y_end {
        let mode = if state.auto_scale { "auto" } else { "fixed" };
        let sync = if state.synced_scale { " sync" } else { "" };
        let stale_suffix = stale_note.map(|n| format!(" [{n}]")).unwrap_or_default();
        let ceil_str = format_bitrate(rx_ceil.max(tx_ceil), config.base_10_bitrate);
        let line = format!(" {mode}{sync} {ceil_str}{stale_suffix}");
        let clipped: String = line.chars().take(max_w).collect();
        grid.write_str(area.x, y, &clipped, theme.inactive_fg, theme.main_bg, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bitrate_handles_base_10_bits_and_binary_bytes() {
        // Base-10 bitrates (bits/sec)
        assert_eq!(format_bitrate(100, true), "800 bps");
        assert_eq!(format_bitrate(125_000, true), "1.00 Mbps");
        assert_eq!(format_bitrate(12_500_000, true), "100 Mbps");
        assert_eq!(format_bitrate(125_000_000, true), "1.00 Gbps");

        // Binary byte rates (bytes/sec)
        assert_eq!(format_bitrate(512, false), "512 B/s");
        assert_eq!(format_bitrate(1024, false), "1.00 KiB/s");
        assert_eq!(format_bitrate(10 * 1024 * 1024, false), "10.0 MiB/s");
    }

    #[test]
    fn compute_net_scale_ceilings_enforces_floor_synced_and_fixed_modes() {
        // Auto-scale floor is 10 KiB/s
        assert_eq!(
            compute_net_scale_ceilings(&[], &[], 0, 0, true, false, 100, 50),
            (AUTO_SCALE_FLOOR_BPS, AUTO_SCALE_FLOOR_BPS)
        );

        // Auto-scale unsynced tracks independent peaks
        assert_eq!(
            compute_net_scale_ceilings(
                &[500_000],
                &[100_000],
                200_000,
                50_000,
                true,
                false,
                100,
                50
            ),
            (500_000, 100_000)
        );

        // Auto-scale synced takes max(rx, tx)
        assert_eq!(
            compute_net_scale_ceilings(
                &[500_000],
                &[100_000],
                200_000,
                50_000,
                true,
                true,
                100,
                50
            ),
            (500_000, 500_000)
        );

        // Fixed scale converts Mbit/s -> bytes/sec (100 Mbit/s = 12_500_000 B/s)
        assert_eq!(
            compute_net_scale_ceilings(&[500_000], &[100_000], 0, 0, false, false, 100, 40),
            (12_500_000, 5_000_000)
        );
        assert_eq!(
            compute_net_scale_ceilings(&[500_000], &[100_000], 0, 0, false, true, 100, 40),
            (12_500_000, 12_500_000)
        );
    }

    #[test]
    fn net_view_state_cycles_interfaces_and_resets_session_totals() {
        let snap = Snapshot {
            net: DataState::Available(vec![
                NetInterfaceSnapshot {
                    luid: 1,
                    name: "Ethernet".to_owned(),
                    rx_total_bytes: 1000,
                    tx_total_bytes: 500,
                    rx_bytes_per_sec: 200,
                    tx_bytes_per_sec: 100,
                    ..NetInterfaceSnapshot::default()
                },
                NetInterfaceSnapshot {
                    luid: 2,
                    name: "Wi-Fi".to_owned(),
                    rx_total_bytes: 2000,
                    tx_total_bytes: 800,
                    ..NetInterfaceSnapshot::default()
                },
            ]),
            ..Snapshot::default()
        };

        let mut hist = UiHistory::default();
        hist.push(&snap);
        let mut state = NetViewState::default();
        state.observe_snapshot(&snap);
        assert_eq!(state.selected_index, 0);

        state.select_next(&snap);
        assert_eq!(state.selected_index, 1);
        state.select_prev(&snap);
        assert_eq!(state.selected_index, 0);

        // Advance totals then reset with `z`
        let mut snap2 = snap.clone();
        if let DataState::Available(ref mut list) = snap2.net {
            list[0].rx_total_bytes = 5000;
            list[0].tx_total_bytes = 2500;
            assert_eq!(state.session_totals(&list[0]), (4000, 2000));
        }
        state.reset_session_totals(&snap2, &mut hist);
        if let DataState::Available(ref list) = snap2.net {
            assert_eq!(state.session_totals(&list[0]), (0, 0));
        }
    }

    #[test]
    fn net_view_state_matches_luid_selector_and_preserves_selection_across_reordering() {
        let eth = NetInterfaceSnapshot {
            luid: 0x0006_0000_0100_0000,
            name: "Ethernet".to_owned(),
            is_up: true,
            ..NetInterfaceSnapshot::default()
        };
        let wifi = NetInterfaceSnapshot {
            luid: 0x0047_0000_0200_0000,
            name: "Wi-Fi".to_owned(),
            is_up: true,
            ..NetInterfaceSnapshot::default()
        };

        // Matches friendly name, hex LUID, decimal LUID, and `luid:` prefix.
        assert!(matches_net_iface(&wifi, "wi-fi"));
        assert!(matches_net_iface(&wifi, "0x0047000002000000"));
        assert!(matches_net_iface(&wifi, &wifi.luid.to_string()));
        assert!(matches_net_iface(&wifi, "luid:0x0047000002000000"));
        assert!(!matches_net_iface(&eth, "0x0047000002000000"));

        // Selecting Wi-Fi by hex LUID picks index 1 even when Ethernet is at index 0.
        let cfg = Config {
            net_iface: "0x0047000002000000".to_owned(),
            ..Config::default()
        };
        let mut state = NetViewState::from_config(&cfg);
        let snap1 = Snapshot {
            net: DataState::Available(vec![eth.clone(), wifi.clone()]),
            ..Snapshot::default()
        };
        state.observe_snapshot(&snap1);
        assert_eq!(state.selected_index, 1);
        assert_eq!(state.selected_luid, Some(wifi.luid));

        // When Ethernet goes down and enumeration reorders to [Wi-Fi, Ethernet],
        // selection stays locked to Wi-Fi by `NET_LUID`.
        let mut eth_down = eth;
        eth_down.is_up = false;
        let snap2 = Snapshot {
            net: DataState::Available(vec![wifi.clone(), eth_down]),
            ..Snapshot::default()
        };
        state.observe_snapshot(&snap2);
        assert_eq!(state.selected_index, 0);
        assert_eq!(state.selected_luid, Some(wifi.luid));
        if let DataState::Available(ref list) = snap2.net {
            let (_, selected) = state.resolve_selected(list).expect("selected adapter");
            assert_eq!(selected.name, "Wi-Fi");
        }
    }
}
