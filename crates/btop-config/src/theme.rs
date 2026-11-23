//! Upstream `aristocratos/btop` v1.4.8 `TrueColor` `.theme` parser (all 48 color keys),
//! 101-step (0..=100) 2-stop and 3-stop RGB gradient engine, curated Apache-2.0 theme bundle,
//! and Windows 11 theme search-path discovery.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use thiserror::Error;

const MAX_THEME_FILE_BYTES: u64 = 256 * 1024;

/// All 48 upstream `aristocratos/btop` v1.4.8 `.theme` color keys.
pub const ALL_THEME_KEYS: [&str; 48] = [
    "main_bg",
    "main_fg",
    "title",
    "hi_fg",
    "selected_bg",
    "selected_fg",
    "inactive_fg",
    "graph_text",
    "meter_bg",
    "proc_misc",
    "cpu_box",
    "mem_box",
    "net_box",
    "proc_box",
    "div_line",
    "followed_bg",
    "followed_fg",
    "proc_pause_bg",
    "proc_follow_bg",
    "proc_banner_bg",
    "proc_banner_fg",
    "temp_start",
    "temp_mid",
    "temp_end",
    "cpu_start",
    "cpu_mid",
    "cpu_end",
    "free_start",
    "free_mid",
    "free_end",
    "cached_start",
    "cached_mid",
    "cached_end",
    "available_start",
    "available_mid",
    "available_end",
    "used_start",
    "used_mid",
    "used_end",
    "download_start",
    "download_mid",
    "download_end",
    "upload_start",
    "upload_mid",
    "upload_end",
    "process_start",
    "process_mid",
    "process_end",
];

/// Curated upstream Apache-2.0 `.theme` files embedded in the binary and shipped under `themes/`.
pub const BUNDLED_THEMES: [(&str, &str); 10] = [
    ("dracula", include_str!("../../../themes/dracula.theme")),
    (
        "tokyo-night",
        include_str!("../../../themes/tokyo-night.theme"),
    ),
    (
        "gruvbox_dark_v2",
        include_str!("../../../themes/gruvbox_dark_v2.theme"),
    ),
    ("nord", include_str!("../../../themes/nord.theme")),
    ("onedark", include_str!("../../../themes/onedark.theme")),
    (
        "kanagawa-wave",
        include_str!("../../../themes/kanagawa-wave.theme"),
    ),
    (
        "solarized_dark",
        include_str!("../../../themes/solarized_dark.theme"),
    ),
    ("monokai", include_str!("../../../themes/monokai.theme")),
    (
        "everforest-dark-medium",
        include_str!("../../../themes/everforest-dark-medium.theme"),
    ),
    (
        "adwaita-dark",
        include_str!("../../../themes/adwaita-dark.theme"),
    ),
];

/// Errors returned while loading or parsing a `.theme` file.
#[derive(Debug, Error)]
pub enum ThemeError {
    #[error("theme parse error at line {line}: {message}")]
    Parse { line: usize, message: String },
    #[error("invalid color for theme key '{key}': '{value}'")]
    InvalidColor { key: String, value: String },
    #[error("theme '{0}' not found")]
    NotFound(String),
    #[error("theme I/O error: {0}")]
    Io(String),
}

/// 24-bit `TrueColor` RGB triplet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ThemeRgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl ThemeRgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// Parsed theme color value (`Transparent` when empty `""`, otherwise 24-bit RGB).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeColor {
    #[default]
    Transparent,
    Rgb(ThemeRgb),
}

impl ThemeColor {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self::Rgb(ThemeRgb { r, g, b })
    }
}

/// Precomputed 101-step (`0..=100`) RGB gradient table supporting both 2-stop (`mid = None`)
/// and 3-stop (`mid = Some(...)`) interpolation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeGradient101 {
    pub start: ThemeRgb,
    pub mid: Option<ThemeRgb>,
    pub end: ThemeRgb,
    steps: [ThemeRgb; 101],
}

impl ThemeGradient101 {
    /// Builds a 101-step (`0..=100`) RGB gradient table.
    ///
    /// - When `mid` is `None`: linear 2-stop interpolation from `start` (`0`) to `end` (`100`).
    /// - When `mid` is `Some(m)`: 3-stop interpolation from `start` (`0`) to `m` (`50`) to `end` (`100`).
    pub fn build(start: ThemeRgb, mid: Option<ThemeRgb>, end: ThemeRgb) -> Self {
        let mut steps = [start; 101];
        for (i, slot) in steps.iter_mut().enumerate() {
            *slot = match mid {
                None => lerp_rgb(start, end, i, 100),
                Some(m) if i <= 50 => lerp_rgb(start, m, i, 50),
                Some(m) => lerp_rgb(m, end, i - 50, 50),
            };
        }
        Self {
            start,
            mid,
            end,
            steps,
        }
    }

