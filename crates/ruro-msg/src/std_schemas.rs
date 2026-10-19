//! Hand-written schemas of the standard message vocabulary.
//!
//! Kept next to the types so a change to a struct has an obvious place to be
//! mirrored. The `schemas_match_encoding` test checks every schema against the
//! actual postcard output, which catches most mistakes.

use crate::schema::{Describe, Field, Schema};
use crate::{
    Bytes, CameraInfo, Encoding, Flag, Header, Image, Imu, JointCommand, JointState, Joy,
    LaserScan, Odometry, PointCloud, Pose, Quat, Scalar, Text, Transform, Transforms, Twist, Vec3,
    register_schema,
};

/// `describe_struct!(Type { field: FieldType = "doc", ... })`
macro_rules! describe_struct {
    ($ty:ident { $($field:ident : $fty:ty $(= $doc:literal)?),* $(,)? }) => {
        impl Describe for $ty {
            fn describe() -> Schema {
                Schema::structure(stringify!($ty), vec![
                    $(Field::new(stringify!($field), <$fty as Describe>::describe())
                        $(.doc($doc))?),*
                ])
            }
        }
    };
}

/// `describe_newtype!(Type(Inner))`
macro_rules! describe_newtype {
    ($ty:ident($inner:ty)) => {
        impl Describe for $ty {
            fn describe() -> Schema {
                Schema::tuple_struct(stringify!($ty), vec![<$inner as Describe>::describe()])
            }
        }
    };
}

describe_newtype!(Scalar(f64));
describe_newtype!(Text(String));
describe_newtype!(Flag(bool));
describe_newtype!(Bytes(Vec<u8>));
describe_newtype!(Transforms(Vec<Transform>));

describe_struct!(Vec3 {
    x: f64,
    y: f64,
    z: f64
});
describe_struct!(Quat {
    x: f64,
    y: f64,
    z: f64,
    w: f64
});
describe_struct!(Pose {
    position: Vec3,
    orientation: Quat
});
describe_struct!(Twist {
    linear: Vec3 = "m/s",
    angular: Vec3 = "rad/s",
});
describe_struct!(JointState {
    position: Vec<f64>,
    velocity: Vec<f64>,
    effort: Vec<f64>,
});
describe_struct!(Header {
    stamp_ns: u64 = "Measurement time in nanoseconds since the Unix epoch.",
    frame_id: String = "Coordinate frame the data is expressed in.",
});
describe_struct!(Transform {
    header: Header,
    child_frame_id: String,
    translation: Vec3,
    rotation: Quat,
});
describe_struct!(Imu {
    header: Header,
    orientation: Quat,
    orientation_covariance: [f64; 9],
    angular_velocity: Vec3 = "rad/s",
    angular_velocity_covariance: [f64; 9],
    linear_acceleration: Vec3 = "m/s²",
    linear_acceleration_covariance: [f64; 9],
});
describe_struct!(LaserScan {
    header: Header,
    angle_min: f32 = "rad",
    angle_max: f32 = "rad",
    angle_increment: f32 = "rad",
    time_increment: f32 = "s",
    scan_time: f32 = "s",
    range_min: f32 = "m",
    range_max: f32 = "m",
    ranges: Vec<f32>,
    intensities: Vec<f32>,
});
describe_struct!(Odometry {
    header: Header,
    child_frame_id: String,
    pose: Pose,
    twist: Twist,
});
describe_struct!(CameraInfo {
    header: Header,
    width: u32,
    height: u32,
    distortion_model: String,
    d: Vec<f64>,
    k: [f64; 9] = "Row-major 3x3 intrinsic matrix.",
    r: [f64; 9],
    p: [f64; 12] = "Row-major 3x4 projection matrix.",
});
describe_struct!(Joy {
    header: Header,
    axes: Vec<f32>,
    buttons: Vec<i32>,
});
describe_struct!(JointCommand {
    header: Header,
    position: Vec<f64>,
    velocity: Vec<f64>,
    effort: Vec<f64>,
});

impl Describe for Encoding {
    fn describe() -> Schema {
        use crate::schema::{Variant, VariantPayload};
        let unit = |name: &str| Variant {
            name: name.into(),
            payload: VariantPayload::Unit,
        };
        Schema::Enum {
            name: "Encoding".into(),
            variants: vec![unit("Rgb8"), unit("Rgba8"), unit("Mono8"), unit("Mono16")],
        }
    }
}

impl Describe for Image {
    fn describe() -> Schema {
        Schema::Custom {
            name: "Image".into(),
            encoding: "ruro/image-v1: u32 LE width, u32 LE height, u8 encoding \
                       (0 rgb8, 1 rgba8, 2 mono8, 3 mono16 LE), then width*height*bytes-per-pixel bytes"
                .into(),
            fields: vec![
                Field::new("width", Schema::U32),
                Field::new("height", Schema::U32),
                Field::new("encoding", Encoding::describe()),
                Field::new("data", Schema::seq(Schema::U8)).doc("Row-major pixels."),
            ],
        }
    }
}

