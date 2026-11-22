//! Windows 11 memory, commit charge, pagefile, and English PDH cache-list FFI helpers.

/// Raw physical memory counters returned by `GlobalMemoryStatusEx`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RawMemoryStatus {
    pub total_phys_bytes: u64,
    pub avail_phys_bytes: u64,
}

/// Raw system commit and page-size counters returned by `GetPerformanceInfo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RawPerformanceInfo {
    pub commit_total_pages: u64,
    pub commit_limit_pages: u64,
    pub page_size_bytes: u64,
}

/// Aggregated pagefile usage across all active paging files returned by `EnumPageFilesW` (in pages).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RawPagefileTotals {
    pub used_pages: u64,
    pub total_pages: u64,
}

/// Instantaneous memory cache list breakdown in bytes from English Windows 11 PDH counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RawMemCacheSample {
    pub standby_core_bytes: u64,
    pub standby_normal_bytes: u64,
    pub standby_reserve_bytes: u64,
    pub modified_bytes: u64,
    pub free_and_zero_bytes: u64,
}

#[cfg(windows)]
mod win {
    use core::ffi::c_void;
    use core::mem::size_of;
    use core::ptr;
    use std::fmt;

    use windows_sys::Win32::System::Performance::{
        PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE, PDH_FMT_LARGE,
        PDH_HCOUNTER, PDH_HQUERY, PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData,
        PdhGetFormattedCounterValue, PdhOpenQueryW,
    };
    use windows_sys::Win32::System::ProcessStatus::{
        ENUM_PAGE_FILE_INFORMATION, EnumPageFilesW, GetPerformanceInfo, PERFORMANCE_INFORMATION,
    };
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    use windows_sys::core::{BOOL, PCWSTR};

    use super::{RawMemCacheSample, RawMemoryStatus, RawPagefileTotals, RawPerformanceInfo};

    const ERROR_SUCCESS_U32: u32 = 0;

    /// Queries total and available physical memory via `GlobalMemoryStatusEx`.
    pub fn query_memory_status() -> Result<RawMemoryStatus, String> {
        // SAFETY: `MEMORYSTATUSEX` is plain-old-data; `dwLength` must be set to its struct size.
        let mut status: MEMORYSTATUSEX = unsafe { core::mem::zeroed() };
        status.dwLength = u32::try_from(size_of::<MEMORYSTATUSEX>())
            .map_err(|_| "MEMORYSTATUSEX size overflow".to_owned())?;
        // SAFETY: `status` is valid and `dwLength` matches `size_of::<MEMORYSTATUSEX>()`.
        let ok = unsafe { GlobalMemoryStatusEx(&raw mut status) };
        if ok == 0 {
            return Err("GlobalMemoryStatusEx failed".to_owned());
        }
        Ok(RawMemoryStatus {
            total_phys_bytes: status.ullTotalPhys,
            avail_phys_bytes: status.ullAvailPhys,
        })
    }

    /// Queries Commit Charge, Commit Limit, and OS page size via `GetPerformanceInfo`.
    pub fn query_performance_info() -> Result<RawPerformanceInfo, String> {
        // SAFETY: `PERFORMANCE_INFORMATION` is plain-old-data with `cb` header size field.
        let mut info: PERFORMANCE_INFORMATION = unsafe { core::mem::zeroed() };
        let cb = u32::try_from(size_of::<PERFORMANCE_INFORMATION>())
            .map_err(|_| "PERFORMANCE_INFORMATION size overflow".to_owned())?;
        info.cb = cb;
        // SAFETY: `info` is valid and `cb` equals `size_of::<PERFORMANCE_INFORMATION>()`.
        let ok = unsafe { GetPerformanceInfo(&raw mut info, cb) };
        if ok == 0 {
            return Err("GetPerformanceInfo failed".to_owned());
        }
        Ok(RawPerformanceInfo {
            commit_total_pages: info.CommitTotal as u64,
            commit_limit_pages: info.CommitLimit as u64,
            page_size_bytes: info.PageSize as u64,
        })
    }

