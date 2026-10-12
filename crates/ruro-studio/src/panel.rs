//! Bottom panel: problems, live watches and output.

use egui::{Align, Align2, Frame, Layout, Margin, RichText, Sense, Stroke, Ui, pos2, vec2};
use ruro_proto::Level;

use crate::app::{Cmd, PanelTab, Studio};
use crate::icons;
use crate::theme::*;
use crate::widgets::{self, Side};

/// Draws the bottom panel. Returns its height.
pub fn show(s: &mut Studio, ui: &mut Ui) -> f32 {
    egui::Panel::bottom("panel")
        .resizable(true)
        .default_size(s.settings.panel_height.unwrap_or(280.0))
        .size_range(140.0..=900.0)
        .frame(Frame::new().fill(CHROME()))
        .show(ui, |ui| {
            // Keep the panel at its size regardless of content.
            ui.set_min_height(ui.available_height());
            widgets::edge(ui, Side::Top);
            Frame::new()
                .inner_margin(Margin {
                    left: 10,
                    right: 8,
                    top: 0,
                    bottom: 0,
                })
                .show(ui, |ui| {
                    ui.set_height(TABSTRIP_H);
                    ui.horizontal_centered(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        let problems = s.issues.len();
                        let watches = s.watches.len();
                        let mut tabs = vec![
                            (PanelTab::Problems, "Problems", problems),
                            (PanelTab::Watch, "Watch", watches),
                            (PanelTab::Output, "Output", 0),
                        ];
                        for (k, _) in &s.plugin_panels {
                            tabs.push((PanelTab::Plugin(k.id), k.title, 0));
                        }
                        for (tab, label, count) in tabs {
                            if tab_button(
                                ui,
                                label,
                                count,
                                s.panel_tab == tab,
                                tab == PanelTab::Problems,
                            )
                            .clicked()
                            {
                                s.panel_tab = tab;
                            }
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            if widgets::icon_button(ui, icons::X, "Close panel (Ctrl+J)").clicked()
                            {
                                s.push(Cmd::TogglePanel);
                            }
                            if s.panel_tab == PanelTab::Output {
                                if widgets::icon_button(ui, icons::TRASH_2, "Clear output")
                                    .clicked()
                                {
                                    s.push(Cmd::ClearOutput);
                                }
                                ui.add_space(6.0);
                                widgets::search_field(ui, &mut s.log_filter, "Filter", 200.0);
                            }
                        });
                    });
                });
            Frame::new()
                .inner_margin(Margin {
                    left: 12,
                    right: 12,
                    top: 4,
                    bottom: 8,
                })
                .show(ui, |ui| match s.panel_tab {
                    PanelTab::Problems => problems(s, ui),
                    PanelTab::Watch => watch(s, ui),
                    PanelTab::Output => output(s, ui),
                    PanelTab::Plugin(id) => plugin(s, ui, id),
                });
        })
        .response
        .rect
        .height()
}

/// A panel contributed through [`crate::plugin::PanelKind`].
fn plugin(s: &mut Studio, ui: &mut Ui, id: &str) {
    let graph = s.doc.to_graph(&s.registry);
    let live = s.live();
    let Some(i) = s.plugin_panels.iter().position(|(k, _)| k.id == id) else {
        return;
    };
    let mut cx = crate::plugin::PanelContext {
        graph: &graph,
        registry: &s.registry,
        status: s.status.as_ref(),
        metrics: &s.metrics,
        live,
        requests: Vec::new(),
    };
    s.plugin_panels[i].1.show(&mut cx, ui);
    for r in std::mem::take(&mut cx.requests) {
        s.push(match r {
            crate::plugin::Request::Focus(n) => Cmd::Focus(Some(n)),
            crate::plugin::Request::Watch { node, port, ty } => Cmd::Watch { node, port, ty },
        });
    }
}

