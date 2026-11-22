//! Windows 11 local fixed-drive (`DRIVE_FIXED`) capacity and `IOCTL_DISK_PERFORMANCE`
//! throughput collector, plus pure filter and rate math.

use std::collections::{BTreeMap, HashMap, HashSet};

use btop_model::DiskSnapshot;
use btop_sys::{
    RawDiskPerformance, RawFixedDrive, enumerate_fixed_drives, query_physical_drive_performance,
};

use crate::CollectError;

/// Extracts the normalized uppercase ASCII drive letter from a mount path or filter token
/// (e.g. `"C:\\"`, `"c:"`, `"c"` -> `Some('C')`).
pub fn extract_drive_letter(token: &str) -> Option<char> {
    let trimmed = token.trim().trim_start_matches('/');
    let ch = trimmed.chars().next()?;
    if !ch.is_ascii_alphabetic() {
        return None;
    }
    let rest = &trimmed[ch.len_utf8()..];
    if rest.is_empty() || rest == ":" || rest == ":\\" || rest == ":/" {
        Some(ch.to_ascii_uppercase())
    } else {
        None
    }
}

/// Evaluates whether a local fixed drive mount (`C:\`, `D:\`, ...) matches `disks_filter`.
///
/// Syntax (case-insensitive):
/// - Empty / whitespace: matches all local fixed drives.
/// - `"exclude=E F"` (or comma-separated `"exclude=E:,F:\\"`): excludes listed drive letters.
/// - `"C D"` (or `"C:,D:\\"`): includes only listed drive letters.
pub fn matches_disks_filter(mount: &str, filter: &str) -> bool {
    let trimmed = filter.trim();
    if trimmed.is_empty() {
        return true;
    }
    let Some(drive_letter) = extract_drive_letter(mount) else {
        return false;
    };

    let (is_exclude, body) = if let Some(rest) = trimmed
        .strip_prefix("exclude=")
        .or_else(|| trimmed.strip_prefix("EXCLUDE="))
    {
        (true, rest)
    } else if trimmed.len() >= 8 && trimmed[..8].eq_ignore_ascii_case("exclude=") {
        (true, &trimmed[8..])
    } else {
        (false, trimmed)
    };

    let letters: Vec<char> = body
        .split(|c: char| c.is_ascii_whitespace() || c == ',' || c == ';')
        .filter_map(extract_drive_letter)
        .collect();

    if letters.is_empty() {
        return true;
    }

    let listed = letters.contains(&drive_letter);
    if is_exclude { !listed } else { listed }
}

/// Computes physical disk busy percentage `100 * (1 - DeltaIdleTime / DeltaQueryTime)`
/// clamped to `0.0..=100.0`, returning `0.0` on counter reset or zero elapsed time.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
pub fn compute_disk_busy_pct(
    prev_idle_100ns: i64,
    curr_idle_100ns: i64,
    prev_query_100ns: i64,
    curr_query_100ns: i64,
) -> f32 {
    if curr_query_100ns <= prev_query_100ns || curr_idle_100ns < prev_idle_100ns {
        return 0.0;
    }
    let delta_query = curr_query_100ns.saturating_sub(prev_query_100ns);
    let delta_idle = curr_idle_100ns.saturating_sub(prev_idle_100ns);
    if delta_query <= 0 {
        return 0.0;
    }
    let idle_ratio = (delta_idle as f64 / delta_query as f64).clamp(0.0, 1.0);
    ((1.0 - idle_ratio) * 100.0).clamp(0.0, 100.0) as f32
}

/// Computes `(read_bytes_per_sec, write_bytes_per_sec, busy_pct)` from two consecutive
/// `IOCTL_DISK_PERFORMANCE` samples, handling counter resets cleanly without underflow.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
pub fn compute_disk_io_rates(
    prev: RawDiskPerformance,
    curr: RawDiskPerformance,
) -> (u64, u64, f32) {
    if curr.query_time_100ns <= prev.query_time_100ns {
        return (0, 0, 0.0);
    }
    let delta_query_100ns = curr.query_time_100ns.saturating_sub(prev.query_time_100ns);
    let elapsed_secs = (delta_query_100ns as f64) / 10_000_000.0;
    if elapsed_secs <= 0.0 || !elapsed_secs.is_finite() {
        return (0, 0, 0.0);
    }

    let read_bps = if curr.bytes_read >= prev.bytes_read {
        let delta_r = curr.bytes_read - prev.bytes_read;
        ((delta_r as f64) / elapsed_secs).round().max(0.0) as u64
    } else {
        0
    };
    let write_bps = if curr.bytes_written >= prev.bytes_written {
        let delta_w = curr.bytes_written - prev.bytes_written;
        ((delta_w as f64) / elapsed_secs).round().max(0.0) as u64
    } else {
        0
    };
    let busy_pct = compute_disk_busy_pct(
        prev.idle_time_100ns,
        curr.idle_time_100ns,
        prev.query_time_100ns,
        curr.query_time_100ns,
    );
    (read_bps, write_bps, busy_pct)
}

/// Converts raw fixed drives plus optional per-physical-drive I/O rate map into `DiskSnapshot`s.
pub fn build_disk_snapshots(
    drives: Vec<RawFixedDrive>,
    io_rates: &BTreeMap<u32, (u64, u64, f32)>,
) -> Vec<DiskSnapshot> {
    drives
        .into_iter()
        .map(|d| {
            let free_bytes = d.free_bytes.min(d.total_bytes);
            let used_bytes = d.total_bytes.saturating_sub(free_bytes);
            let (read_bytes_per_sec, write_bytes_per_sec, busy_pct) =
                match d.physical_drive.and_then(|idx| io_rates.get(&idx).copied()) {
                    Some((r, w, b)) => (Some(r), Some(w), Some(b)),
                    None => (None, None, None),
                };
            DiskSnapshot {
                mount: d.mount,
                label: d.label,
                fs_type: d.fs_type,
                physical_drive: d.physical_drive,
                total_bytes: d.total_bytes,
                used_bytes,
                free_bytes,
                read_bytes_per_sec,
                write_bytes_per_sec,
                busy_pct,
            }
        })
        .collect()
}

