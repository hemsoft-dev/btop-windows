//! Low-level Windows NT/WDK FFI definitions, startup hardening, and waitable timers.
//!
//! All hand-written undocumented NT/WDK structs belong in this crate with
//! compile-time `size_of` and `offset_of` assertions for `x86_64`.

#![allow(non_camel_case_types, non_snake_case)]

#[cfg(windows)]
use core::mem::{offset_of, size_of};

/// Matches `SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION` in `winternl.h` / `phnt`.
///
/// Note: `kernel_time` **includes** `idle_time`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemProcessorPerformanceInformation {
    pub idle_time: i64,
    pub kernel_time: i64,
    pub user_time: i64,
    pub dpc_time: i64,
    pub interrupt_time: i64,
    pub interrupt_count: u32,
    pub reserved: u32,
}

#[cfg(windows)]
const _: () = {
    assert!(size_of::<SystemProcessorPerformanceInformation>() == 48);
    assert!(offset_of!(SystemProcessorPerformanceInformation, idle_time) == 0);
    assert!(offset_of!(SystemProcessorPerformanceInformation, kernel_time) == 8);
    assert!(offset_of!(SystemProcessorPerformanceInformation, user_time) == 16);
    assert!(offset_of!(SystemProcessorPerformanceInformation, interrupt_count) == 40);
};

/// Restricts default DLL search directories to `System32` to prevent DLL hijacking
/// when probing optional vendor libraries (`nvml.dll`).
#[cfg(windows)]
pub fn harden_process_startup() {
    use windows_sys::Win32::System::Diagnostics::Debug::{
        SEM_FAILCRITICALERRORS, SEM_NOOPENFILEERRORBOX, SetErrorMode,
    };
    use windows_sys::Win32::System::LibraryLoader::{
        LOAD_LIBRARY_SEARCH_SYSTEM32, SetDefaultDllDirectories,
    };

    // SAFETY: Process-wide startup configuration calls with constant flags.
    unsafe {
        let _ = SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32);
        let _ = SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX);
    }
}

/// No-op on non-Windows hosts used for portable unit testing.
#[cfg(not(windows))]
pub fn harden_process_startup() {}

#[cfg(windows)]
mod win {
    use core::ffi::c_void;
    use core::mem::size_of;
    use core::ptr;
    use std::sync::Arc;
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_SUCCESS, HANDLE, STATUS_INFO_LENGTH_MISMATCH, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
    use windows_sys::Win32::System::SystemInformation::{
        GetSystemInfo, GetTickCount64, SYSTEM_INFO,
    };
    use windows_sys::Win32::System::Threading::{
        CreateEventW, CreateWaitableTimerW, INFINITE, ResetEvent, SetEvent, SetWaitableTimer,
        WaitForMultipleObjects,
    };

    use super::SystemProcessorPerformanceInformation;

