//! Persistent English Windows 11 PDH query helper for live CPU frequency
//! (`% Processor Performance`) and Task Manager utility (`% Processor Utility`).

/// Sample returned by [`CpuPdhQuery::sample`].
#[derive(Debug, Clone, Default)]
pub struct CpuPdhSample {
    /// Per-logical-processor `% Processor Performance` readings (excluding `_Total`).
    pub per_core_perf_pct: Vec<f64>,
    /// System-wide `\Processor Information(_Total)\% Processor Utility` reading.
    pub total_utility_pct: Option<f64>,
}

#[cfg(windows)]
mod win {
    use core::mem::size_of;
    use core::ptr;
    use std::fmt;

    use windows_sys::Win32::System::Performance::{
        PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE,
        PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA,
        PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
        PdhGetFormattedCounterValue, PdhOpenQueryW,
    };

    use super::CpuPdhSample;

    const ERROR_SUCCESS_U32: u32 = 0;

    /// Single shared `PDH_HQUERY` collecting `% Processor Performance` and `% Processor Utility`.
    ///
    /// Fails gracefully (returning `None`) if PDH counters are unavailable on the host.
    pub struct CpuPdhQuery {
        query: PDH_HQUERY,
        perf_counter: Option<PDH_HCOUNTER>,
        utility_counter: Option<PDH_HCOUNTER>,
        primed: bool,
    }

    // SAFETY: `CpuPdhQuery` owns its PDH query and counter handles exclusively and is
    // accessed from one sampler thread at a time.
    unsafe impl Send for CpuPdhQuery {}