fn tab_button(ui: &mut Ui, label: &str, count: usize, active: bool, alarm: bool) -> egui::Response {
    let text = widgets::galley(ui, label.to_uppercase(), font_medium(11.0), TEXT());
    let badge_w = if count > 0 { 22.0 } else { 0.0 };
    let (rect, resp) = ui.allocate_exact_size(
        vec2(text.size().x + badge_w + 20.0, TABSTRIP_H),
        Sense::click(),
    );
    let color = if active || resp.hovered() {
        TEXT_STRONG()
    } else {
        TEXT_FAINT()
    };
    let p = ui.painter();
    p.galley_with_override_text_color(
        pos2(rect.left() + 10.0, rect.center().y - text.size().y / 2.0),
        text.clone(),
        color,
    );
    if count > 0 {
        let c = pos2(rect.left() + 10.0 + text.size().x + 12.0, rect.center().y);
        let fill = if alarm {
            ERR().gamma_multiply(0.85)
        } else {
            BORDER_STRONG()
        };
        p.circle_filled(c, 8.0, fill);
        p.text(
            c,
            Align2::CENTER_CENTER,
            count.min(99).to_string(),
            font_medium(10.0),
            TEXT_STRONG(),
        );
    }
    if active {
        p.hline(
            (rect.left() + 8.0)..=(rect.right() - 8.0),
            rect.bottom() - 1.5,
            Stroke::new(1.0, ACCENT()),
        );
    }
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, active, label)
    });
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn problems(s: &mut Studio, ui: &mut Ui) {
    if s.issues.is_empty() {
        ui.horizontal(|ui| {
            widgets::icon(ui, icons::CIRCLE_CHECK, 14.0, OK());
            ui.label(RichText::new("No problems. The graph is ready to run.").color(TEXT_DIM()));
        });
        return;
    }
    let issues = s.issues.clone();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for i in issues {
                let (rect, resp) =
                    ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(rect, 0.0, HOVER());
                }
                widgets::paint_icon(
                    ui,
                    egui::Rect::from_center_size(
                        pos2(rect.left() + 10.0, rect.center().y),
                        egui::Vec2::splat(14.0),
                    ),
                    icons::CIRCLE_ALERT,
                    13.0,
                    ERR(),
                );
                let mut x = rect.left() + 24.0;
                if let Some(n) = &i.node {
                    let g = widgets::galley(ui, n.clone(), font_medium(12.5), TEXT_STRONG());
                    let w = g.size().x;
                    ui.painter().galley(
                        pos2(x, rect.center().y - g.size().y / 2.0),
                        g,
                        TEXT_STRONG(),
                    );
                    x += w + 10.0;
                }
                ui.painter().text(
                    pos2(x, rect.center().y),
                    Align2::LEFT_CENTER,
                    &i.message,
                    font(12.5),
                    TEXT(),
                );
                resp.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, true, i.to_string())
                });
                if resp
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    s.push(Cmd::Focus(i.node.clone()));
                }
            }
        });
}

fn watch(s: &mut Studio, ui: &mut Ui) {
    if s.watches.is_empty() {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            widgets::icon(ui, icons::SCAN_EYE, 26.0, TEXT_FAINT());
            ui.add_space(6.0);
            ui.label(
                RichText::new("Nothing watched yet")
                    .font(font_medium(13.0))
                    .color(TEXT_DIM()),
            );
            ui.label(
                RichText::new("Click the eye next to any output port to stream its values here.")
                    .color(TEXT_FAINT()),
            );
        });
        return;
    }
    let mut close = None;
    let h = (ui.available_height() - 4.0).max(100.0);
    egui::ScrollArea::horizontal()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                for w in &mut s.watches {
                    let width = w.width(h);
                    if w.show(ui, vec2(width, h)) {
                        close = Some(w.key());
                    }
                }
            });
        });
    if let Some((node, port)) = close {
        s.push(Cmd::Unwatch { node, port });
    }
}

fn output(s: &mut Studio, ui: &mut Ui) {
    let filter = s.log_filter.to_lowercase();
    let lines: Vec<_> = s
        .logs
        .iter()
        .filter(|l| {
            filter.is_empty()
                || l.message.to_lowercase().contains(&filter)
                || l.node.as_deref().is_some_and(|n| n.contains(&filter))
                || l.group.contains(&filter)
        })
        .collect();
    if lines.is_empty() {
        ui.label(
            RichText::new(if s.logs.is_empty() {
                "No output yet. Logs from running nodes appear here."
            } else {
                "No lines match the filter."
            })
            .color(TEXT_FAINT()),
        );
        return;
    }
    let row_h = 18.0;
    egui::ScrollArea::vertical()
        .stick_to_bottom(true)
        .auto_shrink([false, false])
        .show_rows(ui, row_h, lines.len(), |ui, range| {
            for l in &lines[range] {
                let (rect, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::hover());
                let (tag, color) = match l.level {
                    Level::Error => ("error", ERR()),
                    Level::Warn => ("warn ", WARN()),
                    Level::Info => ("info ", ACCENT_TEXT()),
                    Level::Debug => ("debug", TEXT_FAINT()),
                    Level::Trace => ("trace", TEXT_FAINT()),
                };
                let p = ui.painter();
                let y = rect.center().y;
                let mono = font_mono(12.0);
                p.text(
                    pos2(rect.left(), y),
                    Align2::LEFT_CENTER,
                    clock(l.ts_ns),
                    mono.clone(),
                    TEXT_FAINT(),
                );
                p.text(
                    pos2(rect.left() + 104.0, y),
                    Align2::LEFT_CENTER,
                    tag,
                    mono.clone(),
                    color,
                );
                let source = match &l.node {
                    Some(n) => format!("{}/{n}", l.group),
                    None => l.group.clone(),
                };
                p.text(
                    pos2(rect.left() + 152.0, y),
                    Align2::LEFT_CENTER,
                    widgets::truncate(&source, 18),
                    mono.clone(),
                    TEXT_DIM(),
                );
                let msg_color = if l.level >= Level::Warn {
                    color
                } else {
                    TEXT()
                };
                p.text(
                    pos2(rect.left() + 300.0, y),
                    Align2::LEFT_CENTER,
                    &l.message,
                    mono,
                    msg_color,
                );
            }
        });
}

fn clock(ts_ns: u64) -> String {
    let secs = (ts_ns / 1_000_000_000) as libc::time_t;
    let millis = (ts_ns / 1_000_000) % 1000;
    // SAFETY: localtime_r writes only into the provided struct.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&secs, &mut tm);
        tm
    };
    format!(
        "{:02}:{:02}:{:02}.{millis:03}",
        tm.tm_hour, tm.tm_min, tm.tm_sec
    )
}
