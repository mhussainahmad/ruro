use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Static description of a node type: its ports and parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeDescriptor {
    /// Type name, e.g. `std/blur`.
    pub kind: String,
    /// One-line human description.
    #[serde(default)]
    pub doc: String,
    /// Default trigger rate in Hz, used when the graph does not set one.
    #[serde(default)]
    pub default_rate: Option<f64>,
    #[serde(default)]
    pub inputs: Vec<PortDesc>,
    #[serde(default)]
    pub outputs: Vec<PortDesc>,
    #[serde(default)]
    pub params: Vec<ParamDesc>,
}

impl NodeDescriptor {
    pub fn input(&self, name: &str) -> Option<&PortDesc> {
        self.inputs.iter().find(|p| p.name == name)
    }

    pub fn output(&self, name: &str) -> Option<&PortDesc> {
        self.outputs.iter().find(|p| p.name == name)
    }

    pub fn param(&self, name: &str) -> Option<&ParamDesc> {
        self.params.iter().find(|p| p.name == name)
    }

    /// The category shown in tooling: the part of the kind before the last `/`.
    pub fn category(&self) -> &str {
        self.kind.rsplit_once('/').map_or("", |(c, _)| c)
    }

    /// The short name: the part of the kind after the last `/`.
    pub fn short_name(&self) -> &str {
        self.kind
            .rsplit_once('/')
            .map_or(self.kind.as_str(), |(_, n)| n)
    }
}

/// A typed input or output port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortDesc {
    pub name: String,
    /// Message type name, e.g. `std/Image`.
    pub ty: String,
}

/// Value domain of a parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParamKind {
    Int,
    Float,
    Bool,
    String,
}

impl ParamKind {
    /// Checks that a JSON value belongs to this domain.
    pub fn accepts(self, v: &serde_json::Value) -> bool {
        match self {
            ParamKind::Int => v.as_i64().is_some() || v.as_u64().is_some(),
            ParamKind::Float => v.is_number(),
            ParamKind::Bool => v.is_boolean(),
            ParamKind::String => v.is_string(),
        }
    }
}

/// A tunable parameter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamDesc {
    pub name: String,
    pub kind: ParamKind,
    pub default: serde_json::Value,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub doc: String,
}

/// All node types known to a host, keyed by kind.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    pub nodes: BTreeMap<String, NodeDescriptor>,
    /// Wire-layout hash of each message type used by a port, keyed by type
    /// name (e.g. `std/Pose` → `RS1_…`). Types without a known schema are
    /// absent. Two registries that disagree on a hash were built from
    /// incompatible definitions of that message.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub schema_hashes: BTreeMap<String, String>,
}

impl Registry {
    pub fn new(descriptors: impl IntoIterator<Item = NodeDescriptor>) -> Self {
        Self {
            nodes: descriptors
                .into_iter()
                .map(|d| (d.kind.clone(), d))
                .collect(),
            schema_hashes: BTreeMap::new(),
        }
    }

    /// Wire-layout hash of a message type, if known.
    pub fn schema_hash(&self, ty: &str) -> Option<&str> {
        self.schema_hashes.get(ty).map(String::as_str)
    }

    /// Message types whose schema hash differs between `self` and `other`,
    /// as `(type, ours, theirs)`. Types only one side knows are not reported.
    pub fn schema_mismatches<'a>(
        &'a self,
        other: &'a Registry,
    ) -> Vec<(&'a str, &'a str, &'a str)> {
        self.schema_hashes
            .iter()
            .filter_map(|(ty, ours)| {
                let theirs = other.schema_hashes.get(ty)?;
                (ours != theirs).then_some((ty.as_str(), ours.as_str(), theirs.as_str()))
            })
            .collect()
    }

    pub fn get(&self, kind: &str) -> Option<&NodeDescriptor> {
        self.nodes.get(kind)
    }

    pub fn iter(&self) -> impl Iterator<Item = &NodeDescriptor> {
        self.nodes.values()
    }
}
