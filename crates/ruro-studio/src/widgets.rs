//! Small, consistent building blocks used across the studio chrome.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Galley, Rect, Response, RichText, Sense,
    Stroke, Ui, Vec2, pos2, vec2,
};
use std::sync::Arc;

use crate::icons;
use crate::theme::*;

/// Paints an icon glyph centred in `rect`.
pub fn paint_icon(ui: &Ui, rect: Rect, glyph: &str, size: f32, color: Color32) {
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        glyph,
        font_icon(size),
        color,
    );
}

/// A non-interactive icon occupying a square of `size`.
pub fn icon(ui: &mut Ui, glyph: &str, size: f32, color: Color32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size + 2.0), Sense::hover());
    paint_icon(ui, rect, glyph, size, color);
    resp
}

/// A square, frameless icon button with a hover background.
pub fn icon_button(ui: &mut Ui, glyph: &str, tooltip: &str) -> Response {
    icon_button_sized(ui, glyph, tooltip, 24.0, 15.0, false)
}

pub fn icon_button_sized(
    ui: &mut Ui,
    glyph: &str,
    tooltip: &str,
    side: f32,
    size: f32,
    active: bool,
) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
    let enabled = ui.is_enabled();
    if enabled && (resp.hovered() || active) {
        let fill = if resp.is_pointer_button_down_on() {
            PRESSED()
        } else {
            HOVER()
        };
        ui.painter()
            .rect_filled(rect, CornerRadius::same(RADIUS), fill);
    }
    let color = match (enabled, active || resp.hovered()) {
        (false, _) => TEXT_FAINT(),
        (true, true) => TEXT_STRONG(),
        (true, false) => TEXT_DIM(),
    };
    paint_icon(ui, rect, glyph, size, color);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, tooltip));
    let resp = if enabled {
        resp.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        resp
    };
    if tooltip.is_empty() {
        resp
    } else {
        resp.on_hover_text(tooltip)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    Primary,
    Secondary,
    Danger,
}

/// A labelled button with an optional leading icon.
pub fn button(ui: &mut Ui, glyph: Option<&str>, label: &str, kind: ButtonKind) -> Response {
    let font = font_medium(12.5);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font, Color32::PLACEHOLDER);
    let icon_w = if glyph.is_some() { 18.0 } else { 0.0 };
    let size = vec2(galley.size().x + icon_w + 20.0, 26.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let enabled = ui.is_enabled();
    let hovered = enabled && resp.hovered();
    let (fill, stroke, text) = match (kind, enabled) {
        (_, false) => (CONTROL_DISABLED(), Stroke::NONE, TEXT_FAINT()),
        (ButtonKind::Primary, _) => (
            if hovered { ACCENT_HOVER() } else { ACCENT() },
            Stroke::NONE,
            Color32::WHITE,
        ),
        (ButtonKind::Secondary, _) => (
            if hovered { CONTROL_HOVER() } else { CONTROL() },
            Stroke::new(1.0, BORDER_STRONG()),
            TEXT_STRONG(),
        ),
        (ButtonKind::Danger, _) => (
            if hovered {
                ERR().gamma_multiply(0.3)
            } else {
                Color32::TRANSPARENT
            },
            Stroke::new(1.0, ERR().gamma_multiply(0.6)),
            ERR(),
        ),
    };
    let p = ui.painter();
    p.rect(
        rect,
        CornerRadius::same(RADIUS_SM + 1),
        fill,
        stroke,
        egui::StrokeKind::Inside,
    );
    let mut x = rect.left() + 10.0;
    if let Some(g) = glyph {
        paint_icon(
            ui,
            Rect::from_center_size(pos2(x + 7.0, rect.center().y), Vec2::splat(14.0)),
            g,
            13.0,
            text,
        );
        x += icon_w;
    }
    p.galley_with_override_text_color(
        pos2(x, rect.center().y - galley.size().y / 2.0),
        galley,
        text,
    );
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label));
    if enabled {
        resp.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// Uppercase sidebar/panel section title with an optional disclosure chevron.
pub fn section_header(ui: &mut Ui, title: &str, open: Option<&mut bool>) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
    let mut x = rect.left();
    if let Some(open) = open {
        if resp.clicked() {
            *open = !*open;
        }
        let glyph = if *open {
            icons::CHEVRON_DOWN
        } else {
            icons::CHEVRON_RIGHT
        };
        paint_icon(
            ui,
            Rect::from_center_size(pos2(x + 8.0, rect.center().y), Vec2::splat(14.0)),
            glyph,
            13.0,
            TEXT_DIM(),
        );
        x += 18.0;
    }
    ui.painter().text(
        pos2(x, rect.center().y),
        Align2::LEFT_CENTER,
        title.to_uppercase(),
        font_semibold(10.5),
        TEXT_DIM(),
    );
    resp
}

