//! Windows 11 x64 GPU baseline collector backed by DXGI (`IDXGIFactory6`, excluding software
//! adapters) and a persistent English WDDM PDH query (`\GPU Engine(*)` + `\GPU Adapter Memory(*)`),
//! implementing ADR-0002.

use std::time::{Duration, Instant};

use btop_model::{DataState, GpuSnapshot};
use btop_sys::{
    GpuPdhQuery, GpuPdhSample, MAX_GPUS, RawGpuAdapter, enumerate_hardware_gpus,
    filter_hardware_adapters,
};

const ADAPTER_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// Stateful DXGI + persistent WDDM PDH GPU telemetry collector.
#[derive(Debug)]
pub struct GpuCollector {
    adapters: Vec<RawGpuAdapter>,
    pdh: Option<GpuPdhQuery>,
    last_enum: Instant,
    enum_error: Option<String>,
}

impl Default for GpuCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl GpuCollector {
    /// Enumerates DXGI hardware adapters and opens the persistent English WDDM PDH query.
    pub fn new() -> Self {
        let (adapters, enum_error) = match enumerate_hardware_gpus() {
            Ok(list) => (list, None),
            Err(err) => (Vec::new(), Some(err)),
        };
        let pdh = if adapters.is_empty() {
            None
        } else {
            GpuPdhQuery::open()
        };
        Self {
            adapters,
            pdh,
            last_enum: Instant::now(),
            enum_error,
        }
    }

    /// Collects one GPU telemetry tick.
    ///
    /// On the first call, primes WDDM PDH rate counters and publishes `DataState::WarmingUp`
    /// per adapter (matching ADR-0002). Re-enumerates DXGI adapters if a new LUID appears in
    /// PDH or after 30 seconds.
    pub fn sample(&mut self) -> Vec<DataState<GpuSnapshot>> {
        let now = Instant::now();
        if now.saturating_duration_since(self.last_enum) >= ADAPTER_REFRESH_INTERVAL {
            self.refresh_adapters(now);
        }

        if self.adapters.is_empty() {
            if let Some(ref err) = self.enum_error {
                return vec![DataState::Unavailable(err.clone())];
            }
            return Vec::new();
        }

        if self.pdh.is_none() {
            self.pdh = GpuPdhQuery::open();
        }

        let Some(ref mut pdh) = self.pdh else {
            return build_gpu_snapshots(&self.adapters, None, true);
        };

        match pdh.sample() {
            Ok(None) => build_gpu_snapshots(&self.adapters, None, false),
            Ok(Some(sample)) => {
                // Re-enumerate DXGI adapters immediately if WDDM reports a new hardware LUID.
                let has_unknown_luid = sample
                    .by_luid
                    .keys()
                    .any(|luid| !self.adapters.iter().any(|a| a.luid == *luid));
                if has_unknown_luid
                    && now.saturating_duration_since(self.last_enum) >= Duration::from_secs(5)
                {
                    self.refresh_adapters(now);
                }
                build_gpu_snapshots(&self.adapters, Some(&sample), true)
            }
            Err(_) => {
                // Drop and reopen on next tick; still expose DXGI adapter identity cleanly.
                self.pdh = None;
                build_gpu_snapshots(&self.adapters, None, true)
            }
        }
    }

    fn refresh_adapters(&mut self, now: Instant) {
        self.last_enum = now;
        match enumerate_hardware_gpus() {
            Ok(list) => {
                self.adapters = list;
                self.enum_error = None;
            }
            Err(err) => {
                if self.adapters.is_empty() {
                    self.enum_error = Some(err);
                }
            }
        }
    }
}

