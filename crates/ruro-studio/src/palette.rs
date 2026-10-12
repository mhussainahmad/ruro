//! Command palette and quick-input (open / save as), Ctrl+K.

use std::path::{Path, PathBuf};

use egui::{
    Align2, Color32, CornerRadius, Frame, Key, Margin, Rect, RichText, Sense, Stroke, Vec2, pos2,
    vec2,
};

use crate::app::{Activity, AlignOp, Cmd, PanelTab, Studio, example_files, yaml_files};
use crate::icons;
use crate::theme::ThemeKind;
use crate::theme::*;
use crate::widgets;
use ruro_proto::PlaybackCommand;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PaletteMode {
    Commands,
    Open,
    SaveAs,
    /// Pick a graph file to compare the open graph with.
    Compare,
    /// Type the path of a recording on the daemon to replay.
    Replay,
}

pub struct Palette {
    pub mode: PaletteMode,
    pub query: String,
    selected: usize,
    focus: bool,
}

impl Palette {
    pub fn new(mode: PaletteMode) -> Self {
        let query = match mode {
            PaletteMode::Commands | PaletteMode::Replay => String::new(),
            PaletteMode::Open | PaletteMode::SaveAs | PaletteMode::Compare => {
                std::env::current_dir()
                    .map(|d| format!("{}/", d.display()))
                    .unwrap_or_default()
            }
        };
        Self {
            mode,
            query,
            selected: 0,
            focus: true,
        }
    }
}

struct Item {
    icon: &'static str,
    icon_color: Color32,
    title: String,
    detail: String,
    keys: &'static str,
    cmd: Cmd,
}

fn item(
    icon: &'static str,
    title: impl Into<String>,
    detail: impl Into<String>,
    keys: &'static str,
    cmd: Cmd,
) -> Item {
    Item {
        icon,
        icon_color: TEXT_DIM(),
        title: title.into(),
        detail: detail.into(),
        keys,
        cmd,
    }
}

