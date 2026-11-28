//! Windows entry point (`btop.exe`) with native local process and service actions.

use std::process::ExitCode;

#[cfg(windows)]
mod win_actions {
    use btop_sys::{
        PriorityClass, ServiceActionError, graceful_close_process, query_process_priority,
        set_process_priority, start_windows_service, stop_windows_service, terminate_process,
    };
    use btop_ui::UiPriorityClass;
    use btop_windows::{LiveActionHooks, PriorityOutcome};

    const fn ui_to_sys_priority(class: UiPriorityClass) -> PriorityClass {
        match class {
            UiPriorityClass::Idle => PriorityClass::Idle,
            UiPriorityClass::BelowNormal => PriorityClass::BelowNormal,
            UiPriorityClass::Normal => PriorityClass::Normal,
            UiPriorityClass::AboveNormal => PriorityClass::AboveNormal,
            UiPriorityClass::High => PriorityClass::High,
            UiPriorityClass::Realtime => PriorityClass::Realtime,
        }
    }

    const fn sys_to_ui_priority(class: PriorityClass) -> UiPriorityClass {
        match class {
            PriorityClass::Idle => UiPriorityClass::Idle,
            PriorityClass::BelowNormal => UiPriorityClass::BelowNormal,
            PriorityClass::Normal => UiPriorityClass::Normal,
            PriorityClass::AboveNormal => UiPriorityClass::AboveNormal,
            PriorityClass::High => UiPriorityClass::High,
            PriorityClass::Realtime => UiPriorityClass::Realtime,
        }
    }

    fn do_terminate(pid: u32, create_time_100ns: u64) -> Result<(), String> {
        terminate_process(pid, create_time_100ns).map_err(|err| err.to_string())
    }

    fn do_graceful_close(pid: u32, create_time_100ns: u64) -> Result<usize, String> {
        graceful_close_process(pid, create_time_100ns).map_err(|err| err.to_string())
    }

    fn do_query_priority(pid: u32, create_time_100ns: u64) -> Result<UiPriorityClass, String> {
        query_process_priority(pid, create_time_100ns)
            .map(sys_to_ui_priority)
            .map_err(|err| err.to_string())
    }

    fn do_set_priority(
        pid: u32,
        create_time_100ns: u64,
        class: UiPriorityClass,
    ) -> Result<PriorityOutcome, String> {
        set_process_priority(pid, create_time_100ns, ui_to_sys_priority(class))
            .map(|outcome| PriorityOutcome {
                requested_label: outcome.requested.label(),
                applied_label: outcome.applied.label(),
                clamped: outcome.applied != outcome.requested,
            })
            .map_err(|err| err.to_string())
    }

    fn do_start_service(name: &str) -> Result<(), ServiceActionError> {
        start_windows_service(name)
    }

    fn do_stop_service(name: &str) -> Result<(), ServiceActionError> {
        stop_windows_service(name)
    }

    pub const LIVE_ACTION_HOOKS: LiveActionHooks = LiveActionHooks {
        terminate_process: do_terminate,
        graceful_close_process: do_graceful_close,
        query_process_priority: do_query_priority,
        set_process_priority: do_set_priority,
        start_service: do_start_service,
        stop_service: do_stop_service,
    };
}

fn main() -> ExitCode {
    #[cfg(windows)]
    {
        btop_windows::cli::run_cli("btop", Some(win_actions::LIVE_ACTION_HOOKS))
    }
    #[cfg(not(windows))]
    {
        btop_windows::cli::run_cli_read_only("btop")
    }
}
