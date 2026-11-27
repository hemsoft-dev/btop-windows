//! Low-level Windows NT/WDK FFI definitions, startup hardening, and waitable timers.
//!
//! All hand-written undocumented NT/WDK structs belong in this crate with
//! compile-time `size_of` and `offset_of` assertions for `x86_64`.

#![allow(non_camel_case_types, non_snake_case)]

pub mod disk;
pub mod gpu;
pub mod mem;
pub mod net;
pub mod nvml;
pub mod pdh;
pub mod power;
pub mod proc_actions;
pub mod proc_info;
pub mod services;

use core::mem::{offset_of, size_of};

pub use disk::{
    RawDiskPerformance, RawFixedDrive, enumerate_fixed_drives, query_physical_drive_performance,
};
pub use gpu::{
    GpuPdhMetrics, GpuPdhQuery, GpuPdhSample, MAX_GPUS, RawGpuAdapter,
    aggregate_gpu_engine_utilization, aggregate_gpu_memory_bytes, encode_luid,
    enumerate_hardware_gpus, filter_hardware_adapters, parse_gpu_engine_key, parse_gpu_pdh_luid,
    vendor_name_from_id,
};
pub use mem::{
    MemPdhQuery, RawMemCacheSample, RawMemoryStatus, RawPagefileTotals, RawPerformanceInfo,
    query_memory_status, query_pagefile_totals, query_performance_info,
};
pub use net::{
    RawNetEntry, RawNetInterface, enumerate_hardware_interfaces, refresh_interface_entry,
};
pub use nvml::{
    NVML_ERROR_NOT_SUPPORTED, NVML_SUCCESS, NvmlLibrary, NvmlPciInfo, RawNvmlDevice,
    decode_nvml_field, decode_nvml_pci_ids, decode_nvml_pstate, format_pci_bdf,
    milliwatts_to_watts,
};
pub use pdh::{CpuPdhQuery, CpuPdhSample};
pub use power::{RawPowerStatus, local_utc_offset_secs, query_system_power_status};
pub use proc_actions::{
    PriorityClass, PriorityUpdateOutcome, ProcActionError, graceful_close_process,
    guard_process_identity, guard_terminate_decision, query_process_priority, set_process_priority,
    terminate_process,
};
pub use proc_info::{
    ProcFieldQuery, RawProcessMetadata, SidAccountCache, SystemProcessIdInformation,
    is_ps_protection_active, query_nt_device_drive_map, query_process_command_line,
    query_process_command_line_state, query_process_image_path, query_process_metadata,
    query_process_owner, translate_nt_device_path,
};
pub use services::{
    RawServiceInfo, ServiceActionError, decode_service_start_type, decode_service_state,
    enumerate_windows_services, is_process_elevated, start_windows_service, stop_windows_service,
};

/// Matches `UNICODE_STRING` in `winternl.h` (16 bytes on 64-bit targets).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct UnicodeString {
    pub length: u16,
    pub maximum_length: u16,
    pub buffer: *const u16,
}

impl Default for UnicodeString {
    fn default() -> Self {
        Self {
            length: 0,
            maximum_length: 0,
            buffer: core::ptr::null(),
        }
    }
}

/// Matches `SYSTEM_PROCESS_INFORMATION` in `winternl.h` / `phnt` on `x86_64` Windows 11.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemProcessInformation {
    pub next_entry_offset: u32,
    pub number_of_threads: u32,
    pub working_set_private_size: i64,
    pub hard_fault_count: u32,
    pub number_of_threads_high_watermark: u32,
    pub cycle_time: u64,
    pub create_time: i64,
    pub user_time: i64,
    pub kernel_time: i64,
    pub image_name: UnicodeString,
    pub base_priority: i32,
    pub unique_process_id: usize,
    pub inherited_from_unique_process_id: usize,
    pub handle_count: u32,
    pub session_id: u32,
    pub unique_process_key: usize,
    pub peak_virtual_size: usize,
    pub virtual_size: usize,
    pub page_fault_count: u32,
    pub peak_working_set_size: usize,
    pub working_set_size: usize,
    pub quota_peak_paged_pool_usage: usize,
    pub quota_paged_pool_usage: usize,
    pub quota_peak_non_paged_pool_usage: usize,
    pub quota_non_paged_pool_usage: usize,
    pub pagefile_usage: usize,
    pub peak_pagefile_usage: usize,
    pub private_page_count: usize,
    pub read_operation_count: i64,
    pub write_operation_count: i64,
    pub other_operation_count: i64,
    pub read_transfer_count: i64,
    pub write_transfer_count: i64,
    pub other_transfer_count: i64,
}

