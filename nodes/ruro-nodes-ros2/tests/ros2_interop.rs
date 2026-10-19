//! The generic bridge against real ROS 2 nodes. Runs only when
//! `RURO_ROS2_CONTAINER` names a script that runs a shell command in a ROS 2
//! Jazzy environment with rmw_zenoh, host networking, the router taken from
//! `ZENOH_ROUTER` and the directory `ROS_MOUNT` mounted at the same path,
//! such as a wrapper around `podman run` of a Jazzy image.
//!
//! ROS publishes types ruro has no Rust type for (`example_interfaces` and
//! `rcl_interfaces` messages, a wstring), and demo_nodes_cpp's talker runs;
//! ruro's `ros2/sub` must output exactly the bytes that crossed the router,
//! which must hold the values ROS serialized (ROS leaves padding bytes
//! uninitialised, so two serializations of one message can differ there),
//! and `ros2/field` must read their fields. Then `ros2 topic echo` must receive
//! a message of such a type published by ruro's `ros2/pub`.

use std::io::BufRead;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ruro::msg::{Ros2Any, Scalar, Text};
use ruro::node::testing::Harness;
use serde_json::json;
use zenoh::Wait;

const PUBLISHER: &str = r#"
import sys, time, rclpy
from rclpy.serialization import serialize_message
from example_interfaces.msg import Int64MultiArray, MultiArrayDimension, WString
from rcl_interfaces.msg import Log
rclpy.init()
node = rclpy.create_node('ruro_interop')
ints = Int64MultiArray(data=[1, -2, 3])
ints.layout.dim = [MultiArrayDimension(label='x', size=3, stride=3)]
log = Log(level=Log.WARN, name='interop', msg='sent by rclpy', file='f.py', function='main', line=7)
log.stamp.sec = 5
wide = WString(data='wé \U0001F600')
pubs = []
for topic, ty, msg in [('/ints', Int64MultiArray, ints), ('/ruro_log', Log, log), ('/wide', WString, wide)]:
    pubs.append((node.create_publisher(ty, topic, 10), msg))
    print('HEX', topic, serialize_message(msg).hex(), flush=True)
end = time.time() + float(sys.argv[1])
while time.time() < end:
    for p, m in pubs:
        p.publish(m)
    time.sleep(0.2)
"#;

fn container() -> Option<String> {
    match std::env::var("RURO_ROS2_CONTAINER") {
        Ok(s) if !s.is_empty() => Some(s),
        _ => {
            eprintln!("skipping: RURO_ROS2_CONTAINER is not set (no ROS 2 to test against)");
            None
        }
    }
}

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

/// Runs `cmd` in the ROS 2 environment; stdout lines arrive on the channel.
fn ros(
    script: &str,
    endpoint: &str,
    mount: &PathBuf,
    cmd: &str,
) -> (Child, mpsc::Receiver<String>) {
    let mut child = Command::new(script)
        .arg(cmd)
        .env("ZENOH_ROUTER", endpoint)
        .env("ROS_MOUNT", mount)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("running RURO_ROS2_CONTAINER");
    let out = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    (child, rx)
}

