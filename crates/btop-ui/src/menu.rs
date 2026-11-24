//! Game-inspired overlay menu state machine: Main menu (`Esc`/`m`), categorized Options menu
//! (`F2`/`o`) with inline `TextEdit` validation and live theme preview, scrollable Help reference
//! (`F1`/`?`/`h`), and reusable `MessageBox` (`OK` / `YES_NO`).

use btop_config::{Config, GraphSymbol, ShowGpuInfo};
use btop_term::KeyCode;

use crate::theme::BUILTIN_THEME_NAMES;

/// Single-line UTF-8-safe inline text editor with character-indexed cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    value: String,
    cursor: usize,
}

impl TextEdit {
    pub fn new(initial: impl Into<String>) -> Self {
        let value = initial.into();
        let cursor = value.chars().count();
        Self { value, cursor }
    }

    pub fn as_str(&self) -> &str {
        &self.value
    }

    pub const fn cursor_char_idx(&self) -> usize {
        self.cursor
    }

    pub fn insert(&mut self, ch: char) {
        let byte_idx = char_to_byte_idx(&self.value, self.cursor);
        self.value.insert(byte_idx, ch);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let start = char_to_byte_idx(&self.value, self.cursor - 1);
        let end = char_to_byte_idx(&self.value, self.cursor);
        self.value.replace_range(start..end, "");
        self.cursor -= 1;
        true
    }

    pub fn delete(&mut self) -> bool {
        let len = self.value.chars().count();
        if self.cursor >= len {
            return false;
        }
        let start = char_to_byte_idx(&self.value, self.cursor);
        let end = char_to_byte_idx(&self.value, self.cursor + 1);
        self.value.replace_range(start..end, "");
        true
    }

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        let len = self.value.chars().count();
        self.cursor = (self.cursor + 1).min(len);
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.value.chars().count();
    }

    /// Handles a single editing key event, returning `true` if state changed.
    pub fn handle_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Left => {
                self.move_left();
                true
            }
            KeyCode::Right => {
                self.move_right();
                true
            }
            KeyCode::Home => {
                self.home();
                true
            }
            KeyCode::End => {
                self.end();
                true
            }
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Char(ch) if !ch.is_control() => {
                self.insert(ch);
                true
            }
            _ => false,
        }
    }
}

fn char_to_byte_idx(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map_or(s.len(), |(byte_idx, _)| byte_idx)
}

/// Button configuration for [`MessageBoxState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageBoxKind {
    Ok,
    YesNo,
}

/// Semantic action associated with confirming a [`MessageBoxState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageBoxAction {
    DismissOnly,
    QuitApp,
    ResetOptionsToDefaults,
}

/// Outcome returned when handling input on a [`MessageBoxState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageBoxResult {
    Pending,
    Confirmed(MessageBoxAction),
    Cancelled,
}

/// Reusable centered `OK` or `YES_NO` confirmation dialog with keyboard and mouse support.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageBoxState {
    pub title: String,
    pub lines: Vec<String>,
    pub kind: MessageBoxKind,
    pub selected_confirm: bool,
    pub action: MessageBoxAction,
}

impl MessageBoxState {
    pub fn ok(title: impl Into<String>, lines: Vec<String>) -> Self {
        Self {
            title: title.into(),
            lines,
            kind: MessageBoxKind::Ok,
            selected_confirm: true,
            action: MessageBoxAction::DismissOnly,
        }
    }

    pub fn yes_no(
        title: impl Into<String>,
        lines: Vec<String>,
        action: MessageBoxAction,
        default_yes: bool,
    ) -> Self {
        Self {
            title: title.into(),
            lines,
            kind: MessageBoxKind::YesNo,
            selected_confirm: default_yes,
            action,
        }
    }

    pub fn handle_key(&mut self, code: KeyCode) -> MessageBoxResult {
        self.handle_key_with_vim(code, false)
    }

    pub fn handle_key_with_vim(&mut self, code: KeyCode, vim_keys: bool) -> MessageBoxResult {
        match self.kind {
            MessageBoxKind::Ok => match code {
                KeyCode::Enter | KeyCode::Char(' ') => MessageBoxResult::Confirmed(self.action),
                KeyCode::Esc => MessageBoxResult::Cancelled,
                _ => MessageBoxResult::Pending,
            },
            MessageBoxKind::YesNo => match code {
                KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                    self.selected_confirm = !self.selected_confirm;
                    MessageBoxResult::Pending
                }
                KeyCode::Char('h' | 'l') if vim_keys => {
                    self.selected_confirm = !self.selected_confirm;
                    MessageBoxResult::Pending
                }
                KeyCode::Char('y' | 'Y') => MessageBoxResult::Confirmed(self.action),
                KeyCode::Char('n' | 'N') | KeyCode::Esc => MessageBoxResult::Cancelled,
                KeyCode::Enter | KeyCode::Char(' ') => {
                    if self.selected_confirm {
                        MessageBoxResult::Confirmed(self.action)
                    } else {
                        MessageBoxResult::Cancelled
                    }
                }
                _ => MessageBoxResult::Pending,
            },
        }
    }

    pub const fn handle_button_click(&self, confirm: bool) -> MessageBoxResult {
        match self.kind {
            MessageBoxKind::Ok => MessageBoxResult::Confirmed(self.action),
            MessageBoxKind::YesNo => {
                if confirm {
                    MessageBoxResult::Confirmed(self.action)
                } else {
                    MessageBoxResult::Cancelled
                }
            }
        }
    }
}

/// Top-level Main menu choices (`OPTIONS`, `HELP`, `QUIT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainMenuChoice {
    Options,
    Help,
    Quit,
}

impl MainMenuChoice {
    pub const ALL: [Self; 3] = [Self::Options, Self::Help, Self::Quit];
}

/// Interactive state for the game-inspired Main menu overlay.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MainMenuState {
    pub selected: usize,
}