#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(size_of::<UnicodeString>() == 16);
    assert!(size_of::<SystemProcessInformation>() == 256);
    assert!(offset_of!(SystemProcessInformation, next_entry_offset) == 0);
    assert!(offset_of!(SystemProcessInformation, number_of_threads) == 4);
    assert!(offset_of!(SystemProcessInformation, working_set_private_size) == 8);
    assert!(offset_of!(SystemProcessInformation, create_time) == 32);
    assert!(offset_of!(SystemProcessInformation, user_time) == 40);
    assert!(offset_of!(SystemProcessInformation, kernel_time) == 48);
    assert!(offset_of!(SystemProcessInformation, image_name) == 56);
    assert!(offset_of!(SystemProcessInformation, base_priority) == 72);
    assert!(offset_of!(SystemProcessInformation, unique_process_id) == 80);
    assert!(offset_of!(SystemProcessInformation, inherited_from_unique_process_id) == 88);
    assert!(offset_of!(SystemProcessInformation, working_set_size) == 144);
    assert!(offset_of!(SystemProcessInformation, private_page_count) == 200);
    assert!(offset_of!(SystemProcessInformation, read_transfer_count) == 232);
    assert!(offset_of!(SystemProcessInformation, write_transfer_count) == 240);
};

/// Matches `SYSTEM_THREAD_INFORMATION` in `winternl.h` / `phnt` on `x86_64` Windows 11.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemThreadInformation {
    pub kernel_time: i64,
    pub user_time: i64,
    pub create_time: i64,
    pub wait_time: u32,
    pub reserved0: u32,
    pub start_address: usize,
    pub client_id_unique_process: usize,
    pub client_id_unique_thread: usize,
    pub priority: i32,
    pub base_priority: i32,
    pub context_switches: u32,
    pub thread_state: u32,
    pub wait_reason: u32,
    pub reserved1: u32,
}

#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(size_of::<SystemThreadInformation>() == 80);
    assert!(offset_of!(SystemThreadInformation, thread_state) == 68);
    assert!(offset_of!(SystemThreadInformation, wait_reason) == 72);
};

/// Safe, owned process row decoded from a raw `SYSTEM_PROCESS_INFORMATION` buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawProcessInfo {
    pub pid: u32,
    pub parent_pid: u32,
    pub create_time_100ns: u64,
    pub kernel_time_100ns: u64,
    pub user_time_100ns: u64,
    pub threads: u32,
    /// Number of threads in Ready (1), Running (2), or `DeferredReady` (7) state (0 for Idle PID 0).
    pub runnable_threads: u32,
    pub name: String,
    pub working_set_private_bytes: u64,
    pub io_read_bytes: u64,
    pub io_write_bytes: u64,
}

/// Safely walks a `NextEntryOffset`-linked buffer of `SYSTEM_PROCESS_INFORMATION` records.
///
/// Rejects truncated headers, backward/cyclic offsets, and out-of-bounds `UNICODE_STRING`
/// image-name pointers without panicking or reading outside `raw`.
pub fn parse_process_information_buffer(raw: &[u8]) -> Result<Vec<RawProcessInfo>, String> {
    let entry_size = size_of::<SystemProcessInformation>();
    if raw.len() < entry_size {
        return Err(format!(
            "process information buffer truncated: {} bytes < {entry_size}",
            raw.len()
        ));
    }

    let buf_start = raw.as_ptr() as usize;
    let buf_end = buf_start
        .checked_add(raw.len())
        .ok_or_else(|| "process buffer address overflow".to_owned())?;

    let mut results = Vec::new();
    let mut offset = 0usize;
    let mut visited = 0usize;
    let max_entries = raw.len() / entry_size + 1;

    loop {
        if visited > max_entries {
            return Err("cyclic NextEntryOffset detected in process buffer".to_owned());
        }
        visited += 1;

        let Some(end) = offset.checked_add(entry_size) else {
            return Err("process entry offset overflow".to_owned());
        };
        if end > raw.len() {
            return Err(format!(
                "truncated SYSTEM_PROCESS_INFORMATION entry at offset {offset} (len {})",
                raw.len()
            ));
        }

        // SAFETY: `offset + entry_size <= raw.len()`, and `read_unaligned` handles any alignment.
        let entry = unsafe {
            raw.as_ptr()
                .add(offset)
                .cast::<SystemProcessInformation>()
                .read_unaligned()
        };

        let pid = u32::try_from(entry.unique_process_id).unwrap_or(u32::MAX);
        let parent_pid = u32::try_from(entry.inherited_from_unique_process_id).unwrap_or(0);
        let name = decode_bounded_image_name(entry.image_name, pid, buf_start, buf_end)?;

        let entry_end = if entry.next_entry_offset == 0 {
            raw.len()
        } else {
            let step = usize::try_from(entry.next_entry_offset)
                .map_err(|_| "NextEntryOffset conversion overflow".to_owned())?;
            if step < entry_size {
                return Err(format!(
                    "invalid NextEntryOffset {step} smaller than entry size {entry_size}"
                ));
            }
            let Some(next) = offset.checked_add(step) else {
                return Err("NextEntryOffset addition overflow".to_owned());
            };
            if next >= raw.len() {
                return Err(format!(
                    "NextEntryOffset points outside buffer: {next} >= {}",
                    raw.len()
                ));
            }
            next
        };

        let runnable_threads = if pid == 0 {
            0
        } else {
            count_runnable_threads(raw, end, entry_end, entry.number_of_threads)
        };

        results.push(RawProcessInfo {
            pid,
            parent_pid,
            create_time_100ns: u64::try_from(entry.create_time).unwrap_or(0),
            kernel_time_100ns: u64::try_from(entry.kernel_time).unwrap_or(0),
            user_time_100ns: u64::try_from(entry.user_time).unwrap_or(0),
            threads: entry.number_of_threads,
            runnable_threads,
            name,
            working_set_private_bytes: u64::try_from(entry.working_set_private_size).unwrap_or(0),
            io_read_bytes: u64::try_from(entry.read_transfer_count).unwrap_or(0),
            io_write_bytes: u64::try_from(entry.write_transfer_count).unwrap_or(0),
        });

        if entry.next_entry_offset == 0 {
            break;
        }
        offset = entry_end;
    }

    Ok(results)
}

