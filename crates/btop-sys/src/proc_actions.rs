//! Safe, handle-verified Windows process actions:
//! - Graceful close (`WM_CLOSE` to top-level windows via `EnumWindows`)
//! - Guarded termination (`CreateTime` verification + `IsProcessCritical` BSOD guard + `TerminateProcess`)
//! - Priority class inspection & update (`GetPriorityClass` / `SetPriorityClass`)
//!
//! Every action opens the target process with the minimum required access mask, verifies
//! `GetProcessTimes` `CreateTime` **on that opened handle**, and aborts safely on mismatch.

use std::fmt;

/// Supported Windows process priority classes exposed by the `N` priority dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriorityClass {
    Idle,
    BelowNormal,
    Normal,
    AboveNormal,
    High,
    Realtime,
}

impl PriorityClass {
    pub const ALL: [Self; 6] = [
        Self::Idle,
        Self::BelowNormal,
        Self::Normal,
        Self::AboveNormal,
        Self::High,
        Self::Realtime,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::BelowNormal => "Below Normal",
            Self::Normal => "Normal",
            Self::AboveNormal => "Above Normal",
            Self::High => "High",
            Self::Realtime => "Realtime",
        }
    }

    pub const fn to_win32_flag(self) -> u32 {
        match self {
            Self::Idle => 0x0000_0040,
            Self::BelowNormal => 0x0000_4000,
            Self::Normal => 0x0000_0020,
            Self::AboveNormal => 0x0000_8000,
            Self::High => 0x0000_0080,
            Self::Realtime => 0x0000_0100,
        }
    }

    pub const fn from_win32_flag(flag: u32) -> Option<Self> {
        match flag {
            0x0000_0040 => Some(Self::Idle),
            0x0000_4000 => Some(Self::BelowNormal),
            0x0000_0020 => Some(Self::Normal),
            0x0000_8000 => Some(Self::AboveNormal),
            0x0000_0080 => Some(Self::High),
            0x0000_0100 => Some(Self::Realtime),
            _ => None,
        }
    }
}

/// Structured outcome errors for safe Windows process actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcActionError {
    /// Target is a kernel or critical Windows process (`IsProcessCritical == TRUE`); refused to prevent BSOD.
    CriticalProcess(u32),
    /// Target is the running `btop-windows` process itself; refused so terminal restoration always runs.
    SelfProcess(u32),
    /// Process creation time on the opened handle did not match the selected `ProcessIdentity` (PID reused).
    IdentityMismatch { pid: u32 },
    /// Caller lacks sufficient privileges (or UIPI blocked `WM_CLOSE` to an elevated window).
    AccessDenied(u32),
    /// Target process does not have any top-level window for `WM_CLOSE`.
    NoWindow(u32),
    /// Target process already exited or PID does not exist.
    NotFound(u32),
    /// Other Win32 / platform error.
    Platform(String),
}

