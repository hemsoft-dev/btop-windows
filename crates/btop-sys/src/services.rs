//! Windows Service Control Manager (SCM) read-only enumeration (`EnumServicesStatusExW` +
//! `QueryServiceConfigW`), process elevation check (`TokenElevation`), and elevated Start/Stop.

use std::fmt;

/// Raw service record decoded from `ENUM_SERVICE_STATUS_PROCESSW` + `QUERY_SERVICE_CONFIGW`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawServiceInfo {
    pub name: String,
    pub display_name: String,
    pub state: String,
    pub start_type: String,
    pub pid: Option<u32>,
}

/// Errors returned by elevated service Start (`StartServiceW`) and Stop (`ControlService`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceActionError {
    /// Caller is not running elevated as Administrator.
    NotElevated,
    /// SCM or the service DACL denied the requested control right.
    AccessDenied(String),
    /// Service name does not exist in SCM.
    NotFound(String),
    /// Service is already in the requested state or cannot accept the control right now.
    InvalidState { service: String, detail: String },
    /// Other Win32 / platform error.
    Platform(String),
}

impl fmt::Display for ServiceActionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotElevated => write!(
                f,
                "Administrator privileges required: run btop-windows elevated to start or stop services"
            ),
            Self::AccessDenied(name) => write!(
                f,
                "Access denied controlling service '{name}' (protected or insufficient privileges)"
            ),
            Self::NotFound(name) => write!(f, "Service '{name}' was not found in SCM"),
            Self::InvalidState { service, detail } => {
                write!(f, "Cannot change state of service '{service}': {detail}")
            }
            Self::Platform(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for ServiceActionError {}

/// Decodes Win32 `SERVICE_STATUS_PROCESS::dwCurrentState` into a concise human-readable label.
pub const fn decode_service_state(code: u32) -> &'static str {
    match code {
        1 => "Stopped",
        2 => "Start Pending",
        3 => "Stop Pending",
        4 => "Running",
        5 => "Continue Pending",
        6 => "Pause Pending",
        7 => "Paused",
        _ => "Unknown",
    }
}

/// Decodes Win32 `QUERY_SERVICE_CONFIGW::dwStartType` into a concise start-type label.
pub const fn decode_service_start_type(code: u32) -> &'static str {
    match code {
        0 => "Boot",
        1 => "System",
        2 => "Auto",
        3 => "Manual",
        4 => "Disabled",
        _ => "Unknown",
    }
}

