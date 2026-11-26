//! Pure time-window aggregation and spike-diagnosis summaries for headless `--json --summary`.

use std::cmp::Ordering;
use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::json_output::{JSON_SCHEMA_VERSION, JsonSections};
use crate::proc_query::{HeadlessQuery, QueryFilter, build_acyclic_adjacency};
use crate::{
    BatterySnapshot, CpuSnapshot, DataState, DiskSnapshot, GpuSnapshot, MemSnapshot,
    NetInterfaceSnapshot, ProcessEntry, ProcessIdentity, Snapshot,
};

/// Default number of top processes returned per ranking in `--summary` mode when `--top` is omitted.
pub const DEFAULT_SUMMARY_TOP: usize = 10;

/// Aggregate floating-point statistics (`min`, `avg`, `p95`, `max`) over valid finite samples.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ScalarStats {
    pub count: usize,
    pub min: f64,
    pub avg: f64,
    pub p95: f64,
    pub max: f64,
}

impl ScalarStats {
    /// Computes deterministic nearest-rank `p95` and summary statistics from `values`,
    /// ignoring non-finite (`NaN`/`Inf`) readings. Returns `None` if no finite readings exist.
    pub fn from_f64(values: &[f64]) -> Option<Self> {
        let mut sorted: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
        if sorted.is_empty() {
            return None;
        }
        sorted.sort_by(f64::total_cmp);
        let count = sorted.len();
        let sum: f64 = sorted.iter().sum();
        let p95_idx = nearest_rank_p95_index(count);
        #[allow(clippy::cast_precision_loss)]
        let avg = sum / (count as f64);
        Some(Self {
            count,
            min: sorted[0],
            avg,
            p95: sorted[p95_idx],
            max: sorted[count - 1],
        })
    }

    /// Computes summary statistics from `f32` readings, ignoring non-finite values.
    pub fn from_f32(values: &[f32]) -> Option<Self> {
        let promoted: Vec<f64> = values
            .iter()
            .copied()
            .filter(|v| v.is_finite())
            .map(f64::from)
            .collect();
        Self::from_f64(&promoted)
    }
}

/// Aggregate integer statistics (`min`, `avg`, `p95`, `max`) over `u64` readings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct U64Stats {
    pub count: usize,
    pub min: u64,
    pub avg: u64,
    pub p95: u64,
    pub max: u64,
}

impl U64Stats {
    /// Computes exact integer `min`, `max`, nearest-rank `p95`, and rounded mean via `u128`.
    pub fn from_u64(values: &[u64]) -> Option<Self> {
        if values.is_empty() {
            return None;
        }
        let mut sorted = values.to_vec();
        sorted.sort_unstable();
        let count = sorted.len();
        let sum: u128 = sorted.iter().map(|&v| u128::from(v)).sum();
        let count_u128 = u128::try_from(count).unwrap_or(1);
        let avg_u128 = (sum + (count_u128 / 2)) / count_u128;
        let avg = u64::try_from(avg_u128).unwrap_or(u64::MAX);
        let p95_idx = nearest_rank_p95_index(count);
        Some(Self {
            count,
            min: sorted[0],
            avg,
            p95: sorted[p95_idx],
            max: sorted[count - 1],
        })
    }
}

/// Nearest-rank 95th percentile index for a sorted non-empty slice of length `len`.
const fn nearest_rank_p95_index(len: usize) -> usize {
    if len <= 1 {
        return 0;
    }
    let rank = (len * 95).div_ceil(100);
    let idx = rank.saturating_sub(1);
    if idx >= len { len - 1 } else { idx }
}

/// Aggregated CPU metrics over a sample window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CpuSummary {
    pub model_name: String,
    pub logical_cores: usize,
    pub samples_available: usize,
    pub total_busy_pct: ScalarStats,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utility_pct: Option<ScalarStats>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freq_mhz: Option<ScalarStats>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_temp_celsius: Option<ScalarStats>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_watts: Option<ScalarStats>,
    pub run_queue_1m: ScalarStats,
}

/// Aggregated memory and Windows commit charge metrics over a sample window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemSummary {
    pub total_bytes: u64,
    pub samples_available: usize,
    pub used_bytes: U64Stats,
    pub available_bytes: U64Stats,
    pub commit_used_bytes: U64Stats,
    pub commit_limit_bytes: U64Stats,
    pub pagefile_used_bytes: U64Stats,
}

/// Aggregated per-volume disk capacity, throughput, and utilization metrics over a window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiskSummary {
    pub mount: String,
    pub label: String,
    pub fs_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub physical_drive: Option<u32>,
    pub samples_available: usize,
    pub total_bytes: u64,
    pub free_bytes: U64Stats,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_bytes_per_sec: Option<U64Stats>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_bytes_per_sec: Option<U64Stats>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub busy_pct: Option<ScalarStats>,
}

/// Aggregated per-adapter network throughput and counter-rollback-safe byte deltas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetSummary {
    pub luid: u64,
    pub name: String,
    pub description: String,
    pub samples_available: usize,
    pub rx_bytes_per_sec: U64Stats,
    pub tx_bytes_per_sec: U64Stats,
    pub rx_delta_bytes: u64,
    pub tx_delta_bytes: u64,
}