fn count_runnable_threads(
    raw: &[u8],
    threads_start: usize,
    entry_end: usize,
    declared_threads: u32,
) -> u32 {
    let thread_size = size_of::<SystemThreadInformation>();
    if entry_end <= threads_start {
        return 0;
    }
    let available = (entry_end - threads_start) / thread_size;
    let count = usize::try_from(declared_threads)
        .unwrap_or(0)
        .min(available);
    let mut runnable = 0u32;
    for idx in 0..count {
        let pos = threads_start + idx * thread_size;
        // SAFETY: `pos + thread_size <= entry_end <= raw.len()`.
        let thread = unsafe {
            raw.as_ptr()
                .add(pos)
                .cast::<SystemThreadInformation>()
                .read_unaligned()
        };
        // KTHREAD_STATE: 1 = Ready, 2 = Running, 7 = DeferredReady.
        if matches!(thread.thread_state, 1 | 2 | 7) {
            runnable = runnable.saturating_add(1);
        }
    }
    runnable
}

fn decode_bounded_image_name(
    unicode: UnicodeString,
    pid: u32,
    buf_start: usize,
    buf_end: usize,
) -> Result<String, String> {
    if unicode.length == 0 || unicode.buffer.is_null() {
        return Ok(default_kernel_process_name(pid));
    }
    if !unicode.length.is_multiple_of(2) {
        return Err(format!(
            "odd UNICODE_STRING byte length {} for PID {pid}",
            unicode.length
        ));
    }
    let ptr_addr = unicode.buffer as usize;
    let byte_len = usize::from(unicode.length);
    let Some(ptr_end) = ptr_addr.checked_add(byte_len) else {
        return Err(format!("UNICODE_STRING address overflow for PID {pid}"));
    };
    if ptr_addr < buf_start || ptr_end > buf_end {
        return Err(format!(
            "UNICODE_STRING pointer out of buffer bounds for PID {pid}"
        ));
    }

    let unit_count = byte_len / 2;
    let mut units = Vec::with_capacity(unit_count);
    for idx in 0..unit_count {
        // SAFETY: `[ptr_addr, ptr_end)` is verified strictly inside `[buf_start, buf_end)`.
        let code_unit = unsafe { unicode.buffer.add(idx).read_unaligned() };
        units.push(code_unit);
    }
    let decoded = String::from_utf16_lossy(&units)
        .trim_matches('\0')
        .trim()
        .to_owned();
    if decoded.is_empty() {
        Ok(default_kernel_process_name(pid))
    } else {
        Ok(decoded)
    }
}

