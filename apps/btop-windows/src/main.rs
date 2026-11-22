//! Entry point for `btop-windows`.

use std::path::PathBuf;
use std::process::ExitCode;

use btop_collect::{LiveCollector, ReplaySource, SnapshotSource};
use btop_config::Config;
use btop_model::SnapshotRecorder;
use btop_term::CrosstermInput;
use btop_windows::{ConfigOverrides, load_config, run_tui_loop};
use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "btop-windows",
    version,
    about = "Native Windows terminal resource monitor inspired by aristocratos/btop"
)]
struct Cli {
    /// Path to a config file
    #[arg(short = 'c', long = "config", value_name = "FILE")]
    config_file: Option<PathBuf>,

    /// Start in debug mode with additional logs and metrics
    #[arg(short = 'd', long = "debug")]
    debug: bool,

    /// Set an initial process filter (prefix with '!' for regex)
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
    #[arg(long = "default-config")]
    default_config: bool,

    /// Record timestamped Snapshot JSONL stream to file
    #[arg(long = "record", value_name = "FILE")]
    record: Option<PathBuf>,

    /// Replay a recorded Snapshot JSONL stream instead of querying live Windows APIs
    #[arg(long = "replay", value_name = "FILE")]
    replay: Option<PathBuf>,
}

fn main() -> ExitCode {
    btop_sys::harden_process_startup();

    let cli = Cli::parse();

    if cli.default_config {
        print!("{}", Config::render_default_conf());
        return ExitCode::SUCCESS;
    }

    let overrides = ConfigOverrides {
        update_ms: cli.update_ms,
        force_tty: cli.tty,
        low_color: cli.low_color,
    };

    let config = match load_config(cli.config_file.as_deref(), &overrides) {
        Ok(cfg) => cfg,
        Err(err) => {
            eprintln!("error: failed to load config: {err}");
            return ExitCode::FAILURE;
        }
    };

    let source: Box<dyn SnapshotSource> = if let Some(replay_path) = cli.replay.as_deref() {
        match ReplaySource::from_path(replay_path) {
            Ok(replay) => Box::new(replay),
            Err(err) => {
                eprintln!(
                    "error: failed to load --replay '{}': {err}",
                    replay_path.display()
                );
                return ExitCode::FAILURE;
            }
        }
    } else {
        match LiveCollector::new() {
            Ok(live) => Box::new(live),
            Err(err) => {
                eprintln!("error: failed to initialize live telemetry collector: {err}");
                return ExitCode::FAILURE;
            }
        }
    };

    let recorder = if let Some(record_path) = cli.record.as_deref() {
        match SnapshotRecorder::open(record_path) {
            Ok(rec) => Some(rec),
            Err(err) => {
                eprintln!(
                    "error: failed to open --record '{}': {err}",
                    record_path.display()
                );
                return ExitCode::FAILURE;
            }
        }
    } else {
        None
    };

    let mut input = CrosstermInput;
    match run_tui_loop(
        source,
        recorder,
        &mut input,
        config,
        cli.config_file.as_deref(),
        &overrides,
    ) {
        Ok(_) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}
