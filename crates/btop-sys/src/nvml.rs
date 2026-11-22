//! Optional dynamic loader for NVIDIA Management Library (`nvml.dll`) strictly from
//! `System32` (`LOAD_LIBRARY_SEARCH_SYSTEM32`), calling `nvmlShutdown` before `FreeLibrary`
//! on exit and mapping `NVML_ERROR_NOT_SUPPORTED` fields cleanly to `None`.

use core::ffi::c_char;
use core::mem::{offset_of, size_of};

/// NVML return status: operation succeeded.
pub const NVML_SUCCESS: u32 = 0;
/// NVML return status: feature/metric is not supported on this GPU or driver.
pub const NVML_ERROR_NOT_SUPPORTED: u32 = 3;

/// Matches `nvmlPciInfo_t` in NVIDIA NVML headers (`nvmlDeviceGetPciInfo_v3`).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct NvmlPciInfo {
    pub bus_id_legacy: [c_char; 16],
    pub domain: u32,
    pub bus: u32,
    pub device: u32,
    pub pci_device_id: u32,
    pub pci_sub_system_id: u32,
    pub bus_id: [c_char; 32],
}

const _: () = {
    assert!(size_of::<NvmlPciInfo>() == 68);
    assert!(offset_of!(NvmlPciInfo, domain) == 16);
    assert!(offset_of!(NvmlPciInfo, bus) == 20);
    assert!(offset_of!(NvmlPciInfo, device) == 24);
    assert!(offset_of!(NvmlPciInfo, pci_device_id) == 28);
    assert!(offset_of!(NvmlPciInfo, bus_id) == 36);
};

/// Optional vendor-enriched telemetry sample for one NVIDIA GPU reported by `nvml.dll`.
#[derive(Debug, Clone, PartialEq)]
pub struct RawNvmlDevice {
    pub index: u32,
    pub name: String,
    pub uuid: Option<String>,
    pub pci_bdf: Option<String>,
    pub pci_vendor_id: Option<u32>,
    pub pci_device_id: Option<u32>,
    pub temp_celsius: Option<f32>,
    pub temp_max_celsius: Option<f32>,
    pub core_clock_mhz: Option<u32>,
    pub mem_clock_mhz: Option<u32>,
    pub power_watts: Option<f32>,
    pub power_limit_watts: Option<f32>,
    pub pstate: Option<String>,
    pub encoder_pct: Option<f32>,
    pub decoder_pct: Option<f32>,
    pub pcie_tx_kbs: Option<u64>,
    pub pcie_rx_kbs: Option<u64>,
}

/// Converts an NVML return status + output value into `Option<T>`, mapping
/// `NVML_ERROR_NOT_SUPPORTED` and any other non-success status cleanly to `None`.
pub fn decode_nvml_field<T>(status: u32, value: T) -> Option<T> {
    if status == NVML_SUCCESS {
        Some(value)
    } else {
        None
    }
}

/// Formats a canonical PCI Bus/Device/Function identifier (`dddd:bb:dd.0`).
pub fn format_pci_bdf(domain: u32, bus: u32, device: u32) -> String {
    format!("{domain:04x}:{bus:02x}:{device:02x}.0")
}

/// Decodes NVML's packed `pciDeviceId` (`(device_id << 16) | vendor_id`) into `(vendor_id, device_id)`.
pub const fn decode_nvml_pci_ids(packed: u32) -> (u32, u32) {
    (packed & 0xFFFF, (packed >> 16) & 0xFFFF)
}

/// Decodes an NVML performance state integer (`0..=15`) into `"P0"`..`"P15"`, ignoring `32` (unknown).
pub fn decode_nvml_pstate(pstate: u32) -> Option<String> {
    if pstate <= 15 {
        Some(format!("P{pstate}"))
    } else {
        None
    }
}

