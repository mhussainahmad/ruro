//! ROS 2 parameter services of a group's nodes (`ros2: { parameters: true }`),
//! called the way `ros2 param` does, through an in-process Zenoh router.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ruro_graph::Graph;
use ruro_host::{Executor, Remote};
use ruro_ros2::msgs::rcl_interfaces::{msg, srv};
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

fn double(v: f64) -> msg::ParameterValue {
    msg::ParameterValue {
        r#type: 3,
        double_value: v,
        ..Default::default()
    }
}

#[test]
fn ros_reads_and_sets_parameters_of_ruro_nodes() {
    ruro_nodes_std::link();
    let (_router, endpoint) = router();
    let graph = Graph::from_yaml(&format!(
        "name: p\nros2: {{ parameters: true, router: \"{endpoint}\", domain_id: 9 }}\nnodes:\n  src: {{ type: std/sine, rate: 100 }}\n  gain: {{ type: std/gain, params: {{ k: 3 }} }}\nwires: [src.value -> gain.input]\n"
    ))
    .unwrap();
    let exec = Arc::new(Executor::start(&graph, "main", Remote::Disabled).unwrap());
    let _services = ruro_host::ros2_params::attach(&exec, graph.ros2.as_ref().unwrap()).unwrap();

    let ros = Endpoint::open(EndpointConfig {
        connect: vec![endpoint],
        domain: 9,
        node_name: "ros2cli".into(),
        ..EndpointConfig::default()
    })
    .unwrap();
    let timeout = Duration::from_secs(5);
    let get = ros
        .client::<srv::GetParameters>("/ruro_gain/get_parameters", Qos::services())
        .unwrap();
    assert!(get.raw().wait_for_server(timeout));
    let list = ros
        .client::<srv::ListParameters>("/ruro_gain/list_parameters", Qos::services())
        .unwrap();
    let describe = ros
        .client::<srv::DescribeParameters>("/ruro_gain/describe_parameters", Qos::services())
        .unwrap();
    let set = ros
        .client::<srv::SetParameters>("/ruro_gain/set_parameters", Qos::services())
        .unwrap();
    let types = ros
        .client::<srv::GetParameterTypes>("/ruro_gain/get_parameter_types", Qos::services())
        .unwrap();
    let get_k = || {
        get.call(
            &srv::GetParameters_Request {
                names: vec!["k".into()],
            },
            timeout,
        )
        .unwrap()
        .values[0]
            .clone()
    };

    let names = list
        .call(&srv::ListParameters_Request::default(), timeout)
        .unwrap()
        .result
        .names;
    assert_eq!(names, ["k", "use_sim_time"]);
    // Written as `3` in the graph, a float parameter is still a double.
    assert_eq!(get_k(), double(3.0));
    let t = types
        .call(
            &srv::GetParameterTypes_Request {
                names: vec!["k".into(), "use_sim_time".into(), "nope".into()],
            },
            timeout,
        )
        .unwrap()
        .types;
    assert_eq!(t, [3, 1, 0]);

    let d = describe
        .call(
            &srv::DescribeParameters_Request {
                names: vec!["k".into()],
            },
            timeout,
        )
        .unwrap()
        .descriptors;
    assert_eq!(d[0].description, "Factor applied to every sample.");
    let r = &d[0].floating_point_range[0];
    assert_eq!((r.from_value, r.to_value), (-1000.0, 1000.0));

    let set_k = |value: msg::ParameterValue| {
        set.call(
            &srv::SetParameters_Request {
                parameters: vec![msg::Parameter {
                    name: "k".into(),
                    value,
                }],
            },
            timeout,
        )
        .unwrap()
        .results[0]
            .clone()
    };
    // `ros2 param set /ruro_gain k 2` sends an integer.
    let ok = set_k(msg::ParameterValue {
        r#type: 2,
        integer_value: 2,
        ..Default::default()
    });
    assert!(ok.successful, "{}", ok.reason);
    assert_eq!(get_k(), double(2.0));
    let out = set_k(double(5000.0));
    assert!(
        !out.successful && out.reason.contains("outside [-1000, 1000]"),
        "{}",
        out.reason
    );
    let wrong = set_k(msg::ParameterValue {
        r#type: 4,
        string_value: "fast".into(),
        ..Default::default()
    });
    assert!(
        !wrong.successful && wrong.reason.contains("got a string"),
        "{}",
        wrong.reason
    );
    assert_eq!(get_k(), double(2.0));

    // Changes from elsewhere (`ruro param`, the studio) show up too.
    exec.set_param("gain", "k", json!(-0.5)).unwrap();
    let start = Instant::now();
    while get_k() != double(-0.5) {
        assert!(start.elapsed() < timeout, "{:?}", get_k());
        std::thread::sleep(Duration::from_millis(20));
    }
    exec.shutdown();
}

