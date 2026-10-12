//! Python bindings generated from message schemas.
//!
//! Each struct becomes a `@dataclass` with typed fields and defaults taken
//! from the Rust `Default` implementation. Messages get a `TYPE` class
//! attribute. Every class converts to and from a "raw" form (nested dicts,
//! lists and scalars, i.e. the JSON form of the serde value) with `_to_raw`
//! and `_from_raw`; the native module uses that form to move values between
//! Python and Rust.
//!
//! Types with a hand-written encoding (`Schema::Custom`) are skipped: the
//! native module provides them with zero-copy buffer access.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use ruro_msg::schema::Field;
use ruro_msg::{Message, MessageSchema, Schema};
use serde_json::Value;

/// A message to generate bindings for, with its default value as JSON.
#[derive(Debug, Clone)]
pub struct Entry {
    pub schema: MessageSchema,
    pub default: Value,
}

impl Entry {
    /// An entry for a linked message type.
    pub fn of<M: Message + Default + serde::Serialize>() -> Self {
        Self {
            schema: M::message_schema(),
            default: serde_json::to_value(M::default()).expect("serializable"),
        }
    }
}

/// The standard messages, in the order they appear in the generated module.
pub fn std_entries() -> Vec<Entry> {
    use ruro_msg::*;
    vec![
        Entry::of::<Scalar>(),
        Entry::of::<Text>(),
        Entry::of::<Flag>(),
        Entry::of::<Bytes>(),
        Entry::of::<Vec3>(),
        Entry::of::<Pose>(),
        Entry::of::<Twist>(),
        Entry::of::<JointState>(),
        Entry::of::<JointCommand>(),
        Entry::of::<Transform>(),
        Entry::of::<Transforms>(),
        Entry::of::<Imu>(),
        Entry::of::<LaserScan>(),
        Entry::of::<Odometry>(),
        Entry::of::<CameraInfo>(),
        Entry::of::<Joy>(),
    ]
}

/// Python annotation for a schema.
fn annotation(s: &Schema) -> String {
    match s {
        Schema::Unit => "None".into(),
        Schema::Bool => "bool".into(),
        Schema::U8
        | Schema::U16
        | Schema::U32
        | Schema::U64
        | Schema::U128
        | Schema::I8
        | Schema::I16
        | Schema::I32
        | Schema::I64
        | Schema::I128 => "int".into(),
        Schema::F32 | Schema::F64 => "float".into(),
        Schema::Char | Schema::String => "str".into(),
        Schema::Seq { item } if **item == Schema::U8 => "bytes".into(),
        Schema::Option { some } => format!("Optional[{}]", annotation(some)),
        Schema::Seq { item } | Schema::Array { item, .. } => format!("List[{}]", annotation(item)),
        Schema::Map { key, value } => format!("Dict[{}, {}]", annotation(key), annotation(value)),
        Schema::Tuple { items } => {
            let items: Vec<String> = items.iter().map(annotation).collect();
            format!("Tuple[{}]", items.join(", "))
        }
        Schema::TupleStruct { items, .. } if items.len() == 1 && !is_class(s) => {
            annotation(&items[0])
        }
        Schema::Struct { name, .. } | Schema::TupleStruct { name, .. } => name.clone(),
        _ => "Any".into(),
    }
}

/// Whether the schema becomes a generated class.
fn is_class(s: &Schema) -> bool {
    matches!(s, Schema::Struct { .. } | Schema::TupleStruct { .. })
}

/// Python expression converting raw value `v` into the field type.
fn from_raw(s: &Schema, v: &str) -> String {
    match s {
        Schema::Seq { item } if **item == Schema::U8 => format!("bytes({v})"),
        Schema::Struct { name, .. } | Schema::TupleStruct { name, .. } => {
            format!("{name}._from_raw({v})")
        }
        Schema::Option { some } if needs_conversion(some) => {
            format!("(None if {v} is None else {})", from_raw(some, v))
        }
        Schema::Seq { item } | Schema::Array { item, .. } => {
            if needs_conversion(item) {
                format!("[{} for x in {v}]", from_raw(item, "x"))
            } else {
                format!("list({v})")
            }
        }
        Schema::F32 | Schema::F64 => format!("float({v})"),
        _ => v.to_owned(),
    }
}