/// Converts milliwatts reported by NVML into watts.
pub fn milliwatts_to_watts(mw: u32) -> f32 {
    #[allow(clippy::cast_precision_loss)]
    let w = (mw as f32) / 1000.0;
    w
}

#[cfg(windows)]
mod win {
    use core::ffi::{c_char, c_void};
    use core::ptr;
    use std::fmt;

    use windows_sys::Win32::Foundation::{FreeLibrary, HMODULE};
    use windows_sys::Win32::System::LibraryLoader::{
        GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
    };

    use super::{
        NVML_SUCCESS, NvmlPciInfo, RawNvmlDevice, decode_nvml_field, decode_nvml_pci_ids,
        decode_nvml_pstate, format_pci_bdf, milliwatts_to_watts,
    };

    type NvmlDevice = *mut c_void;
    type FnInit = unsafe extern "C" fn() -> u32;
    type FnShutdown = unsafe extern "C" fn() -> u32;
    type FnDeviceGetCount = unsafe extern "C" fn(*mut u32) -> u32;
    type FnDeviceGetHandleByIndex = unsafe extern "C" fn(u32, *mut NvmlDevice) -> u32;
    type FnDeviceGetString = unsafe extern "C" fn(NvmlDevice, *mut c_char, u32) -> u32;
    type FnDeviceGetPciInfo = unsafe extern "C" fn(NvmlDevice, *mut NvmlPciInfo) -> u32;
    type FnDeviceGetTemp = unsafe extern "C" fn(NvmlDevice, u32, *mut u32) -> u32;
    type FnDeviceGetClock = unsafe extern "C" fn(NvmlDevice, u32, *mut u32) -> u32;
    type FnDeviceGetU32 = unsafe extern "C" fn(NvmlDevice, *mut u32) -> u32;
    type FnDeviceGetEncDec = unsafe extern "C" fn(NvmlDevice, *mut u32, *mut u32) -> u32;
    type FnDeviceGetPcie = unsafe extern "C" fn(NvmlDevice, u32, *mut u32) -> u32;

    const NVML_TEMPERATURE_GPU: u32 = 0;
    const NVML_TEMP_THRESHOLD_SHUTDOWN: u32 = 0;
    const NVML_TEMP_THRESHOLD_SLOWDOWN: u32 = 1;
    const NVML_TEMP_THRESHOLD_GPU_MAX: u32 = 3;
    const NVML_CLOCK_GRAPHICS: u32 = 0;
    const NVML_CLOCK_MEM: u32 = 2;
    const NVML_PCIE_UTIL_TX_BYTES: u32 = 0;
    const NVML_PCIE_UTIL_RX_BYTES: u32 = 1;

    /// Dynamically loaded handle to `C:\Windows\System32\nvml.dll`.
    ///
    /// Guarantees `nvmlShutdown()` runs before `FreeLibrary()` on `Drop`.
    pub struct NvmlLibrary {
        module: HMODULE,
        initialized: bool,
        shutdown: FnShutdown,
        device_get_count: FnDeviceGetCount,
        device_get_handle: FnDeviceGetHandleByIndex,
        device_get_name: Option<FnDeviceGetString>,
        device_get_uuid: Option<FnDeviceGetString>,
        device_get_pci_info: Option<FnDeviceGetPciInfo>,
        device_get_temperature: Option<FnDeviceGetTemp>,
        device_get_temp_threshold: Option<FnDeviceGetTemp>,
        device_get_clock: Option<FnDeviceGetClock>,
        device_get_power_usage: Option<FnDeviceGetU32>,
        device_get_power_limit: Option<FnDeviceGetU32>,
        device_get_pstate: Option<FnDeviceGetU32>,
        device_get_encoder: Option<FnDeviceGetEncDec>,
        device_get_decoder: Option<FnDeviceGetEncDec>,
        device_get_pcie_throughput: Option<FnDeviceGetPcie>,
    }

