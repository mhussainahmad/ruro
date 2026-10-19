//! Window chrome: title bar and menus, activity bar, editor tabs, banners,
//! status bar, the playback timeline, the welcome page and notifications.

use std::time::Instant;

use egui::{
    Align, Align2, Color32, CornerRadius, Frame, Id, Layout, Margin, Rect, RichText, Sense, Stroke,
    Ui, Vec2, pos2, vec2,
};
use ruro_proto::{GroupState, NodeState, PlaybackCommand, PlaybackStatus};

use crate::app::{
    Activity, AlignOp, Cmd, Conn, PanelTab, RecordDraft, Severity, Studio, example_files, file_name,
};
use crate::icons;
use crate::theme::*;
use crate::widgets::{self, ButtonKind, Side};

pub fn title_bar(s: &mut Studio, ui: &mut Ui) {
    egui::Panel::top("titlebar")
        .exact_size(TITLEBAR_H)
        .frame(
            Frame::new()
                .fill(CHROME())
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

            // Command center, centred in the window like VS Code's, and
            // narrow enough to leave the run controls room on small windows.
            let controls_w = run_controls_width(s);
            let w = 440.0_f32
                .min(full.width() * 0.34)
                .min((full.width() / 2.0 - controls_w - 12.0) * 2.0)
                .max(160.0);
            let rect = Rect::from_center_size(full.center(), vec2(w, 24.0));
            let resp = ui.interact(rect, ui.id().with("command-center"), Sense::click());
            let fill = if resp.hovered() {
                COMMAND_CENTER_HOVER()
            } else {
                COMMAND_CENTER()
            };
            ui.painter().rect(
                rect,
                CornerRadius::same(RADIUS + 1),
                fill,
                Stroke::new(1.0, BORDER_STRONG()),
                egui::StrokeKind::Inside,
            );
            let title = s.tab_title(&s.doc);
            let label = if s.doc.parent.is_some() || s.doc.path.is_none() {
                title
            } else {
                format!("{}  ·  {title}", s.doc.name)
            };
            ui.painter().text(
                rect.center() - vec2(8.0, 0.0),
                Align2::CENTER_CENTER,
                format!("{}  {}", icons::SEARCH, widgets::truncate(&label, 48)),
                font(12.5),
                TEXT_DIM(),
            );
            ui.painter().text(
                rect.right_center() - vec2(10.0, 0.0),
                Align2::RIGHT_CENTER,
                "Ctrl K",
                font(11.0),
                TEXT_FAINT(),
            );
            if resp
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text("Command palette")
                .clicked()
            {
                s.push(Cmd::CommandPalette);
            }

            // Run controls on the right.
            let right = Rect::from_min_max(pos2(full.right() - controls_w, full.top()), full.max);
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

/// Room for the run controls at the right of the title bar.
const RUN_CONTROLS_W: f32 = 400.0;
/// Extra room for the overlay picker, shown for graphs with overlays.
const VARIANT_PICKER_W: f32 = 170.0;

fn run_controls_width(s: &Studio) -> f32 {
    if has_overlays(s) {
        RUN_CONTROLS_W + VARIANT_PICKER_W
    } else {
        RUN_CONTROLS_W
    }
}

fn has_overlays(s: &Studio) -> bool {
    s.doc.parent.is_none() && !s.base_doc().overlays.is_empty()
}

/// Label of the overlay picker: what the canvas shows and Run deploys.
pub fn variant_label(s: &Studio) -> String {
    match s.variant_names() {
        [] => "Base graph".to_owned(),
        names => names.join(" + "),
    }
}

/// Picks the overlays applied to the graph on the canvas and when it runs.
fn variant_picker(s: &mut Studio, ui: &mut Ui) {
    let label = widgets::truncate(&variant_label(s), 18);
    let resp = widgets::button(ui, Some(icons::LAYERS), &label, ButtonKind::Secondary)
        .on_hover_text("Overlay: the variant of the graph that is shown and run");
    let names: Vec<String> = s.base_doc().overlays.keys().cloned().collect();
    let selected = s.variant_names().to_vec();
    let mut choice = None;
    egui::Popup::menu(&resp).show(|ui| {
        ui.set_min_width(200.0);
        if widgets::list_row(
            ui,
            icons::WORKFLOW,
            TEXT_DIM(),
            "Base graph",
            "",
            selected.is_empty(),
        )
        .clicked()
        {
            choice = Some(Vec::new());
            ui.close();
        }
        ui.separator();
        for name in &names {
            let on = selected.contains(name);
            let detail = if on { "applied" } else { "" };
            if widgets::list_row(ui, icons::LAYERS, TEXT_DIM(), name, detail, on).clicked() {
                // Overlays combine: a click adds or removes one.
                let mut next = selected.clone();
                if on {
                    next.retain(|n| n != name);
                } else {
                    next.push(name.clone());
                }
                choice = Some(next);
                ui.close();
            }
        }
    });
    if let Some(names) = choice {
        s.push(Cmd::SetVariant(names));
    }
}

fn logo(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(5), ACCENT());
    widgets::paint_icon(ui, rect, icons::WORKFLOW, 14.0, Color32::WHITE);
}

/// A menu entry with a shortcut hint. Returns true when chosen.
fn item(ui: &mut Ui, enabled: bool, label: &str, keys: &str) -> bool {
    let clicked = ui
        .add_enabled(
            enabled,
            egui::Button::new(label)
                .shortcut_text(RichText::new(keys).color(TEXT_FAINT()))
                .min_size(vec2(236.0, 0.0)),
        )
        .clicked();
    if clicked {
        ui.close();
    }
    clicked
}

fn menus(s: &mut Studio, ui: &mut Ui) {
    let selection = !s.selected_nodes().is_empty();
    ui.menu_button("File", |ui| {
        if item(ui, true, "New Graph", "Ctrl+N") {
            s.push(Cmd::NewGraph);
        }
        ui.menu_button("New from Template", |ui| {
            for t in crate::templates::TEMPLATES {
                if item(ui, true, t.title, "") {
                    s.push(Cmd::NewFromTemplate(t.id));
                }
            }
        });
        ui.separator();
        if item(ui, true, "Open…", "Ctrl+O") {
            s.push(Cmd::OpenDialog);
        }
        if item(ui, true, "Quick Open…", "Ctrl+P") {
            s.push(Cmd::QuickOpen);
        }
        let recent = s.settings.recent.clone();
        ui.add_enabled_ui(!recent.is_empty(), |ui| {
            ui.menu_button("Open Recent", |ui| {
                for p in recent {
                    let label = format!("{}   {}", file_name(&p), short_dir(&p));
                    if item(ui, true, &label, "") {
                        s.push(Cmd::Open(p));
                    }
                }
                ui.separator();
                if item(ui, true, "Clear Recent", "") {
                    s.push(Cmd::ClearRecent);
                }
            });
        });
        let examples = example_files();
        if !examples.is_empty() {
            ui.menu_button("Open Example", |ui| {
                for p in examples {
                    let name = p
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    if item(ui, true, &name, "") {
                        s.push(Cmd::Open(p));
                    }
                }
            });
        }
        ui.separator();
        if item(ui, true, "Save", "Ctrl+S") {
            s.push(Cmd::Save);
        }
        if item(ui, true, "Save As…", "Ctrl+Shift+S") {
            s.push(Cmd::SaveAsDialog);
        }
        ui.separator();
        if item(ui, s.doc.path.is_some(), "Compare with Saved", "") {
            s.push(Cmd::CompareWithSaved);
        }
        if item(ui, true, "Compare with File…", "") {
            s.push(Cmd::CompareDialog);
        }
        ui.separator();
        if item(ui, true, "Close Tab", "Ctrl+W") {
            s.push(Cmd::CloseTab(s.doc.uid));
        }
    });
    ui.menu_button("Edit", |ui| {
        if item(ui, s.doc.can_undo(), "Undo", "Ctrl+Z") {
            s.push(Cmd::Undo);
        }
        if item(ui, s.doc.can_redo(), "Redo", "Ctrl+Shift+Z") {
            s.push(Cmd::Redo);
        }
        ui.separator();
        if item(ui, selection, "Cut", "Ctrl+X") {
            s.push(Cmd::Cut);
        }
        if item(ui, selection, "Copy", "Ctrl+C") {
            s.push(Cmd::Copy);
        }
        if item(ui, s.clipboard.is_some(), "Paste", "Ctrl+V")
            && let Some(text) = s.clipboard.clone()
        {
            s.push(Cmd::Paste(text));
        }
        if item(ui, selection, "Duplicate", "Ctrl+D") {
            s.push(Cmd::Duplicate);
        }
        if item(ui, selection, "Delete", "Del") {
            s.push(Cmd::DeleteSelection);
        }
        if item(ui, true, "Select All", "Ctrl+A") {
            s.push(Cmd::SelectAll);
        }
        ui.separator();
        ui.add_enabled_ui(s.selected_nodes().len() > 1, |ui| {
            ui.menu_button("Align", |ui| {
                for op in AlignOp::ALL {
                    if item(ui, true, op.label(), "") {
                        s.push(Cmd::Align(op));
                    }
                }
            });
        });
        if item(ui, selection, "Group into Subgraph", "Ctrl+G") {
            s.push(Cmd::GroupSelection);
        }
        let composite = s
            .focused
            .filter(|id| s.doc.snarl[*id].is_composite())
            .map(|id| s.doc.snarl[id].id.clone());
        if item(ui, composite.is_some(), "Ungroup Subgraph", "")
            && let Some(id) = composite.clone()
        {
            s.push(Cmd::Ungroup(id));
        }
        if item(ui, composite.is_some(), "Open Subgraph", "")
            && let Some(id) = composite
        {
            s.push(Cmd::OpenSubgraph(id));
        }
        ui.separator();
        if item(ui, true, "Add Node…", "Ctrl+K") {
            s.push(Cmd::CommandPalette);
        }
    });
    ui.menu_button("View", |ui| {
        if item(ui, true, "Command Palette…", "Ctrl+K") {
            s.push(Cmd::CommandPalette);
        }
        ui.separator();
        for (a, label) in [
            (Activity::Library, "Node Library"),
            (Activity::Outline, "Outline"),
            (Activity::Processes, "Processes"),
            (Activity::Fleet, "Fleet"),
            (Activity::Changes, "Changes"),
            (Activity::Ros2, "ROS 2 Topics"),
        ] {
            if item(ui, true, label, "") {
                s.push(Cmd::Activity(a));
            }
        }
        ui.separator();
        if item(ui, true, "Toggle Sidebar", "Ctrl+B") {
            s.push(Cmd::ToggleSidebar);
        }
        if item(ui, true, "Toggle Panel", "Ctrl+J") {
            s.push(Cmd::TogglePanel);
        }
        if item(ui, true, "Toggle Inspector", "Ctrl+I") {
            s.push(Cmd::ToggleInspector);
        }
        ui.separator();
        ui.menu_button("Theme", |ui| {
            for (kind, label) in [(ThemeKind::Dark, "Dark"), (ThemeKind::Light, "Light")] {
                let check = if s.theme == kind { "✓" } else { "" };
                if item(ui, true, label, check) {
                    s.push(Cmd::SetTheme(kind));
                }
            }
        });
    });
    ui.menu_button("Run", |ui| {
        let label = if s.live() {
            "Restart Graph"
        } else {
            "Run Graph"
        };
        if item(ui, s.online(), label, "Ctrl+Enter") {
            s.push(Cmd::Deploy);
        }
        if item(ui, s.running_graph().is_some(), "Stop", "") {
            s.push(Cmd::Stop);
        }
        ui.separator();
        if item(ui, s.online(), "Engage Emergency Stop", "F12") {
            s.push(Cmd::Estop(true));
        }
        if item(ui, s.online() && s.estopped(), "Release Emergency Stop", "") {
            s.push(Cmd::Estop(false));
        }
        ui.separator();
        let recording = s.status.as_ref().is_some_and(|st| st.recording.is_some());
        if recording {
            if item(ui, true, "Stop Recording", "") {
                s.push(Cmd::StopRecording);
            }
        } else if item(ui, s.running_graph().is_some(), "Start Recording…", "") {
            s.record_draft = Some(RecordDraft {
                path: s.default_recording(),
            });
        }
        if item(ui, s.online(), "Replay Recording…", "") {
            s.push(Cmd::ReplayDialog);
        }
    });
}

/// The last directory components of a path, for menus.
fn short_dir(p: &std::path::Path) -> String {
    let dir = p
        .parent()
        .map(|d| d.display().to_string())
        .unwrap_or_default();
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = match dir.strip_prefix(&home) {
        Some(rest) if !home.is_empty() => format!("~{rest}"),
        _ => dir,
    };
    widgets::truncate_start(&dir, 36)
}

fn run_controls(s: &mut Studio, ui: &mut Ui) {
    // Rightmost and set apart: the one control that must be found blind.
    let daemon = s.daemon_name().unwrap_or("the daemon").to_owned();
    let tip = if s.estopped() {
        format!(
            "The emergency stop is engaged on `{daemon}`. Release it from the banner or the command palette."
        )
    } else {
        format!(
            "Emergency stop (F12): every node on `{daemon}` and the machines running the rest of its graph goes to its safe state"
        )
    };
    let estop = ui
        .add_enabled_ui(s.online(), |ui| widgets::estop_button(ui, &tip))
        .inner;
    if estop.clicked() {
        s.push(Cmd::Estop(true));
    }
    ui.add_space(6.0);
    let (sep, _) = ui.allocate_exact_size(vec2(1.0, 18.0), Sense::hover());
    ui.painter().vline(
        sep.center().x,
        sep.y_range(),
        Stroke::new(1.0, BORDER_STRONG()),
    );
    ui.add_space(6.0);

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
    let recording = s.status.as_ref().is_some_and(|st| st.recording.is_some());
    let resp = ui
        .add_enabled_ui(s.running_graph().is_some(), |ui| {
            if recording {
                widgets::record_button(ui, true, "Stop recording")
            } else {
                widgets::record_button(ui, false, "Record the running graph to a file")
            }
        })
        .inner;
    s.record_anchor = resp.rect;
    if resp.clicked() {
        if recording {
            s.push(Cmd::StopRecording);
        } else if s.record_draft.is_some() {
            s.record_draft = None;
        } else {
            s.record_draft = Some(RecordDraft {
                path: s.default_recording(),
            });
        }
    }
    if has_overlays(s) {
        variant_picker(s, ui);
    }
}

/// The "start recording" popover under the record button.
pub fn record_popover(s: &mut Studio, ctx: &egui::Context) {
    let Some(mut draft) = s.record_draft.take() else {
        return;
    };
    let width = 340.0;
    let anchor = s.record_anchor;
    let pos = pos2((anchor.right() - width).max(8.0), anchor.bottom() + 8.0);
    let mut start = false;
    let mut cancel = false;
    let daemon = s.daemon_name().unwrap_or("the daemon").to_owned();
    let area = egui::Area::new(Id::new("record-popover"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .show(ctx, |ui| {
            popover_frame().show(ui, |ui| {
                ui.set_width(width - 28.0);
                ui.horizontal(|ui| {
                    widgets::record_dot(ui, 5.0, ERR());
                    ui.label(
                        RichText::new("Record")
                            .font(font_semibold(14.0))
                            .color(TEXT_STRONG()),
                    );
                });
                ui.add_space(2.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(format!(
                            "Records the outputs of the groups running on `{daemon}` to an MCAP file there."
                        ))
                        .font(font(12.0))
                        .color(TEXT_DIM()),
                    )
                    .wrap(),
                );
                ui.add_space(8.0);
                ui.label(
                    RichText::new("File on the daemon")
                        .font(font_medium(11.5))
                        .color(TEXT_DIM()),
                );
                let field = widgets::text_field(ui, &mut draft.path, "recording.mcap");
                if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    start = true;
                }
                if !field.has_focus() && !field.lost_focus() && draft.path.ends_with(".mcap") {
                    field.request_focus();
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        if widgets::button(
                            ui,
                            Some(icons::DISC),
                            "Start recording",
                            ButtonKind::Primary,
                        )
                        .clicked()
                        {
                            start = true;
                        }
                        if widgets::button(ui, None, "Cancel", ButtonKind::Secondary).clicked() {
                            cancel = true;
                        }
                    });
                });
            });
        });
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        cancel = true;
    }
    let clicked_outside = ctx.input(|i| i.pointer.any_pressed())
        && ctx
            .input(|i| i.pointer.interact_pos())
            .is_some_and(|p| !area.response.rect.contains(p) && !anchor.contains(p));
    if start {
        s.push(Cmd::StartRecording(draft.path));
    } else if !cancel && !clicked_outside {
        s.record_draft = Some(draft);
    }
}