    /// Returns the exact RGB entry for discrete step `0..=100` (clamped at 100).
    pub const fn at(&self, step: usize) -> ThemeRgb {
        let idx = if step > 100 { 100 } else { step };
        self.steps[idx]
    }

    /// Returns the full 101-element precomputed RGB slice.
    pub const fn steps(&self) -> &[ThemeRgb; 101] {
        &self.steps
    }

    /// Samples the 101-step gradient for a percentage in `0.0..=100.0`.
    pub fn sample_percent(&self, percent: f32) -> ThemeRgb {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let idx = percent.clamp(0.0, 100.0).round() as usize;
        self.at(idx)
    }
}

fn lerp_rgb(a: ThemeRgb, b: ThemeRgb, num: usize, den: usize) -> ThemeRgb {
    let interp = |c0: u8, c1: u8| -> u8 {
        let f0 = f64::from(c0);
        let f1 = f64::from(c1);
        #[allow(clippy::cast_precision_loss)]
        let t = (num as f64) / (den as f64);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let v = (f0 + (f1 - f0) * t).round().clamp(0.0, 255.0) as u8;
        v
    };
    ThemeRgb {
        r: interp(a.r, b.r),
        g: interp(a.g, b.g),
        b: interp(a.b, b.b),
    }
}

/// Complete parsed `TrueColor` theme containing all 48 upstream attributes and all 9 precomputed
/// 101-step RGB gradients.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeSpec {
    pub main_bg: ThemeColor,
    pub main_fg: ThemeRgb,
    pub title: ThemeRgb,
    pub hi_fg: ThemeRgb,
    pub selected_bg: ThemeRgb,
    pub selected_fg: ThemeRgb,
    pub inactive_fg: ThemeRgb,
    pub graph_text: ThemeRgb,
    pub meter_bg: ThemeRgb,
    pub proc_misc: ThemeRgb,
    pub cpu_box: ThemeRgb,
    pub mem_box: ThemeRgb,
    pub net_box: ThemeRgb,
    pub proc_box: ThemeRgb,
    pub div_line: ThemeRgb,
    pub followed_bg: ThemeRgb,
    pub followed_fg: ThemeRgb,
    pub proc_pause_bg: ThemeRgb,
    pub proc_follow_bg: ThemeRgb,
    pub proc_banner_bg: ThemeRgb,
    pub proc_banner_fg: ThemeRgb,
    pub temp: ThemeGradient101,
    pub cpu: ThemeGradient101,
    pub free: ThemeGradient101,
    pub cached: ThemeGradient101,
    pub available: ThemeGradient101,
    pub used: ThemeGradient101,
    pub download: ThemeGradient101,
    pub upload: ThemeGradient101,
    pub process: ThemeGradient101,
}

impl Default for ThemeSpec {
    fn default() -> Self {
        Self::default_dark()
    }
}

