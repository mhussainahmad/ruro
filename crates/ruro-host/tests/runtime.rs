//! Executor features: control wakeups, simulated time, deterministic groups,
//! timing metrics, e-stop, and record/replay.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use ruro::prelude::*;
use ruro_graph::{Graph, PortRef};
use ruro_host::{Executor, Options, Remote};
use ruro_record::{PlaybackControl, Player, Recording};

fn wait_until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ruro-host-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

/// Counts its steps; input-triggered.
#[derive(Node)]
#[node(kind = "test/counter")]
struct Counter {
    #[input]
    input: Input<Scalar>,
    #[param(default = 0.0)]
    knob: f64,
}

static COUNTER_STEPS: AtomicU64 = AtomicU64::new(0);

impl Node for Counter {
    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        COUNTER_STEPS.fetch_add(1, Ordering::Relaxed);
        while self.input.recv().is_some() {}
        Ok(())
    }
}

/// Sends one value per `kick` parameter change.
#[derive(Node)]
#[node(kind = "test/kicker", rate = 200)]
struct Kicker {
    #[output]
    out: Output<Scalar>,
    #[param(default = 0)]
    kicks: u32,
    sent: u32,
}

impl Node for Kicker {
    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        while self.sent < self.kicks {
            self.sent += 1;
            self.out.send(Scalar(self.sent as f64))?;
        }
        Ok(())
    }
}

#[test]
fn parameter_changes_do_not_step_input_triggered_nodes() {
    let graph = Graph::from_yaml(
        "name: rt1\nnodes:\n  kick: { type: test/kicker }\n  count: { type: test/counter }\nwires: [kick.out -> count.input]\n",
    )
    .unwrap();
    let exec = Executor::start(&graph, "main", Remote::Disabled).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let before = COUNTER_STEPS.load(Ordering::Relaxed);
    for i in 0..20 {
        exec.set_param("count", "knob", serde_json::json!(i as f64))
            .unwrap();
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        COUNTER_STEPS.load(Ordering::Relaxed),
        before,
        "parameter updates must not trigger steps"
    );
    exec.set_param("kick", "kicks", serde_json::json!(1))
        .unwrap();
    assert!(wait_until(Duration::from_secs(2), || {
        COUNTER_STEPS.load(Ordering::Relaxed) == before + 1
    }));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(COUNTER_STEPS.load(Ordering::Relaxed), before + 1);
    exec.stop();
}

/// Publishes simulated time: 10 ms of graph time per (wall-clock) step.
#[derive(Node)]
#[node(kind = "test/sim-clock", rate = 200)]
struct SimClockSrc {
    #[output]
    clock: Output<ruro::msg::Clock>,
}

impl Node for SimClockSrc {
    fn step(&mut self, ctx: &mut Ctx) -> Result {
        self.clock.send(ruro::msg::Clock {
            time_ns: 1_000_000_000 + ctx.steps() * 10_000_000,
        })
    }
}

/// Records graph time and dt of each step.
#[derive(Node)]
#[node(kind = "test/ticker")]
struct Ticker {
    #[output]
    out: Output<Scalar>,
}

static TICKS: Mutex<Vec<(u64, Duration)>> = Mutex::new(Vec::new());

impl Node for Ticker {
    fn step(&mut self, ctx: &mut Ctx) -> Result {
        TICKS.lock().push((ctx.now_ns(), ctx.dt()));
        self.out.send(Scalar(0.0))
    }
}

#[test]
fn timers_follow_simulated_time() {
    // The clock advances 10 ms per step at 200 Hz: simulated time runs at
    // twice wall speed, so a 50 Hz (graph time) ticker steps about 100 times
    // per wall second, each 20 ms of graph time apart.
    for groups in [false, true] {
        TICKS.lock().clear();
        let (cg, tg) = if groups {
            ("sim", "work")
        } else {
            ("main", "main")
        };
        let graph = Graph::from_yaml(&format!(
            "name: rt-sim{groups}\nclock: sim\nnodes:\n  clock: {{ type: test/sim-clock, group: {cg} }}\n  tick: {{ type: test/ticker, rate: 50, group: {tg} }}\n"
        ))
        .unwrap();
        let keys = ruro_proto::Keys::new(&format!("simtest{}", std::process::id()));
        let execs: Vec<Executor> = if groups {
            vec![
                Executor::start(&graph, "work", Remote::Shm(keys.clone())).unwrap(),
                Executor::start(&graph, "sim", Remote::Shm(keys)).unwrap(),
            ]
        } else {
            vec![Executor::start(&graph, "main", Remote::Disabled).unwrap()]
        };
        std::thread::sleep(Duration::from_millis(600));
        drop(execs);
        let ticks = TICKS.lock().clone();
        assert!(
            (30..=80).contains(&ticks.len()),
            "{} ticks in 0.6 s wall ({groups})",
            ticks.len()
        );
        for w in ticks.windows(2).skip(1) {
            assert_eq!(w[1].1, Duration::from_millis(20), "dt follows graph time");
        }
        assert!(ticks[0].0 >= 1_000_000_000, "now_ns is graph time");
    }
}

