//! Responsive multi-panel layout solver and upstream `btop` preset engine (`box:pos:symbol`).

use std::collections::BTreeMap;

use btop_config::{Config, DisablePresets, GraphSymbol};
use btop_term::{Grid, Rect, Size};
use thiserror::Error;

use crate::theme::ThemePalette;
use crate::widgets::{draw_box_frame, fill_rect};

const CPU_MIN_W: u16 = 32;
const CPU_MIN_H: u16 = 7;
const MEM_MIN_W: u16 = 28;
const MEM_MIN_H: u16 = 6;
const NET_MIN_W: u16 = 28;
const NET_MIN_H: u16 = 6;
const PROC_MIN_W: u16 = 36;
const PROC_MIN_H: u16 = 7;
const GPU_MIN_W: u16 = 32;
const GPU_MIN_H: u16 = 6;
const MAX_CUSTOM_PRESETS: usize = 9;

/// Panel identifier supported by `shown_boxes`, `presets`, and toggle hotkeys (`1`..`4`, `5`..`0`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BoxId {
    Cpu,
    Mem,
    Net,
    Proc,
    Gpu(u8),
}

impl BoxId {
    /// Parses a box name (`cpu`, `mem`, `net`, `proc`, `gpu0`..`gpu5`).
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "cpu" => Some(Self::Cpu),
            "mem" => Some(Self::Mem),
            "net" => Some(Self::Net),
            "proc" => Some(Self::Proc),
            "gpu0" => Some(Self::Gpu(0)),
            "gpu1" => Some(Self::Gpu(1)),
            "gpu2" => Some(Self::Gpu(2)),
            "gpu3" => Some(Self::Gpu(3)),
            "gpu4" => Some(Self::Gpu(4)),
            "gpu5" => Some(Self::Gpu(5)),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Mem => "mem",
            Self::Net => "net",
            Self::Proc => "proc",
            Self::Gpu(0) => "gpu0",
            Self::Gpu(1) => "gpu1",
            Self::Gpu(2) => "gpu2",
            Self::Gpu(3) => "gpu3",
            Self::Gpu(4) => "gpu4",
            Self::Gpu(_) => "gpu5",
        }
    }

    pub const fn min_size(self) -> Size {
        match self {
            Self::Cpu => Size {
                width: CPU_MIN_W,
                height: CPU_MIN_H,
            },
            Self::Mem => Size {
                width: MEM_MIN_W,
                height: MEM_MIN_H,
            },
            Self::Net => Size {
                width: NET_MIN_W,
                height: NET_MIN_H,
            },
            Self::Proc => Size {
                width: PROC_MIN_W,
                height: PROC_MIN_H,
            },
            Self::Gpu(_) => Size {
                width: GPU_MIN_W,
                height: GPU_MIN_H,
            },
        }
    }
}

/// Error returned when parsing a malformed preset definition.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PresetParseError {
    #[error("preset definition is empty")]
    Empty,
    #[error("invalid preset entry '{0}': expected box:pos:symbol")]
    MalformedEntry(String),
    #[error("unknown box name '{0}'")]
    UnknownBox(String),
    #[error("invalid position flag '{0}': expected 0 or 1")]
    InvalidPosition(String),
    #[error("unknown graph symbol '{0}'")]
    UnknownSymbol(String),
}

/// One parsed layout preset (`0` = default config preset, `1`..`9` = custom presets).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetSpec {
    pub boxes: Vec<BoxId>,
    pub cpu_bottom: bool,
    pub proc_left: bool,
    pub mem_below_net: bool,
    pub symbols: BTreeMap<BoxId, GraphSymbol>,
}

/// Parses one upstream preset specification (`box:pos:symbol,box:pos:symbol,...`).
pub fn parse_preset_spec(spec: &str) -> Result<PresetSpec, PresetParseError> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Err(PresetParseError::Empty);
    }

    let mut boxes = Vec::new();
    let mut cpu_bottom = false;
    let mut proc_left = false;
    let mut mem_below_net = false;
    let mut symbols = BTreeMap::new();

    for raw_entry in trimmed.split(',') {
        let entry = raw_entry.trim();
        if entry.is_empty() {
            continue;
        }
        let mut parts = entry.split(':');
        let (Some(raw_box), Some(raw_pos), Some(raw_sym), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(PresetParseError::MalformedEntry(entry.to_owned()));
        };

        let box_id = BoxId::parse(raw_box)
            .ok_or_else(|| PresetParseError::UnknownBox(raw_box.trim().to_owned()))?;
        let alt_pos = match raw_pos.trim() {
            "0" => false,
            "1" => true,
            other => return Err(PresetParseError::InvalidPosition(other.to_owned())),
        };
        let symbol = match raw_sym.trim().to_ascii_lowercase().as_str() {
            "default" => None,
            "braille" => Some(GraphSymbol::Braille),
            "block" => Some(GraphSymbol::Block),
            "tty" => Some(GraphSymbol::Tty),
            other => return Err(PresetParseError::UnknownSymbol(other.to_owned())),
        };

        if !boxes.contains(&box_id) {
            boxes.push(box_id);
        }
        if alt_pos {
            match box_id {
                BoxId::Cpu => cpu_bottom = true,
                BoxId::Proc => proc_left = true,
                BoxId::Mem => mem_below_net = true,
                BoxId::Net | BoxId::Gpu(_) => {}
            }
        }
        if let Some(sym) = symbol {
            symbols.insert(box_id, sym);
        }
    }

    if boxes.is_empty() {
        return Err(PresetParseError::Empty);
    }

    Ok(PresetSpec {
        boxes,
        cpu_bottom,
        proc_left,
        mem_below_net,
        symbols,
    })
}

