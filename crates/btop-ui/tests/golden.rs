use std::path::Path;

use btop_config::{Config, GraphSymbol};
use btop_model::parse_jsonl_file;
use btop_term::Size;
use btop_ui::UiHistory;

fn fixture_path(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

fn render_fixture_with_symbol(
    fixture: &str,
    symbol: GraphSymbol,
    width: u16,
    height: u16,
) -> String {
    let snapshots = parse_jsonl_file(&fixture_path(fixture)).expect("load fixture");
    let mut history = UiHistory::default();
    // Seed multiple wave cycles so Braille and Block graphs show rich deterministic shapes.
    for _ in 0..8 {
        for snap in &snapshots {
            history.push(snap);
        }
    }
    let config = Config {
        graph_symbol: symbol,
        ..Config::default()
    };
    let latest = snapshots.last().expect("non-empty fixture");
    btop_ui::render_with_history(latest, &history, &config, Size { width, height }).to_plain_text()
}

#[test]
fn golden_cpu_braille_80x24() {
    let rendered = render_fixture_with_symbol(
        "win11-8core-single-gpu-desktop.jsonl",
        GraphSymbol::Braille,
        80,
        24,
    );
    insta::assert_snapshot!("cpu_braille_80x24", rendered);
}

#[test]
fn golden_cpu_braille_120x40() {
    let rendered = render_fixture_with_symbol(
        "win11-16core-dual-gpu-desktop.jsonl",
        GraphSymbol::Braille,
        120,
        40,
    );
    insta::assert_snapshot!("cpu_braille_120x40", rendered);
}

#[test]
fn golden_cpu_braille_200x60() {
    let rendered = render_fixture_with_symbol(
        "win11-16core-dual-gpu-desktop.jsonl",
        GraphSymbol::Braille,
        200,
        60,
    );
    insta::assert_snapshot!("cpu_braille_200x60", rendered);
}

#[test]
fn golden_cpu_block_80x24() {
    let rendered = render_fixture_with_symbol(
        "win11-8core-single-gpu-desktop.jsonl",
        GraphSymbol::Block,
        80,
        24,
    );
    insta::assert_snapshot!("cpu_block_80x24", rendered);
}

#[test]
fn golden_cpu_block_120x40() {
    let rendered = render_fixture_with_symbol(
        "win11-16core-dual-gpu-desktop.jsonl",
        GraphSymbol::Block,
        120,
        40,
    );
    insta::assert_snapshot!("cpu_block_120x40", rendered);
}

#[test]
fn golden_cpu_block_200x60() {
    let rendered = render_fixture_with_symbol(
        "win11-16core-dual-gpu-desktop.jsonl",
        GraphSymbol::Block,
        200,
        60,
    );
    insta::assert_snapshot!("cpu_block_200x60", rendered);
}
