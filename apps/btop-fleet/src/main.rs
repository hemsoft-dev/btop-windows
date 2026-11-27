//! Cross-platform Unix entry point (`btop-fleet`), avoiding collision with upstream `btop++`.

use std::process::ExitCode;

fn main() -> ExitCode {
    btop_windows::cli::run_cli("btop-fleet")
}
