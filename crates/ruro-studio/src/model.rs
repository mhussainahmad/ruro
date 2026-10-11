//! The edited document: a graph laid out on a canvas.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use egui::{Pos2, pos2};
use egui_snarl::{InPinId, NodeId, OutPinId, Snarl};
use ruro_graph::{Graph, NodeDescriptor, NodeInstance, PortRef, Registry, RestartPolicy, Wire};
use serde_json::Value;

/// A node on the canvas.
#[derive(Debug, Clone)]
pub struct GNode {
    pub id: String,
    pub kind: String,
    pub group: Option<String>,
    pub rate: Option<f64>,
    pub params: BTreeMap<String, Value>,
    pub restart: Option<RestartPolicy>,
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
        }
    }

    /// Current value of a parameter, falling back to the descriptor default.
    pub fn param<'a>(&'a self, desc: &'a ruro_graph::ParamDesc) -> &'a Value {
        self.params.get(&desc.name).unwrap_or(&desc.default)
    }
}

pub struct Doc {
    pub name: String,
    pub path: Option<PathBuf>,
    pub snarl: Snarl<GNode>,
    /// Edits since the last save.
    pub dirty: bool,
    /// Bumped by edits that change what would be deployed (not layout or parameters).
    pub structure_rev: u64,
}

impl Doc {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            path: None,
            snarl: Snarl::new(),
            dirty: false,
            structure_rev: 0,
        }
    }

    pub fn from_graph(
        graph: &Graph,
        registry: &Registry,
        path: Option<PathBuf>,
    ) -> (Self, Vec<String>) {
        let mut doc = Self::new(&graph.name);
        doc.path = path;
        let mut warnings = Vec::new();
        let layout = auto_layout(graph);
        let mut ids: HashMap<&str, NodeId> = HashMap::new();
        for (id, inst) in &graph.nodes {
            let pos = inst
                .pos
                .map(|[x, y]| pos2(x, y))
                .unwrap_or_else(|| layout[id.as_str()]);
            let node = GNode {
                id: id.clone(),
                kind: inst.kind.clone(),
                group: inst.group.clone(),
                rate: inst.rate,
                params: inst.params.clone(),
                restart: inst.restart,
            };
            if registry.get(&inst.kind).is_none() {
                warnings.push(format!("{id}: unknown node type `{}`", inst.kind));
            }
            ids.insert(id.as_str(), doc.snarl.insert_node(pos, node));
        }
        for wire in &graph.wires {
            let resolved = (|| {
                let from = *ids.get(wire.from.node.as_str())?;
                let to = *ids.get(wire.to.node.as_str())?;
                let out = registry
                    .get(&graph.nodes[&wire.from.node].kind)?
                    .outputs
                    .iter()
                    .position(|p| p.name == wire.from.port)?;
                let inp = registry
                    .get(&graph.nodes[&wire.to.node].kind)?
                    .inputs
                    .iter()
                    .position(|p| p.name == wire.to.port)?;
                Some((
                    OutPinId {
                        node: from,
                        output: out,
                    },
                    InPinId {
                        node: to,
                        input: inp,
                    },
                ))
            })();
            match resolved {
                Some((o, i)) => {
                    doc.snarl.connect(o, i);
                }
                None => warnings.push(format!("dropped wire `{wire}`: port not found")),
            }
        }
        (doc, warnings)
    }

    pub fn to_graph(&self, registry: &Registry) -> Graph {
        let mut graph = Graph::new(&self.name);
        let mut entries: Vec<(NodeId, Pos2, &GNode)> = self.snarl.nodes_pos_ids().collect();
        entries.sort_by_key(|(id, _, _)| *id);
        for (_, pos, n) in entries {
            graph.nodes.insert(
                n.id.clone(),
                NodeInstance {
                    kind: n.kind.clone(),
                    group: n.group.clone().filter(|g| !g.is_empty()),
                    rate: n.rate,
                    params: n.params.clone(),
                    restart: n.restart,
                    pos: Some([pos.x.round(), pos.y.round()]),
                },
            );
        }
        let mut wires: Vec<Wire> = self
            .snarl
            .wires()
            .filter_map(|(o, i)| {
                let from = &self.snarl[o.node];
                let to = &self.snarl[i.node];
                let out = registry.get(&from.kind)?.outputs.get(o.output)?;
                let inp = registry.get(&to.kind)?.inputs.get(i.input)?;
                Some(Wire::new(
                    PortRef::new(&from.id, &out.name),
                    PortRef::new(&to.id, &inp.name),
                ))
            })
            .collect();
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

    pub fn unique_id(&self, base: &str) -> String {
        let taken = |id: &str| self.snarl.nodes().any(|n| n.id == id);
        if !taken(base) {
            return base.to_owned();
        }
        (2..)
            .map(|i| format!("{base}{i}"))
            .find(|id| !taken(id))
            .unwrap()
    }

    pub fn add_node(&mut self, desc: &NodeDescriptor, pos: Pos2) -> NodeId {
        let id = self.unique_id(&desc.short_name().replace('-', "_"));
        self.structure_rev += 1;
        self.dirty = true;
        self.snarl
            .insert_node(pos, GNode::new(id, desc.kind.clone()))
    }

    pub fn touch_structure(&mut self) {
        self.structure_rev += 1;
        self.dirty = true;
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
mod tests {
    use super::*;
    use ruro_graph::{PortDesc, Registry};

    fn registry() -> Registry {
        let p = |n: &str| PortDesc {
            name: n.into(),
            ty: "std/Scalar".into(),
        };
        Registry::new([
            NodeDescriptor {
                kind: "t/a".into(),
                doc: String::new(),
                default_rate: Some(10.0),
                inputs: vec![],
                outputs: vec![p("x"), p("y")],
                params: vec![],
            },
            NodeDescriptor {
                kind: "t/b".into(),
                doc: String::new(),
                default_rate: None,
                inputs: vec![p("i"), p("j")],
                outputs: vec![],
                params: vec![],
            },
        ])
    }

    #[test]
    fn graph_roundtrip_through_canvas() {
        let g = Graph::from_yaml(
            "name: rt\nnodes:\n  a: { type: t/a, group: g1, rate: 5, params: { k: 1 } }\n  b: { type: t/b }\nwires: [a.y -> b.j, a.x -> b.i]\n",
        )
        .unwrap();
        let reg = registry();
        let (doc, warnings) = Doc::from_graph(&g, &reg, None);
        assert!(warnings.is_empty());
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
    fn layout_places_downstream_nodes_to_the_right() {
        let g = Graph::from_yaml(
            "name: l\nnodes:\n  a: { type: t/a }\n  b: { type: t/b }\nwires: [a.x -> b.i]\n",
        )
        .unwrap();
        let l = auto_layout(&g);
        assert!(l["b"].x > l["a"].x);
    }
}
