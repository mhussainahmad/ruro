//! C++ nodes (`RURO_PLUGINS`) through a real daemon and host: the daemon
//! lists the plugin's types, and a graph runs one between Rust nodes.
//!
//! The plugin is compiled here with `$RURO_TEST_CXX` (default `c++`); the
//! test is skipped when that builds for another architecture, unless
//! `RURO_REQUIRE_PLUGINS` is set. A test binary of its own: the plugin list
//! is set in the environment, which the daemon and its hosts inherit.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
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

/// Builds the C++ test plugin into `dir`, or None for another architecture.
fn plugin(dir: &Path) -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cxx = std::env::var("RURO_TEST_CXX").unwrap_or_else(|_| "c++".to_owned());
    let out = dir.join("libcpptest.so");
    let result = Command::new(&cxx)
        .args(["-std=c++17", "-shared", "-fPIC", "-O1", "-I"])
        .arg(root.join("sdk/include"))
        .arg("-o")
        .arg(&out)
        .arg(root.join("nodes/ruro-nodes-native/tests/fixtures/cpptest.cpp"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let machine = ruro_daemon::update::elf_arch(&out).unwrap();
    if machine.is_some() && machine != Some(std::env::consts::ARCH) {
        assert!(
            std::env::var_os("RURO_REQUIRE_PLUGINS").is_none(),
            "{cxx} builds for {machine:?}"
        );
        eprintln!("skipped: {cxx} builds for {machine:?} (set RURO_TEST_CXX)");
        return None;
    }
    Some(out)
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
fn a_cpp_node_runs_in_a_graph_through_the_daemon() {
    let dir = std::env::temp_dir().join(format!("ruro-native-nodes-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let Some(lib) = plugin(&dir) else {
        return;
    };
    // SAFETY: the only test in this binary, set before any thread starts.
    unsafe { std::env::set_var("RURO_PLUGINS", &lib) };

    let name = format!("nativenodes-{}", std::process::id());
    let mut config = Config::new();
    config.name = name.clone();
    config.listen = format!("tcp/127.0.0.1:{}", free_port());
    config.host = PathBuf::from(env!("CARGO_BIN_EXE_ruro-host"));
    let endpoint = config.listen.clone();
    let daemon = Daemon::start(config).unwrap();
    let keys = Keys::new(&name);
    let s = ruro_proto::connect(&endpoint).unwrap();
    let request = |op, payload: &str| ruro_proto::request(&s, &keys, op, payload, TIMEOUT);

    let registry = request(Ctl::Registry, "").unwrap();
    assert!(registry.contains("cpptest/gain"), "{registry}");
    assert!(registry.contains("libcpptest.so"), "{registry}");

    let graph = "name: native
nodes:
  src: { type: std/sine, rate: 50, params: { frequency: 1.0, amplitude: 2.0 } }
  gain: { type: cpptest/gain, group: cpp, params: { k: 3.0 } }
wires:
  - { from: src.value, to: gain.input, depth: 16 }
";
    assert_eq!(request(Ctl::Validate, graph).unwrap(), "[]");
    request(Ctl::Deploy, graph).unwrap();
    let node = |m: &GroupMetrics| m.nodes.iter().find(|n| n.id == "gain").cloned();
    let m = wait_metrics(&s, &keys, "cpp", |m| node(m).is_some_and(|n| n.hz > 20.0));
    assert_eq!(node(&m).unwrap().errors, 0);

    let file = dir.join("native.mcap");
    request(Ctl::Record, &file.display().to_string()).unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    request(Ctl::Record, "").unwrap();
    let src = values(&file, "src.value");
    let out = values(&file, "gain.output");
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
