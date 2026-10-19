//! Overlays: named variants of a graph, kept in the same file.
//!
//! An overlay patches the graph it belongs to. A typical use is one graph
//! for a robot with a `sim` and a `real` overlay that swap the driver nodes:
//!
//! ```yaml
//! name: rover
//! nodes:
//!   lidar: { type: ros2/sub-laser-scan, params: { topic: /scan } }
//!   avoid: { type: std/scan-avoid }
//! wires: [lidar.out -> avoid.scan]
//! overlays:
//!   real:
//!     nodes:
//!       lidar: { type: hw/rplidar, params: { device: /dev/ttyUSB0 } }
//!     wires:
//!       remove: [lidar.out -> avoid.scan]
//!       add: [lidar.scan -> avoid.scan]
//! ```
//!
//! Selected overlays are applied in order, before composite nodes are
//! flattened and before validation, so the result is checked like any graph.
//! The rules, applied for each overlay:
//!
//! 1. `clock` replaces the graph's clock.
//! 2. `groups`: each entry sets the fields it names in that group's settings
//!    (`realtime` as a whole); `null` removes the group's settings.
//! 3. `wires.remove`: each wire must exist; it is removed (depth is ignored
//!    when matching).
//! 4. `nodes`, in order: `null` removes the node and every wire touching it.
//!    An entry for a node that does not exist adds it and must have a `type`.
//!    Otherwise the entry patches the node: `group`, `rate`, `restart`, `pos`
//!    and `graph` replace the base's values. If `type` names a different
//!    type, the node's `params` and `graph` are replaced by the overlay's
//!    (the base's belong to the old type); otherwise `params` are merged key
//!    by key and a `null` value removes a key.
//! 5. `wires.add`: the wires are appended.
//!
//! Every mistake is an error: an unknown overlay, a patch without `type` for
//! a node that does not exist, or removing a node, wire or group setting that
//! is not there.

use std::collections::BTreeMap;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::spec::{ClockSource, Graph, GraphError, GroupConfig, NodeInstance, RestartPolicy, Wire};

/// A named variant of a graph: a patch applied on top of it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Overlay {
    /// Replaces the graph's clock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock: Option<ClockSource>,
    /// Group settings to set, or `null` to remove a group's settings.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub groups: IndexMap<String, Option<GroupConfig>>,
    /// Nodes to patch or add, or `null` to remove a node.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub nodes: IndexMap<String, Option<NodePatch>>,
    #[serde(default, skip_serializing_if = "WirePatch::is_empty")]
    pub wires: WirePatch,
}

/// Changes to one node. Fields left out keep the base's value.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NodePatch {
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<f64>,
    /// Merged into the base's parameters; `null` removes one.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart: Option<RestartPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<Box<Graph>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pos: Option<[f32; 2]>,
}

/// Wires an overlay removes and adds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WirePatch {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove: Vec<Wire>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub add: Vec<Wire>,
}

impl WirePatch {
    pub fn is_empty(&self) -> bool {
        self.remove.is_empty() && self.add.is_empty()
    }
}

fn err(overlay: &str, message: impl Into<String>) -> GraphError {
    GraphError::Overlay {
        overlay: overlay.to_owned(),
        message: message.into(),
    }
}

