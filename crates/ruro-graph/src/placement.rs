//! Checks that depend on where groups run: whether a group's host links the
//! node types placed in it, whether the hosts at both ends of a wire agree
//! on its message definition, and the runtime's own conventions.
//!
//! Each check takes `offered`, which gives the node types (and message
//! schema hashes) of the host that runs a group, or `None` when they are
//! not known (for example a custom host on another machine).

use crate::{ClockSource, Graph, Issue, Registry};

/// Nodes placed in a group whose host does not link their type, which would
/// fail to start there. Types unknown to `registry` (every host's types)
/// are left to [`crate::validate`].
pub fn placement_issues<'a>(
    graph: &Graph,
    registry: &Registry,
    offered: impl Fn(&str) -> Option<&'a Registry>,
) -> Vec<Issue> {
    let mut issues = Vec::new();
    for (id, node) in &graph.nodes {
        if registry.get(&node.kind).is_none() {
            continue;
        }
        let group = node.group();
        if offered(group).is_some_and(|r| r.get(&node.kind).is_none()) {
            issues.push(Issue {
                node: Some(id.clone()),
                message: format!(
                    "type `{}` is not linked into the host of group `{group}`",
                    node.kind
                ),
            });
        }
    }
    issues
}

/// Wires between groups whose hosts were built with different definitions
/// of the wire's message type: the consumer could not decode what the
/// producer sends. Only hosts whose schema hashes are known are compared.
pub fn schema_issues<'a>(
    graph: &Graph,
    offered: impl Fn(&str) -> Option<&'a Registry>,
) -> Vec<Issue> {
    let mut issues = Vec::new();
    for wire in &graph.wires {
        let (Some(from), Some(to)) = (
            graph.nodes.get(&wire.from.node),
            graph.nodes.get(&wire.to.node),
        ) else {
            continue;
        };
        let (from_group, to_group) = (from.group(), to.group());
        let (Some(producer), Some(consumer)) = (offered(from_group), offered(to_group)) else {
            continue;
        };
        let Some(ty) = producer
            .get(&from.kind)
            .and_then(|d| d.output(&wire.from.port))
            .map(|p| p.ty.as_str())
        else {
            continue;
        };
        if let (Some(ours), Some(theirs)) = (producer.schema_hash(ty), consumer.schema_hash(ty))
            && ours != theirs
        {
            issues.push(Issue {
                node: None,
                message: format!(
                    "wire `{wire}`: the hosts of groups `{from_group}` and `{to_group}` were built \
                     with different definitions of `{ty}` (schema {} vs {}); rebuild them against \
                     the same message definition",
                    short(ours),
                    short(theirs)
                ),
            });
        }
    }
    issues
}

fn short(hash: &str) -> &str {
    hash.get(..12).unwrap_or(hash)
}

