//! Structural schemas of message encodings.
//!
//! A [`Schema`] describes how a value is laid out on the wire: field names and
//! types, recursively. Most messages use the postcard format, whose layout
//! follows the Rust type exactly; a few (images, point clouds) have hand-written
//! encodings and are described as [`Schema::Custom`].
//!
//! Schemas are used to export message definitions to other languages, to check
//! whether a change to a message breaks existing peers, and to detect when two
//! binaries disagree on a type that shares a name. For the last purpose every
//! schema has a [`hash`](Schema::wire_hash) over its wire layout only: field
//! names and docs do not affect it, because postcard does not encode them.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The wire layout of a value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Schema {
    /// A zero-sized value.
    Unit,
    /// One byte, 0 or 1.
    Bool,
    /// One byte.
    U8,
    /// Varint.
    U16,
    /// Varint.
    U32,
    /// Varint.
    U64,
    /// Varint.
    U128,
    /// One byte, two's complement.
    I8,
    /// Zigzag varint.
    I16,
    /// Zigzag varint.
    I32,
    /// Zigzag varint.
    I64,
    /// Zigzag varint.
    I128,
    /// Four bytes, little endian.
    F32,
    /// Eight bytes, little endian.
    F64,
    /// A Unicode scalar, encoded as a UTF-8 string.
    Char,
    /// Varint length followed by UTF-8 bytes.
    String,
    /// `None` is a zero byte, `Some` a one byte followed by the value.
    Option {
        /// The wrapped value.
        some: Box<Schema>,
    },
    /// Varint length followed by the items.
    Seq {
        /// Item layout.
        item: Box<Schema>,
    },
    /// Exactly `len` items with no length prefix.
    Array {
        /// Item layout.
        item: Box<Schema>,
        /// Number of items.
        len: usize,
    },
    /// Varint length followed by key-value pairs.
    Map {
        /// Key layout.
        key: Box<Schema>,
        /// Value layout.
        value: Box<Schema>,
    },
    /// Items one after another with no prefix.
    Tuple {
        /// Item layouts in order.
        items: Vec<Schema>,
    },
    /// A struct with named fields, encoded as its fields in order.
    Struct {
        /// Rust type name.
        name: String,
        /// Fields in declaration (and wire) order.
        fields: Vec<Field>,
    },
    /// A struct with unnamed fields, encoded as its fields in order. A single
    /// field struct (a newtype) is encoded exactly like its field.
    TupleStruct {
        /// Rust type name.
        name: String,
        /// Field layouts in order.
        items: Vec<Schema>,
    },
    /// An enum, encoded as a varint variant index followed by the variant's fields.
    Enum {
        /// Rust type name.
        name: String,
        /// Variants in declaration order; the index is the position.
        variants: Vec<Variant>,
    },
    /// A hand-written encoding. `encoding` names the format; the fields describe
    /// the logical content and are informative.
    Custom {
        /// Rust type name.
        name: String,
        /// Identifier and short description of the byte layout.
        encoding: String,
        /// Logical content.
        fields: Vec<Field>,
    },
    /// A type whose layout is unknown to the schema system.
    Opaque {
        /// Rust type name.
        name: String,
    },
}

/// A named field of a struct or struct-like enum variant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Field {
    /// Field name.
    pub name: String,
    /// Field layout.
    #[serde(rename = "type")]
    pub ty: Schema,
    /// Documentation from the source, if any.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub doc: String,
}

impl Field {
    /// A field without documentation.
    pub fn new(name: impl Into<String>, ty: Schema) -> Self {
        Self {
            name: name.into(),
            ty,
            doc: String::new(),
        }
    }

    /// Adds documentation.
    pub fn doc(mut self, doc: impl Into<String>) -> Self {
        self.doc = doc.into();
        self
    }
}

/// One variant of an enum.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Variant {
    /// Variant name.
    pub name: String,
    /// Payload shape.
    #[serde(flatten)]
    pub payload: VariantPayload,
}

/// What a variant carries after its index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum VariantPayload {
    /// Nothing.
    Unit,
    /// Unnamed fields in order.
    Tuple {
        /// Field layouts.
        items: Vec<Schema>,
    },
    /// Named fields in order.
    Struct {
        /// Fields.
        fields: Vec<Field>,
    },
}

impl Schema {
    /// A struct schema.
    pub fn structure(name: impl Into<String>, fields: Vec<Field>) -> Self {
        Schema::Struct {
            name: name.into(),
            fields,
        }
    }

    /// A tuple struct schema.
    pub fn tuple_struct(name: impl Into<String>, items: Vec<Schema>) -> Self {
        Schema::TupleStruct {
            name: name.into(),
            items,
        }
    }