/// Aggregated per-GPU utilization, VRAM, power, clock, and thermal metrics over a window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuSummary {
    pub index: u8,
    pub name: String,
    pub vendor: String,
    pub samples_available: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utilization_pct: Option<ScalarStats>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vram_used_bytes: Option<U64Stats>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vram_total_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power_watts: Option<ScalarStats>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temp_celsius: Option<ScalarStats>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core_clock_mhz: Option<ScalarStats>,
}

/// Aggregated battery summary over a sample window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatterySummary {
    pub samples_available: usize,
    pub charge_pct: ScalarStats,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watts: Option<ScalarStats>,
    pub last_status: String,
}

/// Per-process aggregate resource usage across a sample window, keyed by `ProcessIdentity`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessWindowEntry {
    pub pid: u32,
    pub create_time_100ns: u64,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_line: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    pub samples_seen: usize,
    pub avg_cpu_pct: f64,
    pub peak_cpu_pct: f64,
    pub avg_working_set_bytes: u64,
    pub peak_working_set_bytes: u64,
    pub io_read_delta_bytes: u64,
    pub io_write_delta_bytes: u64,
}

/// Hottest processes observed across the sample window ranked by average/peak CPU and memory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcsWindowSummary {
    pub samples_available: usize,
    pub total_unique_processes: usize,
    pub top_limit: usize,
    pub by_cpu: Vec<ProcessWindowEntry>,
    pub by_mem: Vec<ProcessWindowEntry>,
}

/// Compact timestamped scalar sample emitted when `--include-series` is set with `--summary`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SummarySeriesPoint {
    pub captured_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_busy_pct: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_freq_mhz: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_used_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_available_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_commit_used_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_read_bytes_per_sec: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_write_bytes_per_sec: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_max_busy_pct: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_rx_bytes_per_sec: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net_tx_bytes_per_sec: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_max_util_pct: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_max_temp_celsius: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_total_power_watts: Option<f32>,
}

/// Top-level JSON summary emitted to stdout by `btop --json --summary`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonSummaryEnvelope {
    pub schema_version: u32,
    pub btop_version: String,
    pub started_at_ms: u64,
    pub ended_at_ms: u64,
    pub window_ms: u64,
    pub sample_count: usize,
    pub hostname: String,
    pub username: String,
    pub sections: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<CpuSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem: Option<MemSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disks: Option<Vec<DiskSummary>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub net: Option<Vec<NetSummary>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub procs: Option<ProcsWindowSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpus: Option<Vec<GpuSummary>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub battery: Option<BatterySummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series: Option<Vec<SummarySeriesPoint>>,
}

impl JsonSummaryEnvelope {
    /// Aggregates `snapshots` into a deterministic time-window summary envelope honoring
    /// `sections`, process filters/PID scoping/top-N (`query`), and optional `include_series`.
    pub fn from_snapshots(
        snapshots: &[Snapshot],
        btop_version: impl Into<String>,
        measured_window_ms: Option<u64>,
        sections: JsonSections,
        query: &HeadlessQuery,
        compiled_filter: &QueryFilter,
        include_series: bool,
    ) -> Self {
        let started_at_ms = snapshots.first().map_or(0, |s| s.timestamp_ms);
        let ended_at_ms = snapshots.last().map_or(started_at_ms, |s| s.timestamp_ms);
        let ts_span = ended_at_ms.saturating_sub(started_at_ms);
        let window_ms = if ts_span > 0 {
            ts_span
        } else {
            measured_window_ms.unwrap_or(0)
        };
        let hostname = snapshots
            .last()
            .map(|s| s.hostname.clone())
            .unwrap_or_default();
        let username = snapshots
            .last()
            .map(|s| s.username.clone())
            .unwrap_or_default();

        let cpu = sections.cpu.then(|| summarize_cpu(snapshots)).flatten();
        let mem = sections.mem.then(|| summarize_mem(snapshots)).flatten();
        let disks = sections.disks.then(|| summarize_disks(snapshots));
        let net = sections.net.then(|| summarize_net(snapshots));
        let procs = sections
            .procs
            .then(|| summarize_procs(snapshots, query, compiled_filter));
        let gpus = sections.gpus.then(|| summarize_gpus(snapshots));
        let battery = sections
            .battery
            .then(|| summarize_battery(snapshots))
            .flatten();
        let series = include_series.then(|| build_series(snapshots, sections));

        Self {
            schema_version: JSON_SCHEMA_VERSION,
            btop_version: btop_version.into(),
            started_at_ms,
            ended_at_ms,
            window_ms,
            sample_count: snapshots.len(),
            hostname,
            username,
            sections: sections
                .selected_names()
                .into_iter()
                .map(str::to_owned)
                .collect(),
            cpu,
            mem,
            disks,
            net,
            procs,
            gpus,
            battery,
            series,
        }
    }
}