fn popover_frame() -> Frame {
    Frame::new()
        .fill(CARD())
        .stroke(Stroke::new(1.0, BORDER_STRONG()))
        .corner_radius(CornerRadius::same(RADIUS_LG))
        .inner_margin(Margin::same(14))
        .shadow(egui::Shadow {
            offset: [0, 8],
            blur: 28,
            spread: 0,
            color: shadow(140),
        })
}

pub fn activity_bar(s: &mut Studio, ui: &mut Ui) {
    egui::Panel::left("activity")
        .exact_size(48.0)
        .resizable(false)
        .frame(
            Frame::new()
                .fill(CHROME())
                .inner_margin(Margin::symmetric(0, 6)),
        )
        .show(ui, |ui| {
            widgets::edge(ui, Side::Right);
            let changes = s
                .compare
                .as_ref()
                .filter(|c| c.uid == s.doc.uid)
                .map(|c| c.diff.count());
            ui.vertical_centered(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                for (activity, glyph, tip, badge) in [
                    (Activity::Library, icons::BLOCKS, "Node Library", None),
                    (Activity::Outline, icons::WORKFLOW, "Outline", None),
                    (Activity::Processes, icons::CPU, "Processes", None),
                    (
                        Activity::Fleet,
                        icons::NETWORK,
                        "Fleet",
                        Some(s.fleet.len()).filter(|n| *n > 1),
                    ),
                    (Activity::Changes, icons::COMPARE, "Changes", changes),
                    (Activity::Ros2, icons::RADIO_TOWER, "ROS 2 Topics", None),
                ] {
                    let active = s.sidebar_open && s.activity == activity;
                    let (rect, resp) = ui.allocate_exact_size(vec2(48.0, 44.0), Sense::click());
                    if active {
                        ui.painter().vline(
                            rect.left() + 1.0,
                            rect.y_range().shrink(6.0),
                            Stroke::new(2.0, ACCENT()),
                        );
                    }
                    let color = if active || resp.hovered() {
                        TEXT_STRONG()
                    } else {
                        TEXT_FAINT()
                    };
                    widgets::paint_icon(ui, rect, glyph, 21.0, color);
                    if let Some(n) = badge.filter(|n| *n > 0) {
                        let c = rect.center() + vec2(9.0, 8.0);
                        ui.painter().circle_filled(c, 7.5, ACCENT());
                        ui.painter().text(
                            c,
                            Align2::CENTER_CENTER,
                            n.min(99).to_string(),
                            font_medium(9.5),
                            Color32::WHITE,
                        );
                    }
                    resp.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tip)
                    });
                    if resp
                        .on_hover_text(tip)
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .clicked()
                    {
                        s.push(Cmd::Activity(activity));
                    }
                }
            });
            // Theme switch at the bottom, like an editor's settings gear.
            let r = ui.max_rect();
            let rect =
                Rect::from_center_size(pos2(r.center().x, r.bottom() - 22.0), vec2(48.0, 40.0));
            let resp = ui.interact(rect, ui.id().with("theme"), Sense::click());
            let (glyph, tip, next) = match s.theme {
                ThemeKind::Dark => (icons::SUN, "Switch to light theme", ThemeKind::Light),
                ThemeKind::Light => (icons::MOON, "Switch to dark theme", ThemeKind::Dark),
            };
            let color = if resp.hovered() {
                TEXT_STRONG()
            } else {
                TEXT_FAINT()
            };
            widgets::paint_icon(ui, rect, glyph, 19.0, color);
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tip));
            if resp
                .on_hover_text(tip)
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                s.push(Cmd::SetTheme(next));
            }
        });
}

