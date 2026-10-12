//! End-to-end tests through real daemons and host processes: recording and
//! replay, the latched e-stop, heartbeats, graphs spanning two daemons,
//! token and TLS protection, and uploaded host binaries.
//!
//! Every daemon gets its own name and a free port, so these tests can run
//! next to each other and next to a daemon on the default port.

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use ruro_daemon::{Config, Daemon};
use ruro_msg::{Message, Scalar};
use ruro_proto::{Ctl, DaemonStatus, GroupMetrics, Keys, LogLine, PlaybackCommand, Security};
use ruro_record::{McapWriter, Recording};
use zenoh::Wait;

const TIMEOUT: Duration = Duration::from_secs(15);

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn unique(name: &str) -> String {
    format!("{name}-{}", std::process::id())
}

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ruro-daemon-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

struct TestDaemon {
    daemon: Option<Daemon>,
    endpoint: String,
    keys: Keys,
}

impl TestDaemon {
    fn start(name: &str, configure: impl FnOnce(&mut Config)) -> Self {
        let name = unique(name);
        let mut config = Config::new();
        config.name = name.clone();
        config.listen = format!("tcp/127.0.0.1:{}", free_port());
        config.host = PathBuf::from(env!("CARGO_BIN_EXE_ruro-host"));
        configure(&mut config);
        let endpoint = config.listen.clone();
        let daemon = Daemon::start(config).unwrap();
        Self {
            daemon: Some(daemon),
            endpoint,
            keys: Keys::new(&name),
        }
    }

    fn client(&self) -> zenoh::Session {
        self.client_with(&Security::default()).unwrap()
    }

    fn client_with(&self, security: &Security) -> Result<zenoh::Session> {
        ruro_proto::connect_with(&self.endpoint, security)
    }

    fn name(&self) -> &str {
        self.keys.daemon()
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        if let Some(d) = self.daemon.take() {
            d.shutdown();
        }
    }
}

fn request(session: &zenoh::Session, keys: &Keys, op: Ctl, payload: &str) -> Result<String> {
    ruro_proto::request(session, keys, op, payload, TIMEOUT)
}

fn next_status(session: &zenoh::Session, keys: &Keys) -> DaemonStatus {
    let sub = session.declare_subscriber(keys.status()).wait().unwrap();
    let s = sub.recv_timeout(Duration::from_secs(3)).unwrap().unwrap();
    ruro_proto::decode(&s.payload().to_bytes()).unwrap()
}

fn wait_status(
    session: &zenoh::Session,
    keys: &Keys,
    timeout: Duration,
    mut f: impl FnMut(&DaemonStatus) -> bool,
) -> DaemonStatus {
    let sub = session.declare_subscriber(keys.status()).wait().unwrap();
    let start = Instant::now();
    let mut last = None;
    while start.elapsed() < timeout {
        if let Ok(Some(s)) = sub.recv_timeout(Duration::from_millis(200)) {
            let st: DaemonStatus = ruro_proto::decode(&s.payload().to_bytes()).unwrap();
            if f(&st) {
                return st;
            }
            last = Some(st);
        }
    }
    panic!("status condition not met within {timeout:?}; last status: {last:#?}");
}

/// Waits for metrics of `group` matching `f`.
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

fn node_hz(m: &GroupMetrics, id: &str) -> f64 {
    m.nodes.iter().find(|n| n.id == id).map_or(0.0, |n| n.hz)
}

/// Scalar values of `topic` in a recording.
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

/// Copies only the channels named in `topics` into a new recording.
fn filter(src: &Path, dst: &Path, topics: &[&str]) {
    let mut w = McapWriter::create(dst, false).unwrap();
    for m in Recording::open(src).unwrap().messages().unwrap() {
        let m = m.unwrap();
        if topics.contains(&m.topic()) {
            let c = w.channel(m.topic(), m.ty(), m.encoding()).unwrap();
            w.write(c, m.seq as u64, m.log_time, &m.data).unwrap();
        }
    }
    w.finish().unwrap();
}