impl fmt::Display for ProcActionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CriticalProcess(pid) => {
                #[cfg(windows)]
                {
                    write!(
                        f,
                        "Refused: PID {pid} is a critical Windows system process (terminating would cause a BSOD)"
                    )
                }
                #[cfg(not(windows))]
                {
                    write!(
                        f,
                        "Refused: PID {pid} is PID 0/1 or a protected kernel thread"
                    )
                }
            }
            Self::SelfProcess(pid) => write!(
                f,
                "Refused: cannot force-terminate btop-windows itself (PID {pid}); press q to exit cleanly and restore the terminal"
            ),
            Self::IdentityMismatch { pid } => write!(
                f,
                "Aborted: PID {pid} creation time no longer matches the selected process (PID reused)"
            ),
            Self::AccessDenied(pid) => write!(
                f,
                "Access denied for PID {pid} (protected or elevated process requires higher privileges)"
            ),
            Self::NoWindow(pid) => write!(
                f,
                "PID {pid} has no top-level window to receive WM_CLOSE (use k to terminate)"
            ),
            Self::NotFound(pid) => write!(f, "PID {pid} is no longer running"),
            Self::Platform(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for ProcActionError {}

/// Pure safety gate evaluated before calling `TerminateProcess`.
///
/// Refuses kernel PIDs (`0`/`4`), the current `btop-windows` process itself (so terminal
/// restoration always runs via orderly shutdown), missing or mismatched `CreateTime`,
/// confirmed critical system processes (`IsProcessCritical == TRUE`), and fail-safe errors
/// when criticality cannot be determined.
pub fn guard_terminate_decision(
    pid: u32,
    expected_create_time_100ns: u64,
    actual_create_time_100ns: u64,
    is_critical: Result<bool, &str>,
) -> Result<(), ProcActionError> {
    if matches!(pid, 0 | 4) {
        return Err(ProcActionError::CriticalProcess(pid));
    }
    if pid == std::process::id() {
        return Err(ProcActionError::SelfProcess(pid));
    }
    if expected_create_time_100ns == 0 || actual_create_time_100ns != expected_create_time_100ns {
        return Err(ProcActionError::IdentityMismatch { pid });
    }
    match is_critical {
        Ok(true) => Err(ProcActionError::CriticalProcess(pid)),
        Ok(false) => Ok(()),
        Err(reason) => Err(ProcActionError::Platform(format!(
            "Aborted: IsProcessCritical check failed for PID {pid} ({reason})"
        ))),
    }
}

/// Pure identity guard shared by non-terminate actions (`WM_CLOSE`, `SetPriorityClass`).
pub fn guard_process_identity(
    pid: u32,
    expected_create_time_100ns: u64,
    actual_create_time_100ns: u64,
) -> Result<(), ProcActionError> {
    if matches!(pid, 0 | 4) {
        return Err(ProcActionError::CriticalProcess(pid));
    }
    if expected_create_time_100ns == 0 || actual_create_time_100ns != expected_create_time_100ns {
        return Err(ProcActionError::IdentityMismatch { pid });
    }
    Ok(())
}

/// Converts a Unix process start timestamp in whole seconds since the Unix epoch into the
/// 100-nanosecond identity representation stored in `ProcessIdentity::create_time_100ns`.
pub const fn unix_start_time_secs_to_identity(start_time_secs: u64) -> u64 {
    start_time_secs.saturating_mul(10_000_000)
}

/// Linux `PF_KTHREAD` process flag from `<linux/sched.h>` (`0x0020_0000`).
const LINUX_PF_KTHREAD: u64 = 0x0020_0000;

/// Returns `true` when Linux `/proc/<pid>/stat` metadata identifies a kernel thread (`kthreadd`
/// PID 2, direct children of PID 2, or any task carrying `PF_KTHREAD`).
pub const fn is_linux_kernel_thread(pid: u32, parent_pid: u32, flags: u64) -> bool {
    pid == 2 || parent_pid == 2 || (flags & LINUX_PF_KTHREAD) != 0
}

/// Parses `(parent_pid, flags, starttime_ticks)` from one Linux `/proc/<pid>/stat` line, locating
/// the final `')'` delimiter so process names containing spaces or parentheses never shift fields.
pub fn parse_linux_proc_stat_identity(stat_line: &str) -> Option<(u32, u64, u64)> {
    let close_idx = stat_line.rfind(')')?;
    let rest = stat_line.get(close_idx + 1..)?.trim_start();
    let mut fields = rest.split_whitespace();
    let _state = fields.next()?;
    let parent_pid = fields.next()?.parse::<u32>().ok()?;
    // After `state` (0) and `parent_pid` (1): skip pgrp(2), session(3), tty_nr(4), tpgid(5) -> flags(6).
    let flags = fields.nth(4)?.parse::<u64>().ok()?;
    // After `flags` (6): skip 12 fields (7..=18) -> starttime(19).
    let starttime_ticks = fields.nth(12)?.parse::<u64>().ok()?;
    Some((parent_pid, flags, starttime_ticks))
}

/// Pure safety gate evaluated before sending `SIGTERM` or `SIGKILL` to a Unix PID.
///
/// Refuses PID 0, PID 1 (init/systemd/launchd), the running monitor itself (so terminal
/// restoration always runs cleanly), kernel threads, and missing or mismatched start times.
pub fn guard_unix_signal_decision(
    pid: u32,
    expected_create_time_100ns: u64,
    actual_create_time_100ns: u64,
    is_kernel_thread: bool,
) -> Result<(), ProcActionError> {
    if pid <= 1 || is_kernel_thread {
        return Err(ProcActionError::CriticalProcess(pid));
    }
    if pid == std::process::id() {
        return Err(ProcActionError::SelfProcess(pid));
    }
    if expected_create_time_100ns == 0 || actual_create_time_100ns != expected_create_time_100ns {
        return Err(ProcActionError::IdentityMismatch { pid });
    }
    Ok(())
}

/// Outcome of a successful `SetPriorityClass` call, including the effective priority read back
/// from Windows (detecting when `Realtime` is downgraded to `High` without `SeIncreaseBasePriorityPrivilege`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PriorityUpdateOutcome {
    pub requested: PriorityClass,
    pub applied: PriorityClass,
}

