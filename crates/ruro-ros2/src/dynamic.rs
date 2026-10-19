//! Messages of any type, decoded and encoded at run time from their
//! definitions.
//!
//! [`MessageType::compile`] turns a parsed definition (and the definitions it
//! references) into a plan; [`decode`] reads CDR bytes into a
//! [`MessageValue`] tree following that plan and [`MessageValue::encode`]
//! writes one back. Primitive arrays stay typed vectors, so a large image or
//! point cloud is one `Vec<u8>`, not a node per byte.
//!
//! Decoding keeps what the value tree cannot express, so that re-encoding a
//! decoded message gives back exactly the bytes that were decoded
//! ([`Decoded::encode`]): the encapsulation header (big-endian CDR is read and
//! written as such), bytes after the last field, and padding bytes that are
//! not zero (ROS 2's serializer leaves alignment padding and trailing bytes
//! uninitialised). Two things are normalised: a `bool` byte other than 0 or 1
//! reads as `true` and is written as 1, and a string sent with length 0
//! (instead of 1 and a NUL) is written with length 1. ROS 2 writes neither.
//!
//! A `wstring` is a `u32` count of UTF-16 code units followed by the units,
//! each written as 4 bytes, with no terminator; this is what ROS 2 Jazzy's
//! Fast-CDR serializer puts on the wire.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};

use crate::cdr::CdrError;
use crate::idl::{self, Array, BaseType, Interfaces, Primitive, TypeName};

/// The element type of a field, with nested messages resolved.
#[derive(Debug, Clone, PartialEq)]
pub enum Elem {
    Primitive(Primitive),
    /// `string`, with its bound if any.
    String(Option<usize>),
    /// `wstring`, with its bound if any.
    WString(Option<usize>),
    Message(Arc<MessageType>),
}

impl Elem {
    /// The `.msg` spelling of the element type.
    pub fn name(&self) -> String {
        match self {
            Elem::Primitive(p) => p.name().into(),
            Elem::String(None) => "string".into(),
            Elem::String(Some(n)) => format!("string<={n}"),
            Elem::WString(None) => "wstring".into(),
            Elem::WString(Some(n)) => format!("wstring<={n}"),
            Elem::Message(m) => m.name.to_string(),
        }
    }

    /// Smallest encoded size, to sanity-check sequence lengths.
    fn min_size(&self) -> usize {
        match self {
            Elem::Primitive(p) => p.size(),
            Elem::String(_) | Elem::WString(_) => 4,
            Elem::Message(_) => 1,
        }
    }
}

/// One field of a compiled message type.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldDef {
    pub name: String,
    pub elem: Elem,
    pub array: Array,
    /// The default value declared in the definition, parsed.
    pub default: Option<Value>,
}

impl FieldDef {
    /// The `.msg` spelling of the field type, e.g. `float64[<=3]`.
    pub fn type_name(&self) -> String {
        let e = self.elem.name();
        match self.array {
            Array::None => e,
            Array::Fixed(n) => format!("{e}[{n}]"),
            Array::Bounded(n) => format!("{e}[<={n}]"),
            Array::Unbounded => format!("{e}[]"),
        }
    }
}

/// A message type ready for encoding and decoding.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageType {
    pub name: TypeName,
    /// RIHS01 hash.
    pub hash: String,
    pub fields: Vec<FieldDef>,
    /// Constants declared in the definition: name and value.
    pub constants: Vec<(String, Value)>,
}

impl MessageType {
    /// Compiles `name` and every type it references from `interfaces`.
    pub fn compile(interfaces: &Interfaces, name: &TypeName) -> Result<Arc<MessageType>> {
        compile(interfaces, name, &mut BTreeMap::new())
    }

    /// Index of the field called `name`.
    pub fn field_index(&self, name: &str) -> Option<usize> {
        self.fields.iter().position(|f| f.name == name)
    }

    /// A constant's value.
    pub fn constant(&self, name: &str) -> Option<&Value> {
        self.constants
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v)
    }

    /// The message with every field at its declared default, or zero, empty
    /// or (for fixed arrays) the right number of zero elements.
    pub fn default_value(self: &Arc<Self>) -> MessageValue {
        MessageValue {
            ty: self.clone(),
            fields: self
                .fields
                .iter()
                .map(|f| f.default.clone().unwrap_or_else(|| zero_field(f)))
                .collect(),
        }
    }
}

/// Compiles with a cache of already compiled types.
pub fn compile(
    interfaces: &Interfaces,
    name: &TypeName,
    cache: &mut BTreeMap<TypeName, Arc<MessageType>>,
) -> Result<Arc<MessageType>> {
    if let Some(t) = cache.get(name) {
        return Ok(t.clone());
    }
    let spec = interfaces
        .get(name)
        .ok_or_else(|| anyhow!("unknown message type {name}"))?;
    let hash = crate::hash::type_hash(interfaces, name)
        .map_err(|missing| anyhow!("{name} refers to unknown message type {missing}"))?;
    let mut fields = Vec::with_capacity(spec.fields.len());
    for f in &spec.fields {
        let elem = match &f.ty.base {
            BaseType::Primitive(p) => Elem::Primitive(*p),
            BaseType::String { bound } => Elem::String(*bound),
            BaseType::WString { bound } => Elem::WString(*bound),
            BaseType::Named(t) => {
                if t == name {
                    bail!("{name} contains itself");
                }
                Elem::Message(compile(interfaces, t, cache)?)
            }
        };
        let default = match &f.default {
            Some(text) if !matches!(elem, Elem::Message(_)) => Some(
                parse_literal(&elem, f.ty.array, text)
                    .with_context(|| format!("default of {name}.{}", f.name))?,
            ),
            _ => None,
        };
        fields.push(FieldDef {
            name: f.name.clone(),
            elem,
            array: f.ty.array,
            default,
        });
    }
    let mut constants = Vec::with_capacity(spec.constants.len());
    for c in &spec.constants {
        let elem = if let Some(p) = Primitive::parse(&c.ty) {
            Elem::Primitive(p)
        } else if c.ty == "string" || c.ty.starts_with("string<=") {
            Elem::String(None)
        } else if c.ty == "wstring" || c.ty.starts_with("wstring<=") {
            Elem::WString(None)
        } else {
            bail!("constant {name}.{} has unsupported type {}", c.name, c.ty);
        };
        let v = parse_literal(&elem, Array::None, &c.value)
            .with_context(|| format!("constant {name}.{}", c.name))?;
        constants.push((c.name.clone(), v));
    }
    let t = Arc::new(MessageType {
        name: name.clone(),
        hash,
        fields,
        constants,
    });
    cache.insert(name.clone(), t.clone());
    Ok(t)
}

/// A value of a field.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    Byte(u8),
    Char(u8),
    I8(i8),
    U8(u8),
    I16(i16),
    U16(u16),
    I32(i32),
    U32(u32),
    I64(i64),
    U64(u64),
    F32(f32),
    F64(f64),
    String(String),
    WString(String),
    Array(ArrayValue),
    Message(MessageValue),
}

/// An array or sequence. Primitive elements are stored as typed vectors.
#[derive(Debug, Clone, PartialEq)]
pub enum ArrayValue {
    Bool(Vec<bool>),
    Byte(Vec<u8>),
    Char(Vec<u8>),
    I8(Vec<i8>),
    U8(Vec<u8>),
    I16(Vec<i16>),
    U16(Vec<u16>),
    I32(Vec<i32>),
    U32(Vec<u32>),
    I64(Vec<i64>),
    U64(Vec<u64>),
    F32(Vec<f32>),
    F64(Vec<f64>),
    String(Vec<String>),
    WString(Vec<String>),
    Message(Vec<MessageValue>),
}

/// A message: its type and one value per field, in declaration order.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageValue {
    pub ty: Arc<MessageType>,
    pub fields: Vec<Value>,
}

