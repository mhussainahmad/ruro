//! Composite nodes: subgraphs used as a single node.
//!
//! A composite node either references a graph file, `type: graph:path/to/sub.yaml`
//! (relative to the file that uses it), or carries the subgraph inline:
//!
//! ```yaml
//! nodes:
//!   vision:
//!     type: composite
//!     params: { blur.radius: 6 }    # overrides an inner node's parameter
//!     graph:
//!       name: vision
//!       inputs: { image: blur.image }
//!       outputs: { brightness: stats.brightness }
//!       nodes:
//!         blur: { type: std/blur }
//!         stats: { type: std/image-stats }
//!       wires: [blur.out -> stats.image]
//! wires:
//!   - camera.image -> vision.image
//! ```
//!
//! The subgraph's `inputs` and `outputs` name the ports the composite exposes.
//! Deployment runs the [flattened](Graph::flatten) graph, in which inner nodes
//! are named `<composite>__<inner>`.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use indexmap::IndexMap;

use crate::spec::{Graph, GraphError, NodeInstance, PortList, PortRef, Wire};
use crate::{NodeDescriptor, PortDesc, Registry};

/// Node type of a composite whose subgraph is stored inline in `graph`.
pub const COMPOSITE: &str = "composite";
/// Prefix of a node type that references a subgraph file.
pub const GRAPH_PREFIX: &str = "graph:";
/// Separator between a composite's id and an inner node id after flattening.
pub const SEPARATOR: &str = "__";
/// Deepest nesting of composites accepted.
const MAX_DEPTH: usize = 16;

impl NodeInstance {
    /// True for nodes that stand for a subgraph.
    pub fn is_composite(&self) -> bool {
        is_composite_kind(&self.kind)
    }

    /// Path of the referenced subgraph file, for `type: graph:<path>` nodes.
    pub fn subgraph_path(&self) -> Option<&str> {
        self.kind.strip_prefix(GRAPH_PREFIX)
    }
}

/// True if `kind` names a composite node type.
pub fn is_composite_kind(kind: &str) -> bool {
    kind == COMPOSITE || kind.starts_with(GRAPH_PREFIX)
}

/// The flattened id of `inner` inside the composite `parent`.
pub fn flat_id(parent: &str, inner: &str) -> String {
    format!("{parent}{SEPARATOR}{inner}")
}

fn err(node: &str, message: impl Into<String>) -> GraphError {
    GraphError::Composite {
        node: node.to_owned(),
        message: message.into(),
    }
}

/// A resolved subgraph: its graph, the directory its own references are
/// relative to, and the file it came from (for cycle detection).
pub struct Subgraph<'a> {
    pub graph: Cow<'a, Graph>,
    pub base: PathBuf,
    pub file: Option<PathBuf>,
}

/// Loads the subgraph behind composite node `id`.
pub fn subgraph<'a>(
    id: &str,
    inst: &'a NodeInstance,
    base: &Path,
) -> Result<Subgraph<'a>, GraphError> {
    if let Some(rel) = inst.subgraph_path() {
        let path = base.join(rel.trim());
        let graph = Graph::load(&path)
            .map_err(|e| err(id, format!("cannot load `{}`: {e}", path.display())))?;
        let file = path.canonicalize().unwrap_or_else(|_| path.clone());
        let base = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| base.to_path_buf());
        Ok(Subgraph {
            graph: Cow::Owned(graph),
            base,
            file: Some(file),
        })
    } else if inst.kind == COMPOSITE {
        let graph = inst
            .graph
            .as_deref()
            .ok_or_else(|| err(id, "a `composite` node needs an inline `graph`"))?;
        Ok(Subgraph {
            graph: Cow::Borrowed(graph),
            base: base.to_path_buf(),
            file: None,
        })
    } else {
        Err(err(
            id,
            format!("`{}` is not a composite node type", inst.kind),
        ))
    }
}

impl Graph {
    /// Expands composite nodes into plain nodes and wires, resolving subgraph
    /// files relative to the current directory. Graphs without composites are
    /// returned unchanged. Deployment always runs the flattened graph.
    pub fn flatten(&self) -> Result<Graph, GraphError> {
        self.flatten_in(Path::new("."))
    }