/// Parses up to 9 whitespace-separated custom presets from `config.presets`, skipping invalid tokens.
pub fn parse_presets_list(raw: &str) -> Vec<PresetSpec> {
    let mut out = Vec::new();
    for token in raw.split_whitespace() {
        if out.len() >= MAX_CUSTOM_PRESETS {
            break;
        }
        if let Ok(parsed) = parse_preset_spec(token) {
            out.push(parsed);
        }
    }
    out
}

/// Parses `shown_boxes` into deduplicated [`BoxId`] values, falling back to `[BoxId::Cpu]` if empty.
pub fn parse_shown_boxes(raw: &str) -> Vec<BoxId> {
    let mut boxes = Vec::new();
    for token in raw.split_whitespace() {
        if let Some(id) = BoxId::parse(token)
            && !boxes.contains(&id)
        {
            boxes.push(id);
        }
    }
    if boxes.is_empty() {
        boxes.push(BoxId::Cpu);
    }
    boxes
}

/// Active layout and preset state stored in [`crate::UiState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutState {
    /// Preset `0` (config default) followed by up to 9 parsed custom presets (`1`..`9`).
    pub presets: Vec<PresetSpec>,
    /// Active preset index (`Some(0..=9)`), or `None` after manual box toggling (`1`..`4`).
    pub active_preset: Option<u8>,
    pub disable_presets: DisablePresets,
    pub shown_boxes: Vec<BoxId>,
    pub cpu_bottom: bool,
    pub proc_left: bool,
    pub mem_below_net: bool,
    pub symbols: BTreeMap<BoxId, GraphSymbol>,
}

impl Default for LayoutState {
    fn default() -> Self {
        Self::from_config(&Config::default())
    }
}

impl LayoutState {
    pub fn from_config(config: &Config) -> Self {
        let preset_zero = PresetSpec {
            boxes: parse_shown_boxes(&config.shown_boxes),
            cpu_bottom: config.cpu_bottom,
            proc_left: config.proc_left,
            mem_below_net: config.mem_below_net,
            symbols: BTreeMap::new(),
        };
        let mut presets = vec![preset_zero.clone()];
        presets.extend(parse_presets_list(&config.presets));

        let active_preset = match config.disable_presets {
            DisablePresets::Off | DisablePresets::Custom => Some(0),
            DisablePresets::Default | DisablePresets::All => None,
        };

        Self {
            presets,
            active_preset,
            disable_presets: config.disable_presets,
            shown_boxes: preset_zero.boxes,
            cpu_bottom: preset_zero.cpu_bottom,
            proc_left: preset_zero.proc_left,
            mem_below_net: preset_zero.mem_below_net,
            symbols: BTreeMap::new(),
        }
    }

    fn enabled_preset_indices(&self) -> Vec<u8> {
        let count = self.presets.len();
        match self.disable_presets {
            DisablePresets::All => Vec::new(),
            DisablePresets::Custom => {
                if count > 0 {
                    vec![0]
                } else {
                    Vec::new()
                }
            }
            DisablePresets::Default => (1..count).filter_map(|i| u8::try_from(i).ok()).collect(),
            DisablePresets::Off => (0..count).filter_map(|i| u8::try_from(i).ok()).collect(),
        }
    }

    /// Applies preset `index` (`0`..`9`) when permitted by `disable_presets` and `index` exists.
    pub fn apply_preset(&mut self, index: u8) -> bool {
        if !self.enabled_preset_indices().contains(&index) {
            return false;
        }
        let Some(spec) = self.presets.get(usize::from(index)).cloned() else {
            return false;
        };
        self.apply_spec(&spec);
        self.active_preset = Some(index);
        true
    }

    /// Cycles to the next enabled preset (`p`), wrapping around.
    pub fn cycle_preset_next(&mut self) -> bool {
        let indices = self.enabled_preset_indices();
        if indices.is_empty() {
            return false;
        }
        let next = match self
            .active_preset
            .and_then(|cur| indices.iter().position(|&idx| idx == cur))
        {
            Some(pos) => {
                if indices.len() <= 1 {
                    return false;
                }
                indices[(pos + 1) % indices.len()]
            }
            None => indices[0],
        };
        self.apply_preset(next)
    }

