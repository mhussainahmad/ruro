//! End-to-end ONNX Runtime tests with the tiny models in tests/models
//! (built by tools/make_test_models.py).

#![cfg(feature = "onnx")]

use std::time::{Duration, Instant};

use parking_lot::Mutex;
use ruro::prelude::*;
use ruro_graph::Graph;
use ruro_host::{Executor, Remote};
use ruro_nodes_ml::Detections;
use ruro_nodes_ml::onnx::Model;

fn model(name: &str) -> String {
    format!("{}/tests/models/{name}", env!("CARGO_MANIFEST_DIR"))
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

/// Uniform grey image.
#[derive(Node)]
#[node(kind = "test/grey", rate = 50)]
struct Grey {
    #[output]
    image: Output<Image>,
    #[param(default = 255)]
    level: u8,
    #[param(default = 64)]
    size: u32,
}

impl Node for Grey {
    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        let mut img = Image::new(self.size, self.size, Encoding::Rgb8);
        img.data.fill(self.level);
        self.image.send(img)
    }
}

#[derive(Node)]
#[node(kind = "test/state", rate = 50)]
struct State {
    #[output]
    state: Output<JointState>,
}

impl Node for State {
    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        self.state.send(JointState {
            position: vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
            ..Default::default()
        })
    }
}

static DETECTIONS: Mutex<Vec<Detections>> = Mutex::new(Vec::new());
static ACTIONS: Mutex<Vec<Vec<f64>>> = Mutex::new(Vec::new());

#[derive(Node)]
#[node(kind = "test/sink")]
struct Sink {
    #[input]
    detections: Input<Detections>,
    #[input]
    action: Input<JointCommand>,
}

impl Node for Sink {
    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        while let Some(d) = self.detections.recv() {
            DETECTIONS.lock().push(d.msg.clone());
        }
        while let Some(a) = self.action.recv() {
            ACTIONS.lock().push(a.position.clone());
        }
        Ok(())
    }
}

#[test]
fn model_metadata_and_direct_run() {
    let mut m = Model::load(&model("detector.onnx"), 1).unwrap();
    assert_eq!(m.inputs[0].name, "images");
    assert_eq!(m.inputs[0].shape, [1, 3, 32, 32]);
    assert_eq!(m.outputs[0].shape, [1, 6, 3]);
    let out = m
        .run(vec![(
            "images".into(),
            vec![1, 3, 32, 32],
            vec![0.5; 3 * 32 * 32],
        )])
        .unwrap();
    let (name, t) = &out[0];
    assert_eq!(name, "output0");
    assert_eq!(t.shape, [1, 6, 3]);
    // Row 4 (class 0 scores): anchor 0 carries the input mean.
    assert!((t.data[4 * 3] - 0.5).abs() < 1e-6);
    assert_eq!(&t.data[5 * 3..], &[0.0, 0.9, 0.8]);
}

#[test]
fn graphs() {
    detector_graph();
    policy_graph();
}

fn detector_graph() {
    let graph = Graph::from_yaml(&format!(
        r#"
name: det
nodes:
  cam: {{ type: test/grey, params: {{ level: 255, size: 64 }} }}
  det:
    type: ml/onnx
    params: {{ model: "{}", postprocess: yolo, labels: "cup,ball", confidence: 0.5 }}
  sink: {{ type: test/sink }}
wires: [cam.image -> det.image, det.detections -> sink.detections]
"#,
        model("detector.onnx")
    ))
    .unwrap();
    let exec = Executor::start(&graph, "main", Remote::Disabled).unwrap();
    let ok = wait_until(Duration::from_secs(10), || !DETECTIONS.lock().is_empty());
    let m = exec.metrics();
    exec.stop();
    let n = m.nodes.iter().find(|n| n.id == "det").unwrap();
    assert!(ok, "no detections; node error: {:?}", n.last_error);
    let d = DETECTIONS.lock()[0].clone();
    // White image: anchor 0 (class 0, score 1.0) and anchor 1 (class 1, 0.9);
    // anchor 2 overlaps anchor 1 and is suppressed. Boxes scale x2 to 64x64.
    assert_eq!(d.boxes.len(), 2, "{d:?}");
    let a = &d.boxes[0];
    assert_eq!((a.label.as_str(), a.class_id), ("cup", 0));
    assert!((a.score - 1.0).abs() < 1e-6);
    assert_eq!((a.x, a.y, a.w, a.h), (8.0, 8.0, 16.0, 16.0));
    let b = &d.boxes[1];
    assert_eq!((b.label.as_str(), b.score), ("ball", 0.9));
    assert_eq!((b.x, b.y, b.w, b.h), (30.0, 22.0, 20.0, 20.0));
}

