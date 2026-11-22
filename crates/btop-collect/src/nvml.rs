//! Optional non-blocking NVIDIA NVML (`nvml.dll`) background worker, adapter matcher,
//! and snapshot enrichment with timeout-based `DataState::Stale` degradation.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use btop_model::{DataState, GpuSnapshot};
use btop_sys::{NvmlLibrary, RawGpuAdapter, RawNvmlDevice};

const NVIDIA_VENDOR_ID: u32 = 0x10DE;

/// Background NVML polling cadence.
pub const NVML_POLL_INTERVAL: Duration = Duration::from_millis(1000);

/// Age threshold after which an unrefreshed NVML sample degrades to `DataState::Stale`.
pub const NVML_STALE_TIMEOUT: Duration = Duration::from_millis(5000);

/// Non-blocking snapshot of the latest NVML enrichment state.
#[derive(Debug, Clone, PartialEq)]
pub enum NvmlSnapshotState {
    /// Fresh NVML telemetry updated within the configured timeout.
    Fresh(Vec<RawNvmlDevice>),
    /// Driver query has not completed within the stale threshold; retains last known values.
    Stale {
        devices: Vec<RawNvmlDevice>,
        age_ms: u64,
    },
    /// `nvml.dll` is absent, unsupported, or not yet initialized; fall back silently to WDDM.
    Unavailable,
}

#[derive(Debug, Default)]
struct NvmlShared {
    latest: Option<(Vec<RawNvmlDevice>, Instant)>,
    refresh: bool,
    stop: bool,
    exited: bool,
}

/// Non-blocking background worker owning `NvmlLibrary` so slow NVIDIA driver calls never
/// stall the core telemetry sampler or UI thread.
pub struct NvmlWorker {
    state: Arc<(Mutex<NvmlShared>, Condvar)>,
    measure_pcie: Arc<AtomicBool>,
    stale_timeout: Duration,
    handle: Option<JoinHandle<()>>,
}

impl fmt::Debug for NvmlWorker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NvmlWorker")
            .field("measure_pcie", &self.measure_pcie.load(Ordering::Relaxed))
            .field("stale_timeout", &self.stale_timeout)
            .field("running", &self.handle.is_some())
            .finish_non_exhaustive()
    }
}

impl NvmlWorker {
    /// Spawns the optional `nvml.dll` background worker. If `nvml.dll` is absent in `System32`,
    /// the worker exits immediately and silently without error.
    pub fn spawn(measure_pcie_speeds: bool) -> Self {
        Self::spawn_with_driver(
            measure_pcie_speeds,
            NVML_POLL_INTERVAL,
            NVML_STALE_TIMEOUT,
            NvmlLibrary::load,
            NvmlLibrary::query_devices,
        )
    }

    /// Testable constructor accepting custom loader/poller closures to verify missing-DLL
    /// fallback, `NOT_SUPPORTED` fields, and non-blocking stale timeout behavior.
    pub fn spawn_with_driver<T, L, P>(
        measure_pcie_speeds: bool,
        poll_interval: Duration,
        stale_timeout: Duration,
        loader: L,
        mut poller: P,
    ) -> Self
    where
        T: Send + 'static,
        L: FnOnce() -> Option<T> + Send + 'static,
        P: FnMut(&T, bool) -> Vec<RawNvmlDevice> + Send + 'static,
    {
        let state = Arc::new((Mutex::new(NvmlShared::default()), Condvar::new()));
        let measure_pcie = Arc::new(AtomicBool::new(measure_pcie_speeds));

        let thread_state = Arc::clone(&state);
        let thread_pcie = Arc::clone(&measure_pcie);
        let handle = thread::Builder::new()
            .name("btop-nvml".to_owned())
            .spawn(move || {
                let Some(driver) = loader() else {
                    mark_exited(&thread_state);
                    return;
                };

                loop {
                    let pcie_enabled = thread_pcie.load(Ordering::Relaxed);
                    let devices = poller(&driver, pcie_enabled);
                    let now = Instant::now();

                    let (lock, cvar) = &*thread_state;
                    let mut guard = lock
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if guard.stop {
                        break;
                    }
                    if !devices.is_empty() {
                        guard.latest = Some((devices, now));
                    }
                    guard.refresh = false;
                    let (guard, _) = cvar
                        .wait_timeout_while(guard, poll_interval, |s| !s.stop && !s.refresh)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if guard.stop {
                        break;
                    }
                }

                // Dropping `driver` calls `nvmlShutdown()` before `FreeLibrary()`
                // and must complete before marking `exited = true`.
                drop(driver);
                mark_exited(&thread_state);
            })
            .ok();

        Self {
            state,
            measure_pcie,
            stale_timeout,
            handle,
        }
    }