    /// Like [`Graph::flatten`], resolving subgraph files relative to `base`
    /// (normally the directory of the file this graph was loaded from).
    pub fn flatten_in(&self, base: &Path) -> Result<Graph, GraphError> {
        flatten(self, base, &mut Vec::new(), 0)
    }

    /// True if any node is a composite.
    pub fn has_composites(&self) -> bool {
        self.nodes.values().any(NodeInstance::is_composite)
    }

    /// The flattened port that output `node.port` of this graph refers to.
    pub fn resolve_output(
        &self,
        base: &Path,
        node: &str,
        port: &str,
    ) -> Result<PortRef, GraphError> {
        resolve_output(self, base, node, port, 0)
    }

    /// Turns the nodes `ids` into one inline composite node `new_id`. Wires that
    /// cross the boundary become ports of the composite.
    pub fn extract_composite(&mut self, ids: &[String], new_id: &str) -> Result<(), GraphError> {
        let set: HashSet<&str> = ids.iter().map(String::as_str).collect();
        if set.is_empty() {
            return Err(err(new_id, "nothing to group"));
        }
        if let Some(missing) = ids.iter().find(|id| !self.nodes.contains_key(*id)) {
            return Err(err(new_id, format!("no node `{missing}`")));
        }
        if self.nodes.contains_key(new_id) && !set.contains(new_id) {
            return Err(err(new_id, "a node with this id already exists"));
        }
        let origin = ids.iter().filter_map(|id| self.nodes[id].pos).fold(
            None,
            |acc: Option<[f32; 2]>, [x, y]| match acc {
                None => Some([x, y]),
                Some([ax, ay]) => Some([ax.min(x), ay.min(y)]),
            },
        );

        let mut sub = Graph::new(new_id);
        for id in ids {
            let mut inst = self.nodes[id].clone();
            if let (Some([x, y]), Some([ox, oy])) = (inst.pos, origin) {
                inst.pos = Some([x - ox, y - oy]);
            }
            sub.nodes.insert(id.clone(), inst);
        }
        for g in self.nodes.values().filter_map(|n| n.group.as_ref()) {
            if let Some(cfg) = self.groups.get(g) {
                sub.groups.insert(g.clone(), cfg.clone());
            }
        }

        // Exposed port names: the inner port name if free, else `node_port`.
        let expose = |names: &mut Vec<String>, p: &PortRef| {
            let name = if names.contains(&p.port) {
                format!("{}_{}", p.node, p.port)
            } else {
                p.port.clone()
            };
            names.push(name.clone());
            name
        };
        let mut in_names = Vec::new();
        let mut out_names = Vec::new();
        let mut exposed_in: HashMap<PortRef, String> = HashMap::new();
        let mut exposed_out: HashMap<PortRef, String> = HashMap::new();
        let mut outer = Vec::new();
        for w in std::mem::take(&mut self.wires) {
            let from_in = set.contains(w.from.node.as_str());
            let to_in = set.contains(w.to.node.as_str());
            match (from_in, to_in) {
                (true, true) => sub.wires.push(w),
                (false, false) => outer.push(w),
                (false, true) => {
                    let name = exposed_in
                        .entry(w.to.clone())
                        .or_insert_with(|| expose(&mut in_names, &w.to))
                        .clone();
                    sub.inputs
                        .entry(name.clone())
                        .or_insert_with(|| PortList(vec![w.to.clone()]));
                    outer.push(Wire {
                        to: PortRef::new(new_id, name),
                        ..w
                    });
                }
                (true, false) => {
                    let name = exposed_out
                        .entry(w.from.clone())
                        .or_insert_with(|| expose(&mut out_names, &w.from))
                        .clone();
                    sub.outputs.insert(name.clone(), w.from.clone());
                    outer.push(Wire {
                        from: PortRef::new(new_id, name),
                        ..w
                    });
                }
            }
        }
        // Keep a stable outer wire order: unchanged wires first, as before.
        let mut seen = HashSet::new();
        outer.retain(|w| seen.insert(w.clone()));
        self.wires = outer;
        for id in ids {
            self.nodes.shift_remove(id);
        }
        self.nodes.insert(
            new_id.to_owned(),
            NodeInstance {
                kind: COMPOSITE.into(),
                pos: origin,
                graph: Some(Box::new(sub)),
                ..Default::default()
            },
        );
        Ok(())
    }

