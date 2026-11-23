//! Deterministic `200x60` frame render + VT diff latency budget checks against the
//! representative 16-core dual-GPU replay fixture (`< 2.0 ms` p95 in release builds).

use std::fs::File;
use std::hint::black_box;
use std::io::BufReader;
use std::path::PathBuf;
use std::time::Instant;

use btop_config::Config;
use btop_model::parse_jsonl_reader;
use btop_term::{DiffEncoder, Grid, Size};
use btop_ui::{UiHistory, UiState, render_frame};

const BUDGET_P95_MICROS: u128 = 2_000;
const WARMUP_FRAMES: usize = 20;
const MEASURED_FRAMES: usize = 200;

fn representative_fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/win11-16core-dual-gpu-desktop.jsonl")
}

/// Computes the nearest-rank `p`-th percentile (`1..=100`) from a non-empty slice of microsecond timings.
fn percentile_micros(samples: &[u128], p: usize) -> u128 {
    assert!(!samples.is_empty(), "samples must not be empty");
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let rank = (sorted.len() * p).div_ceil(100);
    let idx = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[idx]
}

#[test]
fn percentile_and_representative_fixture_sanity() {
    let vals: Vec<u128> = (1..=100).collect();
    assert_eq!(percentile_micros(&vals, 50), 50);
    assert_eq!(percentile_micros(&vals, 95), 95);
    assert_eq!(percentile_micros(&vals, 99), 99);

    let file = File::open(representative_fixture_path()).expect("open representative fixture");
    let snapshots = parse_jsonl_reader(BufReader::new(file)).expect("parse representative fixture");
    assert!(
        snapshots.len() >= 2,
        "representative fixture should contain multiple ticks, got {}",
        snapshots.len()
    );
}

#[test]
#[ignore = "run with `cargo test --release -p btop-ui --test perf_budget -- --ignored --nocapture`"]
fn render_and_diff_200x60_p95_within_2ms_budget() {
    let file = File::open(representative_fixture_path()).expect("open representative fixture");
    let snapshots = parse_jsonl_reader(BufReader::new(file)).expect("parse representative fixture");
    assert!(!snapshots.is_empty(), "fixture must not be empty");

    let config = Config::default();
    let size = Size {
        width: 200,
        height: 60,
    };
    let diff = DiffEncoder { sync_output: true };
    let mut history = UiHistory::default();
    let mut state = UiState::from_config(&config);
    let mut prev_grid: Option<Grid> = None;

    for i in 0..WARMUP_FRAMES {
        let snap = &snapshots[i % snapshots.len()];
        history.push(snap);
        state.net.observe_snapshot(snap);
        let grid = render_frame(snap, &history, &state, &config, size);
        let vt_frame = diff.encode(prev_grid.as_ref(), &grid);
        black_box(&vt_frame);
        prev_grid = Some(grid);
    }

    let mut timings_us = Vec::with_capacity(MEASURED_FRAMES);
    for i in 0..MEASURED_FRAMES {
        let snap = &snapshots[(WARMUP_FRAMES + i) % snapshots.len()];
        history.push(snap);
        state.net.observe_snapshot(snap);

        let start = Instant::now();
        let grid = render_frame(snap, &history, &state, &config, size);
        let vt_frame = diff.encode(prev_grid.as_ref(), &grid);
        black_box(&vt_frame);
        let elapsed_us = start.elapsed().as_micros();

        timings_us.push(elapsed_us);
        prev_grid = Some(grid);
    }

    let p50 = percentile_micros(&timings_us, 50);
    let p95 = percentile_micros(&timings_us, 95);
    let p99 = percentile_micros(&timings_us, 99);
    let max = timings_us.iter().copied().max().unwrap_or(0);

    #[allow(clippy::cast_precision_loss)]
    {
        println!(
            "[perf-budget] 200x60 render+diff ({MEASURED_FRAMES} frames): p50={:.3} ms, p95={:.3} ms, p99={:.3} ms, max={:.3} ms (budget < {:.3} ms)",
            p50 as f64 / 1000.0,
            p95 as f64 / 1000.0,
            p99 as f64 / 1000.0,
            max as f64 / 1000.0,
            BUDGET_P95_MICROS as f64 / 1000.0,
        );
    }

    assert!(
        p95 < BUDGET_P95_MICROS,
        "200x60 render+diff p95 ({p95} us) exceeded 2.0 ms budget ({BUDGET_P95_MICROS} us)"
    );
}