fn state_value<T>(state: &DataState<T>) -> Option<&T> {
    match state {
        DataState::Available(v) | DataState::Stale { value: v, .. } => Some(v),
        DataState::WarmingUp | DataState::Denied | DataState::Unavailable(_) => None,
    }
}

fn summarize_cpu(snapshots: &[Snapshot]) -> Option<CpuSummary> {
    let cpus: Vec<&CpuSnapshot> = snapshots
        .iter()
        .filter_map(|s| state_value(&s.cpu))
        .collect();
    if cpus.is_empty() {
        return None;
    }

    let busy: Vec<f32> = cpus.iter().map(|c| c.total_busy_pct).collect();
    let total_busy_pct = ScalarStats::from_f32(&busy)?;
    let run_q: Vec<f32> = cpus.iter().map(|c| c.run_queue_ewma[0]).collect();
    let run_queue_1m = ScalarStats::from_f32(&run_q).unwrap_or(ScalarStats {
        count: 0,
        min: 0.0,
        avg: 0.0,
        p95: 0.0,
        max: 0.0,
    });
    let utility: Vec<f32> = cpus.iter().filter_map(|c| c.utility_pct).collect();
    let freqs: Vec<f64> = cpus
        .iter()
        .filter_map(|c| c.freq_mhz)
        .map(f64::from)
        .collect();
    let temps: Vec<f32> = cpus.iter().filter_map(|c| c.package_temp_celsius).collect();
    let watts: Vec<f32> = cpus.iter().filter_map(|c| c.package_watts).collect();

    let last = cpus[cpus.len() - 1];
    Some(CpuSummary {
        model_name: last.model_name.clone(),
        logical_cores: last.cores.len(),
        samples_available: cpus.len(),
        total_busy_pct,
        utility_pct: ScalarStats::from_f32(&utility),
        freq_mhz: ScalarStats::from_f64(&freqs),
        package_temp_celsius: ScalarStats::from_f32(&temps),
        package_watts: ScalarStats::from_f32(&watts),
        run_queue_1m,
    })
}

fn summarize_mem(snapshots: &[Snapshot]) -> Option<MemSummary> {
    let mems: Vec<&MemSnapshot> = snapshots
        .iter()
        .filter_map(|s| state_value(&s.mem))
        .collect();
    if mems.is_empty() {
        return None;
    }

    let used: Vec<u64> = mems.iter().map(|m| m.used_bytes).collect();
    let avail: Vec<u64> = mems.iter().map(|m| m.available_bytes).collect();
    let commit_used: Vec<u64> = mems.iter().map(|m| m.commit_used_bytes).collect();
    let commit_limit: Vec<u64> = mems.iter().map(|m| m.commit_limit_bytes).collect();
    let pagefile_used: Vec<u64> = mems.iter().map(|m| m.pagefile_used_bytes).collect();

    Some(MemSummary {
        total_bytes: mems[mems.len() - 1].total_bytes,
        samples_available: mems.len(),
        used_bytes: U64Stats::from_u64(&used)?,
        available_bytes: U64Stats::from_u64(&avail)?,
        commit_used_bytes: U64Stats::from_u64(&commit_used)?,
        commit_limit_bytes: U64Stats::from_u64(&commit_limit)?,
        pagefile_used_bytes: U64Stats::from_u64(&pagefile_used)?,
    })
}

#[derive(Default)]
struct DiskAccum {
    mount: String,
    label: String,
    fs_type: String,
    physical_drive: Option<u32>,
    total_bytes: u64,
    free_bytes: Vec<u64>,
    read_bps: Vec<u64>,
    write_bps: Vec<u64>,
    busy_pct: Vec<f32>,
}

fn summarize_disks(snapshots: &[Snapshot]) -> Vec<DiskSummary> {
    let mut map: HashMap<String, DiskAccum> = HashMap::new();
    for snap in snapshots {
        let Some(disks) = state_value(&snap.disks) else {
            continue;
        };
        for disk in disks {
            push_disk_sample(&mut map, disk);
        }
    }

    let mut out: Vec<DiskSummary> = map
        .into_values()
        .filter_map(|acc| {
            let free_bytes = U64Stats::from_u64(&acc.free_bytes)?;
            Some(DiskSummary {
                mount: acc.mount,
                label: acc.label,
                fs_type: acc.fs_type,
                physical_drive: acc.physical_drive,
                samples_available: acc.free_bytes.len(),
                total_bytes: acc.total_bytes,
                free_bytes,
                read_bytes_per_sec: U64Stats::from_u64(&acc.read_bps),
                write_bytes_per_sec: U64Stats::from_u64(&acc.write_bps),
                busy_pct: ScalarStats::from_f32(&acc.busy_pct),
            })
        })
        .collect();
    out.sort_by(|a, b| a.mount.cmp(&b.mount));
    out
}