    const SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION_CLASS: u32 = 8;

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtQuerySystemInformation(
            system_information_class: u32,
            system_information: *mut c_void,
            system_information_length: u32,
            return_length: *mut u32,
        ) -> i32;
    }

    /// Queries per-logical-processor kernel/user/idle times for the active processor group
    /// (`<= 64` logical processors on English Windows 11 x64).
    pub fn query_processor_performance()
    -> Result<Vec<SystemProcessorPerformanceInformation>, String> {
        let mut sys_info: SYSTEM_INFO = unsafe { core::mem::zeroed() };
        // SAFETY: `SYSTEM_INFO` is POD and initialized by Win32.
        unsafe { GetSystemInfo(&raw mut sys_info) };
        let initial_count = usize::try_from(sys_info.dwNumberOfProcessors)
            .unwrap_or(1)
            .clamp(1, 64);

        let mut buf = vec![SystemProcessorPerformanceInformation::default(); initial_count];
        for _ in 0..4 {
            let byte_len =
                u32::try_from(buf.len() * size_of::<SystemProcessorPerformanceInformation>())
                    .map_err(|_| "processor performance buffer size overflow".to_owned())?;
            let mut returned_len = 0u32;

            // SAFETY: `buf` is valid writable memory of `byte_len` bytes aligned to the struct.
            let status = unsafe {
                NtQuerySystemInformation(
                    SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION_CLASS,
                    buf.as_mut_ptr().cast::<c_void>(),
                    byte_len,
                    &raw mut returned_len,
                )
            };

            if status >= 0 {
                let count =
                    (returned_len as usize) / size_of::<SystemProcessorPerformanceInformation>();
                buf.truncate(count.min(buf.len()));
                if buf.is_empty() {
                    return Err(
                        "NtQuerySystemInformation returned zero processor entries".to_owned()
                    );
                }
                return Ok(buf);
            }

            if status == STATUS_INFO_LENGTH_MISMATCH {
                let needed = (returned_len as usize)
                    .div_ceil(size_of::<SystemProcessorPerformanceInformation>())
                    .max(buf.len() * 2)
                    .min(64);
                buf.resize(needed, SystemProcessorPerformanceInformation::default());
                continue;
            }

            return Err(format!(
                "NtQuerySystemInformation(SystemProcessorPerformanceInformation) failed with NTSTATUS 0x{status:08X}"
            ));
        }

        Err("NtQuerySystemInformation buffer sizing did not converge".to_owned())
    }

    /// Reads the CPU brand string from Registry `ProcessorNameString`.
    pub fn read_cpu_model_name() -> Option<String> {
        let subkey: Vec<u16> = "HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0\0"
            .encode_utf16()
            .collect();
        let value: Vec<u16> = "ProcessorNameString\0".encode_utf16().collect();

        let mut bytes = 0u32;
        // SAFETY: Querying required byte size for `ProcessorNameString`.
        let size_status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                subkey.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                ptr::null_mut(),
                ptr::null_mut(),
                &raw mut bytes,
            )
        };
        if size_status != ERROR_SUCCESS || bytes < 2 {
            return None;
        }

        let wide_len = (bytes as usize).div_ceil(2);
        let mut buf = vec![0u16; wide_len];
        let mut actual_bytes = u32::try_from(buf.len() * 2).ok()?;
        // SAFETY: `buf` has space for `actual_bytes` bytes of UTF-16 string data.
        let read_status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                subkey.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                ptr::null_mut(),
                buf.as_mut_ptr().cast::<c_void>(),
                &raw mut actual_bytes,
            )
        };
        if read_status != ERROR_SUCCESS {
            return None;
        }

        let units = (actual_bytes as usize) / 2;
        let slice = &buf[..units.min(buf.len())];
        let trimmed_len = slice.iter().position(|&u| u == 0).unwrap_or(slice.len());
        let name = String::from_utf16_lossy(&slice[..trimmed_len])
            .trim()
            .to_owned();
        if name.is_empty() { None } else { Some(name) }
    }

    /// Returns system uptime in whole seconds from `GetTickCount64`.
    pub fn system_uptime_secs() -> u64 {
        // SAFETY: `GetTickCount64` takes no arguments and cannot fail.
        unsafe { GetTickCount64() / 1000 }
    }

    /// Result of waiting on [`WaitableWakeTimer`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum TimerWake {
        Elapsed,
        Woken,
    }

    #[derive(Debug)]
    struct OwnedHandle(HANDLE);

    // SAFETY: Win32 kernel timer/event handles can safely be shared across threads
    // and are closed exactly once when the final owner drops.
    unsafe impl Send for OwnedHandle {}
    unsafe impl Sync for OwnedHandle {}

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            // SAFETY: Handle was created by `CreateWaitableTimerW` / `CreateEventW` and is uniquely owned here.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    /// Cross-thread wakeable handle for interrupting [`WaitableWakeTimer`].
    #[derive(Debug, Clone)]
    pub struct TimerWaker {
        event: Arc<OwnedHandle>,
    }

    impl TimerWaker {
        /// Signals the waitable timer loop to wake immediately (e.g., cadence change or stop).
        pub fn wake(&self) {
            // SAFETY: `self.event` keeps the Win32 event handle alive across threads.
            unsafe {
                let _ = SetEvent(self.event.0);
            }
        }
    }

    /// Windows waitable timer paired with a manual-reset wakeup event so the sampler
    /// sleeps at zero CPU and wakes immediately on cadence adjustments or shutdown.
    #[derive(Debug)]
    pub struct WaitableWakeTimer {
        timer: OwnedHandle,
        wake_event: Arc<OwnedHandle>,
    }

    impl WaitableWakeTimer {
        pub fn new() -> Result<(Self, TimerWaker), String> {
            // SAFETY: Creating unnamed synchronization timer and manual-reset event.
            let raw_timer = unsafe { CreateWaitableTimerW(ptr::null(), 0, ptr::null()) };
            if raw_timer.is_null() {
                return Err("CreateWaitableTimerW failed".to_owned());
            }
            let timer = OwnedHandle(raw_timer);

            // SAFETY: Creating manual-reset event initially unsignaled.
            let raw_event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
            if raw_event.is_null() {
                return Err("CreateEventW failed".to_owned());
            }
            let wake_event = Arc::new(OwnedHandle(raw_event));

            let waker = TimerWaker {
                event: Arc::clone(&wake_event),
            };
            Ok((Self { timer, wake_event }, waker))
        }

        /// Sleeps until `duration` elapses or [`TimerWaker::wake`] is called.
        pub fn wait(&self, duration: Duration) -> Result<TimerWake, String> {
            let ticks_100ns = i64::try_from(duration.as_nanos() / 100)
                .unwrap_or(i64::MAX)
                .max(1);
            let due_time = -ticks_100ns;
            // SAFETY: Relative due time in negative 100ns intervals; one-shot timer (`period = 0`).
            let set_ok = unsafe {
                SetWaitableTimer(self.timer.0, &raw const due_time, 0, None, ptr::null(), 0)
            };
            if set_ok == 0 {
                return Err("SetWaitableTimer failed".to_owned());
            }

            let handles = [self.wake_event.0, self.timer.0];
            // SAFETY: Both handles are valid waitable Win32 kernel objects kept alive by `self`.
            let waited = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };
            if waited == WAIT_OBJECT_0 {
                unsafe {
                    let _ = ResetEvent(self.wake_event.0);
                }
                Ok(TimerWake::Woken)
            } else if waited == WAIT_OBJECT_0 + 1 {
                Ok(TimerWake::Elapsed)
            } else {
                Err(format!("WaitForMultipleObjects failed with code {waited}"))
            }
        }
    }
}

