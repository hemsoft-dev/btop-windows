//! Configuration (`btop.conf`) and `.theme` parsing with upstream `aristocratos/btop`
//! compatibility.

#![forbid(unsafe_code)]

pub mod theme;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use theme::{
    ALL_THEME_KEYS, BUNDLED_THEMES, ThemeColor, ThemeError, ThemeGradient101, ThemeRgb,
    ThemeSearchPaths, ThemeSpec, load_bundled_theme, parse_color_value, parse_theme,
};

/// Resolved `btop.conf` file path and discovery mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigLocation {
    /// Explicitly specified via `-c` / `--config <file>`.
    Explicit(PathBuf),
    /// Portable mode: `btop.conf` adjacent to `btop.exe`.
    Portable(PathBuf),
    /// Standard Windows user config: `%APPDATA%\btop\btop.conf`.
    AppData(PathBuf),
}

impl ConfigLocation {
    pub fn path(&self) -> &Path {
        match self {
            Self::Explicit(p) | Self::Portable(p) | Self::AppData(p) => p.as_path(),
        }
    }
}

/// Resolves the active `btop.conf` location using strict precedence:
/// 1. Explicit `-c` / `--config <file>`
/// 2. Portable mode: exe-adjacent `btop.conf` when that file already exists
/// 3. User config directory: `%APPDATA%\btop\btop.conf` on Windows, or
///    `$XDG_CONFIG_HOME/btop-fleet/btop.conf` (`$HOME/.config/btop-fleet/btop.conf`) on Unix
/// 4. Fallback to exe-adjacent `btop.conf` when no user config directory is available
pub fn resolve_config_location(explicit_path: Option<&Path>) -> Option<ConfigLocation> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    #[cfg(windows)]
    {
        let appdata = std::env::var_os("APPDATA")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        resolve_config_location_with(explicit_path, exe_dir.as_deref(), appdata.as_deref())
    }
    #[cfg(not(windows))]
    {
        let xdg = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        let home = std::env::var_os("HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        resolve_unix_config_location_with(
            explicit_path,
            exe_dir.as_deref(),
            xdg.as_deref(),
            home.as_deref(),
        )
    }
}

/// Pure, testable variant of [`resolve_config_location`] accepting injected Windows directories.
pub fn resolve_config_location_with(
    explicit_path: Option<&Path>,
    exe_dir: Option<&Path>,
    appdata: Option<&Path>,
) -> Option<ConfigLocation> {
    resolve_config_location_in_subdir(explicit_path, exe_dir, appdata, "btop")
}

/// Pure, testable Unix config location resolver using `XDG_CONFIG_HOME` / `$HOME/.config`
/// and the `btop-fleet` subdir so it never collides with upstream `~/.config/btop`.
pub fn resolve_unix_config_location_with(
    explicit_path: Option<&Path>,
    exe_dir: Option<&Path>,
    xdg_config_home: Option<&Path>,
    home_dir: Option<&Path>,
) -> Option<ConfigLocation> {
    let config_root = unix_config_root(xdg_config_home, home_dir);
    resolve_config_location_in_subdir(explicit_path, exe_dir, config_root.as_deref(), "btop-fleet")
}

pub(crate) fn unix_config_root(
    xdg_config_home: Option<&Path>,
    home_dir: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(xdg) = xdg_config_home
        && !xdg.as_os_str().is_empty()
        && xdg.is_absolute()
    {
        return Some(xdg.to_path_buf());
    }
    home_dir
        .filter(|h| !h.as_os_str().is_empty())
        .map(|h| h.join(".config"))
}

fn resolve_config_location_in_subdir(
    explicit_path: Option<&Path>,
    exe_dir: Option<&Path>,
    user_config_root: Option<&Path>,
    subdir: &str,
) -> Option<ConfigLocation> {
    if let Some(path) = explicit_path {
        return Some(ConfigLocation::Explicit(path.to_path_buf()));
    }
    if let Some(dir) = exe_dir {
        let portable = dir.join("btop.conf");
        if portable.is_file() {
            return Some(ConfigLocation::Portable(portable));
        }
    }
    if let Some(root) = user_config_root
        && !root.as_os_str().is_empty()
    {
        return Some(ConfigLocation::AppData(root.join(subdir).join("btop.conf")));
    }
    exe_dir.map(|dir| ConfigLocation::Portable(dir.join("btop.conf")))
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("invalid line {line}: {message}")]
    Parse { line: usize, message: String },
    #[error("invalid value for '{key}': {value}")]
    InvalidValue { key: String, value: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphSymbol {
    Braille,
    Block,
    Tty,
}

impl GraphSymbol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Braille => "braille",
            Self::Block => "block",
            Self::Tty => "tty",
        }
    }
}

/// Upstream `disable_presets` mode (`"Off"`, `"Default"`, `"Custom"`, or `"All"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisablePresets {
    #[default]
    Off,
    Default,
    Custom,
    All,
}

