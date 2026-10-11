//! Window chrome: title bar, activity bar, editor tabs, status bar,
//! welcome page and notifications.

use std::time::Instant;

use egui::{
    Align, Align2, Color32, CornerRadius, Frame, Layout, Margin, Rect, RichText, Sense, Stroke, Ui,
    Vec2, pos2, vec2,
};
use ruro_proto::{GroupState, NodeState};

use crate::app::{Activity, Cmd, Conn, PanelTab, Severity, Studio, example_files};
use crate::icons;
use crate::theme::*;
use crate::widgets::{self, ButtonKind, Side};

pub fn title_bar(s: &mut Studio, ui: &mut Ui) {
    egui::Panel::top("titlebar")
        .exact_size(TITLEBAR_H)
        .frame(
            Frame::new()
                .fill(CHROME)
                .inner_margin(Margin::symmetric(8, 0)),
        )
        .show(ui, |ui| {
            widgets::edge(ui, Side::Bottom);
            let full = ui.max_rect();
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                logo(ui);
                ui.add_space(8.0);
                menus(s, ui);
            });

            // Command center, centred in the window like VS Code's.
            let w = 440.0_f32.min(full.width() * 0.4);
            let rect = Rect::from_center_size(full.center(), vec2(w, 24.0));
            let resp = ui.interact(rect, ui.id().with("command-center"), Sense::click());
            let fill = if resp.hovered() {
                Color32::from_rgb(0x2a, 0x2a, 0x2a)
            } else {
                Color32::from_rgb(0x22, 0x22, 0x22)
            };
            ui.painter().rect(
                rect,
                CornerRadius::same(RADIUS + 1),
                fill,
                Stroke::new(1.0, BORDER_STRONG),
                egui::StrokeKind::Inside,
            );
            let label = match &s.doc.path {
                Some(p) => format!(
                    "{}  ·  {}",
                    s.doc.name,
                    p.file_name().unwrap_or_default().to_string_lossy()
                ),
                None => s.doc.name.clone(),
            };
            ui.painter().text(
                rect.center() - vec2(8.0, 0.0),
                Align2::CENTER_CENTER,
                format!("{}  {label}", icons::SEARCH),
                font(12.5),
                TEXT_DIM,
            );
            ui.painter().text(
                rect.right_center() - vec2(10.0, 0.0),
                Align2::RIGHT_CENTER,
                "Ctrl K",
                font(11.0),
                TEXT_FAINT,
            );
            if resp
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text("Command palette")
                .clicked()
            {
                s.push(Cmd::CommandPalette);
            }

            // Run controls on the right.
            let right = Rect::from_min_max(pos2(full.right() - 360.0, full.top()), full.max);
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(right)
                    .layout(Layout::right_to_left(Align::Center)),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    run_controls(s, ui);
                },
            );
        });
}

fn logo(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(5), ACCENT);
    widgets::paint_icon(ui, rect, icons::WORKFLOW, 14.0, Color32::WHITE);
}

