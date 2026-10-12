//! Compatibility of two versions of a message schema.
//!
//! Messages crossing a process boundary are postcard-encoded. Postcard writes
//! values in declaration order with no field names, tags or lengths for
//! structs, so:
//!
//! - adding, removing or reordering fields changes the byte layout and is
//!   **breaking**: old and new peers misread each other;
//! - changing a field's type is **breaking**, even between "compatible" types
//!   such as `f32` and `f64` or `u32` and `u64` (varints of different ranges);
//! - renaming a field (same position and type) is **wire-compatible**, but
//!   code and tools that address fields by name (Python, JSON previews) see a
//!   different message;
//! - appending an enum variant is **backward-compatible**: new readers decode
//!   old data, old readers fail on the new variant;
//! - documentation and Rust type names do not affect the wire.

use std::fmt;

use ruro_msg::MessageSchema;
use ruro_msg::schema::{Field, Schema, VariantPayload};
use serde::Serialize;

/// How a change affects peers built from the old schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    /// Documentation or naming that never reaches the wire.
    Cosmetic,
    /// Same bytes, different meaning for name-based consumers (field renames).
    WireCompatible,
    /// New readers accept old data; old readers may reject new data.
    BackwardCompatible,
    /// The layout cannot be verified (opaque types).
    Unknown,
    /// Old and new peers cannot exchange messages.
    Breaking,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Severity::Cosmetic => "cosmetic",
            Severity::WireCompatible => "wire-compatible",
            Severity::BackwardCompatible => "backward-compatible",
            Severity::Unknown => "unknown",
            Severity::Breaking => "breaking",
        })
    }
}

/// One difference between two schemas.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Change {
    /// Where the change is, e.g. `header.frame_id` or `boxes[].x`.
    pub path: String,
    pub severity: Severity,
    /// Human-readable description.
    pub what: String,
}

/// The result of comparing two versions of a message.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    pub name: String,
    pub old_hash: String,
    pub new_hash: String,
    pub changes: Vec<Change>,
}

impl Report {
    /// The most severe change, or `None` if the schemas are identical.
    pub fn severity(&self) -> Option<Severity> {
        self.changes.iter().map(|c| c.severity).max()
    }

    /// Whether old and new peers can no longer talk.
    pub fn is_breaking(&self) -> bool {
        self.severity() == Some(Severity::Breaking)
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.severity() {
            None => writeln!(f, "{}: identical ({})", self.name, self.new_hash),
            Some(s) => {
                writeln!(
                    f,
                    "{}: {s} ({} -> {})",
                    self.name, self.old_hash, self.new_hash
                )?;
                for c in &self.changes {
                    let path = if c.path.is_empty() { "." } else { &c.path };
                    writeln!(f, "  {:<20} {path}: {}", c.severity.to_string(), c.what)?;
                }
                Ok(())
            }
        }
    }
}

/// Compares two versions of one message.
pub fn diff(old: &MessageSchema, new: &MessageSchema) -> Report {
    let mut changes = Vec::new();
    if old.name != new.name {
        changes.push(Change {
            path: String::new(),
            severity: Severity::Breaking,
            what: format!(
                "message type renamed from `{}` to `{}`; wires type-check by name",
                old.name, new.name
            ),
        });
    }
    compare(&old.schema, &new.schema, "", &mut changes);
    Report {
        name: new.name.clone(),
        old_hash: old.schema.wire_hash(),
        new_hash: new.schema.wire_hash(),
        changes,
    }
}

fn join(path: &str, part: &str) -> String {
    if path.is_empty() {
        part.to_owned()
    } else if part.starts_with('[') {
        format!("{path}{part}")
    } else {
        format!("{path}.{part}")
    }
}

fn push(out: &mut Vec<Change>, path: &str, severity: Severity, what: String) {
    out.push(Change {
        path: path.to_owned(),
        severity,
        what,
    });
}

