//! Headless `--json` snapshot collection, argv error-mode detection, and stdout/stderr formatting.

use std::ffi::OsString;
use std::io::Write;
use std::time::{Duration, Instant};

use btop_collect::{CollectError, SnapshotSource};
use btop_model::{JsonErrorEnvelope, JsonSections, JsonSnapshotEnvelope, Snapshot};

use crate::AppError;

/// Default live warmup sampling window in milliseconds so delta/rate collectors populate.
pub const DEFAULT_LIVE_SAMPLE_MS: u64 = 500;
/// Maximum allowed `--sample-ms` value (10 seconds).
pub const MAX_SAMPLE_MS: u64 = 10_000;

/// Options controlling a single headless `--json` snapshot invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeadlessJsonOptions {
    pub sections: JsonSections,
    pub sample_ms: u64,
    pub pretty: bool,
    pub is_replay: bool,
}

/// Collects a snapshot for headless `--json` output, enabling SCM polling only when requested
/// and performing a baseline + measurement pair when `sample_ms > 0` in live mode.
pub fn collect_headless_snapshot<F>(
    source: &mut dyn SnapshotSource,
    options: HeadlessJsonOptions,
    sleep_fn: F,
) -> Result<(Snapshot, u64), AppError>
where
    F: FnOnce(Duration),
{
    source.set_services_active(options.sections.services);

    let first = source.next_snapshot()?;
    if options.sample_ms == 0 {
        return Ok((first, 0));
    }

    if options.is_replay {
        return match source.next_snapshot() {
            Ok(second) => {
                let window_ms = second.timestamp_ms.saturating_sub(first.timestamp_ms);
                Ok((second, window_ms))
            }
            Err(CollectError::EndOfStream) => Ok((first, 0)),
            Err(err) => Err(AppError::Collect(err)),
        };
    }

    let start = Instant::now();
    sleep_fn(Duration::from_millis(options.sample_ms));
    let second = source.next_snapshot()?;
    let elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(options.sample_ms);
    Ok((second, elapsed_ms))
}

/// Serializes `envelope` as compact or pretty JSON followed by a trailing newline.
pub fn write_json_envelope<W: Write>(
    writer: &mut W,
    envelope: &JsonSnapshotEnvelope,
    pretty: bool,
) -> Result<(), std::io::Error> {
    let serialized = if pretty {
        serde_json::to_string_pretty(envelope)
    } else {
        serde_json::to_string(envelope)
    }
    .map_err(std::io::Error::other)?;
    writeln!(writer, "{serialized}")?;
    writer.flush()
}

/// Writes a structured JSON error envelope to `writer` (typically `stderr`).
pub fn write_json_error<W: Write>(writer: &mut W, code: &str, message: &str, pretty: bool) {
    let envelope = JsonErrorEnvelope::new(code, message);
    let rendered = if pretty {
        serde_json::to_string_pretty(&envelope)
    } else {
        serde_json::to_string(&envelope)
    };
    if let Ok(line) = rendered {
        let _ = writeln!(writer, "{line}");
        let _ = writer.flush();
    }
}

/// Scans raw CLI arguments (before `clap` error formatting) to determine whether `--json`
/// was passed as an actual option flag rather than as a positional argument after `--` or
/// as a value consumed by a preceding value-taking option.
pub fn raw_args_request_json(args: &[OsString]) -> bool {
    let mut expecting_value = false;
    let mut saw_json = false;

    for raw in args.iter().skip(1) {
        if expecting_value {
            expecting_value = false;
            continue;
        }
        let Some(arg) = raw.to_str() else {
            continue;
        };
        if arg == "--" {
            break;
        }
        if arg == "--json" {
            saw_json = true;
            continue;
        }
        if takes_separate_long_value(arg) {
            expecting_value = true;
            continue;
        }
        if arg.starts_with('-') && !arg.starts_with("--") && arg.len() > 1 {
            expecting_value = short_cluster_expects_next_value(&arg[1..]);
        }
    }

    saw_json
}

/// Scans raw CLI arguments for `--pretty` using the same boundary and value-consumption rules.
pub fn raw_args_request_pretty(args: &[OsString]) -> bool {
    let mut expecting_value = false;
    let mut saw_pretty = false;

    for raw in args.iter().skip(1) {
        if expecting_value {
            expecting_value = false;
            continue;
        }
        let Some(arg) = raw.to_str() else {
            continue;
        };
        if arg == "--" {
            break;
        }
        if arg == "--pretty" {
            saw_pretty = true;
            continue;
        }
        if takes_separate_long_value(arg) {
            expecting_value = true;
            continue;
        }
        if arg.starts_with('-') && !arg.starts_with("--") && arg.len() > 1 {
            expecting_value = short_cluster_expects_next_value(&arg[1..]);
        }
    }

    saw_pretty
}

fn takes_separate_long_value(arg: &str) -> bool {
    matches!(
        arg,
        "--config"
            | "--filter"
            | "--preset"
            | "--themes-dir"
            | "--update"
            | "--record"
            | "--replay"
            | "--sections"
            | "--sample-ms"
            | "--top"
            | "--sort"
            | "--pid"
            | "--services-state"
    )
}