fn default_kernel_process_name(pid: u32) -> String {
    match pid {
        0 => "System Idle Process".to_owned(),
        4 => "System".to_owned(),
        other => format!("pid:{other}"),
    }
}

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
        CloseHandle, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, FILETIME, GetLastError, HANDLE,
        STATUS_INFO_LENGTH_MISMATCH, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX2,
    };
    use windows_sys::Win32::System::Registry::{
        HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
    };
    use windows_sys::Win32::System::SystemInformation::{
        GetLogicalProcessorInformationEx, GetSystemInfo, GetTickCount64, GlobalMemoryStatusEx,
        MEMORYSTATUSEX, PROCESSOR_RELATIONSHIP, RelationProcessorCore, SYSTEM_INFO,
        SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
    };
    use windows_sys::Win32::System::Threading::{
        CreateEventW, CreateWaitableTimerW, GetCurrentProcess, GetProcessTimes, INFINITE,
        ResetEvent, SetEvent, SetWaitableTimer, WaitForMultipleObjects,
    };

    use super::{
        RawProcessInfo, SystemProcessorPerformanceInformation, parse_process_information_buffer,
    };

    const SYSTEM_PROCESS_INFORMATION_CLASS: u32 = 5;
    const SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION_CLASS: u32 = 8;
    const INITIAL_PROC_BUF_BYTES: usize = 256 * 1024;
    const MAX_PROC_BUF_BYTES: usize = 128 * 1024 * 1024;

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtQuerySystemInformation(
            system_information_class: u32,
            system_information: *mut c_void,
            system_information_length: u32,
            return_length: *mut u32,
        ) -> i32;
    }

    /// Queries all running Windows processes via a single `NtQuerySystemInformation(5)` call,
    /// reusing `scratch` across ticks and retrying `STATUS_INFO_LENGTH_MISMATCH` with 25% headroom.
    pub fn query_process_information(scratch: &mut Vec<u8>) -> Result<Vec<RawProcessInfo>, String> {
        if scratch.len() < INITIAL_PROC_BUF_BYTES {
            scratch.resize(INITIAL_PROC_BUF_BYTES, 0);
        }

        for _ in 0..8 {
            let buf_len = u32::try_from(scratch.len())
                .map_err(|_| "process query buffer exceeds u32 length".to_owned())?;
            let mut returned_len = 0u32;

            // SAFETY: `scratch` provides `buf_len` writable bytes for NT to fill.
            let status = unsafe {
                NtQuerySystemInformation(
                    SYSTEM_PROCESS_INFORMATION_CLASS,
                    scratch.as_mut_ptr().cast::<c_void>(),
                    buf_len,
                    &raw mut returned_len,
                )
            };

            if status >= 0 {
                let valid_len = (returned_len as usize).min(scratch.len());
                return parse_process_information_buffer(&scratch[..valid_len]);
            }

            if status == STATUS_INFO_LENGTH_MISMATCH {
                let needed = (returned_len as usize).max(scratch.len());
                let grown = needed
                    .saturating_add(needed / 4)
                    .max(scratch.len().saturating_add(64 * 1024));
                if grown > MAX_PROC_BUF_BYTES {
                    return Err(format!(
                        "process query buffer limit exceeded ({grown} bytes > {MAX_PROC_BUF_BYTES})"
                    ));
                }
                scratch.resize(grown, 0);
                continue;
            }

            return Err(format!(
                "NtQuerySystemInformation(SystemProcessInformation) failed with NTSTATUS 0x{status:08X}"
            ));
        }

        Err("NtQuerySystemInformation(SystemProcessInformation) did not converge".to_owned())
    }

    /// Returns total physical memory in bytes via `GlobalMemoryStatusEx`.
    pub fn total_physical_memory_bytes() -> Option<u64> {
        // SAFETY: `MEMORYSTATUSEX` is POD; `dwLength` must be initialized before calling Win32.
        let mut status: MEMORYSTATUSEX = unsafe { core::mem::zeroed() };
        status.dwLength = u32::try_from(size_of::<MEMORYSTATUSEX>()).ok()?;
        let ok = unsafe { GlobalMemoryStatusEx(&raw mut status) };
        if ok == 0 || status.ullTotalPhys == 0 {
            None
        } else {
            Some(status.ullTotalPhys)
        }
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

    /// Returns total kernel + user CPU time consumed by the current process in 100-nanosecond ticks.
    pub fn query_self_cpu_time_100ns() -> Result<u64, String> {
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
        // SAFETY: `GetCurrentProcess` returns a valid pseudo-handle and all FILETIME outputs are writable.
        let ok = unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &raw mut creation,
                &raw mut exit,
                &raw mut kernel,
                &raw mut user,
            )
        };
        if ok == 0 {
            return Err("GetProcessTimes(GetCurrentProcess) failed".to_owned());
        }
        let to_u64 =
            |ft: FILETIME| (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime);
        Ok(to_u64(kernel).saturating_add(to_u64(user)))
    }

    /// Returns the current process private working set in bytes via Windows 11
    /// `GetProcessMemoryInfo(PROCESS_MEMORY_COUNTERS_EX2::PrivateWorkingSetSize)`,
    /// falling back to `NtQuerySystemInformation(SystemProcessInformation)` on older builds.
    pub fn query_self_private_working_set_bytes() -> Result<u64, String> {
        // SAFETY: `PROCESS_MEMORY_COUNTERS_EX2` is plain-old-data with a `cb` size header.
        let mut counters: PROCESS_MEMORY_COUNTERS_EX2 = unsafe { core::mem::zeroed() };
        let cb = u32::try_from(size_of::<PROCESS_MEMORY_COUNTERS_EX2>())
            .map_err(|_| "PROCESS_MEMORY_COUNTERS_EX2 size overflow".to_owned())?;
        counters.cb = cb;
        // SAFETY: `GetCurrentProcess` returns a valid pseudo-handle and `counters` is writable for `cb` bytes.
        let ok = unsafe {
            GetProcessMemoryInfo(
                GetCurrentProcess(),
                (&raw mut counters).cast::<PROCESS_MEMORY_COUNTERS>(),
                cb,
            )
        };
        if ok != 0 {
            return Ok(counters.PrivateWorkingSetSize as u64);
        }
        let pid = std::process::id();
        let mut scratch = Vec::new();
        query_process_information(&mut scratch)?
            .into_iter()
            .find(|row| row.pid == pid)
            .map(|row| row.working_set_private_bytes)
            .ok_or_else(|| "current process missing from SystemProcessInformation".to_owned())
    }

    /// Returns system uptime in whole seconds from `GetTickCount64`.
    pub fn system_uptime_secs() -> u64 {
        // SAFETY: `GetTickCount64` takes no arguments and cannot fail.
        unsafe { GetTickCount64() / 1000 }
    }

    /// Reads the CPU nominal base frequency in MHz from Registry `~MHz`.
    pub fn read_cpu_nominal_mhz() -> Option<u32> {
        let subkey: Vec<u16> = "HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0\0"
            .encode_utf16()
            .collect();
        let value: Vec<u16> = "~MHz\0".encode_utf16().collect();
        let mut mhz = 0u32;
        let mut size = u32::try_from(size_of::<u32>()).ok()?;
        // SAFETY: Reading a 4-byte REG_DWORD into `mhz`.
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                subkey.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_DWORD,
                ptr::null_mut(),
                (&raw mut mhz).cast::<c_void>(),
                &raw mut size,
            )
        };
        if status == ERROR_SUCCESS && mhz > 0 {
            Some(mhz)
        } else {
            None
        }
    }

    /// Queries per-logical-processor `EfficiencyClass` via
    /// `GetLogicalProcessorInformationEx(RelationProcessorCore)` for single-group Windows 11 CPUs (`<= 64` LPs).
    pub fn query_core_efficiency_classes() -> Vec<u8> {
        let mut required_bytes = 0u32;
        // SAFETY: Querying required buffer length for `RelationProcessorCore`.
        let first_ok = unsafe {
            GetLogicalProcessorInformationEx(
                RelationProcessorCore,
                ptr::null_mut(),
                &raw mut required_bytes,
            )
        };
        if first_ok != 0 || required_bytes == 0 {
            return Vec::new();
        }
        // SAFETY: Reading thread-local Win32 error code immediately after failed call.
        if unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER {
            return Vec::new();
        }

        let u64_len = (required_bytes as usize).div_ceil(size_of::<u64>());
        let mut raw = vec![0u64; u64_len];
        let mut actual_bytes =
            u32::try_from(raw.len() * size_of::<u64>()).unwrap_or(required_bytes);
        // SAFETY: `raw` is 8-byte aligned and sized for `actual_bytes`.
        let ok = unsafe {
            GetLogicalProcessorInformationEx(
                RelationProcessorCore,
                raw.as_mut_ptr()
                    .cast::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>(),
                &raw mut actual_bytes,
            )
        };
        if ok == 0 {
            return Vec::new();
        }

        let total_len = (actual_bytes as usize).min(raw.len() * size_of::<u64>());
        let base_ptr = raw.as_ptr().cast::<u8>();
        // `SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX` records are variable-sized:
        // an 8-byte header (`Relationship: i32`, `Size: u32`) followed by the active union payload.
        // Single-group `RelationProcessorCore` records are 48 bytes on x64 (smaller than the 80-byte union).
        let header_size = size_of::<i32>() + size_of::<u32>();
        let min_proc_record_size = header_size + size_of::<PROCESSOR_RELATIONSHIP>();
        let mut classes = Vec::new();
        let mut offset = 0usize;

        while offset.saturating_add(header_size) <= total_len {
            // SAFETY: `offset + 8 <= total_len`; unaligned reads handle any record stride.
            #[allow(clippy::cast_ptr_alignment)]
            let relationship = unsafe { base_ptr.add(offset).cast::<i32>().read_unaligned() };
            #[allow(clippy::cast_ptr_alignment)]
            let rec_size =
                unsafe { base_ptr.add(offset + 4).cast::<u32>().read_unaligned() } as usize;
            if rec_size < header_size || offset.saturating_add(rec_size) > total_len {
                break;
            }
            if relationship == RelationProcessorCore && rec_size >= min_proc_record_size {
                // SAFETY: `offset + header_size + size_of::<PROCESSOR_RELATIONSHIP>() <= offset + rec_size <= total_len`.
                #[allow(clippy::cast_ptr_alignment)]
                let proc_rel = unsafe {
                    base_ptr
                        .add(offset + header_size)
                        .cast::<PROCESSOR_RELATIONSHIP>()
                        .read_unaligned()
                };
                let eff_class = proc_rel.EfficiencyClass;
                if proc_rel.GroupCount > 0 {
                    let group = proc_rel.GroupMask[0];
                    if group.Group == 0 {
                        let mask = group.Mask;
                        for lp in 0..usize::BITS as usize {
                            if (mask & (1usize << lp)) != 0 {
                                if classes.len() <= lp {
                                    classes.resize(lp + 1, 0);
                                }
                                classes[lp] = eff_class;
                            }
                        }
                    }
                }
            }
            offset = offset.saturating_add(rec_size);
        }

        classes
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

    static HEADLESS_STOP_SLOT: std::sync::Mutex<
        Option<(Arc<std::sync::atomic::AtomicBool>, TimerWaker)>,
    > = std::sync::Mutex::new(None);
    static HEADLESS_CTRL_INSTALLED: std::sync::Once = std::sync::Once::new();

    unsafe extern "system" fn headless_ctrl_handler(ctrl_type: u32) -> i32 {
        use windows_sys::Win32::System::Console::{
            CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT,
            CTRL_SHUTDOWN_EVENT,
        };
        let handled_gracefully = matches!(ctrl_type, CTRL_C_EVENT | CTRL_BREAK_EVENT);
        if handled_gracefully
            || matches!(
                ctrl_type,
                CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT
            )
        {
            if let Ok(guard) = HEADLESS_STOP_SLOT.lock()
                && let Some((ref flag, ref waker)) = *guard
            {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                waker.wake();
            }
            i32::from(handled_gracefully)
        } else {
            0
        }
    }

    /// Installs a `Ctrl+C` / `Ctrl+Break` handler for headless streaming/summary mode that sets
    /// the returned flag and immediately wakes `waker` so the caller can exit cleanly.
    pub fn install_headless_ctrl_c_handler(
        waker: TimerWaker,
    ) -> Arc<std::sync::atomic::AtomicBool> {
        let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        if let Ok(mut guard) = HEADLESS_STOP_SLOT.lock() {
            *guard = Some((Arc::clone(&flag), waker));
        }
        HEADLESS_CTRL_INSTALLED.call_once(|| {
            // SAFETY: `headless_ctrl_handler` matches Win32 `PHANDLER_ROUTINE`.
            unsafe {
                let _ = windows_sys::Win32::System::Console::SetConsoleCtrlHandler(
                    Some(headless_ctrl_handler),
                    1,
                );
            }
        });
        flag
    }
}

