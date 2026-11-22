//! Pure UI layout and widget rendering (`fn(&Snapshot, &UiHistory, &UiState, &Config, Size) -> Grid`).

#![forbid(unsafe_code)]

pub mod cpu_box;
pub mod disk_view;
pub mod gpu_box;
pub mod header;
pub mod layout;
pub mod mem_box;
pub mod net_box;
pub mod proc_actions;
pub mod proc_box;
pub mod proc_detail;
pub mod proc_tree;
pub mod services_view;
pub mod theme;
pub mod widgets;

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use btop_config::Config;
use btop_model::{DataState, GpuSnapshot, ProcessIdentity, Snapshot};
use btop_term::{Grid, Size};

pub use disk_view::{format_rate_bytes_per_sec, parse_io_graph_speeds};
pub use gpu_box::format_compact_gpu_summary;
pub use header::{format_battery, format_clock};
pub use layout::{
    BoxId, ComputedLayout, LayoutState, PresetParseError, PresetSpec, SizeError, compute_layout,
    parse_preset_spec, parse_presets_list, parse_shown_boxes,
};
pub use mem_box::{format_mem_bytes, stacked_physical_segments};
pub use net_box::{AUTO_SCALE_FLOOR_BPS, NetViewState, compute_net_scale_ceilings, format_bitrate};
pub use proc_actions::{ProcActionModal, UiPriorityClass, modal_rect};
pub use proc_box::{ProcSort, ProcViewState};
pub use proc_detail::{
    detail_overlay_height, format_detail_field, format_elapsed_uptime, format_parent_label,
};
pub use proc_tree::{
    CompiledFilter, DisplayProcRow, build_visible_rows, compile_proc_filter, expandable_parents,
    matches_proc_filter,
};
pub use services_view::{
    ServiceRow, ServiceSort, ServiceViewState, build_visible_service_rows, is_pending_service_state,
};

use crate::mem_box::ratio_percent;
use crate::theme::ThemePalette;

const DEFAULT_HISTORY_CAPACITY: usize = 512;
const PROC_HISTORY_CAPACITY: usize = 128;

/// Pure UI interaction state passed into [`render_frame`].
#[derive(Debug, Clone, PartialEq)]
pub struct UiState {
    pub layout: LayoutState,
    pub proc: ProcViewState,
    pub services: ServiceViewState,
    pub net: NetViewState,
    pub show_disks: bool,
    pub io_mode: bool,
    /// Offset from UTC in seconds applied when formatting `clock_format` (`0` = deterministic UTC).
    pub tz_offset_secs: i32,
    /// Optional centered modal dialog (terminate confirmation, priority picker, or refusal notice).
    pub modal: Option<ProcActionModal>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            layout: LayoutState::default(),
            proc: ProcViewState::default(),
            services: ServiceViewState::default(),
            net: NetViewState::default(),
            show_disks: true,
            io_mode: false,
            tz_offset_secs: 0,
            modal: None,
        }
    }
}

impl UiState {
    pub fn from_config(config: &Config) -> Self {
        Self {
            layout: LayoutState::from_config(config),
            proc: ProcViewState::from_config(config),
            services: ServiceViewState::default(),
            net: NetViewState::from_config(config),
            show_disks: config.show_disks,
            io_mode: config.io_mode,
            tz_offset_secs: 0,
            modal: None,
        }
    }

    pub fn toggle_disks(&mut self) {
        self.show_disks = !self.show_disks;
        if !self.show_disks {
            self.io_mode = false;
        }
    }

    pub fn toggle_io_mode(&mut self) {
        self.io_mode = !self.io_mode;
    }
}

/// Per-drive bounded read/write throughput history in bytes/sec.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiskIoSeries {
    read_bps: VecDeque<u64>,
    write_bps: VecDeque<u64>,
}

impl DiskIoSeries {
    pub fn read_slice(&self) -> &[u64] {
        self.read_bps.as_slices().0
    }

    pub fn write_slice(&self) -> &[u64] {
        self.write_bps.as_slices().0
    }
}

/// Per-interface bounded download/upload throughput history and peaks in bytes/sec.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NetIoSeries {
    rx_bps: VecDeque<u64>,
    tx_bps: VecDeque<u64>,
    peak_rx: u64,
    peak_tx: u64,
}