/// Doubles its input and counts what it saw.
#[derive(Node)]
#[node(kind = "test/det-probe")]
struct DetProbe {
    #[input]
    input: Input<Scalar>,
}

static DET_SEEN: AtomicU64 = AtomicU64::new(0);

impl Node for DetProbe {
    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        while self.input.recv().is_some() {
            DET_SEEN.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }
}

#[test]
fn deterministic_group_steps_in_order_without_drops() {
    // Declared consumer-first to show the order comes from the wires.
    let graph = Graph::from_yaml(
        r#"
name: rt-det
groups:
  main:
    realtime: { deterministic: true, rate: 1000 }
nodes:
  probe: { type: test/det-probe }
  gain: { type: std/gain, params: { k: 2.0 } }
  src: { type: std/sine, rate: 1000 }
  slow: { type: std/sine, rate: 250 }
wires: [src.value -> gain.input, gain.output -> probe.input]
"#,
    )
    .unwrap();
    let exec = Executor::start(&graph, "main", Remote::Disabled).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    exec.metrics();
    std::thread::sleep(Duration::from_millis(500));
    let m = exec.metrics();
    exec.stop();
    let node = |id: &str| m.nodes.iter().find(|n| n.id == id).unwrap().clone();
    let (src, gain, probe, slow) = (node("src"), node("gain"), node("probe"), node("slow"));
    // Every value produced in a cycle is consumed in the same cycle, so the
    // depth-1 queues never overflow.
    assert_eq!(gain.dropped + probe.dropped, 0);
    assert!(src.hz > 500.0, "src at {} Hz", src.hz);
    assert!((gain.hz - src.hz).abs() < src.hz * 0.05);
    assert!(
        (slow.hz - src.hz / 4.0).abs() < src.hz * 0.05,
        "slow at {} Hz",
        slow.hz
    );
    let t = src.timing.expect("timer nodes report timing");
    assert!(t.jitter_max_us >= t.jitter_p99_us);
    let produced = DET_SEEN.load(Ordering::Relaxed);
    assert!(produced > 300, "{produced}");
}

/// Busy-waits for `work_us` each step.
#[derive(Node)]
#[node(kind = "test/slow", rate = 100)]
struct Slow {
    #[param(default = 2000)]
    work_us: u64,
}

impl Node for Slow {
    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        let t = Instant::now();
        while t.elapsed() < Duration::from_micros(self.work_us) {
            std::hint::spin_loop();
        }
        Ok(())
    }
}

#[test]
fn deadlines_and_jitter_are_reported() {
    let graph = Graph::from_yaml(
        r#"
name: rt-deadline
groups:
  main:
    realtime: { deadlines_us: { slow: 500 } }
nodes:
  slow: { type: test/slow }
  fast: { type: std/sine, rate: 200 }
"#,
    )
    .unwrap();
    let exec = Executor::start(&graph, "main", Remote::Disabled).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    let m = exec.metrics();
    exec.stop();
    let slow = m.nodes.iter().find(|n| n.id == "slow").unwrap();
    let t = slow.timing.as_ref().unwrap();
    assert!(t.deadline_misses >= 10, "{t:?}");
    assert!(slow.errors >= t.deadline_misses);
    assert!(slow.last_error.as_deref().unwrap().contains("deadline"));
    let fast = m.nodes.iter().find(|n| n.id == "fast").unwrap();
    let ft = fast.timing.as_ref().expect("timer nodes report jitter");
    assert_eq!(ft.deadline_misses, 0);
    assert!(ft.jitter_p50_us <= ft.jitter_p99_us && ft.jitter_p99_us <= ft.jitter_max_us);
}

/// A pretend actuator: commands motion unless e-stopped.
#[derive(Node)]
#[node(kind = "test/actuator", rate = 200)]
struct Actuator {
    #[output]
    command: Output<Scalar>,
}

static ESTOP_CALLS: AtomicU64 = AtomicU64::new(0);
static LAST_COMMAND: Mutex<f64> = Mutex::new(f64::NAN);

