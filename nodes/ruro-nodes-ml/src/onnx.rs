//! ONNX Runtime inference (feature `onnx`): the `ml/onnx` and `ml/policy` nodes.

use std::collections::VecDeque;

use ort::session::Session;
use ort::value::Tensor as OrtTensor;
use ruro::prelude::*;

use crate::preprocess::{Preprocess, Resize};
use crate::{Detections, Tensor, load_labels, parse_floats, yolo};

/// Shape and name of a model input or output; dynamic dimensions are -1.
#[derive(Debug, Clone, PartialEq)]
pub struct Port {
    pub name: String,
    pub shape: Vec<i64>,
}

/// A loaded ONNX model running on the CPU.
pub struct Model {
    session: Session,
    pub inputs: Vec<Port>,
    pub outputs: Vec<Port>,
}

fn ort_err(e: impl std::fmt::Display) -> ruro::anyhow::Error {
    anyhow!("ONNX Runtime: {e}")
}

impl Model {
    pub fn load(path: &str, threads: usize) -> Result<Model> {
        Self::load_on(path, threads, None)
    }

    /// Loads a model to run on the CUDA device `cuda` (feature
    /// `onnx-cuda`), or on the CPU.
    pub fn load_on(path: &str, threads: usize, cuda: Option<u32>) -> Result<Model> {
        Self::load_profiled(path, threads, cuda, None)
    }

    /// As [`Model::load_on`]; with `profile`, ONNX Runtime writes a JSON
    /// profile of every run (each operator's duration and the execution
    /// provider it ran on) to a file starting with that path, written when
    /// [`Model::end_profiling`] is called or the model is dropped.
    pub fn load_profiled(
        path: &str,
        threads: usize,
        cuda: Option<u32>,
        profile: Option<&str>,
    ) -> Result<Model> {
        let mut builder = Session::builder().map_err(ort_err)?;
        if threads > 0 {
            builder = builder.with_intra_threads(threads).map_err(ort_err)?;
        }
        if let Some(prefix) = profile {
            builder = builder.with_profiling(prefix).map_err(ort_err)?;
        }
        if let Some(device) = cuda {
            #[cfg(feature = "onnx-cuda")]
            {
                let ep = ort::ep::CUDA::default()
                    .with_device_id(device as i32)
                    .build()
                    .error_on_failure();
                builder = builder.with_execution_providers([ep]).map_err(|e| {
                    anyhow!(
                        "ONNX Runtime's CUDA provider: {e} (it needs the CUDA 13 runtime, cuBLAS, cuRAND and, for convolutions, cuDNN 9 on the library path)"
                    )
                })?;
            }
            #[cfg(not(feature = "onnx-cuda"))]
            bail!("cuda:{device} needs a host built with the onnx-cuda feature");
        }
        let session = builder
            .commit_from_file(path)
            .map_err(|e| anyhow!("loading {path}: {e}"))?;
        let port = |o: &ort::value::Outlet| Port {
            name: o.name().to_owned(),
            shape: o
                .dtype()
                .tensor_shape()
                .map(|s| s.to_vec())
                .unwrap_or_default(),
        };
        let inputs = session.inputs().iter().map(port).collect();
        let outputs = session.outputs().iter().map(port).collect();
        Ok(Model {
            session,
            inputs,
            outputs,
        })
    }

    pub fn input(&self, name: &str) -> Option<&Port> {
        self.inputs.iter().find(|p| p.name == name)
    }

    /// Writes the profile started with [`Model::load_profiled`]; returns its
    /// file name.
    pub fn end_profiling(&mut self) -> Result<String> {
        self.session.end_profiling().map_err(ort_err)
    }