pub fn editor_tabs(s: &mut Studio, ui: &mut Ui) {
    let (strip, _) = ui.allocate_exact_size(vec2(ui.available_width(), TABSTRIP_H), Sense::hover());
    ui.painter()
        .rect_filled(strip, CornerRadius::ZERO, CHROME());
    ui.painter().hline(
        strip.x_range(),
        strip.bottom() - 0.5,
        Stroke::new(1.0, BORDER()),
    );

    // Breadcrumb-style state on the right of the strip.
    let mut chips: Vec<(String, Color32)> = Vec::new();
    let groups = s.doc.to_graph(&s.registry).groups().len();
    if s.undeployed_changes() {
        chips.push(("● changes not applied".into(), WARN()));
    }
    chips.push((
        format!(
            "{} nodes · {} process{}",
            s.doc.snarl.nodes().count(),
            groups,
            if groups == 1 { "" } else { "es" }
        ),
        TEXT_FAINT(),
    ));
    let chip_galleys: Vec<_> = chips
        .into_iter()
        .map(|(t, c)| widgets::galley(ui, t, font(11.5), c))
        .collect();
    let chips_w: f32 = chip_galleys.iter().map(|g| g.size().x + 16.0).sum::<f32>() + 8.0;

    let tabs: Vec<(u64, String, bool, bool)> = s
        .tabs()
        .iter()
        .map(|d| (d.uid, s.tab_title(d), d.dirty, d.parent.is_some()))
        .collect();
    let mut x = strip.left();
    let max_x = strip.right() - chips_w;
    let mut switch = None;
    let mut close = None;
    for (uid, title, dirty, sub) in tabs {
        let active = uid == s.doc.uid;
        let color = if active { TEXT_STRONG() } else { TEXT_DIM() };
        let text = widgets::galley(ui, widgets::truncate(&title, 40), font(13.0), color);
        let w = (text.size().x + 70.0).min((max_x - x).max(80.0));
        let tab = Rect::from_min_size(pos2(x, strip.top()), vec2(w, TABSTRIP_H));
        x += w;
        let resp = ui.interact(tab, ui.id().with(("tab", uid)), Sense::click());
        let close_rect =
            Rect::from_center_size(pos2(tab.right() - 18.0, tab.center().y), Vec2::splat(20.0));
        let close_resp = ui.interact(close_rect, ui.id().with(("tab-close", uid)), Sense::click());
        let p = ui.painter_at(tab);
        if active {
            p.rect_filled(tab, CornerRadius::ZERO, EDITOR());
            p.hline(tab.x_range(), tab.top() + 0.5, Stroke::new(1.0, ACCENT()));
        } else if resp.hovered() {
            p.rect_filled(tab.shrink2(vec2(0.0, 0.0)), CornerRadius::ZERO, HOVER());
        }
        p.vline(tab.right() - 0.5, tab.y_range(), Stroke::new(1.0, BORDER()));
        widgets::paint_icon(
            ui,
            Rect::from_center_size(pos2(tab.left() + 20.0, tab.center().y), Vec2::splat(16.0)),
            if sub { icons::BOXES } else { icons::WORKFLOW },
            14.0,
            if active { ACCENT_TEXT() } else { TEXT_FAINT() },
        );
        p.galley(
            pos2(tab.left() + 34.0, tab.center().y - text.size().y / 2.0),
            text,
            color,
        );
        if close_resp.hovered() {
            p.rect_filled(close_rect, CornerRadius::same(RADIUS), PRESSED());
        }
        if resp.hovered() || close_resp.hovered() || (active && !dirty) {
            widgets::paint_icon(
                ui,
                close_rect,
                icons::X,
                13.0,
                if close_resp.hovered() {
                    TEXT_STRONG()
                } else {
                    TEXT_DIM()
                },
            );
        } else if dirty {
            p.circle_filled(close_rect.center(), 4.0, TEXT_DIM());
        }
        resp.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, active, &title)
        });
        close_resp.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Close {title}"))
        });
        if close_resp.on_hover_text("Close (Ctrl+W)").clicked()
            || resp.clicked_by(egui::PointerButton::Middle)
        {
            close = Some(uid);
        } else if resp.clicked() {
            switch = Some(uid);
        }
        if x >= max_x {
            break;
        }
    }
    if let Some(uid) = close {
        s.push(Cmd::CloseTab(uid));
    } else if let Some(uid) = switch {
        s.push(Cmd::SwitchTab(uid));
    }

    let mut cx = strip.right() - 12.0;
    for g in chip_galleys.into_iter().rev() {
        cx -= g.size().x;
        if cx > x {
            ui.painter().galley(
                pos2(cx, strip.center().y - g.size().y / 2.0),
                g,
                TEXT_FAINT(),
            );
        }
        cx -= 16.0;
    }
}

