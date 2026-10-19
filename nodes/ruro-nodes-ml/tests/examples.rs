//! The SO-100 example graph is valid with every combination of its overlays,
//! and its `real` overlay changes only the robot nodes.

use std::path::Path;

use ruro_graph::{Graph, validate};

fn load(overlays: &[&str]) -> Graph {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/arm.yaml");
    Graph::load(path).unwrap().with_overlays(overlays).unwrap()
}

/// Node types that only exist when their cargo feature is enabled.
const FEATURE_GATED: &[&str] = &["sim/mujoco", "ml/lerobot-recorder", "ml/onnx", "ml/policy"];

#[test]
fn every_variant_validates() {
    ruro_nodes_ml::link();
    ruro_host::describe(); // links every node crate of the stock host
    let registry = ruro::node::registry();
    for overlays in [&[][..], &["real"], &["policy"], &["real", "policy"]] {
        let g = load(overlays);
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
        assert!(issues.is_empty(), "{overlays:?}: {issues:#?}");
    }
}

#[test]
fn only_the_robot_nodes_differ() {
    let (sim, real) = (load(&[]), load(&["real"]));
    assert_eq!(sim.wires, real.wires);
    assert_eq!(
        sim.nodes.keys().collect::<Vec<_>>(),
        real.nodes.keys().collect::<Vec<_>>()
    );
    for (id, node) in &sim.nodes {
        let other = &real.nodes[id];
        assert_eq!(node.group(), other.group(), "{id}");
        if node.group() == "robot" {
            continue;
        }
        assert_eq!(node.kind, other.kind, "{id}");
        let mut a = node.params.clone();
        let mut b = other.params.clone();
        // The datasets are kept apart; everything else is identical.
        if id == "recorder" {
            assert_ne!(a.remove("root"), b.remove("root"));
        }
        assert_eq!(a, b, "{id}");
    }
    assert_eq!(sim.nodes["arm"].kind, "sim/mujoco");
    // The simulation drives graph time; the real robot runs on the wall clock.
    assert_eq!(sim.clock, ruro_graph::ClockSource::Sim);
    assert_eq!(real.clock, ruro_graph::ClockSource::Wall);
    assert_eq!(real.nodes["arm"].kind, "hw/so100-arm");
    assert!(
        !real.nodes["arm"].params.contains_key("model"),
        "the simulator's parameters do not leak into the driver"
    );
}

#[test]
fn the_policy_replaces_the_gamepad() {
    let g = load(&["real", "policy"]);
    assert!(!g.nodes.contains_key("gamepad") && !g.nodes.contains_key("jog"));
    let into_arm: Vec<String> = g
        .wires
        .iter()
        .filter(|w| w.to.node == "arm")
        .map(ToString::to_string)
        .collect();
    assert_eq!(into_arm, ["policy.action -> arm.command"]);
}

/// The Gazebo rover (both variants) and the Isaac Sim example validate against
/// the stock host's node types. Only the Gazebo one has been run for real.
#[test]
fn simulator_examples_validate() {
    ruro_host::describe();
    let registry = ruro::node::registry();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    for (file, overlays) in [
        ("gazebo/rover.yaml", &[][..]),
        ("gazebo/rover.yaml", &["real"]),
        ("isaac-sim/carter.yaml", &[]),
    ] {
        let g = Graph::load(root.join(file))
            .unwrap()
            .with_overlays(overlays)
            .unwrap();
        let issues: Vec<String> = validate(&g, &registry)
            .into_iter()
            .map(|i| i.to_string())
            .collect();
        assert!(issues.is_empty(), "{file} {overlays:?}: {issues:#?}");
    }
    let real = Graph::load(root.join("gazebo/rover.yaml"))
        .unwrap()
        .with_overlays(&["real"])
        .unwrap();
    assert_eq!(real.nodes["lidar"].kind, "hw/rplidar");
    assert_eq!(real.group_config("robot").machine.as_deref(), Some("rover"));
}
