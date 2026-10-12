//! Schemas derived for user messages.

use ruro::msg::schema::{Field, VariantPayload};
use ruro::msg::{Describe as _, Message as _, Schema};
use ruro::prelude::*;

#[derive(Describe, Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct BoundingBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Describe, Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub enum Label {
    #[default]
    Unknown,
    Class(u32),
    Named {
        name: String,
    },
}

/// Not describable: its layout shows up as opaque.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Blob(pub u8);

#[derive(Message, Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[message(name = "test/Detections")]
pub struct Detections {
    /// Where the camera was.
    pub header: Header,
    pub boxes: Vec<BoundingBox>,
    pub labels: Vec<Label>,
    #[serde(skip)]
    pub cache: Option<u64>,
    pub blob: Blob,
}

#[derive(Message, Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[message(name = "test/Raw", opaque)]
pub struct Raw(pub Vec<u8>);

#[test]
fn derived_schema_follows_the_struct() {
    let Schema::Struct { name, fields } = Detections::schema() else {
        panic!("expected a struct schema");
    };
    assert_eq!(name, "Detections");
    let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(
        names,
        ["header", "boxes", "labels", "blob"],
        "skipped field omitted"
    );
    assert_eq!(fields[0].doc, "Where the camera was.");
    assert_eq!(fields[0].ty, Header::describe());
    let Schema::Seq { item } = &fields[1].ty else {
        panic!()
    };
    assert_eq!(
        **item,
        Schema::structure(
            "BoundingBox",
            ["x", "y", "w", "h"]
                .iter()
                .map(|n| Field::new(*n, Schema::F32))
                .collect()
        )
    );
    let Schema::Seq { item } = &fields[2].ty else {
        panic!()
    };
    let Schema::Enum { variants, .. } = &**item else {
        panic!()
    };
    assert_eq!(variants.len(), 3);
    assert!(matches!(variants[2].payload, VariantPayload::Struct { .. }));
    assert!(matches!(fields[3].ty, Schema::Opaque { .. }));
    assert!(!Detections::schema().is_complete());
}

#[test]
fn derived_schemas_are_registered() {
    let all = ruro::msg::schema::registered();
    let d = all.get("test/Detections").expect("registered");
    assert_eq!(d.hash, Detections::message_schema().hash);
    assert!(
        !all.contains_key("test/Raw"),
        "opaque messages are not registered"
    );
    assert_eq!(Raw::schema(), Schema::opaque("test/Raw"));
}

#[test]
fn derived_messages_still_roundtrip() {
    let d = Detections {
        boxes: vec![BoundingBox::default()],
        labels: vec![Label::Class(3), Label::Named { name: "cup".into() }],
        ..Default::default()
    };
    assert_eq!(Detections::decode(&d.to_bytes()).unwrap(), d);
}
