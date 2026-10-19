//! Graphs made by `ruro ros2 import` validate against the registered
//! `ros2/*` node types.

use std::path::Path;

use ruro::graph::validate;
use ruro_ros2::launch::{ImportOptions, import};

#[test]
fn imported_launch_files_validate() {
    ruro_nodes_ros2::link();
    let fixtures =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/ruro-ros2/tests/fixtures/launch");
    let opts = ImportOptions {
        ament_prefix_path: Some(fixtures.join("prefix").display().to_string()),
        subscribe: vec![("/odom".into(), "nav_msgs/msg/Odometry".into())],
        publish: vec![("/cmd_vel".into(), "geometry_msgs/msg/Twist".into())],
        ..Default::default()
    };
    let registry = ruro::node::registry();
    for file in ["robot.launch.xml", "camera.launch.yaml"] {
        let imported = import(&fixtures.join(file), &opts).unwrap();
        assert!(imported.graph.nodes.len() >= 3, "{file}");
        let issues: Vec<String> = validate(&imported.graph, &registry)
            .into_iter()
            .map(|i| i.to_string())
            .collect();
        assert!(issues.is_empty(), "{file}: {issues:#?}");
    }
}
