//! Low-level Windows NT/WDK FFI definitions and startup hardening.
//!
//! All hand-written undocumented NT/WDK structs belong in this crate with
//! compile-time `size_of` and `offset_of` assertions for `x86_64` and `aarch64`.

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
/// when probing optional vendor libraries (`nvml.dll`, `amdadlx64.dll`, `ControlLib.dll`).
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