    /// Sums `TotalInUse` and `TotalSize` (in pages) across all paging files via `EnumPageFilesW`.
    pub fn query_pagefile_totals() -> Result<RawPagefileTotals, String> {
        unsafe extern "system" fn callback(
            pcontext: *mut c_void,
            ppagefileinfo: *mut ENUM_PAGE_FILE_INFORMATION,
            _lpfilename: PCWSTR,
        ) -> BOOL {
            if pcontext.is_null() || ppagefileinfo.is_null() {
                return 1;
            }
            // SAFETY: `pcontext` points to our stack-allocated `RawPagefileTotals` and
            // `ppagefileinfo` is a valid non-null pointer supplied by `EnumPageFilesW`.
            let acc = unsafe { &mut *pcontext.cast::<RawPagefileTotals>() };
            let info = unsafe { &*ppagefileinfo };
            acc.used_pages = acc.used_pages.saturating_add(info.TotalInUse as u64);
            acc.total_pages = acc.total_pages.saturating_add(info.TotalSize as u64);
            1
        }

        let mut totals = RawPagefileTotals::default();
        // SAFETY: `callback` matches `PENUM_PAGE_FILE_CALLBACKW` and `totals` outlives the call.
        let ok = unsafe { EnumPageFilesW(Some(callback), (&raw mut totals).cast::<c_void>()) };
        if ok == 0 {
            return Err("EnumPageFilesW failed".to_owned());
        }
        Ok(totals)
    }

    /// Persistent English Windows 11 PDH query for memory Standby, Modified, and Free/Zero lists.
    pub struct MemPdhQuery {
        query: PDH_HQUERY,
        standby_core: Option<PDH_HCOUNTER>,
        standby_normal: Option<PDH_HCOUNTER>,
        standby_reserve: Option<PDH_HCOUNTER>,
        modified: Option<PDH_HCOUNTER>,
        free_and_zero: Option<PDH_HCOUNTER>,
    }

    // SAFETY: `MemPdhQuery` uniquely owns its `PDH_HQUERY` and counters and is polled from
    // one sampler thread at a time.
    unsafe impl Send for MemPdhQuery {}

