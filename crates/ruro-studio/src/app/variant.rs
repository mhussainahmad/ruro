//! Overlays: the canvas can show the graph with some of its overlays applied.
//!
//! The effective graph replaces the document on the canvas, read-only, while
//! the document itself is set aside unchanged. Running deploys what is shown;
//! any edit, save or tab change puts the document back first (see
//! [`super::Cmd::keeps_variant`]), so overlays are never baked into the file.

use super::{Severity, Studio};
use crate::diff::{self, GraphDiff};
use crate::model::Doc;

/// A document shown with some of its overlays applied.
pub struct Variant {
    /// The selected overlays, in file order.
    pub names: Vec<String>,
    /// The document, set aside while its effective graph is on the canvas.
    pub base: Doc,
    /// How the effective graph differs from the document.
    pub diff: GraphDiff,
}

impl Studio {
    /// The document of the active tab as edited, even while an overlay is shown.
    pub fn base_doc(&self) -> &Doc {
        self.variant.as_ref().map_or(&self.doc, |v| &v.base)
    }

    /// The overlays applied to what the canvas shows; empty for the document itself.
    pub fn variant_names(&self) -> &[String] {
        self.variant.as_ref().map_or(&[], |v| &v.names)
    }

    /// Shows the active document with the overlays `names` applied, in the
    /// order the file lists them, or the document itself when `names` is empty.
    pub(crate) fn set_variant(&mut self, names: Vec<String>) {
        self.leave_variant();
        if names.is_empty() {
            return;
        }
        let graph = self.doc.to_graph(&self.registry);
        if let Some(missing) = names.iter().find(|n| !graph.overlays.contains_key(*n)) {
            self.toast(
                Severity::Error,
                format!("The graph has no overlay `{missing}`"),
            );
            return;
        }
        let names: Vec<String> = graph
            .overlays
            .keys()
            .filter(|k| names.contains(k))
            .cloned()
            .collect();
        let effective = match graph.with_overlays(&names) {
            Ok(g) => g,
            Err(e) => {
                self.toast(Severity::Error, e.to_string());
                return;
            }
        };
        let plain = graph.with_overlays::<&str>(&[]).unwrap_or(graph);
        let diff = diff::diff(&plain, &effective);
        let (mut view, _) = Doc::from_graph(&effective, &self.registry, self.doc.path.clone());
        // The view stands in for the document: same tab, same state.
        view.uid = self.doc.uid;
        view.dirty = self.doc.dirty;
        view.structure_rev = self.doc.structure_rev;
        self.doc.commit();
        let focused = self.focused_id();
        let base = std::mem::replace(&mut self.doc, view);
        self.variant = Some(Variant { names, base, diff });
        self.after_variant_change(focused);
    }

    /// Puts the document back on the canvas, if an overlay is shown.
    pub(crate) fn leave_variant(&mut self) {
        let focused = self.focused_id();
        if let Some(v) = self.variant.take() {
            self.doc = v.base;
            self.after_variant_change(focused);
        }
    }

    fn focused_id(&self) -> Option<String> {
        self.focused
            .and_then(|id| self.doc.snarl.get_node(id))
            .map(|n| n.id.clone())
    }

    /// Canvas ids differ between the document and its view: keep the focused
    /// node by name and recompute what depends on the document.
    fn after_variant_change(&mut self, focused: Option<String>) {
        self.rename = None;
        self.selection.clear();
        self.marked.clear();
        self.node_rects.clear();
        self.focused = focused.and_then(|id| self.doc.find(&id));
        self.composites_rev = (0, u64::MAX);
        self.issues_at = None;
    }
}