/// Live Windows 11 local fixed-drive capacity and physical disk I/O collector.
#[derive(Debug, Default)]
pub struct DiskCollector {
    prev_perf: HashMap<u32, RawDiskPerformance>,
}

impl DiskCollector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Samples local fixed drives and computes non-elevated `IOCTL_DISK_PERFORMANCE` deltas
    /// once per unique physical drive.
    pub fn sample(&mut self) -> Result<Vec<DiskSnapshot>, CollectError> {
        let drives = enumerate_fixed_drives().map_err(CollectError::Platform)?;
        let unique_disks: HashSet<u32> = drives.iter().filter_map(|d| d.physical_drive).collect();

        let mut rates = BTreeMap::new();
        let mut next_prev = HashMap::with_capacity(unique_disks.len());

        for disk_num in unique_disks {
            if let Ok(curr) = query_physical_drive_performance(disk_num) {
                if let Some(&prev) = self.prev_perf.get(&disk_num) {
                    rates.insert(disk_num, compute_disk_io_rates(prev, curr));
                }
                next_prev.insert(disk_num, curr);
            }
        }
        self.prev_perf = next_prev;

        Ok(build_disk_snapshots(drives, &rates))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_disks_filter_supports_empty_include_and_exclude_lists() {
        assert!(matches_disks_filter("C:\\", ""));
        assert!(matches_disks_filter("D:\\", "   "));

        assert!(matches_disks_filter("C:\\", "C D"));
        assert!(matches_disks_filter("d:\\", "C: D:\\"));
        assert!(!matches_disks_filter("E:\\", "C D"));

        assert!(!matches_disks_filter("E:\\", "exclude=E F"));
        assert!(!matches_disks_filter("f:\\", "EXCLUDE=e:,f:\\"));
        assert!(matches_disks_filter("C:\\", "exclude=E,F"));
        assert!(matches_disks_filter("C:\\", "exclude="));
    }

    #[test]
    fn compute_disk_busy_pct_clamps_and_handles_resets() {
        // 25% idle over 1s -> 75% busy
        let busy = compute_disk_busy_pct(1_000, 3_500_000, 10_000_000, 20_000_000);
        assert!((busy - 65.0).abs() < 0.05);

        // 0% idle delta -> 100% busy
        let full = compute_disk_busy_pct(5_000, 5_000, 10_000, 20_000);
        assert!((full - 100.0).abs() < 0.01);

        // Idle delta larger than query delta clamps cleanly to 0% busy
        let over_idle = compute_disk_busy_pct(0, 20_000, 10_000, 20_000);
        assert!((over_idle - 0.0).abs() < f32::EPSILON);

        // Counter reset (curr < prev) returns 0% instead of underflow or NaN
        assert!((compute_disk_busy_pct(500, 100, 10_000, 20_000) - 0.0).abs() < f32::EPSILON);
        assert!((compute_disk_busy_pct(100, 200, 20_000, 10_000) - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn compute_disk_io_rates_computes_throughput_and_handles_counter_resets() {
        let prev = RawDiskPerformance {
            bytes_read: 1_000_000,
            bytes_written: 2_000_000,
            idle_time_100ns: 10_000_000,
            query_time_100ns: 100_000_000,
        };
        // Exactly 0.5 seconds later: +50 MB read (100 MB/s), +25 MB written (50 MB/s), 25% idle -> 75% busy
        let curr = RawDiskPerformance {
            bytes_read: 51_000_000,
            bytes_written: 27_000_000,
            idle_time_100ns: 11_250_000,
            query_time_100ns: 105_000_000,
        };
        let (r, w, busy) = compute_disk_io_rates(prev, curr);
        assert_eq!(r, 100_000_000);
        assert_eq!(w, 50_000_000);
        assert!((busy - 75.0).abs() < 0.01);

        // Counter reset clamps rates to 0 safely
        let reset = RawDiskPerformance {
            bytes_read: 500,
            bytes_written: 100,
            idle_time_100ns: 100,
            query_time_100ns: 110_000_000,
        };
        let (rr, rw, rb) = compute_disk_io_rates(curr, reset);
        assert_eq!(rr, 0);
        assert_eq!(rw, 0);
        assert!((rb - 0.0).abs() < f32::EPSILON);
    }

    #[cfg(windows)]
    #[test]
    fn live_windows_disk_collector_enumerates_fixed_drives_and_reads_ioctl_perf() {
        let mut collector = DiskCollector::new();
        let first = collector.sample().expect("first disk sample");
        assert!(
            !first.is_empty(),
            "expected at least one local fixed drive on Windows 11"
        );
        assert!(
            first
                .iter()
                .any(|d| d.total_bytes > 0 && d.used_bytes + d.free_bytes == d.total_bytes),
            "expected valid capacity accounting"
        );
        // First tick primes I/O baseline, so rates start as None.
        assert!(first.iter().all(|d| d.read_bytes_per_sec.is_none()));

        std::thread::sleep(std::time::Duration::from_millis(60));
        let second = collector.sample().expect("second disk sample");
        assert!(
            second.iter().any(|d| d.physical_drive.is_some()),
            "expected at least one drive mapped via IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS"
        );
        assert!(
            second.iter().any(|d| d.busy_pct.is_some()),
            "expected non-elevated IOCTL_DISK_PERFORMANCE busy_pct on second tick"
        );
    }
}