/// Notices across the top of the editor: the emergency stop, recovery,
/// comparison, subgraph editing.
pub fn banners(s: &mut Studio, ui: &mut Ui) {
    let stopped = s.estopped_machines();
    if !stopped.is_empty() {
        // A stop engaged directly on a peer is released there, not here.
        let here = s.estopped();
        let text = format!(
            "Emergency stop engaged on {}. Nodes hold their safe state until it is released{}.",
            stopped.join(", "),
            if here { "" } else { " on that machine" }
        );
        let actions: &[(&str, ButtonKind)] = if here {
            &[("Release e-stop", ButtonKind::Danger)]
        } else {
            &[]
        };
        let choice = banner_with(
            ui,
            icons::CIRCLE_STOP,
            ERR(),
            ERR().gamma_multiply(if is_light() { 0.10 } else { 0.16 }),
            &text,
            actions,
        );
        if choice.is_some() {
            s.push(Cmd::Estop(false));
        }
    }
    if !s.recovery.is_empty() {
        let names: Vec<String> = s.recovery.iter().map(|r| r.name.clone()).collect();
        let choice = banner(
            ui,
            icons::HISTORY,
            ACCENT_TEXT(),
            &format!(
                "Unsaved changes from your last session: {}",
                widgets::truncate(&names.join(", "), 60)
            ),
            &[
                ("Restore", ButtonKind::Primary),
                ("Discard", ButtonKind::Secondary),
            ],
        );
        match choice {
            Some(0) => s.push(Cmd::RecoverAutosaves),
            Some(_) => s.push(Cmd::DiscardAutosaves),
            None => {}
        }
    }
    if let Some(c) = s.compare.as_ref().filter(|c| c.uid == s.doc.uid) {
        let d = &c.diff;
        let text = if d.is_empty() {
            format!("No differences from {}", c.label)
        } else {
            format!(
                "Comparing with {} · {} added · {} changed · {} removed · {} wire changes",
                c.label,
                d.added.len(),
                d.changed.len(),
                d.removed.len(),
                d.wires_added.len() + d.wires_removed.len()
            )
        };
        if banner(
            ui,
            icons::COMPARE,
            WARN(),
            &text,
            &[("Close comparison", ButtonKind::Secondary)],
        )
        .is_some()
        {
            s.push(Cmd::CloseCompare);
        }
    }
    if let Some(v) = &s.variant {
        let d = &v.diff;
        let mut parts = vec![format!(
            "Showing overlay {} (read-only)",
            v.names
                .iter()
                .map(|n| format!("`{n}`"))
                .collect::<Vec<_>>()
                .join(" + ")
        )];
        for (n, what) in [
            (d.changed.len(), "changed"),
            (d.added.len(), "added"),
            (d.removed.len(), "removed"),
        ] {
            if n > 0 {
                parts.push(format!("{n} {what}"));
            }
        }
        parts.extend(d.graph.iter().cloned());
        if banner(
            ui,
            icons::LAYERS,
            ACCENT_TEXT(),
            &parts.join(" · "),
            &[("Edit base graph", ButtonKind::Secondary)],
        )
        .is_some()
        {
            s.push(Cmd::SetVariant(Vec::new()));
        }
    }
    if let Some((parent, node)) = s.doc.parent.clone() {
        let parent_title = s
            .doc_by_uid(parent)
            .map(|p| s.tab_title(p))
            .unwrap_or_else(|| "a closed graph".into());
        if banner(
            ui,
            icons::BOXES,
            ACCENT_TEXT(),
            &format!("Subgraph `{node}` of {parent_title}. Edits apply to the parent graph."),
            &[("Back to parent", ButtonKind::Secondary)],
        )
        .is_some()
        {
            s.push(Cmd::SwitchTab(parent));
        }
    }
}