#[cfg(not(windows))]
mod stub {
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Duration;

    use super::{RawProcessInfo, SystemProcessorPerformanceInformation};

    pub fn query_process_information(
        _scratch: &mut Vec<u8>,
    ) -> Result<Vec<RawProcessInfo>, String> {
        Err("Windows process telemetry is only available on Windows".to_owned())
    }

    pub fn total_physical_memory_bytes() -> Option<u64> {
        None
    }

    pub fn query_processor_performance()
    -> Result<Vec<SystemProcessorPerformanceInformation>, String> {
        Err("Windows CPU telemetry is only available on Windows".to_owned())
    }

    pub fn read_cpu_model_name() -> Option<String> {
        None
    }

    pub fn query_self_cpu_time_100ns() -> Result<u64, String> {
        Err("Windows process timing is only available on Windows".to_owned())
    }

    pub fn query_self_private_working_set_bytes() -> Result<u64, String> {
        Err("Windows process memory telemetry is only available on Windows".to_owned())
    }

    pub fn system_uptime_secs() -> u64 {
        0
    }

    pub fn read_cpu_nominal_mhz() -> Option<u32> {
        None
    }

    pub fn query_core_efficiency_classes() -> Vec<u8> {
        Vec::new()
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

    /// Installs a `SIGINT` / `SIGTERM` handler for headless streaming/summary mode on Unix that
    /// sets the returned flag and immediately wakes `waker` via an async-signal-safe self-pipe.
    #[cfg(unix)]
    pub fn install_headless_ctrl_c_handler(
        waker: TimerWaker,
    ) -> Arc<std::sync::atomic::AtomicBool> {
        use std::sync::Once;
        use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

        static SIGNAL_PIPE_WR: AtomicI32 = AtomicI32::new(-1);
        static STOP_SLOT: Mutex<Option<(Arc<AtomicBool>, TimerWaker)>> = Mutex::new(None);
        static INSTALLED: Once = Once::new();

        unsafe extern "C" fn handle_signal(_sig: libc::c_int) {
            let fd = SIGNAL_PIPE_WR.load(Ordering::Acquire);
            if fd >= 0 {
                let byte = 1u8;
                // SAFETY: `write` on a non-blocking pipe fd is async-signal-safe per POSIX.
                unsafe {
                    let _ = libc::write(fd, (&raw const byte).cast::<libc::c_void>(), 1);
                }
            }
        }

        let flag = Arc::new(AtomicBool::new(false));
        match STOP_SLOT.lock() {
            Ok(mut guard) => *guard = Some((Arc::clone(&flag), waker)),
            Err(poisoned) => *poisoned.into_inner() = Some((Arc::clone(&flag), waker)),
        }

        INSTALLED.call_once(|| {
            let mut fds = [-1 as libc::c_int; 2];
            // SAFETY: Creating a local self-pipe and configuring non-blocking write + signal handlers.
            let pipe_ok = unsafe { libc::pipe(fds.as_mut_ptr()) == 0 };
            if !pipe_ok {
                return;
            }
            let (rd, wr) = (fds[0], fds[1]);
            // SAFETY: Setting `O_NONBLOCK` on write end so signal handler can never block.
            unsafe {
                let flags = libc::fcntl(wr, libc::F_GETFL);
                if flags >= 0 {
                    let _ = libc::fcntl(wr, libc::F_SETFL, flags | libc::O_NONBLOCK);
                }
                let _ = libc::fcntl(rd, libc::F_SETFD, libc::FD_CLOEXEC);
                let _ = libc::fcntl(wr, libc::F_SETFD, libc::FD_CLOEXEC);
            }
            SIGNAL_PIPE_WR.store(wr, Ordering::Release);

            let _ = std::thread::Builder::new()
                .name("btop-sigwait".to_owned())
                .spawn(move || {
                    let mut buf = [0u8; 16];
                    loop {
                        // SAFETY: Blocking read on dedicated pipe read end.
                        let n = unsafe {
                            libc::read(rd, buf.as_mut_ptr().cast::<libc::c_void>(), buf.len())
                        };
                        if n > 0 {
                            let guard = match STOP_SLOT.lock() {
                                Ok(g) => g,
                                Err(p) => p.into_inner(),
                            };
                            if let Some((ref stop_flag, ref waker)) = *guard {
                                stop_flag.store(true, Ordering::SeqCst);
                                waker.wake();
                            }
                        } else if n == 0 {
                            break;
                        }
                    }
                });

            for sig in [libc::SIGINT, libc::SIGTERM] {
                // SAFETY: Installing async-signal-safe `handle_signal` for `SIGINT` / `SIGTERM`.
                unsafe {
                    let mut sa: libc::sigaction = core::mem::zeroed();
                    sa.sa_sigaction = handle_signal as *const () as libc::sighandler_t;
                    sa.sa_flags = libc::SA_RESTART;
                    libc::sigemptyset(&raw mut sa.sa_mask);
                    let _ = libc::sigaction(sig, &raw const sa, core::ptr::null_mut());
                }
            }
        });

        flag
    }

    /// Portable no-op Ctrl+C handler stub for non-Windows, non-Unix targets.
    #[cfg(not(unix))]
    pub fn install_headless_ctrl_c_handler(
        _waker: TimerWaker,
    ) -> Arc<std::sync::atomic::AtomicBool> {
        Arc::new(std::sync::atomic::AtomicBool::new(false))
    }
}

#[cfg(not(windows))]
pub use stub::{
    TimerWake, TimerWaker, WaitableWakeTimer, install_headless_ctrl_c_handler,
    query_core_efficiency_classes, query_process_information, query_processor_performance,
    query_self_cpu_time_100ns, query_self_private_working_set_bytes, read_cpu_model_name,
    read_cpu_nominal_mhz, system_uptime_secs, total_physical_memory_bytes,
};
#[cfg(windows)]
pub use win::{
    TimerWake, TimerWaker, WaitableWakeTimer, install_headless_ctrl_c_handler,
    query_core_efficiency_classes, query_process_information, query_processor_performance,
    query_self_cpu_time_100ns, query_self_private_working_set_bytes, read_cpu_model_name,
    read_cpu_nominal_mhz, system_uptime_secs, total_physical_memory_bytes,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_rejects_truncated_and_corrupted_process_buffers() {
        // Too small to hold even one header.
        let tiny = vec![0u8; 64];
        assert!(parse_process_information_buffer(&tiny).is_err());

        // Header with NextEntryOffset pointing past buffer end.
        let mut bad_next = vec![0u8; size_of::<SystemProcessInformation>()];
        let mut header = SystemProcessInformation {
            next_entry_offset: 512,
            unique_process_id: 42,
            ..Default::default()
        };
        unsafe {
            core::ptr::copy_nonoverlapping(
                (&raw const header).cast::<u8>(),
                bad_next.as_mut_ptr(),
                size_of::<SystemProcessInformation>(),
            );
        }
        assert!(parse_process_information_buffer(&bad_next).is_err());

        // Header with NextEntryOffset smaller than entry size (cycle / overlap risk).
        header.next_entry_offset = 16;
        unsafe {
            core::ptr::copy_nonoverlapping(
                (&raw const header).cast::<u8>(),
                bad_next.as_mut_ptr(),
                size_of::<SystemProcessInformation>(),
            );
        }
        assert!(parse_process_information_buffer(&bad_next).is_err());
    }

    #[test]
    fn parser_decodes_valid_chain_and_kernel_pid_names() {
        let entry_size = size_of::<SystemProcessInformation>();
        let name_utf16: Vec<u16> = "notepad.exe".encode_utf16().collect();
        let name_bytes = name_utf16.len() * 2;
        let stride = entry_size + name_bytes;
        let mut buf = vec![0u8; stride + entry_size];

        // Entry 0: PID 0 with empty image name -> "System Idle Process"
        let e0 = SystemProcessInformation {
            next_entry_offset: u32::try_from(stride).expect("stride fits u32"),
            number_of_threads: 16,
            unique_process_id: 0,
            ..Default::default()
        };
        // Entry 1: PID 1234 with image_name pointing into trailing bytes of Entry 0's slot.
        #[allow(clippy::cast_ptr_alignment)]
        let name_ptr = unsafe { buf.as_ptr().add(entry_size).cast::<u16>() };
        let e1 = SystemProcessInformation {
            next_entry_offset: 0,
            number_of_threads: 4,
            working_set_private_size: 4_194_304,
            create_time: 133_801_632_000_000_000,
            user_time: 200,
            kernel_time: 300,
            image_name: UnicodeString {
                length: u16::try_from(name_bytes).expect("len"),
                maximum_length: u16::try_from(name_bytes).expect("len"),
                buffer: name_ptr,
            },
            unique_process_id: 1234,
            inherited_from_unique_process_id: 4,
            read_transfer_count: 1024,
            write_transfer_count: 512,
            ..Default::default()
        };

        unsafe {
            core::ptr::copy_nonoverlapping(
                (&raw const e0).cast::<u8>(),
                buf.as_mut_ptr(),
                entry_size,
            );
            core::ptr::copy_nonoverlapping(
                name_utf16.as_ptr().cast::<u8>(),
                buf.as_mut_ptr().add(entry_size),
                name_bytes,
            );
            core::ptr::copy_nonoverlapping(
                (&raw const e1).cast::<u8>(),
                buf.as_mut_ptr().add(stride),
                entry_size,
            );
        }

        let parsed = parse_process_information_buffer(&buf).expect("parse valid chain");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].pid, 0);
        assert_eq!(parsed[0].name, "System Idle Process");
        assert_eq!(parsed[1].pid, 1234);
        assert_eq!(parsed[1].parent_pid, 4);
        assert_eq!(parsed[1].name, "notepad.exe");
        assert_eq!(parsed[1].working_set_private_bytes, 4_194_304);
        assert_eq!(parsed[1].io_read_bytes, 1024);
        assert_eq!(parsed[1].io_write_bytes, 512);
    }
}
