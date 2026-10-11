//! Primary sidebar: node library, graph outline and running processes.

use std::collections::BTreeMap;

use egui::{Align2, CornerRadius, Frame, Margin, Rect, RichText, Sense, Ui, Vec2, pos2, vec2};
use ruro_proto::{GroupState, NodeState};

use crate::app::{Activity, Cmd, Studio};
use crate::icons;
use crate::theme::*;
use crate::widgets::{self, Side};

pub fn show(s: &mut Studio, ui: &mut Ui) {
    egui::Panel::left("sidebar")
        .resizable(true)
        .default_size(264.0)
        .size_range(200.0..=480.0)
        .frame(Frame::new().fill(CHROME))
        .show(ui, |ui| {
            widgets::edge(ui, Side::Right);
            let title = match s.activity {
                Activity::Library => "Node Library",
                Activity::Outline => "Outline",
                Activity::Processes => "Processes",
            };
            Frame::new()
                .inner_margin(Margin {
                    left: 16,
                    right: 8,
                    top: 0,
                    bottom: 0,
                })
                .show(ui, |ui| {
                    ui.set_height(TABSTRIP_H);
                    ui.horizontal_centered(|ui| {
                        ui.label(
                            RichText::new(title.to_uppercase())
                                .font(font_semibold(11.0))
                                .color(TEXT_DIM),
                        );
                    });
                });
            match s.activity {
                Activity::Library => library(s, ui),
                Activity::Outline => outline(s, ui),
                Activity::Processes => processes(s, ui),
            }
        });
}

fn library(s: &mut Studio, ui: &mut Ui) {
    Frame::new()
        .inner_margin(Margin {
            left: 12,
            right: 12,
            top: 0,
            bottom: 8,
        })
        .show(ui, |ui| {
            widgets::search_field(
                ui,
                &mut s.library_filter,
                "Search nodes",
                ui.available_width(),
            );
        });
    let filter = s.library_filter.to_lowercase();
    let mut categories: BTreeMap<String, Vec<ruro_graph::NodeDescriptor>> = BTreeMap::new();
    for d in s.registry.iter() {
        if filter.is_empty()
            || d.kind.to_lowercase().contains(&filter)
            || d.doc.to_lowercase().contains(&filter)
        {
            categories
                .entry(d.category().to_owned())
                .or_default()
                .push(d.clone());
        }
    }
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if s.registry.nodes.is_empty() {
                empty(
                    ui,
                    "No node types found. Make sure ruro-host is installed next to the studio.",
                );
            } else if categories.is_empty() {
                empty(ui, "No nodes match your search.");
            }
            for (category, list) in categories {
                let key = format!("lib:{category}");
                let mut open = s.section_open(&key);
                Frame::new()
                    .inner_margin(Margin::symmetric(4, 0))
                    .show(ui, |ui| {
                        widgets::section_header(
                            ui,
                            &format!("{category}  ·  {}", list.len()),
                            Some(&mut open),
                        );
                    });
                if open != s.section_open(&key) {
                    s.toggle_section(&key);
                }
                if !open {
                    continue;
                }
                for d in list {
                    let resp = library_row(ui, &d);
                    if resp.clicked() {
                        s.push(Cmd::AddNode(d.kind.clone()));
                    }
                }
                ui.add_space(6.0);
            }
        });
}

fn library_row(ui: &mut Ui, d: &ruro_graph::NodeDescriptor) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(
            rect.shrink2(vec2(6.0, 1.0)),
            CornerRadius::same(RADIUS),
            HOVER,
        );
    }
    let accent = d
        .outputs
        .first()
        .or(d.inputs.first())
        .map_or(TEXT_DIM, |p| type_color(&p.ty));
    let tile = Rect::from_center_size(pos2(rect.left() + 28.0, rect.center().y), Vec2::splat(28.0));
    p.rect_filled(tile, CornerRadius::same(RADIUS), CARD_HEADER);
    widgets::paint_icon(ui, tile, kind_icon(&d.kind), 15.0, accent);
    let x = tile.right() + 10.0;
    p.text(
        pos2(x, rect.top() + 14.0),
        Align2::LEFT_CENTER,
        d.short_name(),
        font_medium(13.0),
        TEXT_STRONG,
    );
    let doc = widgets::truncate(&d.doc, 34);
    p.text(
        pos2(x, rect.top() + 30.0),
        Align2::LEFT_CENTER,
        doc,
        font(11.5),
        TEXT_FAINT,
    );
    let mut summary = String::new();
    for (dir, ports) in [("in", &d.inputs), ("out", &d.outputs)] {
        for port in ports.iter() {
            summary.push_str(&format!("{dir} {}: {}\n", port.name, port.ty));
        }
    }
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            true,
            format!("Add {}", d.short_name()),
        )
    });
    resp.on_hover_ui(|ui| {
        ui.label(
            RichText::new(&d.kind)
                .font(font_mono(12.0))
                .color(ACCENT_TEXT),
        );
        ui.label(&d.doc);
        if !summary.is_empty() {
            ui.label(
                RichText::new(summary.trim_end())
                    .font(font_mono(11.5))
                    .color(TEXT_DIM),
            );
        }
        ui.label(
            RichText::new("Click to add to the graph")
                .small()
                .color(TEXT_FAINT),
        );
    })
    .on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn outline(s: &mut Studio, ui: &mut Ui) {
    let graph = s.doc.to_graph(&s.registry);
    let groups = graph.groups();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if graph.nodes.is_empty() {
                empty(ui, "The graph is empty. Add nodes from the library.");
                return;
            }
            let focused_id = s.focused.map(|id| s.doc.snarl[id].id.clone());
            for (group, ids) in groups {
                let key = format!("outline:{group}");
                let mut open = s.section_open(&key);
                Frame::new()
                    .inner_margin(Margin::symmetric(4, 0))
                    .show(ui, |ui| {
                        widgets::section_header(ui, &format!("process {group}"), Some(&mut open));
                    });
                if open != s.section_open(&key) {
                    s.toggle_section(&key);
                }
                if !open {
                    continue;
                }
                for id in ids {
                    let inst = &graph.nodes[&id];
                    let color = match s.metrics.get(&id).filter(|_| s.live()).map(|m| m.state) {
                        Some(NodeState::Running) => OK,
                        Some(NodeState::Failed) => ERR,
                        Some(_) => WARN,
                        None => TEXT_DIM,
                    };
                    let short = inst
                        .kind
                        .rsplit_once('/')
                        .map_or(inst.kind.as_str(), |(_, n)| n);
                    let selected = focused_id.as_deref() == Some(id.as_str());
                    if widgets::list_row(ui, kind_icon(&inst.kind), color, &id, short, selected)
                        .clicked()
                    {
                        s.push(Cmd::Focus(Some(id.clone())));
                    }
                }
                ui.add_space(4.0);
            }
        });
}