impl Describe for PointCloud {
    fn describe() -> Schema {
        Schema::Custom {
            name: "PointCloud".into(),
            encoding: "ruro/pointcloud-v1: consecutive f32 LE x, y, z triples, no prefix".into(),
            fields: vec![Field::new(
                "points",
                Schema::seq(Schema::array(Schema::F32, 3)),
            )],
        }
    }
}

register_schema!(Image);
register_schema!(PointCloud);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Message;

    /// Encodes `value` by walking `schema` over its JSON form with postcard
    /// rules, which must give the same bytes as the real encoder.
    fn encode_by_schema(schema: &Schema, value: &serde_json::Value, out: &mut Vec<u8>) {
        fn varint(mut v: u128, out: &mut Vec<u8>) {
            loop {
                let byte = (v & 0x7f) as u8;
                v >>= 7;
                if v == 0 {
                    out.push(byte);
                    break;
                }
                out.push(byte | 0x80);
            }
        }
        fn zigzag(v: i128) -> u128 {
            ((v << 1) ^ (v >> 127)) as u128
        }
        match schema {
            Schema::Bool => out.push(value.as_bool().unwrap() as u8),
            Schema::U8 => out.push(value.as_u64().unwrap() as u8),
            Schema::U16 | Schema::U32 | Schema::U64 => varint(value.as_u64().unwrap() as u128, out),
            Schema::I32 | Schema::I64 => varint(zigzag(value.as_i64().unwrap() as i128), out),
            Schema::F32 => out.extend_from_slice(&(value.as_f64().unwrap() as f32).to_le_bytes()),
            Schema::F64 => out.extend_from_slice(&value.as_f64().unwrap().to_le_bytes()),
            Schema::String => {
                let s = value.as_str().unwrap();
                varint(s.len() as u128, out);
                out.extend_from_slice(s.as_bytes());
            }
            Schema::Seq { item } => {
                let items = value.as_array().unwrap();
                varint(items.len() as u128, out);
                items.iter().for_each(|v| encode_by_schema(item, v, out));
            }
            Schema::Array { item, len } => {
                let items = value.as_array().unwrap();
                assert_eq!(items.len(), *len);
                items.iter().for_each(|v| encode_by_schema(item, v, out));
            }
            Schema::TupleStruct { items, .. } if items.len() == 1 => {
                encode_by_schema(&items[0], value, out)
            }
            Schema::Struct { fields, .. } => {
                for f in fields {
                    encode_by_schema(&f.ty, &value[&f.name], out);
                }
            }
            other => panic!("schema walker does not handle {other:?}"),
        }
    }

    fn check<M: Message + serde::Serialize + Default>(sample: M) {
        let schema = M::schema();
        assert!(schema.is_complete(), "{} schema is incomplete", M::TYPE);
        let json = serde_json::to_value(&sample).unwrap();
        let mut by_schema = Vec::new();
        encode_by_schema(&schema, &json, &mut by_schema);
        assert_eq!(by_schema, sample.to_bytes(), "{} schema mismatch", M::TYPE);
    }

    #[test]
    fn schemas_match_encoding() {
        let h = Header::new("base", 1_700_000_000_123);
        let v = Vec3 {
            x: 1.0,
            y: -2.0,
            z: 0.5,
        };
        check(Scalar(1.5));
        check(Text("hi".into()));
        check(Flag(true));
        check(Bytes(vec![1, 2, 255]));
        check(v);
        check(Pose::default());
        check(Twist {
            linear: v,
            angular: v,
        });
        check(JointState {
            position: vec![0.1],
            velocity: vec![],
            effort: vec![3.0, 4.0],
        });
        check(Transform {
            header: h.clone(),
            child_frame_id: "c".into(),
            translation: v,
            rotation: Quat::default(),
        });
        check(Transforms(vec![Transform::default(), Transform::default()]));
        check(Imu {
            header: h.clone(),
            ..Default::default()
        });
        check(LaserScan {
            header: h.clone(),
            ranges: vec![1.0, 2.0],
            angle_max: 3.0,
            ..Default::default()
        });
        check(Odometry {
            header: h.clone(),
            ..Default::default()
        });
        check(CameraInfo {
            header: h.clone(),
            width: 640,
            d: vec![0.1; 5],
            ..Default::default()
        });
        check(Joy {
            header: h.clone(),
            axes: vec![0.5],
            buttons: vec![-1, 300],
        });
        check(JointCommand {
            header: h,
            position: vec![1.0],
            ..Default::default()
        });
    }

    #[test]
    fn std_schemas_are_registered() {
        let all = crate::schema::registered();
        for ty in [
            "std/Scalar",
            "std/Image",
            "std/PointCloud",
            "std/Imu",
            "std/Transforms",
        ] {
            assert!(all.contains_key(ty), "{ty} not registered");
        }
        assert!(matches!(Image::schema(), Schema::Custom { .. }));
        assert_eq!(
            crate::schema::lookup("std/Pose").unwrap().hash,
            Pose::message_schema().hash
        );
    }
}