/// Pure transformation joining DXGI hardware adapters with WDDM PDH metrics by LUID.
///
/// Filters out software adapters defensively, caps at [`MAX_GPUS`], leaves vendor-specific
/// optional fields (`core_clock_mhz`, `temp_celsius`, `power_watts`, etc.) as `None`, and
/// returns `DataState::WarmingUp` for each adapter while `warmed_up == false`.
pub fn build_gpu_snapshots(
    raw_adapters: &[RawGpuAdapter],
    pdh: Option<&GpuPdhSample>,
    warmed_up: bool,
) -> Vec<DataState<GpuSnapshot>> {
    let hardware = filter_hardware_adapters(raw_adapters.to_vec());
    if hardware.is_empty() {
        return Vec::new();
    }
    if !warmed_up {
        return vec![DataState::WarmingUp; hardware.len().min(MAX_GPUS)];
    }

    hardware
        .into_iter()
        .take(MAX_GPUS)
        .enumerate()
        .map(|(idx, adapter)| {
            let metrics = pdh.and_then(|s| s.by_luid.get(&adapter.luid));
            let vram_total = (adapter.dedicated_vram_total_bytes > 0)
                .then_some(adapter.dedicated_vram_total_bytes);
            let shared_total =
                (adapter.shared_vram_total_bytes > 0).then_some(adapter.shared_vram_total_bytes);

            DataState::Available(GpuSnapshot {
                index: u8::try_from(idx).unwrap_or(0),
                name: adapter.name,
                vendor: adapter.vendor,
                pci_bdf: None,
                utilization_pct: metrics.and_then(|m| m.busiest_engine_pct),
                vram_used_bytes: metrics.and_then(|m| m.dedicated_used_bytes),
                vram_total_bytes: vram_total,
                shared_vram_used_bytes: metrics.and_then(|m| m.shared_used_bytes),
                shared_vram_total_bytes: shared_total,
                luid: Some(adapter.luid),
                core_clock_mhz: None,
                mem_clock_mhz: None,
                temp_celsius: None,
                power_watts: None,
                power_limit_watts: None,
                pcie_tx_kbs: None,
                pcie_rx_kbs: None,
                encoder_pct: None,
                decoder_pct: None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use btop_sys::{GpuPdhMetrics, encode_luid, vendor_name_from_id};

    use super::*;

    fn sample_adapters() -> Vec<RawGpuAdapter> {
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

    #[test]
    fn builds_warming_up_and_joined_dual_gpu_snapshots_without_software_adapter() {
        let adapters = sample_adapters();

        let warming = build_gpu_snapshots(&adapters, None, false);
        assert_eq!(warming.len(), 2);
        assert!(warming.iter().all(|g| matches!(g, DataState::WarmingUp)));

        let mut by_luid = BTreeMap::new();
        by_luid.insert(
            encode_luid(0, 0x0001_89EF),
            GpuPdhMetrics {
                busiest_engine_pct: Some(67.5),
                dedicated_used_bytes: Some(12 * 1024 * 1024 * 1024),
                shared_used_bytes: Some(512 * 1024 * 1024),
            },
        );
        by_luid.insert(
            encode_luid(0, 0x0001_E8E2),
            GpuPdhMetrics {
                busiest_engine_pct: Some(4.2),
                dedicated_used_bytes: Some(64 * 1024 * 1024),
                shared_used_bytes: Some(96 * 1024 * 1024),
            },
        );
        let pdh = GpuPdhSample { by_luid };

        let ready = build_gpu_snapshots(&adapters, Some(&pdh), true);
        assert_eq!(ready.len(), 2);

        let DataState::Available(ref gpu0) = ready[0] else {
            panic!("expected gpu0 available");
        };
        assert_eq!(gpu0.index, 0);
        assert_eq!(gpu0.name, "NVIDIA GeForce RTX 5090");
        assert_eq!(gpu0.utilization_pct, Some(67.5));
        assert_eq!(gpu0.vram_used_bytes, Some(12 * 1024 * 1024 * 1024));
        assert_eq!(gpu0.shared_vram_used_bytes, Some(512 * 1024 * 1024));
        assert_eq!(gpu0.temp_celsius, None);
        assert_eq!(gpu0.core_clock_mhz, None);
        assert_eq!(gpu0.power_watts, None);

        let DataState::Available(ref gpu1) = ready[1] else {
            panic!("expected gpu1 available");
        };
        assert_eq!(gpu1.index, 1);
        assert_eq!(gpu1.name, "AMD Radeon(TM) Graphics");
        assert_eq!(gpu1.utilization_pct, Some(4.2));
    }

    #[cfg(windows)]
    #[test]
    fn live_windows_gpu_collector_warms_up_then_reports_cleanly() {
        let mut collector = GpuCollector::new();
        let first = collector.sample();
        if first.is_empty() {
            // Headless Windows CI runners may expose only Microsoft Basic Render Driver.
            return;
        }
        assert!(
            first.iter().all(|g| matches!(
                g,
                DataState::WarmingUp | DataState::Available(_) | DataState::Unavailable(_)
            )),
            "first GPU sample should be WarmingUp or valid state: {first:?}"
        );
        std::thread::sleep(Duration::from_millis(80));
        let second = collector.sample();
        assert_eq!(second.len(), first.len());
        for state in &second {
            if let DataState::Available(gpu) = state {
                assert_ne!(gpu.name, "");
                assert!(!gpu.name.contains("Microsoft Basic Render Driver"));
                if let Some(pct) = gpu.utilization_pct {
                    assert!((0.0..=100.0).contains(&pct));
                }
            }
        }
    }
}
