//! Services and transient-local topics through a local Zenoh router, checked
//! at the level of key expressions, attachments and liveliness tokens
//! against what real rmw_zenoh nodes announce (tests/data/jazzy_tokens.txt).

use std::sync::mpsc;
use std::time::{Duration, Instant};

use ruro_ros2::discovery::{self, EntityKind};
use ruro_ros2::msgs::example_interfaces::srv::{
    AddTwoInts, AddTwoInts_Request, AddTwoInts_Response,
};
use ruro_ros2::msgs::std_msgs::msg::String as RosString;
use ruro_ros2::rmw::{Endpoint, EndpointConfig, Qos, attachment, parse_attachment};
use ruro_ros2::{RosService, cdr};
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
        domain: 0,
        node_name: node.into(),
        namespace: "/".into(),
        credentials: Default::default(),
    }
}

/// The part of a liveliness key after the node name: name, type, hash, QoS.
fn tail(key: &str) -> String {
    key.split('/').skip(9).collect::<Vec<_>>().join("/")
}

fn real_token(kind: &str, needle: &str) -> String {
    include_str!("data/jazzy_tokens.txt")
        .lines()
        .find(|l| l.contains(&format!("/{kind}/")) && l.contains(needle))
        .unwrap_or_else(|| panic!("no {kind} token for {needle}"))
        .to_owned()
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < Duration::from_secs(10), "{what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn services_answer_calls_like_rmw_zenoh() {
    let (router, endpoint) = router();
    let server = Endpoint::open(config(&endpoint, "add_two_ints_server")).unwrap();
    let client_node = Endpoint::open(config(&endpoint, "add_two_ints_client")).unwrap();

    let client = client_node
        .client::<AddTwoInts>("add_two_ints", Qos::services())
        .unwrap();
    assert!(!client.raw().server_available(Duration::from_millis(300)));
    let err = client
        .call(
            &AddTwoInts_Request { a: 1, b: 2 },
            Duration::from_millis(300),
        )
        .unwrap_err();
    assert!(err.to_string().contains("no reply"), "{err}");

    let _service = server
        .service::<AddTwoInts>("/add_two_ints", Qos::services(), |r| AddTwoInts_Response {
            sum: r.a + r.b,
        })
        .unwrap();
    assert!(client.raw().wait_for_server(Duration::from_secs(10)));

    // The tokens match a real add_two_ints_server's from the name on.
    let real = real_token("SS", "%add_two_ints/");
    wait_for("tokens", || {
        let es = discovery::entities(&router, 0, Duration::from_millis(300)).unwrap();
        let find = |kind| {
            es.iter()
                .find(|e| e.kind == kind && e.topic.as_deref() == Some("/add_two_ints"))
                .cloned()
        };
        match (find(EntityKind::Service), find(EntityKind::Client)) {
            (Some(s), Some(c)) => {
                let s_tail = format!(
                    "%add_two_ints/{}/{}/{}",
                    s.dds_type.unwrap(),
                    s.type_hash.unwrap(),
                    s.qos.unwrap()
                );
                assert_eq!(s_tail, tail(&real));
                assert_eq!(c.node, "add_two_ints_client");
                true
            }
            _ => false,
        }
    });

    let sum = client
        .call(&AddTwoInts_Request { a: 2, b: 3 }, Duration::from_secs(5))
        .unwrap();
    assert_eq!(sum.sum, 5);

    // A plain Zenoh get, as an rmw_zenoh client makes it: the reply
    // attachment echoes the sequence number and GID with a new timestamp.
    let gid = [0xab; 16];
    let replies = router
        .get(client.raw().key())
        .payload(cdr::to_vec(&AddTwoInts_Request { a: 40, b: 2 }))
        .attachment(attachment(77, 1, &gid))
        .target(zenoh::query::QueryTarget::AllComplete)
        .consolidation(zenoh::query::ConsolidationMode::None)
        .timeout(Duration::from_secs(5))
        .wait()
        .unwrap();
    let reply = replies.recv().unwrap();
    let sample = reply.result().unwrap();
    assert_eq!(sample.key_expr().as_str(), client.raw().key());
    let info = parse_attachment(&sample.attachment().unwrap().to_bytes()).unwrap();
    assert_eq!(info.sequence, 77);
    assert_eq!(info.publisher_gid, gid);
    assert!(info.source_timestamp_ns > 1);
    let resp: AddTwoInts_Response = cdr::from_slice(&sample.payload().to_bytes()).unwrap();
    assert_eq!(resp.sum, 42);
    assert_eq!(
        client.raw().key(),
        format!(
            "0/add_two_ints/example_interfaces::srv::dds_::AddTwoInts_/{}",
            AddTwoInts::TYPE_HASH
        )
    );

    // A request without an attachment gets no answer.
    let replies = router
        .get(client.raw().key())
        .payload(cdr::to_vec(&AddTwoInts_Request { a: 1, b: 1 }))
        .timeout(Duration::from_millis(500))
        .wait()
        .unwrap();
    assert!(replies.recv().map(|r| r.result().is_err()).unwrap_or(true));
}

