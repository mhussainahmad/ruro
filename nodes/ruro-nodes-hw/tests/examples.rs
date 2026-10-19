//! The camera example graphs validate against the stock host's node types.

use std::path::Path;

use ruro_graph::{Graph, validate};

/// Node types that only exist when their cargo feature is enabled.
const FEATURE_GATED: &[&str] = &["hw/realsense"];

#[test]
fn camera_examples_validate() {
    ruro_nodes_hw::link();
    ruro_host::describe(); // links every node crate of the stock host
    let registry = ruro::node::registry();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    for name in ["rtsp-camera.yaml", "realsense.yaml"] {
        let g = Graph::load(root.join(name)).unwrap();
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
