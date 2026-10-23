//! Messages as JSON, and ports of any standard message type.
//!
//! Most types use their serde form, the shape `ruro schema <type>` prints:
//! `std/Scalar` is a number, `std/Pose` an object with `position` and
//! `orientation`. Bytes travel as base64: `std/Bytes` is a base64 string,
//! `std/Image` is `{width, height, encoding, data}` with `data` in base64,
//! and `std/PointCloud` is `{points}` with the points' little-endian `f32`
//! x, y, z values in base64 (a list of `[x, y, z]` is accepted too).

use anyhow::{Context, Result, bail, ensure};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use ruro_msg::{Bytes, Encoding, Image, Message, PointCloud};
use ruro_node::{Builder, Input, Output};
use serde_json::{Value, json};

/// A message type with a JSON form for other programs.
pub trait JsonMessage: Message + Sized {
    fn to_json(&self) -> Value;
    fn from_json(v: Value) -> Result<Self>;
}

macro_rules! serde_messages {
    ($($t:ident),* $(,)?) => {$(
        impl JsonMessage for ruro_msg::$t {
            fn to_json(&self) -> Value {
                serde_json::to_value(self).expect("standard messages serialize")
            }

            fn from_json(v: Value) -> Result<Self> {
                Ok(serde_json::from_value(v)?)
            }
        }
    )*};
}

serde_messages!(
    Scalar,
    Text,
    Flag,
    Vec3,
    Pose,
    Twist,
    JointState,
    JointCommand,
    Transform,
    Transforms,
    Imu,
    LaserScan,
    Odometry,
    CameraInfo,
    Joy,
);

fn bytes_of(v: &Value) -> Result<Vec<u8>> {
    match v {
        Value::String(s) => B64.decode(s).context("not valid base64"),
        Value::Array(items) => items
            .iter()
            .map(|b| {
                b.as_u64()
                    .and_then(|b| u8::try_from(b).ok())
                    .context("not a byte")
            })
            .collect(),
        _ => bail!("expected a base64 string"),
    }
}

impl JsonMessage for Bytes {
    fn to_json(&self) -> Value {
        Value::String(B64.encode(&self.0))
    }

    fn from_json(v: Value) -> Result<Self> {
        Ok(Bytes(bytes_of(&v)?))
    }
}

fn encoding_name(e: Encoding) -> &'static str {
    match e {
        Encoding::Rgb8 => "rgb8",
        Encoding::Rgba8 => "rgba8",
        Encoding::Mono8 => "mono8",
        Encoding::Mono16 => "mono16",
    }
}

fn parse_encoding(s: &str) -> Result<Encoding> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "rgb8" => Encoding::Rgb8,
        "rgba8" => Encoding::Rgba8,
        "mono8" => Encoding::Mono8,
        "mono16" => Encoding::Mono16,
        other => bail!("unknown encoding `{other}` (expected rgb8, rgba8, mono8 or mono16)"),
    })
}

impl JsonMessage for Image {
    fn to_json(&self) -> Value {
        json!({
            "width": self.width,
            "height": self.height,
            "encoding": encoding_name(self.encoding),
            "data": B64.encode(&self.data),
        })
    }

    fn from_json(v: Value) -> Result<Self> {
        let field = |name: &str| v.get(name).with_context(|| format!("missing `{name}`"));
        let dim = |name: &str| -> Result<u32> {
            field(name)?
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .with_context(|| format!("`{name}` is not a size"))
        };
        let (width, height) = (dim("width")?, dim("height")?);
        let encoding = match v.get("encoding") {
            None => Encoding::Rgb8,
            Some(e) => parse_encoding(e.as_str().context("`encoding` is not a string")?)?,
        };
        let data = bytes_of(field("data")?).context("`data`")?;
        let expected = width as usize * height as usize * encoding.bytes_per_pixel();
        ensure!(
            data.len() == expected,
            "`data` has {} bytes; a {width}x{height} {} image has {expected}",
            data.len(),
            encoding_name(encoding)
        );
        Ok(Image {
            width,
            height,
            encoding,
            data,
        })
    }
}

impl JsonMessage for PointCloud {
    fn to_json(&self) -> Value {
        let mut raw = Vec::with_capacity(self.points.len() * 12);
        for p in &self.points {
            for c in p {
                raw.extend_from_slice(&c.to_le_bytes());
            }
        }
        json!({ "points": B64.encode(raw) })
    }

    fn from_json(v: Value) -> Result<Self> {
        let points = v.get("points").context("missing `points`")?;
        let points = match points {
            Value::String(s) => {
                let raw = B64.decode(s).context("`points` is not valid base64")?;
                ensure!(
                    raw.len() % 12 == 0,
                    "`points` has {} bytes, not a whole number of points (12 bytes each)",
                    raw.len()
                );
                raw.as_chunks::<12>()
                    .0
                    .iter()
                    .map(|p| {
                        let c = |i: usize| f32::from_le_bytes(p[i..i + 4].try_into().unwrap());
                        [c(0), c(4), c(8)]
                    })
                    .collect()
            }
            other => serde_json::from_value(other.clone())
                .context("`points` is neither base64 nor a list of [x, y, z]")?,
        };
        Ok(PointCloud { points })
    }
}