#[cfg(windows)]
mod win {
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, FILETIME, GetLastError, HANDLE,
        HWND, LPARAM,
    };
    use windows_sys::Win32::System::Threading::{
        GetPriorityClass, GetProcessTimes, IsProcessCritical, OpenProcess,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION, PROCESS_TERMINATE,
        SetPriorityClass, TerminateProcess,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GW_OWNER, GetWindow, GetWindowThreadProcessId, IsWindow, IsWindowVisible,
        PostMessageW, WM_CLOSE,
    };

    use super::{
        PriorityClass, PriorityUpdateOutcome, ProcActionError, guard_process_identity,
        guard_terminate_decision,
    };

    struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            // SAFETY: Handle was returned by `OpenProcess` and is uniquely owned here.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    fn filetime_to_u64(ft: FILETIME) -> u64 {
        (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)
    }

    fn open_and_verify(
        pid: u32,
        expected_create_time_100ns: u64,
        desired_access: u32,
    ) -> Result<(OwnedHandle, u64), ProcActionError> {
        if matches!(pid, 0 | 4) {
            return Err(ProcActionError::CriticalProcess(pid));
        }
        if expected_create_time_100ns == 0 {
            return Err(ProcActionError::IdentityMismatch { pid });
        }

        // SAFETY: Opening process with the minimum required access mask for this action.
        let raw = unsafe { OpenProcess(desired_access, 0, pid) };
        if raw.is_null() {
            // SAFETY: Reading thread-local Win32 error immediately after failed `OpenProcess`.
            let err = unsafe { GetLastError() };
            return Err(match err {
                ERROR_ACCESS_DENIED => ProcActionError::AccessDenied(pid),
                ERROR_INVALID_PARAMETER => ProcActionError::NotFound(pid),
                other => {
                    ProcActionError::Platform(format!("OpenProcess(PID {pid}) failed ({other})"))
                }
            });
        }
        let handle = OwnedHandle(raw);

        let mut creation = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut exit = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut kernel = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut user = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        // SAFETY: Valid process handle with `PROCESS_QUERY_LIMITED_INFORMATION` and writable FILETIME pointers.
        let ok = unsafe {
            GetProcessTimes(
                handle.0,
                &raw mut creation,
                &raw mut exit,
                &raw mut kernel,
                &raw mut user,
            )
        };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            return Err(match err {
                ERROR_ACCESS_DENIED => ProcActionError::AccessDenied(pid),
                other => ProcActionError::Platform(format!(
                    "GetProcessTimes(PID {pid}) failed ({other})"
                )),
            });
        }

        let actual_create = filetime_to_u64(creation);
        guard_process_identity(pid, expected_create_time_100ns, actual_create)?;
        Ok((handle, actual_create))
    }

    /// Queries the current Windows priority class of `(pid, expected_create_time_100ns)` using
    /// `PROCESS_QUERY_LIMITED_INFORMATION` and `GetProcessTimes` verification on the handle.
    pub fn query_process_priority(
        pid: u32,
        expected_create_time_100ns: u64,
    ) -> Result<PriorityClass, ProcActionError> {
        let (handle, _) = open_and_verify(
            pid,
            expected_create_time_100ns,
            PROCESS_QUERY_LIMITED_INFORMATION,
        )?;
        // SAFETY: Handle is open with `PROCESS_QUERY_LIMITED_INFORMATION`.
        let raw = unsafe { GetPriorityClass(handle.0) };
        if raw == 0 {
            let err = unsafe { GetLastError() };
            return Err(if err == ERROR_ACCESS_DENIED {
                ProcActionError::AccessDenied(pid)
            } else {
                ProcActionError::Platform(format!("GetPriorityClass(PID {pid}) failed ({err})"))
            });
        }
        PriorityClass::from_win32_flag(raw).ok_or_else(|| {
            ProcActionError::Platform(format!(
                "Unrecognized priority class 0x{raw:08X} for PID {pid}"
            ))
        })
    }

    /// Sets the priority class of `(pid, expected_create_time_100ns)` using the minimum
    /// `PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION` mask, verifying `CreateTime`
    /// on the opened handle before calling `SetPriorityClass`.
    pub fn set_process_priority(
        pid: u32,
        expected_create_time_100ns: u64,
        priority: PriorityClass,
    ) -> Result<PriorityUpdateOutcome, ProcActionError> {
        let (handle, _) = open_and_verify(
            pid,
            expected_create_time_100ns,
            PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION,
        )?;
        // SAFETY: Handle is open with `PROCESS_SET_INFORMATION` and verified against `CreateTime`.
        let ok = unsafe { SetPriorityClass(handle.0, priority.to_win32_flag()) };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            return Err(if err == ERROR_ACCESS_DENIED {
                ProcActionError::AccessDenied(pid)
            } else {
                ProcActionError::Platform(format!("SetPriorityClass(PID {pid}) failed ({err})"))
            });
        }
        // Read back effective priority to detect Windows clamping Realtime -> High without elevation.
        let effective = PriorityClass::from_win32_flag(unsafe { GetPriorityClass(handle.0) })
            .unwrap_or(priority);
        Ok(PriorityUpdateOutcome {
            requested: priority,
            applied: effective,
        })
    }

    /// Terminates `(pid, expected_create_time_100ns)` after opening with
    /// `PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION`, verifying `CreateTime` on that
    /// handle, rejecting self-termination so terminal restoration always runs, and confirming
    /// `IsProcessCritical == FALSE` so an elevated user cannot trigger a BSOD.
    pub fn terminate_process(
        pid: u32,
        expected_create_time_100ns: u64,
    ) -> Result<(), ProcActionError> {
        if pid == std::process::id() {
            return Err(ProcActionError::SelfProcess(pid));
        }
        let (handle, actual_create) = open_and_verify(
            pid,
            expected_create_time_100ns,
            PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
        )?;

        let mut critical_flag = 0i32;
        // SAFETY: Handle has `PROCESS_QUERY_LIMITED_INFORMATION`; `critical_flag` is writable `BOOL`.
        let crit_ok = unsafe { IsProcessCritical(handle.0, &raw mut critical_flag) };
        let crit_check = if crit_ok != 0 {
            Ok(critical_flag != 0)
        } else {
            Err("IsProcessCritical Win32 call failed")
        };

        guard_terminate_decision(pid, expected_create_time_100ns, actual_create, crit_check)?;

        // SAFETY: Handle has `PROCESS_TERMINATE`, verified `CreateTime`, and confirmed non-critical.
        let ok = unsafe { TerminateProcess(handle.0, 1) };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            return Err(if err == ERROR_ACCESS_DENIED {
                ProcActionError::AccessDenied(pid)
            } else {
                ProcActionError::Platform(format!("TerminateProcess(PID {pid}) failed ({err})"))
            });
        }
        Ok(())
    }

    struct EnumState {
        target_pid: u32,
        visible_unowned: Vec<HWND>,
        other_unowned: Vec<HWND>,
    }

    unsafe extern "system" fn enum_windows_cb(hwnd: HWND, lparam: LPARAM) -> i32 {
        // SAFETY: `lparam` is a valid mutable pointer to `EnumState` for the duration of `EnumWindows`.
        let state = unsafe { &mut *(lparam as *mut EnumState) };
        let mut win_pid = 0u32;
        // SAFETY: Querying owning PID of enumerated top-level `hwnd`.
        unsafe {
            let _ = GetWindowThreadProcessId(hwnd, &raw mut win_pid);
        }
        if win_pid == state.target_pid {
            // SAFETY: Checking owner window and visibility on enumerated top-level `hwnd`.
            let owner = unsafe { GetWindow(hwnd, GW_OWNER) };
            if owner.is_null() {
                if unsafe { IsWindowVisible(hwnd) } != 0 {
                    state.visible_unowned.push(hwnd);
                } else {
                    state.other_unowned.push(hwnd);
                }
            }
        }
        1
    }

    /// Gracefully closes `(pid, expected_create_time_100ns)` by pinning the process with a
    /// `CreateTime`-verified `PROCESS_QUERY_LIMITED_INFORMATION` handle, enumerating top-level
    /// windows via `EnumWindows`, and posting `WM_CLOSE`. Returns the number of windows notified.
    pub fn graceful_close_process(
        pid: u32,
        expected_create_time_100ns: u64,
    ) -> Result<usize, ProcActionError> {
        let (_handle, _) = open_and_verify(
            pid,
            expected_create_time_100ns,
            PROCESS_QUERY_LIMITED_INFORMATION,
        )?;

        let mut state = EnumState {
            target_pid: pid,
            visible_unowned: Vec::new(),
            other_unowned: Vec::new(),
        };
        // SAFETY: `enum_windows_cb` only accesses `state` synchronously during `EnumWindows`.
        let _ = unsafe { EnumWindows(Some(enum_windows_cb), (&raw mut state) as LPARAM) };

        let targets = if state.visible_unowned.is_empty() {
            state.other_unowned
        } else {
            state.visible_unowned
        };
        if targets.is_empty() {
            return Err(ProcActionError::NoWindow(pid));
        }

        let mut sent = 0usize;
        let mut saw_access_denied = false;
        for hwnd in targets {
            // Revalidate window existence and owning PID immediately before posting `WM_CLOSE`
            // so a destroyed/reused HWND cannot deliver `WM_CLOSE` to an unrelated process.
            if unsafe { IsWindow(hwnd) } == 0 {
                continue;
            }
            let mut current_pid = 0u32;
            // SAFETY: Querying current owning PID of `hwnd` right before sending `WM_CLOSE`.
            unsafe {
                let _ = GetWindowThreadProcessId(hwnd, &raw mut current_pid);
            }
            if current_pid != pid || !unsafe { GetWindow(hwnd, GW_OWNER) }.is_null() {
                continue;
            }

            // SAFETY: Posting asynchronous `WM_CLOSE` to a revalidated window owned by verified `pid`.
            let ok = unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) };
            if ok != 0 {
                sent += 1;
            } else if unsafe { GetLastError() } == ERROR_ACCESS_DENIED {
                saw_access_denied = true;
            }
        }

        if sent > 0 {
            Ok(sent)
        } else if saw_access_denied {
            Err(ProcActionError::AccessDenied(pid))
        } else {
            Err(ProcActionError::NoWindow(pid))
        }
    }
}

