//! Editing: undo and redo, the clipboard, multi-node operations, parameters,
//! renames and grouping into subgraphs.

use std::collections::HashMap;

use egui::{Pos2, Rect, Vec2, pos2, vec2};
use egui_snarl::{InPinId, NodeId, OutPinId};
use ruro_graph::{Graph, PortRef};
use serde_json::Value;

use super::{Severity, Studio};
use crate::link::Command;
use crate::model::{GNode, Snapshot};

/// Ways to line up several nodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlignOp {
    Left,
    CenterX,
    Top,
    CenterY,
    DistributeX,
    DistributeY,
}

impl AlignOp {
    pub const ALL: [AlignOp; 6] = [
        AlignOp::Left,
        AlignOp::CenterX,
        AlignOp::Top,
        AlignOp::CenterY,
        AlignOp::DistributeX,
        AlignOp::DistributeY,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AlignOp::Left => "Align Left Edges",
            AlignOp::CenterX => "Align Horizontal Centers",
            AlignOp::Top => "Align Top Edges",
            AlignOp::CenterY => "Align Vertical Centers",
            AlignOp::DistributeX => "Distribute Horizontally",
            AlignOp::DistributeY => "Distribute Vertically",
        }
    }

    pub fn icon(self) -> &'static str {
        use crate::icons::*;
        match self {
            AlignOp::Left => ALIGN_LEFT,
            AlignOp::CenterX => ALIGN_CENTER_X,
            AlignOp::Top => ALIGN_TOP,
            AlignOp::CenterY => ALIGN_CENTER_Y,
            AlignOp::DistributeX => DISTRIBUTE_X,
            AlignOp::DistributeY => DISTRIBUTE_Y,
        }
    }
}

/// How far each successive paste of the same nodes is shifted.
const PASTE_STEP: f32 = 32.0;

impl Studio {
    // ----- undo -------------------------------------------------------------

    pub(crate) fn undo(&mut self) {
        match self.doc.undo() {
            Some(left) => self.after_restore(left),
            None => self.toast(Severity::Info, "Nothing to undo"),
        }
    }

    pub(crate) fn redo(&mut self) {
        match self.doc.redo() {
            Some(left) => self.after_restore(left),
            None => self.toast(Severity::Info, "Nothing to redo"),
        }
    }

    /// Brings everything that refers to the document in line after undo or redo.
    fn after_restore(&mut self, left: Snapshot) {
        self.rename = None;
        self.doc.refresh_composites(&self.registry);
        let before = left.nodes();
        let now: Vec<(NodeId, GNode)> = self
            .doc
            .snarl
            .nodes_ids_data()
            .map(|(id, n)| (id, n.value.clone()))
            .collect();
        let live = self.live();
        for (nid, new) in &now {
            let Some(old) = before.get(nid) else { continue };
            if old.id != new.id {
                for w in &mut self.watches {
                    if w.node == old.id {
                        w.node = new.id.clone();
                    }
                }
                continue;
            }
            if !live || !self.metrics.contains_key(&new.id) || old.params == new.params {
                continue;
            }
            let Some(desc) = self.registry.get(&new.kind) else {
                continue;
            };
            for p in &desc.params {
                let (a, b) = (old.param(p), new.param(p));
                if a != b {
                    self.link.send(Command::SetParam {
                        daemon: self.node_daemon(&new.id),
                        node: new.id.clone(),
                        name: p.name.clone(),
                        value: b.to_string(),
                    });
                }
            }
        }
        self.write_back_subgraph();
    }

    // ----- parameters and names ---------------------------------------------

    pub fn set_param(&mut self, node: NodeId, name: &str, value: Value) {
        let n = &mut self.doc.snarl[node];
        n.params.insert(name.to_owned(), value.clone());
        let id = n.id.clone();
        let composite = n.is_composite();
        self.doc.dirty = true;
        if !self.live() {
            return;
        }
        // A composite's `inner.param` targets the flattened inner node.
        let target = if composite {
            name.split_once('.')
                .map(|(inner, p)| (ruro_graph::composite::flat_id(&id, inner), p.to_owned()))
        } else {
            Some((id, name.to_owned()))
        };
        if let Some((node, name)) = target
            && self.metrics.contains_key(&node)
        {
            self.link.send(Command::SetParam {
                daemon: self.node_daemon(&node),
                node,
                name,
                value: value.to_string(),
            });
        }
    }

    /// Renames a node after checking the new name. Returns true on success.
    pub fn rename_node(&mut self, id: NodeId, new: &str) -> bool {
        let old = self.doc.snarl[id].id.clone();
        if new == old {
            return true;
        }
        if !ruro_graph::is_valid_ident(new) {
            self.toast(
                Severity::Error,
                "Node names may only contain letters, digits, - and _",
            );
            return false;
        }
        if self.doc.find(new).is_some() {
            self.toast(
                Severity::Error,
                format!("A node named {new} already exists"),
            );
            return false;
        }
        for w in &mut self.watches {
            if w.node == old {
                w.node = new.to_owned();
            }
        }
        self.doc.rename_node(id, new);
        true
    }

