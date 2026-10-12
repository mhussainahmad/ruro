use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::Path;

use crate::{Graph, GraphError, NodeDescriptor, Registry, composite_descriptor, is_valid_ident};

/// The message of a composite error, without the node prefix the issue already carries.
fn composite_message(e: GraphError) -> String {
    match e {
        GraphError::Composite { message, .. } => message,
        other => other.to_string(),
    }
}

/// A problem found while validating a graph. Every issue blocks deployment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// Node the issue belongs to, if any.
    pub node: Option<String>,
    pub message: String,
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.node {
            Some(n) => write!(f, "{n}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

/// Checks a graph against the node types in `registry`. Subgraph files of
/// composite nodes are resolved relative to the current directory.
pub fn validate(graph: &Graph, registry: &Registry) -> Vec<Issue> {
    validate_in(graph, registry, Path::new("."))
}

/// Checks a graph whose subgraph files are relative to `base`.
pub fn validate_in(graph: &Graph, registry: &Registry, base: &Path) -> Vec<Issue> {
    let mut issues = Vec::new();
    // Composite nodes present the descriptor of their subgraph's interface.
    let mut composites: HashMap<&str, NodeDescriptor> = HashMap::new();
    for (id, inst) in graph.nodes.iter().filter(|(_, n)| n.is_composite()) {
        match composite_descriptor(id, inst, registry, base) {
            Ok(d) => {
                composites.insert(id.as_str(), d);
            }
            Err(e) => issues.push(Issue {
                node: Some(id.clone()),
                message: composite_message(e),
            }),
        }
        if let Ok(sub) = crate::composite::subgraph(id, inst, base) {
            for inner in validate_in(&sub.graph, registry, &sub.base) {
                issues.push(Issue {
                    node: Some(id.clone()),
                    message: match inner.node {
                        Some(n) => format!("{n}: {}", inner.message),
                        None => inner.message,
                    },
                });
            }
        }
    }
    if graph.has_composites()
        && issues.is_empty()
        && let Err(e) = graph.flatten_in(base)
    {
        issues.push(match e {
            GraphError::Composite { node, message } => Issue {
                node: Some(node),
                message,
            },
            other => Issue {
                node: None,
                message: other.to_string(),
            },
        });
    }
    let desc = |id: &str, kind: &str| -> Option<&NodeDescriptor> {
        composites.get(id).or_else(|| registry.get(kind))
    };
    let mut push = |node: Option<&str>, message: String| {
        issues.push(Issue {
            node: node.map(str::to_owned),
            message,
        });
    };

    if !is_valid_ident(&graph.name) {
        push(
            None,
            format!("graph name `{}` must match [A-Za-z0-9_-]+", graph.name),
        );
    }

    for (id, inst) in &graph.nodes {
        let id = id.as_str();
        if !is_valid_ident(id) {
            push(Some(id), "node id must match [A-Za-z0-9_-]+".into());
        }
        if !is_valid_ident(inst.group()) {
            push(
                Some(id),
                format!("group `{}` must match [A-Za-z0-9_-]+", inst.group()),
            );
        }
        let Some(desc) = desc(id, &inst.kind) else {
            if !inst.is_composite() {
                push(Some(id), format!("unknown node type `{}`", inst.kind));
            }
            continue;
        };
        if let Some(rate) = inst.rate
            && !(rate.is_finite() && rate > 0.0 && rate <= 100_000.0)
        {
            push(
                Some(id),
                format!("rate {rate} Hz is out of range (0, 100000]"),
            );
        }
        if !inst.is_composite()
            && desc.inputs.is_empty()
            && inst.rate.or(desc.default_rate).is_none()
        {
            push(Some(id), "source node needs a `rate`".into());
        }
        for (name, value) in &inst.params {
            let Some(p) = desc.param(name) else {
                push(Some(id), format!("unknown parameter `{name}`"));
                continue;
            };
            if !p.kind.accepts(value) {
                push(
                    Some(id),
                    format!("parameter `{name}` expects {:?}, got {value}", p.kind),
                );
                continue;
            }
            if let Some(v) = value.as_f64()
                && (p.min.is_some_and(|min| v < min) || p.max.is_some_and(|max| v > max))
            {
                push(
                    Some(id),
                    format!(
                        "parameter `{name}` = {v} is outside [{:?}, {:?}]",
                        p.min, p.max
                    ),
                );
            }
        }
    }

    let mut seen = HashSet::new();
    for wire in &graph.wires {
        if !seen.insert(wire) {
            push(None, format!("duplicate wire `{wire}`"));
            continue;
        }
        let from_ty = graph
            .nodes
            .get(&wire.from.node)
            .and_then(|n| desc(&wire.from.node, &n.kind))
            .map(|d| d.output(&wire.from.port).map(|p| p.ty.as_str()));
        let to_ty = graph
            .nodes
            .get(&wire.to.node)
            .and_then(|n| desc(&wire.to.node, &n.kind))
            .map(|d| d.input(&wire.to.port).map(|p| p.ty.as_str()));
        if !graph.nodes.contains_key(&wire.from.node) {
            push(
                None,
                format!("wire `{wire}` starts at unknown node `{}`", wire.from.node),
            );
        }
        if !graph.nodes.contains_key(&wire.to.node) {
            push(
                None,
                format!("wire `{wire}` ends at unknown node `{}`", wire.to.node),
            );
        }
        match (from_ty, to_ty) {
            (Some(None), _) => push(
                Some(&wire.from.node),
                format!("no output port `{}`", wire.from.port),
            ),
            (_, Some(None)) => push(
                Some(&wire.to.node),
                format!("no input port `{}`", wire.to.port),
            ),
            (Some(Some(a)), Some(Some(b))) if a != b && !a.is_empty() && !b.is_empty() => {
                push(None, format!("wire `{wire}` connects {a} to {b}"));
            }
            _ => {}
        }
    }

    issues
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NodeDescriptor, ParamDesc, ParamKind, PortDesc};

    fn registry() -> Registry {
        let port = |n: &str, t: &str| PortDesc {
            name: n.into(),
            ty: t.into(),
        };
        Registry::new([
            NodeDescriptor {
                kind: "t/src".into(),
                doc: String::new(),
                default_rate: None,
                inputs: vec![],
                outputs: vec![port("out", "std/Scalar"), port("img", "std/Image")],
                params: vec![ParamDesc {
                    name: "gain".into(),
                    kind: ParamKind::Float,
                    default: serde_json::json!(1.0),
                    min: Some(0.0),
                    max: Some(10.0),
                    doc: String::new(),
                }],
                source: None,
            },
            NodeDescriptor {
                kind: "t/sink".into(),
                doc: String::new(),
                default_rate: None,
                inputs: vec![port("in", "std/Scalar")],
                outputs: vec![],
                params: vec![],
                source: None,
            },
        ])
    }

    fn messages(yaml: &str) -> Vec<String> {
        validate(&Graph::from_yaml(yaml).unwrap(), &registry())
            .into_iter()
            .map(|i| i.to_string())
            .collect()
    }

    #[test]
    fn valid_graph_has_no_issues() {
        let issues = messages(
            "name: ok\nnodes:\n  a: { type: t/src, rate: 10, params: { gain: 2 } }\n  b: { type: t/sink }\nwires: [a.out -> b.in]\n",
        );
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn reports_every_problem() {
        let issues = messages(
            "name: bad\nnodes:\n  a: { type: t/src, params: { gain: 20, nope: 1 } }\n  b: { type: t/sink }\n  c: { type: t/missing }\nwires:\n  - a.img -> b.in\n  - a.zzz -> b.in\n  - a.out -> x.in\n",
        );
        let joined = issues.join("\n");
        for needle in [
            "source node needs a `rate`",
            "outside",
            "unknown parameter `nope`",
            "unknown node type `t/missing`",
            "connects std/Image to std/Scalar",
            "no output port `zzz`",
            "unknown node `x`",
        ] {
            assert!(joined.contains(needle), "missing `{needle}` in:\n{joined}");
        }
    }
}
