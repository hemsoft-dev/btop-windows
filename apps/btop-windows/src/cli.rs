//! Shared CLI entry point used by both `btop` (Windows) and `btop-fleet` (Unix).

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use btop_collect::{FreqMode, LiveCollector, ReplaySource, SnapshotSource};
use btop_config::Config;
use btop_model::{
    HeadlessQuery, JsonSections, JsonSnapshotEnvelope, ProcSortField, QueryFilter, SchemaMode,
    ServiceStateFilter, build_json_schema,
};
use btop_sys::{TimerWake, UnbufferedStdout, WaitableWakeTimer, install_headless_ctrl_c_handler};
use btop_term::CrosstermInput;
use clap::error::ErrorKind;
use clap::{CommandFactory as _, FromArgMatches as _, Parser};

use crate::headless::{
    DEFAULT_INTERVAL_MS, DEFAULT_LIVE_SAMPLE_MS, HeadlessContext, HeadlessJsonOptions,
    HeadlessStreamOptions, HeadlessSummaryOptions, MAX_INTERVAL_MS, MAX_SAMPLE_MS,
    MAX_SUMMARY_WINDOW_MS, MIN_INTERVAL_MS, collect_headless_snapshot, collect_headless_summary,
    raw_args_request_json, raw_args_request_pretty, stream_headless_snapshots, write_json_envelope,
    write_json_error, write_json_summary, write_json_summary_interruptible,
};
use crate::{AppError, ConfigOverrides, load_config, run_tui_loop};

#[derive(Debug, Parser)]
#[command(
    name = "btop",
    version,
    about = "Native terminal resource monitor inspired by aristocratos/btop"
)]
struct Cli {
    /// Path to a config file
    #[arg(short = 'c', long = "config", value_name = "FILE")]
    config_file: Option<PathBuf>,

    /// Start in debug mode with additional logs and metrics
    #[arg(short = 'd', long = "debug")]
    debug: bool,

    /// Set a process/service filter (prefix with '!' for regex)
    #[arg(short = 'f', long = "filter", value_name = "FILTER")]
    filter: Option<String>,

    /// Override automatic UTF locale/capability detection
    #[arg(long = "force-utf")]
    force_utf: bool,

    /// Disable true color, use 256 colors only
    #[arg(short = 'l', long = "low-color")]
    low_color: bool,

    /// Start with a preset (0-9)
    #[arg(short = 'p', long = "preset", value_parser = clap::value_parser!(u8).range(0..=9))]
    preset: Option<u8>,

    /// Force TTY mode with ASCII graph symbols and 16 colors only
    #[arg(short = 't', long = "tty", conflicts_with = "no_tty")]
    tty: bool,

    /// Force disable TTY mode
    #[arg(long = "no-tty", conflicts_with = "tty")]
    no_tty: bool,

    /// Path to a custom themes directory
    #[arg(long = "themes-dir", value_name = "DIR")]
    themes_dir: Option<PathBuf>,

    /// Set initial update rate in milliseconds (>= 100)
    #[arg(short = 'u', long = "update", value_name = "MS")]
    update_ms: Option<u32>,

    /// Print default config to standard output and exit
    #[arg(long = "default-config", conflicts_with = "json")]
    default_config: bool,

    /// Record timestamped Snapshot JSONL stream to file
    #[arg(long = "record", value_name = "FILE", conflicts_with = "json")]
    record: Option<PathBuf>,

    /// Replay a recorded Snapshot JSONL stream instead of querying live OS telemetry
    #[arg(long = "replay", value_name = "FILE")]
    replay: Option<PathBuf>,

    /// Emit structured JSON telemetry to stdout without starting the interactive TUI
    #[arg(long = "json")]
    json: bool,

    /// Pretty-print JSON output (requires --json)
    #[arg(long = "pretty", requires = "json")]
    pretty: bool,

    /// Comma-separated telemetry sections for --json (cpu,mem,disks,net,procs,services,gpus,battery,all)
    #[arg(long = "sections", value_name = "LIST", requires = "json")]
    sections: Option<String>,