/// Python expression converting field value `v` to its raw form.
fn to_raw(s: &Schema, v: &str) -> String {
    match s {
        Schema::Seq { item } if **item == Schema::U8 => format!("list({v})"),
        Schema::Struct { .. } | Schema::TupleStruct { .. } => format!("{v}._to_raw()"),
        Schema::Option { some } if needs_conversion(some) => {
            format!("(None if {v} is None else {})", to_raw(some, v))
        }
        Schema::Seq { item } | Schema::Array { item, .. } => {
            if needs_conversion(item) {
                format!("[{} for x in {v}]", to_raw(item, "x"))
            } else {
                format!("[{} for x in {v}]", scalar_cast(item, "x"))
            }
        }
        other => scalar_cast(other, v),
    }
}

fn scalar_cast(s: &Schema, v: &str) -> String {
    match s {
        Schema::F32 | Schema::F64 => format!("float({v})"),
        Schema::U8
        | Schema::U16
        | Schema::U32
        | Schema::U64
        | Schema::I8
        | Schema::I16
        | Schema::I32
        | Schema::I64 => format!("int({v})"),
        Schema::Bool => format!("bool({v})"),
        Schema::String => format!("str({v})"),
        _ => v.to_owned(),
    }
}

fn needs_conversion(s: &Schema) -> bool {
    match s {
        Schema::Struct { .. } | Schema::TupleStruct { .. } => true,
        Schema::Seq { item } => **item == Schema::U8 || needs_conversion(item),
        Schema::Array { item, .. } | Schema::Option { some: item } => needs_conversion(item),
        _ => false,
    }
}

/// Python literal for a JSON default.
fn literal(v: &Value, s: &Schema) -> String {
    match (v, s) {
        (Value::Null, _) => "None".into(),
        (Value::Bool(b), _) => if *b { "True" } else { "False" }.into(),
        (Value::Number(n), Schema::F32 | Schema::F64) => {
            let f = n.as_f64().unwrap_or(0.0);
            if f.fract() == 0.0 && f.abs() < 1e15 {
                format!("{f:.1}")
            } else {
                format!("{f}")
            }
        }
        (Value::Number(n), _) => n.to_string(),
        (Value::String(s), _) => format!("{s:?}"),
        _ => "None".into(),
    }
}

/// `field_name: Type = default` for a dataclass.
fn field_decl(name: &str, ty: &Schema, default: &Value) -> String {
    let ann = annotation(ty);
    let default = match ty {
        Schema::Struct { name, .. } | Schema::TupleStruct { name, .. } => {
            format!("field(default_factory={name})")
        }
        Schema::Seq { item } if **item == Schema::U8 => "b\"\"".into(),
        Schema::Seq { .. } | Schema::Array { .. } | Schema::Map { .. } => {
            let elems = default.as_array().cloned().unwrap_or_default();
            let item = match ty {
                Schema::Seq { item } | Schema::Array { item, .. } => item.as_ref(),
                _ => &Schema::Unit,
            };
            if elems.is_empty() {
                if matches!(ty, Schema::Map { .. }) {
                    "field(default_factory=dict)".into()
                } else {
                    "field(default_factory=list)".into()
                }
            } else if elems.iter().all(|e| e == &elems[0]) && !is_class(item) {
                format!(
                    "field(default_factory=lambda: [{}] * {})",
                    literal(&elems[0], item),
                    elems.len()
                )
            } else {
                let lits: Vec<String> = elems.iter().map(|e| literal(e, item)).collect();
                format!("field(default_factory=lambda: [{}])", lits.join(", "))
            }
        }
        _ => literal(default, ty),
    };
    format!("{name}: {ann} = {default}")
}

/// A class to emit: its schema, default value and message type, if any.
struct Class {
    schema: Schema,
    default: Value,
    message: Option<String>,
    doc: String,
}

