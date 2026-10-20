//! CDR encoding of messages given as JSON values, driven by parsed
//! definitions instead of generated types, for `ruro ros2 call`. Fields left
//! out take their declared default, or zero.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Map, Value};

use crate::cdr::{CdrReader, CdrWriter};
use crate::idl::{Array, BaseType, Field, Interfaces, Primitive, TypeName};

/// Encodes `value` (a JSON object) as message `name`, header included.
pub fn encode(interfaces: &Interfaces, name: &TypeName, value: &Value) -> Result<Vec<u8>> {
    let mut w = CdrWriter::new();
    encode_message(interfaces, name, value, &mut w)?;
    Ok(w.finish())
}

/// Decodes message `name` into a JSON object.
pub fn decode(interfaces: &Interfaces, name: &TypeName, bytes: &[u8]) -> Result<Value> {
    let mut r = CdrReader::new(bytes)?;
    decode_message(interfaces, name, &mut r)
}

fn spec<'a>(interfaces: &'a Interfaces, name: &TypeName) -> Result<&'a crate::idl::MessageSpec> {
    interfaces
        .get(name)
        .ok_or_else(|| anyhow!("unknown message type {name}"))
}

fn encode_message(
    interfaces: &Interfaces,
    name: &TypeName,
    value: &Value,
    w: &mut CdrWriter,
) -> Result<()> {
    let spec = spec(interfaces, name)?;
    let empty = Map::new();
    let obj = match value {
        Value::Object(o) => o,
        Value::Null => &empty,
        other => bail!("{name}: expected a mapping, got {other}"),
    };
    if let Some(k) = obj
        .keys()
        .find(|k| !spec.fields.iter().any(|f| &f.name == *k))
    {
        bail!("{name} has no field `{k}`");
    }
    if spec.fields.is_empty() {
        w.u8(0);
    }
    for f in &spec.fields {
        let v = match obj.get(&f.name) {
            Some(v) => v.clone(),
            None => default_value(f)?,
        };
        encode_field(interfaces, f, &v, w).with_context(|| format!("{name}.{}", f.name))?;
    }
    Ok(())
}

/// The declared default of a field as a JSON value (`Null` for none).
fn default_value(f: &Field) -> Result<Value> {
    match &f.default {
        None => Ok(Value::Null),
        Some(text) => {
            let v: Value = serde_saphyr::from_str(text)
                .map_err(|e| anyhow!("default `{text}` of {}: {e}", f.name))?;
            Ok(match (&f.ty.base, v) {
                // A string default may be unquoted or quoted.
                (BaseType::String { .. } | BaseType::WString { .. }, Value::Number(n)) => {
                    Value::String(n.to_string())
                }
                (_, v) => v,
            })
        }
    }
}

fn encode_field(interfaces: &Interfaces, f: &Field, v: &Value, w: &mut CdrWriter) -> Result<()> {
    let items = |v: &Value| -> Result<Vec<Value>> {
        match v {
            Value::Array(a) => Ok(a.clone()),
            Value::Null => Ok(Vec::new()),
            other => bail!("expected a list, got {other}"),
        }
    };
    match f.ty.array {
        Array::None => encode_scalar(interfaces, &f.ty.base, v, w),
        Array::Fixed(n) => {
            let mut a = items(v)?;
            if a.is_empty() {
                a = vec![Value::Null; n];
            }
            if a.len() != n {
                bail!("expected {n} items, got {}", a.len());
            }
            for item in &a {
                encode_scalar(interfaces, &f.ty.base, item, w)?;
            }
            Ok(())
        }
        Array::Bounded(_) | Array::Unbounded => {
            let a = items(v)?;
            if let Array::Bounded(max) = f.ty.array
                && a.len() > max
            {
                bail!("at most {max} items allowed, got {}", a.len());
            }
            w.len(a.len());
            for item in &a {
                encode_scalar(interfaces, &f.ty.base, item, w)?;
            }
            Ok(())
        }
    }
}

fn int(v: &Value) -> Result<i128> {
    match v {
        Value::Null => Ok(0),
        Value::Bool(b) => Ok(i128::from(*b)),
        Value::Number(n) => n
            .as_i64()
            .map(i128::from)
            .or_else(|| n.as_u64().map(i128::from))
            .ok_or_else(|| anyhow!("expected an integer, got {n}")),
        other => bail!("expected an integer, got {other}"),
    }
}

fn float(v: &Value) -> Result<f64> {
    match v {
        Value::Null => Ok(0.0),
        Value::Number(n) => n.as_f64().ok_or_else(|| anyhow!("bad number {n}")),
        other => bail!("expected a number, got {other}"),
    }
}

fn ranged<T: TryFrom<i128>>(v: &Value) -> Result<T> {
    let n = int(v)?;
    T::try_from(n).map_err(|_| anyhow!("{n} is out of range"))
}

