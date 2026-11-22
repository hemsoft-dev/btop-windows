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
    /// Default dark `TrueColor` palette inspired by upstream `aristocratos/btop`.
    pub fn default_dark(theme_background: bool) -> Self {
        let rgb = |r, g, b| Rgb { r, g, b };
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
            net_box: rgb_color(142, 116, 178),
            proc_box: rgb_color(104, 138, 192),
            selected_bg: rgb_color(44, 62, 92),
            selected_fg: rgb_color(248, 250, 255),
            followed_bg: rgb_color(34, 82, 98),
            followed_fg: rgb_color(248, 250, 255),
            proc_pause_bg: rgb_color(124, 44, 52),
            proc_follow_bg: rgb_color(30, 88, 112),
            proc_banner_bg: rgb_color(102, 56, 118),
            proc_banner_fg: rgb_color(248, 250, 255),
            meter_bg: rgb_color(48, 54, 68),
            cpu_start: rgb(86, 196, 122),
            cpu_mid: rgb(235, 203, 102),
            cpu_end: rgb(230, 85, 85),
            used_start: rgb(196, 96, 96),
            used_mid: rgb(224, 112, 92),
            used_end: rgb(245, 76, 88),
            available_start: rgb(214, 176, 84),
            available_mid: rgb(196, 212, 98),
            available_end: rgb(132, 222, 126),
            cached_start: rgb(82, 152, 204),
            cached_mid: rgb(98, 186, 224),
            cached_end: rgb(134, 222, 242),
            free_start: rgb(72, 162, 120),
            free_mid: rgb(94, 198, 142),
            free_end: rgb(132, 232, 172),
            download_start: rgb(72, 152, 212),
            download_mid: rgb(96, 196, 235),
            download_end: rgb(148, 232, 248),
            upload_start: rgb(168, 102, 204),
            upload_mid: rgb(212, 118, 192),
            upload_end: rgb(242, 142, 168),
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

    pub const fn download_stops(&self) -> GradientStops {
        GradientStops {
            start: self.download_start,
            mid: self.download_mid,
            end: self.download_end,
        }
    }

    pub const fn upload_stops(&self) -> GradientStops {
        GradientStops {
            start: self.upload_start,
            mid: self.upload_mid,
            end: self.upload_end,
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
