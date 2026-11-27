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

/// Windows `FILETIME` epoch (`1601-01-01`) to Unix epoch (`1970-01-01`) offset in 100ns ticks.
pub const WINDOWS_TO_UNIX_EPOCH_100NS: u64 = 116_444_736_000_000_000;

/// Converts a Unix process start timestamp in whole seconds since the Unix epoch into the
/// Windows `FILETIME`-epoch 100-nanosecond representation stored in `ProcessIdentity::create_time_100ns`.
///
/// Returns `0` when `start_time_secs == 0` so unknown start times remain distinguishable.
pub const fn unix_start_time_secs_to_identity(start_time_secs: u64) -> u64 {
    if start_time_secs == 0 {
        return 0;
    }
    WINDOWS_TO_UNIX_EPOCH_100NS.saturating_add(start_time_secs.saturating_mul(10_000_000))
}

/// Converts a Linux boot time (`btime` in seconds since 1970) plus per-process `/proc/<pid>/stat`
/// `starttime` jiffies into a sub-second `FILETIME`-epoch 100ns identity.
pub fn unix_linux_ticks_to_identity(btime_secs: u64, starttime_ticks: u64, clk_tck: u64) -> u64 {
    if btime_secs == 0 || clk_tck == 0 {
        return 0;
    }
    let ticks_100ns =
        u64::try_from((u128::from(starttime_ticks) * 10_000_000) / u128::from(clk_tck))
            .unwrap_or(u64::MAX);
    WINDOWS_TO_UNIX_EPOCH_100NS
        .saturating_add(btime_secs.saturating_mul(10_000_000))
        .saturating_add(ticks_100ns)
}

/// Converts a macOS `proc_bsdinfo` `(pbi_start_tvsec, pbi_start_tvusec)` pair into a
/// microsecond-precision `FILETIME`-epoch 100ns identity.
pub const fn unix_timeval_to_identity(seconds: u64, micros: u64) -> u64 {
    if seconds == 0 && micros == 0 {
        return 0;
    }
    WINDOWS_TO_UNIX_EPOCH_100NS
        .saturating_add(seconds.saturating_mul(10_000_000))
        .saturating_add(micros.saturating_mul(10))
}

/// Linux `PF_KTHREAD` process flag from `<linux/sched.h>` (`0x0020_0000`).
const LINUX_PF_KTHREAD: u64 = 0x0020_0000;

/// XNU `PROC_FLAG_SYSTEM` bit in `proc_bsdinfo::pbi_flags` (`<sys/proc_info.h>` = `0x0000_0001`).
const MACOS_PROC_FLAG_SYSTEM: u32 = 0x0000_0001;

/// Returns `true` when Linux `/proc/<pid>/stat` metadata identifies a kernel thread (`kthreadd`
/// PID 2, direct children of PID 2, or any task carrying `PF_KTHREAD`).
pub const fn is_linux_kernel_thread(pid: u32, parent_pid: u32, flags: u64) -> bool {
    pid == 2 || parent_pid == 2 || (flags & LINUX_PF_KTHREAD) != 0
}

/// Returns `true` when macOS `proc_bsdinfo::pbi_flags` carries `PROC_FLAG_SYSTEM` (`0x1`).
pub const fn is_macos_system_process(pbi_flags: u32) -> bool {
    (pbi_flags & MACOS_PROC_FLAG_SYSTEM) != 0
}

/// Parsed fields from one Linux `/proc/<pid>/stat` record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinuxProcStat {
    pub parent_pid: u32,
    pub flags: u64,
    pub num_threads: u32,
    pub starttime_ticks: u64,
}

