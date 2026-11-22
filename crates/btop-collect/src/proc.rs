//! Read-only Windows process collector backed by `NtQuerySystemInformation(SystemProcessInformation)`
//! enriched with identity-verified cached per-process image paths (`SystemProcessIdInformation`),
//! command lines (`ProcessCommandLineInformation`), confirmed protection status, and locally
//! SID-cached owner accounts.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use btop_model::{ProcessEntry, ProcessIdentity};
use btop_sys::{
    RawProcessInfo, RawProcessMetadata, SidAccountCache, query_nt_device_drive_map,
    query_process_information, query_process_metadata,
};

use crate::CollectError;

#[derive(Debug, Clone, PartialEq, Eq)]
struct CachedProcessMeta {
    image_path: Option<String>,
    command_line: Option<String>,
    command_line_denied: bool,
    user: Option<String>,
    user_denied: bool,
    is_protected: bool,
}

/// Validates and finalizes an unverified `SystemProcessIdInformation` image path against a
/// post-query `SystemProcessInformation` snapshot so PID reuse between enumeration and
/// `SystemProcessIdInformation` can never attach another executable's path to `identity`.
fn verify_unverified_image_path(
    identity: ProcessIdentity,
    expected_name: &str,
    candidate_path: Option<String>,
    post_query_live_ids: &HashSet<ProcessIdentity>,
) -> Option<String> {
    let path = candidate_path?;
    if !post_query_live_ids.contains(&identity) {
        return None;
    }
    if !expected_name.is_empty()
        && let Some(base) = path.rsplit('\\').next()
        && !base.is_empty()
        && !base.eq_ignore_ascii_case(expected_name)
    {
        return None;
    }
    Some(path)
}

fn meta_from_raw(raw: RawProcessMetadata) -> CachedProcessMeta {
    CachedProcessMeta {
        image_path: raw.image_path.into_option(),
        command_line_denied: raw.command_line.is_denied(),
        command_line: raw.command_line.into_option(),
        user_denied: raw.user.is_denied(),
        user: raw.user.into_option(),
        is_protected: raw.is_protected,
    }
}

/// Tracks per-process CPU time deltas keyed by `(pid, create_time_100ns)` to prevent
/// Windows PID-reuse spikes or stale state.
#[derive(Debug, Default)]
pub struct ProcessDeltaTracker {
    prev_times: HashMap<(u32, u64), u64>,
}

impl ProcessDeltaTracker {
    /// Converts raw `SYSTEM_PROCESS_INFORMATION` records into [`ProcessEntry`] rows,
    /// computing total-system normalized `cpu_pct` (`0.0..=100.0`) from `(pid, create_time_100ns)` deltas.
    pub fn compute_entries(
        &mut self,
        raw: &[RawProcessInfo],
        elapsed_100ns: u64,
        logical_cpus: usize,
    ) -> Vec<ProcessEntry> {
        let cpus = logical_cpus.max(1) as u64;
        let denom = elapsed_100ns.saturating_mul(cpus);
        let mut next_times = HashMap::with_capacity(raw.len());
        let mut entries = Vec::with_capacity(raw.len());

        for proc_info in raw {
            let key = (proc_info.pid, proc_info.create_time_100ns);
            let total_proc_time = proc_info
                .kernel_time_100ns
                .saturating_add(proc_info.user_time_100ns);
            next_times.insert(key, total_proc_time);

            let cpu_pct = if denom == 0 {
                0.0
            } else if let Some(&prev_time) = self.prev_times.get(&key) {
                let delta = total_proc_time.saturating_sub(prev_time);
                #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
                let pct = ((delta as f64 / denom as f64) * 100.0).clamp(0.0, 100.0) as f32;
                pct
            } else {
                0.0
            };

            entries.push(ProcessEntry {
                identity: ProcessIdentity {
                    pid: proc_info.pid,
                    create_time_100ns: proc_info.create_time_100ns,
                },
                parent_pid: proc_info.parent_pid,
                name: proc_info.name.clone(),
                image_path: None,
                command_line: None,
                command_line_denied: false,
                user: None,
                user_denied: false,
                threads: proc_info.threads,
                cpu_pct,
                working_set_private_bytes: proc_info.working_set_private_bytes,
                io_read_bytes: proc_info.io_read_bytes,
                io_write_bytes: proc_info.io_write_bytes,
                is_protected: matches!(proc_info.pid, 0 | 4),
            });
        }

        self.prev_times = next_times;
        entries
    }
}

/// Stateful Windows 11 read-only process collector reusing its NT query buffer across ticks
/// and caching immutable per-process metadata keyed by `(pid, create_time_100ns)` plus a local
/// SID-to-account cache.
#[derive(Debug, Default)]
pub struct ProcessCollector {
    scratch: Vec<u8>,
    tracker: ProcessDeltaTracker,
    meta_cache: HashMap<ProcessIdentity, CachedProcessMeta>,
    sid_cache: SidAccountCache,
    device_map: Vec<(String, String)>,
    last_sample: Option<Instant>,
}

