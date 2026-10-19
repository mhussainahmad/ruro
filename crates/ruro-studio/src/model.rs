//! The edited document: a graph laid out on a canvas, with its edit history.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use egui::{Pos2, pos2};
use egui_snarl::{InPinId, NodeId, OutPinId, Snarl};
use indexmap::IndexMap;
use ruro_graph::{
    ClockSource, Graph, GroupConfig, NodeDescriptor, NodeInstance, Overlay, PortList, PortRef,
    Registry, RestartPolicy, Wire,
};
use serde_json::Value;

/// A node on the canvas.
#[derive(Debug, Clone, PartialEq)]
pub struct GNode {
    pub id: String,
    pub kind: String,
    pub group: Option<String>,
    pub rate: Option<f64>,
    pub params: BTreeMap<String, Value>,
    pub restart: Option<RestartPolicy>,
    /// Inline subgraph of a composite node.
    pub sub: Option<Box<Graph>>,
}

impl GNode {
    pub fn new(id: String, kind: String) -> Self {
        Self {
            id,
            kind,
            group: None,
            rate: None,
            params: BTreeMap::new(),
            restart: None,
            sub: None,
        }
    }

    /// Current value of a parameter, falling back to the descriptor default.
    pub fn param<'a>(&'a self, desc: &'a ruro_graph::ParamDesc) -> &'a Value {
        self.params.get(&desc.name).unwrap_or(&desc.default)
    }

    pub fn is_composite(&self) -> bool {
        ruro_graph::is_composite_kind(&self.kind)
    }

    pub fn instance(&self, pos: Option<Pos2>) -> NodeInstance {
        NodeInstance {
            kind: self.kind.clone(),
            group: self.group.clone().filter(|g| !g.is_empty()),
            rate: self.rate,
            params: self.params.clone(),
            restart: self.restart,
            graph: self.sub.clone(),
            pos: pos.map(|p| [p.x.round(), p.y.round()]),
        }
    }

    pub fn from_instance(id: &str, inst: &NodeInstance) -> Self {
        Self {
            id: id.to_owned(),
            kind: inst.kind.clone(),
            group: inst.group.clone(),
            rate: inst.rate,
            params: inst.params.clone(),
            restart: inst.restart,
            sub: inst.graph.clone(),
        }
    }
}

/// Everything about a document that undo restores.
#[derive(Clone)]
pub struct Snapshot {
    name: String,
    snarl: Snarl<GNode>,
    groups: IndexMap<String, GroupConfig>,
    inputs: IndexMap<String, PortList>,
    outputs: IndexMap<String, PortRef>,
    depths: BTreeMap<(PortRef, PortRef), usize>,
    clock: ClockSource,
    overlays: IndexMap<String, Overlay>,
}

impl Snapshot {
    fn same(&self, other: &Snapshot) -> bool {
        if self.name != other.name
            || self.clock != other.clock
            || self.overlays != other.overlays
            || self.groups != other.groups
            || self.inputs != other.inputs
            || self.outputs != other.outputs
            || self.depths != other.depths
        {
            return false;
        }
        sorted_nodes(&self.snarl) == sorted_nodes(&other.snarl)
            && sorted_wires(&self.snarl) == sorted_wires(&other.snarl)
    }

    /// Nodes by canvas id, for matching nodes across snapshots.
    pub fn nodes(&self) -> HashMap<NodeId, &GNode> {
        self.snarl
            .nodes_ids_data()
            .map(|(id, n)| (id, &n.value))
            .collect()
    }
}

fn sorted_nodes(s: &Snarl<GNode>) -> Vec<(NodeId, Pos2, &GNode)> {
    let mut v: Vec<(NodeId, Pos2, &GNode)> = s.nodes_pos_ids().collect();
    v.sort_by_key(|(id, _, _)| *id);
    v
}

fn sorted_wires(s: &Snarl<GNode>) -> Vec<(OutPinId, InPinId)> {
    let mut v: Vec<(OutPinId, InPinId)> = s.wires().collect();
    v.sort();
    v
}

/// Undo and redo stacks of a document.
struct History {
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// The state after the last committed edit.
    current: Snapshot,
    /// The state that is on disk, if any.
    saved: Option<Snapshot>,
}

/// Most undo steps kept per document.
const MAX_UNDO: usize = 200;

static NEXT_UID: AtomicU64 = AtomicU64::new(1);

