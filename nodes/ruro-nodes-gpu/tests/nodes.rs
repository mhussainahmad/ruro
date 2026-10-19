//! The GPU nodes in running graphs. Tests that need a GPU are skipped (with
//! the reason printed) when the NVIDIA driver or a CUDA device is missing.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use ruro::prelude::*;
use ruro_graph::Graph;
use ruro_host::{Executor, Remote};
use ruro_proto::{Keys, NodeState};

fn gpu() -> bool {
    match ruro_gpu::unavailable() {
        Some(why) => {
            eprintln!("skipping: {why}");
            false
        }
        None => true,
    }
}

/// Byte `i` of frame `n`. The first four bytes hold `n` itself.
fn frame_byte(n: u32, i: usize) -> u8 {
    (i as u32).wrapping_mul(31).wrapping_add(n.wrapping_mul(7)) as u8
}

/// Emits numbered RGB frames.
#[derive(Node)]
#[node(kind = "test/frames", rate = 100)]
struct Frames {
    #[output]
    image: Output<Image>,
    #[param(default = 320, min = 4, max = 4096)]
    width: u32,
    #[param(default = 240, min = 1, max = 4096)]
    height: u32,
    n: u32,
}

impl Node for Frames {
    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        self.n += 1;
        let mut img = self.image.loan();
        img.reshape(self.width, self.height, Encoding::Rgb8);
        for (i, b) in img.data.iter_mut().enumerate() {
            *b = frame_byte(self.n, i);
        }
        img.data[..4].copy_from_slice(&self.n.to_le_bytes());
        img.send()
    }
}

/// Frame numbers received intact, or what was wrong.
static FRAMES: Mutex<Vec<std::result::Result<u32, String>>> = Mutex::new(Vec::new());
static TENSORS: Mutex<Vec<(Vec<u32>, f32, f32)>> = Mutex::new(Vec::new());

/// Checks that frames from `test/frames` arrived unchanged.
#[derive(Node)]
#[node(kind = "test/check-frames")]
struct CheckFrames {
    #[input]
    image: Input<Image>,
}

impl Node for CheckFrames {
    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        while let Some(img) = self.image.recv() {
            let n = u32::from_le_bytes(img.data[..4].try_into().unwrap());
            let bad = img
                .data
                .iter()
                .enumerate()
                .skip(4)
                .find(|&(i, &b)| b != frame_byte(n, i));
            FRAMES.lock().push(match bad {
                None => Ok(n),
                Some((i, b)) => Err(format!("frame {n}: byte {i} is {b}")),
            });
        }
        Ok(())
    }
}

/// Records shape and value range of received tensors.
#[derive(Node)]
#[node(kind = "test/tensors")]
struct Tensors {
    #[input]
    tensor: Input<ruro_nodes_ml::Tensor>,
}

impl Node for Tensors {
    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        while let Some(t) = self.tensor.recv() {
            let (min, max) = t
                .data
                .iter()
                .fold((f32::MAX, f32::MIN), |(a, b), &v| (a.min(v), b.max(v)));
            TENSORS.lock().push((t.shape.clone(), min, max));
        }
        Ok(())
    }
}

fn wait_until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

const GRAPH: &str = r#"
name: gpu-test
nodes:
  frames: { type: test/frames, group: a }
  upload: { type: gpu/upload, group: a }
  download: { type: gpu/download, group: b }
  check: { type: test/check-frames, group: b }
  normalize: { type: gpu/normalize, group: b, params: { width: 64, height: 48, mean: "0.5,0.5,0.5", std: "0.5,0.5,0.5" } }
  to_cpu: { type: gpu/tensor-to-cpu, group: b }
  tensors: { type: test/tensors, group: b }
wires:
  - frames.image -> upload.image
  - upload.out -> download.image
  - download.out -> check.image
  - upload.out -> normalize.image
  - normalize.tensor -> to_cpu.tensor
  - to_cpu.out -> tensors.tensor
"#;

#[test]
fn nodes_are_registered_with_typed_ports() {
    ruro_nodes_gpu::link();
    let registry = ruro::node::registry();
    let port = |kind: &str, input: bool| {
        let d = registry
            .get(kind)
            .unwrap_or_else(|| panic!("{kind} missing"));
        let ports = if input { &d.inputs } else { &d.outputs };
        ports[0].ty.clone()
    };
    assert_eq!(port("gpu/upload", true), "std/Image");
    assert_eq!(port("gpu/upload", false), "gpu/Image");
    assert_eq!(port("gpu/download", false), "std/Image");
    assert_eq!(port("gpu/normalize", false), "gpu/Tensor");
    assert_eq!(port("gpu/tensor-to-cpu", false), "ml/Tensor");
    let issues = ruro_graph::validate(&Graph::from_yaml(GRAPH).unwrap(), &registry);
    assert!(issues.is_empty(), "{issues:?}");
}

#[test]
fn the_example_graph_validates() {
    ruro_host::describe(); // links every node crate of the stock host
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/gpu.yaml");
    let graph = Graph::load(path).unwrap();
    let issues = ruro_graph::validate(&graph, &ruro::node::registry());
    assert!(issues.is_empty(), "{issues:?}");
}

const NO_CUDA_ENV: &str = "RURO_GPU_TEST_NO_CUDA";