#[cfg(windows)]
pub use win::{
    graceful_close_process, query_process_priority, set_process_priority, terminate_process,
};

#[cfg(unix)]
mod unix {
    use super::{
        PriorityClass, PriorityUpdateOutcome, ProcActionError, guard_unix_signal_decision,
        unix_start_time_secs_to_identity,
    };

    #[cfg(target_os = "linux")]
    struct OwnedFd(libc::c_int);

    #[cfg(target_os = "linux")]
    impl Drop for OwnedFd {
        fn drop(&mut self) {
            // SAFETY: File descriptor was opened by `pidfd_open` and is uniquely owned here.
            unsafe {
                let _ = libc::close(self.0);
            }
        }
    }

    fn map_errno(pid: u32, err: i32, op: &str) -> ProcActionError {
        match err {
            libc::ESRCH => ProcActionError::NotFound(pid),
            libc::EPERM | libc::EACCES => ProcActionError::AccessDenied(pid),
            other => ProcActionError::Platform(format!("{op}(PID {pid}) failed (errno {other})")),
        }
    }

    #[cfg(target_os = "linux")]
    fn read_linux_boot_time_secs() -> Result<u64, ProcActionError> {
        let content = std::fs::read_to_string("/proc/stat").map_err(|err| {
            ProcActionError::Platform(format!("failed to read /proc/stat for btime: {err}"))
        })?;
        for line in content.lines() {
            if let Some(rest) = line.strip_prefix("btime ")
                && let Ok(secs) = rest.trim().parse::<u64>()
            {
                return Ok(secs);
            }
        }
        Err(ProcActionError::Platform(
            "missing btime entry in /proc/stat".to_owned(),
        ))
    }

