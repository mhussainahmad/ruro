//! Publisher, subscriber and discovery through a local Zenoh router, checked
//! at the level of key expressions, attachments and liveliness tokens.

use std::sync::mpsc;
use std::time::Duration;

use ruro_ros2::msgs::std_msgs::msg::String as RosString;
use ruro_ros2::rmw::{Endpoint, EndpointConfig, Qos, parse_attachment};
use ruro_ros2::{RosMessage, discovery};
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

fn config(endpoint: &str, node: &str) -> EndpointConfig {
    EndpointConfig {
        connect: vec![endpoint.into()],
        mode: "client".into(),
        domain: 7,
        node_name: node.into(),
        namespace: "/".into(),
        credentials: Default::default(),
    }
}

#[test]
fn publish_subscribe_and_discover() {
    let (router, endpoint) = router();
    let talker = Endpoint::open(config(&endpoint, "talker")).unwrap();
    let listener = Endpoint::open(config(&endpoint, "listener")).unwrap();

    // A plain Zenoh subscriber sees what a ROS node would.
    let (raw_tx, raw_rx) = mpsc::channel();
    let _raw = router
        .declare_subscriber("7/chatter/**")
        .callback(move |s| {
            let _ = raw_tx.send((
                s.key_expr().to_string(),
                s.payload().to_bytes().to_vec(),
                s.attachment().map(|a| a.to_bytes().to_vec()),
            ));
        })
        .wait()
        .unwrap();

    let (tx, rx) = mpsc::channel();
    let _sub = listener
        .subscriber::<RosString>("chatter", Qos::default(), move |msg, info| {
            let _ = tx.send((msg.unwrap(), info));
        })
        .unwrap();
    let publisher = talker
        .publisher::<RosString>("/chatter", Qos::sensor_data())
        .unwrap();
    assert_eq!(
        publisher.raw().key(),
        format!(
            "7/chatter/std_msgs::msg::dds_::String_/{}",
            RosString::TYPE_HASH
        )
    );

    // Discovery sees both endpoints once their tokens propagate.
    let mut topics = Vec::new();
    for _ in 0..50 {
        topics = discovery::list_topics(&router, 7, Duration::from_millis(500)).unwrap();
        if topics
            .iter()
            .any(|t| t.name == "/chatter" && t.publishers == 1 && t.subscribers == 1)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let chatter = topics.iter().find(|t| t.name == "/chatter").expect("topic");
    assert_eq!(chatter.ros_type, "std_msgs/msg/String");
    assert_eq!(chatter.type_hash, RosString::TYPE_HASH);
    assert!(chatter.best_effort);
    assert!(chatter.nodes.contains(&"/talker".to_owned()));
    assert!(chatter.nodes.contains(&"/listener".to_owned()));
    let entities = discovery::entities(&router, 7, Duration::from_millis(500)).unwrap();
    assert!(
        entities
            .iter()
            .any(|e| e.kind == discovery::EntityKind::Node && e.node == "talker")
    );

    // Messages arrive decoded, with the attachment rmw_zenoh expects.
    let mut got = None;
    for i in 0..50 {
        publisher
            .publish(&RosString {
                data: format!("hello {i}"),
            })
            .unwrap();
        if let Ok(m) = rx.recv_timeout(Duration::from_millis(100)) {
            got = Some(m);
            break;
        }
    }
    let (msg, info) = got.expect("message delivered");
    assert!(msg.data.starts_with("hello "));
    let info = info.expect("attachment");
    assert!(info.sequence >= 1);
    assert_eq!(info.publisher_gid, publisher.raw().gid());

    let (key, payload, attachment) = raw_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(key, publisher.raw().key());
    assert_eq!(&payload[..4], &[0, 1, 0, 0], "CDR_LE encapsulation");
    let attachment = attachment.expect("attachment");
    assert_eq!(attachment.len(), 33);
    assert!(parse_attachment(&attachment).is_some());
}
