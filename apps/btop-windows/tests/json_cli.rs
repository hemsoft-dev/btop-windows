//! End-to-end CLI integration tests for headless `--json` mode, subsystem selection,
//! warmup sampling, and machine-readable stderr error contracts.

use std::path::PathBuf;
use std::process::Command;

use btop_model::{
    JSON_SCHEMA_VERSION, JsonErrorEnvelope, JsonSnapshotEnvelope, JsonSummaryEnvelope,
};

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

    // Running services filter on live Windows 11 x64 returns only Running services.
    let run_out = Command::new(btop_bin())
        .args([
            "--json",
            "--sections",
            "services",
            "--services-state",
            "running",
            "--sample-ms",
            "0",
        ])
        .output()
        .expect("run live running services");
    assert!(run_out.status.success());
    let run_env: JsonSnapshotEnvelope =
        serde_json::from_slice(&run_out.stdout).expect("parse running services");
    let Some(btop_model::DataState::Available(running)) = run_env.services else {
        panic!("expected available running services");
    };
    assert_ne!(running.len(), 0);
    assert!(
        running
            .iter()
            .all(|s| s.service.state.eq_ignore_ascii_case("running"))
    );
}

#[test]
fn json_proc_flat_and_tree_queries_on_replay_fixture() {
    let fixture = fixture_path();

    // 1. Flat --top + --sort mem + metadata.
    let flat_out = Command::new(btop_bin())
        .args([
            "--json",
            "--sections",
            "procs",
            "--sort",
            "mem",
            "--top",
            "5",
            "--replay",
        ])
        .arg(&fixture)
        .output()
        .expect("run flat top 5 mem");
    assert!(flat_out.status.success());
    let flat_env: JsonSnapshotEnvelope =
        serde_json::from_slice(&flat_out.stdout).expect("parse flat json");
    assert!(flat_env.proc_tree.is_none());
    let Some(btop_model::DataState::Available(procs)) = flat_env.procs else {
        panic!("expected available procs");
    };
    assert_eq!(procs.len(), 5);
    assert!(
        procs
            .windows(2)
            .all(|w| w[0].working_set_private_bytes >= w[1].working_set_private_bytes)
    );
    let meta = flat_env.procs_meta.expect("procs_meta");
    assert_eq!(meta.returned, 5);
    assert!(meta.total_processes > 5);
    assert!(meta.truncated);
    assert_eq!(meta.sort, btop_model::ProcSortField::Mem);
    assert!(!meta.tree);

    // 2. Nested --tree + regex --filter + --top.
    let tree_out = Command::new(btop_bin())
        .args([
            "--json",
            "--sections",
            "procs",
            "--tree",
            "--filter",
            "!^(cargo|rust-analyzer)",
            "--sort",
            "cpu",
            "--top",
            "10",
            "--replay",
        ])
        .arg(&fixture)
        .output()
        .expect("run tree regex filter");
    assert!(tree_out.status.success());
    let tree_env: JsonSnapshotEnvelope =
        serde_json::from_slice(&tree_out.stdout).expect("parse tree json");
    assert!(
        tree_env.procs.is_none(),
        "flat procs omitted in --tree mode"
    );
    let Some(btop_model::DataState::Available(roots)) = tree_env.proc_tree else {
        panic!("expected available proc_tree");
    };
    assert_ne!(roots.len(), 0);
    assert!(roots.iter().all(|r| r.depth == 0));
    let tree_meta = tree_env.procs_meta.expect("tree meta");
    assert!(tree_meta.tree);
    assert!(tree_meta.total_matched > 0);
}

