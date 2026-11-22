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

fn render_fixture_with_config(fixture: &str, config: &Config, width: u16, height: u16) -> String {
    let snapshots = parse_jsonl_file(&fixture_path(fixture)).expect("load fixture");
    let mut history = UiHistory::default();
    // Seed multiple wave cycles so Braille and Block graphs show rich deterministic shapes.
    for _ in 0..8 {
        for snap in &snapshots {
            history.push(snap);
        }
    }
    let latest = snapshots.last().expect("non-empty fixture");
    btop_ui::render_with_history(latest, &history, config, Size { width, height }).to_plain_text()
}

fn cpu_only_config(symbol: GraphSymbol) -> Config {
    Config {
        graph_symbol: symbol,
        shown_boxes: "cpu".to_owned(),
        ..Config::default()
    }
}

#[test]
fn golden_cpu_braille_80x24() {
    let rendered = render_fixture_with_config(
        "win11-8core-single-gpu-desktop.jsonl",
        &cpu_only_config(GraphSymbol::Braille),
        80,
        24,
    );
    insta::assert_snapshot!("cpu_braille_80x24", rendered);
}

#[test]
fn golden_cpu_braille_120x40() {
    let rendered = render_fixture_with_config(
        "win11-16core-dual-gpu-desktop.jsonl",
        &cpu_only_config(GraphSymbol::Braille),
        120,
        40,
    );
    insta::assert_snapshot!("cpu_braille_120x40", rendered);
}

#[test]
fn golden_cpu_braille_200x60() {
    let rendered = render_fixture_with_config(
        "win11-16core-dual-gpu-desktop.jsonl",
        &cpu_only_config(GraphSymbol::Braille),
        200,
        60,
    );
    insta::assert_snapshot!("cpu_braille_200x60", rendered);
}

#[test]
fn golden_cpu_block_80x24() {
    let rendered = render_fixture_with_config(
        "win11-8core-single-gpu-desktop.jsonl",
        &cpu_only_config(GraphSymbol::Block),
        80,
        24,
    );
    insta::assert_snapshot!("cpu_block_80x24", rendered);
}

#[test]
fn golden_cpu_block_120x40() {
    let rendered = render_fixture_with_config(
        "win11-16core-dual-gpu-desktop.jsonl",
        &cpu_only_config(GraphSymbol::Block),
        120,
        40,
    );
    insta::assert_snapshot!("cpu_block_120x40", rendered);
}

#[test]
fn golden_cpu_block_200x60() {
    let rendered = render_fixture_with_config(
        "win11-16core-dual-gpu-desktop.jsonl",
        &cpu_only_config(GraphSymbol::Block),
        200,
        60,
    );
    insta::assert_snapshot!("cpu_block_200x60", rendered);
}

#[test]
fn golden_proc_panel_80x24() {
    let rendered = render_fixture_with_config(
        "win11-8core-single-gpu-desktop.jsonl",
        &Config::default(),
        80,
        24,
    );
    insta::assert_snapshot!("proc_panel_80x24", rendered);
}

#[test]
fn golden_proc_panel_120x40() {
    let rendered = render_fixture_with_config(
        "win11-16core-dual-gpu-desktop.jsonl",
        &Config::default(),
        120,
        40,
    );
    insta::assert_snapshot!("proc_panel_120x40", rendered);
}

#[test]
fn golden_proc_panel_200x60() {
    let rendered = render_fixture_with_config(
        "win11-16core-dual-gpu-desktop.jsonl",
        &Config::default(),
        200,
        60,
    );
    insta::assert_snapshot!("proc_panel_200x60", rendered);
}

fn mem_only_config(mem_graphs: bool) -> Config {
    Config {
        shown_boxes: "mem".to_owned(),
        mem_graphs,
        show_disks: false,
        ..Config::default()
    }
}

#[test]
fn golden_mem_graph_80x24() {
    let rendered = render_fixture_with_config(
        "win11-8core-single-gpu-desktop.jsonl",
        &mem_only_config(true),
        80,
        24,
    );
    insta::assert_snapshot!("mem_graph_80x24", rendered);
}

#[test]
fn golden_mem_graph_120x40() {
    let rendered = render_fixture_with_config(
        "win11-16core-dual-gpu-desktop.jsonl",
        &mem_only_config(true),
        120,
        40,
    );
    insta::assert_snapshot!("mem_graph_120x40", rendered);
}

#[test]
fn golden_mem_graph_200x60() {
    let rendered = render_fixture_with_config(
        "win11-16core-dual-gpu-desktop.jsonl",
        &mem_only_config(true),
        200,
        60,
    );
    insta::assert_snapshot!("mem_graph_200x60", rendered);
}

#[test]
fn golden_mem_meter_80x24() {
    let rendered = render_fixture_with_config(
        "win11-8core-single-gpu-desktop.jsonl",
        &mem_only_config(false),
        80,
        24,
    );
    insta::assert_snapshot!("mem_meter_80x24", rendered);
}

#[test]
fn golden_mem_meter_120x40() {
    let rendered = render_fixture_with_config(
        "win11-16core-dual-gpu-desktop.jsonl",
        &mem_only_config(false),
        120,
        40,
    );
    insta::assert_snapshot!("mem_meter_120x40", rendered);
}

#[test]
fn golden_mem_meter_200x60() {
    let rendered = render_fixture_with_config(
        "win11-16core-dual-gpu-desktop.jsonl",
        &mem_only_config(false),
        200,
        60,
    );
    insta::assert_snapshot!("mem_meter_200x60", rendered);
}

#[test]
fn golden_disks_split_120x40() {
    let cfg = Config {
        show_disks: true,
        io_mode: false,
        ..Config::default()
    };
    let rendered = render_fixture_with_config("win11-16core-dual-gpu-desktop.jsonl", &cfg, 120, 40);
    insta::assert_snapshot!("disks_split_120x40", rendered);
}

#[test]
fn golden_disks_split_200x60() {
    let cfg = Config {
        show_disks: true,
        io_mode: false,
        ..Config::default()
    };
    let rendered = render_fixture_with_config("win11-16core-dual-gpu-desktop.jsonl", &cfg, 200, 60);
    insta::assert_snapshot!("disks_split_200x60", rendered);
}

#[test]
fn golden_disk_io_mode_120x40() {
    let cfg = Config {
        show_disks: true,
        io_mode: true,
        ..Config::default()
    };
    let rendered = render_fixture_with_config("win11-16core-dual-gpu-desktop.jsonl", &cfg, 120, 40);
    insta::assert_snapshot!("disk_io_mode_120x40", rendered);
}

#[test]
fn golden_disk_io_mode_200x60() {
    let cfg = Config {
        show_disks: true,
        io_mode: true,
        ..Config::default()
    };
    let rendered = render_fixture_with_config("win11-16core-dual-gpu-desktop.jsonl", &cfg, 200, 60);
    insta::assert_snapshot!("disk_io_mode_200x60", rendered);
}
