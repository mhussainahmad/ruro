//! The connection to a daemon: events, deployment, watches, recording,
//! playback and the fleet of daemons on the network.

use std::collections::{BTreeSet, HashSet};
use std::time::Instant;

use ruro_graph::Graph;
use ruro_proto::GroupState;

use super::{Conn, FLEET_STALE, FleetEntry, MAX_LOGS, PanelTab, Severity, Studio, capitalize};
use crate::link::{Command, ESTOP_OP, Event};
use crate::model::Doc;
use crate::watch::Watch;

impl Studio {
    pub(crate) fn handle_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.link.events.try_recv() {
            self.handle_event(ctx, event);
        }
    }

    pub(crate) fn handle_event(&mut self, ctx: &egui::Context, event: Event) {
        match event {
            Event::Connected { daemon, endpoint } => {
                self.conn = Conn::Online { daemon, endpoint };
                // A new connection follows no peers yet.
                self.peers_sent = None;
                for w in &self.watches {
                    self.link.send(Command::Watch {
                        daemon: w.daemon.clone(),
                        node: w.tap.0.clone(),
                        port: w.tap.1.clone(),
                    });
                }
            }
            Event::Offline(reason) => {
                self.conn = Conn::Offline(reason);
                self.status = None;
                self.peers_sent = None;
                self.metrics.clear();
                self.recording_since = None;
            }
            Event::Registry(r) => {
                self.registry = r;
                self.composites_rev = (0, u64::MAX);
                if self.want_running_graph && self.doc.snarl.nodes().next().is_none() {
                    self.link.send(Command::FetchGraph);
                }
            }
            Event::Status(s) => {
                match (&s.recording, self.recording_since) {
                    (Some(_), None) => self.recording_since = Some(Instant::now()),
                    (None, Some(_)) => self.recording_since = None,
                    _ => {}
                }
                self.fleet.insert(
                    s.name.clone(),
                    FleetEntry {
                        status: s.clone(),
                        seen: Instant::now(),
                    },
                );
                self.status = Some(s);
                // Metrics of nodes no longer running, here or on a peer, go.
                let peers = self.peer_daemons();
                let running: HashSet<String> = self
                    .status
                    .iter()
                    .chain(
                        peers
                            .iter()
                            .filter_map(|p| Some(&self.fleet.get(p)?.status)),
                    )
                    .flat_map(|st| &st.groups)
                    .filter(|g| g.state == GroupState::Running)
                    .flat_map(|g| g.nodes.iter().cloned())
                    .collect();
                self.metrics.retain(|id, _| running.contains(id));
                self.sync_peers();
            }
            Event::Fleet(s) => {
                self.fleet.insert(
                    s.name.clone(),
                    FleetEntry {
                        status: s,
                        seen: Instant::now(),
                    },
                );
                self.sync_peers();
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
                    .find(|w| w.tap.0 == node && w.tap.1 == port)
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
                    self.deployed_variant = std::mem::take(&mut self.pending_variant);
                    self.toast(Severity::Success, format!("Running {}", self.doc.name));
                }
                ("stop", Ok(_)) => {
                    self.deployed_rev = None;
                    self.metrics.clear();
                    self.toast(Severity::Info, "Stopped");
                }
                ("kill", Ok(msg)) => self.toast(Severity::Warning, capitalize(&msg)),
                // The daemon says what it records, or what it wrote.
                ("record", Ok(msg)) => self.toast(Severity::Success, capitalize(&msg)),
                ("stop recording", Ok(msg)) => self.toast(Severity::Info, capitalize(&msg)),
                (ESTOP_OP, Ok(msg)) => {
                    // "e-stop engaged", "e-stop released" or "e-stop stays
                    // engaged: …", followed by what each peer did.
                    let severity = if msg.starts_with("e-stop released") {
                        Severity::Success
                    } else {
                        Severity::Warning
                    };
                    self.toast(severity, capitalize(&msg));
                }
                (ESTOP_OP, Err(e)) => {
                    // "e-stop engaged here, but not on `robot` (…)" is partly
                    // done: say what happened rather than "failed".
                    let text = if e.starts_with("e-stop") {
                        capitalize(&e)
                    } else {
                        format!("E-stop failed: {e}")
                    };
                    self.toast(Severity::Error, text);
                }
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
                    self.new_tab(doc);
                    self.doc.reset_history(true);
                    self.deployed_rev = Some(rev);
                    self.deployed_variant.clear();
                    self.toast(
                        Severity::Info,
                        format!("Attached to running graph {}", graph.name),
                    );
                }
            }
        }
    }

    /// The document to deploy: the open graph, or the graph a subgraph belongs to.
    fn root_doc(&self) -> &Doc {
        let mut doc = &self.doc;
        while let Some((parent, _)) = &doc.parent {
            match self.doc_by_uid(*parent) {
                Some(p) => doc = p,
                None => break,
            }
        }
        doc
    }

    pub(crate) fn deploy(&mut self) {
        let root = self.root_doc();
        let root_uid = root.uid;
        let graph = root.to_graph(&self.registry);
        let base = root.base_dir();
        if graph.nodes.is_empty() {
            self.toast(
                Severity::Warning,
                "The graph is empty. Add nodes from the library or with Ctrl+K.",
            );
            return;
        }
        self.issues = ruro_graph::validate_in(&graph, &self.registry, &base);
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
        // The daemon cannot see subgraph files: send the flattened graph.
        match graph.flatten_in(&base).and_then(|g| g.to_yaml()) {
            Ok(yaml) => {
                if root_uid != self.doc.uid {
                    self.switch_tab(root_uid);
                }
                self.link.send(Command::Deploy(yaml));
                self.pending_deploy = Some(self.doc.structure_rev);
                // With an overlay shown, the canvas is the graph with it applied.
                self.pending_variant = self.variant_names().to_vec();
                self.deployed_graph = Some(graph.name);
            }
            Err(e) => self.toast(Severity::Error, e.to_string()),
        }
    }

    /// The running (flattened) port behind output `node.port` of the open graph.
    fn tap_of(&self, node: &str, port: &str) -> (String, String) {
        let is_composite = self
            .doc
            .find(node)
            .is_some_and(|n| self.doc.snarl[n].is_composite());
        if is_composite
            && let Ok(p) =
                self.doc
                    .to_graph(&self.registry)
                    .resolve_output(&self.doc.base_dir(), node, port)
        {
            return (p.node, p.port);
        }
        (node.to_owned(), port.to_owned())
    }

    pub(crate) fn watch(&mut self, node: String, port: String, ty: String) {
        if !self.is_watched(&node, &port) {
            let tap = self.tap_of(&node, &port);
            let daemon = self.node_daemon(&tap.0);
            self.link.send(Command::Watch {
                daemon: daemon.clone(),
                node: tap.0.clone(),
                port: tap.1.clone(),
            });
            let mut w = Watch::new(node, port, ty, tap);
            w.daemon = daemon;
            self.watches.push(w);
        }
        self.panel_open = true;
        self.panel_tab = PanelTab::Watch;
    }

    pub(crate) fn unwatch(&mut self, node: &str, port: &str) {
        if let Some(i) = self
            .watches
            .iter()
            .position(|w| w.node == node && w.port == port)
        {
            let w = self.watches.remove(i);
            self.link.send(Command::Unwatch {
                node: w.tap.0,
                port: w.tap.1,
            });
        }
    }

    /// A default recording file name for the open graph.
    pub fn default_recording(&self) -> String {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let secs = secs as libc::time_t;
        // SAFETY: localtime_r writes only into the provided struct.
        let tm = unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            libc::localtime_r(&secs, &mut tm);
            tm
        };
        format!(
            "{}-{:04}{:02}{:02}-{:02}{:02}{:02}.mcap",
            self.running_graph().unwrap_or(&self.doc.name),
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }

    pub(crate) fn start_recording(&mut self, path: String) {
        let path = path.trim().to_owned();
        if path.is_empty() {
            return;
        }
        self.record_draft = None;
        if self.running_graph().is_none() {
            self.toast(Severity::Warning, "Run a graph before recording it");
            return;
        }
        self.link.send(Command::Record(path));
    }

    /// The other daemons running groups of the running graph: those named as
    /// a group's `machine` in the graph being edited that report running a
    /// graph of the same name.
    pub fn peer_daemons(&self) -> BTreeSet<String> {
        let (Some(me), Some(graph)) = (self.daemon_name(), self.running_graph()) else {
            return BTreeSet::new();
        };
        let root = self.root_doc();
        if root.name != graph {
            return BTreeSet::new();
        }
        let machines: HashSet<&str> = root
            .groups
            .values()
            .filter_map(|g| g.machine.as_deref())
            .collect();
        self.fleet
            .values()
            .filter(|f| f.seen.elapsed() < FLEET_STALE)
            .map(|f| &f.status)
            .filter(|st| st.name != me && st.graph.as_deref() == Some(graph))
            .filter(|st| machines.contains(st.name.as_str()))
            .map(|st| st.name.clone())
            .collect()
    }

    /// The peer daemon running `node` (a running, flattened node id), or
    /// `None` when it is the connected daemon's or not known to be a peer's.
    pub fn node_daemon(&self, node: &str) -> Option<String> {
        self.peer_daemons().into_iter().find(|p| {
            self.fleet.get(p).is_some_and(|f| {
                f.status
                    .groups
                    .iter()
                    .any(|g| g.nodes.iter().any(|n| n == node))
            })
        })
    }

    /// Follows the metrics and logs of the peers running the graph, and
    /// moves watches to the daemon that runs their node.
    pub(crate) fn sync_peers(&mut self) {
        if !self.online() {
            return;
        }
        let peers = self.peer_daemons();
        // A new connection follows no peers, so there is nothing to say
        // until there are some.
        let follows = self
            .peers_sent
            .as_ref()
            .map_or(peers.is_empty(), |p| *p == peers);
        if !follows {
            self.link.send(Command::Peers(peers.clone()));
        }
        self.peers_sent = Some(peers);
        for i in 0..self.watches.len() {
            let owner = self.node_daemon(&self.watches[i].tap.0);
            let w = &mut self.watches[i];
            if w.daemon != owner {
                w.daemon = owner;
                self.link.send(Command::Unwatch {
                    node: w.tap.0.clone(),
                    port: w.tap.1.clone(),
                });
                self.link.send(Command::Watch {
                    daemon: w.daemon.clone(),
                    node: w.tap.0.clone(),
                    port: w.tap.1.clone(),
                });
            }
        }
    }

    pub(crate) fn estop(&mut self, engage: bool) {
        if !self.online() {
            self.toast(Severity::Error, "Not connected to a daemon");
            return;
        }
        self.link.send(Command::Estop(engage));
    }

    /// Whether the emergency stop is engaged on the connected daemon.
    pub fn estopped(&self) -> bool {
        self.status.as_ref().is_some_and(|s| s.estopped)
    }

    /// The connected daemon and the peers running the graph whose emergency
    /// stop is engaged. A stop engaged directly on a peer does not travel to
    /// the daemon that deployed the graph, so peers are listed too.
    pub fn estopped_machines(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .status
            .iter()
            .filter(|s| s.estopped)
            .map(|s| s.name.clone())
            .collect();
        v.extend(
            self.peer_daemons()
                .into_iter()
                .filter(|p| self.fleet.get(p).is_some_and(|f| f.status.estopped)),
        );
        v
    }

    pub(crate) fn connect_daemon(&mut self, name: String) {
        if self.daemon_name() == Some(name.as_str()) {
            return;
        }
        self.status = None;
        self.metrics.clear();
        self.deployed_rev = None;
        self.recording_since = None;
        self.peers_sent = None;
        self.conn = Conn::Connecting;
        self.link.send(Command::SwitchDaemon(name));
    }
}