pub struct Doc {
    /// Identifies the document for the lifetime of the process.
    pub uid: u64,
    pub name: String,
    pub path: Option<PathBuf>,
    pub snarl: Snarl<GNode>,
    pub groups: IndexMap<String, GroupConfig>,
    /// Exposed ports, when this graph is used as a subgraph.
    pub inputs: IndexMap<String, PortList>,
    pub outputs: IndexMap<String, PortRef>,
    /// Queue depth of wires that set one.
    pub depths: BTreeMap<(PortRef, PortRef), usize>,
    pub clock: ClockSource,
    /// Named variants of the graph, kept as they are in the file.
    pub overlays: IndexMap<String, Overlay>,
    /// Differs from the file on disk (or has never been saved).
    pub dirty: bool,
    /// Bumped by edits that change what would be deployed (not layout or parameters).
    pub structure_rev: u64,
    /// Descriptors presented by composite nodes, by node id.
    pub composites: HashMap<String, NodeDescriptor>,
    /// For a subgraph opened from an inline composite: the parent document and node id.
    pub parent: Option<(u64, String)>,
    /// Wires of nodes whose type is not known yet, such as graphs opened
    /// before the daemon sent its node types. Saved and deployed as they are
    /// until [`Doc::resolve_pending`] puts them on the canvas.
    pub pending: Vec<Wire>,
    history: History,
}

impl Doc {
    pub fn new(name: &str) -> Self {
        let empty = Snapshot {
            name: String::new(),
            snarl: Snarl::new(),
            groups: IndexMap::new(),
            inputs: IndexMap::new(),
            outputs: IndexMap::new(),
            depths: BTreeMap::new(),
            clock: ClockSource::Wall,
            overlays: IndexMap::new(),
        };
        let mut doc = Self {
            uid: NEXT_UID.fetch_add(1, Ordering::Relaxed),
            name: name.into(),
            path: None,
            snarl: Snarl::new(),
            groups: IndexMap::new(),
            inputs: IndexMap::new(),
            outputs: IndexMap::new(),
            depths: BTreeMap::new(),
            clock: ClockSource::Wall,
            overlays: IndexMap::new(),
            dirty: false,
            structure_rev: 0,
            composites: HashMap::new(),
            parent: None,
            pending: Vec::new(),
            history: History {
                undo: Vec::new(),
                redo: Vec::new(),
                current: empty,
                saved: None,
            },
        };
        doc.reset_history(false);
        doc
    }

    pub fn from_graph(
        graph: &Graph,
        registry: &Registry,
        path: Option<PathBuf>,
    ) -> (Self, Vec<String>) {
        let mut doc = Self::new(&graph.name);
        doc.path = path;
        let warnings = doc.load_graph(graph, registry);
        doc.reset_history(doc.path.is_some());
        (doc, warnings)
    }

    /// Replaces the content with `graph`, keeping identity, path and history.
    pub fn load_graph(&mut self, graph: &Graph, registry: &Registry) -> Vec<String> {
        self.name = graph.name.clone();
        self.snarl = Snarl::new();
        self.groups = graph.groups.clone();
        self.inputs = graph.inputs.clone();
        self.outputs = graph.outputs.clone();
        self.clock = graph.clock;
        self.overlays = graph.overlays.clone();
        self.depths = graph
            .wires
            .iter()
            .filter_map(|w| Some(((w.from.clone(), w.to.clone()), w.depth?)))
            .collect();
        let mut warnings = Vec::new();
        let layout = auto_layout(graph);
        let mut ids: HashMap<&str, NodeId> = HashMap::new();
        for (id, inst) in &graph.nodes {
            let pos = inst
                .pos
                .map(|[x, y]| pos2(x, y))
                .unwrap_or_else(|| layout[id.as_str()]);
            if !inst.is_composite() && registry.get(&inst.kind).is_none() {
                warnings.push(format!("{id}: unknown node type `{}`", inst.kind));
            }
            ids.insert(
                id.as_str(),
                self.snarl.insert_node(pos, GNode::from_instance(id, inst)),
            );
        }
        warnings.extend(self.refresh_composites(registry));
        self.pending.clear();
        for wire in &graph.wires {
            let typed = |n: &str| {
                ids.get(n)
                    .is_some_and(|&id| self.descriptor(&self.snarl[id], registry).is_some())
            };
            let exists = |n: &str| ids.contains_key(n);
            if exists(&wire.from.node)
                && exists(&wire.to.node)
                && !(typed(&wire.from.node) && typed(&wire.to.node))
            {
                self.pending.push(wire.clone());
                continue;
            }
            match self.resolve_wire(wire, registry) {
                Some((o, i)) => {
                    self.snarl.connect(o, i);
                }
                None => warnings.push(format!("dropped wire `{wire}`: port not found")),
            }
        }
        self.structure_rev += 1;
        warnings
    }

