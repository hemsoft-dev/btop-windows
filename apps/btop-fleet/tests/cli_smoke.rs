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

#[cfg(unix)]
#[test]
fn btop_fleet_live_unix_json_diagnose_conforms_to_schema() {
    use btop_model::{DataState, SchemaMode, build_json_schema, validate_against_schema};

    let live_out = Command::new(fleet_bin())
        .args([
            "--json",
            "--sections",
            "cpu,mem,disks,net,procs,gpus,services,battery",
            "--sample-ms",
            "250",
            "--diagnose",
        ])
        .output()
        .expect("run live btop-fleet --json --diagnose");
    assert!(
        live_out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&live_out.stderr)
    );
    let raw_val: serde_json::Value =
        serde_json::from_slice(&live_out.stdout).expect("parse live json value");
    let schema = build_json_schema(SchemaMode::Snapshot);
    let errors = validate_against_schema(&schema, &raw_val);
    assert!(
        errors.is_empty(),
        "live Unix --json output violated schema: {errors:?}"
    );

    let env: JsonSnapshotEnvelope =
        serde_json::from_value(raw_val).expect("deserialize live snapshot envelope");
    assert!(matches!(env.cpu, Some(DataState::Available(_))));
    assert!(matches!(env.mem, Some(DataState::Available(_))));
    assert!(matches!(env.disks, Some(DataState::Available(_))));
    assert!(matches!(env.net, Some(DataState::Available(_))));
    assert!(matches!(env.procs, Some(DataState::Available(_))));
    assert!(matches!(env.services, Some(DataState::Unavailable(_))));
    assert!(matches!(env.battery, Some(DataState::Unavailable(_))));
    assert!(
        env.gpus.as_ref().is_some_and(
            |g| !g.is_empty() && g.iter().all(|s| matches!(s, DataState::Unavailable(_)))
        )
    );
    assert!(env.diagnostics.is_some());
}
