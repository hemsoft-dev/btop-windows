//! Pure UI layout and widget rendering (`fn(&Snapshot, &UiHistory, &Config, Size) -> Grid`).

#![forbid(unsafe_code)]

pub mod cpu_box;
pub mod theme;
pub mod widgets;

use std::collections::VecDeque;

use btop_config::Config;
use btop_model::{DataState, Snapshot};
use btop_term::{Grid, Rect, Size};

use crate::theme::ThemePalette;

const DEFAULT_HISTORY_CAPACITY: usize = 512;

/// Bounded ring-buffer history passed into pure UI rendering for time-series graphs.
#[derive(Debug, Clone, PartialEq)]
pub struct UiHistory {
    capacity: usize,
    cpu_total: VecDeque<f32>,
    cpu_peak: VecDeque<f32>,
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
        }
    }

    /// Creates a single-sample history seeded from `snapshot`.
    pub fn from_snapshot(snapshot: &Snapshot) -> Self {
        let mut history = Self::default();
        history.push(snapshot);
        history
    }

    /// Appends the CPU totals from `snapshot` into the bounded history series.
    pub fn push(&mut self, snapshot: &Snapshot) {
        if let DataState::Available(cpu) | DataState::Stale { value: cpu, .. } = &snapshot.cpu {
            let total = cpu.total_busy_pct.clamp(0.0, 100.0);
            let peak = cpu
                .cores
                .iter()
                .map(|c| c.busy_pct.clamp(0.0, 100.0))
                .fold(total, f32::max);

            push_bounded(&mut self.cpu_total, total, self.capacity);
            push_bounded(&mut self.cpu_peak, peak, self.capacity);
        }
    }

    pub fn cpu_total_slice(&self) -> &[f32] {
        self.cpu_total.as_slices().0
    }

    pub fn cpu_peak_slice(&self) -> &[f32] {
        self.cpu_peak.as_slices().0
    }
}

fn push_bounded(buf: &mut VecDeque<f32>, value: f32, capacity: usize) {
    if buf.len() >= capacity {
        buf.pop_front();
    }
    buf.push_back(value);
    buf.make_contiguous();
}

/// Renders a complete terminal frame from a single snapshot into a deterministic [`Grid`].
pub fn render(snapshot: &Snapshot, config: &Config, size: Size) -> Grid {
    let history = UiHistory::from_snapshot(snapshot);
    render_with_history(snapshot, &history, config, size)
}

/// Renders a complete terminal frame with accumulated time-series history into a [`Grid`].
pub fn render_with_history(
    snapshot: &Snapshot,
    history: &UiHistory,
    config: &Config,
    size: Size,
) -> Grid {
    let mut grid = Grid::new(size);
    let theme = ThemePalette::default_dark(config.theme_background);

    if size.width < 20 || size.height < 6 {
        grid.write_str(0, 0, "Terminal too small", theme.hi_fg, theme.main_bg, true);
        return grid;
    }

    let full_rect = Rect {
        x: 0,
        y: 0,
        width: size.width,
        height: size.height,
    };
    cpu_box::render_cpu_box(&mut grid, full_rect, snapshot, history, config, &theme);
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