/// One full-width notice bar. Returns the index of a clicked action.
fn banner(
    ui: &mut Ui,
    glyph: &str,
    tint: Color32,
    text: &str,
    actions: &[(&str, ButtonKind)],
) -> Option<usize> {
    banner_with(ui, glyph, tint, Color32::TRANSPARENT, text, actions)
}

/// A notice bar washed with `wash` over the chrome colour.
fn banner_with(
    ui: &mut Ui,
    glyph: &str,
    tint: Color32,
    wash: Color32,
    text: &str,
    actions: &[(&str, ButtonKind)],
) -> Option<usize> {
    let mut clicked = None;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::ZERO, CHROME());
    ui.painter().rect_filled(rect, CornerRadius::ZERO, wash);
    ui.painter().hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, BORDER()),
    );
    ui.painter()
        .vline(rect.left() + 1.0, rect.y_range(), Stroke::new(2.0, tint));
    widgets::paint_icon(
        ui,
        Rect::from_center_size(pos2(rect.left() + 22.0, rect.center().y), Vec2::splat(16.0)),
        glyph,
        14.0,
        tint,
    );
    ui.painter().text(
        pos2(rect.left() + 38.0, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        font(12.5),
        TEXT(),
    );
    let mut inner = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(vec2(10.0, 6.0)))
            .layout(Layout::right_to_left(Align::Center)),
    );
    inner.spacing_mut().item_spacing.x = 6.0;
    for (i, (label, kind)) in actions.iter().enumerate().rev() {
        if widgets::button(&mut inner, None, label, *kind).clicked() {
            clicked = Some(i);
        }
    }
    clicked
}