fn menus(s: &mut Studio, ui: &mut Ui) {
    let item = |ui: &mut Ui, label: &str, keys: &str| {
        ui.add(
            egui::Button::new(label)
                .shortcut_text(keys)
                .min_size(vec2(220.0, 0.0)),
        )
    };
    ui.menu_button("File", |ui| {
        if item(ui, "New Graph", "Ctrl+N").clicked() {
            s.push(Cmd::NewGraph);
            ui.close();
        }
        if item(ui, "Open…", "Ctrl+O").clicked() {
            s.push(Cmd::OpenDialog);
            ui.close();
        }
        let examples = example_files();
        if !examples.is_empty() {
            ui.menu_button("Open Example", |ui| {
                for p in examples {
                    let name = p
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    if ui.button(name).clicked() {
                        s.push(Cmd::Open(p));
                        ui.close();
                    }
                }
            });
        }
        ui.separator();
        if item(ui, "Save", "Ctrl+S").clicked() {
            s.push(Cmd::Save);
            ui.close();
        }
        if item(ui, "Save As…", "Ctrl+Shift+S").clicked() {
            s.push(Cmd::SaveAsDialog);
            ui.close();
        }
    });
    ui.menu_button("Edit", |ui| {
        if item(ui, "Add Node…", "Ctrl+K").clicked() {
            s.push(Cmd::CommandPalette);
            ui.close();
        }
        if ui
            .add_enabled(
                s.focused.is_some(),
                egui::Button::new("Delete Node")
                    .shortcut_text("Del")
                    .min_size(vec2(220.0, 0.0)),
            )
            .clicked()
        {
            s.push(Cmd::DeleteFocused);
            ui.close();
        }
    });
    ui.menu_button("View", |ui| {
        if item(ui, "Command Palette…", "Ctrl+K").clicked() {
            s.push(Cmd::CommandPalette);
            ui.close();
        }
        ui.separator();
        if item(ui, "Node Library", "").clicked() {
            s.push(Cmd::Activity(Activity::Library));
            ui.close();
        }
        if item(ui, "Outline", "").clicked() {
            s.push(Cmd::Activity(Activity::Outline));
            ui.close();
        }
        if item(ui, "Processes", "").clicked() {
            s.push(Cmd::Activity(Activity::Processes));
            ui.close();
        }
        ui.separator();
        if item(ui, "Toggle Sidebar", "Ctrl+B").clicked() {
            s.push(Cmd::ToggleSidebar);
            ui.close();
        }
        if item(ui, "Toggle Panel", "Ctrl+J").clicked() {
            s.push(Cmd::TogglePanel);
            ui.close();
        }
        if item(ui, "Toggle Inspector", "Ctrl+I").clicked() {
            s.push(Cmd::ToggleInspector);
            ui.close();
        }
    });
    ui.menu_button("Run", |ui| {
        let label = if s.live() {
            "Restart Graph"
        } else {
            "Run Graph"
        };
        if ui
            .add_enabled(
                s.online(),
                egui::Button::new(label)
                    .shortcut_text("Ctrl+Enter")
                    .min_size(vec2(220.0, 0.0)),
            )
            .clicked()
        {
            s.push(Cmd::Deploy);
            ui.close();
        }
        if ui
            .add_enabled(
                s.running_graph().is_some(),
                egui::Button::new("Stop").min_size(vec2(220.0, 0.0)),
            )
            .clicked()
        {
            s.push(Cmd::Stop);
            ui.close();
        }
    });
}

fn run_controls(s: &mut Studio, ui: &mut Ui) {
    let live = s.live();
    let label = if s.undeployed_changes() {
        "Apply changes"
    } else if live {
        "Restart"
    } else {
        "Run"
    };
    let glyph = if live { icons::ROTATE_CW } else { icons::PLAY };
    let run = ui
        .add_enabled_ui(s.online(), |ui| {
            widgets::button(ui, Some(glyph), label, ButtonKind::Primary)
        })
        .inner
        .on_hover_text("Validate and run the graph (Ctrl+Enter)");
    if run.clicked() {
        s.push(Cmd::Deploy);
    }
    let stop = ui
        .add_enabled_ui(s.running_graph().is_some(), |ui| {
            widgets::button(ui, Some(icons::SQUARE), "Stop", ButtonKind::Secondary)
        })
        .inner;
    if stop.clicked() {
        s.push(Cmd::Stop);
    }
}

pub fn activity_bar(s: &mut Studio, ui: &mut Ui) {
    egui::Panel::left("activity")
        .exact_size(48.0)
        .resizable(false)
        .frame(
            Frame::new()
                .fill(CHROME)
                .inner_margin(Margin::symmetric(0, 6)),
        )
        .show(ui, |ui| {
            widgets::edge(ui, Side::Right);
            ui.vertical_centered(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                for (activity, glyph, tip) in [
                    (Activity::Library, icons::BLOCKS, "Node Library"),
                    (Activity::Outline, icons::WORKFLOW, "Outline"),
                    (Activity::Processes, icons::CPU, "Processes"),
                ] {
                    let active = s.sidebar_open && s.activity == activity;
                    let (rect, resp) = ui.allocate_exact_size(vec2(48.0, 44.0), Sense::click());
                    if active {
                        ui.painter().vline(
                            rect.left() + 1.0,
                            rect.y_range().shrink(6.0),
                            Stroke::new(2.0, ACCENT),
                        );
                    }
                    let color = if active || resp.hovered() {
                        TEXT_STRONG
                    } else {
                        TEXT_FAINT
                    };
                    widgets::paint_icon(ui, rect, glyph, 21.0, color);
                    if resp
                        .on_hover_text(tip)
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .clicked()
                    {
                        s.push(Cmd::Activity(activity));
                    }
                }
            });
        });
}