impl MainMenuState {
    pub fn move_up(&mut self) {
        self.selected = if self.selected == 0 {
            MainMenuChoice::ALL.len() - 1
        } else {
            self.selected - 1
        };
    }

    pub fn move_down(&mut self) {
        self.selected = (self.selected + 1) % MainMenuChoice::ALL.len();
    }

    pub fn selected_choice(&self) -> MainMenuChoice {
        MainMenuChoice::ALL
            .get(self.selected)
            .copied()
            .unwrap_or(MainMenuChoice::Options)
    }
}

/// Categories displayed as tabs inside the Options menu (`F2` / `o`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionCategory {
    General,
    Cpu,
    Gpu,
    Mem,
    Net,
    Proc,
}

impl OptionCategory {
    pub const ALL: [Self; 6] = [
        Self::General,
        Self::Cpu,
        Self::Gpu,
        Self::Mem,
        Self::Net,
        Self::Proc,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Cpu => "CPU",
            Self::Gpu => "GPU",
            Self::Mem => "MEM",
            Self::Net => "NET",
            Self::Proc => "PROC",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|&c| c == self).unwrap_or(0)
    }

    #[must_use]
    pub fn next(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    #[must_use]
    pub fn prev(self) -> Self {
        let idx = self.index();
        Self::ALL[if idx == 0 {
            Self::ALL.len() - 1
        } else {
            idx - 1
        }]
    }

    pub const fn options(self) -> &'static [OptionId] {
        match self {
            Self::General => &[
                OptionId::ColorTheme,
                OptionId::ThemeBackground,
                OptionId::RoundedCorners,
                OptionId::TerminalSync,
                OptionId::BackgroundUpdate,
                OptionId::SaveConfigOnExit,
                OptionId::UpdateMs,
                OptionId::ShownBoxes,
                OptionId::ClockFormat,
                OptionId::GraphSymbol,
                OptionId::VimKeys,
                OptionId::DisableMouse,
                OptionId::CpuBottom,
                OptionId::ProcLeft,
                OptionId::MemBelowNet,
                OptionId::ShowBattery,
            ],
            Self::Cpu => &[OptionId::CpuUtilMode, OptionId::FreqMode],
            Self::Gpu => &[
                OptionId::ShowGpuInfo,
                OptionId::GpuMirrorGraph,
                OptionId::GraphSymbolGpu,
                OptionId::NvmlMeasurePcieSpeeds,
                OptionId::CustomGpuName0,
                OptionId::CustomGpuName1,
            ],
            Self::Mem => &[
                OptionId::MemGraphs,
                OptionId::ShowSwap,
                OptionId::Base10Sizes,
                OptionId::ShowDisks,
                OptionId::ShowIoStat,
                OptionId::IoMode,
                OptionId::IoGraphCombined,
                OptionId::DisksFilter,
            ],
            Self::Net => &[
                OptionId::NetIface,
                OptionId::NetAuto,
                OptionId::NetSync,
                OptionId::NetDownload,
                OptionId::NetUpload,
                OptionId::SwapUploadDownload,
                OptionId::Base10Bitrate,
            ],
            Self::Proc => &[
                OptionId::ProcSorting,
                OptionId::ProcReversed,
                OptionId::ProcTree,
                OptionId::ProcAggregate,
                OptionId::ProcCpuGraphs,
                OptionId::ProcFollowDetailed,
                OptionId::ProcPerCore,
                OptionId::ProcMemBytes,
            ],
        }
    }
}

/// Value editor kind for an option row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionKind {
    Bool,
    Enum,
    Text,
    Int,
}

/// Every Windows 11 v1 setting editable live from the Options menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionId {
    ColorTheme,
    ThemeBackground,
    RoundedCorners,
    TerminalSync,
    BackgroundUpdate,
    SaveConfigOnExit,
    UpdateMs,
    ShownBoxes,
    ClockFormat,
    GraphSymbol,
    VimKeys,
    DisableMouse,
    CpuBottom,
    ProcLeft,
    MemBelowNet,
    ShowBattery,
    CpuUtilMode,
    FreqMode,
    ShowGpuInfo,
    GpuMirrorGraph,
    GraphSymbolGpu,
    NvmlMeasurePcieSpeeds,
    CustomGpuName0,
    CustomGpuName1,
    MemGraphs,
    ShowSwap,
    Base10Sizes,
    ShowDisks,
    ShowIoStat,
    IoMode,
    IoGraphCombined,
    DisksFilter,
    NetIface,
    NetAuto,
    NetSync,
    NetDownload,
    NetUpload,
    SwapUploadDownload,
    Base10Bitrate,
    ProcSorting,
    ProcReversed,
    ProcTree,
    ProcAggregate,
    ProcCpuGraphs,
    ProcFollowDetailed,
    ProcPerCore,
    ProcMemBytes,
}

const CPU_UTIL_MODES: [&str; 2] = ["time", "utility"];
const FREQ_MODES: [&str; 5] = ["average", "highest", "lowest", "first", "range"];
const PROC_SORT_MODES: [&str; 6] = [
    "cpu lazy",
    "cpu direct",
    "memory",
    "pid",
    "program",
    "threads",
];