impl ThemeSpec {
    /// Built-in `Default` dark `TrueColor` palette matching `btop-windows`'s default look.
    pub fn default_dark() -> Self {
        let rgb = ThemeRgb::new;
        Self {
            main_bg: ThemeColor::rgb(16, 18, 24),
            main_fg: rgb(204, 208, 218),
            title: rgb(238, 240, 246),
            hi_fg: rgb(229, 181, 103),
            selected_bg: rgb(44, 62, 92),
            selected_fg: rgb(248, 250, 255),
            inactive_fg: rgb(102, 110, 126),
            graph_text: rgb(160, 168, 184),
            meter_bg: rgb(48, 54, 68),
            proc_misc: rgb(118, 182, 214),
            cpu_box: rgb(98, 150, 112),
            mem_box: rgb(170, 146, 94),
            net_box: rgb(142, 116, 178),
            proc_box: rgb(104, 138, 192),
            div_line: rgb(60, 68, 82),
            followed_bg: rgb(34, 82, 98),
            followed_fg: rgb(248, 250, 255),
            proc_pause_bg: rgb(124, 44, 52),
            proc_follow_bg: rgb(30, 88, 112),
            proc_banner_bg: rgb(102, 56, 118),
            proc_banner_fg: rgb(248, 250, 255),
            temp: ThemeGradient101::build(
                rgb(82, 152, 204),
                Some(rgb(235, 203, 102)),
                rgb(230, 85, 85),
            ),
            cpu: ThemeGradient101::build(
                rgb(86, 196, 122),
                Some(rgb(235, 203, 102)),
                rgb(230, 85, 85),
            ),
            free: ThemeGradient101::build(
                rgb(72, 162, 120),
                Some(rgb(94, 198, 142)),
                rgb(132, 232, 172),
            ),
            cached: ThemeGradient101::build(
                rgb(82, 152, 204),
                Some(rgb(98, 186, 224)),
                rgb(134, 222, 242),
            ),
            available: ThemeGradient101::build(
                rgb(214, 176, 84),
                Some(rgb(196, 212, 98)),
                rgb(132, 222, 126),
            ),
            used: ThemeGradient101::build(
                rgb(196, 96, 96),
                Some(rgb(224, 112, 92)),
                rgb(245, 76, 88),
            ),
            download: ThemeGradient101::build(
                rgb(72, 152, 212),
                Some(rgb(96, 196, 235)),
                rgb(148, 232, 248),
            ),
            upload: ThemeGradient101::build(
                rgb(168, 102, 204),
                Some(rgb(212, 118, 192)),
                rgb(242, 142, 168),
            ),
            process: ThemeGradient101::build(
                rgb(86, 196, 122),
                Some(rgb(235, 203, 102)),
                rgb(230, 85, 85),
            ),
        }
    }
}

/// Parses an upstream `aristocratos/btop` `.theme` document (`theme[key]="#RRGGBB"`).
///
/// Supports `#RRGGBB` 24-bit hex, `#XX` greyscale, `r g b` decimal triplets, empty `""`
/// transparent background / omitted mid-stop (2-stop gradient), and clean defaults for
/// omitted v1.4 pause/follow banner keys.
pub fn parse_theme(input: &str) -> Result<ThemeSpec, ThemeError> {
    let mut spec = ThemeSpec::default_dark();
    let mut raw_grads = RawGradients::from_spec(&spec);

    for (idx, raw_line) in input.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((lhs, rhs)) = line.split_once('=') else {
            return Err(ThemeError::Parse {
                line: idx + 1,
                message: "expected '='".to_owned(),
            });
        };
        let key = parse_theme_key(lhs.trim()).ok_or_else(|| ThemeError::Parse {
            line: idx + 1,
            message: format!("expected theme[key], found '{}'", lhs.trim()),
        })?;
        let val = unquote_theme_val(rhs.trim());
        let parsed = parse_color_value(key, &val)?;
        apply_theme_key(&mut spec, &mut raw_grads, key, parsed);
    }

    raw_grads.finalize(&mut spec);
    Ok(spec)
}

#[derive(Debug, Clone, Copy)]
struct RawGradientStops {
    start: ThemeRgb,
    mid: Option<ThemeRgb>,
    end: ThemeRgb,
    start_explicit: bool,
    end_explicit: bool,
}

impl RawGradientStops {
    const fn from_grad(g: &ThemeGradient101) -> Self {
        Self {
            start: g.start,
            mid: g.mid,
            end: g.end,
            start_explicit: false,
            end_explicit: false,
        }
    }

    fn set_start(&mut self, color: ThemeColor) {
        if let ThemeColor::Rgb(rgb) = color {
            self.start = rgb;
            self.start_explicit = true;
            if !self.end_explicit {
                self.end = rgb;
            }
        }
    }

    fn set_mid(&mut self, color: ThemeColor) {
        self.mid = match color {
            ThemeColor::Rgb(rgb) => Some(rgb),
            ThemeColor::Transparent => None,
        };
    }

    fn set_end(&mut self, color: ThemeColor) {
        match color {
            ThemeColor::Rgb(rgb) => {
                self.end = rgb;
                self.end_explicit = true;
            }
            ThemeColor::Transparent => {
                if self.start_explicit {
                    self.end = self.start;
                }
            }
        }
    }

    fn build(&self) -> ThemeGradient101 {
        ThemeGradient101::build(self.start, self.mid, self.end)
    }
}

struct RawGradients {
    temp: RawGradientStops,
    cpu: RawGradientStops,
    free: RawGradientStops,
    cached: RawGradientStops,
    available: RawGradientStops,
    used: RawGradientStops,
    download: RawGradientStops,
    upload: RawGradientStops,
    process: RawGradientStops,
}