pub fn editor_tabs(s: &mut Studio, ui: &mut Ui) {
    let (strip, _) = ui.allocate_exact_size(vec2(ui.available_width(), TABSTRIP_H), Sense::hover());
    let p = ui.painter();
    p.rect_filled(strip, CornerRadius::ZERO, CHROME);
    p.hline(
        strip.x_range(),
        strip.bottom() - 0.5,
        Stroke::new(1.0, BORDER),
    );

    let name = match &s.doc.path {
        Some(path) => path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string(),
        None => format!("{}.yaml", s.doc.name),
    };
    let text = widgets::galley(ui, name, font(13.0), TEXT_STRONG);
    let tab = Rect::from_min_size(strip.min, vec2(text.size().x + 70.0, TABSTRIP_H));
    p.rect_filled(tab, CornerRadius::ZERO, EDITOR);
    p.hline(tab.x_range(), tab.top() + 0.5, Stroke::new(1.0, ACCENT));
    p.vline(tab.right() - 0.5, tab.y_range(), Stroke::new(1.0, BORDER));
    widgets::paint_icon(
        ui,
        Rect::from_center_size(pos2(tab.left() + 20.0, tab.center().y), Vec2::splat(16.0)),
        icons::WORKFLOW,
        14.0,
        ACCENT_TEXT,
    );
    p.galley(
        pos2(tab.left() + 34.0, tab.center().y - text.size().y / 2.0),
        text,
        TEXT_STRONG,
    );
    if s.doc.dirty {
        p.circle_filled(pos2(tab.right() - 18.0, tab.center().y), 4.0, TEXT_DIM);
    }

    // Breadcrumb-style state on the right of the strip.
    let mut x = strip.right() - 12.0;
    let mut chip = |text: String, color: Color32| {
        let g = widgets::galley(ui, text, font(11.5), color);
        x -= g.size().x;
        ui.painter()
            .galley(pos2(x, strip.center().y - g.size().y / 2.0), g, color);
        x -= 16.0;
    };
    let groups = s.doc.to_graph(&s.registry).groups().len();
    chip(
        format!(
            "{} nodes · {} process{}",
            s.doc.snarl.nodes().count(),
            groups,
            if groups == 1 { "" } else { "es" }
        ),
        TEXT_FAINT,
    );
    if s.undeployed_changes() {
        chip("● changes not applied".into(), WARN);
    }
}

pub fn status_bar(s: &mut Studio, ui: &mut Ui) {
    egui::Panel::bottom("statusbar")
        .exact_size(STATUSBAR_H)
        .frame(Frame::new().fill(CHROME))
        .show(ui, |ui| {
            widgets::edge(ui, Side::Top);
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;

                // Remote indicator, VS Code style.
                let (fill, text, tip) = match &s.conn {
                    Conn::Online { daemon, endpoint } => (
                        ACCENT,
                        daemon.clone(),
                        format!("Connected to daemon `{daemon}` at {endpoint}"),
                    ),
                    Conn::Connecting => (
                        Color32::from_rgb(0x3a, 0x3a, 0x3a),
                        "connecting".into(),
                        "Looking for a daemon".into(),
                    ),
                    Conn::Offline(reason) => (
                        Color32::from_rgb(0x6b, 0x1f, 0x1f),
                        "offline".into(),
                        reason.clone(),
                    ),
                };
                let g = widgets::galley(ui, text, font_medium(12.0), Color32::WHITE);
                let (rect, resp) =
                    ui.allocate_exact_size(vec2(g.size().x + 34.0, STATUSBAR_H), Sense::hover());
                ui.painter().rect_filled(rect, CornerRadius::ZERO, fill);
                widgets::paint_icon(
                    ui,
                    Rect::from_center_size(
                        pos2(rect.left() + 13.0, rect.center().y),
                        Vec2::splat(14.0),
                    ),
                    icons::RADIO_TOWER,
                    12.0,
                    Color32::WHITE,
                );
                ui.painter().galley(
                    pos2(rect.left() + 25.0, rect.center().y - g.size().y / 2.0),
                    g,
                    Color32::WHITE,
                );
                resp.on_hover_text(tip);

                // Graph state.
                let (glyph, color, text) = if s.live() {
                    let failed = s.metrics.values().any(|m| m.state == NodeState::Failed)
                        || s.status.as_ref().is_some_and(|st| {
                            st.groups.iter().any(|g| g.state != GroupState::Running)
                        });
                    if failed {
                        (
                            icons::TRIANGLE_ALERT,
                            WARN,
                            format!("{} degraded", s.doc.name),
                        )
                    } else {
                        (icons::CIRCLE_DOT, OK, format!("{} running", s.doc.name))
                    }
                } else if let Some(other) = s.running_graph() {
                    (
                        icons::CIRCLE_DOT,
                        TEXT_DIM,
                        format!("daemon is running {other}"),
                    )
                } else {
                    (icons::CIRCLE, TEXT_DIM, "idle".into())
                };
                status_item(ui, glyph, color, &text, "").clicked();

                let errors = s.issues.len();
                if status_item(
                    ui,
                    icons::CIRCLE_ALERT,
                    if errors > 0 { ERR } else { TEXT_DIM },
                    &errors.to_string(),
                    "Problems",
                )
                .clicked()
                {
                    s.push(Cmd::ShowPanel(PanelTab::Problems));
                }
                let failing = s
                    .metrics
                    .values()
                    .filter(|m| m.errors > 0 || m.state == NodeState::Failed)
                    .count();
                if status_item(
                    ui,
                    icons::TRIANGLE_ALERT,
                    if failing > 0 { WARN } else { TEXT_DIM },
                    &failing.to_string(),
                    "Nodes reporting errors",
                )
                .clicked()
                {
                    s.push(Cmd::ShowPanel(PanelTab::Output));
                }

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    status_item(
                        ui,
                        "",
                        TEXT_DIM,
                        concat!("ruro ", env!("CARGO_PKG_VERSION")),
                        "",
                    );
                    if s.live() {
                        let cpu: f64 = s.metrics.values().map(|m| m.cpu_pct).sum();
                        let rate: f64 = s.metrics.values().map(|m| m.hz).sum();
                        status_item(
                            ui,
                            icons::CPU,
                            TEXT_DIM,
                            &format!("{cpu:.0}% CPU"),
                            "CPU used by all node threads",
                        );
                        status_item(
                            ui,
                            icons::ACTIVITY,
                            TEXT_DIM,
                            &format!("{rate:.0} steps/s"),
                            "Node steps per second, all nodes",
                        );
                        if let Some(st) = &s.status {
                            let restarts: u32 = st.groups.iter().map(|g| g.restarts).sum();
                            if restarts > 0 {
                                status_item(
                                    ui,
                                    icons::ROTATE_CW,
                                    WARN,
                                    &format!(
                                        "{restarts} restart{}",
                                        if restarts == 1 { "" } else { "s" }
                                    ),
                                    "Process restarts since deploy",
                                );
                            }
                        }
                    }
                });
            });
        });
}

