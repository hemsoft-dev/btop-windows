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
    if let Err(errors) = validate_against_schema(&schema, &raw_val) {
        panic!("live Unix --json output violated schema: {errors:?}");
    }

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

#[test]
fn btop_fleet_agent_rejects_wildcard_bind_and_serves_samples_over_loopback() {
    use std::io::{BufRead as _, BufReader, Read as _, Write as _};
    use std::net::TcpStream;
    use std::process::Stdio;
    use std::time::Duration;

    use btop_model::{DataState, FLEET_SCHEMA_VERSION, FleetSamplesResponse};

    // 1. Wildcard 0.0.0.0 bind must be rejected immediately with exit code 2.
    let bad = Command::new(fleet_bin())
        .args(["agent", "--bind", "0.0.0.0:9127"])
        .output()
        .expect("run btop-fleet agent --bind 0.0.0.0:9127");
    assert_eq!(bad.status.code(), Some(2));
    let bad_err = String::from_utf8_lossy(&bad.stderr);
    assert!(
        bad_err.contains("0.0.0.0"),
        "expected forbidden wildcard bind error, got: {bad_err}"
    );

    // 2. Start live agent bound to 127.0.0.1:0 with 1s interval.
    let mut child = Command::new(fleet_bin())
        .args([
            "agent",
            "--bind",
            "127.0.0.1:0",
            "--interval",
            "1",
            "--retention",
            "1",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn btop-fleet agent");

    let stderr = child.stderr.take().expect("agent stderr");
    let mut reader = BufReader::new(stderr);
    let mut ready_line = String::new();
    reader
        .read_line(&mut ready_line)
        .expect("read agent ready line");
    let addr_part = ready_line
        .split("http://")
        .nth(1)
        .and_then(|s| s.split("/v1/samples").next())
        .unwrap_or_else(|| panic!("unexpected ready line: {ready_line:?}"));

    let http_req = |req: &str| -> String {
        let mut stream = TcpStream::connect(addr_part).expect("connect to live agent");
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("set timeout");
        stream.write_all(req.as_bytes()).expect("write request");
        stream.flush().expect("flush");
        let mut resp = String::new();
        stream.read_to_string(&mut resp).expect("read response");
        resp
    };

    // GET /v1/samples?since_ms=0 returns valid NodeSample JSON with zero process/user fields.
    let raw_full = http_req(&format!(
        "GET /v1/samples?since_ms=0 HTTP/1.1\r\nHost: {addr_part}\r\n\r\n"
    ));
    assert!(raw_full.starts_with("HTTP/1.1 200 OK\r\n"));
    let (_, body) = raw_full.split_once("\r\n\r\n").expect("split response");
    for forbidden in ["username", "processes", "command_line", "gpus", "services"] {
        assert!(
            !body.contains(&format!("\"{forbidden}\"")),
            "agent payload leaked forbidden field {forbidden:?}: {body}"
        );
    }
    let full: FleetSamplesResponse = serde_json::from_str(body).expect("parse fleet samples");
    assert_eq!(full.schema_version, FLEET_SCHEMA_VERSION);
    assert_eq!(full.interval_ms, 1_000);
    assert!(
        !full.samples.is_empty(),
        "expected initial post-warmup sample"
    );
    let first = &full.samples[0];
    assert!(first.mem_total_bytes > 0);
    assert!(matches!(first.cpu_busy_pct, DataState::Available(_)));

    // Incremental cursor at newest timestamp returns empty list immediately, then newer sample.
    let last_ts = full.samples.last().expect("last sample").timestamp_ms;
    let raw_empty = http_req(&format!(
        "GET /v1/samples?since_ms={last_ts} HTTP/1.1\r\nHost: {addr_part}\r\n\r\n"
    ));
    let (_, empty_body) = raw_empty.split_once("\r\n\r\n").expect("split empty");
    let empty_resp: FleetSamplesResponse =
        serde_json::from_str(empty_body).expect("parse empty delta");
    assert_eq!(empty_resp.samples.len(), 0);

    std::thread::sleep(Duration::from_millis(1_150));
    let raw_delta = http_req(&format!(
        "GET /v1/samples?since_ms={last_ts} HTTP/1.1\r\nHost: {addr_part}\r\n\r\n"
    ));
    let (_, delta_body) = raw_delta.split_once("\r\n\r\n").expect("split delta");
    let delta_resp: FleetSamplesResponse = serde_json::from_str(delta_body).expect("parse delta");
    assert!(
        !delta_resp.samples.is_empty(),
        "expected newer sample after 1s interval"
    );
    assert!(delta_resp.samples.iter().all(|s| s.timestamp_ms > last_ts));

    // POST must be rejected with 405 Method Not Allowed.
    let raw_post = http_req(&format!(
        "POST /v1/samples HTTP/1.1\r\nHost: {addr_part}\r\nContent-Length: 0\r\n\r\n"
    ));
    assert!(raw_post.starts_with("HTTP/1.1 405 Method Not Allowed\r\n"));

    let _ = child.kill();
    let _ = child.wait();
}
