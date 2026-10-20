//! The generic bridge nodes through a local Zenoh router: messages of types
//! ruro has no Rust type for pass through unchanged, and `ros2/field` and
//! `ros2/decode` read them.

use std::time::{Duration, Instant};

use ruro::msg::{Ros2Any, Scalar, Text};
use ruro::node::testing::Harness;
use ruro_ros2::rmw::{Endpoint, EndpointConfig, Qos};
use serde_json::json;
use zenoh::Wait;

fn router() -> (zenoh::Session, String) {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let endpoint = format!("tcp/127.0.0.1:{port}");
    let mut c = zenoh::Config::default();
    c.insert_json5("mode", "\"router\"").unwrap();
    c.insert_json5("listen/endpoints", &format!("[\"{endpoint}\"]"))
        .unwrap();
    c.insert_json5("scouting/multicast/enabled", "false")
        .unwrap();
    (zenoh::open(c).wait().unwrap(), endpoint)
}

fn interfaces() -> String {
    concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../crates/ruro-ros2/tests/fixtures/interfaces"
    )
    .to_owned()
}

/// A message serialized by ROS 2 itself, from the ruro-ros2 fixtures.
fn fixture(ty: &str, nth: usize) -> Vec<u8> {
    let text = include_str!("../../../crates/ruro-ros2/tests/data/cdr_fixtures.jsonl");
    let line = text
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .filter(|f| f["type"] == ty)
        .nth(nth)
        .unwrap();
    let hex = line["cdr"].as_str().unwrap();
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

fn wait_for<T: ruro::msg::Message>(h: &mut Harness, port: &str, mut poke: impl FnMut()) -> Vec<T> {
    let start = Instant::now();
    loop {
        poke();
        let got = h.take::<T>(port).unwrap();
        if !got.is_empty() {
            return got;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "nothing arrived");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn any_type_passes_through_unchanged_and_fields_are_readable() {
    ruro_nodes_ros2::link();
    let (_router, endpoint) = router();
    let ty = "ruro_test_msgs/msg/Nested";
    let bytes = fixture(ty, 2);
    let mut sub = Harness::new(
        "ros2/sub",
        json!({
            "topic": "/nested", "type": ty, "interfaces": interfaces(),
            "router": endpoint, "domain_id": 11, "qos": "best_effort", "depth": 3
        }),
    )
    .unwrap();
    sub.step().unwrap();
    // The publisher takes type and hash from its input.
    let mut publ = Harness::new(
        "ros2/pub",
        json!({ "topic": "/nested", "router": endpoint, "domain_id": 11, "qos": "best_effort", "depth": 3 }),
    )
    .unwrap();
    let hash = "RIHS01_424a040638974c58f649be9275f97dd896a69c7044222dd90236a87216252b2a";
    let msg = Ros2Any {
        type_name: ty.into(),
        type_hash: hash.into(),
        cdr: bytes.clone(),
    };
    let got = wait_for::<Ros2Any>(&mut sub, "out", || {
        publ.push("input", msg.clone()).unwrap();
        publ.step().unwrap();
    });
    assert_eq!(got[0], msg, "same type, hash and bytes");

    // A message of another type is not published on this topic.
    std::thread::sleep(Duration::from_millis(200));
    sub.take::<Ros2Any>("out").unwrap();
    publ.push(
        "input",
        Ros2Any {
            type_name: "std_msgs/msg/String".into(),
            type_hash: String::new(),
            cdr: vec![0, 1, 0, 0, 1, 0, 0, 0, 0],
        },
    )
    .unwrap();
    publ.step().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(sub.take::<Ros2Any>("out").unwrap().is_empty());

    // Fields of the received message.
    let decoded = {
        let mut lib = ruro_ros2::search::TypeLibrary::bundled();
        lib.add_dir(std::path::Path::new(&interfaces()));
        let t = lib.resolve_str(ty).unwrap();
        ruro_ros2::dynamic::decode(&t, &bytes).unwrap().value
    };
    let mut field = Harness::new(
        "ros2/field",
        json!({ "path": "two[1].i16", "interfaces": interfaces() }),
    )
    .unwrap();
    field.push("input", got[0].clone()).unwrap();
    field.step().unwrap();
    let expected = decoded.get("two").unwrap();
    let ruro_ros2::dynamic::Value::Array(two) = expected else {
        panic!()
    };
    let ruro_ros2::dynamic::Value::Message(second) = two.get(1).unwrap() else {
        panic!()
    };
    let i16 = second.get("i16").unwrap().as_f64().unwrap();
    assert_eq!(field.take::<Scalar>("value").unwrap(), [Scalar(i16)]);
    field.set_param("path", json!("header.frame_id")).unwrap();
    field.push("input", got[0].clone()).unwrap();
    field.step().unwrap();
    let frame = decoded.get("header").unwrap();
    let ruro_ros2::dynamic::Value::Message(h) = frame else {
        panic!()
    };
    assert_eq!(
        field.take::<Text>("text").unwrap(),
        [Text(
            h.get("frame_id").unwrap().as_str().unwrap().to_owned()
        )]
    );

    let mut decode = Harness::new("ros2/decode", json!({ "interfaces": interfaces() })).unwrap();
    decode.push("input", got[0].clone()).unwrap();
    decode.step().unwrap();
    let yaml = decode.take::<Text>("out").unwrap();
    assert_eq!(yaml[0].0, decoded.to_yaml(Some(64)));
    assert!(
        yaml[0].0.starts_with("header:\n  stamp:\n"),
        "{}",
        yaml[0].0
    );
}

#[test]
fn a_type_without_a_definition_uses_the_announced_hash() {
    ruro_nodes_ros2::link();
    let (_router, endpoint) = router();
    // A ROS node publishing a type nobody here has a definition for.
    let talker = Endpoint::open(EndpointConfig {
        connect: vec![endpoint.clone()],
        domain: 12,
        node_name: "talker".into(),
        ..EndpointConfig::default()
    })
    .unwrap();
    let hash = format!("RIHS01_{}", "ab".repeat(32));
    let publisher = talker
        .raw_publisher(
            "/mystery",
            "nope_msgs::msg::dds_::Thing_",
            &hash,
            Qos::default(),
        )
        .unwrap();
    let mut sub = Harness::new(
        "ros2/sub",
        json!({ "topic": "/mystery", "type": "nope_msgs/msg/Thing", "router": endpoint, "domain_id": 12 }),
    )
    .unwrap();
    let payload = vec![0, 1, 0, 0, 42, 0, 0, 0];
    let start = Instant::now();
    let got = loop {
        // Steps fail and retry until the publisher's announcement is seen.
        let _ = sub.step();
        publisher.put(payload.clone()).unwrap();
        let got = sub.take::<Ros2Any>("out").unwrap();
        if !got.is_empty() {
            break got;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "nothing arrived");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(got[0].type_hash, hash);
    assert_eq!(got[0].cdr, payload);
    // Without a definition the preview just names the type.
    assert_eq!(
        ruro::msg::Message::preview(&got[0]),
        ruro::msg::Preview::Text("nope_msgs/msg/Thing (8 bytes of CDR)".into())
    );
}

#[test]
fn publishers_appearing_later_with_another_hash_are_followed() {
    ruro_nodes_ros2::link();
    let (_router, endpoint) = router();
    // Subscribed before any publisher exists: the local hash.
    let mut sub = Harness::new(
        "ros2/sub",
        json!({ "topic": "/late", "type": "std_msgs/msg/String", "router": endpoint, "domain_id": 13 }),
    )
    .unwrap();
    sub.step().unwrap();
    // Then a ROS node with another version of the definition.
    let talker = Endpoint::open(EndpointConfig {
        connect: vec![endpoint.clone()],
        domain: 13,
        node_name: "talker".into(),
        ..EndpointConfig::default()
    })
    .unwrap();
    let hash = format!("RIHS01_{}", "cd".repeat(32));
    let publisher = talker
        .raw_publisher(
            "/late",
            "std_msgs::msg::dds_::String_",
            &hash,
            Qos::default(),
        )
        .unwrap();
    let payload = vec![0, 1, 0, 0, 3, 0, 0, 0, b'h', b'i', 0];
    let start = Instant::now();
    let got = loop {
        // Each step (1 Hz in a graph) looks at the publishers again.
        sub.step().unwrap();
        publisher.put(payload.clone()).unwrap();
        let got = sub.take::<Ros2Any>("out").unwrap();
        if !got.is_empty() {
            break got;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "nothing arrived");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(got[0].type_hash, hash);
    assert_eq!(got[0].cdr, payload);
}

#[test]
fn generic_kinds_are_registered() {
    ruro_nodes_ros2::link();
    let reg = ruro::node::registry();
    for kind in ["ros2/sub", "ros2/pub", "ros2/field", "ros2/decode"] {
        let d = reg.get(kind).unwrap_or_else(|| panic!("{kind} missing"));
        if kind == "ros2/sub" || kind == "ros2/pub" {
            assert!(d.param("type").is_some(), "{kind} has a `type` parameter");
            assert!(d.param("type_hash").is_some());
        }
    }
}