    // ----- clipboard ----------------------------------------------------------

    /// The selected nodes and the wires between them, as a graph.
    fn selection_graph(&self, nodes: &[NodeId]) -> Graph {
        let full = self.doc.to_graph(&self.registry);
        let ids: Vec<&str> = nodes
            .iter()
            .filter_map(|n| self.doc.snarl.get_node(*n))
            .map(|n| n.id.as_str())
            .collect();
        let mut g = Graph::new("clipboard");
        for (id, inst) in &full.nodes {
            if ids.contains(&id.as_str()) {
                g.nodes.insert(id.clone(), inst.clone());
                if let Some(cfg) = inst.group.as_ref().and_then(|gr| full.groups.get(gr)) {
                    g.groups.insert(inst.group.clone().unwrap(), cfg.clone());
                }
            }
        }
        g.wires = full
            .wires
            .into_iter()
            .filter(|w| g.nodes.contains_key(&w.from.node) && g.nodes.contains_key(&w.to.node))
            .collect();
        g
    }

    pub(crate) fn copy(&mut self, ctx: &egui::Context, cut: bool) {
        let nodes = self.selected_nodes();
        if nodes.is_empty() {
            return;
        }
        let g = self.selection_graph(&nodes);
        let Ok(yaml) = g.to_yaml() else { return };
        ctx.copy_text(yaml.clone());
        self.clipboard = Some(yaml);
        self.paste_count = 0;
        if cut {
            self.delete_nodes(&nodes);
        }
    }

    pub(crate) fn paste(&mut self, text: &str) {
        let Ok(g) = Graph::from_yaml(text) else {
            self.toast(Severity::Info, "The clipboard does not hold ruro nodes");
            return;
        };
        if g.nodes.is_empty() {
            return;
        }
        self.paste_count += 1;
        let added = self.insert_graph(&g, Vec2::splat(PASTE_STEP * self.paste_count as f32));
        self.marked = added.iter().copied().collect();
        self.focused = added.first().copied();
    }

    pub(crate) fn duplicate(&mut self) {
        let nodes = self.selected_nodes();
        if nodes.is_empty() {
            return;
        }
        let g = self.selection_graph(&nodes);
        let added = self.insert_graph(&g, Vec2::splat(PASTE_STEP));
        self.marked = added.iter().copied().collect();
        self.focused = added.first().copied();
    }

    /// Adds the nodes and wires of `g` to the document, renaming nodes whose
    /// ids are taken. Returns the new canvas nodes.
    pub(crate) fn insert_graph(&mut self, g: &Graph, offset: Vec2) -> Vec<NodeId> {
        let mut ids: HashMap<&str, (NodeId, String)> = HashMap::new();
        let mut added = Vec::new();
        for (id, inst) in &g.nodes {
            let new = self.doc.unique_id(id);
            let pos = inst.pos.map_or(Pos2::ZERO, |[x, y]| pos2(x, y)) + offset;
            let nid = self
                .doc
                .snarl
                .insert_node(pos, GNode::from_instance(&new, inst));
            ids.insert(id.as_str(), (nid, new));
            added.push(nid);
        }
        for (name, cfg) in &g.groups {
            self.doc
                .groups
                .entry(name.clone())
                .or_insert_with(|| cfg.clone());
        }
        self.doc.refresh_composites(&self.registry);
        for w in &g.wires {
            let (Some((from, from_id)), Some((to, to_id))) =
                (ids.get(w.from.node.as_str()), ids.get(w.to.node.as_str()))
            else {
                continue;
            };
            let out = self
                .doc
                .descriptor(&self.doc.snarl[*from], &self.registry)
                .and_then(|d| d.outputs.iter().position(|p| p.name == w.from.port));
            let inp = self
                .doc
                .descriptor(&self.doc.snarl[*to], &self.registry)
                .and_then(|d| d.inputs.iter().position(|p| p.name == w.to.port));
            if let (Some(output), Some(input)) = (out, inp) {
                self.doc.snarl.connect(
                    OutPinId {
                        node: *from,
                        output,
                    },
                    InPinId { node: *to, input },
                );
                if let Some(depth) = w.depth {
                    self.doc.depths.insert(
                        (
                            PortRef::new(from_id, &w.from.port),
                            PortRef::new(to_id, &w.to.port),
                        ),
                        depth,
                    );
                }
            }
        }
        self.doc.touch_structure();
        added
    }

    // ----- selection ----------------------------------------------------------

    pub(crate) fn select_all(&mut self, _ctx: &egui::Context) {
        self.marked = self.doc.snarl.node_ids().map(|(id, _)| id).collect();
    }

    pub(crate) fn delete_selection(&mut self) {
        let nodes = self.selected_nodes();
        self.delete_nodes(&nodes);
    }

    pub(crate) fn delete_nodes(&mut self, nodes: &[NodeId]) {
        let mut any = false;
        for &id in nodes {
            if let Some(node) = self.doc.snarl.get_node(id) {
                let name = node.id.clone();
                self.doc.snarl.remove_node(id);
                self.doc.forget_node(&name);
                any = true;
            }
        }
        if any {
            self.focused = None;
            self.marked.clear();
            self.doc.touch_structure();
        }
    }