    /// The canvas pins a wire connects, if its nodes and ports exist.
    fn resolve_wire(&self, wire: &Wire, registry: &Registry) -> Option<(OutPinId, InPinId)> {
        let from = self.find(&wire.from.node)?;
        let to = self.find(&wire.to.node)?;
        let output = self
            .descriptor(&self.snarl[from], registry)?
            .outputs
            .iter()
            .position(|p| p.name == wire.from.port)?;
        let input = self
            .descriptor(&self.snarl[to], registry)?
            .inputs
            .iter()
            .position(|p| p.name == wire.to.port)?;
        Some((OutPinId { node: from, output }, InPinId { node: to, input }))
    }

    /// Puts pending wires whose node types are now known on the canvas,
    /// without making an edit of it. Returns warnings for wires whose ports
    /// do not exist, which are dropped.
    pub fn resolve_pending(&mut self, registry: &Registry) -> Vec<String> {
        let mut warnings = Vec::new();
        for wire in std::mem::take(&mut self.pending) {
            let typed = |n: &str| {
                self.find(n)
                    .is_some_and(|id| self.descriptor(&self.snarl[id], registry).is_some())
            };
            if !(typed(&wire.from.node) && typed(&wire.to.node)) {
                self.pending.push(wire);
                continue;
            }
            let Some((o, i)) = self.resolve_wire(&wire, registry) else {
                warnings.push(format!("dropped wire `{wire}`: port not found"));
                continue;
            };
            self.snarl.connect(o, i);
            // The wire was there all along: the history gets it too.
            let h = &mut self.history;
            let snaps = h.undo.iter_mut().chain(&mut h.redo);
            for snap in snaps
                .chain(std::iter::once(&mut h.current))
                .chain(h.saved.as_mut())
            {
                let id = |n: NodeId| snap.snarl.get_node(n).map(|g| g.id.as_str());
                if id(o.node) == Some(wire.from.node.as_str())
                    && id(i.node) == Some(wire.to.node.as_str())
                {
                    snap.snarl.connect(o, i);
                }
            }
        }
        warnings
    }

