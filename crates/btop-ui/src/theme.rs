//! Default `TrueColor` `btop` color palette and 3-stop RGB gradient interpolation.

use btop_term::{Color, Rgb};

/// Three-stop RGB gradient (`0% -> start`, `50% -> mid`, `100% -> end`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GradientStops {
    pub start: Rgb,
    pub mid: Rgb,
    pub end: Rgb,
}

impl GradientStops {
    pub fn sample(self, percent: f32) -> Color {
        gradient_3stop(percent, self.start, self.mid, self.end)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemePalette {
    pub main_bg: Color,
    pub main_fg: Color,
    pub title: Color,
    pub hi_fg: Color,
    pub inactive_fg: Color,
    pub div_line: Color,
    pub cpu_box: Color,
    pub mem_box: Color,
    pub proc_box: Color,
    pub selected_bg: Color,
    pub selected_fg: Color,
    pub meter_bg: Color,
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
            mem_box: rgb_color(170, 146, 94),
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
            used_start: Rgb {
                r: 196,
                g: 96,
                b: 96,
            },
            used_mid: Rgb {
                r: 224,
                g: 112,
                b: 92,
            },
            used_end: Rgb {
                r: 245,
                g: 76,
                b: 88,
            },
            available_start: Rgb {
                r: 214,
                g: 176,
                b: 84,
            },
            available_mid: Rgb {
                r: 196,
                g: 212,
                b: 98,
            },
            available_end: Rgb {
                r: 132,
                g: 222,
                b: 126,
            },
            cached_start: Rgb {
                r: 82,
                g: 152,
                b: 204,
            },
            cached_mid: Rgb {
                r: 98,
                g: 186,
                b: 224,
            },
            cached_end: Rgb {
                r: 134,
                g: 222,
                b: 242,
            },
            free_start: Rgb {
                r: 72,
                g: 162,
                b: 120,
            },
            free_mid: Rgb {
                r: 94,
                g: 198,
                b: 142,
            },
            free_end: Rgb {
                r: 132,
                g: 232,
                b: 172,
            },
        }
    }

    pub const fn cpu_stops(&self) -> GradientStops {
        GradientStops {
            start: self.cpu_start,
            mid: self.cpu_mid,
            end: self.cpu_end,
        }
    }

    pub const fn used_stops(&self) -> GradientStops {
        GradientStops {
            start: self.used_start,
            mid: self.used_mid,
            end: self.used_end,
        }
    }

    pub const fn available_stops(&self) -> GradientStops {
        GradientStops {
            start: self.available_start,
            mid: self.available_mid,
            end: self.available_end,
        }
    }

    pub const fn cached_stops(&self) -> GradientStops {
        GradientStops {
            start: self.cached_start,
            mid: self.cached_mid,
            end: self.cached_end,
        }
    }

    pub const fn free_stops(&self) -> GradientStops {
        GradientStops {
            start: self.free_start,
            mid: self.free_mid,
            end: self.free_end,
        }
    }

    /// Interpolates a 3-stop CPU gradient color for `percent` in `0.0..=100.0`.
    pub fn cpu_gradient(&self, percent: f32) -> Color {
        self.cpu_stops().sample(percent)
    }

    pub fn used_gradient(&self, percent: f32) -> Color {
        self.used_stops().sample(percent)
    }

    pub fn available_gradient(&self, percent: f32) -> Color {
        self.available_stops().sample(percent)
    }

    pub fn cached_gradient(&self, percent: f32) -> Color {
        self.cached_stops().sample(percent)
    }

    pub fn free_gradient(&self, percent: f32) -> Color {
        self.free_stops().sample(percent)
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