fn processes(s: &mut Studio, ui: &mut Ui) {
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let Some(status) = s.status.clone() else {
                empty(ui, "Not connected to a daemon.");
                return;
            };
            Frame::new()
                .inner_margin(Margin::symmetric(16, 4))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(format!("daemon {}  ·  pid {}", status.name, status.pid))
                            .font(font(12.0))
                            .color(TEXT_DIM),
                    );
                    match &status.graph {
                        Some(g) => ui.label(
                            RichText::new(format!("running {g}"))
                                .font(font(12.0))
                                .color(TEXT_DIM),
                        ),
                        None => ui.label(
                            RichText::new("no graph running")
                                .font(font(12.0))
                                .color(TEXT_FAINT),
                        ),
                    };
                });
            ui.add_space(6.0);
            for g in &status.groups {
                let (color, state) = match g.state {
                    GroupState::Running => (OK, "running"),
                    GroupState::Backoff => (WARN, "restarting"),
                    GroupState::Failed => (ERR, "failed"),
                    GroupState::Stopped => (TEXT_FAINT, "stopped"),
                };
                Frame::new()
                    .fill(CARD)
                    .stroke(egui::Stroke::new(1.0, BORDER))
                    .corner_radius(CornerRadius::same(RADIUS + 1))
                    .inner_margin(Margin::same(10))
                    .outer_margin(Margin::symmetric(12, 4))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            widgets::icon(ui, icons::SERVER, 14.0, color);
                            ui.label(
                                RichText::new(&g.name)
                                    .font(font_medium(13.0))
                                    .color(TEXT_STRONG),
                            );
                            ui.label(RichText::new(state).font(font(11.5)).color(color));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if g.pid.is_some()
                                        && widgets::icon_button(
                                            ui,
                                            icons::SKULL,
                                            "Kill this process (SIGKILL) to test supervision",
                                        )
                                        .clicked()
                                    {
                                        s.push(Cmd::Kill(g.name.clone()));
                                    }
                                },
                            );
                        });
                        let cpu: f64 = g
                            .nodes
                            .iter()
                            .filter_map(|n| s.metrics.get(n))
                            .map(|m| m.cpu_pct)
                            .sum();
                        ui.horizontal(|ui| {
                            let pid = g.pid.map_or("—".to_owned(), |p| p.to_string());
                            ui.label(
                                RichText::new(format!("pid {pid}"))
                                    .font(font_mono(11.5))
                                    .color(TEXT_DIM),
                            );
                            ui.label(
                                RichText::new(format!("{cpu:.0}% cpu"))
                                    .font(font_mono(11.5))
                                    .color(TEXT_DIM),
                            );
                            if g.restarts > 0 {
                                ui.label(
                                    RichText::new(format!("{} {}", icons::ROTATE_CW, g.restarts))
                                        .font(font_mono(11.5))
                                        .color(WARN),
                                )
                                .on_hover_text("Restarts since deploy");
                            }
                        });
                        if let Some(exit) = &g.last_exit {
                            ui.label(
                                RichText::new(format!("last exit: {exit}"))
                                    .font(font(11.5))
                                    .color(TEXT_FAINT),
                            );
                        }
                        ui.add_space(2.0);
                        for n in &g.nodes {
                            ui.horizontal(|ui| {
                                let m = s.metrics.get(n).cloned();
                                let c = match m.as_ref().map(|m| m.state) {
                                    Some(NodeState::Running) => OK,
                                    Some(NodeState::Failed) => ERR,
                                    _ => TEXT_FAINT,
                                };
                                widgets::dot(ui, c, 3.0);
                                if ui
                                    .add(
                                        egui::Label::new(RichText::new(n).color(TEXT))
                                            .sense(Sense::click()),
                                    )
                                    .clicked()
                                {
                                    s.push(Cmd::Focus(Some(n.clone())));
                                }
                                if let Some(m) = &m {
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            ui.label(
                                                RichText::new(format!("{:.1} Hz", m.hz))
                                                    .font(font_mono(11.5))
                                                    .color(TEXT_DIM),
                                            );
                                        },
                                    );
                                }
                            });
                        }
                    });
            }
        });
}

fn empty(ui: &mut Ui, text: &str) {
    Frame::new()
        .inner_margin(Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(text).color(TEXT_FAINT)).wrap());
        });
}
