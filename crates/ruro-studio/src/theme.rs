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

/// Light or dark. The studio draws from one thread, so the active theme is
/// thread-local: parallel UI tests can render different themes side by side.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeKind {
    #[default]
    Dark,
    Light,
}

thread_local! {
    static ACTIVE: std::cell::Cell<ThemeKind> = const { std::cell::Cell::new(ThemeKind::Dark) };
}

pub fn active() -> ThemeKind {
    ACTIVE.with(|a| a.get())
}

pub fn is_light() -> bool {
    active() == ThemeKind::Light
}

/// One complete set of colour tokens.
struct Palette {
    chrome: Color32,
    editor: Color32,
    card: Color32,
    card_header: Color32,
    field: Color32,
    hover: Color32,
    pressed: Color32,
    selected: Color32,
    list_selected: Color32,
    border: Color32,
    border_strong: Color32,
    grid_dot: Color32,
    text_strong: Color32,
    text: Color32,
    text_dim: Color32,
    text_faint: Color32,
    accent: Color32,
    accent_hover: Color32,
    accent_text: Color32,
    ok: Color32,
    warn: Color32,
    err: Color32,
    idle: Color32,
    control: Color32,
    control_hover: Color32,
    control_disabled: Color32,
    command_center: Color32,
    command_center_hover: Color32,
    status_connecting: Color32,
    status_offline: Color32,
    /// The emergency-stop button: a deep red that white text reads on.
    estop: Color32,
    estop_hover: Color32,
    /// Opacity multiplier for drop shadows.
    shadow: f32,
}

const DARK: Palette = Palette {
    chrome: Color32::from_rgb(0x18, 0x18, 0x18),
    editor: Color32::from_rgb(0x1f, 0x1f, 0x1f),
    card: Color32::from_rgb(0x25, 0x25, 0x26),
    card_header: Color32::from_rgb(0x2b, 0x2b, 0x2c),
    field: Color32::from_rgb(0x31, 0x31, 0x31),
    hover: Color32::from_rgb(0x2a, 0x2d, 0x2e),
    pressed: Color32::from_rgb(0x37, 0x37, 0x3d),
    selected: Color32::from_rgb(0x04, 0x39, 0x5e),
    list_selected: Color32::from_rgb(0x37, 0x37, 0x3d),
    border: Color32::from_rgb(0x2b, 0x2b, 0x2b),
    border_strong: Color32::from_rgb(0x3c, 0x3c, 0x3c),
    grid_dot: Color32::from_rgb(0x30, 0x30, 0x30),
    text_strong: Color32::from_rgb(0xe8, 0xe8, 0xe8),
    text: Color32::from_rgb(0xcc, 0xcc, 0xcc),
    text_dim: Color32::from_rgb(0x9d, 0x9d, 0x9d),
    text_faint: Color32::from_rgb(0x6e, 0x6e, 0x6e),
    accent: Color32::from_rgb(0x00, 0x78, 0xd4),
    accent_hover: Color32::from_rgb(0x02, 0x6e, 0xc1),
    accent_text: Color32::from_rgb(0x4d, 0xaa, 0xfc),
    ok: Color32::from_rgb(0x3f, 0xb9, 0x50),
    warn: Color32::from_rgb(0xcc, 0xa7, 0x00),
    err: Color32::from_rgb(0xf1, 0x4c, 0x4c),
    idle: Color32::from_rgb(0x5a, 0x5a, 0x5a),
    control: Color32::from_rgb(0x31, 0x31, 0x31),
    control_hover: Color32::from_rgb(0x3c, 0x3c, 0x3c),
    control_disabled: Color32::from_rgb(0x2a, 0x2a, 0x2a),
    command_center: Color32::from_rgb(0x22, 0x22, 0x22),
    command_center_hover: Color32::from_rgb(0x2a, 0x2a, 0x2a),
    status_connecting: Color32::from_rgb(0x3a, 0x3a, 0x3a),
    status_offline: Color32::from_rgb(0x6b, 0x1f, 0x1f),
    estop: Color32::from_rgb(0xc4, 0x2b, 0x1c),
    estop_hover: Color32::from_rgb(0xd6, 0x3a, 0x2a),
    shadow: 1.0,
};