pub fn status_bar(s: &mut Studio, ui: &mut Ui) {
    egui::Panel::bottom("statusbar")
        .exact_size(STATUSBAR_H)
        .frame(Frame::new().fill(CHROME()))
        .show(ui, |ui| {
            widgets::edge(ui, Side::Top);
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;

                // Remote indicator, VS Code style.
                let (fill, text, tip) = match &s.conn {
                    Conn::Online { daemon, endpoint } => (
                        ACCENT(),
                        daemon.clone(),
                        format!(
                            "Connected to daemon `{daemon}` at {endpoint}. Click for all daemons."
                        ),
                    ),
                    Conn::Connecting => (
                        STATUS_CONNECTING(),
                        "connecting".into(),
                        "Looking for a daemon".into(),
                    ),
                    Conn::Offline(reason) => (STATUS_OFFLINE(), "offline".into(), reason.clone()),
                };
                let g = widgets::galley(ui, text, font_medium(12.0), Color32::WHITE);
                let (rect, resp) =
                    ui.allocate_exact_size(vec2(g.size().x + 34.0, STATUSBAR_H), Sense::click());
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
                if resp
                    .on_hover_text(tip)
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    s.push(Cmd::Activity(Activity::Fleet));
                }

                // Graph state.
                if !s.estopped_machines().is_empty() {
                    let here = s.estopped();
                    let tip = if here {
                        "Emergency stop engaged. Click to release it."
                    } else {
                        "Emergency stop engaged on a machine running part of the graph. Release it there."
                    };
                    if estop_item(ui, here).on_hover_text(tip).clicked() && here {
                        s.push(Cmd::Estop(false));
                    }
                }
                let (glyph, color, text) = if !s.estopped_machines().is_empty() {
                    (
                        icons::CIRCLE_STOP,
                        ERR(),
                        match s.running_graph() {
                            Some(g) => format!("{g} held in safe state"),
                            None => "idle".into(),
                        },
                    )
                } else if s.live() {
                    let failed = s.metrics.values().any(|m| m.state == NodeState::Failed)
                        || s.status.as_ref().is_some_and(|st| {
                            st.groups.iter().any(|g| g.state != GroupState::Running)
                        });
                    if failed {
                        (
                            icons::TRIANGLE_ALERT,
                            WARN(),
                            format!("{} degraded", s.doc.name),
                        )
                    } else {
                        (icons::CIRCLE_DOT, OK(), format!("{} running", s.doc.name))
                    }
                } else if let Some(other) = s.running_graph() {
                    (
                        icons::CIRCLE_DOT,
                        TEXT_DIM(),
                        format!("daemon is running {other}"),
                    )
                } else {
                    (icons::CIRCLE, TEXT_DIM(), "idle".into())
                };
                status_item(ui, glyph, color, &text, "");

                if let Some(path) = s.status.as_ref().and_then(|st| st.recording.clone()) {
                    let secs = s.recording_since.map_or(0, |t| t.elapsed().as_secs());
                    if recording_item(ui, secs)
                        .on_hover_text(format!("Recording to {path}. Click to stop."))
                        .clicked()
                    {
                        s.push(Cmd::StopRecording);
                    }
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(50));
                }

                let errors = s.issues.len();
                if status_item(
                    ui,
                    icons::CIRCLE_ALERT,
                    if errors > 0 { ERR() } else { TEXT_DIM() },
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
                    if failing > 0 { WARN() } else { TEXT_DIM() },
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
                        TEXT_DIM(),
                        concat!("ruro ", env!("CARGO_PKG_VERSION")),
                        "",
                    );
                    if s.live() {
                        let cpu: f64 = s.metrics.values().map(|m| m.cpu_pct).sum();
                        let rate: f64 = s.metrics.values().map(|m| m.hz).sum();
                        status_item(
                            ui,
                            icons::CPU,
                            TEXT_DIM(),
                            &format!("{cpu:.0}% CPU"),
                            "CPU used by all node threads",
                        );
                        status_item(
                            ui,
                            icons::ACTIVITY,
                            TEXT_DIM(),
                            &format!("{rate:.0} steps/s"),
                            "Node steps per second, all nodes",
                        );
                        if let Some(st) = &s.status {
                            let restarts: u32 = st.groups.iter().map(|g| g.restarts).sum();
                            if restarts > 0 {
                                status_item(
                                    ui,
                                    icons::ROTATE_CW,
                                    WARN(),
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

/// "E-STOP" on solid red, while the emergency stop is engaged.
fn estop_item(ui: &mut Ui, releasable: bool) -> egui::Response {
    let g = widgets::galley(ui, "E-STOP ENGAGED", font_semibold(11.5), Color32::WHITE);
    let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 36.0, STATUSBAR_H), Sense::click());
    let fill = if resp.hovered() {
        ESTOP_HOVER()
    } else {
        ESTOP()
    };
    ui.painter().rect_filled(rect, CornerRadius::ZERO, fill);
    let sign = pos2(rect.left() + 14.0, rect.center().y);
    widgets::paint_octagon(ui, sign, 6.0, Color32::WHITE);
    ui.painter().rect_filled(
        Rect::from_center_size(sign, vec2(6.0, 1.8)),
        CornerRadius::same(1),
        fill,
    );
    ui.painter().galley(
        pos2(rect.left() + 26.0, rect.center().y - g.size().y / 2.0),
        g,
        Color32::WHITE,
    );
    let label = if releasable {
        "Release emergency stop"
    } else {
        "Emergency stop engaged"
    };
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    if releasable {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// "REC 00:12" with a pulsing dot.
fn recording_item(ui: &mut Ui, secs: u64) -> egui::Response {
    let text = format!("REC {:02}:{:02}", secs / 60, secs % 60);
    let g = widgets::galley(ui, text, font_mono(11.5), ERR());
    let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 34.0, STATUSBAR_H), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::ZERO, HOVER());
    }
    let t = ui.input(|i| i.time) as f32;
    let pulse = 0.55 + 0.45 * (t * 4.0).sin().abs();
    ui.painter().circle_filled(
        pos2(rect.left() + 14.0, rect.center().y),
        4.0,
        ERR().gamma_multiply(pulse),
    );
    ui.painter().galley(
        pos2(rect.left() + 24.0, rect.center().y - g.size().y / 2.0),
        g,
        ERR(),
    );
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Stop recording")
    });
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn status_item(ui: &mut Ui, glyph: &str, color: Color32, text: &str, tip: &str) -> egui::Response {
    let g = widgets::galley(ui, text.to_owned(), font(12.0), TEXT());
    let icon_w = if glyph.is_empty() { 0.0 } else { 17.0 };
    let (rect, resp) = ui.allocate_exact_size(
        vec2(g.size().x + icon_w + 16.0, STATUSBAR_H),
        Sense::click(),
    );
    if resp.hovered() && !tip.is_empty() {
        ui.painter().rect_filled(rect, CornerRadius::ZERO, HOVER());
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
        .galley(pos2(x, rect.center().y - g.size().y / 2.0), g, TEXT());
    if tip.is_empty() {
        resp
    } else {
        resp.on_hover_text(tip)
    }
}

/// Draws `add` into `rect` on a layer above the canvas, clipped to `clip`.
///
/// The canvas draws its nodes on a layer of its own, so widgets placed over
/// it in the same layer as the panel would never receive the pointer.
fn overlay<R>(
    ctx: &egui::Context,
    id: &str,
    rect: Rect,
    clip: Rect,
    add: impl FnOnce(&mut Ui) -> R,
) -> R {
    egui::Area::new(Id::new(id))
        .order(egui::Order::Middle)
        .fixed_pos(rect.min)
        .constrain(false)
        .show(ctx, |ui| {
            ui.set_clip_rect(clip);
            ui.scope_builder(egui::UiBuilder::new().max_rect(rect), add)
                .inner
        })
        .inner
}

/// Floating controls in the canvas corner.
pub fn canvas_controls(s: &mut Studio, ui: &mut Ui, canvas: Rect) {
    let rect = Rect::from_min_size(canvas.right_bottom() - vec2(84.0, 44.0), vec2(72.0, 32.0));
    overlay(&ui.ctx().clone(), "canvas-controls", rect, canvas, |ui| {
        Frame::new()
            .fill(CARD())
            .stroke(Stroke::new(1.0, BORDER_STRONG()))
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

/// Seconds as `m:ss.s`.
fn clock(secs: f64) -> String {
    let secs = secs.max(0.0);
    let m = (secs / 60.0).floor();
    format!("{m:.0}:{:04.1}", secs - m * 60.0)
}

/// Replay controls along the bottom of the canvas while a recording plays.
pub fn timeline(s: &mut Studio, ui: &mut Ui, canvas: Rect, pb: &PlaybackStatus) {
    let w = (canvas.width() - 220.0).clamp(360.0, 820.0);
    let rect = Rect::from_center_size(
        pos2(canvas.center().x, canvas.bottom() - 34.0),
        vec2(w, 44.0),
    );
    let mut send = None;
    overlay(&ui.ctx().clone(), "timeline", rect, canvas, |ui| {
        Frame::new()
            .fill(CARD())
            .stroke(Stroke::new(1.0, BORDER_STRONG()))
            .corner_radius(CornerRadius::same(RADIUS_LG))
            .inner_margin(Margin::symmetric(8, 6))
            .shadow(egui::Shadow {
                offset: [0, 6],
                blur: 20,
                spread: 0,
                color: shadow(120),
            })
            .show(ui, |ui| {
                ui.set_width(w - 16.0);
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let (glyph, tip, cmd) = if pb.paused {
                        (icons::PLAY, "Play", PlaybackCommand::Resume)
                    } else {
                        (icons::PAUSE, "Pause", PlaybackCommand::Pause)
                    };
                    if widgets::icon_button_sized(ui, glyph, tip, 28.0, 15.0, false).clicked() {
                        send = Some(cmd);
                    }
                    if widgets::icon_button_sized(
                        ui,
                        icons::SKIP_BACK,
                        "Back to start",
                        28.0,
                        14.0,
                        false,
                    )
                    .clicked()
                    {
                        send = Some(PlaybackCommand::Seek { position_s: 0.0 });
                    }
                    let shown = s.scrub.unwrap_or(pb.position_s);
                    ui.label(
                        RichText::new(clock(shown))
                            .font(font_mono(12.0))
                            .color(TEXT_STRONG()),
                    );
                    // Leave room for the duration, rate and close on the right.
                    let track_w = (ui.available_width() - 150.0).max(80.0);
                    if let Some(seek) = scrubber(s, ui, track_w, pb) {
                        send = Some(PlaybackCommand::Seek { position_s: seek });
                    }
                    ui.label(
                        RichText::new(clock(pb.duration_s))
                            .font(font_mono(12.0))
                            .color(TEXT_DIM()),
                    );
                    let mut rate = pb.rate;
                    egui::ComboBox::from_id_salt("playback-rate")
                        .selected_text(format_rate(rate))
                        .width(58.0)
                        .show_ui(ui, |ui| {
                            for r in [0.25, 0.5, 1.0, 2.0, 4.0] {
                                if ui.selectable_value(&mut rate, r, format_rate(r)).clicked() {
                                    send = Some(PlaybackCommand::SetRate { rate: r });
                                }
                            }
                        });
                    if widgets::icon_button_sized(ui, icons::X, "Stop replay", 24.0, 13.0, false)
                        .clicked()
                    {
                        send = Some(PlaybackCommand::Stop);
                    }
                });
            });
    });
    if let Some(c) = send {
        s.push(Cmd::Playback(c));
    }
}

fn format_rate(r: f64) -> String {
    if r.fract() == 0.0 {
        format!("{r:.0}×")
    } else {
        format!("{r}×")
    }
}

/// The seek bar. Returns a position to seek to when the user lets go.
fn scrubber(s: &mut Studio, ui: &mut Ui, width: f32, pb: &PlaybackStatus) -> Option<f64> {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 24.0), Sense::click_and_drag());
    let dur = pb.duration_s.max(1e-3);
    let at = |x: f32| (((x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64) * dur;
    if (resp.dragged() || resp.is_pointer_button_down_on())
        && let Some(p) = resp.interact_pointer_pos()
    {
        s.scrub = Some(at(p.x));
    }
    let mut seek = None;
    if resp.drag_stopped() || resp.clicked() {
        seek = s
            .scrub
            .take()
            .or_else(|| resp.interact_pointer_pos().map(|p| at(p.x)));
    }
    let pos = s.scrub.unwrap_or(pb.position_s);
    let frac = (pos / dur).clamp(0.0, 1.0) as f32;
    let track = Rect::from_center_size(rect.center(), vec2(rect.width(), 4.0));
    let p = ui.painter();
    p.rect_filled(track, CornerRadius::same(2), BORDER_STRONG());
    p.rect_filled(
        Rect::from_min_max(
            track.min,
            pos2(track.left() + track.width() * frac, track.bottom()),
        ),
        CornerRadius::same(2),
        ACCENT(),
    );
    let knob = pos2(track.left() + track.width() * frac, track.center().y);
    let r = if resp.hovered() || resp.dragged() {
        7.0
    } else {
        6.0
    };
    p.circle(knob, r, Color32::WHITE, Stroke::new(2.0, ACCENT()));
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Slider, true, "Playback position")
    });
    resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    seek
}