fn status_item(ui: &mut Ui, glyph: &str, color: Color32, text: &str, tip: &str) -> egui::Response {
    let g = widgets::galley(ui, text.to_owned(), font(12.0), TEXT);
    let icon_w = if glyph.is_empty() { 0.0 } else { 17.0 };
    let (rect, resp) = ui.allocate_exact_size(
        vec2(g.size().x + icon_w + 16.0, STATUSBAR_H),
        Sense::click(),
    );
    if resp.hovered() && !tip.is_empty() {
        ui.painter().rect_filled(rect, CornerRadius::ZERO, HOVER);
    }
    let mut x = rect.left() + 8.0;
    if !glyph.is_empty() {
        widgets::paint_icon(
            ui,
            Rect::from_center_size(pos2(x + 6.0, rect.center().y), Vec2::splat(14.0)),
            glyph,
            12.0,
            color,
        );
        x += icon_w;
    }
    ui.painter()
        .galley(pos2(x, rect.center().y - g.size().y / 2.0), g, TEXT);
    if tip.is_empty() {
        resp
    } else {
        resp.on_hover_text(tip)
    }
}

/// Floating controls in the canvas corner.
pub fn canvas_controls(s: &mut Studio, ui: &mut Ui, canvas: Rect) {
    let rect = Rect::from_min_size(canvas.right_bottom() - vec2(84.0, 44.0), vec2(72.0, 32.0));
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        Frame::new()
            .fill(CARD)
            .stroke(Stroke::new(1.0, BORDER_STRONG))
            .corner_radius(CornerRadius::same(RADIUS + 1))
            .inner_margin(Margin::same(3))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    if widgets::icon_button(ui, icons::FOCUS, "Fit graph to view (Ctrl+0)")
                        .clicked()
                    {
                        s.push(Cmd::FitView);
                    }
                    if widgets::icon_button(ui, icons::PLUS, "Add node (Ctrl+K)").clicked() {
                        s.push(Cmd::CommandPalette);
                    }
                });
            });
    });
}

