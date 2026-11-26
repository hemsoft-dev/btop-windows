//! Headless `--json` snapshot, streaming JSONL, and time-window summary collection,
//! argv error-mode detection, and stdout/stderr formatting.

use std::ffi::OsString;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use btop_collect::{CollectError, SnapshotSource};
use btop_model::{
    HeadlessQuery, JsonErrorEnvelope, JsonSections, JsonSnapshotEnvelope, JsonSummaryEnvelope,
    QueryFilter, Snapshot,
};

use crate::AppError;

/// Default live warmup sampling window in milliseconds so delta/rate collectors populate.
pub const DEFAULT_LIVE_SAMPLE_MS: u64 = 500;
/// Maximum allowed `--sample-ms` value (10 seconds).
pub const MAX_SAMPLE_MS: u64 = 10_000;
/// Minimum allowed `--interval-ms` value for streaming and `--summary` mode.
pub const MIN_INTERVAL_MS: u64 = 100;
/// Default sampling interval in milliseconds for streaming and `--summary` mode.
pub const DEFAULT_INTERVAL_MS: u64 = 1_000;
/// Maximum allowed `--interval-ms` value (60 seconds).
pub const MAX_INTERVAL_MS: u64 = 60_000;
/// Default time window in milliseconds for live `--summary` mode when neither `--window-ms` nor `--count` is given.
pub const DEFAULT_SUMMARY_WINDOW_MS: u64 = 3_000;
/// Maximum allowed `--window-ms` value (5 minutes).
pub const MAX_SUMMARY_WINDOW_MS: u64 = 300_000;

/// Options controlling a single headless `--json` snapshot invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeadlessJsonOptions {
    pub sections: JsonSections,
    pub sample_ms: u64,
    pub pretty: bool,
    pub is_replay: bool,
}

/// Options controlling multi-sample NDJSON streaming (`--count`, `--interval-ms`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeadlessStreamOptions {
    pub sections: JsonSections,
    pub sample_ms: u64,
    pub count: usize,
    pub interval_ms: u64,
    pub is_replay: bool,
}

/// Options controlling aggregated time-window summary mode (`--summary`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeadlessSummaryOptions {
    pub sections: JsonSections,
    pub explicit_sample_ms: Option<u64>,
    pub count: Option<usize>,
    pub window_ms: Option<u64>,
    pub interval_ms: u64,
    pub include_series: bool,
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

/// Shared immutable query/version context for headless streaming and summary runs.
#[derive(Debug, Clone, Copy)]
pub struct HeadlessContext<'a> {
    pub query: &'a HeadlessQuery,
    pub filter: &'a QueryFilter,
    pub btop_version: &'a str,
}

/// Streams newline-delimited JSON snapshots to `writer`, flushing after every line.
///
/// Reuses one `SnapshotSource` instance across all ticks, stops cleanly on `BrokenPipe`,
/// `stop_flag` (`Ctrl+C`), `CollectError::EndOfStream` (in replay mode), or after emitting
/// `options.count` lines (`options.count == 0` streams continuously in live mode or drains
/// all frames in replay mode).
pub fn stream_headless_snapshots<W, F>(
    source: &mut dyn SnapshotSource,
    options: HeadlessStreamOptions,
    ctx: HeadlessContext<'_>,
    stop_flag: &AtomicBool,
    writer: &mut W,
    mut wait_fn: F,
) -> Result<usize, AppError>
where
    W: Write,
    F: FnMut(Duration) -> bool,
{
    source.set_services_active(options.sections.services);

    let mut last_tick_at = Instant::now();
    let mut prev_ts_ms: Option<u64> = None;

    // Optional live warmup baseline tick so the first emitted JSON line already has delta rates.
    if !options.is_replay && options.sample_ms > 0 {
        let baseline = source.next_snapshot()?;
        prev_ts_ms = Some(baseline.timestamp_ms);
        last_tick_at = Instant::now();
        if stop_flag.load(Ordering::SeqCst)
            || !wait_fn(Duration::from_millis(options.sample_ms))
            || stop_flag.load(Ordering::SeqCst)
        {
            return Ok(0);
        }
    }

    let interval = Duration::from_millis(options.interval_ms);
    let mut emitted = 0usize;

    while options.count == 0 || emitted < options.count {
        if stop_flag.load(Ordering::SeqCst) {
            break;
        }

        if emitted > 0 && !options.is_replay {
            let elapsed = last_tick_at.elapsed();
            if let Some(remaining) = interval.checked_sub(elapsed)
                && !remaining.is_zero()
                && (!wait_fn(remaining) || stop_flag.load(Ordering::SeqCst))
            {
                break;
            }
        }

        let snap = match source.next_snapshot() {
            Ok(snap) => snap,
            Err(CollectError::EndOfStream) => break,
            Err(err) => return Err(AppError::Collect(err)),
        };

        let sample_window_ms = if options.is_replay {
            prev_ts_ms.map_or(0, |prev| snap.timestamp_ms.saturating_sub(prev))
        } else if emitted == 0 && options.sample_ms == 0 {
            0
        } else {
            u64::try_from(last_tick_at.elapsed().as_millis()).unwrap_or(options.interval_ms)
        };
        prev_ts_ms = Some(snap.timestamp_ms);
        last_tick_at = Instant::now();

        let envelope = JsonSnapshotEnvelope::from_snapshot_with_query(
            snap,
            ctx.btop_version,
            sample_window_ms,
            options.sections,
            ctx.query,
            ctx.filter,
        );

        match write_json_envelope(writer, &envelope, false) {
            Ok(()) => emitted += 1,
            Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => break,
            Err(err) => return Err(AppError::Io(err)),
        }
    }

    Ok(emitted)
}