    /// Runs the model with one float32 input given as a `gpu/Tensor`: one in
    /// device memory is read there, without a copy to the host. Returns
    /// every output, in host memory.
    #[cfg(feature = "onnx-cuda")]
    pub fn run_gpu(&mut self, input: &str, t: &ruro_gpu::Tensor) -> Result<Vec<(String, Tensor)>> {
        use ort::memory::{AllocationDevice, AllocatorType, MemoryInfo, MemoryType};
        use ruro_gpu::{DType, Device};
        ensure!(
            t.dtype == DType::F32,
            "the input tensor is {:?}, not f32",
            t.dtype
        );
        ensure!(t.is_contiguous(), "the input tensor is not contiguous");
        let shape: Vec<i64> = t.shape.iter().map(|&d| d as i64).collect();
        let outputs = match t.device() {
            Device::Cuda(n) => {
                let buf = t.buffer().context("device tensor without a buffer")?;
                let info = MemoryInfo::new(
                    AllocationDevice::CUDA,
                    n as i32,
                    AllocatorType::Device,
                    MemoryType::Default,
                )
                .map_err(ort_err)?;
                // SAFETY: the pointer is the device memory of `t`, which
                // outlives the run, and holds `shape` f32 values on device n.
                let value = unsafe {
                    ort::value::TensorRefMut::<f32>::from_raw(
                        info,
                        buf.device_ptr() as *mut std::ffi::c_void,
                        shape.into(),
                    )
                }
                .map_err(ort_err)?;
                self.session
                    .run(ort::inputs![input => value])
                    .map_err(ort_err)?
            }
            Device::Cpu => {
                let data = t.to_f32_vec()?;
                let value = OrtTensor::from_array((shape, data)).map_err(ort_err)?;
                self.session
                    .run(ort::inputs![input => value])
                    .map_err(ort_err)?
            }
        };
        let mut result = Vec::new();
        for (name, value) in &outputs {
            let (shape, data) = value.try_extract_tensor::<f32>().map_err(ort_err)?;
            result.push((
                name.to_owned(),
                Tensor::new(shape.iter().map(|&d| d as u32).collect(), data.to_vec()),
            ));
        }
        Ok(result)
    }

    /// Runs the model on float32 inputs and returns every output as a tensor.
    pub fn run(
        &mut self,
        inputs: Vec<(String, Vec<i64>, Vec<f32>)>,
    ) -> Result<Vec<(String, Tensor)>> {
        let mut values = Vec::with_capacity(inputs.len());
        for (name, shape, data) in inputs {
            let t = OrtTensor::from_array((shape, data)).map_err(ort_err)?;
            values.push((
                std::borrow::Cow::from(name),
                ort::session::SessionInputValue::from(t),
            ));
        }
        let outputs = self.session.run(values).map_err(ort_err)?;
        let mut result = Vec::new();
        for (name, value) in &outputs {
            let (shape, data) = value.try_extract_tensor::<f32>().map_err(ort_err)?;
            result.push((
                name.to_owned(),
                Tensor::new(shape.iter().map(|&d| d as u32).collect(), data.to_vec()),
            ));
        }
        Ok(result)
    }
}

/// Fixed spatial size of an NCHW image input, if the model declares one.
fn model_hw(port: &Port) -> Option<(usize, usize)> {
    match port.shape.as_slice() {
        [_, _, h, w] if *h > 0 && *w > 0 => Some((*h as usize, *w as usize)),
        _ => None,
    }
}

fn triple(s: &str, what: &str) -> Result<[f32; 3]> {
    let v = parse_floats(s)?;
    v.try_into()
        .map_err(|_| anyhow!("{what} needs three comma separated values"))
}

/// Runs an ONNX image model (classification, detection, segmentation...) on
/// ONNX Runtime. Each image is resized, normalised and fed to the model's
/// first input as NCHW float32; the first output is published as a tensor and,
/// with `postprocess: yolo`, decoded into detections in image pixels.
#[derive(Node)]
#[node(kind = "ml/onnx")]
pub struct Onnx {
    #[input]
    image: Input<Image>,
    /// The model's first output.
    #[output]
    tensor: Output<Tensor>,
    /// Decoded boxes (only with `postprocess: yolo`).
    #[output]
    detections: Output<Detections>,
    /// `.onnx` model file.
    #[param(default = "")]
    model: String,
    /// Input width; 0 takes it from the model.
    #[param(default = 0, min = 0, max = 8192)]
    width: u32,
    /// Input height; 0 takes it from the model.
    #[param(default = 0, min = 0, max = 8192)]
    height: u32,
    /// `letterbox` (keep aspect ratio) or `stretch`.
    #[param(default = "letterbox")]
    resize: String,
    /// Per-channel mean, applied after scaling pixels to 0..1.
    #[param(default = "0,0,0")]
    mean: String,
    /// Per-channel standard deviation, applied after the mean.
    #[param(default = "1,1,1")]
    std: String,
    /// Feed channels in BGR order instead of RGB.
    #[param(default = false)]
    bgr: bool,
    /// `none` or `yolo`.
    #[param(default = "none")]
    postprocess: String,
    /// YOLO output layout: `auto`, `v5` or `v8`.
    #[param(default = "auto")]
    yolo_layout: String,
    /// Minimum detection confidence.
    #[param(default = 0.25, min = 0, max = 1)]
    confidence: f32,
    /// NMS IoU threshold.
    #[param(default = 0.45, min = 0, max = 1)]
    iou: f32,
    #[param(default = 100, min = 1, max = 10000)]
    max_detections: u32,
    /// Class names: comma separated or a file with one per line.
    #[param(default = "")]
    labels: String,
    /// ONNX Runtime intra-op threads (0 = library default).
    #[param(default = 0, min = 0, max = 256)]
    threads: u32,
    net: Option<Model>,
    names: Vec<String>,
}

