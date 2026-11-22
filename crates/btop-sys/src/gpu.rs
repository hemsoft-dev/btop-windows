//! Windows 11 x64 GPU baseline telemetry backed by DXGI (`IDXGIFactory6`, excluding
//! `DXGI_ADAPTER_FLAG_SOFTWARE`) and a persistent English WDDM PDH query (`\GPU Engine(*)`
//! and `\GPU Adapter Memory(*)`), matching ADR-0002.

use std::collections::BTreeMap;

/// Maximum number of dedicated hardware GPUs tracked (`gpu0..gpu5`).
pub const MAX_GPUS: usize = 6;

const DXGI_SOFTWARE_FLAG_BIT: u32 = 2;
const MICROSOFT_VENDOR_ID: u32 = 0x1414;
const MICROSOFT_BASIC_RENDER_DEVICE_ID: u32 = 0x008C;

/// Returns `true` when an adapter is a software/WARP rasterizer (`DXGI_ADAPTER_FLAG_SOFTWARE`,
/// or `Microsoft Basic Render Driver` (`0x1414:0x008C`), which headless Hyper-V/Azure VMs can
/// expose without setting `DXGI_ADAPTER_FLAG_SOFTWARE`).
pub fn is_software_adapter(flags: u32, vendor_id: u32, device_id: u32, name: &str) -> bool {
    (flags & DXGI_SOFTWARE_FLAG_BIT) != 0
        || (vendor_id == MICROSOFT_VENDOR_ID && device_id == MICROSOFT_BASIC_RENDER_DEVICE_ID)
        || name.eq_ignore_ascii_case("Microsoft Basic Render Driver")
}

/// Hardware adapter metadata enumerated via DXGI (`IDXGIFactory6::EnumAdapters1`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawGpuAdapter {
    pub name: String,
    pub vendor: String,
    pub vendor_id: u32,
    pub device_id: u32,
    pub luid: u64,
    pub dedicated_vram_total_bytes: u64,
    pub shared_vram_total_bytes: u64,
    pub is_software: bool,
}

/// System-wide per-adapter GPU telemetry aggregated from one WDDM PDH sample.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GpuPdhMetrics {
    /// Busiest engine utilization percentage in `0.0..=100.0` (matching Windows Task Manager).
    pub busiest_engine_pct: Option<f32>,
    /// System-wide dedicated GPU memory usage in bytes.
    pub dedicated_used_bytes: Option<u64>,
    /// System-wide shared GPU memory usage in bytes.
    pub shared_used_bytes: Option<u64>,
}

/// Sample returned by [`GpuPdhQuery::sample`], keyed by DXGI adapter LUID.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GpuPdhSample {
    pub by_luid: BTreeMap<u64, GpuPdhMetrics>,
}

/// Packs a Win32 `LUID { HighPart, LowPart }` into a canonical `u64` key.
pub fn encode_luid(high_part: i32, low_part: u32) -> u64 {
    let hi = u64::from(high_part.cast_unsigned());
    (hi << 32) | u64::from(low_part)
}

/// Maps well-known PCI vendor IDs to clean human-readable vendor names.
pub fn vendor_name_from_id(vendor_id: u32) -> String {
    match vendor_id {
        0x10DE => "NVIDIA".to_owned(),
        0x1002 | 0x1022 => "AMD".to_owned(),
        0x8086 => "Intel".to_owned(),
        0x1414 => "Microsoft".to_owned(),
        other => format!("0x{other:04X}"),
    }
}

/// Filters out software adapters (`DXGI_ADAPTER_FLAG_SOFTWARE`, e.g. Microsoft Basic Render Driver)
/// and caps the hardware adapter list at [`MAX_GPUS`].
pub fn filter_hardware_adapters(adapters: Vec<RawGpuAdapter>) -> Vec<RawGpuAdapter> {
    adapters
        .into_iter()
        .filter(|a| !a.is_software && !is_software_adapter(0, a.vendor_id, a.device_id, &a.name))
        .take(MAX_GPUS)
        .collect()
}

