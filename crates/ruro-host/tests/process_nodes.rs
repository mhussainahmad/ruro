//! A node implemented by another program (`RURO_PROCESS_NODES`), through a
//! real daemon and host: the daemon lists its type, and a graph runs it in a
//! group of its own between Rust nodes.
//!
//! A test binary of its own: the manifest list is set in the environment,
//! which the daemon and its hosts inherit.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ruro_daemon::{Config, Daemon};
use ruro_msg::{Message, Scalar};
use ruro_proto::{Ctl, GroupMetrics, Keys};
use ruro_record::Recording;
use zenoh::Wait;

/// Generous: under emulation (the aarch64 CI job) hosts start slowly.
const TIMEOUT: Duration = Duration::from_secs(45);

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn node_py() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../nodes/ruro-nodes-process/tests/fixtures/node.py")
        .canonicalize()
        .unwrap()
}

fn wait_metrics(
    session: &zenoh::Session,
    keys: &Keys,
    group: &str,
    mut f: impl FnMut(&GroupMetrics) -> bool,
) -> GroupMetrics {
    let sub = session
        .declare_subscriber(keys.metrics(group))
        .wait()
        .unwrap();
    let start = Instant::now();
    let mut last = None;
    while start.elapsed() < TIMEOUT {
        if let Ok(Some(s)) = sub.recv_timeout(Duration::from_millis(200)) {
            let m: GroupMetrics = ruro_proto::decode(&s.payload().to_bytes()).unwrap();
            if f(&m) {
                return m;
            }
            last = Some(m);
        }
    }
    panic!("metrics of `{group}` never matched; last: {last:#?}");
}

fn values(path: &Path, topic: &str) -> Vec<f64> {
    Recording::open(path)
        .unwrap()
        .messages()
        .unwrap()
        .map(Result::unwrap)
        .filter(|m| m.topic() == topic)
        .map(|m| Scalar::decode(&m.data).unwrap().0)
        .collect()
}

#[test]
fn a_process_node_runs_in_a_graph_through_the_daemon() {
    let dir = std::env::temp_dir().join(format!("ruro-process-nodes-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = dir.join("scale.node.yaml");
    std::fs::write(
        &manifest,
        format!(
            "kind: test/scale
doc: Scales its input, in Python.
command: [python3, -I, \"{}\"]
inputs: {{ input: std/Scalar }}
outputs: {{ output: std/Scalar, info: std/Text }}
params:
  k: {{ default: 2.0, min: -100, max: 100 }}
start_timeout: 30
",
            node_py().display()
        ),
    )
    .unwrap();
    // SAFETY: the only test in this binary, set before any thread starts.
    unsafe { std::env::set_var("RURO_PROCESS_NODES", &dir) };

    let name = format!("procnodes-{}", std::process::id());
    let mut config = Config::new();
    config.name = name.clone();
    config.listen = format!("tcp/127.0.0.1:{}", free_port());
    config.host = PathBuf::from(env!("CARGO_BIN_EXE_ruro-host"));
    let endpoint = config.listen.clone();
    let daemon = Daemon::start(config).unwrap();
    let keys = Keys::new(&name);
    let s = ruro_proto::connect(&endpoint).unwrap();
    let request = |op, payload: &str| ruro_proto::request(&s, &keys, op, payload, TIMEOUT);

    // Listed with the host's own node types, with its manifest as source.
    let registry = request(Ctl::Registry, "").unwrap();
    assert!(registry.contains("test/scale"), "{registry}");
    assert!(registry.contains("scale.node.yaml"), "{registry}");

    let graph = "name: proc
nodes:
  src: { type: std/sine, rate: 50, params: { frequency: 1.0, amplitude: 2.0 } }
  scale: { type: test/scale, group: ext, params: { k: 3.0 } }
wires:
  - { from: src.value, to: scale.input, depth: 16 }
";
    let problems = request(Ctl::Validate, graph).unwrap();
    assert_eq!(problems, "[]", "{problems}");
    request(Ctl::Deploy, graph).unwrap();
    let node = |m: &GroupMetrics| m.nodes.iter().find(|n| n.id == "scale").cloned();
    let m = wait_metrics(&s, &keys, "ext", |m| node(m).is_some_and(|n| n.hz > 20.0));
    let n = node(&m).unwrap();
    assert_eq!(n.errors, 0, "{:?}", n.last_error);

    let file = dir.join("proc.mcap");
    request(Ctl::Record, &file.display().to_string()).unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    request(Ctl::Record, "").unwrap();
    let src = values(&file, "src.value");
    let out = values(&file, "scale.output");
    assert!(out.len() > 20, "{} outputs", out.len());
    // The groups start and stop recording at slightly different times
    // (further apart under emulation), so outputs at the ends may come from
    // inputs that were not recorded; every other output is three times an
    // input.
    let tripled: Vec<f64> = src.iter().map(|v| v * 3.0).collect();
    let matched: Vec<bool> = out.iter().map(|v| tripled.contains(v)).collect();
    let first = matched
        .iter()
        .position(|m| *m)
        .expect("no output matches an input");
    let last = matched.iter().rposition(|m| *m).unwrap();
    assert!(
        matched[first..=last].iter().all(|m| *m) && last - first + 1 >= out.len() / 2,
        "outputs matching an input: {matched:?}"
    );

    request(Ctl::Stop, "").unwrap();
    daemon.shutdown();
    let _ = std::fs::remove_dir_all(ruro_daemon::runtime_dir(&name));
    let _ = std::fs::remove_dir_all(&dir);
}
