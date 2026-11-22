//! Safe, bounded Win32/NT per-process metadata queries:
//! - `NtQueryInformationProcess(ProcessCommandLineInformation = 60)` with `CreateTime` guard
//! - `NtQuerySystemInformation(SystemProcessIdInformation = 88)` for full image paths (works on PPL)
//! - `OpenProcessToken` + `GetTokenInformation(TokenUser)` + `LookupAccountSidW` locally cached by SID

use core::mem::{offset_of, size_of};
use std::collections::HashMap;

use crate::UnicodeString;

/// Explicit query outcome distinguishing privilege denial (e.g., PPL) from other unavailability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcFieldQuery<T> {
    Available(T),
    Denied,
    Unavailable(String),
}

impl<T> ProcFieldQuery<T> {
    pub fn into_option(self) -> Option<T> {
        match self {
            Self::Available(v) => Some(v),
            Self::Denied | Self::Unavailable(_) => None,
        }
    }

    pub const fn is_denied(&self) -> bool {
        matches!(self, Self::Denied)
    }
}

/// Matches `SYSTEM_PROCESS_ID_INFORMATION` in `phnt` (24 bytes on `x86_64`).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemProcessIdInformation {
    pub process_id: usize,
    pub image_name: UnicodeString,
}

#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(size_of::<SystemProcessIdInformation>() == 24);
    assert!(offset_of!(SystemProcessIdInformation, process_id) == 0);
    assert!(offset_of!(SystemProcessIdInformation, image_name) == 8);
};

/// Local in-memory cache mapping raw binary Windows SID bytes to resolved `DOMAIN\User` names.
#[derive(Debug, Default, Clone)]
pub struct SidAccountCache {
    entries: HashMap<Vec<u8>, String>,
    hits: u64,
    misses: u64,
}

