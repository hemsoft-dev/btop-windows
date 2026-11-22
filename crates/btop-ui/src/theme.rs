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

/// Built-in `TrueColor` theme names selectable for instant live preview in Options.
pub const BUILTIN_THEME_NAMES: [&str; 6] = [
    "Default",
    "Dracula",
    "Nord",
    "Gruvbox",
    "Catppuccin",
    "Monokai",
];

impl ThemePalette {
    /// Resolves a built-in theme palette by name (case-insensitive), falling back to `Default`.
    pub fn for_name(name: &str, theme_background: bool) -> Self {
        let mut palette = match name.trim().to_ascii_lowercase().as_str() {
            "dracula" => Self::dracula(),
            "nord" => Self::nord(),
            "gruvbox" => Self::gruvbox(),
            "catppuccin" => Self::catppuccin(),
            "monokai" => Self::monokai(),
            _ => Self::default_dark(theme_background),
        };
        if !theme_background {
            palette.main_bg = Color::Reset;
        }
        palette
    }

    fn dracula() -> Self {
        let rgb = |r, g, b| Rgb { r, g, b };
        Self {
            main_bg: rgb_color(40, 42, 54),
            main_fg: rgb_color(248, 248, 242),
            title: rgb_color(255, 121, 198),
            hi_fg: rgb_color(241, 250, 140),
            inactive_fg: rgb_color(98, 114, 164),
            div_line: rgb_color(68, 71, 90),
            cpu_box: rgb_color(189, 147, 249),
            mem_box: rgb_color(80, 250, 123),
            net_box: rgb_color(139, 233, 253),
            proc_box: rgb_color(255, 184, 108),
            selected_bg: rgb_color(68, 71, 90),
            selected_fg: rgb_color(255, 121, 198),
            followed_bg: rgb_color(56, 78, 104),
            followed_fg: rgb_color(248, 248, 242),
            proc_pause_bg: rgb_color(140, 48, 68),
            proc_follow_bg: rgb_color(48, 96, 128),
            proc_banner_bg: rgb_color(116, 72, 168),
            proc_banner_fg: rgb_color(248, 248, 242),
            meter_bg: rgb_color(68, 71, 90),
            cpu_start: rgb(80, 250, 123),
            cpu_mid: rgb(241, 250, 140),
            cpu_end: rgb(255, 85, 85),
            used_start: rgb(255, 184, 108),
            used_mid: rgb(255, 121, 198),
            used_end: rgb(255, 85, 85),
            available_start: rgb(241, 250, 140),
            available_mid: rgb(139, 233, 253),
            available_end: rgb(80, 250, 123),
            cached_start: rgb(139, 233, 253),
            cached_mid: rgb(189, 147, 249),
            cached_end: rgb(255, 121, 198),
            free_start: rgb(80, 250, 123),
            free_mid: rgb(139, 233, 253),
            free_end: rgb(189, 147, 249),
            download_start: rgb(139, 233, 253),
            download_mid: rgb(189, 147, 249),
            download_end: rgb(255, 121, 198),
            upload_start: rgb(255, 184, 108),
            upload_mid: rgb(255, 121, 198),
            upload_end: rgb(255, 85, 85),
        }
    }