/// Asks whether to save a document before closing it.
pub fn close_confirmation(s: &mut Studio, ctx: &egui::Context) {
    let Some(uid) = s.confirm_close else { return };
    let Some(doc) = s.doc_by_uid(uid) else {
        s.confirm_close = None;
        return;
    };
    let title = s.tab_title(doc);
    let mut choice = None;
    let modal = egui::Modal::new(Id::new("confirm-close"))
        .frame(popover_frame().inner_margin(Margin::same(20)))
        .backdrop_color(shadow(110))
        .show(ctx, |ui| {
            ui.set_width(380.0);
            ui.horizontal(|ui| {
                widgets::icon(ui, icons::TRIANGLE_ALERT, 18.0, WARN());
                ui.label(
                    RichText::new(format!("Save changes to {title}?"))
                        .font(font_semibold(15.0))
                        .color(TEXT_STRONG()),
                );
            });
            ui.add_space(4.0);
            ui.label(
                RichText::new("Your changes will be lost if you close without saving.")
                    .color(TEXT_DIM()),
            );
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if widgets::button(ui, None, "Save", ButtonKind::Primary).clicked() {
                        choice = Some(0);
                    }
                    if widgets::button(ui, None, "Cancel", ButtonKind::Secondary).clicked() {
                        choice = Some(1);
                    }
                    if widgets::button(ui, None, "Don't Save", ButtonKind::Secondary).clicked() {
                        choice = Some(2);
                    }
                });
            });
        });
    if modal.should_close() && choice.is_none() {
        choice = Some(1);
    }
    match choice {
        Some(0) => {
            s.confirm_close = None;
            s.push(Cmd::Save);
            s.push(Cmd::CloseTab(uid));
        }
        Some(1) => s.confirm_close = None,
        Some(2) => s.push(Cmd::DiscardTab(uid)),
        _ => {}
    }
}