fn commands(s: &Studio) -> Vec<Item> {
    let mut v = vec![
        item(
            icons::PLAY,
            if s.live() {
                "Run: Restart Graph"
            } else {
                "Run: Run Graph"
            },
            "",
            "Ctrl+Enter",
            Cmd::Deploy,
        ),
        item(icons::SQUARE, "Run: Stop", "", "", Cmd::Stop),
        item(
            icons::FILE_PLUS,
            "File: New Graph",
            "",
            "Ctrl+N",
            Cmd::NewGraph,
        ),
        item(
            icons::FOLDER_OPEN,
            "File: Open…",
            "",
            "Ctrl+O",
            Cmd::OpenDialog,
        ),
        item(icons::SAVE, "File: Save", "", "Ctrl+S", Cmd::Save),
        item(
            icons::SAVE,
            "File: Save As…",
            "",
            "Ctrl+Shift+S",
            Cmd::SaveAsDialog,
        ),
    ];
    for p in example_files() {
        let name = p
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        v.push(item(
            icons::WORKFLOW,
            format!("File: Open Example {name}"),
            p.display().to_string(),
            "",
            Cmd::Open(p),
        ));
    }
    for d in s.registry.iter() {
        let mut it = item(
            kind_icon(&d.kind),
            format!("Add Node: {}", d.short_name()),
            d.doc.clone(),
            "",
            Cmd::AddNode(d.kind.clone()),
        );
        it.icon_color = d
            .outputs
            .first()
            .or(d.inputs.first())
            .map_or(TEXT_DIM(), |p| type_color(&p.ty));
        v.push(it);
    }
    for n in s.doc.snarl.nodes() {
        v.push(item(
            icons::CROSSHAIR,
            format!("Go to Node: {}", n.id),
            n.kind.clone(),
            "",
            Cmd::Focus(Some(n.id.clone())),
        ));
        if let Some(d) = s.registry.get(&n.kind) {
            for p in &d.outputs {
                if !s.is_watched(&n.id, &p.name) {
                    let mut it = item(
                        icons::EYE,
                        format!("Watch: {}.{}", n.id, p.name),
                        p.ty.clone(),
                        "",
                        Cmd::Watch {
                            node: n.id.clone(),
                            port: p.name.clone(),
                            ty: p.ty.clone(),
                        },
                    );
                    it.icon_color = type_color(&p.ty);
                    v.push(it);
                }
            }
        }
    }
    if let Some(st) = &s.status {
        for g in st.groups.iter().filter(|g| g.pid.is_some()) {
            v.push(item(
                icons::SKULL,
                format!("Process: Kill {}", g.name),
                "simulate a crash; the supervisor restarts it",
                "",
                Cmd::Kill(g.name.clone()),
            ));
        }
    }
    v.extend([
        item(
            icons::BLOCKS,
            "View: Node Library",
            "",
            "",
            Cmd::Activity(Activity::Library),
        ),
        item(
            icons::WORKFLOW,
            "View: Outline",
            "",
            "",
            Cmd::Activity(Activity::Outline),
        ),
        item(
            icons::CPU,
            "View: Processes",
            "",
            "",
            Cmd::Activity(Activity::Processes),
        ),
        item(
            icons::CIRCLE_ALERT,
            "View: Problems",
            "",
            "",
            Cmd::ShowPanel(PanelTab::Problems),
        ),
        item(
            icons::SCAN_EYE,
            "View: Watch",
            "",
            "",
            Cmd::ShowPanel(PanelTab::Watch),
        ),
        item(
            icons::TERMINAL,
            "View: Output",
            "",
            "",
            Cmd::ShowPanel(PanelTab::Output),
        ),
        item(icons::FOCUS, "View: Fit Graph", "", "Ctrl+0", Cmd::FitView),
        item(
            icons::PANEL_LEFT,
            "View: Toggle Sidebar",
            "",
            "Ctrl+B",
            Cmd::ToggleSidebar,
        ),
        item(
            icons::PANEL_BOTTOM,
            "View: Toggle Panel",
            "",
            "Ctrl+J",
            Cmd::TogglePanel,
        ),
        item(
            icons::PANEL_RIGHT,
            "View: Toggle Inspector",
            "",
            "Ctrl+I",
            Cmd::ToggleInspector,
        ),
        item(
            icons::NETWORK,
            "View: Fleet",
            "daemons on the network",
            "",
            Cmd::Activity(Activity::Fleet),
        ),
        item(
            icons::COMPARE,
            "View: Changes",
            "",
            "",
            Cmd::Activity(Activity::Changes),
        ),
    ]);
    for (k, _) in &s.plugin_panels {
        v.push(item(
            icons::PANEL_BOTTOM,
            format!("View: {}", k.title),
            "",
            "",
            Cmd::ShowPanel(PanelTab::Plugin(k.id)),
        ));
    }
    v.extend(edit_commands(s));
    v.extend(more_commands(s));
    v
}

fn edit_commands(s: &Studio) -> Vec<Item> {
    let mut v = vec![
        item(icons::UNDO, "Edit: Undo", "", "Ctrl+Z", Cmd::Undo),
        item(icons::REDO, "Edit: Redo", "", "Ctrl+Shift+Z", Cmd::Redo),
        item(
            icons::SQUARE_DASHED,
            "Edit: Select All",
            "",
            "Ctrl+A",
            Cmd::SelectAll,
        ),
    ];
    let selected = s.selected_nodes().len();
    if selected > 0 {
        v.extend([
            item(icons::COPY, "Edit: Copy", "", "Ctrl+C", Cmd::Copy),
            item(icons::SCISSORS, "Edit: Cut", "", "Ctrl+X", Cmd::Cut),
            item(
                icons::COPY_PLUS,
                "Edit: Duplicate",
                "",
                "Ctrl+D",
                Cmd::Duplicate,
            ),
            item(
                icons::TRASH_2,
                "Edit: Delete Selection",
                "",
                "Del",
                Cmd::DeleteSelection,
            ),
            item(
                icons::GROUP,
                "Edit: Group into Subgraph",
                "",
                "Ctrl+G",
                Cmd::GroupSelection,
            ),
        ]);
    }
    if let Some(text) = s.clipboard.clone() {
        v.push(item(
            icons::CLIPBOARD_PASTE,
            "Edit: Paste",
            "",
            "Ctrl+V",
            Cmd::Paste(text),
        ));
    }
    if selected > 1 {
        for op in AlignOp::ALL {
            v.push(item(
                op.icon(),
                format!("Arrange: {}", op.label()),
                "",
                "",
                Cmd::Align(op),
            ));
        }
    }
    if let Some(n) = s.focused.and_then(|id| s.doc.snarl.get_node(id)) {
        if n.is_composite() {
            v.push(item(
                icons::OPEN_IN,
                format!("Subgraph: Open {}", n.id),
                "",
                "",
                Cmd::OpenSubgraph(n.id.clone()),
            ));
            v.push(item(
                icons::UNGROUP,
                format!("Subgraph: Ungroup {}", n.id),
                "",
                "",
                Cmd::Ungroup(n.id.clone()),
            ));
        } else if s.registry.get(&n.kind).is_some_and(|d| d.source.is_some()) {
            v.push(item(
                icons::FILE_CODE,
                "Open Node Source",
                n.kind.clone(),
                "",
                Cmd::OpenNodeSource(n.id.clone()),
            ));
        }
    }
    v
}