/// Modelled on the neutral light themes of modern editors: white editor,
/// off-white chrome, grey hairlines and the same blue accent.
const LIGHT: Palette = Palette {
    chrome: Color32::from_rgb(0xf8, 0xf8, 0xf8),
    editor: Color32::from_rgb(0xff, 0xff, 0xff),
    card: Color32::from_rgb(0xff, 0xff, 0xff),
    card_header: Color32::from_rgb(0xf5, 0xf5, 0xf5),
    field: Color32::from_rgb(0xff, 0xff, 0xff),
    hover: Color32::from_rgb(0xed, 0xed, 0xed),
    pressed: Color32::from_rgb(0xe2, 0xe2, 0xe4),
    selected: Color32::from_rgb(0xd5, 0xe8, 0xfa),
    list_selected: Color32::from_rgb(0xe4, 0xe6, 0xea),
    border: Color32::from_rgb(0xe5, 0xe5, 0xe5),
    border_strong: Color32::from_rgb(0xd0, 0xd0, 0xd0),
    grid_dot: Color32::from_rgb(0xdc, 0xdc, 0xdc),
    text_strong: Color32::from_rgb(0x1e, 0x1e, 0x1e),
    text: Color32::from_rgb(0x3b, 0x3b, 0x3b),
    text_dim: Color32::from_rgb(0x61, 0x61, 0x61),
    text_faint: Color32::from_rgb(0x8c, 0x8c, 0x8c),
    accent: Color32::from_rgb(0x00, 0x78, 0xd4),
    accent_hover: Color32::from_rgb(0x00, 0x6a, 0xbc),
    accent_text: Color32::from_rgb(0x00, 0x5f, 0xb8),
    ok: Color32::from_rgb(0x2a, 0x8f, 0x3c),
    warn: Color32::from_rgb(0xa8, 0x76, 0x00),
    err: Color32::from_rgb(0xd1, 0x34, 0x38),
    idle: Color32::from_rgb(0xb4, 0xb4, 0xb4),
    control: Color32::from_rgb(0xff, 0xff, 0xff),
    control_hover: Color32::from_rgb(0xf2, 0xf2, 0xf2),
    control_disabled: Color32::from_rgb(0xee, 0xee, 0xee),
    command_center: Color32::from_rgb(0xef, 0xef, 0xef),
    command_center_hover: Color32::from_rgb(0xe6, 0xe6, 0xe6),
    status_connecting: Color32::from_rgb(0x6e, 0x6e, 0x6e),
    status_offline: Color32::from_rgb(0xa1, 0x26, 0x0d),
    estop: Color32::from_rgb(0xc4, 0x2b, 0x1c),
    estop_hover: Color32::from_rgb(0xa8, 0x22, 0x15),
    shadow: 0.28,
};

fn palette() -> &'static Palette {
    match active() {
        ThemeKind::Dark => &DARK,
        ThemeKind::Light => &LIGHT,
    }
}