impl SidAccountCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolves `sid_bytes` from cache or calls `lookup` once on a cache miss.
    pub fn resolve_with<F>(&mut self, sid_bytes: &[u8], lookup: F) -> Option<String>
    where
        F: FnOnce() -> Option<String>,
    {
        if let Some(cached) = self.entries.get(sid_bytes) {
            self.hits = self.hits.saturating_add(1);
            return Some(cached.clone());
        }
        self.misses = self.misses.saturating_add(1);
        let resolved = lookup()?;
        self.entries.insert(sid_bytes.to_vec(), resolved.clone());
        Some(resolved)
    }

    pub const fn hits(&self) -> u64 {
        self.hits
    }

    pub const fn misses(&self) -> u64 {
        self.misses
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Translates an NT device path (e.g., `\Device\HarddiskVolume3\Windows\System32\lsass.exe`)
/// into a Win32 drive path (`C:\Windows\System32\lsass.exe`) using a `(nt_prefix, drive)` table.
pub fn translate_nt_device_path(nt_path: &str, device_map: &[(String, String)]) -> String {
    for (prefix, drive) in device_map {
        if let Some(rest) = nt_path.strip_prefix(prefix.as_str())
            && (rest.is_empty() || rest.starts_with('\\'))
        {
            return format!("{drive}{rest}");
        }
        // Case-insensitive match for English Windows NT device paths.
        if nt_path.len() >= prefix.len()
            && nt_path.is_char_boundary(prefix.len())
            && nt_path[..prefix.len()].eq_ignore_ascii_case(prefix)
        {
            let rest = &nt_path[prefix.len()..];
            if rest.is_empty() || rest.starts_with('\\') {
                return format!("{drive}{rest}");
            }
        }
    }
    nt_path.to_owned()
}

#[cfg(windows)]
mod win {
    use core::ffi::c_void;
    use core::mem::size_of;
    use core::ptr;

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_INSUFFICIENT_BUFFER, FILETIME, GetLastError,
        HANDLE, STATUS_ACCESS_DENIED, STATUS_BUFFER_OVERFLOW, STATUS_BUFFER_TOO_SMALL,
        STATUS_INFO_LENGTH_MISMATCH,
    };
    use windows_sys::Win32::Security::{
        GetLengthSid, GetTokenInformation, IsValidSid, LookupAccountSidW, SID_NAME_USE,
        TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::QueryDosDeviceW;
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    use super::{
        ProcFieldQuery, SidAccountCache, SystemProcessIdInformation, translate_nt_device_path,
    };
    use crate::UnicodeString;

    const PROCESS_COMMAND_LINE_INFORMATION: u32 = 60;
    const SYSTEM_PROCESS_ID_INFORMATION_CLASS: u32 = 88;
    const INITIAL_CMDLINE_BYTES: usize = 2048;
    const MAX_CMDLINE_BYTES: usize = 256 * 1024;
    const INITIAL_IMAGE_PATH_WCHARS: usize = 260;
    const MAX_IMAGE_PATH_WCHARS: usize = 32_768;

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtQueryInformationProcess(
            process_handle: HANDLE,
            process_information_class: u32,
            process_information: *mut c_void,
            process_information_length: u32,
            return_length: *mut u32,
        ) -> i32;

        fn NtQuerySystemInformation(
            system_information_class: u32,
            system_information: *mut c_void,
            system_information_length: u32,
            return_length: *mut u32,
        ) -> i32;
    }

    struct OwnedWinHandle(HANDLE);

    impl Drop for OwnedWinHandle {
        fn drop(&mut self) {
            // SAFETY: Handle was returned by Win32 `OpenProcess` or `OpenProcessToken`.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    fn filetime_to_u64(ft: FILETIME) -> u64 {
        (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)
    }

    fn open_verified_process(
        pid: u32,
        expected_create_time_100ns: u64,
    ) -> ProcFieldQuery<OwnedWinHandle> {
        if matches!(pid, 0 | 4) {
            return ProcFieldQuery::Denied;
        }

        // SAFETY: Opening process with minimal read-only query rights (`PROCESS_QUERY_LIMITED_INFORMATION`).
        let raw_handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if raw_handle.is_null() {
            // SAFETY: Reading thread-local Win32 error code immediately after failed `OpenProcess`.
            let err = unsafe { GetLastError() };
            return if err == ERROR_ACCESS_DENIED {
                ProcFieldQuery::Denied
            } else {
                ProcFieldQuery::Unavailable(format!("OpenProcess failed ({err})"))
            };
        }
        let handle = OwnedWinHandle(raw_handle);

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
            if ok == 0 {
                let err = unsafe { GetLastError() };
                return if err == ERROR_ACCESS_DENIED {
                    ProcFieldQuery::Denied
                } else {
                    ProcFieldQuery::Unavailable(format!("GetProcessTimes failed ({err})"))
                };
            }
            if filetime_to_u64(creation) != expected_create_time_100ns {
                return ProcFieldQuery::Unavailable("process identity changed".to_owned());
            }
        }

        ProcFieldQuery::Available(handle)
    }

    /// Queries the user-mode command line of `pid` via `NtQueryInformationProcess(60)`
    /// (`ProcessCommandLineInformation`), verifying `GetProcessTimes` matches
    /// `expected_create_time_100ns` before reading.
    pub fn query_process_command_line_state(
        pid: u32,
        expected_create_time_100ns: u64,
    ) -> ProcFieldQuery<String> {
        let handle = match open_verified_process(pid, expected_create_time_100ns) {
            ProcFieldQuery::Available(h) => h,
            ProcFieldQuery::Denied => return ProcFieldQuery::Denied,
            ProcFieldQuery::Unavailable(reason) => return ProcFieldQuery::Unavailable(reason),
        };

        let mut buf = vec![0u8; INITIAL_CMDLINE_BYTES];
        for _ in 0..4 {
            let Ok(buf_len) = u32::try_from(buf.len()) else {
                return ProcFieldQuery::Unavailable("command line buffer overflow".to_owned());
            };
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
                return match decode_cmdline_buffer(&buf[..valid_len]) {
                    Some(cmd) => ProcFieldQuery::Available(cmd),
                    None => ProcFieldQuery::Unavailable("empty command line".to_owned()),
                };
            }

            if status == STATUS_ACCESS_DENIED {
                return ProcFieldQuery::Denied;
            }

            if matches!(
                status,
                STATUS_INFO_LENGTH_MISMATCH | STATUS_BUFFER_TOO_SMALL | STATUS_BUFFER_OVERFLOW
            ) {
                let needed = (returned_len as usize).max(buf.len().saturating_mul(2));
                if needed > MAX_CMDLINE_BYTES {
                    return ProcFieldQuery::Unavailable("command line exceeds limit".to_owned());
                }
                buf.resize(needed, 0);
                continue;
            }

            return ProcFieldQuery::Unavailable(format!(
                "ProcessCommandLineInformation failed (0x{status:08X})"
            ));
        }

        ProcFieldQuery::Unavailable("ProcessCommandLineInformation did not converge".to_owned())
    }

    /// Convenience wrapper returning `Some(command_line)` when accessible, or `None` otherwise.
    pub fn query_process_command_line(pid: u32, expected_create_time_100ns: u64) -> Option<String> {
        query_process_command_line_state(pid, expected_create_time_100ns).into_option()
    }

    /// Builds a `(nt_device_prefix, drive_letter)` lookup table via `QueryDosDeviceW` (`A:`..`Z:`)
    /// sorted longest-prefix-first for translating `SystemProcessIdInformation` paths.
    pub fn query_nt_device_drive_map() -> Vec<(String, String)> {
        let mut entries = Vec::new();
        let mut wide_buf = vec![0u16; 512];
        for letter in b'A'..=b'Z' {
            let drive = format!("{}:", char::from(letter));
            let drive_wide: Vec<u16> = drive.encode_utf16().chain(core::iter::once(0)).collect();
            let buf_len = u32::try_from(wide_buf.len()).unwrap_or(512);
            // SAFETY: `drive_wide` is NUL-terminated UTF-16 and `wide_buf` has `buf_len` elements.
            let copied =
                unsafe { QueryDosDeviceW(drive_wide.as_ptr(), wide_buf.as_mut_ptr(), buf_len) };
            if copied == 0 {
                continue;
            }
            let slice = &wide_buf[..(copied as usize).min(wide_buf.len())];
            let first_end = slice.iter().position(|&u| u == 0).unwrap_or(slice.len());
            let target = String::from_utf16_lossy(&slice[..first_end]);
            if !target.is_empty() {
                entries.push((target, drive));
            }
        }
        entries.sort_by_key(|a| core::cmp::Reverse(a.0.len()));
        entries
    }

    /// Queries the full executable image path of `pid` via
    /// `NtQuerySystemInformation(SystemProcessIdInformation = 88)` without requiring `OpenProcess`,
    /// so PPL processes such as `csrss.exe` and `lsass.exe` still report their image path.
    pub fn query_process_image_path(
        pid: u32,
        device_map: &[(String, String)],
    ) -> ProcFieldQuery<String> {
        if matches!(pid, 0 | 4) {
            return ProcFieldQuery::Unavailable("kernel process".to_owned());
        }

        let mut wide_buf = vec![0u16; INITIAL_IMAGE_PATH_WCHARS];
        for _ in 0..4 {
            let max_bytes = u16::try_from(wide_buf.len().saturating_mul(2)).unwrap_or(u16::MAX - 1);
            let mut info = SystemProcessIdInformation {
                process_id: pid as usize,
                image_name: UnicodeString {
                    length: 0,
                    maximum_length: max_bytes,
                    buffer: wide_buf.as_ptr(),
                },
            };
            let mut returned_len = 0u32;
            let info_len = u32::try_from(size_of::<SystemProcessIdInformation>()).unwrap_or(24);

            // SAFETY: `info` points to `wide_buf` with valid byte capacity `maximum_length`.
            let status = unsafe {
                NtQuerySystemInformation(
                    SYSTEM_PROCESS_ID_INFORMATION_CLASS,
                    (&raw mut info).cast::<c_void>(),
                    info_len,
                    &raw mut returned_len,
                )
            };

            if status >= 0 {
                let units = (usize::from(info.image_name.length) / 2).min(wide_buf.len());
                let raw = String::from_utf16_lossy(&wide_buf[..units])
                    .trim_matches('\0')
                    .trim()
                    .to_owned();
                if raw.is_empty() {
                    return ProcFieldQuery::Unavailable("empty image path".to_owned());
                }
                return ProcFieldQuery::Available(translate_nt_device_path(&raw, device_map));
            }

            if status == STATUS_ACCESS_DENIED {
                return ProcFieldQuery::Denied;
            }

            if status == STATUS_INFO_LENGTH_MISMATCH {
                let needed_wchars = usize::from(info.image_name.maximum_length)
                    .div_ceil(2)
                    .max(wide_buf.len().saturating_mul(2));
                if needed_wchars > MAX_IMAGE_PATH_WCHARS {
                    return ProcFieldQuery::Unavailable("image path exceeds limit".to_owned());
                }
                wide_buf.resize(needed_wchars, 0);
                continue;
            }

            return ProcFieldQuery::Unavailable(format!(
                "SystemProcessIdInformation failed (0x{status:08X})"
            ));
        }

        ProcFieldQuery::Unavailable("SystemProcessIdInformation did not converge".to_owned())
    }

    /// Queries the owner user (`DOMAIN\User`) of `pid` via `OpenProcessToken` +
    /// `GetTokenInformation(TokenUser)` + `LookupAccountSidW`, locally cached by raw SID bytes.
    pub fn query_process_owner(
        pid: u32,
        expected_create_time_100ns: u64,
        sid_cache: &mut SidAccountCache,
    ) -> ProcFieldQuery<String> {
        let proc_handle = match open_verified_process(pid, expected_create_time_100ns) {
            ProcFieldQuery::Available(h) => h,
            ProcFieldQuery::Denied => return ProcFieldQuery::Denied,
            ProcFieldQuery::Unavailable(reason) => return ProcFieldQuery::Unavailable(reason),
        };

        let mut raw_token: HANDLE = ptr::null_mut();
        // SAFETY: Opening process token with read-only `TOKEN_QUERY` access.
        let tok_ok = unsafe { OpenProcessToken(proc_handle.0, TOKEN_QUERY, &raw mut raw_token) };
        if tok_ok == 0 || raw_token.is_null() {
            let err = unsafe { GetLastError() };
            return if err == ERROR_ACCESS_DENIED {
                ProcFieldQuery::Denied
            } else {
                ProcFieldQuery::Unavailable(format!("OpenProcessToken failed ({err})"))
            };
        }
        let token = OwnedWinHandle(raw_token);

        let mut needed_bytes = 0u32;
        // SAFETY: First call queries required `TOKEN_USER` buffer size.
        let _ = unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                ptr::null_mut(),
                0,
                &raw mut needed_bytes,
            )
        };
        if needed_bytes == 0 || (needed_bytes as usize) < size_of::<TOKEN_USER>() {
            let err = unsafe { GetLastError() };
            return if err == ERROR_ACCESS_DENIED {
                ProcFieldQuery::Denied
            } else {
                ProcFieldQuery::Unavailable(format!("GetTokenInformation size failed ({err})"))
            };
        }

        let u64_len = (needed_bytes as usize).div_ceil(size_of::<u64>());
        let mut buf = vec![0u64; u64_len];
        let mut actual_bytes = 0u32;
        let buf_bytes = u32::try_from(buf.len() * size_of::<u64>()).unwrap_or(needed_bytes);
        // SAFETY: `buf` is 8-byte aligned and at least `needed_bytes` long.
        let ok = unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                buf.as_mut_ptr().cast::<c_void>(),
                buf_bytes,
                &raw mut actual_bytes,
            )
        };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            return if err == ERROR_ACCESS_DENIED {
                ProcFieldQuery::Denied
            } else {
                ProcFieldQuery::Unavailable(format!("GetTokenInformation failed ({err})"))
            };
        }

        // SAFETY: `buf` holds a valid `TOKEN_USER` structure written by Win32.
        let token_user = unsafe { &*buf.as_ptr().cast::<TOKEN_USER>() };
        let sid = token_user.User.Sid;
        if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
            return ProcFieldQuery::Unavailable("invalid token SID".to_owned());
        }
        let sid_len = unsafe { GetLengthSid(sid) } as usize;
        if sid_len == 0 || sid_len > 256 {
            return ProcFieldQuery::Unavailable("unexpected SID length".to_owned());
        }
        // SAFETY: `IsValidSid(sid)` succeeded and `GetLengthSid(sid)` gives its exact byte length.
        let sid_bytes = unsafe { core::slice::from_raw_parts(sid.cast::<u8>(), sid_len) };

        match sid_cache.resolve_with(sid_bytes, || lookup_account_sid_local(sid)) {
            Some(name) => ProcFieldQuery::Available(name),
            None => ProcFieldQuery::Unavailable("LookupAccountSidW unresolved".to_owned()),
        }
    }

    fn lookup_account_sid_local(sid: *mut c_void) -> Option<String> {
        let mut name_len = 0u32;
        let mut domain_len = 0u32;
        let mut sid_use: SID_NAME_USE = 0;

        // SAFETY: First call queries required UTF-16 buffer lengths for local account lookup.
        let first_ok = unsafe {
            LookupAccountSidW(
                ptr::null(),
                sid,
                ptr::null_mut(),
                &raw mut name_len,
                ptr::null_mut(),
                &raw mut domain_len,
                &raw mut sid_use,
            )
        };
        if first_ok != 0 || unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER {
            return None;
        }
        if name_len == 0 {
            return None;
        }

        let mut name_buf = vec![0u16; (name_len as usize).min(512)];
        let mut domain_buf = vec![0u16; (domain_len as usize).clamp(1, 512)];
        let mut actual_name_len = u32::try_from(name_buf.len()).ok()?;
        let mut actual_domain_len = u32::try_from(domain_buf.len()).ok()?;

        // SAFETY: Output buffers are sized to the required lengths returned by Win32.
        let ok = unsafe {
            LookupAccountSidW(
                ptr::null(),
                sid,
                name_buf.as_mut_ptr(),
                &raw mut actual_name_len,
                domain_buf.as_mut_ptr(),
                &raw mut actual_domain_len,
                &raw mut sid_use,
            )
        };
        if ok == 0 {
            return None;
        }

        let name =
            String::from_utf16_lossy(&name_buf[..(actual_name_len as usize).min(name_buf.len())])
                .trim_matches('\0')
                .to_owned();
        let domain = String::from_utf16_lossy(
            &domain_buf[..(actual_domain_len as usize).min(domain_buf.len())],
        )
        .trim_matches('\0')
        .to_owned();
        if name.is_empty() {
            return None;
        }
        if domain.is_empty() {
            Some(name)
        } else {
            Some(format!("{domain}\\{name}"))
        }
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
pub use win::{
    query_nt_device_drive_map, query_process_command_line, query_process_command_line_state,
    query_process_image_path, query_process_owner,
};

#[cfg(not(windows))]
pub fn query_process_command_line(_pid: u32, _expected_create_time_100ns: u64) -> Option<String> {
    None
}

#[cfg(not(windows))]
pub fn query_process_command_line_state(
    _pid: u32,
    _expected_create_time_100ns: u64,
) -> ProcFieldQuery<String> {
    ProcFieldQuery::Unavailable("non-Windows host".to_owned())
}

#[cfg(not(windows))]
pub fn query_nt_device_drive_map() -> Vec<(String, String)> {
    Vec::new()
}

#[cfg(not(windows))]
pub fn query_process_image_path(
    _pid: u32,
    _device_map: &[(String, String)],
) -> ProcFieldQuery<String> {
    ProcFieldQuery::Unavailable("non-Windows host".to_owned())
}

#[cfg(not(windows))]
pub fn query_process_owner(
    _pid: u32,
    _expected_create_time_100ns: u64,
    _sid_cache: &mut SidAccountCache,
) -> ProcFieldQuery<String> {
    ProcFieldQuery::Unavailable("non-Windows host".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sid_account_cache_records_hits_and_avoids_duplicate_lookups() {
        let mut cache = SidAccountCache::new();
        let sid_a = [1u8, 5, 0, 0, 0, 0, 0, 5, 21, 0, 0, 0];
        let sid_b = [1u8, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0];
        let mut calls = 0u32;

        let first = cache.resolve_with(&sid_a, || {
            calls += 1;
            Some("HOME\\franz".to_owned())
        });
        assert_eq!(first.as_deref(), Some("HOME\\franz"));
        assert_eq!(calls, 1);
        assert_eq!(cache.misses(), 1);
        assert_eq!(cache.hits(), 0);

        let second = cache.resolve_with(&sid_a, || {
            calls += 1;
            Some("SHOULD_NOT_CALL".to_owned())
        });
        assert_eq!(second.as_deref(), Some("HOME\\franz"));
        assert_eq!(calls, 1);
        assert_eq!(cache.misses(), 1);
        assert_eq!(cache.hits(), 1);

        let system = cache.resolve_with(&sid_b, || {
            calls += 1;
            Some("NT AUTHORITY\\SYSTEM".to_owned())
        });
        assert_eq!(system.as_deref(), Some("NT AUTHORITY\\SYSTEM"));
        assert_eq!(calls, 2);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn translates_nt_device_paths_to_win32_drive_letters() {
        let map = vec![
            ("\\Device\\HarddiskVolume10".to_owned(), "D:".to_owned()),
            ("\\Device\\HarddiskVolume3".to_owned(), "C:".to_owned()),
        ];
        assert_eq!(
            translate_nt_device_path(
                "\\Device\\HarddiskVolume3\\Windows\\System32\\csrss.exe",
                &map
            ),
            "C:\\Windows\\System32\\csrss.exe"
        );
        assert_eq!(
            translate_nt_device_path("\\device\\harddiskvolume10\\Games\\app.exe", &map),
            "D:\\Games\\app.exe"
        );
        // Must not partially match HarddiskVolume1 against HarddiskVolume10.
        assert_eq!(
            translate_nt_device_path("\\Device\\HarddiskVolume30\\foo.exe", &map),
            "\\Device\\HarddiskVolume30\\foo.exe"
        );
    }

    #[cfg(windows)]
    #[test]
    fn live_windows_queries_current_process_image_path_and_owner_with_sid_cache() {
        let pid = std::process::id();
        let dev_map = query_nt_device_drive_map();
        let img = query_process_image_path(pid, &dev_map);
        match img {
            ProcFieldQuery::Available(path) => {
                assert!(path.to_ascii_lowercase().ends_with(".exe"), "path={path}");
            }
            other => panic!("expected Available image path for current process, got {other:?}"),
        }

        let mut cache = SidAccountCache::new();
        let owner1 = query_process_owner(pid, 0, &mut cache);
        let owner2 = query_process_owner(pid, 0, &mut cache);
        assert!(matches!(owner1, ProcFieldQuery::Available(ref s) if !s.is_empty()));
        assert_eq!(owner1, owner2);
        assert_eq!(cache.misses(), 1);
        assert_eq!(cache.hits(), 1);
    }
}