impl Onnx {
    fn preprocess(&self) -> Result<Preprocess> {
        let net = self.net.as_ref().context("model not loaded")?;
        let declared = model_hw(&net.inputs[0]);
        let (h, w) = match (self.height, self.width, declared) {
            (0, 0, Some(hw)) => hw,
            (0, 0, None) => bail!("model input size is dynamic; set width and height"),
            (h, w, _) => (h as usize, w as usize),
        };
        Ok(Preprocess {
            width: w,
            height: h,
            resize: Resize::parse(&self.resize)?,
            mean: triple(&self.mean, "mean")?,
            std: triple(&self.std, "std")?,
            bgr: self.bgr,
            ..Default::default()
        })
    }
}

impl Node for Onnx {
    fn start(&mut self, _ctx: &mut Ctx) -> Result {
        ensure!(
            !self.model.is_empty(),
            "set the `model` parameter to an .onnx file"
        );
        let net = Model::load(&self.model, self.threads as usize)?;
        ensure!(
            !net.inputs.is_empty() && !net.outputs.is_empty(),
            "model has no inputs or outputs"
        );
        info!(
            "{}: inputs {:?}, outputs {:?}",
            self.model, net.inputs, net.outputs
        );
        self.net = Some(net);
        self.names = load_labels(&self.labels)?;
        self.preprocess()?;
        ensure!(
            matches!(self.postprocess.as_str(), "none" | "yolo"),
            "postprocess must be none or yolo"
        );
        Ok(())
    }

    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        let Some(img) = self.image.latest() else {
            return Ok(());
        };
        let pre = self.preprocess()?;
        let (data, map) = pre.run(&img)?;
        let net = self.net.as_mut().context("model not loaded")?;
        let input = net.inputs[0].name.clone();
        let shape = vec![1, 3, pre.height as i64, pre.width as i64];
        let mut outputs = net.run(vec![(input, shape, data)])?;
        let (_, out) = outputs.swap_remove(0);
        if self.postprocess == "yolo" {
            let cfg = yolo::Config {
                layout: yolo::Layout::parse(&self.yolo_layout)?,
                confidence: self.confidence,
                iou: self.iou,
                max_detections: self.max_detections as usize,
            };
            let mut boxes = yolo::decode(&out.shape_usize(), &out.data, &cfg)?;
            yolo::to_source(&mut boxes, &map, img.width, img.height);
            yolo::label(&mut boxes, &self.names);
            self.detections.send(Detections {
                header: Header::new("", img.stamp_ns),
                boxes,
            })?;
        }
        self.tensor.send(out)
    }
}