/// Checks that need the runtime's conventions rather than node descriptors:
/// a simulated clock needs exactly one `std/Clock` output, and real-time
/// settings must name nodes of their group and use valid values.
pub fn runtime_issues(graph: &Graph, registry: &Registry) -> Vec<Issue> {
    let mut issues = Vec::new();
    let mut push = |message: String| issues.push(Issue::graph(message));
    if graph.clock == ClockSource::Sim {
        let clocks = graph
            .nodes
            .values()
            .filter_map(|n| registry.get(&n.kind))
            .flat_map(|d| d.outputs.iter())
            .filter(|p| p.ty == "std/Clock")
            .count();
        if clocks != 1 {
            push(format!(
                "`clock: sim` needs exactly one std/Clock output, found {clocks}"
            ));
        }
    }
    let groups = graph.groups();
    for (name, config) in &graph.groups {
        let Some(rt) = &config.realtime else { continue };
        for node in rt.deadlines_us.keys() {
            if !groups.get(name).is_some_and(|ns| ns.contains(node)) {
                push(format!(
                    "group `{name}`: deadline for `{node}`, which is not in the group"
                ));
            }
        }
        if let Some(p) = rt.priority
            && !(1..=99).contains(&p)
        {
            push(format!("group `{name}`: priority {p} is outside 1-99"));
        }
        if let Some(r) = rt.rate
            && !(r.is_finite() && r > 0.0 && r <= 100_000.0)
        {
            push(format!(
                "group `{name}`: rate {r} Hz is out of range (0, 100000]"
            ));
        }
    }
    issues
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NodeDescriptor, PortDesc};

    fn desc(kind: &str) -> NodeDescriptor {
        NodeDescriptor {
            kind: kind.into(),
            ..Default::default()
        }
    }

    #[test]
    fn nodes_must_run_in_a_group_whose_host_links_their_type() {
        let std_host = Registry::new([desc("std/sine")]);
        let custom_host = Registry::new([desc("std/sine"), desc("my/scale")]);
        let all = Registry::new([desc("std/sine"), desc("my/scale")]);
        let g = Graph::from_yaml(
            "name: x\ngroups:\n  custom: { host: my-host }\n  far: { machine: robot, host: other }\nnodes:\n  a: { type: std/sine, group: custom }\n  b: { type: my/scale }\n  c: { type: my/scale, group: custom }\n  d: { type: my/scale, group: far }\n",
        )
        .unwrap();
        let offered = |group: &str| match group {
            "custom" => Some(&custom_host),
            "far" => None,
            _ => Some(&std_host),
        };
        let issues = placement_issues(&g, &all, offered);
        assert_eq!(issues.len(), 1, "{issues:?}");
        let text = issues[0].to_string();
        assert!(text.starts_with("b:") && text.contains("`main`"), "{text}");
    }

    #[test]
    fn hosts_must_agree_on_the_messages_they_exchange() {
        let host = |hash: &str| {
            let mut r = Registry::new([NodeDescriptor {
                kind: "t/node".into(),
                default_rate: Some(1.0),
                inputs: vec![PortDesc {
                    name: "in".into(),
                    ty: "my/Pose".into(),
                }],
                outputs: vec![PortDesc {
                    name: "out".into(),
                    ty: "my/Pose".into(),
                }],
                ..Default::default()
            }]);
            r.schema_hashes.insert("my/Pose".into(), hash.into());
            r
        };
        let (old, new) = (host("aaaaaaaaaaaaaaaa"), host("bbbbbbbbbbbbbbbb"));
        let g = Graph::from_yaml(
            "name: x\ngroups:\n  custom: { host: my-host }\nnodes:\n  a: { type: t/node }\n  b: { type: t/node, group: custom }\n  c: { type: t/node, group: unknown }\nwires: [a.out -> b.in, a.out -> c.in, b.out -> a.in]\n",
        )
        .unwrap();
        let offered = |group: &str| match group {
            "main" => Some(&old),
            "custom" => Some(&new),
            _ => None,
        };
        let issues: Vec<String> = schema_issues(&g, offered)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(issues.len(), 2, "{issues:?}");
        assert!(issues[0].contains("a.out -> b.in"), "{issues:?}");
        assert!(issues[0].contains("`my/Pose`") && issues[0].contains("aaaaaaaaaaaa"));
        assert!(issues[1].contains("b.out -> a.in"), "{issues:?}");
        let same = |_: &str| Some(&old);
        assert!(schema_issues(&g, same).is_empty());
    }

    #[test]
    fn runtime_checks() {
        let registry = Registry::new([]);
        let g = Graph::from_yaml(
            "name: x\nclock: sim\ngroups:\n  a:\n    realtime: { priority: 120, rate: -1, deadlines_us: { ghost: 5 } }\nnodes:\n  n: { type: t/x, group: a }\n",
        )
        .unwrap();
        let issues = runtime_issues(&g, &registry);
        assert_eq!(issues.len(), 4, "{issues:?}");
        assert!(issues[0].message.contains("std/Clock"));
    }
}