    // SAFETY: `NvmlLibrary` owns its `HMODULE` exclusively and is moved onto a single
    // dedicated background worker thread for its entire lifetime.
    unsafe impl Send for NvmlLibrary {}

    impl fmt::Debug for NvmlLibrary {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("NvmlLibrary")
                .field("initialized", &self.initialized)
                .finish_non_exhaustive()
        }
    }

    impl NvmlLibrary {
        /// Attempts to load `nvml.dll` strictly from `System32` (`LOAD_LIBRARY_SEARCH_SYSTEM32`)
        /// and initialize NVML. Returns `None` silently when `nvml.dll` is absent or unavailable.
        pub fn load() -> Option<Self> {
            Self::load_named("nvml.dll")
        }

        /// Loads a specific DLL name strictly from `System32` (used by tests to verify missing-DLL fallback).
        pub fn load_named(dll_name: &str) -> Option<Self> {
            let wide: Vec<u16> = dll_name.encode_utf16().chain(core::iter::once(0)).collect();
            // SAFETY: Explicitly restricts DLL resolution to `System32` via `LOAD_LIBRARY_SEARCH_SYSTEM32`.
            let module = unsafe {
                LoadLibraryExW(wide.as_ptr(), ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32)
            };
            if module.is_null() {
                return None;
            }

            let init: Option<FnInit> = resolve_sym(module, b"nvmlInit_v2\0")
                .or_else(|| resolve_sym(module, b"nvmlInit\0"));
            let shutdown: Option<FnShutdown> = resolve_sym(module, b"nvmlShutdown\0");
            let device_get_count: Option<FnDeviceGetCount> =
                resolve_sym(module, b"nvmlDeviceGetCount_v2\0")
                    .or_else(|| resolve_sym(module, b"nvmlDeviceGetCount\0"));
            let device_get_handle: Option<FnDeviceGetHandleByIndex> =
                resolve_sym(module, b"nvmlDeviceGetHandleByIndex_v2\0")
                    .or_else(|| resolve_sym(module, b"nvmlDeviceGetHandleByIndex\0"));

            let (Some(init), Some(shutdown), Some(device_get_count), Some(device_get_handle)) =
                (init, shutdown, device_get_count, device_get_handle)
            else {
                // SAFETY: Releasing module handle when required symbols are missing.
                unsafe {
                    let _ = FreeLibrary(module);
                }
                return None;
            };

            // SAFETY: Valid `nvmlInit_v2` / `nvmlInit` function pointer from loaded `nvml.dll`.
            let init_status = unsafe { init() };
            if init_status != NVML_SUCCESS {
                // SAFETY: Releasing module handle when NVML initialization fails.
                unsafe {
                    let _ = FreeLibrary(module);
                }
                return None;
            }

            let device_get_pci_info = resolve_sym(module, b"nvmlDeviceGetPciInfo_v3\0")
                .or_else(|| resolve_sym(module, b"nvmlDeviceGetPciInfo_v2\0"))
                .or_else(|| resolve_sym(module, b"nvmlDeviceGetPciInfo\0"));
            let device_get_power_limit = resolve_sym(module, b"nvmlDeviceGetEnforcedPowerLimit\0")
                .or_else(|| resolve_sym(module, b"nvmlDeviceGetPowerManagementLimit\0"));

            Some(Self {
                module,
                initialized: true,
                shutdown,
                device_get_count,
                device_get_handle,
                device_get_name: resolve_sym(module, b"nvmlDeviceGetName\0"),
                device_get_uuid: resolve_sym(module, b"nvmlDeviceGetUUID\0"),
                device_get_pci_info,
                device_get_temperature: resolve_sym(module, b"nvmlDeviceGetTemperature\0"),
                device_get_temp_threshold: resolve_sym(
                    module,
                    b"nvmlDeviceGetTemperatureThreshold\0",
                ),
                device_get_clock: resolve_sym(module, b"nvmlDeviceGetClockInfo\0"),
                device_get_power_usage: resolve_sym(module, b"nvmlDeviceGetPowerUsage\0"),
                device_get_power_limit,
                device_get_pstate: resolve_sym(module, b"nvmlDeviceGetPerformanceState\0"),
                device_get_encoder: resolve_sym(module, b"nvmlDeviceGetEncoderUtilization\0"),
                device_get_decoder: resolve_sym(module, b"nvmlDeviceGetDecoderUtilization\0"),
                device_get_pcie_throughput: resolve_sym(module, b"nvmlDeviceGetPcieThroughput\0"),
            })
        }

        /// Queries all NVML devices once. Any unsupported field maps cleanly to `None`.
        pub fn query_devices(&self, measure_pcie_speeds: bool) -> Vec<RawNvmlDevice> {
            let mut count = 0u32;
            // SAFETY: Valid initialized NVML library and writable `u32`.
            if unsafe { (self.device_get_count)(&raw mut count) } != NVML_SUCCESS {
                return Vec::new();
            }

            let capped = count.min(16);
            let mut out = Vec::with_capacity(capped as usize);
            for idx in 0..capped {
                let mut handle: NvmlDevice = ptr::null_mut();
                // SAFETY: Querying NVML device handle for `idx < count`.
                if unsafe { (self.device_get_handle)(idx, &raw mut handle) } != NVML_SUCCESS
                    || handle.is_null()
                {
                    continue;
                }
                out.push(self.query_single_device(idx, handle, measure_pcie_speeds));
            }
            out
        }

        fn query_single_device(
            &self,
            index: u32,
            handle: NvmlDevice,
            measure_pcie_speeds: bool,
        ) -> RawNvmlDevice {
            let name = self
                .device_get_name
                .and_then(|f| read_nvml_string(f, handle, 96))
                .unwrap_or_else(|| format!("NVIDIA GPU {index}"));
            let uuid = self
                .device_get_uuid
                .and_then(|f| read_nvml_string(f, handle, 96));
            let (pci_bdf, pci_vendor_id, pci_device_id) = self.query_pci_info(handle);

            let temp_celsius = self.device_get_temperature.and_then(|f| {
                let mut t = 0u32;
                // SAFETY: Valid NVML handle and writable `u32`.
                let status = unsafe { f(handle, NVML_TEMPERATURE_GPU, &raw mut t) };
                #[allow(clippy::cast_precision_loss)]
                decode_nvml_field(status, t as f32)
            });
            let temp_max_celsius = self.query_temp_threshold(handle);

            let core_clock_mhz = self.query_clock(handle, NVML_CLOCK_GRAPHICS);
            let mem_clock_mhz = self.query_clock(handle, NVML_CLOCK_MEM);

            let power_watts = self.device_get_power_usage.and_then(|f| {
                let mut mw = 0u32;
                let status = unsafe { f(handle, &raw mut mw) };
                decode_nvml_field(status, milliwatts_to_watts(mw))
            });
            let power_limit_watts = self.device_get_power_limit.and_then(|f| {
                let mut mw = 0u32;
                let status = unsafe { f(handle, &raw mut mw) };
                decode_nvml_field(status, milliwatts_to_watts(mw))
            });
            let pstate = self.device_get_pstate.and_then(|f| {
                let mut ps = 0u32;
                let status = unsafe { f(handle, &raw mut ps) };
                decode_nvml_field(status, ps).and_then(decode_nvml_pstate)
            });

            let encoder_pct = query_enc_dec(self.device_get_encoder, handle);
            let decoder_pct = query_enc_dec(self.device_get_decoder, handle);
            let (tx_kbs, rx_kbs) = self.query_pcie_throughput(handle, measure_pcie_speeds);

            RawNvmlDevice {
                index,
                name,
                uuid,
                pci_bdf,
                pci_vendor_id,
                pci_device_id,
                temp_celsius,
                temp_max_celsius,
                core_clock_mhz,
                mem_clock_mhz,
                power_watts,
                power_limit_watts,
                pstate,
                encoder_pct,
                decoder_pct,
                pcie_tx_kbs: tx_kbs,
                pcie_rx_kbs: rx_kbs,
            }
        }

        fn query_pci_info(&self, handle: NvmlDevice) -> (Option<String>, Option<u32>, Option<u32>) {
            let Some(get_pci) = self.device_get_pci_info else {
                return (None, None, None);
            };
            let mut pci = NvmlPciInfo::default();
            // SAFETY: `NvmlPciInfo` matches `nvmlPciInfo_t` layout.
            let status = unsafe { get_pci(handle, &raw mut pci) };
            if status == NVML_SUCCESS {
                let (vid, did) = decode_nvml_pci_ids(pci.pci_device_id);
                (
                    Some(format_pci_bdf(pci.domain, pci.bus, pci.device)),
                    Some(vid),
                    Some(did),
                )
            } else {
                (None, None, None)
            }
        }

        fn query_temp_threshold(&self, handle: NvmlDevice) -> Option<f32> {
            let func = self.device_get_temp_threshold?;
            for threshold_kind in [
                NVML_TEMP_THRESHOLD_GPU_MAX,
                NVML_TEMP_THRESHOLD_SLOWDOWN,
                NVML_TEMP_THRESHOLD_SHUTDOWN,
            ] {
                let mut t = 0u32;
                // SAFETY: Valid NVML handle and writable `u32`.
                let status = unsafe { func(handle, threshold_kind, &raw mut t) };
                if let Some(v) = decode_nvml_field(status, t)
                    && v > 0
                {
                    #[allow(clippy::cast_precision_loss)]
                    return Some(v as f32);
                }
            }
            None
        }

        fn query_clock(&self, handle: NvmlDevice, clock_kind: u32) -> Option<u32> {
            let func = self.device_get_clock?;
            let mut mhz = 0u32;
            // SAFETY: Valid NVML handle and writable `u32`.
            let status = unsafe { func(handle, clock_kind, &raw mut mhz) };
            decode_nvml_field(status, mhz)
        }

        fn query_pcie_throughput(
            &self,
            handle: NvmlDevice,
            measure_pcie_speeds: bool,
        ) -> (Option<u64>, Option<u64>) {
            if !measure_pcie_speeds {
                return (None, None);
            }
            let Some(func) = self.device_get_pcie_throughput else {
                return (None, None);
            };
            let mut tx_val = 0u32;
            // SAFETY: Valid NVML handle and writable `u32`.
            let tx_status = unsafe { func(handle, NVML_PCIE_UTIL_TX_BYTES, &raw mut tx_val) };
            let mut rx_val = 0u32;
            // SAFETY: Valid NVML handle and writable `u32`.
            let rx_status = unsafe { func(handle, NVML_PCIE_UTIL_RX_BYTES, &raw mut rx_val) };
            (
                decode_nvml_field(tx_status, u64::from(tx_val)),
                decode_nvml_field(rx_status, u64::from(rx_val)),
            )
        }
    }

    impl Drop for NvmlLibrary {
        fn drop(&mut self) {
            if self.initialized {
                // SAFETY: Always call `nvmlShutdown()` before `FreeLibrary()` on exit.
                unsafe {
                    let _ = (self.shutdown)();
                }
                self.initialized = false;
            }
            if !self.module.is_null() {
                // SAFETY: `self.module` was returned by `LoadLibraryExW` and is uniquely owned.
                unsafe {
                    let _ = FreeLibrary(self.module);
                }
                self.module = ptr::null_mut();
            }
        }
    }

    fn query_enc_dec(func: Option<FnDeviceGetEncDec>, handle: NvmlDevice) -> Option<f32> {
        let f = func?;
        let mut util = 0u32;
        let mut period = 0u32;
        // SAFETY: Valid NVML handle and writable `u32` pointers.
        let status = unsafe { f(handle, &raw mut util, &raw mut period) };
        #[allow(clippy::cast_precision_loss)]
        decode_nvml_field(status, (util.min(100)) as f32)
    }

    fn resolve_sym<T: Copy>(module: HMODULE, name_nul: &[u8]) -> Option<T> {
        // SAFETY: `module` is valid and `name_nul` is NUL-terminated ASCII.
        let proc = unsafe { GetProcAddress(module, name_nul.as_ptr()) }?;
        let raw_ptr = proc as usize;
        if raw_ptr == 0 {
            return None;
        }
        // SAFETY: Caller specifies exact C function pointer type matching the NVML ABI.
        Some(unsafe { core::mem::transmute_copy(&raw_ptr) })
    }

    fn read_nvml_string(func: FnDeviceGetString, handle: NvmlDevice, cap: usize) -> Option<String> {
        let mut buf = vec![0u8; cap];
        let len = u32::try_from(buf.len()).ok()?;
        // SAFETY: `buf` provides `len` writable bytes for a NUL-terminated ASCII string.
        let status = unsafe { func(handle, buf.as_mut_ptr().cast::<c_char>(), len) };
        if status != NVML_SUCCESS {
            return None;
        }
        let nul = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        let text = String::from_utf8_lossy(&buf[..nul]).trim().to_owned();
        if text.is_empty() { None } else { Some(text) }
    }
}

