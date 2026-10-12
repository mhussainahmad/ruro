//! The sim/real example graphs are valid and differ only in the robot nodes.

use std::path::Path;

use ruro_graph::{Graph, validate};

fn load(name: &str) -> Graph {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    Graph::load(root.join(name)).unwrap()
}

/// Node types that only exist when their cargo feature is enabled.
const FEATURE_GATED: &[&str] = &["sim/mujoco", "ml/lerobot-recorder", "ml/onnx", "ml/policy"];

#[test]
fn sim_and_real_examples_validate() {
    ruro_nodes_ml::link();
    ruro_host::describe(); // links every node crate of the stock host
    let registry = ruro::node::registry();
    for name in ["arm-sim.yaml", "arm-real.yaml"] {
        let g = load(name);
        let gated: Vec<&str> = g
            .nodes
            .iter()
            .filter(|(_, n)| {
                FEATURE_GATED.contains(&n.kind.as_str()) && registry.get(&n.kind).is_none()
            })
            .map(|(id, _)| id.as_str())
            .collect();
        let issues: Vec<String> = validate(&g, &registry)
            .into_iter()
            .map(|i| i.to_string())
            .filter(|i| {
                !gated
                    .iter()
                    .any(|id| i.contains(&format!("{id}:")) || i.contains(&format!("{id}.")))
            })
            .collect();
        assert!(issues.is_empty(), "{name}: {issues:#?}");
    }
}

#[test]
fn only_the_robot_nodes_differ() {
    let (sim, real) = (load("arm-sim.yaml"), load("arm-real.yaml"));
    assert_eq!(sim.wires, real.wires);
    for (id, node) in &sim.nodes {
        let other = &real.nodes[id];
        if node.group() == "robot" {
            continue;
        }
        assert_eq!(node.kind, other.kind, "{id}");
        let mut a = node.params.clone();
        let mut b = other.params.clone();
        // The datasets are kept apart; everything else is identical.
        a.remove("root");
        b.remove("root");
        assert_eq!(a, b, "{id}");
    }
    assert_eq!(sim.nodes["arm"].kind, "sim/mujoco");
    // The simulation drives graph time; the real robot runs on the wall clock.
    assert_eq!(sim.clock, ruro_graph::ClockSource::Sim);
    assert_eq!(real.clock, ruro_graph::ClockSource::Wall);
    assert_eq!(real.nodes["arm"].kind, "hw/so100-arm");
}
