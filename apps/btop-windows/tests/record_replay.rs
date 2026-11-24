use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use btop_collect::{ReplaySource, StubLiveSource};
use btop_config::Config;
use btop_model::SnapshotRecorder;
use btop_term::Size;
use btop_windows::run_headless_frames;

fn temp_jsonl_path(tag: &str) -> PathBuf {
    let unique = format!(
        "btop-windows-{tag}-{}-{}.jsonl",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    );
    std::env::temp_dir().join(unique)
}

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

#[test]
fn record_then_replay_produces_identical_rendered_frames() {
    let path = temp_jsonl_path("roundtrip");
    let config = Config {
        update_ms: 500,
        ..Config::default()
    };
    let size = Size {
        width: 100,
        height: 24,
    };

    let mut live = StubLiveSource::new(config.update_ms);
    let recorder = SnapshotRecorder::open(&path).expect("open recorder");
    let recorded_frames =
        run_headless_frames(&mut live, Some(&recorder), &config, size, 4).expect("record frames");
    let stats = recorder.finish().expect("finish recorder");
    assert_eq!(stats.written, 4);
    assert_eq!(stats.dropped, 0);

    let mut replay = ReplaySource::from_path(&path)
        .expect("load recorded jsonl")
        .with_looping(false);
    let replayed_frames =
        run_headless_frames(&mut replay, None, &config, size, 4).expect("replay frames");

    let _ = fs::remove_file(&path);
    assert_eq!(recorded_frames, replayed_frames);
}

#[test]
fn committed_fixtures_replay_cleanly_without_windows_apis() {
    let config = Config::default();
    let size = Size {
        width: 120,
        height: 40,
    };

    for file in [
        "win11-8core-single-gpu-desktop.jsonl",
        "win11-16core-dual-gpu-desktop.jsonl",
    ] {
        let mut replay = ReplaySource::from_path(&fixture_path(file))
            .expect("load fixture")
            .with_looping(false);
        assert_eq!(replay.len(), 3);
        let frames =
            run_headless_frames(&mut replay, None, &config, size, 10).expect("render fixture");
        assert_eq!(frames.len(), 3);
        let text = frames[0].to_plain_text();
        assert!(text.contains("cpu"));
        assert!(text.contains("WIN11-"));
    }
}

#[test]
fn cli_replay_malformed_jsonl_exits_with_clear_error_and_no_panic() {
    let path = temp_jsonl_path("malformed");
    fs::write(&path, "{\"timestamp_ms\":1}\n{bad json line\n").expect("write malformed file");

    let output = Command::new(env!("CARGO_BIN_EXE_btop"))
        .arg("--replay")
        .arg(&path)
        .output()
        .expect("run btop binary");

    let _ = fs::remove_file(&path);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("malformed JSONL snapshot on line"),
        "expected clear line-numbered parse error, got: {stderr}"
    );
    assert!(!stderr.contains("panicked at"));
}

#[test]
fn cli_replay_missing_file_exits_with_clear_error() {
    let path = temp_jsonl_path("does-not-exist");
    let output = Command::new(env!("CARGO_BIN_EXE_btop"))
        .arg("--replay")
        .arg(&path)
        .output()
        .expect("run btop binary");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed to load --replay"),
        "expected clear missing-file error, got: {stderr}"
    );
}