/// Extracts the `luid_0x{HIGH}_0x{LOW}` token from a WDDM PDH counter instance name.
///
/// Examples:
/// - `pid_4216_luid_0x00000000_0x000189EF_phys_0_eng_0_engtype_3D` -> `Some(0x00000000_000189EF)`
/// - `luid_0x00000000_0x0001E8E2_phys_0` -> `Some(0x00000000_0001E8E2)`
pub fn parse_gpu_pdh_luid(instance: &str) -> Option<u64> {
    let lower = instance.to_ascii_lowercase();
    let idx = lower.find("luid_0x")?;
    let rest = &lower[idx + "luid_0x".len()..];
    let (hi_hex, after_hi) = rest.split_once("_0x")?;
    let lo_hex = after_hi
        .split('_')
        .next()
        .filter(|segment| !segment.is_empty())?;
    let high = u32::from_str_radix(hi_hex, 16).ok()?;
    let low = u32::from_str_radix(lo_hex, 16).ok()?;
    Some((u64::from(high) << 32) | u64::from(low))
}

/// Extracts `(luid, engine_key)` from a `\GPU Engine(*)` instance name, stripping the leading
/// `pid_<N>_` prefix so utilization across all processes on the same physical engine sums cleanly.
pub fn parse_gpu_engine_key(instance: &str) -> Option<(u64, String)> {
    let lower = instance.to_ascii_lowercase();
    let idx = lower.find("luid_0x")?;
    let rest = &lower[idx + "luid_0x".len()..];
    let (hi_hex, after_hi) = rest.split_once("_0x")?;
    let (lo_hex, engine_suffix) = after_hi.split_once('_')?;
    if engine_suffix.is_empty() {
        return None;
    }
    let high = u32::from_str_radix(hi_hex, 16).ok()?;
    let low = u32::from_str_radix(lo_hex, 16).ok()?;
    let luid = (u64::from(high) << 32) | u64::from(low);
    Some((luid, engine_suffix.to_owned()))
}

/// Computes Task Manager's headline GPU utilization per adapter LUID:
/// sums utilization across all PIDs sharing `(luid, engine_key)`, clamps each engine to
/// `0.0..=100.0`, and selects the maximum across engines for each adapter.
pub fn aggregate_gpu_engine_utilization(samples: &[(String, f64)]) -> BTreeMap<u64, f32> {
    let mut engine_totals: BTreeMap<(u64, String), f64> = BTreeMap::new();
    for (instance, value) in samples {
        if !value.is_finite() || *value < 0.0 {
            continue;
        }
        if let Some((luid, engine_key)) = parse_gpu_engine_key(instance) {
            *engine_totals.entry((luid, engine_key)).or_insert(0.0) += *value;
        }
    }

    let mut per_adapter: BTreeMap<u64, f32> = BTreeMap::new();
    for ((luid, _engine), sum) in engine_totals {
        #[allow(clippy::cast_possible_truncation)]
        let clamped = sum.clamp(0.0, 100.0) as f32;
        per_adapter
            .entry(luid)
            .and_modify(|max_pct| *max_pct = max_pct.max(clamped))
            .or_insert(clamped);
    }
    per_adapter
}

/// Sums `\GPU Adapter Memory(*)` byte counters per adapter LUID across physical adapter instances.
pub fn aggregate_gpu_memory_bytes(samples: &[(String, u64)]) -> BTreeMap<u64, u64> {
    let mut per_adapter: BTreeMap<u64, u64> = BTreeMap::new();
    for (instance, bytes) in samples {
        if let Some(luid) = parse_gpu_pdh_luid(instance) {
            per_adapter
                .entry(luid)
                .and_modify(|total| *total = total.saturating_add(*bytes))
                .or_insert(*bytes);
        }
    }
    per_adapter
}