/// Run in a child process with a missing driver library.
#[test]
fn nodes_fail_to_start_without_cuda_child() {
    if std::env::var(NO_CUDA_ENV).is_err() {
        return;
    }
    let graph = Graph::from_yaml(
        "name: no-cuda\nnodes:\n  frames: { type: test/frames }\n  upload: { type: gpu/upload }\nwires: [frames.image -> upload.image]\n",
    )
    .unwrap();
    let exec = Executor::start(&graph, "main", Remote::Disabled).unwrap();
    let failed = wait_until(Duration::from_secs(5), || {
        exec.metrics()
            .nodes
            .iter()
            .any(|n| n.id == "upload" && n.state == NodeState::Failed)
    });
    let m = exec.metrics();
    let upload = m.nodes.iter().find(|n| n.id == "upload").unwrap();
    assert!(failed, "upload is {:?}", upload.state);
    let err = upload.last_error.clone().unwrap_or_default();
    assert!(err.contains("CUDA unavailable"), "{err}");
    exec.stop();
}

#[test]
fn nodes_fail_to_start_without_cuda() {
    // In a child process, because the driver is loaded once per process.
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "nodes_fail_to_start_without_cuda_child"])
        .env(NO_CUDA_ENV, "1")
        .env(ruro_gpu::LIB_ENV, "/nonexistent/libcuda.so.1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn pipelines() {
    if !gpu() {
        return;
    }
    in_process_pipeline_and_preview();
    cross_process_pipeline();
}

fn check_results(what: &str) {
    let frames = FRAMES.lock();
    assert!(
        frames.iter().all(|r| r.is_ok()),
        "{what}: {:?}",
        frames.iter().find(|r| r.is_err())
    );
    let tensors = TENSORS.lock();
    let (shape, min, max) = tensors.last().unwrap();
    assert_eq!(shape, &vec![1, 3, 48, 64], "{what}");
    // Pixels span 0..255, normalized with mean and std 0.5: -1..1.
    assert!(
        *min >= -1.0 - 1e-5 && *max <= 1.0 + 1e-5 && max > min,
        "{what}: {min}..{max}"
    );
}

fn in_process_pipeline_and_preview() {
    FRAMES.lock().clear();
    TENSORS.lock().clear();
    let graph = Graph::from_yaml(
        &GRAPH
            .replace("group: a", "group: main")
            .replace("group: b", "group: main"),
    )
    .unwrap();
    let exec = Executor::start(&graph, "main", Remote::Disabled).unwrap();
    let previews = Arc::new(AtomicU64::new(0));
    let p = previews.clone();
    let upload = exec
        .outputs()
        .iter()
        .find(|(port, _)| port.node == "upload")
        .unwrap()
        .1
        .clone();
    upload.tap().set_sink(Arc::new(move |preview| {
        assert!(
            matches!(
                preview,
                ruro_msg::Preview::Image {
                    width: 320,
                    height: 240,
                    ..
                }
            ),
            "{preview:?}"
        );
        p.fetch_add(1, Ordering::Relaxed);
    }));
    upload.tap().set_active(true);
    assert!(
        wait_until(Duration::from_secs(10), || FRAMES.lock().len() > 20
            && !TENSORS.lock().is_empty()),
        "frames {}, tensors {}: {:?}",
        FRAMES.lock().len(),
        TENSORS.lock().len(),
        exec.metrics()
            .nodes
            .iter()
            .map(|n| (n.id.clone(), n.state, n.last_error.clone()))
            .collect::<Vec<_>>()
    );
    assert!(
        previews.load(Ordering::Relaxed) > 0,
        "a watched GPU image previews"
    );
    check_results("in process");
    exec.stop();
}

const CHILD_ENV: &str = "RURO_GPU_TEST_GROUP_A";

/// Run in a child process: group `a` of the graph (frames and upload).
#[test]
fn group_a_child() {
    let Ok(keys) = std::env::var(CHILD_ENV) else {
        return;
    };
    let graph = Graph::from_yaml(GRAPH).unwrap();
    let exec = Executor::start(&graph, "a", Remote::Shm(Keys::new(&keys))).unwrap();
    // Until the parent kills us (or goes away).
    std::thread::sleep(Duration::from_secs(60));
    exec.stop();
}

struct Child(std::process::Child);

impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn cross_process_pipeline() {
    FRAMES.lock().clear();
    TENSORS.lock().clear();
    let keys = format!("gputest{}", std::process::id());
    let graph = Graph::from_yaml(GRAPH).unwrap();
    let b = Executor::start(&graph, "b", Remote::Shm(Keys::new(&keys))).unwrap();
    let _child = Child(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "group_a_child"])
            .env(CHILD_ENV, &keys)
            .spawn()
            .unwrap(),
    );
    assert!(
        wait_until(Duration::from_secs(20), || FRAMES.lock().len() > 50
            && TENSORS.lock().len() > 10),
        "frames {}, tensors {}: {:?}",
        FRAMES.lock().len(),
        TENSORS.lock().len(),
        b.metrics()
            .nodes
            .iter()
            .map(|n| (n.id.clone(), n.state, n.last_error.clone()))
            .collect::<Vec<_>>()
    );
    check_results("across processes");
    // Frames arrive intact and in order. A keep-latest wire may drop some
    // when the receiving process falls behind (on a loaded machine), so
    // only a weak floor is checked.
    let frames: Vec<u32> = FRAMES.lock().iter().map(|r| *r.as_ref().unwrap()).collect();
    assert!(frames.windows(2).all(|w| w[1] > w[0]), "{frames:?}");
    let span = (frames.last().unwrap() - frames[0] + 1) as usize;
    eprintln!("received {} of {span} frames", frames.len());
    assert!(
        frames.len() * 2 >= span,
        "{} of {span} frames",
        frames.len()
    );
    b.stop();
}