impl OptionId {
    pub const fn key_name(self) -> &'static str {
        match self {
            Self::ColorTheme => "color_theme",
            Self::ThemeBackground => "theme_background",
            Self::RoundedCorners => "rounded_corners",
            Self::TerminalSync => "terminal_sync",
            Self::BackgroundUpdate => "background_update",
            Self::SaveConfigOnExit => "save_config_on_exit",
            Self::UpdateMs => "update_ms",
            Self::ShownBoxes => "shown_boxes",
            Self::ClockFormat => "clock_format",
            Self::GraphSymbol => "graph_symbol",
            Self::VimKeys => "vim_keys",
            Self::DisableMouse => "disable_mouse",
            Self::CpuBottom => "cpu_bottom",
            Self::ProcLeft => "proc_left",
            Self::MemBelowNet => "mem_below_net",
            Self::ShowBattery => "show_battery",
            Self::CpuUtilMode => "cpu_util_mode",
            Self::FreqMode => "freq_mode",
            Self::ShowGpuInfo => "show_gpu_info",
            Self::GpuMirrorGraph => "gpu_mirror_graph",
            Self::GraphSymbolGpu => "graph_symbol_gpu",
            Self::NvmlMeasurePcieSpeeds => "nvml_measure_pcie_speeds",
            Self::CustomGpuName0 => "custom_gpu_name0",
            Self::CustomGpuName1 => "custom_gpu_name1",
            Self::MemGraphs => "mem_graphs",
            Self::ShowSwap => "show_swap",
            Self::Base10Sizes => "base_10_sizes",
            Self::ShowDisks => "show_disks",
            Self::ShowIoStat => "show_io_stat",
            Self::IoMode => "io_mode",
            Self::IoGraphCombined => "io_graph_combined",
            Self::DisksFilter => "disks_filter",
            Self::NetIface => "net_iface",
            Self::NetAuto => "net_auto",
            Self::NetSync => "net_sync",
            Self::NetDownload => "net_download",
            Self::NetUpload => "net_upload",
            Self::SwapUploadDownload => "swap_upload_download",
            Self::Base10Bitrate => "base_10_bitrate",
            Self::ProcSorting => "proc_sorting",
            Self::ProcReversed => "proc_reversed",
            Self::ProcTree => "proc_tree",
            Self::ProcAggregate => "proc_aggregate",
            Self::ProcCpuGraphs => "proc_cpu_graphs",
            Self::ProcFollowDetailed => "proc_follow_detailed",
            Self::ProcPerCore => "proc_per_core",
            Self::ProcMemBytes => "proc_mem_bytes",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::ColorTheme => "Active color theme palette with instant live preview.",
            Self::ThemeBackground => "Fill background with theme color; False keeps terminal bg.",
            Self::RoundedCorners => "Draw rounded box corners instead of square borders.",
            Self::TerminalSync => "Wrap frames in DEC 2026 synchronized output sequences.",
            Self::BackgroundUpdate => {
                "Keep refreshing live telemetry while overlay menus are open."
            }
            Self::SaveConfigOnExit => "Save updated settings to btop.conf cleanly on exit.",
            Self::UpdateMs => "Telemetry refresh interval in milliseconds (100..=60000).",
            Self::ShownBoxes => "Space-separated panels: cpu mem net proc gpu0..gpu5.",
            Self::ClockFormat => "Top bar clock format (strftime plus /host /user /uptime).",
            Self::GraphSymbol => "Default graph glyph set: braille, block, or tty.",
            Self::VimKeys => "Enable h/j/k/l/g/G navigation (Help -> Shift+H, Kill -> Shift+K).",
            Self::DisableMouse => "Ignore mouse clicks and wheel events in the TUI.",
            Self::CpuBottom => "Place the CPU box below MEM/NET/PROC panels.",
            Self::ProcLeft => "Place the process panel on the left half of the layout.",
            Self::MemBelowNet => "Stack MEM below NET instead of above NET.",
            Self::ShowBattery => "Show battery charge indicator when a battery is present.",
            Self::CpuUtilMode => "CPU metric: 'time' (NT kernel/user) or 'utility' (Task Mgr).",
            Self::FreqMode => "CPU frequency aggregation: average, highest, lowest, first, range.",
            Self::ShowGpuInfo => "Compact GPU summary in CPU box: Auto, On, or Off.",
            Self::GpuMirrorGraph => {
                "Mirror GPU utilization graph vertically around the centerline."
            }
            Self::GraphSymbolGpu => "Per-GPU graph symbol override (default, braille, block, tty).",
            Self::NvmlMeasurePcieSpeeds => "Query optional NVIDIA NVML PCIe Tx/Rx throughput.",
            Self::CustomGpuName0 => "Optional display name override for gpu0.",
            Self::CustomGpuName1 => "Optional display name override for gpu1.",
            Self::MemGraphs => "Show time-series graphs in MEM box (False = meters only).",
            Self::ShowSwap => "Show Windows pagefile / swap usage bar in MEM box.",
            Self::Base10Sizes => "Display byte sizes in base-10 (GB) instead of binary (GiB).",
            Self::ShowDisks => "Display local fixed NTFS/ReFS volumes inside MEM box.",
            Self::ShowIoStat => "Show per-drive read/write throughput stats on disk rows.",
            Self::IoMode => "Show dedicated Disk I/O graphs instead of capacity bars.",
            Self::IoGraphCombined => "Combine disk read and write into a single I/O graph.",
            Self::DisksFilter => "Drive filter: e.g. 'C D' or 'exclude=E F' (blank = all).",
            Self::NetIface => "Preferred network adapter name (blank = auto-select active).",
            Self::NetAuto => "Auto-scale NET graphs to observed traffic peaks.",
            Self::NetSync => "Use identical Y-axis scale for Download and Upload graphs.",
            Self::NetDownload => "Fixed Download graph ceiling in Mbit/s when net_auto=False.",
            Self::NetUpload => "Fixed Upload graph ceiling in Mbit/s when net_auto=False.",
            Self::SwapUploadDownload => "Swap top/bottom positions of Download and Upload graphs.",
            Self::Base10Bitrate => "Display network rates in bits/s (Mbit/s) vs bytes/s.",
            Self::ProcSorting => "Process table sort column (cpu lazy, memory, pid, etc.).",
            Self::ProcReversed => "Reverse active process table sort direction.",
            Self::ProcTree => "Render processes as a parent/child hierarchy tree.",
            Self::ProcAggregate => "Sum descendant CPU% into parent rows when in tree view.",
            Self::ProcCpuGraphs => "Render inline per-process CPU sparkline column.",
            Self::ProcFollowDetailed => "Keep selection locked onto the inspected detail process.",
            Self::ProcPerCore => "Scale process CPU% relative to one logical core vs total.",
            Self::ProcMemBytes => "Show process memory in bytes (False = % of physical RAM).",
        }
    }

    pub const fn kind(self) -> OptionKind {
        match self {
            Self::ColorTheme
            | Self::GraphSymbol
            | Self::CpuUtilMode
            | Self::FreqMode
            | Self::ShowGpuInfo
            | Self::GraphSymbolGpu
            | Self::ProcSorting => OptionKind::Enum,
            Self::UpdateMs | Self::NetDownload | Self::NetUpload => OptionKind::Int,
            Self::ShownBoxes
            | Self::ClockFormat
            | Self::CustomGpuName0
            | Self::CustomGpuName1
            | Self::DisksFilter
            | Self::NetIface => OptionKind::Text,
            _ => OptionKind::Bool,
        }
    }

    pub fn value_string(self, cfg: &Config) -> String {
        match self {
            Self::ColorTheme => cfg.color_theme.clone(),
            Self::ThemeBackground => fmt_bool(cfg.theme_background),
            Self::RoundedCorners => fmt_bool(cfg.rounded_corners),
            Self::TerminalSync => fmt_bool(cfg.terminal_sync),
            Self::BackgroundUpdate => fmt_bool(cfg.background_update),
            Self::SaveConfigOnExit => fmt_bool(cfg.save_config_on_exit),
            Self::UpdateMs => cfg.update_ms.to_string(),
            Self::ShownBoxes => cfg.shown_boxes.clone(),
            Self::ClockFormat => cfg.clock_format.clone(),
            Self::GraphSymbol => cfg.graph_symbol.as_str().to_owned(),
            Self::VimKeys => fmt_bool(cfg.vim_keys),
            Self::DisableMouse => fmt_bool(cfg.disable_mouse),
            Self::CpuBottom => fmt_bool(cfg.cpu_bottom),
            Self::ProcLeft => fmt_bool(cfg.proc_left),
            Self::MemBelowNet => fmt_bool(cfg.mem_below_net),
            Self::ShowBattery => fmt_bool(cfg.show_battery),
            Self::CpuUtilMode => cfg.cpu_util_mode.clone(),
            Self::FreqMode => cfg.freq_mode.clone(),
            Self::ShowGpuInfo => cfg.show_gpu_info.as_str().to_owned(),
            Self::GpuMirrorGraph => fmt_bool(cfg.gpu_mirror_graph),
            Self::GraphSymbolGpu => cfg
                .graph_symbol_gpu
                .map_or("default", GraphSymbol::as_str)
                .to_owned(),
            Self::NvmlMeasurePcieSpeeds => fmt_bool(cfg.nvml_measure_pcie_speeds),
            Self::CustomGpuName0 => cfg.custom_gpu_names[0].clone(),
            Self::CustomGpuName1 => cfg.custom_gpu_names[1].clone(),
            Self::MemGraphs => fmt_bool(cfg.mem_graphs),
            Self::ShowSwap => fmt_bool(cfg.show_swap),
            Self::Base10Sizes => fmt_bool(cfg.base_10_sizes),
            Self::ShowDisks => fmt_bool(cfg.show_disks),
            Self::ShowIoStat => fmt_bool(cfg.show_io_stat),
            Self::IoMode => fmt_bool(cfg.io_mode),
            Self::IoGraphCombined => fmt_bool(cfg.io_graph_combined),
            Self::DisksFilter => cfg.disks_filter.clone(),
            Self::NetIface => cfg.net_iface.clone(),
            Self::NetAuto => fmt_bool(cfg.net_auto),
            Self::NetSync => fmt_bool(cfg.net_sync),
            Self::NetDownload => cfg.net_download.to_string(),
            Self::NetUpload => cfg.net_upload.to_string(),
            Self::SwapUploadDownload => fmt_bool(cfg.swap_upload_download),
            Self::Base10Bitrate => fmt_bool(cfg.base_10_bitrate),
            Self::ProcSorting => cfg.proc_sorting.clone(),
            Self::ProcReversed => fmt_bool(cfg.proc_reversed),
            Self::ProcTree => fmt_bool(cfg.proc_tree),
            Self::ProcAggregate => fmt_bool(cfg.proc_aggregate),
            Self::ProcCpuGraphs => fmt_bool(cfg.proc_cpu_graphs),
            Self::ProcFollowDetailed => fmt_bool(cfg.proc_follow_detailed),
            Self::ProcPerCore => fmt_bool(cfg.proc_per_core),
            Self::ProcMemBytes => fmt_bool(cfg.proc_mem_bytes),
        }
    }

    /// Toggles booleans or steps enums/integers cleanly (`dir = -1` left, `+1` right).
    pub fn step(self, cfg: &mut Config, dir: i8) -> bool {
        self.step_with_themes(cfg, dir, &[])
    }

    /// Like [`Self::step`], using `available_themes` when cycling [`OptionId::ColorTheme`].
    pub fn step_with_themes(self, cfg: &mut Config, dir: i8, available_themes: &[String]) -> bool {
        match self {
            Self::ColorTheme => {
                if available_themes.is_empty() {
                    cfg.color_theme = cycle_str(&BUILTIN_THEME_NAMES, &cfg.color_theme, dir);
                } else {
                    let refs: Vec<&str> = available_themes.iter().map(String::as_str).collect();
                    cfg.color_theme = cycle_str(&refs, &cfg.color_theme, dir);
                }
            }
            Self::ThemeBackground => cfg.theme_background = !cfg.theme_background,
            Self::RoundedCorners => cfg.rounded_corners = !cfg.rounded_corners,
            Self::TerminalSync => cfg.terminal_sync = !cfg.terminal_sync,
            Self::BackgroundUpdate => cfg.background_update = !cfg.background_update,
            Self::SaveConfigOnExit => cfg.save_config_on_exit = !cfg.save_config_on_exit,
            Self::UpdateMs => {
                let delta: i32 = if dir >= 0 { 100 } else { -100 };
                let next = i64::from(cfg.update_ms)
                    .saturating_add(i64::from(delta))
                    .clamp(100, 60_000);
                cfg.update_ms = u32::try_from(next).unwrap_or(100);
            }
            Self::GraphSymbol => {
                let all = [GraphSymbol::Braille, GraphSymbol::Block, GraphSymbol::Tty];
                cfg.graph_symbol = cycle_copy(&all, cfg.graph_symbol, dir);
            }
            Self::VimKeys => cfg.vim_keys = !cfg.vim_keys,
            Self::DisableMouse => cfg.disable_mouse = !cfg.disable_mouse,
            Self::CpuBottom => cfg.cpu_bottom = !cfg.cpu_bottom,
            Self::ProcLeft => cfg.proc_left = !cfg.proc_left,
            Self::MemBelowNet => cfg.mem_below_net = !cfg.mem_below_net,
            Self::ShowBattery => cfg.show_battery = !cfg.show_battery,
            Self::CpuUtilMode => {
                cfg.cpu_util_mode = cycle_str(&CPU_UTIL_MODES, &cfg.cpu_util_mode, dir);
            }
            Self::FreqMode => {
                cfg.freq_mode = cycle_str(&FREQ_MODES, &cfg.freq_mode, dir);
            }
            Self::ShowGpuInfo => {
                let all = [ShowGpuInfo::Auto, ShowGpuInfo::On, ShowGpuInfo::Off];
                cfg.show_gpu_info = cycle_copy(&all, cfg.show_gpu_info, dir);
            }
            Self::GpuMirrorGraph => cfg.gpu_mirror_graph = !cfg.gpu_mirror_graph,
            Self::GraphSymbolGpu => {
                let all = [
                    None,
                    Some(GraphSymbol::Braille),
                    Some(GraphSymbol::Block),
                    Some(GraphSymbol::Tty),
                ];
                cfg.graph_symbol_gpu = cycle_copy(&all, cfg.graph_symbol_gpu, dir);
            }
            Self::NvmlMeasurePcieSpeeds => {
                cfg.nvml_measure_pcie_speeds = !cfg.nvml_measure_pcie_speeds;
            }
            Self::MemGraphs => cfg.mem_graphs = !cfg.mem_graphs,
            Self::ShowSwap => cfg.show_swap = !cfg.show_swap,
            Self::Base10Sizes => cfg.base_10_sizes = !cfg.base_10_sizes,
            Self::ShowDisks => cfg.show_disks = !cfg.show_disks,
            Self::ShowIoStat => cfg.show_io_stat = !cfg.show_io_stat,
            Self::IoMode => cfg.io_mode = !cfg.io_mode,
            Self::IoGraphCombined => cfg.io_graph_combined = !cfg.io_graph_combined,
            Self::NetAuto => cfg.net_auto = !cfg.net_auto,
            Self::NetSync => cfg.net_sync = !cfg.net_sync,
            Self::NetDownload => {
                cfg.net_download = step_positive_u64(cfg.net_download, dir, 50);
            }
            Self::NetUpload => {
                cfg.net_upload = step_positive_u64(cfg.net_upload, dir, 50);
            }
            Self::SwapUploadDownload => cfg.swap_upload_download = !cfg.swap_upload_download,
            Self::Base10Bitrate => cfg.base_10_bitrate = !cfg.base_10_bitrate,
            Self::ProcSorting => {
                cfg.proc_sorting = cycle_str(&PROC_SORT_MODES, &cfg.proc_sorting, dir);
            }
            Self::ProcReversed => cfg.proc_reversed = !cfg.proc_reversed,
            Self::ProcTree => cfg.proc_tree = !cfg.proc_tree,
            Self::ProcAggregate => cfg.proc_aggregate = !cfg.proc_aggregate,
            Self::ProcCpuGraphs => cfg.proc_cpu_graphs = !cfg.proc_cpu_graphs,
            Self::ProcFollowDetailed => cfg.proc_follow_detailed = !cfg.proc_follow_detailed,
            Self::ProcPerCore => cfg.proc_per_core = !cfg.proc_per_core,
            Self::ProcMemBytes => cfg.proc_mem_bytes = !cfg.proc_mem_bytes,
            Self::ShownBoxes
            | Self::ClockFormat
            | Self::CustomGpuName0
            | Self::CustomGpuName1
            | Self::DisksFilter
            | Self::NetIface => return false,
        }
        true
    }

    /// Validates and commits an inline [`TextEdit`] value onto `cfg`.
    pub fn apply_text(self, cfg: &mut Config, input: &str) -> Result<(), String> {
        let trimmed = input.trim();
        match self {
            Self::UpdateMs => {
                let parsed: u32 = trimmed
                    .parse()
                    .map_err(|_| "update_ms must be an integer between 100 and 60000".to_owned())?;
                if !(100..=60_000).contains(&parsed) {
                    return Err("update_ms must be between 100 and 60000 ms".to_owned());
                }
                cfg.update_ms = parsed;
            }
            Self::NetDownload => {
                let parsed: u64 = trimmed
                    .parse()
                    .map_err(|_| "net_download must be a positive integer (Mbit/s)".to_owned())?;
                if parsed == 0 || parsed > 1_000_000 {
                    return Err("net_download must be between 1 and 1000000 Mbit/s".to_owned());
                }
                cfg.net_download = parsed;
            }
            Self::NetUpload => {
                let parsed: u64 = trimmed
                    .parse()
                    .map_err(|_| "net_upload must be a positive integer (Mbit/s)".to_owned())?;
                if parsed == 0 || parsed > 1_000_000 {
                    return Err("net_upload must be between 1 and 1000000 Mbit/s".to_owned());
                }
                cfg.net_upload = parsed;
            }
            Self::ShownBoxes => {
                if trimmed.is_empty() {
                    return Err(
                        "shown_boxes must include at least one valid box (cpu, mem, net, proc, gpu0..gpu5)"
                            .to_owned(),
                    );
                }
                for token in trimmed.split_whitespace() {
                    if !matches!(
                        token.to_ascii_lowercase().as_str(),
                        "cpu"
                            | "mem"
                            | "net"
                            | "proc"
                            | "gpu0"
                            | "gpu1"
                            | "gpu2"
                            | "gpu3"
                            | "gpu4"
                            | "gpu5"
                    ) {
                        return Err(format!(
                            "unknown box '{token}' (expected cpu, mem, net, proc, gpu0..gpu5)"
                        ));
                    }
                }
                trimmed.clone_into(&mut cfg.shown_boxes);
            }
            Self::ClockFormat => {
                trimmed.clone_into(&mut cfg.clock_format);
            }
            Self::CustomGpuName0 => {
                trimmed.clone_into(&mut cfg.custom_gpu_names[0]);
            }
            Self::CustomGpuName1 => {
                trimmed.clone_into(&mut cfg.custom_gpu_names[1]);
            }
            Self::DisksFilter => {
                trimmed.clone_into(&mut cfg.disks_filter);
            }
            Self::NetIface => {
                trimmed.clone_into(&mut cfg.net_iface);
            }
            _ => return Err("option does not accept free-form text input".to_owned()),
        }
        Ok(())
    }
}