macro_rules! names {
    ($($t:ident),* $(,)?) => {
        /// Message types process nodes can use.
        pub const SUPPORTED: &[&str] = &[$(<ruro_msg::$t as Message>::TYPE),*];
    };
}
ruro_msg::for_each_std_message! { names }

pub fn supported(ty: &str) -> bool {
    SUPPORTED.contains(&ty)
}

macro_rules! ports {
    ($($t:ident),* $(,)?) => {
        /// An input of any standard message type.
        pub enum AnyInput {
            $($t(Input<ruro_msg::$t>),)*
        }

        /// An output of any standard message type.
        pub enum AnyOutput {
            $($t(Output<ruro_msg::$t>),)*
        }

        impl AnyInput {
            pub fn declare(b: &mut Builder, ty: &str, name: &str) -> Result<Self> {
                Ok(match ty {
                    $(<ruro_msg::$t as Message>::TYPE => Self::$t(b.input(name)?),)*
                    other => bail!("process nodes do not support message type {other}"),
                })
            }

            /// Takes every queued message, oldest first, as
            /// `{seq, stamp_ns, msg}`.
            pub fn drain(&self) -> Vec<Value> {
                let mut out = Vec::new();
                match self {
                    $(Self::$t(i) => {
                        while let Some(s) = i.recv() {
                            out.push(json!({
                                "seq": s.seq,
                                "stamp_ns": s.stamp_ns,
                                "msg": s.msg.to_json(),
                            }));
                        }
                    })*
                }
                out
            }
        }

        impl AnyOutput {
            pub fn declare(b: &mut Builder, ty: &str, name: &str) -> Result<Self> {
                Ok(match ty {
                    $(<ruro_msg::$t as Message>::TYPE => Self::$t(b.output(name)?),)*
                    other => bail!("process nodes do not support message type {other}"),
                })
            }

            pub fn send(&self, v: Value) -> Result<()> {
                match self {
                    $(Self::$t(o) => {
                        let msg = <ruro_msg::$t as JsonMessage>::from_json(v).with_context(|| {
                            format!("not a valid {}", <ruro_msg::$t as Message>::TYPE)
                        })?;
                        o.send(msg)
                    })*
                }
            }
        }
    };
}
ruro_msg::for_each_std_message! { ports }

#[cfg(test)]
mod tests {
    use super::*;
    use ruro_msg::{Pose, Scalar, Transform, Transforms};

    fn round_trip<T: JsonMessage + PartialEq + std::fmt::Debug>(m: T) -> Value {
        let v = m.to_json();
        assert_eq!(T::from_json(v.clone()).unwrap(), m);
        v
    }

    #[test]
    fn plain_types_use_their_serde_form() {
        assert_eq!(round_trip(Scalar(1.5)), json!(1.5));
        let v = round_trip(Pose::default());
        assert_eq!(v["orientation"]["w"], json!(1.0));
        let v = round_trip(Transforms(vec![Transform::default()]));
        assert!(v.is_array());
    }

    #[test]
    fn bytes_images_and_clouds_travel_as_base64() {
        assert_eq!(round_trip(Bytes(b"hi".to_vec())), json!("aGk="));
        assert_eq!(Bytes::from_json(json!([104, 105])).unwrap().0, b"hi");

        let mut img = Image::new(2, 1, Encoding::Rgb8);
        img.data.copy_from_slice(&[1, 2, 3, 4, 5, 6]);
        let v = round_trip(img);
        assert_eq!(v["encoding"], "rgb8");
        assert_eq!(v["data"], B64.encode([1, 2, 3, 4, 5, 6]));
        let short = json!({"width": 2, "height": 2, "data": "AQID"});
        let e = format!("{:#}", Image::from_json(short).unwrap_err());
        assert!(e.contains("has 3 bytes") && e.contains("has 12"), "{e}");

        let cloud = PointCloud {
            points: vec![[1.0, 2.0, 3.0], [-1.0, 0.5, 0.0]],
        };
        round_trip(cloud.clone());
        let listed = json!({"points": [[1.0, 2.0, 3.0], [-1.0, 0.5, 0.0]]});
        assert_eq!(PointCloud::from_json(listed).unwrap(), cloud);
    }

    #[test]
    fn numbers_cross_exactly() {
        // As a program prints them (shortest round-trip form) and back.
        for x in [-2.8967501709585886_f64, 0.1 + 0.2, 1e-310, f64::MAX, -0.0] {
            let text = serde_json::to_string(&Scalar(x).to_json()).unwrap();
            let back = Scalar::from_json(serde_json::from_str(&text).unwrap()).unwrap();
            assert_eq!(back.0.to_bits(), x.to_bits(), "{text}");
        }
    }

    #[test]
    fn every_standard_type_is_supported() {
        assert!(supported("std/Image") && supported("std/Joy"));
        assert!(!supported("ros2/Any"));
    }
}