#[test]
fn record_and_replay_through_the_daemon() {
    let d = TestDaemon::start("rec", |_| {});
    let s = d.client();
    // Source and gain run in separate processes, so replayed messages also
    // cross shared memory on their way to the gain.
    let graph = r#"
name: rec-demo
nodes:
  src: { type: std/sine, group: a, rate: 100, params: { frequency: 2.0, amplitude: 3.0 } }
  gain: { type: std/gain, group: b, params: { k: 0.5 } }
wires:
  - { from: src.value, to: gain.input, depth: 16 }
"#;
    request(&s, &d.keys, Ctl::Deploy, graph).unwrap();
    wait_metrics(&s, &d.keys, "b", |m| node_hz(m, "gain") > 50.0);

    let first = tmp("daemon-first.mcap");
    let reply = request(&s, &d.keys, Ctl::Record, &first.display().to_string()).unwrap();
    assert!(reply.contains("recording 2 group(s)"), "{reply}");
    let st = next_status(&s, &d.keys);
    assert_eq!(
        st.recording.as_deref(),
        Some(first.display().to_string().as_str())
    );
    std::thread::sleep(Duration::from_millis(1500));
    let reply = request(&s, &d.keys, Ctl::Record, "").unwrap();
    assert!(reply.contains("2 channel(s)"), "{reply}");
    assert!(
        request(&s, &d.keys, Ctl::Record, "").is_err(),
        "not recording any more"
    );

    let src_first = values(&first, "src.value");
    let gain_first = values(&first, "gain.output");
    assert!(gain_first.len() > 100, "{}", gain_first.len());
    // The two groups start recording at slightly different times, so match
    // values rather than positions.
    let halves: Vec<f64> = src_first.iter().map(|v| v * 0.5).collect();
    let matched = gain_first.iter().filter(|g| halves.contains(g)).count();
    assert!(
        matched + 3 >= gain_first.len(),
        "{matched} of {}",
        gain_first.len()
    );

    // Replay only the source; the gain recomputes from the recorded values.
    let replay = tmp("daemon-src-only.mcap");
    filter(&first, &replay, &["src.value"]);
    let second = tmp("daemon-second.mcap");
    request(&s, &d.keys, Ctl::Record, &second.display().to_string()).unwrap();
    let start = PlaybackCommand::Start {
        file: replay.display().to_string(),
        rate: 2.0,
    };
    let reply = request(
        &s,
        &d.keys,
        Ctl::Playback,
        &serde_json::to_string(&start).unwrap(),
    )
    .unwrap();
    assert!(reply.contains("replaying src"), "{reply}");
    let st = wait_status(&s, &d.keys, Duration::from_secs(3), |st| {
        st.playback.is_some()
    });
    let p = st.playback.unwrap();
    assert!(p.duration_s > 1.3 && p.duration_s < 3.0, "{p:?}");
    assert_eq!(p.rate, 2.0);
    wait_status(&s, &d.keys, TIMEOUT, |st| st.playback.is_none());
    std::thread::sleep(Duration::from_millis(300));
    request(&s, &d.keys, Ctl::Record, "").unwrap();

    // The second recording holds live values before the replay started and
    // then exactly the gain outputs of the first run.
    let gain_second = values(&second, "gain.output");
    let expected = &gain_first[..gain_first.len() - 1];
    let found = gain_second.windows(expected.len()).any(|w| w == expected);
    assert!(
        found,
        "replayed gain outputs differ: first run {} values, second run {}",
        gain_first.len(),
        gain_second.len()
    );
    // After the replay the source runs again.
    wait_metrics(&s, &d.keys, "a", |m| node_hz(m, "src") > 50.0);
    request(&s, &d.keys, Ctl::Stop, "").unwrap();
}