    /// A sequence of `item`.
    pub fn seq(item: Schema) -> Self {
        Schema::Seq {
            item: Box::new(item),
        }
    }

    /// A fixed-size array of `item`.
    pub fn array(item: Schema, len: usize) -> Self {
        Schema::Array {
            item: Box::new(item),
            len,
        }
    }

    /// A schema for a type whose layout is unknown.
    pub fn opaque(name: impl Into<String>) -> Self {
        Schema::Opaque { name: name.into() }
    }

    /// Whether the layout is fully known, i.e. no part is [`Schema::Opaque`].
    pub fn is_complete(&self) -> bool {
        let mut complete = true;
        self.visit(&mut |s| {
            if matches!(s, Schema::Opaque { .. }) {
                complete = false;
            }
        });
        complete
    }

    /// The Rust type name for named schemas.
    pub fn type_name(&self) -> Option<&str> {
        match self {
            Schema::Struct { name, .. }
            | Schema::TupleStruct { name, .. }
            | Schema::Enum { name, .. }
            | Schema::Custom { name, .. }
            | Schema::Opaque { name } => Some(name),
            _ => None,
        }
    }

    /// A short human-readable name, e.g. `Vec<f64>` or `[f32; 3]`.
    pub fn display_name(&self) -> String {
        match self {
            Schema::Unit => "()".into(),
            Schema::Bool => "bool".into(),
            Schema::U8 => "u8".into(),
            Schema::U16 => "u16".into(),
            Schema::U32 => "u32".into(),
            Schema::U64 => "u64".into(),
            Schema::U128 => "u128".into(),
            Schema::I8 => "i8".into(),
            Schema::I16 => "i16".into(),
            Schema::I32 => "i32".into(),
            Schema::I64 => "i64".into(),
            Schema::I128 => "i128".into(),
            Schema::F32 => "f32".into(),
            Schema::F64 => "f64".into(),
            Schema::Char => "char".into(),
            Schema::String => "String".into(),
            Schema::Option { some } => format!("Option<{}>", some.display_name()),
            Schema::Seq { item } => format!("Vec<{}>", item.display_name()),
            Schema::Array { item, len } => format!("[{}; {len}]", item.display_name()),
            Schema::Map { key, value } => {
                format!("Map<{}, {}>", key.display_name(), value.display_name())
            }
            Schema::Tuple { items } => {
                let items: Vec<String> = items.iter().map(Schema::display_name).collect();
                format!("({})", items.join(", "))
            }
            Schema::Struct { name, .. }
            | Schema::TupleStruct { name, .. }
            | Schema::Enum { name, .. }
            | Schema::Custom { name, .. }
            | Schema::Opaque { name } => name.clone(),
        }
    }

