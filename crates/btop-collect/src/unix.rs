//! Cross-platform Unix (`Linux` and `macOS`) local telemetry collector backed by targeted
//! `sysinfo` refreshes, implementing ADR-0003.
//!
//! Pure filtering, APFS/bind-mount deduplication, memory accounting, and normalization helpers
//! are compiled unconditionally so Windows, Linux, and macOS CI all test them.

use std::collections::BTreeMap;

use btop_model::{DiskSnapshot, MemSnapshot};

/// Raw mounted filesystem record passed into the pure Unix disk filter and deduplicator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawUnixDisk {
    pub device_name: String,
    pub mount_point: String,
    pub fs_type: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub read_bytes_per_sec: Option<u64>,
    pub write_bytes_per_sec: Option<u64>,
}

const IGNORED_UNIX_FS_TYPES: &[&str] = &[
    "autofs",
    "binfmt_misc",
    "bpf",
    "cgroup",
    "cgroup2",
    "configfs",
    "debugfs",
    "devfs",
    "devpts",
    "devtmpfs",
    "efivarfs",
    "fdescfs",
    "fuse.gvfsd-fuse",
    "fuse.lxcfs",
    "fuse.portal",
    "fuse.snapfuse",
    "fusectl",
    "hugetlbfs",
    "linprocfs",
    "linsysfs",
    "mqueue",
    "nfsd",
    "nsfs",
    "nullfs",
    "overlay",
    "overlayfs",
    "proc",
    "procfs",
    "pstore",
    "ramfs",
    "rpc_pipefs",
    "securityfs",
    "squashfs",
    "sysfs",
    "tmpfs",
    "tracefs",
];

const IGNORED_UNIX_MOUNT_PREFIXES: &[&str] = &[
    "/dev",
    "/proc",
    "/sys",
    "/run",
    "/snap",
    "/var/lib/docker",
    "/var/lib/containers",
    "/private/var/vm",
    "/private/var/run",
    "/Library/Developer/CoreSimulator/Volumes",
    "/System/Volumes/VM",
    "/System/Volumes/Preboot",
    "/System/Volumes/Update",
    "/System/Volumes/xarts",
    "/System/Volumes/iSCPreboot",
    "/System/Volumes/Hardware",
    "/System/Volumes/Recovery",
    "/System/Volumes/FieldService",
    "/System/Volumes/Data/home",
];

fn mount_matches_prefix(mount: &str, prefix: &str) -> bool {
    mount == prefix
        || mount
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Returns `true` when a Unix filesystem mount represents a real local storage volume rather
/// than virtual kernel mounts, snap images, simulator runtimes, or macOS system helper volumes.
pub fn should_include_unix_disk(
    mount_point: &str,
    fs_type: &str,
    device_name: &str,
    total_bytes: u64,
) -> bool {
    if total_bytes == 0 || mount_point.is_empty() {
        return false;
    }
    let fs_lower = fs_type.trim().to_ascii_lowercase();
    if IGNORED_UNIX_FS_TYPES.contains(&fs_lower.as_str()) {
        return false;
    }
    if device_name.starts_with("/dev/loop")
        || device_name.starts_with("map ")
        || device_name.eq_ignore_ascii_case("none")
    {
        return false;
    }
    // Preserve `/run/media` (Linux removable and local drives mounted by udisks2) while filtering
    // runtime tmpfs trees under `/run`.
    if !mount_matches_prefix(mount_point, "/run/media")
        && IGNORED_UNIX_MOUNT_PREFIXES
            .iter()
            .any(|prefix| mount_matches_prefix(mount_point, prefix))
    {
        return false;
    }
    true
}

/// Extracts the macOS APFS container prefix (e.g. `"/dev/disk3s1s1"` -> `Some("disk3")`) so
/// sibling volumes sharing one APFS container (`/` and `/System/Volumes/Data`) are deduplicated
/// rather than double-counting container capacity.
pub fn apfs_container_key<'a>(fs_type: &str, device_name: &'a str) -> Option<&'a str> {
    if !fs_type.eq_ignore_ascii_case("apfs") {
        return None;
    }
    let trimmed = device_name.strip_prefix("/dev/").unwrap_or(device_name);
    let rest = trimmed.strip_prefix("disk")?;
    let digits_len = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits_len == 0 {
        return None;
    }
    let after_digits = &rest[digits_len..];
    if after_digits.is_empty() || after_digits.starts_with('s') {
        Some(&trimmed[..4 + digits_len])
    } else {
        None
    }
}

fn prefer_mount(candidate: &str, existing: &str) -> bool {
    if candidate == "/" && existing != "/" {
        return true;
    }
    if existing == "/" {
        return false;
    }
    (candidate.len(), candidate) < (existing.len(), existing)
}

/// Filters noisy Unix mounts, deduplicates macOS APFS container siblings and Linux bind/Btrfs
/// duplicate device mounts, and returns deterministic [`DiskSnapshot`] rows (`"/"` first).
pub fn deduplicate_unix_disks(raw: Vec<RawUnixDisk>) -> Vec<DiskSnapshot> {
    let mut by_key: BTreeMap<String, RawUnixDisk> = BTreeMap::new();

    for disk in raw {
        if !should_include_unix_disk(
            &disk.mount_point,
            &disk.fs_type,
            &disk.device_name,
            disk.total_bytes,
        ) {
            continue;
        }
        let dedup_key =
            if let Some(container) = apfs_container_key(&disk.fs_type, &disk.device_name) {
                format!("apfs:dev:{container}")
            } else if disk.fs_type.eq_ignore_ascii_case("apfs")
                && (disk.mount_point == "/" || disk.mount_point == "/System/Volumes/Data")
            {
                // Fallback when `statfs` is unavailable: only collapse the macOS boot root and
                // its paired `/System/Volumes/Data` sibling sharing identical capacity.
                format!("apfs:root:{}", disk.total_bytes)
            } else if disk.device_name.starts_with("/dev/") {
                format!("dev:{}:{}", disk.device_name, disk.total_bytes)
            } else {
                format!("mnt:{}", disk.mount_point)
            };

        match by_key.get_mut(&dedup_key) {
            Some(existing) => {
                // Keep the canonical mount point (`/` preferred, else shortest path) and retain
                // non-None I/O rates or lower available space if a sibling reported more detail.
                let read_rate = existing.read_bytes_per_sec.or(disk.read_bytes_per_sec);
                let write_rate = existing.write_bytes_per_sec.or(disk.write_bytes_per_sec);
                let avail = existing.available_bytes.min(disk.available_bytes);
                if prefer_mount(&disk.mount_point, &existing.mount_point) {
                    *existing = disk;
                }
                existing.available_bytes = avail;
                existing.read_bytes_per_sec = read_rate;
                existing.write_bytes_per_sec = write_rate;
            }
            None => {
                by_key.insert(dedup_key, disk);
            }
        }
    }

    let mut out: Vec<DiskSnapshot> = by_key
        .into_values()
        .map(|disk| {
            let free = disk.available_bytes.min(disk.total_bytes);
            let used = disk.total_bytes.saturating_sub(free);
            DiskSnapshot {
                mount: disk.mount_point,
                label: disk.device_name,
                fs_type: disk.fs_type,
                physical_drive: None,
                total_bytes: disk.total_bytes,
                used_bytes: used,
                free_bytes: free,
                read_bytes_per_sec: disk.read_bytes_per_sec,
                write_bytes_per_sec: disk.write_bytes_per_sec,
                busy_pct: None,
            }
        })
        .collect();

    out.sort_by(|a, b| {
        let a_root = a.mount != "/";
        let b_root = b.mount != "/";
        a_root.cmp(&b_root).then_with(|| a.mount.cmp(&b.mount))
    });
    out
}

