//! `ros2/Any`: a ROS 2 message of any type, kept as its serialized bytes.

use serde::{Deserialize, Serialize};

use crate::schema::{Describe, Field, Schema};
use crate::{Preview, impl_postcard_message};

/// A ROS 2 message of any type, carried losslessly as the CDR bytes that
/// travel on a ROS 2 topic or sit in a rosbag2 file.
///
/// The generic ROS 2 bridge nodes (`ros2/sub`, `ros2/pub`) and imported bags
/// use it for types that have no ruro counterpart; `ros2/field` and
/// `ros2/decode` read fields out of it given the type's definition.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ros2Any {
    /// ROS type name, e.g. `sensor_msgs/msg/CompressedImage`.
    pub type_name: String,
    /// RIHS01 type hash, e.g. `RIHS01_df66…`; empty if unknown.
    pub type_hash: String,
    /// The serialized message, starting with its 4-byte encapsulation header.
    #[serde(with = "byte_vec")]
    pub cdr: Vec<u8>,
}
impl_postcard_message!(Ros2Any, "ros2/Any", preview: preview, schema);

impl Describe for Ros2Any {
    fn describe() -> Schema {
        Schema::structure(
            "Ros2Any",
            vec![
                Field::new("type_name", String::describe())
                    .doc("ROS type name, e.g. sensor_msgs/msg/CompressedImage."),
                Field::new("type_hash", String::describe()).doc("RIHS01 type hash."),
                Field::new("cdr", Vec::<u8>::describe())
                    .doc("CDR bytes, encapsulation header included."),
            ],
        )
    }
}

/// Decodes [`Ros2Any`] messages for previews. A crate that can decode ROS
/// types (the ROS 2 bridge nodes) submits one with `inventory::submit!`; the
/// first that returns a preview wins.
pub struct Ros2AnyPreviewer {
    pub preview: fn(&Ros2Any) -> Option<Preview>,
}

inventory::collect!(Ros2AnyPreviewer);

fn preview(m: &Ros2Any) -> Preview {
    for p in inventory::iter::<Ros2AnyPreviewer> {
        if let Some(preview) = (p.preview)(m) {
            return preview;
        }
    }
    Preview::Text(format!(
        "{} ({} bytes of CDR)",
        if m.type_name.is_empty() {
            "ROS 2 message"
        } else {
            &m.type_name
        },
        m.cdr.len()
    ))
}

/// Serializes a `Vec<u8>` as bytes: the same postcard encoding as a sequence
/// of `u8`, without going through the bytes one by one.
mod byte_vec {
    use serde::de::{SeqAccess, Visitor};
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(v)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Vec<u8>;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("bytes")
            }

            fn visit_bytes<E>(self, v: &[u8]) -> Result<Vec<u8>, E> {
                Ok(v.to_vec())
            }

            fn visit_byte_buf<E>(self, v: Vec<u8>) -> Result<Vec<u8>, E> {
                Ok(v)
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<u8>, A::Error> {
                let mut v = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(1 << 20));
                while let Some(b) = seq.next_element()? {
                    v.push(b);
                }
                Ok(v)
            }
        }
        d.deserialize_bytes(V)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Message;

    #[test]
    fn bytes_encode_like_a_sequence_of_u8() {
        #[derive(Serialize)]
        struct Plain {
            type_name: String,
            type_hash: String,
            cdr: Vec<u8>,
        }
        let m = Ros2Any {
            type_name: "std_msgs/msg/String".into(),
            type_hash: "RIHS01_x".into(),
            cdr: vec![0, 1, 0, 0, 1, 0, 0, 0, 0],
        };
        let plain = Plain {
            type_name: m.type_name.clone(),
            type_hash: m.type_hash.clone(),
            cdr: m.cdr.clone(),
        };
        assert_eq!(m.to_bytes(), postcard::to_allocvec(&plain).unwrap());
        assert_eq!(Ros2Any::decode(&m.to_bytes()).unwrap(), m);
        // JSON (used by tooling) round-trips too.
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(serde_json::from_str::<Ros2Any>(&json).unwrap(), m);
    }

    #[test]
    fn preview_without_a_decoder_names_the_type() {
        let m = Ros2Any {
            type_name: "x_msgs/msg/Y".into(),
            cdr: vec![0; 12],
            ..Default::default()
        };
        assert_eq!(
            m.preview(),
            Preview::Text("x_msgs/msg/Y (12 bytes of CDR)".into())
        );
        assert!(crate::schema::registered().contains_key("ros2/Any"));
    }
}