/// Collects a multi-sample window from one `SnapshotSource` instance and aggregates it into a
/// single [`JsonSummaryEnvelope`].
pub fn collect_headless_summary<F>(
    source: &mut dyn SnapshotSource,
    options: HeadlessSummaryOptions,
    ctx: HeadlessContext<'_>,
    stop_flag: &AtomicBool,
    mut wait_fn: F,
) -> Result<JsonSummaryEnvelope, AppError>
where
    F: FnMut(Duration) -> bool,
{
    source.set_services_active(options.sections.services);
    let mut snapshots = Vec::new();
    let window_start = Instant::now();

    if options.is_replay {
        let mut first_ts: Option<u64> = None;
        loop {
            if let Some(max_count) = options.count
                && snapshots.len() >= max_count
            {
                break;
            }
            match source.next_snapshot() {
                Ok(snap) => {
                    let start_ts = *first_ts.get_or_insert(snap.timestamp_ms);
                    let span_ms = snap.timestamp_ms.saturating_sub(start_ts);
                    snapshots.push(snap);
                    if let Some(target_window) = options.window_ms
                        && options.count.is_none()
                        && span_ms >= target_window
                    {
                        break;
                    }
                }
                Err(CollectError::EndOfStream) => break,
                Err(err) => return Err(AppError::Collect(err)),
            }
        }
    } else {
        let target_window = options
            .window_ms
            .or_else(|| options.count.is_none().then_some(DEFAULT_SUMMARY_WINDOW_MS));
        let interval = Duration::from_millis(options.interval_ms);

        // Take a baseline tick so live delta rates are populated on the first recorded sample,
        // unless the caller explicitly asked for `--sample-ms 0`.
        let warmup_ms = match options.explicit_sample_ms {
            Some(0) => 0,
            Some(ms) => ms,
            None => options.interval_ms.min(DEFAULT_LIVE_SAMPLE_MS),
        };
        if warmup_ms > 0 {
            let _baseline = source.next_snapshot()?;
            if !stop_flag.load(Ordering::SeqCst) {
                let _ = wait_fn(Duration::from_millis(warmup_ms));
            }
        }

        let collection_start = Instant::now();
        let mut last_tick_at = collection_start;

        loop {
            if stop_flag.load(Ordering::SeqCst) && !snapshots.is_empty() {
                break;
            }
            if !snapshots.is_empty() {
                let elapsed = last_tick_at.elapsed();
                if let Some(remaining) = interval.checked_sub(elapsed)
                    && !remaining.is_zero()
                    && (!wait_fn(remaining) || stop_flag.load(Ordering::SeqCst))
                {
                    break;
                }
            }

            let snap = match source.next_snapshot() {
                Ok(snap) => snap,
                Err(CollectError::EndOfStream) => break,
                Err(err) => return Err(AppError::Collect(err)),
            };
            last_tick_at = Instant::now();
            snapshots.push(snap);

            if let Some(max_count) = options.count
                && snapshots.len() >= max_count
            {
                break;
            }
            if let Some(window_ms) = target_window {
                let elapsed_ms =
                    u64::try_from(collection_start.elapsed().as_millis()).unwrap_or(window_ms);
                if elapsed_ms >= window_ms {
                    break;
                }
            }
        }
    }

    let measured_ms = u64::try_from(window_start.elapsed().as_millis()).ok();
    Ok(JsonSummaryEnvelope::from_snapshots(
        &snapshots,
        ctx.btop_version,
        measured_ms,
        options.sections,
        ctx.query,
        ctx.filter,
        options.include_series,
    ))
}

