//! Bridge nodes talking through a local Zenoh router: a ruro publisher node
//! feeds a ruro subscriber node over the rmw_zenoh protocol, and the topics
//! show up in ROS 2 graph discovery.

use std::time::{Duration, Instant};

use ruro::msg::{Clock, Encoding, Image, Text, Twist, Vec3};
use ruro::node::testing::Harness;
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

/// Pushes `msg` into the publisher until the subscriber outputs something.
fn relay<T: ruro::msg::Message + Clone>(
    publisher: &mut Harness,
    subscriber: &mut Harness,
    msg: T,
) -> Vec<T> {
    let start = Instant::now();
    loop {
        publisher.push("input", msg.clone()).unwrap();
        publisher.step().unwrap();
        let got = subscriber.take::<T>("out").unwrap();
        if !got.is_empty() {
            return got;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "nothing arrived");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn bridges_relay_through_ros2_topics() {
    ruro_nodes_ros2::link();
    let (router, endpoint) = router();
    let params = |topic: &str| json!({ "topic": topic, "router": endpoint, "domain_id": 3 });

    let mut sub = Harness::new("ros2/sub-twist", params("/cmd_vel")).unwrap();
    sub.step().unwrap();
    let mut publ = Harness::new("ros2/pub-twist", params("/cmd_vel")).unwrap();
    let twist = Twist {
        linear: Vec3 {
            x: 0.5,
            y: 0.0,
            z: 0.0,
        },
        angular: Vec3 {
            x: 0.0,
            y: 0.0,
            z: 1.25,
        },
    };
    let got = relay(&mut publ, &mut sub, twist);
    assert_eq!(got[0], twist);

    // Images go through sensor_msgs/Image.
    let mut sub = Harness::new("ros2/sub-image", params("/camera/image_raw")).unwrap();
    sub.step().unwrap();
    let mut publ = Harness::new("ros2/pub-image", params("/camera/image_raw")).unwrap();
    let mut img = Image::new(4, 3, Encoding::Rgb8);
    img.data
        .iter_mut()
        .enumerate()
        .for_each(|(i, b)| *b = i as u8);
    assert_eq!(relay(&mut publ, &mut sub, img.clone())[0], img);

    // Simulated time goes through rosgraph_msgs/Clock.
    let mut sub = Harness::new("ros2/sub-clock", params("/clock")).unwrap();
    sub.step().unwrap();
    let mut publ = Harness::new("ros2/pub-clock", params("/clock")).unwrap();
    let t = Clock {
        time_ns: 12_000_000_345,
    };
    assert_eq!(relay(&mut publ, &mut sub, t)[0], t);

    // Strings, and a parameter change that moves the subscription.
    let mut sub = Harness::new("ros2/sub-string", params("/a")).unwrap();
    sub.step().unwrap();
    let mut publ = Harness::new("ros2/pub-string", params("/b")).unwrap();
    sub.set_param("topic", json!("/b")).unwrap();
    sub.step().unwrap();
    assert_eq!(
        relay(&mut publ, &mut sub, Text("hello ros".into()))[0],
        Text("hello ros".into())
    );

    // Liveliness declarations reach the router shortly after data can flow.
    let start = Instant::now();
    let topics = loop {
        let topics = ruro_ros2::list_topics(&router, 3, Duration::from_millis(300)).unwrap();
        let done = topics
            .iter()
            .any(|t| t.name == "/b" && t.publishers == 1 && t.subscribers == 1)
            && !topics.iter().any(|t| t.name == "/a");
        if done || start.elapsed() > Duration::from_secs(10) {
            break topics;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let names: Vec<(&str, &str)> = topics
        .iter()
        .map(|t| (t.name.as_str(), t.ros_type.as_str()))
        .collect();
    assert!(
        !names.iter().any(|(n, _)| *n == "/a"),
        "moved subscription is gone: {names:?}"
    );
    assert!(
        names.contains(&("/cmd_vel", "geometry_msgs/msg/Twist")),
        "{names:?}"
    );
    assert!(names.contains(&("/camera/image_raw", "sensor_msgs/msg/Image")));
    assert!(names.contains(&("/b", "std_msgs/msg/String")), "{names:?}");
    assert!(
        names.contains(&("/clock", "rosgraph_msgs/msg/Clock")),
        "{names:?}"
    );
    let cmd = topics.iter().find(|t| t.name == "/cmd_vel").unwrap();
    assert!(cmd.publishers >= 1 && cmd.subscribers >= 1);
}

/// Waits for the subscriber to output something.
fn received<T: ruro::msg::Message>(subscriber: &mut Harness) -> Vec<T> {
    let start = Instant::now();
    loop {
        let got = subscriber.take::<T>("out").unwrap();
        if !got.is_empty() {
            return got;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "nothing arrived");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn twist_publisher_sends_zero_while_stopped() {
    ruro_nodes_ros2::link();
    let (_router, endpoint) = router();
    let params = json!({ "topic": "/estop_vel", "router": endpoint, "domain_id": 4 });
    let mut sub = Harness::new("ros2/sub-twist", params.clone()).unwrap();
    sub.step().unwrap();
    let mut publ = Harness::new("ros2/pub-twist", params).unwrap();
    let moving = |x: f64| Twist {
        linear: Vec3 { x, y: 0.0, z: 0.0 },
        angular: Vec3::default(),
    };
    let zero = Twist::default();
    // Discovery has finished once a message gets through.
    relay(&mut publ, &mut sub, moving(0.5));
    std::thread::sleep(Duration::from_millis(100));
    sub.take::<Twist>("out").unwrap();

    publ.set_estop(true);
    assert_eq!(received::<Twist>(&mut sub), [zero], "zero on the stop");

    // Input while stopped: a zero goes out in its place.
    for _ in 0..3 {
        publ.push("input", moving(1.0)).unwrap();
        publ.step().unwrap();
    }
    std::thread::sleep(Duration::from_millis(200));
    let got = sub.take::<Twist>("out").unwrap();
    assert_eq!(got, [zero; 3]);

    // Input queued during the stop is not forwarded after the release.
    publ.push("input", moving(2.0)).unwrap();
    publ.set_estop(false);
    publ.step().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(sub.take::<Twist>("out").unwrap().is_empty());

    assert_eq!(relay(&mut publ, &mut sub, moving(0.25))[0], moving(0.25));
}

#[test]
fn every_bridge_kind_is_registered() {
    ruro_nodes_ros2::link();
    let reg = ruro::node::registry();
    for t in [
        "twist",
        "pose-stamped",
        "image",
        "camera-info",
        "laser-scan",
        "imu",
        "joint-state",
        "joy",
        "point-cloud2",
        "odometry",
        "tf",
        "float64",
        "string",
        "bool",
        "clock",
    ] {
        for dir in ["sub", "pub"] {
            let kind = format!("ros2/{dir}-{t}");
            let d = reg.get(&kind).unwrap_or_else(|| panic!("{kind} missing"));
            assert!(d.param("topic").is_some());
        }
    }
    let sub = reg.get("ros2/sub-laser-scan").unwrap();
    assert_eq!(sub.outputs[0].ty, "std/LaserScan");
    assert_eq!(
        sub.doc,
        "Subscribes to a ROS 2 `sensor_msgs/LaserScan` topic and outputs each message."
    );
    // Command publishers document their e-stop behaviour.
    let twist = reg.get("ros2/pub-twist").unwrap();
    assert!(
        twist.doc.contains("\n\nE-stop: a zero twist"),
        "{}",
        twist.doc
    );
    assert!(
        !reg.get("ros2/pub-joint-state")
            .unwrap()
            .doc
            .contains("E-stop")
    );
}

/// A router that, like a ruro daemon started with a token, only accepts
/// sessions that present it.
fn router_with_token(token: &str) -> (zenoh::Session, String, std::path::PathBuf) {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let endpoint = format!("tcp/127.0.0.1:{port}");
    let dictionary = std::env::temp_dir().join(format!("ruro-ros2-auth-{port}"));
    std::fs::write(&dictionary, format!("ruro:{token}\n")).unwrap();
    let mut c = zenoh::Config::default();
    let mut set = |k: &str, v: String| c.insert_json5(k, &v).unwrap();
    set("mode", "\"router\"".into());
    set("listen/endpoints", format!("[\"{endpoint}\"]"));
    set("scouting/multicast/enabled", "false".into());
    set("transport/auth/usrpwd/user", "\"ruro\"".into());
    set("transport/auth/usrpwd/password", format!("{token:?}"));
    set(
        "transport/auth/usrpwd/dictionary_file",
        format!("{:?}", dictionary.display().to_string()),
    );
    (zenoh::open(c).wait().unwrap(), endpoint, dictionary)
}

#[test]
fn bridges_present_a_token_to_routers_that_require_one() {
    ruro_nodes_ros2::link();
    let (_router, endpoint, dictionary) = router_with_token("open sesame");
    let params = |token: &str| json!({ "topic": "/secure", "router": endpoint, "domain_id": 5, "token": token });
    let refused = Harness::new("ros2/sub-float64", params("wrong")).and_then(|mut h| h.step());
    let err = format!("{:#}", refused.expect_err("a wrong token is refused"));
    assert!(err.contains("cannot reach Zenoh"), "{err}");

    let mut sub = Harness::new("ros2/sub-float64", params("open sesame")).unwrap();
    sub.step().unwrap();
    let mut publ = Harness::new("ros2/pub-float64", params("open sesame")).unwrap();
    let got = relay(&mut publ, &mut sub, ruro::msg::Scalar(2.5));
    assert_eq!(got[0], ruro::msg::Scalar(2.5));
    let _ = std::fs::remove_file(dictionary);

    // A token given as a parameter is required: a router that does not
    // authenticate refuses it. (One from the environment, meant for the
    // daemon, is dropped instead.)
    let (_plain, endpoint) = router();
    let params = json!({ "topic": "/open", "router": endpoint, "domain_id": 6, "token": "given" });
    assert!(
        Harness::new("ros2/sub-float64", params)
            .and_then(|mut h| h.step())
            .is_err()
    );
}