    /// Calls `f` on this schema and every nested schema, depth first.
    pub fn visit(&self, f: &mut dyn FnMut(&Schema)) {
        f(self);
        match self {
            Schema::Option { some: s }
            | Schema::Seq { item: s }
            | Schema::Array { item: s, .. } => s.visit(f),
            Schema::Map { key, value } => {
                key.visit(f);
                value.visit(f);
            }
            Schema::Tuple { items } | Schema::TupleStruct { items, .. } => {
                items.iter().for_each(|i| i.visit(f))
            }
            Schema::Struct { fields, .. } | Schema::Custom { fields, .. } => {
                fields.iter().for_each(|x| x.ty.visit(f))
            }
            Schema::Enum { variants, .. } => {
                for v in variants {
                    match &v.payload {
                        VariantPayload::Unit => {}
                        VariantPayload::Tuple { items } => items.iter().for_each(|i| i.visit(f)),
                        VariantPayload::Struct { fields } => {
                            fields.iter().for_each(|x| x.ty.visit(f))
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// A canonical string of the wire layout alone: no names, no docs.
    ///
    /// Newtypes are transparent, named and unnamed structs are both plain
    /// tuples, because that is how postcard encodes them. Two schemas with the
    /// same signature encode identically.
    pub fn wire_signature(&self) -> String {
        let mut out = String::new();
        self.write_signature(&mut out);
        out
    }

    fn write_signature(&self, out: &mut String) {
        let list = |out: &mut String, items: &mut dyn Iterator<Item = &Schema>| {
            out.push('(');
            for (i, s) in items.enumerate() {
                if i > 0 {
                    out.push(',');
                }
                s.write_signature(out);
            }
            out.push(')');
        };
        match self {
            Schema::Unit => out.push_str("unit"),
            Schema::Bool => out.push_str("bool"),
            Schema::U8 => out.push_str("u8"),
            Schema::U16 => out.push_str("u16"),
            Schema::U32 => out.push_str("u32"),
            Schema::U64 => out.push_str("u64"),
            Schema::U128 => out.push_str("u128"),
            Schema::I8 => out.push_str("i8"),
            Schema::I16 => out.push_str("i16"),
            Schema::I32 => out.push_str("i32"),
            Schema::I64 => out.push_str("i64"),
            Schema::I128 => out.push_str("i128"),
            Schema::F32 => out.push_str("f32"),
            Schema::F64 => out.push_str("f64"),
            Schema::Char => out.push_str("char"),
            Schema::String => out.push_str("string"),
            Schema::Option { some } => {
                out.push_str("option<");
                some.write_signature(out);
                out.push('>');
            }
            Schema::Seq { item } => {
                out.push_str("seq<");
                item.write_signature(out);
                out.push('>');
            }
            Schema::Array { item, len } => {
                out.push('[');
                item.write_signature(out);
                let _ = write!(out, ";{len}]");
            }
            Schema::Map { key, value } => {
                out.push_str("map<");
                key.write_signature(out);
                out.push(',');
                value.write_signature(out);
                out.push('>');
            }
            Schema::TupleStruct { items, .. } if items.len() == 1 => items[0].write_signature(out),
            Schema::Tuple { items } | Schema::TupleStruct { items, .. } => {
                list(out, &mut items.iter())
            }
            Schema::Struct { fields, .. } => list(out, &mut fields.iter().map(|f| &f.ty)),
            Schema::Enum { variants, .. } => {
                out.push_str("enum{");
                for (i, v) in variants.iter().enumerate() {
                    if i > 0 {
                        out.push('|');
                    }
                    match &v.payload {
                        VariantPayload::Unit => out.push_str("unit"),
                        VariantPayload::Tuple { items } if items.len() == 1 => {
                            items[0].write_signature(out)
                        }
                        VariantPayload::Tuple { items } => list(out, &mut items.iter()),
                        VariantPayload::Struct { fields } => {
                            list(out, &mut fields.iter().map(|f| &f.ty))
                        }
                    }
                }
                out.push('}');
            }
            Schema::Custom { encoding, .. } => {
                let _ = write!(out, "custom:{}", encoding.split(':').next().unwrap_or(""));
            }
            Schema::Opaque { name } => {
                let _ = write!(out, "opaque:{name}");
            }
        }
    }

    /// Hash of the wire layout, e.g. `RS1_3f2a…` (16 hex digits).
    pub fn wire_hash(&self) -> String {
        let digest = Sha256::digest(self.wire_signature().as_bytes());
        let mut out = String::from("RS1_");
        for b in &digest[..8] {
            let _ = write!(out, "{b:02x}");
        }
        out
    }
}

/// The schema of a message type, as exported by tooling.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageSchema {
    /// Message type name, e.g. `std/Pose`.
    pub name: String,
    /// Wire hash, see [`Schema::wire_hash`].
    pub hash: String,
    /// Layout.
    pub schema: Schema,
}

impl MessageSchema {
    /// Bundles a schema with its type name and hash.
    pub fn new(name: impl Into<String>, schema: Schema) -> Self {
        Self {
            name: name.into(),
            hash: schema.wire_hash(),
            schema,
        }
    }
}

/// Types that know their wire layout.
///
/// Implemented for primitives, strings, collections and the standard message
/// vocabulary. `#[derive(Message)]` and `#[derive(Describe)]` implement it for
/// user types.
pub trait Describe {
    /// The layout of `Self`.
    fn describe() -> Schema;
}

macro_rules! prim {
    ($($t:ty => $s:ident),* $(,)?) => {$(
        impl Describe for $t {
            fn describe() -> Schema {
                Schema::$s
            }
        }
    )*};
}

prim!(
    () => Unit, bool => Bool, u8 => U8, u16 => U16, u32 => U32, u64 => U64, u128 => U128,
    i8 => I8, i16 => I16, i32 => I32, i64 => I64, i128 => I128, f32 => F32, f64 => F64,
    char => Char, String => String, str => String, usize => U64, isize => I64,
);

impl<T: Describe> Describe for Option<T> {
    fn describe() -> Schema {
        Schema::Option {
            some: Box::new(T::describe()),
        }
    }
}

impl<T: Describe> Describe for Vec<T> {
    fn describe() -> Schema {
        Schema::seq(T::describe())
    }
}

impl<T: Describe> Describe for [T] {
    fn describe() -> Schema {
        Schema::seq(T::describe())
    }
}

impl<T: Describe, const N: usize> Describe for [T; N] {
    fn describe() -> Schema {
        Schema::array(T::describe(), N)
    }
}

impl<T: Describe + ?Sized> Describe for Box<T> {
    fn describe() -> Schema {
        T::describe()
    }
}

impl<T: Describe + ?Sized> Describe for Arc<T> {
    fn describe() -> Schema {
        T::describe()
    }
}

impl<K: Describe, V: Describe> Describe for BTreeMap<K, V> {
    fn describe() -> Schema {
        Schema::Map {
            key: Box::new(K::describe()),
            value: Box::new(V::describe()),
        }
    }
}

impl<K: Describe, V: Describe, S> Describe for HashMap<K, V, S> {
    fn describe() -> Schema {
        Schema::Map {
            key: Box::new(K::describe()),
            value: Box::new(V::describe()),
        }
    }
}

macro_rules! tuple {
    ($($t:ident),+) => {
        impl<$($t: Describe),+> Describe for ($($t,)+) {
            fn describe() -> Schema {
                Schema::Tuple { items: vec![$($t::describe()),+] }
            }
        }
    };
}

tuple!(A);
tuple!(A, B);
tuple!(A, B, C);
tuple!(A, B, C, D);
tuple!(A, B, C, D, E);
tuple!(A, B, C, D, E, F);

/// A registered message schema, collected from every linked crate.
pub struct SchemaEntry {
    /// Message type name.
    pub ty: &'static str,
    /// Produces the layout.
    pub schema: fn() -> Schema,
}

inventory::collect!(SchemaEntry);

/// Schemas of every message type linked into this binary, keyed by type name.
pub fn registered() -> BTreeMap<&'static str, MessageSchema> {
    inventory::iter::<SchemaEntry>
        .into_iter()
        .map(|e| (e.ty, MessageSchema::new(e.ty, (e.schema)())))
        .collect()
}

/// The schema of one linked message type.
pub fn lookup(ty: &str) -> Option<MessageSchema> {
    inventory::iter::<SchemaEntry>
        .into_iter()
        .find(|e| e.ty == ty)
        .map(|e| MessageSchema::new(e.ty, (e.schema)()))
}

#[doc(hidden)]
pub mod __probe {
    //! Picks a field's schema when its type implements [`Describe`] and falls
    //! back to an opaque schema otherwise, so deriving never fails to compile
    //! because of a field type the schema system does not know.

    use std::marker::PhantomData;

    use super::{Describe, Schema};

    pub struct Probe<T: ?Sized>(PhantomData<T>);

    impl<T: ?Sized> Probe<T> {
        pub const fn new() -> Self {
            Probe(PhantomData)
        }
    }

    impl<T: ?Sized> Default for Probe<T> {
        fn default() -> Self {
            Self::new()
        }
    }

    pub trait ViaDescribe {
        fn schema_of(&self) -> Schema;
    }

    impl<T: Describe + ?Sized> ViaDescribe for &Probe<T> {
        fn schema_of(&self) -> Schema {
            T::describe()
        }
    }

    pub trait ViaFallback {
        fn schema_of(&self) -> Schema;
    }

    impl<T: ?Sized> ViaFallback for Probe<T> {
        fn schema_of(&self) -> Schema {
            Schema::opaque(std::any::type_name::<T>())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitives_and_containers() {
        assert_eq!(Vec::<f64>::describe(), Schema::seq(Schema::F64));
        assert_eq!(<[f32; 3]>::describe(), Schema::array(Schema::F32, 3));
        assert_eq!(
            <Option<String>>::describe().display_name(),
            "Option<String>"
        );
        assert_eq!(<(u8, i32)>::describe().wire_signature(), "(u8,i32)");
    }

    #[test]
    fn signature_ignores_names_and_newtypes() {
        let a = Schema::structure("A", vec![Field::new("x", Schema::F64)]);
        let b = Schema::structure("B", vec![Field::new("renamed", Schema::F64).doc("d")]);
        assert_eq!(a.wire_hash(), b.wire_hash());
        let newtype = Schema::tuple_struct("N", vec![Schema::F64]);
        assert_eq!(newtype.wire_signature(), "f64");
        let c = Schema::structure("A", vec![Field::new("x", Schema::F32)]);
        assert_ne!(a.wire_hash(), c.wire_hash());
    }

    #[test]
    #[allow(clippy::needless_borrow)]
    fn probe_falls_back_to_opaque() {
        use __probe::{Probe, ViaDescribe as _, ViaFallback as _};
        struct Unknown;
        assert_eq!((&&Probe::<u32>::new()).schema_of(), Schema::U32);
        assert!(matches!(
            (&&Probe::<Unknown>::new()).schema_of(),
            Schema::Opaque { .. }
        ));
    }

    #[test]
    fn json_shape_is_stable() {
        let s = Schema::structure("P", vec![Field::new("v", Schema::seq(Schema::U8))]);
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"struct","name":"P","fields":[{"name":"v","type":{"kind":"seq","item":{"kind":"u8"}}}]}"#
        );
        assert_eq!(serde_json::from_str::<Schema>(&json).unwrap(), s);
    }
}
