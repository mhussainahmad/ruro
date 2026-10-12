//! Documents: tabs, opening and saving, templates, subgraphs, comparison,
//! recent files, settings and autosave.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ruro_graph::Graph;

use super::{Severity, Studio, file_name};
use crate::dialogs::Purpose;
use crate::diff::Compare;
use crate::model::Doc;
use crate::palette::{Palette, PaletteMode};

/// How often unsaved documents are written to the autosave directory.
const AUTOSAVE_EVERY: Duration = Duration::from_secs(20);

impl Studio {
    /// The tab label of a document.
    pub fn tab_title(&self, doc: &Doc) -> String {
        if let Some((parent, node)) = &doc.parent {
            let parent = self
                .doc_by_uid(*parent)
                .map(|p| self.tab_title(p))
                .unwrap_or_else(|| "subgraph".into());
            return format!("{parent} › {node}");
        }
        match &doc.path {
            Some(p) => file_name(p),
            None => format!("{}.yaml", doc.name),
        }
    }

    /// True for an untitled, unchanged, empty document that a new one can replace.
    fn pristine(doc: &Doc) -> bool {
        doc.path.is_none()
            && doc.parent.is_none()
            && !doc.dirty
            && doc.snarl.nodes().next().is_none()
    }

    /// Opens `doc` in a new tab and shows it. An untouched empty tab is reused.
    pub(crate) fn new_tab(&mut self, doc: Doc) {
        self.want_running_graph = false;
        if Self::pristine(&self.doc) {
            let old = self.doc.uid;
            if let Some(slot) = self.tab_order.iter_mut().find(|u| **u == old) {
                *slot = doc.uid;
            }
            self.doc = doc;
        } else {
            let i = self
                .tab_order
                .iter()
                .position(|u| *u == self.doc.uid)
                .map_or(self.tab_order.len(), |i| i + 1);
            self.tab_order.insert(i, doc.uid);
            let previous = std::mem::replace(&mut self.doc, doc);
            self.parked.push(previous);
        }
        self.after_switch();
    }

    pub(crate) fn switch_tab(&mut self, uid: u64) {
        if uid == self.doc.uid {
            return;
        }
        let Some(i) = self.parked.iter().position(|d| d.uid == uid) else {
            return;
        };
        self.doc.commit();
        std::mem::swap(&mut self.doc, &mut self.parked[i]);
        self.after_switch();
    }

    fn after_switch(&mut self) {
        self.rename = None;
        self.focused = None;
        self.selection.clear();
        self.node_rects.clear();
        // Fit only documents never shown before; others keep their view.
        self.fit_pending = !self.shown.contains(&self.doc.uid);
        self.shown.insert(self.doc.uid);
        self.composites_rev = (0, u64::MAX);
        self.issues_at = None;
    }

    pub(crate) fn close_tab(&mut self, uid: u64, discard: bool) {
        let Some(doc) = self.doc_by_uid(uid) else {
            return;
        };
        if doc.dirty && doc.parent.is_none() && !discard {
            self.switch_tab(uid);
            self.confirm_close = Some(uid);
            return;
        }
        if let Some(store) = &self.store {
            store.remove_autosave(uid);
        }
        self.confirm_close = None;
        if self.compare.as_ref().is_some_and(|c| c.uid == uid) {
            self.compare = None;
        }
        let pos = self.tab_order.iter().position(|u| *u == uid);
        self.tab_order.retain(|u| *u != uid);
        if self.doc.uid == uid {
            // Show the neighbouring tab, or a fresh empty document.
            let next = pos
                .and_then(|i| {
                    self.tab_order
                        .get(i.min(self.tab_order.len().saturating_sub(1)))
                })
                .copied();
            match next.and_then(|n| self.parked.iter().position(|d| d.uid == n)) {
                Some(i) => {
                    self.doc = self.parked.remove(i);
                }
                None => {
                    self.doc = Doc::new("untitled");
                    self.tab_order.push(self.doc.uid);
                }
            }
            self.after_switch();
        } else {
            self.parked.retain(|d| d.uid != uid);
        }
    }

    pub(crate) fn new_from_template(&mut self, id: &str) {
        let Some(t) = crate::templates::get(id) else {
            return;
        };
        if t.yaml.is_empty() {
            self.new_tab(Doc::new("untitled"));
            return;
        }
        match Graph::from_yaml(t.yaml) {
            Ok(graph) => {
                let (mut doc, _) = Doc::from_graph(&graph, &self.registry, None);
                doc.reset_history(false);
                self.new_tab(doc);
            }
            Err(e) => self.toast(Severity::Error, format!("Template {id}: {e}")),
        }
    }