    impl fmt::Debug for MemPdhQuery {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("MemPdhQuery")
                .field("has_standby_core", &self.standby_core.is_some())
                .field("has_standby_normal", &self.standby_normal.is_some())
                .field("has_standby_reserve", &self.standby_reserve.is_some())
                .field("has_modified", &self.modified.is_some())
                .field("has_free_and_zero", &self.free_and_zero.is_some())
                .finish_non_exhaustive()
        }
    }

    impl MemPdhQuery {
        /// Opens one shared English `PDH_HQUERY` for the 5 memory cache list byte counters.
        pub fn open() -> Option<Self> {
            let mut query: PDH_HQUERY = ptr::null_mut();
            // SAFETY: Opening a live PDH query with null data source.
            let status = unsafe { PdhOpenQueryW(ptr::null(), 0, &raw mut query) };
            if status != ERROR_SUCCESS_U32 || query.is_null() {
                return None;
            }

            let standby_core = add_english_counter(query, "\\Memory\\Standby Cache Core Bytes");
            let standby_normal =
                add_english_counter(query, "\\Memory\\Standby Cache Normal Priority Bytes");
            let standby_reserve =
                add_english_counter(query, "\\Memory\\Standby Cache Reserve Bytes");
            let modified = add_english_counter(query, "\\Memory\\Modified Page List Bytes");
            let free_and_zero = add_english_counter(query, "\\Memory\\Free & Zero Page List Bytes");

            if standby_core.is_none()
                && standby_normal.is_none()
                && standby_reserve.is_none()
                && modified.is_none()
                && free_and_zero.is_none()
            {
                // SAFETY: Closing opened query on failure to register any counter.
                unsafe {
                    let _ = PdhCloseQuery(query);
                }
                return None;
            }

            Some(Self {
                query,
                standby_core,
                standby_normal,
                standby_reserve,
                modified,
                free_and_zero,
            })
        }

        /// Collects an instantaneous snapshot of Windows cache lists in bytes.
        pub fn sample(&mut self) -> Option<RawMemCacheSample> {
            // SAFETY: `self.query` is a valid opened `PDH_HQUERY`.
            let status = unsafe { PdhCollectQueryData(self.query) };
            if status != ERROR_SUCCESS_U32 {
                return None;
            }

            let standby_core = self.standby_core.and_then(read_scalar_u64);
            let standby_normal = self.standby_normal.and_then(read_scalar_u64);
            let standby_reserve = self.standby_reserve.and_then(read_scalar_u64);
            let modified = self.modified.and_then(read_scalar_u64);
            let free_and_zero = self.free_and_zero.and_then(read_scalar_u64);

            if standby_core.is_none()
                && standby_normal.is_none()
                && standby_reserve.is_none()
                && modified.is_none()
                && free_and_zero.is_none()
            {
                return None;
            }

            Some(RawMemCacheSample {
                standby_core_bytes: standby_core.unwrap_or(0),
                standby_normal_bytes: standby_normal.unwrap_or(0),
                standby_reserve_bytes: standby_reserve.unwrap_or(0),
                modified_bytes: modified.unwrap_or(0),
                free_and_zero_bytes: free_and_zero.unwrap_or(0),
            })
        }
    }

    impl Drop for MemPdhQuery {
        fn drop(&mut self) {
            if !self.query.is_null() {
                // SAFETY: `self.query` is uniquely owned by `self`.
                unsafe {
                    let _ = PdhCloseQuery(self.query);
                }
            }
        }
    }

    fn add_english_counter(query: PDH_HQUERY, path: &str) -> Option<PDH_HCOUNTER> {
        let wide: Vec<u16> = path.encode_utf16().chain(core::iter::once(0)).collect();
        let mut counter: PDH_HCOUNTER = ptr::null_mut();
        // SAFETY: `query` is valid and `wide` is NUL-terminated UTF-16.
        let status = unsafe { PdhAddEnglishCounterW(query, wide.as_ptr(), 0, &raw mut counter) };
        if status == ERROR_SUCCESS_U32 && !counter.is_null() {
            Some(counter)
        } else {
            None
        }
    }

    fn read_scalar_u64(counter: PDH_HCOUNTER) -> Option<u64> {
        // SAFETY: `PDH_FMT_COUNTERVALUE` is plain-old-data filled by PDH.
        let mut val: PDH_FMT_COUNTERVALUE = unsafe { core::mem::zeroed() };
        let status = unsafe {
            PdhGetFormattedCounterValue(counter, PDH_FMT_LARGE, ptr::null_mut(), &raw mut val)
        };
        if status != ERROR_SUCCESS_U32 {
            return None;
        }
        if val.CStatus != PDH_CSTATUS_VALID_DATA && val.CStatus != PDH_CSTATUS_NEW_DATA {
            return None;
        }
        // SAFETY: `PDH_FMT_LARGE` populates `largeValue: i64`.
        let raw = unsafe { val.Anonymous.largeValue };
        u64::try_from(raw).ok()
    }
}

#[cfg(not(windows))]
mod stub {
    use super::{RawMemCacheSample, RawMemoryStatus, RawPagefileTotals, RawPerformanceInfo};

    pub fn query_memory_status() -> Result<RawMemoryStatus, String> {
        Err("Windows memory telemetry is only available on Windows".to_owned())
    }

    pub fn query_performance_info() -> Result<RawPerformanceInfo, String> {
        Err("Windows performance info is only available on Windows".to_owned())
    }

    pub fn query_pagefile_totals() -> Result<RawPagefileTotals, String> {
        Err("Windows pagefile enumeration is only available on Windows".to_owned())
    }

    #[derive(Debug)]
    pub struct MemPdhQuery;

    impl MemPdhQuery {
        pub fn open() -> Option<Self> {
            None
        }

        pub fn sample(&mut self) -> Option<RawMemCacheSample> {
            None
        }
    }
}

#[cfg(not(windows))]
pub use stub::{MemPdhQuery, query_memory_status, query_pagefile_totals, query_performance_info};
#[cfg(windows)]
pub use win::{MemPdhQuery, query_memory_status, query_pagefile_totals, query_performance_info};