    /// Cycles to the previous enabled preset (`Shift+P`), wrapping around.
    pub fn cycle_preset_prev(&mut self) -> bool {
        let indices = self.enabled_preset_indices();
        if indices.is_empty() {
            return false;
        }
        let prev = match self
            .active_preset
            .and_then(|cur| indices.iter().position(|&idx| idx == cur))
        {
            Some(pos) => {
                if indices.len() <= 1 {
                    return false;
                }
                indices[(pos + indices.len() - 1) % indices.len()]
            }
            None => *indices.last().unwrap_or(&0),
        };
        self.apply_preset(prev)
    }

    /// Toggles visibility of `box_id` (`1`..`4`). Refuses to hide the last visible box.
    pub fn toggle_box(&mut self, box_id: BoxId) -> bool {
        if let Some(pos) = self.shown_boxes.iter().position(|&id| id == box_id) {
            if self.shown_boxes.len() <= 1 {
                return false;
            }
            self.shown_boxes.remove(pos);
        } else {
            self.shown_boxes.push(box_id);
        }
        self.active_preset = None;
        true
    }

    /// Returns the effective graph symbol for `box_id` (preset override or global default).
    pub fn symbol_for(&self, box_id: BoxId, default_symbol: GraphSymbol) -> GraphSymbol {
        self.symbols.get(&box_id).copied().unwrap_or(default_symbol)
    }

    /// Short header badge string (`p0`..`p9`, or `p*` when manually toggled).
    pub fn preset_label(&self) -> String {
        match self.active_preset {
            Some(idx) => format!("p{idx}"),
            None => "p*".to_owned(),
        }
    }

    fn apply_spec(&mut self, spec: &PresetSpec) {
        self.shown_boxes.clone_from(&spec.boxes);
        self.cpu_bottom = spec.cpu_bottom;
        self.proc_left = spec.proc_left;
        self.mem_below_net = spec.mem_below_net;
        self.symbols.clone_from(&spec.symbols);
    }
}

/// Viewport is smaller than the minimum dimensions required for the active box set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("terminal size {current_w}x{current_h} is below minimum {req_w}x{req_h}",
    current_w = self.current.width,
    current_h = self.current.height,
    req_w = self.required.width,
    req_h = self.required.height)]
pub struct SizeError {
    pub current: Size,
    pub required: Size,
}

/// Exact non-overlapping bounding rectangles computed for all visible boxes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ComputedLayout {
    pub rects: BTreeMap<BoxId, Rect>,
}

impl ComputedLayout {
    pub fn get(&self, id: BoxId) -> Option<Rect> {
        self.rects.get(&id).copied()
    }

    pub fn cpu(&self) -> Option<Rect> {
        self.get(BoxId::Cpu)
    }

    pub fn mem(&self) -> Option<Rect> {
        self.get(BoxId::Mem)
    }

    pub fn net(&self) -> Option<Rect> {
        self.get(BoxId::Net)
    }

    pub fn proc(&self) -> Option<Rect> {
        self.get(BoxId::Proc)
    }

