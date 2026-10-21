use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Group used for nodes that do not name one. All such nodes share a process.
pub const DEFAULT_GROUP: &str = "main";

#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error("invalid port reference `{0}`: expected `node.port`")]
    BadPortRef(String),
    #[error("invalid wire `{0}`: expected `node.port -> node.port`")]
    BadWire(String),
    #[error("yaml: {0}")]
    Yaml(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("composite node `{node}`: {message}")]
    Composite { node: String, message: String },
    #[error("overlay `{overlay}`: {message}")]
    Overlay { overlay: String, message: String },
}

/// A complete dataflow graph.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Graph {
    pub name: String,
    /// Settings for process groups, keyed by group name. Groups without an
    /// entry use the defaults.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub groups: IndexMap<String, GroupConfig>,
    /// Ports this graph exposes when used as a composite node: exposed input
    /// name to the inner input port(s) it feeds.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub inputs: IndexMap<String, PortList>,
    /// Exposed output name to the inner output port it carries.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub outputs: IndexMap<String, PortRef>,
    /// Source of graph time. `sim` makes every host follow the graph's
    /// `std/Clock` output instead of the wall clock.
    #[serde(default, skip_serializing_if = "ClockSource::is_wall")]
    pub clock: ClockSource,
    /// How the graph's nodes appear to ROS 2. `None`: they do not (bridge
    /// nodes still talk to ROS 2 on their own).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ros2: Option<Ros2Settings>,
    #[serde(default)]
    pub nodes: IndexMap<String, NodeInstance>,
    #[serde(default)]
    pub wires: Vec<Wire>,
    /// Named variants of the graph, selected when it is run (see
    /// [`Graph::with_overlays`]).
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub overlays: IndexMap<String, crate::Overlay>,
}

/// How and where a process group runs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GroupConfig {
    /// Daemon (machine) that runs the group. `None` means the deploying daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    /// Host program that executes the group, e.g. a host with extra node
    /// libraries linked in. `None` means the daemon's default host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Real-time scheduling for the group's threads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub realtime: Option<Realtime>,
}

/// Real-time execution settings for a process group.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Realtime {
    /// SCHED_FIFO priority (1-99). `None` keeps the default scheduler.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
    /// CPUs the group's threads are pinned to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cpus: Vec<usize>,
    /// Run all nodes of the group on one thread in a fixed order each cycle,
    /// instead of one thread per node.
    #[serde(default)]
    pub deterministic: bool,
    /// Cycle rate in Hz of a deterministic group. Defaults to the highest
    /// rate of its nodes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<f64>,
    /// Step deadlines in microseconds, keyed by node id. A step that takes
    /// longer is reported as an error and counted in the node's metrics.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub deadlines_us: IndexMap<String, u64>,
}

/// How a graph's nodes appear to ROS 2 (the graph's `ros2:` section).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ros2Settings {
    /// Every node offers the ROS 2 parameter services as the ROS node
    /// `/ruro_<node id>`, so `ros2 param` and rqt can read and set its
    /// parameters.
    #[serde(default)]
    pub parameters: bool,
    /// Zenoh router shared with the ROS 2 nodes.
    #[serde(default = "Ros2Settings::default_router")]
    pub router: String,
    /// `ROS_DOMAIN_ID`; unset uses the environment variable (default 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain_id: Option<u32>,
}

impl Ros2Settings {
    fn default_router() -> String {
        "tcp/127.0.0.1:7447".into()
    }

    /// The ROS node a ruro node appears as: `ruro_` and its id, with
    /// characters ROS does not allow in node names replaced by `_`.
    pub fn node_name(id: &str) -> String {
        let id: String = id
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        format!("ruro_{id}")
    }
}

impl Default for Ros2Settings {
    fn default() -> Self {
        Self {
            parameters: false,
            router: Self::default_router(),
            domain_id: None,
        }
    }
}

/// Where graph time comes from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClockSource {
    /// Wall-clock time.
    #[default]
    Wall,
    /// Simulated time, published by the graph's one `std/Clock` output.
    Sim,
}

impl ClockSource {
    pub fn is_wall(&self) -> bool {
        *self == ClockSource::Wall
    }
}

/// One instance of a node type inside a graph.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeInstance {
    /// Node type, e.g. `std/blur`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Process group. Nodes in the same group share a process and exchange
    /// messages without copying; separate groups are isolated processes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Timer trigger in Hz. Without it the node runs whenever an input receives data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<f64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart: Option<RestartPolicy>,
    /// Inline subgraph of a `type: composite` node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<Box<Graph>>,
    /// Editor position.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pos: Option<[f32; 2]>,
}

impl NodeInstance {
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            ..Default::default()
        }
    }

    pub fn group(&self) -> &str {
        self.group.as_deref().unwrap_or(DEFAULT_GROUP)
    }
}

/// What the supervisor does when a group process exits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    Never,
    #[default]
    OnFailure,
    Always,
}

/// `node.port`
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PortRef {
    pub node: String,
    pub port: String,
}