fn more_commands(s: &Studio) -> Vec<Item> {
    let mut v = Vec::new();
    for t in crate::templates::TEMPLATES {
        v.push(item(
            t.icon,
            format!("File: New from Template: {}", t.title),
            t.description,
            "",
            Cmd::NewFromTemplate(t.id),
        ));
    }
    v.push(item(
        icons::SEARCH,
        "File: Quick Open…",
        "",
        "Ctrl+P",
        Cmd::QuickOpen,
    ));
    for p in s.settings.recent.iter().take(8) {
        v.push(item(
            icons::HISTORY,
            format!("File: Open Recent {}", crate::app::file_name(p)),
            p.display().to_string(),
            "",
            Cmd::Open(p.clone()),
        ));
    }
    if s.doc.path.is_some() {
        v.push(item(
            icons::COMPARE,
            "File: Compare with Saved",
            "",
            "",
            Cmd::CompareWithSaved,
        ));
    }
    v.push(item(
        icons::FILE_DIFF,
        "File: Compare with File…",
        "",
        "",
        Cmd::CompareDialog,
    ));
    if s.compare.is_some() {
        v.push(item(
            icons::X,
            "File: Close Comparison",
            "",
            "",
            Cmd::CloseCompare,
        ));
    }
    v.push(item(
        icons::X,
        "File: Close Tab",
        "",
        "Ctrl+W",
        Cmd::CloseTab(s.doc.uid),
    ));
    for d in s.tabs() {
        if d.uid != s.doc.uid {
            v.push(item(
                icons::WORKFLOW,
                format!("Go to Tab: {}", s.tab_title(d)),
                "",
                "",
                Cmd::SwitchTab(d.uid),
            ));
        }
    }
    let (theme, glyph) = match s.theme {
        ThemeKind::Dark => (ThemeKind::Light, icons::SUN),
        ThemeKind::Light => (ThemeKind::Dark, icons::MOON),
    };
    v.push(item(
        glyph,
        format!(
            "Preferences: {} Theme",
            if theme == ThemeKind::Light {
                "Light"
            } else {
                "Dark"
            }
        ),
        "",
        "",
        Cmd::SetTheme(theme),
    ));
    let recording = s.status.as_ref().is_some_and(|st| st.recording.is_some());
    if recording {
        v.push(item(
            icons::CIRCLE_STOP,
            "Run: Stop Recording",
            "",
            "",
            Cmd::StopRecording,
        ));
    } else if s.running_graph().is_some() {
        v.push(item(
            icons::DISC,
            "Run: Start Recording",
            s.default_recording(),
            "",
            Cmd::StartRecording(s.default_recording()),
        ));
    }
    if s.online() {
        v.push(item(
            icons::FILM,
            "Run: Replay Recording…",
            "",
            "",
            Cmd::ReplayDialog,
        ));
    }
    if let Some(pb) = s.status.as_ref().and_then(|st| st.playback.as_ref()) {
        let (label, cmd) = if pb.paused {
            ("Replay: Resume", PlaybackCommand::Resume)
        } else {
            ("Replay: Pause", PlaybackCommand::Pause)
        };
        v.push(item(icons::PAUSE, label, "", "", Cmd::Playback(cmd)));
        v.push(item(
            icons::X,
            "Replay: Stop",
            "",
            "",
            Cmd::Playback(PlaybackCommand::Stop),
        ));
    }
    let current = s.daemon_name();
    for name in s.fleet.keys() {
        if Some(name.as_str()) != current {
            v.push(item(
                icons::PLUG,
                format!("Fleet: Connect to {name}"),
                "",
                "",
                Cmd::ConnectDaemon(name.clone()),
            ));
        }
    }
    v
}