#[cfg(windows)]
mod win {
    use core::mem::size_of;
    use core::ptr;
    use std::collections::BTreeMap;
    use std::fmt;

    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory2, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_CREATE_FACTORY_FLAGS,
        DXGI_ERROR_NOT_FOUND, IDXGIFactory6,
    };
    use windows_sys::Win32::System::Performance::{
        PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE,
        PDH_FMT_LARGE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA, PdhAddEnglishCounterW,
        PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW,
    };

    use super::{
        GpuPdhMetrics, GpuPdhSample, RawGpuAdapter, aggregate_gpu_engine_utilization,
        aggregate_gpu_memory_bytes, encode_luid, filter_hardware_adapters, is_software_adapter,
        vendor_name_from_id,
    };

    const ERROR_SUCCESS_U32: u32 = 0;
    const PDH_FMT_NOCAP100: u32 = 0x0000_8000;
    const MAX_PDH_ARRAY_BYTES: usize = 16 * 1024 * 1024;

    /// Enumerates hardware GPU adapters via `IDXGIFactory6::EnumAdapters1`, excluding
    /// `DXGI_ADAPTER_FLAG_SOFTWARE` (Microsoft Basic Render Driver). Never invokes
    /// `IDXGIAdapter3::QueryVideoMemoryInfo`.
    pub fn enumerate_hardware_gpus() -> Result<Vec<RawGpuAdapter>, String> {
        // SAFETY: `CreateDXGIFactory2` creates a standard COM `IDXGIFactory6` instance.
        let factory: IDXGIFactory6 = unsafe { CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)) }
            .map_err(|err| format!("CreateDXGIFactory2(IDXGIFactory6) failed: {err}"))?;

        let mut raw = Vec::new();
        for index in 0u32..32 {
            // SAFETY: `EnumAdapters1` returns `DXGI_ERROR_NOT_FOUND` once all adapters are listed.
            let adapter = match unsafe { factory.EnumAdapters1(index) } {
                Ok(a) => a,
                Err(err) if err.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(err) => {
                    return Err(format!(
                        "IDXGIFactory6::EnumAdapters1({index}) failed: {err}"
                    ));
                }
            };
            // SAFETY: `GetDesc1` fills `DXGI_ADAPTER_DESC1` for a valid `IDXGIAdapter1`.
            let desc = unsafe { adapter.GetDesc1() }
                .map_err(|err| format!("IDXGIAdapter1::GetDesc1({index}) failed: {err}"))?;

            let nul = desc
                .Description
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(desc.Description.len());
            let name = String::from_utf16_lossy(&desc.Description[..nul])
                .trim()
                .to_owned();
            let software_flag = u32::try_from(DXGI_ADAPTER_FLAG_SOFTWARE.0).unwrap_or(2);
            let is_software = is_software_adapter(
                desc.Flags & software_flag,
                desc.VendorId,
                desc.DeviceId,
                &name,
            );

            raw.push(RawGpuAdapter {
                name: if name.is_empty() {
                    format!("GPU {index}")
                } else {
                    name
                },
                vendor: vendor_name_from_id(desc.VendorId),
                vendor_id: desc.VendorId,
                device_id: desc.DeviceId,
                luid: encode_luid(desc.AdapterLuid.HighPart, desc.AdapterLuid.LowPart),
                dedicated_vram_total_bytes: desc.DedicatedVideoMemory as u64,
                shared_vram_total_bytes: desc.SharedSystemMemory as u64,
                is_software,
            });
        }

        Ok(filter_hardware_adapters(raw))
    }

    /// Persistent English WDDM PDH query for system-wide GPU utilization and VRAM usage
    /// (`\GPU Engine(*)\Utilization Percentage`, `\GPU Adapter Memory(*)\Dedicated Usage`,
    /// `\GPU Adapter Memory(*)\Shared Usage`).
    pub struct GpuPdhQuery {
        query: PDH_HQUERY,
        engine_counter: Option<PDH_HCOUNTER>,
        dedicated_counter: Option<PDH_HCOUNTER>,
        shared_counter: Option<PDH_HCOUNTER>,
        primed: bool,
    }

    // SAFETY: `GpuPdhQuery` owns its `PDH_HQUERY` and counter handles exclusively and is
    // accessed from one collector thread at a time.
    unsafe impl Send for GpuPdhQuery {}

    impl fmt::Debug for GpuPdhQuery {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("GpuPdhQuery")
                .field("has_engine", &self.engine_counter.is_some())
                .field("has_dedicated", &self.dedicated_counter.is_some())
                .field("has_shared", &self.shared_counter.is_some())
                .field("primed", &self.primed)
                .finish_non_exhaustive()
        }
    }

    impl GpuPdhQuery {
        /// Opens the persistent English WDDM PDH query without blocking for a delta yet.
        /// The first [`Self::sample`] call primes the rate counters and returns `Ok(None)`
        /// so callers can publish `DataState::WarmingUp` (per ADR-0002).
        pub fn open() -> Option<Self> {
            let mut query: PDH_HQUERY = ptr::null_mut();
            // SAFETY: Opening a fresh live PDH query.
            let open_status = unsafe { PdhOpenQueryW(ptr::null(), 0, &raw mut query) };
            if open_status != ERROR_SUCCESS_U32 || query.is_null() {
                return None;
            }

            let engine_counter =
                add_english_counter(query, "\\GPU Engine(*)\\Utilization Percentage");
            let dedicated_counter =
                add_english_counter(query, "\\GPU Adapter Memory(*)\\Dedicated Usage");
            let shared_counter =
                add_english_counter(query, "\\GPU Adapter Memory(*)\\Shared Usage");

            if engine_counter.is_none() && dedicated_counter.is_none() && shared_counter.is_none() {
                // SAFETY: Closing unused query handle on early failure.
                unsafe {
                    let _ = PdhCloseQuery(query);
                }
                return None;
            }

            Some(Self {
                query,
                engine_counter,
                dedicated_counter,
                shared_counter,
                primed: false,
            })
        }

        /// Collects one WDDM PDH tick.
        ///
        /// Returns `Ok(None)` on the initial baseline call while rate counters warm up, and
        /// `Ok(Some(sample))` on subsequent ticks.
        pub fn sample(&mut self) -> Result<Option<GpuPdhSample>, String> {
            // SAFETY: `self.query` is a valid opened `PDH_HQUERY`.
            let collect_status = unsafe { PdhCollectQueryData(self.query) };
            if collect_status != ERROR_SUCCESS_U32 {
                return Err(format!(
                    "PdhCollectQueryData failed (0x{collect_status:08X})"
                ));
            }
            if !self.primed {
                self.primed = true;
                return Ok(None);
            }

            let engine_raw = self
                .engine_counter
                .map(read_double_array)
                .unwrap_or_default();
            let ded_raw = self
                .dedicated_counter
                .map(read_large_array)
                .unwrap_or_default();
            let shr_raw = self
                .shared_counter
                .map(read_large_array)
                .unwrap_or_default();

            let util_map = aggregate_gpu_engine_utilization(&engine_raw);
            let ded_map = aggregate_gpu_memory_bytes(&ded_raw);
            let shr_map = aggregate_gpu_memory_bytes(&shr_raw);

            let mut by_luid: BTreeMap<u64, GpuPdhMetrics> = BTreeMap::new();
            for (luid, pct) in util_map {
                by_luid.entry(luid).or_default().busiest_engine_pct = Some(pct);
            }
            for (luid, bytes) in ded_map {
                by_luid.entry(luid).or_default().dedicated_used_bytes = Some(bytes);
            }
            for (luid, bytes) in shr_map {
                by_luid.entry(luid).or_default().shared_used_bytes = Some(bytes);
            }

            Ok(Some(GpuPdhSample { by_luid }))
        }
    }

    impl Drop for GpuPdhQuery {
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

    fn read_pdh_array_raw(counter: PDH_HCOUNTER, fmt: u32) -> Option<(Vec<u64>, usize)> {
        let mut buf_size = 0u32;
        let mut item_count = 0u32;
        // SAFETY: First call queries required byte size for formatted counter array.
        let first = unsafe {
            PdhGetFormattedCounterArrayW(
                counter,
                fmt,
                &raw mut buf_size,
                &raw mut item_count,
                ptr::null_mut(),
            )
        };
        if first != PDH_MORE_DATA || buf_size == 0 {
            return None;
        }
        let alloc_bytes = (buf_size as usize)
            .saturating_add(4096)
            .min(MAX_PDH_ARRAY_BYTES);
        let mut raw = vec![0u64; alloc_bytes.div_ceil(size_of::<u64>())];
        let mut actual_bytes = u32::try_from(size_of_val(raw.as_slice())).ok()?;
        let mut count = 0u32;
        // SAFETY: `raw` is 8-byte aligned and provides `actual_bytes` writable bytes.
        let status = unsafe {
            PdhGetFormattedCounterArrayW(
                counter,
                fmt,
                &raw mut actual_bytes,
                &raw mut count,
                raw.as_mut_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>(),
            )
        };
        if status != ERROR_SUCCESS_U32 || count == 0 {
            return None;
        }
        let max_items = (actual_bytes as usize).min(size_of_val(raw.as_slice()))
            / size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
        let valid_count = (count as usize).min(max_items);
        Some((raw, valid_count))
    }

    fn read_double_array(counter: PDH_HCOUNTER) -> Vec<(String, f64)> {
        let Some((raw, valid_count)) =
            read_pdh_array_raw(counter, PDH_FMT_DOUBLE | PDH_FMT_NOCAP100)
        else {
            return Vec::new();
        };
        let buf_start = raw.as_ptr().cast::<u8>() as usize;
        let buf_end = buf_start.saturating_add(size_of_val(raw.as_slice()));
        let items_ptr = raw.as_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>();

        let mut out = Vec::with_capacity(valid_count);
        for idx in 0..valid_count {
            // SAFETY: `idx < valid_count` is strictly inside `raw`.
            let item = unsafe { &*items_ptr.add(idx) };
            if item.FmtValue.CStatus != PDH_CSTATUS_VALID_DATA
                && item.FmtValue.CStatus != PDH_CSTATUS_NEW_DATA
            {
                continue;
            }
            let name = decode_wide_in_buffer(item.szName, buf_start, buf_end);
            if name.is_empty() {
                continue;
            }
            // SAFETY: `PDH_FMT_DOUBLE` populates `doubleValue`.
            let val = unsafe { item.FmtValue.Anonymous.doubleValue };
            if val.is_finite() {
                out.push((name, val));
            }
        }
        out
    }

    fn read_large_array(counter: PDH_HCOUNTER) -> Vec<(String, u64)> {
        let Some((raw, valid_count)) = read_pdh_array_raw(counter, PDH_FMT_LARGE) else {
            return Vec::new();
        };
        let buf_start = raw.as_ptr().cast::<u8>() as usize;
        let buf_end = buf_start.saturating_add(size_of_val(raw.as_slice()));
        let items_ptr = raw.as_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>();

        let mut out = Vec::with_capacity(valid_count);
        for idx in 0..valid_count {
            // SAFETY: `idx < valid_count` is strictly inside `raw`.
            let item = unsafe { &*items_ptr.add(idx) };
            if item.FmtValue.CStatus != PDH_CSTATUS_VALID_DATA
                && item.FmtValue.CStatus != PDH_CSTATUS_NEW_DATA
            {
                continue;
            }
            let name = decode_wide_in_buffer(item.szName, buf_start, buf_end);
            if name.is_empty() {
                continue;
            }
            // SAFETY: `PDH_FMT_LARGE` populates `largeValue`.
            let val = unsafe { item.FmtValue.Anonymous.largeValue };
            if val >= 0 {
                out.push((name, val.cast_unsigned()));
            }
        }
        out
    }

    fn decode_wide_in_buffer(ptr: *const u16, buf_start: usize, buf_end: usize) -> String {
        if ptr.is_null() {
            return String::new();
        }
        let addr = ptr as usize;
        if addr < buf_start || addr >= buf_end {
            return String::new();
        }
        let max_units = ((buf_end - addr) / size_of::<u16>()).min(512);
        let mut units = Vec::new();
        for idx in 0..max_units {
            // SAFETY: `addr + (idx + 1) * 2 <= buf_end`, strictly inside the PDH output buffer.
            let ch = unsafe { ptr.add(idx).read_unaligned() };
            if ch == 0 {
                break;
            }
            units.push(ch);
        }
        String::from_utf16_lossy(&units)
    }
}

