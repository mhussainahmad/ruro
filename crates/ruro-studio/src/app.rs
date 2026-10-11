//! Studio state and behaviour. Layout lives in the `shell`, `sidebar`,
//! `inspector`, `panel` and `palette` modules; every user action, whichever
//! surface triggers it, goes through [`Cmd`].

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::{Id, Key, KeyboardShortcut, Modifiers, Ui};
use egui_snarl::NodeId;
use egui_snarl::ui::{SnarlStyle, SnarlWidget};
use ruro_graph::{Graph, Issue, Registry};
use ruro_proto::{DaemonStatus, GroupState, LogLine, NodeMetrics};
use serde_json::Value;

use crate::canvas::{self, Viewer};
use crate::link::{Command, Event, Link};
use crate::model::Doc;
use crate::palette::{Palette, PaletteMode};
use crate::watch::Watch;
use crate::{inspector, panel, shell, sidebar};

pub const CANVAS_ID: &str = "ruro-canvas";
const MAX_LOGS: usize = 5000;

pub struct Options {
    pub endpoint: String,
    pub daemon: Option<String>,
    pub spawn_daemon: bool,
    pub file: Option<PathBuf>,
}

pub enum Conn {
    Connecting,
    Online { daemon: String, endpoint: String },
    Offline(String),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Activity {
    Library,
    Outline,
    Processes,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PanelTab {
    Problems,
    Watch,
    Output,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Severity {
    Info,
    Success,
    Warning,
    Error,
}

pub struct Toast {
    pub severity: Severity,
    pub text: String,
    pub until: Instant,
}

/// Everything a user can do. Menus, shortcuts, the command palette and
/// widgets all enqueue these; [`Studio::apply`] performs them after drawing.
#[derive(Clone, Debug)]
pub enum Cmd {
    NewGraph,
    OpenDialog,
    Open(PathBuf),
    Save,
    SaveAsDialog,
    SaveAs(PathBuf),
    Deploy,
    Stop,
    Kill(String),
    AddNode(String),
    Watch {
        node: String,
        port: String,
        ty: String,
    },
    Unwatch {
        node: String,
        port: String,
    },
    Focus(Option<String>),
    DeleteFocused,
    Activity(Activity),
    ToggleSidebar,
    ToggleInspector,
    TogglePanel,
    ShowPanel(PanelTab),
    CommandPalette,
    ClearOutput,
    FitView,
}

pub struct Studio {
    pub(crate) link: Link,
    pub(crate) conn: Conn,
    pub(crate) registry: Registry,
    pub(crate) doc: Doc,
    pub(crate) style: SnarlStyle,
    pub(crate) status: Option<DaemonStatus>,
    pub(crate) metrics: HashMap<String, NodeMetrics>,
    /// `doc.structure_rev` at the last successful deploy from this window.
    pub(crate) deployed_rev: Option<u64>,
    pub(crate) pending_deploy: Option<u64>,
    pub(crate) watches: Vec<Watch>,
    pub(crate) logs: VecDeque<LogLine>,
    pub(crate) log_filter: String,
    pub(crate) issues: Vec<Issue>,
    pub(crate) activity: Activity,
    pub(crate) sidebar_open: bool,
    pub(crate) inspector_open: bool,
    pub(crate) panel_open: bool,
    pub(crate) panel_tab: PanelTab,
    pub(crate) toasts: Vec<Toast>,
    pub(crate) palette: Option<Palette>,
    pub(crate) library_filter: String,
    pub(crate) rename: Option<(NodeId, String)>,
    pub(crate) focused: Option<NodeId>,
    /// Collapsed inspector and sidebar sections, by title.
    pub(crate) collapsed: HashSet<String>,
    pub(crate) queue: Vec<Cmd>,
    /// Fit the canvas view to the graph on the next frame.
    pub(crate) fit_pending: bool,
    /// Screen rect of every node drawn last frame.
    pub(crate) node_rects: Vec<(NodeId, egui::Rect)>,
    want_running_graph: bool,
    title: String,
}

impl Studio {
    pub fn new(cc: &eframe::CreationContext<'_>, opts: Options) -> Self {
        crate::theme::install(&cc.egui_ctx);
        let link = if opts.endpoint.is_empty() {
            Link::disabled()
        } else {
            Link::start(
                opts.endpoint,
                opts.daemon,
                opts.spawn_daemon,
                cc.egui_ctx.clone(),
            )
        };
        let mut studio = Self {
            link,
            conn: Conn::Connecting,
            registry: local_registry(),
            doc: Doc::new("untitled"),
            style: canvas::style(),
            status: None,
            metrics: HashMap::new(),
            deployed_rev: None,
            pending_deploy: None,
            watches: Vec::new(),
            logs: VecDeque::new(),
            log_filter: String::new(),
            issues: Vec::new(),
            activity: Activity::Library,
            sidebar_open: true,
            inspector_open: true,
            panel_open: true,
            panel_tab: PanelTab::Watch,
            toasts: Vec::new(),
            palette: None,
            library_filter: String::new(),
            rename: None,
            focused: None,
            collapsed: HashSet::new(),
            queue: Vec::new(),
            fit_pending: true,
            node_rects: Vec::new(),
            want_running_graph: true,
            title: String::new(),
        };
        if let Some(path) = opts.file {
            studio.open(&path);
        }
        studio
    }

    pub fn push(&mut self, cmd: Cmd) {
        self.queue.push(cmd);
    }

    // ----- state queries ---------------------------------------------------

    pub fn online(&self) -> bool {
        matches!(self.conn, Conn::Online { .. })
    }

    /// True while the open graph is the one running on the daemon.
    pub fn live(&self) -> bool {
        self.online() && self.running_graph() == Some(self.doc.name.as_str())
    }

    pub fn running_graph(&self) -> Option<&str> {
        self.status.as_ref().and_then(|s| s.graph.as_deref())
    }

    /// The graph has structural edits that the running copy does not have.
    pub fn undeployed_changes(&self) -> bool {
        self.live()
            && self
                .deployed_rev
                .is_some_and(|r| r != self.doc.structure_rev)
    }

    pub fn is_watched(&self, node: &str, port: &str) -> bool {
        self.watches
            .iter()
            .any(|w| w.node == node && w.port == port)
    }

    pub fn section_open(&self, title: &str) -> bool {
        !self.collapsed.contains(title)
    }

    pub fn toggle_section(&mut self, title: &str) {
        if !self.collapsed.remove(title) {
            self.collapsed.insert(title.to_owned());
        }
    }

    pub fn toast(&mut self, severity: Severity, text: impl Into<String>) {
        let secs = match severity {
            Severity::Error => 8,
            Severity::Warning => 6,
            _ => 4,
        };
        self.toasts.push(Toast {
            severity,
            text: text.into(),
            until: Instant::now() + Duration::from_secs(secs),
        });
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
    }

    // ----- commands --------------------------------------------------------

    fn apply(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::NewGraph => {
                self.replace_doc(Doc::new("untitled"));
                self.want_running_graph = false;
            }
            Cmd::OpenDialog => self.palette = Some(Palette::new(PaletteMode::Open)),
            Cmd::Open(p) => self.open(&p),
            Cmd::Save => self.save(None),
            Cmd::SaveAsDialog => self.palette = Some(Palette::new(PaletteMode::SaveAs)),
            Cmd::SaveAs(p) => self.save(Some(p)),
            Cmd::Deploy => self.deploy(),
            Cmd::Stop => self.link.send(Command::Stop),
            Cmd::Kill(group) => self.link.send(Command::Kill(group)),
            Cmd::AddNode(kind) => {
                if let Some(d) = self.registry.get(&kind).cloned() {
                    let n = self.doc.snarl.nodes().count() as f32;
                    let pos = egui::pos2(120.0 + (n % 4.0) * 40.0, 120.0 + (n % 5.0) * 36.0);
                    let id = self.doc.add_node(&d, pos);
                    self.focused = Some(id);
                }
            }
            Cmd::Watch { node, port, ty } => {
                if !self.is_watched(&node, &port) {
                    self.link.send(Command::Watch {
                        node: node.clone(),
                        port: port.clone(),
                    });
                    self.watches.push(Watch::new(node, port, ty));
                }
                self.panel_open = true;
                self.panel_tab = PanelTab::Watch;
            }
            Cmd::Unwatch { node, port } => {
                self.watches.retain(|w| !(w.node == node && w.port == port));
                self.link.send(Command::Unwatch { node, port });
            }
            Cmd::Focus(id) => {
                self.focused = id.and_then(|id| self.doc.find(&id));
                self.inspector_open |= self.focused.is_some();
            }
            Cmd::DeleteFocused => {
                if let Some(id) = self.focused.take()
                    && self.doc.snarl.get_node(id).is_some()
                {
                    self.doc.snarl.remove_node(id);
                    self.doc.touch_structure();
                }
            }
            Cmd::Activity(a) => {
                if self.sidebar_open && self.activity == a {
                    self.sidebar_open = false;
                } else {
                    self.activity = a;
                    self.sidebar_open = true;
                }
            }
            Cmd::ToggleSidebar => self.sidebar_open = !self.sidebar_open,
            Cmd::ToggleInspector => self.inspector_open = !self.inspector_open,
            Cmd::TogglePanel => self.panel_open = !self.panel_open,
            Cmd::ShowPanel(tab) => {
                self.panel_open = true;
                self.panel_tab = tab;
            }
            Cmd::CommandPalette => self.palette = Some(Palette::new(PaletteMode::Commands)),
            Cmd::ClearOutput => self.logs.clear(),
            Cmd::FitView => self.fit_pending = true,
        }
    }

    fn open(&mut self, path: &Path) {
        match Graph::load(path) {
            Ok(graph) => {
                let (doc, warnings) =
                    Doc::from_graph(&graph, &self.registry, Some(path.to_path_buf()));
                self.replace_doc(doc);
                self.want_running_graph = false;
                for w in warnings {
                    self.toast(Severity::Warning, w);
                }
            }
            Err(e) => self.toast(
                Severity::Error,
                format!("Cannot open {}: {e}", path.display()),
            ),
        }
    }

    fn replace_doc(&mut self, doc: Doc) {
        for w in self.watches.drain(..) {
            self.link.send(Command::Unwatch {
                node: w.node,
                port: w.port,
            });
        }
        self.doc = doc;
        self.deployed_rev = None;
        self.rename = None;
        self.focused = None;
        self.fit_pending = true;
    }

    fn save(&mut self, path: Option<PathBuf>) {
        let Some(path) = path.or_else(|| self.doc.path.clone()) else {
            self.palette = Some(Palette::new(PaletteMode::SaveAs));
            return;
        };
        match self.doc.to_graph(&self.registry).save(&path) {
            Ok(()) => {
                self.doc.path = Some(path.clone());
                self.doc.dirty = false;
                self.toast(Severity::Info, format!("Saved {}", path.display()));
            }
            Err(e) => self.toast(Severity::Error, format!("Cannot save: {e}")),
        }
    }

    fn deploy(&mut self) {
        let graph = self.doc.to_graph(&self.registry);
        if graph.nodes.is_empty() {
            self.toast(
                Severity::Warning,
                "The graph is empty. Add nodes from the library or with Ctrl+K.",
            );
            return;
        }
        self.issues = ruro_graph::validate(&graph, &self.registry);
        if !self.issues.is_empty() {
            self.panel_open = true;
            self.panel_tab = PanelTab::Problems;
            self.toast(
                Severity::Error,
                format!("Fix {} problem(s) before running", self.issues.len()),
            );
            return;
        }
        if !self.online() {
            self.toast(Severity::Error, "Not connected to a daemon");
            return;
        }
        match graph.to_yaml() {
            Ok(yaml) => {
                self.link.send(Command::Deploy(yaml));
                self.pending_deploy = Some(self.doc.structure_rev);
            }
            Err(e) => self.toast(Severity::Error, e.to_string()),
        }
    }

    pub fn set_param(&mut self, node: NodeId, name: &str, value: Value) {
        let n = &mut self.doc.snarl[node];
        n.params.insert(name.to_owned(), value.clone());
        let id = n.id.clone();
        self.doc.dirty = true;
        if self.live() && self.metrics.contains_key(&id) {
            self.link.send(Command::SetParam {
                node: id,
                name: name.to_owned(),
                value: value.to_string(),
            });
        }
    }

    // ----- events ----------------------------------------------------------

    fn handle_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.link.events.try_recv() {
            self.handle_event(ctx, event);
        }
    }

    pub(crate) fn handle_event(&mut self, ctx: &egui::Context, event: Event) {
        match event {
            Event::Connected { daemon, endpoint } => {
                self.conn = Conn::Online { daemon, endpoint };
                for w in &self.watches {
                    self.link.send(Command::Watch {
                        node: w.node.clone(),
                        port: w.port.clone(),
                    });
                }
            }
            Event::Offline(reason) => {
                self.conn = Conn::Offline(reason);
                self.status = None;
                self.metrics.clear();
            }
            Event::Registry(r) => {
                self.registry = r;
                if self.want_running_graph && self.doc.snarl.nodes().next().is_none() {
                    self.link.send(Command::FetchGraph);
                }
            }
            Event::Status(s) => {
                let running: HashSet<&str> = s
                    .groups
                    .iter()
                    .filter(|g| g.state == GroupState::Running)
                    .flat_map(|g| g.nodes.iter().map(String::as_str))
                    .collect();
                self.metrics.retain(|id, _| running.contains(id.as_str()));
                self.status = Some(s);
            }
            Event::Metrics(m) => {
                for n in m.nodes {
                    self.metrics.insert(n.id.clone(), n);
                }
            }
            Event::Tap { node, port, frame } => {
                if let Some(w) = self
                    .watches
                    .iter_mut()
                    .find(|w| w.node == node && w.port == port)
                {
                    w.push(ctx, frame);
                }
            }
            Event::Log(line) => {
                if self.logs.len() == MAX_LOGS {
                    self.logs.pop_front();
                }
                self.logs.push_back(line);
            }
            Event::Reply { op, result } => match (op, result) {
                ("deploy", Ok(_)) => {
                    self.deployed_rev = self.pending_deploy.take();
                    self.toast(Severity::Success, format!("Running {}", self.doc.name));
                }
                ("stop", Ok(_)) => {
                    self.deployed_rev = None;
                    self.metrics.clear();
                    self.toast(Severity::Info, "Stopped");
                }
                ("kill", Ok(msg)) => self.toast(Severity::Warning, capitalize(&msg)),
                (_, Ok(_)) => {}
                (op, Err(e)) => {
                    self.pending_deploy = None;
                    self.toast(Severity::Error, format!("{} failed: {e}", capitalize(op)));
                }
            },
            Event::RunningGraph(yaml) => {
                self.want_running_graph = false;
                if self.doc.snarl.nodes().next().is_none()
                    && let Ok(graph) = Graph::from_yaml(&yaml)
                {
                    let (doc, _) = Doc::from_graph(&graph, &self.registry, None);
                    let rev = doc.structure_rev;
                    self.replace_doc(doc);
                    self.deployed_rev = Some(rev);
                    self.toast(
                        Severity::Info,
                        format!("Attached to running graph {}", graph.name),
                    );
                }
            }
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let cmd = |key| KeyboardShortcut::new(Modifiers::COMMAND, key);
        let cmd_shift = |key| KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, key);
        let bindings = [
            (cmd(Key::K), Cmd::CommandPalette),
            (cmd_shift(Key::P), Cmd::CommandPalette),
            (cmd_shift(Key::S), Cmd::SaveAsDialog),
            (cmd(Key::S), Cmd::Save),
            (cmd(Key::O), Cmd::OpenDialog),
            (cmd(Key::N), Cmd::NewGraph),
            (cmd(Key::Enter), Cmd::Deploy),
            (cmd(Key::B), Cmd::ToggleSidebar),
            (cmd(Key::J), Cmd::TogglePanel),
            (cmd(Key::I), Cmd::ToggleInspector),
            (cmd(Key::Num0), Cmd::FitView),
        ];
        for (shortcut, c) in bindings {
            if ctx.input_mut(|i| i.consume_shortcut(&shortcut)) {
                self.push(c);
            }
        }
        if self.palette.is_none() && !ctx.egui_wants_keyboard_input() {
            if ctx.input(|i| i.key_pressed(Key::Delete)) {
                self.push(Cmd::DeleteFocused);
            }
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.focused = None;
            }
        }
    }