/// Applies `$body` to the vector inside an [`ArrayValue`], whatever its
/// element type.
macro_rules! each_array {
    ($a:expr, $v:ident => $body:expr) => {
        match $a {
            ArrayValue::Bool($v) => $body,
            ArrayValue::Byte($v) => $body,
            ArrayValue::Char($v) => $body,
            ArrayValue::I8($v) => $body,
            ArrayValue::U8($v) => $body,
            ArrayValue::I16($v) => $body,
            ArrayValue::U16($v) => $body,
            ArrayValue::I32($v) => $body,
            ArrayValue::U32($v) => $body,
            ArrayValue::I64($v) => $body,
            ArrayValue::U64($v) => $body,
            ArrayValue::F32($v) => $body,
            ArrayValue::F64($v) => $body,
            ArrayValue::String($v) => $body,
            ArrayValue::WString($v) => $body,
            ArrayValue::Message($v) => $body,
        }
    };
}

impl ArrayValue {
    /// Number of elements.
    pub fn len(&self) -> usize {
        each_array!(self, v => v.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Element `i` as a value.
    pub fn get(&self, i: usize) -> Option<Value> {
        Some(match self {
            ArrayValue::Bool(v) => Value::Bool(*v.get(i)?),
            ArrayValue::Byte(v) => Value::Byte(*v.get(i)?),
            ArrayValue::Char(v) => Value::Char(*v.get(i)?),
            ArrayValue::I8(v) => Value::I8(*v.get(i)?),
            ArrayValue::U8(v) => Value::U8(*v.get(i)?),
            ArrayValue::I16(v) => Value::I16(*v.get(i)?),
            ArrayValue::U16(v) => Value::U16(*v.get(i)?),
            ArrayValue::I32(v) => Value::I32(*v.get(i)?),
            ArrayValue::U32(v) => Value::U32(*v.get(i)?),
            ArrayValue::I64(v) => Value::I64(*v.get(i)?),
            ArrayValue::U64(v) => Value::U64(*v.get(i)?),
            ArrayValue::F32(v) => Value::F32(*v.get(i)?),
            ArrayValue::F64(v) => Value::F64(*v.get(i)?),
            ArrayValue::String(v) => Value::String(v.get(i)?.clone()),
            ArrayValue::WString(v) => Value::WString(v.get(i)?.clone()),
            ArrayValue::Message(v) => Value::Message(v.get(i)?.clone()),
        })
    }

    /// An empty array for elements of type `elem`.
    pub fn empty(elem: &Elem) -> Self {
        match elem {
            Elem::Primitive(p) => match p {
                Primitive::Bool => ArrayValue::Bool(Vec::new()),
                Primitive::Byte => ArrayValue::Byte(Vec::new()),
                Primitive::Char => ArrayValue::Char(Vec::new()),
                Primitive::Int8 => ArrayValue::I8(Vec::new()),
                Primitive::Uint8 => ArrayValue::U8(Vec::new()),
                Primitive::Int16 => ArrayValue::I16(Vec::new()),
                Primitive::Uint16 => ArrayValue::U16(Vec::new()),
                Primitive::Int32 => ArrayValue::I32(Vec::new()),
                Primitive::Uint32 => ArrayValue::U32(Vec::new()),
                Primitive::Int64 => ArrayValue::I64(Vec::new()),
                Primitive::Uint64 => ArrayValue::U64(Vec::new()),
                Primitive::Float32 => ArrayValue::F32(Vec::new()),
                Primitive::Float64 => ArrayValue::F64(Vec::new()),
            },
            Elem::String(_) => ArrayValue::String(Vec::new()),
            Elem::WString(_) => ArrayValue::WString(Vec::new()),
            Elem::Message(_) => ArrayValue::Message(Vec::new()),
        }
    }

    /// Appends a value, which must have the array's element type.
    pub fn push(&mut self, v: Value) -> Result<()> {
        match (self, v) {
            (ArrayValue::Bool(a), Value::Bool(x)) => a.push(x),
            (ArrayValue::Byte(a), Value::Byte(x)) => a.push(x),
            (ArrayValue::Char(a), Value::Char(x)) => a.push(x),
            (ArrayValue::I8(a), Value::I8(x)) => a.push(x),
            (ArrayValue::U8(a), Value::U8(x)) => a.push(x),
            (ArrayValue::I16(a), Value::I16(x)) => a.push(x),
            (ArrayValue::U16(a), Value::U16(x)) => a.push(x),
            (ArrayValue::I32(a), Value::I32(x)) => a.push(x),
            (ArrayValue::U32(a), Value::U32(x)) => a.push(x),
            (ArrayValue::I64(a), Value::I64(x)) => a.push(x),
            (ArrayValue::U64(a), Value::U64(x)) => a.push(x),
            (ArrayValue::F32(a), Value::F32(x)) => a.push(x),
            (ArrayValue::F64(a), Value::F64(x)) => a.push(x),
            (ArrayValue::String(a), Value::String(x)) => a.push(x),
            (ArrayValue::WString(a), Value::WString(x)) => a.push(x),
            (ArrayValue::Message(a), Value::Message(x)) => a.push(x),
            (_, v) => bail!("array element of the wrong type: {}", v.kind()),
        }
        Ok(())
    }
}

impl Value {
    /// The value as a number, for numeric and boolean values.
    pub fn as_f64(&self) -> Option<f64> {
        Some(match *self {
            Value::Bool(b) => f64::from(u8::from(b)),
            Value::Byte(v) | Value::Char(v) | Value::U8(v) => f64::from(v),
            Value::I8(v) => f64::from(v),
            Value::I16(v) => f64::from(v),
            Value::U16(v) => f64::from(v),
            Value::I32(v) => f64::from(v),
            Value::U32(v) => f64::from(v),
            Value::I64(v) => v as f64,
            Value::U64(v) => v as f64,
            Value::F32(v) => f64::from(v),
            Value::F64(v) => v,
            _ => return None,
        })
    }

    /// The text of a string value.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) | Value::WString(s) => Some(s),
            _ => None,
        }
    }

    /// A short name of the value's kind, for error messages.
    pub fn kind(&self) -> &'static str {
        match self {
            Value::Bool(_) => "bool",
            Value::Byte(_) => "byte",
            Value::Char(_) => "char",
            Value::I8(_) => "int8",
            Value::U8(_) => "uint8",
            Value::I16(_) => "int16",
            Value::U16(_) => "uint16",
            Value::I32(_) => "int32",
            Value::U32(_) => "uint32",
            Value::I64(_) => "int64",
            Value::U64(_) => "uint64",
            Value::F32(_) => "float32",
            Value::F64(_) => "float64",
            Value::String(_) => "string",
            Value::WString(_) => "wstring",
            Value::Array(_) => "array",
            Value::Message(_) => "message",
        }
    }

    /// The value as JSON: bytes and chars as numbers, arrays as lists,
    /// messages as objects in field order. Non-finite floats become `null`.
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::Value as J;
        let num = |f: f64| serde_json::Number::from_f64(f).map_or(J::Null, J::Number);
        match self {
            Value::Bool(b) => J::Bool(*b),
            Value::Byte(v) | Value::Char(v) | Value::U8(v) => (*v).into(),
            Value::I8(v) => (*v).into(),
            Value::I16(v) => (*v).into(),
            Value::U16(v) => (*v).into(),
            Value::I32(v) => (*v).into(),
            Value::U32(v) => (*v).into(),
            Value::I64(v) => (*v).into(),
            Value::U64(v) => (*v).into(),
            Value::F32(v) => num(f64::from(*v)),
            Value::F64(v) => num(*v),
            Value::String(s) | Value::WString(s) => J::String(s.clone()),
            Value::Message(m) => m.to_json(),
            Value::Array(a) => match a {
                ArrayValue::Bool(v) => J::Array(v.iter().map(|b| J::Bool(*b)).collect()),
                ArrayValue::Byte(v) | ArrayValue::Char(v) | ArrayValue::U8(v) => {
                    J::Array(v.iter().map(|x| (*x).into()).collect())
                }
                ArrayValue::I8(v) => J::Array(v.iter().map(|x| (*x).into()).collect()),
                ArrayValue::I16(v) => J::Array(v.iter().map(|x| (*x).into()).collect()),
                ArrayValue::U16(v) => J::Array(v.iter().map(|x| (*x).into()).collect()),
                ArrayValue::I32(v) => J::Array(v.iter().map(|x| (*x).into()).collect()),
                ArrayValue::U32(v) => J::Array(v.iter().map(|x| (*x).into()).collect()),
                ArrayValue::I64(v) => J::Array(v.iter().map(|x| (*x).into()).collect()),
                ArrayValue::U64(v) => J::Array(v.iter().map(|x| (*x).into()).collect()),
                ArrayValue::F32(v) => J::Array(v.iter().map(|x| num(f64::from(*x))).collect()),
                ArrayValue::F64(v) => J::Array(v.iter().map(|x| num(*x)).collect()),
                ArrayValue::String(v) | ArrayValue::WString(v) => {
                    J::Array(v.iter().map(|s| J::String(s.clone())).collect())
                }
                ArrayValue::Message(v) => J::Array(v.iter().map(MessageValue::to_json).collect()),
            },
        }
    }
}