impl NetIoSeries {
    pub fn rx_slice(&self) -> &[u64] {
        self.rx_bps.as_slices().0
    }

    pub fn tx_slice(&self) -> &[u64] {
        self.tx_bps.as_slices().0
    }

    pub fn peak_rx(&self) -> u64 {
        self.peak_rx
    }

    pub fn peak_tx(&self) -> u64 {
        self.peak_tx
    }
}

/// Stable identity key for per-GPU utilization history so topology reordering never mixes
/// samples from two different adapters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum GpuHistoryKey {
    Luid(u64),
    SlotName(u8, String),
}

impl GpuHistoryKey {
    fn for_gpu(slot: u8, gpu: &GpuSnapshot) -> Self {
        match gpu.luid {
            Some(luid) => Self::Luid(luid),
            None => Self::SlotName(slot, gpu.name.clone()),
        }
    }
}

/// Bounded ring-buffer history passed into pure UI rendering for time-series graphs.
#[derive(Debug, Clone, PartialEq)]
pub struct UiHistory {
    capacity: usize,
    cpu_total: VecDeque<f32>,
    cpu_peak: VecDeque<f32>,
    mem_used: VecDeque<f32>,
    mem_available: VecDeque<f32>,
    mem_cached: VecDeque<f32>,
    mem_free: VecDeque<f32>,
    disks: BTreeMap<String, DiskIoSeries>,
    net: BTreeMap<u64, NetIoSeries>,
    gpus: BTreeMap<GpuHistoryKey, VecDeque<f32>>,
    proc_cpu: BTreeMap<ProcessIdentity, VecDeque<f32>>,
    proc_mem: BTreeMap<ProcessIdentity, VecDeque<u64>>,
}

impl Default for UiHistory {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_HISTORY_CAPACITY)
    }
}

impl UiHistory {
    pub fn with_capacity(capacity: usize) -> Self {
        let cap = capacity.max(1);
        Self {
            capacity: cap,
            cpu_total: VecDeque::with_capacity(cap),
            cpu_peak: VecDeque::with_capacity(cap),
            mem_used: VecDeque::with_capacity(cap),
            mem_available: VecDeque::with_capacity(cap),
            mem_cached: VecDeque::with_capacity(cap),
            mem_free: VecDeque::with_capacity(cap),
            disks: BTreeMap::new(),
            net: BTreeMap::new(),
            gpus: BTreeMap::new(),
            proc_cpu: BTreeMap::new(),
            proc_mem: BTreeMap::new(),
        }
    }

    /// Creates a single-sample history seeded from `snapshot`.
    pub fn from_snapshot(snapshot: &Snapshot) -> Self {
        let mut history = Self::default();
        history.push(snapshot);
        history
    }

    /// Creates a single-sample history seeded from `snapshot` using `config.cpu_util_mode`.
    pub fn from_snapshot_with_config(snapshot: &Snapshot, config: &Config) -> Self {
        let mut history = Self::default();
        history.push_with_util_mode(
            snapshot,
            config.cpu_util_mode.eq_ignore_ascii_case("utility"),
        );
        history
    }

    /// Appends the CPU, memory, and disk I/O totals from `snapshot` into the bounded history.
    pub fn push(&mut self, snapshot: &Snapshot) {
        self.push_with_util_mode(snapshot, false);
    }