/// Collects classes depth first so dependencies are defined before use.
fn collect(
    schema: &Schema,
    default: &Value,
    message: Option<&str>,
    out: &mut Vec<Class>,
    seen: &mut BTreeMap<String, ()>,
) {
    let fields: Vec<(Schema, Value)> = match schema {
        Schema::Struct { fields, .. } => fields
            .iter()
            .map(|f| {
                (
                    f.ty.clone(),
                    default.get(&f.name).cloned().unwrap_or(Value::Null),
                )
            })
            .collect(),
        Schema::TupleStruct { items, .. } if items.len() == 1 => {
            vec![(items[0].clone(), default.clone())]
        }
        Schema::TupleStruct { items, .. } => items
            .iter()
            .enumerate()
            .map(|(i, t)| (t.clone(), default.get(i).cloned().unwrap_or(Value::Null)))
            .collect(),
        _ => return,
    };
    for (ty, value) in &fields {
        let mut nested = ty;
        let mut nested_default = value.clone();
        while let Schema::Seq { item }
        | Schema::Array { item, .. }
        | Schema::Option { some: item } = nested
        {
            nested = item;
            nested_default = Value::Null;
        }
        if is_class(nested) {
            let d = if nested_default.is_null() {
                Value::Null
            } else {
                nested_default
            };
            collect(nested, &d, None, out, seen);
        }
    }
    let name = schema.type_name().unwrap_or_default().to_owned();
    if seen.insert(name, ()).is_none() {
        out.push(Class {
            schema: schema.clone(),
            default: default.clone(),
            message: message.map(str::to_owned),
            doc: String::new(),
        });
    }
}

fn emit_class(c: &Class, out: &mut String) {
    let name = c.schema.type_name().unwrap_or("Unnamed");
    let fields: Vec<Field> = match &c.schema {
        Schema::Struct { fields, .. } => fields.clone(),
        Schema::TupleStruct { items, .. } if items.len() == 1 => {
            vec![Field::new("value", items[0].clone())]
        }
        Schema::TupleStruct { items, .. } => items
            .iter()
            .enumerate()
            .map(|(i, t)| Field::new(format!("f{i}"), t.clone()))
            .collect(),
        _ => return,
    };
    let newtype = matches!(&c.schema, Schema::TupleStruct { items, .. } if items.len() == 1);
    let tuple = matches!(&c.schema, Schema::TupleStruct { .. }) && !newtype;
    let _ = writeln!(out, "\n\n@dataclass\nclass {name}:");
    let doc = match &c.message {
        Some(ty) => format!("``{ty}``{}", if c.doc.is_empty() { "" } else { ": " }),
        None => String::new(),
    };
    let _ = writeln!(out, "    \"\"\"{doc}{}\"\"\"\n", c.doc);
    if let Some(ty) = &c.message {
        let _ = writeln!(out, "    TYPE: ClassVar[str] = {ty:?}\n");
    }
    for (i, f) in fields.iter().enumerate() {
        let default = if newtype {
            c.default.clone()
        } else if tuple {
            c.default.get(i).cloned().unwrap_or(Value::Null)
        } else {
            c.default.get(&f.name).cloned().unwrap_or(Value::Null)
        };
        let _ = writeln!(out, "    {}", field_decl(&f.name, &f.ty, &default));
        if !f.doc.is_empty() {
            let _ = writeln!(out, "    \"\"\"{}\"\"\"", f.doc);
        }
    }

    // _to_raw
    let _ = writeln!(out, "\n    def _to_raw(self) -> Any:");
    if newtype {
        let _ = writeln!(
            out,
            "        return {}",
            to_raw(&fields[0].ty, "self.value")
        );
    } else if tuple {
        let items: Vec<String> = fields
            .iter()
            .map(|f| to_raw(&f.ty, &format!("self.{}", f.name)))
            .collect();
        let _ = writeln!(out, "        return [{}]", items.join(", "));
    } else {
        let _ = writeln!(out, "        return {{");
        for f in &fields {
            let _ = writeln!(
                out,
                "            {:?}: {},",
                f.name,
                to_raw(&f.ty, &format!("self.{}", f.name))
            );
        }
        let _ = writeln!(out, "        }}");
    }

    // _from_raw
    let _ = writeln!(
        out,
        "\n    @classmethod\n    def _from_raw(cls, raw: Any) -> \"{name}\":"
    );
    if newtype {
        let _ = writeln!(
            out,
            "        return cls({})",
            from_raw(&fields[0].ty, "raw")
        );
    } else if tuple {
        let items: Vec<String> = fields
            .iter()
            .enumerate()
            .map(|(i, f)| from_raw(&f.ty, &format!("raw[{i}]")))
            .collect();
        let _ = writeln!(out, "        return cls({})", items.join(", "));
    } else {
        let _ = writeln!(out, "        return cls(");
        for f in &fields {
            let _ = writeln!(
                out,
                "            {}={},",
                f.name,
                from_raw(&f.ty, &format!("raw[{:?}]", f.name))
            );
        }
        let _ = writeln!(out, "        )");
    }
}

