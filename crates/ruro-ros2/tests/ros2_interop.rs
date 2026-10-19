//! Interoperability with real ROS 2 nodes (Jazzy, `rmw_zenoh_cpp`).
//!
//! Skipped unless `RURO_ROS2_CONTAINER` names a script that runs a shell
//! command in a ROS 2 environment with host networking and honours
//! `ZENOH_ROUTER` (the router endpoint the ROS nodes connect to), e.g. a
//! wrapper around `podman run --network host … bash -c "source
//! /opt/ros/jazzy/setup.bash; $*"`. Each test starts its own Zenoh router
//! in this process and runs ROS nodes against it.

use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ruro_ros2::msgs::example_interfaces::srv::{
    AddTwoInts, AddTwoInts_Request, AddTwoInts_Response,
};
use ruro_ros2::msgs::geometry_msgs::msg::TransformStamped;
use ruro_ros2::msgs::tf2_msgs::msg::TFMessage;
use ruro_ros2::rmw::{Endpoint, EndpointConfig, Qos};
use zenoh::Wait;

/// The container script, or `None` (with a message) to skip.
fn container() -> Option<String> {
    match std::env::var("RURO_ROS2_CONTAINER") {
        Ok(s) if !s.is_empty() => Some(s),
        _ => {
            eprintln!("skipped: set RURO_ROS2_CONTAINER to a ROS 2 run script to test interop");
            None
        }
    }
}

struct Ros {
    script: String,
    endpoint: String,
    _router: zenoh::Session,
}

impl Ros {
    fn start() -> Option<Self> {
        let script = container()?;
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
        let router = zenoh::open(c).wait().unwrap();
        Some(Self {
            script,
            endpoint,
            _router: router,
        })
    }

    /// Runs `cmd` in the ROS environment, stopped by `timeout` inside the
    /// container (killing the container client may leave it running).
    fn spawn(&self, cmd: &str, timeout_s: u32) -> Child {
        Command::new(&self.script)
            .arg(format!("timeout {timeout_s} {cmd}"))
            .env("ZENOH_ROUTER", &self.endpoint)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("running the ROS 2 container script")
    }

    /// Runs `cmd` to completion and returns its output.
    fn run(&self, cmd: &str, timeout_s: u32) -> String {
        let out = self.spawn(cmd, timeout_s).wait_with_output().unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        eprintln!("--- {cmd}\n{text}");
        text
    }

    fn endpoint(&self, node: &str) -> Endpoint {
        Endpoint::open(EndpointConfig {
            connect: vec![self.endpoint.clone()],
            domain: 0,
            node_name: node.into(),
            ..EndpointConfig::default()
        })
        .unwrap()
    }
}

/// Container startup on a loaded machine can take a while.
const START: Duration = Duration::from_secs(90);

#[test]
fn ruro_calls_a_ros_service() {
    let Some(ros) = Ros::start() else { return };
    let mut server = ros.spawn("ros2 run demo_nodes_cpp add_two_ints_server", 150);
    let node = ros.endpoint("ruro_client");
    let client = node
        .client::<AddTwoInts>("/add_two_ints", Qos::services())
        .unwrap();
    assert!(
        client.raw().wait_for_server(START),
        "add_two_ints_server did not appear"
    );
    let resp = client
        .call(&AddTwoInts_Request { a: 2, b: 3 }, Duration::from_secs(10))
        .unwrap();
    assert_eq!(resp.sum, 5);
    let resp = client
        .call(
            &AddTwoInts_Request { a: -7, b: 50 },
            Duration::from_secs(10),
        )
        .unwrap();
    assert_eq!(resp.sum, 43);
    let _ = server.kill();
    let _ = server.wait();
}

#[test]
fn ros_calls_a_ruro_service() {
    let Some(ros) = Ros::start() else { return };
    let node = ros.endpoint("ruro_add_two_ints_server");
    let _service = node
        .service::<AddTwoInts>("/add_two_ints", Qos::services(), |r| AddTwoInts_Response {
            sum: r.a + r.b,
        })
        .unwrap();
    let out = ros.run(
        "ros2 service call /add_two_ints example_interfaces/srv/AddTwoInts '{a: 2, b: 40}'",
        120,
    );
    assert!(out.contains("sum=42"), "no response in:\n{out}");
}

fn transform(child: &str) -> TFMessage {
    let mut t = TransformStamped::default();
    t.header.frame_id = "world".into();
    t.child_frame_id = child.into();
    t.transform.translation.x = 1.5;
    t.transform.rotation.w = 1.0;
    TFMessage {
        transforms: vec![t],
    }
}

#[test]
fn tf_static_is_latched_both_ways() {
    let Some(ros) = Ros::start() else { return };
    let node = ros.endpoint("ruro_tf");

    // ruro latches a transform; a ROS subscriber started afterwards gets it.
    let publisher = node
        .publisher::<TFMessage>("/tf_static", Qos::transient_local(1))
        .unwrap();
    publisher.publish(&transform("ruro_frame")).unwrap();
    let out = ros.run(
        "ros2 topic echo --once --qos-durability transient_local --qos-reliability reliable \
         /tf_static tf2_msgs/msg/TFMessage",
        120,
    );
    assert!(
        out.contains("child_frame_id: ruro_frame"),
        "ruro's latched transform did not reach ROS:
{out}"
    );

    // A ROS static transform publisher started now is seen by a ruro
    // subscriber created after it has published.
    let mut stp = ros.spawn(
        "ros2 run tf2_ros static_transform_publisher --x 2 --frame-id world --child-frame-id ros_frame",
        150,
    );
    let (tx, rx) = mpsc::channel();
    // Wait for the ROS publisher to exist before subscribing, so the
    // sample can only come from its cache.
    let start = Instant::now();
    loop {
        let topics = ruro_ros2::list_topics(node.session(), 0, Duration::from_millis(500)).unwrap();
        if topics
            .iter()
            .any(|t| t.name == "/tf_static" && t.publishers >= 2)
        {
            break;
        }
        assert!(
            start.elapsed() < START,
            "static_transform_publisher did not appear"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    std::thread::sleep(Duration::from_secs(1));
    let _sub = node
        .subscriber::<TFMessage>("/tf_static", Qos::transient_local(10), move |m, _| {
            if let Ok(m) = m {
                for t in m.transforms {
                    let _ = tx.send((t.child_frame_id, t.transform.translation.x));
                }
            }
        })
        .unwrap();
    let mut from_ros = None;
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok((frame, x)) if frame == "ros_frame" => {
                from_ros = Some(x);
                break;
            }
            _ => {}
        }
    }
    assert_eq!(
        from_ros,
        Some(2.0),
        "ROS's latched transform did not reach ruro"
    );
    let _ = stp.kill();
    let _ = stp.wait();
}