#[test]
fn json_proc_pid_slicing_and_query_error_boundaries() {
    let fixture = fixture_path();

    // 1. --pid subtree slicing for rust-analyzer (PID 4216 in committed dual-GPU fixture).
    let pid_out = Command::new(btop_bin())
        .args([
            "--json",
            "--sections",
            "procs",
            "--pid",
            "4216",
            "--tree",
            "--replay",
        ])
        .arg(&fixture)
        .output()
        .expect("run pid 4216 tree");
    assert!(pid_out.status.success());
    let pid_env: JsonSnapshotEnvelope =
        serde_json::from_slice(&pid_out.stdout).expect("parse pid json");
    let Some(btop_model::DataState::Available(pid_roots)) = pid_env.proc_tree else {
        panic!("expected available proc_tree for pid 4216");
    };
    assert_ne!(pid_roots.len(), 0);
    let pid_meta = pid_env.procs_meta.expect("pid meta");
    assert_eq!(pid_meta.pid, Some(4216));
    assert!(pid_meta.total_matched >= 1);

    // 2. Invalid --sort, --services-state, and invalid !regex produce structured JSON errors.
    for args in [
        &["--json", "--sort", "bogus"][..],
        &["--json", "--services-state", "paused"][..],
        &["--json", "--filter", "![unclosed"][..],
    ] {
        let bad = Command::new(btop_bin())
            .args(args)
            .output()
            .expect("run invalid query flag");
        assert!(!bad.status.success(), "expected error for {args:?}");
        assert_eq!(bad.stdout, b"");
        let parsed: JsonErrorEnvelope =
            serde_json::from_slice(&bad.stderr).expect("structured stderr json");
        assert_eq!(parsed.error.code, "cli_usage");
    }

    // 3. Value-consumption boundary: `--sort --json` must NOT emit JSON stderr.
    for args in [
        &["--sort", "--json"][..],
        &["--pid", "--json"][..],
        &["--services-state", "--json"][..],
        &["--top", "--json"][..],
        &["--count", "--json"][..],
        &["--interval-ms", "--json"][..],
        &["--window-ms", "--json"][..],
    ] {
        let non_json = Command::new(btop_bin())
            .args(args)
            .output()
            .expect("run non-json value consumption");
        assert!(!non_json.status.success());
        assert_eq!(non_json.stdout, b"");
        assert!(serde_json::from_slice::<JsonErrorEnvelope>(&non_json.stderr).is_err());
    }
}

#[test]
fn json_streaming_count_and_replay_summary_with_series() {
    let fixture = fixture_path();

    // 1. Streaming `--count 2` on replay emits exactly 2 valid NDJSON lines.
    let stream_out = Command::new(btop_bin())
        .args([
            "--json",
            "--sections",
            "cpu,mem,gpus",
            "--count",
            "2",
            "--replay",
        ])
        .arg(&fixture)
        .output()
        .expect("run replay stream count 2");
    assert!(
        stream_out.status.success(),
        "stream failed: {}",
        String::from_utf8_lossy(&stream_out.stderr)
    );
    let stream_text = String::from_utf8(stream_out.stdout).expect("utf8 stream");
    let lines: Vec<&str> = stream_text.lines().collect();
    assert_eq!(lines.len(), 2);
    for line in lines {
        let env: JsonSnapshotEnvelope = serde_json::from_str(line).expect("parse ndjson line");
        assert_eq!(env.schema_version, JSON_SCHEMA_VERSION);
        assert!(env.cpu.is_some());
        assert!(env.mem.is_some());
        assert!(env.gpus.is_some());
        assert!(env.disks.is_none());
    }

    // 2. Deterministic `--summary --include-series` on committed dual-GPU fixture.
    let sum_out = Command::new(btop_bin())
        .args(["--json", "--summary", "--include-series", "--replay"])
        .arg(&fixture)
        .output()
        .expect("run replay summary");
    assert!(
        sum_out.status.success(),
        "summary failed: {}",
        String::from_utf8_lossy(&sum_out.stderr)
    );
    let summary: JsonSummaryEnvelope =
        serde_json::from_slice(&sum_out.stdout).expect("parse summary json");
    assert_eq!(summary.schema_version, JSON_SCHEMA_VERSION);
    assert_eq!(summary.sample_count, 3);
    assert!(summary.window_ms > 0);
    assert!(
        !summary.sections.iter().any(|s| s == "services"),
        "summary sections must not advertise unsupported services"
    );
    let cpu = summary.cpu.expect("cpu summary");
    assert_eq!(cpu.samples_available, 3);
    assert!(cpu.total_busy_pct.max >= cpu.total_busy_pct.min);
    let mem = summary.mem.expect("mem summary");
    assert_eq!(mem.samples_available, 3);
    let gpus = summary.gpus.expect("gpus summary");
    assert_eq!(gpus.len(), 2);
    let procs = summary.procs.expect("procs summary");
    assert_ne!(procs.by_cpu.len(), 0);
    assert_ne!(procs.by_mem.len(), 0);
    let series = summary.series.expect("series");
    assert_eq!(series.len(), 3);

    // 3. Single-frame replay summary reports deterministic window_ms = 0.
    let single_out = Command::new(btop_bin())
        .args(["--json", "--summary", "--count", "1", "--replay"])
        .arg(&fixture)
        .output()
        .expect("run single-frame replay summary");
    assert!(single_out.status.success());
    let single_sum: JsonSummaryEnvelope =
        serde_json::from_slice(&single_out.stdout).expect("parse single replay summary");
    assert_eq!(single_sum.sample_count, 1);
    assert_eq!(single_sum.window_ms, 0);
}

