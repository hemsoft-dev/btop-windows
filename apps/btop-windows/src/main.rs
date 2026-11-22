//! Entry point for `btop-windows`.

use std::path::PathBuf;
use std::process::ExitCode;

use btop_config::Config;
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

    let _config = Config {
        update_ms: cli.update_ms.map_or(2000, |ms| ms.max(100)),
        force_tty: cli.tty,
        truecolor: !cli.low_color,
        ..Config::default()
    };

    println!(
        "btop-windows {} scaffold initialized. Live TUI loop arrives in Milestone M1.",
        env!("CARGO_PKG_VERSION")
    );
    ExitCode::SUCCESS
}
