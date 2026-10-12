use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Static description of a node type: its ports and parameters.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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
    /// Where the node is implemented, as `path:line`, if known. Tools use it
    /// to open the node's source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
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

    /// The parameter `name`, or an error that suggests the closest names
    /// (or lists every parameter when none is close).
    pub fn param_or_suggest(&self, name: &str) -> Result<&ParamDesc, String> {
        if let Some(p) = self.param(name) {
            return Ok(p);
        }
        if self.params.is_empty() {
            return Err(format!("`{}` has no parameters", self.kind));
        }
        let quoted = |names: Vec<&str>| {
            names
                .iter()
                .map(|n| format!("`{n}`"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut close: Vec<(usize, &str)> = self
            .params
            .iter()
            .map(|p| (edit_distance(name, &p.name), p.name.as_str()))
            .filter(|(d, n)| *d <= (n.len().max(name.len()) / 3).max(2))
            .collect();
        close.sort();
        Err(if close.is_empty() {
            format!(
                "unknown parameter `{name}` of `{}`; it has {}",
                self.kind,
                quoted(self.params.iter().map(|p| p.name.as_str()).collect())
            )
        } else {
            format!(
                "unknown parameter `{name}` of `{}`; did you mean {}?",
                self.kind,
                quoted(close.into_iter().map(|(_, n)| n).collect())
            )
        })
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
/// Levenshtein distance between two names.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diag = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let next = (diag + usize::from(ca != *cb))
                .min(row[j] + 1)
                .min(row[j + 1] + 1);
            diag = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

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
impl ParamDesc {
    /// Checks a value's type and range.
    pub fn check(&self, value: &serde_json::Value) -> Result<(), String> {
        let name = &self.name;
        if !self.kind.accepts(value) {
            let expected = match self.kind {
                ParamKind::Int => "an integer",
                ParamKind::Float => "a number",
                ParamKind::Bool => "true or false",
                ParamKind::String => "a string",
            };
            return Err(format!(
                "parameter `{name}` expects {expected}, got {value}"
            ));
        }
        if let Some(v) = value.as_f64()
            && (self.min.is_some_and(|min| v < min) || self.max.is_some_and(|max| v > max))
        {
            let bound = |b: Option<f64>| b.map_or("..".to_owned(), |b| b.to_string());
            return Err(format!(
                "parameter `{name}` = {v} is outside [{}, {}]",
                bound(self.min),
                bound(self.max)
            ));
        }
        Ok(())
    }

    /// Reads a value as typed on a command line: JSON (`1.5`, `true`,
    /// `"text"`), or else the text itself as a string. A string parameter
    /// takes the text as is, so `5` or `true` can be strings too. The
    /// result is checked with [`ParamDesc::check`].
    pub fn parse(&self, text: &str) -> Result<serde_json::Value, String> {
        use serde_json::Value;
        let value = match serde_json::from_str::<Value>(text) {
            Ok(Value::String(s)) => Value::String(s),
            Ok(_) | Err(_) if self.kind == ParamKind::String => Value::String(text.to_owned()),
            Ok(v) => v,
            Err(_) => Value::String(text.to_owned()),
        };
        self.check(&value)?;
        Ok(value)
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pid() -> NodeDescriptor {
        let p = |name: &str, kind, min, max| ParamDesc {
            name: name.into(),
            kind,
            default: json!(0),
            min,
            max,
            doc: String::new(),
        };
        NodeDescriptor {
            kind: "std/pid".into(),
            params: vec![
                p("kp", ParamKind::Float, Some(-10.0), Some(10.0)),
                p("ki", ParamKind::Float, None, None),
                p("window", ParamKind::Int, Some(1.0), None),
                p("label", ParamKind::String, None, None),
                p("enabled", ParamKind::Bool, None, None),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn unknown_parameters_get_suggestions() {
        let d = pid();
        let err = d.param_or_suggest("kq").unwrap_err();
        assert!(err.contains("did you mean `ki`, `kp`?"), "{err}");
        let err = d.param_or_suggest("windwo").unwrap_err();
        assert!(err.contains("`window`") && !err.contains("`kp`"), "{err}");
        let err = d.param_or_suggest("frequency").unwrap_err();
        assert!(err.contains("it has `kp`, `ki`, `window`"), "{err}");
        assert_eq!(d.param_or_suggest("kp").unwrap().name, "kp");
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }

    #[test]
    fn typed_values_are_checked() {
        let d = pid();
        let param = |n: &str| d.param(n).unwrap();
        assert_eq!(param("kp").parse("-1.5").unwrap(), json!(-1.5));
        assert!(
            param("kp")
                .parse("-11")
                .unwrap_err()
                .contains("outside [-10, 10]")
        );
        assert!(
            param("kp")
                .parse("fast")
                .unwrap_err()
                .contains("expects a number")
        );
        assert!(
            param("window")
                .parse("2.5")
                .unwrap_err()
                .contains("integer")
        );
        assert!(
            param("window")
                .parse("0")
                .unwrap_err()
                .contains("outside [1, ..]")
        );
        assert_eq!(param("window").parse("3").unwrap(), json!(3));
        assert_eq!(param("label").parse("5").unwrap(), json!("5"));
        assert_eq!(param("label").parse("\"a b\"").unwrap(), json!("a b"));
        assert_eq!(param("enabled").parse("true").unwrap(), json!(true));
        assert!(param("enabled").parse("yes").is_err());
    }
}
