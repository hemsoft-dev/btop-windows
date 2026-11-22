//! Safe, bounded Win32/NT per-process metadata queries (`ProcessCommandLineInformation`)
//! with `CreateTime` verification to prevent Windows PID-reuse mismatches.

#[cfg(windows)]
mod win {
    use core::ffi::c_void;
    use core::mem::size_of;

    use windows_sys::Win32::Foundation::{
        CloseHandle, FILETIME, HANDLE, STATUS_BUFFER_OVERFLOW, STATUS_BUFFER_TOO_SMALL,
        STATUS_INFO_LENGTH_MISMATCH,
    };
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    use crate::UnicodeString;

    const PROCESS_COMMAND_LINE_INFORMATION: u32 = 60;
    const INITIAL_CMDLINE_BYTES: usize = 2048;
    const MAX_CMDLINE_BYTES: usize = 256 * 1024;

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtQueryInformationProcess(
            process_handle: HANDLE,
            process_information_class: u32,
            process_information: *mut c_void,
            process_information_length: u32,
            return_length: *mut u32,
        ) -> i32;
    }

    struct ProcessHandle(HANDLE);

    impl Drop for ProcessHandle {
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

    /// Queries the user-mode command line of `pid` via `NtQueryInformationProcess(60)`
    /// (`ProcessCommandLineInformation`), verifying `GetProcessTimes` matches
    /// `expected_create_time_100ns` before reading.
    ///
    /// Returns `None` cleanly for kernel processes, PPL/access-denied processes, PID-reuse
    /// mismatches, or empty command lines.
    pub fn query_process_command_line(pid: u32, expected_create_time_100ns: u64) -> Option<String> {
        if matches!(pid, 0 | 4) {
            return None;
        }

        // SAFETY: Opening process with minimal read-only query rights (`PROCESS_QUERY_LIMITED_INFORMATION`).
        let raw_handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if raw_handle.is_null() {
            return None;
        }
        let handle = ProcessHandle(raw_handle);

        if expected_create_time_100ns > 0 {
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
            // SAFETY: Valid process handle and writable FILETIME output pointers.
            let ok = unsafe {
                GetProcessTimes(
                    handle.0,
                    &raw mut creation,
                    &raw mut exit,
                    &raw mut kernel,
                    &raw mut user,
                )
            };
            if ok == 0 || filetime_to_u64(creation) != expected_create_time_100ns {
                return None;
            }
        }

        let mut buf = vec![0u8; INITIAL_CMDLINE_BYTES];
        for _ in 0..4 {
            let buf_len = u32::try_from(buf.len()).ok()?;
            let mut returned_len = 0u32;

            // SAFETY: `buf` provides `buf_len` writable bytes for `UNICODE_STRING` + UTF-16 payload.
            let status = unsafe {
                NtQueryInformationProcess(
                    handle.0,
                    PROCESS_COMMAND_LINE_INFORMATION,
                    buf.as_mut_ptr().cast::<c_void>(),
                    buf_len,
                    &raw mut returned_len,
                )
            };

            if status >= 0 {
                let valid_len = (returned_len as usize).min(buf.len());
                return decode_cmdline_buffer(&buf[..valid_len]);
            }

            if matches!(
                status,
                STATUS_INFO_LENGTH_MISMATCH | STATUS_BUFFER_TOO_SMALL | STATUS_BUFFER_OVERFLOW
            ) {
                let needed = (returned_len as usize).max(buf.len().saturating_mul(2));
                if needed > MAX_CMDLINE_BYTES {
                    return None;
                }
                buf.resize(needed, 0);
                continue;
            }

            return None;
        }
        None
    }

    fn decode_cmdline_buffer(raw: &[u8]) -> Option<String> {
        if raw.len() < size_of::<UnicodeString>() {
            return None;
        }
        // SAFETY: `raw.len() >= size_of::<UnicodeString>()`; `read_unaligned` handles any alignment.
        let header = unsafe { raw.as_ptr().cast::<UnicodeString>().read_unaligned() };
        if header.length == 0 || header.buffer.is_null() || !header.length.is_multiple_of(2) {
            return None;
        }

        let buf_start = raw.as_ptr() as usize;
        let buf_end = buf_start.checked_add(raw.len())?;
        let ptr_addr = header.buffer as usize;
        let byte_len = usize::from(header.length);
        let ptr_end = ptr_addr.checked_add(byte_len)?;
        if ptr_addr < buf_start || ptr_end > buf_end {
            return None;
        }

        let unit_count = byte_len / 2;
        let mut units = Vec::with_capacity(unit_count);
        for idx in 0..unit_count {
            // SAFETY: `[ptr_addr, ptr_end)` is strictly inside `[buf_start, buf_end)`.
            let unit = unsafe { header.buffer.add(idx).read_unaligned() };
            units.push(unit);
        }
        let decoded = String::from_utf16_lossy(&units)
            .trim_matches('\0')
            .trim()
            .to_owned();
        if decoded.is_empty() {
            None
        } else {
            Some(decoded)
        }
    }
}

#[cfg(windows)]
pub use win::query_process_command_line;

#[cfg(not(windows))]
pub fn query_process_command_line(_pid: u32, _expected_create_time_100ns: u64) -> Option<String> {
    None
}