fn interfaces() -> String {
    concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../crates/ruro-ros2/tests/fixtures/interfaces"
    )
    .to_owned()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn first(h: &mut Harness, deadline: Instant) -> Ros2Any {
    loop {
        let _ = h.step();
        if let Some(m) = h.take::<Ros2Any>("out").unwrap().into_iter().next() {
            return m;
        }
        assert!(Instant::now() < deadline, "nothing received from ROS 2");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn ros2_nodes_and_ruro_exchange_any_type_byte_for_byte() {
    let Some(script) = container() else { return };
    ruro_nodes_ros2::link();
    let (router, endpoint) = router();
    // What crosses the router, per topic.
    let (wire_tx, wire_rx) = mpsc::channel::<(String, Vec<u8>)>();
    let _wire = router
        .declare_subscriber("0/**")
        .callback(move |s| {
            let key = s.key_expr().as_str().to_owned();
            let _ = wire_tx.send((key, s.payload().to_bytes().to_vec()));
        })
        .wait()
        .unwrap();
    let dir = std::env::temp_dir().join(format!("ruro-interop-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("pub.py"), PUBLISHER).unwrap();
    let (mut publisher, lines) = ros(
        &script,
        &endpoint,
        &dir,
        &format!(
            "(ros2 run demo_nodes_cpp talker >/dev/null 2>&1 &) ; python3 {}/pub.py 40",
            dir.display()
        ),
    );
    let mut expected = std::collections::BTreeMap::new();
    while expected.len() < 3 {
        let line = lines
            .recv_timeout(Duration::from_secs(60))
            .expect("the ROS publisher printed its messages");
        if let Some(rest) = line.strip_prefix("HEX ") {
            let (topic, hex) = rest.split_once(' ').unwrap();
            expected.insert(topic.to_owned(), unhex(hex));
        }
    }
    let sub = |topic: &str, ty: &str| {
        Harness::new(
            "ros2/sub",
            json!({
                "topic": topic, "type": ty, "interfaces": interfaces(),
                "router": endpoint, "domain_id": 0
            }),
        )
        .unwrap()
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut ints = sub("/ints", "example_interfaces/msg/Int64MultiArray");
    let mut log = sub("/ruro_log", "rcl_interfaces/msg/Log");
    let mut wide = sub("/wide", "example_interfaces/msg/WString");
    let mut chatter = sub("/chatter", "std_msgs/msg/String");
    let got_ints = first(&mut ints, deadline);
    let got_log = first(&mut log, deadline);
    let got_wide = first(&mut wide, deadline);
    let got_chatter = first(&mut chatter, deadline);
    let mut wire: std::collections::BTreeMap<String, Vec<Vec<u8>>> = Default::default();
    while let Ok((key, bytes)) = wire_rx.try_recv() {
        let topic = key.split('/').nth(1).unwrap_or_default().to_owned();
        wire.entry(format!("/{topic}")).or_default().push(bytes);
    }
    let mut lib = ruro_ros2::search::TypeLibrary::bundled();
    lib.add_dir(std::path::Path::new(&interfaces()));
    for (topic, got) in [
        ("/ints", &got_ints),
        ("/ruro_log", &got_log),
        ("/wide", &got_wide),
    ] {
        assert!(
            wire[topic].contains(&got.cdr),
            "{topic}: ruro's bytes are not what crossed the router"
        );
        let ty = lib.resolve_str(&got.type_name).unwrap();
        assert_eq!(got.type_hash, ty.hash, "{topic}");
        let ours = ruro_ros2::dynamic::decode(&ty, &got.cdr).unwrap();
        let theirs = ruro_ros2::dynamic::decode(&ty, &expected[topic]).unwrap();
        assert_eq!(ours.value, theirs.value, "{topic}: values");
        assert_eq!(ours.encode().unwrap(), got.cdr, "{topic}: re-encoding");
    }

    let field = |path: &str, msg: &Ros2Any| -> (Vec<Scalar>, Vec<Text>) {
        let mut f = Harness::new(
            "ros2/field",
            json!({ "path": path, "interfaces": interfaces() }),
        )
        .unwrap();
        f.push("input", msg.clone()).unwrap();
        f.step().unwrap();
        (f.take("value").unwrap(), f.take("text").unwrap())
    };
    assert_eq!(field("data[1]", &got_ints).0, [Scalar(-2.0)]);
    assert_eq!(
        field("layout.dim[0].label", &got_ints).1,
        [Text("x".into())]
    );
    assert_eq!(field("msg", &got_log).1, [Text("sent by rclpy".into())]);
    assert_eq!(field("stamp.sec", &got_log).0, [Scalar(5.0)]);
    assert_eq!(field("data", &got_wide).1, [Text("wé 😀".into())]);
    assert!(
        field("data", &got_chatter).1[0]
            .0
            .starts_with("Hello World: ")
    );
    let _ = publisher.kill();

    // ruro publishes a type it has no Rust type for; ROS reads it.
    let ty = lib
        .resolve_str("example_interfaces/msg/Int64MultiArray")
        .unwrap();
    let msg =
        ruro_ros2::dynamic::MessageValue::from_json(&ty, &json!({ "data": [7, 8, 9] })).unwrap();
    let out = Ros2Any {
        type_name: ty.name.to_string(),
        type_hash: ty.hash.clone(),
        cdr: msg.encode().unwrap(),
    };
    let mut publ = Harness::new(
        "ros2/pub",
        json!({ "topic": "/from_ruro", "router": endpoint, "domain_id": 0 }),
    )
    .unwrap();
    let (mut echo, echoed) = ros(
        &script,
        &endpoint,
        &dir,
        "timeout 60 ros2 topic echo --once /from_ruro example_interfaces/msg/Int64MultiArray",
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut text = String::new();
    while !text.contains("- 9") {
        publ.push("input", out.clone()).unwrap();
        publ.step().unwrap();
        while let Ok(l) = echoed.try_recv() {
            text.push_str(&l);
            text.push('\n');
        }
        assert!(
            Instant::now() < deadline,
            "ros2 topic echo got nothing:\n{text}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(text.contains("data:\n- 7\n- 8\n- 9"), "{text}");
    let _ = echo.wait();
}

/// Every interface definition installed in the ROS 2 environment: parsed
/// from `.msg`/`.srv`/`.action` and from `.idl`, with the RIHS01 hash
/// rosidl generated for it.
#[test]
fn every_installed_interface_parses_and_hashes_like_rosidl() {
    let Some(script) = container() else { return };
    let dir = std::env::temp_dir().join(format!("ruro-interop-share-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let copy = format!(
        "cd /opt/ros/jazzy/share && for d in */msg */srv */action; do mkdir -p {0}/$d; \
         cp $d/*.msg $d/*.srv $d/*.action $d/*.idl $d/*.json {0}/$d/ 2>/dev/null; done; \
         chmod -R a+rwX {0}",
        dir.display()
    );
    let status = Command::new(&script)
        .arg(&copy)
        .env("ROS_MOUNT", &dir)
        .status()
        .unwrap();
    assert!(status.success());
    let mut lib = ruro_ros2::search::TypeLibrary::bundled();
    lib.add_dir(&dir);
    let mut hashed = 0;
    let mut idl = 0;
    for pkg in std::fs::read_dir(&dir).unwrap().flatten() {
        for sub in ["msg", "srv", "action"] {
            let Ok(files) = std::fs::read_dir(pkg.path().join(sub)) else {
                continue;
            };
            for f in files.flatten().map(|f| f.path()) {
                match f.extension().and_then(|e| e.to_str()) {
                    Some("json") => {
                        let doc: serde_json::Value =
                            serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
                        for h in doc["type_hashes"].as_array().unwrap() {
                            let name = h["type_name"].as_str().unwrap();
                            // Bare service and action types are not messages.
                            let Ok(t) = lib.resolve_str(name) else {
                                assert_ne!(sub, "msg", "{name}");
                                continue;
                            };
                            assert_eq!(t.hash, h["hash_string"].as_str().unwrap(), "{name}");
                            hashed += 1;
                        }
                    }
                    Some("idl") => {
                        let specs =
                            ruro_ros2::idl_file::parse_idl(&std::fs::read_to_string(&f).unwrap())
                                .unwrap_or_else(|e| panic!("{}: {e:#}", f.display()));
                        for s in &specs {
                            lib.resolve(&s.name).unwrap();
                        }
                        let mut from_idl = lib.interfaces().clone();
                        for s in specs.iter().cloned() {
                            from_idl.add(s);
                        }
                        for s in &specs {
                            let want = lib.resolve(&s.name).unwrap().hash.clone();
                            let got = ruro_ros2::hash::type_hash(&from_idl, &s.name).unwrap();
                            assert_eq!(got, want, "{} from {}", s.name, f.display());
                            idl += 1;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    assert!(lib.warnings().is_empty(), "{:#?}", lib.warnings());
    eprintln!("{hashed} type hashes and {idl} .idl types checked");
    assert!(hashed > 500 && idl > 200, "{hashed} {idl}");
}
