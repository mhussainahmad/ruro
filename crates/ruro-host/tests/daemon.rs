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

/// Generous: under emulation (the aarch64 CI job) hosts start and restart
/// many times slower.
const TIMEOUT: Duration = Duration::from_secs(45);

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

/// Removes files and directories made with `tmp`. (The directory holding
/// them stays: tests running in parallel share it.)
fn cleanup(paths: &[&Path]) {
    for p in paths {
        let _ = std::fs::remove_file(p).or_else(|_| std::fs::remove_dir_all(p));
    }
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
        // The runtime directory holds sockets, graphs and uploaded binaries.
        let _ = std::fs::remove_dir_all(ruro_daemon::runtime_dir(self.name()));
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
    // The two groups start and stop recording at slightly different times
    // (further apart on a loaded machine), so match values rather than
    // positions, and allow for the ends.
    let halves: Vec<f64> = src_first.iter().map(|v| v * 0.5).collect();
    let matched = gain_first.iter().filter(|g| halves.contains(g)).count();
    assert!(
        matched + 3.max(gain_first.len() / 20) >= gain_first.len(),
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
    // then the gain of every recorded source value, in order. (Not the gain
    // outputs of the first run: its groups stopped recording a few messages
    // apart, more so on a slow machine.)
    let gain_second = values(&second, "gain.output");
    let expected = &halves[..halves.len() - 1];
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
    cleanup(&[&first, &replay, &second]);
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
    // Clients get back the graph as they deployed it.
    assert_eq!(request(&s, &d1.keys, Ctl::Graph, "").unwrap(), graph);

    let m = wait_metrics(&s, &d1.keys, "vision", |m| node_hz(m, "stats") > 10.0);
    let stats = m.nodes.iter().find(|n| n.id == "stats").unwrap();
    assert!(stats.input_age_ms.is_some());
    wait_metrics(&s, &d2.keys, "math", |m| node_hz(m, "gain") > 50.0);
    let left = next_status(&s, &d1.keys);
    let right = next_status(&s, &d2.keys);
    let names = |st: &DaemonStatus| st.groups.iter().map(|g| g.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&left), ["control", "vision"]);
    assert_eq!(names(&right), ["cams", "math"]);

    // Parameters of nodes on the peer are set through the deploying daemon.
    let reply = request(&s, &d1.keys, Ctl::Param, &param("gain", "k", "-2")).unwrap();
    assert!(reply.contains("gain.k = -2"), "{reply}");

    // Stopping on the deploying daemon stops the peer's groups too.
    request(&s, &d1.keys, Ctl::Stop, "").unwrap();
    wait_status(&s, &d2.keys, TIMEOUT, |st| st.graph.is_none());
}

fn param(node: &str, name: &str, value: &str) -> String {
    serde_json::to_string(&ruro_proto::ParamRequest {
        node: node.into(),
        name: name.into(),
        value: value.into(),
    })
    .unwrap()
}

#[test]
fn parameters_are_checked_and_confirmed() {
    let d = TestDaemon::start("params", |_| {});
    let s = d.client();
    let graph = "name: params\nnodes:\n  src: { type: std/sine, rate: 50 }\n  gain: { type: std/gain, group: b }\nwires: [src.value -> gain.input]\n";
    request(&s, &d.keys, Ctl::Deploy, graph).unwrap();
    wait_metrics(&s, &d.keys, "b", |m| node_hz(m, "gain") > 20.0);

    let set = |node: &str, name: &str, value: &str| {
        request(&s, &d.keys, Ctl::Param, &param(node, name, value)).map_err(|e| format!("{e:#}"))
    };
    let reply = set("gain", "k", "-2.5").unwrap();
    assert_eq!(reply, "gain.k = -2.5");
    let err = set("gain", "kk", "1").unwrap_err();
    assert!(err.contains("did you mean `k`?"), "{err}");
    let err = set("gain", "k", "fast").unwrap_err();
    assert!(err.contains("expects a number"), "{err}");
    let err = set("gain", "k", "5000").unwrap_err();
    assert!(err.contains("outside [-1000, 1000]"), "{err}");
    let err = set("nobody", "k", "1").unwrap_err();
    assert!(err.contains("no node `nobody`"), "{err}");

    // The host checks values sent without the daemon too: an invalid one is
    // logged and dropped, a valid one applied.
    let logs = s.declare_subscriber(d.keys.all_logs()).wait().unwrap();
    let put = |value: &str| {
        let msg = ruro_proto::SetParam {
            name: "k".into(),
            value: value.into(),
        };
        s.put(d.keys.param("gain"), ruro_proto::encode(&msg))
            .wait()
            .unwrap();
    };
    put("\"high\"");
    put("3");
    let (mut refused, mut applied) = (false, false);
    let start = Instant::now();
    while start.elapsed() < TIMEOUT && !(refused && applied) {
        if let Ok(Some(sample)) = logs.recv_timeout(Duration::from_millis(200)) {
            let line: LogLine = ruro_proto::decode(&sample.payload().to_bytes()).unwrap();
            refused |= line.message.contains("expects a number");
            applied |= line.message.contains("gain.k = 3");
        }
    }
    assert!(refused && applied, "refused {refused}, applied {applied}");
    request(&s, &d.keys, Ctl::Stop, "").unwrap();
}