    /// Replaces composite node `id` by its inner nodes (one level). Inner ids
    /// are kept where free. Returns the ids of the inserted nodes.
    pub fn inline_composite(&mut self, id: &str, base: &Path) -> Result<Vec<String>, GraphError> {
        let inst = self
            .nodes
            .get(id)
            .cloned()
            .ok_or_else(|| err(id, "no such node"))?;
        let sub = subgraph(id, &inst, base)?;
        let sub = sub.graph.into_owned();
        let overrides = param_overrides(id, &inst, &sub)?;

        self.nodes.shift_remove(id);
        let mut rename: HashMap<String, String> = HashMap::new();
        let mut added = Vec::new();
        for (cid, cinst) in &sub.nodes {
            let new = self.unique_id(cid);
            let mut c = cinst.clone();
            if c.group.is_none() {
                c.group = inst.group.clone();
            }
            if c.restart.is_none() {
                c.restart = inst.restart;
            }
            if let (Some([x, y]), Some([ox, oy])) = (c.pos, inst.pos) {
                c.pos = Some([x + ox, y + oy]);
            }
            for (p, v) in overrides.get(cid.as_str()).into_iter().flatten() {
                c.params.insert((*p).to_owned(), (*v).clone());
            }
            self.nodes.insert(new.clone(), c);
            rename.insert(cid.clone(), new.clone());
            added.push(new);
        }
        for (g, cfg) in &sub.groups {
            self.groups.entry(g.clone()).or_insert_with(|| cfg.clone());
        }
        let inner = |p: &PortRef| PortRef::new(rename[&p.node].clone(), p.port.clone());
        let mut wires = Vec::new();
        for w in sub.wires.iter() {
            wires.push(Wire {
                from: inner(&w.from),
                to: inner(&w.to),
                depth: w.depth,
            });
        }
        for w in std::mem::take(&mut self.wires) {
            let froms = if w.from.node == id {
                match sub.outputs.get(&w.from.port) {
                    Some(p) => vec![inner(p)],
                    None => continue,
                }
            } else {
                vec![w.from.clone()]
            };
            let tos = if w.to.node == id {
                match sub.inputs.get(&w.to.port) {
                    Some(list) => list.0.iter().map(inner).collect(),
                    None => continue,
                }
            } else {
                vec![w.to.clone()]
            };
            for from in &froms {
                for to in &tos {
                    wires.push(Wire {
                        from: from.clone(),
                        to: to.clone(),
                        depth: w.depth,
                    });
                }
            }
        }
        self.wires = wires;
        Ok(added)
    }
}

/// Parameter overrides by inner node: `(param, value)` pairs.
type Overrides<'a> = HashMap<&'a str, Vec<(&'a str, &'a serde_json::Value)>>;

/// Parameter overrides `inner.param: value` on a composite, by inner node.
fn param_overrides<'a>(
    id: &str,
    inst: &'a NodeInstance,
    sub: &Graph,
) -> Result<Overrides<'a>, GraphError> {
    let mut out: Overrides<'a> = HashMap::new();
    for (key, value) in &inst.params {
        let (node, param) = key.split_once('.').ok_or_else(|| {
            err(
                id,
                format!("parameter `{key}` must name an inner node: `node.param`"),
            )
        })?;
        if !sub.nodes.contains_key(node) {
            return Err(err(
                id,
                format!("parameter `{key}`: no inner node `{node}`"),
            ));
        }
        out.entry(node).or_default().push((param, value));
    }
    Ok(out)
}