impl PortRef {
    pub fn new(node: impl Into<String>, port: impl Into<String>) -> Self {
        Self {
            node: node.into(),
            port: port.into(),
        }
    }
}

impl fmt::Display for PortRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.node, self.port)
    }
}

impl FromStr for PortRef {
    type Err = GraphError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (node, port) = s
            .trim()
            .split_once('.')
            .ok_or_else(|| GraphError::BadPortRef(s.into()))?;
        if node.is_empty() || port.is_empty() {
            return Err(GraphError::BadPortRef(s.into()));
        }
        Ok(Self::new(node, port))
    }
}

impl Serialize for PortRef {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for PortRef {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        String::deserialize(d)?.parse().map_err(D::Error::custom)
    }
}

/// One or more ports, written as `node.port` or a list of them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PortList(pub Vec<PortRef>);

impl Serialize for PortList {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.0.as_slice() {
            [one] => one.serialize(s),
            many => many.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for PortList {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            One(PortRef),
            Many(Vec<PortRef>),
        }
        Ok(match Repr::deserialize(d)? {
            Repr::One(p) => PortList(vec![p]),
            Repr::Many(v) => PortList(v),
        })
    }
}

/// A directed connection from an output port to an input port.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Wire {
    pub from: PortRef,
    pub to: PortRef,
    /// How many messages the input keeps; older ones are dropped. Default 1.
    pub depth: Option<usize>,
}

impl Wire {
    pub fn new(from: PortRef, to: PortRef) -> Self {
        Self {
            from,
            to,
            depth: None,
        }
    }

    pub fn depth(&self) -> usize {
        self.depth.unwrap_or(1).max(1)
    }
}

impl fmt::Display for Wire {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} -> {}", self.from, self.to)
    }
}

impl FromStr for Wire {
    type Err = GraphError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (a, b) = s
            .split_once("->")
            .ok_or_else(|| GraphError::BadWire(s.into()))?;
        Ok(Self::new(a.parse()?, b.parse()?))
    }
}

/// Wires serialize as `"a.out -> b.in"`, or as a map when they carry options.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum WireRepr {
    Short(String),
    Long {
        from: String,
        to: String,
        depth: Option<usize>,
    },
}

impl Serialize for Wire {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.depth {
            None => WireRepr::Short(self.to_string()),
            Some(depth) => WireRepr::Long {
                from: self.from.to_string(),
                to: self.to.to_string(),
                depth: Some(depth),
            },
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for Wire {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        match WireRepr::deserialize(d)? {
            WireRepr::Short(s) => s.parse().map_err(D::Error::custom),
            WireRepr::Long { from, to, depth } => Ok(Wire {
                from: from.parse().map_err(D::Error::custom)?,
                to: to.parse().map_err(D::Error::custom)?,
                depth,
            }),
        }
    }
}

impl Graph {
    /// Settings of `group`, or the defaults.
    pub fn group_config(&self, group: &str) -> GroupConfig {
        self.groups.get(group).cloned().unwrap_or_default()
    }

    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }

    pub fn from_yaml(text: &str) -> Result<Self, GraphError> {
        serde_saphyr::from_str(text).map_err(|e| GraphError::Yaml(e.to_string()))
    }

    pub fn to_yaml(&self) -> Result<String, GraphError> {
        serde_saphyr::to_string(self).map_err(|e| GraphError::Yaml(e.to_string()))
    }

    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self, GraphError> {
        Self::from_yaml(&std::fs::read_to_string(path)?)
    }

    pub fn save(&self, path: impl AsRef<std::path::Path>) -> Result<(), GraphError> {
        std::fs::write(path, self.to_yaml()?)?;
        Ok(())
    }

    /// Node ids grouped by process group, in declaration order.
    pub fn groups(&self) -> BTreeMap<String, Vec<String>> {
        let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (id, node) in &self.nodes {
            out.entry(node.group().to_string())
                .or_default()
                .push(id.clone());
        }
        out
    }

    pub fn group_of(&self, node: &str) -> Option<&str> {
        self.nodes.get(node).map(NodeInstance::group)
    }

    /// Wires that cross a process boundary.
    pub fn is_remote(&self, wire: &Wire) -> bool {
        self.group_of(&wire.from.node) != self.group_of(&wire.to.node)
    }

    /// Output ports of `group` that feed at least one input in another group.
    pub fn exported_outputs(&self, group: &str) -> BTreeSet<PortRef> {
        self.wires
            .iter()
            .filter(|w| self.group_of(&w.from.node) == Some(group) && self.is_remote(w))
            .map(|w| w.from.clone())
            .collect()
    }