fn fmt_bool(v: bool) -> String {
    if v {
        "True".to_owned()
    } else {
        "False".to_owned()
    }
}

fn cycle_str(choices: &[&str], current: &str, dir: i8) -> String {
    let idx = choices
        .iter()
        .position(|c| c.eq_ignore_ascii_case(current))
        .unwrap_or(0);
    let next = if dir >= 0 {
        (idx + 1) % choices.len()
    } else if idx == 0 {
        choices.len() - 1
    } else {
        idx - 1
    };
    choices[next].to_owned()
}

fn cycle_copy<T: Copy + PartialEq>(choices: &[T], current: T, dir: i8) -> T {
    let idx = choices.iter().position(|c| *c == current).unwrap_or(0);
    let next = if dir >= 0 {
        (idx + 1) % choices.len()
    } else if idx == 0 {
        choices.len() - 1
    } else {
        idx - 1
    };
    choices[next]
}

fn step_positive_u64(current: u64, dir: i8, step: u64) -> u64 {
    if dir >= 0 {
        current.saturating_add(step).min(1_000_000)
    } else {
        current.saturating_sub(step).max(1)
    }
}

/// Interactive state for the categorized Options menu (`F2` / `o`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionsState {
    pub category: OptionCategory,
    pub selected: usize,
    pub scroll_offset: usize,
    pub editing: Option<TextEdit>,
    pub validation_error: Option<String>,
}

