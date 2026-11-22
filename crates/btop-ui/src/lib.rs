//! Pure UI layout and widget rendering (`fn(&Snapshot, &Config, Size) -> Grid`).

#![forbid(unsafe_code)]

use btop_config::Config;
use btop_model::{DataState, Snapshot};
use btop_term::{Color, Grid, HitAction, Rect, Size};

/// Renders a complete terminal frame into a deterministic cell `Grid`.
pub fn render(snapshot: &Snapshot, config: &Config, size: Size) -> Grid {
    let mut grid = Grid::new(size);
    if size.width < 10 || size.height < 3 {
        return grid;
    }

    let cpu_summary = match &snapshot.cpu {
        DataState::Available(cpu) => {
            format!("CPU {:>5.1}% ({})", cpu.total_busy_pct, cpu.model_name)
        }
        DataState::WarmingUp => "CPU warming up...".to_owned(),
        DataState::Stale { value, age_ms } => {
            format!("CPU {:>5.1}% (stale {}ms)", value.total_busy_pct, age_ms)
        }
        DataState::Denied => "CPU denied".to_owned(),
        DataState::Unavailable(reason) => format!("CPU unavailable: {reason}"),
    };

    let header = format!(
        "btop-windows | {} | update {}ms",
        snapshot.hostname, config.update_ms
    );
    grid.write_str(1, 0, &header, Color::Reset, Color::Reset, true);
    grid.write_str(1, 1, &cpu_summary, Color::Reset, Color::Reset, false);
    grid.hit_map.push(
        Rect {
            x: 1,
            y: 1,
            width: size.width.saturating_sub(2),
            height: 1,
        },
        HitAction::ToggleBox("cpu".to_owned()),
    );
    grid
}

#[cfg(test)]
mod tests {
    use btop_model::CpuSnapshot;

    use super::*;

    #[test]
    fn render_produces_deterministic_grid() {
        let snap = Snapshot {
            hostname: "TEST-HOST".to_owned(),
            cpu: DataState::Available(CpuSnapshot {
                model_name: "Ryzen 9".to_owned(),
                total_busy_pct: 12.5,
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
        assert_eq!(grid.get(1, 0).map(|c| c.ch), Some('b'));
        assert_eq!(
            grid.hit_map.hit_test(2, 1),
            Some(&HitAction::ToggleBox("cpu".to_owned()))
        );
    }
}