/// Generates a Python module with a dataclass per struct reachable from `entries`.
pub fn generate(entries: &[Entry]) -> String {
    let mut classes = Vec::new();
    let mut seen = BTreeMap::new();
    for e in entries {
        if matches!(
            e.schema.schema,
            Schema::Custom { .. } | Schema::Opaque { .. }
        ) {
            continue;
        }
        collect(
            &e.schema.schema,
            &e.default,
            Some(&e.schema.name),
            &mut classes,
            &mut seen,
        );
    }
    let mut out = String::new();
    out.push_str(
        "# Message classes for ruro, generated from the message schemas by\n\
         # `ruro schema --python`. Do not edit by hand.\n\
         \n\
         from __future__ import annotations\n\
         \n\
         from dataclasses import dataclass, field\n\
         from typing import Any, ClassVar, Dict, List, Optional, Tuple\n",
    );
    let names: Vec<String> = classes
        .iter()
        .filter_map(|c| c.schema.type_name().map(|n| format!("{n:?}")))
        .collect();
    let _ = writeln!(out, "\n__all__ = [{}]", names.join(", "));
    let _ = writeln!(
        out,
        "\n#: Message type name to class, for every message in this module.\nMESSAGES: Dict[str, type] = {{}}"
    );
    for c in &classes {
        emit_class(c, &mut out);
    }
    out.push('\n');
    for c in classes.iter().filter(|c| c.message.is_some()) {
        let name = c.schema.type_name().unwrap_or_default();
        let _ = writeln!(out, "MESSAGES[{name}.TYPE] = {name}");
    }
    out
}

/// The module generated for the standard messages.
pub fn generate_std() -> String {
    generate(&std_entries())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn std_module_has_classes_and_defaults() {
        let py = generate_std();
        assert!(py.contains("class Scalar:"));
        assert!(py.contains("    value: float = 0.0"));
        assert!(py.contains("TYPE: ClassVar[str] = \"std/Pose\""));
        assert!(py.contains("class Quat:"), "nested structs are generated");
        assert!(py.contains("    w: float = 1.0"), "defaults come from Rust");
        assert!(py.contains(
            "orientation_covariance: List[float] = field(default_factory=lambda: [0.0] * 9)"
        ));
        assert!(py.contains("    value: bytes = b\"\""));
        assert!(
            !py.contains("class Image"),
            "custom encodings come from the native module"
        );
        // Header is defined before the first class that uses it.
        let header = py.find("class Header:").unwrap();
        let transform = py.find("class Transform:").unwrap();
        assert!(header < transform);
    }

    #[test]
    fn checked_in_module_is_current() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../ruro-py/python/ruro/msg.py");
        let Ok(on_disk) = std::fs::read_to_string(path) else {
            return;
        };
        assert!(
            on_disk == generate_std(),
            "{path} is stale; regenerate with `ruro schema --python > {path}`"
        );
    }
}