#[test]
fn estop_is_latched_and_silent_hosts_are_restarted() {
    let d = TestDaemon::start("safety", |c| {
        c.heartbeat_timeout = Duration::from_millis(1500);
    });
    let s = d.client();
    let logs = s.declare_subscriber(d.keys.all_logs()).wait().unwrap();

    // Engaged before the graph exists: hosts must start stopped.
    assert_eq!(
        request(&s, &d.keys, Ctl::Estop, "engage").unwrap(),
        "e-stop engaged"
    );
    let graph = "name: safety\nnodes:\n  arm: { type: std/fake-arm, group: hw }\n";
    request(&s, &d.keys, Ctl::Deploy, graph).unwrap();
    let start = Instant::now();
    let mut seen = false;
    while start.elapsed() < TIMEOUT && !seen {
        if let Ok(Some(sample)) = logs.recv_timeout(Duration::from_millis(200)) {
            let line: LogLine = ruro_proto::decode(&sample.payload().to_bytes()).unwrap();
            seen = line.message.contains("emergency stop");
        }
    }
    assert!(seen, "the arm never saw the e-stop");
    assert!(next_status(&s, &d.keys).estopped);
    assert_eq!(
        request(&s, &d.keys, Ctl::Estop, "release").unwrap(),
        "e-stop released"
    );
    assert!(request(&s, &d.keys, Ctl::Estop, "maybe").is_err());

    // A host that hangs (here: stopped) is killed and restarted.
    let st = wait_status(&s, &d.keys, TIMEOUT, |st| {
        st.groups.first().is_some_and(|g| g.pid.is_some())
    });
    let pid = st.groups[0].pid.unwrap();
    // SAFETY: plain signal delivery to the daemon's child.
    unsafe { libc::kill(pid as i32, libc::SIGSTOP) };
    let st = wait_status(&s, &d.keys, TIMEOUT, |st| {
        st.groups[0].restarts >= 1 && st.groups[0].pid.is_some_and(|p| p != pid)
    });
    let exit = st.groups[0].last_exit.as_deref().unwrap();
    assert!(exit.contains("no heartbeat"), "{exit}");
    request(&s, &d.keys, Ctl::Stop, "").unwrap();
}

#[test]
fn graphs_span_two_daemons() {
    let d1 = TestDaemon::start("left", |_| {});
    let peer = d1.endpoint.clone();
    let d2 = TestDaemon::start("right", |c| c.peers = vec![peer]);
    let s = d1.client();
    // Wait until the daemons see each other.
    let start = Instant::now();
    while ruro_proto::discover(&s, Some(d2.name()), Duration::from_millis(500)).is_err() {
        assert!(start.elapsed() < TIMEOUT, "peer daemon not reachable");
    }

    // An image source on `right` feeds statistics on `left` (images cross
    // the network compressed), and a sine on `left` feeds a gain on `right`.
    let graph = format!(
        r#"
name: span
groups:
  cams: {{ machine: {right} }}
  math: {{ machine: {right} }}
nodes:
  cam: {{ type: std/fake-camera, group: cams, rate: 30, params: {{ width: 160, height: 120 }} }}
  stats: {{ type: std/image-stats, group: vision }}
  src: {{ type: std/sine, group: control, rate: 100 }}
  gain: {{ type: std/gain, group: math }}
wires: [cam.image -> stats.image, src.value -> gain.input]
"#,
        right = d2.name()
    );
    let reply = request(&s, &d1.keys, Ctl::Deploy, &graph).unwrap();
    assert!(reply.contains(d2.name()), "{reply}");

    let m = wait_metrics(&s, &d1.keys, "vision", |m| node_hz(m, "stats") > 10.0);
    let stats = m.nodes.iter().find(|n| n.id == "stats").unwrap();
    assert!(stats.input_age_ms.is_some());
    wait_metrics(&s, &d2.keys, "math", |m| node_hz(m, "gain") > 50.0);
    let left = next_status(&s, &d1.keys);
    let right = next_status(&s, &d2.keys);
    let names = |st: &DaemonStatus| st.groups.iter().map(|g| g.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&left), ["control", "vision"]);
    assert_eq!(names(&right), ["cams", "math"]);

    // Stopping on the deploying daemon stops the peer's groups too.
    request(&s, &d1.keys, Ctl::Stop, "").unwrap();
    wait_status(&s, &d2.keys, TIMEOUT, |st| st.graph.is_none());
}

/// A CA and a certificate for `localhost` signed by it, as PEM files.
fn certificates(dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
    use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};
    std::fs::create_dir_all(dir).unwrap();
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let key = KeyPair::generate().unwrap();
    let cert = CertificateParams::new(vec!["localhost".to_owned()])
        .unwrap()
        .signed_by(&key, &ca, &ca_key)
        .unwrap();
    let paths = (
        dir.join("ca.pem"),
        dir.join("cert.pem"),
        dir.join("key.pem"),
    );
    std::fs::write(&paths.0, ca.pem()).unwrap();
    std::fs::write(&paths.1, cert.pem()).unwrap();
    std::fs::write(&paths.2, key.serialize_pem()).unwrap();
    paths
}