#[cfg(windows)]
mod win {
    use core::ffi::c_void;
    use core::mem::size_of;
    use core::ptr;
    use std::collections::BTreeMap;

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_INSUFFICIENT_BUFFER, ERROR_MORE_DATA,
        ERROR_SERVICE_ALREADY_RUNNING, ERROR_SERVICE_CANNOT_ACCEPT_CTRL,
        ERROR_SERVICE_DOES_NOT_EXIST, ERROR_SERVICE_NOT_ACTIVE, GetLastError, HANDLE,
    };
    use windows_sys::Win32::Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    };
    use windows_sys::Win32::System::Services::{
        CloseServiceHandle, ControlService, ENUM_SERVICE_STATUS_PROCESSW, EnumServicesStatusExW,
        OpenSCManagerW, OpenServiceW, QUERY_SERVICE_CONFIGW, QueryServiceConfigW,
        SC_ENUM_PROCESS_INFO, SC_HANDLE, SC_MANAGER_CONNECT, SC_MANAGER_ENUMERATE_SERVICE,
        SERVICE_CONTROL_STOP, SERVICE_QUERY_CONFIG, SERVICE_START, SERVICE_STATE_ALL,
        SERVICE_STATUS, SERVICE_STOP, SERVICE_WIN32, StartServiceW,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    use super::{
        RawServiceInfo, ServiceActionError, decode_service_start_type, decode_service_state,
    };

    const MAX_SCM_ENUM_BYTES: usize = 8 * 1024 * 1024;
    const MAX_SVC_CONFIG_BYTES: usize = 64 * 1024;

    struct ScmHandle(SC_HANDLE);

    impl Drop for ScmHandle {
        fn drop(&mut self) {
            // SAFETY: Handle was returned by `OpenSCManagerW` or `OpenServiceW`.
            unsafe {
                let _ = CloseServiceHandle(self.0);
            }
        }
    }

    struct TokenHandle(HANDLE);

    impl Drop for TokenHandle {
        fn drop(&mut self) {
            // SAFETY: Handle was returned by `OpenProcessToken`.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    /// Returns `true` when the current process token is elevated (`TokenElevation.TokenIsElevated != 0`).
    pub fn is_process_elevated() -> bool {
        let mut raw_token: HANDLE = ptr::null_mut();
        // SAFETY: Querying read-only token of the current process.
        let ok = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut raw_token) };
        if ok == 0 || raw_token.is_null() {
            return false;
        }
        let token = TokenHandle(raw_token);
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut ret_len = 0u32;
        let size = u32::try_from(size_of::<TOKEN_ELEVATION>()).unwrap_or(4);
        // SAFETY: Valid token handle and properly sized `TOKEN_ELEVATION` struct.
        let info_ok = unsafe {
            GetTokenInformation(
                token.0,
                TokenElevation,
                (&raw mut elevation).cast::<c_void>(),
                size,
                &raw mut ret_len,
            )
        };
        info_ok != 0 && elevation.TokenIsElevated != 0
    }

    /// Enumerates all Win32 services via `OpenSCManagerW(SC_MANAGER_ENUMERATE_SERVICE)` and
    /// `EnumServicesStatusExW`, resolving each service's start type via `QueryServiceConfigW`
    /// with a caller-owned cache keyed by service name.
    pub fn enumerate_windows_services(
        start_type_cache: &mut BTreeMap<String, String>,
    ) -> Result<Vec<RawServiceInfo>, String> {
        // SAFETY: Opening local SCM read-only with `SC_MANAGER_ENUMERATE_SERVICE`.
        let raw_scm =
            unsafe { OpenSCManagerW(ptr::null(), ptr::null(), SC_MANAGER_ENUMERATE_SERVICE) };
        if raw_scm.is_null() {
            let err = unsafe { GetLastError() };
            return Err(format!("OpenSCManagerW failed ({err})"));
        }
        let scm = ScmHandle(raw_scm);

        let mut bytes_needed = 0u32;
        let mut services_returned = 0u32;
        let mut resume_handle = 0u32;

        // First call queries required buffer size.
        let _ = unsafe {
            EnumServicesStatusExW(
                scm.0,
                SC_ENUM_PROCESS_INFO,
                SERVICE_WIN32,
                SERVICE_STATE_ALL,
                ptr::null_mut(),
                0,
                &raw mut bytes_needed,
                &raw mut services_returned,
                &raw mut resume_handle,
                ptr::null(),
            )
        };
        let initial_err = unsafe { GetLastError() };
        if initial_err != ERROR_MORE_DATA || bytes_needed == 0 {
            if bytes_needed == 0 && services_returned == 0 {
                return Ok(Vec::new());
            }
            return Err(format!(
                "EnumServicesStatusExW size query failed ({initial_err})"
            ));
        }

        let mut buf_size = (bytes_needed as usize)
            .saturating_add(4096)
            .min(MAX_SCM_ENUM_BYTES);
        let mut buf = vec![0u64; buf_size.div_ceil(size_of::<u64>())];

        for _ in 0..4 {
            bytes_needed = 0;
            services_returned = 0;
            resume_handle = 0;
            let buf_bytes = u32::try_from(buf.len() * size_of::<u64>())
                .map_err(|_| "SCM buffer overflow".to_owned())?;
            // SAFETY: `buf` is 8-byte aligned and provides `buf_bytes` writable bytes.
            let ok = unsafe {
                EnumServicesStatusExW(
                    scm.0,
                    SC_ENUM_PROCESS_INFO,
                    SERVICE_WIN32,
                    SERVICE_STATE_ALL,
                    buf.as_mut_ptr().cast::<u8>(),
                    buf_bytes,
                    &raw mut bytes_needed,
                    &raw mut services_returned,
                    &raw mut resume_handle,
                    ptr::null(),
                )
            };
            if ok != 0 {
                return Ok(decode_scm_buffer(
                    scm.0,
                    &buf,
                    services_returned,
                    start_type_cache,
                ));
            }

            let err = unsafe { GetLastError() };
            if err == ERROR_MORE_DATA {
                buf_size = buf_size
                    .saturating_add(bytes_needed as usize)
                    .saturating_add(4096);
                if buf_size > MAX_SCM_ENUM_BYTES {
                    return Err("EnumServicesStatusExW buffer limit exceeded".to_owned());
                }
                buf.resize(buf_size.div_ceil(size_of::<u64>()), 0);
                continue;
            }
            return Err(format!("EnumServicesStatusExW failed ({err})"));
        }

        Err("EnumServicesStatusExW did not converge".to_owned())
    }

    fn decode_scm_buffer(
        scm: SC_HANDLE,
        buf: &[u64],
        services_returned: u32,
        start_type_cache: &mut BTreeMap<String, String>,
    ) -> Vec<RawServiceInfo> {
        let count = services_returned as usize;
        let max_count = size_of_val(buf) / size_of::<ENUM_SERVICE_STATUS_PROCESSW>();
        let safe_count = count.min(max_count);
        // SAFETY: `EnumServicesStatusExW` wrote `safe_count` initialized records into `buf`.
        let slice = unsafe {
            core::slice::from_raw_parts(
                buf.as_ptr().cast::<ENUM_SERVICE_STATUS_PROCESSW>(),
                safe_count,
            )
        };
        let buf_start = buf.as_ptr().cast::<u8>() as usize;
        let buf_end = buf_start.saturating_add(size_of_val(buf));
        let mut results = Vec::with_capacity(safe_count);
        for item in slice {
            let name = read_pwstr_in_buffer(item.lpServiceName, buf_start, buf_end);
            if name.is_empty() {
                continue;
            }
            let display_name = {
                let d = read_pwstr_in_buffer(item.lpDisplayName, buf_start, buf_end);
                if d.is_empty() { name.clone() } else { d }
            };
            let state = decode_service_state(item.ServiceStatusProcess.dwCurrentState).to_owned();
            let pid = match item.ServiceStatusProcess.dwProcessId {
                0 => None,
                p => Some(p),
            };
            let start_type = start_type_cache
                .entry(name.clone())
                .or_insert_with(|| {
                    query_service_start_type(scm, &name).unwrap_or_else(|| "Unknown".to_owned())
                })
                .clone();
            results.push(RawServiceInfo {
                name,
                display_name,
                state,
                start_type,
                pid,
            });
        }
        results
    }

    fn query_service_start_type(scm: SC_HANDLE, name: &str) -> Option<String> {
        let wide_name: Vec<u16> = name.encode_utf16().chain(core::iter::once(0)).collect();
        // SAFETY: Read-only `SERVICE_QUERY_CONFIG` handle on an open SCM handle.
        let raw_svc = unsafe { OpenServiceW(scm, wide_name.as_ptr(), SERVICE_QUERY_CONFIG) };
        if raw_svc.is_null() {
            return None;
        }
        let svc = ScmHandle(raw_svc);

        let mut needed = 0u32;
        // SAFETY: First call queries required `QUERY_SERVICE_CONFIGW` buffer size.
        let _ = unsafe { QueryServiceConfigW(svc.0, ptr::null_mut(), 0, &raw mut needed) };
        if needed == 0
            || unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER
            || (needed as usize) > MAX_SVC_CONFIG_BYTES
        {
            return None;
        }

        let mut buf = vec![0u64; (needed as usize).div_ceil(size_of::<u64>())];
        let buf_bytes = u32::try_from(buf.len() * size_of::<u64>()).ok()?;
        let mut actual = 0u32;
        // SAFETY: `buf` is 8-byte aligned and sized to `needed` bytes.
        let ok = unsafe {
            QueryServiceConfigW(
                svc.0,
                buf.as_mut_ptr().cast::<QUERY_SERVICE_CONFIGW>(),
                buf_bytes,
                &raw mut actual,
            )
        };
        if ok == 0 {
            return None;
        }
        // SAFETY: `QueryServiceConfigW` initialized `QUERY_SERVICE_CONFIGW` at start of `buf`.
        let cfg = unsafe { &*buf.as_ptr().cast::<QUERY_SERVICE_CONFIGW>() };
        Some(decode_service_start_type(cfg.dwStartType).to_owned())
    }

    /// Starts a Windows service via `StartServiceW` (requires elevated Administrator token).
    pub fn start_windows_service(service_name: &str) -> Result<(), ServiceActionError> {
        if !is_process_elevated() {
            return Err(ServiceActionError::NotElevated);
        }
        let raw_scm = unsafe { OpenSCManagerW(ptr::null(), ptr::null(), SC_MANAGER_CONNECT) };
        if raw_scm.is_null() {
            let err = unsafe { GetLastError() };
            return Err(map_scm_error(service_name, "OpenSCManagerW", err));
        }
        let scm = ScmHandle(raw_scm);
        let wide_name: Vec<u16> = service_name
            .encode_utf16()
            .chain(core::iter::once(0))
            .collect();
        // SAFETY: Opening target service with minimum `SERVICE_START` access right.
        let raw_svc = unsafe { OpenServiceW(scm.0, wide_name.as_ptr(), SERVICE_START) };
        if raw_svc.is_null() {
            let err = unsafe { GetLastError() };
            return Err(map_scm_error(service_name, "OpenServiceW", err));
        }
        let svc = ScmHandle(raw_svc);
        // SAFETY: Valid `SERVICE_START` handle, zero arguments.
        let ok = unsafe { StartServiceW(svc.0, 0, ptr::null()) };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            return Err(map_scm_error(service_name, "StartServiceW", err));
        }
        Ok(())
    }

    /// Stops a Windows service via `ControlService(SERVICE_CONTROL_STOP)` (requires Administrator).
    pub fn stop_windows_service(service_name: &str) -> Result<(), ServiceActionError> {
        if !is_process_elevated() {
            return Err(ServiceActionError::NotElevated);
        }
        let raw_scm = unsafe { OpenSCManagerW(ptr::null(), ptr::null(), SC_MANAGER_CONNECT) };
        if raw_scm.is_null() {
            let err = unsafe { GetLastError() };
            return Err(map_scm_error(service_name, "OpenSCManagerW", err));
        }
        let scm = ScmHandle(raw_scm);
        let wide_name: Vec<u16> = service_name
            .encode_utf16()
            .chain(core::iter::once(0))
            .collect();
        // SAFETY: Opening target service with minimum `SERVICE_STOP` access right.
        let raw_svc = unsafe { OpenServiceW(scm.0, wide_name.as_ptr(), SERVICE_STOP) };
        if raw_svc.is_null() {
            let err = unsafe { GetLastError() };
            return Err(map_scm_error(service_name, "OpenServiceW", err));
        }
        let svc = ScmHandle(raw_svc);
        // SAFETY: `SERVICE_STATUS` is POD output struct filled by `ControlService`.
        let mut status: SERVICE_STATUS = unsafe { core::mem::zeroed() };
        let ok = unsafe { ControlService(svc.0, SERVICE_CONTROL_STOP, &raw mut status) };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            return Err(map_scm_error(service_name, "ControlService(STOP)", err));
        }
        Ok(())
    }

    fn map_scm_error(service_name: &str, op: &str, err: u32) -> ServiceActionError {
        match err {
            ERROR_ACCESS_DENIED => ServiceActionError::AccessDenied(service_name.to_owned()),
            ERROR_SERVICE_DOES_NOT_EXIST => ServiceActionError::NotFound(service_name.to_owned()),
            ERROR_SERVICE_ALREADY_RUNNING => ServiceActionError::InvalidState {
                service: service_name.to_owned(),
                detail: "service is already running".to_owned(),
            },
            ERROR_SERVICE_NOT_ACTIVE => ServiceActionError::InvalidState {
                service: service_name.to_owned(),
                detail: "service is not running".to_owned(),
            },
            ERROR_SERVICE_CANNOT_ACCEPT_CTRL => ServiceActionError::InvalidState {
                service: service_name.to_owned(),
                detail: "service cannot accept stop control right now".to_owned(),
            },
            other => {
                ServiceActionError::Platform(format!("{op}('{service_name}') failed ({other})"))
            }
        }
    }

    fn read_pwstr_in_buffer(ptr: *mut u16, buf_start: usize, buf_end: usize) -> String {
        if ptr.is_null() {
            return String::new();
        }
        let addr = ptr as usize;
        if addr < buf_start || addr >= buf_end {
            return String::new();
        }
        let max_units = ((buf_end - addr) / size_of::<u16>()).min(1024);
        let mut units = Vec::new();
        for idx in 0..max_units {
            // SAFETY: `addr + (idx + 1) * 2 <= buf_end`, strictly inside the SCM output buffer.
            let ch = unsafe { ptr.add(idx).read_unaligned() };
            if ch == 0 {
                break;
            }
            units.push(ch);
        }
        String::from_utf16_lossy(&units).trim().to_owned()
    }
}