fn flatten(
    g: &Graph,
    base: &Path,
    files: &mut Vec<PathBuf>,
    depth: usize,
) -> Result<Graph, GraphError> {
    if !g.has_composites() {
        return Ok(g.clone());
    }
    if depth >= MAX_DEPTH {
        return Err(err(
            &g.name,
            format!("composites are nested more than {MAX_DEPTH} levels deep"),
        ));
    }
    let mut out = Graph {
        name: g.name.clone(),
        groups: g.groups.clone(),
        ..Default::default()
    };
    let mut ins: HashMap<&str, IndexMap<String, PortList>> = HashMap::new();
    let mut outs: HashMap<&str, IndexMap<String, PortRef>> = HashMap::new();

    for (id, inst) in &g.nodes {
        if !inst.is_composite() {
            if out.nodes.insert(id.clone(), inst.clone()).is_some() {
                return Err(err(id, "flattened node id collides with another node"));
            }
            continue;
        }
        let sub = subgraph(id, inst, base)?;
        if let Some(file) = &sub.file {
            if files.contains(file) {
                let chain: Vec<String> = files
                    .iter()
                    .chain([file])
                    .map(|f| f.display().to_string())
                    .collect();
                return Err(err(
                    id,
                    format!("subgraph includes itself: {}", chain.join(" -> ")),
                ));
            }
            files.push(file.clone());
        }
        let flat = flatten(&sub.graph, &sub.base, files, depth + 1).map_err(|e| nest(id, e));
        if sub.file.is_some() {
            files.pop();
        }
        let flat = flat?;
        for (name, ports) in &flat.inputs {
            for p in &ports.0 {
                if !flat.nodes.contains_key(&p.node) {
                    return Err(err(
                        id,
                        format!("input `{name}` maps to unknown node `{}`", p.node),
                    ));
                }
            }
        }
        for (name, p) in &flat.outputs {
            if !flat.nodes.contains_key(&p.node) {
                return Err(err(
                    id,
                    format!("output `{name}` maps to unknown node `{}`", p.node),
                ));
            }
        }
        let overrides = param_overrides(id, inst, &sub.graph)?;
        let prefix = |p: &PortRef| PortRef::new(flat_id(id, &p.node), p.port.clone());
        for (cid, cinst) in &flat.nodes {
            let mut c = cinst.clone();
            if c.group.is_none() {
                c.group = inst.group.clone();
            }
            if c.restart.is_none() {
                c.restart = inst.restart;
            }
            for (p, v) in overrides.get(cid.as_str()).into_iter().flatten() {
                c.params.insert((*p).to_owned(), (*v).clone());
            }
            let nid = flat_id(id, cid);
            if g.nodes.contains_key(&nid) || out.nodes.insert(nid.clone(), c).is_some() {
                return Err(err(
                    id,
                    format!("flattened id `{nid}` collides with another node"),
                ));
            }
        }
        for w in &flat.wires {
            out.wires.push(Wire {
                from: prefix(&w.from),
                to: prefix(&w.to),
                depth: w.depth,
            });
        }
        for (name, cfg) in &flat.groups {
            out.groups
                .entry(name.clone())
                .or_insert_with(|| cfg.clone());
        }
        ins.insert(
            id,
            flat.inputs
                .iter()
                .map(|(k, v)| (k.clone(), PortList(v.0.iter().map(prefix).collect())))
                .collect(),
        );
        outs.insert(
            id,
            flat.outputs
                .iter()
                .map(|(k, v)| (k.clone(), prefix(v)))
                .collect(),
        );
    }

    let source = |p: &PortRef| -> Result<PortRef, GraphError> {
        match outs.get(p.node.as_str()) {
            Some(map) => map
                .get(&p.port)
                .cloned()
                .ok_or_else(|| err(&p.node, format!("has no output `{}`", p.port))),
            None => Ok(p.clone()),
        }
    };
    let sinks = |p: &PortRef| -> Result<Vec<PortRef>, GraphError> {
        match ins.get(p.node.as_str()) {
            Some(map) => map
                .get(&p.port)
                .map(|l| l.0.clone())
                .ok_or_else(|| err(&p.node, format!("has no input `{}`", p.port))),
            None => Ok(vec![p.clone()]),
        }
    };
    for w in &g.wires {
        let from = source(&w.from)?;
        for to in sinks(&w.to)? {
            out.wires.push(Wire {
                from: from.clone(),
                to,
                depth: w.depth,
            });
        }
    }
    for (name, list) in &g.inputs {
        let mut ports = Vec::new();
        for p in &list.0 {
            ports.extend(sinks(p)?);
        }
        out.inputs.insert(name.clone(), PortList(ports));
    }
    for (name, p) in &g.outputs {
        out.outputs.insert(name.clone(), source(p)?);
    }
    Ok(out)
}