impl Node for Actuator {
    fn step(&mut self, ctx: &mut Ctx) -> Result {
        let v = if ctx.estopped() { 0.0 } else { 1.0 };
        *LAST_COMMAND.lock() = v;
        self.command.send(Scalar(v))
    }

    fn on_estop(&mut self, _ctx: &mut Ctx) {
        ESTOP_CALLS.fetch_add(1, Ordering::Relaxed);
        *LAST_COMMAND.lock() = 0.0;
    }
}

#[test]
fn estop_reaches_nodes_and_can_be_released() {
    let graph =
        Graph::from_yaml("name: rt-estop\nnodes:\n  arm: { type: test/actuator }\n").unwrap();
    let exec = Executor::start(&graph, "main", Remote::Disabled).unwrap();
    assert!(wait_until(Duration::from_secs(1), || *LAST_COMMAND.lock() == 1.0));
    exec.set_estop(true);
    assert!(wait_until(Duration::from_secs(1), || ESTOP_CALLS
        .load(Ordering::Relaxed)
        == 1));
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(*LAST_COMMAND.lock(), 0.0);
    exec.set_estop(true);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(
        ESTOP_CALLS.load(Ordering::Relaxed),
        1,
        "engaging twice calls the hook once"
    );
    exec.set_estop(false);
    assert!(wait_until(Duration::from_secs(1), || *LAST_COMMAND.lock() == 1.0));
    exec.stop();

    // A host started while the e-stop is latched comes up stopped.
    *LAST_COMMAND.lock() = f64::NAN;
    let mut opts = Options::new(Remote::Disabled);
    opts.estopped = true;
    let exec = Executor::start_with(&graph, "main", opts).unwrap();
    assert!(wait_until(Duration::from_secs(1), || *LAST_COMMAND.lock() == 0.0));
    assert!(exec.estopped());
    exec.stop();
}

/// Values of `topic` in a recording, in order.
fn recorded(path: &std::path::Path, topic: &str) -> Vec<f64> {
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
fn replay_reproduces_downstream_outputs() {
    let graph = Graph::from_yaml(
        r#"
name: rt-replay
nodes:
  src: { type: std/sine, rate: 200, params: { frequency: 3.0, amplitude: 2.0 } }
  gain: { type: std/gain, params: { k: -1.5 } }
wires:
  - { from: src.value, to: gain.input, depth: 16 }
"#,
    )
    .unwrap();

    // Run 1: record the live graph.
    let first = tmp("replay-first.mcap");
    let exec = Executor::start(&graph, "main", Remote::Disabled).unwrap();
    exec.start_recording(&first).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    let (written, dropped) = exec.stop_recording().unwrap().unwrap();
    exec.stop();
    assert_eq!(dropped, 0);
    assert!(written > 100);
    let info = Recording::open(&first).unwrap().info().unwrap();
    let src_channel = info
        .channels
        .iter()
        .find(|c| c.topic == "src.value")
        .unwrap();
    assert_eq!(
        (src_channel.ty.as_str(), src_channel.encoding.as_str()),
        ("std/Scalar", ruro_record::ENCODING_POSTCARD)
    );

    // Run 2: replay `src` and record what `gain` does with it.
    let second = tmp("replay-second.mcap");
    let mut opts = Options::new(Remote::Disabled);
    opts.muted = vec!["src".into()];
    let exec = Executor::start_with(&graph, "main", opts).unwrap();
    exec.start_recording(&second).unwrap();
    let player = Player::open(&first).unwrap();
    let ctl = PlaybackControl::new(2.0);
    let src = PortRef::new("src", "value");
    player
        .run(&ctl, |m| {
            if m.topic() == "src.value" {
                exec.inject(&src, m.seq as u64, ruro_msg::now_ns(), &m.data)
                    .unwrap();
            }
        })
        .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    exec.stop_recording().unwrap();
    exec.stop();

    let src_first = recorded(&first, "src.value");
    let gain_first = recorded(&first, "gain.output");
    let src_second = recorded(&second, "src.value");
    let gain_second = recorded(&second, "gain.output");
    // The live run may have recorded a few last src values whose gain output
    // was still in flight when recording stopped.
    let n = gain_first.len();
    assert!(src_first.len() >= n && src_first.len() <= n + 3);
    assert_eq!(
        gain_second.len(),
        src_second.len(),
        "replay ran to completion"
    );
    assert_eq!(
        src_second, src_first,
        "replayed outputs are recorded as sent"
    );
    assert_eq!(
        gain_second[..n],
        gain_first[..],
        "downstream re-ran identically"
    );
    assert!(gain_first.iter().any(|v| *v != gain_first[0]));
}