impl Default for OptionsState {
    fn default() -> Self {
        Self {
            category: OptionCategory::General,
            selected: 0,
            scroll_offset: 0,
            editing: None,
            validation_error: None,
        }
    }
}

impl OptionsState {
    pub fn options(&self) -> &'static [OptionId] {
        self.category.options()
    }

    pub fn selected_option(&self) -> Option<OptionId> {
        self.options().get(self.selected).copied()
    }

    pub fn set_category(&mut self, category: OptionCategory) {
        if self.category != category {
            self.category = category;
            self.selected = 0;
            self.scroll_offset = 0;
            self.editing = None;
            self.validation_error = None;
        }
    }

    pub fn next_category(&mut self) {
        self.set_category(self.category.next());
    }

    pub fn prev_category(&mut self) {
        self.set_category(self.category.prev());
    }

    pub fn move_up(&mut self, visible_rows: usize) {
        let len = self.options().len();
        if len == 0 {
            return;
        }
        self.selected = self.selected.saturating_sub(1);
        self.validation_error = None;
        self.clamp_scroll(visible_rows);
    }

    pub fn move_down(&mut self, visible_rows: usize) {
        let len = self.options().len();
        if len == 0 {
            return;
        }
        self.selected = (self.selected + 1).min(len - 1);
        self.validation_error = None;
        self.clamp_scroll(visible_rows);
    }

    pub fn select_index(&mut self, index: usize, visible_rows: usize) {
        let len = self.options().len();
        if len == 0 {
            return;
        }
        self.selected = index.min(len - 1);
        self.validation_error = None;
        self.clamp_scroll(visible_rows);
    }

    pub fn clamp_scroll(&mut self, visible_rows: usize) {
        let len = self.options().len();
        let vis = visible_rows.max(1);
        if len == 0 {
            self.selected = 0;
            self.scroll_offset = 0;
            return;
        }
        self.selected = self.selected.min(len - 1);
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset.saturating_add(vis) {
            self.scroll_offset = self.selected.saturating_add(1).saturating_sub(vis);
        }
        let max_scroll = len.saturating_sub(vis);
        self.scroll_offset = self.scroll_offset.min(max_scroll);
    }

    pub fn begin_edit(&mut self, cfg: &Config) -> bool {
        let Some(opt) = self.selected_option() else {
            return false;
        };
        if matches!(opt.kind(), OptionKind::Text | OptionKind::Int) {
            self.editing = Some(TextEdit::new(opt.value_string(cfg)));
            self.validation_error = None;
            true
        } else {
            false
        }
    }

    pub fn cancel_edit(&mut self) {
        self.editing = None;
        self.validation_error = None;
    }

    pub fn commit_edit(&mut self, cfg: &mut Config) -> Result<(), String> {
        let Some(opt) = self.selected_option() else {
            self.editing = None;
            return Ok(());
        };
        let Some(ref editor) = self.editing else {
            return Ok(());
        };
        match opt.apply_text(cfg, editor.as_str()) {
            Ok(()) => {
                self.editing = None;
                self.validation_error = None;
                Ok(())
            }
            Err(err) => {
                self.validation_error = Some(err.clone());
                Err(err)
            }
        }
    }
}

