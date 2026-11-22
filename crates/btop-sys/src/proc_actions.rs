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
            Self::CriticalProcess(pid) => write!(
                f,
                "Refused: PID {pid} is a critical Windows system process (terminating would cause a BSOD)"
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
/// Refuses kernel PIDs (`0`/`4`), missing or mismatched `CreateTime`, confirmed critical
/// system processes (`IsProcessCritical == TRUE`), and fail-safe errors when criticality
/// cannot be determined.
pub fn guard_terminate_decision(
    pid: u32,
    expected_create_time_100ns: u64,
    actual_create_time_100ns: u64,
    is_critical: Result<bool, &str>,
) -> Result<(), ProcActionError> {
    if matches!(pid, 0 | 4) {
        return Err(ProcActionError::CriticalProcess(pid));
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
    /// handle, and confirming `IsProcessCritical == FALSE` so an elevated user cannot trigger a BSOD.
    pub fn terminate_process(
        pid: u32,
        expected_create_time_100ns: u64,
    ) -> Result<(), ProcActionError> {
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

#[cfg(not(windows))]
pub fn query_process_priority(
    _pid: u32,
    _expected_create_time_100ns: u64,
) -> Result<PriorityClass, ProcActionError> {
    Err(ProcActionError::Platform(
        "process priority query requires Windows".to_owned(),
    ))
}

#[cfg(not(windows))]
pub fn set_process_priority(
    _pid: u32,
    _expected_create_time_100ns: u64,
    _priority: PriorityClass,
) -> Result<PriorityUpdateOutcome, ProcActionError> {
    Err(ProcActionError::Platform(
        "SetPriorityClass requires Windows".to_owned(),
    ))
}

#[cfg(not(windows))]
pub fn terminate_process(pid: u32, expected_create_time_100ns: u64) -> Result<(), ProcActionError> {
    guard_terminate_decision(
        pid,
        expected_create_time_100ns,
        expected_create_time_100ns,
        Ok(false),
    )?;
    Err(ProcActionError::Platform(
        "TerminateProcess requires Windows".to_owned(),
    ))
}

#[cfg(not(windows))]
pub fn graceful_close_process(
    pid: u32,
    expected_create_time_100ns: u64,
) -> Result<usize, ProcActionError> {
    guard_process_identity(pid, expected_create_time_100ns, expected_create_time_100ns)?;
    Err(ProcActionError::Platform(
        "WM_CLOSE requires Windows".to_owned(),
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

        // Verified non-critical user process is permitted.
        assert_eq!(
            guard_terminate_decision(4820, 1000, 1000, Ok(false)),
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
}