    fn nord() -> Self {
        let rgb = |r, g, b| Rgb { r, g, b };
        Self {
            main_bg: rgb_color(46, 52, 64),
            main_fg: rgb_color(216, 222, 233),
            title: rgb_color(236, 239, 244),
            hi_fg: rgb_color(136, 192, 208),
            inactive_fg: rgb_color(76, 86, 106),
            div_line: rgb_color(67, 76, 94),
            cpu_box: rgb_color(136, 192, 208),
            mem_box: rgb_color(163, 190, 140),
            net_box: rgb_color(180, 142, 173),
            proc_box: rgb_color(129, 161, 193),
            selected_bg: rgb_color(67, 76, 94),
            selected_fg: rgb_color(236, 239, 244),
            followed_bg: rgb_color(59, 84, 106),
            followed_fg: rgb_color(236, 239, 244),
            proc_pause_bg: rgb_color(142, 74, 82),
            proc_follow_bg: rgb_color(64, 98, 122),
            proc_banner_bg: rgb_color(94, 129, 172),
            proc_banner_fg: rgb_color(236, 239, 244),
            meter_bg: rgb_color(59, 66, 82),
            cpu_start: rgb(163, 190, 140),
            cpu_mid: rgb(235, 203, 139),
            cpu_end: rgb(191, 97, 106),
            used_start: rgb(208, 135, 112),
            used_mid: rgb(191, 97, 106),
            used_end: rgb(180, 82, 92),
            available_start: rgb(235, 203, 139),
            available_mid: rgb(163, 190, 140),
            available_end: rgb(143, 188, 187),
            cached_start: rgb(129, 161, 193),
            cached_mid: rgb(136, 192, 208),
            cached_end: rgb(143, 188, 187),
            free_start: rgb(143, 188, 187),
            free_mid: rgb(163, 190, 140),
            free_end: rgb(196, 218, 176),
            download_start: rgb(94, 129, 172),
            download_mid: rgb(129, 161, 193),
            download_end: rgb(136, 192, 208),
            upload_start: rgb(180, 142, 173),
            upload_mid: rgb(208, 135, 112),
            upload_end: rgb(191, 97, 106),
        }
    }

    fn gruvbox() -> Self {
        let rgb = |r, g, b| Rgb { r, g, b };
        Self {
            main_bg: rgb_color(40, 40, 40),
            main_fg: rgb_color(235, 219, 178),
            title: rgb_color(251, 241, 199),
            hi_fg: rgb_color(250, 189, 47),
            inactive_fg: rgb_color(146, 131, 116),
            div_line: rgb_color(80, 73, 69),
            cpu_box: rgb_color(184, 187, 38),
            mem_box: rgb_color(250, 189, 47),
            net_box: rgb_color(211, 134, 155),
            proc_box: rgb_color(131, 165, 152),
            selected_bg: rgb_color(80, 73, 69),
            selected_fg: rgb_color(250, 189, 47),
            followed_bg: rgb_color(69, 102, 92),
            followed_fg: rgb_color(251, 241, 199),
            proc_pause_bg: rgb_color(157, 48, 38),
            proc_follow_bg: rgb_color(69, 133, 136),
            proc_banner_bg: rgb_color(177, 98, 134),
            proc_banner_fg: rgb_color(251, 241, 199),
            meter_bg: rgb_color(60, 56, 54),
            cpu_start: rgb(152, 151, 26),
            cpu_mid: rgb(250, 189, 47),
            cpu_end: rgb(251, 73, 52),
            used_start: rgb(254, 128, 25),
            used_mid: rgb(251, 73, 52),
            used_end: rgb(204, 36, 29),
            available_start: rgb(250, 189, 47),
            available_mid: rgb(184, 187, 38),
            available_end: rgb(142, 192, 124),
            cached_start: rgb(131, 165, 152),
            cached_mid: rgb(142, 192, 124),
            cached_end: rgb(184, 187, 38),
            free_start: rgb(104, 157, 106),
            free_mid: rgb(142, 192, 124),
            free_end: rgb(184, 187, 38),
            download_start: rgb(69, 133, 136),
            download_mid: rgb(131, 165, 152),
            download_end: rgb(142, 192, 124),
            upload_start: rgb(211, 134, 155),
            upload_mid: rgb(254, 128, 25),
            upload_end: rgb(251, 73, 52),
        }
    }

