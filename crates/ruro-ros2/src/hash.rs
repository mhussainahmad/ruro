//! RIHS01 type hashes (REP-2011).
//!
//! The hash identifies a message's structure. It is the SHA-256 of a JSON
//! rendering of the type description: the message's fields plus, sorted by
//! name, every message it references transitively. Field defaults and
//! constants are not part of it. The JSON is rendered exactly as Python's
//! `json.dumps(..., separators=(', ', ': '))` does in
//! `rosidl_generator_type_description`, since the bytes are what is hashed.
//! rmw_zenoh puts the hash in every topic key, so a mismatch means no data.

use std::fmt::Write as _;

use sha2::{Digest, Sha256};

use crate::idl::{Array, BaseType, Field, Interfaces, MessageSpec, Primitive, TypeName};

/// `type_id` values of `type_description_interfaces/msg/FieldType`.
mod id {
    pub const NESTED_TYPE: u8 = 1;
    pub const INT8: u8 = 2;
    pub const UINT8: u8 = 3;
    pub const INT16: u8 = 4;
    pub const UINT16: u8 = 5;
    pub const INT32: u8 = 6;
    pub const UINT32: u8 = 7;
    pub const INT64: u8 = 8;
    pub const UINT64: u8 = 9;
    pub const FLOAT: u8 = 10;
    pub const DOUBLE: u8 = 11;
    pub const BOOLEAN: u8 = 15;
    pub const BYTE: u8 = 16;
    pub const STRING: u8 = 17;
    pub const WSTRING: u8 = 18;
    pub const BOUNDED_STRING: u8 = 21;
    pub const BOUNDED_WSTRING: u8 = 22;
    /// Offsets added to the element id for arrays and sequences.
    pub const ARRAY: u8 = 48;
    pub const BOUNDED_SEQUENCE: u8 = 96;
    pub const UNBOUNDED_SEQUENCE: u8 = 144;
}

fn primitive_id(p: Primitive) -> u8 {
    match p {
        Primitive::Bool => id::BOOLEAN,
        Primitive::Byte => id::BYTE,
        // `char` is `uint8` in ROS 2 interface definitions.
        Primitive::Char | Primitive::Uint8 => id::UINT8,
        Primitive::Int8 => id::INT8,
        Primitive::Int16 => id::INT16,
        Primitive::Uint16 => id::UINT16,
        Primitive::Int32 => id::INT32,
        Primitive::Uint32 => id::UINT32,
        Primitive::Int64 => id::INT64,
        Primitive::Uint64 => id::UINT64,
        Primitive::Float32 => id::FLOAT,
        Primitive::Float64 => id::DOUBLE,
    }
}

/// `(type_id, capacity, string_capacity, nested_type_name)` of a field.
fn field_type(f: &Field) -> (u8, usize, usize, String) {
    let (base, string_capacity, nested) = match &f.ty.base {
        BaseType::Primitive(p) => (primitive_id(*p), 0, String::new()),
        BaseType::String { bound: None } => (id::STRING, 0, String::new()),
        BaseType::String { bound: Some(n) } => (id::BOUNDED_STRING, *n, String::new()),
        BaseType::WString { bound: None } => (id::WSTRING, 0, String::new()),
        BaseType::WString { bound: Some(n) } => (id::BOUNDED_WSTRING, *n, String::new()),
        BaseType::Named(t) => (id::NESTED_TYPE, 0, t.to_string()),
    };
    let (offset, capacity) = match f.ty.array {
        Array::None => (0, 0),
        Array::Fixed(n) => (id::ARRAY, n),
        Array::Bounded(n) => (id::BOUNDED_SEQUENCE, n),
        Array::Unbounded => (id::UNBOUNDED_SEQUENCE, 0),
    };
    (base + offset, capacity, string_capacity, nested)
}

/// A JSON string literal as Python's `json.dumps(ensure_ascii=True)` writes it.
fn json_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `{"type_name": ..., "fields": [...]}` for one message.
fn individual(spec: &MessageSpec, out: &mut String) {
    out.push_str("{\"type_name\": ");
    json_str(&spec.name.to_string(), out);
    out.push_str(", \"fields\": [");
    let implicit;
    let fields: &[Field] = if spec.fields.is_empty() {
        // Empty messages get a placeholder member, as in rosidl.
        implicit = [Field {
            name: "structure_needs_at_least_one_member".into(),
            ty: crate::idl::FieldType {
                base: BaseType::Primitive(Primitive::Uint8),
                array: Array::None,
            },
            default: None,
        }];
        &implicit
    } else {
        &spec.fields
    };
    for (i, f) in fields.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        let (type_id, capacity, string_capacity, nested) = field_type(f);
        out.push_str("{\"name\": ");
        json_str(&f.name, out);
        let _ = write!(
            out,
            ", \"type\": {{\"type_id\": {type_id}, \"capacity\": {capacity}, \
             \"string_capacity\": {string_capacity}, \"nested_type_name\": "
        );
        json_str(&nested, out);
        out.push_str("}}");
    }
    out.push_str("]}");
}

/// The exact JSON text that is hashed for `name`.
pub fn hashable_description(interfaces: &Interfaces, name: &TypeName) -> Result<String, TypeName> {
    let spec = interfaces.get(name).ok_or_else(|| name.clone())?;
    let refs = interfaces.references(name)?;
    let mut out = String::from("{\"type_description\": ");
    individual(spec, &mut out);
    out.push_str(", \"referenced_type_descriptions\": [");
    for (i, r) in refs.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        individual(interfaces.get(r).expect("references exist"), &mut out);
    }
    out.push_str("]}");
    Ok(out)
}

/// The `RIHS01_<sha256 hex>` hash of a message. Fails with the name of a
/// referenced message that is not in `interfaces`.
pub fn type_hash(interfaces: &Interfaces, name: &TypeName) -> Result<String, TypeName> {
    let json = hashable_description(interfaces, name)?;
    let digest = Sha256::digest(json.as_bytes());
    let mut out = String::with_capacity(71);
    out.push_str("RIHS01_");
    for b in digest {
        let _ = write!(out, "{b:02x}");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::idl::parse_message;

    #[test]
    fn std_msgs_string_description_and_hash() {
        let mut i = Interfaces::default();
        let name = TypeName::msg("std_msgs", "String");
        i.add(parse_message(name.clone(), "string data\n", "String.msg").unwrap());
        assert_eq!(
            hashable_description(&i, &name).unwrap(),
            r#"{"type_description": {"type_name": "std_msgs/msg/String", "fields": [{"name": "data", "type": {"type_id": 17, "capacity": 0, "string_capacity": 0, "nested_type_name": ""}}]}, "referenced_type_descriptions": []}"#
        );
        // Published by rmw_zenoh's design document and in std_msgs/msg/String.json.
        assert_eq!(
            type_hash(&i, &name).unwrap(),
            "RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18"
        );
    }

    #[test]
    fn missing_reference_is_reported() {
        let mut i = Interfaces::default();
        let name = TypeName::msg("p", "M");
        i.add(parse_message(name.clone(), "q/Other x\n", "M.msg").unwrap());
        assert_eq!(type_hash(&i, &name), Err(TypeName::msg("q", "Other")));
    }

    #[test]
    fn json_escaping_matches_python() {
        let mut s = String::new();
        json_str("a\"b\\\u{e9}", &mut s);
        // Python: json.dumps('a"b\\\u00e9') with ensure_ascii.
        let backslash = '\\';
        assert_eq!(
            s,
            format!("\"a{backslash}\"b{backslash}{backslash}{backslash}u00e9\"")
        );
    }
}
