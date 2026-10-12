//! Graph templates built into the studio.

use crate::icons;

pub struct Template {
    pub id: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub icon: &'static str,
    /// Graph YAML; empty for a blank graph.
    pub yaml: &'static str,
}

pub const TEMPLATES: &[Template] = &[
    Template {
        id: "empty",
        title: "Empty graph",
        description: "A blank canvas",
        icon: icons::FILE,
        yaml: "",
    },
    Template {
        id: "camera-pipeline",
        title: "Camera pipeline",
        description: "Camera in its own process feeding blur, grayscale and statistics",
        icon: icons::CAMERA,
        yaml: include_str!("../templates/camera-pipeline.yaml"),
    },
    Template {
        id: "pid-loop",
        title: "PID loop",
        description: "A PID controller driving a simulated joint along a trajectory",
        icon: icons::GAUGE,
        yaml: include_str!("../templates/pid-loop.yaml"),
    },
    Template {
        id: "sensor-fusion",
        title: "Sensor fusion stub",
        description: "Two isolated sensors conditioned by a subgraph, ready for an estimator",
        icon: icons::MERGE,
        yaml: include_str!("../templates/sensor-fusion.yaml"),
    },
];

pub fn get(id: &str) -> Option<&'static Template> {
    TEMPLATES.iter().find(|t| t.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_are_valid_graphs() {
        ruro_nodes_std::link();
        let registry = ruro_node::registry();
        for t in TEMPLATES.iter().filter(|t| !t.yaml.is_empty()) {
            let g = ruro_graph::Graph::from_yaml(t.yaml).unwrap();
            let issues = ruro_graph::validate(&g, &registry);
            assert!(issues.is_empty(), "{}: {issues:?}", t.id);
        }
    }
}