impl DisablePresets {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Default => "Default",
            Self::Custom => "Custom",
            Self::All => "All",
        }
    }

    pub fn parse(key: &str, val: &str) -> Result<Self, ConfigError> {
        match val.trim().to_ascii_lowercase().as_str() {
            "off" | "false" | "0" | "no" => Ok(Self::Off),
            "default" => Ok(Self::Default),
            "custom" => Ok(Self::Custom),
            "all" | "true" | "1" | "yes" => Ok(Self::All),
            _ => Err(ConfigError::InvalidValue {
                key: key.to_owned(),
                value: val.to_owned(),
            }),
        }
    }
}

/// Upstream `show_gpu_info` mode controlling compact GPU summary inside the CPU box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShowGpuInfo {
    #[default]
    Auto,
    On,
    Off,
}

impl ShowGpuInfo {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::On => "On",
            Self::Off => "Off",
        }
    }

    pub fn parse(key: &str, val: &str) -> Result<Self, ConfigError> {
        match val.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "on" | "true" | "1" | "yes" => Ok(Self::On),
            "off" | "false" | "0" | "no" => Ok(Self::Off),
            _ => Err(ConfigError::InvalidValue {
                key: key.to_owned(),
                value: val.to_owned(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub color_theme: String,
    pub theme_background: bool,
    pub truecolor: bool,
    pub force_tty: bool,
    pub rounded_corners: bool,
    pub terminal_sync: bool,
    pub background_update: bool,
    pub save_config_on_exit: bool,
    pub vim_keys: bool,
    pub disable_mouse: bool,
    pub graph_symbol: GraphSymbol,
    pub shown_boxes: String,
    pub presets: String,
    pub disable_presets: DisablePresets,
    pub cpu_bottom: bool,
    pub proc_left: bool,
    pub mem_below_net: bool,
    pub show_battery: bool,
    pub update_ms: u32,
    pub clock_format: String,
    pub proc_sorting: String,
    pub proc_reversed: bool,
    pub proc_tree: bool,
    pub proc_aggregate: bool,
    pub proc_cpu_graphs: bool,
    pub proc_follow_detailed: bool,
    pub proc_per_core: bool,
    pub proc_mem_bytes: bool,
    pub mem_graphs: bool,
    pub show_swap: bool,
    pub base_10_sizes: bool,
    pub disks_filter: String,
    pub show_disks: bool,
    pub show_io_stat: bool,
    pub io_mode: bool,
    pub io_graph_combined: bool,
    pub io_graph_speeds: String,
    pub net_iface: String,
    pub net_download: u64,
    pub net_upload: u64,
    pub net_auto: bool,
    pub net_sync: bool,
    pub swap_upload_download: bool,
    pub base_10_bitrate: bool,
    pub cpu_util_mode: String,
    pub freq_mode: String,
    pub show_gpu_info: ShowGpuInfo,
    pub gpu_mirror_graph: bool,
    pub graph_symbol_gpu: Option<GraphSymbol>,
    pub nvml_measure_pcie_speeds: bool,
    pub custom_gpu_names: [String; 6],
    /// Unknown or deferred upstream keys preserved verbatim when saving `btop.conf`.
    pub preserved_unknown: BTreeMap<String, String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            color_theme: "Default".to_owned(),
            theme_background: true,
            truecolor: true,
            force_tty: false,
            rounded_corners: true,
            terminal_sync: true,
            background_update: true,
            save_config_on_exit: true,
            vim_keys: false,
            disable_mouse: false,
            graph_symbol: GraphSymbol::Braille,
            shown_boxes: "cpu mem net proc".to_owned(),
            presets: "cpu:1:default,proc:0:default cpu:0:default,mem:0:default,net:0:default cpu:0:block,mem:0:block,net:0:block".to_owned(),
            disable_presets: DisablePresets::Off,
            cpu_bottom: false,
            proc_left: false,
            mem_below_net: false,
            show_battery: true,
            update_ms: 2000,
            clock_format: "%X".to_owned(),
            proc_sorting: "cpu lazy".to_owned(),
            proc_reversed: false,
            proc_tree: false,
            proc_aggregate: false,
            proc_cpu_graphs: true,
            proc_follow_detailed: true,
            proc_per_core: false,
            proc_mem_bytes: true,
            mem_graphs: true,
            show_swap: true,
            base_10_sizes: false,
            disks_filter: String::new(),
            show_disks: true,
            show_io_stat: true,
            io_mode: false,
            io_graph_combined: false,
            io_graph_speeds: String::new(),
            net_iface: String::new(),
            net_download: 100,
            net_upload: 100,
            net_auto: true,
            net_sync: true,
            swap_upload_download: false,
            base_10_bitrate: true,
            cpu_util_mode: "time".to_owned(),
            freq_mode: "average".to_owned(),
            show_gpu_info: ShowGpuInfo::Auto,
            gpu_mirror_graph: true,
            graph_symbol_gpu: None,
            nvml_measure_pcie_speeds: true,
            custom_gpu_names: std::array::from_fn(|_| String::new()),
            preserved_unknown: BTreeMap::new(),
        }
    }
}

impl Config {
    /// Returns the non-empty user-configured custom name for `gpu{idx}` (`0..=5`), if set.
    pub fn custom_gpu_name(&self, idx: u8) -> Option<&str> {
        let name = self.custom_gpu_names.get(usize::from(idx))?.trim();
        if name.is_empty() { None } else { Some(name) }
    }

    /// Returns the effective graph symbol for GPU boxes (`graph_symbol_gpu` or fallback `graph_symbol`).
    pub fn effective_gpu_graph_symbol(&self) -> GraphSymbol {
        self.graph_symbol_gpu.unwrap_or(self.graph_symbol)
    }

    /// Renders a default `btop.conf` compatible with upstream `aristocratos/btop`.
    pub fn render_default_conf() -> String {
        Self::default().render_conf()
    }

    /// Serializes this [`Config`] to upstream-compatible `key = value` `btop.conf` format,
    /// preserving any unknown or deferred upstream keys in [`Self::preserved_unknown`].
    pub fn render_conf(&self) -> String {
        let mut out = format!(
            concat!(
                "#? Config file for btop-windows (English Windows 11 x64)\n\n",
                "#* Name of a .theme file or \"Default\".\n",
                "color_theme = \"{color_theme}\"\n\n",
                "#* Show theme background color; set False for terminal transparency.\n",
                "theme_background = {theme_background}\n\n",
                "#* Preserved upstream display flags (Windows 11 Terminal is TrueColor natively).\n",
                "truecolor = {truecolor}\n",
                "force_tty = {force_tty}\n\n",
                "#* Rounded corners on boxes.\n",
                "rounded_corners = {rounded_corners}\n\n",
                "#* Use DEC 2026 synchronized output sequences to prevent tearing.\n",
                "terminal_sync = {terminal_sync}\n\n",
                "#* Continue updating live telemetry in the background while overlay menus are open.\n",
                "background_update = {background_update}\n\n",
                "#* Save updated settings to btop.conf cleanly on exit.\n",
                "save_config_on_exit = {save_config_on_exit}\n\n",
                "#* Enable h/j/k/l/g/G vim navigation (moves Help to H and Terminate to K).\n",
                "vim_keys = {vim_keys}\n\n",
                "#* Disable mouse click and scroll wheel handling.\n",
                "disable_mouse = {disable_mouse}\n\n",
                "#* Default graph symbol: \"braille\" or \"block\".\n",
                "graph_symbol = \"{graph_symbol}\"\n\n",
                "#* Shown panels: \"cpu mem net proc\" and \"gpu0\" through \"gpu5\".\n",
                "shown_boxes = \"{shown_boxes}\"\n",
                "#* Layout presets 1-9 in upstream box:pos:symbol syntax (Preset 0 uses shown_boxes).\n",
                "presets = \"{presets}\"\n",
                "disable_presets = \"{disable_presets}\"\n",
                "cpu_bottom = {cpu_bottom}\n",
                "proc_left = {proc_left}\n",
                "mem_below_net = {mem_below_net}\n",
                "show_battery = {show_battery}\n\n",
                "#* Update interval in milliseconds (>= 100).\n",
                "update_ms = {update_ms}\n\n",
                "#* Clock format (strftime plus /host, /user, /uptime).\n",
                "clock_format = \"{clock_format}\"\n\n",
                "#* Process sort column.\n",
                "proc_sorting = \"{proc_sorting}\"\n",
                "proc_reversed = {proc_reversed}\n",
                "proc_tree = {proc_tree}\n",
                "proc_aggregate = {proc_aggregate}\n",
                "proc_cpu_graphs = {proc_cpu_graphs}\n",
                "proc_follow_detailed = {proc_follow_detailed}\n",
                "proc_per_core = {proc_per_core}\n",
                "proc_mem_bytes = {proc_mem_bytes}\n",
            ),
            color_theme = sanitize_conf_str(&self.color_theme),
            theme_background = bool_title(self.theme_background),
            truecolor = bool_title(self.truecolor),
            force_tty = bool_title(self.force_tty),
            rounded_corners = bool_title(self.rounded_corners),
            terminal_sync = bool_title(self.terminal_sync),
            background_update = bool_title(self.background_update),
            save_config_on_exit = bool_title(self.save_config_on_exit),
            vim_keys = bool_title(self.vim_keys),
            disable_mouse = bool_title(self.disable_mouse),
            graph_symbol = self.graph_symbol.as_str(),
            shown_boxes = sanitize_conf_str(&self.shown_boxes),
            presets = sanitize_conf_str(&self.presets),
            disable_presets = self.disable_presets.as_str(),
            cpu_bottom = bool_title(self.cpu_bottom),
            proc_left = bool_title(self.proc_left),
            mem_below_net = bool_title(self.mem_below_net),
            show_battery = bool_title(self.show_battery),
            update_ms = self.update_ms,
            clock_format = sanitize_conf_str(&self.clock_format),
            proc_sorting = sanitize_conf_str(&self.proc_sorting),
            proc_reversed = bool_title(self.proc_reversed),
            proc_tree = bool_title(self.proc_tree),
            proc_aggregate = bool_title(self.proc_aggregate),
            proc_cpu_graphs = bool_title(self.proc_cpu_graphs),
            proc_follow_detailed = bool_title(self.proc_follow_detailed),
            proc_per_core = bool_title(self.proc_per_core),
            proc_mem_bytes = bool_title(self.proc_mem_bytes),
        );
        out.push_str(&render_mem_net_cpu_defaults(self));
        out.push_str(&render_gpu_defaults(self));
        if !self.preserved_unknown.is_empty() {
            out.push_str("\n#* Preserved upstream / unknown keys.\n");
            for (key, val) in &self.preserved_unknown {
                let clean_key = sanitize_conf_key(key);
                if clean_key.is_empty() {
                    continue;
                }
                let clean_val = sanitize_conf_str(val);
                if is_bare_conf_literal(&clean_val) {
                    let _ = writeln!(out, "{clean_key} = {clean_val}");
                } else {
                    let _ = writeln!(out, "{clean_key} = \"{clean_val}\"");
                }
            }
        }
        out
    }

    /// Atomically writes `self.render_conf()` to `path`, creating parent directories if needed.
    pub fn save_to_path(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let content = self.render_conf();
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("btop.conf");
        let tmp_name = format!("{file_name}.tmp.{}", std::process::id());
        let tmp_path = path.with_file_name(tmp_name);
        std::fs::write(&tmp_path, content.as_bytes())?;
        if let Err(err) = std::fs::rename(&tmp_path, path) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(err);
        }
        Ok(())
    }

    /// Parses an upstream-style `key = value` config string while preserving unknown keys.
    pub fn parse(input: &str) -> Result<Self, ConfigError> {
        let mut cfg = Self::default();
        for (idx, raw_line) in input.lines().enumerate() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((raw_key, raw_val)) = line.split_once('=') else {
                return Err(ConfigError::Parse {
                    line: idx + 1,
                    message: "expected '='".to_owned(),
                });
            };
            let key = raw_key.trim();
            let val = unquote(raw_val.trim());
            apply_config_entry(&mut cfg, key, val)?;
        }
        Ok(cfg)
    }
}

