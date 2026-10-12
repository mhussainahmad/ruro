//! Comparing two versions of a graph: what was added, removed and changed.

use std::collections::{BTreeSet, HashMap};

use ruro_graph::{Graph, NodeInstance, Wire};

/// How a node differs from the base version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Added,
    Changed,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct GraphDiff {
    pub added: Vec<String>,
    /// Removed nodes, with their last known instance (for drawing ghosts).
    pub removed: Vec<(String, NodeInstance)>,
    /// Changed nodes and a description of each change.
    pub changed: Vec<(String, Vec<String>)>,
    pub wires_added: Vec<Wire>,
    pub wires_removed: Vec<Wire>,
    /// Changes to the graph itself: name, process settings, interface.
    pub graph: Vec<String>,
}

impl GraphDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.removed.is_empty()
            && self.changed.is_empty()
            && self.wires_added.is_empty()
            && self.wires_removed.is_empty()
            && self.graph.is_empty()
    }

    pub fn count(&self) -> usize {
        self.added.len()
            + self.removed.len()
            + self.changed.len()
            + self.wires_added.len()
            + self.wires_removed.len()
            + self.graph.len()
    }
}

fn show(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn opt<T: std::fmt::Debug>(v: &Option<T>, none: &str) -> String {
    match v {
        Some(v) => format!("{v:?}").trim_matches('"').to_owned(),
        None => none.to_owned(),
    }
}

/// Describes how `node` changed between `a` and `b`, ignoring its position.
fn node_changes(a: &NodeInstance, b: &NodeInstance) -> Vec<String> {
    let mut out = Vec::new();
    if a.kind != b.kind {
        out.push(format!("type {} → {}", a.kind, b.kind));
    }
    if a.group() != b.group() {
        out.push(format!("process {} → {}", a.group(), b.group()));
    }
    if a.rate != b.rate {
        out.push(format!(
            "rate {} → {}",
            opt(&a.rate, "on input"),
            opt(&b.rate, "on input")
        ));
    }
    if a.restart != b.restart {
        out.push(format!(
            "restart {} → {}",
            opt(&a.restart, "default"),
            opt(&b.restart, "default")
        ));
    }
    let keys: BTreeSet<&String> = a.params.keys().chain(b.params.keys()).collect();
    for k in keys {
        match (a.params.get(k), b.params.get(k)) {
            (Some(x), Some(y)) if x != y => out.push(format!("{k} {} → {}", show(x), show(y))),
            (Some(x), None) => out.push(format!("{k} {} → default", show(x))),
            (None, Some(y)) => out.push(format!("{k} default → {}", show(y))),
            _ => {}
        }
    }
    if a.graph != b.graph {
        out.push("subgraph edited".into());
    }
    out
}

/// What changed from `base` to `current`.
pub fn diff(base: &Graph, current: &Graph) -> GraphDiff {
    let mut d = GraphDiff::default();
    if base.name != current.name {
        d.graph
            .push(format!("name {} → {}", base.name, current.name));
    }
    if base.groups != current.groups {
        d.graph.push("process settings changed".into());
    }
    if base.inputs != current.inputs || base.outputs != current.outputs {
        d.graph.push("exposed ports changed".into());
    }
    for (id, n) in &current.nodes {
        match base.nodes.get(id) {
            None => d.added.push(id.clone()),
            Some(old) => {
                let c = node_changes(old, n);
                if !c.is_empty() {
                    d.changed.push((id.clone(), c));
                }
            }
        }
    }
    for (id, n) in &base.nodes {
        if !current.nodes.contains_key(id) {
            d.removed.push((id.clone(), n.clone()));
        }
    }
    let wires = |g: &Graph| -> BTreeSet<String> { g.wires.iter().map(|w| w.to_string()).collect() };
    let (a, b) = (wires(base), wires(current));
    d.wires_added = current
        .wires
        .iter()
        .filter(|w| !a.contains(&w.to_string()))
        .cloned()
        .collect();
    d.wires_removed = base
        .wires
        .iter()
        .filter(|w| !b.contains(&w.to_string()))
        .cloned()
        .collect();
    d
}

/// A comparison shown in the studio: one open document against a base graph.
pub struct Compare {
    /// The document being compared.
    pub uid: u64,
    /// What the base is, e.g. "saved version" or a file name.
    pub label: String,
    pub base: Graph,
    pub diff: GraphDiff,
}

impl Compare {
    pub fn new(uid: u64, label: String, base: Graph, current: &Graph) -> Self {
        let diff = diff(&base, current);
        Self {
            uid,
            label,
            base,
            diff,
        }
    }

    pub fn update(&mut self, current: &Graph) {
        self.diff = diff(&self.base, current);
    }

    /// Canvas highlights, by node id.
    pub fn marks(&self) -> Marks {
        Marks {
            nodes: self
                .diff
                .added
                .iter()
                .map(|id| (id.clone(), Mark::Added))
                .chain(
                    self.diff
                        .changed
                        .iter()
                        .map(|(id, _)| (id.clone(), Mark::Changed)),
                )
                .collect(),
            ghosts: self
                .diff
                .removed
                .iter()
                .filter_map(|(id, n)| Some((id.clone(), n.kind.clone(), n.pos?)))
                .collect(),
        }
    }
}

/// What the canvas highlights while comparing.
#[derive(Default)]
pub struct Marks {
    pub nodes: HashMap<String, Mark>,
    /// Removed nodes: id, type and position, drawn as outlines.
    pub ghosts: Vec<(String, String, [f32; 2])>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_added_removed_and_changed() {
        let a = Graph::from_yaml(
            "name: g\nnodes:\n  cam: { type: std/fake-camera, rate: 30, pos: [0, 0] }\n  blur: { type: std/blur, params: { radius: 4 }, pos: [300, 0] }\n  old: { type: std/gain, pos: [0, 200] }\nwires: [cam.image -> blur.image]\n",
        )
        .unwrap();
        let b = Graph::from_yaml(
            "name: g\nnodes:\n  cam: { type: std/fake-camera, rate: 15, pos: [50, 0] }\n  blur: { type: std/blur, params: { radius: 6 }, group: vision }\n  stats: { type: std/image-stats }\nwires: [blur.out -> stats.image]\n",
        )
        .unwrap();
        let d = diff(&a, &b);
        assert_eq!(d.added, ["stats"]);
        assert_eq!(d.removed[0].0, "old");
        let changed: HashMap<_, _> = d.changed.iter().cloned().collect();
        assert_eq!(changed["cam"], ["rate 30.0 → 15.0"]);
        assert_eq!(changed["blur"], ["process main → vision", "radius 4 → 6"]);
        assert_eq!(d.wires_added[0].to_string(), "blur.out -> stats.image");
        assert_eq!(d.wires_removed[0].to_string(), "cam.image -> blur.image");
        assert_eq!(d.count(), 6);
        assert!(diff(&a, &a).is_empty(), "moving nodes is not a change");
    }
}