fn push_disk_sample(map: &mut HashMap<String, DiskAccum>, disk: &DiskSnapshot) {
    let entry = map.entry(disk.mount.clone()).or_insert_with(|| DiskAccum {
        mount: disk.mount.clone(),
        ..DiskAccum::default()
    });
    entry.label.clone_from(&disk.label);
    entry.fs_type.clone_from(&disk.fs_type);
    entry.physical_drive = disk.physical_drive;
    entry.total_bytes = disk.total_bytes;
    entry.free_bytes.push(disk.free_bytes);
    if let Some(read) = disk.read_bytes_per_sec {
        entry.read_bps.push(read);
    }
    if let Some(write) = disk.write_bytes_per_sec {
        entry.write_bps.push(write);
    }
    if let Some(busy) = disk.busy_pct {
        entry.busy_pct.push(busy);
    }
}

#[derive(Default)]
struct NetAccum {
    luid: u64,
    name: String,
    description: String,
    rx_bps: Vec<u64>,
    tx_bps: Vec<u64>,
    last_rx_total: Option<u64>,
    last_tx_total: Option<u64>,
    rx_delta_bytes: u64,
    tx_delta_bytes: u64,
}

fn summarize_net(snapshots: &[Snapshot]) -> Vec<NetSummary> {
    let mut map: HashMap<u64, NetAccum> = HashMap::new();
    for snap in snapshots {
        let Some(ifaces) = state_value(&snap.net) else {
            continue;
        };
        for iface in ifaces {
            push_net_sample(&mut map, iface);
        }
    }

    let mut out: Vec<NetSummary> = map
        .into_values()
        .filter_map(|acc| {
            let rx_bytes_per_sec = U64Stats::from_u64(&acc.rx_bps)?;
            let tx_bytes_per_sec = U64Stats::from_u64(&acc.tx_bps)?;
            Some(NetSummary {
                luid: acc.luid,
                name: acc.name,
                description: acc.description,
                samples_available: acc.rx_bps.len(),
                rx_bytes_per_sec,
                tx_bytes_per_sec,
                rx_delta_bytes: acc.rx_delta_bytes,
                tx_delta_bytes: acc.tx_delta_bytes,
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.luid.cmp(&b.luid)));
    out
}

fn push_net_sample(map: &mut HashMap<u64, NetAccum>, iface: &NetInterfaceSnapshot) {
    let entry = map.entry(iface.luid).or_insert_with(|| NetAccum {
        luid: iface.luid,
        ..NetAccum::default()
    });
    entry.name.clone_from(&iface.name);
    entry.description.clone_from(&iface.description);
    entry.rx_bps.push(iface.rx_bytes_per_sec);
    entry.tx_bps.push(iface.tx_bytes_per_sec);
    if let Some(prev) = entry.last_rx_total {
        entry.rx_delta_bytes = entry
            .rx_delta_bytes
            .saturating_add(iface.rx_total_bytes.saturating_sub(prev));
    }
    entry.last_rx_total = Some(iface.rx_total_bytes);
    if let Some(prev) = entry.last_tx_total {
        entry.tx_delta_bytes = entry
            .tx_delta_bytes
            .saturating_add(iface.tx_total_bytes.saturating_sub(prev));
    }
    entry.last_tx_total = Some(iface.tx_total_bytes);
}

#[derive(Default)]
struct GpuAccum {
    index: u8,
    name: String,
    vendor: String,
    samples: usize,
    util: Vec<f32>,
    vram_used: Vec<u64>,
    vram_total: Option<u64>,
    power: Vec<f32>,
    temp: Vec<f32>,
    core_clock: Vec<f64>,
}

fn summarize_gpus(snapshots: &[Snapshot]) -> Vec<GpuSummary> {
    let mut map: HashMap<u8, GpuAccum> = HashMap::new();
    for snap in snapshots {
        for gpu_state in &snap.gpus {
            let Some(gpu) = state_value(gpu_state) else {
                continue;
            };
            push_gpu_sample(&mut map, gpu);
        }
    }

    let mut out: Vec<GpuSummary> = map
        .into_values()
        .map(|acc| GpuSummary {
            index: acc.index,
            name: acc.name,
            vendor: acc.vendor,
            samples_available: acc.samples,
            utilization_pct: ScalarStats::from_f32(&acc.util),
            vram_used_bytes: U64Stats::from_u64(&acc.vram_used),
            vram_total_bytes: acc.vram_total,
            power_watts: ScalarStats::from_f32(&acc.power),
            temp_celsius: ScalarStats::from_f32(&acc.temp),
            core_clock_mhz: ScalarStats::from_f64(&acc.core_clock),
        })
        .collect();
    out.sort_by_key(|g| g.index);
    out
}

fn push_gpu_sample(map: &mut HashMap<u8, GpuAccum>, gpu: &GpuSnapshot) {
    let entry = map.entry(gpu.index).or_insert_with(|| GpuAccum {
        index: gpu.index,
        ..GpuAccum::default()
    });
    entry.name.clone_from(&gpu.name);
    entry.vendor.clone_from(&gpu.vendor);
    entry.samples += 1;
    if let Some(u) = gpu.utilization_pct {
        entry.util.push(u);
    }
    if let Some(v) = gpu.vram_used_bytes {
        entry.vram_used.push(v);
    }
    if gpu.vram_total_bytes.is_some() {
        entry.vram_total = gpu.vram_total_bytes;
    }
    if let Some(p) = gpu.power_watts {
        entry.power.push(p);
    }
    if let Some(t) = gpu.temp_celsius {
        entry.temp.push(t);
    }
    if let Some(c) = gpu.core_clock_mhz {
        entry.core_clock.push(f64::from(c));
    }
}

fn summarize_battery(snapshots: &[Snapshot]) -> Option<BatterySummary> {
    let batteries: Vec<&BatterySnapshot> = snapshots
        .iter()
        .filter_map(|s| state_value(&s.battery))
        .collect();
    if batteries.is_empty() {
        return None;
    }
    let charges: Vec<f64> = batteries.iter().map(|b| f64::from(b.charge_pct)).collect();
    let watts: Vec<f32> = batteries.iter().filter_map(|b| b.watts).collect();
    Some(BatterySummary {
        samples_available: batteries.len(),
        charge_pct: ScalarStats::from_f64(&charges)?,
        watts: ScalarStats::from_f32(&watts),
        last_status: batteries[batteries.len() - 1].status.clone(),
    })
}

struct ProcAccum {
    identity: ProcessIdentity,
    name: String,
    command_line: Option<String>,
    user: Option<String>,
    samples_seen: usize,
    cpu_sum: f64,
    cpu_valid_samples: usize,
    cpu_peak: f64,
    mem_sum: u128,
    mem_peak: u64,
    last_io_read: Option<u64>,
    last_io_write: Option<u64>,
    io_read_delta: u64,
    io_write_delta: u64,
}

fn summarize_procs(
    snapshots: &[Snapshot],
    query: &HeadlessQuery,
    compiled_filter: &QueryFilter,
) -> ProcsWindowSummary {
    let mut map: HashMap<ProcessIdentity, ProcAccum> = HashMap::new();
    let mut samples_available = 0usize;

    for snap in snapshots {
        let Some(procs) = state_value(&snap.processes) else {
            continue;
        };
        samples_available += 1;

        let pid_mask = query
            .pid
            .map(|target| compute_pid_lineage_mask(procs, target));

        for (idx, proc_entry) in procs.iter().enumerate() {
            if let Some(ref mask) = pid_mask
                && !mask[idx]
            {
                continue;
            }
            if !compiled_filter.matches_process(proc_entry) {
                continue;
            }
            push_proc_sample(&mut map, proc_entry);
        }
    }

    let entries: Vec<ProcessWindowEntry> = map
        .into_values()
        .map(|acc| {
            #[allow(clippy::cast_precision_loss)]
            let avg_cpu_pct = if acc.cpu_valid_samples > 0 {
                acc.cpu_sum / (acc.cpu_valid_samples as f64)
            } else {
                0.0
            };
            let denom = u128::try_from(acc.samples_seen.max(1)).unwrap_or(1);
            let avg_mem = u64::try_from((acc.mem_sum + denom / 2) / denom).unwrap_or(u64::MAX);
            ProcessWindowEntry {
                pid: acc.identity.pid,
                create_time_100ns: acc.identity.create_time_100ns,
                name: acc.name,
                command_line: acc.command_line,
                user: acc.user,
                samples_seen: acc.samples_seen,
                avg_cpu_pct,
                peak_cpu_pct: acc.cpu_peak,
                avg_working_set_bytes: avg_mem,
                peak_working_set_bytes: acc.mem_peak,
                io_read_delta_bytes: acc.io_read_delta,
                io_write_delta_bytes: acc.io_write_delta,
            }
        })
        .collect();

    let total_unique_processes = entries.len();
    let limit = match query.top {
        Some(0) => total_unique_processes,
        Some(n) => n,
        None => DEFAULT_SUMMARY_TOP,
    };

    let mut by_cpu = entries.clone();
    by_cpu.sort_by(|a, b| {
        b.avg_cpu_pct
            .total_cmp(&a.avg_cpu_pct)
            .then_with(|| b.peak_cpu_pct.total_cmp(&a.peak_cpu_pct))
            .then_with(|| a.pid.cmp(&b.pid))
    });
    by_cpu.truncate(limit);

    let mut by_mem = entries;
    by_mem.sort_by(|a, b| {
        b.peak_working_set_bytes
            .cmp(&a.peak_working_set_bytes)
            .then_with(|| b.avg_working_set_bytes.cmp(&a.avg_working_set_bytes))
            .then_with(|| a.pid.cmp(&b.pid))
    });
    by_mem.truncate(limit);

    ProcsWindowSummary {
        samples_available,
        total_unique_processes,
        top_limit: limit,
        by_cpu,
        by_mem,
    }
}

fn push_proc_sample(map: &mut HashMap<ProcessIdentity, ProcAccum>, proc_entry: &ProcessEntry) {
    let acc = map.entry(proc_entry.identity).or_insert_with(|| ProcAccum {
        identity: proc_entry.identity,
        name: proc_entry.name.clone(),
        command_line: proc_entry.command_line.clone(),
        user: proc_entry.user.clone(),
        samples_seen: 0,
        cpu_sum: 0.0,
        cpu_valid_samples: 0,
        cpu_peak: 0.0,
        mem_sum: 0,
        mem_peak: 0,
        last_io_read: None,
        last_io_write: None,
        io_read_delta: 0,
        io_write_delta: 0,
    });
    acc.samples_seen += 1;
    acc.name.clone_from(&proc_entry.name);
    if proc_entry.command_line.is_some() {
        acc.command_line.clone_from(&proc_entry.command_line);
    }
    if proc_entry.user.is_some() {
        acc.user.clone_from(&proc_entry.user);
    }
    if proc_entry.cpu_pct.is_finite() {
        let cpu = f64::from(proc_entry.cpu_pct.max(0.0));
        acc.cpu_sum += cpu;
        acc.cpu_valid_samples += 1;
        if cpu.total_cmp(&acc.cpu_peak) == Ordering::Greater {
            acc.cpu_peak = cpu;
        }
    }
    acc.mem_sum = acc
        .mem_sum
        .saturating_add(u128::from(proc_entry.working_set_private_bytes));
    acc.mem_peak = acc.mem_peak.max(proc_entry.working_set_private_bytes);
    if let Some(prev) = acc.last_io_read {
        acc.io_read_delta = acc
            .io_read_delta
            .saturating_add(proc_entry.io_read_bytes.saturating_sub(prev));
    }
    acc.last_io_read = Some(proc_entry.io_read_bytes);
    if let Some(prev) = acc.last_io_write {
        acc.io_write_delta = acc
            .io_write_delta
            .saturating_add(proc_entry.io_write_bytes.saturating_sub(prev));
    }
    acc.last_io_write = Some(proc_entry.io_write_bytes);
}

fn compute_pid_lineage_mask(procs: &[ProcessEntry], target_pid: u32) -> Vec<bool> {
    let mut keep = vec![false; procs.len()];
    let Some(target_idx) = procs.iter().position(|p| p.identity.pid == target_pid) else {
        return keep;
    };
    let (parent_idx, children) = build_acyclic_adjacency(procs);

    let mut cur = Some(target_idx);
    while let Some(idx) = cur {
        if keep[idx] {
            break;
        }
        keep[idx] = true;
        cur = parent_idx[idx];
    }

    let mut stack = vec![target_idx];
    while let Some(idx) = stack.pop() {
        keep[idx] = true;
        for &child in &children[idx] {
            if !keep[child] {
                stack.push(child);
            }
        }
    }
    keep
}

fn build_series(snapshots: &[Snapshot], sections: JsonSections) -> Vec<SummarySeriesPoint> {
    snapshots
        .iter()
        .map(|snap| {
            let cpu = if sections.cpu {
                state_value(&snap.cpu)
            } else {
                None
            };
            let mem = if sections.mem {
                state_value(&snap.mem)
            } else {
                None
            };
            let (disk_read, disk_write, disk_busy) = if sections.disks {
                summarize_disk_point(state_value(&snap.disks))
            } else {
                (None, None, None)
            };
            let (net_rx, net_tx) = if sections.net {
                summarize_net_point(state_value(&snap.net))
            } else {
                (None, None)
            };
            let (gpu_util, gpu_temp, gpu_power) = if sections.gpus {
                summarize_gpu_point(&snap.gpus)
            } else {
                (None, None, None)
            };

            SummarySeriesPoint {
                captured_at_ms: snap.timestamp_ms,
                cpu_busy_pct: cpu.map(|c| c.total_busy_pct),
                cpu_freq_mhz: cpu.and_then(|c| c.freq_mhz),
                mem_used_bytes: mem.map(|m| m.used_bytes),
                mem_available_bytes: mem.map(|m| m.available_bytes),
                mem_commit_used_bytes: mem.map(|m| m.commit_used_bytes),
                disk_read_bytes_per_sec: disk_read,
                disk_write_bytes_per_sec: disk_write,
                disk_max_busy_pct: disk_busy,
                net_rx_bytes_per_sec: net_rx,
                net_tx_bytes_per_sec: net_tx,
                gpu_max_util_pct: gpu_util,
                gpu_max_temp_celsius: gpu_temp,
                gpu_total_power_watts: gpu_power,
            }
        })
        .collect()
}

fn summarize_disk_point(
    disks: Option<&Vec<DiskSnapshot>>,
) -> (Option<u64>, Option<u64>, Option<f32>) {
    let Some(list) = disks else {
        return (None, None, None);
    };
    let mut read_sum = 0u64;
    let mut has_read = false;
    let mut write_sum = 0u64;
    let mut has_write = false;
    let mut max_busy: Option<f32> = None;

    for d in list {
        if let Some(r) = d.read_bytes_per_sec {
            read_sum = read_sum.saturating_add(r);
            has_read = true;
        }
        if let Some(w) = d.write_bytes_per_sec {
            write_sum = write_sum.saturating_add(w);
            has_write = true;
        }
        if let Some(b) = d.busy_pct
            && b.is_finite()
        {
            max_busy = Some(max_busy.map_or(b, |prev| prev.max(b)));
        }
    }
    (
        has_read.then_some(read_sum),
        has_write.then_some(write_sum),
        max_busy,
    )
}

fn summarize_net_point(net: Option<&Vec<NetInterfaceSnapshot>>) -> (Option<u64>, Option<u64>) {
    let Some(list) = net else {
        return (None, None);
    };
    if list.is_empty() {
        return (None, None);
    }
    let rx = list
        .iter()
        .fold(0u64, |acc, n| acc.saturating_add(n.rx_bytes_per_sec));
    let tx = list
        .iter()
        .fold(0u64, |acc, n| acc.saturating_add(n.tx_bytes_per_sec));
    (Some(rx), Some(tx))
}

fn summarize_gpu_point(gpus: &[DataState<GpuSnapshot>]) -> (Option<f32>, Option<f32>, Option<f32>) {
    let mut max_util: Option<f32> = None;
    let mut max_temp: Option<f32> = None;
    let mut power_sum = 0.0f32;
    let mut has_power = false;

    for state in gpus {
        let Some(g) = state_value(state) else {
            continue;
        };
        if let Some(u) = g.utilization_pct
            && u.is_finite()
        {
            max_util = Some(max_util.map_or(u, |prev| prev.max(u)));
        }
        if let Some(t) = g.temp_celsius
            && t.is_finite()
        {
            max_temp = Some(max_temp.map_or(t, |prev| prev.max(t)));
        }
        if let Some(p) = g.power_watts
            && p.is_finite()
        {
            power_sum += p;
            has_power = true;
        }
    }

    (max_util, max_temp, has_power.then_some(power_sum))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::win11_16core_dual_gpu_desktop;

    fn sample_proc(
        pid: u32,
        create_time: u64,
        name: &str,
        cpu: f32,
        mem: u64,
        io_r: u64,
        io_w: u64,
    ) -> ProcessEntry {
        ProcessEntry {
            identity: ProcessIdentity {
                pid,
                create_time_100ns: create_time,
            },
            parent_pid: 4,
            name: name.to_owned(),
            image_path: None,
            command_line: Some(format!("{name} --arg")),
            command_line_denied: false,
            user: Some("franz".to_owned()),
            user_denied: false,
            threads: 4,
            cpu_pct: cpu,
            working_set_private_bytes: mem,
            io_read_bytes: io_r,
            io_write_bytes: io_w,
            is_protected: false,
        }
    }

    #[test]
    fn scalar_and_u64_stats_compute_exact_min_avg_p95_max_and_filter_non_finite() {
        let single = ScalarStats::from_f32(&[42.5]).expect("single");
        assert_eq!(single.count, 1);
        assert!((single.min - 42.5).abs() < 1e-6);
        assert!((single.avg - 42.5).abs() < 1e-6);
        assert!((single.p95 - 42.5).abs() < 1e-6);
        assert!((single.max - 42.5).abs() < 1e-6);

        // 20 readings 1..=20 plus NaN/Inf -> nearest-rank p95 is index 18 -> value 19.0.
        let mut vals: Vec<f32> = (1u16..=20).map(f32::from).collect();
        vals.push(f32::NAN);
        vals.push(f32::INFINITY);
        let stats = ScalarStats::from_f32(&vals).expect("multi");
        assert_eq!(stats.count, 20);
        assert!((stats.min - 1.0).abs() < 1e-6);
        assert!((stats.avg - 10.5).abs() < 1e-6);
        assert!((stats.p95 - 19.0).abs() < 1e-6);
        assert!((stats.max - 20.0).abs() < 1e-6);
        assert!(ScalarStats::from_f32(&[f32::NAN]).is_none());

        let ustats = U64Stats::from_u64(&[100, 200, 300, 1000]).expect("u64");
        assert_eq!(ustats.count, 4);
        assert_eq!(ustats.min, 100);
        assert_eq!(ustats.avg, 400);
        assert_eq!(ustats.p95, 1000);
        assert_eq!(ustats.max, 1000);
    }

    fn net_sample(rx_bps: u64, tx_bps: u64, rx_total: u64, tx_total: u64) -> NetInterfaceSnapshot {
        NetInterfaceSnapshot {
            luid: 1,
            name: "Ethernet".to_owned(),
            description: "NIC".to_owned(),
            is_up: true,
            rx_bytes_per_sec: rx_bps,
            tx_bytes_per_sec: tx_bps,
            rx_total_bytes: rx_total,
            tx_total_bytes: tx_total,
            ..NetInterfaceSnapshot::default()
        }
    }

    fn cpu_sample(busy: f32, freq_mhz: u32, run_q: f32) -> CpuSnapshot {
        CpuSnapshot {
            model_name: "Ryzen".to_owned(),
            total_busy_pct: busy,
            freq_mhz: Some(freq_mhz),
            run_queue_ewma: [run_q, run_q * 0.5, run_q * 0.25],
            ..CpuSnapshot::default()
        }
    }

    #[test]
    fn summary_handles_warming_up_gaps_counter_rollbacks_and_pid_reuse() {
        let snap0 = Snapshot {
            timestamp_ms: 1_000,
            hostname: "HOME".to_owned(),
            username: "franz".to_owned(),
            cpu: DataState::WarmingUp,
            mem: DataState::Unavailable("unavailable".to_owned()),
            net: DataState::Available(vec![net_sample(100, 50, 1_000, 500)]),
            processes: DataState::Available(vec![sample_proc(
                100,
                10,
                "worker_old.exe",
                25.0,
                1_000,
                5_000,
                2_000,
            )]),
            ..Snapshot::default()
        };

        // Counter rollback on net (1000 -> 200) and PID 100 reused with new create_time_100ns = 20.
        let snap1 = Snapshot {
            timestamp_ms: 2_000,
            hostname: "HOME".to_owned(),
            username: "franz".to_owned(),
            cpu: DataState::Available(cpu_sample(40.0, 4200, 2.0)),
            mem: DataState::Unavailable("unavailable".to_owned()),
            net: DataState::Available(vec![net_sample(300, 150, 200, 100)]),
            processes: DataState::Available(vec![
                sample_proc(100, 10, "worker_old.exe", 55.0, 2_000, 1_000, 3_000),
                sample_proc(100, 20, "worker_new.exe", 10.0, 4_000, 500, 500),
            ]),
            ..Snapshot::default()
        };

        // Post-rollback forward progress: net 200 -> 600 (+400), worker_old io_read 1000 -> 1500 (+500).
        let snap2 = Snapshot {
            timestamp_ms: 3_000,
            hostname: "HOME".to_owned(),
            username: "franz".to_owned(),
            cpu: DataState::Available(cpu_sample(80.0, 4800, 4.0)),
            mem: DataState::Unavailable("unavailable".to_owned()),
            net: DataState::Available(vec![net_sample(500, 250, 600, 350)]),
            processes: DataState::Available(vec![
                sample_proc(100, 10, "worker_old.exe", 40.0, 3_000, 1_500, 4_000),
                sample_proc(100, 20, "worker_new.exe", 20.0, 6_000, 900, 700),
            ]),
            ..Snapshot::default()
        };

        let env = JsonSummaryEnvelope::from_snapshots(
            &[snap0, snap1, snap2],
            "0.1.1",
            None,
            JsonSections::all(),
            &HeadlessQuery::default(),
            &QueryFilter::None,
            true,
        );

        assert_eq!(env.sample_count, 3);
        assert_eq!(env.window_ms, 2_000);
        let cpu = env.cpu.expect("cpu available on 2/3 samples");
        assert_eq!(cpu.samples_available, 2);
        assert!((cpu.total_busy_pct.avg - 60.0).abs() < 1e-6);
        assert!(env.mem.is_none());

        let net = env.net.expect("net");
        assert_eq!(net.len(), 1);
        assert_eq!(net[0].rx_delta_bytes, 400);
        assert_eq!(net[0].tx_delta_bytes, 250);

        let procs = env.procs.expect("procs");
        assert_eq!(procs.total_unique_processes, 2);
        assert_eq!(procs.by_cpu[0].name, "worker_old.exe");
        assert!((procs.by_cpu[0].avg_cpu_pct - 40.0).abs() < 1e-6);
        assert_eq!(procs.by_cpu[0].io_read_delta_bytes, 500);
        assert_eq!(procs.by_cpu[0].io_write_delta_bytes, 2_000);
        assert_eq!(procs.by_mem[0].name, "worker_new.exe");
        assert_eq!(procs.by_mem[0].peak_working_set_bytes, 6_000);

        let series = env.series.expect("series included");
        assert_eq!(series.len(), 3);
        assert!(series[0].cpu_busy_pct.is_none());
        assert_eq!(series[1].cpu_busy_pct, Some(40.0));
        assert_eq!(series[2].cpu_busy_pct, Some(80.0));
    }

    #[test]
    fn summary_on_committed_dual_gpu_fixture_honors_sections_and_top() {
        let fixtures = win11_16core_dual_gpu_desktop();
        let sections = JsonSections::parse_csv("cpu,mem,gpus,procs").expect("sections");
        let query = HeadlessQuery {
            top: Some(3),
            ..HeadlessQuery::default()
        };
        let env = JsonSummaryEnvelope::from_snapshots(
            &fixtures,
            "0.1.1",
            None,
            sections,
            &query,
            &QueryFilter::None,
            false,
        );
        assert_eq!(env.sample_count, fixtures.len());
        assert!(env.cpu.is_some());
        assert!(env.mem.is_some());
        assert!(env.gpus.is_some());
        assert!(env.disks.is_none());
        assert!(env.net.is_none());
        assert!(env.battery.is_none());
        assert!(env.series.is_none());
        let procs = env.procs.expect("procs");
        assert_eq!(procs.by_cpu.len(), 3);
        assert_eq!(procs.by_mem.len(), 3);
    }
}