    /// Warmup sampling window in milliseconds for --json live delta metrics (0-10000, default 500)
    #[arg(
        long = "sample-ms",
        value_name = "MS",
        requires = "json",
        value_parser = clap::value_parser!(u64).range(0..=MAX_SAMPLE_MS)
    )]
    sample_ms: Option<u64>,

    /// Limit returned processes (or top-level trees with --tree) in --json mode (0 = unlimited)
    #[arg(long = "top", value_name = "N", requires = "json")]
    top: Option<usize>,

    /// Sort field for --json process output (cpu, mem, pid, name, threads, io)
    #[arg(long = "sort", value_name = "FIELD", requires = "json")]
    sort: Option<String>,

    /// Reverse the process sort order in --json mode
    #[arg(long = "reverse", requires = "json")]
    reverse: bool,

    /// Scope --json process output to a PID plus its verified ancestors and descendants
    #[arg(long = "pid", value_name = "PID", requires = "json")]
    pid: Option<u32>,

    /// Emit nested parent-child process tree graph with subtree totals in --json mode
    #[arg(long = "tree", requires = "json")]
    tree: bool,

    /// Filter Windows Services by state in --json mode (all, running, stopped)
    #[arg(long = "services-state", value_name = "STATE", requires = "json")]
    services_state: Option<String>,

    /// Number of samples to stream as NDJSON (0 = stream until Ctrl+C or broken pipe; requires --json)
    #[arg(long = "count", value_name = "N", requires = "json")]
    count: Option<usize>,

    /// Sampling interval in milliseconds for --count streaming or --summary mode (100-60000, default 1000)
    #[arg(
        long = "interval-ms",
        value_name = "MS",
        requires = "json",
        value_parser = clap::value_parser!(u64).range(MIN_INTERVAL_MS..=MAX_INTERVAL_MS)
    )]
    interval_ms: Option<u64>,

    /// Aggregate a multi-sample time window into one compact JSON summary object (requires --json)
    #[arg(long = "summary", requires = "json")]
    summary: bool,

    /// Target window duration in milliseconds for --summary mode (100-300000, default 3000 live)
    #[arg(
        long = "window-ms",
        value_name = "MS",
        requires = "summary",
        value_parser = clap::value_parser!(u64).range(MIN_INTERVAL_MS..=MAX_SUMMARY_WINDOW_MS)
    )]
    window_ms: Option<u64>,

    /// Include a compact timestamped scalar series array inside --summary JSON output
    #[arg(long = "include-series", requires = "summary")]
    include_series: bool,

    /// Attach pure deterministic system health and bottleneck diagnostics to --json output
    #[arg(long = "diagnose", requires = "json")]
    diagnose: bool,

    /// Print Draft 2020-12 JSON Schema for headless --json output and exit (all, snapshot, summary, diagnose, error)
    #[arg(
        long = "schema",
        value_name = "MODE",
        num_args = 0..=1,
        default_missing_value = "all",
        conflicts_with_all = ["json", "default_config", "record"]
    )]
    schema: Option<String>,
}

fn emit_error(json_mode: bool, pretty: bool, code: &str, message: &str) {
    if json_mode {
        write_json_error(&mut io::stderr().lock(), code, message, pretty);
    } else {
        eprintln!("error: {message}");
    }
}

fn parse_cli_or_exit(bin_name: &'static str, raw_args: &[OsString]) -> Result<Cli, ExitCode> {
    let matches_res = Cli::command()
        .name(bin_name)
        .bin_name(bin_name)
        .try_get_matches_from(raw_args);
    match matches_res.and_then(|matches| Cli::from_arg_matches(&matches)) {
        Ok(cli) => Ok(cli),
        Err(err) => {
            if matches!(
                err.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) {
                let _ = err.print();
                return Err(ExitCode::SUCCESS);
            }
            if raw_args_request_json(raw_args) {
                let pretty = raw_args_request_pretty(raw_args);
                let message = err.render().to_string();
                write_json_error(
                    &mut io::stderr().lock(),
                    "cli_usage",
                    message.trim(),
                    pretty,
                );
                return Err(ExitCode::from(2));
            }
            let _ = err.print();
            let code = u8::try_from(err.exit_code()).unwrap_or(2);
            Err(ExitCode::from(code))
        }
    }
}