    /// Updates `nvml_measure_pcie_speeds` live without restarting the worker and wakes it
    /// so the updated `PCIe` preference takes effect promptly.
    pub fn set_measure_pcie_speeds(&self, enabled: bool) {
        self.measure_pcie.store(enabled, Ordering::Relaxed);
        let (lock, cvar) = &*self.state;
        let mut guard = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.refresh = true;
        cvar.notify_all();
    }

    /// Reads the latest cached NVML sample without blocking on driver queries.
    pub fn latest(&self) -> NvmlSnapshotState {
        let (lock, _) = &*self.state;
        let Ok(guard) = lock.try_lock() else {
            return NvmlSnapshotState::Unavailable;
        };
        let Some((ref devices, updated_at)) = guard.latest else {
            return NvmlSnapshotState::Unavailable;
        };
        let age = Instant::now().saturating_duration_since(updated_at);
        if age > self.stale_timeout {
            NvmlSnapshotState::Stale {
                devices: devices.clone(),
                age_ms: u64::try_from(age.as_millis()).unwrap_or(u64::MAX),
            }
        } else {
            NvmlSnapshotState::Fresh(devices.clone())
        }
    }
}

impl Drop for NvmlWorker {
    fn drop(&mut self) {
        let (lock, cvar) = &*self.state;
        {
            let mut guard = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.stop = true;
            cvar.notify_all();
            // Wait briefly for clean `nvmlShutdown()` + `FreeLibrary()`; if a wedged driver
            // call is stalled, do not hang process exit indefinitely.
            let _ = cvar.wait_timeout_while(guard, Duration::from_millis(300), |s| !s.exited);
        }
        if let Some(handle) = self.handle.take()
            && lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .exited
        {
            let _ = handle.join();
        }
    }
}

fn mark_exited(state: &(Mutex<NvmlShared>, Condvar)) {
    let (lock, cvar) = state;
    let mut guard = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.exited = true;
    cvar.notify_all();
}