fn render_mem_net_cpu_defaults(cfg: &Config) -> String {
    format!(
        concat!(
            "\n#* Show historical graphs inside the memory box (False = percentage meters).\n",
            "mem_graphs = {mem_graphs}\n",
            "show_swap = {show_swap}\n",
            "base_10_sizes = {base_10_sizes}\n\n",
            "#* Local fixed drive filter (e.g. \"C D\" or \"exclude=E F\").\n",
            "disks_filter = \"{disks_filter}\"\n",
            "show_disks = {show_disks}\n",
            "show_io_stat = {show_io_stat}\n",
            "io_mode = {io_mode}\n",
            "io_graph_combined = {io_graph_combined}\n",
            "io_graph_speeds = \"{io_graph_speeds}\"\n\n",
            "#* Network interface selection and scaling (Mbit/s ceilings when net_auto = False).\n",
            "net_iface = \"{net_iface}\"\n",
            "net_download = {net_download}\n",
            "net_upload = {net_upload}\n",
            "net_auto = {net_auto}\n",
            "net_sync = {net_sync}\n",
            "swap_upload_download = {swap_upload_download}\n",
            "base_10_bitrate = {base_10_bitrate}\n\n",
            "#* Windows CPU utilization mode: \"time\" or \"utility\" (Task Manager).\n",
            "cpu_util_mode = \"{cpu_util_mode}\"\n\n",
            "#* CPU frequency aggregation: \"first\", \"range\", \"lowest\", \"highest\", or \"average\".\n",
            "freq_mode = \"{freq_mode}\"\n",
        ),
        mem_graphs = bool_title(cfg.mem_graphs),
        show_swap = bool_title(cfg.show_swap),
        base_10_sizes = bool_title(cfg.base_10_sizes),
        disks_filter = sanitize_conf_str(&cfg.disks_filter),
        show_disks = bool_title(cfg.show_disks),
        show_io_stat = bool_title(cfg.show_io_stat),
        io_mode = bool_title(cfg.io_mode),
        io_graph_combined = bool_title(cfg.io_graph_combined),
        io_graph_speeds = sanitize_conf_str(&cfg.io_graph_speeds),
        net_iface = sanitize_conf_str(&cfg.net_iface),
        net_download = cfg.net_download,
        net_upload = cfg.net_upload,
        net_auto = bool_title(cfg.net_auto),
        net_sync = bool_title(cfg.net_sync),
        swap_upload_download = bool_title(cfg.swap_upload_download),
        base_10_bitrate = bool_title(cfg.base_10_bitrate),
        cpu_util_mode = sanitize_conf_str(&cfg.cpu_util_mode),
        freq_mode = sanitize_conf_str(&cfg.freq_mode),
    )
}

