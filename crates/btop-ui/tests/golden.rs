use std::path::Path;

use btop_config::Config;
use btop_model::parse_jsonl_file;
use btop_term::Size;

fn fixture_path(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

#[test]
fn golden_win11_8core_single_gpu_80x24() {
    let snapshots = parse_jsonl_file(&fixture_path("win11-8core-single-gpu-desktop.jsonl"))
        .expect("load 8-core fixture");
    let grid = btop_ui::render(
        &snapshots[0],
        &Config::default(),
        Size {
            width: 80,
            height: 24,
        },
    );
    insta::assert_snapshot!("win11_8core_single_gpu_80x24", grid.to_plain_text());
}

#[test]
fn golden_win11_16core_dual_gpu_120x40() {
    let snapshots = parse_jsonl_file(&fixture_path("win11-16core-dual-gpu-desktop.jsonl"))
        .expect("load 16-core dual-GPU fixture");
    let grid = btop_ui::render(
        &snapshots[0],
        &Config::default(),
        Size {
            width: 120,
            height: 40,
        },
    );
    insta::assert_snapshot!("win11_16core_dual_gpu_120x40", grid.to_plain_text());
}