/// Two daemons, the second connected to the first, both answering.
fn two_daemons(left: &str, right: &str) -> (TestDaemon, TestDaemon, zenoh::Session) {
    let d1 = TestDaemon::start(left, |_| {});
    let peer = d1.endpoint.clone();
    let d2 = TestDaemon::start(right, |c| c.peers = vec![peer]);
    let s = d1.client();
    let start = Instant::now();
    while ruro_proto::discover(&s, Some(d2.name()), Duration::from_millis(500)).is_err() {
        assert!(start.elapsed() < TIMEOUT, "peer daemon not reachable");
    }
    (d1, d2, s)
}

/// A source on `right`, a gain here and a second gain on `right` again, so
/// data crosses machines both ways.
fn spanning_graph(right: &str) -> String {
    format!(
        r#"
name: rec-span
groups:
  a: {{ machine: {right} }}
  c: {{ machine: {right} }}
nodes:
  src: {{ type: std/sine, group: a, rate: 100, params: {{ frequency: 2.0 }} }}
  gain: {{ type: std/gain, group: b, params: {{ k: 0.5 }} }}
  twice: {{ type: std/gain, group: c, params: {{ k: 2.0 }} }}
wires:
  - {{ from: src.value, to: gain.input, depth: 16 }}
  - {{ from: gain.output, to: twice.input, depth: 16 }}
"#
    )
}

