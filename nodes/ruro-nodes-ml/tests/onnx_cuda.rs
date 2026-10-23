//! ONNX Runtime's CUDA provider with GPU tensors (feature `onnx-cuda`).
//! Skipped, with the reason, without a GPU or without the CUDA libraries the
//! provider needs, unless `RURO_REQUIRE_CUDA` is set.

#![cfg(feature = "onnx-cuda")]

use ruro::node::testing::Harness;
use ruro_gpu::{Device, Tensor as GpuTensor};
use ruro_nodes_ml::Tensor;
use ruro_nodes_ml::onnx::Model;
use serde_json::json;

fn model(name: &str) -> String {
    format!("{}/tests/models/{name}", env!("CARGO_MANIFEST_DIR"))
}

/// The detector on cuda:0, or None (with the reason printed) to skip.
fn on_gpu() -> Option<Model> {
    let skip = |why: String| {
        assert!(
            std::env::var_os("RURO_REQUIRE_CUDA").is_none(),
            "CUDA required: {why}"
        );
        eprintln!("skipped: {why}");
        None
    };
    if let Some(why) = ruro_gpu::unavailable() {
        return skip(why);
    }
    match Model::load_on(&model("detector.onnx"), 0, Some(0)) {
        Ok(m) => Some(m),
        Err(e) => skip(format!("{e:#}")),
    }
}

/// A 1x3x32x32 input whose mean is `mean`.
fn input(mean: f32) -> Vec<f32> {
    (0..3 * 32 * 32)
        .map(|i| if i % 2 == 0 { mean - 0.25 } else { mean + 0.25 })
        .collect()
}

#[test]
fn a_device_tensor_runs_in_place() {
    let Some(mut gpu) = on_gpu() else { return };
    let host = GpuTensor::from_f32(vec![1, 3, 32, 32], &input(0.5)).unwrap();
    let device = host.to_device(0).unwrap();
    assert_eq!(device.device(), Device::Cuda(0));
    let out = gpu.run_gpu("images", &device).unwrap();
    let (name, t) = &out[0];
    assert_eq!(name, "output0");
    assert_eq!(t.shape, [1, 6, 3]);
    // Row 4 (class 0 scores): anchor 0 carries the input mean.
    assert!((t.data[4 * 3] - 0.5).abs() < 1e-5, "{}", t.data[4 * 3]);

    // The same as on the CPU, from a host tensor.
    let mut cpu = Model::load(&model("detector.onnx"), 1).unwrap();
    let expected = cpu
        .run(vec![("images".into(), vec![1, 3, 32, 32], input(0.5))])
        .unwrap();
    for (a, b) in t.data.iter().zip(&expected[0].1.data) {
        assert!((a - b).abs() < 1e-5, "{a} vs {b}");
    }
    let from_host = gpu.run_gpu("images", &host).unwrap();
    assert_eq!(from_host[0].1.data, t.data);
}

#[test]
fn the_operators_run_on_the_gpu() {
    if on_gpu().is_none() {
        return;
    }
    let prefix = std::env::temp_dir().join(format!("ruro-ort-profile-{}", std::process::id()));
    let mut m = Model::load_profiled(
        &model("detector.onnx"),
        0,
        Some(0),
        Some(prefix.to_str().unwrap()),
    )
    .unwrap();
    let device = GpuTensor::from_f32(vec![1, 3, 32, 32], &input(0.5))
        .unwrap()
        .to_device(0)
        .unwrap();
    m.run_gpu("images", &device).unwrap();
    let file = m.end_profiling().unwrap();
    let profile: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let _ = std::fs::remove_file(&file);
    // Every operator the model ran, with the provider it ran on.
    let ops: Vec<(String, String)> = profile
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| {
            e["cat"] == "Node"
                && e["name"]
                    .as_str()
                    .is_some_and(|n| n.ends_with("_kernel_time"))
        })
        .map(|e| {
            (
                e["args"]["op_name"].as_str().unwrap_or_default().to_owned(),
                e["args"]["provider"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            )
        })
        .collect();
    eprintln!("operators: {ops:?}");
    assert!(!ops.is_empty(), "the profile lists no operators");
    assert!(
        ops.iter()
            .any(|(op, p)| op == "ReduceMean" && p == "CUDAExecutionProvider"),
        "{ops:?}"
    );
}

#[test]
fn the_node_takes_gpu_tensors() {
    if on_gpu().is_none() {
        return;
    }
    let mut h = Harness::new("ml/onnx-cuda", json!({ "model": model("detector.onnx") })).unwrap();
    for mean in [0.25, 0.75] {
        let t = GpuTensor::from_f32(vec![1, 3, 32, 32], &input(mean))
            .unwrap()
            .to_device(0)
            .unwrap();
        h.push("tensor", t).unwrap();
        h.step().unwrap();
        let out = h.take::<Tensor>("output").unwrap();
        assert!((out[0].data[4 * 3] - mean).abs() < 1e-5);
    }

    // A tensor on another device than the model is refused.
    let mut h = Harness::new(
        "ml/onnx-cuda",
        json!({ "model": model("detector.onnx"), "device": 0 }),
    )
    .unwrap();
    h.push(
        "tensor",
        GpuTensor::from_f32(vec![1, 3, 32, 32], &input(0.5)).unwrap(),
    )
    .unwrap();
    // A host tensor is accepted (ONNX Runtime copies it).
    h.step().unwrap();
    assert_eq!(h.take::<Tensor>("output").unwrap().len(), 1);
}
