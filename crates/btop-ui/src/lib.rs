//! Pure UI layout and widget rendering (`fn(&Snapshot, &UiHistory, &UiState, &Config, Size) -> Grid`).

#![forbid(unsafe_code)]

pub mod cpu_box;
pub mod mem_box;
pub mod proc_box;
pub mod theme;
pub mod widgets;

use std::collections::VecDeque;

use btop_config::Config;
use btop_model::{DataState, Snapshot};
use btop_term::{Grid, Rect, Size};

pub use mem_box::{format_mem_bytes, stacked_physical_segments};
pub use proc_box::{ProcSort, ProcViewState};

use crate::mem_box::ratio_percent;
use crate::theme::ThemePalette;

const DEFAULT_HISTORY_CAPACITY: usize = 512;

/// Pure UI interaction state passed into [`render_frame`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UiState {
    pub proc: ProcViewState,
}

impl UiState {
    pub fn from_config(config: &Config) -> Self {
        Self {
            proc: ProcViewState::from_config(config),
        }
    }
}

/// Bounded ring-buffer history passed into pure UI rendering for time-series graphs.
#[derive(Debug, Clone, PartialEq)]
pub struct UiHistory {
    capacity: usize,
    cpu_total: VecDeque<f32>,
    cpu_peak: VecDeque<f32>,
    mem_used: VecDeque<f32>,
    mem_available: VecDeque<f32>,
    mem_cached: VecDeque<f32>,
    mem_free: VecDeque<f32>,
}

impl Default for UiHistory {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_HISTORY_CAPACITY)
    }
}

impl UiHistory {
    pub fn with_capacity(capacity: usize) -> Self {
        let cap = capacity.max(1);
        Self {
            capacity: cap,
            cpu_total: VecDeque::with_capacity(cap),
            cpu_peak: VecDeque::with_capacity(cap),
            mem_used: VecDeque::with_capacity(cap),
            mem_available: VecDeque::with_capacity(cap),
            mem_cached: VecDeque::with_capacity(cap),
            mem_free: VecDeque::with_capacity(cap),
        }
    }

    /// Creates a single-sample history seeded from `snapshot`.
    pub fn from_snapshot(snapshot: &Snapshot) -> Self {
        let mut history = Self::default();
        history.push(snapshot);
        history
    }

    /// Creates a single-sample history seeded from `snapshot` using `config.cpu_util_mode`.
    pub fn from_snapshot_with_config(snapshot: &Snapshot, config: &Config) -> Self {
        let mut history = Self::default();
        history.push_with_util_mode(
            snapshot,
            config.cpu_util_mode.eq_ignore_ascii_case("utility"),
        );
        history
    }

    /// Appends the CPU and memory totals from `snapshot` into the bounded history series.
    pub fn push(&mut self, snapshot: &Snapshot) {
        self.push_with_util_mode(snapshot, false);
    }

    /// Appends CPU totals (time-based or `% Processor Utility`) and memory percentages.
    pub fn push_with_util_mode(&mut self, snapshot: &Snapshot, use_utility: bool) {
        if let DataState::Available(cpu) | DataState::Stale { value: cpu, .. } = &snapshot.cpu {
            let total = if use_utility {
                cpu.utility_pct.unwrap_or(cpu.total_busy_pct)
            } else {
                cpu.total_busy_pct
            }
            .clamp(0.0, 100.0);
            let peak = cpu
                .cores
                .iter()
                .map(|c| c.busy_pct.clamp(0.0, 100.0))
                .fold(total, f32::max);

            push_bounded(&mut self.cpu_total, total, self.capacity);
            push_bounded(&mut self.cpu_peak, peak, self.capacity);
        }

        if let DataState::Available(mem) | DataState::Stale { value: mem, .. } = &snapshot.mem {
            push_bounded(
                &mut self.mem_used,
                ratio_percent(mem.used_bytes, mem.total_bytes),
                self.capacity,
            );
            push_bounded(
                &mut self.mem_available,
                ratio_percent(mem.available_bytes, mem.total_bytes),
                self.capacity,
            );
            push_bounded(
                &mut self.mem_cached,
                ratio_percent(mem.cached_bytes, mem.total_bytes),
                self.capacity,
            );
            push_bounded(
                &mut self.mem_free,
                ratio_percent(mem.free_bytes, mem.total_bytes),
                self.capacity,
            );
        }
    }

    pub fn cpu_total_slice(&self) -> &[f32] {
        self.cpu_total.as_slices().0
    }

    pub fn cpu_peak_slice(&self) -> &[f32] {
        self.cpu_peak.as_slices().0
    }

    pub fn mem_used_slice(&self) -> &[f32] {
        self.mem_used.as_slices().0
    }

    pub fn mem_available_slice(&self) -> &[f32] {
        self.mem_available.as_slices().0
    }

    pub fn mem_cached_slice(&self) -> &[f32] {
        self.mem_cached.as_slices().0
    }

    pub fn mem_free_slice(&self) -> &[f32] {
        self.mem_free.as_slices().0
    }
}

fn push_bounded(buf: &mut VecDeque<f32>, value: f32, capacity: usize) {
    if buf.len() >= capacity {
        buf.pop_front();
    }
    buf.push_back(value);
    buf.make_contiguous();
}

/// Computes the number of visible process rows for a given viewport size and config.
pub fn visible_proc_rows(size: Size, config: &Config) -> usize {
    let (_, _, proc_rect) = compute_layout(size, config);
    proc_rect.map_or(0, |r| usize::from(r.height.saturating_sub(3)))
}

