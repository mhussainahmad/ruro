//! Secondary sidebar: properties of the focused node, or of the graph.

use egui::{Frame, Margin, RichText, Ui};
use egui_snarl::NodeId;
use ruro_graph::{ParamDesc, ParamKind, RestartPolicy};
use ruro_proto::NodeState;
use serde_json::Value;

use crate::app::{Cmd, PanelTab, Studio};
use crate::icons;
use crate::theme::*;
use crate::widgets::{self, Side};

pub fn show(s: &mut Studio, ui: &mut Ui) {
    egui::Panel::right("inspector")
        .resizable(true)
        .default_size(312.0)
        .size_range(260.0..=520.0)
        .frame(Frame::new().fill(CHROME))
        .show(ui, |ui| {
            widgets::edge(ui, Side::Left);
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
                            RichText::new("INSPECTOR")
                                .font(font_semibold(11.0))
                                .color(TEXT_DIM),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if widgets::icon_button(
                                ui,
                                icons::PANEL_RIGHT,
                                "Hide inspector (Ctrl+I)",
                            )
                            .clicked()
                            {
                                s.push(Cmd::ToggleInspector);
                            }
                        });
                    });
                });
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    Frame::new()
                        .inner_margin(Margin {
                            left: 16,
                            right: 14,
                            top: 2,
                            bottom: 16,
                        })
                        .show(ui, |ui| {
                            match s.focused.filter(|&id| s.doc.snarl.get_node(id).is_some()) {
                                Some(id) => node(s, ui, id),
                                None => graph(s, ui),
                            }
                        });
                });
        });
}

fn section(s: &mut Studio, ui: &mut Ui, title: &str, add: impl FnOnce(&mut Studio, &mut Ui)) {
    ui.add_space(6.0);
    let mut open = s.section_open(title);
    widgets::section_header(ui, title, Some(&mut open));
    if open != s.section_open(title) {
        s.toggle_section(title);
    }
    if open {
        ui.add_space(2.0);
        add(s, ui);
    }
    ui.add_space(6.0);
    widgets::hairline(ui);
}

fn graph(s: &mut Studio, ui: &mut Ui) {
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        widgets::icon(ui, icons::WORKFLOW, 16.0, ACCENT_TEXT);
        ui.label(
            RichText::new("Graph")
                .font(font_semibold(15.0))
                .color(TEXT_STRONG),
        );
    });
    ui.add_space(8.0);
    widgets::hairline(ui);
    section(s, ui, "Properties", |s, ui| {
        widgets::prop_row(ui, "Name", "Used in shared-memory topic names", |ui| {
            let mut name = s.doc.name.clone();
            if widgets::text_field(ui, &mut name, "graph name").changed() {
                let clean: String = name
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                    .collect();
                if clean != s.doc.name {
                    s.doc.name = clean;
                    s.doc.touch_structure();
                }
            }
        });
        widgets::prop_row(ui, "File", "", |ui| {
            let path = s
                .doc
                .path
                .as_ref()
                .map_or("not saved".to_owned(), |p| p.display().to_string());
            ui.label(RichText::new(widgets::truncate(&path, 34)).color(TEXT_DIM))
                .on_hover_text(path);
        });
        let graph = s.doc.to_graph(&s.registry);
        widgets::prop_row(ui, "Nodes", "", |ui| {
            ui.label(graph.nodes.len().to_string())
        });
        widgets::prop_row(ui, "Wires", "", |ui| {
            ui.label(graph.wires.len().to_string())
        });
        widgets::prop_row(
            ui,
            "Processes",
            "Each process group runs as its own OS process",
            |ui| {
                ui.label(
                    graph
                        .groups()
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", "),
                )
            },
        );
    });
    if !s.issues.is_empty() {
        let n = s.issues.len();
        section(s, ui, "Problems", |s, ui| {
            ui.horizontal(|ui| {
                widgets::icon(ui, icons::CIRCLE_ALERT, 14.0, ERR);
                if ui
                    .link(format!(
                        "{n} problem{} block running",
                        if n == 1 { "" } else { "s" }
                    ))
                    .clicked()
                {
                    s.push(Cmd::ShowPanel(PanelTab::Problems));
                }
            });
        });
    }
    section(s, ui, "Getting started", |_, ui| {
        for (glyph, text) in [
            (
                icons::BLOCKS,
                "Click a node in the library, or right-click the canvas, to add it.",
            ),
            (
                icons::CABLE,
                "Drag from an output port to an input of the same type to connect.",
            ),
            (
                icons::EYE,
                "Use the eye next to an output to watch its live values.",
            ),
            (
                icons::LAYERS,
                "Give nodes a process name to isolate them; same process means zero-copy.",
            ),
        ] {
            ui.horizontal_top(|ui| {
                widgets::icon(ui, glyph, 13.0, TEXT_FAINT);
                ui.add(
                    egui::Label::new(RichText::new(text).font(font(12.0)).color(TEXT_DIM)).wrap(),
                );
            });
            ui.add_space(4.0);
        }
    });
}

