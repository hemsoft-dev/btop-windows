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
        shown_boxes: "cpu mem proc".to_owned(),
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
        shown_boxes: "cpu mem proc".to_owned(),
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
        shown_boxes: "cpu mem proc".to_owned(),
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
        shown_boxes: "cpu mem proc".to_owned(),
        show_disks: true,
        io_mode: true,
        ..Config::default()
    };
    let rendered = render_fixture_with_config("win11-16core-dual-gpu-desktop.jsonl", &cfg, 200, 60);
    insta::assert_snapshot!("disk_io_mode_200x60", rendered);
}

fn net_only_config(net_auto: bool) -> Config {
    Config {
        shown_boxes: "net".to_owned(),
        net_auto,
        net_sync: true,
        net_download: 500,
        net_upload: 250,
        ..Config::default()
    }
}

#[test]
fn golden_net_auto_80x24() {
    let rendered = render_fixture_with_config(
        "win11-8core-single-gpu-desktop.jsonl",
        &net_only_config(true),
        80,
        24,
    );
    insta::assert_snapshot!("net_auto_80x24", rendered);
}

#[test]
fn golden_net_auto_120x40() {
    let rendered = render_fixture_with_config(
        "win11-16core-dual-gpu-desktop.jsonl",
        &net_only_config(true),
        120,
        40,
    );
    insta::assert_snapshot!("net_auto_120x40", rendered);
}

#[test]
fn golden_net_auto_200x60() {
    let rendered = render_fixture_with_config(
        "win11-16core-dual-gpu-desktop.jsonl",
        &net_only_config(true),
        200,
        60,
    );
    insta::assert_snapshot!("net_auto_200x60", rendered);
}

#[test]
fn golden_net_fixed_80x24() {
    let rendered = render_fixture_with_config(
        "win11-8core-single-gpu-desktop.jsonl",
        &net_only_config(false),
        80,
        24,
    );
    insta::assert_snapshot!("net_fixed_80x24", rendered);
}

#[test]
fn golden_net_fixed_120x40() {
    let rendered = render_fixture_with_config(
        "win11-16core-dual-gpu-desktop.jsonl",
        &net_only_config(false),
        120,
        40,
    );
    insta::assert_snapshot!("net_fixed_120x40", rendered);
}

#[test]
fn golden_net_fixed_200x60() {
    let rendered = render_fixture_with_config(
        "win11-16core-dual-gpu-desktop.jsonl",
        &net_only_config(false),
        200,
        60,
    );
    insta::assert_snapshot!("net_fixed_200x60", rendered);
}

fn render_fixture_preset(fixture: &str, preset: u8, width: u16, height: u16) -> String {
    let snapshots = parse_jsonl_file(&fixture_path(fixture)).expect("load fixture");
    let mut history = UiHistory::default();
    for _ in 0..8 {
        for snap in &snapshots {
            history.push(snap);
        }
    }
    let latest = snapshots.last().expect("non-empty fixture");
    let config = Config::default();
    let mut state = btop_ui::UiState::from_config(&config);
    assert!(
        state.layout.apply_preset(preset),
        "preset {preset} should exist in default config"
    );
    btop_ui::render_frame(latest, &history, &state, &config, Size { width, height }).to_plain_text()
}

#[test]
fn golden_preset_0_120x40() {
    let rendered = render_fixture_preset("win11-16core-dual-gpu-desktop.jsonl", 0, 120, 40);
    insta::assert_snapshot!("preset_0_120x40", rendered);
}

#[test]
fn golden_preset_1_120x40() {
    let rendered = render_fixture_preset("win11-16core-dual-gpu-desktop.jsonl", 1, 120, 40);
    insta::assert_snapshot!("preset_1_120x40", rendered);
}

#[test]
fn golden_preset_2_120x40() {
    let rendered = render_fixture_preset("win11-16core-dual-gpu-desktop.jsonl", 2, 120, 40);
    insta::assert_snapshot!("preset_2_120x40", rendered);
}

#[test]
fn golden_size_error_40x12() {
    let rendered = render_fixture_preset("win11-8core-single-gpu-desktop.jsonl", 0, 40, 12);
    insta::assert_snapshot!("size_error_40x12", rendered);
}

fn render_proc_mode(
    configure: impl FnOnce(&mut btop_ui::UiState, &btop_model::Snapshot, &UiHistory),
) -> String {
    let snapshots =
        parse_jsonl_file(&fixture_path("win11-16core-dual-gpu-desktop.jsonl")).expect("fixture");
    let mut history = UiHistory::default();
    for _ in 0..4 {
        for snap in &snapshots {
            history.push(snap);
        }
    }
    let latest = snapshots.last().expect("non-empty fixture");
    let config = Config {
        shown_boxes: "proc".to_owned(),
        ..Config::default()
    };
    let mut state = btop_ui::UiState::from_config(&config);
    configure(&mut state, latest, &history);
    btop_ui::render_frame(
        latest,
        &history,
        &state,
        &config,
        Size {
            width: 120,
            height: 40,
        },
    )
    .to_plain_text()
}

#[test]
fn golden_proc_tree_120x40() {
    let rendered = render_proc_mode(|state, _, _| {
        state.proc.tree_mode = true;
        state.proc.aggregate = true;
    });
    insta::assert_snapshot!("proc_tree_120x40", rendered);
}

#[test]
fn golden_proc_regex_filter_120x40() {
    let rendered = render_proc_mode(|state, _, _| {
        state.proc.set_filter("!(code|rust)");
    });
    insta::assert_snapshot!("proc_regex_filter_120x40", rendered);
}

#[test]
fn golden_proc_paused_120x40() {
    let rendered = render_proc_mode(|state, snap, history| {
        if let btop_model::DataState::Available(ref list) = snap.processes {
            state.proc.toggle_pause(Some(list), Some(history));
        }
    });
    insta::assert_snapshot!("proc_paused_120x40", rendered);
}

#[test]
fn golden_proc_followed_120x40() {
    let rendered = render_proc_mode(|state, snap, history| {
        if let btop_model::DataState::Available(ref list) = snap.processes {
            let (rows, _) = btop_ui::build_visible_rows(list, &state.proc);
            state.proc.selected = 1;
            let _ = state.proc.toggle_follow(&rows);
            state.proc.toggle_pause(Some(list), Some(history));
        }
    });
    insta::assert_snapshot!("proc_followed_120x40", rendered);
}