    // ----- frame -----------------------------------------------------------

    fn canvas(&mut self, ui: &mut Ui) {
        let area = ui.available_rect_before_wrap();
        let watched: HashSet<(String, String)> = self.watches.iter().map(Watch::key).collect();
        let live = self.live();
        let mut viewer = Viewer {
            registry: &self.registry,
            metrics: &self.metrics,
            watched: &watched,
            live,
            focused: self.focused,
            hovered: None,
            fit: std::mem::take(&mut self.fit_pending).then_some(area),
            rects: Vec::new(),
            actions: Vec::new(),
        };
        let resp = SnarlWidget::new()
            .id(Id::new(CANVAS_ID))
            .style(self.style)
            .show(&mut self.doc.snarl, &mut viewer, ui);
        let hovered = viewer.hovered;
        self.node_rects = std::mem::take(&mut viewer.rects);
        let actions = std::mem::take(&mut viewer.actions);
        let over_canvas = ui
            .input(|i| i.pointer.interact_pos())
            .is_some_and(|p| area.contains(p));
        if ui.input(|i| i.pointer.primary_clicked()) && over_canvas && resp.contains_pointer() {
            self.focused = hovered;
            if hovered.is_some() {
                self.inspector_open = true;
            }
        }
        for a in actions {
            match a {
                canvas::Action::Cmd(c) => self.push(c),
                canvas::Action::Toast(t) => self.toast(Severity::Warning, t),
                canvas::Action::StructureChanged => self.doc.touch_structure(),
            }
        }
        if self.doc.snarl.nodes().next().is_none() {
            shell::welcome(self, ui);
        } else {
            shell::canvas_controls(self, ui, area);
        }
    }

