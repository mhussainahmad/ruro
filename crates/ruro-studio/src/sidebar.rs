//! Primary sidebar: node library, graph outline and running processes.

use std::collections::BTreeMap;

use egui::{Align2, CornerRadius, Frame, Margin, Rect, RichText, Sense, Ui, Vec2, pos2, vec2};
use ruro_proto::{GroupState, NodeState};

use crate::app::{Activity, Cmd, Studio};
use crate::icons;
use crate::theme::*;
use crate::widgets::{self, Side};

/// Draws the sidebar. Returns its width.
pub fn show(s: &mut Studio, ui: &mut Ui) -> f32 {
    egui::Panel::left("sidebar")
        .resizable(true)
        .default_size(s.settings.sidebar_width.unwrap_or(264.0))
        .size_range(200.0..=480.0)
        .frame(Frame::new().fill(CHROME()))
        .show(ui, |ui| {
            widgets::edge(ui, Side::Right);
            let title = match s.activity {
                Activity::Library => "Node Library",
                Activity::Outline => "Outline",
                Activity::Processes => "Processes",
                Activity::Fleet => "Fleet",
                Activity::Changes => "Changes",
                Activity::Ros2 => "ROS 2 Topics",
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
                                .color(TEXT_DIM()),
                        );
                    });
                });
            match s.activity {
                Activity::Library => library(s, ui),
                Activity::Outline => outline(s, ui),
                Activity::Processes => processes(s, ui),
                Activity::Fleet => fleet(s, ui),
                Activity::Changes => changes(s, ui),
                Activity::Ros2 => crate::ros::show(s, ui),
            }
        })
        .response
        .rect
        .width()
}

/// Every daemon seen on the network, with a way to switch to it.
fn fleet(s: &mut Studio, ui: &mut Ui) {
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if s.fleet.is_empty() {
                empty(
                    ui,
                    "No daemons seen yet. Daemons announce themselves once the studio is connected to one of them.",
                );
                return;
            }
            let current = s.daemon_name().map(str::to_owned);
            let entries: Vec<_> = s.fleet.values().cloned().collect();
            for e in entries {
                let st = &e.status;
                let reachable = e.seen.elapsed() < crate::app::FLEET_STALE;
                let connected = current.as_deref() == Some(st.name.as_str());
                let (dot, state) = match (reachable, &st.graph) {
                    (false, _) => (TEXT_FAINT(), "unreachable".to_owned()),
                    (true, Some(g)) => (OK(), format!("running {g}")),
                    (true, None) => (TEXT_DIM(), "idle".to_owned()),
                };
                let stroke = if connected { ACCENT() } else { BORDER() };
                Frame::new()
                    .fill(CARD())
                    .stroke(egui::Stroke::new(1.0, stroke))
                    .corner_radius(CornerRadius::same(RADIUS + 1))
                    .inner_margin(Margin::same(10))
                    .outer_margin(Margin::symmetric(12, 4))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            widgets::icon(ui, icons::SERVER, 14.0, if reachable { TEXT() } else { TEXT_FAINT() });
                            ui.label(
                                RichText::new(&st.name)
                                    .font(font_medium(13.0))
                                    .color(TEXT_STRONG()),
                            );
                            widgets::dot(ui, dot, 3.5);
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if connected {
                                        ui.label(
                                            RichText::new("connected")
                                                .font(font(11.5))
                                                .color(ACCENT_TEXT()),
                                        );
                                    } else if reachable
                                        && widgets::button(ui, None, "Connect", widgets::ButtonKind::Secondary)
                                            .on_hover_text(format!("Use daemon `{}` for running and inspecting", st.name))
                                            .clicked()
                                    {
                                        s.push(Cmd::ConnectDaemon(st.name.clone()));
                                    }
                                },
                            );
                        });
                        let groups = st.groups.len();
                        let running = st
                            .groups
                            .iter()
                            .filter(|g| g.state == GroupState::Running)
                            .count();
                        let state = if groups > 0 {
                            format!("{state} · {running}/{groups} processes")
                        } else {
                            state
                        };
                        ui.label(RichText::new(state).font(font(12.0)).color(TEXT_DIM()));
                        ui.label(
                            RichText::new(format!("v{} · up {}", st.version, uptime(st.uptime_s)))
                                .font(font_mono(11.0))
                                .color(TEXT_FAINT()),
                        );
                        if st.recording.is_some() {
                            ui.horizontal(|ui| {
                                widgets::dot(ui, ERR(), 3.0);
                                ui.label(RichText::new("recording").font(font(11.5)).color(ERR()));
                            });
                        }
                    });
            }
            Frame::new()
                .inner_margin(Margin::symmetric(16, 8))
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(
                                "Place a process on another machine with the Machine setting in the inspector.",
                            )
                            .font(font(11.5))
                            .color(TEXT_FAINT()),
                        )
                        .wrap(),
                    );
                });
        });
}