    /// The descriptor of a canvas node: a registry type or a composite's interface.
    pub fn descriptor<'a>(
        &'a self,
        node: &GNode,
        registry: &'a Registry,
    ) -> Option<&'a NodeDescriptor> {
        if node.is_composite() {
            self.composites.get(&node.id)
        } else {
            registry.get(&node.kind)
        }
    }

    /// Directory that subgraph file references are relative to.
    pub fn base_dir(&self) -> PathBuf {
        self.path
            .as_deref()
            .and_then(Path::parent)
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// Recomputes the interfaces of composite nodes. Returns the errors.
    pub fn refresh_composites(&mut self, registry: &Registry) -> Vec<String> {
        let base = self.base_dir();
        let mut errors = Vec::new();
        self.composites = self
            .snarl
            .nodes()
            .filter(|n| n.is_composite())
            .filter_map(|n| {
                match ruro_graph::composite_descriptor(&n.id, &n.instance(None), registry, &base) {
                    Ok(d) => Some((n.id.clone(), d)),
                    Err(e) => {
                        errors.push(e.to_string());
                        None
                    }
                }
            })
            .collect();
        errors
    }

    pub fn to_graph(&self, registry: &Registry) -> Graph {
        let mut graph = Graph::new(&self.name);
        graph.groups = self.groups.clone();
        graph.inputs = self.inputs.clone();
        graph.outputs = self.outputs.clone();
        graph.clock = self.clock;
        graph.overlays = self.overlays.clone();
        let mut entries: Vec<(NodeId, Pos2, &GNode)> = self.snarl.nodes_pos_ids().collect();
        entries.sort_by_key(|(id, _, _)| *id);
        for (_, pos, n) in entries {
            graph.nodes.insert(n.id.clone(), n.instance(Some(pos)));
        }
        let mut wires: Vec<Wire> = self
            .snarl
            .wires()
            .filter_map(|(o, i)| {
                let from = &self.snarl[o.node];
                let to = &self.snarl[i.node];
                let out = self.descriptor(from, registry)?.outputs.get(o.output)?;
                let inp = self.descriptor(to, registry)?.inputs.get(i.input)?;
                let from = PortRef::new(&from.id, &out.name);
                let to = PortRef::new(&to.id, &inp.name);
                let depth = self.depths.get(&(from.clone(), to.clone())).copied();
                Some(Wire { from, to, depth })
            })
            .collect();
        wires.extend(
            self.pending
                .iter()
                .filter(|w| graph.nodes.contains_key(&w.from.node))
                .filter(|w| graph.nodes.contains_key(&w.to.node))
                .cloned(),
        );
        wires.sort_by(|a, b| (&a.from, &a.to).cmp(&(&b.from, &b.to)));
        graph.wires = wires;
        graph
    }

    pub fn find(&self, id: &str) -> Option<NodeId> {
        self.snarl
            .nodes_ids_data()
            .find(|(_, n)| n.value.id == id)
            .map(|(nid, _)| nid)
    }

    /// `base` if no node uses it, else `base` with the next free number: a
    /// copy of `blur2` becomes `blur3`, not `blur22`.
    pub fn unique_id(&self, base: &str) -> String {
        let taken = |id: &str| self.snarl.nodes().any(|n| n.id == id);
        if !taken(base) {
            return base.to_owned();
        }
        let stem = base.trim_end_matches(|c: char| c.is_ascii_digit());
        let base = if stem.is_empty() { base } else { stem };
        (2..)
            .map(|i| format!("{base}{i}"))
            .find(|id| !taken(id))
            .unwrap()
    }

    pub fn add_node(&mut self, desc: &NodeDescriptor, pos: Pos2) -> NodeId {
        let id = self.unique_id(&desc.short_name().replace('-', "_"));
        self.touch_structure();
        self.snarl
            .insert_node(pos, GNode::new(id, desc.kind.clone()))
    }

    /// Renames a node, updating references to it by name.
    pub fn rename_node(&mut self, node: NodeId, new: &str) {
        let old = std::mem::replace(&mut self.snarl[node].id, new.to_owned());
        // An inline subgraph named after its node follows the rename.
        if let Some(sub) = self.snarl[node].sub.as_mut()
            && sub.name == old
        {
            sub.name = new.to_owned();
        }
        let fix = |p: &PortRef| {
            if p.node == old {
                PortRef::new(new, &p.port)
            } else {
                p.clone()
            }
        };
        self.depths = std::mem::take(&mut self.depths)
            .into_iter()
            .map(|((a, b), d)| ((fix(&a), fix(&b)), d))
            .collect();
        for list in self.inputs.values_mut() {
            list.0 = list.0.iter().map(fix).collect();
        }
        for p in self.outputs.values_mut() {
            *p = fix(p);
        }
        for w in &mut self.pending {
            w.from = fix(&w.from);
            w.to = fix(&w.to);
        }
        if let Some(d) = self.composites.remove(&old) {
            self.composites.insert(new.to_owned(), d);
        }
        self.touch_structure();
    }

    pub fn touch_structure(&mut self) {
        self.structure_rev += 1;
        self.dirty = true;
    }

    // ----- history ----------------------------------------------------------

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            name: self.name.clone(),
            snarl: self.snarl.clone(),
            groups: self.groups.clone(),
            inputs: self.inputs.clone(),
            outputs: self.outputs.clone(),
            depths: self.depths.clone(),
            clock: self.clock,
            overlays: self.overlays.clone(),
        }
    }

    /// Forgets the history. The current state becomes the base and, if
    /// `saved`, the state on disk.
    pub fn reset_history(&mut self, saved: bool) {
        let now = self.snapshot();
        self.history.undo.clear();
        self.history.redo.clear();
        self.history.saved = saved.then(|| now.clone());
        self.history.current = now;
        self.dirty = !saved && self.snarl.nodes().next().is_some();
    }

    /// Records the edits made since the last commit as one undo step.
    /// Returns true if there were any.
    pub fn commit(&mut self) -> bool {
        let now = self.snapshot();
        if now.same(&self.history.current) {
            return false;
        }
        let prev = std::mem::replace(&mut self.history.current, now);
        self.history.undo.push(prev);
        if self.history.undo.len() > MAX_UNDO {
            self.history.undo.remove(0);
        }
        self.history.redo.clear();
        self.update_dirty();
        true
    }

    /// Marks the current state as the one on disk.
    pub fn mark_saved(&mut self) {
        self.commit();
        self.history.saved = Some(self.history.current.clone());
        self.dirty = false;
    }

    fn update_dirty(&mut self) {
        self.dirty = match &self.history.saved {
            Some(saved) => !saved.same(&self.history.current),
            None => self.history.current.snarl.nodes().next().is_some(),
        };
    }

    pub fn can_undo(&self) -> bool {
        !self.history.undo.is_empty() || !self.snapshot().same(&self.history.current)
    }

    pub fn can_redo(&self) -> bool {
        !self.history.redo.is_empty()
    }

    /// Steps back one edit. Returns the state that was left, for resyncing.
    pub fn undo(&mut self) -> Option<Snapshot> {
        self.commit();
        let snap = self.history.undo.pop()?;
        let left = std::mem::replace(&mut self.history.current, snap.clone());
        self.history.redo.push(left.clone());
        self.restore(snap);
        Some(left)
    }

    /// Re-applies the last undone edit. Returns the state that was left.
    pub fn redo(&mut self) -> Option<Snapshot> {
        self.commit();
        let snap = self.history.redo.pop()?;
        let left = std::mem::replace(&mut self.history.current, snap.clone());
        self.history.undo.push(left.clone());
        self.restore(snap);
        Some(left)
    }

    fn restore(&mut self, s: Snapshot) {
        self.name = s.name;
        self.snarl = s.snarl;
        self.groups = s.groups;
        self.inputs = s.inputs;
        self.outputs = s.outputs;
        self.depths = s.depths;
        self.clock = s.clock;
        self.overlays = s.overlays;
        self.structure_rev += 1;
        self.update_dirty();
    }
}