fn zero(elem: &Elem) -> Value {
    match elem {
        Elem::Primitive(p) => match p {
            Primitive::Bool => Value::Bool(false),
            Primitive::Byte => Value::Byte(0),
            Primitive::Char => Value::Char(0),
            Primitive::Int8 => Value::I8(0),
            Primitive::Uint8 => Value::U8(0),
            Primitive::Int16 => Value::I16(0),
            Primitive::Uint16 => Value::U16(0),
            Primitive::Int32 => Value::I32(0),
            Primitive::Uint32 => Value::U32(0),
            Primitive::Int64 => Value::I64(0),
            Primitive::Uint64 => Value::U64(0),
            Primitive::Float32 => Value::F32(0.0),
            Primitive::Float64 => Value::F64(0.0),
        },
        Elem::String(_) => Value::String(String::new()),
        Elem::WString(_) => Value::WString(String::new()),
        Elem::Message(m) => Value::Message(m.default_value()),
    }
}

fn zero_field(f: &FieldDef) -> Value {
    match f.array {
        Array::None => zero(&f.elem),
        Array::Fixed(n) => {
            let mut a = ArrayValue::empty(&f.elem);
            for _ in 0..n {
                a.push(zero(&f.elem)).expect("same element type");
            }
            Value::Array(a)
        }
        Array::Bounded(_) | Array::Unbounded => Value::Array(ArrayValue::empty(&f.elem)),
    }
}

impl MessageValue {
    /// A field's value by name.
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.ty.field_index(name).map(|i| &self.fields[i])
    }

    /// Replaces a field's value. The value is checked when encoding.
    pub fn set(&mut self, name: &str, v: Value) -> Result<()> {
        let i = self
            .ty
            .field_index(name)
            .ok_or_else(|| anyhow!("{} has no field `{name}`", self.ty.name))?;
        self.fields[i] = v;
        Ok(())
    }

    /// The message as a JSON object (see [`Value::to_json`]).
    pub fn to_json(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        for (f, v) in self.ty.fields.iter().zip(&self.fields) {
            map.insert(f.name.clone(), v.to_json());
        }
        serde_json::Value::Object(map)
    }

    /// Builds a message from JSON in the form [`to_json`](Self::to_json)
    /// writes. Missing fields take their default value.
    pub fn from_json(ty: &Arc<MessageType>, json: &serde_json::Value) -> Result<Self> {
        let mut m = ty.default_value();
        let Some(obj) = json.as_object() else {
            bail!("expected an object for {}", ty.name);
        };
        for (k, v) in obj {
            let i = ty
                .field_index(k)
                .ok_or_else(|| anyhow!("{} has no field `{k}`", ty.name))?;
            let f = &ty.fields[i];
            m.fields[i] = field_from_json(f, v).with_context(|| format!("field `{k}`"))?;
        }
        Ok(m)
    }

    /// Encodes as little-endian CDR, with zero padding.
    pub fn encode(&self) -> Result<Vec<u8>, CdrError> {
        let mut w = Writer::new(crate::cdr::CDR_LE, &[]);
        write_message(&mut w, self)?;
        Ok(w.buf)
    }

    /// Renders the message as YAML, as `ros2 topic echo` does. Arrays longer
    /// than `max_array` elements are cut short with a note of their length.
    pub fn to_yaml(&self, max_array: Option<usize>) -> String {
        let mut out = String::new();
        yaml_message(self, 0, max_array, &mut out);
        out
    }
}

fn scalar_from_json(elem: &Elem, v: &serde_json::Value) -> Result<Value> {
    let int = |lo: i128, hi: i128| -> Result<i128> {
        let n = v
            .as_i64()
            .map(i128::from)
            .or_else(|| v.as_u64().map(i128::from))
            .or_else(|| {
                v.as_f64()
                    .filter(|f| f.fract() == 0.0 && f.is_finite())
                    .map(|f| f as i128)
            })
            .ok_or_else(|| anyhow!("expected an integer, got {v}"))?;
        if !(lo..=hi).contains(&n) {
            bail!("{n} is out of range");
        }
        Ok(n)
    };
    let float = || {
        v.as_f64()
            .ok_or_else(|| anyhow!("expected a number, got {v}"))
    };
    Ok(match elem {
        Elem::Primitive(p) => match p {
            Primitive::Bool => Value::Bool(match v {
                serde_json::Value::Bool(b) => *b,
                _ => int(0, 1)? == 1,
            }),
            Primitive::Byte => Value::Byte(int(0, 255)? as u8),
            Primitive::Char => Value::Char(match v.as_str() {
                Some(s) if s.len() == 1 => s.as_bytes()[0],
                _ => int(0, 255)? as u8,
            }),
            Primitive::Int8 => Value::I8(int(i8::MIN.into(), i8::MAX.into())? as i8),
            Primitive::Uint8 => Value::U8(int(0, u8::MAX.into())? as u8),
            Primitive::Int16 => Value::I16(int(i16::MIN.into(), i16::MAX.into())? as i16),
            Primitive::Uint16 => Value::U16(int(0, u16::MAX.into())? as u16),
            Primitive::Int32 => Value::I32(int(i32::MIN.into(), i32::MAX.into())? as i32),
            Primitive::Uint32 => Value::U32(int(0, u32::MAX.into())? as u32),
            Primitive::Int64 => Value::I64(int(i64::MIN.into(), i64::MAX.into())? as i64),
            Primitive::Uint64 => Value::U64(int(0, u64::MAX.into())? as u64),
            Primitive::Float32 => Value::F32(float()? as f32),
            Primitive::Float64 => Value::F64(float()?),
        },
        Elem::String(_) | Elem::WString(_) => {
            let s = v
                .as_str()
                .ok_or_else(|| anyhow!("expected a string, got {v}"))?
                .to_owned();
            if matches!(elem, Elem::String(_)) {
                Value::String(s)
            } else {
                Value::WString(s)
            }
        }
        Elem::Message(m) => Value::Message(MessageValue::from_json(m, v)?),
    })
}

fn field_from_json(f: &FieldDef, v: &serde_json::Value) -> Result<Value> {
    if f.array == Array::None {
        return scalar_from_json(&f.elem, v);
    }
    let mut a = ArrayValue::empty(&f.elem);
    match v {
        // Byte arrays may be given as a string.
        serde_json::Value::String(s)
            if matches!(
                f.elem,
                Elem::Primitive(Primitive::Byte | Primitive::Uint8 | Primitive::Char)
            ) =>
        {
            for b in s.bytes() {
                a.push(scalar_from_json(&f.elem, &b.into())?)?;
            }
        }
        serde_json::Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                a.push(scalar_from_json(&f.elem, item).with_context(|| format!("element {i}"))?)?;
            }
        }
        other => bail!("expected a list, got {other}"),
    }
    Ok(Value::Array(a))
}

// ---------------------------------------------------------------------------
// Default values and constants