impl ProcessCollector {
    pub fn new() -> Self {
        Self {
            scratch: Vec::with_capacity(256 * 1024),
            tracker: ProcessDeltaTracker::default(),
            meta_cache: HashMap::new(),
            sid_cache: SidAccountCache::new(),
            device_map: query_nt_device_drive_map(),
            last_sample: None,
        }
    }

    /// Returns the local SID lookup cache statistics `(hits, misses)` for testing and diagnostics.
    pub const fn sid_cache_stats(&self) -> (u64, u64) {
        (self.sid_cache.hits(), self.sid_cache.misses())
    }

    /// Collects all running processes via a single non-elevated `NtQuerySystemInformation` call.
    pub fn sample(&mut self, logical_cpus: usize) -> Result<Vec<ProcessEntry>, CollectError> {
        let (entries, _) = self.sample_with_runnable(logical_cpus)?;
        Ok(entries)
    }

    /// Collects all running processes and the aggregate count of Ready/Running threads
    /// across non-idle processes for `RunQ` EWMA computation.
    pub fn sample_with_runnable(
        &mut self,
        logical_cpus: usize,
    ) -> Result<(Vec<ProcessEntry>, u32), CollectError> {
        let now = Instant::now();
        let raw = query_process_information(&mut self.scratch).map_err(CollectError::Platform)?;
        let elapsed_100ns = self.last_sample.map_or(0, |prev| {
            u64::try_from(now.saturating_duration_since(prev).as_nanos() / 100).unwrap_or(u64::MAX)
        });
        self.last_sample = Some(now);
        let runnable_threads = raw
            .iter()
            .fold(0u32, |acc, r| acc.saturating_add(r.runnable_threads));
        let mut entries = self
            .tracker
            .compute_entries(&raw, elapsed_100ns, logical_cpus);

        let mut live_ids = HashSet::with_capacity(entries.len());
        let mut pending_recheck: Vec<(usize, CachedProcessMeta)> = Vec::new();

        for (idx, entry) in entries.iter_mut().enumerate() {
            let id = entry.identity;
            live_ids.insert(id);
            if let Some(cached) = self.meta_cache.get(&id) {
                apply_cached_meta(entry, cached);
                continue;
            }

            let queried = query_process_metadata(
                id.pid,
                id.create_time_100ns,
                &self.device_map,
                &mut self.sid_cache,
            );
            let verified = queried.image_path_verified;
            let meta = meta_from_raw(queried);

            if verified {
                apply_cached_meta(entry, &meta);
                self.meta_cache.insert(id, meta);
            } else {
                // Access-denied or failed OpenProcess: defer accepting/caching `image_path`
                // until a post-query snapshot proves `(pid, create_time_100ns)` is still alive.
                pending_recheck.push((idx, meta));
            }
        }

        if !pending_recheck.is_empty() {
            let post_live: HashSet<ProcessIdentity> = query_process_information(&mut self.scratch)
                .map(|rows| {
                    rows.into_iter()
                        .map(|r| ProcessIdentity {
                            pid: r.pid,
                            create_time_100ns: r.create_time_100ns,
                        })
                        .collect()
                })
                .unwrap_or_default();

            for (idx, mut meta) in pending_recheck {
                let entry = &mut entries[idx];
                let id = entry.identity;
                let still_alive = post_live.contains(&id);
                meta.image_path =
                    verify_unverified_image_path(id, &entry.name, meta.image_path, &post_live);
                apply_cached_meta(entry, &meta);
                if still_alive {
                    self.meta_cache.insert(id, meta);
                }
            }
        }

        self.meta_cache.retain(|id, _| live_ids.contains(id));

        Ok((entries, runnable_threads))
    }
}