fn policy_graph() {
    // Image level 51 -> 0.2 after scaling; the policy adds 0.2 and 0.4.
    let graph = Graph::from_yaml(&format!(
        r#"
name: pol
nodes:
  cam: {{ type: test/grey, params: {{ level: 51, size: 16 }} }}
  robot: {{ type: test/state }}
  policy: {{ type: ml/policy, rate: 50, params: {{ model: "{}" }} }}
  sink: {{ type: test/sink }}
wires:
  - cam.image -> policy.image
  - robot.state -> policy.state
  - policy.action -> sink.action
"#,
        model("policy.onnx")
    ))
    .unwrap();
    let exec = Executor::start(&graph, "main", Remote::Disabled).unwrap();
    let ok = wait_until(Duration::from_secs(10), || ACTIONS.lock().len() >= 4);
    let m = exec.metrics();
    exec.stop();
    let n = m.nodes.iter().find(|n| n.id == "policy").unwrap();
    assert!(ok, "no actions; node error: {:?}", n.last_error);
    let actions = ACTIONS.lock().clone();
    let expect = |k: f64| -> Vec<f64> { (1..=6).map(|j| j as f64 + k).collect() };
    for (i, a) in actions.iter().take(4).enumerate() {
        let k = if i % 2 == 0 { 0.2 } else { 0.4 };
        let e = expect(k);
        assert!(
            a.iter().zip(&e).all(|(x, y)| (x - y).abs() < 1e-5),
            "action {i}: {a:?} vs {e:?}"
        );
    }
}

#[test]
fn policy_sends_nothing_while_stopped_and_replans_after_the_release() {
    use ruro::node::testing::Harness;
    let mut h = Harness::new(
        "ml/policy",
        serde_json::json!({ "model": model("policy.onnx") }),
    )
    .unwrap();
    let mut img = Image::new(16, 16, Encoding::Rgb8);
    img.data.fill(51);
    let state = |base: f64| JointState {
        position: (1..=6).map(|j| base + j as f64).collect(),
        ..Default::default()
    };
    let actions = |h: &mut Harness| -> Vec<Vec<f64>> {
        let a: Vec<JointCommand> = h.take("action").unwrap();
        a.into_iter().map(|a| a.position).collect()
    };
    let close = |a: &[f64], base: f64, k: f64| {
        a.iter()
            .zip(1..=6)
            .all(|(x, j)| (x - (base + j as f64 + k)).abs() < 1e-5)
    };

    h.push("image", img).unwrap();
    h.push("state", state(0.0)).unwrap();
    h.step().unwrap();
    let a = actions(&mut h);
    assert!(a.len() == 1 && close(&a[0], 0.0, 0.2), "{a:?}");

    // The second action of the chunk (+0.4) is never sent.
    h.set_estop(true);
    for _ in 0..3 {
        h.push("state", state(10.0)).unwrap();
        h.step().unwrap();
    }
    assert!(actions(&mut h).is_empty(), "no actions while stopped");

    h.set_estop(false);
    h.step().unwrap();
    let a = actions(&mut h);
    assert!(
        a.len() == 1 && close(&a[0], 10.0, 0.2),
        "re-planned from the latest state: {a:?}"
    );
}