    /// Appends CPU totals (time-based or `% Processor Utility`), memory %, and disk I/O rates.
    pub fn push_with_util_mode(&mut self, snapshot: &Snapshot, use_utility: bool) {
        if let DataState::Available(cpu) | DataState::Stale { value: cpu, .. } = &snapshot.cpu {
            let total = if use_utility {
                cpu.utility_pct.unwrap_or(cpu.total_busy_pct)
            } else {
                cpu.total_busy_pct
            }
            .clamp(0.0, 100.0);
            let peak = cpu
                .cores
                .iter()
                .map(|c| c.busy_pct.clamp(0.0, 100.0))
                .fold(total, f32::max);

            push_bounded(&mut self.cpu_total, total, self.capacity);
            push_bounded(&mut self.cpu_peak, peak, self.capacity);
        }

        if let DataState::Available(mem) | DataState::Stale { value: mem, .. } = &snapshot.mem {
            push_bounded(
                &mut self.mem_used,
                ratio_percent(mem.used_bytes, mem.total_bytes),
                self.capacity,
            );
            push_bounded(
                &mut self.mem_available,
                ratio_percent(mem.available_bytes, mem.total_bytes),
                self.capacity,
            );
            push_bounded(
                &mut self.mem_cached,
                ratio_percent(mem.cached_bytes, mem.total_bytes),
                self.capacity,
            );
            push_bounded(
                &mut self.mem_free,
                ratio_percent(mem.free_bytes, mem.total_bytes),
                self.capacity,
            );
        }

        if let DataState::Available(disks) | DataState::Stale { value: disks, .. } = &snapshot.disks
        {
            for disk in disks {
                if let (Some(read_bps), Some(write_bps)) =
                    (disk.read_bytes_per_sec, disk.write_bytes_per_sec)
                {
                    let entry = self.disks.entry(disk.mount.clone()).or_default();
                    push_bounded_u64(&mut entry.read_bps, read_bps, self.capacity);
                    push_bounded_u64(&mut entry.write_bps, write_bps, self.capacity);
                }
            }
        }

        if let DataState::Available(ifaces) | DataState::Stale { value: ifaces, .. } = &snapshot.net
        {
            for iface in ifaces {
                let entry = self.net.entry(iface.luid).or_default();
                push_bounded_u64(&mut entry.rx_bps, iface.rx_bytes_per_sec, self.capacity);
                push_bounded_u64(&mut entry.tx_bps, iface.tx_bytes_per_sec, self.capacity);
                entry.peak_rx = entry.peak_rx.max(iface.rx_bytes_per_sec);
                entry.peak_tx = entry.peak_tx.max(iface.tx_bytes_per_sec);
            }
        }

        if let DataState::Available(procs) | DataState::Stale { value: procs, .. } =
            &snapshot.processes
        {
            let mut live_ids = BTreeSet::new();
            for proc_entry in procs {
                live_ids.insert(proc_entry.identity);
                let cpu_series = self.proc_cpu.entry(proc_entry.identity).or_default();
                push_bounded(
                    cpu_series,
                    proc_entry.cpu_pct.clamp(0.0, 100.0),
                    PROC_HISTORY_CAPACITY,
                );
                let mem_series = self.proc_mem.entry(proc_entry.identity).or_default();
                push_bounded_u64(
                    mem_series,
                    proc_entry.working_set_private_bytes,
                    PROC_HISTORY_CAPACITY,
                );
            }
            self.proc_cpu.retain(|id, _| live_ids.contains(id));
            self.proc_mem.retain(|id, _| live_ids.contains(id));
        }

        let mut live_gpu_keys = BTreeSet::new();
        let mut saw_ready_gpu = false;
        for (slot, gpu_state) in snapshot.gpus.iter().enumerate() {
            if let DataState::Available(gpu) | DataState::Stale { value: gpu, .. } = gpu_state {
                saw_ready_gpu = true;
                let idx = u8::try_from(slot).unwrap_or(gpu.index);
                let key = GpuHistoryKey::for_gpu(idx, gpu);
                live_gpu_keys.insert(key.clone());
                if let Some(pct) = gpu.utilization_pct {
                    let series = self.gpus.entry(key).or_default();
                    push_bounded(series, pct.clamp(0.0, 100.0), self.capacity);
                }
            }
        }
        if saw_ready_gpu || snapshot.gpus.is_empty() {
            self.gpus.retain(|k, _| live_gpu_keys.contains(k));
        }
    }

    pub fn cpu_total_slice(&self) -> &[f32] {
        self.cpu_total.as_slices().0
    }

    pub fn cpu_peak_slice(&self) -> &[f32] {
        self.cpu_peak.as_slices().0
    }

    pub fn mem_used_slice(&self) -> &[f32] {
        self.mem_used.as_slices().0
    }

    pub fn mem_available_slice(&self) -> &[f32] {
        self.mem_available.as_slices().0
    }