    #[cfg(target_os = "linux")]
    fn query_unix_process_meta(pid: u32) -> Result<(u64, bool), ProcActionError> {
        let stat_path = format!("/proc/{pid}/stat");
        let stat_line = std::fs::read_to_string(&stat_path).map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => ProcActionError::NotFound(pid),
            std::io::ErrorKind::PermissionDenied => ProcActionError::AccessDenied(pid),
            _ => ProcActionError::Platform(format!("failed to read {stat_path}: {err}")),
        })?;
        let (parent_pid, flags, starttime_ticks) =
            super::parse_linux_proc_stat_identity(&stat_line).ok_or_else(|| {
                ProcActionError::Platform(format!("malformed /proc/{pid}/stat record"))
            })?;
        let clk_tck = {
            // SAFETY: Querying POSIX clock ticks per second.
            let raw = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
            u64::try_from(raw).ok().filter(|&v| v > 0).unwrap_or(100)
        };
        let btime = read_linux_boot_time_secs()?;
        let start_secs = btime.saturating_add(starttime_ticks / clk_tck);
        let is_kthread = super::is_linux_kernel_thread(pid, parent_pid, flags);
        Ok((unix_start_time_secs_to_identity(start_secs), is_kthread))
    }

    #[cfg(target_os = "macos")]
    fn query_unix_process_meta(pid: u32) -> Result<(u64, bool), ProcActionError> {
        const P_SYSTEM: u32 = 0x0000_0200;

        let Ok(c_pid) = libc::c_int::try_from(pid) else {
            return Err(ProcActionError::NotFound(pid));
        };
        let mut info: libc::proc_bsdinfo = unsafe { core::mem::zeroed() };
        let size = i32::try_from(size_of::<libc::proc_bsdinfo>()).unwrap_or(0);
        // SAFETY: Valid output buffer for `PROC_PIDTBSDINFO` of exact size `size`.
        let rc = unsafe {
            libc::proc_pidinfo(
                c_pid,
                libc::PROC_PIDTBSDINFO,
                0,
                (&raw mut info).cast::<libc::c_void>(),
                size,
            )
        };
        if rc <= 0 {
            let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            if err == 0 || err == libc::ESRCH {
                return Err(ProcActionError::NotFound(pid));
            }
            return Err(map_errno(pid, err, "proc_pidinfo"));
        }
        let is_kthread = (info.pbi_flags & P_SYSTEM) != 0;
        Ok((
            unix_start_time_secs_to_identity(info.pbi_start_tvsec),
            is_kthread,
        ))
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn query_unix_process_meta(_pid: u32) -> Result<(u64, bool), ProcActionError> {
        Err(ProcActionError::Platform(
            "process start-time verification is only implemented on Linux and macOS".to_owned(),
        ))
    }

    #[cfg(target_os = "linux")]
    fn send_guarded_signal(
        pid: u32,
        expected_create_time_100ns: u64,
        sig: libc::c_int,
    ) -> Result<(), ProcActionError> {
        if pid <= 1 {
            return Err(ProcActionError::CriticalProcess(pid));
        }
        if pid == std::process::id() {
            return Err(ProcActionError::SelfProcess(pid));
        }
        if expected_create_time_100ns == 0 {
            return Err(ProcActionError::IdentityMismatch { pid });
        }
        let Ok(raw_pid) = libc::pid_t::try_from(pid) else {
            return Err(ProcActionError::NotFound(pid));
        };

        // Hold a `pidfd` across `/proc/<pid>/stat` inspection and signal delivery so PID reuse
        // cannot redirect the signal to a newly spawned process (falling back to `kill` only on
        // kernels without `pidfd_open`).
        // SAFETY: Invoking Linux `SYS_pidfd_open(pid, 0)`.
        let pidfd_rc = unsafe { libc::syscall(libc::SYS_pidfd_open, raw_pid, 0 as libc::c_uint) };
        let pidfd = if pidfd_rc >= 0 {
            Some(OwnedFd(i32::try_from(pidfd_rc).unwrap_or(-1)))
        } else {
            let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            if err == libc::ENOSYS {
                None
            } else {
                return Err(map_errno(pid, err, "pidfd_open"));
            }
        };

        let (actual_create_time_100ns, is_kernel_thread) = query_unix_process_meta(pid)?;
        guard_unix_signal_decision(
            pid,
            expected_create_time_100ns,
            actual_create_time_100ns,
            is_kernel_thread,
        )?;

        if let Some(ref fd) = pidfd {
            // SAFETY: Delivering `sig` through a verified, pinned `pidfd`.
            let rc = unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    fd.0,
                    sig,
                    core::ptr::null::<libc::c_void>(),
                    0 as libc::c_uint,
                )
            };
            if rc == 0 {
                return Ok(());
            }
            let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            if err != libc::ENOSYS {
                return Err(map_errno(pid, err, "pidfd_send_signal"));
            }
        }

        // SAFETY: Verified non-critical target PID with matching start time.
        if unsafe { libc::kill(raw_pid, sig) } != 0 {
            let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            return Err(map_errno(pid, err, "kill"));
        }
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    fn send_guarded_signal(
        pid: u32,
        expected_create_time_100ns: u64,
        sig: libc::c_int,
    ) -> Result<(), ProcActionError> {
        if pid <= 1 {
            return Err(ProcActionError::CriticalProcess(pid));
        }
        if pid == std::process::id() {
            return Err(ProcActionError::SelfProcess(pid));
        }
        if expected_create_time_100ns == 0 {
            return Err(ProcActionError::IdentityMismatch { pid });
        }
        let Ok(raw_pid) = libc::pid_t::try_from(pid) else {
            return Err(ProcActionError::NotFound(pid));
        };

        let (actual_create_time_100ns, is_kernel_thread) = query_unix_process_meta(pid)?;
        guard_unix_signal_decision(
            pid,
            expected_create_time_100ns,
            actual_create_time_100ns,
            is_kernel_thread,
        )?;

        // SAFETY: Verified non-critical target PID with matching start time.
        if unsafe { libc::kill(raw_pid, sig) } != 0 {
            let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            return Err(map_errno(pid, err, "kill"));
        }
        Ok(())
    }

    /// Queries the live start-time identity (`create_time_100ns`) for `pid` on Unix.
    #[cfg(test)]
    pub(crate) fn query_start_time_100ns(pid: u32) -> Result<u64, ProcActionError> {
        query_unix_process_meta(pid).map(|(start, _)| start)
    }

    pub fn query_process_priority(
        _pid: u32,
        _expected_create_time_100ns: u64,
    ) -> Result<PriorityClass, ProcActionError> {
        Err(ProcActionError::Platform(
            "process priority controls are not supported on Linux or macOS in v1".to_owned(),
        ))
    }

    pub fn set_process_priority(
        _pid: u32,
        _expected_create_time_100ns: u64,
        _priority: PriorityClass,
    ) -> Result<PriorityUpdateOutcome, ProcActionError> {
        Err(ProcActionError::Platform(
            "process priority controls are not supported on Linux or macOS in v1".to_owned(),
        ))
    }

    /// Sends guarded `SIGKILL` to `(pid, expected_create_time_100ns)` on Unix after verifying
    /// start time and rejecting PID <= 1, self, and kernel threads.
    pub fn terminate_process(
        pid: u32,
        expected_create_time_100ns: u64,
    ) -> Result<(), ProcActionError> {
        send_guarded_signal(pid, expected_create_time_100ns, libc::SIGKILL)
    }

    /// Sends guarded `SIGTERM` to `(pid, expected_create_time_100ns)` on Unix after verifying
    /// start time and rejecting PID <= 1, self, and kernel threads.
    pub fn graceful_close_process(
        pid: u32,
        expected_create_time_100ns: u64,
    ) -> Result<usize, ProcActionError> {
        send_guarded_signal(pid, expected_create_time_100ns, libc::SIGTERM).map(|()| 1)
    }
}