/// One row in the scrollable Windows 11 v1 Help keybindings reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HelpEntry {
    pub key: &'static str,
    pub description: &'static str,
    pub is_section: bool,
}

impl HelpEntry {
    const fn section(title: &'static str) -> Self {
        Self {
            key: title,
            description: "",
            is_section: true,
        }
    }

    const fn bind(key: &'static str, description: &'static str) -> Self {
        Self {
            key,
            description,
            is_section: false,
        }
    }
}

/// Accurate Windows 11 v1 keybinding reference rendered inside the Help overlay (`F1` / `?` / `h`).
pub const HELP_BINDINGS: &[HelpEntry] = &[
    HelpEntry::section("Menus & General"),
    HelpEntry::bind("Esc / m", "Open or close the game-inspired Main menu"),
    HelpEntry::bind("F2 / o", "Open Options menu (live editing & theme preview)"),
    HelpEntry::bind(
        "F1 / ? / h",
        "Open Help keybindings reference (Shift+H when vim_keys=True)",
    ),
    HelpEntry::bind(
        "q / Ctrl+C",
        "Quit btop-windows and restore Windows Terminal",
    ),
    HelpEntry::bind("Ctrl+R", "Reload btop.conf and refresh collectors"),
    HelpEntry::bind("Ctrl+L", "Force full screen redraw"),
    HelpEntry::bind(
        "+ / -",
        "Adjust update_ms cadence (+/- 100 ms, floor 100 ms)",
    ),
    HelpEntry::section("Layout & Panels"),
    HelpEntry::bind("1 / 2 / 3 / 4", "Toggle CPU, MEM, NET, and PROC panels"),
    HelpEntry::bind(
        "5 .. 9 / 0",
        "Toggle dedicated GPU panels gpu0 through gpu5",
    ),
    HelpEntry::bind("p / Shift+P", "Cycle layout presets forward / backward"),
    HelpEntry::bind(
        "d / i",
        "Toggle local fixed disks list / Disk I/O graph mode",
    ),
    HelpEntry::section("Network Panel"),
    HelpEntry::bind("b / n", "Select previous / next hardware network adapter"),
    HelpEntry::bind(
        "a / y / z",
        "Toggle auto-scale, sync scale, or reset session totals",
    ),
    HelpEntry::section("Processes & Tree"),
    HelpEntry::bind(
        "Up / Down / PgUp / PgDn",
        "Navigate process list (j/k/g/G when vim_keys=True)",
    ),
    HelpEntry::bind(
        "Left / Right / r",
        "Cycle sort column (h/l if vim_keys) / reverse sort",
    ),
    HelpEntry::bind(
        "e / Space / Shift+E",
        "Toggle tree view / collapse branch / collapse all",
    ),
    HelpEntry::bind(
        "f or / / Del",
        "Edit process filter (prefix '!' for regex) / clear",
    ),
    HelpEntry::bind(
        "Enter / Esc",
        "Open or close Selected Process Detail overlay",
    ),
    HelpEntry::bind(
        "u / Shift+F",
        "Pause process list / follow selected process identity",
    ),
    HelpEntry::bind("c / %", "Toggle per-core CPU% / memory bytes vs percent"),
    HelpEntry::section("Safe Windows Process & Service Actions"),
    HelpEntry::bind("t", "Graceful close via WM_CLOSE (interactive GUI apps)"),
    HelpEntry::bind(
        "k (K if vim_keys)",
        "Guarded TerminateProcess (CreateTime + critical guard)",
    ),
    HelpEntry::bind(
        "Shift+N",
        "Open Windows Priority Class selector (SetPriorityClass)",
    ),
    HelpEntry::bind(
        "Shift+S",
        "Toggle Windows Services (SCM) view inside PROC panel",
    ),
    HelpEntry::bind(
        "s / x (Services)",
        "Start / Stop selected service (requires Administrator)",
    ),
];

