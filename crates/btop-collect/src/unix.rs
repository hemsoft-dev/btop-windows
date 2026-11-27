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
    if IGNORED_UNIX_MOUNT_PREFIXES
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
                format!("apfs:{container}:{}", disk.total_bytes)
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

/// Returns `true` for physical Unix network interfaces, excluding loopback (`lo`/`lo0`),
/// Tailscale and VPN tunnels (`tailscale*`, `utun*`, `tun*`, `wg*`), Docker/VM virtual bridges
/// (`docker*`, `veth*`, `br-*`, `virbr*`, `vnet*`), and Apple virtual peer interfaces.
pub fn should_include_unix_interface(name: &str) -> bool {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    if is_loopback_iface(&lower) {
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

/// Builds a consistent Unix [`MemSnapshot`] using `used = total - available` so Linux page
/// cache and macOS reclaimable memory do not falsely inflate memory pressure.
pub fn compute_unix_mem_snapshot(
    total_bytes: u64,
    available_bytes: u64,
    free_bytes: u64,
    swap_total_bytes: u64,
    swap_used_bytes: u64,
) -> MemSnapshot {
    let available = available_bytes.min(total_bytes);
    let free = free_bytes.min(available);
    let used = total_bytes.saturating_sub(available);
    let cached = available.saturating_sub(free);
    let pagefile_used = swap_used_bytes.min(swap_total_bytes);
    MemSnapshot {
        total_bytes,
        used_bytes: used,
        available_bytes: available,
        cached_bytes: cached,
        free_bytes: free,
        commit_used_bytes: used.saturating_add(pagefile_used),
        commit_limit_bytes: total_bytes.saturating_add(swap_total_bytes),
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

#[cfg(unix)]
mod live {
    use std::collections::HashMap;
    use std::net::IpAddr;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use btop_model::{
        CoreKind, CoreSample, CpuSnapshot, DataState, LogicalCpuId, NetInterfaceSnapshot,
        ProcessEntry, ProcessIdentity, Snapshot,
    };
    use btop_sys::unix_start_time_secs_to_identity;
    use sysinfo::{
        CpuRefreshKind, DiskRefreshKind, Disks, InterfaceOperationalState, MemoryRefreshKind,
        Networks, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System, UpdateKind, Users,
    };

    use super::{
        RawUnixDisk, compute_unix_mem_snapshot, deduplicate_unix_disks, normalize_unix_cpu_pct,
        normalize_unix_proc_cpu_pct, should_include_unix_interface, stable_interface_luid,
    };
    use crate::math::clamped_rate_per_sec;
    use crate::{CollectError, SnapshotSource};

    const USERS_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

    fn process_refresh_kind() -> ProcessRefreshKind {
        ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory()
            .with_disk_usage()
            .with_cmd(UpdateKind::OnlyIfNotSet)
            .with_exe(UpdateKind::OnlyIfNotSet)
            .with_user(UpdateKind::OnlyIfNotSet)
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
                last_sample_at: now,
                last_users_refresh: now,
                prev_net_totals,
                prev_disk_totals,
            })
        }

        fn sample_cpu(&self) -> CpuSnapshot {
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
                            group: 0,
                            number: u8::try_from(idx).unwrap_or(u8::MAX),
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

            let load_avg = System::load_average().ok().map_or([0.0; 3], |la| {
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
            });

            CpuSnapshot {
                model_name,
                total_busy_pct: normalize_unix_cpu_pct(self.sys.global_cpu_usage()),
                utility_pct: None,
                freq_mhz,
                freq_range_mhz: None,
                package_temp_celsius: None,
                package_watts: None,
                run_queue_ewma: load_avg,
                uptime_secs: System::uptime().unwrap_or(0),
                cores,
            }
        }

        fn sample_disks(&mut self, elapsed_ms: u64) -> Vec<btop_model::DiskSnapshot> {
            self.disks.refresh_specifics(true, disk_refresh_kind());
            let mut next_totals = HashMap::with_capacity(self.disks.len());
            let mut raw = Vec::with_capacity(self.disks.len());

            for disk in &self.disks {
                let mount = disk.mount_point().to_string_lossy().into_owned();
                let fs_type = disk.file_system().to_string_lossy().into_owned();
                let device = disk.name().to_string_lossy().into_owned();
                let usage = disk.usage();
                let prev = self.prev_disk_totals.get(&mount).copied();
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

            self.prev_disk_totals = next_totals;
            deduplicate_unix_disks(raw)
        }

        fn sample_networks(&mut self, elapsed_ms: u64) -> Vec<NetInterfaceSnapshot> {
            self.networks.refresh(true);
            let mut next_totals = HashMap::with_capacity(self.networks.iter().count());
            let mut out = Vec::new();

            for (raw_name, data) in &self.networks {
                let name = raw_name.to_string_lossy().into_owned();
                let rx_total = data.total_received();
                let tx_total = data.total_transmitted();
                next_totals.insert(name.clone(), (rx_total, tx_total));

                if !should_include_unix_interface(&name) {
                    continue;
                }

                let (rx_rate, tx_rate) =
                    if let Some(&(prev_rx, prev_tx)) = self.prev_net_totals.get(&name) {
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

            self.prev_net_totals = next_totals;
            out.sort_by(|a, b| b.is_up.cmp(&a.is_up).then_with(|| a.name.cmp(&b.name)));
            out
        }

        fn sample_processes(&self, logical_cpus: usize) -> Vec<ProcessEntry> {
            let mut entries = Vec::with_capacity(self.sys.processes().len());
            for (pid, proc_info) in self.sys.processes() {
                let raw_pid = pid.as_u32();
                let parent_pid = proc_info.parent().map_or(0, sysinfo::Pid::as_u32);
                let name = proc_info.name().to_string_lossy().into_owned();
                let image_path = proc_info.exe().map(|p| p.display().to_string());
                let cmd_parts: Vec<String> = proc_info
                    .cmd()
                    .iter()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect();
                let command_line = (!cmd_parts.is_empty()).then(|| cmd_parts.join(" "));
                let user = proc_info
                    .user_id()
                    .and_then(|uid| self.users.get_user_by_id(uid))
                    .map(|u| u.name().to_owned());
                let threads = proc_info.tasks().map_or(1, |tasks| {
                    u32::try_from(tasks.len()).unwrap_or(u32::MAX).max(1)
                });
                let disk_io = proc_info.disk_usage();
                let is_kernel = proc_info
                    .thread_kind()
                    .is_some_and(|k| matches!(k, sysinfo::ThreadKind::Kernel));
                entries.push(ProcessEntry {
                    identity: ProcessIdentity {
                        pid: raw_pid,
                        create_time_100ns: unix_start_time_secs_to_identity(proc_info.start_time()),
                    },
                    parent_pid,
                    name,
                    image_path,
                    command_line,
                    command_line_denied: false,
                    user,
                    user_denied: false,
                    threads,
                    cpu_pct: normalize_unix_proc_cpu_pct(proc_info.cpu_usage(), logical_cpus),
                    working_set_private_bytes: proc_info.memory(),
                    io_read_bytes: disk_io.total_read_bytes,
                    io_write_bytes: disk_io.total_written_bytes,
                    is_protected: raw_pid <= 1 || is_kernel,
                });
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

            self.sys.refresh_cpu_specifics(cpu_refresh_kind());
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

            let cpu = self.sample_cpu();
            let logical_cpus = cpu.cores.len().max(1);
            let mem = compute_unix_mem_snapshot(
                self.sys.total_memory(),
                self.sys.available_memory(),
                self.sys.free_memory(),
                self.sys.total_swap(),
                self.sys.used_swap(),
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
                cpu: DataState::Available(cpu),
                mem: DataState::Available(mem),
                disks: DataState::Available(disks),
                net: DataState::Available(net),
                processes: DataState::Available(processes),
                services: DataState::Unavailable(
                    "OS service manager telemetry is unavailable on Linux and macOS in v1"
                        .to_owned(),
                ),
                gpus: vec![DataState::Unavailable(
                    "GPU telemetry is unavailable on Linux and macOS in v1".to_owned(),
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
pub use live::UnixLiveCollector;

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
            "br-98765432",
            "virbr0",
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
        );
        assert_eq!(mem.used_bytes, 12_000_000_000);
        assert_eq!(mem.available_bytes, 20_000_000_000);
        assert_eq!(mem.free_bytes, 4_000_000_000);
        assert_eq!(mem.cached_bytes, 16_000_000_000);
        assert_eq!(mem.used_bytes + mem.available_bytes, mem.total_bytes);
        assert_eq!(mem.pagefile_used_bytes, 1_000_000_000);
        assert_eq!(mem.pagefile_total_bytes, 8_000_000_000);

        assert!((normalize_unix_cpu_pct(f32::NAN) - 0.0).abs() < f32::EPSILON);
        assert!((normalize_unix_cpu_pct(150.0) - 100.0).abs() < f32::EPSILON);
        assert!((normalize_unix_proc_cpu_pct(400.0, 8) - 50.0).abs() < 0.01);
        assert!((normalize_unix_proc_cpu_pct(-5.0, 8) - 0.0).abs() < f32::EPSILON);
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
        let _warmup = collector.next_snapshot().expect("warmup tick");
        thread::sleep(Duration::from_millis(250));
        let snap = collector.next_snapshot().expect("measured tick");

        assert!(!snap.hostname.is_empty());
        assert!(snap.total_phys_mem_bytes.is_some_and(|b| b > 0));

        match &snap.cpu {
            DataState::Available(cpu) => {
                assert!(!cpu.cores.is_empty());
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
                assert!(
                    procs.iter().any(|p| p.identity.pid == my_pid),
                    "expected current process PID {my_pid} in Unix process list"
                );
            }
            other => panic!("expected Available processes on Unix, got {other:?}"),
        }

        assert!(matches!(snap.services, DataState::Unavailable(_)));
        assert!(matches!(snap.battery, DataState::Unavailable(_)));
        assert!(!snap.gpus.is_empty());
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