/// Parses `(parent_pid, flags, num_threads, starttime_ticks)` from raw `/proc/<pid>/stat` bytes,
/// locating the final `b')'` delimiter so arbitrary non-UTF-8 or parenthesized `comm` names never
/// fail or shift numeric fields.
pub fn parse_linux_proc_stat_bytes(stat_bytes: &[u8]) -> Option<LinuxProcStat> {
    let close_idx = stat_bytes.iter().rposition(|&b| b == b')')?;
    let tail = std::str::from_utf8(stat_bytes.get(close_idx + 1..)?).ok()?;
    let mut fields = tail.split_whitespace();
    let _state = fields.next()?;
    let parent_pid = fields.next()?.parse::<u32>().ok()?;
    // After `state`(0) and `parent_pid`(1): skip pgrp(2), session(3), tty_nr(4), tpgid(5) -> flags(6).
    let flags = fields.nth(4)?.parse::<u64>().ok()?;
    // After `flags`(6): skip minflt..=nice (10 fields, 7..=16) -> num_threads(17).
    let num_threads = fields.nth(10)?.parse::<u32>().ok()?.max(1);
    // After `num_threads`(17): skip itrealvalue(18) -> starttime(19).
    let starttime_ticks = fields.nth(1)?.parse::<u64>().ok()?;
    Some(LinuxProcStat {
        parent_pid,
        flags,
        num_threads,
        starttime_ticks,
    })
}

/// Parses `(parent_pid, flags, starttime_ticks)` from a UTF-8 `/proc/<pid>/stat` line.
pub fn parse_linux_proc_stat_identity(stat_line: &str) -> Option<(u32, u64, u64)> {
    let parsed = parse_linux_proc_stat_bytes(stat_line.as_bytes())?;
    Some((parsed.parent_pid, parsed.flags, parsed.starttime_ticks))
}

/// Computes private resident bytes `(resident - shared) * page_size` from Linux `/proc/<pid>/statm`.
pub fn parse_linux_proc_statm_private_bytes(statm: &str, page_size: u64) -> Option<u64> {
    let mut fields = statm.split_whitespace();
    let _size = fields.next()?.parse::<u64>().ok()?;
    let resident = fields.next()?.parse::<u64>().ok()?;
    let shared = fields.next()?.parse::<u64>().ok()?;
    Some(resident.saturating_sub(shared).saturating_mul(page_size))
}