    pub fn gpus(&self) -> Vec<(u8, Rect)> {
        self.rects
            .iter()
            .filter_map(|(&id, &rect)| match id {
                BoxId::Gpu(idx) => Some((idx, rect)),
                _ => None,
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BandKind {
    Cpu,
    Gpu,
    Lower,
}

#[derive(Debug, Clone, Copy)]
struct ActivePanels {
    cpu: bool,
    mem: bool,
    net: bool,
    proc: bool,
    gpu_rows: usize,
    proc_left: bool,
    mem_below_net: bool,
}

impl ActivePanels {
    const fn has_gpus(self) -> bool {
        self.gpu_rows > 0
    }

    const fn has_lower(self) -> bool {
        self.mem || self.net || self.proc
    }
}

/// Computes exact, non-overlapping [`Rect`] geometry for any subset of `cpu`, `mem`, `net`,
/// `proc`, and `gpu0..gpu5`, honoring `cpu_bottom`, `proc_left`, `mem_below_net`, and minimum
/// per-box dimensions.
pub fn compute_layout(size: Size, state: &LayoutState) -> Result<ComputedLayout, SizeError> {
    let mut gpus: Vec<u8> = state
        .shown_boxes
        .iter()
        .filter_map(|id| match *id {
            BoxId::Gpu(idx) => Some(idx),
            _ => None,
        })
        .collect();
    gpus.sort_unstable();
    gpus.dedup();

    let (gpu_cols, gpu_rows) = gpu_grid_dims(gpus.len(), size.width);
    let panels = ActivePanels {
        cpu: state.shown_boxes.contains(&BoxId::Cpu),
        mem: state.shown_boxes.contains(&BoxId::Mem),
        net: state.shown_boxes.contains(&BoxId::Net),
        proc: state.shown_boxes.contains(&BoxId::Proc),
        gpu_rows,
        proc_left: state.proc_left,
        mem_below_net: state.mem_below_net,
    };

    let required = compute_required_size(panels);
    if size.width < required.width || size.height < required.height {
        return Err(SizeError {
            current: size,
            required,
        });
    }

    let cpu_min_h = if panels.cpu { CPU_MIN_H } else { 0 };
    let gpu_min_h = u16::try_from(gpu_rows)
        .unwrap_or(0)
        .saturating_mul(GPU_MIN_H);
    let lower_min_h = lower_band_min_height(panels.mem, panels.net, panels.proc);

    let band_order = if state.cpu_bottom {
        [BandKind::Lower, BandKind::Gpu, BandKind::Cpu]
    } else {
        [BandKind::Cpu, BandKind::Gpu, BandKind::Lower]
    };

    let band_heights =
        allocate_band_heights(size.height, panels, cpu_min_h, gpu_min_h, lower_min_h);

    let mut rects = BTreeMap::new();
    let mut cursor_y = 0u16;

    for kind in band_order {
        let h = match kind {
            BandKind::Cpu => band_heights.0,
            BandKind::Gpu => band_heights.1,
            BandKind::Lower => band_heights.2,
        };
        if h == 0 {
            continue;
        }
        let band_rect = Rect {
            x: 0,
            y: cursor_y,
            width: size.width,
            height: h,
        };
        cursor_y = cursor_y.saturating_add(h);

        match kind {
            BandKind::Cpu => {
                rects.insert(BoxId::Cpu, band_rect);
            }
            BandKind::Gpu => {
                place_gpu_grid(band_rect, &gpus, gpu_cols, gpu_rows, &mut rects);
            }
            BandKind::Lower => {
                place_lower_band(band_rect, panels, &mut rects);
            }
        }
    }

    Ok(ComputedLayout { rects })
}

fn gpu_grid_dims(gpu_count: usize, viewport_width: u16) -> (usize, usize) {
    if gpu_count == 0 {
        return (0, 0);
    }
    let fit_cols = (usize::from(viewport_width) / usize::from(GPU_MIN_W)).max(1);
    let cols = fit_cols.min(gpu_count).min(3);
    let rows = gpu_count.div_ceil(cols);
    (cols, rows)
}

fn lower_band_min_height(show_mem: bool, show_net: bool, show_proc: bool) -> u16 {
    let mem_net_h = match (show_mem, show_net) {
        (true, true) => MEM_MIN_H + NET_MIN_H,
        (true, false) => MEM_MIN_H,
        (false, true) => NET_MIN_H,
        (false, false) => 0,
    };
    let proc_h = if show_proc { PROC_MIN_H } else { 0 };
    mem_net_h.max(proc_h)
}

fn compute_required_size(panels: ActivePanels) -> Size {
    let mut min_w = 0u16;
    let mut min_h = 0u16;

    if panels.cpu {
        min_w = min_w.max(CPU_MIN_W);
        min_h = min_h.saturating_add(CPU_MIN_H);
    }
    if panels.has_gpus() {
        min_w = min_w.max(GPU_MIN_W);
        min_h = min_h.saturating_add(
            u16::try_from(panels.gpu_rows)
                .unwrap_or(1)
                .saturating_mul(GPU_MIN_H),
        );
    }
    let mem_net_w = match (panels.mem, panels.net) {
        (true, true) => MEM_MIN_W.max(NET_MIN_W),
        (true, false) => MEM_MIN_W,
        (false, true) => NET_MIN_W,
        (false, false) => 0,
    };
    let lower_w = match (mem_net_w > 0, panels.proc) {
        (true, true) => mem_net_w.saturating_add(PROC_MIN_W),
        (true, false) => mem_net_w,
        (false, true) => PROC_MIN_W,
        (false, false) => 0,
    };
    if lower_w > 0 {
        min_w = min_w.max(lower_w);
        min_h = min_h.saturating_add(lower_band_min_height(panels.mem, panels.net, panels.proc));
    }

    Size {
        width: min_w.max(24),
        height: min_h.max(6),
    }
}

fn allocate_band_heights(
    total_h: u16,
    panels: ActivePanels,
    cpu_min: u16,
    gpu_min: u16,
    lower_min: u16,
) -> (u16, u16, u16) {
    match (panels.cpu, panels.has_gpus(), panels.has_lower()) {
        (true, false, false) => (total_h, 0, 0),
        (false, true, false) => (0, total_h, 0),
        (false, false, true) => (0, 0, total_h),
        (true, false, true) => {
            let cpu_cap = total_h.saturating_sub(lower_min);
            let cpu_target = u16::try_from((u32::from(total_h) * 2 / 5).clamp(8, 22)).unwrap_or(22);
            let cpu_h = cpu_target.clamp(cpu_min, cpu_cap.max(cpu_min));
            (cpu_h, 0, total_h.saturating_sub(cpu_h))
        }
        (true, true, false) => {
            let cpu_cap = total_h.saturating_sub(gpu_min);
            let cpu_target = u16::try_from((u32::from(total_h) * 50) / 100).unwrap_or(total_h / 2);
            let cpu_h = cpu_target.clamp(cpu_min, cpu_cap.max(cpu_min));
            (cpu_h, total_h.saturating_sub(cpu_h), 0)
        }
        (false, true, true) => {
            let gpu_cap = total_h.saturating_sub(lower_min);
            let gpu_target = u16::try_from((u32::from(total_h) * 35) / 100).unwrap_or(gpu_min);
            let gpu_h = gpu_target.clamp(gpu_min, gpu_cap.max(gpu_min));
            (0, gpu_h, total_h.saturating_sub(gpu_h))
        }
        (true, true, true) => {
            let cpu_cap = total_h.saturating_sub(gpu_min.saturating_add(lower_min));
            let cpu_target =
                u16::try_from((u32::from(total_h) * 32 / 100).clamp(7, 18)).unwrap_or(18);
            let cpu_h = cpu_target.clamp(cpu_min, cpu_cap.max(cpu_min));
            let remaining = total_h.saturating_sub(cpu_h);
            let gpu_cap = remaining.saturating_sub(lower_min);
            let gpu_target = u16::try_from((u32::from(total_h) * 25) / 100).unwrap_or(gpu_min);
            let gpu_h = gpu_target.clamp(gpu_min, gpu_cap.max(gpu_min));
            let lower_h = remaining.saturating_sub(gpu_h);
            (cpu_h, gpu_h, lower_h)
        }
        (false, false, false) => (0, 0, 0),
    }
}

fn place_gpu_grid(
    area: Rect,
    gpus: &[u8],
    cols: usize,
    rows: usize,
    out: &mut BTreeMap<BoxId, Rect>,
) {
    if gpus.is_empty() || cols == 0 || rows == 0 {
        return;
    }
    let rows_u16 = u16::try_from(rows).unwrap_or(1);
    let base_row_h = area.height / rows_u16;
    let rem_h = area.height % rows_u16;
    let mut y = area.y;

    for row_idx in 0..rows {
        let start = row_idx * cols;
        let end = (start + cols).min(gpus.len());
        let row_gpus = &gpus[start..end];
        let row_h =
            base_row_h.saturating_add(u16::from(u16::try_from(row_idx).unwrap_or(0) < rem_h));

        let count_u16 = u16::try_from(row_gpus.len()).unwrap_or(1);
        let base_col_w = area.width / count_u16;
        let rem_w = area.width % count_u16;
        let mut x = area.x;

        for (col_idx, &gpu_idx) in row_gpus.iter().enumerate() {
            let col_w =
                base_col_w.saturating_add(u16::from(u16::try_from(col_idx).unwrap_or(0) < rem_w));
            out.insert(
                BoxId::Gpu(gpu_idx),
                Rect {
                    x,
                    y,
                    width: col_w,
                    height: row_h,
                },
            );
            x = x.saturating_add(col_w);
        }
        y = y.saturating_add(row_h);
    }
}

fn place_lower_band(area: Rect, panels: ActivePanels, out: &mut BTreeMap<BoxId, Rect>) {
    let show_mem_net = panels.mem || panels.net;
    match (show_mem_net, panels.proc) {
        (true, true) => {
            let mem_net_min_w = if panels.mem { MEM_MIN_W } else { NET_MIN_W };
            let max_mem_net = area.width.saturating_sub(PROC_MIN_W);
            let ideal =
                u16::try_from((u32::from(area.width) * 42 / 100).clamp(32, 80)).unwrap_or(80);
            let mem_net_w = ideal.clamp(mem_net_min_w, max_mem_net.max(mem_net_min_w));
            let proc_w = area.width.saturating_sub(mem_net_w);

            let (mem_net_area, proc_area) = if panels.proc_left {
                (
                    Rect {
                        x: area.x.saturating_add(proc_w),
                        width: mem_net_w,
                        ..area
                    },
                    Rect {
                        x: area.x,
                        width: proc_w,
                        ..area
                    },
                )
            } else {
                (
                    Rect {
                        x: area.x,
                        width: mem_net_w,
                        ..area
                    },
                    Rect {
                        x: area.x.saturating_add(mem_net_w),
                        width: proc_w,
                        ..area
                    },
                )
            };

            place_mem_net_column(
                mem_net_area,
                panels.mem,
                panels.net,
                panels.mem_below_net,
                out,
            );
            out.insert(BoxId::Proc, proc_area);
        }
        (true, false) => {
            place_mem_net_column(area, panels.mem, panels.net, panels.mem_below_net, out);
        }
        (false, true) => {
            out.insert(BoxId::Proc, area);
        }
        (false, false) => {}
    }
}

fn place_mem_net_column(
    area: Rect,
    show_mem: bool,
    show_net: bool,
    mem_below_net: bool,
    out: &mut BTreeMap<BoxId, Rect>,
) {
    match (show_mem, show_net) {
        (true, true) => {
            let max_mem = area.height.saturating_sub(NET_MIN_H);
            let ideal_mem =
                u16::try_from((u32::from(area.height) * 55) / 100).unwrap_or(area.height / 2);
            let mem_h = ideal_mem.clamp(MEM_MIN_H, max_mem.max(MEM_MIN_H));
            let net_h = area.height.saturating_sub(mem_h);

            if mem_below_net {
                out.insert(
                    BoxId::Net,
                    Rect {
                        y: area.y,
                        height: net_h,
                        ..area
                    },
                );
                out.insert(
                    BoxId::Mem,
                    Rect {
                        y: area.y.saturating_add(net_h),
                        height: mem_h,
                        ..area
                    },
                );
            } else {
                out.insert(
                    BoxId::Mem,
                    Rect {
                        y: area.y,
                        height: mem_h,
                        ..area
                    },
                );
                out.insert(
                    BoxId::Net,
                    Rect {
                        y: area.y.saturating_add(mem_h),
                        height: net_h,
                        ..area
                    },
                );
            }
        }
        (true, false) => {
            out.insert(BoxId::Mem, area);
        }
        (false, true) => {
            out.insert(BoxId::Net, area);
        }
        (false, false) => {}
    }
}

/// Renders a clean centered `SizeError` screen when the terminal is below minimum dimensions.
pub fn render_size_error(grid: &mut Grid, err: SizeError, config: &Config, theme: &ThemePalette) {
    let size = grid.size();
    let full = Rect {
        x: 0,
        y: 0,
        width: size.width,
        height: size.height,
    };
    fill_rect(grid, full, theme.main_fg, theme.main_bg);

    let title = "Terminal size too small";
    let current_line = format!("Current: {}x{}", err.current.width, err.current.height);
    let required_line = format!("Minimum: {}x{}", err.required.width, err.required.height);
    let hint = "Resize window, or press p / 1-4";

    if size.width >= 26 && size.height >= 7 {
        let box_w = size.width.min(48);
        let box_h = size.height.min(7);
        let box_rect = Rect {
            x: (size.width.saturating_sub(box_w)) / 2,
            y: (size.height.saturating_sub(box_h)) / 2,
            width: box_w,
            height: box_h,
        };
        draw_box_frame(
            grid,
            box_rect,
            theme.hi_fg,
            theme.main_bg,
            config.rounded_corners,
        );
        let max_inner = usize::from(box_w.saturating_sub(4));
        write_centered_clipped(
            grid,
            box_rect.x + 2,
            box_rect.y + 1,
            max_inner,
            title,
            theme.hi_fg,
            theme.main_bg,
            true,
        );
        write_centered_clipped(
            grid,
            box_rect.x + 2,
            box_rect.y + 2,
            max_inner,
            &current_line,
            theme.main_fg,
            theme.main_bg,
            false,
        );
        write_centered_clipped(
            grid,
            box_rect.x + 2,
            box_rect.y + 3,
            max_inner,
            &required_line,
            theme.title,
            theme.main_bg,
            true,
        );
        if box_h >= 7 {
            write_centered_clipped(
                grid,
                box_rect.x + 2,
                box_rect.y + 5,
                max_inner,
                hint,
                theme.inactive_fg,
                theme.main_bg,
                false,
            );
        }
    } else {
        let max_w = usize::from(size.width);
        if size.height >= 1 {
            write_centered_clipped(grid, 0, 0, max_w, title, theme.hi_fg, theme.main_bg, true);
        }
        if size.height >= 2 {
            let compact = format!(
                "{}x{} < {}x{}",
                err.current.width, err.current.height, err.required.width, err.required.height
            );
            write_centered_clipped(
                grid,
                0,
                1,
                max_w,
                &compact,
                theme.main_fg,
                theme.main_bg,
                false,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn write_centered_clipped(
    grid: &mut Grid,
    x: u16,
    y: u16,
    max_width: usize,
    text: &str,
    fg: btop_term::Color,
    bg: btop_term::Color,
    bold: bool,
) {
    if max_width == 0 || y >= grid.size().height {
        return;
    }
    let clipped: String = text.chars().take(max_width).collect();
    let len = clipped.chars().count();
    let pad = u16::try_from(max_width.saturating_sub(len) / 2).unwrap_or(0);
    grid.write_str(x.saturating_add(pad), y, &clipped, fg, bg, bold);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_layout_invariants(size: Size, state: &LayoutState, layout: &ComputedLayout) {
        assert_eq!(layout.rects.len(), state.shown_boxes.len());
        let all: Vec<(BoxId, Rect)> = layout.rects.iter().map(|(&k, &v)| (k, v)).collect();
        let mut area_sum = 0u32;

        for &(id, r) in &all {
            let min = id.min_size();
            assert!(
                r.width >= min.width && r.height >= min.height,
                "{id:?} rect {r:?} smaller than minimum {min:?} at viewport {size:?}"
            );
            assert!(
                r.x.saturating_add(r.width) <= size.width
                    && r.y.saturating_add(r.height) <= size.height,
                "{id:?} rect {r:?} out of bounds for {size:?}"
            );
            area_sum += u32::from(r.width) * u32::from(r.height);
        }

        // Non-overlapping check for every pair.
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                let (_, a) = all[i];
                let (_, b) = all[j];
                let separated = a.x + a.width <= b.x
                    || b.x + b.width <= a.x
                    || a.y + a.height <= b.y
                    || b.y + b.height <= a.y;
                assert!(separated, "overlapping rects {:?} and {:?}", all[i], all[j]);
            }
        }

        // Complete viewport coverage (no wasted dead gaps).
        assert_eq!(
            area_sum,
            u32::from(size.width) * u32::from(size.height),
            "layout rects should tile the viewport cleanly"
        );
    }

    #[test]
    fn parses_upstream_preset_syntax_and_rejects_malformed_entries() {
        let spec = parse_preset_spec("cpu:1:block,proc:1:default,mem:1:default,net:0:tty")
            .expect("parse valid preset");
        assert_eq!(
            spec.boxes,
            vec![BoxId::Cpu, BoxId::Proc, BoxId::Mem, BoxId::Net]
        );
        assert!(spec.cpu_bottom);
        assert!(spec.proc_left);
        assert!(spec.mem_below_net);
        assert_eq!(spec.symbols.get(&BoxId::Cpu), Some(&GraphSymbol::Block));
        assert_eq!(spec.symbols.get(&BoxId::Proc), None);
        assert_eq!(spec.symbols.get(&BoxId::Net), Some(&GraphSymbol::Tty));

        // Only `mem:1` controls `mem_below_net`; `net:1` leaves it unchanged (upstream parity).
        let net_pos_one =
            parse_preset_spec("mem:0:default,net:1:default").expect("parse mem:0 net:1");
        assert!(!net_pos_one.mem_below_net);

        let gpu_spec = parse_preset_spec("cpu:0:default,gpu0:0:braille,gpu5:0:block")
            .expect("parse gpu preset");
        assert_eq!(
            gpu_spec.boxes,
            vec![BoxId::Cpu, BoxId::Gpu(0), BoxId::Gpu(5)]
        );

        assert!(matches!(
            parse_preset_spec(""),
            Err(PresetParseError::Empty)
        ));
        assert!(matches!(
            parse_preset_spec("cpu:0"),
            Err(PresetParseError::MalformedEntry(_))
        ));
        assert!(matches!(
            parse_preset_spec("foo:0:default"),
            Err(PresetParseError::UnknownBox(_))
        ));
        assert!(matches!(
            parse_preset_spec("cpu:2:default"),
            Err(PresetParseError::InvalidPosition(_))
        ));
        assert!(matches!(
            parse_preset_spec("cpu:0:ascii"),
            Err(PresetParseError::UnknownSymbol(_))
        ));
    }

    #[test]
    fn preset_cycling_honors_disable_presets_and_box_toggles_keep_last_box() {
        let cfg = Config::default();
        let mut state = LayoutState::from_config(&cfg);
        assert_eq!(state.active_preset, Some(0));
        assert_eq!(state.presets.len(), 4);

        assert!(state.cycle_preset_next());
        assert_eq!(state.active_preset, Some(1));
        assert!(state.cpu_bottom);
        assert_eq!(state.shown_boxes, vec![BoxId::Cpu, BoxId::Proc]);

        assert!(state.cycle_preset_next());
        assert_eq!(state.active_preset, Some(2));
        assert!(!state.cpu_bottom);
        assert_eq!(state.shown_boxes, vec![BoxId::Cpu, BoxId::Mem, BoxId::Net]);

        assert!(state.cycle_preset_prev());
        assert_eq!(state.active_preset, Some(1));

        assert!(state.cycle_preset_prev());
        assert_eq!(state.active_preset, Some(0));

        // Wrap backwards from 0 to last preset.
        assert!(state.cycle_preset_prev());
        assert_eq!(state.active_preset, Some(3));
        assert_eq!(
            state.symbol_for(BoxId::Cpu, GraphSymbol::Braille),
            GraphSymbol::Block
        );

        // Box toggle transitions to custom (`None`) and refuses hiding the final box.
        assert!(state.apply_preset(1));
        assert!(state.toggle_box(BoxId::Proc));
        assert_eq!(state.active_preset, None);
        assert_eq!(state.shown_boxes, vec![BoxId::Cpu]);
        assert!(!state.toggle_box(BoxId::Cpu));
        assert_eq!(state.shown_boxes, vec![BoxId::Cpu]);

        // `DisablePresets::All` rejects cycling and direct apply.
        let all_disabled_cfg = Config {
            disable_presets: DisablePresets::All,
            ..Config::default()
        };
        let mut all_disabled = LayoutState::from_config(&all_disabled_cfg);
        assert!(!all_disabled.cycle_preset_next());
        assert!(!all_disabled.cycle_preset_prev());
        assert!(!all_disabled.apply_preset(0));
        assert!(!all_disabled.apply_preset(1));

        // `DisablePresets::Default` skips Preset 0 and cycles strictly over custom presets 1..=3.
        let default_disabled_cfg = Config {
            disable_presets: DisablePresets::Default,
            ..Config::default()
        };
        let mut default_disabled = LayoutState::from_config(&default_disabled_cfg);
        assert!(!default_disabled.apply_preset(0));
        assert!(default_disabled.cycle_preset_next());
        assert_eq!(default_disabled.active_preset, Some(1));
        assert!(default_disabled.cycle_preset_prev());
        assert_eq!(default_disabled.active_preset, Some(3));

        // `DisablePresets::Custom` allows only Preset 0 and rejects custom presets 1..=3.
        let custom_disabled_cfg = Config {
            disable_presets: DisablePresets::Custom,
            ..Config::default()
        };
        let mut custom_disabled = LayoutState::from_config(&custom_disabled_cfg);
        assert_eq!(custom_disabled.active_preset, Some(0));
        assert!(!custom_disabled.apply_preset(1));
        assert!(!custom_disabled.cycle_preset_next());
        assert!(custom_disabled.toggle_box(BoxId::Proc));
        assert!(custom_disabled.cycle_preset_next());
        assert_eq!(custom_disabled.active_preset, Some(0));
    }

    fn check_orientation(layout: &ComputedLayout, state: &LayoutState) {
        if let (Some(cpu), Some(proc_r)) = (layout.cpu(), layout.proc()) {
            if state.cpu_bottom {
                assert!(cpu.y >= proc_r.y + proc_r.height);
            } else {
                assert!(proc_r.y >= cpu.y + cpu.height);
            }
        }
        if let (Some(mem), Some(proc_r)) = (layout.mem(), layout.proc()) {
            if state.proc_left {
                assert!(proc_r.x + proc_r.width <= mem.x);
            } else {
                assert!(mem.x + mem.width <= proc_r.x);
            }
        }
        if let (Some(mem), Some(net)) = (layout.mem(), layout.net()) {
            if state.mem_below_net {
                assert!(net.y + net.height <= mem.y);
            } else {
                assert!(mem.y + mem.height <= net.y);
            }
        }
    }

    #[test]
    fn layout_solver_handles_all_orientations_gpus_and_viewports() {
        let viewports = [
            Size {
                width: 40,
                height: 12,
            },
            Size {
                width: 80,
                height: 24,
            },
            Size {
                width: 120,
                height: 40,
            },
            Size {
                width: 200,
                height: 60,
            },
        ];

        let box_sets: &[&[BoxId]] = &[
            &[BoxId::Cpu],
            &[BoxId::Mem],
            &[BoxId::Net],
            &[BoxId::Proc],
            &[BoxId::Cpu, BoxId::Proc],
            &[BoxId::Cpu, BoxId::Mem, BoxId::Net],
            &[BoxId::Cpu, BoxId::Mem, BoxId::Net, BoxId::Proc],
            &[BoxId::Mem, BoxId::Net, BoxId::Proc],
            &[BoxId::Cpu, BoxId::Gpu(0), BoxId::Gpu(1), BoxId::Proc],
            &[
                BoxId::Cpu,
                BoxId::Gpu(0),
                BoxId::Gpu(1),
                BoxId::Gpu(2),
                BoxId::Gpu(3),
                BoxId::Gpu(4),
                BoxId::Gpu(5),
                BoxId::Mem,
                BoxId::Net,
                BoxId::Proc,
            ],
        ];

        for &size in &viewports {
            for &boxes in box_sets {
                for cpu_bottom in [false, true] {
                    for proc_left in [false, true] {
                        for mem_below_net in [false, true] {
                            let state = LayoutState {
                                presets: Vec::new(),
                                active_preset: Some(0),
                                disable_presets: DisablePresets::Off,
                                shown_boxes: boxes.to_vec(),
                                cpu_bottom,
                                proc_left,
                                mem_below_net,
                                symbols: BTreeMap::new(),
                            };
                            match compute_layout(size, &state) {
                                Ok(layout) => {
                                    assert_layout_invariants(size, &state, &layout);
                                    check_orientation(&layout, &state);
                                }
                                Err(err) => {
                                    assert!(
                                        size.width < err.required.width
                                            || size.height < err.required.height
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn narrow_40x12_fits_single_box_and_errors_for_four_boxes() {
        let single_cpu = LayoutState::from_config(&Config {
            shown_boxes: "cpu".to_owned(),
            ..Config::default()
        });
        assert!(
            compute_layout(
                Size {
                    width: 40,
                    height: 12,
                },
                &single_cpu
            )
            .is_ok()
        );

        let full_four = LayoutState::from_config(&Config::default());
        let err = compute_layout(
            Size {
                width: 40,
                height: 12,
            },
            &full_four,
        )
        .expect_err("40x12 must trigger SizeError for all 4 boxes");
        assert_eq!(
            err.required,
            Size {
                width: 64,
                height: 19,
            }
        );
    }
}