#[cfg(not(windows))]
mod stub {
    use super::{GpuPdhSample, RawGpuAdapter};

    pub fn enumerate_hardware_gpus() -> Result<Vec<RawGpuAdapter>, String> {
        Err("DXGI GPU enumeration requires Windows".to_owned())
    }

    #[derive(Debug)]
    pub struct GpuPdhQuery;

    impl GpuPdhQuery {
        pub fn open() -> Option<Self> {
            None
        }

        pub fn sample(&mut self) -> Result<Option<GpuPdhSample>, String> {
            Ok(None)
        }
    }
}

#[cfg(not(windows))]
pub use stub::{GpuPdhQuery, enumerate_hardware_gpus};
#[cfg(windows)]
pub use win::{GpuPdhQuery, enumerate_hardware_gpus};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_out_software_adapters_and_maps_vendors() {
        let raw = vec![
            RawGpuAdapter {
                name: "NVIDIA GeForce RTX 5090".to_owned(),
                vendor: vendor_name_from_id(0x10DE),
                vendor_id: 0x10DE,
                device_id: 0x2B85,
                luid: encode_luid(0, 0x0001_89EF),
                dedicated_vram_total_bytes: 32 * 1024 * 1024 * 1024,
                shared_vram_total_bytes: 32 * 1024 * 1024 * 1024,
                is_software: false,
            },
            RawGpuAdapter {
                name: "Microsoft Basic Render Driver".to_owned(),
                vendor: vendor_name_from_id(0x1414),
                vendor_id: 0x1414,
                device_id: 0x008C,
                luid: encode_luid(0, 0x0001_9999),
                dedicated_vram_total_bytes: 0,
                shared_vram_total_bytes: 16 * 1024 * 1024 * 1024,
                is_software: true,
            },
            RawGpuAdapter {
                name: "Microsoft Basic Render Driver".to_owned(),
                vendor: vendor_name_from_id(0x1414),
                vendor_id: 0x1414,
                device_id: 0x008C,
                luid: encode_luid(0, 0x0001_999A),
                dedicated_vram_total_bytes: 0,
                shared_vram_total_bytes: 16 * 1024 * 1024 * 1024,
                // Headless Hyper-V/Azure VMs can expose WARP without DXGI_ADAPTER_FLAG_SOFTWARE.
                is_software: false,
            },
            RawGpuAdapter {
                name: "AMD Radeon(TM) Graphics".to_owned(),
                vendor: vendor_name_from_id(0x1002),
                vendor_id: 0x1002,
                device_id: 0x13C0,
                luid: encode_luid(0, 0x0001_E8E2),
                dedicated_vram_total_bytes: 2 * 1024 * 1024 * 1024,
                shared_vram_total_bytes: 32 * 1024 * 1024 * 1024,
                is_software: false,
            },
        ];

        let filtered = filter_hardware_adapters(raw);
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].vendor, "NVIDIA");
        assert_eq!(filtered[1].vendor, "AMD");
        assert!(filtered.iter().all(|a| !a.is_software));
    }

    #[test]
    fn parses_wddm_pdh_luids_and_computes_busiest_engine_utilization_and_vram() {
        let luid_nv = encode_luid(0, 0x0001_89EF);
        let luid_amd = encode_luid(0, 0x0001_E8E2);

        assert_eq!(
            parse_gpu_pdh_luid("luid_0x00000000_0x000189EF_phys_0"),
            Some(luid_nv)
        );
        assert_eq!(
            parse_gpu_pdh_luid("pid_4216_luid_0x00000000_0x0001E8E2_phys_0_eng_0_engtype_3D"),
            Some(luid_amd)
        );

        let engine_samples = vec![
            (
                "pid_100_luid_0x00000000_0x000189EF_phys_0_eng_0_engtype_3D".to_owned(),
                42.5,
            ),
            (
                "pid_200_luid_0x00000000_0x000189EF_phys_0_eng_0_engtype_3D".to_owned(),
                31.0,
            ),
            (
                "pid_100_luid_0x00000000_0x000189EF_phys_0_eng_1_engtype_Copy".to_owned(),
                18.0,
            ),
            (
                "pid_300_luid_0x00000000_0x0001E8E2_phys_0_eng_0_engtype_VideoDecode".to_owned(),
                12.5,
            ),
        ];
        let util = aggregate_gpu_engine_utilization(&engine_samples);
        assert!((util[&luid_nv] - 73.5).abs() < 0.01);
        assert!((util[&luid_amd] - 12.5).abs() < 0.01);

        let mem_samples = vec![
            (
                "luid_0x00000000_0x000189EF_phys_0".to_owned(),
                12_000_000_000,
            ),
            ("luid_0x00000000_0x0001E8E2_phys_0".to_owned(), 58_000_000),
        ];
        let mem = aggregate_gpu_memory_bytes(&mem_samples);
        assert_eq!(mem[&luid_nv], 12_000_000_000);
        assert_eq!(mem[&luid_amd], 58_000_000);
    }

    #[cfg(windows)]
    #[test]
    fn live_windows_dxgi_and_wddm_pdh_baseline_runs_cleanly() {
        let adapters = enumerate_hardware_gpus().expect("DXGI hardware GPU enumeration");
        assert!(
            adapters.iter().all(|a| !a.is_software),
            "software adapters such as Microsoft Basic Render Driver must be excluded"
        );
        if let Some(mut pdh) = GpuPdhQuery::open() {
            // Initial tick warms up rate counters cleanly.
            assert_eq!(pdh.sample().expect("prime WDDM PDH"), None);
            std::thread::sleep(std::time::Duration::from_millis(80));
            let sample = pdh
                .sample()
                .expect("collect WDDM PDH sample")
                .expect("sample after warmup");
            for adapter in &adapters {
                if let Some(metrics) = sample.by_luid.get(&adapter.luid)
                    && let Some(pct) = metrics.busiest_engine_pct
                {
                    assert!((0.0..=100.0).contains(&pct), "invalid GPU util pct: {pct}");
                }
            }
        }
    }
}