#[test]
fn recording_and_replay_cover_every_machine() {
    let (d1, d2, s) = two_daemons("recleft", "recright");
    request(&s, &d1.keys, Ctl::Deploy, &spanning_graph(d2.name())).unwrap();
    wait_metrics(&s, &d1.keys, "b", |m| node_hz(m, "gain") > 50.0);
    wait_metrics(&s, &d2.keys, "c", |m| node_hz(m, "twice") > 50.0);

    let first = tmp("span-first.mcap");
    let reply = request(&s, &d1.keys, Ctl::Record, &first.display().to_string()).unwrap();
    assert!(
        reply.contains("recording 3 group(s)")
            && reply.contains(&format!("on `{}`: a, c", d2.name())),
        "{reply}"
    );
    let peer_status = next_status(&s, &d2.keys);
    assert!(
        peer_status
            .recording
            .as_deref()
            .unwrap_or_default()
            .contains(d1.name()),
        "{peer_status:?}"
    );
    std::thread::sleep(Duration::from_millis(1500));
    let reply = request(&s, &d1.keys, Ctl::Record, "").unwrap();
    assert!(
        reply.contains("3 channel(s)") && reply.contains(&format!("from `{}`: a, c", d2.name())),
        "{reply}"
    );
    // Both daemons share this machine's clock: measured, and nothing to correct.
    assert!(!reply.contains("clock"), "{reply}");
    wait_status(&s, &d2.keys, TIMEOUT, |st| st.recording.is_none());

    // One file, with every channel of both machines on one timeline.
    let rec = Recording::open(&first).unwrap();
    let info = rec.info().unwrap();
    let topics: Vec<&str> = info.channels.iter().map(|c| c.topic.as_str()).collect();
    assert_eq!(
        topics
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        ["gain.output", "src.value", "twice.output"].into(),
        "{topics:?}"
    );
    let times: Vec<u64> = rec
        .messages()
        .unwrap()
        .map(|m| m.unwrap().log_time)
        .collect();
    assert!(times.windows(2).all(|w| w[0] <= w[1]), "out of order");
    // Each value travels src (peer) -> gain (here) -> twice (peer): its
    // stamps grow along the way, by much less than a sample period. (Only
    // values sent once are matched up.)
    let stamps = |topic: &str| -> BTreeMap<u64, u64> {
        let mut seen: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
        for m in rec.messages().unwrap().map(Result::unwrap) {
            if m.topic() == topic {
                let v = Scalar::decode(&m.data).unwrap().0.to_bits();
                seen.entry(v).or_default().push(m.log_time);
            }
        }
        seen.into_iter()
            .filter(|(_, t)| t.len() == 1)
            .map(|(v, t)| (v, t[0]))
            .collect()
    };
    let (src, gain, twice) = (
        stamps("src.value"),
        stamps("gain.output"),
        stamps("twice.output"),
    );
    let mut hops = 0;
    for (v, t) in &src {
        let half = (f64::from_bits(*v) * 0.5).to_bits();
        let (Some(g), Some(w)) = (gain.get(&half), twice.get(v)) else {
            continue;
        };
        assert!(t <= g && g <= w, "src {t}, gain {g}, twice {w}");
        assert!(w - t < 50_000_000, "src {t}, twice {w}");
        hops += 1;
    }
    assert!(hops > 50, "{hops} values made it through all three nodes");
    let gain_first = values(&first, "gain.output");
    let twice_first = values(&first, "twice.output");
    assert!(gain_first.len() > 100, "{}", gain_first.len());

    // Replaying the source mutes it on the peer and feeds the gain here,
    // whose outputs feed the second gain on the peer.
    let replay = tmp("span-src-only.mcap");
    filter(&first, &replay, &["src.value"]);
    let second = tmp("span-second.mcap");
    request(&s, &d1.keys, Ctl::Record, &second.display().to_string()).unwrap();
    let start = PlaybackCommand::Start {
        file: replay.display().to_string(),
        rate: 2.0,
    };
    let reply = request(
        &s,
        &d1.keys,
        Ctl::Playback,
        &serde_json::to_string(&start).unwrap(),
    )
    .unwrap();
    assert!(
        reply.contains("replaying src") && !reply.contains("not replayed"),
        "{reply}"
    );
    wait_status(&s, &d1.keys, Duration::from_secs(3), |st| {
        st.playback.is_some()
    });
    wait_status(&s, &d1.keys, TIMEOUT, |st| st.playback.is_none());
    std::thread::sleep(Duration::from_millis(300));
    request(&s, &d1.keys, Ctl::Record, "").unwrap();
    // What the replayed source values make the gains compute (not the gain
    // outputs of the first run, whose groups stopped recording a few
    // messages apart).
    let src_first = values(&first, "src.value");
    let halves: Vec<f64> = src_first.iter().map(|v| v * 0.5).collect();
    let contains = |run: &[f64], first: &[f64]| {
        let expected = &first[1..first.len() - 1];
        run.windows(expected.len()).any(|w| w == expected)
    };
    let gain_second = values(&second, "gain.output");
    assert!(
        contains(&gain_second, &halves),
        "replayed gain outputs differ: first run {} values, second run {}",
        gain_first.len(),
        gain_second.len()
    );
    let twice_second = values(&second, "twice.output");
    assert!(
        contains(&twice_second, &src_first),
        "the gain on the peer was not fed: first run {} values, second run {}",
        twice_first.len(),
        twice_second.len()
    );
    // After the replay the source on the peer runs again.
    wait_metrics(&s, &d2.keys, "a", |m| node_hz(m, "src") > 50.0);
    request(&s, &d1.keys, Ctl::Stop, "").unwrap();
    cleanup(&[&first, &replay, &second]);
}

#[test]
fn a_recording_keeps_what_was_recorded_when_a_peer_disappears() {
    let (d1, mut d2, s) = two_daemons("recgone", "recvanish");
    let right = d2.name().to_owned();
    request(&s, &d1.keys, Ctl::Deploy, &spanning_graph(&right)).unwrap();
    wait_metrics(&s, &d1.keys, "b", |m| node_hz(m, "gain") > 50.0);

    let file = tmp("span-gone.mcap");
    let reply = request(&s, &d1.keys, Ctl::Record, &file.display().to_string()).unwrap();
    assert!(reply.contains(&format!("on `{right}`: a, c")), "{reply}");
    std::thread::sleep(Duration::from_millis(800));
    // The peer machine goes away mid-recording.
    d2.daemon.take().unwrap().shutdown();
    std::thread::sleep(Duration::from_millis(300));

    let start = Instant::now();
    let reply = request(&s, &d1.keys, Ctl::Record, "").unwrap();
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
    assert!(
        reply.contains(&format!("not recorded: the groups on `{right}`")),
        "{reply}"
    );
    let info = Recording::open(&file).unwrap().info().unwrap();
    let topics: Vec<&str> = info.channels.iter().map(|c| c.topic.as_str()).collect();
    assert_eq!(topics, ["gain.output"], "{reply}");
    assert!(values(&file, "gain.output").len() > 30);
    let _ = request(&s, &d1.keys, Ctl::Stop, "");
    cleanup(&[&file]);
}