/// A struct-like view: named fields in wire order. Newtypes and tuple structs
/// get positional names so both sides compare the same way.
fn as_fields(s: &Schema) -> Option<Vec<Field>> {
    match s {
        Schema::Struct { fields, .. } => Some(fields.clone()),
        Schema::TupleStruct { items, .. } if items.len() != 1 => Some(
            items
                .iter()
                .enumerate()
                .map(|(i, t)| Field::new(i.to_string(), t.clone()))
                .collect(),
        ),
        Schema::Tuple { items } => Some(
            items
                .iter()
                .enumerate()
                .map(|(i, t)| Field::new(i.to_string(), t.clone()))
                .collect(),
        ),
        _ => None,
    }
}

/// Unwraps newtypes, which postcard encodes as their single field.
fn transparent(s: &Schema) -> &Schema {
    match s {
        Schema::TupleStruct { items, .. } if items.len() == 1 => transparent(&items[0]),
        other => other,
    }
}

fn compare(old: &Schema, new: &Schema, path: &str, out: &mut Vec<Change>) {
    if let (Some(a), Some(b)) = (old.type_name(), new.type_name())
        && a != b
        && !matches!(old, Schema::Opaque { .. })
        && !matches!(new, Schema::Opaque { .. })
    {
        push(
            out,
            path,
            Severity::Cosmetic,
            format!("type renamed from `{a}` to `{b}`"),
        );
    }
    let (old, new) = (transparent(old), transparent(new));
    match (old, new) {
        (Schema::Opaque { name: a }, Schema::Opaque { name: b }) => {
            let what = if a == b {
                format!("`{a}` has no schema; compatibility cannot be checked")
            } else {
                format!("opaque `{a}` replaced by opaque `{b}`")
            };
            push(out, path, Severity::Unknown, what);
        }
        (Schema::Opaque { name }, _) | (_, Schema::Opaque { name }) => push(
            out,
            path,
            Severity::Unknown,
            format!("`{name}` has no schema on one side; compatibility cannot be checked"),
        ),
        (Schema::Option { some: a }, Schema::Option { some: b })
        | (Schema::Seq { item: a }, Schema::Seq { item: b }) => {
            compare(a, b, &join(path, "[]"), out)
        }
        (Schema::Array { item: a, len: la }, Schema::Array { item: b, len: lb }) => {
            if la != lb {
                push(
                    out,
                    path,
                    Severity::Breaking,
                    format!("array length changed from {la} to {lb}"),
                );
            }
            compare(a, b, &join(path, "[]"), out);
        }
        (Schema::Map { key: ka, value: va }, Schema::Map { key: kb, value: vb }) => {
            compare(ka, kb, &join(path, "{key}"), out);
            compare(va, vb, &join(path, "{value}"), out);
        }
        (Schema::Enum { variants: va, .. }, Schema::Enum { variants: vb, .. }) => {
            compare_enums(va, vb, path, out)
        }
        (
            Schema::Custom {
                encoding: ea,
                fields: fa,
                ..
            },
            Schema::Custom {
                encoding: eb,
                fields: fb,
                ..
            },
        ) => {
            let id = |e: &str| e.split(':').next().unwrap_or("").trim().to_owned();
            if id(ea) != id(eb) {
                push(
                    out,
                    path,
                    Severity::Breaking,
                    format!("custom encoding changed from `{}` to `{}`", id(ea), id(eb)),
                );
            } else {
                // Same hand-written encoding: fields are informative only.
                compare_fields(fa, fb, path, out, Severity::Cosmetic);
            }
        }
        _ => match (as_fields(old), as_fields(new)) {
            (Some(fa), Some(fb)) => compare_fields(&fa, &fb, path, out, Severity::Breaking),
            _ => {
                if old.wire_signature() != new.wire_signature() {
                    push(
                        out,
                        path,
                        Severity::Breaking,
                        format!(
                            "type changed from {} to {}",
                            old.display_name(),
                            new.display_name()
                        ),
                    );
                }
            }
        },
    }
}