const IGNORED_IFACE_PREFIXES: &[&str] = &[
    "tailscale",
    "utun",
    "tun",
    "tap",
    "wg",
    "docker",
    "veth",
    "br-",
    "vmbr",
    "virbr",
    "vnet",
    "cni",
    "flannel",
    "podman",
    "cali",
    "kube",
    "dummy",
    "sit",
    "gre",
    "gif",
    "stf",
    "anpi",
    "bridge",
    "awdl",
    "llw",
    "pktap",
    "ap",
];

fn is_loopback_iface(lower: &str) -> bool {
    lower == "lo"
        || lower
            .strip_prefix("lo")
            .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

fn is_numbered_bridge_iface(lower: &str) -> bool {
    lower
        .strip_prefix("br")
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

fn is_vlan_iface(lower: &str) -> bool {
    if lower
        .strip_prefix("vlan")
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
    {
        return true;
    }
    lower.rsplit_once('.').is_some_and(|(parent, id)| {
        !parent.is_empty() && !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())
    })
}

/// Returns `true` for physical Unix network interfaces, excluding loopback (`lo`/`lo0`),
/// Tailscale and VPN tunnels (`tailscale*`, `utun*`, `tun*`, `wg*`), Docker/VM/software bridges
/// (`docker*`, `veth*`, `br0`, `br-*`, `vmbr*`, `virbr*`, `vnet*`), VLAN sub-interfaces
/// (`enp2s0.100`, `vlan100`), and Apple virtual peer interfaces.
pub fn should_include_unix_interface(name: &str) -> bool {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    if is_loopback_iface(&lower) || is_numbered_bridge_iface(&lower) || is_vlan_iface(&lower) {
        return false;
    }
    !IGNORED_IFACE_PREFIXES
        .iter()
        .any(|prefix| lower.starts_with(prefix))
}

/// Computes a deterministic 64-bit FNV-1a interface identifier for Unix NIC names so UI
/// interface selection remains stable across ticks.
pub fn stable_interface_luid(name: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in name.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Parses Linux `/proc/meminfo` `Committed_AS` and `CommitLimit` (in KiB) into byte counts.
pub fn parse_linux_meminfo_commit(meminfo: &str) -> Option<(u64, u64)> {
    let mut committed_kib = None;
    let mut limit_kib = None;
    for line in meminfo.lines() {
        if let Some(rest) = line.strip_prefix("Committed_AS:") {
            committed_kib = rest
                .split_whitespace()
                .next()
                .and_then(|v| v.parse::<u64>().ok());
        } else if let Some(rest) = line.strip_prefix("CommitLimit:") {
            limit_kib = rest
                .split_whitespace()
                .next()
                .and_then(|v| v.parse::<u64>().ok());
        }
    }
    match (committed_kib, limit_kib) {
        (Some(used), Some(limit)) if limit > 0 => {
            Some((used.saturating_mul(1024), limit.saturating_mul(1024)))
        }
        _ => None,
    }
}

/// Builds a consistent Unix [`MemSnapshot`] using `used = total - available` so Linux page
/// cache and macOS reclaimable memory do not falsely inflate memory pressure.
///
/// `commit` carries real OS virtual-memory commit accounting (`(committed, limit)`) when exposed
/// by the kernel (Linux `/proc/meminfo`), or `None` -> `(0, 0)` on macOS so physical RAM usage is
/// never misreported as Windows commit charge.
pub fn compute_unix_mem_snapshot(
    total_bytes: u64,
    available_bytes: u64,
    free_bytes: u64,
    swap_total_bytes: u64,
    swap_used_bytes: u64,
    commit: Option<(u64, u64)>,
) -> MemSnapshot {
    let available = available_bytes.min(total_bytes);
    let free = free_bytes.min(available);
    let used = total_bytes.saturating_sub(available);
    let cached = available.saturating_sub(free);
    let pagefile_used = swap_used_bytes.min(swap_total_bytes);
    let (commit_used_bytes, commit_limit_bytes) = commit.unwrap_or((0, 0));
    MemSnapshot {
        total_bytes,
        used_bytes: used,
        available_bytes: available,
        cached_bytes: cached,
        free_bytes: free,
        commit_used_bytes,
        commit_limit_bytes,
        pagefile_used_bytes: pagefile_used,
        pagefile_total_bytes: swap_total_bytes,
        compressed_bytes: None,
    }
}

/// Clamps raw CPU utilization percentage to finite `[0.0, 100.0]`.
pub fn normalize_unix_cpu_pct(raw: f32) -> f32 {
    if raw.is_finite() {
        raw.clamp(0.0, 100.0)
    } else {
        0.0
    }
}

/// Normalizes `sysinfo`'s per-core process CPU percentage (`0.0..=100.0 * logical_cpus`) onto the
/// total-system `0.0..=100.0` scale used by `btop-model` and `btop-ui`.
#[allow(clippy::cast_precision_loss)]
pub fn normalize_unix_proc_cpu_pct(raw_proc_pct: f32, logical_cpus: usize) -> f32 {
    if !raw_proc_pct.is_finite() || raw_proc_pct <= 0.0 {
        return 0.0;
    }
    let denom = logical_cpus.max(1) as f32;
    (raw_proc_pct / denom).clamp(0.0, 100.0)
}

/// Parses Linux `/proc/stat`'s `procs_running <N>` counter (runnable CPU tasks excluding
/// `TASK_UNINTERRUPTIBLE` disk I/O waiters).
pub fn parse_linux_procs_running(proc_stat: &str) -> Option<u32> {
    for line in proc_stat.lines() {
        if let Some(rest) = line.strip_prefix("procs_running ")
            && let Ok(count) = rest.trim().parse::<u32>()
        {
            return Some(count);
        }
    }
    None
}

#[cfg(unix)]
mod live {
    use std::collections::HashMap;
    use std::net::IpAddr;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use btop_model::{
        CoreKind, CoreSample, CpuSnapshot, DataState, LogicalCpuId, NetInterfaceSnapshot,
        ProcessEntry, ProcessIdentity, Snapshot, UNIX_GPUS_UNAVAILABLE_REASON,
        UNIX_SERVICES_UNAVAILABLE_REASON,
    };
    use btop_sys::{ProcActionError, query_unix_process_meta, unix_mount_device_name};
    use sysinfo::{
        CpuRefreshKind, DiskRefreshKind, Disks, InterfaceOperationalState,
        MINIMUM_CPU_UPDATE_INTERVAL, MemoryRefreshKind, Networks, ProcessRefreshKind,
        ProcessesToUpdate, RefreshKind, System, Users,
    };

    use super::{
        RawUnixDisk, compute_unix_mem_snapshot, deduplicate_unix_disks, normalize_unix_cpu_pct,
        normalize_unix_proc_cpu_pct, should_include_unix_interface, stable_interface_luid,
    };
    #[cfg(target_os = "linux")]
    use super::{parse_linux_meminfo_commit, parse_linux_procs_running};
    #[cfg(target_os = "linux")]
    use crate::cpu::RunQueueEwma;
    use crate::math::clamped_rate_per_sec;
    use crate::{CollectError, SnapshotSource};

    const USERS_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

    fn process_refresh_kind() -> ProcessRefreshKind {
        ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory()
            .with_disk_usage()
    }

    fn cpu_refresh_kind() -> CpuRefreshKind {
        CpuRefreshKind::everything()
    }

    fn disk_refresh_kind() -> DiskRefreshKind {
        DiskRefreshKind::everything()
    }

    fn disk_io_rate_bytes_per_sec(
        prev_totals: Option<(u64, u64)>,
        curr_read_total: u64,
        curr_write_total: u64,
        elapsed_ms: u64,
    ) -> (Option<u64>, Option<u64>) {
        // If total cumulative I/O counters are both zero across refreshes, this OS/device does
        // not report per-disk byte counters; represent them honestly as `None` rather than 0.
        let Some((prev_r, prev_w)) = prev_totals else {
            return (None, None);
        };
        if elapsed_ms == 0
            || (prev_r == 0 && prev_w == 0 && curr_read_total == 0 && curr_write_total == 0)
        {
            return (None, None);
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let r_rate = clamped_rate_per_sec(prev_r, curr_read_total, elapsed_ms).round() as u64;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let w_rate = clamped_rate_per_sec(prev_w, curr_write_total, elapsed_ms).round() as u64;
        (Some(r_rate), Some(w_rate))
    }

    fn sample_unix_disks(
        disks: &mut Disks,
        prev_disk_totals: &mut HashMap<String, (u64, u64)>,
        elapsed_ms: u64,
    ) -> Vec<btop_model::DiskSnapshot> {
        disks.refresh_specifics(true, disk_refresh_kind());
        let mut next_totals = HashMap::with_capacity(disks.len());
        let mut raw = Vec::with_capacity(disks.len());

        for disk in disks.iter() {
            let mount_path = disk.mount_point();
            let mount = mount_path.to_string_lossy().into_owned();
            let fs_type = disk.file_system().to_string_lossy().into_owned();
            // On macOS, `sysinfo::Disk::name()` is the volume label rather than the BSD mount
            // device; resolve the actual `/dev/diskNsM` path via `statfs(2)` so APFS container
            // deduplication keys on real kernel containers.
            let device = unix_mount_device_name(mount_path)
                .unwrap_or_else(|| disk.name().to_string_lossy().into_owned());
            let usage = disk.usage();
            let prev = prev_disk_totals.get(&mount).copied();
            let (read_rate, write_rate) = disk_io_rate_bytes_per_sec(
                prev,
                usage.total_read_bytes,
                usage.total_written_bytes,
                elapsed_ms,
            );
            next_totals.insert(
                mount.clone(),
                (usage.total_read_bytes, usage.total_written_bytes),
            );
            raw.push(RawUnixDisk {
                device_name: device,
                mount_point: mount,
                fs_type,
                total_bytes: disk.total_space(),
                available_bytes: disk.available_space(),
                read_bytes_per_sec: read_rate,
                write_bytes_per_sec: write_rate,
            });
        }

        *prev_disk_totals = next_totals;
        deduplicate_unix_disks(raw)
    }

    fn sample_unix_networks(
        networks: &mut Networks,
        prev_net_totals: &mut HashMap<String, (u64, u64)>,
        elapsed_ms: u64,
    ) -> Vec<NetInterfaceSnapshot> {
        networks.refresh(true);
        let mut next_totals = HashMap::with_capacity(networks.iter().count());
        let mut out = Vec::new();

        for (raw_name, data) in networks.iter() {
            let name = raw_name.to_string_lossy().into_owned();
            let rx_total = data.total_received();
            let tx_total = data.total_transmitted();
            next_totals.insert(name.clone(), (rx_total, tx_total));

            if !should_include_unix_interface(&name) {
                continue;
            }
            #[cfg(target_os = "linux")]
            if std::path::Path::new("/sys/class/net").exists()
                && !std::path::Path::new(&format!("/sys/class/net/{name}/device")).exists()
            {
                // On Linux, only hardware-backed NICs expose `/sys/class/net/<name>/device`;
                // VLANs, bridges, veths, bonds, and tunnels live under `/sys/devices/virtual`
                // and are excluded to avoid double-counting traffic with their physical parent.
                continue;
            }

            let (rx_rate, tx_rate) = if let Some(&(prev_rx, prev_tx)) = prev_net_totals.get(&name) {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let rx = clamped_rate_per_sec(prev_rx, rx_total, elapsed_ms).round() as u64;
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let tx = clamped_rate_per_sec(prev_tx, tx_total, elapsed_ms).round() as u64;
                (rx, tx)
            } else {
                (0, 0)
            };

            let mut ipv4 = None;
            let mut ipv6 = None;
            for net in data.ip_networks() {
                match net.addr {
                    IpAddr::V4(v4) if ipv4.is_none() => ipv4 = Some(v4.to_string()),
                    IpAddr::V6(v6) if ipv6.is_none() && !v6.is_loopback() => {
                        ipv6 = Some(v6.to_string());
                    }
                    _ => {}
                }
            }

            let is_up = match data.operational_state() {
                InterfaceOperationalState::Up => true,
                InterfaceOperationalState::Down => false,
                _ => ipv4.is_some() || ipv6.is_some() || rx_total > 0 || tx_total > 0,
            };
            let link_speed_bps = data
                .transmit_link_speed()
                .or_else(|| data.receive_link_speed())
                .filter(|&speed| speed > 0);

            out.push(NetInterfaceSnapshot {
                luid: stable_interface_luid(&name),
                description: name.clone(),
                name,
                is_up,
                ipv4,
                ipv6,
                rx_bytes_per_sec: rx_rate,
                tx_bytes_per_sec: tx_rate,
                rx_total_bytes: rx_total,
                tx_total_bytes: tx_total,
                link_speed_bps,
            });
        }

        *prev_net_totals = next_totals;
        out.sort_by(|a, b| b.is_up.cmp(&a.is_up).then_with(|| a.name.cmp(&b.name)));
        out
    }

    /// Lightweight Linux and macOS Fleet Agent collector polling strictly CPU usage, memory,
    /// disks, and physical network interfaces without touching processes or users.
    #[derive(Debug)]
    pub struct UnixAgentCollector {
        sys: System,
        disks: Disks,
        networks: Networks,
        cpu_baseline_at: Instant,
        cpu_measured: bool,
        last_sample_at: Instant,
        prev_net_totals: HashMap<String, (u64, u64)>,
        prev_disk_totals: HashMap<String, (u64, u64)>,
    }

    impl UnixAgentCollector {
        pub fn new() -> Result<Self, CollectError> {
            let initial_refresh = RefreshKind::nothing()
                .with_cpu(CpuRefreshKind::nothing().with_usage())
                .with_memory(MemoryRefreshKind::everything());
            let sys = System::new_with_specifics(initial_refresh).map_err(|err| {
                CollectError::Platform(format!("sysinfo agent system init failed: {err}"))
            })?;
            let disks =
                Disks::new_with_refreshed_list_specifics(disk_refresh_kind()).map_err(|err| {
                    CollectError::Platform(format!("sysinfo agent disks init failed: {err}"))
                })?;
            let networks = Networks::new_with_refreshed_list().map_err(|err| {
                CollectError::Platform(format!("sysinfo agent networks init failed: {err}"))
            })?;

            let prev_net_totals = networks
                .iter()
                .map(|(name, data)| {
                    (
                        name.to_string_lossy().into_owned(),
                        (data.total_received(), data.total_transmitted()),
                    )
                })
                .collect();
            let prev_disk_totals = disks
                .iter()
                .map(|disk| {
                    let usage = disk.usage();
                    (
                        disk.mount_point().to_string_lossy().into_owned(),
                        (usage.total_read_bytes, usage.total_written_bytes),
                    )
                })
                .collect();

            let now = Instant::now();
            Ok(Self {
                sys,
                disks,
                networks,
                cpu_baseline_at: now,
                cpu_measured: false,
                last_sample_at: now,
                prev_net_totals,
                prev_disk_totals,
            })
        }

        pub fn sample_node(
            &mut self,
            timestamp_ms: u64,
        ) -> Result<btop_model::NodeSample, CollectError> {
            let now = Instant::now();
            let elapsed_ms = u64::try_from(
                now.saturating_duration_since(self.last_sample_at)
                    .as_millis(),
            )
            .unwrap_or(u64::MAX);
            self.last_sample_at = now;

            let cpu_ready = self.cpu_measured
                || now.saturating_duration_since(self.cpu_baseline_at)
                    >= MINIMUM_CPU_UPDATE_INTERVAL;
            let cpu_busy_pct = if cpu_ready {
                self.sys
                    .refresh_cpu_specifics(CpuRefreshKind::nothing().with_usage());
                self.cpu_measured = true;
                DataState::Available(normalize_unix_cpu_pct(self.sys.global_cpu_usage()))
            } else {
                DataState::WarmingUp
            };

            self.sys.refresh_memory();
            let mem = compute_unix_mem_snapshot(
                self.sys.total_memory(),
                self.sys.available_memory(),
                self.sys.free_memory(),
                self.sys.total_swap(),
                self.sys.used_swap(),
                None,
            );

            let disks = sample_unix_disks(&mut self.disks, &mut self.prev_disk_totals, elapsed_ms);
            let (disk_max_used_pct, disk_io) =
                crate::agent::summarize_disks_for_node_sample(&disks, elapsed_ms > 0);

            let ifaces =
                sample_unix_networks(&mut self.networks, &mut self.prev_net_totals, elapsed_ms);
            let net_io = crate::agent::summarize_net_for_node_sample(&ifaces, elapsed_ms > 0);

            Ok(btop_model::NodeSample {
                timestamp_ms,
                cpu_busy_pct,
                mem_used_bytes: mem.used_bytes,
                mem_total_bytes: mem.total_bytes,
                disk_max_used_pct,
                disk_io,
                net_io,
                uptime_secs: System::uptime().unwrap_or(0),
            })
        }
    }

    /// Live Linux and macOS telemetry collector backed by targeted single-threaded `sysinfo`
    /// refreshes, marking GPU, OS services, battery, and CPU thermal sensors `Unavailable` in v1.
    #[derive(Debug)]
    pub struct UnixLiveCollector {
        hostname: String,
        username: String,
        sys: System,
        disks: Disks,
        networks: Networks,
        users: Users,
        cpu_baseline_at: Instant,
        cpu_measured: bool,
        #[cfg(target_os = "linux")]
        runq: RunQueueEwma,
        last_sample_at: Instant,
        last_users_refresh: Instant,
        prev_net_totals: HashMap<String, (u64, u64)>,
        prev_disk_totals: HashMap<String, (u64, u64)>,
    }

    impl UnixLiveCollector {
        /// Initializes `sysinfo` handles and primes CPU, disk, network, and process baselines.
        pub fn new() -> Result<Self, CollectError> {
            let initial_refresh = RefreshKind::nothing()
                .with_cpu(cpu_refresh_kind())
                .with_memory(MemoryRefreshKind::everything())
                .with_processes(process_refresh_kind());
            let sys = System::new_with_specifics(initial_refresh).map_err(|err| {
                CollectError::Platform(format!("sysinfo system init failed: {err}"))
            })?;
            let disks =
                Disks::new_with_refreshed_list_specifics(disk_refresh_kind()).map_err(|err| {
                    CollectError::Platform(format!("sysinfo disks init failed: {err}"))
                })?;
            let networks = Networks::new_with_refreshed_list().map_err(|err| {
                CollectError::Platform(format!("sysinfo networks init failed: {err}"))
            })?;
            let users = Users::new_with_refreshed_list().map_err(|err| {
                CollectError::Platform(format!("sysinfo users init failed: {err}"))
            })?;

            let hostname = System::host_name()
                .ok()
                .filter(|h| !h.trim().is_empty())
                .or_else(|| std::env::var("HOSTNAME").ok())
                .unwrap_or_else(|| "unix-host".to_owned());
            let username = std::env::var("USER")
                .or_else(|_| std::env::var("LOGNAME"))
                .unwrap_or_else(|_| "user".to_owned());

            let prev_net_totals = networks
                .iter()
                .map(|(name, data)| {
                    (
                        name.to_string_lossy().into_owned(),
                        (data.total_received(), data.total_transmitted()),
                    )
                })
                .collect();
            let prev_disk_totals = disks
                .iter()
                .map(|disk| {
                    let usage = disk.usage();
                    (
                        disk.mount_point().to_string_lossy().into_owned(),
                        (usage.total_read_bytes, usage.total_written_bytes),
                    )
                })
                .collect();

            let now = Instant::now();
            Ok(Self {
                hostname,
                username,
                sys,
                disks,
                networks,
                users,
                cpu_baseline_at: now,
                cpu_measured: false,
                #[cfg(target_os = "linux")]
                runq: RunQueueEwma::default(),
                last_sample_at: now,
                last_users_refresh: now,
                prev_net_totals,
                prev_disk_totals,
            })
        }

        #[cfg(target_os = "linux")]
        fn sample_run_queue_ewma(&mut self, elapsed_ms: u64) -> [f32; 3] {
            let runnable = std::fs::read_to_string("/proc/stat")
                .ok()
                .and_then(|s| parse_linux_procs_running(&s))
                .map_or(0, |n| n.saturating_sub(1));
            #[allow(clippy::cast_precision_loss)]
            let dt_secs = (elapsed_ms as f64) / 1000.0;
            self.runq.update(runnable, dt_secs)
        }

        #[cfg(not(target_os = "linux"))]
        #[allow(clippy::unused_self)]
        fn sample_run_queue_ewma(&mut self, _elapsed_ms: u64) -> [f32; 3] {
            // On macOS, XNU `getloadavg` counts only runnable CPU threads (unlike Linux `/proc/loadavg`
            // which includes uninterruptible disk I/O waiters).
            System::load_average().ok().map_or([0.0; 3], |la| {
                #[allow(clippy::cast_possible_truncation)]
                [
                    if la.one.is_finite() {
                        la.one as f32
                    } else {
                        0.0
                    },
                    if la.five.is_finite() {
                        la.five as f32
                    } else {
                        0.0
                    },
                    if la.fifteen.is_finite() {
                        la.fifteen as f32
                    } else {
                        0.0
                    },
                ]
            })
        }

        fn sample_cpu(&mut self, elapsed_ms: u64) -> CpuSnapshot {
            let cpus = self.sys.cpus();
            let model_name = cpus
                .first()
                .map(|c| c.brand().trim())
                .filter(|s| !s.is_empty())
                .map_or_else(
                    || {
                        if cfg!(target_os = "macos") {
                            "Apple Silicon CPU".to_owned()
                        } else {
                            "Linux CPU".to_owned()
                        }
                    },
                    ToOwned::to_owned,
                );

            let mut freq_sum = 0u64;
            let mut freq_count = 0u64;
            let cores: Vec<CoreSample> = cpus
                .iter()
                .enumerate()
                .map(|(idx, cpu)| {
                    let mhz = cpu.frequency();
                    if mhz > 0 {
                        freq_sum = freq_sum.saturating_add(mhz);
                        freq_count += 1;
                    }
                    CoreSample {
                        id: LogicalCpuId {
                            group: u16::try_from(idx / 64).unwrap_or(u16::MAX),
                            number: u8::try_from(idx % 64).unwrap_or(0),
                        },
                        busy_pct: normalize_unix_cpu_pct(cpu.usage()),
                        kind: CoreKind::Uniform,
                        temp_celsius: None,
                    }
                })
                .collect();

            let freq_mhz = freq_sum
                .checked_div(freq_count)
                .and_then(|v| u32::try_from(v).ok());

            let run_queue_ewma = self.sample_run_queue_ewma(elapsed_ms);

            CpuSnapshot {
                model_name,
                total_busy_pct: normalize_unix_cpu_pct(self.sys.global_cpu_usage()),
                utility_pct: None,
                freq_mhz,
                freq_range_mhz: None,
                package_temp_celsius: None,
                package_watts: None,
                run_queue_ewma,
                uptime_secs: System::uptime().unwrap_or(0),
                cores,
            }
        }

        fn sample_disks(&mut self, elapsed_ms: u64) -> Vec<btop_model::DiskSnapshot> {
            sample_unix_disks(&mut self.disks, &mut self.prev_disk_totals, elapsed_ms)
        }

        fn sample_networks(&mut self, elapsed_ms: u64) -> Vec<NetInterfaceSnapshot> {
            sample_unix_networks(&mut self.networks, &mut self.prev_net_totals, elapsed_ms)
        }

        fn sample_processes(&self, logical_cpus: usize) -> Vec<ProcessEntry> {
            let mut entries = Vec::with_capacity(self.sys.processes().len());
            for (pid, proc_info) in self.sys.processes() {
                // On Linux, `sysinfo` exposes individual userland threads (`ThreadKind::Userland`)
                // alongside thread-group leaders. Skip them so multi-threaded processes do not
                // appear as duplicate rows with repeated RSS or un-signalable TIDs.
                if matches!(proc_info.thread_kind(), Some(sysinfo::ThreadKind::Userland)) {
                    continue;
                }
                let raw_pid = pid.as_u32();
                // Query bracketed live kernel metadata for `raw_pid` directly so `name`,
                // `parent_pid`, `image_path`, `command_line`, `user`, and `create_time_100ns`
                // all belong to the exact same kernel process instance (preventing mixed rows on
                // PID reuse and preserving processes that rename themselves via `prctl`).
                let kernel_meta = match query_unix_process_meta(raw_pid) {
                    Ok(meta) => Some(meta),
                    Err(ProcActionError::NotFound(_)) => continue,
                    Err(_) => None,
                };
                let disk_io = proc_info.disk_usage();
                if let Some(meta) = kernel_meta {
                    let user = meta.uid.and_then(|raw_uid| {
                        self.users
                            .iter()
                            .find(|u| **u.id() == raw_uid)
                            .map(|u| u.name().to_owned())
                    });
                    let is_kernel = meta.is_kernel_thread
                        || proc_info
                            .thread_kind()
                            .is_some_and(|k| matches!(k, sysinfo::ThreadKind::Kernel));
                    entries.push(ProcessEntry {
                        identity: ProcessIdentity {
                            pid: raw_pid,
                            create_time_100ns: meta.create_time_100ns,
                        },
                        parent_pid: meta.parent_pid,
                        name: meta.comm,
                        image_path: meta.image_path,
                        command_line: meta.command_line,
                        command_line_denied: false,
                        user,
                        user_denied: false,
                        threads: meta.num_threads.max(1),
                        cpu_pct: normalize_unix_proc_cpu_pct(proc_info.cpu_usage(), logical_cpus),
                        working_set_private_bytes: meta
                            .private_rss_bytes
                            .unwrap_or_else(|| proc_info.memory()),
                        io_read_bytes: disk_io.total_read_bytes,
                        io_write_bytes: disk_io.total_written_bytes,
                        is_protected: raw_pid <= 1 || is_kernel,
                    });
                } else {
                    // Fail closed (`create_time_100ns = 0`) when kernel metadata cannot be read
                    // so unverified rows stay display-only and can never be signaled.
                    let parent_pid = proc_info.parent().map_or(0, sysinfo::Pid::as_u32);
                    let name = proc_info.name().to_string_lossy().into_owned();
                    let threads = proc_info.tasks().map_or(1, |tasks| {
                        u32::try_from(tasks.len()).unwrap_or(u32::MAX).max(1)
                    });
                    let is_kernel = proc_info
                        .thread_kind()
                        .is_some_and(|k| matches!(k, sysinfo::ThreadKind::Kernel));
                    entries.push(ProcessEntry {
                        identity: ProcessIdentity {
                            pid: raw_pid,
                            create_time_100ns: 0,
                        },
                        parent_pid,
                        name,
                        image_path: None,
                        command_line: None,
                        command_line_denied: true,
                        user: None,
                        user_denied: true,
                        threads,
                        cpu_pct: normalize_unix_proc_cpu_pct(proc_info.cpu_usage(), logical_cpus),
                        working_set_private_bytes: proc_info.memory(),
                        io_read_bytes: disk_io.total_read_bytes,
                        io_write_bytes: disk_io.total_written_bytes,
                        is_protected: raw_pid <= 1 || is_kernel,
                    });
                }
            }
            entries
        }
    }

    impl SnapshotSource for UnixLiveCollector {
        fn next_snapshot(&mut self) -> Result<Snapshot, CollectError> {
            let now = Instant::now();
            let elapsed_ms = u64::try_from(
                now.saturating_duration_since(self.last_sample_at)
                    .as_millis(),
            )
            .unwrap_or(u64::MAX);
            self.last_sample_at = now;

            if now.saturating_duration_since(self.last_users_refresh) >= USERS_REFRESH_INTERVAL {
                self.users.refresh();
                self.last_users_refresh = now;
            }

            let cpu_ready = self.cpu_measured
                || now.saturating_duration_since(self.cpu_baseline_at)
                    >= MINIMUM_CPU_UPDATE_INTERVAL;
            if cpu_ready {
                self.sys.refresh_cpu_specifics(cpu_refresh_kind());
                self.cpu_measured = true;
            }
            self.sys
                .refresh_memory_specifics(MemoryRefreshKind::everything());
            self.sys.refresh_processes_specifics(
                ProcessesToUpdate::All,
                true,
                process_refresh_kind(),
            );

            let timestamp_ms = u64::try_from(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis()),
            )
            .unwrap_or(u64::MAX);

            let logical_cpus = self.sys.cpus().len().max(1);
            let cpu_state = if cpu_ready {
                DataState::Available(self.sample_cpu(elapsed_ms))
            } else {
                DataState::WarmingUp
            };
            #[cfg(target_os = "linux")]
            let commit = std::fs::read_to_string("/proc/meminfo")
                .ok()
                .and_then(|s| parse_linux_meminfo_commit(&s));
            #[cfg(not(target_os = "linux"))]
            let commit = None;
            let mem = compute_unix_mem_snapshot(
                self.sys.total_memory(),
                self.sys.available_memory(),
                self.sys.free_memory(),
                self.sys.total_swap(),
                self.sys.used_swap(),
                commit,
            );
            let total_phys_mem_bytes = (mem.total_bytes > 0).then_some(mem.total_bytes);
            let disks = self.sample_disks(elapsed_ms);
            let net = self.sample_networks(elapsed_ms);
            let processes = self.sample_processes(logical_cpus);

            Ok(Snapshot {
                timestamp_ms,
                hostname: self.hostname.clone(),
                username: self.username.clone(),
                total_phys_mem_bytes,
                cpu: cpu_state,
                mem: DataState::Available(mem),
                disks: DataState::Available(disks),
                net: DataState::Available(net),
                processes: DataState::Available(processes),
                services: DataState::Unavailable(UNIX_SERVICES_UNAVAILABLE_REASON.to_owned()),
                gpus: vec![DataState::Unavailable(
                    UNIX_GPUS_UNAVAILABLE_REASON.to_owned(),
                )],
                battery: DataState::Unavailable(
                    "Battery telemetry is unavailable on Linux and macOS in v1".to_owned(),
                ),
            })
        }

        fn allows_live_actions(&self) -> bool {
            true
        }
    }
}

#[cfg(unix)]
pub use live::{UnixAgentCollector, UnixLiveCollector};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_noisy_unix_filesystems_and_mounts() {
        assert!(should_include_unix_disk(
            "/",
            "ext4",
            "/dev/nvme0n1p2",
            500_000_000_000
        ));
        assert!(should_include_unix_disk(
            "/home",
            "btrfs",
            "/dev/nvme0n1p2",
            500_000_000_000
        ));
        assert!(should_include_unix_disk(
            "/Volumes/SSD",
            "apfs",
            "/dev/disk4s1",
            1_000_000_000_000
        ));
        assert!(should_include_unix_disk(
            "/run/media/franz/ExternalUSB",
            "ext4",
            "/dev/sdb1",
            2_000_000_000_000
        ));

        assert!(!should_include_unix_disk(
            "/run",
            "tmpfs",
            "tmpfs",
            2_000_000_000
        ));
        assert!(!should_include_unix_disk(
            "/snap/core22/100",
            "squashfs",
            "/dev/loop0",
            100_000_000
        ));
        assert!(!should_include_unix_disk(
            "/var/lib/docker/overlay2/abc",
            "overlay",
            "overlay",
            500_000_000_000
        ));
        assert!(!should_include_unix_disk(
            "/sys/firmware/efi/efivars",
            "efivarfs",
            "efivarfs",
            128_000
        ));
        assert!(!should_include_unix_disk(
            "/System/Volumes/VM",
            "apfs",
            "/dev/disk3s6",
            926_000_000_000
        ));
        assert!(!should_include_unix_disk(
            "/System/Volumes/Preboot",
            "apfs",
            "/dev/disk3s2",
            926_000_000_000
        ));
        assert!(!should_include_unix_disk(
            "/Library/Developer/CoreSimulator/Volumes/iOS_23C54",
            "apfs",
            "/dev/disk7s1",
            16_000_000_000
        ));
        assert!(!should_include_unix_disk("/dev", "devfs", "devfs", 200_000));
        assert!(!should_include_unix_disk("/", "ext4", "/dev/sda1", 0));
    }

    #[test]
    fn deduplicates_macos_apfs_containers_and_linux_btrfs_subvolumes() {
        let mac_disks = deduplicate_unix_disks(vec![
            RawUnixDisk {
                device_name: "/dev/disk3s5".to_owned(),
                mount_point: "/System/Volumes/Data".to_owned(),
                fs_type: "apfs".to_owned(),
                total_bytes: 994_000_000_000,
                available_bytes: 394_000_000_000,
                read_bytes_per_sec: Some(1024),
                write_bytes_per_sec: Some(2048),
            },
            RawUnixDisk {
                device_name: "/dev/disk3s1s1".to_owned(),
                mount_point: "/".to_owned(),
                fs_type: "apfs".to_owned(),
                total_bytes: 994_000_000_000,
                available_bytes: 394_000_000_000,
                read_bytes_per_sec: None,
                write_bytes_per_sec: None,
            },
            RawUnixDisk {
                device_name: "/dev/disk3s6".to_owned(),
                mount_point: "/System/Volumes/VM".to_owned(),
                fs_type: "apfs".to_owned(),
                total_bytes: 994_000_000_000,
                available_bytes: 394_000_000_000,
                read_bytes_per_sec: None,
                write_bytes_per_sec: None,
            },
            RawUnixDisk {
                device_name: "/dev/disk5s1".to_owned(),
                mount_point: "/Volumes/External".to_owned(),
                fs_type: "apfs".to_owned(),
                total_bytes: 2_000_000_000_000,
                available_bytes: 1_500_000_000_000,
                read_bytes_per_sec: None,
                write_bytes_per_sec: None,
            },
        ]);
        assert_eq!(mac_disks.len(), 2);
        assert_eq!(mac_disks[0].mount, "/");
        assert_eq!(mac_disks[0].total_bytes, 994_000_000_000);
        assert_eq!(mac_disks[0].used_bytes, 600_000_000_000);
        assert_eq!(mac_disks[0].read_bytes_per_sec, Some(1024));
        assert_eq!(mac_disks[1].mount, "/Volumes/External");

        // macOS `sysinfo` reports volume labels (e.g. `"Macintosh HD"`) rather than `/dev/diskNsM`.
        let mac_sysinfo_disks = deduplicate_unix_disks(vec![
            RawUnixDisk {
                device_name: "Macintosh HD".to_owned(),
                mount_point: "/".to_owned(),
                fs_type: "apfs".to_owned(),
                total_bytes: 994_610_155_520,
                available_bytes: 396_826_159_103,
                read_bytes_per_sec: Some(25_206),
                write_bytes_per_sec: Some(4_096),
            },
            RawUnixDisk {
                device_name: "Macintosh HD".to_owned(),
                mount_point: "/System/Volumes/Data".to_owned(),
                fs_type: "apfs".to_owned(),
                total_bytes: 994_610_155_520,
                available_bytes: 396_826_155_007,
                read_bytes_per_sec: Some(25_206),
                write_bytes_per_sec: Some(4_096),
            },
            RawUnixDisk {
                device_name: "External SSD".to_owned(),
                mount_point: "/Volumes/External SSD".to_owned(),
                fs_type: "apfs".to_owned(),
                total_bytes: 2_000_000_000_000,
                available_bytes: 1_200_000_000_000,
                read_bytes_per_sec: None,
                write_bytes_per_sec: None,
            },
        ]);
        assert_eq!(mac_sysinfo_disks.len(), 2);
        assert_eq!(mac_sysinfo_disks[0].mount, "/");
        assert_eq!(mac_sysinfo_disks[0].total_bytes, 994_610_155_520);
        assert_eq!(mac_sysinfo_disks[1].mount, "/Volumes/External SSD");

        // Linux Btrfs subvolumes on one block device collapse to `/`.
        let linux_disks = deduplicate_unix_disks(vec![
            RawUnixDisk {
                device_name: "/dev/nvme0n1p2".to_owned(),
                mount_point: "/home".to_owned(),
                fs_type: "btrfs".to_owned(),
                total_bytes: 500_000_000_000,
                available_bytes: 200_000_000_000,
                read_bytes_per_sec: None,
                write_bytes_per_sec: None,
            },
            RawUnixDisk {
                device_name: "/dev/nvme0n1p2".to_owned(),
                mount_point: "/".to_owned(),
                fs_type: "btrfs".to_owned(),
                total_bytes: 500_000_000_000,
                available_bytes: 200_000_000_000,
                read_bytes_per_sec: None,
                write_bytes_per_sec: None,
            },
        ]);
        assert_eq!(linux_disks.len(), 1);
        assert_eq!(linux_disks[0].mount, "/");
    }

    #[test]
    fn keeps_independent_apfs_disks_distinct_while_deduplicating_container_siblings() {
        // Two independent APFS volumes with identical label and capacity on different containers
        // (or without `/dev/diskN` device paths) must stay distinct, while sibling volumes sharing
        // one non-root container (`/dev/disk4s1` and `/dev/disk4s2`) must collapse into one row.
        let mac_multi_container = deduplicate_unix_disks(vec![
            RawUnixDisk {
                device_name: "/dev/disk4s1".to_owned(),
                mount_point: "/Volumes/DataA".to_owned(),
                fs_type: "apfs".to_owned(),
                total_bytes: 2_000_000_000_000,
                available_bytes: 1_200_000_000_000,
                read_bytes_per_sec: None,
                write_bytes_per_sec: None,
            },
            RawUnixDisk {
                device_name: "/dev/disk4s2".to_owned(),
                mount_point: "/Volumes/DataB".to_owned(),
                fs_type: "apfs".to_owned(),
                total_bytes: 2_000_000_000_000,
                available_bytes: 1_200_000_000_000,
                read_bytes_per_sec: None,
                write_bytes_per_sec: None,
            },
            RawUnixDisk {
                device_name: "/dev/disk5s1".to_owned(),
                mount_point: "/Volumes/Backup".to_owned(),
                fs_type: "apfs".to_owned(),
                total_bytes: 2_000_000_000_000,
                available_bytes: 1_200_000_000_000,
                read_bytes_per_sec: None,
                write_bytes_per_sec: None,
            },
            RawUnixDisk {
                device_name: "SameLabel".to_owned(),
                mount_point: "/Volumes/DiskOne".to_owned(),
                fs_type: "apfs".to_owned(),
                total_bytes: 1_000_000_000_000,
                available_bytes: 500_000_000_000,
                read_bytes_per_sec: None,
                write_bytes_per_sec: None,
            },
            RawUnixDisk {
                device_name: "SameLabel".to_owned(),
                mount_point: "/Volumes/DiskTwo".to_owned(),
                fs_type: "apfs".to_owned(),
                total_bytes: 1_000_000_000_000,
                available_bytes: 500_000_000_000,
                read_bytes_per_sec: None,
                write_bytes_per_sec: None,
            },
        ]);
        assert_eq!(mac_multi_container.len(), 4);
        assert!(
            mac_multi_container
                .iter()
                .any(|d| d.mount == "/Volumes/DataA")
        );
        assert!(
            !mac_multi_container
                .iter()
                .any(|d| d.mount == "/Volumes/DataB")
        );
        assert!(
            mac_multi_container
                .iter()
                .any(|d| d.mount == "/Volumes/Backup")
        );
        assert!(
            mac_multi_container
                .iter()
                .any(|d| d.mount == "/Volumes/DiskOne")
        );
        assert!(
            mac_multi_container
                .iter()
                .any(|d| d.mount == "/Volumes/DiskTwo")
        );
    }

    #[test]
    fn filters_loopback_tailscale_docker_and_virtual_unix_interfaces() {
        for physical in [
            "eth0", "enp2s0", "eno1", "ens33", "wlan0", "wlp3s0", "en0", "en1",
        ] {
            assert!(
                should_include_unix_interface(physical),
                "{physical} should be included as a physical NIC"
            );
        }
        for excluded in [
            "lo",
            "lo0",
            "tailscale0",
            "utun0",
            "utun8",
            "docker0",
            "veth4a1b2c3",
            "br0",
            "br1",
            "br-98765432",
            "vmbr0",
            "virbr0",
            "enp2s0.100",
            "eth0.20",
            "vlan100",
            "vnet0",
            "wg0",
            "awdl0",
            "llw0",
            "anpi0",
            "bridge0",
            "gif0",
            "stf0",
            "ap1",
        ] {
            assert!(
                !should_include_unix_interface(excluded),
                "{excluded} should be excluded from physical NIC totals"
            );
        }
    }

    #[test]
    fn computes_unix_memory_and_cpu_normalization_accurately() {
        let mem = compute_unix_mem_snapshot(
            32_000_000_000,
            20_000_000_000,
            4_000_000_000,
            8_000_000_000,
            1_000_000_000,
            None,
        );
        assert_eq!(mem.used_bytes, 12_000_000_000);
        assert_eq!(mem.available_bytes, 20_000_000_000);
        assert_eq!(mem.free_bytes, 4_000_000_000);
        assert_eq!(mem.cached_bytes, 16_000_000_000);
        assert_eq!(mem.used_bytes + mem.available_bytes, mem.total_bytes);
        assert_eq!(mem.commit_used_bytes, 0);
        assert_eq!(mem.commit_limit_bytes, 0);
        assert_eq!(mem.pagefile_used_bytes, 1_000_000_000);
        assert_eq!(mem.pagefile_total_bytes, 8_000_000_000);

        let meminfo = "MemTotal: 16000000 kB\nCommitLimit: 12000000 kB\nCommitted_AS: 4000000 kB\n";
        assert_eq!(
            parse_linux_meminfo_commit(meminfo),
            Some((4_000_000 * 1024, 12_000_000 * 1024))
        );
        assert_eq!(parse_linux_meminfo_commit("MemTotal: 16000000 kB\n"), None);

        assert!((normalize_unix_cpu_pct(f32::NAN) - 0.0).abs() < f32::EPSILON);
        assert!((normalize_unix_cpu_pct(150.0) - 100.0).abs() < f32::EPSILON);
        assert!((normalize_unix_proc_cpu_pct(400.0, 8) - 50.0).abs() < 0.01);
        assert!((normalize_unix_proc_cpu_pct(-5.0, 8) - 0.0).abs() < f32::EPSILON);

        let proc_stat = "cpu 1 2 3 4\nbtime 1700000000\nprocs_running 5\nprocs_blocked 12\n";
        assert_eq!(parse_linux_procs_running(proc_stat), Some(5));
        assert_eq!(parse_linux_procs_running("cpu 1 2 3\n"), None);
        assert_eq!(
            btop_sys::proc_actions::parse_linux_proc_cmdline_bytes(b"/usr/bin/sleep\x0030\x00"),
            Some("/usr/bin/sleep 30".to_owned())
        );
        assert_eq!(
            btop_sys::proc_actions::parse_linux_proc_cmdline_bytes(b""),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn live_unix_collector_populates_core_metrics_and_marks_unsupported_unavailable() {
        use std::thread;
        use std::time::Duration;

        use btop_model::{
            DataState, HeadlessQuery, JsonSections, JsonSnapshotEnvelope, QueryFilter, SchemaMode,
            build_json_schema, validate_against_schema,
        };

        use crate::SnapshotSource as _;

        let mut collector = UnixLiveCollector::new().expect("init UnixLiveCollector");
        let warmup = collector.next_snapshot().expect("warmup tick");
        assert!(
            matches!(warmup.cpu, DataState::WarmingUp),
            "immediate first Unix sample inside MINIMUM_CPU_UPDATE_INTERVAL must report WarmingUp"
        );
        thread::sleep(Duration::from_millis(250));
        let snap = collector.next_snapshot().expect("measured tick");

        assert_ne!(snap.hostname, "");
        assert!(snap.total_phys_mem_bytes.is_some_and(|b| b > 0));

        match &snap.cpu {
            DataState::Available(cpu) => {
                assert_ne!(cpu.cores.len(), 0);
                assert!(cpu.total_busy_pct.is_finite());
                assert!(cpu.package_temp_celsius.is_none());
                assert!(cpu.package_watts.is_none());
            }
            other => panic!("expected Available CPU on Unix, got {other:?}"),
        }

        match &snap.mem {
            DataState::Available(mem) => {
                assert!(mem.total_bytes > 0);
                assert_eq!(mem.used_bytes + mem.available_bytes, mem.total_bytes);
            }
            other => panic!("expected Available memory on Unix, got {other:?}"),
        }

        assert!(matches!(snap.disks, DataState::Available(_)));
        assert!(matches!(snap.net, DataState::Available(_)));
        match &snap.processes {
            DataState::Available(procs) => {
                let my_pid = std::process::id();
                let mine = procs
                    .iter()
                    .find(|p| p.identity.pid == my_pid)
                    .expect("expected current process PID in Unix process list");
                assert!(
                    mine.identity.create_time_100ns > btop_sys::WINDOWS_TO_UNIX_EPOCH_100NS,
                    "Unix ProcessIdentity must use FILETIME 1601 epoch"
                );
                assert!(mine.threads >= 1);
            }
            other => panic!("expected Available processes on Unix, got {other:?}"),
        }

        // End-to-end verification: identity collected by `UnixLiveCollector` must match
        // `btop_sys::graceful_close_process` directly (independent of the guard's own helper).
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn disposable sleep child");
        let child_pid = child.id();
        let snap_with_child = collector.next_snapshot().expect("snapshot with child");
        let child_identity = match &snap_with_child.processes {
            DataState::Available(procs) => procs
                .iter()
                .find(|p| p.identity.pid == child_pid)
                .map(|p| p.identity)
                .expect("child process present in UnixLiveCollector snapshot"),
            other => panic!("expected Available processes, got {other:?}"),
        };
        assert_eq!(
            btop_sys::graceful_close_process(child_identity.pid, child_identity.create_time_100ns),
            Ok(1)
        );
        let status = child.wait().expect("reap signaled child");
        assert!(!status.success());

        assert!(matches!(snap.services, DataState::Unavailable(_)));
        assert!(matches!(snap.battery, DataState::Unavailable(_)));
        assert_ne!(snap.gpus.len(), 0);
        assert!(
            snap.gpus
                .iter()
                .all(|g| matches!(g, DataState::Unavailable(_)))
        );

        let envelope = JsonSnapshotEnvelope::from_snapshot_with_options(
            snap,
            "0.2.0",
            250,
            JsonSections::all(),
            &HeadlessQuery::default(),
            &QueryFilter::None,
            true,
        );
        let value = serde_json::to_value(&envelope).expect("serialize envelope");
        let schema = build_json_schema(SchemaMode::Snapshot);
        if let Err(errors) = validate_against_schema(&schema, &value) {
            panic!("Unix live snapshot failed JSON schema validation: {errors:?}");
        }
    }
}