impl RawGradients {
    const fn from_spec(spec: &ThemeSpec) -> Self {
        Self {
            temp: RawGradientStops::from_grad(&spec.temp),
            cpu: RawGradientStops::from_grad(&spec.cpu),
            free: RawGradientStops::from_grad(&spec.free),
            cached: RawGradientStops::from_grad(&spec.cached),
            available: RawGradientStops::from_grad(&spec.available),
            used: RawGradientStops::from_grad(&spec.used),
            download: RawGradientStops::from_grad(&spec.download),
            upload: RawGradientStops::from_grad(&spec.upload),
            process: RawGradientStops::from_grad(&spec.process),
        }
    }

    fn finalize(self, spec: &mut ThemeSpec) {
        spec.temp = self.temp.build();
        spec.cpu = self.cpu.build();
        spec.free = self.free.build();
        spec.cached = self.cached.build();
        spec.available = self.available.build();
        spec.used = self.used.build();
        spec.download = self.download.build();
        spec.upload = self.upload.build();
        spec.process = self.process.build();
    }
}

fn parse_theme_key(lhs: &str) -> Option<&str> {
    let rest = lhs.strip_prefix("theme[")?;
    let key = rest.strip_suffix(']')?.trim();
    if key.is_empty() { None } else { Some(key) }
}

fn unquote_theme_val(raw: &str) -> String {
    // Strip inline comments outside quotes.
    let trimmed = raw.trim();
    if let Some(after_open) = trimmed.strip_prefix('"')
        && let Some(end_quote) = after_open.find('"')
    {
        return after_open[..end_quote].trim().to_owned();
    }
    if let Some(after_open) = trimmed.strip_prefix('\'')
        && let Some(end_quote) = after_open.find('\'')
    {
        return after_open[..end_quote].trim().to_owned();
    }
    let before_comment = trimmed.split('#').next().unwrap_or(trimmed);
    if trimmed.starts_with('#') {
        trimmed
            .split_whitespace()
            .next()
            .unwrap_or(trimmed)
            .to_owned()
    } else {
        before_comment.trim().to_owned()
    }
}

/// Parses `#RRGGBB`, `#XX` (greyscale), `r g b` (decimal 0..=255), or `""` (transparent).
pub fn parse_color_value(key: &str, value: &str) -> Result<ThemeColor, ThemeError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(ThemeColor::Transparent);
    }
    if let Some(hex) = trimmed.strip_prefix('#') {
        if !hex.is_ascii() {
            return Err(ThemeError::InvalidColor {
                key: key.to_owned(),
                value: trimmed.to_owned(),
            });
        }
        return match hex.len() {
            2 => {
                let g = u8::from_str_radix(hex, 16).map_err(|_| ThemeError::InvalidColor {
                    key: key.to_owned(),
                    value: trimmed.to_owned(),
                })?;
                Ok(ThemeColor::rgb(g, g, g))
            }
            6 => {
                let r = u8::from_str_radix(&hex[0..2], 16);
                let g = u8::from_str_radix(&hex[2..4], 16);
                let b = u8::from_str_radix(&hex[4..6], 16);
                match (r, g, b) {
                    (Ok(r), Ok(g), Ok(b)) => Ok(ThemeColor::rgb(r, g, b)),
                    _ => Err(ThemeError::InvalidColor {
                        key: key.to_owned(),
                        value: trimmed.to_owned(),
                    }),
                }
            }
            _ => Err(ThemeError::InvalidColor {
                key: key.to_owned(),
                value: trimmed.to_owned(),
            }),
        };
    }

    // Upstream btop also permits space-separated decimal RGB triplets ("255 128 0").
    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    if parts.len() == 3
        && let (Ok(r), Ok(g), Ok(b)) = (
            parts[0].parse::<u8>(),
            parts[1].parse::<u8>(),
            parts[2].parse::<u8>(),
        )
    {
        return Ok(ThemeColor::rgb(r, g, b));
    }

    Err(ThemeError::InvalidColor {
        key: key.to_owned(),
        value: trimmed.to_owned(),
    })
}

