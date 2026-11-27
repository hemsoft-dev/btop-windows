//! End-to-end smoke tests for the `btop-fleet` binary across Windows, Linux, and macOS.

use std::path::PathBuf;
use std::process::Command;

use btop_model::{JSON_SCHEMA_DRAFT_2020_12, JsonSnapshotEnvelope};

fn fleet_bin() -> &'static str {
    env!("CARGO_BIN_EXE_btop-fleet")
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("win11-16core-dual-gpu-desktop.jsonl")
}

#[test]
fn btop_fleet_version_help_and_default_config_succeed() {
    let ver = Command::new(fleet_bin())
        .arg("--version")
        .output()
        .expect("run btop-fleet --version");
    assert!(ver.status.success());
    let ver_str = String::from_utf8(ver.stdout).expect("utf-8 version");
    assert!(
        ver_str.starts_with("btop-fleet "),
        "expected binary name prefix 'btop-fleet ', got: {ver_str:?}"
    );

    let cfg = Command::new(fleet_bin())
        .arg("--default-config")
        .output()
        .expect("run btop-fleet --default-config");
    assert!(cfg.status.success());
    let cfg_str = String::from_utf8(cfg.stdout).expect("utf-8 default config");
    assert!(cfg_str.contains("color_theme ="));
}

#[test]
fn btop_fleet_schema_and_json_replay_work_cross_platform() {
    let schema_out = Command::new(fleet_bin())
        .arg("--schema")
        .output()
        .expect("run btop-fleet --schema");
    assert!(schema_out.status.success());
    let schema: serde_json::Value =
        serde_json::from_slice(&schema_out.stdout).expect("parse schema json");
    assert_eq!(schema["$schema"], JSON_SCHEMA_DRAFT_2020_12);

    let replay_out = Command::new(fleet_bin())
        .args(["--json", "--diagnose", "--replay"])
        .arg(fixture_path())
        .output()
        .expect("run btop-fleet --json --diagnose --replay");
    assert!(
        replay_out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&replay_out.stderr)
    );
    let env: JsonSnapshotEnvelope =
        serde_json::from_slice(&replay_out.stdout).expect("parse replay snapshot");
    assert!(env.diagnostics.is_some());
    assert_eq!(env.hostname, "WIN11-HYBRID16");
}