fn node(s: &mut Studio, ui: &mut Ui, id: NodeId) {
    let n = s.doc.snarl[id].clone();
    let desc = s.registry.get(&n.kind).cloned();
    let live_metrics = s.metrics.get(&n.id).filter(|_| s.live()).cloned();

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        let tint = desc
            .as_ref()
            .and_then(|d| d.outputs.first().or(d.inputs.first()))
            .map_or(TEXT_DIM, |p| type_color(&p.ty));
        widgets::icon(ui, kind_icon(&n.kind), 16.0, tint);
        let buf = match &mut s.rename {
            Some((nid, buf)) if *nid == id => buf,
            _ => {
                s.rename = Some((id, n.id.clone()));
                &mut s.rename.as_mut().unwrap().1
            }
        };
        let resp = ui.add(
            egui::TextEdit::singleline(buf)
                .font(font_semibold(15.0))
                .frame(Frame::NONE)
                .desired_width(ui.available_width() - 30.0),
        );
        if let Some(m) = &live_metrics {
            let c = match m.state {
                NodeState::Running => OK,
                NodeState::Failed => ERR,
                _ => WARN,
            };
            widgets::dot(ui, c, 4.0).on_hover_text(format!("{:?}", m.state).to_lowercase());
        }
        if resp.lost_focus() {
            let new = buf.trim().to_owned();
            s.rename = None;
            if new != n.id {
                rename(s, id, &n.id, new);
            }
        }
    });
    ui.label(
        RichText::new(&n.kind)
            .font(font_mono(11.5))
            .color(ACCENT_TEXT),
    );
    if let Some(d) = &desc {
        ui.add(egui::Label::new(RichText::new(&d.doc).font(font(12.0)).color(TEXT_DIM)).wrap());
    } else {
        ui.label(
            RichText::new("This node type is not available on the connected host.").color(ERR),
        );
    }
    ui.add_space(8.0);
    widgets::hairline(ui);

    section(s, ui, "Execution", |s, ui| {
        widgets::prop_row(
            ui,
            "Process",
            "Nodes in one process share memory (zero-copy); separate processes isolate crashes",
            |ui| {
                let mut group = n.group.clone().unwrap_or_default();
                if widgets::text_field(ui, &mut group, ruro_graph::DEFAULT_GROUP).changed() {
                    let clean: String = group
                        .chars()
                        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                        .collect();
                    s.doc.snarl[id].group = (!clean.is_empty()).then_some(clean);
                    s.doc.touch_structure();
                }
            },
        );
        let default_rate = desc.as_ref().and_then(|d| d.default_rate);
        let is_source = desc.as_ref().is_some_and(|d| d.inputs.is_empty());
        widgets::prop_row(
            ui,
            "Trigger",
            "Sources run on a timer; other nodes run when an input receives data unless given a rate",
            |ui| {
                let mut timer = n.rate.is_some() || is_source;
                if ui
                    .add_enabled(!is_source, egui::Checkbox::without_text(&mut timer))
                    .changed()
                {
                    s.doc.snarl[id].rate = timer.then(|| default_rate.unwrap_or(30.0));
                    s.doc.touch_structure();
                }
                if timer {
                    let mut hz = n.rate.or(default_rate).unwrap_or(30.0);
                    if ui
                        .add(
                            egui::DragValue::new(&mut hz)
                                .range(0.1..=10_000.0)
                                .speed(1.0)
                                .suffix(" Hz"),
                        )
                        .changed()
                    {
                        s.doc.snarl[id].rate = Some(hz);
                        s.doc.touch_structure();
                    }
                } else {
                    ui.label(RichText::new("on input").color(TEXT_DIM));
                }
            },
        );
        widgets::prop_row(
            ui,
            "Restart",
            "What the supervisor does when this node's process exits",
            |ui| {
                let current = n.restart.unwrap_or_default();
                egui::ComboBox::from_id_salt("restart")
                    .selected_text(policy_name(current))
                    .width(ui.available_width())
                    .show_ui(ui, |ui| {
                        for p in [
                            RestartPolicy::OnFailure,
                            RestartPolicy::Always,
                            RestartPolicy::Never,
                        ] {
                            if ui.selectable_label(current == p, policy_name(p)).clicked() {
                                s.doc.snarl[id].restart = Some(p);
                                s.doc.touch_structure();
                            }
                        }
                    });
            },
        );
    });

    if let Some(d) = desc.as_ref().filter(|d| !d.params.is_empty()) {
        let live_params = live_metrics.is_some();
        section(s, ui, "Parameters", |s, ui| {
            if live_params {
                ui.horizontal(|ui| {
                    widgets::icon(ui, icons::ZAP, 12.0, OK);
                    ui.label(
                        RichText::new("Changes apply to the running node instantly")
                            .font(font(11.5))
                            .color(TEXT_DIM),
                    );
                });
                ui.add_space(2.0);
            }
            for p in &d.params {
                let current = n.param(p).clone();
                let hint = if p.doc.is_empty() {
                    format!("default {}", p.default)
                } else {
                    format!("{}\ndefault {}", p.doc, p.default)
                };
                let changed = widgets::prop_row(ui, &p.name, &hint, |ui| {
                    let new = param_widget(ui, p, &current);
                    let reset = current != p.default
                        && widgets::icon_button_sized(
                            ui,
                            icons::ROTATE_CW,
                            &format!("Reset to {}", p.default),
                            20.0,
                            12.0,
                            false,
                        )
                        .clicked();
                    if reset { Some(p.default.clone()) } else { new }
                });
                if let Some(v) = changed {
                    s.set_param(id, &p.name, v);
                }
            }
        });
    }

    if let Some(d) = desc.clone() {
        section(s, ui, "Ports", |s, ui| {
            for (dir, ports) in [("in", &d.inputs), ("out", &d.outputs)] {
                for p in ports.iter() {
                    ui.horizontal(|ui| {
                        ui.set_min_height(24.0);
                        widgets::dot(ui, type_color(&p.ty), 4.0);
                        ui.label(RichText::new(&p.name).color(TEXT));
                        ui.label(
                            RichText::new(format!("{dir} · {}", p.ty))
                                .font(font_mono(11.0))
                                .color(TEXT_FAINT),
                        );
                        if dir == "out" {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let watched = s.is_watched(&n.id, &p.name);
                                    let glyph = if watched { icons::EYE } else { icons::EYE_OFF };
                                    let tip = if watched {
                                        "Stop watching"
                                    } else {
                                        "Watch live values"
                                    };
                                    if widgets::icon_button_sized(
                                        ui, glyph, tip, 22.0, 13.0, watched,
                                    )
                                    .clicked()
                                    {
                                        s.push(if watched {
                                            Cmd::Unwatch {
                                                node: n.id.clone(),
                                                port: p.name.clone(),
                                            }
                                        } else {
                                            Cmd::Watch {
                                                node: n.id.clone(),
                                                port: p.name.clone(),
                                                ty: p.ty.clone(),
                                            }
                                        });
                                    }
                                },
                            );
                        }
                    });
                }
            }
        });
    }

    if let Some(m) = live_metrics {
        section(s, ui, "Runtime", |_, ui| {
            let row = |ui: &mut Ui, k: &str, v: String| {
                widgets::prop_row(ui, k, "", |ui| {
                    ui.label(RichText::new(v).font(font_mono(12.0)).color(TEXT))
                });
            };
            row(ui, "State", format!("{:?}", m.state).to_lowercase());
            row(ui, "Rate", format!("{:.1} Hz", m.hz));
            row(ui, "Step p50", widgets::fmt_us(m.step_p50_us));
            row(ui, "Step p99", widgets::fmt_us(m.step_p99_us));
            row(ui, "CPU", format!("{:.1} %", m.cpu_pct));
            if let Some(age) = m.input_age_ms {
                row(ui, "Input age", widgets::fmt_us(age * 1000.0));
            }
            row(ui, "Dropped", m.dropped.to_string());
            row(ui, "Errors", m.errors.to_string());
            if let Some(e) = &m.last_error {
                ui.add_space(2.0);
                let color = if m.state == NodeState::Failed {
                    ERR
                } else {
                    WARN
                };
                ui.add(
                    egui::Label::new(RichText::new(e).font(font_mono(11.5)).color(color)).wrap(),
                );
            }
        });
    }

    ui.add_space(10.0);
    if widgets::button(
        ui,
        Some(icons::TRASH_2),
        "Delete node",
        widgets::ButtonKind::Danger,
    )
    .clicked()
    {
        s.push(Cmd::DeleteFocused);
    }
}