/// Runs a robot policy exported to ONNX (for example a LeRobot ACT or
/// diffusion policy): joint state and, optionally, a camera image in; joint
/// command out. Models that output an action chunk (`[1, K, D]`) have their
/// actions executed one per step before the policy runs again.
///
/// E-stop: no actions are sent while stopped (the drivers hold the robot) and
/// the rest of the current action chunk is dropped. Observations keep being
/// read, so after the release the policy runs again on the latest joint state
/// and image instead of resuming a chunk planned before the stop.
#[derive(Node)]
#[node(kind = "ml/policy", rate = 30)]
pub struct Policy {
    /// Robot joint positions, fed to `state_input`.
    #[input]
    state: Input<JointState>,
    /// Camera image, fed to `image_input` (when set).
    #[input]
    image: Input<Image>,
    /// Joint position targets.
    #[output]
    action: Output<JointCommand>,
    /// `.onnx` model file.
    #[param(default = "")]
    model: String,
    /// Name of the state input.
    #[param(default = "observation.state")]
    state_input: String,
    /// Name of the image input; empty for state-only policies.
    #[param(default = "observation.image")]
    image_input: String,
    /// Name of the action output; empty takes the first output.
    #[param(default = "")]
    action_output: String,
    /// Image width when the model input is dynamic.
    #[param(default = 0, min = 0, max = 4096)]
    width: u32,
    /// Image height when the model input is dynamic.
    #[param(default = 0, min = 0, max = 4096)]
    height: u32,
    /// Actions to execute from each chunk before re-running (0 = whole chunk).
    #[param(default = 0, min = 0, max = 1000)]
    actions_per_inference: u32,
    /// Per-joint state normalisation `(x - mean) / std`; empty to skip.
    #[param(default = "")]
    state_mean: String,
    #[param(default = "")]
    state_std: String,
    /// Per-joint action de-normalisation `a * std + mean`; empty to skip.
    #[param(default = "")]
    action_mean: String,
    #[param(default = "")]
    action_std: String,
    #[param(default = 0, min = 0, max = 256)]
    threads: u32,
    net: Option<Model>,
    queue: VecDeque<Vec<f64>>,
    last_state: Option<Vec<f64>>,
    last_image: Option<std::sync::Arc<ruro::Sample<Image>>>,
    /// Stopped by the e-stop.
    held: bool,
}

fn affine(values: &mut [f32], mean: &str, std: &str, inverse: bool) -> Result<()> {
    let (m, s) = (parse_floats(mean)?, parse_floats(std)?);
    if m.is_empty() && s.is_empty() {
        return Ok(());
    }
    ensure!(
        m.len() == values.len() && s.len() == values.len(),
        "normalisation needs {} means and stds",
        values.len()
    );
    for ((v, m), s) in values.iter_mut().zip(m).zip(s) {
        *v = if inverse { *v * s + m } else { (*v - m) / s };
    }
    Ok(())
}

impl Policy {
    fn infer(&mut self) -> Result<bool> {
        let Some(state) = self.last_state.clone() else {
            return Ok(false);
        };
        let use_image = !self.image_input.is_empty();
        if use_image && self.last_image.is_none() {
            return Ok(false);
        }
        let net = self.net.as_mut().context("model not loaded")?;
        let mut s: Vec<f32> = state.iter().map(|&v| v as f32).collect();
        affine(&mut s, &self.state_mean, &self.state_std, false)?;
        let mut inputs = vec![(self.state_input.clone(), vec![1, s.len() as i64], s)];
        if use_image {
            let port = net
                .input(&self.image_input)
                .with_context(|| format!("model has no input `{}`", self.image_input))?;
            let (h, w) = match (self.height, self.width, model_hw(port)) {
                (0, 0, Some(hw)) => hw,
                (0, 0, None) => bail!("image input size is dynamic; set width and height"),
                (h, w, _) => (h as usize, w as usize),
            };
            let pre = Preprocess {
                width: w,
                height: h,
                resize: Resize::Stretch,
                ..Default::default()
            };
            let (data, _) = pre.run(self.last_image.as_ref().unwrap())?;
            inputs.push((
                self.image_input.clone(),
                vec![1, 3, h as i64, w as i64],
                data,
            ));
        }
        let outputs = net.run(inputs)?;
        let out = if self.action_output.is_empty() {
            &outputs[0].1
        } else {
            &outputs
                .iter()
                .find(|(n, _)| *n == self.action_output)
                .with_context(|| format!("model has no output `{}`", self.action_output))?
                .1
        };
        let dim = *out.shape.last().context("scalar action output")? as usize;
        ensure!(dim > 0, "empty action");
        let take = match self.actions_per_inference as usize {
            0 => usize::MAX,
            n => n,
        };
        for chunk in out.data.chunks_exact(dim).take(take) {
            let mut a = chunk.to_vec();
            affine(&mut a, &self.action_mean, &self.action_std, true)?;
            self.queue.push_back(a.into_iter().map(f64::from).collect());
        }
        Ok(true)
    }
}

impl Node for Policy {
    fn start(&mut self, _ctx: &mut Ctx) -> Result {
        ensure!(
            !self.model.is_empty(),
            "set the `model` parameter to an .onnx file"
        );
        let net = Model::load(&self.model, self.threads as usize)?;
        ensure!(
            net.input(&self.state_input).is_some(),
            "model has no input `{}` (inputs: {:?})",
            self.state_input,
            net.inputs
        );
        info!(
            "{}: inputs {:?}, outputs {:?}",
            self.model, net.inputs, net.outputs
        );
        self.net = Some(net);
        Ok(())
    }