fn uptime(secs: f64) -> String {
    let s = secs as u64;
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86399 => format!("{}h {}m", s / 3600, (s % 3600) / 60),
        _ => format!("{}d {}h", s / 86400, (s % 86400) / 3600),
    }
}

/// Differences between the open graph and the version it is compared with.
fn changes(s: &mut Studio, ui: &mut Ui) {
    let Some(c) = s.compare.as_ref().filter(|c| c.uid == s.doc.uid) else {
        Frame::new()
            .inner_margin(Margin::symmetric(16, 4))
            .show(ui, |ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new("Compare the graph with its saved version or another file to review what changed.")
                            .color(TEXT_FAINT()),
                    )
                    .wrap(),
                );
                ui.add_space(10.0);
                ui.add_enabled_ui(s.doc.path.is_some(), |ui| {
                    if widgets::button(ui, Some(icons::COMPARE), "Compare with saved", widgets::ButtonKind::Secondary)
                        .clicked()
                    {
                        s.push(Cmd::CompareWithSaved);
                    }
                });
                ui.add_space(6.0);
                if widgets::button(ui, Some(icons::FILE_DIFF), "Compare with file…", widgets::ButtonKind::Secondary)
                    .clicked()
                {
                    s.push(Cmd::CompareDialog);
                }
            });
        return;
    };
    let d = c.diff.clone();
    let label = c.label.clone();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            Frame::new()
                .inner_margin(Margin::symmetric(16, 2))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(format!("Against {label}"))
                            .font(font(12.0))
                            .color(TEXT_DIM()),
                    );
                });
            if d.is_empty() {
                empty(ui, "No differences.");
                return;
            }
            let section = |ui: &mut Ui, title: &str, n: usize| {
                if n > 0 {
                    Frame::new()
                        .inner_margin(Margin::symmetric(4, 0))
                        .show(ui, |ui| {
                            widgets::section_header(ui, &format!("{title}  ·  {n}"), None);
                        });
                }
            };
            section(ui, "Added", d.added.len());
            for id in &d.added {
                if widgets::list_row(ui, icons::PLUS, OK(), id, "added", false).clicked() {
                    s.push(Cmd::Focus(Some(id.clone())));
                }
            }
            section(ui, "Changed", d.changed.len());
            for (id, what) in &d.changed {
                if widgets::list_row(ui, icons::CIRCLE_DOT, WARN(), id, "", false)
                    .on_hover_text(what.join("\n"))
                    .clicked()
                {
                    s.push(Cmd::Focus(Some(id.clone())));
                }
                for w in what {
                    Frame::new()
                        .inner_margin(Margin {
                            left: 34,
                            right: 8,
                            top: 0,
                            bottom: 2,
                        })
                        .show(ui, |ui| {
                            ui.label(RichText::new(w).font(font_mono(11.5)).color(TEXT_DIM()));
                        });
                }
            }
            section(ui, "Removed", d.removed.len());
            for (id, n) in &d.removed {
                let short = n.kind.rsplit('/').next().unwrap_or("");
                let trailing = if short.replace('-', "_") == *id {
                    ""
                } else {
                    short
                };
                widgets::list_row(ui, icons::MINUS, ERR(), id, trailing, false);
            }
            let wires = d.wires_added.len() + d.wires_removed.len();
            section(ui, "Wires", wires);
            for w in &d.wires_added {
                widgets::list_row(ui, icons::PLUS, OK(), &w.to_string(), "", false);
            }
            for w in &d.wires_removed {
                widgets::list_row(ui, icons::MINUS, ERR(), &w.to_string(), "", false);
            }
            section(ui, "Graph", d.graph.len());
            for g in &d.graph {
                widgets::list_row(ui, icons::INFO, WARN(), g, "", false);
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
            HOVER(),
        );
    }
    let accent = d
        .outputs
        .first()
        .or(d.inputs.first())
        .map_or(TEXT_DIM(), |p| type_color(&p.ty));
    let tile = Rect::from_center_size(pos2(rect.left() + 28.0, rect.center().y), Vec2::splat(28.0));
    p.rect_filled(tile, CornerRadius::same(RADIUS), CARD_HEADER());
    widgets::paint_icon(ui, tile, kind_icon(&d.kind), 15.0, accent);
    let x = tile.right() + 10.0;
    p.text(
        pos2(x, rect.top() + 14.0),
        Align2::LEFT_CENTER,
        d.short_name(),
        font_medium(13.0),
        TEXT_STRONG(),
    );
    let doc = widgets::truncate(&d.doc, 34);
    p.text(
        pos2(x, rect.top() + 30.0),
        Align2::LEFT_CENTER,
        doc,
        font(11.5),
        TEXT_FAINT(),
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
                .color(ACCENT_TEXT()),
        );
        ui.label(&d.doc);
        if !summary.is_empty() {
            ui.label(
                RichText::new(summary.trim_end())
                    .font(font_mono(11.5))
                    .color(TEXT_DIM()),
            );
        }
        ui.label(
            RichText::new("Click to add to the graph")
                .small()
                .color(TEXT_FAINT()),
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
                        Some(NodeState::Running) => OK(),
                        Some(NodeState::Failed) => ERR(),
                        Some(_) => WARN(),
                        None => TEXT_DIM(),
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
                            .color(TEXT_DIM()),
                    );
                    match &status.graph {
                        Some(g) => ui.label(
                            RichText::new(format!("running {g}"))
                                .font(font(12.0))
                                .color(TEXT_DIM()),
                        ),
                        None => ui.label(
                            RichText::new("no graph running")
                                .font(font(12.0))
                                .color(TEXT_FAINT()),
                        ),
                    };
                });
            ui.add_space(6.0);
            for g in &status.groups {
                let (color, state) = match g.state {
                    GroupState::Running => (OK(), "running"),
                    GroupState::Backoff => (WARN(), "restarting"),
                    GroupState::Failed => (ERR(), "failed"),
                    GroupState::Stopped => (TEXT_FAINT(), "stopped"),
                };
                Frame::new()
                    .fill(CARD())
                    .stroke(egui::Stroke::new(1.0, BORDER()))
                    .corner_radius(CornerRadius::same(RADIUS + 1))
                    .inner_margin(Margin::same(10))
                    .outer_margin(Margin::symmetric(12, 4))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            widgets::icon(ui, icons::SERVER, 14.0, color);
                            ui.label(
                                RichText::new(&g.name)
                                    .font(font_medium(13.0))
                                    .color(TEXT_STRONG()),
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
                                    .color(TEXT_DIM()),
                            );
                            ui.label(
                                RichText::new(format!("{cpu:.0}% cpu"))
                                    .font(font_mono(11.5))
                                    .color(TEXT_DIM()),
                            );
                            if g.restarts > 0 {
                                ui.label(
                                    RichText::new(format!("{} {}", icons::ROTATE_CW, g.restarts))
                                        .font(font_mono(11.5))
                                        .color(WARN()),
                                )
                                .on_hover_text("Restarts since deploy");
                            }
                        });
                        if let Some(exit) = &g.last_exit {
                            ui.label(
                                RichText::new(format!("last exit: {exit}"))
                                    .font(font(11.5))
                                    .color(TEXT_FAINT()),
                            );
                        }
                        ui.add_space(2.0);
                        for n in &g.nodes {
                            ui.horizontal(|ui| {
                                let m = s.metrics.get(n).cloned();
                                let c = match m.as_ref().map(|m| m.state) {
                                    Some(NodeState::Running) => OK(),
                                    Some(NodeState::Failed) => ERR(),
                                    _ => TEXT_FAINT(),
                                };
                                widgets::dot(ui, c, 3.0);
                                if ui
                                    .add(
                                        egui::Label::new(RichText::new(n).color(TEXT()))
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
                                                    .color(TEXT_DIM()),
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
            ui.add(egui::Label::new(RichText::new(text).color(TEXT_FAINT())).wrap());
        });
}