/// Start page shown on an empty canvas.
pub fn welcome(s: &mut Studio, ui: &mut Ui, canvas: Rect) {
    let area = canvas;
    let w = 700.0_f32.min(area.width() - 48.0);
    let rect = Rect::from_center_size(area.center(), vec2(w, 470.0));
    overlay(&ui.ctx().clone(), "welcome", rect, canvas, |ui| {
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
                ui.painter()
                    .rect_filled(r, CornerRadius::same(10), ACCENT());
                widgets::paint_icon(ui, r, icons::WORKFLOW, 24.0, Color32::WHITE);
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new("ruro studio")
                            .font(font_semibold(22.0))
                            .color(TEXT_STRONG()),
                    );
                    ui.label(
                        RichText::new("Build, run and inspect robot dataflow graphs.")
                            .color(TEXT_DIM()),
                    );
                });
            });
            ui.add_space(26.0);
            ui.columns(2, |cols| {
                let ui = &mut cols[0];
                heading(ui, "Start");
                if link_row(ui, icons::FILE_PLUS, "New graph", "") {
                    s.push(Cmd::NewGraph);
                }
                if link_row(ui, icons::FOLDER_OPEN, "Open…", "") {
                    s.push(Cmd::OpenDialog);
                }
                if link_row(ui, icons::SEARCH, "Quick open…", "") {
                    s.push(Cmd::QuickOpen);
                }
                if link_row(ui, icons::COMMAND, "Command palette", "") {
                    s.push(Cmd::CommandPalette);
                }
                let recent: Vec<_> = s.settings.recent.iter().take(5).cloned().collect();
                if !recent.is_empty() {
                    ui.add_space(16.0);
                    heading(ui, "Recent");
                    for p in recent {
                        if link_row(ui, icons::HISTORY, &file_name(&p), &short_dir(&p)) {
                            s.push(Cmd::Open(p));
                        }
                    }
                }
                let examples = example_files();
                if !examples.is_empty() {
                    ui.add_space(16.0);
                    heading(ui, "Examples");
                    for p in examples {
                        let name = p
                            .file_stem()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();
                        if link_row(ui, icons::WORKFLOW, &name, "") {
                            s.push(Cmd::Open(p));
                        }
                    }
                }

                let ui = &mut cols[1];
                heading(ui, "Templates");
                for t in crate::templates::TEMPLATES
                    .iter()
                    .filter(|t| !t.yaml.is_empty())
                {
                    if template_card(ui, t) {
                        s.push(Cmd::NewFromTemplate(t.id));
                    }
                    ui.add_space(6.0);
                }
                ui.add_space(12.0);
                heading(ui, "Shortcuts");
                for (what, keys) in [
                    ("Command palette", "Ctrl K"),
                    ("Run graph", "Ctrl ↵"),
                    ("Undo / Redo", "Ctrl Z / Ctrl ⇧ Z"),
                    ("Duplicate", "Ctrl D"),
                    ("Toggle panel", "Ctrl J"),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(what).color(TEXT_DIM()));
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

fn heading(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .font(font_medium(14.0))
            .color(TEXT_STRONG()),
    );
    ui.add_space(4.0);
}

fn template_card(ui: &mut Ui, t: &crate::templates::Template) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 50.0), Sense::click());
    let fill = if resp.hovered() { HOVER() } else { CARD() };
    ui.painter().rect(
        rect,
        CornerRadius::same(RADIUS + 1),
        fill,
        Stroke::new(
            1.0,
            if resp.hovered() {
                BORDER_STRONG()
            } else {
                BORDER()
            },
        ),
        egui::StrokeKind::Inside,
    );
    let tile = Rect::from_center_size(pos2(rect.left() + 25.0, rect.center().y), Vec2::splat(30.0));
    ui.painter()
        .rect_filled(tile, CornerRadius::same(RADIUS), CARD_HEADER());
    widgets::paint_icon(ui, tile, t.icon, 15.0, ACCENT_TEXT());
    ui.painter().text(
        pos2(tile.right() + 12.0, rect.top() + 16.0),
        Align2::LEFT_CENTER,
        t.title,
        font_medium(13.0),
        TEXT_STRONG(),
    );
    ui.painter().text(
        pos2(tile.right() + 12.0, rect.top() + 34.0),
        Align2::LEFT_CENTER,
        widgets::truncate(t.description, ((rect.width() - 70.0) / 6.3) as usize),
        font(11.5),
        TEXT_FAINT(),
    );
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            true,
            format!("Template {}", t.title),
        )
    });
    resp.on_hover_text(t.description)
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

fn link_row(ui: &mut Ui, glyph: &str, label: &str, detail: &str) -> bool {
    let resp = ui
        .horizontal(|ui| {
            widgets::icon(ui, glyph, 14.0, ACCENT_TEXT());
            let r = ui.add(
                egui::Label::new(RichText::new(label).color(ACCENT_TEXT())).sense(Sense::click()),
            );
            if !detail.is_empty() {
                ui.label(RichText::new(detail).font(font(11.5)).color(TEXT_FAINT()));
            }
            r
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
                    Severity::Info => (icons::INFO, ACCENT_TEXT()),
                    Severity::Success => (icons::CIRCLE_CHECK, OK()),
                    Severity::Warning => (icons::TRIANGLE_ALERT, WARN()),
                    Severity::Error => (icons::CIRCLE_ALERT, ERR()),
                };
                Frame::new()
                    .fill(CARD())
                    .stroke(Stroke::new(1.0, BORDER_STRONG()))
                    .corner_radius(CornerRadius::same(RADIUS + 1))
                    .inner_margin(Margin::symmetric(12, 10))
                    .shadow(egui::Shadow {
                        offset: [0, 6],
                        blur: 20,
                        spread: 0,
                        color: shadow(130),
                    })
                    .show(ui, |ui| {
                        ui.set_width(356.0);
                        ui.horizontal_top(|ui| {
                            widgets::icon(ui, glyph, 15.0, color);
                            ui.add_space(4.0);
                            ui.add(egui::Label::new(RichText::new(&t.text).color(TEXT())).wrap())
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
