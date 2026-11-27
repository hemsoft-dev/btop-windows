//! Cross-platform entry point (`btop-fleet`), non-colliding with upstream `btop++`.
//!
//! On Linux and macOS (`cfg(unix)`), wires guarded local POSIX process signals (`SIGTERM` /
//! `SIGKILL` verified against target start time and rejecting PID 1 and kernel threads).
//! On Windows (`cfg(not(unix))`), remains permanently read-only and never links Windows
//! process-termination or SCM service-control code.

use std::process::ExitCode;

#[cfg(unix)]
mod unix_actions {
    use btop_sys::{ServiceActionError, graceful_close_process, terminate_process};
    use btop_ui::UiPriorityClass;
    use btop_windows::{LiveActionHooks, PriorityOutcome};

    fn do_sigkill(pid: u32, create_time_100ns: u64) -> Result<(), String> {
        terminate_process(pid, create_time_100ns).map_err(|err| err.to_string())
    }

    fn do_sigterm(pid: u32, create_time_100ns: u64) -> Result<usize, String> {
        graceful_close_process(pid, create_time_100ns).map_err(|err| err.to_string())
    }

    fn unsupported_query_priority(_pid: u32, _create_time: u64) -> Result<UiPriorityClass, String> {
        Err("Process priority controls are not supported on Linux or macOS in v1".to_owned())
    }

    fn unsupported_set_priority(
        _pid: u32,
        _create_time: u64,
        _class: UiPriorityClass,
    ) -> Result<PriorityOutcome, String> {
        Err("Process priority controls are not supported on Linux or macOS in v1".to_owned())
    }

    fn unsupported_service(_name: &str) -> Result<(), ServiceActionError> {
        Err(ServiceActionError::Platform(
            "OS service controls are not supported on Linux or macOS in v1".to_owned(),
        ))
    }

    pub const UNIX_LIVE_ACTIONS: LiveActionHooks = LiveActionHooks {
        terminate_process: do_sigkill,
        graceful_close_process: do_sigterm,
        query_process_priority: unsupported_query_priority,
        set_process_priority: unsupported_set_priority,
        start_service: unsupported_service,
        stop_service: unsupported_service,
    };
}

fn main() -> ExitCode {
    #[cfg(unix)]
    {
        btop_windows::cli::run_cli("btop-fleet", Some(unix_actions::UNIX_LIVE_ACTIONS))
    }
    #[cfg(not(unix))]
    {
        btop_windows::cli::run_cli_read_only("btop-fleet")
    }
}
