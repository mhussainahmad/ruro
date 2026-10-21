//! Service nodes with real ROS 2 tools (Jazzy, `rmw_zenoh_cpp`). Skipped
//! unless `RURO_ROS2_CONTAINER` names a script that runs a shell command in
//! a ROS 2 environment with host networking and honours `ZENOH_ROUTER`
//! (see `crates/ruro-ros2/tests/ros2_interop.rs`).

use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ruro::msg::Flag;
use ruro::node::testing::Harness;
use serde_json::json;
use zenoh::Wait;

#[test]
fn ros_calls_a_serve_trigger_node() {
    let Some(script) = std::env::var("RURO_ROS2_CONTAINER")
        .ok()
        .filter(|s| !s.is_empty())
    else {
        eprintln!("skipped: set RURO_ROS2_CONTAINER to a ROS 2 run script to test interop");
        return;
    };
    ruro_nodes_ros2::link();
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
    let _router = zenoh::open(c).wait().unwrap();

    let mut server = Harness::new(
        "ros2/serve-trigger",
        json!({ "service": "/start", "router": endpoint, "domain_id": 0, "message": "started" }),
    )
    .unwrap();
    server.step().unwrap();
    let child = Command::new(&script)
        .arg("timeout 120 ros2 service call /start std_srvs/srv/Trigger '{}'")
        .env("ZENOH_ROUTER", &endpoint)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let out = child.wait_with_output().unwrap();
        let _ = tx.send(String::from_utf8_lossy(&out.stdout).into_owned());
    });
    let start = Instant::now();
    let text = loop {
        server.step().unwrap();
        if let Ok(text) = rx.try_recv() {
            break text;
        }
        assert!(start.elapsed() < Duration::from_secs(150), "no answer");
        std::thread::sleep(Duration::from_millis(20));
    };
    eprintln!("{text}");
    assert!(
        text.contains("success=True") && text.contains("message='started'"),
        "{text}"
    );
    assert_eq!(server.take::<Flag>("triggered").unwrap(), [Flag(true)]);
}

#[test]
fn a_ros_clock_drives_sub_clock() {
    let Some(script) = std::env::var("RURO_ROS2_CONTAINER")
        .ok()
        .filter(|s| !s.is_empty())
    else {
        eprintln!("skipped: set RURO_ROS2_CONTAINER to a ROS 2 run script to test interop");
        return;
    };
    ruro_nodes_ros2::link();
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
    let _router = zenoh::open(c).wait().unwrap();

    let mut clock = Harness::new(
        "ros2/sub-clock",
        json!({ "router": endpoint, "domain_id": 0 }),
    )
    .unwrap();
    clock.step().unwrap();
    // What Gazebo's bridge or `ros2 bag play --clock` publish.
    let mut child = Command::new(&script)
        .arg(
            "timeout 120 ros2 topic pub -r 20 /clock rosgraph_msgs/msg/Clock \
             '{clock: {sec: 1234, nanosec: 5}}'",
        )
        .env("ZENOH_ROUTER", &endpoint)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let start = Instant::now();
    let got = loop {
        let got = clock.take::<ruro::msg::Clock>("out").unwrap();
        if !got.is_empty() {
            break got;
        }
        assert!(start.elapsed() < Duration::from_secs(120), "no clock");
        std::thread::sleep(Duration::from_millis(50));
    };
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(got[0].time_ns, 1_234_000_000_005);
}