fn short_cluster_expects_next_value(cluster: &str) -> bool {
    let mut chars = cluster.chars();
    while let Some(ch) = chars.next() {
        if matches!(ch, 'c' | 'f' | 'p' | 'u') {
            // If additional characters remain in the cluster they are the attached value
            // (e.g. `-f--json`); otherwise the next argv token is the option's value.
            return chars.next().is_none();
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use btop_collect::ReplaySource;
    use btop_model::fixtures::win11_16core_dual_gpu_desktop;

    use super::*;

    fn os_args(items: &[&str]) -> Vec<OsString> {
        items.iter().map(OsString::from).collect()
    }

    #[test]
    fn raw_args_detects_json_before_and_after_errors_and_respects_boundaries() {
        assert!(raw_args_request_json(&os_args(&[
            "btop",
            "--json",
            "--unknown"
        ])));
        assert!(raw_args_request_json(&os_args(&[
            "btop",
            "--unknown",
            "--json"
        ])));
        assert!(raw_args_request_json(&os_args(&[
            "btop", "--json", "--json", "--bad"
        ])));
        assert!(raw_args_request_json(&os_args(&[
            "btop",
            "-df--json",
            "--json"
        ])));

        // Consumed as value of value-taking flag: MUST NOT trigger JSON mode.
        assert!(!raw_args_request_json(&os_args(&[
            "btop", "--filter", "--json"
        ])));
        assert!(!raw_args_request_json(&os_args(&[
            "btop", "-f", "--json", "--bad"
        ])));
        assert!(!raw_args_request_json(&os_args(&[
            "btop", "-df", "--json", "--bad"
        ])));
        assert!(!raw_args_request_json(&os_args(&[
            "btop", "--config", "--json"
        ])));
        assert!(!raw_args_request_json(&os_args(&[
            "btop",
            "--sections",
            "--json"
        ])));

        // After `--` positional separator: MUST NOT trigger JSON mode.
        assert!(!raw_args_request_json(&os_args(&["btop", "--", "--json"])));
        assert!(!raw_args_request_json(&os_args(&[
            "btop",
            "--unknown",
            "--",
            "--json"
        ])));
    }

    #[derive(Default)]
    struct RecordingSource {
        snapshots: Vec<Snapshot>,
        cursor: usize,
        services_active_calls: Vec<bool>,
    }

    impl SnapshotSource for RecordingSource {
        fn next_snapshot(&mut self) -> Result<Snapshot, CollectError> {
            if self.cursor >= self.snapshots.len() {
                return Err(CollectError::EndOfStream);
            }
            let snap = self.snapshots[self.cursor].clone();
            self.cursor += 1;
            Ok(snap)
        }

        fn set_services_active(&mut self, active: bool) {
            self.services_active_calls.push(active);
        }
    }

    #[test]
    fn collect_headless_snapshot_performs_warmup_pair_and_toggles_services() {
        let fixtures = win11_16core_dual_gpu_desktop();
        let mut src = RecordingSource {
            snapshots: fixtures.clone(),
            ..RecordingSource::default()
        };

        let mut slept = None;
        let opts = HeadlessJsonOptions {
            sections: JsonSections::parse_csv("cpu,mem").expect("sections"),
            sample_ms: 250,
            pretty: false,
            is_replay: false,
        };
        let (snap, _window) = collect_headless_snapshot(&mut src, opts, |d| {
            slept = Some(d);
        })
        .expect("collect live pair");

        assert_eq!(slept, Some(Duration::from_millis(250)));
        assert_eq!(src.cursor, 2);
        assert_eq!(snap, fixtures[1]);
        assert_eq!(src.services_active_calls, vec![false]);

        // Zero sample_ms takes a single static tick with no sleep and enables services when asked.
        let mut src_static = RecordingSource {
            snapshots: fixtures.clone(),
            ..RecordingSource::default()
        };
        let mut slept_static = false;
        let opts_static = HeadlessJsonOptions {
            sections: JsonSections::parse_csv("services").expect("services"),
            sample_ms: 0,
            pretty: false,
            is_replay: false,
        };
        let (snap0, window0) = collect_headless_snapshot(&mut src_static, opts_static, |_| {
            slept_static = true;
        })
        .expect("collect single");
        assert!(!slept_static);
        assert_eq!(src_static.cursor, 1);
        assert_eq!(window0, 0);
        assert_eq!(snap0, fixtures[0]);
        assert_eq!(src_static.services_active_calls, vec![true]);
    }

    #[test]
    fn collect_headless_snapshot_replay_does_not_sleep_and_handles_single_frame() {
        let fixtures = win11_16core_dual_gpu_desktop();
        let mut replay = ReplaySource::from_snapshots(fixtures.clone())
            .expect("replay")
            .with_looping(false);
        let mut slept = false;
        let (snap, window) = collect_headless_snapshot(
            &mut replay,
            HeadlessJsonOptions {
                sections: JsonSections::all(),
                sample_ms: 500,
                pretty: false,
                is_replay: true,
            },
            |_| slept = true,
        )
        .expect("replay two frames");
        assert!(!slept);
        assert_eq!(snap, fixtures[1]);
        assert_eq!(
            window,
            fixtures[1]
                .timestamp_ms
                .saturating_sub(fixtures[0].timestamp_ms)
        );

        let mut single = ReplaySource::from_snapshots(vec![fixtures[0].clone()])
            .expect("single")
            .with_looping(false);
        let (only, only_window) = collect_headless_snapshot(
            &mut single,
            HeadlessJsonOptions {
                sections: JsonSections::all(),
                sample_ms: 500,
                pretty: false,
                is_replay: true,
            },
            |_| slept = true,
        )
        .expect("single replay");
        assert!(!slept);
        assert_eq!(only, fixtures[0]);
        assert_eq!(only_window, 0);
    }
}