/// Normalizes a GPU name for resilient DXGI <-> NVML matching.
fn normalize_gpu_name(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

/// Normalizes PCI BDF strings (`00000000:01:00.0` and `0000:01:00.0`) to a canonical lowercase form.
pub fn normalize_pci_bdf(bdf: &str) -> String {
    let trimmed = bdf.trim().to_ascii_lowercase();
    if let Some((domain, rest)) = trimmed.split_once(':')
        && let Ok(dom_val) = u32::from_str_radix(domain, 16)
    {
        return format!("{dom_val:04x}:{rest}");
    }
    trimmed
}

/// Matches NVML devices 1-to-1 onto DXGI NVIDIA hardware adapters (`vendor_id == 0x10DE`),
/// keyed by DXGI `AdapterLuid`.
///
/// Priority order:
/// 1. Exact `(vendor_id, device_id)` + normalized name match
/// 2. Exact normalized name match (or substring fallback when NVIDIA adds/omits prefix)
/// 3. Exact `(vendor_id, device_id)` match
/// 4. Final 1-to-1 fallback when exactly one NVIDIA adapter and one NVML device remain
pub fn match_nvml_devices(
    adapters: &[RawGpuAdapter],
    devices: &[RawNvmlDevice],
) -> BTreeMap<u64, RawNvmlDevice> {
    let nvidia_adapters: Vec<&RawGpuAdapter> = adapters
        .iter()
        .filter(|a| !a.is_software && a.vendor_id == NVIDIA_VENDOR_ID)
        .collect();
    if nvidia_adapters.is_empty() || devices.is_empty() {
        return BTreeMap::new();
    }

    let mut matched: BTreeMap<u64, RawNvmlDevice> = BTreeMap::new();
    let mut claimed = vec![false; devices.len()];

    // Pass 1: exact (vendor_id, device_id) + normalized name match.
    for adapter in &nvidia_adapters {
        let a_name = normalize_gpu_name(&adapter.name);
        if let Some((idx, dev)) = devices.iter().enumerate().find(|(i, d)| {
            !claimed[*i]
                && d.pci_vendor_id == Some(adapter.vendor_id)
                && d.pci_device_id == Some(adapter.device_id)
                && normalize_gpu_name(&d.name) == a_name
        }) {
            claimed[idx] = true;
            matched.insert(adapter.luid, dev.clone());
        }
    }

    // Pass 2: exact or substring normalized name match.
    for adapter in &nvidia_adapters {
        if matched.contains_key(&adapter.luid) {
            continue;
        }
        let a_name = normalize_gpu_name(&adapter.name);
        if let Some((idx, dev)) = devices.iter().enumerate().find(|(i, d)| {
            if claimed[*i] {
                return false;
            }
            let d_name = normalize_gpu_name(&d.name);
            d_name == a_name || d_name.contains(&a_name) || a_name.contains(&d_name)
        }) {
            claimed[idx] = true;
            matched.insert(adapter.luid, dev.clone());
        }
    }

    // Pass 3: exact (vendor_id, device_id) match.
    for adapter in &nvidia_adapters {
        if matched.contains_key(&adapter.luid) {
            continue;
        }
        if let Some((idx, dev)) = devices.iter().enumerate().find(|(i, d)| {
            !claimed[*i]
                && d.pci_vendor_id == Some(adapter.vendor_id)
                && d.pci_device_id == Some(adapter.device_id)
        }) {
            claimed[idx] = true;
            matched.insert(adapter.luid, dev.clone());
        }
    }

    // Pass 4: unambiguous 1-to-1 fallback when one NVIDIA adapter and one NVML device remain.
    let remaining_adapters: Vec<&RawGpuAdapter> = nvidia_adapters
        .into_iter()
        .filter(|a| !matched.contains_key(&a.luid))
        .collect();
    let remaining_devices: Vec<(usize, &RawNvmlDevice)> = devices
        .iter()
        .enumerate()
        .filter(|(i, _)| !claimed[*i])
        .collect();
    if remaining_adapters.len() == 1 && remaining_devices.len() == 1 {
        matched.insert(remaining_adapters[0].luid, remaining_devices[0].1.clone());
    }

    matched
}

/// Applies optional NVML enrichment onto baseline WDDM GPU snapshots.
///
/// When `nvml_state` is [`NvmlSnapshotState::Unavailable`], baseline snapshots are untouched.
/// When `nvml_state` is [`NvmlSnapshotState::Stale`], matched NVIDIA adapters retain their last
/// known NVML metrics and transition to `DataState::Stale { value, age_ms }`.
pub fn apply_nvml_enrichment(
    snapshots: &mut [DataState<GpuSnapshot>],
    adapters: &[RawGpuAdapter],
    nvml_state: &NvmlSnapshotState,
) {
    let (devices, stale_age_ms) = match nvml_state {
        NvmlSnapshotState::Fresh(devs) => (devs.as_slice(), None),
        NvmlSnapshotState::Stale { devices, age_ms } => (devices.as_slice(), Some(*age_ms)),
        NvmlSnapshotState::Unavailable => return,
    };

    let by_luid = match_nvml_devices(adapters, devices);
    if by_luid.is_empty() {
        return;
    }

    for slot in snapshots.iter_mut() {
        let Some(luid) = (match slot {
            DataState::Available(g) | DataState::Stale { value: g, .. } => g.luid,
            _ => None,
        }) else {
            continue;
        };
        let Some(nvml) = by_luid.get(&luid) else {
            continue;
        };

        let apply_fields = |gpu: &mut GpuSnapshot| {
            gpu.pci_bdf.clone_from(&nvml.pci_bdf);
            gpu.temp_celsius = nvml.temp_celsius;
            gpu.temp_max_celsius = nvml.temp_max_celsius;
            gpu.core_clock_mhz = nvml.core_clock_mhz;
            gpu.mem_clock_mhz = nvml.mem_clock_mhz;
            gpu.power_watts = nvml.power_watts;
            gpu.power_limit_watts = nvml.power_limit_watts;
            gpu.pstate.clone_from(&nvml.pstate);
            gpu.encoder_pct = nvml.encoder_pct;
            gpu.decoder_pct = nvml.decoder_pct;
            gpu.pcie_tx_kbs = nvml.pcie_tx_kbs;
            gpu.pcie_rx_kbs = nvml.pcie_rx_kbs;
        };

        match slot {
            DataState::Available(gpu) => {
                apply_fields(gpu);
                if let Some(age_ms) = stale_age_ms {
                    let enriched = gpu.clone();
                    *slot = DataState::Stale {
                        value: enriched,
                        age_ms,
                    };
                }
            }
            DataState::Stale { value, age_ms } => {
                apply_fields(value);
                if let Some(nvml_age) = stale_age_ms {
                    *age_ms = (*age_ms).max(nvml_age);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use btop_sys::{
        NVML_ERROR_NOT_SUPPORTED, NVML_SUCCESS, decode_nvml_field, encode_luid, vendor_name_from_id,
    };

    use super::*;
    use crate::build_gpu_snapshots;

    fn sample_dxgi_adapters() -> Vec<RawGpuAdapter> {
        vec![
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
                name: "AMD Radeon(TM) Graphics".to_owned(),
                vendor: vendor_name_from_id(0x1002),
                vendor_id: 0x1002,
                device_id: 0x13C0,
                luid: encode_luid(0, 0x0001_E8E2),
                dedicated_vram_total_bytes: 2 * 1024 * 1024 * 1024,
                shared_vram_total_bytes: 32 * 1024 * 1024 * 1024,
                is_software: false,
            },
        ]
    }

    fn sample_nvml_device() -> RawNvmlDevice {
        RawNvmlDevice {
            index: 0,
            name: "NVIDIA GeForce RTX 5090".to_owned(),
            uuid: Some("GPU-11111111-2222-3333-4444-555555555555".to_owned()),
            pci_bdf: Some("0000:01:00.0".to_owned()),
            pci_vendor_id: Some(0x10DE),
            pci_device_id: Some(0x2B85),
            temp_celsius: decode_nvml_field(NVML_SUCCESS, 47.0),
            temp_max_celsius: decode_nvml_field(NVML_SUCCESS, 90.0),
            core_clock_mhz: decode_nvml_field(NVML_SUCCESS, 2782),
            mem_clock_mhz: decode_nvml_field(NVML_SUCCESS, 14001),
            power_watts: decode_nvml_field(NVML_SUCCESS, 326.9),
            power_limit_watts: decode_nvml_field(NVML_SUCCESS, 600.0),
            pstate: Some("P0".to_owned()),
            encoder_pct: decode_nvml_field(NVML_ERROR_NOT_SUPPORTED, 0.0),
            decoder_pct: decode_nvml_field(NVML_SUCCESS, 8.0),
            pcie_tx_kbs: decode_nvml_field(NVML_SUCCESS, 120_000),
            pcie_rx_kbs: decode_nvml_field(NVML_ERROR_NOT_SUPPORTED, 0),
        }
    }

    #[test]
    fn missing_nvml_dll_worker_falls_back_silently_to_wddm_baseline() {
        let worker = NvmlWorker::spawn_with_driver(
            true,
            Duration::from_millis(20),
            Duration::from_millis(100),
            || Option::<()>::None,
            |(), _| Vec::new(),
        );
        thread::sleep(Duration::from_millis(30));
        assert_eq!(worker.latest(), NvmlSnapshotState::Unavailable);

        let adapters = sample_dxgi_adapters();
        let mut snaps = build_gpu_snapshots(&adapters, None, true);
        let baseline = snaps.clone();
        apply_nvml_enrichment(&mut snaps, &adapters, &worker.latest());
        assert_eq!(snaps, baseline);
    }

    #[test]
    fn matches_nvidia_adapter_preserves_not_supported_none_and_leaves_amd_untouched() {
        let adapters = sample_dxgi_adapters();
        let nvml_dev = sample_nvml_device();
        let matched = match_nvml_devices(&adapters, std::slice::from_ref(&nvml_dev));
        assert_eq!(matched.len(), 1);
        assert!(matched.contains_key(&adapters[0].luid));
        assert!(!matched.contains_key(&adapters[1].luid));

        let mut snaps = build_gpu_snapshots(&adapters, None, true);
        apply_nvml_enrichment(
            &mut snaps,
            &adapters,
            &NvmlSnapshotState::Fresh(vec![nvml_dev]),
        );

        let DataState::Available(ref nv) = snaps[0] else {
            panic!("expected NVIDIA Available");
        };
        assert_eq!(nv.pci_bdf.as_deref(), Some("0000:01:00.0"));
        assert_eq!(nv.temp_celsius, Some(47.0));
        assert_eq!(nv.temp_max_celsius, Some(90.0));
        assert_eq!(nv.core_clock_mhz, Some(2782));
        assert_eq!(nv.mem_clock_mhz, Some(14001));
        assert_eq!(nv.pstate.as_deref(), Some("P0"));
        assert_eq!(nv.encoder_pct, None);
        assert_eq!(nv.decoder_pct, Some(8.0));
        assert_eq!(nv.pcie_tx_kbs, Some(120_000));
        assert_eq!(nv.pcie_rx_kbs, None);

        let DataState::Available(ref amd) = snaps[1] else {
            panic!("expected AMD Available");
        };
        assert_eq!(amd.temp_celsius, None);
        assert_eq!(amd.power_watts, None);
        assert_eq!(amd.pci_bdf, None);
    }

    #[test]
    fn slow_nvml_driver_does_not_block_and_degrades_to_stale_after_timeout() {
        let mut call_num = 0u32;
        let worker = NvmlWorker::spawn_with_driver(
            true,
            Duration::from_millis(15),
            Duration::from_millis(60),
            || Some(()),
            move |(), _| {
                call_num += 1;
                if call_num > 1 {
                    // Simulate a wedged driver call on the second poll.
                    thread::sleep(Duration::from_millis(250));
                }
                vec![sample_nvml_device()]
            },
        );

        // Wait for the initial fresh poll.
        let mut saw_fresh = false;
        for _ in 0..20 {
            if matches!(worker.latest(), NvmlSnapshotState::Fresh(_)) {
                saw_fresh = true;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(saw_fresh, "expected initial fresh NVML sample");

        // Wait past stale_timeout (60ms) while the second poll is blocked for 250ms;
        // `latest()` must return immediately with `Stale` rather than blocking!
        thread::sleep(Duration::from_millis(95));
        let start = Instant::now();
        let state = worker.latest();
        assert!(
            start.elapsed() < Duration::from_millis(25),
            "NVML latest() must never block on slow driver calls"
        );
        assert!(matches!(state, NvmlSnapshotState::Stale { .. }));

        let adapters = sample_dxgi_adapters();
        let mut snaps = build_gpu_snapshots(&adapters, None, true);
        apply_nvml_enrichment(&mut snaps, &adapters, &state);
        assert!(matches!(snaps[0], DataState::Stale { .. }));
        assert!(matches!(snaps[1], DataState::Available(_)));
    }

    #[test]
    fn normalizes_long_and_short_pci_bdf_strings() {
        assert_eq!(normalize_pci_bdf("00000000:01:00.0"), "0000:01:00.0");
        assert_eq!(normalize_pci_bdf("0000:0A:00.0"), "0000:0a:00.0");
    }
}