/// Compares fields in wire order. `layout` is the severity of a layout
/// change: breaking for postcard structs, cosmetic for the informative fields
/// of a custom encoding.
fn compare_fields(
    old: &[Field],
    new: &[Field],
    path: &str,
    out: &mut Vec<Change>,
    layout: Severity,
) {
    let common = old.len().min(new.len());
    for i in 0..common {
        let (a, b) = (&old[i], &new[i]);
        let field_path = join(path, &b.name);
        if a.name != b.name {
            let moved =
                new.iter().any(|f| f.name == a.name) || old.iter().any(|f| f.name == b.name);
            if moved {
                push(
                    out,
                    &field_path,
                    layout,
                    format!(
                        "fields reordered: position {i} was `{}`, is now `{}`",
                        a.name, b.name
                    ),
                );
            } else {
                push(
                    out,
                    &field_path,
                    Severity::WireCompatible.min(layout),
                    format!(
                        "field renamed from `{}` to `{}` (same bytes; name-based consumers break)",
                        a.name, b.name
                    ),
                );
            }
        }
        if a.doc != b.doc {
            push(
                out,
                &field_path,
                Severity::Cosmetic,
                "documentation changed".into(),
            );
        }
        let mut nested = Vec::new();
        compare(&a.ty, &b.ty, &field_path, &mut nested);
        for mut c in nested {
            if layout == Severity::Cosmetic {
                c.severity = c.severity.min(Severity::Cosmetic);
            }
            out.push(c);
        }
    }
    for f in &old[common..] {
        push(
            out,
            &join(path, &f.name),
            layout,
            format!("field `{}` removed", f.name),
        );
    }
    for f in &new[common..] {
        push(
            out,
            &join(path, &f.name),
            layout,
            format!(
                "field `{}` added (postcard has no optional fields: old peers misread it)",
                f.name
            ),
        );
    }
}

