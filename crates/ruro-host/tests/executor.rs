//! End-to-end tests of the executor using the standard nodes.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use ruro::prelude::*;
use ruro_graph::Graph;
use ruro_host::{Executor, Remote};
use ruro_proto::{Keys, NodeState};

/// Records every scalar it receives.
#[derive(Node)]
#[node(kind = "test/probe")]
struct Probe {
    #[input]
    input: Input<Scalar>,
}

static PROBE: Mutex<Vec<f64>> = Mutex::new(Vec::new());
static PROBE_SEEN: AtomicU64 = AtomicU64::new(0);

impl Node for Probe {
    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        while let Some(s) = self.input.recv() {
            PROBE.lock().push(s.0);
            PROBE_SEEN.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }
}

/// Panics on its third step.
#[derive(Node)]
#[node(kind = "test/crashy", rate = 200)]
struct Crashy {
    #[output]
    out: Output<Scalar>,
}

impl Node for Crashy {
    fn step(&mut self, ctx: &mut Ctx) -> Result {
        if ctx.steps() == 2 {
            panic!("boom");
        }
        self.out.send(Scalar(1.0))
    }
}

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

// The probe is global, so tests that use it run in one test function.
#[test]
fn pipelines() {
    in_process_pipeline_and_live_params();
    cross_process_pipeline_over_shared_memory();
}

fn in_process_pipeline_and_live_params() {
    PROBE.lock().clear();
    let graph = Graph::from_yaml(
        r#"
name: t1
nodes:
  src: { type: std/sine, rate: 500, params: { frequency: 0.0, offset: 3.0 } }
  gain: { type: std/gain, params: { k: 2.0 } }
  probe: { type: test/probe }
wires: [src.value -> gain.input, gain.output -> probe.input]
"#,
    )
    .unwrap();
    let exec = Executor::start(&graph, "main", Remote::Disabled).unwrap();
    assert!(wait_until(Duration::from_secs(2), || PROBE.lock().len() > 10));
    assert!(
        PROBE.lock().iter().all(|&v| v == 6.0),
        "{:?}",
        &PROBE.lock()[..5]
    );

    exec.set_param("gain", "k", serde_json::json!(-1.0))
        .unwrap();
    assert!(wait_until(Duration::from_secs(2), || PROBE.lock().last()
        == Some(&-3.0)));

    std::thread::sleep(Duration::from_millis(300));
    let m = exec.metrics();
    let src = m.nodes.iter().find(|n| n.id == "src").unwrap();
    assert_eq!(src.state, NodeState::Running);
    assert!(src.hz > 100.0, "src ran at {} Hz", src.hz);
    let probe = m.nodes.iter().find(|n| n.id == "probe").unwrap();
    assert!(probe.input_age_ms.is_some());
    exec.stop();
}

fn cross_process_pipeline_over_shared_memory() {
    PROBE.lock().clear();
    let graph = Graph::from_yaml(
        r#"
name: t2
nodes:
  src: { type: std/sine, group: a, rate: 500, params: { frequency: 0.0, offset: 5.0 } }
  probe: { type: test/probe, group: b }
wires: [src.value -> probe.input]
"#,
    )
    .unwrap();
    let keys = Keys::new(&format!("test{}", std::process::id()));
    let b = Executor::start(&graph, "b", Remote::Shm(keys.clone())).unwrap();
    let a = Executor::start(&graph, "a", Remote::Shm(keys)).unwrap();
    assert!(
        wait_until(Duration::from_secs(3), || PROBE.lock().len() > 10),
        "no data crossed groups"
    );
    assert!(PROBE.lock().iter().all(|&v| v == 5.0));
    a.stop();
    b.stop();
}

#[test]
fn panicking_node_is_isolated() {
    let graph = Graph::from_yaml(
        r#"
name: t3
nodes:
  bad: { type: test/crashy }
  good: { type: std/sine, rate: 200 }
"#,
    )
    .unwrap();
    let exec = Executor::start(&graph, "main", Remote::Disabled).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let m = exec.metrics();
    let bad = m.nodes.iter().find(|n| n.id == "bad").unwrap();
    let good = m.nodes.iter().find(|n| n.id == "good").unwrap();
    assert_eq!(bad.state, NodeState::Failed);
    assert!(bad.last_error.as_deref().unwrap().contains("boom"));
    assert_eq!(good.state, NodeState::Running);
    assert!(good.hz > 50.0);
    exec.stop();
}

#[test]
fn taps_fire_only_when_active() {
    let graph = Graph::from_yaml("name: t4\nnodes:\n  cam: { type: std/fake-camera, rate: 200, params: { width: 64, height: 48 } }\n").unwrap();
    let exec = Executor::start(&graph, "main", Remote::Disabled).unwrap();
    let out = exec.outputs().values().next().unwrap().clone();
    let hits = Arc::new(AtomicU64::new(0));
    let h = hits.clone();
    out.tap().set_sink(Arc::new(move |p| {
        assert!(matches!(
            p,
            ruro_msg::Preview::Image {
                width: 64,
                height: 48,
                ..
            }
        ));
        h.fetch_add(1, Ordering::Relaxed);
    }));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(hits.load(Ordering::Relaxed), 0);
    out.tap().set_active(true);
    std::thread::sleep(Duration::from_millis(400));
    let n = hits.load(Ordering::Relaxed);
    assert!(
        (5..=20).contains(&n),
        "tap fired {n} times in 400ms (limit is 30 Hz)"
    );
    exec.stop();
}

#[test]
fn unknown_node_type_fails_to_start() {
    let graph = Graph::from_yaml("name: t5\nnodes:\n  x: { type: nope/nope }\n").unwrap();
    let err = Executor::start(&graph, "main", Remote::Disabled)
        .err()
        .unwrap();
    assert!(format!("{err:#}").contains("unknown node type"));
}