    pub fn mem_cached_slice(&self) -> &[f32] {
        self.mem_cached.as_slices().0
    }

    pub fn mem_free_slice(&self) -> &[f32] {
        self.mem_free.as_slices().0
    }

    pub fn disk_series(&self, mount: &str) -> Option<&DiskIoSeries> {
        self.disks.get(mount)
    }

    pub fn net_series(&self, luid: u64) -> Option<&NetIoSeries> {
        self.net.get(&luid)
    }

    pub fn reset_net_peaks(&mut self, luid: u64) {
        if let Some(entry) = self.net.get_mut(&luid) {
            entry.peak_rx = 0;
            entry.peak_tx = 0;
        }
    }

    pub fn proc_cpu_slice(&self, id: ProcessIdentity) -> &[f32] {
        self.proc_cpu.get(&id).map_or(&[], |buf| buf.as_slices().0)
    }

    pub fn proc_mem_slice(&self, id: ProcessIdentity) -> &[u64] {
        self.proc_mem.get(&id).map_or(&[], |buf| buf.as_slices().0)
    }

    /// Returns the utilization history slice associated with the specific GPU adapter identity
    /// (`luid` when present, falling back to `(slot, name)` for synthetic fixtures without LUID).
    pub fn gpu_util_slice_for(&self, gpu_idx: u8, gpu: &GpuSnapshot) -> &[f32] {
        let key = GpuHistoryKey::for_gpu(gpu_idx, gpu);
        self.gpus.get(&key).map_or(&[], |buf| buf.as_slices().0)
    }

    /// Clones per-process CPU history buffers for pausing the process panel cleanly.
    pub fn snapshot_proc_cpu_map(&self) -> BTreeMap<ProcessIdentity, Vec<f32>> {
        self.proc_cpu
            .iter()
            .map(|(&id, buf)| (id, buf.iter().copied().collect()))
            .collect()
    }

    /// Clones per-process private working-set history buffers for pausing the detail overlay cleanly.
    pub fn snapshot_proc_mem_map(&self) -> BTreeMap<ProcessIdentity, Vec<u64>> {
        self.proc_mem
            .iter()
            .map(|(&id, buf)| (id, buf.iter().copied().collect()))
            .collect()
    }
}

fn push_bounded(buf: &mut VecDeque<f32>, value: f32, capacity: usize) {
    if buf.len() >= capacity {
        buf.pop_front();
    }
    buf.push_back(value);
    buf.make_contiguous();
}

fn push_bounded_u64(buf: &mut VecDeque<u64>, value: u64, capacity: usize) {
    if buf.len() >= capacity {
        buf.pop_front();
    }
    buf.push_back(value);
    buf.make_contiguous();
}

/// Returns `true` iff `state.modal` is set and will be visibly drawn at `size`
/// (layout succeeds and the modal dialog fits inside `size`).
pub fn is_modal_visible(size: Size, state: &UiState) -> bool {
    let Some(ref modal) = state.modal else {
        return false;
    };
    compute_layout(size, &state.layout).is_ok() && modal_rect(size, modal).is_some()
}

/// Computes the number of visible process rows for a given viewport size and active UI layout state.
pub fn visible_proc_rows(size: Size, state: &UiState) -> usize {
    let extra = state.proc.extra_header_rows();
    compute_layout(size, &state.layout)
        .ok()
        .and_then(|layout| layout.proc())
        .map_or(0, |r| {
            let inner_h = r.height.saturating_sub(2);
            let detail_h = detail_overlay_height(inner_h, state.proc.detailed.is_some());
            usize::from(inner_h.saturating_sub(detail_h).saturating_sub(1)).saturating_sub(extra)
        })
}

/// Computes the number of visible Windows Services rows inside the `proc` panel when Services view is active.
pub fn visible_service_rows(size: Size, state: &UiState) -> usize {
    let extra = state.services.extra_header_rows();
    compute_layout(size, &state.layout)
        .ok()
        .and_then(|layout| layout.proc())
        .map_or(0, |r| {
            let inner_h = r.height.saturating_sub(2);
            usize::from(inner_h.saturating_sub(1)).saturating_sub(extra)
        })
}