/// Start page shown on an empty canvas.
pub fn welcome(s: &mut Studio, ui: &mut Ui) {
    let area = ui.max_rect();
    let w = 520.0_f32.min(area.width() - 40.0);
    let rect = Rect::from_center_size(area.center(), vec2(w, 360.0));
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
                ui.painter().rect_filled(r, CornerRadius::same(10), ACCENT);
                widgets::paint_icon(ui, r, icons::WORKFLOW, 24.0, Color32::WHITE);
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new("ruro studio")
                            .font(font_semibold(22.0))
                            .color(TEXT_STRONG),
                    );
                    ui.label(
                        RichText::new("Build, run and inspect robot dataflow graphs.")
                            .color(TEXT_DIM),
                    );
                });
            });
            ui.add_space(26.0);
            ui.columns(2, |cols| {
                let ui = &mut cols[0];
                ui.label(
                    RichText::new("Start")
                        .font(font_medium(14.0))
                        .color(TEXT_STRONG),
                );
                ui.add_space(4.0);
                if link_row(ui, icons::FILE_PLUS, "New graph") {
                    s.push(Cmd::NewGraph);
                }
                if link_row(ui, icons::FOLDER_OPEN, "Open…") {
                    s.push(Cmd::OpenDialog);
                }
                if link_row(ui, icons::COMMAND, "Command palette") {
                    s.push(Cmd::CommandPalette);
                }
                let examples = example_files();
                if !examples.is_empty() {
                    ui.add_space(14.0);
                    ui.label(
                        RichText::new("Examples")
                            .font(font_medium(14.0))
                            .color(TEXT_STRONG),
                    );
                    ui.add_space(4.0);
                    for p in examples {
                        let name = p
                            .file_stem()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();
                        if link_row(ui, icons::WORKFLOW, &name) {
                            s.push(Cmd::Open(p));
                        }
                    }
                }

                let ui = &mut cols[1];
                ui.label(
                    RichText::new("Shortcuts")
                        .font(font_medium(14.0))
                        .color(TEXT_STRONG),
                );
                ui.add_space(6.0);
                for (what, keys) in [
                    ("Command palette", "Ctrl K"),
                    ("Add node", "Right-click"),
                    ("Run graph", "Ctrl ↵"),
                    ("Save", "Ctrl S"),
                    ("Toggle panel", "Ctrl J"),
                    ("Toggle sidebar", "Ctrl B"),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(what).color(TEXT_DIM));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            widgets::kbd(ui, keys)
                        });
                    });
                    ui.add_space(2.0);
                }
            });
        });
    });
}

fn link_row(ui: &mut Ui, glyph: &str, label: &str) -> bool {
    let resp = ui
        .horizontal(|ui| {
            widgets::icon(ui, glyph, 14.0, ACCENT_TEXT);
            ui.add(egui::Label::new(RichText::new(label).color(ACCENT_TEXT)).sense(Sense::click()))
        })
        .inner;
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

/// Notifications in the bottom-right corner.
pub fn toasts(s: &mut Studio, ctx: &egui::Context) {
    let now = Instant::now();
    s.toasts.retain(|t| t.until > now);
    if s.toasts.is_empty() {
        return;
    }
    let mut close = None;
    egui::Area::new(egui::Id::new("toasts"))
        .anchor(Align2::RIGHT_BOTTOM, vec2(-14.0, -(STATUSBAR_H + 12.0)))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.set_width(380.0);
            for (i, t) in s.toasts.iter().enumerate() {
                let (glyph, color) = match t.severity {
                    Severity::Info => (icons::INFO, ACCENT_TEXT),
                    Severity::Success => (icons::CIRCLE_CHECK, OK),
                    Severity::Warning => (icons::TRIANGLE_ALERT, WARN),
                    Severity::Error => (icons::CIRCLE_ALERT, ERR),
                };
                Frame::new()
                    .fill(CARD)
                    .stroke(Stroke::new(1.0, BORDER_STRONG))
                    .corner_radius(CornerRadius::same(RADIUS + 1))
                    .inner_margin(Margin::symmetric(12, 10))
                    .shadow(egui::Shadow {
                        offset: [0, 6],
                        blur: 20,
                        spread: 0,
                        color: Color32::from_black_alpha(130),
                    })
                    .show(ui, |ui| {
                        ui.set_width(356.0);
                        ui.horizontal_top(|ui| {
                            widgets::icon(ui, glyph, 15.0, color);
                            ui.add_space(4.0);
                            ui.add(egui::Label::new(RichText::new(&t.text).color(TEXT)).wrap())
                                .on_hover_text(&t.text);
                            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                                if widgets::icon_button_sized(ui, icons::X, "", 20.0, 13.0, false)
                                    .clicked()
                                {
                                    close = Some(i);
                                }
                            });
                        });
                    });
                ui.add_space(8.0);
            }
        });
    if let Some(i) = close {
        s.toasts.remove(i);
    }
}
