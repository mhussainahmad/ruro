//! `ruro check`: validates a graph as the daemon would before deploying it.

use std::path::Path;

use anyhow::{Context, Result, bail};
use ruro_graph::{Graph, Issue, Registry};
use ruro_proto::{Ctl, Keys};

/// What a check found: problems, and things that could not be checked.
#[derive(Debug, Default)]
pub struct Findings {
    pub issues: Vec<String>,
    pub warnings: Vec<String>,
}

/// Checks the graph at `path`. With a daemon to ask (`daemon`), against the
/// hosts each group would run on, including peers and uploaded hosts;
/// otherwise against this machine's default host.
pub fn check(path: &Path, daemon: Option<(&zenoh::Session, &Keys)>) -> Result<()> {
    let graph = Graph::load(path).with_context(|| format!("loading {}", path.display()))?;
    let base = crate::base_dir(path);
    // Composites are expanded first: their own problems are reported with
    // the node that uses them, and the summary counts the nodes that run.
    let flat = match graph.flatten_in(&base) {
        Ok(flat) => flat,
        Err(_) => {
            let registry = match daemon {
                Some((s, k)) => remote_registry(s, k)?,
                None => local_registry()?,
            };
            let issues = ruro_graph::validate_in(&graph, &registry, &base);
            return report(
                path,
                &graph,
                Findings {
                    issues: issues.iter().map(ToString::to_string).collect(),
                    warnings: Vec::new(),
                },
            );
        }
    };
    let findings = match daemon {
        Some((session, keys)) => {
            let body = ruro_proto::request(
                session,
                keys,
                Ctl::Validate,
                &flat.to_yaml()?,
                crate::TIMEOUT,
            )
            .with_context(|| format!("checking with daemon `{}`", keys.daemon()))?;
            Findings {
                issues: serde_json::from_str(&body).context("malformed reply")?,
                warnings: Vec::new(),
            }
        }
        None => offline(&flat, &local_registry()?),
    };
    report(path, &flat, findings)
}

fn report(path: &Path, graph: &Graph, findings: Findings) -> Result<()> {
    for w in &findings.warnings {
        eprintln!("warning: {w}");
    }
    if !findings.issues.is_empty() {
        for i in &findings.issues {
            eprintln!("  {i}");
        }
        bail!("{} issue(s) in {}", findings.issues.len(), path.display());
    }
    let groups = graph.groups().len();
    println!(
        "{}: ok ({} nodes, {} wires, {groups} process group{})",
        path.display(),
        graph.nodes.len(),
        graph.wires.len(),
        if groups == 1 { "" } else { "s" }
    );
    Ok(())
}

/// Checks a flattened graph against the node types of the local default
/// host. Groups that run elsewhere (`machine`) or on a custom host (`host`)
/// cannot be checked without a daemon: their node types are taken on trust.
pub fn offline(flat: &Graph, local: &Registry) -> Findings {
    let elsewhere = |group: &str| {
        let c = flat.group_config(group);
        c.host.is_some() || c.machine.is_some()
    };
    let offered = |group: &str| (!elsewhere(group)).then_some(local);
    let mut findings = Findings::default();
    let issues: Vec<Issue> = ruro_graph::validate(flat, local)
        .into_iter()
        .chain(ruro_graph::runtime_issues(flat, local))
        .chain(ruro_graph::placement_issues(flat, local, offered))
        .collect();
    for issue in issues {
        let trusted = issue.is_unknown_type()
            && issue
                .node
                .as_deref()
                .and_then(|n| flat.nodes.get(n))
                .is_some_and(|n| elsewhere(n.group()));
        if trusted {
            findings.warnings.push(format!(
                "{issue} (not checked: its group runs on another host; run `ruro check` with a \
                 daemon to check it)"
            ));
        } else {
            findings.issues.push(issue.to_string());
        }
    }
    findings
}

fn local_registry() -> Result<Registry> {
    ruro_daemon::load_registry(&ruro_daemon::default_host_binary())
}

fn remote_registry(session: &zenoh::Session, keys: &Keys) -> Result<Registry> {
    let body = ruro_proto::request(session, keys, Ctl::Registry, "", crate::TIMEOUT)?;
    Ok(serde_json::from_str(&body)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruro_graph::NodeDescriptor;

    #[test]
    fn offline_checks_trust_the_types_of_other_hosts() {
        let local = Registry::new([NodeDescriptor {
            kind: "std/sine".into(),
            default_rate: Some(10.0),
            ..Default::default()
        }]);
        let graph = Graph::from_yaml(
            "name: g\ngroups:\n  custom: { host: my-host }\nnodes:\n  a: { type: std/sine }\n  b: { type: my/scale, group: custom, rate: 1 }\n  c: { type: my/typo, rate: 1 }\n",
        )
        .unwrap();
        let f = offline(&graph, &local);
        assert_eq!(f.issues.len(), 1, "{f:?}");
        assert!(f.issues[0].contains("my/typo"), "{f:?}");
        assert_eq!(f.warnings.len(), 1, "{f:?}");
        assert!(f.warnings[0].contains("my/scale"), "{f:?}");
    }
}