    fn catppuccin() -> Self {
        let rgb = |r, g, b| Rgb { r, g, b };
        Self {
            main_bg: rgb_color(30, 30, 46),
            main_fg: rgb_color(205, 214, 244),
            title: rgb_color(245, 224, 220),
            hi_fg: rgb_color(203, 166, 247),
            inactive_fg: rgb_color(108, 112, 134),
            div_line: rgb_color(69, 71, 90),
            cpu_box: rgb_color(166, 227, 161),
            mem_box: rgb_color(249, 226, 175),
            net_box: rgb_color(203, 166, 247),
            proc_box: rgb_color(137, 180, 250),
            selected_bg: rgb_color(69, 71, 90),
            selected_fg: rgb_color(245, 224, 220),
            followed_bg: rgb_color(49, 78, 108),
            followed_fg: rgb_color(245, 224, 220),
            proc_pause_bg: rgb_color(168, 62, 86),
            proc_follow_bg: rgb_color(72, 116, 158),
            proc_banner_bg: rgb_color(136, 92, 178),
            proc_banner_fg: rgb_color(245, 224, 220),
            meter_bg: rgb_color(49, 50, 68),
            cpu_start: rgb(166, 227, 161),
            cpu_mid: rgb(249, 226, 175),
            cpu_end: rgb(243, 139, 168),
            used_start: rgb(250, 179, 135),
            used_mid: rgb(235, 160, 172),
            used_end: rgb(243, 139, 168),
            available_start: rgb(249, 226, 175),
            available_mid: rgb(166, 227, 161),
            available_end: rgb(148, 226, 213),
            cached_start: rgb(137, 180, 250),
            cached_mid: rgb(116, 199, 236),
            cached_end: rgb(148, 226, 213),
            free_start: rgb(148, 226, 213),
            free_mid: rgb(166, 227, 161),
            free_end: rgb(206, 242, 198),
            download_start: rgb(137, 180, 250),
            download_mid: rgb(116, 199, 236),
            download_end: rgb(148, 226, 213),
            upload_start: rgb(203, 166, 247),
            upload_mid: rgb(245, 194, 231),
            upload_end: rgb(243, 139, 168),
        }
    }

    fn monokai() -> Self {
        let rgb = |r, g, b| Rgb { r, g, b };
        Self {
            main_bg: rgb_color(39, 40, 34),
            main_fg: rgb_color(248, 248, 242),
            title: rgb_color(255, 255, 255),
            hi_fg: rgb_color(249, 38, 114),
            inactive_fg: rgb_color(117, 113, 94),
            div_line: rgb_color(73, 72, 62),
            cpu_box: rgb_color(166, 226, 46),
            mem_box: rgb_color(253, 151, 31),
            net_box: rgb_color(174, 129, 255),
            proc_box: rgb_color(102, 217, 239),
            selected_bg: rgb_color(73, 72, 62),
            selected_fg: rgb_color(166, 226, 46),
            followed_bg: rgb_color(52, 86, 98),
            followed_fg: rgb_color(248, 248, 242),
            proc_pause_bg: rgb_color(158, 32, 76),
            proc_follow_bg: rgb_color(54, 118, 138),
            proc_banner_bg: rgb_color(122, 78, 176),
            proc_banner_fg: rgb_color(248, 248, 242),
            meter_bg: rgb_color(62, 61, 50),
            cpu_start: rgb(166, 226, 46),
            cpu_mid: rgb(230, 219, 116),
            cpu_end: rgb(249, 38, 114),
            used_start: rgb(253, 151, 31),
            used_mid: rgb(249, 38, 114),
            used_end: rgb(220, 24, 92),
            available_start: rgb(230, 219, 116),
            available_mid: rgb(166, 226, 46),
            available_end: rgb(102, 217, 239),
            cached_start: rgb(102, 217, 239),
            cached_mid: rgb(174, 129, 255),
            cached_end: rgb(249, 38, 114),
            free_start: rgb(102, 217, 239),
            free_mid: rgb(166, 226, 46),
            free_end: rgb(230, 219, 116),
            download_start: rgb(102, 217, 239),
            download_mid: rgb(174, 129, 255),
            download_end: rgb(249, 38, 114),
            upload_start: rgb(253, 151, 31),
            upload_mid: rgb(249, 38, 114),
            upload_end: rgb(230, 219, 116),
        }
    }

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