#[cfg(not(windows))]
mod stub {
    use super::RawNvmlDevice;

    #[derive(Debug)]
    pub struct NvmlLibrary;

    impl NvmlLibrary {
        pub fn load() -> Option<Self> {
            None
        }

        pub fn load_named(_dll_name: &str) -> Option<Self> {
            None
        }

        pub fn query_devices(&self, _measure_pcie_speeds: bool) -> Vec<RawNvmlDevice> {
            Vec::new()
        }
    }
}

#[cfg(not(windows))]
pub use stub::NvmlLibrary;
#[cfg(windows)]
pub use win::NvmlLibrary;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_nvml_not_supported_to_none_and_decodes_pstates_and_pci_ids() {
        assert_eq!(decode_nvml_field(NVML_SUCCESS, 55u32), Some(55));
        assert_eq!(decode_nvml_field(NVML_ERROR_NOT_SUPPORTED, 55u32), None);
        assert_eq!(decode_nvml_pstate(0), Some("P0".to_owned()));
        assert_eq!(decode_nvml_pstate(8), Some("P8".to_owned()));
        assert_eq!(decode_nvml_pstate(32), None);
        assert_eq!(decode_nvml_pci_ids(0x2B85_10DE), (0x10DE, 0x2B85));
        assert_eq!(format_pci_bdf(0, 1, 0), "0000:01:00.0");
        assert!((milliwatts_to_watts(326_900) - 326.9).abs() < 0.01);
    }

    #[test]
    fn missing_nvml_dll_returns_none_silently() {
        assert!(NvmlLibrary::load_named("btop_nonexistent_nvml_probe.dll").is_none());
    }

    #[cfg(windows)]
    #[test]
    fn live_windows_nvml_probe_handles_present_or_absent_dll_cleanly() {
        let Some(nvml) = NvmlLibrary::load() else {
            // Non-NVIDIA host or CI VM without nvml.dll: silent None fallback is expected.
            return;
        };
        let devices = nvml.query_devices(true);
        for dev in &devices {
            assert_ne!(dev.name, "");
            if let Some(temp) = dev.temp_celsius {
                assert!((0.0..=140.0).contains(&temp), "unexpected GPU temp: {temp}");
            }
            if let Some(power) = dev.power_watts {
                assert!(power >= 0.0, "negative GPU power: {power}");
            }
        }
        // Dropping `nvml` exercises `nvmlShutdown()` + `FreeLibrary()`.
        drop(nvml);
    }
}
