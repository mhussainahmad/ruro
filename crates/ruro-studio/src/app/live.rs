//! The connection to a daemon: events, deployment, watches, recording,
//! playback and the fleet of daemons on the network.

use std::collections::HashSet;
use std::time::Instant;

use ruro_graph::Graph;
use ruro_proto::GroupState;

use super::{Conn, FleetEntry, MAX_LOGS, PanelTab, Severity, Studio, capitalize};
use crate::link::{Command, Event};
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
                for w in &self.watches {
                    self.link.send(Command::Watch {
                        node: w.tap.0.clone(),
                        port: w.tap.1.clone(),
                    });
                }
            }
            Event::Offline(reason) => {
                self.conn = Conn::Offline(reason);
                self.status = None;
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
                let running: HashSet<&str> = s
                    .groups
                    .iter()
                    .filter(|g| g.state == GroupState::Running)
                    .flat_map(|g| g.nodes.iter().map(String::as_str))
                    .collect();
                self.metrics.retain(|id, _| running.contains(id.as_str()));
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
            }
            Event::Fleet(s) => {
                self.fleet.insert(
                    s.name.clone(),
                    FleetEntry {
                        status: s,
                        seen: Instant::now(),
                    },
                );
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
            self.link.send(Command::Watch {
                node: tap.0.clone(),
                port: tap.1.clone(),
            });
            self.watches.push(Watch::new(node, port, ty, tap));
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

    pub(crate) fn connect_daemon(&mut self, name: String) {
        if self.daemon_name() == Some(name.as_str()) {
            return;
        }
        self.status = None;
        self.metrics.clear();
        self.deployed_rev = None;
        self.recording_since = None;
        self.conn = Conn::Connecting;
        self.link.send(Command::SwitchDaemon(name));
    }
}