fn rename(s: &mut Studio, id: NodeId, old: &str, new: String) {
    if !ruro_graph::is_valid_ident(&new) {
        s.toast(
            crate::app::Severity::Error,
            "Node names may only contain letters, digits, - and _",
        );
    } else if s.doc.find(&new).is_some() {
        s.toast(
            crate::app::Severity::Error,
            format!("A node named {new} already exists"),
        );
    } else {
        for w in &mut s.watches {
            if w.node == old {
                w.node = new.clone();
            }
        }
        s.doc.snarl[id].id = new;
        s.doc.touch_structure();
    }
}

fn policy_name(p: RestartPolicy) -> &'static str {
    match p {
        RestartPolicy::OnFailure => "On failure",
        RestartPolicy::Always => "Always",
        RestartPolicy::Never => "Never",
    }
}

/// Editor for one parameter. Returns the new value if it changed.
fn param_widget(ui: &mut Ui, p: &ParamDesc, current: &Value) -> Option<Value> {
    ui.spacing_mut().slider_width = (ui.available_width() - 86.0).max(60.0);
    match p.kind {
        ParamKind::Int => {
            let mut v = current.as_i64().unwrap_or(0);
            let changed = match (p.min, p.max) {
                (Some(a), Some(b)) => ui
                    .add(egui::Slider::new(&mut v, a as i64..=b as i64))
                    .changed(),
                _ => ui.add(egui::DragValue::new(&mut v)).changed(),
            };
            changed.then(|| Value::from(v))
        }
        ParamKind::Float => {
            let mut v = current.as_f64().unwrap_or(0.0);
            let changed = match (p.min, p.max) {
                (Some(a), Some(b)) => ui
                    .add(
                        egui::Slider::new(&mut v, a..=b)
                            .logarithmic(b - a > 100.0 && a >= 0.0)
                            .max_decimals(3),
                    )
                    .changed(),
                _ => ui.add(egui::DragValue::new(&mut v).speed(0.01)).changed(),
            };
            changed.then(|| Value::from(v))
        }
        ParamKind::Bool => {
            let mut v = current.as_bool().unwrap_or(false);
            ui.add(egui::Checkbox::without_text(&mut v))
                .changed()
                .then(|| Value::Bool(v))
        }
        ParamKind::String => {
            let mut v = current.as_str().unwrap_or_default().to_owned();
            widgets::text_field(ui, &mut v, "")
                .changed()
                .then(|| Value::String(v))
        }
    }
}