macro_rules! tokens {
    ($($(#[$doc:meta])* $name:ident => $field:ident),* $(,)?) => {
        $(
            $(#[$doc])*
            #[allow(non_snake_case)]
            #[inline]
            pub fn $name() -> Color32 {
                palette().$field
            }
        )*
    };
}

// Colour tokens. They read the active theme, so they are functions; the
// upper-case names mark them as design tokens rather than computations.
tokens! {
    /// Title bar, sidebars, panels and status bar.
    CHROME => chrome,
    /// The canvas.
    EDITOR => editor,
    CARD => card,
    CARD_HEADER => card_header,
    FIELD => field,
    HOVER => hover,
    PRESSED => pressed,
    /// Keyboard selection in lists and text.
    SELECTED => selected,
    /// The current row of a tree or list.
    LIST_SELECTED => list_selected,
    BORDER => border,
    BORDER_STRONG => border_strong,
    GRID_DOT => grid_dot,
    TEXT_STRONG => text_strong,
    TEXT => text,
    TEXT_DIM => text_dim,
    TEXT_FAINT => text_faint,
    ACCENT => accent,
    ACCENT_HOVER => accent_hover,
    ACCENT_TEXT => accent_text,
    OK => ok,
    WARN => warn,
    ERR => err,
    IDLE => idle,
    /// Secondary buttons and other raised controls.
    CONTROL => control,
    CONTROL_HOVER => control_hover,
    CONTROL_DISABLED => control_disabled,
    COMMAND_CENTER => command_center,
    COMMAND_CENTER_HOVER => command_center_hover,
    STATUS_CONNECTING => status_connecting,
    STATUS_OFFLINE => status_offline,
    ESTOP => estop,
    ESTOP_HOVER => estop_hover,
}

/// A drop shadow colour of the given strength, softened in the light theme.
pub fn shadow(alpha: u8) -> Color32 {
    Color32::from_black_alpha((alpha as f32 * palette().shadow) as u8)
}

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
    let light = is_light();
    let pick = |dark: [u8; 3], light_rgb: [u8; 3]| {
        let [r, g, b] = if light { light_rgb } else { dark };
        Color32::from_rgb(r, g, b)
    };
    match ty {
        "std/Image" => pick([0xc5, 0x86, 0xc0], [0xa2, 0x45, 0xb5]),
        "std/Scalar" => pick([0x4f, 0xc1, 0xff], [0x00, 0x70, 0xc1]),
        "std/JointState" => pick([0xd7, 0xba, 0x7d], [0xa8, 0x72, 0x14]),
        "std/Pose" => pick([0x4e, 0xc9, 0xb0], [0x10, 0x86, 0x6c]),
        "std/Twist" => pick([0xb5, 0xce, 0xa8], [0x4f, 0x8a, 0x2c]),
        "std/Vec3" => pick([0xce, 0x91, 0x78], [0xb3, 0x56, 0x2b]),
        "std/PointCloud" => pick([0x9c, 0xdc, 0xfe], [0x1f, 0x8a, 0xc0]),
        "std/Text" => pick([0xd4, 0xd4, 0xd4], [0x6e, 0x6e, 0x6e]),
        other => {
            // FNV-1a onto a ring of evenly bright hues, so custom types stay distinct.
            let h = other.bytes().fold(0xcbf29ce484222325u64, |h, b| {
                (h ^ b as u64).wrapping_mul(0x100000001b3)
            });
            let (s, v) = if light { (0.72, 0.62) } else { (0.45, 0.9) };
            egui::ecolor::Hsva::new((h % 360) as f32 / 360.0, s, v, 1.0).into()
        }
    }
}

/// Extra series colours for plots, after a port's own colour.
pub fn series_color(i: usize) -> Color32 {
    const DARK_SERIES: [Color32; 5] = [
        Color32::from_rgb(0xd7, 0xba, 0x7d),
        Color32::from_rgb(0x4e, 0xc9, 0xb0),
        Color32::from_rgb(0xc5, 0x86, 0xc0),
        Color32::from_rgb(0xce, 0x91, 0x78),
        Color32::from_rgb(0xb5, 0xce, 0xa8),
    ];
    const LIGHT_SERIES: [Color32; 5] = [
        Color32::from_rgb(0xa8, 0x72, 0x14),
        Color32::from_rgb(0x10, 0x86, 0x6c),
        Color32::from_rgb(0xa2, 0x45, 0xb5),
        Color32::from_rgb(0xb3, 0x56, 0x2b),
        Color32::from_rgb(0x4f, 0x8a, 0x2c),
    ];
    let set = if is_light() {
        &LIGHT_SERIES
    } else {
        &DARK_SERIES
    };
    set[i % set.len()]
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

/// Installs fonts and the given theme.
pub fn install(ctx: &egui::Context, kind: ThemeKind) {
    install_fonts(ctx);
    apply(ctx, kind);
}

/// Switches the active theme and restyles every widget.
pub fn apply(ctx: &egui::Context, kind: ThemeKind) {
    ACTIVE.with(|a| a.set(kind));
    let light = kind == ThemeKind::Light;
    ctx.set_theme(if light { Theme::Light } else { Theme::Dark });
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
        v.dark_mode = !light;
        v.override_text_color = None;
        v.panel_fill = CHROME();
        v.window_fill = CARD();
        v.extreme_bg_color = FIELD();
        v.faint_bg_color = CARD();
        v.code_bg_color = FIELD();
        v.window_stroke = Stroke::new(1.0, BORDER_STRONG());
        v.window_corner_radius = CornerRadius::same(RADIUS_LG);
        v.menu_corner_radius = CornerRadius::same(RADIUS);
        v.window_shadow = Shadow {
            offset: [0, 8],
            blur: 28,
            spread: 0,
            color: shadow(140),
        };
        v.popup_shadow = Shadow {
            offset: [0, 4],
            blur: 16,
            spread: 0,
            color: shadow(120),
        };
        v.hyperlink_color = ACCENT_TEXT();
        v.selection.bg_fill = SELECTED();
        v.selection.stroke = Stroke::new(1.0, ACCENT_TEXT());
        v.warn_fg_color = WARN();
        v.error_fg_color = ERR();
        v.text_cursor.stroke = Stroke::new(1.5, TEXT_STRONG());
        v.striped = false;
        v.slider_trailing_fill = true;
        v.handle_shape = egui::style::HandleShape::Circle;
        v.collapsing_header_frame = false;

        let r = CornerRadius::same(RADIUS_SM);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = CHROME();
        w.noninteractive.weak_bg_fill = CHROME();
        w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER());
        w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT());
        w.noninteractive.corner_radius = r;

        // Slider rails and check boxes; distinct from text fields in the light theme.
        w.inactive.bg_fill = if light {
            Color32::from_rgb(0xdd, 0xdd, 0xdd)
        } else {
            FIELD()
        };
        w.inactive.weak_bg_fill = Color32::TRANSPARENT;
        w.inactive.bg_stroke = Stroke::NONE;
        w.inactive.fg_stroke = Stroke::new(1.0, TEXT());
        w.inactive.corner_radius = r;

        w.hovered.bg_fill = CONTROL_HOVER();
        w.hovered.weak_bg_fill = HOVER();
        w.hovered.bg_stroke = Stroke::NONE;
        w.hovered.fg_stroke = Stroke::new(1.0, TEXT_STRONG());
        w.hovered.corner_radius = r;
        w.hovered.expansion = 0.0;

        w.active.bg_fill = PRESSED();
        w.active.weak_bg_fill = PRESSED();
        w.active.bg_stroke = Stroke::new(1.0, ACCENT());
        w.active.fg_stroke = Stroke::new(1.0, TEXT_STRONG());
        w.active.corner_radius = r;
        w.active.expansion = 0.0;

        w.open.bg_fill = PRESSED();
        w.open.weak_bg_fill = PRESSED();
        w.open.bg_stroke = Stroke::NONE;
        w.open.fg_stroke = Stroke::new(1.0, TEXT_STRONG());
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