#[cfg(unix)]
pub use unix::{
    graceful_close_process, query_process_priority, set_process_priority, terminate_process,
};

#[cfg(not(any(windows, unix)))]
pub fn query_process_priority(
    _pid: u32,
    _expected_create_time_100ns: u64,
) -> Result<PriorityClass, ProcActionError> {
    Err(ProcActionError::Platform(
        "process priority query requires Windows".to_owned(),
    ))
}

#[cfg(not(any(windows, unix)))]
pub fn set_process_priority(
    _pid: u32,
    _expected_create_time_100ns: u64,
    _priority: PriorityClass,
) -> Result<PriorityUpdateOutcome, ProcActionError> {
    Err(ProcActionError::Platform(
        "SetPriorityClass requires Windows".to_owned(),
    ))
}

#[cfg(not(any(windows, unix)))]
pub fn terminate_process(pid: u32, expected_create_time_100ns: u64) -> Result<(), ProcActionError> {
    guard_terminate_decision(
        pid,
        expected_create_time_100ns,
        expected_create_time_100ns,
        Ok(false),
    )?;
    Err(ProcActionError::Platform(
        "TerminateProcess requires Windows or Unix".to_owned(),
    ))
}

#[cfg(not(any(windows, unix)))]
pub fn graceful_close_process(
    pid: u32,
    expected_create_time_100ns: u64,
) -> Result<usize, ProcActionError> {
    guard_process_identity(pid, expected_create_time_100ns, expected_create_time_100ns)?;
    Err(ProcActionError::Platform(
        "graceful close requires Windows or Unix".to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn critical_process_guard_refuses_kernel_critical_and_unverified_targets() {
        // Kernel pseudo-processes 0 and 4 are always refused.
        assert_eq!(
            guard_terminate_decision(0, 1000, 1000, Ok(false)),
            Err(ProcActionError::CriticalProcess(0))
        );
        assert_eq!(
            guard_terminate_decision(4, 1000, 1000, Ok(false)),
            Err(ProcActionError::CriticalProcess(4))
        );

        // Confirmed critical system process (IsProcessCritical == TRUE) is always refused.
        assert_eq!(
            guard_terminate_decision(720, 1000, 1000, Ok(true)),
            Err(ProcActionError::CriticalProcess(720))
        );

        // If IsProcessCritical fails, fail-safe refuses termination.
        assert!(matches!(
            guard_terminate_decision(720, 1000, 1000, Err("access error")),
            Err(ProcActionError::Platform(_))
        ));

        // CreateTime mismatch or zero expected create time aborts cleanly.
        assert_eq!(
            guard_terminate_decision(4820, 1000, 2000, Ok(false)),
            Err(ProcActionError::IdentityMismatch { pid: 4820 })
        );
        assert_eq!(
            guard_terminate_decision(4820, 0, 1000, Ok(false)),
            Err(ProcActionError::IdentityMismatch { pid: 4820 })
        );

        // Terminating the running monitor itself is refused so terminal restoration always runs.
        let my_pid = std::process::id();
        assert_eq!(
            guard_terminate_decision(my_pid, 1000, 1000, Ok(false)),
            Err(ProcActionError::SelfProcess(my_pid))
        );
        assert_eq!(
            terminate_process(my_pid, 1000),
            Err(ProcActionError::SelfProcess(my_pid))
        );

        // Verified non-critical user process is permitted (using a synthetic PID != current PID).
        let other_pid = my_pid.wrapping_add(1).max(100);
        assert_eq!(
            guard_terminate_decision(other_pid, 1000, 1000, Ok(false)),
            Ok(())
        );
    }

    #[test]
    fn priority_class_win32_flags_roundtrip() {
        for class in PriorityClass::ALL {
            assert_eq!(
                PriorityClass::from_win32_flag(class.to_win32_flag()),
                Some(class)
            );
        }
        assert_eq!(PriorityClass::from_win32_flag(0), None);
    }

    #[test]
    fn unix_signal_guard_rejects_pid_1_kernel_threads_self_and_mismatched_start_time() {
        let valid_time = unix_start_time_secs_to_identity(1_700_000_000);
        assert_eq!(
            guard_unix_signal_decision(0, valid_time, valid_time, false),
            Err(ProcActionError::CriticalProcess(0))
        );
        assert_eq!(
            guard_unix_signal_decision(1, valid_time, valid_time, false),
            Err(ProcActionError::CriticalProcess(1))
        );
        assert_eq!(
            guard_unix_signal_decision(42, valid_time, valid_time, true),
            Err(ProcActionError::CriticalProcess(42))
        );
        let my_pid = std::process::id();
        assert_eq!(
            guard_unix_signal_decision(my_pid, valid_time, valid_time, false),
            Err(ProcActionError::SelfProcess(my_pid))
        );
        let other_pid = my_pid.wrapping_add(1).max(100);
        assert_eq!(
            guard_unix_signal_decision(other_pid, valid_time, valid_time + 10_000_000, false),
            Err(ProcActionError::IdentityMismatch { pid: other_pid })
        );
        assert_eq!(
            guard_unix_signal_decision(other_pid, 0, valid_time, false),
            Err(ProcActionError::IdentityMismatch { pid: other_pid })
        );
        assert_eq!(
            guard_unix_signal_decision(other_pid, valid_time, valid_time, false),
            Ok(())
        );
    }

    #[test]
    fn parses_linux_proc_stat_with_spaces_and_parentheses_in_comm() {
        let stat =
            "1234 (kworker/0:1 (events)) S 2 0 0 0 -1 69238880 0 0 0 0 0 0 0 0 20 0 1 0 98765 0 0";
        let (parent_pid, flags, starttime) =
            parse_linux_proc_stat_identity(stat).expect("parse /proc/stat line");
        assert_eq!(parent_pid, 2);
        assert_eq!(flags, 69_238_880);
        assert_eq!(starttime, 98_765);
        assert!(is_linux_kernel_thread(1234, parent_pid, flags));
        assert!(is_linux_kernel_thread(2, 0, 0));
        assert!(is_linux_kernel_thread(555, 1, 0x0020_0000));
        assert!(!is_linux_kernel_thread(555, 1, 0x0040_0000));
    }

    #[cfg(unix)]
    #[test]
    fn unix_live_guarded_signals_verify_start_time_and_reject_pid_1() {
        use std::process::Command;

        let valid_time = unix_start_time_secs_to_identity(1_700_000_000);
        assert_eq!(
            terminate_process(1, valid_time),
            Err(ProcActionError::CriticalProcess(1))
        );
        assert_eq!(
            graceful_close_process(1, valid_time),
            Err(ProcActionError::CriticalProcess(1))
        );

        let mut child_term = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn disposable sleep child for SIGTERM");
        let pid_term = child_term.id();
        let actual_start = unix::query_start_time_100ns(pid_term).expect("query child start time");
        assert!(actual_start > 0);

        // Wrong start time must refuse and leave child alive.
        assert_eq!(
            graceful_close_process(pid_term, actual_start.saturating_add(10_000_000)),
            Err(ProcActionError::IdentityMismatch { pid: pid_term })
        );
        assert!(
            child_term.try_wait().expect("poll child").is_none(),
            "child must stay alive when start time mismatches"
        );

        // Matching start time delivers SIGTERM cleanly.
        assert_eq!(graceful_close_process(pid_term, actual_start), Ok(1));
        let status = child_term.wait().expect("reap SIGTERM child");
        assert!(!status.success());

        // Matching start time delivers SIGKILL cleanly.
        let mut child_kill = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn disposable sleep child for SIGKILL");
        let pid_kill = child_kill.id();
        let kill_start = unix::query_start_time_100ns(pid_kill).expect("query kill child start");
        assert_eq!(terminate_process(pid_kill, kill_start), Ok(()));
        let status_kill = child_kill.wait().expect("reap SIGKILL child");
        assert!(!status_kill.success());
    }
}
