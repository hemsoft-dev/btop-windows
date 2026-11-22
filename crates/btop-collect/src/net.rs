//! Windows 11 64-bit IP Helper (`GetIfTable2`, `GetIfEntry2`, `GetAdaptersAddresses`)
//! network throughput collector keyed by `NET_LUID`.

use std::collections::HashMap;
use std::time::Instant;

use btop_model::NetInterfaceSnapshot;
use btop_sys::{RawNetInterface, enumerate_hardware_interfaces, refresh_interface_entry};

use crate::CollectError;
use crate::math::clamped_rate_per_sec;

/// Computes non-negative `(rx_bytes_per_sec, tx_bytes_per_sec)` from two consecutive
/// 64-bit octet readings, clamping counter rollbacks or adapter resets to `0`.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn compute_net_rates(
    prev_rx: u64,
    prev_tx: u64,
    curr_rx: u64,
    curr_tx: u64,
    elapsed_ms: u64,
) -> (u64, u64) {
    let rx = clamped_rate_per_sec(prev_rx, curr_rx, elapsed_ms).round() as u64;
    let tx = clamped_rate_per_sec(prev_tx, curr_tx, elapsed_ms).round() as u64;
    (rx, tx)
}

#[derive(Debug, Clone, Copy)]
struct PrevCounters {
    in_octets: u64,
    out_octets: u64,
    sampled_at: Instant,
}

/// Live Windows 11 network interface collector keyed by `NET_LUID`.
///
/// Enumerates hardware interfaces via `GetIfTable2` + `GetAdaptersAddresses`, then uses
/// lightweight `GetIfEntry2` refreshes between periodic full table scans.
#[derive(Debug)]
pub struct NetCollector {
    cached_ifaces: Vec<RawNetInterface>,
    prev: HashMap<u64, PrevCounters>,
    ticks_since_enum: u32,
}

impl Default for NetCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl NetCollector {
    const REENUM_INTERVAL_TICKS: u32 = 10;

    pub fn new() -> Self {
        Self {
            cached_ifaces: Vec::new(),
            prev: HashMap::new(),
            ticks_since_enum: Self::REENUM_INTERVAL_TICKS,
        }
    }

    /// Samples all hardware network interfaces and returns 64-bit rates and totals.
    pub fn sample(&mut self) -> Result<Vec<NetInterfaceSnapshot>, CollectError> {
        let now = Instant::now();

        if self.cached_ifaces.is_empty() || self.ticks_since_enum >= Self::REENUM_INTERVAL_TICKS {
            self.cached_ifaces = enumerate_hardware_interfaces().map_err(CollectError::Platform)?;
            self.ticks_since_enum = 0;
        } else {
            self.ticks_since_enum += 1;
            let mut stale = false;
            for iface in &mut self.cached_ifaces {
                if let Ok(entry) = refresh_interface_entry(iface.luid) {
                    iface.is_up = entry.is_up;
                    iface.in_octets = entry.in_octets;
                    iface.out_octets = entry.out_octets;
                    iface.link_speed_bps = entry.link_speed_bps;
                } else {
                    stale = true;
                }
            }
            if stale {
                self.cached_ifaces =
                    enumerate_hardware_interfaces().map_err(CollectError::Platform)?;
                self.ticks_since_enum = 0;
            }
        }

        let mut snapshots = Vec::with_capacity(self.cached_ifaces.len());
        let mut next_prev = HashMap::with_capacity(self.cached_ifaces.len());

        for iface in &self.cached_ifaces {
            let (rx_bps, tx_bps) = if let Some(prev) = self.prev.get(&iface.luid) {
                let elapsed_ms =
                    u64::try_from(now.saturating_duration_since(prev.sampled_at).as_millis())
                        .unwrap_or(u64::MAX);
                compute_net_rates(
                    prev.in_octets,
                    prev.out_octets,
                    iface.in_octets,
                    iface.out_octets,
                    elapsed_ms,
                )
            } else {
                (0, 0)
            };

            next_prev.insert(
                iface.luid,
                PrevCounters {
                    in_octets: iface.in_octets,
                    out_octets: iface.out_octets,
                    sampled_at: now,
                },
            );

            snapshots.push(NetInterfaceSnapshot {
                luid: iface.luid,
                name: iface.name.clone(),
                description: iface.description.clone(),
                is_up: iface.is_up,
                ipv4: iface.ipv4.clone(),
                ipv6: iface.ipv6.clone(),
                rx_bytes_per_sec: rx_bps,
                tx_bytes_per_sec: tx_bps,
                rx_total_bytes: iface.in_octets,
                tx_total_bytes: iface.out_octets,
                link_speed_bps: iface.link_speed_bps,
            });
        }

        self.prev = next_prev;
        Ok(snapshots)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_net_rates_clamps_counter_rollbacks_and_zero_elapsed() {
        // Normal 500ms tick: +1 MiB rx -> 2 MiB/s, +256 KiB tx -> 512 KiB/s
        let (rx, tx) = compute_net_rates(10_000, 5_000, 10_000 + 1_048_576, 5_000 + 262_144, 500);
        assert_eq!(rx, 2_097_152);
        assert_eq!(tx, 524_288);

        // Counter rollback / adapter reset clamps cleanly to 0
        let (rrx, rtx) = compute_net_rates(50_000_000, 20_000_000, 1024, 512, 1000);
        assert_eq!(rrx, 0);
        assert_eq!(rtx, 0);

        // Zero elapsed time clamps to 0 without division by zero
        assert_eq!(compute_net_rates(0, 0, 10_000, 10_000, 0), (0, 0));
    }

    #[cfg(windows)]
    #[test]
    fn live_windows_net_collector_enumerates_hardware_interfaces_and_refreshes_by_luid() {
        let mut collector = NetCollector::new();
        let first = collector.sample().expect("first network sample");
        assert!(
            !first.is_empty(),
            "expected at least one hardware network interface on Windows 11"
        );
        assert!(first.iter().all(|i| i.luid != 0 && !i.name.is_empty()));

        std::thread::sleep(std::time::Duration::from_millis(50));
        let second = collector
            .sample()
            .expect("second network sample via GetIfEntry2");
        assert_eq!(second.len(), first.len());
    }
}