/// Positions for nodes without one: columns by distance from the sources.
fn auto_layout(graph: &Graph) -> HashMap<&str, Pos2> {
    let mut depth: HashMap<&str, usize> = graph.nodes.keys().map(|k| (k.as_str(), 0)).collect();
    // Longest-path layering, bounded so cycles terminate.
    for _ in 0..graph.nodes.len() {
        let mut changed = false;
        for w in &graph.wires {
            let (Some(&a), Some(&b)) = (
                depth.get(w.from.node.as_str()),
                depth.get(w.to.node.as_str()),
            ) else {
                continue;
            };
            if b < a + 1 && a + 1 < graph.nodes.len() {
                depth.insert(w.to.node.as_str(), a + 1);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut rows: HashMap<usize, usize> = HashMap::new();
    graph
        .nodes
        .keys()
        .map(|k| {
            let col = depth[k.as_str()];
            let row = rows.entry(col).or_default();
            let pos = pos2(40.0 + col as f32 * 300.0, 60.0 + *row as f32 * 190.0);
            *row += 1;
            (k.as_str(), pos)
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ruro_graph::{PortDesc, Registry};

    pub(crate) fn registry() -> Registry {
        let p = |n: &str| PortDesc {
            name: n.into(),
            ty: "std/Scalar".into(),
        };
        Registry::new([
            NodeDescriptor {
                kind: "t/a".into(),
                default_rate: Some(10.0),
                outputs: vec![p("x"), p("y")],
                ..Default::default()
            },
            NodeDescriptor {
                kind: "t/b".into(),
                inputs: vec![p("i"), p("j")],
                outputs: vec![p("o")],
                ..Default::default()
            },
        ])
    }

    const ROUNDTRIP: &str = r#"
name: rt
clock: sim
groups:
  g1: { machine: robot, realtime: { priority: 50 } }
nodes:
  a: { type: t/a, group: g1, rate: 5, params: { k: 1 }, restart: always }
  b: { type: t/b }
  c:
    type: composite
    graph:
      name: inner
      inputs: { i: b.i }
      outputs: { o: b.o }
      nodes:
        b: { type: t/b }
wires:
  - a.x -> b.i
  - { from: a.y, to: b.j, depth: 4 }
  - a.x -> c.i
overlays:
  real:
    clock: wall
    nodes:
      a: { params: { k: 2 } }
      b: null
"#;

    #[test]
    fn graph_roundtrip_through_canvas() {
        let g = Graph::from_yaml(ROUNDTRIP).unwrap();
        let reg = registry();
        let (doc, warnings) = Doc::from_graph(&g, &reg, None);
        assert!(warnings.is_empty(), "{warnings:?}");
        let mut back = doc.to_graph(&reg);
        for n in back.nodes.values_mut() {
            n.pos = None;
        }
        let mut expected = g.clone();
        expected
            .wires
            .sort_by(|a, b| (&a.from, &a.to).cmp(&(&b.from, &b.to)));
        assert_eq!(back, expected);
    }

    #[test]
    fn unknown_ports_are_reported() {
        let g = Graph::from_yaml(
            "name: w\nnodes:\n  a: { type: t/a }\n  b: { type: t/b }\nwires: [a.zz -> b.i]\n",
        )
        .unwrap();
        let (_, warnings) = Doc::from_graph(&g, &registry(), None);
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn wires_to_types_not_yet_known_survive_until_the_registry_arrives() {
        // Files open before the daemon has sent its node types.
        let g = Graph::from_yaml(
            "name: w\nnodes:\n  a: { type: t/a }\n  b: { type: t/b }\nwires: [a.x -> b.i, a.zz -> b.i]\n",
        )
        .unwrap();
        let empty = Registry::default();
        let (mut doc, warnings) = Doc::from_graph(&g, &empty, Some("/tmp/w.yaml".into()));
        assert_eq!(warnings.len(), 2, "two unknown types: {warnings:?}");
        assert_eq!(doc.snarl.wires().count(), 0);
        assert_eq!(
            doc.to_graph(&empty).wires,
            g.wires,
            "saved and deployed as loaded"
        );

        let reg = registry();
        let warnings = doc.resolve_pending(&reg);
        assert_eq!(warnings.len(), 1, "a.zz does not exist: {warnings:?}");
        assert_eq!(doc.snarl.wires().count(), 1);
        assert_eq!(doc.to_graph(&reg).wires, g.wires[..1]);
        assert!(doc.pending.is_empty());
        assert!(!doc.dirty && !doc.can_undo(), "nothing the user did");
    }

    #[test]
    fn layout_places_downstream_nodes_to_the_right() {
        let g = Graph::from_yaml(
            "name: l\nnodes:\n  a: { type: t/a }\n  b: { type: t/b }\nwires: [a.x -> b.i]\n",
        )
        .unwrap();
        let l = auto_layout(&g);
        assert!(l["b"].x > l["a"].x);
    }

    #[test]
    fn undo_and_redo_restore_states_and_dirtiness() {
        let reg = registry();
        let g = Graph::from_yaml("name: u\nnodes:\n  a: { type: t/a }\n").unwrap();
        let (mut doc, _) = Doc::from_graph(&g, &reg, Some("/tmp/u.yaml".into()));
        assert!(!doc.dirty && !doc.can_undo());

        let b = doc.add_node(reg.get("t/b").unwrap(), pos2(300.0, 0.0));
        assert!(doc.commit());
        assert!(!doc.commit(), "nothing new to commit");
        doc.snarl.get_node_info_mut(b).unwrap().pos = pos2(320.0, 10.0);
        doc.commit();
        assert!(doc.dirty);

        doc.undo().unwrap();
        assert_eq!(doc.snarl.get_node_info(b).unwrap().pos, pos2(300.0, 0.0));
        doc.undo().unwrap();
        assert_eq!(doc.snarl.nodes().count(), 1);
        assert!(!doc.dirty, "back at the saved state");
        assert!(doc.undo().is_none());

        doc.redo().unwrap();
        doc.redo().unwrap();
        assert!(doc.redo().is_none());
        assert_eq!(doc.snarl.get_node_info(b).unwrap().pos, pos2(320.0, 10.0));
        assert!(doc.dirty);

        // A new edit drops the redo stack.
        doc.undo().unwrap();
        doc.name = "renamed".into();
        doc.commit();
        assert!(!doc.can_redo());
    }

    #[test]
    fn uncommitted_edits_are_undone_first() {
        let reg = registry();
        let (mut doc, _) = Doc::from_graph(&Graph::new("x"), &reg, None);
        doc.add_node(reg.get("t/a").unwrap(), pos2(0.0, 0.0));
        assert!(doc.can_undo());
        doc.undo().unwrap();
        assert_eq!(doc.snarl.nodes().count(), 0);
    }
}