/// Parameters without the `null` values, which only mean "remove" in a patch.
fn present(params: &BTreeMap<String, serde_json::Value>) -> BTreeMap<String, serde_json::Value> {
    params
        .iter()
        .filter(|(_, v)| !v.is_null())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

impl Graph {
    /// Names of the graph's overlays, in file order.
    pub fn overlay_names(&self) -> Vec<&str> {
        self.overlays.keys().map(String::as_str).collect()
    }

    /// The graph with the overlays `names` applied in order. The result has
    /// no overlays of its own; with no names it is the base graph.
    pub fn with_overlays<S: AsRef<str>>(&self, names: &[S]) -> Result<Graph, GraphError> {
        let mut out = self.clone();
        out.overlays.clear();
        for name in names {
            let name = name.as_ref();
            let overlay = self.overlays.get(name).ok_or_else(|| {
                let known = if self.overlays.is_empty() {
                    "the graph has no overlays".to_owned()
                } else {
                    format!("the graph has {}", self.overlay_names().join(", "))
                };
                err(name, format!("no such overlay; {known}"))
            })?;
            overlay.apply(name, &mut out)?;
        }
        Ok(out)
    }
}

impl Overlay {
    /// Applies this overlay, named `name` in errors, to `g`.
    pub fn apply(&self, name: &str, g: &mut Graph) -> Result<(), GraphError> {
        if let Some(clock) = self.clock {
            g.clock = clock;
        }
        for (group, patch) in &self.groups {
            match patch {
                None => {
                    if g.groups.shift_remove(group).is_none() {
                        return Err(err(
                            name,
                            format!("cannot remove settings of group `{group}`: it has none"),
                        ));
                    }
                }
                Some(p) => {
                    let c = g.groups.entry(group.clone()).or_default();
                    if p.machine.is_some() {
                        c.machine.clone_from(&p.machine);
                    }
                    if p.host.is_some() {
                        c.host.clone_from(&p.host);
                    }
                    if p.realtime.is_some() {
                        c.realtime.clone_from(&p.realtime);
                    }
                }
            }
        }
        for wire in &self.wires.remove {
            let before = g.wires.len();
            g.wires
                .retain(|w| !(w.from == wire.from && w.to == wire.to));
            if g.wires.len() == before {
                return Err(err(
                    name,
                    format!("cannot remove wire `{wire}`: it is not in the graph"),
                ));
            }
        }
        for (id, patch) in &self.nodes {
            let Some(p) = patch else {
                if g.remove_node(id).is_none() {
                    return Err(err(
                        name,
                        format!("cannot remove node `{id}`: it is not in the graph"),
                    ));
                }
                continue;
            };
            match g.nodes.get_mut(id) {
                Some(node) => p.patch(node),
                None => {
                    let kind = p.kind.clone().ok_or_else(|| {
                        err(
                            name,
                            format!("node `{id}` is not in the graph; to add it, give its `type`"),
                        )
                    })?;
                    let node = NodeInstance {
                        kind,
                        group: p.group.clone(),
                        rate: p.rate,
                        params: present(&p.params),
                        restart: p.restart,
                        graph: p.graph.clone(),
                        pos: p.pos,
                    };
                    g.nodes.insert(id.clone(), node);
                }
            }
        }
        g.wires.extend(self.wires.add.iter().cloned());
        Ok(())
    }
}

impl NodePatch {
    fn patch(&self, node: &mut NodeInstance) {
        match &self.kind {
            Some(kind) if *kind != node.kind => {
                node.kind = kind.clone();
                node.params = present(&self.params);
                node.graph = self.graph.clone();
            }
            _ => {
                for (k, v) in &self.params {
                    if v.is_null() {
                        node.params.remove(k);
                    } else {
                        node.params.insert(k.clone(), v.clone());
                    }
                }
                if self.graph.is_some() {
                    node.graph.clone_from(&self.graph);
                }
            }
        }
        if self.group.is_some() {
            node.group.clone_from(&self.group);
        }
        if self.rate.is_some() {
            node.rate = self.rate;
        }
        if self.restart.is_some() {
            node.restart = self.restart;
        }
        if self.pos.is_some() {
            node.pos = self.pos;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ROBOT: &str = r#"
name: robot
groups:
  robot: { machine: bench }
nodes:
  arm:
    type: sim/arm
    group: robot
    rate: 100
    params: { model: arm.xml, speed: 1.0 }
  camera: { type: std/fake-camera, group: robot, rate: 30, params: { width: 640 } }
  jog: { type: hw/joint-jog, rate: 50, params: { axes: "0,1" } }
  recorder: { type: ml/recorder, params: { root: data/sim, fps: 30 } }
wires:
  - jog.command -> arm.command
  - arm.state -> jog.state
  - camera.image -> recorder.image
overlays:
  real:
    clock: wall
    groups:
      robot: { machine: robot-pi }
    nodes:
      arm:
        type: hw/arm
        params: { device: /dev/ttyACM0 }
      recorder:
        params: { root: data/real, fps: null }
  bench:
    nodes:
      camera: null
      viewer: { type: std/image-stats, pos: [10, 20] }
    wires:
      add: [jog.command -> viewer.image]
"#;

    fn robot() -> Graph {
        Graph::from_yaml(ROBOT).unwrap()
    }

    #[test]
    fn overlays_parse_and_roundtrip() {
        let g = robot();
        assert_eq!(g.overlay_names(), ["real", "bench"]);
        assert_eq!(g.overlays["bench"].nodes["camera"], None);
        assert_eq!(
            g.overlays["real"].nodes["recorder"]
                .as_ref()
                .unwrap()
                .params["fps"],
            serde_json::Value::Null
        );
        let again = Graph::from_yaml(&g.to_yaml().unwrap()).unwrap();
        assert_eq!(again, g);
        // A graph without overlays does not mention them.
        let plain = g.with_overlays::<&str>(&[]).unwrap();
        assert!(plain.overlays.is_empty());
        assert!(!plain.to_yaml().unwrap().contains("overlays"));
    }

    #[test]
    fn changing_the_type_replaces_params_and_keeps_placement() {
        let g = robot().with_overlays(&["real"]).unwrap();
        let arm = &g.nodes["arm"];
        assert_eq!(arm.kind, "hw/arm");
        // The simulator's parameters belong to the old type.
        assert_eq!(
            arm.params,
            BTreeMap::from([("device".into(), json!("/dev/ttyACM0"))])
        );
        assert_eq!((arm.group(), arm.rate), ("robot", Some(100.0)));
        // Same type: parameters are merged, `null` removes one.
        assert_eq!(
            g.nodes["recorder"].params,
            BTreeMap::from([("root".into(), json!("data/real"))])
        );
        assert_eq!(g.group_config("robot").machine.as_deref(), Some("robot-pi"));
        // Untouched nodes, wires and their order stay as they were.
        assert_eq!(
            g.nodes.keys().collect::<Vec<_>>(),
            ["arm", "camera", "jog", "recorder"]
        );
        assert_eq!(g.wires, robot().wires);
        assert!(g.overlays.is_empty());
    }

    #[test]
    fn removing_and_adding_nodes_and_wires() {
        let g = robot().with_overlays(&["bench"]).unwrap();
        assert!(!g.nodes.contains_key("camera"));
        // The removed node's wires go with it.
        let wires: Vec<String> = g.wires.iter().map(ToString::to_string).collect();
        assert_eq!(
            wires,
            [
                "jog.command -> arm.command",
                "arm.state -> jog.state",
                "jog.command -> viewer.image"
            ]
        );
        assert_eq!(g.nodes["viewer"].kind, "std/image-stats");
        assert_eq!(g.nodes["viewer"].pos, Some([10.0, 20.0]));
    }

    /// `overlays:` with one more overlay parsed from YAML.
    fn with(name: &str, yaml: &str) -> Graph {
        let mut g = robot();
        let o = Graph::from_yaml(&format!("name: x\noverlays:\n  {name}: {yaml}\n")).unwrap();
        g.overlays.extend(o.overlays);
        g
    }

    #[test]
    fn overlays_compose_in_order() {
        let g = robot().with_overlays(&["real", "bench"]).unwrap();
        assert_eq!(g.nodes["arm"].kind, "hw/arm");
        assert!(g.nodes.contains_key("viewer") && !g.nodes.contains_key("camera"));
        let g = with("slow", "{ nodes: { arm: { rate: 10 } } }");
        // A later overlay patches what an earlier one left.
        let a = g.with_overlays(&["real", "slow"]).unwrap();
        assert_eq!(
            (a.nodes["arm"].kind.as_str(), a.nodes["arm"].rate),
            ("hw/arm", Some(10.0))
        );
        let b = g.with_overlays(&["slow", "real"]).unwrap();
        assert_eq!(b.nodes["arm"].rate, Some(10.0));
    }

    #[test]
    fn the_clock_can_be_switched() {
        let g = with("sim", "{ clock: sim }");
        assert_eq!(g.with_overlays(&["sim"]).unwrap().clock, ClockSource::Sim);
        assert_eq!(
            g.with_overlays(&["sim", "real"]).unwrap().clock,
            ClockSource::Wall
        );
    }

    fn error(yaml_overlay: &str, select: &[&str]) -> String {
        let g = Graph::from_yaml(&format!("{ROBOT}  broken:\n{yaml_overlay}")).unwrap();
        g.with_overlays(select).unwrap_err().to_string()
    }

    #[test]
    fn mistakes_are_errors() {
        let e = robot().with_overlays(&["rea"]).unwrap_err().to_string();
        assert!(
            e.contains("overlay `rea`") && e.contains("real, bench"),
            "{e}"
        );
        let e = Graph::new("g")
            .with_overlays(&["sim"])
            .unwrap_err()
            .to_string();
        assert!(e.contains("has no overlays"), "{e}");
        for (overlay, needle) in [
            (
                "    nodes:\n      armm: { params: { x: 1 } }\n",
                "node `armm` is not in the graph",
            ),
            (
                "    nodes:\n      gone: null\n",
                "cannot remove node `gone`",
            ),
            (
                "    wires:\n      remove: [arm.state -> recorder.state]\n",
                "cannot remove wire",
            ),
            ("    groups:\n      vision: null\n", "group `vision`"),
        ] {
            let e = error(overlay, &["broken"]);
            assert!(e.contains("overlay `broken`") && e.contains(needle), "{e}");
        }
    }
}