fn write_serialized_json<W: Write>(
    writer: &mut W,
    serialized: serde_json::Result<String>,
) -> Result<(), std::io::Error> {
    let mut text = serialized.map_err(std::io::Error::other)?;
    text.push('\n');
    writer.write_all(text.as_bytes())?;
    writer.flush()
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
    };
    write_serialized_json(writer, serialized)
}

/// Serializes `summary` as compact or pretty JSON followed by a trailing newline.
pub fn write_json_summary<W: Write>(
    writer: &mut W,
    summary: &JsonSummaryEnvelope,
    pretty: bool,
) -> Result<(), std::io::Error> {
    let serialized = if pretty {
        serde_json::to_string_pretty(summary)
    } else {
        serde_json::to_string(summary)
    };
    write_serialized_json(writer, serialized)
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
            | "--count"
            | "--interval-ms"
            | "--window-ms"
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
        for flag in [
            "--filter",
            "--config",
            "--sections",
            "--count",
            "--interval-ms",
            "--window-ms",
        ] {
            assert!(
                !raw_args_request_json(&os_args(&["btop", flag, "--json"])),
                "{flag} --json must not trigger json error mode"
            );
        }
        assert!(!raw_args_request_json(&os_args(&[
            "btop", "-f", "--json", "--bad"
        ])));
        assert!(!raw_args_request_json(&os_args(&[
            "btop", "-df", "--json", "--bad"
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

    #[test]
    fn stream_headless_snapshots_emits_ndjson_and_stops_cleanly_on_broken_pipe() {
        struct FailOnSecondWrite(usize);
        impl Write for FailOnSecondWrite {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0 += 1;
                if self.0 > 1 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "pipe closed",
                    ));
                }
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let fixtures = win11_16core_dual_gpu_desktop();
        let mut replay = ReplaySource::from_snapshots(fixtures.clone())
            .expect("replay")
            .with_looping(false);
        let stop = AtomicBool::new(false);
        let mut buf = Vec::new();
        let query = HeadlessQuery::default();
        let ctx = HeadlessContext {
            query: &query,
            filter: &QueryFilter::None,
            btop_version: "0.1.1",
        };
        let emitted = stream_headless_snapshots(
            &mut replay,
            HeadlessStreamOptions {
                sections: JsonSections::parse_csv("cpu,mem").expect("sections"),
                sample_ms: 0,
                count: 2,
                interval_ms: 500,
                is_replay: true,
            },
            ctx,
            &stop,
            &mut buf,
            |_| true,
        )
        .expect("stream replay");
        assert_eq!(emitted, 2);
        let lines: Vec<&str> = std::str::from_utf8(&buf).expect("utf8").lines().collect();
        assert_eq!(lines.len(), 2);
        for line in lines {
            let parsed: JsonSnapshotEnvelope = serde_json::from_str(line).expect("valid json line");
            assert!(parsed.cpu.is_some());
            assert!(parsed.disks.is_none());
        }

        // BrokenPipe stops cleanly without error.
        let mut replay2 = ReplaySource::from_snapshots(fixtures)
            .expect("replay2")
            .with_looping(false);
        let mut broken = FailOnSecondWrite(0);
        let emitted_before_pipe = stream_headless_snapshots(
            &mut replay2,
            HeadlessStreamOptions {
                sections: JsonSections::parse_csv("cpu").expect("cpu"),
                sample_ms: 0,
                count: 0,
                interval_ms: 100,
                is_replay: true,
            },
            ctx,
            &stop,
            &mut broken,
            |_| true,
        )
        .expect("broken pipe must return Ok");
        assert_eq!(emitted_before_pipe, 1);
    }

    #[test]
    fn collect_headless_summary_aggregates_replay_without_sleeping() {
        let fixtures = win11_16core_dual_gpu_desktop();
        let mut replay = ReplaySource::from_snapshots(fixtures.clone())
            .expect("replay")
            .with_looping(false);
        let stop = AtomicBool::new(false);
        let mut slept = false;
        let query = HeadlessQuery::default();
        let ctx = HeadlessContext {
            query: &query,
            filter: &QueryFilter::None,
            btop_version: "0.1.1",
        };
        let summary = collect_headless_summary(
            &mut replay,
            HeadlessSummaryOptions {
                sections: JsonSections::all(),
                explicit_sample_ms: None,
                count: None,
                window_ms: None,
                interval_ms: 500,
                include_series: true,
                pretty: false,
                is_replay: true,
            },
            ctx,
            &stop,
            |_| {
                slept = true;
                true
            },
        )
        .expect("summary");
        assert!(!slept);
        assert_eq!(summary.sample_count, fixtures.len());
        assert_eq!(summary.series.as_ref().map(Vec::len), Some(fixtures.len()));
    }
}