/// Recordings are files on the daemon, which may be another machine: the
/// user types a path.
fn replay(query: &str) -> Vec<Item> {
    let typed = query.trim();
    if typed.is_empty() {
        return Vec::new();
    }
    vec![item(
        icons::FILM,
        format!("Replay {typed}"),
        "on the connected daemon",
        "Enter",
        Cmd::Playback(PlaybackCommand::Start {
            file: typed.to_owned(),
            rate: 1.0,
        }),
    )]
}

fn files(s: &Studio, query: &str, mode: PaletteMode) -> Vec<Item> {
    let save = mode == PaletteMode::SaveAs;
    let compare = mode == PaletteMode::Compare;
    let mut v = files_in(query, save);
    if compare {
        for it in &mut v {
            if let Cmd::Open(p) = &it.cmd {
                it.cmd = Cmd::CompareWith(p.clone());
                it.title = it.title.replacen("Open ", "Compare with ", 1);
            }
        }
    }
    if mode == PaletteMode::Open {
        let typed = query.trim();
        let recent: Vec<Item> = s
            .settings
            .recent
            .iter()
            .filter(|p| p.display().to_string().starts_with(typed) || typed.ends_with('/'))
            .take(5)
            .map(|p| {
                let mut it = item(
                    icons::HISTORY,
                    crate::app::file_name(p),
                    p.parent()
                        .map(|d| d.display().to_string())
                        .unwrap_or_default(),
                    "recent",
                    Cmd::Open(p.clone()),
                );
                it.icon_color = ACCENT_TEXT();
                it
            })
            .collect();
        v.retain(|it| {
            !recent
                .iter()
                .any(|r| matches!((&r.cmd, &it.cmd), (Cmd::Open(a), Cmd::Open(b)) if a == b))
        });
        let at = usize::from(v.first().is_some_and(|it| it.keys == "Enter"));
        for (i, r) in recent.into_iter().enumerate() {
            v.insert(at + i, r);
        }
    }
    v
}

fn files_in(query: &str, save: bool) -> Vec<Item> {
    let q = Path::new(query);
    let dir = if query.ends_with('/') {
        q.to_path_buf()
    } else {
        q.parent().map(Path::to_path_buf).unwrap_or_default()
    };
    let dir = if dir.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        dir
    };
    let typed = query.trim();
    let mut v = Vec::new();
    if save && !typed.ends_with('/') && !typed.is_empty() {
        v.push(item(
            icons::SAVE,
            format!("Save as {typed}"),
            "",
            "Enter",
            Cmd::SaveAs(PathBuf::from(typed)),
        ));
    }
    let mut seen = Vec::new();
    for p in yaml_files(&dir)
        .into_iter()
        .chain(if save { Vec::new() } else { example_files() })
    {
        if seen.contains(&p) {
            continue;
        }
        seen.push(p.clone());
        let name = p
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let parent = p
            .parent()
            .map(|d| d.display().to_string())
            .unwrap_or_default();
        let cmd = if save {
            Cmd::SaveAs(p.clone())
        } else {
            Cmd::Open(p.clone())
        };
        v.push(item(icons::FILE, name, parent, "", cmd));
    }
    if !save && !typed.is_empty() && !typed.ends_with('/') && Path::new(typed).is_file() {
        v.insert(
            0,
            item(
                icons::FOLDER_OPEN,
                format!("Open {typed}"),
                "",
                "Enter",
                Cmd::Open(PathBuf::from(typed)),
            ),
        );
    }
    v
}

