//! A daemon running graphs on `ruro-host-py`: Rust and Python nodes in one
//! process, values observed through the daemon's taps like `ruro echo` does.
//!
//! This is the only test in its binary because it sets an environment
//! variable that the daemon passes on to the hosts it starts.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ruro_proto::{Ctl, Keys, LogLine, Preview, TapFrame};
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

/// Removes a test daemon's runtime directory when the test ends, also when
/// it fails.
struct RuntimeDir(std::path::PathBuf);

impl Drop for RuntimeDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn rust_and_python_nodes_share_a_graph() {
    let module = repo().join("examples/python/threshold.py");
    // SAFETY: this is the only test in the binary and no other thread has
    // started yet; the daemon passes the variable on to its hosts.
    unsafe { std::env::set_var("RURO_PY_MODULES", &module) };

    let port = free_port();
    let endpoint = format!("tcp/127.0.0.1:{port}");
    let name = format!("pye2e{}", std::process::id());
    // Declared before the daemon, so it is dropped (and the directory
    // removed) after the daemon has stopped.
    let _dir = RuntimeDir(ruro_daemon::runtime_dir(&name));
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

    // The emergency stop reaches the Python node: its hook runs, and its
    // steps see `ctx.estopped` until the stop is released.
    let logs = session.declare_subscriber(keys.all_logs()).wait().unwrap();
    let estop = |cmd: &str| {
        ruro_proto::request(&session, &keys, Ctl::Estop, cmd, Duration::from_secs(10)).unwrap()
    };
    estop("engage");
    let start = Instant::now();
    loop {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "on_estop never ran"
        );
        if let Ok(Some(sample)) = logs.recv_timeout(Duration::from_millis(200)) {
            let line: LogLine = ruro_proto::decode(&sample.payload().to_bytes()).unwrap();
            if line.message.contains("holding the flag low") {
                break;
            }
        }
    }
    frames.lock().unwrap().clear();
    std::thread::sleep(Duration::from_millis(1500));
    let v = values(&frames.lock().unwrap());
    assert!(!v.is_empty() && v.iter().all(|x| *x == 0.0), "{v:?}");
    estop("release");
    let start = Instant::now();
    while !values(&frames.lock().unwrap()).contains(&1.0) {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "never high after release"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let _ = ruro_proto::request(&session, &keys, Ctl::Stop, "", Duration::from_secs(10));
    drop(session);
    daemon.shutdown();
}