fn compare_enums(
    old: &[ruro_msg::schema::Variant],
    new: &[ruro_msg::schema::Variant],
    path: &str,
    out: &mut Vec<Change>,
) {
    let common = old.len().min(new.len());
    for i in 0..common {
        let (a, b) = (&old[i], &new[i]);
        let vpath = join(path, &b.name);
        if a.name != b.name {
            let moved = new.iter().any(|v| v.name == a.name);
            let (severity, what) = if moved {
                (
                    Severity::Breaking,
                    format!(
                        "variants reordered: index {i} was `{}`, is now `{}`",
                        a.name, b.name
                    ),
                )
            } else {
                (
                    Severity::WireCompatible,
                    format!("variant renamed from `{}` to `{}`", a.name, b.name),
                )
            };
            push(out, &vpath, severity, what);
        }
        let payload = |p: &VariantPayload| -> Schema {
            match p {
                VariantPayload::Unit => Schema::Unit,
                VariantPayload::Tuple { items } if items.len() == 1 => items[0].clone(),
                VariantPayload::Tuple { items } => Schema::Tuple {
                    items: items.clone(),
                },
                VariantPayload::Struct { fields } => Schema::Struct {
                    name: String::new(),
                    fields: fields.clone(),
                },
            }
        };
        compare(&payload(&a.payload), &payload(&b.payload), &vpath, out);
    }
    for v in &old[common..] {
        push(
            out,
            &join(path, &v.name),
            Severity::Breaking,
            format!("variant `{}` removed", v.name),
        );
    }
    for v in &new[common..] {
        push(
            out,
            &join(path, &v.name),
            Severity::BackwardCompatible,
            format!(
                "variant `{}` appended (old readers reject messages that use it)",
                v.name
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruro_msg::schema::{Field, Variant};

    fn msg(fields: Vec<Field>) -> MessageSchema {
        MessageSchema::new("t/M", Schema::structure("M", fields))
    }

    fn f(name: &str, ty: Schema) -> Field {
        Field::new(name, ty)
    }

    #[test]
    fn identical_schemas_have_no_changes() {
        let a = msg(vec![f("x", Schema::F64)]);
        let r = diff(&a, &a);
        assert_eq!(r.severity(), None);
        assert!(r.to_string().contains("identical"));
    }

    #[test]
    fn added_removed_and_reordered_fields_break() {
        let old = msg(vec![f("x", Schema::F64), f("y", Schema::F64)]);
        let added = msg(vec![
            f("x", Schema::F64),
            f("y", Schema::F64),
            f("z", Schema::F64),
        ]);
        let removed = msg(vec![f("x", Schema::F64)]);
        let reordered = msg(vec![f("y", Schema::F64), f("x", Schema::F64)]);
        for new in [added, removed, reordered] {
            let r = diff(&old, &new);
            assert!(r.is_breaking(), "{r}");
        }
    }

    #[test]
    fn type_changes_break_even_when_widening() {
        let old = msg(vec![f("x", Schema::F32)]);
        let new = msg(vec![f("x", Schema::F64)]);
        let r = diff(&old, &new);
        assert!(r.is_breaking());
        assert_eq!(r.changes[0].path, "x");
        assert!(r.changes[0].what.contains("f32 to f64"));
    }

    #[test]
    fn renames_are_wire_compatible_and_docs_cosmetic() {
        let old = msg(vec![f("x", Schema::F64)]);
        let new = msg(vec![Field::new("pos", Schema::F64).doc("metres")]);
        let r = diff(&old, &new);
        assert_eq!(r.severity(), Some(Severity::WireCompatible));
        assert_eq!(r.old_hash, r.new_hash);
    }

    #[test]
    fn nested_changes_report_their_path() {
        let inner = |t| Schema::structure("P", vec![f("v", t)]);
        let old = msg(vec![f("pts", Schema::seq(inner(Schema::U32)))]);
        let new = msg(vec![f("pts", Schema::seq(inner(Schema::I32)))]);
        let r = diff(&old, &new);
        assert_eq!(r.changes.last().unwrap().path, "pts[].v");
        assert!(r.is_breaking());
    }

    #[test]
    fn enum_append_is_backward_compatible() {
        let e = |names: &[&str]| Schema::Enum {
            name: "E".into(),
            variants: names
                .iter()
                .map(|n| Variant {
                    name: (*n).into(),
                    payload: VariantPayload::Unit,
                })
                .collect(),
        };
        let old = msg(vec![f("e", e(&["A", "B"]))]);
        let appended = msg(vec![f("e", e(&["A", "B", "C"]))]);
        let removed = msg(vec![f("e", e(&["A"]))]);
        let swapped = msg(vec![f("e", e(&["B", "A"]))]);
        assert_eq!(
            diff(&old, &appended).severity(),
            Some(Severity::BackwardCompatible)
        );
        assert!(diff(&old, &removed).is_breaking());
        assert!(diff(&old, &swapped).is_breaking());
    }

    #[test]
    fn opaque_parts_are_unknown() {
        let old = msg(vec![f("x", Schema::opaque("Blob"))]);
        let r = diff(&old, &old);
        assert_eq!(r.severity(), Some(Severity::Unknown));
    }

    #[test]
    fn custom_encoding_change_breaks() {
        let c = |enc: &str| {
            MessageSchema::new(
                "t/Img",
                Schema::Custom {
                    name: "Img".into(),
                    encoding: enc.into(),
                    fields: vec![f("w", Schema::U32)],
                },
            )
        };
        assert_eq!(diff(&c("img-v1: a"), &c("img-v1: b")).severity(), None);
        assert!(diff(&c("img-v1: a"), &c("img-v2: a")).is_breaking());
    }

    #[test]
    fn newtype_wrapping_is_transparent() {
        let old = msg(vec![f("x", Schema::F64)]);
        let new = msg(vec![f(
            "x",
            Schema::tuple_struct("Metres", vec![Schema::F64]),
        )]);
        assert_eq!(diff(&old, &new).severity(), None);
    }
}