/// Splits the inside of `[a, "b, c", d]` at top-level commas.
fn split_list(s: &str) -> Result<Vec<String>> {
    let inner = s
        .trim()
        .strip_prefix('[')
        .and_then(|r| r.strip_suffix(']'))
        .ok_or_else(|| anyhow!("expected a list in brackets, got `{s}`"))?;
    let mut items = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in inner.chars() {
        if let Some(q) = quote {
            cur.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => {
                quote = Some(c);
                cur.push(c);
            }
            ',' => items.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    if quote.is_some() {
        bail!("unterminated quote in `{s}`");
    }
    if !cur.trim().is_empty() || !items.is_empty() {
        items.push(cur);
    }
    Ok(items.into_iter().map(|i| i.trim().to_owned()).collect())
}

/// A string literal: quoted with `'` or `"` (backslash escapes the quote and
/// itself), or the bare text.
fn parse_string_literal(s: &str) -> Result<String> {
    let s = s.trim();
    let Some(q) = s.chars().next().filter(|c| *c == '"' || *c == '\'') else {
        return Ok(s.to_owned());
    };
    let inner = s[1..]
        .strip_suffix(q)
        .ok_or_else(|| anyhow!("unterminated string `{s}`"))?;
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(n) if n == q || n == '\\' => out.push(n),
                Some(n) => {
                    out.push('\\');
                    out.push(n);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    Ok(out)
}

fn parse_scalar_literal(elem: &Elem, s: &str) -> Result<Value> {
    let s = s.trim();
    let int = |s: &str| -> Result<i128> {
        let (neg, digits) = match s.strip_prefix('-') {
            Some(d) => (true, d),
            None => (false, s.strip_prefix('+').unwrap_or(s)),
        };
        let n = if let Some(h) = digits
            .strip_prefix("0x")
            .or_else(|| digits.strip_prefix("0X"))
        {
            i128::from_str_radix(h, 16)
        } else {
            digits.parse()
        }
        .map_err(|_| anyhow!("bad integer `{s}`"))?;
        Ok(if neg { -n } else { n })
    };
    let ranged = |lo: i128, hi: i128| -> Result<i128> {
        let n = int(s)?;
        if !(lo..=hi).contains(&n) {
            bail!("{n} is out of range");
        }
        Ok(n)
    };
    Ok(match elem {
        Elem::Primitive(p) => match p {
            Primitive::Bool => Value::Bool(match s.to_ascii_lowercase().as_str() {
                "true" | "1" => true,
                "false" | "0" => false,
                _ => bail!("bad bool `{s}`"),
            }),
            Primitive::Byte => Value::Byte(ranged(0, 255)? as u8),
            Primitive::Char => Value::Char(ranged(0, 255)? as u8),
            Primitive::Int8 => Value::I8(ranged(i8::MIN.into(), i8::MAX.into())? as i8),
            Primitive::Uint8 => Value::U8(ranged(0, u8::MAX.into())? as u8),
            Primitive::Int16 => Value::I16(ranged(i16::MIN.into(), i16::MAX.into())? as i16),
            Primitive::Uint16 => Value::U16(ranged(0, u16::MAX.into())? as u16),
            Primitive::Int32 => Value::I32(ranged(i32::MIN.into(), i32::MAX.into())? as i32),
            Primitive::Uint32 => Value::U32(ranged(0, u32::MAX.into())? as u32),
            Primitive::Int64 => Value::I64(ranged(i64::MIN.into(), i64::MAX.into())? as i64),
            Primitive::Uint64 => Value::U64(ranged(0, u64::MAX.into())? as u64),
            Primitive::Float32 => Value::F32(s.parse().map_err(|_| anyhow!("bad float `{s}`"))?),
            Primitive::Float64 => Value::F64(s.parse().map_err(|_| anyhow!("bad float `{s}`"))?),
        },
        Elem::String(_) => Value::String(parse_string_literal(s)?),
        Elem::WString(_) => Value::WString(parse_string_literal(s)?),
        Elem::Message(m) => bail!("{} cannot have a literal value", m.name),
    })
}

/// Parses a default or constant value as written in a `.msg` file.
pub fn parse_literal(elem: &Elem, array: Array, s: &str) -> Result<Value> {
    if array == Array::None {
        return parse_scalar_literal(elem, s);
    }
    let mut a = ArrayValue::empty(elem);
    for item in split_list(s)? {
        a.push(parse_scalar_literal(elem, &item)?)?;
    }
    let n = a.len();
    match array {
        Array::Fixed(len) if n != len => bail!("expected {len} values, got {n}"),
        Array::Bounded(max) if n > max => bail!("at most {max} values allowed, got {n}"),
        _ => {}
    }
    Ok(Value::Array(a))
}

// ---------------------------------------------------------------------------
// Decoding

/// A decoded message plus what is needed to re-encode it byte for byte.
#[derive(Debug, Clone, PartialEq)]
pub struct Decoded {
    pub value: MessageValue,
    /// Encapsulation header as received.
    pub header: [u8; 4],
    /// Non-zero padding bytes: offset in the message and value.
    pub padding: Vec<(u32, u8)>,
    /// Bytes after the last field.
    pub tail: Vec<u8>,
}

impl Decoded {
    /// Re-encodes the message; for an unchanged value this gives back the
    /// decoded bytes exactly.
    pub fn encode(&self) -> Result<Vec<u8>, CdrError> {
        let mut w = Writer::new(self.header, &self.padding);
        write_message(&mut w, &self.value)?;
        w.buf.extend_from_slice(&self.tail);
        Ok(w.buf)
    }
}

/// Decodes CDR bytes (encapsulation header included) as a message of type `ty`.
pub fn decode(ty: &Arc<MessageType>, bytes: &[u8]) -> Result<Decoded, CdrError> {
    let mut r = Reader::new(bytes)?;
    let value = read_message(&mut r, ty)?;
    Ok(Decoded {
        value,
        header: bytes[..4].try_into().expect("checked length"),
        padding: r.padding,
        tail: bytes[r.pos..].to_vec(),
    })
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    big: bool,
    padding: Vec<(u32, u8)>,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Result<Self, CdrError> {
        if data.len() < 4 {
            return Err(CdrError::Truncated(data.len()));
        }
        let header: [u8; 4] = data[..4].try_into().unwrap();
        let big = match (header[0], header[1]) {
            (0, 0) => true,
            (0, 1) => false,
            _ => return Err(CdrError::Encapsulation(header)),
        };
        Ok(Self {
            data,
            pos: 4,
            big,
            padding: Vec::new(),
        })
    }

    fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    /// Skips padding, remembering bytes that are not zero.
    fn skip_padding(&mut self, n: usize) -> Result<(), CdrError> {
        if self.remaining() < n {
            return Err(CdrError::Truncated(self.pos));
        }
        for i in self.pos..self.pos + n {
            if self.data[i] != 0 {
                self.padding.push((i as u32, self.data[i]));
            }
        }
        self.pos += n;
        Ok(())
    }

    fn align(&mut self, n: usize) -> Result<(), CdrError> {
        let pad = (n - (self.pos - 4) % n) % n;
        self.skip_padding(pad)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], CdrError> {
        if self.remaining() < n {
            return Err(CdrError::Truncated(self.pos));
        }
        let b = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(b)
    }

    fn fixed<const N: usize>(&mut self) -> Result<[u8; N], CdrError> {
        self.align(N)?;
        let mut a: [u8; N] = self.take(N)?.try_into().unwrap();
        if self.big {
            a.reverse();
        }
        Ok(a)
    }

    fn u32(&mut self) -> Result<u32, CdrError> {
        Ok(u32::from_le_bytes(self.fixed()?))
    }

    /// A sequence length, checked against the bytes left.
    fn len(&mut self, min_elem: usize) -> Result<usize, CdrError> {
        let n = self.u32()? as usize;
        if n.saturating_mul(min_elem) > self.remaining() {
            return Err(CdrError::BadLength(n, self.remaining()));
        }
        Ok(n)
    }

    fn string(&mut self) -> Result<String, CdrError> {
        let n = self.len(1)?;
        if n == 0 {
            return Ok(String::new());
        }
        let b = self.take(n)?;
        let (text, nul) = b.split_at(n - 1);
        if nul != [0] {
            return Err(CdrError::BadString);
        }
        String::from_utf8(text.to_vec()).map_err(|_| CdrError::BadString)
    }

    fn wstring(&mut self) -> Result<String, CdrError> {
        let n = self.len(4)?;
        let mut units = Vec::with_capacity(n);
        for _ in 0..n {
            let u = self.u32()?;
            units
                .push(u16::try_from(u).map_err(|_| {
                    CdrError::Invalid(format!("wstring unit {u:#x} is not UTF-16"))
                })?);
        }
        String::from_utf16(&units)
            .map_err(|_| CdrError::Invalid("wstring is not valid UTF-16".into()))
    }

    fn primitive(&mut self, p: Primitive) -> Result<Value, CdrError> {
        Ok(match p {
            Primitive::Bool => Value::Bool(self.take(1)?[0] != 0),
            Primitive::Byte => Value::Byte(self.take(1)?[0]),
            Primitive::Char => Value::Char(self.take(1)?[0]),
            Primitive::Int8 => Value::I8(self.take(1)?[0] as i8),
            Primitive::Uint8 => Value::U8(self.take(1)?[0]),
            Primitive::Int16 => Value::I16(i16::from_le_bytes(self.fixed()?)),
            Primitive::Uint16 => Value::U16(u16::from_le_bytes(self.fixed()?)),
            Primitive::Int32 => Value::I32(i32::from_le_bytes(self.fixed()?)),
            Primitive::Uint32 => Value::U32(u32::from_le_bytes(self.fixed()?)),
            Primitive::Int64 => Value::I64(i64::from_le_bytes(self.fixed()?)),
            Primitive::Uint64 => Value::U64(u64::from_le_bytes(self.fixed()?)),
            Primitive::Float32 => Value::F32(f32::from_le_bytes(self.fixed()?)),
            Primitive::Float64 => Value::F64(f64::from_le_bytes(self.fixed()?)),
        })
    }
}

/// Reads `n` primitives of a fixed size into a typed vector.
macro_rules! read_vec {
    ($r:expr, $n:expr, $t:ty) => {{
        let mut v: Vec<$t> = Vec::with_capacity($n);
        for _ in 0..$n {
            v.push(<$t>::from_le_bytes($r.fixed()?));
        }
        v
    }};
}

fn read_array(r: &mut Reader<'_>, elem: &Elem, n: usize) -> Result<ArrayValue, CdrError> {
    Ok(match elem {
        Elem::Primitive(p) => match p {
            Primitive::Bool => ArrayValue::Bool(r.take(n)?.iter().map(|b| *b != 0).collect()),
            Primitive::Byte => ArrayValue::Byte(r.take(n)?.to_vec()),
            Primitive::Char => ArrayValue::Char(r.take(n)?.to_vec()),
            Primitive::Uint8 => ArrayValue::U8(r.take(n)?.to_vec()),
            Primitive::Int8 => ArrayValue::I8(r.take(n)?.iter().map(|b| *b as i8).collect()),
            Primitive::Int16 => ArrayValue::I16(read_vec!(r, n, i16)),
            Primitive::Uint16 => ArrayValue::U16(read_vec!(r, n, u16)),
            Primitive::Int32 => ArrayValue::I32(read_vec!(r, n, i32)),
            Primitive::Uint32 => ArrayValue::U32(read_vec!(r, n, u32)),
            Primitive::Int64 => ArrayValue::I64(read_vec!(r, n, i64)),
            Primitive::Uint64 => ArrayValue::U64(read_vec!(r, n, u64)),
            Primitive::Float32 => ArrayValue::F32(read_vec!(r, n, f32)),
            Primitive::Float64 => ArrayValue::F64(read_vec!(r, n, f64)),
        },
        Elem::String(_) => {
            let mut v = Vec::with_capacity(n.min(r.remaining()));
            for _ in 0..n {
                v.push(r.string()?);
            }
            ArrayValue::String(v)
        }
        Elem::WString(_) => {
            let mut v = Vec::with_capacity(n.min(r.remaining()));
            for _ in 0..n {
                v.push(r.wstring()?);
            }
            ArrayValue::WString(v)
        }
        Elem::Message(m) => {
            let mut v = Vec::with_capacity(n.min(r.remaining()));
            for _ in 0..n {
                v.push(read_message(r, m)?);
            }
            ArrayValue::Message(v)
        }
    })
}

fn read_message(r: &mut Reader<'_>, ty: &Arc<MessageType>) -> Result<MessageValue, CdrError> {
    if ty.fields.is_empty() {
        // Empty messages carry one placeholder byte.
        r.skip_padding(1)?;
        return Ok(MessageValue {
            ty: ty.clone(),
            fields: Vec::new(),
        });
    }
    let mut fields = Vec::with_capacity(ty.fields.len());
    for f in &ty.fields {
        let v = match f.array {
            Array::None => match &f.elem {
                Elem::Primitive(p) => r.primitive(*p)?,
                Elem::String(_) => Value::String(r.string()?),
                Elem::WString(_) => Value::WString(r.wstring()?),
                Elem::Message(m) => Value::Message(read_message(r, m)?),
            },
            Array::Fixed(n) => {
                if n.saturating_mul(f.elem.min_size()) > r.remaining() {
                    return Err(CdrError::BadLength(n, r.remaining()));
                }
                Value::Array(read_array(r, &f.elem, n)?)
            }
            Array::Bounded(_) | Array::Unbounded => {
                let n = r.len(f.elem.min_size())?;
                Value::Array(read_array(r, &f.elem, n)?)
            }
        };
        fields.push(v);
    }
    Ok(MessageValue {
        ty: ty.clone(),
        fields,
    })
}

// ---------------------------------------------------------------------------
// Encoding

struct Writer<'p> {
    buf: Vec<u8>,
    big: bool,
    /// Padding bytes to restore, sorted by offset.
    patches: &'p [(u32, u8)],
    next: usize,
}

impl<'p> Writer<'p> {
    fn new(header: [u8; 4], patches: &'p [(u32, u8)]) -> Self {
        let mut buf = Vec::with_capacity(64);
        buf.extend_from_slice(&header);
        Self {
            buf,
            big: header[1] == 0,
            patches,
            next: 0,
        }
    }

    /// Writes one padding byte: zero, or the byte decoded at this offset.
    fn pad(&mut self) {
        let off = self.buf.len() as u32;
        while self.next < self.patches.len() && self.patches[self.next].0 < off {
            self.next += 1;
        }
        let b = match self.patches.get(self.next) {
            Some((o, b)) if *o == off => {
                self.next += 1;
                *b
            }
            _ => 0,
        };
        self.buf.push(b);
    }

    fn align(&mut self, n: usize) {
        let pad = (n - (self.buf.len() - 4) % n) % n;
        for _ in 0..pad {
            self.pad();
        }
    }

    fn fixed<const N: usize>(&mut self, mut le: [u8; N]) {
        self.align(N);
        if self.big {
            le.reverse();
        }
        self.buf.extend_from_slice(&le);
    }

    fn u32(&mut self, v: u32) {
        self.fixed(v.to_le_bytes());
    }

    fn len(&mut self, n: usize) -> Result<(), CdrError> {
        let n = u32::try_from(n).map_err(|_| CdrError::Invalid("sequence too long".into()))?;
        self.u32(n);
        Ok(())
    }

    fn string(&mut self, s: &str, bound: Option<usize>) -> Result<(), CdrError> {
        if bound.is_some_and(|b| s.len() > b) {
            return Err(CdrError::Invalid(format!(
                "string of {} bytes exceeds its bound {}",
                s.len(),
                bound.unwrap()
            )));
        }
        if s.as_bytes().contains(&0) {
            return Err(CdrError::Invalid("string contains a NUL byte".into()));
        }
        self.len(s.len() + 1)?;
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
        Ok(())
    }

    fn wstring(&mut self, s: &str, bound: Option<usize>) -> Result<(), CdrError> {
        let units: Vec<u16> = s.encode_utf16().collect();
        if bound.is_some_and(|b| units.len() > b) {
            return Err(CdrError::Invalid(format!(
                "wstring of {} characters exceeds its bound {}",
                units.len(),
                bound.unwrap()
            )));
        }
        self.len(units.len())?;
        for u in units {
            self.u32(u32::from(u));
        }
        Ok(())
    }
}

fn mismatch(f: &FieldDef, v: &Value) -> CdrError {
    CdrError::Invalid(format!(
        "field `{}` is {} but got {}",
        f.name,
        f.type_name(),
        v.kind()
    ))
}

fn write_scalar(w: &mut Writer<'_>, f: &FieldDef, elem: &Elem, v: &Value) -> Result<(), CdrError> {
    match (elem, v) {
        (Elem::Primitive(p), v) => match (p, v) {
            (Primitive::Bool, Value::Bool(b)) => w.buf.push(u8::from(*b)),
            (Primitive::Byte, Value::Byte(b))
            | (Primitive::Char, Value::Char(b))
            | (Primitive::Uint8, Value::U8(b)) => w.buf.push(*b),
            (Primitive::Int8, Value::I8(b)) => w.buf.push(*b as u8),
            (Primitive::Int16, Value::I16(x)) => w.fixed(x.to_le_bytes()),
            (Primitive::Uint16, Value::U16(x)) => w.fixed(x.to_le_bytes()),
            (Primitive::Int32, Value::I32(x)) => w.fixed(x.to_le_bytes()),
            (Primitive::Uint32, Value::U32(x)) => w.fixed(x.to_le_bytes()),
            (Primitive::Int64, Value::I64(x)) => w.fixed(x.to_le_bytes()),
            (Primitive::Uint64, Value::U64(x)) => w.fixed(x.to_le_bytes()),
            (Primitive::Float32, Value::F32(x)) => w.fixed(x.to_le_bytes()),
            (Primitive::Float64, Value::F64(x)) => w.fixed(x.to_le_bytes()),
            _ => return Err(mismatch(f, v)),
        },
        (Elem::String(bound), Value::String(s)) => w.string(s, *bound)?,
        (Elem::WString(bound), Value::WString(s)) => w.wstring(s, *bound)?,
        (Elem::Message(m), Value::Message(mv)) => write_nested(w, f, m, mv)?,
        _ => return Err(mismatch(f, v)),
    }
    Ok(())
}

fn write_nested(
    w: &mut Writer<'_>,
    f: &FieldDef,
    ty: &MessageType,
    m: &MessageValue,
) -> Result<(), CdrError> {
    if m.ty.name != ty.name {
        return Err(CdrError::Invalid(format!(
            "field `{}` is {} but got {}",
            f.name, ty.name, m.ty.name
        )));
    }
    write_message(w, m)
}

macro_rules! write_vec {
    ($w:expr, $v:expr) => {
        for x in $v {
            $w.fixed(x.to_le_bytes());
        }
    };
}

fn write_array(w: &mut Writer<'_>, f: &FieldDef, a: &ArrayValue) -> Result<(), CdrError> {
    let n = a.len();
    match f.array {
        Array::Fixed(len) if n != len => {
            return Err(CdrError::Invalid(format!(
                "field `{}` needs {len} elements, got {n}",
                f.name
            )));
        }
        Array::Bounded(max) if n > max => {
            return Err(CdrError::Invalid(format!(
                "field `{}` takes at most {max} elements, got {n}",
                f.name
            )));
        }
        Array::Bounded(_) | Array::Unbounded => w.len(n)?,
        _ => {}
    }
    match (&f.elem, a) {
        (Elem::Primitive(Primitive::Bool), ArrayValue::Bool(v)) => {
            w.buf.extend(v.iter().map(|b| u8::from(*b)))
        }
        (Elem::Primitive(Primitive::Byte), ArrayValue::Byte(v))
        | (Elem::Primitive(Primitive::Char), ArrayValue::Char(v))
        | (Elem::Primitive(Primitive::Uint8), ArrayValue::U8(v)) => w.buf.extend_from_slice(v),
        (Elem::Primitive(Primitive::Int8), ArrayValue::I8(v)) => {
            w.buf.extend(v.iter().map(|b| *b as u8))
        }
        (Elem::Primitive(Primitive::Int16), ArrayValue::I16(v)) => write_vec!(w, v),
        (Elem::Primitive(Primitive::Uint16), ArrayValue::U16(v)) => write_vec!(w, v),
        (Elem::Primitive(Primitive::Int32), ArrayValue::I32(v)) => write_vec!(w, v),
        (Elem::Primitive(Primitive::Uint32), ArrayValue::U32(v)) => write_vec!(w, v),
        (Elem::Primitive(Primitive::Int64), ArrayValue::I64(v)) => write_vec!(w, v),
        (Elem::Primitive(Primitive::Uint64), ArrayValue::U64(v)) => write_vec!(w, v),
        (Elem::Primitive(Primitive::Float32), ArrayValue::F32(v)) => write_vec!(w, v),
        (Elem::Primitive(Primitive::Float64), ArrayValue::F64(v)) => write_vec!(w, v),
        (Elem::String(bound), ArrayValue::String(v)) => {
            for s in v {
                w.string(s, *bound)?;
            }
        }
        (Elem::WString(bound), ArrayValue::WString(v)) => {
            for s in v {
                w.wstring(s, *bound)?;
            }
        }
        (Elem::Message(m), ArrayValue::Message(v)) => {
            for mv in v {
                write_nested(w, f, m, mv)?;
            }
        }
        _ => {
            return Err(CdrError::Invalid(format!(
                "field `{}` is {}, got an array of another type",
                f.name,
                f.type_name()
            )));
        }
    }
    Ok(())
}

fn write_message(w: &mut Writer<'_>, m: &MessageValue) -> Result<(), CdrError> {
    if m.ty.fields.is_empty() {
        w.pad();
        return Ok(());
    }
    if m.fields.len() != m.ty.fields.len() {
        return Err(CdrError::Invalid(format!(
            "{} has {} fields, got {} values",
            m.ty.name,
            m.ty.fields.len(),
            m.fields.len()
        )));
    }
    for (f, v) in m.ty.fields.iter().zip(&m.fields) {
        match (f.array, v) {
            (Array::None, v) => write_scalar(w, f, &f.elem, v)?,
            (_, Value::Array(a)) => write_array(w, f, a)?,
            (_, v) => return Err(mismatch(f, v)),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// YAML

fn yaml_str(s: &str, out: &mut String) {
    out.push('\'');
    for c in s.chars() {
        match c {
            '\'' => out.push_str("''"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('\'');
}

fn yaml_scalar(v: &Value, out: &mut String) {
    match v {
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::String(s) | Value::WString(s) => yaml_str(s, out),
        Value::F32(f) => yaml_float(f64::from(*f), &f.to_string(), out),
        Value::F64(f) => yaml_float(*f, &f.to_string(), out),
        v => {
            let _ = write!(out, "{}", v.to_json());
        }
    }
}

fn yaml_float(f: f64, text: &str, out: &mut String) {
    if f.is_nan() {
        out.push_str(".nan");
    } else if f.is_infinite() {
        out.push_str(if f > 0.0 { ".inf" } else { "-.inf" });
    } else if text.contains(['.', 'e', 'E']) {
        out.push_str(text);
    } else {
        let _ = write!(out, "{text}.0");
    }
}

fn indent(n: usize, out: &mut String) {
    for _ in 0..n {
        out.push_str("  ");
    }
}

fn yaml_message(m: &MessageValue, depth: usize, max: Option<usize>, out: &mut String) {
    if m.fields.is_empty() {
        indent(depth, out);
        out.push_str("{}\n");
        return;
    }
    for (f, v) in m.ty.fields.iter().zip(&m.fields) {
        indent(depth, out);
        out.push_str(&f.name);
        out.push(':');
        yaml_value(v, depth, max, out);
    }
}

/// Writes a value after `key:`.
fn yaml_value(v: &Value, depth: usize, max: Option<usize>, out: &mut String) {
    match v {
        Value::Message(m) if m.fields.is_empty() => out.push_str(" {}\n"),
        Value::Message(m) => {
            out.push('\n');
            yaml_message(m, depth + 1, max, out);
        }
        Value::Array(a) if a.is_empty() => out.push_str(" []\n"),
        Value::Array(a) => {
            out.push('\n');
            let n = a.len();
            let shown = max.map_or(n, |m| n.min(m));
            for i in 0..shown {
                indent(depth, out);
                out.push_str("- ");
                match a.get(i).expect("in range") {
                    Value::Message(m) if !m.fields.is_empty() => {
                        // The first field goes on the dash line.
                        let mut inner = String::new();
                        yaml_message(&m, depth + 1, max, &mut inner);
                        out.push_str(inner.trim_start());
                    }
                    Value::Message(_) => out.push_str("{}\n"),
                    e => {
                        yaml_scalar(&e, out);
                        out.push('\n');
                    }
                }
            }
            if shown < n {
                indent(depth, out);
                let _ = writeln!(out, "# ... {} more ({n} in all)", n - shown);
            }
        }
        v => {
            out.push(' ');
            yaml_scalar(v, out);
            out.push('\n');
        }
    }
}

// ---------------------------------------------------------------------------
// Field paths

/// One step of a [`FieldPath`].
#[derive(Debug, Clone, PartialEq)]
enum Step {
    Field(usize),
    /// Array index; negative counts from the end.
    Index(i64),
}

/// A path to a value inside a message, such as `pose.position.x` or
/// `layout.dim[0].size` (`[-1]` is the last element), checked against a
/// message type.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldPath {
    text: String,
    steps: Vec<Step>,
}

impl FieldPath {
    /// Parses `path` and checks it against `ty`. An empty path is the whole
    /// message.
    pub fn compile(ty: &Arc<MessageType>, path: &str) -> Result<Self> {
        let mut steps = Vec::new();
        let mut cur: Option<Arc<MessageType>> = Some(ty.clone());
        // The array-ness of the value the last step lands on.
        let mut pending_array: Option<(Array, Elem)> = None;
        for part in path.split('.').filter(|p| !p.is_empty()) {
            let (name, mut rest) = match part.find('[') {
                Some(i) => (&part[..i], &part[i..]),
                None => (part, ""),
            };
            if !name.is_empty() {
                if pending_array.is_some() {
                    bail!("`{path}`: index the array before `.{name}`");
                }
                let m = cur
                    .take()
                    .ok_or_else(|| anyhow!("`{path}`: `{name}` is not inside a message"))?;
                let i = m.field_index(name).ok_or_else(|| {
                    anyhow!(
                        "`{path}`: {} has no field `{name}` (fields: {})",
                        m.name,
                        m.fields
                            .iter()
                            .map(|f| f.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })?;
                let f = &m.fields[i];
                steps.push(Step::Field(i));
                if f.array == Array::None {
                    cur = match &f.elem {
                        Elem::Message(t) => Some(t.clone()),
                        _ => None,
                    };
                } else {
                    pending_array = Some((f.array, f.elem.clone()));
                }
            }
            while !rest.is_empty() {
                let close = rest
                    .find(']')
                    .ok_or_else(|| anyhow!("`{path}`: missing `]`"))?;
                let idx: i64 = rest[1..close]
                    .trim()
                    .parse()
                    .map_err(|_| anyhow!("`{path}`: bad index `{}`", &rest[1..close]))?;
                let (_, elem) = pending_array.take().ok_or_else(|| {
                    anyhow!("`{path}`: `[{idx}]` indexes something that is not an array")
                })?;
                steps.push(Step::Index(idx));
                cur = match elem {
                    Elem::Message(t) => Some(t),
                    _ => None,
                };
                rest = &rest[close + 1..];
                if !rest.is_empty() && !rest.starts_with('[') {
                    bail!("`{path}`: unexpected `{rest}`");
                }
            }
        }
        Ok(Self {
            text: path.to_owned(),
            steps,
        })
    }

    /// The path as written.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The value at this path, or `None` if an index is out of range.
    pub fn get(&self, m: &MessageValue) -> Option<Value> {
        enum At<'a> {
            Msg(&'a MessageValue),
            Val(&'a Value),
            Owned(Value),
        }
        let mut at = At::Msg(m);
        for s in &self.steps {
            at = match (s, at) {
                (Step::Field(i), At::Msg(m)) | (Step::Field(i), At::Val(Value::Message(m))) => {
                    At::Val(m.fields.get(*i)?)
                }
                (Step::Index(i), At::Val(Value::Array(a))) => {
                    let n = a.len() as i64;
                    let i = if *i < 0 { n + i } else { *i };
                    if !(0..n).contains(&i) {
                        return None;
                    }
                    match a {
                        ArrayValue::Message(v) => At::Msg(&v[i as usize]),
                        a => At::Owned(a.get(i as usize)?),
                    }
                }
                _ => return None,
            };
        }
        Some(match at {
            At::Msg(m) => Value::Message(m.clone()),
            At::Val(v) => v.clone(),
            At::Owned(v) => v,
        })
    }
}

// ---------------------------------------------------------------------------
// Concatenated definitions (rosbag2 and MCAP `ros2msg` schemas)

/// Separator between definitions in a concatenated schema.
const SEPARATOR: &str =
    "================================================================================";

/// Parses a `ros2msg` schema as rosbag2 writes it: the definition of `root`,
/// then for each referenced type a line of `=`, a line `MSG: pkg/Type` and its
/// definition.
pub fn parse_concatenated(root: &TypeName, text: &str) -> Result<Vec<idl::MessageSpec>> {
    let mut specs = Vec::new();
    let mut name = root.clone();
    let mut body = String::new();
    let flush = |name: &TypeName, body: &str, specs: &mut Vec<idl::MessageSpec>| {
        let file = format!("{name}");
        idl::parse_message(name.clone(), body, &file)
            .map(|s| specs.push(s))
            .map_err(anyhow::Error::from)
    };
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        if line.trim_end() == SEPARATOR {
            flush(&name, &body, &mut specs)?;
            body.clear();
            let header = lines
                .next()
                .ok_or_else(|| anyhow!("schema ends after a separator"))?;
            let ty = header
                .strip_prefix("MSG: ")
                .ok_or_else(|| {
                    anyhow!("expected `MSG: pkg/Type` after a separator, got `{header}`")
                })?
                .trim();
            name = TypeName::parse(ty).ok_or_else(|| anyhow!("bad type name `{ty}`"))?;
            continue;
        }
        body.push_str(line);
        body.push('\n');
    }
    flush(&name, &body, &mut specs)?;
    Ok(specs)
}

/// Parses a type definition as rosbag2 and MCAP files store it, with schema
/// encoding `ros2msg` or `ros2idl`.
pub fn parse_schema(root: &TypeName, encoding: &str, text: &str) -> Result<Vec<idl::MessageSpec>> {
    match encoding {
        "ros2msg" => parse_concatenated(root, text),
        "ros2idl" => crate::idl_file::parse_concatenated_idl(text),
        other => bail!("unsupported schema encoding `{other}`"),
    }
}

/// Compiles `root` from a schema as [`parse_schema`] reads it.
pub fn compile_schema(root: &TypeName, encoding: &str, text: &str) -> Result<Arc<MessageType>> {
    let mut interfaces = Interfaces::default();
    for s in parse_schema(root, encoding, text)? {
        interfaces.add(s);
    }
    MessageType::compile(&interfaces, root)
}

/// Renders the definition of `root` and every type it references in the
/// concatenated `ros2msg` form, from the parsed definitions. Comments and
/// layout are not kept; types and names, defaults and constants are.
pub fn concatenated_definition(interfaces: &Interfaces, root: &TypeName) -> Result<String> {
    let refs = interfaces
        .references(root)
        .map_err(|missing| anyhow!("{root} refers to unknown message type {missing}"))?;
    let mut out = String::new();
    let render = |spec: &idl::MessageSpec, out: &mut String| {
        for c in &spec.constants {
            let _ = writeln!(out, "{} {}={}", c.ty, c.name, c.value);
        }
        for f in &spec.fields {
            let mut ty = match &f.ty.base {
                BaseType::Primitive(p) => p.name().to_owned(),
                BaseType::String { bound: None } => "string".into(),
                BaseType::String { bound: Some(n) } => format!("string<={n}"),
                BaseType::WString { bound: None } => "wstring".into(),
                BaseType::WString { bound: Some(n) } => format!("wstring<={n}"),
                BaseType::Named(t) if t.namespace == "msg" => format!("{}/{}", t.package, t.name),
                BaseType::Named(t) => t.to_string(),
            };
            match f.ty.array {
                Array::None => {}
                Array::Fixed(n) => ty.push_str(&format!("[{n}]")),
                Array::Bounded(n) => ty.push_str(&format!("[<={n}]")),
                Array::Unbounded => ty.push_str("[]"),
            }
            match &f.default {
                Some(d) => {
                    let _ = writeln!(out, "{ty} {} {d}", f.name);
                }
                None => {
                    let _ = writeln!(out, "{ty} {}", f.name);
                }
            }
        }
    };
    render(interfaces.get(root).expect("checked"), &mut out);
    for r in refs {
        let _ = writeln!(out, "{SEPARATOR}");
        if r.namespace == "msg" {
            let _ = writeln!(out, "MSG: {}/{}", r.package, r.name);
        } else {
            let _ = writeln!(out, "MSG: {r}");
        }
        render(interfaces.get(&r).expect("referenced"), &mut out);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types(defs: &[(&str, &str)]) -> Interfaces {
        let mut i = crate::bundled_interfaces();
        for (name, text) in defs {
            let n = TypeName::parse(name).unwrap();
            i.add(idl::parse_message(n, text, name).unwrap());
        }
        i
    }

    fn compile_one(i: &Interfaces, name: &str) -> Arc<MessageType> {
        MessageType::compile(i, &TypeName::parse(name).unwrap()).unwrap()
    }

    #[test]
    fn string_layout_and_roundtrip() {
        let i = types(&[]);
        let t = compile_one(&i, "std_msgs/msg/String");
        let bytes = [0, 1, 0, 0, 6, 0, 0, 0, b'h', b'e', b'l', b'l', b'o', 0];
        let d = decode(&t, &bytes).unwrap();
        assert_eq!(d.value.get("data"), Some(&Value::String("hello".into())));
        assert_eq!(d.encode().unwrap(), bytes);
        assert_eq!(d.value.encode().unwrap(), bytes);
        assert_eq!(
            t.hash,
            "RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18"
        );
    }

    #[test]
    fn padding_tail_and_big_endian_are_kept() {
        let i = types(&[("p/msg/M", "uint8 a\nuint32 b\n")]);
        let t = compile_one(&i, "p/msg/M");
        // Garbage in the three padding bytes and after the message.
        let bytes = [0, 1, 0, 0, 7, 0xaa, 0, 0xbb, 1, 0, 0, 0, 0xde, 0xad];
        let d = decode(&t, &bytes).unwrap();
        assert_eq!(d.value.get("b"), Some(&Value::U32(1)));
        assert_eq!(d.padding, vec![(5, 0xaa), (7, 0xbb)]);
        assert_eq!(d.tail, [0xde, 0xad]);
        assert_eq!(d.encode().unwrap(), bytes);
        // Clean encoding zeroes padding and drops the tail.
        assert_eq!(
            d.value.encode().unwrap(),
            [0, 1, 0, 0, 7, 0, 0, 0, 1, 0, 0, 0]
        );
        let be = [0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 1];
        let d = decode(&t, &be).unwrap();
        assert_eq!(d.value.get("b"), Some(&Value::U32(1)));
        assert_eq!(d.encode().unwrap(), be);
    }

    #[test]
    fn wstrings_are_utf16_units_of_four_bytes() {
        let i = types(&[("p/msg/W", "wstring data\n")]);
        let t = compile_one(&i, "p/msg/W");
        // From rclpy.serialization.serialize_message(WString(data="hé€😀")).
        let hex = "000100000500000068000000e9000000ac2000003dd8000000de00005f3a3a57";
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        let d = decode(&t, &bytes).unwrap();
        assert_eq!(d.value.get("data").unwrap().as_str(), Some("hé€😀"));
        assert_eq!(d.tail, [0x5f, 0x3a, 0x3a, 0x57]);
        assert_eq!(d.encode().unwrap(), bytes);
    }

    #[test]
    fn defaults_and_constants() {
        let i = types(&[(
            "p/msg/D",
            "int32 A=-5\nstring S=\"x y\"\nfloat64 F = 1e3\n\
             float32 f 1.5\nint32[3] a [1, 2, 3]\nstring[] names [\"a, b\", 'c']\n\
             bool flag true\nuint8[] none\nstd_msgs/Header h\nstring t 'it\\'s'\n",
        )]);
        let t = compile_one(&i, "p/msg/D");
        assert_eq!(t.constant("A"), Some(&Value::I32(-5)));
        assert_eq!(t.constant("S"), Some(&Value::String("x y".into())));
        assert_eq!(t.constant("F"), Some(&Value::F64(1000.0)));
        let m = t.default_value();
        assert_eq!(m.get("f"), Some(&Value::F32(1.5)));
        assert_eq!(
            m.get("a"),
            Some(&Value::Array(ArrayValue::I32(vec![1, 2, 3])))
        );
        assert_eq!(
            m.get("names"),
            Some(&Value::Array(ArrayValue::String(vec![
                "a, b".into(),
                "c".into()
            ])))
        );
        assert_eq!(m.get("flag"), Some(&Value::Bool(true)));
        assert_eq!(m.get("t").unwrap().as_str(), Some("it's"));
        let bytes = m.encode().unwrap();
        assert_eq!(decode(&t, &bytes).unwrap().value, m);
        assert!(parse_literal(&Elem::Primitive(Primitive::Int8), Array::None, "200").is_err());
        assert!(parse_literal(&Elem::Primitive(Primitive::Int8), Array::Fixed(2), "[1]").is_err());
    }

    #[test]
    fn json_paths_and_yaml() {
        let i = types(&[]);
        let t = compile_one(&i, "geometry_msgs/msg/PoseArray");
        let m = MessageValue::from_json(
            &t,
            &serde_json::json!({
                "header": { "frame_id": "map", "stamp": { "sec": 3 } },
                "poses": [
                    { "position": { "x": 1.5 } },
                    { "position": { "x": -2.0, "y": 4 } }
                ]
            }),
        )
        .unwrap();
        let bytes = m.encode().unwrap();
        let d = decode(&t, &bytes).unwrap();
        assert_eq!(d.value, m);
        let path = |p: &str| FieldPath::compile(&t, p).unwrap();
        assert_eq!(path("poses[1].position.x").get(&m), Some(Value::F64(-2.0)));
        assert_eq!(path("poses[-1].position.y").get(&m), Some(Value::F64(4.0)));
        assert_eq!(path("poses[2].position.x").get(&m), None);
        assert_eq!(path("header.stamp.sec").get(&m), Some(Value::I32(3)));
        assert!(FieldPath::compile(&t, "poses.position").is_err());
        assert!(FieldPath::compile(&t, "header[0]").is_err());
        assert!(FieldPath::compile(&t, "nope").is_err());
        let yaml = m.to_yaml(Some(1));
        assert!(
            yaml.starts_with("header:\n  stamp:\n    sec: 3\n"),
            "{yaml}"
        );
        assert!(yaml.contains("  frame_id: 'map'\n"), "{yaml}");
        assert!(yaml.contains("poses:\n- position:\n    x: 1.5\n"), "{yaml}");
        assert!(yaml.contains("# ... 1 more (2 in all)"), "{yaml}");
    }

    #[test]
    fn bounds_and_type_mismatches_are_refused_when_encoding() {
        let i = types(&[("p/msg/B", "string<=3 s\nint16[<=2] v\n")]);
        let t = compile_one(&i, "p/msg/B");
        let mut m = t.default_value();
        m.set("s", Value::String("abcd".into())).unwrap();
        assert!(m.encode().is_err());
        m.set("s", Value::String("abc".into())).unwrap();
        m.set("v", Value::Array(ArrayValue::I16(vec![1, 2, 3])))
            .unwrap();
        assert!(m.encode().is_err());
        m.set("v", Value::Array(ArrayValue::I32(vec![1]))).unwrap();
        assert!(m.encode().is_err());
        m.set("v", Value::Array(ArrayValue::I16(vec![1]))).unwrap();
        assert!(m.encode().is_ok());
    }

    #[test]
    fn corrupt_input_is_rejected() {
        let i = types(&[]);
        let t = compile_one(&i, "sensor_msgs/msg/Image");
        assert!(decode(&t, &[0, 1, 0, 0, 1]).is_err());
        assert!(decode(&t, &[0, 2, 0, 0]).is_err());
        let huge = [0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0x7f];
        assert!(decode(&t, &huge).is_err());
    }

    #[test]
    fn concatenated_definitions_roundtrip() {
        let i = types(&[]);
        let root = TypeName::parse("geometry_msgs/msg/PoseStamped").unwrap();
        let text = concatenated_definition(&i, &root).unwrap();
        assert!(text.contains("\nMSG: geometry_msgs/Pose\n"), "{text}");
        let specs = parse_concatenated(&root, &text).unwrap();
        let mut j = Interfaces::default();
        for s in specs {
            j.add(s);
        }
        assert_eq!(
            crate::hash::type_hash(&j, &root),
            crate::hash::type_hash(&i, &root)
        );
    }
}
