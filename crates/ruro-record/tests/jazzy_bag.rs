//! Import of a bag recorded by `ros2 bag record -s mcap` in ROS 2 Jazzy
//! (`data/jazzy_bag`): `/chatter` from demo_nodes_cpp's talker, `/rosout`,
//! and `/ints` (`example_interfaces/msg/Int64MultiArray`) and `/wide`
//! (`example_interfaces/msg/WString`) from `ros2 topic pub`. None of these
//! types has a ruro equivalent, so every message must come out as `ros2/Any`
//! with its bytes unchanged.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ruro_msg::{Message, Ros2Any};
use ruro_record::Recording;
use ruro_record::rosbag::{ImportOptions, import, schema_type_hash};
use ruro_ros2::dynamic;
use ruro_ros2::idl::TypeName;

fn bag() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/jazzy_bag")
}

/// The messages of the bag itself, per topic.
fn originals() -> BTreeMap<String, Vec<Vec<u8>>> {
    let rec = Recording::open(&bag().join("b1_0.mcap")).unwrap();
    let mut by_topic: BTreeMap<String, Vec<Vec<u8>>> = BTreeMap::new();
    for m in rec.messages().unwrap() {
        let m = m.unwrap();
        by_topic
            .entry(m.topic().to_owned())
            .or_default()
            .push(m.data.to_vec());
    }
    by_topic
}

#[test]
fn every_message_survives_byte_for_byte() {
    let out = std::env::temp_dir().join(format!("ruro-jazzy-bag-{}.mcap", std::process::id()));
    let report = import(&bag(), &out, &ImportOptions::default()).unwrap();
    let originals = originals();
    let counts: BTreeMap<&str, u64> = report
        .topics
        .iter()
        .map(|t| (t.ros_topic.as_str(), t.raw))
        .collect();
    // The counts metadata.yaml lists.
    assert_eq!(
        counts,
        [("/chatter", 3), ("/ints", 3), ("/rosout", 15), ("/wide", 2)]
            .into_iter()
            .collect::<BTreeMap<_, _>>()
    );
    for t in &report.topics {
        assert_eq!((t.converted, t.skipped), (0, 0), "{}", t.ros_topic);
        assert!(t.type_hash.starts_with("RIHS01_"), "{}", t.ros_topic);
    }

    let rec = Recording::open(&out).unwrap();
    let mut imported: BTreeMap<String, Vec<Ros2Any>> = BTreeMap::new();
    for m in rec.messages().unwrap() {
        let m = m.unwrap();
        assert_eq!(m.ty(), "ros2/Any");
        let topic = m.channel.metadata["ros2_topic"].clone();
        imported
            .entry(topic)
            .or_default()
            .push(Ros2Any::decode(&m.data).unwrap());
    }
    for (topic, msgs) in &imported {
        let bytes: Vec<Vec<u8>> = msgs.iter().map(|m| m.cdr.clone()).collect();
        assert_eq!(&bytes, &originals[topic], "{topic}");
    }

    // Decoded with the definitions the bag carries.
    let decode_all = |topic: &str| -> Vec<serde_json::Value> {
        let rec = Recording::open(&out).unwrap();
        let mut values = Vec::new();
        for m in rec.messages().unwrap() {
            let m = m.unwrap();
            if m.channel.metadata["ros2_topic"] != topic {
                continue;
            }
            let any = Ros2Any::decode(&m.data).unwrap();
            let name = TypeName::parse(&any.type_name).unwrap();
            let ty = dynamic::compile_schema(
                &name,
                &m.channel.metadata["ros2_schema_encoding"],
                &m.channel.metadata["ros2_schema"],
            )
            .unwrap();
            assert_eq!(
                ty.hash, any.type_hash,
                "{topic}: hash of the bag's definition"
            );
            let d = dynamic::decode(&ty, &any.cdr).unwrap();
            assert_eq!(d.encode().unwrap(), any.cdr, "{topic}: re-encoding");
            values.push(d.value.to_json());
        }
        values
    };
    let chatter = decode_all("/chatter");
    assert_eq!(chatter[0]["data"], "Hello World: 1");
    let ints = decode_all("/ints");
    assert_eq!(ints[0]["data"], serde_json::json!([1, -2]));
    assert_eq!(ints[0]["layout"]["dim"][0]["label"], "a");
    assert_eq!(ints[0]["layout"]["data_offset"], 1);
    assert_eq!(decode_all("/wide")[0]["data"], "hi");
    let rosout = decode_all("/rosout");
    assert!(rosout.iter().any(|v| v["name"] == "talker"), "{rosout:?}");
}

#[test]
fn hashes_computed_from_the_bag_schemas_match_the_recorded_ones() {
    let rec = Recording::open(&bag().join("b1_0.mcap")).unwrap();
    let info = rec.info().unwrap();
    assert_eq!(info.channels.len(), 4);
    let mut seen = std::collections::BTreeSet::new();
    for m in rec.messages().unwrap() {
        let m = m.unwrap();
        if !seen.insert(m.channel.id) {
            continue;
        }
        let recorded = &m.channel.metadata["topic_type_hash"];
        assert_eq!(
            &schema_type_hash(&m.channel).unwrap(),
            recorded,
            "{}",
            m.topic()
        );
    }
    assert_eq!(seen.len(), 4);
}
