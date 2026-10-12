//! A daemon running graphs on `ruro-host-py`: Rust and Python nodes in one
//! process, values observed through the daemon's taps like `ruro echo` does.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ruro_proto::{Ctl, Keys, Preview, TapFrame};
use zenoh::Wait;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn repo() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

fn values(frames: &[TapFrame]) -> Vec<f64> {
    frames
        .iter()
        .filter_map(|f| match &f.preview {
            Preview::Values(v) => v.first().map(|(_, x)| *x),
            _ => None,
        })
        .collect()
}

#[test]
fn rust_and_python_nodes_share_a_graph() {
    let module = repo().join("examples/python/threshold.py");
    // SAFETY: set before any other thread of this test process reads the
    // environment; the daemon passes it on to the host processes it starts.
    unsafe { std::env::set_var("RURO_PY_MODULES", &module) };

    let port = free_port();
    let endpoint = format!("tcp/127.0.0.1:{port}");
    let name = format!("pye2e{}", std::process::id());
    let mut config = ruro_daemon::Config::new();
    config.name = name.clone();
    config.listen = endpoint.clone();
    config.host = env!("CARGO_BIN_EXE_ruro-host-py").into();
    let daemon = ruro_daemon::Daemon::start(config).expect("daemon starts");

    let session = ruro_proto::connect(&endpoint).unwrap();
    let keys = Keys::new(&name);
    let registry =
        ruro_proto::request(&session, &keys, Ctl::Registry, "", Duration::from_secs(10)).unwrap();
    assert!(
        registry.contains("py/threshold"),
        "python nodes are in the registry"
    );
    assert!(registry.contains("std/sine"));

    let frames: Arc<Mutex<Vec<TapFrame>>> = Arc::new(Mutex::new(Vec::new()));
    let means: Arc<Mutex<Vec<TapFrame>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = |store: Arc<Mutex<Vec<TapFrame>>>| {
        move |sample: zenoh::sample::Sample| {
            if let Ok(f) = ruro_proto::decode::<TapFrame>(&sample.payload().to_bytes()) {
                store.lock().unwrap().push(f);
            }
        }
    };
    let _flags = session
        .declare_subscriber(keys.tap("threshold", "high"))
        .callback(sink(frames.clone()))
        .wait()
        .unwrap();
    let _means = session
        .declare_subscriber(keys.tap("brightness", "mean"))
        .callback(sink(means.clone()))
        .wait()
        .unwrap();

    let yaml = std::fs::read_to_string(repo().join("examples/python-threshold.yaml")).unwrap();
    let reply = ruro_proto::request(&session, &keys, Ctl::Deploy, &yaml, Duration::from_secs(20));
    assert!(reply.is_ok(), "deploy: {reply:?}");

    // The sine (2 Hz, amplitude 1) crosses the 0.5 limit both ways within a second.
    let start = Instant::now();
    let numpy = std::env::var_os("VIRTUAL_ENV").is_some();
    loop {
        let v = values(&frames.lock().unwrap());
        let done = v.contains(&0.0) && v.contains(&1.0);
        let means_ok = !numpy || !values(&means.lock().unwrap()).is_empty();
        if done && means_ok {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "timed out: flags {v:?}, means {:?}",
            values(&means.lock().unwrap())
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    if numpy {
        let m = values(&means.lock().unwrap());
        assert!(m.iter().all(|x| (0.0..=255.0).contains(x)), "{m:?}");
    }

    let _ = ruro_proto::request(&session, &keys, Ctl::Stop, "", Duration::from_secs(10));
    drop(session);
    daemon.shutdown();
}

#[test]
fn host_describes_python_nodes() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ruro-host-py"))
        .arg("describe")
        .env(
            "RURO_PY_MODULES",
            repo().join("examples/python/threshold.py"),
        )
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let registry: ruro_graph::Registry = serde_json::from_slice(&out.stdout).unwrap();
    let t = registry
        .get("py/threshold")
        .expect("python node registered");
    assert_eq!(t.inputs[0].ty, "std/Scalar");
    assert_eq!(t.params[0].name, "limit");
    assert!(registry.get("tf/lookup").is_some(), "tf nodes are linked");
    assert_eq!(
        registry.schema_hash("std/Flag"),
        Some(
            <ruro_msg::Flag as ruro_msg::Message>::message_schema()
                .hash
                .as_str()
        )
    );

    // A module that fails to import is reported but does not take the host down.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ruro-host-py"))
        .arg("describe")
        .env("RURO_PY_MODULES", "no_such_module_anywhere")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no_such_module_anywhere"));
}
