//! Windows 11 memory, commit charge, pagefile, and English PDH cache-list collector.

use btop_model::MemSnapshot;
use btop_sys::{
    MemPdhQuery, RawMemCacheSample, RawMemoryStatus, RawPagefileTotals, RawPerformanceInfo,
    query_memory_status, query_pagefile_totals, query_performance_info,
};

use crate::CollectError;

const DEFAULT_PAGE_SIZE_BYTES: u64 = 4096;

/// Converts pagefile page counts from `EnumPageFilesW` into `(used_bytes, total_bytes)`.
///
/// Uses saturating arithmetic and clamps `used_bytes <= total_bytes`.
pub fn pagefile_pages_to_bytes(
    used_pages: u64,
    total_pages: u64,
    page_size_bytes: u64,
) -> (u64, u64) {
    let total_bytes = total_pages.saturating_mul(page_size_bytes);
    let used_bytes = used_pages.saturating_mul(page_size_bytes).min(total_bytes);
    (used_bytes, total_bytes)
}

/// Computes a consistent [`MemSnapshot`] from raw Windows 11 memory, commit, pagefile, and
/// PDH cache-list readings.
///
/// Invariants:
/// - `used_bytes + available_bytes == total_bytes`
/// - `cached_bytes = (standby_core + standby_normal + standby_reserve + modified).min(total_bytes)`
/// - `free_bytes = free_and_zero.min(available_bytes)`
/// - Standby is NOT double-counted inside `used_bytes`; Windows `available_bytes` already
///   includes Standby + Free/Zero pages.
pub fn compute_mem_snapshot(
    status: RawMemoryStatus,
    perf: Option<RawPerformanceInfo>,
    pagefiles: Option<RawPagefileTotals>,
    cache: Option<RawMemCacheSample>,
) -> MemSnapshot {
    let total_bytes = status.total_phys_bytes;
    let available_bytes = status.avail_phys_bytes.min(total_bytes);
    let used_bytes = total_bytes.saturating_sub(available_bytes);

    let (cached_bytes, free_bytes) = if let Some(c) = cache {
        let standby = c
            .standby_core_bytes
            .saturating_add(c.standby_normal_bytes)
            .saturating_add(c.standby_reserve_bytes);
        let cached = standby.saturating_add(c.modified_bytes).min(total_bytes);
        let free = c.free_and_zero_bytes.min(available_bytes);
        (cached, free)
    } else {
        (0, 0)
    };

    let page_size = perf
        .map(|p| p.page_size_bytes)
        .filter(|&ps| ps > 0)
        .unwrap_or(DEFAULT_PAGE_SIZE_BYTES);

    let (commit_used_bytes, commit_limit_bytes) = if let Some(p) = perf {
        (
            p.commit_total_pages.saturating_mul(page_size),
            p.commit_limit_pages.saturating_mul(page_size),
        )
    } else {
        (0, 0)
    };

    let (pagefile_used_bytes, pagefile_total_bytes) = if let Some(pf) = pagefiles {
        pagefile_pages_to_bytes(pf.used_pages, pf.total_pages, page_size)
    } else {
        (0, 0)
    };

    MemSnapshot {
        total_bytes,
        used_bytes,
        available_bytes,
        cached_bytes,
        free_bytes,
        commit_used_bytes,
        commit_limit_bytes,
        pagefile_used_bytes,
        pagefile_total_bytes,
        compressed_bytes: None,
    }
}

/// Live Windows 11 memory collector combining `GlobalMemoryStatusEx`, `GetPerformanceInfo`,
/// `EnumPageFilesW`, and a persistent English PDH query for cache list breakdown.
#[derive(Debug)]
pub struct MemoryCollector {
    pdh: Option<MemPdhQuery>,
}

impl Default for MemoryCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryCollector {
    /// Initializes the memory collector and opens the shared English PDH cache query.
    pub fn new() -> Self {
        Self {
            pdh: MemPdhQuery::open(),
        }
    }