fn parse_headless_params(
    cli: &Cli,
) -> Result<(JsonSections, HeadlessQuery, QueryFilter), ExitCode> {
    if cli.summary && cli.count == Some(0) {
        emit_error(
            true,
            cli.pretty,
            "cli_usage",
            "--count 0 (continuous streaming) cannot be combined with --summary; specify a positive --count or --window-ms",
        );
        return Err(ExitCode::from(2));
    }

    if !cli.summary && cli.count.is_some() && cli.pretty {
        emit_error(
            true,
            cli.pretty,
            "cli_usage",
            "--pretty cannot be combined with streaming --count without --summary (NDJSON requires one JSON object per line)",
        );
        return Err(ExitCode::from(2));
    }

    let sections = match cli.sections.as_deref() {
        Some(spec) => match JsonSections::parse_csv(spec) {
            Ok(parsed) => parsed,
            Err(msg) => {
                emit_error(true, cli.pretty, "cli_usage", &msg);
                return Err(ExitCode::from(2));
            }
        },
        None => JsonSections::all(),
    };

    if cli.summary
        && !sections.cpu
        && !sections.mem
        && !sections.disks
        && !sections.net
        && !sections.procs
        && !sections.gpus
        && !sections.battery
    {
        emit_error(
            true,
            cli.pretty,
            "cli_usage",
            "--summary does not support --sections services alone; include at least one time-series section (cpu, mem, disks, net, procs, gpus, battery)",
        );
        return Err(ExitCode::from(2));
    }

    let sort = match cli.sort.as_deref() {
        Some(val) => match ProcSortField::parse_str(val) {
            Ok(field) => field,
            Err(msg) => {
                emit_error(true, cli.pretty, "cli_usage", &msg);
                return Err(ExitCode::from(2));
            }
        },
        None => ProcSortField::Cpu,
    };

    let services_state = match cli.services_state.as_deref() {
        Some(val) => match ServiceStateFilter::parse_str(val) {
            Ok(state) => state,
            Err(msg) => {
                emit_error(true, cli.pretty, "cli_usage", &msg);
                return Err(ExitCode::from(2));
            }
        },
        None => ServiceStateFilter::All,
    };

    let compiled_filter = match QueryFilter::compile(cli.filter.as_deref()) {
        Ok(compiled) => compiled,
        Err(msg) => {
            emit_error(true, cli.pretty, "cli_usage", &msg);
            return Err(ExitCode::from(2));
        }
    };

    let query = HeadlessQuery {
        top: cli.top,
        sort,
        reversed: cli.reverse,
        filter: cli.filter.clone(),
        pid: cli.pid,
        tree: cli.tree,
        services_state,
    };
    Ok((sections, query, compiled_filter))
}

fn build_snapshot_source(cli: &Cli, config: &Config) -> Result<Box<dyn SnapshotSource>, ExitCode> {
    if let Some(replay_path) = cli.replay.as_deref() {
        match ReplaySource::from_path(replay_path) {
            Ok(replay) => Ok(Box::new(if cli.json {
                replay.with_looping(false)
            } else {
                replay
            })),
            Err(err) => {
                emit_error(
                    cli.json,
                    cli.pretty,
                    "replay_error",
                    &format!("failed to load --replay '{}': {err}", replay_path.display()),
                );
                Err(ExitCode::FAILURE)
            }
        }
    } else {
        let freq_mode = FreqMode::from_config_str(&config.freq_mode);
        match LiveCollector::with_options(freq_mode, config.nvml_measure_pcie_speeds) {
            Ok(live) => Ok(Box::new(live)),
            Err(err) => {
                emit_error(
                    cli.json,
                    cli.pretty,
                    "collector_error",
                    &format!("failed to initialize live telemetry collector: {err}"),
                );
                Err(ExitCode::FAILURE)
            }
        }
    }
}

/// Runs the CLI application under the given binary name (`"btop"` on Windows) with live
/// process/service actions enabled when compiled with the `live-actions` feature.
pub fn run_cli(bin_name: &'static str) -> ExitCode {
    run_cli_with_actions(bin_name, cfg!(feature = "live-actions"))
}

/// Runs the CLI application in permanently read-only mode (`"btop-fleet"`), disabling all live
/// process termination, priority changes, and service start/stop actions even if workspace
/// feature unification compiles the `live-actions` feature into `btop-windows`.
pub fn run_cli_read_only(bin_name: &'static str) -> ExitCode {
    run_cli_with_actions(bin_name, false)
}

