//! Windows 11 memory, commit charge, pagefile, and English PDH cache-list collector.

use btop_model::MemSnapshot;
use btop_sys::{
    MemPdhQuery, RawMemCacheSample, RawMemoryStatus, RawPagefileTotals, RawPerformanceInfo,
    query_memory_status, query_pagefile_totals, query_performance_info,
};

use crate::CollectError;

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

/// Computes a consistent [`MemSnapshot`] from complete Windows 11 memory, commit, pagefile, and
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
    perf: RawPerformanceInfo,
    pagefiles: RawPagefileTotals,
    cache: RawMemCacheSample,
) -> Result<MemSnapshot, CollectError> {
    if perf.page_size_bytes == 0 {
        return Err(CollectError::Platform(
            "GetPerformanceInfo returned zero page size".to_owned(),
        ));
    }

    let total_bytes = status.total_phys_bytes;
    let available_bytes = status.avail_phys_bytes.min(total_bytes);
    let used_bytes = total_bytes.saturating_sub(available_bytes);

    let standby = cache
        .standby_core_bytes
        .saturating_add(cache.standby_normal_bytes)
        .saturating_add(cache.standby_reserve_bytes);
    let cached_bytes = standby
        .saturating_add(cache.modified_bytes)
        .min(total_bytes);
    let free_bytes = cache.free_and_zero_bytes.min(available_bytes);

    let page_size = perf.page_size_bytes;
    let commit_used_bytes = perf.commit_total_pages.saturating_mul(page_size);
    let commit_limit_bytes = perf.commit_limit_pages.saturating_mul(page_size);
    let (pagefile_used_bytes, pagefile_total_bytes) =
        pagefile_pages_to_bytes(pagefiles.used_pages, pagefiles.total_pages, page_size);

    Ok(MemSnapshot {
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
    })
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
    ///
    /// Propagates any Windows API or PDH failure as `Err` so callers publish
    /// `DataState::Unavailable` rather than misleading zero values.
    pub fn sample(&mut self) -> Result<MemSnapshot, CollectError> {
        let status = query_memory_status().map_err(CollectError::Platform)?;
        let perf = query_performance_info().map_err(CollectError::Platform)?;
        let pagefiles = query_pagefile_totals().map_err(CollectError::Platform)?;
        let cache = self
            .pdh
            .as_mut()
            .and_then(MemPdhQuery::sample)
            .ok_or_else(|| {
                CollectError::Platform("English memory cache PDH counters unavailable".to_owned())
            })?;
        compute_mem_snapshot(status, perf, pagefiles, cache)
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
            RawPerformanceInfo {
                commit_total_pages: 8_126_464,  // 31 GiB in 4 KiB pages
                commit_limit_pages: 18_874_368, // 72 GiB in 4 KiB pages
                page_size_bytes: 4096,
            },
            RawPagefileTotals {
                used_pages: 262_144,    // 1 GiB
                total_pages: 2_097_152, // 8 GiB
            },
            RawMemCacheSample {
                standby_core_bytes: 2 * GIB,
                standby_normal_bytes: 7 * GIB,
                standby_reserve_bytes: GIB,
                modified_bytes: GIB,
                free_and_zero_bytes: 28 * GIB,
            },
        )
        .expect("complete memory sample");

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
            RawPerformanceInfo {
                commit_total_pages: 1024,
                commit_limit_pages: 2048,
                page_size_bytes: 4096,
            },
            RawPagefileTotals::default(),
            RawMemCacheSample::default(),
        )
        .expect("clamped memory sample");
        assert_eq!(clamped.available_bytes, 16 * GIB);
        assert_eq!(clamped.used_bytes, 0);
        assert_eq!(
            clamped.used_bytes + clamped.available_bytes,
            clamped.total_bytes
        );

        // Rejects invalid zero page size instead of fabricating numbers.
        assert!(
            compute_mem_snapshot(
                RawMemoryStatus {
                    total_phys_bytes: 16 * GIB,
                    avail_phys_bytes: 8 * GIB,
                },
                RawPerformanceInfo {
                    commit_total_pages: 1024,
                    commit_limit_pages: 2048,
                    page_size_bytes: 0,
                },
                RawPagefileTotals::default(),
                RawMemCacheSample::default(),
            )
            .is_err()
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