    /// Samples current physical memory, cache breakdown, commit charge, and pagefile usage.
    pub fn sample(&mut self) -> Result<MemSnapshot, CollectError> {
        let status = query_memory_status().map_err(CollectError::Platform)?;
        let perf = query_performance_info().ok();
        let pagefiles = query_pagefile_totals().ok();
        let cache = self.pdh.as_mut().and_then(MemPdhQuery::sample);
        Ok(compute_mem_snapshot(status, perf, pagefiles, cache))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;

    #[test]
    fn used_plus_available_always_equals_total_and_cache_breakdown_is_exact() {
        let snap = compute_mem_snapshot(
            RawMemoryStatus {
                total_phys_bytes: 64 * GIB,
                avail_phys_bytes: 38 * GIB,
            },
            Some(RawPerformanceInfo {
                commit_total_pages: 8_126_464,  // 31 GiB in 4 KiB pages
                commit_limit_pages: 18_874_368, // 72 GiB in 4 KiB pages
                page_size_bytes: 4096,
            }),
            Some(RawPagefileTotals {
                used_pages: 262_144,    // 1 GiB
                total_pages: 2_097_152, // 8 GiB
            }),
            Some(RawMemCacheSample {
                standby_core_bytes: 2 * GIB,
                standby_normal_bytes: 7 * GIB,
                standby_reserve_bytes: GIB,
                modified_bytes: GIB,
                free_and_zero_bytes: 28 * GIB,
            }),
        );

        assert_eq!(snap.total_bytes, 64 * GIB);
        assert_eq!(snap.used_bytes, 26 * GIB);
        assert_eq!(snap.available_bytes, 38 * GIB);
        assert_eq!(snap.used_bytes + snap.available_bytes, snap.total_bytes);
        assert_eq!(snap.cached_bytes, 11 * GIB);
        assert_eq!(snap.free_bytes, 28 * GIB);
        assert_eq!(snap.commit_used_bytes, 31 * GIB);
        assert_eq!(snap.commit_limit_bytes, 72 * GIB);
        assert_eq!(snap.pagefile_used_bytes, GIB);
        assert_eq!(snap.pagefile_total_bytes, 8 * GIB);

        // Defensively handles available > total without underflow.
        let clamped = compute_mem_snapshot(
            RawMemoryStatus {
                total_phys_bytes: 16 * GIB,
                avail_phys_bytes: 20 * GIB,
            },
            None,
            None,
            None,
        );
        assert_eq!(clamped.available_bytes, 16 * GIB);
        assert_eq!(clamped.used_bytes, 0);
        assert_eq!(
            clamped.used_bytes + clamped.available_bytes,
            clamped.total_bytes
        );
    }

    #[test]
    fn pagefile_pages_to_bytes_handles_4k_pages_zero_and_clamping() {
        assert_eq!(pagefile_pages_to_bytes(256, 1024, 4096), (MIB, 4 * MIB));
        assert_eq!(pagefile_pages_to_bytes(0, 0, 4096), (0, 0));
        // Clamps used <= total when transient pagefile shrink reports used > total.
        assert_eq!(
            pagefile_pages_to_bytes(2048, 1024, 4096),
            (4 * MIB, 4 * MIB)
        );
        // Saturates without overflow on huge inputs.
        assert_eq!(
            pagefile_pages_to_bytes(u64::MAX, u64::MAX, 4096),
            (u64::MAX, u64::MAX)
        );
    }

    #[cfg(windows)]
    #[test]
    fn live_windows_memory_collector_reports_consistent_ram_commit_and_cache() {
        let mut collector = MemoryCollector::new();
        let snap = collector.sample().expect("live memory sample");
        assert!(snap.total_bytes > 0, "expected non-zero physical RAM");
        assert_eq!(snap.used_bytes + snap.available_bytes, snap.total_bytes);
        assert!(
            snap.commit_limit_bytes >= snap.total_bytes,
            "commit limit ({}) should be at least physical RAM ({})",
            snap.commit_limit_bytes,
            snap.total_bytes
        );
        assert!(
            snap.commit_used_bytes > 0,
            "expected non-zero commit charge"
        );
        assert!(
            snap.cached_bytes > 0,
            "expected English PDH cache breakdown to be populated on Windows 11"
        );
        assert!(snap.free_bytes <= snap.available_bytes);
    }
}