fn apply_theme_key(spec: &mut ThemeSpec, grads: &mut RawGradients, key: &str, color: ThemeColor) {
    let set_rgb = |dst: &mut ThemeRgb, c: ThemeColor| {
        if let ThemeColor::Rgb(rgb) = c {
            *dst = rgb;
        }
    };
    match key.to_ascii_lowercase().as_str() {
        "main_bg" => spec.main_bg = color,
        "main_fg" => set_rgb(&mut spec.main_fg, color),
        "title" => set_rgb(&mut spec.title, color),
        "hi_fg" => set_rgb(&mut spec.hi_fg, color),
        "selected_bg" => set_rgb(&mut spec.selected_bg, color),
        "selected_fg" => set_rgb(&mut spec.selected_fg, color),
        "inactive_fg" => set_rgb(&mut spec.inactive_fg, color),
        "graph_text" => set_rgb(&mut spec.graph_text, color),
        "meter_bg" => set_rgb(&mut spec.meter_bg, color),
        "proc_misc" => set_rgb(&mut spec.proc_misc, color),
        "cpu_box" => set_rgb(&mut spec.cpu_box, color),
        "mem_box" => set_rgb(&mut spec.mem_box, color),
        "net_box" => set_rgb(&mut spec.net_box, color),
        "proc_box" => set_rgb(&mut spec.proc_box, color),
        "div_line" => set_rgb(&mut spec.div_line, color),
        "followed_bg" => set_rgb(&mut spec.followed_bg, color),
        "followed_fg" => set_rgb(&mut spec.followed_fg, color),
        "proc_pause_bg" => set_rgb(&mut spec.proc_pause_bg, color),
        "proc_follow_bg" => set_rgb(&mut spec.proc_follow_bg, color),
        "proc_banner_bg" => set_rgb(&mut spec.proc_banner_bg, color),
        "proc_banner_fg" => set_rgb(&mut spec.proc_banner_fg, color),
        "temp_start" => grads.temp.set_start(color),
        "temp_mid" => grads.temp.set_mid(color),
        "temp_end" => grads.temp.set_end(color),
        "cpu_start" => grads.cpu.set_start(color),
        "cpu_mid" => grads.cpu.set_mid(color),
        "cpu_end" => grads.cpu.set_end(color),
        "free_start" => grads.free.set_start(color),
        "free_mid" => grads.free.set_mid(color),
        "free_end" => grads.free.set_end(color),
        "cached_start" => grads.cached.set_start(color),
        "cached_mid" => grads.cached.set_mid(color),
        "cached_end" => grads.cached.set_end(color),
        "available_start" => grads.available.set_start(color),
        "available_mid" => grads.available.set_mid(color),
        "available_end" => grads.available.set_end(color),
        "used_start" => grads.used.set_start(color),
        "used_mid" => grads.used.set_mid(color),
        "used_end" => grads.used.set_end(color),
        "download_start" => grads.download.set_start(color),
        "download_mid" => grads.download.set_mid(color),
        "download_end" => grads.download.set_end(color),
        "upload_start" => grads.upload.set_start(color),
        "upload_mid" => grads.upload.set_mid(color),
        "upload_end" => grads.upload.set_end(color),
        "process_start" => grads.process.set_start(color),
        "process_mid" => grads.process.set_mid(color),
        "process_end" => grads.process.set_end(color),
        _ => {}
    }
}

/// Ordered theme search directories: `--themes-dir`, exe-adjacent `themes\`, and
/// `%APPDATA%\btop-windows\themes`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ThemeSearchPaths {
    pub cli_dir: Option<PathBuf>,
    pub exe_dir: Option<PathBuf>,
    pub appdata_dir: Option<PathBuf>,
}