#[test]
fn json_streaming_and_summary_reject_invalid_flag_combinations() {
    for args in [
        &["--json", "--summary", "--count", "0"][..],
        &["--json", "--summary", "--sections", "services"][..],
        &["--json", "--count", "2", "--pretty"][..],
        &["--json", "--interval-ms", "50"][..],
        &["--json", "--window-ms", "500"][..],
    ] {
        let bad = Command::new(btop_bin())
            .args(args)
            .output()
            .expect("run invalid stream/summary flag combo");
        assert!(!bad.status.success(), "expected error for {args:?}");
        assert_eq!(bad.stdout, b"");
        let parsed: JsonErrorEnvelope =
            serde_json::from_slice(&bad.stderr).expect("structured stderr json");
        assert_eq!(parsed.error.code, "cli_usage");
    }
}

#[cfg(windows)]
#[test]
fn json_streaming_broken_pipe_exits_cleanly() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let mut child = Command::new(btop_bin())
        .args([
            "--json",
            "--sections",
            "cpu",
            "--count",
            "0",
            "--interval-ms",
            "100",
            "--sample-ms",
            "0",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn continuous stream");

    {
        let stdout = child.stdout.take().expect("stdout pipe");
        let mut reader = BufReader::new(stdout);
        let mut first_line = String::new();
        let bytes = reader.read_line(&mut first_line).expect("read first line");
        assert_ne!(bytes, 0);
        let parsed: JsonSnapshotEnvelope =
            serde_json::from_str(first_line.trim()).expect("valid first json line");
        assert_eq!(parsed.schema_version, JSON_SCHEMA_VERSION);
        // Drop reader/stdout here to close the pipe while child is still streaming.
    }

    let output = child.wait_with_output().expect("wait for broken-pipe exit");
    assert!(
        output.status.success(),
        "expected clean exit 0 on broken pipe, got {:?}, stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stderr.len(), 0);
}

#[cfg(windows)]
#[test]
fn live_windows_streaming_and_summary_populate_real_metrics() {
    let stream = Command::new(btop_bin())
        .args([
            "--json",
            "--sections",
            "cpu,mem,gpus",
            "--count",
            "2",
            "--interval-ms",
            "150",
            "--sample-ms",
            "100",
        ])
        .output()
        .expect("run live stream");
    assert!(
        stream.status.success(),
        "live stream failed: {}",
        String::from_utf8_lossy(&stream.stderr)
    );
    let lines: Vec<&str> = std::str::from_utf8(&stream.stdout)
        .expect("utf8")
        .lines()
        .collect();
    assert_eq!(lines.len(), 2);
    for line in lines {
        let env: JsonSnapshotEnvelope = serde_json::from_str(line).expect("parse live line");
        assert!(matches!(env.cpu, Some(btop_model::DataState::Available(_))));
    }

    let sum = Command::new(btop_bin())
        .args([
            "--json",
            "--summary",
            "--include-series",
            "--sections",
            "cpu,mem,procs",
            "--window-ms",
            "400",
            "--interval-ms",
            "150",
            "--sample-ms",
            "100",
        ])
        .output()
        .expect("run live summary");
    assert!(
        sum.status.success(),
        "live summary failed: {}",
        String::from_utf8_lossy(&sum.stderr)
    );
    let summary: JsonSummaryEnvelope =
        serde_json::from_slice(&sum.stdout).expect("parse live summary");
    assert!(summary.sample_count >= 2);
    assert!(summary.cpu.is_some());
    assert!(summary.mem.is_some());
    assert!(summary.procs.is_some());
    assert_eq!(
        summary.series.as_ref().map(Vec::len),
        Some(summary.sample_count)
    );
}

#[test]
fn json_diagnose_attaches_deterministic_report_for_snapshot_and_summary() {
    let fixture = fixture_path();

    // 1. Single-snapshot --diagnose on committed dual-GPU fixture.
    let snap_out = Command::new(btop_bin())
        .args(["--json", "--diagnose", "--replay"])
        .arg(&fixture)
        .output()
        .expect("run replay --diagnose");
    assert!(
        snap_out.status.success(),
        "diagnose failed: {}",
        String::from_utf8_lossy(&snap_out.stderr)
    );
    let snap_env: JsonSnapshotEnvelope =
        serde_json::from_slice(&snap_out.stdout).expect("parse diagnose snapshot");
    let diag = snap_env.diagnostics.expect("diagnostics attached");
    assert_ne!(diag.checks_evaluated.len(), 0);

    // 2. Section subset skips unselected sections cleanly without false warnings.
    let subset_out = Command::new(btop_bin())
        .args(["--json", "--diagnose", "--sections", "cpu,mem", "--replay"])
        .arg(&fixture)
        .output()
        .expect("run replay --diagnose --sections cpu,mem");
    assert!(subset_out.status.success());
    let subset_env: JsonSnapshotEnvelope =
        serde_json::from_slice(&subset_out.stdout).expect("parse subset diagnose");
    let subset_diag = subset_env.diagnostics.expect("subset diagnostics");
    assert!(
        subset_diag
            .skipped_checks
            .iter()
            .any(|s| s.check == "disk_health" && s.reason == "section not selected")
    );
    assert!(
        subset_diag
            .skipped_checks
            .iter()
            .any(|s| s.check == "gpu_health" && s.reason == "section not selected")
    );

    // 3. Combined --summary --diagnose on committed dual-GPU fixture (even with restrictive --filter/--top).
    let sum_out = Command::new(btop_bin())
        .args([
            "--json",
            "--summary",
            "--diagnose",
            "--top",
            "1",
            "--filter",
            "nonexistent-filter-token",
            "--replay",
        ])
        .arg(&fixture)
        .output()
        .expect("run replay --summary --diagnose");
    assert!(sum_out.status.success());
    let sum_env: JsonSummaryEnvelope =
        serde_json::from_slice(&sum_out.stdout).expect("parse summary diagnose");
    let sum_diag = sum_env.diagnostics.expect("summary diagnostics");
    assert_ne!(sum_diag.checks_evaluated.len(), 0);
    assert!(
        sum_diag
            .checks_evaluated
            .iter()
            .any(|c| c == "process_subtree_cpu_dominance")
    );

    // 4. --diagnose without --json fails cleanly with plain clap stderr (not JSON).
    let no_json = Command::new(btop_bin())
        .args(["--diagnose"])
        .output()
        .expect("run --diagnose without --json");
    assert!(!no_json.status.success());
    assert!(serde_json::from_slice::<JsonErrorEnvelope>(&no_json.stderr).is_err());
}

#[cfg(windows)]
#[test]
fn live_windows_diagnose_handles_unavailable_sensors_honestly() {
    let out = Command::new(btop_bin())
        .args(["--json", "--diagnose", "--sample-ms", "250"])
        .output()
        .expect("run live --diagnose");
    assert!(
        out.status.success(),
        "live diagnose failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env: JsonSnapshotEnvelope =
        serde_json::from_slice(&out.stdout).expect("parse live diagnose json");
    let diag = env.diagnostics.expect("live diagnostics");
    assert_ne!(diag.checks_evaluated.len(), 0);
    // System Idle Process (PID 0) must never appear as a CPU dominance finding.
    assert!(
        !diag.findings.iter().any(|f| {
            f.code == "process_cpu_dominance"
                && f.evidence.entity.as_ref().and_then(|e| e.pid) == Some(0)
        }),
        "PID 0 must not trigger process_cpu_dominance"
    );
}
