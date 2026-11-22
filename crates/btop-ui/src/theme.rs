//! Default `TrueColor` `btop` color palette and 3-stop RGB gradient interpolation.

use btop_term::{Color, Rgb};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemePalette {
    pub main_bg: Color,
    pub main_fg: Color,
    pub title: Color,
    pub hi_fg: Color,
    pub inactive_fg: Color,
    pub div_line: Color,
    pub cpu_box: Color,
    pub proc_box: Color,
    pub selected_bg: Color,
    pub selected_fg: Color,
    pub meter_bg: Color,
    pub cpu_start: Rgb,
    pub cpu_mid: Rgb,
    pub cpu_end: Rgb,
}

impl ThemePalette {
    /// Default dark `TrueColor` palette inspired by upstream `aristocratos/btop`.
    pub fn default_dark(theme_background: bool) -> Self {
        Self {
            main_bg: if theme_background {
                rgb_color(16, 18, 24)
            } else {
                Color::Reset
            },
            main_fg: rgb_color(204, 208, 218),
            title: rgb_color(238, 240, 246),
            hi_fg: rgb_color(229, 181, 103),
            inactive_fg: rgb_color(102, 110, 126),
            div_line: rgb_color(60, 68, 82),
            cpu_box: rgb_color(98, 150, 112),
            proc_box: rgb_color(104, 138, 192),
            selected_bg: rgb_color(44, 62, 92),
            selected_fg: rgb_color(248, 250, 255),
            meter_bg: rgb_color(48, 54, 68),
            cpu_start: Rgb {
                r: 86,
                g: 196,
                b: 122,
            },
            cpu_mid: Rgb {
                r: 235,
                g: 203,
                b: 102,
            },
            cpu_end: Rgb {
                r: 230,
                g: 85,
                b: 85,
            },
        }
    }

    /// Interpolates a 3-stop CPU gradient color for `percent` in `0.0..=100.0`.
    pub fn cpu_gradient(&self, percent: f32) -> Color {
        gradient_3stop(percent, self.cpu_start, self.cpu_mid, self.cpu_end)
    }
}

pub const fn rgb_color(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(Rgb { r, g, b })
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