fn box_config<'a>(config: &'a Config, state: &UiState, id: BoxId) -> Cow<'a, Config> {
    let default_sym = match id {
        BoxId::Gpu(_) => config.effective_gpu_graph_symbol(),
        _ => config.graph_symbol,
    };
    let sym = state.layout.symbol_for(id, default_sym);
    if sym == config.graph_symbol {
        Cow::Borrowed(config)
    } else {
        let mut cloned = config.clone();
        cloned.graph_symbol = sym;
        Cow::Owned(cloned)
    }
}

/// Renders a complete terminal frame from a single snapshot into a deterministic [`Grid`].
pub fn render(snapshot: &Snapshot, config: &Config, size: Size) -> Grid {
    let history = UiHistory::from_snapshot_with_config(snapshot, config);
    let state = UiState::from_config(config);
    render_frame(snapshot, &history, &state, config, size)
}

/// Renders a complete terminal frame with accumulated time-series history into a [`Grid`].
pub fn render_with_history(
    snapshot: &Snapshot,
    history: &UiHistory,
    config: &Config,
    size: Size,
) -> Grid {
    let state = UiState::from_config(config);
    render_frame(snapshot, history, &state, config, size)
}

/// Renders a complete terminal frame with history and interactive UI state into a [`Grid`].
pub fn render_frame(
    snapshot: &Snapshot,
    history: &UiHistory,
    state: &UiState,
    config: &Config,
    size: Size,
) -> Grid {
    let mut grid = Grid::new(size);
    let theme = ThemePalette::default_dark(config.theme_background);

    let layout = match compute_layout(size, &state.layout) {
        Ok(computed) => computed,
        Err(size_err) => {
            layout::render_size_error(&mut grid, size_err, config, &theme);
            return grid;
        }
    };

    let has_dedicated_gpu_boxes = !layout.gpus().is_empty();
    if let Some(rect) = layout.cpu() {
        let cfg = box_config(config, state, BoxId::Cpu);
        cpu_box::render_cpu_box(
            &mut grid,
            rect,
            snapshot,
            history,
            has_dedicated_gpu_boxes,
            &cfg,
            &theme,
        );
    }
    if let Some(rect) = layout.mem() {
        let cfg = box_config(config, state, BoxId::Mem);
        mem_box::render_mem_box(&mut grid, rect, snapshot, history, state, &cfg, &theme);
    }
    if let Some(rect) = layout.net() {
        let cfg = box_config(config, state, BoxId::Net);
        net_box::render_net_box(&mut grid, rect, snapshot, history, &state.net, &cfg, &theme);
    }
    if let Some(rect) = layout.proc() {
        let cfg = box_config(config, state, BoxId::Proc);
        if state.services.active {
            services_view::render_services_view(
                &mut grid,
                rect,
                snapshot,
                &state.services,
                &cfg,
                &theme,
            );
        } else {
            proc_box::render_proc_box(
                &mut grid,
                rect,
                snapshot,
                history,
                &state.proc,
                &cfg,
                &theme,
            );
        }
    }
    for (gpu_idx, rect) in layout.gpus() {
        let cfg = box_config(config, state, BoxId::Gpu(gpu_idx));
        gpu_box::render_gpu_box(&mut grid, rect, gpu_idx, snapshot, history, &cfg, &theme);
    }

    header::render_top_header(&mut grid, &layout, snapshot, state, config, &theme);
    if let Some(ref modal) = state.modal {
        proc_actions::render_proc_action_modal(&mut grid, modal, config, &theme);
    }
    grid
}

#[cfg(test)]
mod tests {
    use btop_model::CpuSnapshot;
    use btop_term::HitAction;

    use super::*;

    #[test]
    fn render_produces_deterministic_grid_and_cpu_hit_region() {
        let snap = Snapshot {
            hostname: "TEST-HOST".to_owned(),
            cpu: DataState::Available(CpuSnapshot {
                model_name: "Ryzen 9".to_owned(),
                total_busy_pct: 12.5,
                uptime_secs: 3600,
                ..CpuSnapshot::default()
            }),
            ..Snapshot::default()
        };
        let grid = render(
            &snap,
            &Config::default(),
            Size {
                width: 80,
                height: 24,
            },
        );
        assert_eq!(grid.get(0, 0).map(|c| c.ch), Some('╭'));
        assert_eq!(
            grid.hit_map.hit_test(3, 0),
            Some(&HitAction::ToggleBox("cpu".to_owned()))
        );
    }