/// Interactive scroll state for the Help keybindings overlay.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HelpState {
    pub scroll_offset: usize,
}

impl HelpState {
    pub fn max_scroll(visible_rows: usize) -> usize {
        HELP_BINDINGS.len().saturating_sub(visible_rows.max(1))
    }

    pub fn scroll_up(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }

    pub fn scroll_down(&mut self, lines: usize, visible_rows: usize) {
        self.scroll_offset = self
            .scroll_offset
            .saturating_add(lines)
            .min(Self::max_scroll(visible_rows));
    }

    pub fn home(&mut self) {
        self.scroll_offset = 0;
    }

    pub fn end(&mut self, visible_rows: usize) {
        self.scroll_offset = Self::max_scroll(visible_rows);
    }

    pub fn clamp(&mut self, visible_rows: usize) {
        self.scroll_offset = self.scroll_offset.min(Self::max_scroll(visible_rows));
    }
}

/// Top-level overlay menu stack rendered on top of the dimmed live dashboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayMenu {
    Main(MainMenuState),
    Options(OptionsState),
    Help(HelpState),
    MessageBox {
        dialog: MessageBoxState,
        return_to: Option<Box<OverlayMenu>>,
    },
}

impl OverlayMenu {
    pub fn main() -> Self {
        Self::Main(MainMenuState::default())
    }

    pub fn options() -> Self {
        Self::Options(OptionsState::default())
    }

    pub fn help() -> Self {
        Self::Help(HelpState::default())
    }

    pub fn message_box(dialog: MessageBoxState, return_to: Option<Self>) -> Self {
        Self::MessageBox {
            dialog,
            return_to: return_to.map(Box::new),
        }
    }