/// With the real `ros2 param` (Jazzy, rmw_zenoh) against a graph deployed
/// through a daemon, which is also the Zenoh router. Skipped unless
/// `RURO_ROS2_CONTAINER` names a script that runs a shell command in ROS 2
/// with host networking and honours `ZENOH_ROUTER` (see scripts/ros2/run.sh).
#[test]
fn ros2_param_tunes_a_deployed_graph() {
    use ruro_proto::{Ctl, Keys, Preview, TapFrame};
    let Some(script) = std::env::var("RURO_ROS2_CONTAINER")
        .ok()
        .filter(|s| !s.is_empty())
    else {
        eprintln!("skipped: set RURO_ROS2_CONTAINER to a ROS 2 run script to test interop");
        return;
    };
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let endpoint = format!("tcp/127.0.0.1:{port}");
    let name = format!("rosparam{}", std::process::id());
    let mut config = ruro_daemon::Config::new();
    config.name = name.clone();
    config.listen = endpoint.clone();
    config.host = env!("CARGO_BIN_EXE_ruro-host").into();
    let daemon = ruro_daemon::Daemon::start(config).unwrap();
    let session = ruro_proto::connect(&endpoint).unwrap();
    let keys = Keys::new(&name);
    // A constant 1.5 through the gain: its output shows k.
    let graph = format!(
        "name: tune\nros2: {{ parameters: true, router: \"{endpoint}\" }}\nnodes:\n  src: {{ type: std/sine, rate: 50, params: {{ frequency: 0.0, offset: 1.5 }} }}\n  gain: {{ type: std/gain, group: g, params: {{ k: 3 }} }}\nwires: [src.value -> gain.input]\n"
    );
    ruro_proto::request(
        &session,
        &keys,
        Ctl::Deploy,
        &graph,
        Duration::from_secs(10),
    )
    .unwrap();
    let tap = session
        .declare_subscriber(keys.tap("gain", "output"))
        .wait()
        .unwrap();
    let latest = || {
        let start = Instant::now();
        let mut last = None;
        while start.elapsed() < Duration::from_secs(2) || last.is_none() {
            if let Ok(Some(s)) = tap.recv_timeout(Duration::from_millis(100)) {
                let f: TapFrame = ruro_proto::decode(&s.payload().to_bytes()).unwrap();
                if let Preview::Values(v) = f.preview {
                    last = v.first().map(|(_, x)| *x);
                }
            }
            assert!(start.elapsed() < Duration::from_secs(20), "no gain output");
        }
        last.unwrap()
    };
    assert_eq!(latest(), 4.5);

    let commands = [
        "ros2 node list",
        "ros2 param list /ruro_gain",
        "ros2 param get /ruro_gain k",
        "ros2 param describe /ruro_gain k",
        "ros2 param set /ruro_gain k 2",
        "ros2 param set /ruro_gain k 5000",
        "ros2 param dump /ruro_gain",
    ];
    let shell: Vec<String> = commands
        .iter()
        .map(|c| format!("echo '>>> {c}'; timeout 60 {c} 2>&1"))
        .collect();
    let out = std::process::Command::new(&script)
        .arg(shell.join("; "))
        .env("ZENOH_ROUTER", &endpoint)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    eprintln!("{text}");
    let section = |cmd: &str| {
        let start = text.find(&format!(">>> {cmd}\n")).unwrap() + cmd.len() + 5;
        let rest = &text[start..];
        rest[..rest.find(">>> ").unwrap_or(rest.len())].to_owned()
    };
    let nodes = section("ros2 node list");
    assert_eq!(nodes.matches("/ruro_gain").count(), 1, "{nodes}");
    assert!(nodes.contains("/ruro_src"), "{nodes}");
    let list = section("ros2 param list /ruro_gain");
    assert!(
        list.contains("k") && list.contains("use_sim_time"),
        "{list}"
    );
    let get = section("ros2 param get /ruro_gain k");
    assert!(get.contains("Double value is: 3.0"), "{get}");
    let describe = section("ros2 param describe /ruro_gain k");
    assert!(
        describe.contains("Type: double")
            && describe.contains("Factor applied to every sample.")
            && describe.contains("Min value: -1000.0")
            && describe.contains("Max value: 1000.0"),
        "{describe}"
    );
    let set = section("ros2 param set /ruro_gain k 2");
    assert!(set.contains("Set parameter successful"), "{set}");
    let refused = section("ros2 param set /ruro_gain k 5000");
    assert!(
        refused.contains("Setting parameter failed") && refused.contains("outside [-1000, 1000]"),
        "{refused}"
    );
    let dump = section("ros2 param dump /ruro_gain");
    assert!(
        dump.contains("ros__parameters") && dump.contains("k: 2.0"),
        "{dump}"
    );
    // The node took the value.
    assert_eq!(latest(), 3.0);

    let _ = ruro_proto::request(&session, &keys, Ctl::Stop, "", Duration::from_secs(10));
    drop(session);
    daemon.shutdown();
    let _ = std::fs::remove_dir_all(ruro_daemon::runtime_dir(&name));
}