/// A full-width list row: icon, label, optional trailing text. Returns the row response.
pub fn list_row(
    ui: &mut Ui,
    glyph: &str,
    glyph_color: Color32,
    label: &str,
    trailing: &str,
    selected: bool,
) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, CornerRadius::ZERO, LIST_SELECTED());
    } else if resp.hovered() {
        p.rect_filled(rect, CornerRadius::ZERO, HOVER());
    }
    paint_icon(
        ui,
        Rect::from_center_size(pos2(rect.left() + 18.0, rect.center().y), Vec2::splat(16.0)),
        glyph,
        14.0,
        glyph_color,
    );
    p.text(
        pos2(rect.left() + 34.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font(13.0),
        if selected { TEXT_STRONG() } else { TEXT() },
    );
    if !trailing.is_empty() {
        p.text(
            pos2(rect.right() - 10.0, rect.center().y),
            Align2::RIGHT_CENTER,
            trailing,
            font(11.5),
            TEXT_FAINT(),
        );
    }
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, label)
    });
    resp
}

/// Label on the left in a fixed column, control on the right.
pub fn prop_row<R>(ui: &mut Ui, label: &str, hint: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.horizontal(|ui| {
        ui.set_min_height(26.0);
        let (rect, resp) = ui.allocate_exact_size(vec2(92.0, 22.0), Sense::hover());
        ui.painter().text(
            pos2(rect.left(), rect.center().y),
            Align2::LEFT_CENTER,
            label,
            font(12.5),
            TEXT_DIM(),
        );
        if !hint.is_empty() {
            resp.on_hover_text(hint);
        }
        add(ui)
    })
    .inner
}

/// A filled status dot.
pub fn dot(ui: &mut Ui, color: Color32, radius: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(radius * 2.0 + 4.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), radius, color);
    resp
}

/// A keyboard shortcut keycap.
pub fn kbd(ui: &mut Ui, keys: &str) {
    let galley = ui
        .painter()
        .layout_no_wrap(keys.to_owned(), font(11.0), TEXT_DIM());
    let size = galley.size() + vec2(10.0, 4.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(RADIUS_SM),
        CARD_HEADER(),
        Stroke::new(1.0, BORDER_STRONG()),
        egui::StrokeKind::Inside,
    );
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, TEXT_DIM());
}

/// A search/text field with a leading icon, drawn as one bordered control.
pub fn search_field(ui: &mut Ui, text: &mut String, hint: &str, width: f32) -> Response {
    let frame = egui::Frame::new()
        .fill(FIELD())
        .stroke(Stroke::new(1.0, BORDER_STRONG()))
        .corner_radius(CornerRadius::same(RADIUS_SM + 1))
        .inner_margin(egui::Margin::symmetric(6, 2));
    frame
        .show(ui, |ui| {
            ui.set_width(width - 14.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                icon(ui, icons::SEARCH, 13.0, TEXT_FAINT());
                ui.add(
                    egui::TextEdit::singleline(text)
                        .frame(egui::Frame::NONE)
                        .hint_text(RichText::new(hint).color(TEXT_FAINT()))
                        .desired_width(f32::INFINITY)
                        .margin(Vec2::ZERO),
                )
            })
            .inner
        })
        .inner
}