    impl fmt::Debug for CpuPdhQuery {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("CpuPdhQuery")
                .field("has_perf", &self.perf_counter.is_some())
                .field("has_utility", &self.utility_counter.is_some())
                .field("primed", &self.primed)
                .finish_non_exhaustive()
        }
    }

    impl CpuPdhQuery {
        /// Opens the shared English PDH query and primes the first baseline collection.
        pub fn open() -> Option<Self> {
            let mut query: PDH_HQUERY = ptr::null_mut();
            // SAFETY: Opening a fresh live PDH query with null data source and user data.
            let open_status = unsafe { PdhOpenQueryW(ptr::null(), 0, &raw mut query) };
            if open_status != ERROR_SUCCESS_U32 || query.is_null() {
                return None;
            }

            let perf_counter =
                add_english_counter(query, "\\Processor Information(*)\\% Processor Performance");
            let utility_counter = add_english_counter(
                query,
                "\\Processor Information(_Total)\\% Processor Utility",
            );

            if perf_counter.is_none() && utility_counter.is_none() {
                // SAFETY: Closing the opened query handle on early failure.
                unsafe {
                    let _ = PdhCloseQuery(query);
                }
                return None;
            }

            // SAFETY: Priming rate counters with an initial collection pass.
            let primed = unsafe { PdhCollectQueryData(query) == ERROR_SUCCESS_U32 };
            Some(Self {
                query,
                perf_counter,
                utility_counter,
                primed,
            })
        }

        /// Collects one tick from the shared PDH query. Returns `None` if PDH fails or is still warming up.
        pub fn sample(&mut self) -> Option<CpuPdhSample> {
            // SAFETY: `self.query` is a valid opened `PDH_HQUERY`.
            let collect_status = unsafe { PdhCollectQueryData(self.query) };
            if collect_status != ERROR_SUCCESS_U32 {
                return None;
            }
            if !self.primed {
                self.primed = true;
                return None;
            }

            let per_core_perf_pct = self.perf_counter.map(read_perf_array).unwrap_or_default();
            let total_utility_pct = self.utility_counter.and_then(read_scalar_double);

            Some(CpuPdhSample {
                per_core_perf_pct,
                total_utility_pct,
            })
        }
    }

    impl Drop for CpuPdhQuery {
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

    fn read_scalar_double(counter: PDH_HCOUNTER) -> Option<f64> {
        // SAFETY: `PDH_FMT_COUNTERVALUE` is plain-old-data filled by PDH.
        let mut val: PDH_FMT_COUNTERVALUE = unsafe { core::mem::zeroed() };
        let status = unsafe {
            PdhGetFormattedCounterValue(counter, PDH_FMT_DOUBLE, ptr::null_mut(), &raw mut val)
        };
        if status != ERROR_SUCCESS_U32 {
            return None;
        }
        if val.CStatus != PDH_CSTATUS_VALID_DATA && val.CStatus != PDH_CSTATUS_NEW_DATA {
            return None;
        }
        // SAFETY: `PDH_FMT_DOUBLE` populates the `doubleValue` union arm.
        let v = unsafe { val.Anonymous.doubleValue };
        if v.is_finite() { Some(v) } else { None }
    }

    fn read_perf_array(counter: PDH_HCOUNTER) -> Vec<f64> {
        let mut buf_size = 0u32;
        let mut item_count = 0u32;
        // SAFETY: Querying required byte size for formatted counter array.
        let first = unsafe {
            PdhGetFormattedCounterArrayW(
                counter,
                PDH_FMT_DOUBLE,
                &raw mut buf_size,
                &raw mut item_count,
                ptr::null_mut(),
            )
        };
        if first != PDH_MORE_DATA || buf_size == 0 {
            return Vec::new();
        }

        // Allocate aligned u64 backing storage so `PDH_FMT_COUNTERVALUE_ITEM_W` alignment is guaranteed.
        let u64_len = (buf_size as usize).div_ceil(size_of::<u64>());
        let mut raw = vec![0u64; u64_len];
        let mut actual_bytes = u32::try_from(raw.len() * size_of::<u64>()).unwrap_or(buf_size);
        let mut count = 0u32;
        // SAFETY: `raw` is 8-byte aligned and at least `buf_size` bytes long.
        let status = unsafe {
            PdhGetFormattedCounterArrayW(
                counter,
                PDH_FMT_DOUBLE,
                &raw mut actual_bytes,
                &raw mut count,
                raw.as_mut_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>(),
            )
        };
        if status != ERROR_SUCCESS_U32 || count == 0 {
            return Vec::new();
        }

        let max_items = (actual_bytes as usize) / size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
        let valid_count = (count as usize).min(max_items);
        let items_ptr = raw.as_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>();

        let mut values = Vec::with_capacity(valid_count);
        for idx in 0..valid_count {
            // SAFETY: `idx < valid_count` is strictly inside the allocated `raw` buffer.
            let item = unsafe { &*items_ptr.add(idx) };
            let name = decode_wide_cstr(item.szName);
            if name.contains("_Total") {
                continue;
            }
            if item.FmtValue.CStatus != PDH_CSTATUS_VALID_DATA
                && item.FmtValue.CStatus != PDH_CSTATUS_NEW_DATA
            {
                continue;
            }
            // SAFETY: `PDH_FMT_DOUBLE` populates `doubleValue`.
            let v = unsafe { item.FmtValue.Anonymous.doubleValue };
            if v.is_finite() && v > 0.0 {
                values.push(v);
            }
        }
        values
    }

    fn decode_wide_cstr(ptr: *const u16) -> String {
        if ptr.is_null() {
            return String::new();
        }
        let mut len = 0usize;
        // SAFETY: PDH guarantees NUL-terminated instance names within the returned buffer;
        // cap scan at 256 UTF-16 units for defense in depth.
        while len < 256 && unsafe { *ptr.add(len) } != 0 {
            len += 1;
        }
        // SAFETY: `len` code units were verified non-NUL above.
        let slice = unsafe { core::slice::from_raw_parts(ptr, len) };
        String::from_utf16_lossy(slice)
    }
}

#[cfg(not(windows))]
mod stub {
    use super::CpuPdhSample;

    #[derive(Debug)]
    pub struct CpuPdhQuery;

    impl CpuPdhQuery {
        pub fn open() -> Option<Self> {
            None
        }

        pub fn sample(&mut self) -> Option<CpuPdhSample> {
            None
        }
    }
}

#[cfg(not(windows))]
pub use stub::CpuPdhQuery;
#[cfg(windows)]
pub use win::CpuPdhQuery;
