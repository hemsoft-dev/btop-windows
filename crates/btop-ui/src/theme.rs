//! `TrueColor` `btop` color palette backed by parsed 48-key `.theme` specs and precomputed
//! 101-step (0..=100) RGB gradients.

use btop_config::{ThemeColor, ThemeGradient101, ThemeRgb, ThemeSearchPaths, ThemeSpec};
use btop_term::{Color, Rgb};

/// Built-in `TrueColor` theme names selectable out of the box (in addition to any user `.theme`
/// files discovered via `--themes-dir`, exe-adjacent `themes\`, or `%APPDATA%\btop\themes`).
pub const BUILTIN_THEME_NAMES: [&str; 11] = [
    "Default",
    "dracula",
    "tokyo-night",
    "gruvbox_dark_v2",
    "nord",
    "onedark",
    "kanagawa-wave",
    "solarized_dark",
    "monokai",
    "everforest-dark-medium",
    "adwaita-dark",
];

/// Three-stop RGB gradient (`0% -> start`, `50% -> mid`, `100% -> end`) or 2-stop when `mid` is `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GradientStops {
    pub start: Rgb,
    pub mid: Option<Rgb>,
    pub end: Rgb,
}

impl GradientStops {
    pub fn sample(self, percent: f32) -> Color {
        match self.mid {
            Some(mid) => gradient_3stop(percent, self.start, mid, self.end),
            None => gradient_2stop(percent, self.start, self.end),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemePalette {
    pub main_bg: Color,
    pub main_fg: Color,
    pub title: Color,
    pub hi_fg: Color,
    pub inactive_fg: Color,
    pub graph_text: Color,
    pub proc_misc: Color,
    pub div_line: Color,
    pub cpu_box: Color,
    pub mem_box: Color,
    pub net_box: Color,
    pub proc_box: Color,
    pub selected_bg: Color,
    pub selected_fg: Color,
    pub followed_bg: Color,
    pub followed_fg: Color,
    pub proc_pause_bg: Color,
    pub proc_follow_bg: Color,
    pub proc_banner_bg: Color,
    pub proc_banner_fg: Color,
    pub meter_bg: Color,
    pub temp_grad: ThemeGradient101,
    pub cpu_grad: ThemeGradient101,
    pub free_grad: ThemeGradient101,
    pub cached_grad: ThemeGradient101,
    pub available_grad: ThemeGradient101,
    pub used_grad: ThemeGradient101,
    pub download_grad: ThemeGradient101,
    pub upload_grad: ThemeGradient101,
    pub process_grad: ThemeGradient101,
    pub cpu_start: Rgb,
    pub cpu_mid: Rgb,
    pub cpu_end: Rgb,
    pub used_start: Rgb,
    pub used_mid: Rgb,
    pub used_end: Rgb,
    pub available_start: Rgb,
    pub available_mid: Rgb,
    pub available_end: Rgb,
    pub cached_start: Rgb,
    pub cached_mid: Rgb,
    pub cached_end: Rgb,
    pub free_start: Rgb,
    pub free_mid: Rgb,
    pub free_end: Rgb,
    pub download_start: Rgb,
    pub download_mid: Rgb,
    pub download_end: Rgb,
    pub upload_start: Rgb,
    pub upload_mid: Rgb,
    pub upload_end: Rgb,
}

impl ThemePalette {
    /// Converts a parsed 48-key [`ThemeSpec`] into a rendering [`ThemePalette`], honoring
    /// `theme_background = false` and transparent `main_bg = ""` via [`Color::Reset`].
    pub fn from_spec(spec: &ThemeSpec, theme_background: bool) -> Self {
        let main_bg = if theme_background {
            match spec.main_bg {
                ThemeColor::Transparent => Color::Reset,
                ThemeColor::Rgb(rgb) => to_color(rgb),
            }
        } else {
            Color::Reset
        };
        Self {
            main_bg,
            main_fg: to_color(spec.main_fg),
            title: to_color(spec.title),
            hi_fg: to_color(spec.hi_fg),
            inactive_fg: to_color(spec.inactive_fg),
            graph_text: to_color(spec.graph_text),
            proc_misc: to_color(spec.proc_misc),
            div_line: to_color(spec.div_line),
            cpu_box: to_color(spec.cpu_box),
            mem_box: to_color(spec.mem_box),
            net_box: to_color(spec.net_box),
            proc_box: to_color(spec.proc_box),
            selected_bg: to_color(spec.selected_bg),
            selected_fg: to_color(spec.selected_fg),
            followed_bg: to_color(spec.followed_bg),
            followed_fg: to_color(spec.followed_fg),
            proc_pause_bg: to_color(spec.proc_pause_bg),
            proc_follow_bg: to_color(spec.proc_follow_bg),
            proc_banner_bg: to_color(spec.proc_banner_bg),
            proc_banner_fg: to_color(spec.proc_banner_fg),
            meter_bg: to_color(spec.meter_bg),
            temp_grad: spec.temp,
            cpu_grad: spec.cpu,
            free_grad: spec.free,
            cached_grad: spec.cached,
            available_grad: spec.available,
            used_grad: spec.used,
            download_grad: spec.download,
            upload_grad: spec.upload,
            process_grad: spec.process,
            cpu_start: to_rgb(spec.cpu.start),
            cpu_mid: to_rgb(spec.cpu.at(50)),
            cpu_end: to_rgb(spec.cpu.end),
            used_start: to_rgb(spec.used.start),
            used_mid: to_rgb(spec.used.at(50)),
            used_end: to_rgb(spec.used.end),
            available_start: to_rgb(spec.available.start),
            available_mid: to_rgb(spec.available.at(50)),
            available_end: to_rgb(spec.available.end),
            cached_start: to_rgb(spec.cached.start),
            cached_mid: to_rgb(spec.cached.at(50)),
            cached_end: to_rgb(spec.cached.end),
            free_start: to_rgb(spec.free.start),
            free_mid: to_rgb(spec.free.at(50)),
            free_end: to_rgb(spec.free.end),
            download_start: to_rgb(spec.download.start),
            download_mid: to_rgb(spec.download.at(50)),
            download_end: to_rgb(spec.download.end),
            upload_start: to_rgb(spec.upload.start),
            upload_mid: to_rgb(spec.upload.at(50)),
            upload_end: to_rgb(spec.upload.end),
        }
    }

    /// Resolves a theme by name or path using `search_paths`, falling back cleanly to `Default`.
    pub fn resolve(name: &str, theme_background: bool, search_paths: &ThemeSearchPaths) -> Self {
        let spec = search_paths.load_theme_or_default(name);
        Self::from_spec(&spec, theme_background)
    }

    /// Resolves a bundled or `Default` theme by name (case-insensitive) without touching disk.
    pub fn for_name(name: &str, theme_background: bool) -> Self {
        let spec = btop_config::load_bundled_theme(name)
            .ok()
            .flatten()
            .unwrap_or_else(ThemeSpec::default_dark);
        Self::from_spec(&spec, theme_background)
    }

    /// Default dark `TrueColor` palette inspired by upstream `aristocratos/btop`.
    pub fn default_dark(theme_background: bool) -> Self {
        Self::from_spec(&ThemeSpec::default_dark(), theme_background)
    }

    pub const fn cpu_stops(&self) -> GradientStops {
        grad_to_stops(&self.cpu_grad)
    }

    pub const fn used_stops(&self) -> GradientStops {
        grad_to_stops(&self.used_grad)
    }

    pub const fn available_stops(&self) -> GradientStops {
        grad_to_stops(&self.available_grad)
    }

    pub const fn cached_stops(&self) -> GradientStops {
        grad_to_stops(&self.cached_grad)
    }

    pub const fn free_stops(&self) -> GradientStops {
        grad_to_stops(&self.free_grad)
    }

    pub const fn download_stops(&self) -> GradientStops {
        grad_to_stops(&self.download_grad)
    }

    pub const fn upload_stops(&self) -> GradientStops {
        grad_to_stops(&self.upload_grad)
    }

    /// Samples the precomputed 101-step temperature gradient for `percent` in `0.0..=100.0`.
    pub fn temp_gradient(&self, percent: f32) -> Color {
        to_color(self.temp_grad.sample_percent(percent))
    }

    /// Samples the precomputed 101-step CPU gradient for `percent` in `0.0..=100.0`.
    pub fn cpu_gradient(&self, percent: f32) -> Color {
        to_color(self.cpu_grad.sample_percent(percent))
    }

    pub fn used_gradient(&self, percent: f32) -> Color {
        to_color(self.used_grad.sample_percent(percent))
    }

    pub fn available_gradient(&self, percent: f32) -> Color {
        to_color(self.available_grad.sample_percent(percent))
    }

    pub fn cached_gradient(&self, percent: f32) -> Color {
        to_color(self.cached_grad.sample_percent(percent))
    }

    pub fn free_gradient(&self, percent: f32) -> Color {
        to_color(self.free_grad.sample_percent(percent))
    }

    pub fn process_gradient(&self, percent: f32) -> Color {
        to_color(self.process_grad.sample_percent(percent))
    }
}

const fn to_rgb(c: ThemeRgb) -> Rgb {
    Rgb {
        r: c.r,
        g: c.g,
        b: c.b,
    }
}

const fn to_color(c: ThemeRgb) -> Color {
    Color::Rgb(to_rgb(c))
}

const fn grad_to_stops(g: &ThemeGradient101) -> GradientStops {
    GradientStops {
        start: to_rgb(g.start),
        mid: match g.mid {
            Some(m) => Some(to_rgb(m)),
            None => None,
        },
        end: to_rgb(g.end),
    }
}

pub const fn rgb_color(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(Rgb { r, g, b })
}

/// Linearly interpolates across a 2-stop RGB gradient (`0% -> start`, `100% -> end`).
pub fn gradient_2stop(percent: f32, start: Rgb, end: Rgb) -> Color {
    let t = percent.clamp(0.0, 100.0) / 100.0;
    Color::Rgb(Rgb {
        r: lerp_u8(start.r, end.r, t),
        g: lerp_u8(start.g, end.g, t),
        b: lerp_u8(start.b, end.b, t),
    })
}

/// Linearly interpolates across a 3-stop RGB gradient (`0% -> start`, `50% -> mid`, `100% -> end`).
pub fn gradient_3stop(percent: f32, start: Rgb, mid: Rgb, end: Rgb) -> Color {
    let clamped = percent.clamp(0.0, 100.0);
    let (a, b, t) = if clamped <= 50.0 {
        (start, mid, clamped / 50.0)
    } else {
        (mid, end, (clamped - 50.0) / 50.0)
    };
    Color::Rgb(Rgb {
        r: lerp_u8(a.r, b.r, t),
        g: lerp_u8(a.g, b.g, t),
        b: lerp_u8(a.b, b.b, t),
    })
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    let af = f32::from(a);
    let bf = f32::from(b);
    (af + (bf - af) * t.clamp(0.0, 1.0)).round() as u8
}

#[cfg(test)]
mod tests {
    use btop_config::Config;
    use btop_model::fixtures::win11_16core_dual_gpu_desktop;
    use btop_term::Size;

    use super::*;
    use crate::render;

    #[test]
    fn bundled_themes_render_distinct_truecolor_cells_and_honor_transparent_background() {
        let snap = win11_16core_dual_gpu_desktop()[0].clone();
        let size = Size {
            width: 120,
            height: 40,
        };

        let g_default = render(&snap, &Config::default(), size);
        let g_dracula = render(
            &snap,
            &Config {
                color_theme: "dracula".to_owned(),
                ..Config::default()
            },
            size,
        );
        let g_tokyo = render(
            &snap,
            &Config {
                color_theme: "tokyo-night".to_owned(),
                ..Config::default()
            },
            size,
        );
        let g_transparent = render(
            &snap,
            &Config {
                color_theme: "tokyo-night".to_owned(),
                theme_background: false,
                ..Config::default()
            },
            size,
        );

        let default_corner = g_default.get(0, 0).expect("corner");
        let dracula_corner = g_dracula.get(0, 0).expect("corner");
        let tokyo_corner = g_tokyo.get(0, 0).expect("corner");
        let transp_corner = g_transparent.get(0, 0).expect("corner");

        assert_ne!(default_corner.fg, dracula_corner.fg);
        assert_ne!(dracula_corner.fg, tokyo_corner.fg);
        assert_eq!(dracula_corner.bg, rgb_color(0x28, 0x2a, 0x36));
        assert_eq!(tokyo_corner.bg, rgb_color(0x1a, 0x1b, 0x26));
        assert_eq!(transp_corner.bg, Color::Reset);
    }

    #[test]
    fn process_rows_use_process_gradient_distinct_from_cpu_gradient() {
        let snap = win11_16core_dual_gpu_desktop()[0].clone();
        let cfg = Config {
            shown_boxes: "proc".to_owned(),
            ..Config::default()
        };
        let spec = btop_config::parse_theme(concat!(
            "theme[cpu_start]=\"#00ff00\"\n",
            "theme[cpu_mid]=\"#00ff00\"\n",
            "theme[cpu_end]=\"#00ff00\"\n",
            "theme[process_start]=\"#ff00ff\"\n",
            "theme[process_mid]=\"#ff00ff\"\n",
            "theme[process_end]=\"#ff00ff\"\n",
        ))
        .expect("parse custom process gradient theme");
        let palette = ThemePalette::from_spec(&spec, true);
        let hist = crate::UiHistory::from_snapshot(&snap);
        let mut state = crate::UiState::from_config(&cfg);
        state.resolved_theme = Some(palette);

        let grid = crate::render_frame(
            &snap,
            &hist,
            &state,
            &cfg,
            Size {
                width: 120,
                height: 24,
            },
        );
        let magenta = rgb_color(0xFF, 0x00, 0xFF);
        let green = rgb_color(0x00, 0xFF, 0x00);
        let mut saw_magenta = false;
        let mut saw_green = false;
        for y in 0..24 {
            for x in 0..120 {
                if let Some(cell) = grid.get(x, y) {
                    if cell.fg == magenta {
                        saw_magenta = true;
                    }
                    if cell.fg == green {
                        saw_green = true;
                    }
                }
            }
        }
        assert!(
            saw_magenta,
            "expected process rows/sparklines to render with process_gradient (#ff00ff)"
        );
        assert!(
            !saw_green,
            "process-only panel should not fall back to cpu_gradient (#00ff00)"
        );
    }
}
