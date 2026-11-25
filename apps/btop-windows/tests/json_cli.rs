//! End-to-end CLI integration tests for headless `--json` mode, subsystem selection,
//! warmup sampling, and machine-readable stderr error contracts.

use std::path::PathBuf;
use std::process::Command;

use btop_model::{JSON_SCHEMA_VERSION, JsonErrorEnvelope, JsonSnapshotEnvelope};

fn btop_bin() -> &'static str {
    env!("CARGO_BIN_EXE_btop")
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("win11-16core-dual-gpu-desktop.jsonl")
}

#[test]
fn json_replay_default_all_sections_and_pretty_output() {
    let fixture = fixture_path();
    let out = Command::new(btop_bin())
        .args(["--json", "--replay"])
        .arg(&fixture)
        .output()
        .expect("run btop --json --replay");

    assert!(
        out.status.success(),
        "expected exit 0, stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stderr.is_empty(),
        "expected empty stderr, got: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert_eq!(
        stdout.lines().count(),
        1,
        "compact JSON output must be a single line"
    );
    let env: JsonSnapshotEnvelope = serde_json::from_str(&stdout).expect("parse snapshot envelope");
    assert_eq!(env.schema_version, JSON_SCHEMA_VERSION);
    assert_eq!(env.btop_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(env.sample_window_ms, 0);
    assert_eq!(
        env.sections,
        vec![
            "cpu", "mem", "disks", "net", "procs", "services", "gpus", "battery"
        ]
    );
    assert!(env.cpu.is_some());
    assert!(env.mem.is_some());
    assert!(env.disks.is_some());
    assert!(env.net.is_some());
    assert!(env.procs.is_some());
    assert!(env.services.is_some());
    assert!(env.gpus.is_some());
    assert!(env.battery.is_some());

    // Pretty output produces multi-line JSON decoding to the exact same struct.
    let pretty_out = Command::new(btop_bin())
        .args(["--json", "--pretty", "--replay"])
        .arg(&fixture)
        .output()
        .expect("run btop --json --pretty");
    assert!(pretty_out.status.success());
    let pretty_stdout = String::from_utf8(pretty_out.stdout).expect("utf-8 pretty stdout");
    assert!(
        pretty_stdout.lines().count() > 10,
        "pretty output must span multiple lines"
    );
    let pretty_env: JsonSnapshotEnvelope =
        serde_json::from_str(&pretty_stdout).expect("parse pretty envelope");
    assert_eq!(pretty_env, env);
}

#[test]
fn json_replay_sections_subset_omits_unselected_subsystems() {
    let out = Command::new(btop_bin())
        .args(["--json", "--sections", "cpu,mem,gpus", "--replay"])
        .arg(fixture_path())
        .output()
        .expect("run subset sections");
    assert!(out.status.success());
    assert_eq!(out.stderr, b"");

    let value: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid json");
    assert_eq!(value["schema_version"], JSON_SCHEMA_VERSION);
    assert_eq!(value["sections"], serde_json::json!(["cpu", "mem", "gpus"]));
    assert_eq!(value["cpu"]["state"], "available");
    assert_eq!(value["mem"]["state"], "available");
    assert!(value["gpus"].is_array());
    assert!(value.get("disks").is_none());
    assert!(value.get("net").is_none());
    assert!(value.get("procs").is_none());
    assert!(value.get("services").is_none());
    assert!(value.get("battery").is_none());
}

#[test]
fn json_replay_with_sample_ms_advances_to_second_frame_without_sleeping() {
    let out = Command::new(btop_bin())
        .args([
            "--json",
            "--sections",
            "cpu",
            "--sample-ms",
            "500",
            "--replay",
        ])
        .arg(fixture_path())
        .output()
        .expect("run replay with sample-ms");
    assert!(out.status.success());
    let env: JsonSnapshotEnvelope = serde_json::from_slice(&out.stdout).expect("parse json");
    assert_eq!(env.sample_window_ms, 2000);
}

#[test]
fn json_errors_emit_structured_stderr_for_flag_positions_conflicts_and_runtime_failures() {
    let cases: &[(&[&str], &str)] = &[
        // --json before failing flag
        (&["--json", "--unknown-flag"], "cli_usage"),
        // --json after failing flag
        (&["--unknown-flag", "--json"], "cli_usage"),
        // repeated boolean --json
        (
            &["--json", "--json", "--replay", "nonexistent.jsonl"],
            "cli_usage",
        ),
        // invalid --sections value
        (&["--json", "--sections", "cpu,invalid"], "cli_usage"),
        // out-of-range --sample-ms
        (&["--json", "--sample-ms", "999999"], "cli_usage"),
        // conflict with --default-config and --record
        (&["--json", "--default-config"], "cli_usage"),
        (&["--default-config", "--json"], "cli_usage"),
        (&["--json", "--record", "out.jsonl"], "cli_usage"),
        // missing replay file at runtime
        (
            &["--json", "--replay", "does-not-exist-98765.jsonl"],
            "replay_error",
        ),
    ];

    for (args, expected_code) in cases {
        let out = Command::new(btop_bin())
            .args(*args)
            .output()
            .expect("run failing json invocation");
        assert!(!out.status.success(), "expected failure for args {args:?}");
        assert!(
            out.stdout.is_empty(),
            "stdout must stay empty on error for args {args:?}, got: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
        let parsed: JsonErrorEnvelope = serde_json::from_str(&stderr).unwrap_or_else(|e| {
            panic!("expected structured JSON error on stderr for {args:?}, got '{stderr}' ({e})")
        });
        assert_eq!(parsed.schema_version, JSON_SCHEMA_VERSION);
        assert_eq!(parsed.error.code, *expected_code, "args={args:?}");
        assert_ne!(parsed.error.message, "");
    }
}

#[test]
fn non_json_failures_and_boundaries_preserve_plain_stderr() {
    // `--` boundary: `--json` after `--` is positional, not a JSON option.
    for args in [
        &["--", "--json"][..],
        &["--unknown-flag", "--", "--json"][..],
        &["--filter=--json", "--unknown-flag"][..],
        &["-f--json", "--unknown-flag"][..],
        &["--filter", "--json"][..],
        &["--unknown-flag"][..],
        &["--replay", "does-not-exist-98765.jsonl"][..],
    ] {
        let out = Command::new(btop_bin())
            .args(args)
            .output()
            .expect("run non-json failing invocation");
        assert!(!out.status.success(), "expected failure for {args:?}");
        assert_eq!(out.stdout, b"");
        let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
        assert!(
            serde_json::from_str::<JsonErrorEnvelope>(&stderr).is_err(),
            "non-JSON failure {args:?} must NOT emit JSON stderr: {stderr}"
        );
        assert!(
            stderr.contains("error:"),
            "expected standard error prefix for {args:?}, got: {stderr}"
        );
    }

    // `--help` and `--version` exit 0 even when paired with `--json`.
    for args in [&["--json", "--help"][..], &["--json", "--version"][..]] {
        let out = Command::new(btop_bin())
            .args(args)
            .output()
            .expect("run help/version");
        assert!(out.status.success(), "expected exit 0 for {args:?}");
        assert_ne!(out.stdout, b"");
    }
}

#[cfg(windows)]
#[test]
fn live_windows_json_snapshot_runs_without_tty_and_populates_deltas() {
    // Piped stdout/stderr (no attached VT console) with 300ms warmup sample.
    let out = Command::new(btop_bin())
        .args([
            "--json",
            "--sections",
            "cpu,mem,disks,net,procs,gpus",
            "--sample-ms",
            "300",
        ])
        .output()
        .expect("run live btop --json");
    assert!(
        out.status.success(),
        "live --json failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stderr, b"");

    let env: JsonSnapshotEnvelope = serde_json::from_slice(&out.stdout).expect("parse live json");
    assert_eq!(env.schema_version, JSON_SCHEMA_VERSION);
    assert!(env.sample_window_ms >= 250);
    assert!(
        env.services.is_none(),
        "services must be omitted when not selected"
    );
    assert!(
        env.battery.is_none(),
        "battery must be omitted when not selected"
    );
    assert!(matches!(env.cpu, Some(btop_model::DataState::Available(_))));
    assert!(matches!(env.mem, Some(btop_model::DataState::Available(_))));
    assert!(matches!(env.procs, Some(btop_model::DataState::Available(ref p)) if !p.is_empty()));

    // Static `--sample-ms 0` returns immediately with services enabled when requested.
    let svc_out = Command::new(btop_bin())
        .args(["--json", "--sections", "services", "--sample-ms", "0"])
        .output()
        .expect("run live services --sample-ms 0");
    assert!(
        svc_out.status.success(),
        "live services --json failed: {}",
        String::from_utf8_lossy(&svc_out.stderr)
    );
    let svc_env: JsonSnapshotEnvelope =
        serde_json::from_slice(&svc_out.stdout).expect("parse live services json");
    assert_eq!(svc_env.sample_window_ms, 0);
    assert!(matches!(
        svc_env.services,
        Some(btop_model::DataState::Available(ref s)) if !s.is_empty()
    ));
}