impl ThemeSearchPaths {
    /// Builds standard Windows 11 search paths with optional `--themes-dir` override.
    pub fn resolve(cli_themes_dir: Option<&Path>) -> Self {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|dir| dir.join("themes")));
        let appdata_dir = std::env::var_os("APPDATA")
            .filter(|v| !v.is_empty())
            .map(|dir| PathBuf::from(dir).join("btop-windows").join("themes"));
        Self {
            cli_dir: cli_themes_dir.map(Path::to_path_buf),
            exe_dir,
            appdata_dir,
        }
    }

    /// Returns the ordered list of search directories (`--themes-dir`, exe `themes\`, `%APPDATA%`).
    pub fn ordered_dirs(&self) -> Vec<&Path> {
        let mut dirs = Vec::with_capacity(3);
        if let Some(ref d) = self.cli_dir {
            dirs.push(d.as_path());
        }
        if let Some(ref d) = self.exe_dir {
            dirs.push(d.as_path());
        }
        if let Some(ref d) = self.appdata_dir {
            dirs.push(d.as_path());
        }
        dirs
    }

    /// Discovers all selectable theme names: `"Default"`, all bundled curated themes, and any
    /// `*.theme` files found across the search directories.
    pub fn discover_themes(&self) -> Vec<String> {
        let mut names = vec!["Default".to_owned()];
        let mut seen = BTreeSet::new();
        seen.insert("default".to_owned());

        for dir in self.ordered_dirs() {
            if let Ok(entries) = std::fs::read_dir(dir) {
                let mut dir_stems: Vec<String> = entries
                    .flatten()
                    .filter_map(|entry| {
                        let path = entry.path();
                        let ext = path.extension()?.to_str()?;
                        if ext.eq_ignore_ascii_case("theme") {
                            path.file_stem()?.to_str().map(ToOwned::to_owned)
                        } else {
                            None
                        }
                    })
                    .collect();
                dir_stems.sort_by_key(|s| s.to_ascii_lowercase());
                for stem in dir_stems {
                    if seen.insert(stem.to_ascii_lowercase()) {
                        names.push(stem);
                    }
                }
            }
        }

        for (bundled, _) in BUNDLED_THEMES {
            if seen.insert(bundled.to_ascii_lowercase()) {
                names.push(bundled.to_owned());
            }
        }
        names
    }

    /// Loads a theme by name or explicit `.theme` path using strict search-order precedence:
    /// 1. `"Default"` -> built-in default palette
    /// 2. Explicit `.theme` file path on disk (when it contains a path separator or ends in `.theme`)
    /// 3. `--themes-dir`
    /// 4. Exe-adjacent `themes\`
    /// 5. `%APPDATA%\btop-windows\themes`
    /// 6. Embedded curated Apache-2.0 bundle
    pub fn load_theme(&self, name_or_path: &str) -> Result<ThemeSpec, ThemeError> {
        let trimmed = name_or_path.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("default") {
            return Ok(ThemeSpec::default_dark());
        }

        // Direct explicit `.theme` file path (e.g. `color_theme = "C:\\themes\\custom.theme"`).
        let explicit = Path::new(trimmed);
        let has_theme_ext = explicit
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("theme"));
        if (trimmed.contains('/') || trimmed.contains('\\')) && has_theme_ext && explicit.is_file()
        {
            return read_and_parse_theme_file(explicit);
        }

        // Safe relative theme stem lookup across ordered search directories (reject traversal).
        let stem = strip_theme_ext(trimmed);
        if is_safe_theme_stem(stem) {
            let file_name = format!("{stem}.theme");
            for dir in self.ordered_dirs() {
                let candidate = dir.join(&file_name);
                if candidate.is_file() {
                    return read_and_parse_theme_file(&candidate);
                }
            }
        }

        // Built-in curated Apache-2.0 theme bundle.
        if let Some(spec) = load_bundled_theme(stem)? {
            return Ok(spec);
        }

        Err(ThemeError::NotFound(trimmed.to_owned()))
    }

    /// Loads a theme or falls back cleanly to [`ThemeSpec::default_dark`].
    pub fn load_theme_or_default(&self, name_or_path: &str) -> ThemeSpec {
        self.load_theme(name_or_path)
            .unwrap_or_else(|_| ThemeSpec::default_dark())
    }
}

/// Loads one of the embedded curated Apache-2.0 themes by stem (case-insensitive).
pub fn load_bundled_theme(name: &str) -> Result<Option<ThemeSpec>, ThemeError> {
    let trimmed = name.trim();
    let stem = strip_theme_ext(trimmed);
    for (bundled_name, content) in BUNDLED_THEMES {
        if bundled_name.eq_ignore_ascii_case(stem) {
            return parse_theme(content).map(Some);
        }
    }
    Ok(None)
}

fn strip_theme_ext(name: &str) -> &str {
    let ext_len = ".theme".len();
    if name.len() >= ext_len
        && let Some(tail) = name.get(name.len() - ext_len..)
        && tail.eq_ignore_ascii_case(".theme")
    {
        &name[..name.len() - ext_len]
    } else {
        name
    }
}

fn is_safe_theme_stem(stem: &str) -> bool {
    !stem.is_empty()
        && !stem.contains('/')
        && !stem.contains('\\')
        && !stem.contains("..")
        && !stem.contains(':')
}