/// A CA and a certificate for `localhost` signed by it, as PEM files.
fn certificates(dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let (ca, mut certs) = certificates_for(dir, &["localhost"]);
    let (cert, key) = certs.remove(0);
    (ca, cert, key)
}

/// A CA and, for each name, a certificate signed by it: `(ca, [(cert, key)])`.
fn certificates_for(dir: &Path, names: &[&str]) -> (PathBuf, Vec<(PathBuf, PathBuf)>) {
    use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};
    std::fs::create_dir_all(dir).unwrap();
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let ca_path = dir.join("ca.pem");
    std::fs::write(&ca_path, ca.pem()).unwrap();
    let certs = names
        .iter()
        .map(|name| {
            let key = KeyPair::generate().unwrap();
            let cert = CertificateParams::new(vec![(*name).to_owned()])
                .unwrap()
                .signed_by(&key, &ca, &ca_key)
                .unwrap();
            let paths = (
                dir.join(format!("{name}.pem")),
                dir.join(format!("{name}-key.pem")),
            );
            std::fs::write(&paths.0, cert.pem()).unwrap();
            std::fs::write(&paths.1, key.serialize_pem()).unwrap();
            paths
        })
        .collect();
    (ca_path, certs)
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

    // Refused sessions are reported as such, not as a missing daemon.
    let reject = |security: Security, why: &str, hint: &str| {
        let err = match d.client_with(&security) {
            Ok(s) => match request(&s, &d.keys, Ctl::Ping, "") {
                Ok(_) => panic!("a client {why} was let in"),
                Err(e) => e,
            },
            Err(e) => e,
        };
        let failure = ruro_proto::ConnectError::failure_of(&err);
        assert_eq!(
            failure,
            Some(ruro_proto::ConnectFailure::Refused),
            "{why}: {err:#}"
        );
        let msg = format!("{err:#}");
        assert!(
            msg.contains(hint) && !msg.contains("ruro daemon`"),
            "{why}: {msg}"
        );
    };
    reject(
        Security {
            ca: Some(ca.clone()),
            ..Default::default()
        },
        "without the token",
        "token",
    );
    reject(
        Security {
            ca: Some(ca.clone()),
            token: Some("wrong".into()),
            ..Default::default()
        },
        "with a wrong token",
        "token",
    );
    reject(
        Security {
            token: Some("correct horse".into()),
            ..Default::default()
        },
        "that does not trust the daemon's certificate",
        "--tls-ca",
    );
    let (other_ca, _) = certificates_for(&tmp("tls-other"), &["localhost"]);
    reject(
        Security {
            ca: Some(other_ca),
            token: Some("correct horse".into()),
            ..Default::default()
        },
        "that trusts another CA",
        "--tls-ca",
    );
    cleanup(&[&tmp("tls-other")]);
    request(&s, &d.keys, Ctl::Stop, "").unwrap();
    cleanup(&[&tmp("tls")]);
}

#[test]
fn mutual_tls_requires_client_certificates() {
    let (ca, certs) = certificates_for(&tmp("mtls"), &["localhost", "studio"]);
    let [(cert, key), (client_cert, client_key)] = <[_; 2]>::try_from(certs).unwrap();
    let d = TestDaemon::start("mtls", |c| {
        c.listen = format!("tls/localhost:{}", free_port());
        c.security = Security {
            cert: Some(cert.clone()),
            key: Some(key.clone()),
            ca: Some(ca.clone()),
            token: None,
        };
    });
    let with_cert = Security {
        cert: Some(client_cert),
        key: Some(client_key),
        ca: Some(ca.clone()),
        token: None,
    };
    let s = d
        .client_with(&with_cert)
        .expect("client with a certificate");
    request(&s, &d.keys, Ctl::Ping, "").unwrap();
    let without = Security {
        ca: Some(ca),
        ..Default::default()
    };
    let err = d
        .client_with(&without)
        .and_then(|s| request(&s, &d.keys, Ctl::Ping, ""))
        .expect_err("a client without a certificate was let in");
    assert!(format!("{err:#}").contains("client certificate"), "{err:#}");
    cleanup(&[&tmp("mtls")]);
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
