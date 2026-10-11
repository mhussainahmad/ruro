use anyhow::{Context, anyhow};
use ruro_graph::ParamKind;
use serde_json::Value;

use crate::Result;

/// A Rust type usable as a node parameter.
pub trait ParamValue: Sized {
    const KIND: ParamKind;
    fn from_json(v: &Value) -> Result<Self>;
    fn to_json(&self) -> Value;
}

macro_rules! int_param {
    ($($t:ty),*) => {$(
        impl ParamValue for $t {
            const KIND: ParamKind = ParamKind::Int;

            fn from_json(v: &Value) -> Result<Self> {
                let n = v
                    .as_i64()
                    .map(i128::from)
                    .or_else(|| v.as_u64().map(i128::from))
                    .or_else(|| v.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i128))
                    .ok_or_else(|| anyhow!("expected an integer, got {v}"))?;
                <$t>::try_from(n).with_context(|| format!("{n} does not fit in {}", stringify!($t)))
            }

            fn to_json(&self) -> Value {
                Value::from(*self)
            }
        }
    )*};
}

int_param!(i8, i16, i32, i64, u8, u16, u32, u64, usize, isize);

macro_rules! float_param {
    ($($t:ty),*) => {$(
        impl ParamValue for $t {
            const KIND: ParamKind = ParamKind::Float;

            fn from_json(v: &Value) -> Result<Self> {
                v.as_f64().map(|f| f as $t).ok_or_else(|| anyhow!("expected a number, got {v}"))
            }

            fn to_json(&self) -> Value {
                Value::from(*self as f64)
            }
        }
    )*};
}

float_param!(f32, f64);

impl ParamValue for bool {
    const KIND: ParamKind = ParamKind::Bool;

    fn from_json(v: &Value) -> Result<Self> {
        v.as_bool()
            .ok_or_else(|| anyhow!("expected true or false, got {v}"))
    }

    fn to_json(&self) -> Value {
        Value::Bool(*self)
    }
}

impl ParamValue for String {
    const KIND: ParamKind = ParamKind::String;

    fn from_json(v: &Value) -> Result<Self> {
        v.as_str()
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("expected a string, got {v}"))
    }

    fn to_json(&self) -> Value {
        Value::String(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ints_check_range() {
        assert_eq!(u8::from_json(&json!(200)).unwrap(), 200);
        assert!(u8::from_json(&json!(300)).is_err());
        assert!(u32::from_json(&json!(-1)).is_err());
        assert_eq!(i64::from_json(&json!(4.0)).unwrap(), 4);
        assert!(i64::from_json(&json!(4.5)).is_err());
    }

    #[test]
    fn floats_accept_ints() {
        assert_eq!(f64::from_json(&json!(3)).unwrap(), 3.0);
    }

    #[test]
    fn strings_and_bools() {
        assert!(bool::from_json(&json!("yes")).is_err());
        assert_eq!(String::from_json(&json!("x")).unwrap(), "x");
    }
}