#[test]
fn tls_and_token_protect_the_daemon() {
    let (ca, cert, key) = certificates(&tmp("tls"));
    let d = TestDaemon::start("secure", |c| {
        c.listen = format!("tls/localhost:{}", free_port());
        c.security = Security {
            cert: Some(cert.clone()),
            key: Some(key.clone()),
            ca: None,
            token: Some("correct horse".into()),
        };
    });
    let good = Security {
        ca: Some(ca.clone()),
        token: Some("correct horse".into()),
        ..Default::default()
    };
    let s = d.client_with(&good).expect("trusted client with the token");
    request(&s, &d.keys, Ctl::Ping, "").unwrap();
    // Hosts authenticate over the daemon's local socket with the same token.
    let graph = "name: sec\nnodes:\n  src: { type: std/sine, rate: 50 }\n";
    request(&s, &d.keys, Ctl::Deploy, graph).unwrap();
    wait_metrics(&s, &d.keys, "main", |m| node_hz(m, "src") > 20.0);

    let reject = |security: Security, why: &str| {
        let ok = d
            .client_with(&security)
            .and_then(|s| request(&s, &d.keys, Ctl::Ping, ""))
            .is_ok();
        assert!(!ok, "a client {why} was let in");
    };
    reject(
        Security {
            ca: Some(ca.clone()),
            ..Default::default()
        },
        "without the token",
    );
    reject(
        Security {
            ca: Some(ca.clone()),
            token: Some("wrong".into()),
            ..Default::default()
        },
        "with a wrong token",
    );
    reject(
        Security {
            token: Some("correct horse".into()),
            ..Default::default()
        },
        "that does not trust the daemon's certificate",
    );
    request(&s, &d.keys, Ctl::Stop, "").unwrap();
}

#[test]
fn uploaded_host_binaries_run_groups() {
    let d = TestDaemon::start("upload", |_| {});
    let s = d.client();
    let binary = std::fs::read(env!("CARGO_BIN_EXE_ruro-host")).unwrap();
    let sha256 = {
        use sha2::Digest;
        sha2::Sha256::digest(&binary)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    let mut reply = String::new();
    for (i, part) in binary.chunks(4 << 20).enumerate() {
        let chunk = ruro_proto::UploadChunk {
            name: "custom-host".into(),
            offset: (i * (4 << 20)) as u64,
            total: binary.len() as u64,
            sha256: sha256.clone(),
            data: part.to_vec(),
        };
        reply = ruro_proto::request_bytes(
            &s,
            &d.keys,
            Ctl::Upload,
            ruro_proto::encode(&chunk),
            TIMEOUT,
        )
        .unwrap();
    }
    assert!(reply.contains("stored host `custom-host`"), "{reply}");

    let graph = "name: up\ngroups:\n  special: { host: custom-host }\nnodes:\n  a: { type: std/sine, group: special, rate: 50 }\n  b: { type: std/sine, rate: 50 }\n";
    request(&s, &d.keys, Ctl::Deploy, graph).unwrap();
    let st = wait_status(&s, &d.keys, TIMEOUT, |st| {
        st.groups.iter().all(|g| g.pid.is_some())
    });
    let exes: BTreeMap<String, PathBuf> = st
        .groups
        .iter()
        .map(|g| {
            let exe = std::fs::read_link(format!("/proc/{}/exe", g.pid.unwrap())).unwrap();
            (g.name.clone(), exe)
        })
        .collect();
    assert!(exes["special"].ends_with("hosts/custom-host"), "{exes:?}");
    assert_eq!(exes["main"], PathBuf::from(env!("CARGO_BIN_EXE_ruro-host")));
    wait_metrics(&s, &d.keys, "special", |m| node_hz(m, "a") > 20.0);

    let bad = "name: up2\ngroups:\n  g: { host: no-such-host }\nnodes:\n  a: { type: std/sine, group: g, rate: 5 }\n";
    let err = request(&s, &d.keys, Ctl::Deploy, bad).unwrap_err();
    assert!(format!("{err:#}").contains("no-such-host"));
    request(&s, &d.keys, Ctl::Stop, "").unwrap();
}