fn encode_scalar(
    interfaces: &Interfaces,
    base: &BaseType,
    v: &Value,
    w: &mut CdrWriter,
) -> Result<()> {
    match base {
        BaseType::Primitive(p) => match p {
            Primitive::Bool => w.bool(match v {
                Value::Bool(b) => *b,
                other => int(other)? != 0,
            }),
            Primitive::Byte | Primitive::Char | Primitive::IdlChar | Primitive::Uint8 => {
                w.u8(ranged(v)?)
            }
            Primitive::Int8 => w.i8(ranged(v)?),
            Primitive::Int16 => w.i16(ranged(v)?),
            Primitive::Uint16 => w.u16(ranged(v)?),
            Primitive::Int32 => w.i32(ranged(v)?),
            Primitive::Uint32 => w.u32(ranged(v)?),
            Primitive::Int64 => w.i64(ranged(v)?),
            Primitive::Uint64 => w.u64(ranged(v)?),
            Primitive::Float32 => w.f32(float(v)? as f32),
            Primitive::Float64 => w.f64(float(v)?),
        },
        BaseType::String { bound } | BaseType::WString { bound } => {
            let s = match v {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                other => other.to_string(),
            };
            // Bounds count bytes of a string, UTF-16 units of a wstring.
            let wide = matches!(base, BaseType::WString { .. });
            let len = if wide {
                s.encode_utf16().count()
            } else {
                s.len()
            };
            if let Some(max) = bound
                && len > *max
            {
                bail!("string longer than {max}");
            }
            if wide {
                w.wstring(&s);
            } else {
                w.string(&s);
            }
        }
        BaseType::Named(t) => encode_message(interfaces, t, v, w)?,
    }
    Ok(())
}

fn decode_message(interfaces: &Interfaces, name: &TypeName, r: &mut CdrReader) -> Result<Value> {
    let spec = spec(interfaces, name)?;
    let mut out = Map::new();
    if spec.fields.is_empty() {
        r.u8()?;
    }
    for f in &spec.fields {
        let v = match f.ty.array {
            Array::None => decode_scalar(interfaces, &f.ty.base, r)?,
            Array::Fixed(n) => Value::Array(
                (0..n)
                    .map(|_| decode_scalar(interfaces, &f.ty.base, r))
                    .collect::<Result<_>>()?,
            ),
            Array::Bounded(_) | Array::Unbounded => {
                let n = r.len(1)?;
                Value::Array(
                    (0..n)
                        .map(|_| decode_scalar(interfaces, &f.ty.base, r))
                        .collect::<Result<_>>()?,
                )
            }
        };
        out.insert(f.name.clone(), v);
    }
    Ok(Value::Object(out))
}

fn decode_scalar(interfaces: &Interfaces, base: &BaseType, r: &mut CdrReader) -> Result<Value> {
    Ok(match base {
        BaseType::Primitive(p) => match p {
            Primitive::Bool => Value::Bool(r.bool()?),
            Primitive::Byte | Primitive::Char | Primitive::IdlChar | Primitive::Uint8 => {
                r.u8()?.into()
            }
            Primitive::Int8 => r.i8()?.into(),
            Primitive::Int16 => r.i16()?.into(),
            Primitive::Uint16 => r.u16()?.into(),
            Primitive::Int32 => r.i32()?.into(),
            Primitive::Uint32 => r.u32()?.into(),
            Primitive::Int64 => r.i64()?.into(),
            Primitive::Uint64 => r.u64()?.into(),
            Primitive::Float32 => f64::from(r.f32()?).into(),
            Primitive::Float64 => r.f64()?.into(),
        },
        BaseType::String { .. } => Value::String(r.string()?),
        BaseType::WString { .. } => Value::String(r.wstring()?),
        BaseType::Named(t) => decode_message(interfaces, t, r)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdr;
    use crate::msgs::example_interfaces::srv::AddTwoInts_Request;
    use crate::msgs::geometry_msgs::msg::Twist;
    use crate::msgs::std_srvs::srv::Trigger_Response;
    use serde_json::json;

    #[test]
    fn matches_generated_encoding() {
        let i = crate::bundled_interfaces();
        let name = TypeName::parse("example_interfaces/srv/AddTwoInts_Request").unwrap();
        let b = encode(&i, &name, &json!({"a": 2, "b": -40})).unwrap();
        assert_eq!(b, cdr::to_vec(&AddTwoInts_Request { a: 2, b: -40 }));

        let twist = TypeName::parse("geometry_msgs/msg/Twist").unwrap();
        let b = encode(&i, &twist, &json!({"linear": {"x": 0.5}})).unwrap();
        let t: Twist = cdr::from_slice(&b).unwrap();
        assert_eq!(t.linear.x, 0.5);

        let resp = Trigger_Response {
            success: true,
            message: "ok".into(),
        };
        let name = TypeName::parse("std_srvs/srv/Trigger_Response").unwrap();
        assert_eq!(
            decode(&i, &name, &cdr::to_vec(&resp)).unwrap(),
            json!({"success": true, "message": "ok"})
        );
        // Empty requests carry the placeholder byte.
        let empty = TypeName::parse("std_srvs/srv/Trigger_Request").unwrap();
        assert_eq!(encode(&i, &empty, &Value::Null).unwrap(), [0, 1, 0, 0, 0]);
    }

    #[test]
    fn wstrings_are_utf16_like_ros() {
        let i = crate::bundled_interfaces();
        let name = TypeName::parse("example_interfaces/msg/WString").unwrap();
        let b = encode(&i, &name, &json!({"data": "hé"})).unwrap();
        let mut w = CdrWriter::new();
        w.wstring("hé");
        assert_eq!(&b[4..], &w.finish()[4..]);
        assert_eq!(decode(&i, &name, &b).unwrap(), json!({"data": "hé"}));
    }

    #[test]
    fn errors_name_the_field() {
        let i = crate::bundled_interfaces();
        let name = TypeName::parse("example_interfaces/srv/AddTwoInts_Request").unwrap();
        let e = encode(&i, &name, &json!({"c": 1})).unwrap_err();
        assert!(e.to_string().contains("no field `c`"), "{e}");
        let e = encode(&i, &name, &json!({"a": "x"})).unwrap_err();
        assert!(format!("{e:#}").contains(".a"), "{e:#}");
        let b = TypeName::parse("std_msgs/msg/Byte").unwrap();
        assert!(encode(&i, &b, &json!({"data": 300})).is_err());
    }
}