/// Attributes an error inside a subgraph to the composite that contains it.
fn nest(id: &str, e: GraphError) -> GraphError {
    match e {
        GraphError::Composite { node, message } => err(id, format!("{node}: {message}")),
        other => err(id, other.to_string()),
    }
}

fn resolve_output(
    g: &Graph,
    base: &Path,
    node: &str,
    port: &str,
    depth: usize,
) -> Result<PortRef, GraphError> {
    let inst = g.nodes.get(node).ok_or_else(|| err(node, "no such node"))?;
    if !inst.is_composite() {
        return Ok(PortRef::new(node, port));
    }
    if depth >= MAX_DEPTH {
        return Err(err(node, "composites are nested too deeply"));
    }
    let sub = subgraph(node, inst, base)?;
    let inner = sub
        .graph
        .outputs
        .get(port)
        .ok_or_else(|| err(node, format!("has no output `{port}`")))?;
    let p = resolve_output(&sub.graph, &sub.base, &inner.node, &inner.port, depth + 1)?;
    Ok(PortRef::new(flat_id(node, &p.node), p.port))
}

/// The descriptor a composite node presents: its exposed ports (typed by the
/// inner ports they map to) and the inner nodes' parameters as `node.param`.
pub fn composite_descriptor(
    id: &str,
    inst: &NodeInstance,
    registry: &Registry,
    base: &Path,
) -> Result<NodeDescriptor, GraphError> {
    descriptor(id, inst, registry, base, &mut Vec::new())
}