fn apply_cached_meta(entry: &mut ProcessEntry, meta: &CachedProcessMeta) {
    entry.image_path.clone_from(&meta.image_path);
    entry.command_line.clone_from(&meta.command_line);
    entry.command_line_denied = meta.command_line_denied;
    entry.user.clone_from(&meta.user);
    entry.user_denied = meta.user_denied;
    entry.is_protected = entry.is_protected || meta.is_protected;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_proc(pid: u32, create: u64, kernel: u64, user: u64, name: &str) -> RawProcessInfo {
        RawProcessInfo {
            pid,
            parent_pid: 4,
            create_time_100ns: create,
            kernel_time_100ns: kernel,
            user_time_100ns: user,
            threads: 4,
            runnable_threads: 1,
            name: name.to_owned(),
            working_set_private_bytes: 8 * 1024 * 1024,
            io_read_bytes: 1024,
            io_write_bytes: 512,
        }
    }

    #[test]
    fn pid_reuse_does_not_inherit_previous_process_cpu_time() {
        let mut tracker = ProcessDeltaTracker::default();

        // Tick 0: PID 4820 (old_proc) has accumulated 50_000_000 (5s) of CPU time.
        let tick0 = [raw_proc(4820, 1000, 30_000_000, 20_000_000, "old.exe")];
        let out0 = tracker.compute_entries(&tick0, 0, 4);
        assert_eq!(out0.len(), 1);
        assert!((out0[0].cpu_pct - 0.0).abs() < f32::EPSILON);

        // Tick 1: PID 4820 still alive, gained 10_000_000 (1s) over 1s across 4 CPUs => 25.0%.
        let tick1 = [raw_proc(4820, 1000, 35_000_000, 25_000_000, "old.exe")];
        let out1 = tracker.compute_entries(&tick1, 10_000_000, 4);
        assert!((out1[0].cpu_pct - 25.0).abs() < 0.01);

        // Tick 2: PID 4820 exited and was reused with a newer create_time (2000) and fresh CPU time.
        // Because (4820, 2000) != (4820, 1000), it must not inherit or underflow against old.exe.
        let tick2 = [raw_proc(4820, 2000, 80_000_000, 10_000_000, "reused.exe")];
        let out2 = tracker.compute_entries(&tick2, 10_000_000, 4);
        assert_eq!(out2[0].name, "reused.exe");
        assert!((out2[0].cpu_pct - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn unverified_image_path_is_rejected_on_pid_reuse_or_name_mismatch() {
        let old_id = ProcessIdentity {
            pid: 4820,
            create_time_100ns: 1000,
        };
        let reused_id = ProcessIdentity {
            pid: 4820,
            create_time_100ns: 2000,
        };

        // Post-query snapshot contains only the reused process -> original identity must reject path.
        let mut post_reused = HashSet::new();
        post_reused.insert(reused_id);
        assert_eq!(
            verify_unverified_image_path(
                old_id,
                "old.exe",
                Some("C:\\Tools\\replacement.exe".to_owned()),
                &post_reused,
            ),
            None
        );

        // Post-query snapshot still contains exact (pid, create_time) AND matching executable name -> accept.
        let mut post_alive = HashSet::new();
        post_alive.insert(old_id);
        assert_eq!(
            verify_unverified_image_path(
                old_id,
                "csrss.exe",
                Some("C:\\Windows\\System32\\csrss.exe".to_owned()),
                &post_alive,
            )
            .as_deref(),
            Some("C:\\Windows\\System32\\csrss.exe")
        );

        // Basename mismatch is also rejected defensively.
        assert_eq!(
            verify_unverified_image_path(
                old_id,
                "csrss.exe",
                Some("C:\\Windows\\System32\\other.exe".to_owned()),
                &post_alive,
            ),
            None
        );
    }

    #[test]
    fn field_access_denial_does_not_falsely_mark_process_as_protected_ppl() {
        let denied_meta = RawProcessMetadata {
            image_path: btop_sys::ProcFieldQuery::Available(
                "C:\\Program Files\\Service\\svc.exe".to_owned(),
            ),
            image_path_verified: true,
            command_line: btop_sys::ProcFieldQuery::Denied,
            user: btop_sys::ProcFieldQuery::Denied,
            is_protected: false,
        };
        let cached = meta_from_raw(denied_meta);
        assert!(cached.command_line_denied);
        assert!(cached.user_denied);
        assert!(
            !cached.is_protected,
            "ACL query denial alone must not claim confirmed PPL protection"
        );
    }

    #[cfg(windows)]
    #[test]
    fn live_windows_process_collector_finds_current_pid_metadata_and_sid_cache_hits() {
        let mut collector = ProcessCollector::new();
        let first = collector.sample(1).expect("first live process sample");
        assert!(!first.is_empty(), "process list must not be empty");

        let my_pid = std::process::id();
        let me = first
            .iter()
            .find(|p| p.identity.pid == my_pid)
            .expect("current test process must appear in SystemProcessInformation");
        assert!(
            me.identity.create_time_100ns > 0,
            "current process create_time must be non-zero"
        );
        assert!(
            me.working_set_private_bytes > 0,
            "current process working_set_private_bytes must be positive"
        );
        assert!(
            !me.name.is_empty(),
            "current process name must be non-empty"
        );
        assert!(
            me.command_line.as_deref().is_some_and(|c| !c.is_empty()),
            "current process command_line must be populated for live argument filtering"
        );
        assert!(
            me.image_path.as_deref().is_some_and(|p| !p.is_empty()),
            "current process image_path must be populated via SystemProcessIdInformation"
        );
        assert!(
            me.user.as_deref().is_some_and(|u| !u.is_empty()),
            "current process owner user must be populated via locally cached SID lookup"
        );
        assert!(!me.is_protected);

        let (hits, misses) = collector.sid_cache_stats();
        assert!(misses >= 1, "at least one SID must be resolved");
        assert!(
            hits >= 1,
            "multiple processes owned by the same user must hit the local SID cache"
        );

        // Verify kernel entries render cleanly with non-empty names when present.
        for proc_entry in &first {
            if matches!(proc_entry.identity.pid, 0 | 4) {
                assert_ne!(proc_entry.name, "");
                assert!(proc_entry.is_protected);
            }
        }
    }
}
