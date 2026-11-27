//! Windows entry point (`btop.exe`).

use std::process::ExitCode;

fn main() -> ExitCode {
    btop_windows::cli::run_cli("btop")
}
