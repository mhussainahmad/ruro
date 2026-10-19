//! Code generated for a user package that builds on the bundled ones: the
//! checked-in output compiles (it is a module of this test), round-trips
//! through CDR, and matches what the generator produces today.

use std::collections::BTreeMap;
use std::path::Path;

use ruro_ros2::RosMessage;
use ruro_ros2::cdr::{from_slice, to_vec};

#[rustfmt::skip]
#[path = "fixtures/demo_msgs.rs"]
mod demo;

use demo::demo_msgs::msg::{Detail, Detection};
use demo::demo_msgs::srv::{Lookup_Request, Lookup_Response};

fn generate() -> String {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/demo_msgs");
    let mut interfaces = ruro_ros2::bundled_interfaces();
    interfaces.load_package(&dir).unwrap();
    let externs: BTreeMap<String, String> = ruro_ros2::bundled_packages()
        .into_iter()
        .map(|p| (p.clone(), format!("ruro_ros2::msgs::{p}")))
        .collect();
    ruro_ros2::codegen::generate(
        &interfaces,
        &ruro_ros2::codegen::Options {
            packages: vec!["demo_msgs".into()],
            root: "crate::demo".into(),
            runtime: "ruro_ros2".into(),
            externs,
        },
    )
    .unwrap()
}

#[test]
fn fixture_is_current() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/demo_msgs.rs");
    let code = generate();
    if std::env::var_os("RURO_REGEN").is_some() {
        std::fs::write(&path, &code).unwrap();
        return;
    }
    assert!(
        std::fs::read_to_string(&path).unwrap() == code,
        "fixture is stale; regenerate with RURO_REGEN=1"
    );
}

#[test]
fn defaults_constants_and_roundtrip() {
    let d = Detection::default();
    assert_eq!(d.kind, Detection::KIND_BOX);
    assert_eq!(d.label, "unknown");
    assert_eq!(d.scores, [0.1, 0.2, 0.3, 0.4]);
    assert_eq!(Detection::TYPE_NAME, "demo_msgs/msg/Detection");

    let mut d = Detection {
        kind: Detection::KIND_POLY,
        blob: vec![1, 2, 3],
        ..Default::default()
    };
    d.header.frame_id = "cam".into();
    d.corners.push(ruro_ros2::msgs::geometry_msgs::msg::Point {
        x: 1.0,
        y: 2.0,
        z: 3.0,
    });
    d.detail = Detail {
        r#type: -4,
        flags: vec![true, false],
    };
    d.extra.push(Detail::default());
    assert_eq!(from_slice::<Detection>(&to_vec(&d)).unwrap(), d);

    let req = Lookup_Request { name: "x".into() };
    assert_eq!(from_slice::<Lookup_Request>(&to_vec(&req)).unwrap(), req);
    assert_eq!(Lookup_Request::TYPE_NAME, "demo_msgs/srv/Lookup_Request");
    let resp = Lookup_Response { ok: true, found: d };
    assert_eq!(from_slice::<Lookup_Response>(&to_vec(&resp)).unwrap(), resp);
}

#[test]
fn wstring_arrays_encode_like_the_dynamic_codec() {
    use demo::demo_msgs::msg::Names;
    use ruro_ros2::dynamic::{MessageType, decode};
    use ruro_ros2::idl::TypeName;
    let n = Names {
        title: "é".into(),
        names: vec!["a".into(), "😀".into()],
        pair: ["x".into(), String::new()],
        short: vec!["ok".into()],
    };
    let bytes = to_vec(&n);
    assert_eq!(from_slice::<Names>(&bytes).unwrap(), n);
    // The dynamic codec is checked against bytes ROS 2 itself serialized.
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/demo_msgs");
    let mut interfaces = ruro_ros2::bundled_interfaces();
    interfaces.load_package(&dir).unwrap();
    let ty = MessageType::compile(
        &interfaces,
        &TypeName::parse("demo_msgs/msg/Names").unwrap(),
    )
    .unwrap();
    let value = decode(&ty, &bytes).unwrap();
    assert_eq!(value.value.encode().unwrap(), bytes);
    let json = value.value.to_json();
    assert_eq!(json["names"][1], "😀");
    assert_eq!(json["pair"][0], "x");
}
