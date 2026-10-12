//! Message schema tooling: export, compatibility checks and code generation.
//!
//! Schemas come from [`ruro_msg::schema`]: every message type linked into a
//! binary registers its layout. This crate turns them into JSON, compares two
//! versions of a message for wire compatibility, and generates bindings for
//! other languages.

pub mod cli;
pub mod diff;
pub mod python;

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use ruro_msg::MessageSchema;

pub use diff::{Change, Report, Severity, diff};

/// Every message schema linked into this binary, sorted by type name.
pub fn linked() -> Vec<MessageSchema> {
    ruro_msg::schema::registered().into_values().collect()
}

/// Schemas as pretty JSON: one object for a single schema, an array otherwise.
pub fn to_json(schemas: &[MessageSchema]) -> String {
    let value = match schemas {
        [one] => serde_json::to_value(one),
        many => serde_json::to_value(many),
    }
    .expect("schemas are serializable");
    serde_json::to_string_pretty(&value).expect("json is serializable")
}

/// Parses schema JSON as written by [`to_json`]: a single schema or an array.
pub fn from_json(text: &str) -> Result<Vec<MessageSchema>> {
    let value: serde_json::Value = serde_json::from_str(text)?;
    Ok(if value.is_array() {
        serde_json::from_value(value)?
    } else {
        vec![serde_json::from_value(value)?]
    })
}

/// Loads schemas from a JSON file.
pub fn load(path: &Path) -> Result<Vec<MessageSchema>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    from_json(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Compares two sets of schemas. Types present on one side only are reported
/// as breaking changes (a peer would have nothing to decode them with).
pub fn diff_sets(old: &[MessageSchema], new: &[MessageSchema]) -> Vec<Report> {
    let old: BTreeMap<&str, &MessageSchema> = old.iter().map(|s| (s.name.as_str(), s)).collect();
    let new_map: BTreeMap<&str, &MessageSchema> =
        new.iter().map(|s| (s.name.as_str(), s)).collect();
    // A single schema on each side is compared even if it was renamed.
    if old.len() == 1 && new_map.len() == 1 {
        let (a, b) = (
            old.values().next().unwrap(),
            new_map.values().next().unwrap(),
        );
        return vec![diff(a, b)];
    }
    let mut reports = Vec::new();
    for (name, a) in &old {
        match new_map.get(name) {
            Some(b) => reports.push(diff(a, b)),
            None => reports.push(Report {
                name: (*name).to_owned(),
                old_hash: a.hash.clone(),
                new_hash: "-".into(),
                changes: vec![Change {
                    path: String::new(),
                    severity: Severity::Breaking,
                    what: "message type removed".into(),
                }],
            }),
        }
    }
    for (name, b) in &new_map {
        if !old.contains_key(name) {
            reports.push(Report {
                name: (*name).to_owned(),
                old_hash: "-".into(),
                new_hash: b.hash.clone(),
                changes: vec![Change {
                    path: String::new(),
                    severity: Severity::Cosmetic,
                    what: "message type added".into(),
                }],
            });
        }
    }
    reports
}

/// Finds one schema by type name, accepting a missing `std/` prefix.
pub fn find<'a>(schemas: &'a [MessageSchema], ty: &str) -> Result<&'a MessageSchema> {
    let prefixed = format!("std/{ty}");
    schemas
        .iter()
        .find(|s| s.name == ty)
        .or_else(|| schemas.iter().find(|s| s.name == prefixed))
        .ok_or_else(|| anyhow!("no schema for message type `{ty}`"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruro_msg::Schema;
    use ruro_msg::schema::Field;

    #[test]
    fn json_roundtrip_single_and_many() {
        let all = linked();
        assert!(all.iter().any(|s| s.name == "std/Pose"));
        let one = find(&all, "Pose").unwrap().clone();
        assert_eq!(
            from_json(&to_json(std::slice::from_ref(&one))).unwrap(),
            [one]
        );
        assert_eq!(from_json(&to_json(&all)).unwrap(), all);
    }

    #[test]
    fn set_diff_reports_removed_and_added_types() {
        let a = MessageSchema::new("t/A", Schema::structure("A", vec![]));
        let b = MessageSchema::new(
            "t/B",
            Schema::structure("B", vec![Field::new("x", Schema::U8)]),
        );
        let c = MessageSchema::new("t/C", Schema::Unit);
        let reports = diff_sets(&[a.clone(), b.clone()], &[a, c]);
        let by_name: BTreeMap<_, _> = reports.iter().map(|r| (r.name.as_str(), r)).collect();
        assert!(by_name["t/B"].is_breaking());
        assert_eq!(by_name["t/C"].severity(), Some(Severity::Cosmetic));
        assert_eq!(by_name["t/A"].severity(), None);
    }

    #[test]
    fn unknown_type_is_an_error() {
        assert!(find(&linked(), "nope/Nothing").is_err());
    }
}