#[test]
fn deferred_replies_and_concurrent_calls() {
    let (_router, endpoint) = router();
    let node = Endpoint::open(config(&endpoint, "deferred")).unwrap();
    let (tx, rx) = mpsc::channel();
    let _service = node
        .service_deferred::<AddTwoInts>("slow_add", Qos::services(), move |req| {
            let _ = tx.send(req);
        })
        .unwrap();
    let client = node
        .client::<AddTwoInts>("slow_add", Qos::services())
        .unwrap();
    assert!(client.raw().wait_for_server(Duration::from_secs(10)));
    let first = client
        .send(&AddTwoInts_Request { a: 1, b: 1 }, Duration::from_secs(10))
        .unwrap();
    let second = client
        .send(
            &AddTwoInts_Request { a: 10, b: 10 },
            Duration::from_secs(10),
        )
        .unwrap();
    assert!(first.try_recv().is_none(), "nothing answered yet");
    // Answer in reverse order from another thread.
    let mut pending: Vec<_> = (0..2)
        .map(|_| rx.recv_timeout(Duration::from_secs(5)).unwrap())
        .collect();
    assert_eq!(pending[0].info().sequence + 1, pending[1].info().sequence);
    std::thread::spawn(move || {
        while let Some(p) = pending.pop() {
            let sum = p.request.a + p.request.b;
            p.respond(&AddTwoInts_Response { sum }).unwrap();
        }
    })
    .join()
    .unwrap();
    assert_eq!(second.wait().unwrap().sum, 20);
    assert_eq!(first.wait().unwrap().sum, 2);
}

#[test]
fn transient_local_reaches_late_subscribers() {
    let (_router, endpoint) = router();
    let talker = Endpoint::open(config(&endpoint, "latcher")).unwrap();
    // Another session (peer mode), so history travels through the router.
    let listener = Endpoint::open(EndpointConfig {
        mode: "peer".into(),
        ..config(&endpoint, "late")
    })
    .unwrap();
    let publisher = talker
        .publisher::<RosString>("/latched", Qos::transient_local(2))
        .unwrap();
    for i in 0..3 {
        publisher
            .publish(&RosString {
                data: format!("m{i}"),
            })
            .unwrap();
    }
    std::thread::sleep(Duration::from_millis(300));

    let (tx, rx) = mpsc::channel();
    let _late = listener
        .subscriber::<RosString>("/latched", Qos::transient_local(2), move |m, _| {
            let _ = tx.send(m.unwrap().data);
        })
        .unwrap();
    let (vtx, vrx) = mpsc::channel();
    let _volatile = listener
        .subscriber::<RosString>("/latched", Qos::default(), move |m, _| {
            let _ = vtx.send(m.unwrap().data);
        })
        .unwrap();
    let got: Vec<String> = (0..2)
        .map(|_| rx.recv_timeout(Duration::from_secs(10)).expect("history"))
        .collect();
    assert_eq!(got, ["m1", "m2"], "the last `depth` samples, in order");
    assert!(vrx.recv_timeout(Duration::from_millis(500)).is_err());

    // New samples reach both.
    publisher.publish(&RosString { data: "m3".into() }).unwrap();
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "m3");
    assert_eq!(vrx.recv_timeout(Duration::from_secs(5)).unwrap(), "m3");
}

#[test]
fn every_node_describes_its_types() {
    use ruro_ros2::msgs::type_description_interfaces::srv::{
        GetTypeDescription, GetTypeDescription_Request,
    };
    let (_router, endpoint) = router();
    let _node = Endpoint::open(config(&endpoint, "describer")).unwrap();
    let asker = Endpoint::open(config(&endpoint, "asker")).unwrap();
    let client = asker
        .client::<GetTypeDescription>("/describer/get_type_description", Qos::services())
        .unwrap();
    assert!(client.raw().wait_for_server(Duration::from_secs(10)));
    let ask = |type_name: &str, type_hash: &str| {
        client
            .call(
                &GetTypeDescription_Request {
                    type_name: type_name.into(),
                    type_hash: type_hash.into(),
                    include_type_sources: false,
                },
                Duration::from_secs(5),
            )
            .unwrap()
    };

    let r = ask("geometry_msgs/msg/PoseStamped", "");
    assert!(r.successful, "{}", r.failure_reason);
    let d = &r.type_description;
    assert_eq!(
        d.type_description.type_name,
        "geometry_msgs/msg/PoseStamped"
    );
    let fields: Vec<&str> = d
        .type_description
        .fields
        .iter()
        .map(|f| f.name.as_str())
        .collect();
    assert_eq!(fields, ["header", "pose"]);
    // Referenced types, sorted by name, as in the hashed description.
    let refs: Vec<&str> = d
        .referenced_type_descriptions
        .iter()
        .map(|t| t.type_name.as_str())
        .collect();
    assert_eq!(
        refs,
        [
            "builtin_interfaces/msg/Time",
            "geometry_msgs/msg/Point",
            "geometry_msgs/msg/Pose",
            "geometry_msgs/msg/Quaternion",
            "std_msgs/msg/Header"
        ]
    );
    // Default values are given (Quaternion.w defaults to 1).
    let quat = &d.referenced_type_descriptions[3];
    assert_eq!(quat.fields[3].name, "w");
    assert_eq!(quat.fields[3].default_value, "1.0");

    // The hash is checked when given.
    use ruro_ros2::RosMessage;
    let string_hash = RosString::TYPE_HASH;
    assert!(ask("std_msgs/msg/String", string_hash).successful);
    let r = ask("std_msgs/msg/String", "RIHS01_00");
    assert!(
        !r.successful && r.failure_reason.contains("has hash"),
        "{}",
        r.failure_reason
    );
    let r = ask("no_such_pkg/msg/Thing", "");
    assert!(!r.successful, "unknown types are refused");
    let r = ask("not a type", "");
    assert!(
        r.failure_reason.contains("not a type name"),
        "{}",
        r.failure_reason
    );
}