    /// Wires that end in `node.port`.
    pub fn wires_into<'a>(&'a self, to: &'a PortRef) -> impl Iterator<Item = &'a Wire> + 'a {
        self.wires.iter().filter(move |w| &w.to == to)
    }

    /// Removes a node and every wire touching it.
    pub fn remove_node(&mut self, id: &str) -> Option<NodeInstance> {
        self.wires.retain(|w| w.from.node != id && w.to.node != id);
        self.nodes.shift_remove(id)
    }

    /// A fresh node id derived from `base` that is not used yet.
    pub fn unique_id(&self, base: &str) -> String {
        let base: String = base
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        if !self.nodes.contains_key(&base) {
            return base;
        }
        (2..)
            .map(|i| format!("{base}{i}"))
            .find(|id| !self.nodes.contains_key(id))
            .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const YAML: &str = r#"
name: demo
nodes:
  cam:
    type: std/fake-camera
    group: sensors
    rate: 30
    params:
      width: 640
  blur:
    type: std/blur
    params: { radius: 3 }
  stats:
    type: std/image-stats
wires:
  - cam.image -> blur.image
  - from: blur.image
    to: stats.image
    depth: 4
"#;

    #[test]
    fn parses_yaml() {
        let g = Graph::from_yaml(YAML).unwrap();
        assert_eq!(g.name, "demo");
        assert_eq!(g.nodes.keys().collect::<Vec<_>>(), ["cam", "blur", "stats"]);
        assert_eq!(g.nodes["cam"].rate, Some(30.0));
        assert_eq!(g.nodes["blur"].params["radius"], serde_json::json!(3));
        assert_eq!(
            g.wires[0],
            Wire::new(PortRef::new("cam", "image"), PortRef::new("blur", "image"))
        );
        assert_eq!(g.wires[1].depth(), 4);
    }

    #[test]
    fn yaml_roundtrip() {
        let g = Graph::from_yaml(YAML).unwrap();
        let again = Graph::from_yaml(&g.to_yaml().unwrap()).unwrap();
        assert_eq!(g, again);
    }

    #[test]
    fn groups_and_remote_wires() {
        let g = Graph::from_yaml(YAML).unwrap();
        let groups = g.groups();
        assert_eq!(groups["sensors"], ["cam"]);
        assert_eq!(groups[DEFAULT_GROUP], ["blur", "stats"]);
        assert!(g.is_remote(&g.wires[0]));
        assert!(!g.is_remote(&g.wires[1]));
        assert_eq!(g.exported_outputs("sensors").len(), 1);
        assert!(g.exported_outputs(DEFAULT_GROUP).is_empty());
    }

    #[test]
    fn bad_wire_is_rejected() {
        assert!("cam.image blur.image".parse::<Wire>().is_err());
        assert!("cam -> blur.image".parse::<Wire>().is_err());
    }

    #[test]
    fn unique_ids() {
        let g = Graph::from_yaml(YAML).unwrap();
        assert_eq!(g.unique_id("blur"), "blur2");
        assert_eq!(g.unique_id("image stats"), "image_stats");
    }
}

#[cfg(test)]
mod group_config_tests {
    use super::*;

    #[test]
    fn group_settings_parse_and_default() {
        let g = Graph::from_yaml(
            "name: rt\ngroups:\n  control:\n    machine: robot\n    realtime: { priority: 80, cpus: [2, 3], deterministic: true }\nnodes:\n  a: { type: t/x, group: control }\n",
        )
        .unwrap();
        let c = g.group_config("control");
        assert_eq!(c.machine.as_deref(), Some("robot"));
        let rt = c.realtime.unwrap();
        assert_eq!(
            (rt.priority, rt.cpus, rt.deterministic),
            (Some(80), vec![2, 3], true)
        );
        assert_eq!(g.group_config("other"), GroupConfig::default());
        assert_eq!(Graph::from_yaml(&g.to_yaml().unwrap()).unwrap(), g);
    }

    #[test]
    fn clock_rate_and_deadlines_parse() {
        let g = Graph::from_yaml(
            "name: sim\nclock: sim\ngroups:\n  c:\n    realtime: { deterministic: true, rate: 1000, deadlines_us: { a: 250 } }\nnodes:\n  a: { type: t/x, group: c }\n",
        )
        .unwrap();
        assert_eq!(g.clock, ClockSource::Sim);
        let rt = g.group_config("c").realtime.unwrap();
        assert_eq!(rt.rate, Some(1000.0));
        assert_eq!(rt.deadlines_us["a"], 250);
        assert_eq!(Graph::from_yaml(&g.to_yaml().unwrap()).unwrap(), g);
        let plain = Graph::from_yaml("name: p\n").unwrap();
        assert_eq!(plain.clock, ClockSource::Wall);
        assert!(!plain.to_yaml().unwrap().contains("clock"));
    }

    #[test]
    fn ros2_settings_parse() {
        let g = Graph::from_yaml("name: r\nros2: { parameters: true, domain_id: 3 }\n").unwrap();
        let r = g.ros2.clone().unwrap();
        assert!(r.parameters);
        assert_eq!(r.router, "tcp/127.0.0.1:7447");
        assert_eq!(r.domain_id, Some(3));
        assert_eq!(Graph::from_yaml(&g.to_yaml().unwrap()).unwrap(), g);
        assert!(
            !Graph::from_yaml("name: p\n")
                .unwrap()
                .to_yaml()
                .unwrap()
                .contains("ros2")
        );
        assert!(Graph::from_yaml("name: r\nros2: { parameter: true }\n").is_err());
    }
}