    fn step(&mut self, ctx: &mut Ctx) -> Result {
        if let Some(s) = self.state.latest() {
            self.last_state = Some(s.position.clone());
        }
        if let Some(i) = self.image.latest() {
            self.last_image = Some(i);
        }
        if ctx.estopped() || self.held {
            // Actions planned before the stop are stale.
            self.queue.clear();
            self.held = ctx.estopped();
            if self.held {
                return Ok(());
            }
        }
        if self.queue.is_empty() && !self.infer()? {
            return Ok(());
        }
        if let Some(a) = self.queue.pop_front() {
            self.action.send(JointCommand {
                header: Header::new("", ruro::msg::now_ns()),
                position: a,
                ..Default::default()
            })?;
        }
        Ok(())
    }

    fn on_estop(&mut self, _ctx: &mut Ctx) {
        self.held = true;
        self.queue.clear();
    }
}

/// Runs an ONNX model on an NVIDIA GPU with ONNX Runtime's CUDA provider
/// (feature `onnx-cuda`). The input is a float32 `gpu/Tensor`, for example
/// from `gpu/normalize`: one in device memory is fed to the model where it
/// is, without passing through the host. Publishes one output as an
/// `ml/Tensor` in host memory, for `ml/yolo-decode` and other CPU nodes.
#[cfg(feature = "onnx-cuda")]
#[derive(Node)]
#[node(kind = "ml/onnx-cuda")]
pub struct OnnxCuda {
    #[input]
    tensor: Input<ruro_gpu::Tensor>,
    /// The chosen model output, in host memory.
    #[output]
    output: Output<Tensor>,
    /// `.onnx` model file.
    #[param(default = "")]
    model: String,
    /// CUDA device the model runs on.
    #[param(default = 0, min = 0, max = 64)]
    device: u32,
    /// Model input fed with the tensor; empty takes the first.
    #[param(default = "")]
    input_name: String,
    /// Model output published; empty takes the first.
    #[param(default = "")]
    output_name: String,
    /// Path prefix of an ONNX Runtime profile (JSON) to write when the node
    /// stops; empty for none.
    #[param(default = "")]
    profile: String,
    net: Option<Model>,
}

#[cfg(feature = "onnx-cuda")]
impl Node for OnnxCuda {
    fn start(&mut self, _ctx: &mut Ctx) -> Result {
        ensure!(
            !self.model.is_empty(),
            "set the `model` parameter to an .onnx file"
        );
        let profile = Some(self.profile.as_str()).filter(|p| !p.is_empty());
        let net = Model::load_profiled(&self.model, 0, Some(self.device), profile)?;
        if !self.input_name.is_empty() {
            ensure!(
                net.input(&self.input_name).is_some(),
                "model has no input `{}` (inputs: {:?})",
                self.input_name,
                net.inputs
            );
        }
        info!(
            "{} on cuda:{}: inputs {:?}, outputs {:?}",
            self.model, self.device, net.inputs, net.outputs
        );
        self.net = Some(net);
        Ok(())
    }

    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        let Some(t) = self.tensor.latest() else {
            return Ok(());
        };
        if let ruro_gpu::Device::Cuda(n) = t.device() {
            ensure!(
                n == self.device,
                "the tensor is on cuda:{n}, the model on cuda:{}",
                self.device
            );
        }
        let net = self.net.as_mut().context("model not loaded")?;
        let input = if self.input_name.is_empty() {
            net.inputs[0].name.clone()
        } else {
            self.input_name.clone()
        };
        let outputs = net.run_gpu(&input, &t)?;
        let out = if self.output_name.is_empty() {
            outputs.into_iter().next()
        } else {
            outputs.into_iter().find(|(n, _)| *n == self.output_name)
        };
        let (_, out) =
            out.with_context(|| format!("model has no output `{}`", self.output_name))?;
        self.output.send(out)
    }

    fn stop(&mut self, _ctx: &mut Ctx) {
        if !self.profile.is_empty()
            && let Some(net) = self.net.as_mut()
        {
            match net.end_profiling() {
                Ok(file) => info!("ONNX Runtime profile: {file}"),
                Err(e) => warn!("{e:#}"),
            }
        }
    }
}