#[cfg(windows)]
pub use win::{
    enumerate_windows_services, is_process_elevated, start_windows_service, stop_windows_service,
};

#[cfg(not(windows))]
pub fn is_process_elevated() -> bool {
    false
}

#[cfg(not(windows))]
pub fn enumerate_windows_services(
    _start_type_cache: &mut std::collections::BTreeMap<String, String>,
) -> Result<Vec<RawServiceInfo>, String> {
    Err("Windows SCM enumeration requires Windows".to_owned())
}

#[cfg(not(windows))]
pub fn start_windows_service(_service_name: &str) -> Result<(), ServiceActionError> {
    Err(ServiceActionError::NotElevated)
}

#[cfg(not(windows))]
pub fn stop_windows_service(_service_name: &str) -> Result<(), ServiceActionError> {
    Err(ServiceActionError::NotElevated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_service_states_and_start_types() {
        assert_eq!(decode_service_state(1), "Stopped");
        assert_eq!(decode_service_state(2), "Start Pending");
        assert_eq!(decode_service_state(3), "Stop Pending");
        assert_eq!(decode_service_state(4), "Running");
        assert_eq!(decode_service_state(7), "Paused");
        assert_eq!(decode_service_start_type(2), "Auto");
        assert_eq!(decode_service_start_type(3), "Manual");
        assert_eq!(decode_service_start_type(4), "Disabled");
    }

    #[cfg(windows)]
    #[test]
    fn live_windows_scm_enumerates_services_cleanly() {
        let mut cache = std::collections::BTreeMap::new();
        let services = enumerate_windows_services(&mut cache).expect("enumerate SCM services");
        assert!(
            !services.is_empty(),
            "Windows 11 SCM must return at least one service"
        );
        assert!(
            services.iter().any(|s| s.state == "Running"),
            "at least one Windows service must be Running"
        );
        assert!(
            services
                .iter()
                .any(|s| matches!(s.start_type.as_str(), "Auto" | "Manual" | "Disabled")),
            "QueryServiceConfigW must decode concrete Windows service start types"
        );
        assert!(
            services
                .iter()
                .all(|s| !s.name.is_empty() && !s.display_name.is_empty()),
            "every service must have non-empty name and display_name"
        );
        // Verify non-elevated start/stop refuses safely without touching SCM when not elevated.
        if !is_process_elevated() {
            assert_eq!(
                start_windows_service("W32Time"),
                Err(ServiceActionError::NotElevated)
            );
            assert_eq!(
                stop_windows_service("W32Time"),
                Err(ServiceActionError::NotElevated)
            );
        }
    }
}