fn render_gpu_defaults(cfg: &Config) -> String {
    format!(
        concat!(
            "\n#* GPU display options: compact CPU-box info (\"Auto\", \"On\", \"Off\"), mirrored graph, and per-GPU custom names.\n",
            "show_gpu_info = \"{show_gpu_info}\"\n",
            "gpu_mirror_graph = {gpu_mirror_graph}\n",
            "graph_symbol_gpu = \"{graph_symbol_gpu}\"\n",
            "nvml_measure_pcie_speeds = {nvml_measure_pcie_speeds}\n",
            "custom_gpu_name0 = \"{gpu0}\"\n",
            "custom_gpu_name1 = \"{gpu1}\"\n",
            "custom_gpu_name2 = \"{gpu2}\"\n",
            "custom_gpu_name3 = \"{gpu3}\"\n",
            "custom_gpu_name4 = \"{gpu4}\"\n",
            "custom_gpu_name5 = \"{gpu5}\"\n",
        ),
        show_gpu_info = cfg.show_gpu_info.as_str(),
        gpu_mirror_graph = bool_title(cfg.gpu_mirror_graph),
        graph_symbol_gpu = cfg.graph_symbol_gpu.map_or("default", GraphSymbol::as_str),
        nvml_measure_pcie_speeds = bool_title(cfg.nvml_measure_pcie_speeds),
        gpu0 = sanitize_conf_str(&cfg.custom_gpu_names[0]),
        gpu1 = sanitize_conf_str(&cfg.custom_gpu_names[1]),
        gpu2 = sanitize_conf_str(&cfg.custom_gpu_names[2]),
        gpu3 = sanitize_conf_str(&cfg.custom_gpu_names[3]),
        gpu4 = sanitize_conf_str(&cfg.custom_gpu_names[4]),
        gpu5 = sanitize_conf_str(&cfg.custom_gpu_names[5]),
    )
}

