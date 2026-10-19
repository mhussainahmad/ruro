//! The dynamic codec against bytes written by ROS 2 itself.
//!
//! `data/cdr_fixtures.jsonl` holds, for every bundled type, the types in
//! `fixtures/interfaces` and a few more, messages serialized by ROS 2 Jazzy
//! (`rclpy.serialization.serialize_message` with rmw_zenoh_cpp) together with
//! the values ROS reads back from those bytes. Each must decode to the same
//! values and re-encode to the same bytes. `data/jazzy_fixture_hashes.txt`
//! holds the RIHS01 hashes rosidl generated for those types.

use std::path::Path;

use ruro_ros2::dynamic::{self, MessageValue};
use ruro_ros2::search::TypeLibrary;

fn library() -> TypeLibrary {
    let mut lib = TypeLibrary::bundled();
    lib.add_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/interfaces"));
    lib
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn ros_serialized_messages_decode_and_roundtrip_byte_for_byte() {
    let mut lib = library();
    let mut checked = 0;
    let mut types = std::collections::BTreeSet::new();
    let mut with_garbage = 0;
    for line in include_str!("data/cdr_fixtures.jsonl").lines() {
        let fixture: serde_json::Value = serde_json::from_str(line).unwrap();
        let ty = fixture["type"].as_str().unwrap();
        let bytes = unhex(fixture["cdr"].as_str().unwrap());
        let t = lib
            .resolve_str(ty)
            .unwrap_or_else(|e| panic!("{ty}: {e:#}"));
        let decoded = dynamic::decode(&t, &bytes).unwrap_or_else(|e| panic!("{ty}: {e}"));
        assert_eq!(
            decoded.value.to_json(),
            fixture["value"],
            "{ty}: decoded value differs from what ROS read"
        );
        assert_eq!(
            decoded.encode().unwrap(),
            bytes,
            "{ty}: re-encoding differs"
        );
        if !decoded.padding.is_empty() || !decoded.tail.is_empty() {
            with_garbage += 1;
        }
        // A clean encoding decodes to the same value.
        let clean = decoded.value.encode().unwrap();
        assert_eq!(
            dynamic::decode(&t, &clean).unwrap().value,
            decoded.value,
            "{ty}"
        );
        // So does a message rebuilt from the JSON form.
        let rebuilt = MessageValue::from_json(&t, &fixture["value"]).unwrap();
        assert_eq!(rebuilt.encode().unwrap(), clean, "{ty}: from_json");
        types.insert(ty.to_owned());
        checked += 1;
    }
    assert!(lib.warnings().is_empty(), "{:?}", lib.warnings());
    // 113 bundled types and 12 others, three messages each.
    assert!(types.len() >= 125, "{} types", types.len());
    assert_eq!(checked, types.len() * 3);
    // ROS leaves padding and trailing bytes uninitialised; the fixtures have
    // such messages, which is what makes the byte comparison meaningful.
    assert!(
        with_garbage > 10,
        "{with_garbage} messages with non-zero padding"
    );
}

#[test]
fn type_hashes_match_rosidl() {
    let mut lib = library();
    let mut checked = 0;
    for line in include_str!("data/jazzy_fixture_hashes.txt")
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let (name, expected) = line.split_once(' ').unwrap();
        let t = lib
            .resolve_str(name)
            .unwrap_or_else(|e| panic!("{name}: {e:#}"));
        assert_eq!(t.hash, expected, "{name}");
        checked += 1;
    }
    assert!(checked >= 180, "{checked}");
}

#[test]
fn constants_and_defaults_of_the_test_package() {
    let mut lib = library();
    let p = lib.resolve_str("ruro_test_msgs/msg/Primitives").unwrap();
    use dynamic::Value;
    assert_eq!(p.constant("SMALL"), Some(&Value::I8(-3)));
    assert_eq!(p.constant("BIG"), Some(&Value::U32(4_000_000_000)));
    assert_eq!(p.constant("YES"), Some(&Value::Bool(true)));
    assert_eq!(
        p.constant("QUOTED").and_then(Value::as_str),
        Some("it's \"q\"")
    );
    // The first fixture of each type is the default-constructed message.
    let first = include_str!("data/cdr_fixtures.jsonl")
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|f| f["type"] == "ruro_test_msgs/msg/Primitives")
        .unwrap();
    assert_eq!(p.default_value().to_json(), first["value"]);
    assert_eq!(
        p.default_value().encode().unwrap(),
        dynamic::decode(&p, &unhex(first["cdr"].as_str().unwrap()))
            .unwrap()
            .value
            .encode()
            .unwrap()
    );
    let a = lib.resolve_str("ruro_test_msgs/msg/Arrays").unwrap();
    let first = include_str!("data/cdr_fixtures.jsonl")
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|f| f["type"] == "ruro_test_msgs/msg/Arrays")
        .unwrap();
    assert_eq!(a.default_value().to_json(), first["value"]);
}

#[test]
fn idl_files_generated_by_rosidl_give_the_same_types() {
    // fixtures/idl holds the .idl files rosidl generated for ruro_test_msgs.
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/idl");
    let mut lib = library();
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        let specs = ruro_ros2::idl_file::parse_idl(&text).unwrap();
        for spec in &specs {
            lib.resolve(&spec.name).unwrap();
        }
        let mut from_idl = lib.interfaces().clone();
        for spec in specs.iter().cloned() {
            from_idl.add(spec);
        }
        for spec in &specs {
            let from_msg = lib.resolve(&spec.name).unwrap();
            let t = dynamic::MessageType::compile(&from_idl, &spec.name).unwrap();
            assert_eq!(t.hash, from_msg.hash, "{}", spec.name);
            assert_eq!(t.constants, from_msg.constants, "{}", spec.name);
            assert_eq!(
                t.default_value().to_json(),
                from_msg.default_value().to_json(),
                "{}",
                spec.name
            );
            checked += 1;
        }
    }
    // 4 messages, a request and response, goal, result and feedback.
    assert_eq!(checked, 9);
}