/// Subsequence match score: higher is better, `None` if not all characters appear in order.
fn score(query: &str, text: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let text_l = text.to_lowercase();
    if let Some(i) = text_l.find(&query.to_lowercase()) {
        return Some(1000 - i as i32);
    }
    let mut it = text_l.chars().enumerate();
    let mut score = 0;
    let mut last = 0;
    for qc in query.to_lowercase().chars().filter(|c| !c.is_whitespace()) {
        let (i, _) = it.find(|(_, c)| *c == qc)?;
        score += if i == last + 1 { 8 } else { 1 };
        last = i;
    }
    Some(score)
}

pub fn show(s: &mut Studio, ctx: &egui::Context) {
    let Some(mut pal) = s.palette.take() else {
        return;
    };
    let mut items = match pal.mode {
        PaletteMode::Commands => commands(s),
        PaletteMode::Replay => replay(&pal.query),
        mode => files(s, &pal.query, mode),
    };
    if pal.mode == PaletteMode::Commands {
        let mut scored: Vec<(i32, Item)> = items
            .into_iter()
            .filter_map(|it| score(&pal.query, &it.title).map(|sc| (sc, it)))
            .collect();
        scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        items = scored.into_iter().map(|(_, it)| it).collect();
    }
    pal.selected = pal.selected.min(items.len().saturating_sub(1));

    let (up, down, enter, esc) = ctx.input_mut(|i| {
        (
            i.consume_key(egui::Modifiers::NONE, Key::ArrowUp),
            i.consume_key(egui::Modifiers::NONE, Key::ArrowDown),
            i.consume_key(egui::Modifiers::NONE, Key::Enter),
            i.consume_key(egui::Modifiers::NONE, Key::Escape),
        )
    });
    if up {
        pal.selected = pal.selected.saturating_sub(1);
    }
    if down && pal.selected + 1 < items.len() {
        pal.selected += 1;
    }

    let mut chosen: Option<Cmd> = None;
    let mut close = esc;
    let screen = ctx.content_rect();
    let width = 620.0_f32.min(screen.width() - 40.0);
    let area = egui::Area::new(egui::Id::new("palette"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos2(
            screen.center().x - width / 2.0,
            screen.top() + TITLEBAR_H + 6.0,
        ))
        .show(ctx, |ui| {
            Frame::new()
                .fill(CARD())
                .stroke(Stroke::new(1.0, BORDER_STRONG()))
                .corner_radius(CornerRadius::same(RADIUS_LG))
                .shadow(egui::Shadow {
                    offset: [0, 10],
                    blur: 32,
                    spread: 0,
                    color: shadow(160),
                })
                .inner_margin(Margin::same(6))
                .show(ui, |ui| {
                    ui.set_width(width - 12.0);
                    let hint = match pal.mode {
                        PaletteMode::Commands => {
                            "Type a command, a node to add, or a port to watch"
                        }
                        PaletteMode::Open => "Path of a graph to open",
                        PaletteMode::SaveAs => "Path to save the graph to",
                        PaletteMode::Compare => "Path of a graph to compare with",
                        PaletteMode::Replay => "Path of a recording on the daemon, e.g. run.mcap",
                    };
                    let field = Frame::new()
                        .fill(FIELD())
                        .stroke(Stroke::new(1.0, ACCENT()))
                        .corner_radius(CornerRadius::same(RADIUS))
                        .inner_margin(Margin::symmetric(8, 5))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                widgets::icon(
                                    ui,
                                    if pal.mode == PaletteMode::Commands {
                                        icons::COMMAND
                                    } else {
                                        icons::FOLDER_OPEN
                                    },
                                    14.0,
                                    TEXT_DIM(),
                                );
                                ui.add(
                                    egui::TextEdit::singleline(&mut pal.query)
                                        .frame(Frame::NONE)
                                        .font(font(14.0))
                                        .hint_text(RichText::new(hint).color(TEXT_FAINT()))
                                        .desired_width(f32::INFINITY),
                                )
                            })
                            .inner
                        })
                        .inner;
                    if pal.focus {
                        field.request_focus();
                        pal.focus = false;
                    }
                    if field.changed() {
                        pal.selected = 0;
                    }
                    ui.add_space(4.0);
                    if items.is_empty() {
                        ui.add_space(4.0);
                        ui.label(RichText::new("  No matching results").color(TEXT_FAINT()));
                        ui.add_space(4.0);
                    }
                    egui::ScrollArea::vertical()
                        .max_height(420.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for (i, it) in items.iter().enumerate() {
                                let (rect, resp) = ui.allocate_exact_size(
                                    vec2(ui.available_width(), 28.0),
                                    Sense::click(),
                                );
                                let sel = i == pal.selected;
                                if sel {
                                    ui.painter().rect_filled(
                                        rect,
                                        CornerRadius::same(RADIUS),
                                        SELECTED(),
                                    );
                                    if up || down {
                                        ui.scroll_to_rect(rect, None);
                                    }
                                } else if resp.hovered() {
                                    ui.painter().rect_filled(
                                        rect,
                                        CornerRadius::same(RADIUS),
                                        HOVER(),
                                    );
                                }
                                let p = ui.painter();
                                widgets::paint_icon(
                                    ui,
                                    Rect::from_center_size(
                                        pos2(rect.left() + 16.0, rect.center().y),
                                        Vec2::splat(16.0),
                                    ),
                                    it.icon,
                                    14.0,
                                    it.icon_color,
                                );
                                let title = widgets::galley(
                                    ui,
                                    it.title.clone(),
                                    font(13.0),
                                    TEXT_STRONG(),
                                );
                                let tw = title.size().x;
                                p.galley(
                                    pos2(
                                        rect.left() + 32.0,
                                        rect.center().y - title.size().y / 2.0,
                                    ),
                                    title,
                                    TEXT_STRONG(),
                                );
                                if !it.detail.is_empty() {
                                    let room =
                                        ((rect.width() - tw - 150.0) / 6.5).max(0.0) as usize;
                                    if room > 6 {
                                        p.text(
                                            pos2(rect.left() + 42.0 + tw, rect.center().y),
                                            Align2::LEFT_CENTER,
                                            widgets::truncate(&it.detail, room),
                                            font(12.0),
                                            TEXT_FAINT(),
                                        );
                                    }
                                }
                                if !it.keys.is_empty() {
                                    p.text(
                                        pos2(rect.right() - 10.0, rect.center().y),
                                        Align2::RIGHT_CENTER,
                                        it.keys,
                                        font(11.5),
                                        TEXT_DIM(),
                                    );
                                }
                                if resp.clicked() {
                                    chosen = Some(it.cmd.clone());
                                }
                            }
                        });
                });
        });
    if enter {
        match items.get(pal.selected) {
            Some(it) => chosen = Some(it.cmd.clone()),
            None if pal.mode == PaletteMode::SaveAs && !pal.query.trim().is_empty() => {
                chosen = Some(Cmd::SaveAs(PathBuf::from(pal.query.trim())))
            }
            None => {}
        }
    }
    // Clicking outside dismisses, like any popup.
    if ctx.input(|i| i.pointer.any_pressed())
        && !area.response.contains_pointer()
        && !area.response.hovered()
    {
        let inside = ctx
            .input(|i| i.pointer.interact_pos())
            .is_some_and(|p| area.response.rect.contains(p));
        if !inside {
            close = true;
        }
    }
    match chosen {
        Some(cmd) => s.push(cmd),
        None if !close => s.palette = Some(pal),
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::score;

    #[test]
    fn substring_beats_subsequence() {
        assert!(score("blur", "Add Node: blur").unwrap() > score("blr", "Add Node: blur").unwrap());
        assert!(score("xyz", "Add Node: blur").is_none());
        assert_eq!(score("", "anything"), Some(0));
    }

    #[test]
    fn earlier_match_ranks_higher() {
        assert!(score("run", "Run: Stop").unwrap() > score("run", "View: Run things").unwrap());
    }
}