    #[test]
    fn history_keeps_slices_contiguous_and_bounded() {
        let mut hist = UiHistory::with_capacity(3);
        for pct in [10.0, 20.0, 30.0, 40.0] {
            let snap = Snapshot {
                cpu: DataState::Available(CpuSnapshot {
                    total_busy_pct: pct,
                    ..CpuSnapshot::default()
                }),
                ..Snapshot::default()
            };
            hist.push(&snap);
        }
        assert_eq!(hist.cpu_total_slice(), &[20.0, 30.0, 40.0]);
    }

    #[test]
    fn top_header_renders_clock_and_hides_battery_when_absent() {
        let mut snap = Snapshot {
            timestamp_ms: 1_795_375_845_000, // 19:30:45 UTC
            hostname: "LAPTOP".to_owned(),
            username: "franz".to_owned(),
            cpu: DataState::Available(CpuSnapshot {
                model_name: "Ryzen 9".to_owned(),
                total_busy_pct: 25.0,
                uptime_secs: 3600,
                ..CpuSnapshot::default()
            }),
            battery: DataState::Unavailable("no battery".to_owned()),
            ..Snapshot::default()
        };
        let cfg = Config {
            shown_boxes: "cpu".to_owned(),
            clock_format: "%X /host".to_owned(),
            ..Config::default()
        };
        let without_bat = render(
            &snap,
            &cfg,
            Size {
                width: 100,
                height: 16,
            },
        )
        .to_plain_text();
        assert!(without_bat.contains("19:30:45 LAPTOP"));
        assert!(!without_bat.contains("BAT "));

        snap.battery = DataState::Available(btop_model::BatterySnapshot {
            charge_pct: 78,
            status: "discharging".to_owned(),
            watts: None,
            remaining_secs: Some(7200),
        });
        let with_bat = render(
            &snap,
            &cfg,
            Size {
                width: 100,
                height: 16,
            },
        )
        .to_plain_text();
        assert!(with_bat.contains("BAT 78% - 2h00m"));
    }

    #[test]
    fn gpu_history_tracks_adapter_luid_across_slot_reordering() {
        let make_gpu = |index: u8, name: &str, luid: u64, util: f32| GpuSnapshot {
            index,
            name: name.to_owned(),
            vendor: "NVIDIA".to_owned(),
            pci_bdf: None,
            utilization_pct: Some(util),
            vram_used_bytes: None,
            vram_total_bytes: None,
            shared_vram_used_bytes: None,
            shared_vram_total_bytes: None,
            luid: Some(luid),
            core_clock_mhz: None,
            mem_clock_mhz: None,
            temp_celsius: None,
            temp_max_celsius: None,
            power_watts: None,
            power_limit_watts: None,
            pstate: None,
            pcie_tx_kbs: None,
            pcie_rx_kbs: None,
            encoder_pct: None,
            decoder_pct: None,
        };

        let mut hist = UiHistory::default();
        let gpu_a = make_gpu(0, "Adapter A", 0xAAAA, 92.0);
        let gpu_b_slot1 = make_gpu(1, "Adapter B", 0xBBBB, 11.0);
        hist.push(&Snapshot {
            gpus: vec![
                DataState::Available(gpu_a.clone()),
                DataState::Available(gpu_b_slot1),
            ],
            ..Snapshot::default()
        });

        // Adapter A disappears; Adapter B shifts from slot 1 into slot 0.
        let gpu_b_slot0 = make_gpu(0, "Adapter B", 0xBBBB, 18.0);
        hist.push(&Snapshot {
            gpus: vec![DataState::Available(gpu_b_slot0.clone())],
            ..Snapshot::default()
        });

        assert_eq!(hist.gpu_util_slice_for(0, &gpu_b_slot0), &[11.0, 18.0]);
        assert!(
            hist.gpu_util_slice_for(0, &gpu_a).is_empty(),
            "departed adapter history must be pruned and never inherited by slot 0"
        );
    }
}
