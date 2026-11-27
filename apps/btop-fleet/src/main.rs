//! Cross-platform entry point (`btop-fleet`), permanently read-only and non-colliding
//! with upstream `btop++`.

use std::process::ExitCode;

fn main() -> ExitCode {
    btop_windows::cli::run_cli_read_only("btop-fleet")
}