fn apply_config_entry(cfg: &mut Config, key: &str, val: String) -> Result<(), ConfigError> {
    if apply_gpu_config_entry(cfg, key, &val)? {
        return Ok(());
    }
    match key {
        "color_theme" => cfg.color_theme = val,
        "theme_background" => cfg.theme_background = parse_bool(key, &val)?,
        "truecolor" => cfg.truecolor = parse_bool(key, &val)?,
        "force_tty" => cfg.force_tty = parse_bool(key, &val)?,
        "rounded_corners" => cfg.rounded_corners = parse_bool(key, &val)?,
        "terminal_sync" => cfg.terminal_sync = parse_bool(key, &val)?,
        "background_update" => cfg.background_update = parse_bool(key, &val)?,
        "save_config_on_exit" => cfg.save_config_on_exit = parse_bool(key, &val)?,
        "vim_keys" => cfg.vim_keys = parse_bool(key, &val)?,
        "disable_mouse" => cfg.disable_mouse = parse_bool(key, &val)?,
        "shown_boxes" => cfg.shown_boxes = val,
        "presets" => cfg.presets = val,
        "disable_presets" => cfg.disable_presets = DisablePresets::parse(key, &val)?,
        "cpu_bottom" => cfg.cpu_bottom = parse_bool(key, &val)?,
        "proc_left" => cfg.proc_left = parse_bool(key, &val)?,
        "mem_below_net" => cfg.mem_below_net = parse_bool(key, &val)?,
        "show_battery" => cfg.show_battery = parse_bool(key, &val)?,
        "clock_format" => cfg.clock_format = val,
        "proc_sorting" => cfg.proc_sorting = val,
        "proc_reversed" => cfg.proc_reversed = parse_bool(key, &val)?,
        "proc_tree" => cfg.proc_tree = parse_bool(key, &val)?,
        "proc_aggregate" => cfg.proc_aggregate = parse_bool(key, &val)?,
        "proc_cpu_graphs" => cfg.proc_cpu_graphs = parse_bool(key, &val)?,
        "proc_follow_detailed" => cfg.proc_follow_detailed = parse_bool(key, &val)?,
        "proc_per_core" => cfg.proc_per_core = parse_bool(key, &val)?,
        "proc_mem_bytes" => cfg.proc_mem_bytes = parse_bool(key, &val)?,
        "mem_graphs" => cfg.mem_graphs = parse_bool(key, &val)?,
        "show_swap" => cfg.show_swap = parse_bool(key, &val)?,
        "base_10_sizes" => cfg.base_10_sizes = parse_bool(key, &val)?,
        "disks_filter" => cfg.disks_filter = val,
        "show_disks" => cfg.show_disks = parse_bool(key, &val)?,
        "show_io_stat" => cfg.show_io_stat = parse_bool(key, &val)?,
        "io_mode" => cfg.io_mode = parse_bool(key, &val)?,
        "io_graph_combined" => cfg.io_graph_combined = parse_bool(key, &val)?,
        "io_graph_speeds" => cfg.io_graph_speeds = val,
        "net_iface" => cfg.net_iface = val,
        "net_download" => {
            cfg.net_download = val
                .parse::<u64>()
                .map_err(|_| ConfigError::InvalidValue {
                    key: key.to_owned(),
                    value: val.clone(),
                })?
                .max(1);
        }
        "net_upload" => {
            cfg.net_upload = val
                .parse::<u64>()
                .map_err(|_| ConfigError::InvalidValue {
                    key: key.to_owned(),
                    value: val.clone(),
                })?
                .max(1);
        }
        "net_auto" => cfg.net_auto = parse_bool(key, &val)?,
        "net_sync" => cfg.net_sync = parse_bool(key, &val)?,
        "swap_upload_download" => cfg.swap_upload_download = parse_bool(key, &val)?,
        "base_10_bitrate" => cfg.base_10_bitrate = parse_bool(key, &val)?,
        "cpu_util_mode" => cfg.cpu_util_mode = val,
        "freq_mode" => cfg.freq_mode = val,
        "graph_symbol" => cfg.graph_symbol = parse_graph_symbol(key, &val)?,
        "update_ms" => {
            let parsed: u32 = val.parse().map_err(|_| ConfigError::InvalidValue {
                key: key.to_owned(),
                value: val.clone(),
            })?;
            cfg.update_ms = parsed.max(100);
        }
        other => {
            cfg.preserved_unknown.insert(other.to_owned(), val);
        }
    }
    Ok(())
}