    /// Draws one frame. Shared by the native app and the UI tests.
    pub fn frame(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        self.handle_events(&ctx);
        self.issues = ruro_graph::validate(&self.doc.to_graph(&self.registry), &self.registry);
        self.focused = self
            .focused
            .filter(|&id| self.doc.snarl.get_node(id).is_some());
        self.shortcuts(&ctx);

        shell::title_bar(self, ui);
        shell::status_bar(self, ui);
        shell::activity_bar(self, ui);
        if self.sidebar_open {
            sidebar::show(self, ui);
        }
        if self.inspector_open {
            inspector::show(self, ui);
        }
        if self.panel_open {
            panel::show(self, ui);
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(crate::theme::EDITOR))
            .show(ui, |ui| {
                shell::editor_tabs(self, ui);
                self.canvas(ui);
            });

        crate::palette::show(self, &ctx);
        shell::toasts(self, &ctx);

        for cmd in std::mem::take(&mut self.queue) {
            self.apply(cmd);
        }

        let title = format!(
            "{}{} — ruro studio",
            self.doc.name,
            if self.doc.dirty { " ●" } else { "" }
        );
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
        if self.live() || !self.toasts.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}

impl eframe::App for Studio {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }
}

/// Node types from the local host binary, so editing works before a daemon connects.
fn local_registry() -> Registry {
    let host = std::env::current_exe()
        .ok()
        .map(|e| e.with_file_name("ruro-host"))
        .filter(|p| p.exists())
        .unwrap_or_else(|| "ruro-host".into());
    std::process::Command::new(host)
        .arg("describe")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| serde_json::from_slice(&o.stdout).ok())
        .unwrap_or_default()
}

/// `*.yaml` files in `dir`, sorted.
pub fn yaml_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "yaml" || x == "yml"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files.truncate(200);
    files
}

/// `examples/*.yaml` in the working directory or above the executable.
pub fn example_files() -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from("examples")];
    if let Ok(exe) = std::env::current_exe() {
        dirs.extend(exe.ancestors().skip(1).take(5).map(|a| a.join("examples")));
    }
    dirs.into_iter()
        .map(|d| yaml_files(&d))
        .find(|f| !f.is_empty())
        .unwrap_or_default()
}

pub fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