fn descriptor(
    id: &str,
    inst: &NodeInstance,
    registry: &Registry,
    base: &Path,
    files: &mut Vec<PathBuf>,
) -> Result<NodeDescriptor, GraphError> {
    if files.len() >= MAX_DEPTH {
        return Err(err(id, "composites are nested too deeply"));
    }
    let sub = subgraph(id, inst, base)?;
    if let Some(f) = &sub.file {
        if files.contains(f) {
            return Err(err(id, "subgraph includes itself"));
        }
        files.push(f.clone());
    } else {
        // Inline graphs cannot form cycles, but bound the depth all the same.
        files.push(PathBuf::from(format!("<inline {}>", files.len())));
    }
    let result = (|| {
        let g = &sub.graph;
        let mut inner: HashMap<&str, NodeDescriptor> = HashMap::new();
        for (cid, c) in &g.nodes {
            let d = if c.is_composite() {
                descriptor(cid, c, registry, &sub.base, files).map_err(|e| nest(id, e))?
            } else {
                match registry.get(&c.kind) {
                    Some(d) => d.clone(),
                    None => continue,
                }
            };
            inner.insert(cid.as_str(), d);
        }
        let port_ty = |p: &PortRef, input: bool| -> Result<String, GraphError> {
            if !g.nodes.contains_key(&p.node) {
                return Err(err(
                    id,
                    format!("`{p}` refers to unknown inner node `{}`", p.node),
                ));
            }
            let Some(d) = inner.get(p.node.as_str()) else {
                // Unknown node type: reported by validation of the subgraph.
                return Ok(String::new());
            };
            let port = if input {
                d.input(&p.port)
            } else {
                d.output(&p.port)
            };
            port.map(|p| p.ty.clone()).ok_or_else(|| {
                err(
                    id,
                    format!(
                        "`{p}` is not an {} of `{}`",
                        if input { "input" } else { "output" },
                        p.node
                    ),
                )
            })
        };
        let mut inputs = Vec::new();
        for (name, list) in &g.inputs {
            let first = list
                .0
                .first()
                .ok_or_else(|| err(id, format!("input `{name}` maps to no port")))?;
            let ty = port_ty(first, true)?;
            for other in &list.0[1..] {
                let t = port_ty(other, true)?;
                if t != ty && !t.is_empty() && !ty.is_empty() {
                    return Err(err(
                        id,
                        format!("input `{name}` feeds ports of different types ({ty}, {t})"),
                    ));
                }
            }
            inputs.push(PortDesc {
                name: name.clone(),
                ty,
            });
        }
        let mut outputs = Vec::new();
        for (name, p) in &g.outputs {
            outputs.push(PortDesc {
                name: name.clone(),
                ty: port_ty(p, false)?,
            });
        }
        let mut params = Vec::new();
        for (cid, d) in &inner {
            for p in &d.params {
                let mut p = p.clone();
                p.name = format!("{cid}.{}", p.name);
                if let Some(v) = g.nodes[*cid].params.get(&p.name[cid.len() + 1..]) {
                    p.default = v.clone();
                }
                params.push(p);
            }
        }
        params.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(NodeDescriptor {
            kind: inst.kind.clone(),
            doc: format!(
                "Subgraph `{}` with {} node{}",
                g.name,
                g.nodes.len(),
                if g.nodes.len() == 1 { "" } else { "s" }
            ),
            default_rate: None,
            inputs,
            outputs,
            params,
            source: inst
                .subgraph_path()
                .map(|p| base.join(p).display().to_string()),
        })
    })();
    files.pop();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ParamDesc, ParamKind, validate_in};

    fn registry() -> Registry {
        let p = |n: &str, t: &str| PortDesc {
            name: n.into(),
            ty: t.into(),
        };
        Registry::new([
            NodeDescriptor {
                kind: "t/cam".into(),
                doc: String::new(),
                default_rate: Some(30.0),
                inputs: vec![],
                outputs: vec![p("image", "std/Image")],
                params: vec![],
                source: None,
            },
            NodeDescriptor {
                kind: "t/blur".into(),
                doc: String::new(),
                default_rate: None,
                inputs: vec![p("image", "std/Image")],
                outputs: vec![p("out", "std/Image")],
                params: vec![ParamDesc {
                    name: "radius".into(),
                    kind: ParamKind::Int,
                    default: serde_json::json!(2),
                    min: Some(0.0),
                    max: Some(32.0),
                    doc: String::new(),
                }],
                source: None,
            },
            NodeDescriptor {
                kind: "t/stats".into(),
                doc: String::new(),
                default_rate: None,
                inputs: vec![p("image", "std/Image")],
                outputs: vec![p("brightness", "std/Scalar")],
                params: vec![],
                source: None,
            },
        ])
    }

    const INLINE: &str = r#"
name: top
nodes:
  cam: { type: t/cam, group: camera }
  vision:
    type: composite
    group: vision
    params: { blur.radius: 6 }
    graph:
      name: vision
      inputs: { image: blur.image }
      outputs: { brightness: stats.brightness }
      nodes:
        blur: { type: t/blur }
        stats: { type: t/stats, group: stats }
      wires: [blur.out -> stats.image]
  sink: { type: t/stats }
wires:
  - cam.image -> vision.image
  - cam.image -> sink.image