fn apply_gpu_config_entry(cfg: &mut Config, key: &str, val: &str) -> Result<bool, ConfigError> {
    match key {
        "show_gpu_info" => cfg.show_gpu_info = ShowGpuInfo::parse(key, val)?,
        "gpu_mirror_graph" => cfg.gpu_mirror_graph = parse_bool(key, val)?,
        "graph_symbol_gpu" => {
            cfg.graph_symbol_gpu = match val.trim().to_ascii_lowercase().as_str() {
                "" | "default" => None,
                _ => Some(parse_graph_symbol(key, val)?),
            };
        }
        "nvml_measure_pcie_speeds" => cfg.nvml_measure_pcie_speeds = parse_bool(key, val)?,
        "custom_gpu_name0" => val.clone_into(&mut cfg.custom_gpu_names[0]),
        "custom_gpu_name1" => val.clone_into(&mut cfg.custom_gpu_names[1]),
        "custom_gpu_name2" => val.clone_into(&mut cfg.custom_gpu_names[2]),
        "custom_gpu_name3" => val.clone_into(&mut cfg.custom_gpu_names[3]),
        "custom_gpu_name4" => val.clone_into(&mut cfg.custom_gpu_names[4]),
        "custom_gpu_name5" => val.clone_into(&mut cfg.custom_gpu_names[5]),
        _ => return Ok(false),
    }
    Ok(true)
}

fn parse_graph_symbol(key: &str, val: &str) -> Result<GraphSymbol, ConfigError> {
    match val.trim().to_ascii_lowercase().as_str() {
        "braille" => Ok(GraphSymbol::Braille),
        "block" => Ok(GraphSymbol::Block),
        "tty" => Ok(GraphSymbol::Tty),
        _ => Err(ConfigError::InvalidValue {
            key: key.to_owned(),
            value: val.to_owned(),
        }),
    }
}

fn bool_title(v: bool) -> &'static str {
    if v { "True" } else { "False" }
}

fn sanitize_conf_str(s: &str) -> String {
    s.chars().filter(|&ch| !matches!(ch, '\r' | '\n')).collect()
}

fn sanitize_conf_key(s: &str) -> String {
    s.trim()
        .chars()
        .filter(|&ch| !matches!(ch, '\r' | '\n' | '=' | '#'))
        .collect()
}

fn is_bare_conf_literal(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    if matches!(s, "True" | "False" | "true" | "false") {
        return true;
    }
    let rest = s.strip_prefix('-').unwrap_or(s);
    !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
}

fn unquote(s: &str) -> String {
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        s[1..s.len() - 1].to_owned()
    } else {
        s.to_owned()
    }
}

