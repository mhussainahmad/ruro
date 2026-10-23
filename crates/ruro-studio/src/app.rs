//! Studio state and behaviour. Layout lives in the `shell`, `sidebar`,
//! `inspector`, `panel` and `palette` modules; every user action, whichever
//! surface triggers it, goes through [`Cmd`].
//!
//! Behaviour is split by concern: documents and files (`docs`), editing
//! (`edit`), the connection to a daemon (`live`) and overlays (`variant`).

mod docs;
mod edit;
mod live;
mod variant;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::{Id, Key, KeyboardShortcut, Modifiers, Ui};
use egui_snarl::NodeId;
use egui_snarl::ui::{SnarlStyle, SnarlWidget};
use ruro_graph::{Issue, Registry};
use ruro_proto::{DaemonStatus, LogLine, NodeMetrics, PlaybackCommand};

use crate::canvas::{self, Viewer};
use crate::diff::Compare;
use crate::link::Link;
use crate::model::Doc;
use crate::palette::{Palette, PaletteMode};
use crate::settings::{Recovery, Settings, Store};
use crate::theme::ThemeKind;
use crate::watch::Watch;
use crate::{dialogs, inspector, panel, shell, sidebar};

pub use edit::AlignOp;

pub const CANVAS_ID: &str = "ruro-canvas";
const MAX_LOGS: usize = 5000;
/// Daemons not heard from for this long are shown as unreachable.
pub const FLEET_STALE: Duration = Duration::from_secs(5);

pub struct Options {
    pub endpoint: String,
    /// The endpoint was given (`--connect`); otherwise the one last chosen
    /// with Connect to Daemon is used, if any.
    pub endpoint_given: bool,
    pub daemon: Option<String>,
    /// TLS certificates and access token for reaching the daemon.
    pub security: ruro_proto::Security,
    pub spawn_daemon: bool,
    pub file: Option<PathBuf>,
    /// Settings and autosave locations. `None` keeps everything in memory.
    pub store: Option<Store>,
    /// Use the desktop's file dialogs (through the XDG portal) for Open and Save As.
    pub native_dialogs: bool,
}