    /// Node rectangles in graph space, from last frame's layout.
    fn graph_rects(&self, nodes: &[NodeId]) -> Vec<(NodeId, Rect)> {
        let inv = self.canvas_transform.inverse();
        nodes
            .iter()
            .filter_map(|n| {
                let screen = self.node_rects.iter().find(|(id, _)| id == n)?.1;
                Some((*n, inv * screen))
            })
            .collect()
    }

    pub(crate) fn align(&mut self, op: AlignOp) {
        let nodes = self.selected_nodes();
        let rects = self.graph_rects(&nodes);
        if rects.len() < 2 {
            self.toast(Severity::Info, "Select two or more nodes to align them");
            return;
        }
        let mut targets: Vec<(NodeId, Vec2)> = Vec::new();
        match op {
            AlignOp::Left => {
                let x = rects.iter().map(|(_, r)| r.left()).fold(f32::MAX, f32::min);
                targets.extend(rects.iter().map(|(n, r)| (*n, vec2(x - r.left(), 0.0))));
            }
            AlignOp::Top => {
                let y = rects.iter().map(|(_, r)| r.top()).fold(f32::MAX, f32::min);
                targets.extend(rects.iter().map(|(n, r)| (*n, vec2(0.0, y - r.top()))));
            }
            AlignOp::CenterX => {
                let x = rects.iter().map(|(_, r)| r.center().x).sum::<f32>() / rects.len() as f32;
                targets.extend(rects.iter().map(|(n, r)| (*n, vec2(x - r.center().x, 0.0))));
            }
            AlignOp::CenterY => {
                let y = rects.iter().map(|(_, r)| r.center().y).sum::<f32>() / rects.len() as f32;
                targets.extend(rects.iter().map(|(n, r)| (*n, vec2(0.0, y - r.center().y))));
            }
            AlignOp::DistributeX | AlignOp::DistributeY => {
                let horizontal = op == AlignOp::DistributeX;
                let key = |r: &Rect| {
                    if horizontal {
                        r.center().x
                    } else {
                        r.center().y
                    }
                };
                let mut sorted = rects.clone();
                sorted.sort_by(|a, b| key(&a.1).total_cmp(&key(&b.1)));
                let (first, last) = (key(&sorted[0].1), key(&sorted[sorted.len() - 1].1));
                let step = (last - first) / (sorted.len() - 1) as f32;
                for (i, (n, r)) in sorted.iter().enumerate() {
                    let d = first + step * i as f32 - key(r);
                    targets.push((
                        *n,
                        if horizontal {
                            vec2(d, 0.0)
                        } else {
                            vec2(0.0, d)
                        },
                    ));
                }
            }
        }
        for (n, delta) in targets {
            if let Some(info) = self.doc.snarl.get_node_info_mut(n) {
                info.pos += delta;
            }
        }
    }

    // ----- subgraphs ------------------------------------------------------------

    pub(crate) fn group_selection(&mut self) {
        let mut ids: Vec<String> = self
            .selected_nodes()
            .iter()
            .map(|n| self.doc.snarl[*n].id.clone())
            .collect();
        if ids.is_empty() {
            self.toast(Severity::Info, "Select the nodes to group into a subgraph");
            return;
        }
        ids.sort();
        let mut g = self.doc.to_graph(&self.registry);
        let new_id = g.unique_id("group");
        match g.extract_composite(&ids, &new_id) {
            Ok(()) => {
                self.reload(&g);
                self.focused = self.doc.find(&new_id);
                self.marked.clear();
            }
            Err(e) => self.toast(Severity::Error, e.to_string()),
        }
    }

    pub(crate) fn ungroup(&mut self, id: &str) {
        let mut g = self.doc.to_graph(&self.registry);
        match g.inline_composite(id, &self.doc.base_dir()) {
            Ok(added) => {
                self.reload(&g);
                self.marked = added.iter().filter_map(|id| self.doc.find(id)).collect();
                self.focused = None;
            }
            Err(e) => self.toast(Severity::Error, e.to_string()),
        }
    }

    /// Replaces the document content after a whole-graph edit, as one undo step.
    fn reload(&mut self, g: &Graph) {
        self.doc.commit();
        for w in self.doc.load_graph(g, &self.registry) {
            self.toast(Severity::Warning, w);
        }
        self.doc.touch_structure();
        self.rename = None;
    }

    pub(crate) fn open_node_source(&mut self, id: &str) {
        let Some(node) = self.doc.find(id).map(|n| self.doc.snarl[n].clone()) else {
            return;
        };
        let source = self
            .doc
            .descriptor(&node, &self.registry)
            .and_then(|d| d.source.clone());
        let Some(source) = source else {
            self.toast(
                Severity::Info,
                format!("The source of {} is not known", node.kind),
            );
            return;
        };
        match crate::source::open(&source) {
            Ok(how) => self.toast(Severity::Info, how),
            Err(e) => self.toast(Severity::Warning, e),
        }
    }
}