fn parse_bool(key: &str, val: &str) -> Result<bool, ConfigError> {
    match val.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Ok(true),
        "false" | "0" | "no" => Ok(false),
        _ => Err(ConfigError::InvalidValue {
            key: key.to_owned(),
            value: val.to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_conf_roundtrips_and_preserves_unix_keys() {
        let mut text = Config::render_default_conf();
        text.push_str("\nuse_fstab = True\nzfs_arc_cached = True\n");
        let parsed = Config::parse(&text).expect("parse default conf");
        assert_eq!(parsed.update_ms, 2000);
        assert_eq!(parsed.graph_symbol, GraphSymbol::Braille);
        assert!(parsed.mem_graphs);
        assert!(parsed.show_swap);
        assert!(!parsed.base_10_sizes);
        assert_eq!(
            parsed
                .preserved_unknown
                .get("use_fstab")
                .map(String::as_str),
            Some("True")
        );

        let custom = Config::parse(
            "background_update = False\nmem_graphs = False\nshow_swap = False\nbase_10_sizes = True\ndisks_filter = \"exclude=E\"\nshow_disks = False\nshow_io_stat = False\nio_mode = True\nio_graph_combined = True\nio_graph_speeds = \"C:500\"\nnet_iface = \"Ethernet\"\nnet_download = 250\nnet_upload = 75\nnet_auto = False\nnet_sync = False\nswap_upload_download = True\nbase_10_bitrate = False\npresets = \"cpu:1:block,proc:1:default\"\ndisable_presets = True\ncpu_bottom = True\nproc_left = True\nmem_below_net = True\nshow_battery = False\nproc_aggregate = True\nproc_cpu_graphs = False\nproc_follow_detailed = False\n",
        )
        .expect("parse custom mem, disk, net, layout, and proc flags");
        assert!(!custom.background_update);
        assert!(!custom.mem_graphs);
        assert!(!custom.show_swap);
        assert!(custom.base_10_sizes);
        assert_eq!(custom.disks_filter, "exclude=E");
        assert!(!custom.show_disks);
        assert!(!custom.show_io_stat);
        assert!(custom.io_mode);
        assert!(custom.io_graph_combined);
        assert_eq!(custom.io_graph_speeds, "C:500");
        assert_eq!(custom.net_iface, "Ethernet");
        assert_eq!(custom.net_download, 250);
        assert_eq!(custom.net_upload, 75);
        assert!(!custom.net_auto);
        assert!(!custom.net_sync);
        assert!(custom.swap_upload_download);
        assert!(!custom.base_10_bitrate);
        assert_eq!(custom.presets, "cpu:1:block,proc:1:default");
        assert_eq!(custom.disable_presets, DisablePresets::All);
        assert!(custom.cpu_bottom);
        assert!(custom.proc_left);
        assert!(custom.mem_below_net);
        assert!(!custom.show_battery);
        assert!(custom.proc_aggregate);
        assert!(!custom.proc_cpu_graphs);
        assert!(!custom.proc_follow_detailed);

        for (raw, expected) in [
            ("Off", DisablePresets::Off),
            ("Default", DisablePresets::Default),
            ("Custom", DisablePresets::Custom),
            ("All", DisablePresets::All),
            ("False", DisablePresets::Off),
            ("True", DisablePresets::All),
        ] {
            let line = format!("disable_presets = \"{raw}\"\n");
            let parsed_mode = Config::parse(&line).expect("parse upstream disable_presets");
            assert_eq!(parsed_mode.disable_presets, expected);
        }
    }

    #[test]
    fn clamps_update_ms_floor() {
        let parsed = Config::parse("update_ms = 25\n").expect("parse");
        assert_eq!(parsed.update_ms, 100);
    }

    #[test]
    fn parses_gpu_config_keys_and_custom_names() {
        let parsed = Config::parse(
            "show_gpu_info = \"On\"\ngpu_mirror_graph = False\ngraph_symbol_gpu = \"block\"\nnvml_measure_pcie_speeds = False\ncustom_gpu_name0 = \"Primary 5090\"\ncustom_gpu_name1 = \"Radeon iGPU\"\n",
        )
        .expect("parse GPU config keys");
        assert_eq!(parsed.show_gpu_info, ShowGpuInfo::On);
        assert!(!parsed.gpu_mirror_graph);
        assert_eq!(parsed.graph_symbol_gpu, Some(GraphSymbol::Block));
        assert_eq!(parsed.effective_gpu_graph_symbol(), GraphSymbol::Block);
        assert!(!parsed.nvml_measure_pcie_speeds);
        assert_eq!(parsed.custom_gpu_name(0), Some("Primary 5090"));
        assert_eq!(parsed.custom_gpu_name(1), Some("Radeon iGPU"));
        assert_eq!(parsed.custom_gpu_name(2), None);
    }

    #[test]
    fn roundtrip_save_preserves_unknown_unix_keys_and_mutated_settings() {
        let initial = concat!(
            "color_theme = \"Default\"\n",
            "truecolor = False\n",
            "force_tty = True\n",
            "update_ms = 2000\n",
            "vim_keys = False\n",
            "save_config_on_exit = True\n",
            "use_fstab = True\n",
            "zfs_arc_cached = True\n",
            "proc_info_smaps = False\n",
            "custom_unix_sensor = \"coretemp/Package \"0\"\"\n",
        );
        let mut cfg = Config::parse(initial).expect("parse initial conf");
        assert!(!cfg.truecolor);
        assert!(cfg.force_tty);
        assert_eq!(cfg.preserved_unknown.len(), 4);

        // Mutate settings as a user would in Options / hotkeys.
        cfg.color_theme = "tokyo-night".to_owned();
        cfg.theme_background = false;
        cfg.update_ms = 1200;
        cfg.vim_keys = true;
        cfg.disable_mouse = true;
        cfg.proc_sorting = "memory".to_owned();
        cfg.proc_reversed = true;
        cfg.proc_tree = true;
        cfg.proc_per_core = true;
        cfg.proc_mem_bytes = false;
        cfg.custom_gpu_names[0] = "RTX \"5090\"".to_owned();

        let unique = format!(
            "btop-windows-roundtrip-{}-{}.conf",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        );
        let path = std::env::temp_dir().join("btop-roundtrip-dir").join(unique);
        cfg.save_to_path(&path).expect("save btop.conf to disk");

        let saved_text = std::fs::read_to_string(&path).expect("read saved conf");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(path.parent().expect("parent dir"));

        let reloaded = Config::parse(&saved_text).expect("re-parse saved conf");
        assert_eq!(reloaded.color_theme, "tokyo-night");
        assert!(!reloaded.theme_background);
        assert!(!reloaded.truecolor);
        assert!(reloaded.force_tty);
        assert_eq!(reloaded.update_ms, 1200);
        assert!(reloaded.vim_keys);
        assert!(reloaded.disable_mouse);
        assert!(reloaded.save_config_on_exit);
        assert_eq!(reloaded.proc_sorting, "memory");
        assert!(reloaded.proc_reversed);
        assert!(reloaded.proc_tree);
        assert!(reloaded.proc_per_core);
        assert!(!reloaded.proc_mem_bytes);
        assert_eq!(reloaded.custom_gpu_name(0), Some("RTX \"5090\""));

        // All unknown Unix keys must survive cleanly verbatim.
        assert_eq!(
            reloaded
                .preserved_unknown
                .get("use_fstab")
                .map(String::as_str),
            Some("True")
        );
        assert_eq!(
            reloaded
                .preserved_unknown
                .get("zfs_arc_cached")
                .map(String::as_str),
            Some("True")
        );
        assert_eq!(
            reloaded
                .preserved_unknown
                .get("proc_info_smaps")
                .map(String::as_str),
            Some("False")
        );
        assert_eq!(
            reloaded
                .preserved_unknown
                .get("custom_unix_sensor")
                .map(String::as_str),
            Some("coretemp/Package \"0\"")
        );
    }

    #[test]
    fn resolves_explicit_portable_and_appdata_config_locations() {
        let root = std::env::temp_dir().join(format!(
            "btop-cfg-loc-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let exe_dir = root.join("bin");
        let appdata = root.join("appdata");
        std::fs::create_dir_all(&exe_dir).expect("mkdir exe_dir");
        std::fs::create_dir_all(&appdata).expect("mkdir appdata");

        // 1. Without exe-adjacent btop.conf, falls back to %APPDATA%\btop\btop.conf.
        assert_eq!(
            resolve_config_location_with(None, Some(&exe_dir), Some(&appdata)),
            Some(ConfigLocation::AppData(
                appdata.join("btop").join("btop.conf")
            ))
        );

        // 2. When exe-adjacent btop.conf exists, portable mode takes precedence over %APPDATA%.
        let portable_conf = exe_dir.join("btop.conf");
        std::fs::write(&portable_conf, "update_ms = 1000\n").expect("write portable conf");
        assert_eq!(
            resolve_config_location_with(None, Some(&exe_dir), Some(&appdata)),
            Some(ConfigLocation::Portable(portable_conf.clone()))
        );

        // 3. Explicit -c path always overrides both portable and %APPDATA%.
        let explicit = root.join("custom.conf");
        assert_eq!(
            resolve_config_location_with(Some(&explicit), Some(&exe_dir), Some(&appdata)),
            Some(ConfigLocation::Explicit(explicit))
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resolves_unix_xdg_and_home_config_locations_without_btop_collision() {
        let root = std::env::temp_dir().join(format!(
            "btop-unix-cfg-loc-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let exe_dir = root.join("bin");
        let xdg = root.join("xdg");
        let home = root.join("home");
        std::fs::create_dir_all(&exe_dir).expect("mkdir exe_dir");

        assert_eq!(
            resolve_unix_config_location_with(None, Some(&exe_dir), Some(&xdg), Some(&home)),
            Some(ConfigLocation::AppData(
                xdg.join("btop-fleet").join("btop.conf")
            ))
        );
        assert_eq!(
            resolve_unix_config_location_with(None, Some(&exe_dir), None, Some(&home)),
            Some(ConfigLocation::AppData(
                home.join(".config").join("btop-fleet").join("btop.conf")
            ))
        );
        // Per the XDG Base Directory Specification, relative XDG_CONFIG_HOME values are ignored.
        assert_eq!(
            resolve_unix_config_location_with(
                None,
                Some(&exe_dir),
                Some(Path::new("relative/xdg")),
                Some(&home)
            ),
            Some(ConfigLocation::AppData(
                home.join(".config").join("btop-fleet").join("btop.conf")
            ))
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