"#;

    #[test]
    fn graphs_without_composites_are_unchanged() {
        let g = Graph::from_yaml("name: a\nnodes:\n  x: { type: t/cam }\n").unwrap();
        assert_eq!(g.flatten().unwrap(), g);
    }

    #[test]
    fn inline_composite_is_expanded_with_prefixed_ids() {
        let g = Graph::from_yaml(INLINE).unwrap();
        let f = g.flatten().unwrap();
        assert_eq!(
            f.nodes.keys().collect::<Vec<_>>(),
            ["cam", "vision__blur", "vision__stats", "sink"]
        );
        assert!(f.nodes.keys().all(|id| crate::is_valid_ident(id)));
        // Groups are inherited unless the inner node sets its own.
        assert_eq!(f.nodes["vision__blur"].group.as_deref(), Some("vision"));
        assert_eq!(f.nodes["vision__stats"].group.as_deref(), Some("stats"));
        // Parameter overrides reach the inner node.
        assert_eq!(
            f.nodes["vision__blur"].params["radius"],
            serde_json::json!(6)
        );
        let wires: Vec<String> = f.wires.iter().map(|w| w.to_string()).collect();
        assert_eq!(
            wires,
            [
                "vision__blur.out -> vision__stats.image",
                "cam.image -> vision__blur.image",
                "cam.image -> sink.image",
            ]
        );
        assert!(!f.has_composites());
        assert!(validate_in(&f, &registry(), Path::new(".")).is_empty());
    }

    #[test]
    fn composite_outputs_and_nested_composites_resolve() {
        let yaml = r#"
name: n
nodes:
  cam: { type: t/cam }
  outer:
    type: composite
    graph:
      name: outer
      inputs: { image: inner.image }
      outputs: { level: inner.level }
      nodes:
        inner:
          type: composite
          graph:
            name: inner
            inputs: { image: [blur.image, stats.image] }
            outputs: { level: stats.brightness }
            nodes:
              blur: { type: t/blur }
              stats: { type: t/stats }
  sink: { type: t/stats }
wires: [cam.image -> outer.image]
"#;
        let g = Graph::from_yaml(yaml).unwrap();
        let f = g.flatten().unwrap();
        assert!(f.nodes.contains_key("outer__inner__stats"));
        let wires: Vec<String> = f.wires.iter().map(|w| w.to_string()).collect();
        assert_eq!(
            wires,
            [
                "cam.image -> outer__inner__blur.image",
                "cam.image -> outer__inner__stats.image",
            ]
        );
        assert_eq!(
            g.resolve_output(Path::new("."), "outer", "level").unwrap(),
            PortRef::new("outer__inner__stats", "brightness")
        );
        let d =
            composite_descriptor("outer", &g.nodes["outer"], &registry(), Path::new(".")).unwrap();
        assert_eq!(d.inputs[0].ty, "std/Image");
        assert_eq!(d.outputs[0].ty, "std/Scalar");
        assert_eq!(d.params[0].name, "inner.blur.radius");
    }

    fn tempdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ruro-graph-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn file_subgraphs_resolve_relative_to_the_parent() {
        let dir = tempdir("files");
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(
            dir.join("lib/vision.yaml"),
            "name: vision\ninputs: { image: blur.image }\noutputs: { out: blur.out }\nnodes:\n  blur: { type: t/blur }\n",
        )
        .unwrap();
        let g = Graph::from_yaml(
            "name: top\nnodes:\n  cam: { type: t/cam }\n  v: { type: 'graph:lib/vision.yaml' }\nwires: [cam.image -> v.image]\n",
        )
        .unwrap();
        let f = g.flatten_in(&dir).unwrap();
        assert_eq!(f.wires[0].to_string(), "cam.image -> v__blur.image");
        assert!(g.flatten_in(Path::new("/nonexistent")).is_err());
        assert!(validate_in(&g, &registry(), &dir).is_empty());
    }

    #[test]
    fn cycles_are_detected() {
        let dir = tempdir("cycle");
        std::fs::write(
            dir.join("a.yaml"),
            "name: a\nnodes:\n  b: { type: 'graph:b.yaml' }\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("b.yaml"),
            "name: b\nnodes:\n  a: { type: 'graph:a.yaml' }\n",
        )
        .unwrap();
        let g = Graph::load(dir.join("a.yaml")).unwrap();
        let e = g.flatten_in(&dir).unwrap_err().to_string();
        assert!(e.contains("includes itself"), "{e}");
    }

    #[test]
    fn bad_mappings_are_errors() {
        let bad_output = INLINE.replace("stats.brightness", "nope.brightness");
        let e = Graph::from_yaml(&bad_output)
            .unwrap()
            .flatten()
            .unwrap_err();
        assert!(e.to_string().contains("unknown node `nope`"), "{e}");

        let bad_port = INLINE.replace("cam.image -> vision.image", "cam.image -> vision.depth");
        let e = Graph::from_yaml(&bad_port).unwrap().flatten().unwrap_err();
        assert!(e.to_string().contains("has no input `depth`"), "{e}");

        let bad_param = INLINE.replace("blur.radius: 6", "radius: 6");
        let e = Graph::from_yaml(&bad_param).unwrap().flatten().unwrap_err();
        assert!(e.to_string().contains("node.param"), "{e}");

        let missing = "name: m\nnodes:\n  c: { type: composite }\n";
        assert!(Graph::from_yaml(missing).unwrap().flatten().is_err());
    }

    #[test]
    fn validation_reports_composite_problems() {
        let reg = registry();
        let ok = Graph::from_yaml(INLINE).unwrap();
        assert!(
            validate_in(&ok, &reg, Path::new(".")).is_empty(),
            "{:?}",
            validate_in(&ok, &reg, Path::new("."))
        );

        // A type mismatch on a wire into the composite.
        let mismatch = INLINE.replace("cam.image -> sink.image", "vision.brightness -> sink.image");
        let issues = validate_in(&Graph::from_yaml(&mismatch).unwrap(), &reg, Path::new("."));
        assert!(
            issues
                .iter()
                .any(|i| i.message.contains("connects std/Scalar to std/Image")),
            "{issues:?}"
        );

        // Problems inside the subgraph are reported on the composite node.
        let inner = INLINE.replace(
            "blur: { type: t/blur }",
            "blur: { type: t/blur, params: { radius: 99 } }",
        );
        let issues = validate_in(&Graph::from_yaml(&inner).unwrap(), &reg, Path::new("."));
        assert!(
            issues
                .iter()
                .any(|i| i.node.as_deref() == Some("vision") && i.message.contains("outside")),
            "{issues:?}"
        );
        // Overrides are range-checked too.
        let over = INLINE.replace("blur.radius: 6", "blur.radius: 99");
        let issues = validate_in(&Graph::from_yaml(&over).unwrap(), &reg, Path::new("."));
        assert!(
            issues.iter().any(|i| i.message.contains("outside")),
            "{issues:?}"
        );
    }

    #[test]
    fn extract_and_inline_roundtrip() {
        let mut g = Graph::from_yaml(
            "name: r\nnodes:\n  cam: { type: t/cam, pos: [0, 0] }\n  blur: { type: t/blur, pos: [300, 0] }\n  stats: { type: t/stats, pos: [600, 0] }\n  sink: { type: t/stats, pos: [900, 0] }\nwires:\n  - cam.image -> blur.image\n  - blur.out -> stats.image\n  - blur.out -> sink.image\n",
        )
        .unwrap();
        let before = g.flatten().unwrap();
        g.extract_composite(&["blur".into(), "stats".into()], "vision")
            .unwrap();
        assert_eq!(
            g.nodes.keys().collect::<Vec<_>>(),
            ["cam", "sink", "vision"]
        );
        let sub = g.nodes["vision"].graph.as_deref().unwrap();
        assert_eq!(sub.inputs["image"].0, [PortRef::new("blur", "image")]);
        assert_eq!(sub.outputs["out"], PortRef::new("blur", "out"));
        assert_eq!(g.nodes["vision"].pos, Some([300.0, 0.0]));
        assert!(validate_in(&g, &registry(), Path::new(".")).is_empty());

        // The flattened graph wires the same ports as before grouping.
        let flat = g.flatten().unwrap();
        let strip = |w: &Wire| w.to_string().replace("vision__", "");
        let mut a: Vec<String> = before.wires.iter().map(strip).collect();
        let mut b: Vec<String> = flat.wires.iter().map(strip).collect();
        a.sort();
        b.sort();
        assert_eq!(a, b);

        let added = g.inline_composite("vision", Path::new(".")).unwrap();
        assert_eq!(added, ["blur", "stats"]);
        assert_eq!(g.nodes["stats"].pos, Some([600.0, 0.0]));
        let mut c: Vec<String> = g.wires.iter().map(|w| w.to_string()).collect();
        c.sort();
        let mut a: Vec<String> = before.wires.iter().map(|w| w.to_string()).collect();
        a.sort();
        assert_eq!(a, c);
    }

    #[test]
    fn interface_yaml_roundtrip() {
        let g = Graph::from_yaml(INLINE).unwrap();
        let again = Graph::from_yaml(&g.to_yaml().unwrap()).unwrap();
        assert_eq!(g, again);
    }
}