#[cfg(not(windows))]
mod stub {
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Duration;

    use super::SystemProcessorPerformanceInformation;

    pub fn query_processor_performance()
    -> Result<Vec<SystemProcessorPerformanceInformation>, String> {
        Err("Windows CPU telemetry is only available on Windows".to_owned())
    }

    pub fn read_cpu_model_name() -> Option<String> {
        None
    }

    pub fn system_uptime_secs() -> u64 {
        0
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum TimerWake {
        Elapsed,
        Woken,
    }

    #[derive(Debug, Clone)]
    pub struct TimerWaker {
        state: Arc<(Mutex<bool>, Condvar)>,
    }

    impl TimerWaker {
        pub fn wake(&self) {
            let (lock, cv) = &*self.state;
            let mut woken = match lock.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            *woken = true;
            cv.notify_all();
        }
    }

    #[derive(Debug)]
    pub struct WaitableWakeTimer {
        state: Arc<(Mutex<bool>, Condvar)>,
    }

    impl WaitableWakeTimer {
        pub fn new() -> Result<(Self, TimerWaker), String> {
            let state = Arc::new((Mutex::new(false), Condvar::new()));
            Ok((
                Self {
                    state: Arc::clone(&state),
                },
                TimerWaker { state },
            ))
        }

        pub fn wait(&self, duration: Duration) -> Result<TimerWake, String> {
            let (lock, cv) = &*self.state;
            let mut woken = match lock.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            if *woken {
                *woken = false;
                return Ok(TimerWake::Woken);
            }
            let (mut guard, result) = match cv.wait_timeout(woken, duration) {
                Ok(pair) => pair,
                Err(poisoned) => poisoned.into_inner(),
            };
            if *guard {
                *guard = false;
                Ok(TimerWake::Woken)
            } else if result.timed_out() {
                Ok(TimerWake::Elapsed)
            } else {
                Ok(TimerWake::Woken)
            }
        }
    }
}

#[cfg(not(windows))]
pub use stub::{
    TimerWake, TimerWaker, WaitableWakeTimer, query_processor_performance, read_cpu_model_name,
    system_uptime_secs,
};
#[cfg(windows)]
pub use win::{
    TimerWake, TimerWaker, WaitableWakeTimer, query_processor_performance, read_cpu_model_name,
    system_uptime_secs,
};
