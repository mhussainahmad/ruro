//! Design tokens, fonts and global widget styling.
//!
//! The palette follows the grammar of modern code editors: neutral charcoal
//! surfaces separated by hairline borders, one blue accent for focus and
//! primary actions, and saturated colours reserved for data (message types and
//! runtime state). Widgets read these tokens instead of hard-coding colours.

use std::sync::Arc;

use egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Shadow, Stroke,
    TextStyle, Theme, Visuals,
};

// Surfaces, darkest to lightest.
pub const CHROME: Color32 = Color32::from_rgb(0x18, 0x18, 0x18);
pub const EDITOR: Color32 = Color32::from_rgb(0x1f, 0x1f, 0x1f);
pub const CARD: Color32 = Color32::from_rgb(0x25, 0x25, 0x26);
pub const CARD_HEADER: Color32 = Color32::from_rgb(0x2b, 0x2b, 0x2c);
pub const FIELD: Color32 = Color32::from_rgb(0x31, 0x31, 0x31);
pub const HOVER: Color32 = Color32::from_rgb(0x2a, 0x2d, 0x2e);
pub const PRESSED: Color32 = Color32::from_rgb(0x37, 0x37, 0x3d);
pub const SELECTED: Color32 = Color32::from_rgb(0x04, 0x39, 0x5e);

// Lines.
pub const BORDER: Color32 = Color32::from_rgb(0x2b, 0x2b, 0x2b);
pub const BORDER_STRONG: Color32 = Color32::from_rgb(0x3c, 0x3c, 0x3c);
pub const GRID_DOT: Color32 = Color32::from_rgb(0x30, 0x30, 0x30);

// Text.
pub const TEXT_STRONG: Color32 = Color32::from_rgb(0xe8, 0xe8, 0xe8);
pub const TEXT: Color32 = Color32::from_rgb(0xcc, 0xcc, 0xcc);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x9d, 0x9d, 0x9d);
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x6e, 0x6e, 0x6e);

// Accent and state.
pub const ACCENT: Color32 = Color32::from_rgb(0x00, 0x78, 0xd4);
pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0x02, 0x6e, 0xc1);
pub const ACCENT_TEXT: Color32 = Color32::from_rgb(0x4d, 0xaa, 0xfc);
pub const OK: Color32 = Color32::from_rgb(0x3f, 0xb9, 0x50);
pub const WARN: Color32 = Color32::from_rgb(0xcc, 0xa7, 0x00);
pub const ERR: Color32 = Color32::from_rgb(0xf1, 0x4c, 0x4c);
pub const IDLE: Color32 = Color32::from_rgb(0x5a, 0x5a, 0x5a);

pub const RADIUS_SM: u8 = 3;
pub const RADIUS: u8 = 5;
pub const RADIUS_LG: u8 = 8;

/// Heights of the fixed chrome rows.
pub const TITLEBAR_H: f32 = 36.0;
pub const STATUSBAR_H: f32 = 24.0;
pub const TABSTRIP_H: f32 = 35.0;
pub const ROW_H: f32 = 24.0;

/// Font families beyond egui's two defaults.
pub fn medium() -> FontFamily {
    FontFamily::Name("medium".into())
}

pub fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

pub fn icon_family() -> FontFamily {
    FontFamily::Name("icons".into())
}

pub fn font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

pub fn font_medium(size: f32) -> FontId {
    FontId::new(size, medium())
}

pub fn font_semibold(size: f32) -> FontId {
    FontId::new(size, semibold())
}

pub fn font_mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

pub fn font_icon(size: f32) -> FontId {
    FontId::new(size, icon_family())
}

/// Stable colour for a message type, used for ports, wires and legends.
pub fn type_color(ty: &str) -> Color32 {
    match ty {
        "std/Image" => Color32::from_rgb(0xc5, 0x86, 0xc0),
        "std/Scalar" => Color32::from_rgb(0x4f, 0xc1, 0xff),
        "std/JointState" => Color32::from_rgb(0xd7, 0xba, 0x7d),
        "std/Pose" => Color32::from_rgb(0x4e, 0xc9, 0xb0),
        "std/Twist" => Color32::from_rgb(0xb5, 0xce, 0xa8),
        "std/Vec3" => Color32::from_rgb(0xce, 0x91, 0x78),
        "std/PointCloud" => Color32::from_rgb(0x9c, 0xdc, 0xfe),
        "std/Text" => Color32::from_rgb(0xd4, 0xd4, 0xd4),
        other => {
            // FNV-1a onto a ring of evenly bright hues, so custom types stay distinct.
            let h = other.bytes().fold(0xcbf29ce484222325u64, |h, b| {
                (h ^ b as u64).wrapping_mul(0x100000001b3)
            });
            egui::ecolor::Hsva::new((h % 360) as f32 / 360.0, 0.45, 0.9, 1.0).into()
        }
    }
}

/// Icon for a node type, chosen from what it does.
pub fn kind_icon(kind: &str) -> &'static str {
    use crate::icons::*;
    match kind.rsplit_once('/').map_or(kind, |(_, n)| n) {
        "fake-camera" => CAMERA,
        "blur" => DROPLET,
        "grayscale" => CONTRAST,
        "image-stats" => CHART_LINE,
        "sine" => WAVES,
        "gain" => SLIDERS_HORIZONTAL,
        "pid" => GAUGE,
        "fake-arm" => BOT,
        _ => BOX,
    }
}

pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);
    ctx.set_theme(Theme::Dark);
    ctx.global_style_mut(|style| {
        style.text_styles = [
            (TextStyle::Heading, font_semibold(15.0)),
            (TextStyle::Body, font(13.0)),
            (TextStyle::Button, font(13.0)),
            (TextStyle::Small, font(11.0)),
            (TextStyle::Monospace, font_mono(12.0)),
        ]
        .into();

        let s = &mut style.spacing;
        s.item_spacing = egui::vec2(6.0, 4.0);
        s.button_padding = egui::vec2(8.0, 3.0);
        s.interact_size = egui::vec2(24.0, 22.0);
        s.icon_width = 14.0;
        s.icon_width_inner = 8.0;
        s.menu_margin = Margin::same(4);
        s.window_margin = Margin::same(10);
        s.combo_width = 120.0;
        s.slider_rail_height = 4.0;
        s.scroll = egui::style::ScrollStyle::thin();

        style.interaction.tooltip_delay = 0.35;
        style.animation_time = 0.08;

        let v: &mut Visuals = &mut style.visuals;
        v.dark_mode = true;
        v.override_text_color = None;
        v.panel_fill = CHROME;
        v.window_fill = CARD;
        v.extreme_bg_color = FIELD;
        v.faint_bg_color = CARD;
        v.code_bg_color = FIELD;
        v.window_stroke = Stroke::new(1.0, BORDER_STRONG);
        v.window_corner_radius = CornerRadius::same(RADIUS_LG);
        v.menu_corner_radius = CornerRadius::same(RADIUS);
        v.window_shadow = Shadow {
            offset: [0, 8],
            blur: 28,
            spread: 0,
            color: Color32::from_black_alpha(140),
        };
        v.popup_shadow = Shadow {
            offset: [0, 4],
            blur: 16,
            spread: 0,
            color: Color32::from_black_alpha(120),
        };
        v.hyperlink_color = ACCENT_TEXT;
        v.selection.bg_fill = SELECTED;
        v.selection.stroke = Stroke::new(1.0, ACCENT_TEXT);
        v.warn_fg_color = WARN;
        v.error_fg_color = ERR;
        v.text_cursor.stroke = Stroke::new(1.5, TEXT_STRONG);
        v.striped = false;
        v.slider_trailing_fill = true;
        v.handle_shape = egui::style::HandleShape::Circle;
        v.collapsing_header_frame = false;

        let r = CornerRadius::same(RADIUS_SM);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = CHROME;
        w.noninteractive.weak_bg_fill = CHROME;
        w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        w.noninteractive.corner_radius = r;

        w.inactive.bg_fill = FIELD;
        w.inactive.weak_bg_fill = Color32::TRANSPARENT;
        w.inactive.bg_stroke = Stroke::NONE;
        w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        w.inactive.corner_radius = r;

        w.hovered.bg_fill = Color32::from_rgb(0x3a, 0x3a, 0x3a);
        w.hovered.weak_bg_fill = HOVER;
        w.hovered.bg_stroke = Stroke::NONE;
        w.hovered.fg_stroke = Stroke::new(1.0, TEXT_STRONG);
        w.hovered.corner_radius = r;
        w.hovered.expansion = 0.0;

        w.active.bg_fill = PRESSED;
        w.active.weak_bg_fill = PRESSED;
        w.active.bg_stroke = Stroke::new(1.0, ACCENT);
        w.active.fg_stroke = Stroke::new(1.0, TEXT_STRONG);
        w.active.corner_radius = r;
        w.active.expansion = 0.0;

        w.open.bg_fill = PRESSED;
        w.open.weak_bg_fill = PRESSED;
        w.open.bg_stroke = Stroke::NONE;
        w.open.fg_stroke = Stroke::new(1.0, TEXT_STRONG);
        w.open.corner_radius = r;
    });
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let mut add = |name: &str, bytes: &'static [u8]| {
        fonts
            .font_data
            .insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
    };
    add("inter", include_bytes!("../assets/fonts/Inter-Regular.ttf"));
    add(
        "inter-medium",
        include_bytes!("../assets/fonts/Inter-Medium.ttf"),
    );
    add(
        "inter-semibold",
        include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
    );
    add(
        "jetbrains-mono",
        include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
    );
    add("lucide", include_bytes!("../assets/icons/lucide.ttf"));

    // Each family falls back to the icon font so glyphs can be mixed into text,
    // then to egui's bundled fonts for anything the subsets do not cover.
    let fallback: Vec<String> = fonts.families[&FontFamily::Proportional].clone();
    let family = |primary: &str| {
        let mut f = vec![primary.to_owned(), "lucide".to_owned()];
        f.extend(fallback.iter().cloned());
        f
    };
    fonts
        .families
        .insert(FontFamily::Proportional, family("inter"));
    fonts.families.insert(medium(), family("inter-medium"));
    fonts.families.insert(semibold(), family("inter-semibold"));
    fonts
        .families
        .insert(FontFamily::Monospace, family("jetbrains-mono"));
    fonts
        .families
        .insert(icon_family(), vec!["lucide".to_owned()]);
    ctx.set_fonts(fonts);
}