fn read_and_parse_theme_file(path: &Path) -> Result<ThemeSpec, ThemeError> {
    let meta = std::fs::metadata(path).map_err(|e| ThemeError::Io(e.to_string()))?;
    if meta.len() > MAX_THEME_FILE_BYTES {
        return Err(ThemeError::Io(format!(
            "theme file '{}' exceeds {} byte limit",
            path.display(),
            MAX_THEME_FILE_BYTES
        )));
    }
    let content = std::fs::read_to_string(path).map_err(|e| ThemeError::Io(e.to_string()))?;
    parse_theme(&content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_48_theme_keys_hex_greyscale_transparent_and_defaults() {
        assert_eq!(ALL_THEME_KEYS.len(), 48);

        let full_theme = r##"
# Full 48-key test theme
theme[main_bg]=""
theme[main_fg]="#cc"
theme[title]="#ffffff"
theme[hi_fg]="255 128 0"
theme[selected_bg]="#112233"
theme[selected_fg]="#eeffee"
theme[inactive_fg]="#55"
theme[graph_text]="#aabbcc"
theme[meter_bg]="#222222"
theme[proc_misc]="#336699"
theme[cpu_box]="#102030"
theme[mem_box]="#203040"
theme[net_box]="#304050"
theme[proc_box]="#405060"
theme[div_line]="#506070"
theme[followed_bg]="#010203"
theme[followed_fg]="#fdfeff"
theme[proc_pause_bg]="#aa1122"
theme[proc_follow_bg]="#1155aa"
theme[proc_banner_bg]="#662288"
theme[proc_banner_fg]="#fafbfc"
theme[temp_start]="#0000ff"
theme[temp_mid]=""
theme[temp_end]="#ff0000"
theme[cpu_start]="#00ff00"
theme[cpu_mid]="#ffff00"
theme[cpu_end]="#ff0000"
theme[free_start]="#101010"
theme[free_mid]="#202020"
theme[free_end]="#303030"
theme[cached_start]="#111111"
theme[cached_mid]="#222222"
theme[cached_end]="#333333"
theme[available_start]="#121212"
theme[available_mid]="#242424"
theme[available_end]="#363636"
theme[used_start]="#131313"
theme[used_mid]="#262626"
theme[used_end]="#393939"
theme[download_start]="#141414"
theme[download_mid]="#282828"
theme[download_end]="#3c3c3c"
theme[upload_start]="#151515"
theme[upload_mid]="#2a2a2a"
theme[upload_end]="#3f3f3f"
theme[process_start]="#161616"
theme[process_mid]="#2c2c2c"
theme[process_end]="#424242"
"##;

        let parsed = parse_theme(full_theme).expect("parse 48-key theme");
        assert_eq!(parsed.main_bg, ThemeColor::Transparent);
        assert_eq!(parsed.main_fg, ThemeRgb::new(0xCC, 0xCC, 0xCC));
        assert_eq!(parsed.inactive_fg, ThemeRgb::new(0x55, 0x55, 0x55));
        assert_eq!(parsed.hi_fg, ThemeRgb::new(255, 128, 0));
        assert_eq!(parsed.followed_bg, ThemeRgb::new(1, 2, 3));
        assert_eq!(parsed.proc_pause_bg, ThemeRgb::new(0xAA, 0x11, 0x22));
        assert_eq!(parsed.proc_banner_fg, ThemeRgb::new(0xFA, 0xFB, 0xFC));

        // 2-stop gradient (temp_mid = ""): midpoint at 50 is linear average of blue and red.
        assert_eq!(parsed.temp.at(0), ThemeRgb::new(0, 0, 255));
        assert_eq!(parsed.temp.at(50), ThemeRgb::new(128, 0, 128));
        assert_eq!(parsed.temp.at(100), ThemeRgb::new(255, 0, 0));

        // 3-stop gradient (cpu): 0=green, 50=yellow, 100=red, 25=halfway green->yellow.
        assert_eq!(parsed.cpu.steps().len(), 101);
        assert_eq!(parsed.cpu.at(0), ThemeRgb::new(0, 255, 0));
        assert_eq!(parsed.cpu.at(25), ThemeRgb::new(128, 255, 0));
        assert_eq!(parsed.cpu.at(50), ThemeRgb::new(255, 255, 0));
        assert_eq!(parsed.cpu.at(75), ThemeRgb::new(255, 128, 0));
        assert_eq!(parsed.cpu.at(100), ThemeRgb::new(255, 0, 0));

        // Omitted v1.4 banner keys keep clean default values.
        let minimal = parse_theme("theme[cpu_box]=\"#123456\"\n").expect("parse minimal theme");
        let defaults = ThemeSpec::default_dark();
        assert_eq!(minimal.cpu_box, ThemeRgb::new(0x12, 0x34, 0x56));
        assert_eq!(minimal.proc_pause_bg, defaults.proc_pause_bg);
        assert_eq!(minimal.followed_bg, defaults.followed_bg);

        // Invalid ASCII and multi-byte Unicode hex strings fail cleanly without panicking.
        assert!(matches!(
            parse_theme("theme[title]=\"#ZZZZZZ\"\n"),
            Err(ThemeError::InvalidColor { ref key, .. }) if key == "title"
        ));
        assert!(matches!(
            parse_theme("theme[title]=\"#a€bc\"\n"),
            Err(ThemeError::InvalidColor { ref key, .. }) if key == "title"
        ));
    }

    #[test]
    fn parses_every_bundled_theme_file_with_zero_errors_and_valid_gradients() {
        assert_eq!(BUNDLED_THEMES.len(), 10);
        for (name, content) in BUNDLED_THEMES {
            let spec = parse_theme(content).unwrap_or_else(|e| panic!("theme {name} failed: {e}"));
            for grad in [
                &spec.temp,
                &spec.cpu,
                &spec.free,
                &spec.cached,
                &spec.available,
                &spec.used,
                &spec.download,
                &spec.upload,
                &spec.process,
            ] {
                assert_eq!(grad.at(0), grad.start, "theme {name} gradient start");
                assert_eq!(grad.at(100), grad.end, "theme {name} gradient end");
                if let Some(mid) = grad.mid {
                    assert_eq!(grad.at(50), mid, "theme {name} gradient midpoint");
                }
            }
        }
    }

    #[test]
    fn theme_search_paths_follow_cli_then_exe_then_appdata_then_bundled_precedence() {
        let base = std::env::temp_dir().join(format!(
            "btop-theme-search-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let cli = base.join("cli");
        let exe = base.join("exe");
        let appdata = base.join("appdata");
        std::fs::create_dir_all(&cli).expect("cli dir");
        std::fs::create_dir_all(&exe).expect("exe dir");
        std::fs::create_dir_all(&appdata).expect("appdata dir");

        std::fs::write(appdata.join("custom.theme"), "theme[cpu_box]=\"#111111\"\n")
            .expect("appdata theme");
        std::fs::write(exe.join("custom.theme"), "theme[cpu_box]=\"#222222\"\n")
            .expect("exe theme");
        std::fs::write(cli.join("custom.theme"), "theme[cpu_box]=\"#333333\"\n")
            .expect("cli theme");

        // All three present -> --themes-dir wins (#333333).
        let paths = ThemeSearchPaths {
            cli_dir: Some(cli.clone()),
            exe_dir: Some(exe.clone()),
            appdata_dir: Some(appdata.clone()),
        };
        assert_eq!(
            paths.load_theme("custom").expect("cli override").cpu_box,
            ThemeRgb::new(0x33, 0x33, 0x33)
        );
        let discovered = paths.discover_themes();
        assert_eq!(discovered[0], "Default");
        assert!(discovered.iter().any(|n| n == "custom"));
        assert!(discovered.iter().any(|n| n == "tokyo-night"));

        // Without cli_dir -> exe-adjacent themes\ wins (#222222).
        let paths_no_cli = ThemeSearchPaths {
            cli_dir: None,
            exe_dir: Some(exe),
            appdata_dir: Some(appdata.clone()),
        };
        assert_eq!(
            paths_no_cli
                .load_theme("custom")
                .expect("exe override")
                .cpu_box,
            ThemeRgb::new(0x22, 0x22, 0x22)
        );

        // Without cli_dir or exe_dir -> %APPDATA% wins (#111111).
        let paths_appdata = ThemeSearchPaths {
            cli_dir: None,
            exe_dir: None,
            appdata_dir: Some(appdata),
        };
        assert_eq!(
            paths_appdata
                .load_theme("custom")
                .expect("appdata theme")
                .cpu_box,
            ThemeRgb::new(0x11, 0x11, 0x11)
        );

        // Traversal stem is safely rejected.
        assert!(paths_appdata.load_theme("..\\custom").is_err());

        let _ = std::fs::remove_dir_all(&base);
    }
}