fn run_cli_with_actions(bin_name: &'static str, enable_live_actions: bool) -> ExitCode {
    btop_sys::harden_process_startup();

    let raw_args: Vec<OsString> = std::env::args_os().collect();
    let cli = match parse_cli_or_exit(bin_name, &raw_args) {
        Ok(cli) => cli,
        Err(code) => return code,
    };

    if cli.default_config {
        print!("{}", Config::render_default_conf());
        return ExitCode::SUCCESS;
    }

    if let Some(mode_raw) = cli.schema.as_deref() {
        return run_schema_output(mode_raw);
    }

    let headless_params = if cli.json {
        match parse_headless_params(&cli) {
            Ok(params) => Some(params),
            Err(code) => return code,
        }
    } else {
        None
    };

    let overrides = ConfigOverrides {
        update_ms: cli.update_ms,
        preset: cli.preset,
        filter: cli.filter.clone(),
        themes_dir: cli.themes_dir.clone(),
        force_tty: cli.tty,
        low_color: cli.low_color,
    };

    let config = match load_config(cli.config_file.as_deref(), &overrides) {
        Ok(cfg) => cfg,
        Err(err) => {
            emit_error(
                cli.json,
                cli.pretty,
                "config_error",
                &format!("failed to load config: {err}"),
            );
            return ExitCode::FAILURE;
        }
    };

    let is_replay = cli.replay.is_some();
    let mut source = match build_snapshot_source(&cli, &config) {
        Ok(src) => src,
        Err(code) => return code,
    };

    if let Some((sections, query, compiled_filter)) = headless_params {
        return dispatch_headless(
            source.as_mut(),
            &cli,
            is_replay,
            sections,
            &query,
            &compiled_filter,
        );
    }

    let mut input = CrosstermInput;
    match run_tui_loop(
        source,
        cli.record.as_deref(),
        &mut input,
        config,
        cli.config_file.as_deref(),
        &overrides,
        enable_live_actions,
    ) {
        Ok(_) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run_schema_output(mode_raw: &str) -> ExitCode {
    let mode = match SchemaMode::parse_str(mode_raw) {
        Ok(mode) => mode,
        Err(msg) => {
            eprintln!("error: {msg}");
            return ExitCode::from(2);
        }
    };
    let schema = build_json_schema(mode);
    match serde_json::to_string_pretty(&schema) {
        Ok(text) => {
            use std::io::Write as _;
            let mut stdout = io::stdout().lock();
            if let Err(err) = writeln!(stdout, "{text}").and_then(|()| stdout.flush()) {
                if err.kind() == io::ErrorKind::BrokenPipe {
                    return ExitCode::SUCCESS;
                }
                eprintln!("error: failed to write JSON schema: {err}");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("error: failed to serialize JSON schema: {err}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch_headless(
    source: &mut dyn SnapshotSource,
    cli: &Cli,
    is_replay: bool,
    sections: JsonSections,
    query: &HeadlessQuery,
    compiled_filter: &QueryFilter,
) -> ExitCode {
    let ctx = HeadlessContext {
        query,
        filter: compiled_filter,
        btop_version: env!("CARGO_PKG_VERSION"),
    };

    if cli.summary {
        let options = HeadlessSummaryOptions {
            sections,
            explicit_sample_ms: cli.sample_ms,
            count: cli.count,
            window_ms: cli.window_ms,
            interval_ms: cli.interval_ms.unwrap_or(DEFAULT_INTERVAL_MS),
            include_series: cli.include_series,
            pretty: cli.pretty,
            is_replay,
            diagnose: cli.diagnose,
        };
        return run_headless_summary(source, options, ctx);
    }

    if let Some(count) = cli.count {
        let sample_ms = cli
            .sample_ms
            .unwrap_or(if is_replay { 0 } else { DEFAULT_LIVE_SAMPLE_MS });
        let options = HeadlessStreamOptions {
            sections,
            sample_ms,
            count,
            interval_ms: cli.interval_ms.unwrap_or(DEFAULT_INTERVAL_MS),
            is_replay,
            diagnose: cli.diagnose,
        };
        return run_headless_stream(source, options, ctx);
    }

    let sample_ms = cli
        .sample_ms
        .unwrap_or(if is_replay { 0 } else { DEFAULT_LIVE_SAMPLE_MS });
    let options = HeadlessJsonOptions {
        sections,
        sample_ms,
        pretty: cli.pretty,
        is_replay,
        diagnose: cli.diagnose,
    };
    run_headless_json(source, options, query, compiled_filter)
}

fn setup_interruptible_wait() -> (Arc<AtomicBool>, Box<dyn FnMut(Duration) -> bool>) {
    if let Ok((timer, waker)) = WaitableWakeTimer::new() {
        let stop_flag = install_headless_ctrl_c_handler(waker);
        let stop_check = Arc::clone(&stop_flag);
        let waiter = Box::new(move |dur: Duration| {
            if stop_check.load(Ordering::SeqCst) {
                return false;
            }
            match timer.wait(dur) {
                Ok(TimerWake::Elapsed) => !stop_check.load(Ordering::SeqCst),
                Ok(TimerWake::Woken) | Err(_) => false,
            }
        });
        (stop_flag, waiter)
    } else {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_check = Arc::clone(&stop_flag);
        let waiter = Box::new(move |dur: Duration| {
            std::thread::sleep(dur);
            !stop_check.load(Ordering::SeqCst)
        });
        (stop_flag, waiter)
    }
}

fn run_headless_json(
    source: &mut dyn SnapshotSource,
    options: HeadlessJsonOptions,
    query: &HeadlessQuery,
    compiled_filter: &QueryFilter,
) -> ExitCode {
    let (snapshot, sample_window_ms) =
        match collect_headless_snapshot(source, options, std::thread::sleep) {
            Ok(result) => result,
            Err(err) => {
                emit_error(
                    true,
                    options.pretty,
                    "collector_error",
                    &format!("failed to collect telemetry snapshot: {err}"),
                );
                return ExitCode::FAILURE;
            }
        };

    let envelope = JsonSnapshotEnvelope::from_snapshot_with_options(
        snapshot,
        env!("CARGO_PKG_VERSION"),
        sample_window_ms,
        options.sections,
        query,
        compiled_filter,
        options.diagnose,
    );

    if let Err(err) = write_json_envelope(&mut io::stdout().lock(), &envelope, options.pretty) {
        if err.kind() == io::ErrorKind::BrokenPipe {
            return ExitCode::SUCCESS;
        }
        emit_error(
            true,
            options.pretty,
            "io_error",
            &format!("failed to write JSON output: {err}"),
        );
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}

fn run_headless_stream(
    source: &mut dyn SnapshotSource,
    options: HeadlessStreamOptions,
    ctx: HeadlessContext<'_>,
) -> ExitCode {
    let (stop_flag, mut waiter) = setup_interruptible_wait();
    let mut stdout = UnbufferedStdout;
    match stream_headless_snapshots(source, options, ctx, &stop_flag, &mut stdout, &mut waiter) {
        Ok(_) => ExitCode::SUCCESS,
        Err(AppError::Io(err)) => {
            emit_error(
                true,
                false,
                "io_error",
                &format!("failed to write streaming JSON output: {err}"),
            );
            ExitCode::FAILURE
        }
        Err(err) => {
            emit_error(
                true,
                false,
                "collector_error",
                &format!("failed while streaming JSON telemetry: {err}"),
            );
            ExitCode::FAILURE
        }
    }
}

fn run_headless_summary(
    source: &mut dyn SnapshotSource,
    options: HeadlessSummaryOptions,
    ctx: HeadlessContext<'_>,
) -> ExitCode {
    let (stop_flag, mut waiter) = setup_interruptible_wait();
    let summary = match collect_headless_summary(source, options, ctx, &stop_flag, &mut waiter) {
        Ok(summary) => summary,
        Err(err) => {
            emit_error(
                true,
                options.pretty,
                "collector_error",
                &format!("failed while collecting summary window: {err}"),
            );
            return ExitCode::FAILURE;
        }
    };

    let stopped_during_collection =
        stop_flag.load(Ordering::SeqCst) || btop_sys::headless_signal_received();
    if summary.sample_count == 0 && stopped_during_collection {
        return ExitCode::SUCCESS;
    }

    // When a termination signal interrupted collection after at least one sample was captured,
    // emit the partial summary directly instead of aborting on the already-raised stop flag.
    // Otherwise use the interruptible writer so a blocked stdout pipe stops promptly on signal.
    let write_res = if stopped_during_collection {
        write_json_summary(&mut io::stdout().lock(), &summary, options.pretty)
    } else {
        let mut stdout = UnbufferedStdout;
        write_json_summary_interruptible(&mut stdout, &summary, options.pretty, &stop_flag)
    };
    if let Err(err) = write_res {
        if err.kind() == io::ErrorKind::BrokenPipe
            || (!stopped_during_collection
                && err.kind() == io::ErrorKind::Interrupted
                && (stop_flag.load(Ordering::SeqCst) || btop_sys::headless_signal_received()))
        {
            return ExitCode::SUCCESS;
        }
        emit_error(
            true,
            options.pretty,
            "io_error",
            &format!("failed to write JSON summary output: {err}"),
        );
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}