    pub fn is_text_editing(&self) -> bool {
        matches!(self, Self::Options(opts) if opts.editing.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_edit_supports_utf8_cursor_navigation_insert_backspace_and_delete() {
        let mut edit = TextEdit::new("aé");
        assert_eq!(edit.cursor_char_idx(), 2);
        edit.move_left();
        assert_eq!(edit.cursor_char_idx(), 1);
        edit.insert('X');
        assert_eq!(edit.as_str(), "aXé");
        assert_eq!(edit.cursor_char_idx(), 2);
        edit.home();
        assert_eq!(edit.cursor_char_idx(), 0);
        assert!(edit.delete());
        assert_eq!(edit.as_str(), "Xé");
        edit.end();
        assert!(edit.backspace());
        assert_eq!(edit.as_str(), "X");
        assert!(edit.backspace());
        assert_eq!(edit.as_str(), "");
        assert!(!edit.backspace());
    }

    #[test]
    fn options_validation_rejects_invalid_values_and_applies_valid_edits_live() {
        let mut cfg = Config::default();
        let mut opts = OptionsState::default();

        // Cycle color_theme for instant live theme preview.
        assert_eq!(opts.selected_option(), Some(OptionId::ColorTheme));
        assert!(OptionId::ColorTheme.step(&mut cfg, 1));
        assert_eq!(cfg.color_theme, "dracula");
        assert!(OptionId::ColorTheme.step(&mut cfg, -1));
        assert_eq!(cfg.color_theme, "Default");

        // Move to update_ms and test invalid + valid TextEdit commits.
        let update_idx = OptionCategory::General
            .options()
            .iter()
            .position(|&o| o == OptionId::UpdateMs)
            .expect("update_ms in General");
        opts.select_index(update_idx, 12);
        assert!(opts.begin_edit(&cfg));
        opts.editing = Some(TextEdit::new("50"));
        assert!(opts.commit_edit(&mut cfg).is_err());
        assert!(opts.validation_error.is_some());
        assert_eq!(cfg.update_ms, 2000);

        opts.editing = Some(TextEdit::new("abc"));
        assert!(opts.commit_edit(&mut cfg).is_err());

        opts.editing = Some(TextEdit::new("500"));
        assert!(opts.commit_edit(&mut cfg).is_ok());
        assert_eq!(cfg.update_ms, 500);
        assert!(opts.editing.is_none());
        assert!(opts.validation_error.is_none());

        // Validate shown_boxes rejects unknown tokens and accepts valid boxes.
        assert!(OptionId::ShownBoxes.apply_text(&mut cfg, "").is_err());
        assert!(
            OptionId::ShownBoxes
                .apply_text(&mut cfg, "cpu bogus")
                .is_err()
        );
        assert!(
            OptionId::ShownBoxes
                .apply_text(&mut cfg, "cpu gpu0 proc")
                .is_ok()
        );
        assert_eq!(cfg.shown_boxes, "cpu gpu0 proc");

        // Validate integer bandwidth bounds in NET tab.
        assert!(OptionId::NetDownload.apply_text(&mut cfg, "0").is_err());
        assert!(OptionId::NetDownload.apply_text(&mut cfg, "1000").is_ok());
        assert_eq!(cfg.net_download, 1000);
    }

    #[test]
    fn main_menu_options_and_help_navigation_wrap_and_clamp() {
        let mut main = MainMenuState::default();
        assert_eq!(main.selected_choice(), MainMenuChoice::Options);
        main.move_up();
        assert_eq!(main.selected_choice(), MainMenuChoice::Quit);
        main.move_down();
        assert_eq!(main.selected_choice(), MainMenuChoice::Options);

        let mut opts = OptionsState::default();
        opts.next_category();
        assert_eq!(opts.category, OptionCategory::Cpu);
        opts.prev_category();
        assert_eq!(opts.category, OptionCategory::General);
        opts.prev_category();
        assert_eq!(opts.category, OptionCategory::Proc);

        let mut help = HelpState::default();
        help.scroll_down(100, 10);
        assert_eq!(help.scroll_offset, HelpState::max_scroll(10));
        help.scroll_up(5);
        assert_eq!(help.scroll_offset, HelpState::max_scroll(10) - 5);
        help.home();
        assert_eq!(help.scroll_offset, 0);
    }

    #[test]
    fn messagebox_ok_and_yes_no_return_expected_keyboard_and_mouse_results() {
        let mut ok = MessageBoxState::ok("Info", vec!["Saved.".to_owned()]);
        assert_eq!(
            ok.handle_key(KeyCode::Enter),
            MessageBoxResult::Confirmed(MessageBoxAction::DismissOnly)
        );
        assert_eq!(ok.handle_key(KeyCode::Esc), MessageBoxResult::Cancelled);
        assert_eq!(
            ok.handle_button_click(true),
            MessageBoxResult::Confirmed(MessageBoxAction::DismissOnly)
        );

        let mut yn = MessageBoxState::yes_no(
            "Reset Options",
            vec!["Reset all options to defaults?".to_owned()],
            MessageBoxAction::ResetOptionsToDefaults,
            false,
        );
        assert_eq!(yn.handle_key(KeyCode::Enter), MessageBoxResult::Cancelled);
        assert_eq!(yn.handle_key(KeyCode::Left), MessageBoxResult::Pending);
        assert!(yn.selected_confirm);
        assert_eq!(
            yn.handle_key(KeyCode::Enter),
            MessageBoxResult::Confirmed(MessageBoxAction::ResetOptionsToDefaults)
        );
        assert_eq!(
            yn.handle_key(KeyCode::Char('n')),
            MessageBoxResult::Cancelled
        );
        assert_eq!(
            yn.handle_key(KeyCode::Char('y')),
            MessageBoxResult::Confirmed(MessageBoxAction::ResetOptionsToDefaults)
        );
        assert_eq!(
            yn.handle_button_click(true),
            MessageBoxResult::Confirmed(MessageBoxAction::ResetOptionsToDefaults)
        );
        assert_eq!(yn.handle_button_click(false), MessageBoxResult::Cancelled);
    }
}