/// A plain single-line text field in the house style.
pub fn text_field(ui: &mut Ui, text: &mut String, hint: &str) -> Response {
    let resp = ui.add(
        egui::TextEdit::singleline(text)
            .hint_text(RichText::new(hint).color(TEXT_FAINT()))
            .desired_width(f32::INFINITY)
            .background_color(FIELD())
            .margin(vec2(6.0, 3.0)),
    );
    // On a light background a white field needs an outline to read as one.
    if is_light() && !resp.has_focus() {
        ui.painter().rect_stroke(
            resp.rect,
            CornerRadius::same(RADIUS_SM),
            Stroke::new(1.0, BORDER_STRONG()),
            egui::StrokeKind::Inside,
        );
    }
    resp
}

/// A horizontal hairline across the available width.
pub fn hairline(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter()
        .hline(rect.x_range(), rect.center().y, Stroke::new(1.0, BORDER()));
}

/// Draws a border line on one edge of the current panel.
pub fn edge(ui: &Ui, side: Side) {
    let r = ui.max_rect();
    let stroke = Stroke::new(1.0, BORDER());
    let p = ui.painter();
    match side {
        Side::Top => p.hline(r.x_range(), r.top() + 0.5, stroke),
        Side::Bottom => p.hline(r.x_range(), r.bottom() - 0.5, stroke),
        Side::Left => p.vline(r.left() + 0.5, r.y_range(), stroke),
        Side::Right => p.vline(r.right() - 0.5, r.y_range(), stroke),
    };
}

#[derive(Clone, Copy)]
pub enum Side {
    Top,
    Bottom,
    Left,
    Right,
}

/// A text galley in the given font, for custom painting.
pub fn galley(ui: &Ui, text: impl Into<String>, font: FontId, color: Color32) -> Arc<Galley> {
    ui.painter().layout_no_wrap(text.into(), font, color)
}

/// Formats a duration given in microseconds.
pub fn fmt_us(us: f64) -> String {
    if us >= 1_000_000.0 {
        format!("{:.2} s", us / 1_000_000.0)
    } else if us >= 1000.0 {
        format!("{:.1} ms", us / 1000.0)
    } else {
        format!("{us:.0} µs")
    }
}

/// Keeps the end of `s`, which is the informative part of a path.
pub fn truncate_start(s: &str, n: usize) -> String {
    let count = s.chars().count();
    if count <= n {
        s.to_owned()
    } else {
        format!("…{}", s.chars().skip(count + 1 - n).collect::<String>())
    }
}

/// A red recording dot.
pub fn record_dot(ui: &mut Ui, radius: f32, color: Color32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(radius * 2.0 + 6.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), radius, color);
    resp
}

/// The title bar's record button: a red dot, or a red square while recording.
pub fn record_button(ui: &mut Ui, recording: bool, tooltip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(28.0, 26.0), Sense::click());
    let enabled = ui.is_enabled();
    let p = ui.painter();
    let (fill, stroke) = if recording {
        (
            ERR().gamma_multiply(0.16),
            Stroke::new(1.0, ERR().gamma_multiply(0.7)),
        )
    } else if enabled && resp.hovered() {
        (CONTROL_HOVER(), Stroke::new(1.0, BORDER_STRONG()))
    } else {
        (Color32::TRANSPARENT, Stroke::new(1.0, BORDER_STRONG()))
    };
    p.rect(
        rect,
        CornerRadius::same(RADIUS_SM + 1),
        fill,
        stroke,
        egui::StrokeKind::Inside,
    );
    let color = if enabled { ERR() } else { TEXT_FAINT() };
    if recording {
        p.rect_filled(
            Rect::from_center_size(rect.center(), Vec2::splat(9.0)),
            CornerRadius::same(2),
            color,
        );
    } else {
        p.circle_filled(rect.center(), 5.0, color);
    }
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, tooltip));
    let resp = if enabled {
        resp.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        resp
    };
    resp.on_hover_text(tooltip)
}

pub fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_owned()
    } else {
        format!(
            "{}…",
            s.chars().take(n.saturating_sub(1)).collect::<String>()
        )
    }
}