fn split_top_bottom(area: Rect) -> (Rect, Rect) {
    let top_h = ((area.height * 2) / 5).clamp(8, 22);
    let bottom_h = area.height.saturating_sub(top_h);
    (
        Rect {
            height: top_h,
            ..area
        },
        Rect {
            y: area.y.saturating_add(top_h),
            height: bottom_h,
            ..area
        },
    )
}

fn split_mem_proc(area: Rect) -> (Rect, Rect) {
    let mem_w = ((area.width * 42) / 100).clamp(32, 80);
    let proc_w = area.width.saturating_sub(mem_w);
    (
        Rect {
            width: mem_w,
            ..area
        },
        Rect {
            x: area.x.saturating_add(mem_w),
            width: proc_w,
            ..area
        },
    )
}

fn compute_layout(size: Size, config: &Config) -> (Option<Rect>, Option<Rect>, Option<Rect>) {
    let boxes: Vec<&str> = config.shown_boxes.split_whitespace().collect();
    let show_cpu = boxes.contains(&"cpu") || boxes.is_empty();
    let show_mem = boxes.contains(&"mem");
    let show_proc = boxes.contains(&"proc");

    let full = Rect {
        x: 0,
        y: 0,
        width: size.width,
        height: size.height,
    };

    match (show_cpu, show_mem, show_proc) {
        (true, true, true) if size.height >= 14 => {
            let (cpu_rect, bottom) = split_top_bottom(full);
            if size.width >= 68 {
                let (mem_rect, proc_rect) = split_mem_proc(bottom);
                (Some(cpu_rect), Some(mem_rect), Some(proc_rect))
            } else {
                // Narrow viewport fallback: keep CPU + proc readable rather than squeezing columns.
                (Some(cpu_rect), None, Some(bottom))
            }
        }
        (true, true, false) if size.height >= 14 => {
            let (cpu_rect, mem_rect) = split_top_bottom(full);
            (Some(cpu_rect), Some(mem_rect), None)
        }
        (true, false, true) if size.height >= 14 => {
            let (cpu_rect, proc_rect) = split_top_bottom(full);
            (Some(cpu_rect), None, Some(proc_rect))
        }
        (false, true, true) if size.width >= 68 => {
            let (mem_rect, proc_rect) = split_mem_proc(full);
            (None, Some(mem_rect), Some(proc_rect))
        }
        (false, true, false) => (None, Some(full), None),
        (false, _, true) => (None, None, Some(full)),
        _ => (Some(full), None, None),
    }
}

/// Renders a complete terminal frame from a single snapshot into a deterministic [`Grid`].
pub fn render(snapshot: &Snapshot, config: &Config, size: Size) -> Grid {
    let history = UiHistory::from_snapshot_with_config(snapshot, config);
    let state = UiState::from_config(config);
    render_frame(snapshot, &history, &state, config, size)
}

/// Renders a complete terminal frame with accumulated time-series history into a [`Grid`].
pub fn render_with_history(
    snapshot: &Snapshot,
    history: &UiHistory,
    config: &Config,
    size: Size,
) -> Grid {
    let state = UiState::from_config(config);
    render_frame(snapshot, history, &state, config, size)
}

/// Renders a complete terminal frame with history and interactive UI state into a [`Grid`].
pub fn render_frame(
    snapshot: &Snapshot,
    history: &UiHistory,
    state: &UiState,
    config: &Config,
    size: Size,
) -> Grid {
    let mut grid = Grid::new(size);
    let theme = ThemePalette::default_dark(config.theme_background);

    if size.width < 24 || size.height < 6 {
        grid.write_str(0, 0, "Terminal too small", theme.hi_fg, theme.main_bg, true);
        return grid;
    }

    let (cpu_rect, mem_rect, proc_rect) = compute_layout(size, config);
    if let Some(rect) = cpu_rect {
        cpu_box::render_cpu_box(&mut grid, rect, snapshot, history, config, &theme);
    }
    if let Some(rect) = mem_rect {
        mem_box::render_mem_box(&mut grid, rect, snapshot, history, config, &theme);
    }
    if let Some(rect) = proc_rect {
        proc_box::render_proc_box(&mut grid, rect, snapshot, &state.proc, config, &theme);
    }
    grid
}

#[cfg(test)]
mod tests {
    use btop_model::CpuSnapshot;
    use btop_term::HitAction;

    use super::*;

    #[test]
    fn render_produces_deterministic_grid_and_cpu_hit_region() {
        let snap = Snapshot {
            hostname: "TEST-HOST".to_owned(),
            cpu: DataState::Available(CpuSnapshot {
                model_name: "Ryzen 9".to_owned(),
                total_busy_pct: 12.5,
                uptime_secs: 3600,
                ..CpuSnapshot::default()
            }),
            ..Snapshot::default()
        };
        let grid = render(
            &snap,
            &Config::default(),
            Size {
                width: 80,
                height: 24,
            },
        );
        assert_eq!(grid.get(0, 0).map(|c| c.ch), Some('╭'));
        assert_eq!(
            grid.hit_map.hit_test(3, 0),
            Some(&HitAction::ToggleBox("cpu".to_owned()))
        );
    }

    #[test]
    fn history_keeps_slices_contiguous_and_bounded() {
        let mut hist = UiHistory::with_capacity(3);
        for pct in [10.0, 20.0, 30.0, 40.0] {
            let snap = Snapshot {
                cpu: DataState::Available(CpuSnapshot {
                    total_busy_pct: pct,
                    ..CpuSnapshot::default()
                }),
                ..Snapshot::default()
            };
            hist.push(&snap);
        }
        assert_eq!(hist.cpu_total_slice(), &[20.0, 30.0, 40.0]);
    }
}