impl Options {
    /// No daemon, no persistence, no native dialogs: for tests and previews.
    #[cfg(test)]
    pub fn offline() -> Self {
        Self {
            endpoint: String::new(),
            endpoint_given: true,
            daemon: None,
            security: Default::default(),
            spawn_daemon: false,
            file: None,
            store: None,
            native_dialogs: false,
        }
    }
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
    Fleet,
    Changes,
    Ros2,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PanelTab {
    Problems,
    Watch,
    Output,
    /// A panel contributed by a plugin, by id.
    Plugin(&'static str),
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
/// What the Connect to Daemon dialog holds while open.
#[derive(Default)]
pub struct ConnectForm {
    pub endpoint: String,
    /// CA certificate for `tls/` endpoints; empty for none.
    pub tls_ca: String,
    /// Client certificate and its key, for daemons that require them
    /// (mutual TLS); empty for none.
    pub tls_cert: String,
    pub tls_key: String,
    /// Access token; empty for none.
    pub token: String,
}

#[derive(Clone, Debug)]
pub enum Cmd {
    // Connection.
    ConnectDialog,
    Connect {
        endpoint: String,
        tls_ca: Option<PathBuf>,
        /// Client certificate and key, for mutual TLS.
        client: Option<(PathBuf, PathBuf)>,
        token: Option<String>,
    },
    // Documents.
    NewGraph,
    NewFromTemplate(&'static str),
    OpenDialog,
    QuickOpen,
    Open(PathBuf),
    Save,
    SaveAsDialog,
    SaveAs(PathBuf),
    SwitchTab(u64),
    CloseTab(u64),
    /// Closes without asking, discarding changes.
    DiscardTab(u64),
    NextTab,
    ClearRecent,
    RecoverAutosaves,
    DiscardAutosaves,
    // Editing.
    Undo,
    Redo,
    Copy,
    Cut,
    Paste(String),
    Duplicate,
    SelectAll,
    DeleteSelection,
    DeleteNode(String),
    Align(AlignOp),
    AddNode(String),
    /// Adds a node of `kind` with parameter values already set.
    AddConfiguredNode {
        kind: String,
        params: Vec<(String, serde_json::Value)>,
    },
    RosDiscover,
    Focus(Option<String>),
    GroupSelection,
    Ungroup(String),
    OpenSubgraph(String),
    OpenNodeSource(String),
    // Comparison.
    CompareWithSaved,
    CompareDialog,
    CompareWith(PathBuf),
    CloseCompare,
    /// Shows the graph with these overlays applied (none: the graph itself).
    SetVariant(Vec<String>),
    // Running.
    Deploy,
    Stop,
    /// Engages (`true`) or releases the emergency stop.
    Estop(bool),
    Kill(String),
    StartRecording(String),
    StopRecording,
    ReplayDialog,
    Playback(PlaybackCommand),
    ConnectDaemon(String),
    Watch {
        node: String,
        port: String,
        ty: String,
    },
    Unwatch {
        node: String,
        port: String,
    },
    // View.
    Activity(Activity),
    ToggleSidebar,
    ToggleInspector,
    TogglePanel,
    ShowPanel(PanelTab),
    CommandPalette,
    ClearOutput,
    FitView,
    SetTheme(ThemeKind),
}

impl Cmd {
    /// Commands that keep an overlay on the canvas. Every other command acts
    /// on the document itself, which is put back first.
    fn keeps_variant(&self) -> bool {
        matches!(
            self,
            Cmd::SetVariant(_)
                | Cmd::QuickOpen
                | Cmd::ClearRecent
                | Cmd::Copy
                | Cmd::SelectAll
                | Cmd::Focus(_)
                | Cmd::RosDiscover
                | Cmd::OpenNodeSource(_)
                | Cmd::CloseCompare
                | Cmd::Deploy
                | Cmd::Stop
                | Cmd::Estop(_)
                | Cmd::Kill(_)
                | Cmd::StartRecording(_)
                | Cmd::StopRecording
                | Cmd::ReplayDialog
                | Cmd::Playback(_)
                | Cmd::ConnectDaemon(_)
                | Cmd::Watch { .. }
                | Cmd::Unwatch { .. }
                | Cmd::Activity(_)
                | Cmd::ToggleSidebar
                | Cmd::ToggleInspector
                | Cmd::TogglePanel
                | Cmd::ShowPanel(_)
                | Cmd::CommandPalette
                | Cmd::ClearOutput
                | Cmd::FitView
                | Cmd::SetTheme(_)
        )
    }
}

/// A daemon seen on the network.
#[derive(Clone)]
pub struct FleetEntry {
    pub status: DaemonStatus,
    pub seen: Instant,
}

/// What the "record" popover in the title bar holds while open.
pub struct RecordDraft {
    pub path: String,
}

pub struct Studio {
    pub(crate) link: Link,
    pub(crate) conn: Conn,
    /// The endpoint and credentials the link uses, to connect again.
    pub(crate) endpoint: String,
    pub(crate) security: ruro_proto::Security,
    pub(crate) spawn_daemon: bool,
    pub(crate) connect_form: Option<ConnectForm>,
    pub(crate) registry: Registry,
    /// The document in the active tab.
    pub(crate) doc: Doc,
    /// The other open documents.
    pub(crate) parked: Vec<Doc>,
    /// Tab order, by document uid.
    pub(crate) tab_order: Vec<u64>,
    pub(crate) style: SnarlStyle,
    pub(crate) status: Option<DaemonStatus>,
    pub(crate) metrics: HashMap<String, NodeMetrics>,
    pub(crate) fleet: BTreeMap<String, FleetEntry>,
    /// The peers the link was last told to follow.
    pub(crate) peers_sent: Option<BTreeSet<String>>,
    pub(crate) ros: crate::ros::RosBrowser,
    /// When the current recording started, as seen by this window.
    pub(crate) recording_since: Option<Instant>,
    pub(crate) record_draft: Option<RecordDraft>,
    /// Where the record button was drawn, to place its popover.
    pub(crate) record_anchor: egui::Rect,
    /// Playback position while the user drags the seek bar.
    pub(crate) scrub: Option<f64>,
    /// Panels contributed by plugins, created once.
    pub(crate) plugin_panels: Vec<(
        &'static crate::plugin::PanelKind,
        Box<dyn crate::plugin::StudioPanel>,
    )>,
    /// `doc.structure_rev` at the last successful deploy from this window.
    pub(crate) deployed_rev: Option<u64>,
    pub(crate) pending_deploy: Option<u64>,
    /// Overlays of the last successful deploy, and of the one in flight.
    pub(crate) deployed_variant: Vec<String>,
    pub(crate) pending_variant: Vec<String>,
    /// Name of the graph last deployed from this window.
    pub(crate) deployed_graph: Option<String>,
    pub(crate) watches: Vec<Watch>,
    pub(crate) logs: VecDeque<LogLine>,
    pub(crate) log_filter: String,
    pub(crate) issues: Vec<Issue>,
    issues_at: Option<(u64, u64, Instant)>,
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
    /// Screen positions of every port drawn last frame.
    pub(crate) pin_pos: canvas::PinPositions,
    /// Nodes selected on the canvas (shift-click or shift-drag), last frame.
    pub(crate) selection: Vec<NodeId>,
    /// Nodes selected by the studio itself (select all, paste), drawn highlighted.
    pub(crate) marked: HashSet<NodeId>,
    /// Graph-to-screen transform of the canvas, last frame.
    pub(crate) canvas_transform: egui::emath::TSTransform,
    /// Screen rect of the canvas, last frame.
    pub(crate) canvas_rect: egui::Rect,
    /// Comparison shown on the canvas, by document uid.
    pub(crate) compare: Option<Compare>,
    /// Overlays applied to the active document on the canvas.
    pub(crate) variant: Option<variant::Variant>,
    /// Text last copied, for pasting where no system clipboard exists.
    pub(crate) clipboard: Option<String>,
    paste_count: u32,
    /// Document waiting for an answer to "save changes?" before closing.
    pub(crate) confirm_close: Option<u64>,
    pub(crate) settings: Settings,
    store: Option<Store>,
    pub(crate) recovery: Vec<Recovery>,
    pub(crate) theme: ThemeKind,
    native_dialogs: bool,
    dialogs: dialogs::Dialogs,
    last_autosave: Instant,
    last_settings: Option<String>,
    composites_rev: (u64, u64),
    /// Documents whose canvas has been shown (and fitted) at least once.
    shown: HashSet<u64>,
    want_running_graph: bool,
    title: String,
}

impl Studio {
    pub fn new(cc: &eframe::CreationContext<'_>, opts: Options) -> Self {
        let settings = opts
            .store
            .as_ref()
            .map(Store::load_settings)
            .unwrap_or_default();
        let theme = settings.theme;
        crate::theme::install(&cc.egui_ctx, theme);
        let mut opts = opts;
        // Without --connect, the daemon last chosen in the studio.
        if !opts.endpoint_given
            && let Some(saved) = &settings.endpoint
        {
            opts.endpoint = saved.clone();
            if opts.security.ca.is_none() {
                opts.security.ca = settings.tls_ca.clone();
            }
            if opts.security.cert.is_none()
                && let Some((cert, key)) = &settings.tls_client
            {
                opts.security.cert = Some(cert.clone());
                opts.security.key = Some(key.clone());
            }
        }
        // Off Linux there is no local daemon to start: ask where the robot is.
        let ask_connect =
            !opts.endpoint_given && settings.endpoint.is_none() && !cfg!(target_os = "linux");
        let (endpoint, security, spawn_daemon) = (
            opts.endpoint.clone(),
            opts.security.clone(),
            opts.spawn_daemon,
        );
        let link = if opts.endpoint.is_empty() {
            Link::disabled()
        } else {
            Link::start(
                opts.endpoint,
                opts.daemon,
                opts.security,
                opts.spawn_daemon,
                cc.egui_ctx.clone(),
            )
        };
        let recovery = opts
            .store
            .as_ref()
            .map(Store::take_recoverable)
            .unwrap_or_default();
        let doc = Doc::new("untitled");
        let mut studio = Self {
            link,
            conn: Conn::Connecting,
            registry: local_registry(),
            tab_order: vec![doc.uid],
            doc,
            parked: Vec::new(),
            style: canvas::style(),
            status: None,
            metrics: HashMap::new(),
            fleet: BTreeMap::new(),
            peers_sent: None,
            ros: crate::ros::RosBrowser::default(),
            recording_since: None,
            record_draft: None,
            record_anchor: egui::Rect::NOTHING,
            scrub: None,
            plugin_panels: crate::plugin::panels()
                .into_iter()
                .map(|k| (k, (k.create)()))
                .collect(),
            deployed_rev: None,
            pending_deploy: None,
            deployed_variant: Vec::new(),
            pending_variant: Vec::new(),
            deployed_graph: None,
            watches: Vec::new(),
            logs: VecDeque::new(),
            log_filter: String::new(),
            issues: Vec::new(),
            issues_at: None,
            activity: Activity::Library,
            sidebar_open: settings.sidebar_open,
            inspector_open: settings.inspector_open,
            panel_open: settings.panel_open,
            panel_tab: PanelTab::Watch,
            toasts: Vec::new(),
            palette: None,
            library_filter: String::new(),
            rename: None,
            focused: None,
            collapsed: settings.collapsed.iter().cloned().collect(),
            queue: Vec::new(),
            fit_pending: true,
            node_rects: Vec::new(),
            pin_pos: Default::default(),
            selection: Vec::new(),
            marked: HashSet::new(),
            canvas_transform: Default::default(),
            canvas_rect: egui::Rect::NOTHING,
            compare: None,
            variant: None,
            clipboard: None,
            paste_count: 0,
            confirm_close: None,
            endpoint,
            security,
            spawn_daemon,
            connect_form: None,
            settings,
            store: opts.store,
            recovery,
            theme,
            native_dialogs: opts.native_dialogs,
            dialogs: dialogs::Dialogs::new(cc.egui_ctx.clone()),
            last_autosave: Instant::now(),
            last_settings: None,
            composites_rev: (0, u64::MAX),
            shown: HashSet::new(),
            want_running_graph: true,
            title: String::new(),
        };
        studio.shown.insert(studio.doc.uid);
        if ask_connect {
            studio.push(Cmd::ConnectDialog);
        }
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

    /// Name of the daemon this window talks to, if connected.
    pub fn daemon_name(&self) -> Option<&str> {
        match &self.conn {
            Conn::Online { daemon, .. } => Some(daemon),
            _ => None,
        }
    }

    /// True while the open graph is the one running on the daemon.
    pub fn live(&self) -> bool {
        self.online()
            && self.doc.parent.is_none()
            && self.running_graph() == Some(self.doc.name.as_str())
    }

    pub fn running_graph(&self) -> Option<&str> {
        self.status.as_ref().and_then(|s| s.graph.as_deref())
    }

    /// The graph has structural edits that the running copy does not have.
    pub fn undeployed_changes(&self) -> bool {
        self.live()
            && self.deployed_rev.is_some_and(|r| {
                r != self.doc.structure_rev || self.deployed_variant != self.variant_names()
            })
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

    /// Nodes an edit command applies to: the canvas selection, or the focused node.
    pub fn selected_nodes(&self) -> Vec<NodeId> {
        let mut v: Vec<NodeId> = Vec::new();
        for &id in self.selection.iter().chain(&self.marked) {
            if self.doc.snarl.get_node(id).is_some() && !v.contains(&id) {
                v.push(id);
            }
        }
        if let Some(f) = self.focused
            && !v.contains(&f)
            && self.doc.snarl.get_node(f).is_some()
        {
            v.push(f);
        }
        v
    }

    /// All open documents, in tab order.
    pub fn tabs(&self) -> Vec<&Doc> {
        self.tab_order
            .iter()
            .filter_map(|uid| self.doc_by_uid(*uid))
            .collect()
    }

    pub fn doc_by_uid(&self, uid: u64) -> Option<&Doc> {
        if self.doc.uid == uid {
            Some(&self.doc)
        } else {
            self.parked.iter().find(|d| d.uid == uid)
        }
    }

    pub fn doc_by_uid_mut(&mut self, uid: u64) -> Option<&mut Doc> {
        if self.doc.uid == uid {
            Some(&mut self.doc)
        } else {
            self.parked.iter_mut().find(|d| d.uid == uid)
        }
    }

    pub fn canvas_id(&self) -> Id {
        Id::new(CANVAS_ID).with(self.doc.uid)
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

    fn apply(&mut self, ctx: &egui::Context, cmd: Cmd) {
        if self.variant.is_some() && !cmd.keeps_variant() {
            // Saving writes the document, not the overlay shown; the
            // overlay stays on the canvas afterwards.
            let resume =
                matches!(cmd, Cmd::Save | Cmd::SaveAs(_)).then(|| self.variant_names().to_vec());
            self.leave_variant();
            self.apply(ctx, cmd);
            if let Some(names) = resume {
                self.set_variant(names);
            }
            return;
        }
        match cmd {
            Cmd::NewGraph => self.new_tab(Doc::new("untitled")),
            Cmd::NewFromTemplate(id) => self.new_from_template(id),
            Cmd::OpenDialog => self.open_dialog(dialogs::Purpose::Open),
            Cmd::QuickOpen => self.palette = Some(Palette::new(PaletteMode::Open)),
            Cmd::ConnectDialog => {
                let path = |p: &Option<PathBuf>| {
                    p.as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default()
                };
                self.connect_form = Some(ConnectForm {
                    endpoint: self.endpoint.clone(),
                    tls_ca: path(&self.security.ca),
                    tls_cert: path(&self.security.cert),
                    tls_key: path(&self.security.key),
                    token: self.security.token.clone().unwrap_or_default(),
                })
            }
            Cmd::Connect {
                endpoint,
                tls_ca,
                client,
                token,
            } => self.connect_to(ctx, endpoint, tls_ca, client, token),
            Cmd::Open(p) => self.open(&p),
            Cmd::Save => self.save(None),
            Cmd::SaveAsDialog => self.open_dialog(dialogs::Purpose::SaveAs),
            Cmd::SaveAs(p) => self.save(Some(p)),
            Cmd::SwitchTab(uid) => self.switch_tab(uid),
            Cmd::CloseTab(uid) => self.close_tab(uid, false),
            Cmd::DiscardTab(uid) => self.close_tab(uid, true),
            Cmd::NextTab => {
                if let Some(i) = self.tab_order.iter().position(|u| *u == self.doc.uid) {
                    let next = self.tab_order[(i + 1) % self.tab_order.len()];
                    self.switch_tab(next);
                }
            }
            Cmd::ClearRecent => self.settings.recent.clear(),
            Cmd::RecoverAutosaves => self.recover(),
            Cmd::DiscardAutosaves => self.discard_recovery(),
            Cmd::Undo => self.undo(),
            Cmd::Redo => self.redo(),
            Cmd::Copy => self.copy(ctx, false),
            Cmd::Cut => self.copy(ctx, true),
            Cmd::Paste(text) => self.paste(&text),
            Cmd::Duplicate => self.duplicate(),
            Cmd::SelectAll => self.select_all(ctx),
            Cmd::DeleteSelection => self.delete_selection(),
            Cmd::DeleteNode(id) => {
                if let Some(n) = self.doc.find(&id) {
                    self.delete_nodes(&[n]);
                }
            }
            Cmd::Align(op) => self.align(op),
            Cmd::AddNode(kind) => {
                if let Some(d) = self.registry.get(&kind).cloned() {
                    // Near the middle of the visible canvas, cascading so
                    // nodes added in a row do not cover each other.
                    let n = self.doc.snarl.nodes().count() as f32;
                    let center = if self.canvas_rect.is_positive() {
                        self.canvas_transform.inverse() * self.canvas_rect.center()
                    } else {
                        egui::pos2(240.0, 160.0)
                    };
                    let pos =
                        center + egui::vec2(-120.0 + (n % 5.0) * 28.0, -50.0 + (n % 5.0) * 28.0);
                    let id = self.doc.add_node(&d, pos);
                    self.focused = Some(id);
                }
            }
            Cmd::AddConfiguredNode { kind, params } => {
                self.apply(ctx, Cmd::AddNode(kind));
                if let Some(id) = self.focused {
                    let node = &mut self.doc.snarl[id];
                    node.params.extend(params);
                    self.doc.dirty = true;
                }
            }
            Cmd::RosDiscover => {
                self.activity = Activity::Ros2;
                self.sidebar_open = true;
                self.ros.refresh(ctx);
            }
            Cmd::Focus(id) => {
                self.focused = id.and_then(|id| self.doc.find(&id));
                self.inspector_open |= self.focused.is_some();
            }
            Cmd::GroupSelection => self.group_selection(),
            Cmd::Ungroup(id) => self.ungroup(&id),
            Cmd::OpenSubgraph(id) => self.open_subgraph(&id),
            Cmd::OpenNodeSource(id) => self.open_node_source(&id),
            Cmd::CompareWithSaved => self.compare_with_saved(),
            Cmd::CompareDialog => self.open_dialog(dialogs::Purpose::Compare),
            Cmd::CompareWith(p) => self.compare_with(&p),
            Cmd::CloseCompare => self.compare = None,
            Cmd::SetVariant(names) => self.set_variant(names),
            Cmd::Deploy => self.deploy(),
            Cmd::Stop => self.link.send(crate::link::Command::Stop),
            Cmd::Estop(engage) => self.estop(engage),
            Cmd::Kill(group) => self.link.send(crate::link::Command::Kill(group)),
            Cmd::StartRecording(path) => self.start_recording(path),
            Cmd::StopRecording => self.link.send(crate::link::Command::Record(String::new())),
            Cmd::ReplayDialog => self.palette = Some(Palette::new(PaletteMode::Replay)),
            Cmd::Playback(c) => self.link.send(crate::link::Command::Playback(c)),
            Cmd::ConnectDaemon(name) => self.connect_daemon(name),
            Cmd::Watch { node, port, ty } => self.watch(node, port, ty),
            Cmd::Unwatch { node, port } => self.unwatch(&node, &port),
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
            Cmd::SetTheme(kind) => self.set_theme(ctx, kind),
        }
    }

    /// Connects to another daemon, and remembers it for the next start.
    fn connect_to(
        &mut self,
        ctx: &egui::Context,
        endpoint: String,
        tls_ca: Option<PathBuf>,
        client: Option<(PathBuf, PathBuf)>,
        token: Option<String>,
    ) {
        self.security.ca = tls_ca.clone();
        let (cert, key) = client.clone().unzip();
        self.security.cert = cert;
        self.security.key = key;
        self.security.token = token;
        self.endpoint = endpoint.clone();
        // Dropping the old link ends its threads.
        self.link = Link::start(
            endpoint.clone(),
            None,
            self.security.clone(),
            self.spawn_daemon,
            ctx.clone(),
        );
        self.conn = Conn::Connecting;
        self.status = None;
        self.metrics.clear();
        self.peers_sent = None;
        self.want_running_graph = true;
        self.settings.endpoint = Some(endpoint);
        self.settings.tls_ca = tls_ca;
        self.settings.tls_client = client;
    }

    pub(crate) fn set_theme(&mut self, ctx: &egui::Context, kind: ThemeKind) {
        self.theme = kind;
        self.settings.theme = kind;
        crate::theme::apply(ctx, kind);
        self.style = canvas::style();
        for w in &mut self.watches {
            w.theme_changed();
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let cmd = |key| KeyboardShortcut::new(Modifiers::COMMAND, key);
        let cmd_shift = |key| KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, key);
        let mut bindings = vec![
            (cmd(Key::K), Cmd::CommandPalette),
            (cmd_shift(Key::P), Cmd::CommandPalette),
            (cmd_shift(Key::S), Cmd::SaveAsDialog),
            (cmd(Key::S), Cmd::Save),
            (cmd(Key::O), Cmd::OpenDialog),
            (cmd(Key::P), Cmd::QuickOpen),
            (cmd(Key::N), Cmd::NewGraph),
            (cmd(Key::W), Cmd::CloseTab(self.doc.uid)),
            (
                KeyboardShortcut::new(Modifiers::CTRL, Key::Tab),
                Cmd::NextTab,
            ),
            (cmd(Key::Enter), Cmd::Deploy),
            (cmd(Key::B), Cmd::ToggleSidebar),
            (cmd(Key::J), Cmd::TogglePanel),
            (cmd(Key::I), Cmd::ToggleInspector),
            (cmd(Key::Num0), Cmd::FitView),
            // Works from anywhere, text fields included. Releasing is
            // deliberately not a single key.
            (
                KeyboardShortcut::new(Modifiers::NONE, Key::F12),
                Cmd::Estop(true),
            ),
        ];
        // Editing shortcuts belong to a focused text field when there is one.
        let text_focus = ctx.egui_wants_keyboard_input();
        if self.palette.is_none() && !text_focus {
            bindings.extend([
                (cmd_shift(Key::Z), Cmd::Redo),
                (cmd(Key::Z), Cmd::Undo),
                (cmd(Key::Y), Cmd::Redo),
                (cmd(Key::D), Cmd::Duplicate),
                (cmd(Key::A), Cmd::SelectAll),
                (cmd(Key::G), Cmd::GroupSelection),
            ]);
        }
        for (shortcut, c) in bindings {
            if ctx.input_mut(|i| i.consume_shortcut(&shortcut)) {
                self.push(c);
            }
        }
        if self.palette.is_some() || text_focus {
            return;
        }
        // The platform turns Ctrl+C/X/V into clipboard events.
        let events = ctx.input(|i| i.events.clone());
        for e in events {
            match e {
                egui::Event::Copy => self.push(Cmd::Copy),
                egui::Event::Cut => self.push(Cmd::Cut),
                egui::Event::Paste(text) => self.push(Cmd::Paste(text)),
                _ => {}
            }
        }
        if ctx.input(|i| i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace)) {
            self.push(Cmd::DeleteSelection);
        }
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.focused = None;
        }
    }

    // ----- frame -----------------------------------------------------------

    fn canvas(&mut self, ui: &mut Ui) {
        let area = ui.available_rect_before_wrap();
        self.canvas_rect = area;
        let canvas_id = self.canvas_id();
        let over_canvas = ui
            .input(|i| i.pointer.interact_pos())
            .is_some_and(|p| area.contains(p));
        let on_node = |p: egui::Pos2| self.node_rects.iter().any(|(_, r)| r.contains(p));
        // A plain click on empty canvas clears the selection. The canvas widget
        // only does that for Ctrl+click, so present the click as one.
        let clear_selection = ui.input(|i| {
            i.pointer.primary_clicked()
                && i.modifiers.is_none()
                && i.pointer.interact_pos().is_some_and(|p| !on_node(p))
        }) && over_canvas
            && self.palette.is_none()
            && !pointer_over_overlay(ui.ctx());
        if clear_selection {
            ui.ctx().input_mut(|i| i.modifiers.command = true);
        }
        if ui.input(|i| i.pointer.primary_clicked() && !i.modifiers.shift)
            && over_canvas
            && !pointer_over_overlay(ui.ctx())
        {
            self.marked.clear();
        }
        let watched: HashSet<(String, String)> = self.watches.iter().map(Watch::key).collect();
        let live = self.live();
        let marks = match &self.variant {
            Some(v) => v.diff.marks(),
            None => self
                .compare
                .as_ref()
                .filter(|c| c.uid == self.doc.uid)
                .map(Compare::marks)
                .unwrap_or_default(),
        };
        let mut viewer = Viewer {
            registry: &self.registry,
            composites: &self.doc.composites,
            metrics: &self.metrics,
            watched: &watched,
            live,
            focused: self.focused,
            marked: &self.marked,
            marks: &marks,
            read_only: self.variant.is_some(),
            transform: None,
            hovered: None,
            fit: std::mem::take(&mut self.fit_pending).then_some(area),
            rects: Vec::new(),
            pins: Default::default(),
            actions: Vec::new(),
        };
        let resp = SnarlWidget::new().id(canvas_id).style(self.style).show(
            &mut self.doc.snarl,
            &mut viewer,
            ui,
        );
        if clear_selection {
            ui.ctx().input_mut(|i| i.modifiers.command = false);
        }
        let hovered = viewer.hovered;
        if let Some(t) = viewer.transform {
            self.canvas_transform = t;
        }
        self.node_rects = std::mem::take(&mut viewer.rects);
        self.pin_pos = std::mem::take(&mut viewer.pins);
        let actions = std::mem::take(&mut viewer.actions);
        self.selection = egui_snarl::ui::get_selected_nodes(canvas_id, ui.ctx());
        // A plain click focuses a node; shift-click adds to the selection instead.
        if ui.input(|i| i.pointer.primary_clicked() && !i.modifiers.shift)
            && over_canvas
            && resp.contains_pointer()
            && !pointer_over_overlay(ui.ctx())
        {
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
        if self.doc.snarl.nodes().next().is_none() && self.doc.parent.is_none() {
            shell::welcome(self, ui, area);
        } else {
            shell::canvas_controls(self, ui, area);
        }
        if let Some(pb) = self.status.as_ref().and_then(|s| s.playback.clone()) {
            shell::timeline(self, ui, area, &pb);
        }
    }

    /// Recomputes derived document state: composite interfaces and problems.
    fn refresh(&mut self) {
        let key = (self.doc.uid, self.doc.structure_rev);
        if self.composites_rev != key {
            self.composites_rev = key;
            self.doc.refresh_composites(&self.registry);
            if !self.doc.pending.is_empty() {
                for w in self.doc.resolve_pending(&self.registry) {
                    self.toast(Severity::Warning, w);
                }
            }
        }
        // Subgraph files are read from disk, so validate those graphs less often.
        let has_files = self
            .doc
            .snarl
            .nodes()
            .any(|n| n.kind.starts_with(ruro_graph::GRAPH_PREFIX));
        let stale = match self.issues_at {
            Some((uid, rev, at)) => {
                uid != self.doc.uid
                    || !has_files
                    || rev != self.doc.structure_rev
                    || at.elapsed() > Duration::from_millis(500)
            }
            None => true,
        };
        if stale {
            let graph = self.doc.to_graph(&self.registry);
            self.issues = ruro_graph::validate_in(&graph, &self.registry, &self.doc.base_dir());
            self.issues_at = Some((self.doc.uid, self.doc.structure_rev, Instant::now()));
        }
        if let Some(c) = &mut self.compare
            && let Some(doc) = std::iter::once(&self.doc)
                .chain(&self.parked)
                .find(|d| d.uid == c.uid)
        {
            c.update(&doc.to_graph(&self.registry));
        }
    }

    /// Draws one frame. Shared by the native app and the UI tests.
    pub fn frame(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        self.handle_events(&ctx);
        self.poll_dialogs();
        self.refresh();
        self.focused = self
            .focused
            .filter(|&id| self.doc.snarl.get_node(id).is_some());
        self.shortcuts(&ctx);

        shell::title_bar(self, ui);
        shell::status_bar(self, ui);
        shell::activity_bar(self, ui);
        if self.sidebar_open {
            let w = sidebar::show(self, ui);
            self.settings.sidebar_width = Some(w);
        }
        if self.inspector_open {
            let w = inspector::show(self, ui);
            self.settings.inspector_width = Some(w);
        }
        if self.panel_open {
            let h = panel::show(self, ui);
            self.settings.panel_height = Some(h);
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(crate::theme::EDITOR()))
            .show(ui, |ui| {
                shell::editor_tabs(self, ui);
                shell::banners(self, ui);
                self.canvas(ui);
            });

        crate::palette::show(self, &ctx);
        shell::record_popover(self, &ctx);
        shell::close_confirmation(self, &ctx);
        shell::connect_dialog(self, &ctx);
        shell::toasts(self, &ctx);

        for cmd in std::mem::take(&mut self.queue) {
            self.apply(&ctx, cmd);
        }
        self.after_frame(&ctx);
    }

    /// Records finished edits in the history and keeps files and the title in sync.
    fn after_frame(&mut self, ctx: &egui::Context) {
        // Commit once an interaction settles, so a drag or a slider scrub is one step.
        let settled = !ctx.input(|i| i.pointer.any_down()) && !ctx.egui_wants_keyboard_input();
        if settled && self.doc.commit() {
            self.write_back_subgraph();
        }
        self.autosave_tick();
        self.save_settings_tick();

        let title = format!(
            "{}{} — ruro studio",
            self.doc.name,
            if self.doc.dirty { " ●" } else { "" }
        );
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
        if self.live()
            || !self.toasts.is_empty()
            || self.status.as_ref().is_some_and(|s| s.recording.is_some())
        {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}

impl eframe::App for Studio {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.shutdown();
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

/// The example graphs offered by File > Open Example and the welcome page:
/// the first directory of [`example_dirs`] that has any.
pub fn example_files() -> Vec<PathBuf> {
    let exe = std::env::current_exe().ok();
    let data_dirs = std::env::var_os("XDG_DATA_DIRS");
    first_yaml_files(&example_dirs(exe.as_deref(), data_dirs.as_deref()))
}

/// The `*.yaml` files of the first of `dirs` that has any.
fn first_yaml_files(dirs: &[PathBuf]) -> Vec<PathBuf> {
    dirs.iter()
        .map(|d| yaml_files(d))
        .find(|f| !f.is_empty())
        .unwrap_or_default()
}

/// Where example graphs are looked for, in order:
///
/// - `examples/` in the working directory (a source checkout);
/// - `examples/` next to or above the executable (`target/release` in a
///   checkout, the AppImage and the Flatpak's `/app`);
/// - `share/ruro/examples` under the executable's prefix and each of
///   `$XDG_DATA_DIRS` (default `/usr/local/share:/usr/share`), where the
///   Debian package and the Docker image install them;
/// - `share/doc/ruro/examples` under the same directories, where older
///   packages put them.
pub fn example_dirs(exe: Option<&Path>, xdg_data_dirs: Option<&std::ffi::OsStr>) -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from("examples")];
    if let Some(exe) = exe {
        dirs.extend(exe.ancestors().skip(1).take(5).map(|a| a.join("examples")));
    }
    let mut data: Vec<PathBuf> = Vec::new();
    // <prefix>/bin/ruro-studio -> <prefix>/share
    if let Some(prefix) = exe.and_then(Path::parent).and_then(Path::parent) {
        data.push(prefix.join("share"));
    }
    let xdg = xdg_data_dirs
        .filter(|v| !v.is_empty())
        .unwrap_or(std::ffi::OsStr::new("/usr/local/share:/usr/share"));
    data.extend(std::env::split_paths(xdg).filter(|p| p.is_absolute()));
    for sub in ["ruro/examples", "doc/ruro/examples"] {
        for d in &data {
            let dir = d.join(sub);
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }
    dirs
}

pub fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// True when the pointer is over a popup, menu or overlay rather than the canvas.
fn pointer_over_overlay(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.pointer.interact_pos())
        .and_then(|p| ctx.layer_id_at(p))
        .is_some_and(|l| l.order != egui::Order::Background)
}

/// File name of a path, for display.
pub fn file_name(p: &Path) -> String {
    p.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string()
}

#[cfg(test)]
mod tests;