/// Live per-process metadata queried from the Unix kernel so telemetry collection and guarded
/// signal delivery share identical sub-second start-time identities, thread counts, and private
/// memory accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnixProcessMeta {
    pub create_time_100ns: u64,
    pub is_kernel_thread: bool,
    pub num_threads: u32,
    pub private_rss_bytes: Option<u64>,
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
    fn cached_linux_boot_time_secs() -> Result<u64, ProcActionError> {
        use std::sync::OnceLock;
        static BOOT_TIME: OnceLock<u64> = OnceLock::new();
        if let Some(&secs) = BOOT_TIME.get() {
            return Ok(secs);
        }
        let content = std::fs::read_to_string("/proc/stat").map_err(|err| {
            ProcActionError::Platform(format!("failed to read /proc/stat for btime: {err}"))
        })?;
        for line in content.lines() {
            if let Some(rest) = line.strip_prefix("btime ")
                && let Ok(secs) = rest.trim().parse::<u64>()
            {
                let _ = BOOT_TIME.set(secs);
                return Ok(*BOOT_TIME.get().unwrap_or(&secs));
            }
        }
        Err(ProcActionError::Platform(
            "missing btime entry in /proc/stat".to_owned(),
        ))
    }

    #[cfg(target_os = "linux")]
    fn cached_linux_clk_tck() -> u64 {
        use std::sync::OnceLock;
        static CLK_TCK: OnceLock<u64> = OnceLock::new();
        *CLK_TCK.get_or_init(|| {
            // SAFETY: Querying POSIX clock ticks per second.
            let raw = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
            u64::try_from(raw).ok().filter(|&v| v > 0).unwrap_or(100)
        })
    }

    #[cfg(target_os = "linux")]
    fn cached_linux_page_size() -> u64 {
        use std::sync::OnceLock;
        static PAGE_SIZE: OnceLock<u64> = OnceLock::new();
        *PAGE_SIZE.get_or_init(|| {
            // SAFETY: Querying OS page size.
            let raw = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
            u64::try_from(raw).ok().filter(|&v| v > 0).unwrap_or(4096)
        })
    }

    #[cfg(target_os = "linux")]
    pub fn query_unix_process_meta(pid: u32) -> Result<super::UnixProcessMeta, ProcActionError> {
        let stat_path = format!("/proc/{pid}/stat");
        // Read raw bytes so process names containing non-UTF-8 bytes never fail parsing.
        let stat_bytes = std::fs::read(&stat_path).map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => ProcActionError::NotFound(pid),
            std::io::ErrorKind::PermissionDenied => ProcActionError::AccessDenied(pid),
            _ => ProcActionError::Platform(format!("failed to read {stat_path}: {err}")),
        })?;
        let parsed = super::parse_linux_proc_stat_bytes(&stat_bytes).ok_or_else(|| {
            ProcActionError::Platform(format!("malformed /proc/{pid}/stat record"))
        })?;
        let clk_tck = cached_linux_clk_tck();
        let btime = cached_linux_boot_time_secs()?;
        let create_time_100ns =
            super::unix_linux_ticks_to_identity(btime, parsed.starttime_ticks, clk_tck);
        let is_kernel_thread = super::is_linux_kernel_thread(pid, parsed.parent_pid, parsed.flags);
        let private_rss_bytes = std::fs::read_to_string(format!("/proc/{pid}/statm"))
            .ok()
            .and_then(|s| {
                super::parse_linux_proc_statm_private_bytes(&s, cached_linux_page_size())
            });
        Ok(super::UnixProcessMeta {
            create_time_100ns,
            is_kernel_thread,
            num_threads: parsed.num_threads,
            private_rss_bytes,
        })
    }

    #[cfg(target_os = "macos")]
    pub fn query_unix_process_meta(pid: u32) -> Result<super::UnixProcessMeta, ProcActionError> {
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

        let mut num_threads = 1u32;
        let mut task_info: libc::proc_taskinfo = unsafe { core::mem::zeroed() };
        let task_size = i32::try_from(size_of::<libc::proc_taskinfo>()).unwrap_or(0);
        // SAFETY: Valid output buffer for `PROC_PIDTASKINFO` of exact size `task_size`.
        let task_rc = unsafe {
            libc::proc_pidinfo(
                c_pid,
                libc::PROC_PIDTASKINFO,
                0,
                (&raw mut task_info).cast::<libc::c_void>(),
                task_size,
            )
        };
        if task_rc > 0 && task_info.pti_threadnum > 0 {
            num_threads = u32::try_from(task_info.pti_threadnum).unwrap_or(1);
        }

        let mut private_rss_bytes = None;
        let mut rusage: libc::rusage_info_v4 = unsafe { core::mem::zeroed() };
        // SAFETY: Valid `rusage_info_v4` buffer for `RUSAGE_INFO_V4` (`ri_phys_footprint`).
        let rusage_rc = unsafe {
            libc::proc_pid_rusage(
                c_pid,
                libc::RUSAGE_INFO_V4,
                (&raw mut rusage).cast::<libc::rusage_info_t>(),
            )
        };
        if rusage_rc == 0 {
            private_rss_bytes = Some(rusage.ri_phys_footprint);
        }

        Ok(super::UnixProcessMeta {
            create_time_100ns: super::unix_timeval_to_identity(
                info.pbi_start_tvsec,
                info.pbi_start_tvusec,
            ),
            is_kernel_thread: super::is_macos_system_process(info.pbi_flags),
            num_threads,
            private_rss_bytes,
        })
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub fn query_unix_process_meta(_pid: u32) -> Result<super::UnixProcessMeta, ProcActionError> {
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

        // Fail closed when the kernel lacks `pidfd_open` instead of falling back to racy bare `kill`.
        // SAFETY: Invoking Linux `SYS_pidfd_open(pid, 0)`.
        let pidfd_rc = unsafe { libc::syscall(libc::SYS_pidfd_open, raw_pid, 0 as libc::c_uint) };
        if pidfd_rc < 0 {
            let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            if err == libc::ENOSYS {
                return Err(ProcActionError::Platform(
                    "kernel lacks pidfd_open required for PID-reuse-safe signaling".to_owned(),
                ));
            }
            return Err(map_errno(pid, err, "pidfd_open"));
        }
        let pidfd = OwnedFd(i32::try_from(pidfd_rc).unwrap_or(-1));

        let meta = query_unix_process_meta(pid)?;
        guard_unix_signal_decision(
            pid,
            expected_create_time_100ns,
            meta.create_time_100ns,
            meta.is_kernel_thread,
        )?;

        // Confirm the pinned `pidfd` has not become readable (exited) before signaling; if it is
        // still alive, `/proc/<pid>/stat` is guaranteed to belong to this exact `struct pid`.
        let mut pfd = libc::pollfd {
            fd: pidfd.0,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: Valid single-element `pollfd` slice with immediate timeout 0.
        let poll_rc = unsafe { libc::poll(&raw mut pfd, 1, 0) };
        if poll_rc < 0 {
            let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            return Err(map_errno(pid, err, "poll(pidfd)"));
        }
        if poll_rc > 0 && (pfd.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR)) != 0 {
            return Err(ProcActionError::NotFound(pid));
        }

        // SAFETY: Delivering `sig` strictly through the verified, pinned `pidfd` (no bare `kill`).
        let rc = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                pidfd.0,
                sig,
                core::ptr::null::<libc::c_void>(),
                0 as libc::c_uint,
            )
        };
        if rc != 0 {
            let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            if err == libc::ENOSYS {
                return Err(ProcActionError::Platform(
                    "kernel lacks pidfd_send_signal required for PID-reuse-safe signaling"
                        .to_owned(),
                ));
            }
            return Err(map_errno(pid, err, "pidfd_send_signal"));
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

        // Pre-verify microsecond start time and `PROC_FLAG_SYSTEM` before touching the process.
        let meta = query_unix_process_meta(pid)?;
        guard_unix_signal_decision(
            pid,
            expected_create_time_100ns,
            meta.create_time_100ns,
            meta.is_kernel_thread,
        )?;

        // Darwin lacks a `pidfd` syscall; freeze the target with `SIGSTOP` so it cannot voluntarily
        // exit and recycle its PID, then re-verify microsecond identity before delivering `sig`.
        // SAFETY: Target already passed pre-verification; `SIGSTOP` is non-destructive and reversed
        // with `SIGCONT` if re-verification detects any mismatch.
        if unsafe { libc::kill(raw_pid, libc::SIGSTOP) } != 0 {
            let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            return Err(map_errno(pid, err, "kill(SIGSTOP)"));
        }

        let recheck = query_unix_process_meta(pid).and_then(|verified| {
            guard_unix_signal_decision(
                pid,
                expected_create_time_100ns,
                verified.create_time_100ns,
                verified.is_kernel_thread,
            )
        });
        if let Err(err) = recheck {
            // SAFETY: Resume the non-matching process untouched before returning the guard error.
            let _ = unsafe { libc::kill(raw_pid, libc::SIGCONT) };
            return Err(err);
        }

        // SAFETY: Target is stopped and re-verified with matching microsecond start time.
        if unsafe { libc::kill(raw_pid, sig) } != 0 {
            let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            if sig != libc::SIGKILL {
                let _ = unsafe { libc::kill(raw_pid, libc::SIGCONT) };
            }
            return Err(map_errno(pid, err, "kill"));
        }
        if sig != libc::SIGKILL {
            // Wake the stopped target so it immediately handles `SIGTERM`.
            // SAFETY: Target is the verified process that just received `SIGTERM`.
            let _ = unsafe { libc::kill(raw_pid, libc::SIGCONT) };
        }
        Ok(())
    }

    /// Queries the live start-time identity (`create_time_100ns`) for `pid` on Unix.
    #[cfg(test)]
    pub(crate) fn query_start_time_100ns(pid: u32) -> Result<u64, ProcActionError> {
        query_unix_process_meta(pid).map(|meta| meta.create_time_100ns)
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
    graceful_close_process, query_process_priority, query_unix_process_meta, set_process_priority,
    terminate_process,
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
    fn parses_linux_proc_stat_with_spaces_parentheses_and_non_utf8_bytes() {
        let stat =
            "1234 (kworker/0:1 (events)) S 2 0 0 0 -1 69238880 0 0 0 0 0 0 0 0 20 0 7 0 98765 0 0";
        let parsed = parse_linux_proc_stat_bytes(stat.as_bytes()).expect("parse /proc/stat line");
        assert_eq!(parsed.parent_pid, 2);
        assert_eq!(parsed.flags, 69_238_880);
        assert_eq!(parsed.num_threads, 7);
        assert_eq!(parsed.starttime_ticks, 98_765);
        assert_eq!(
            parse_linux_proc_stat_identity(stat),
            Some((2, 69_238_880, 98_765))
        );
        assert!(is_linux_kernel_thread(
            1234,
            parsed.parent_pid,
            parsed.flags
        ));
        assert!(is_linux_kernel_thread(2, 0, 0));
        assert!(is_linux_kernel_thread(555, 1, 0x0020_0000));
        assert!(!is_linux_kernel_thread(555, 1, 0x0040_0000));

        // Non-UTF-8 comm bytes must still parse cleanly because we locate the final `b')'`.
        let mut raw = b"4242 (bad\xff\xfe)name) R 1 4242 4242 0 -1 4194304 0 0 0 0 0 0 0 0 20 0 4 0 123456 0 0"
            .to_vec();
        let non_utf8 = parse_linux_proc_stat_bytes(&raw).expect("parse non-utf8 /proc/stat");
        assert_eq!(non_utf8.parent_pid, 1);
        assert_eq!(non_utf8.num_threads, 4);
        assert_eq!(non_utf8.starttime_ticks, 123_456);
        assert!(!is_linux_kernel_thread(
            4242,
            non_utf8.parent_pid,
            non_utf8.flags
        ));
        raw.clear();

        // Linux `/proc/<pid>/statm` private resident pages = (resident - shared) * page_size.
        assert_eq!(
            parse_linux_proc_statm_private_bytes("1000 500 200 10 0 300 0", 4096),
            Some(300 * 4096)
        );
        assert_eq!(
            parse_linux_proc_statm_private_bytes("1000 100 150 0 0 0 0", 4096),
            Some(0)
        );

        // macOS `PROC_FLAG_SYSTEM` is bit 0 (`0x1`), NOT `PROC_FLAG_PC_THROTTLE` (`0x200`).
        assert!(is_macos_system_process(0x0000_0001));
        assert!(!is_macos_system_process(0x0000_0200));

        // Sub-second FILETIME-epoch conversions distinguish same-second starts and align with 1601 epoch.
        let t1 = unix_linux_ticks_to_identity(1_700_000_000, 1005, 100);
        let t2 = unix_linux_ticks_to_identity(1_700_000_000, 1006, 100);
        assert!(t1 > WINDOWS_TO_UNIX_EPOCH_100NS);
        assert_eq!(t2 - t1, 100_000); // 10ms = 100,000 * 100ns

        let m1 = unix_timeval_to_identity(1_700_000_000, 123_456);
        let m2 = unix_timeval_to_identity(1_700_000_000, 123_457);
        assert_eq!(m2 - m1, 10); // 1us = 10 * 100ns
        assert_eq!(unix_start_time_secs_to_identity(0), 0);
        assert_eq!(
            unix_start_time_secs_to_identity(1_700_000_000),
            WINDOWS_TO_UNIX_EPOCH_100NS + 1_700_000_000 * 10_000_000
        );
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