    pub(crate) fn open_dialog(&mut self, purpose: Purpose) {
        if !self.native_dialogs {
            self.palette = Some(Palette::new(match purpose {
                Purpose::Open => PaletteMode::Open,
                Purpose::SaveAs => PaletteMode::SaveAs,
                Purpose::Compare => PaletteMode::Compare,
            }));
            return;
        }
        let dir = self
            .doc
            .path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok());
        let name = Some(format!("{}.yaml", self.doc.name));
        self.dialogs.show(purpose, dir, name);
    }

    pub(crate) fn poll_dialogs(&mut self) {
        if let Some((purpose, path)) = self.dialogs.poll() {
            let Some(path) = path else { return };
            self.push(match purpose {
                Purpose::Open => super::Cmd::Open(path),
                Purpose::SaveAs => super::Cmd::SaveAs(path),
                Purpose::Compare => super::Cmd::CompareWith(path),
            });
        }
    }

    pub(crate) fn open(&mut self, path: &Path) {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if let Some(uid) = self
            .tabs()
            .iter()
            .find(|d| {
                d.path
                    .as_ref()
                    .is_some_and(|p| p.canonicalize().unwrap_or_else(|_| p.clone()) == canonical)
            })
            .map(|d| d.uid)
        {
            self.switch_tab(uid);
            return;
        }
        match Graph::load(path) {
            Ok(graph) => {
                let (doc, warnings) =
                    Doc::from_graph(&graph, &self.registry, Some(path.to_path_buf()));
                self.new_tab(doc);
                self.settings.add_recent(path);
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

    pub(crate) fn save(&mut self, path: Option<PathBuf>) {
        if self.doc.parent.is_some() && path.is_none() {
            // A subgraph is saved as part of its parent.
            self.write_back_subgraph();
            if let Some((parent, _)) = self.doc.parent.clone() {
                let current = self.doc.uid;
                self.switch_tab(parent);
                self.save(None);
                self.switch_tab(current);
            }
            return;
        }
        let Some(path) = path.or_else(|| self.doc.path.clone()) else {
            self.open_dialog(Purpose::SaveAs);
            return;
        };
        let path = if path.extension().is_none() {
            path.with_extension("yaml")
        } else {
            path
        };
        match self.doc.to_graph(&self.registry).save(&path) {
            Ok(()) => {
                if self.doc.parent.take().is_some() {
                    // Saved a subgraph to its own file: it is now a document of its own.
                    self.doc.reset_history(true);
                }
                self.doc.path = Some(path.clone());
                self.doc.mark_saved();
                self.settings.add_recent(&path);
                if let Some(store) = &self.store {
                    store.remove_autosave(self.doc.uid);
                }
                self.toast(Severity::Info, format!("Saved {}", path.display()));
                // Graphs that use this file as a subgraph pick up the change.
                for d in &mut self.parked {
                    d.structure_rev += 1;
                }
            }
            Err(e) => self.toast(Severity::Error, format!("Cannot save: {e}")),
        }
    }

    // ----- subgraphs --------------------------------------------------------

    pub(crate) fn open_subgraph(&mut self, id: &str) {
        let Some(node) = self.doc.find(id).map(|n| self.doc.snarl[n].clone()) else {
            return;
        };
        if let Some(rel) = node.kind.strip_prefix(ruro_graph::GRAPH_PREFIX) {
            let path = self.doc.base_dir().join(rel.trim());
            self.open(&path);
            return;
        }
        let Some(sub) = node.sub.as_deref() else {
            self.toast(Severity::Warning, format!("{id} is not a subgraph"));
            return;
        };
        let parent = self.doc.uid;
        // Reuse an open tab of this subgraph.
        if let Some(uid) = self
            .tabs()
            .iter()
            .find(|d| d.parent.as_ref() == Some(&(parent, id.to_owned())))
            .map(|d| d.uid)
        {
            self.switch_tab(uid);
            return;
        }
        let (mut doc, warnings) = Doc::from_graph(sub, &self.registry, None);
        doc.path = None;
        doc.parent = Some((parent, id.to_owned()));
        doc.reset_history(true);
        for w in warnings {
            self.toast(Severity::Warning, w);
        }
        self.new_tab(doc);
    }

    /// Copies the active subgraph document into its parent's composite node.
    pub(crate) fn write_back_subgraph(&mut self) {
        let Some((parent, node)) = self.doc.parent.clone() else {
            return;
        };
        let mut graph = self.doc.to_graph(&self.registry);
        if graph.name.is_empty() {
            graph.name = node.clone();
        }
        let registry = self.registry.clone();
        let Some(p) = self.doc_by_uid_mut(parent) else {
            return;
        };
        let Some(nid) = p.find(&node) else {
            return;
        };
        if p.snarl[nid].sub.as_deref() == Some(&graph) {
            return;
        }
        p.snarl[nid].sub = Some(Box::new(graph));
        p.touch_structure();
        p.refresh_composites(&registry);
        p.commit();
    }

    // ----- comparison -------------------------------------------------------

    pub(crate) fn compare_with_saved(&mut self) {
        let Some(path) = self.doc.path.clone() else {
            self.toast(Severity::Info, "This graph has not been saved yet");
            return;
        };
        self.compare_against(&path, "saved version".into());
    }

    pub(crate) fn compare_with(&mut self, path: &Path) {
        self.compare_against(path, file_name(path));
    }

    fn compare_against(&mut self, path: &Path, label: String) {
        match Graph::load(path) {
            Ok(base) => {
                let current = self.doc.to_graph(&self.registry);
                self.compare = Some(Compare::new(self.doc.uid, label, base, &current));
                self.activity = super::Activity::Changes;
                self.sidebar_open = true;
            }
            Err(e) => self.toast(
                Severity::Error,
                format!("Cannot read {}: {e}", path.display()),
            ),
        }
    }

    // ----- autosave and settings ---------------------------------------------

    pub(crate) fn recover(&mut self) {
        let items = std::mem::take(&mut self.recovery);
        for r in items {
            match Graph::load(&r.file) {
                Ok(graph) => {
                    let (mut doc, _) = Doc::from_graph(&graph, &self.registry, r.original.clone());
                    // Unsaved relative to the file on disk, if there is one.
                    doc.reset_history(false);
                    doc.dirty = true;
                    self.new_tab(doc);
                }
                Err(e) => self.toast(Severity::Error, format!("Cannot recover {}: {e}", r.name)),
            }
            if let Some(store) = &self.store {
                store.forget(&r);
            }
        }
    }

    pub(crate) fn discard_recovery(&mut self) {
        for r in std::mem::take(&mut self.recovery) {
            if let Some(store) = &self.store {
                store.forget(&r);
            }
        }
    }

    /// Writes unsaved documents to the autosave directory now and then.
    pub(crate) fn autosave_tick(&mut self) {
        if self.last_autosave.elapsed() < AUTOSAVE_EVERY {
            return;
        }
        self.last_autosave = Instant::now();
        self.autosave_all();
    }

    fn autosave_all(&mut self) {
        let Some(store) = &self.store else { return };
        for d in std::iter::once(&self.doc).chain(&self.parked) {
            if d.dirty && d.parent.is_none() {
                if let Ok(yaml) = d.to_graph(&self.registry).to_yaml()
                    && let Err(e) = store.write_autosave(d.uid, &d.name, d.path.as_deref(), &yaml)
                {
                    tracing::warn!("autosave failed: {e}");
                }
            } else {
                store.remove_autosave(d.uid);
            }
        }
    }

    pub(crate) fn save_settings_tick(&mut self) {
        let Some(store) = &self.store else { return };
        self.settings.sidebar_open = self.sidebar_open;
        self.settings.inspector_open = self.inspector_open;
        self.settings.panel_open = self.panel_open;
        self.settings.theme = self.theme;
        let mut collapsed: Vec<String> = self.collapsed.iter().cloned().collect();
        collapsed.sort();
        self.settings.collapsed = collapsed;
        self.settings.open_files = self
            .tabs()
            .iter()
            .filter(|d| d.parent.is_none())
            .filter_map(|d| d.path.clone())
            .collect();
        let Ok(text) = serde_json::to_string_pretty(&self.settings) else {
            return;
        };
        if self.last_settings.as_deref() != Some(&text) {
            if let Err(e) = store.save_settings(&text) {
                tracing::warn!("cannot save settings: {e}");
            }
            self.last_settings = Some(text);
        }
    }

    /// Reopens the graphs that were open when the studio last closed.
    pub fn restore_session(&mut self) {
        for p in self.settings.open_files.clone() {
            if p.is_file() {
                self.open(&p);
            }
        }
    }

    /// Called when the window closes: unsaved work is kept for recovery.
    pub(crate) fn shutdown(&mut self) {
        self.doc.commit();
        self.autosave_all();
        self.save_settings_tick();
    }
}
