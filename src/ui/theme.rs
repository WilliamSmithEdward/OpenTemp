//! Colour tokens and egui styling. Light and dark are each tuned for their own
//! surface rather than one being an automatic inversion of the other.

use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Theme, Visuals};

#[derive(Clone, Copy)]
pub struct Palette {
    pub page: Color32,
    pub surface: Color32,
    pub surface_raised: Color32,
    pub sidebar: Color32,
    pub border: Color32,
    pub grid: Color32,
    pub baseline: Color32,
    pub text: Color32,
    pub text_secondary: Color32,
    pub text_muted: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    /// Categorical series colours, in fixed order (never cycled).
    pub series: [Color32; 4],
    pub good: Color32,
    pub warning: Color32,
    pub serious: Color32,
    pub critical: Color32,
    pub track: Color32,
}

const fn hex(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

pub const LIGHT: Palette = Palette {
    page: hex(0xf3f3f0),
    surface: hex(0xfcfcfb),
    surface_raised: hex(0xffffff),
    sidebar: hex(0xebebe7),
    border: Color32::from_rgba_premultiplied(0, 0, 0, 24),
    grid: hex(0xe1e0d9),
    baseline: hex(0xc3c2b7),
    text: hex(0x0b0b0b),
    text_secondary: hex(0x52514e),
    text_muted: hex(0x75736e),
    accent: hex(0x2a78d6),
    accent_soft: hex(0xdbe8f8),
    series: [hex(0x2a78d6), hex(0xeb6834), hex(0x1baf7a), hex(0x6250d6)],
    good: hex(0x0ca30c),
    warning: hex(0xc98500),
    serious: hex(0xd95926),
    critical: hex(0xd03b3b),
    track: hex(0xe6e5df),
};

pub const DARK: Palette = Palette {
    page: hex(0x0f0f0e),
    surface: hex(0x1a1a19),
    surface_raised: hex(0x222220),
    sidebar: hex(0x141413),
    border: Color32::from_rgba_premultiplied(26, 26, 26, 26),
    grid: hex(0x2c2c2a),
    baseline: hex(0x383835),
    text: hex(0xf0efec),
    text_secondary: hex(0xc3c2b7),
    text_muted: hex(0x8f8d87),
    accent: hex(0x3987e5),
    accent_soft: hex(0x1e3350),
    series: [hex(0x3987e5), hex(0xd95926), hex(0x199e70), hex(0x9085e9)],
    good: hex(0x0ca30c),
    warning: hex(0xfab219),
    serious: hex(0xec835a),
    critical: hex(0xe66767),
    track: hex(0x2e2e2b),
};

pub fn palette(ui: &egui::Ui) -> &'static Palette {
    if ui.visuals().dark_mode { &DARK } else { &LIGHT }
}

/// Severity of a reading, used for colour *and* an accompanying text label.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Normal,
    Elevated,
    High,
    Critical,
}

impl Level {
    pub fn color(self, p: &Palette) -> Color32 {
        match self {
            Level::Normal => p.good,
            Level::Elevated => p.warning,
            Level::High => p.serious,
            Level::Critical => p.critical,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Level::Normal => "Normal",
            Level::Elevated => "Elevated",
            Level::High => "High",
            Level::Critical => "Critical",
        }
    }

    /// Bar / accent colour: neutral accent while normal so the screen is not a sea of green.
    pub fn bar(self, p: &Palette) -> Color32 {
        if self == Level::Normal { p.accent } else { self.color(p) }
    }

    pub fn for_load(pct: f32) -> Self {
        match pct {
            p if p >= 95.0 => Level::Critical,
            p if p >= 85.0 => Level::High,
            p if p >= 70.0 => Level::Elevated,
            _ => Level::Normal,
        }
    }

    pub fn for_temp(c: f32, warn: f32, hot: f32) -> Self {
        if c >= hot {
            Level::Critical
        } else if c >= warn {
            Level::High
        } else if c >= warn - 10.0 {
            Level::Elevated
        } else {
            Level::Normal
        }
    }
}

fn visuals_for(p: &Palette, dark: bool) -> Visuals {
    let mut v = if dark { Visuals::dark() } else { Visuals::light() };
    v.panel_fill = p.page;
    v.window_fill = p.surface_raised;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    v.extreme_bg_color = p.surface;
    v.faint_bg_color = if dark { Color32::from_rgb(0x1f, 0x1f, 0x1d) } else { Color32::from_rgb(0xf6, 0xf6, 0xf3) };
    v.code_bg_color = p.track;
    v.hyperlink_color = p.accent;
    v.selection.bg_fill = p.accent_soft;
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.override_text_color = None;
    v.weak_text_color = Some(p.text_muted);
    v.striped = true;

    let radius = CornerRadius::same(6);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.surface;
    w.noninteractive.weak_bg_fill = p.surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text_secondary);
    w.noninteractive.corner_radius = radius;
    for (state, fill) in [
        (&mut w.inactive, p.track),
        (&mut w.hovered, if dark { Color32::from_rgb(0x3a, 0x3a, 0x37) } else { Color32::from_rgb(0xdc, 0xdb, 0xd4) }),
        (&mut w.active, p.accent_soft),
        (&mut w.open, p.track),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.corner_radius = radius;
        state.bg_stroke = Stroke::NONE;
        state.fg_stroke = Stroke::new(1.0, p.text);
    }
    w.hovered.bg_stroke = Stroke::new(1.0, p.baseline);
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    v
}

pub fn install(ctx: &egui::Context) {
    ctx.set_visuals_of(Theme::Dark, visuals_for(&DARK, true));
    ctx.set_visuals_of(Theme::Light, visuals_for(&LIGHT, false));
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.interact_size.y = 26.0;
        style.spacing.scroll.bar_width = 8.0;
        style.text_styles = [
            (TextStyle::Small, FontId::new(11.5, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(13.5, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(13.5, FontFamily::Proportional)),
            (TextStyle::Heading, FontId::new(20.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(12.5, FontFamily::Monospace)),
        ]
        .into();
    });
}
