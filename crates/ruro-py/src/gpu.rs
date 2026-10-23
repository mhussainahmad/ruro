//! GPU tensors (`gpu/Tensor`) in Python.
//!
//! A received tensor in device memory exposes `__cuda_array_interface__`
//! (version 3), so `torch.as_tensor(t, device="cuda")`, `cupy.asarray(t)` or
//! `numba.cuda.as_cuda_array(t)` see the same device memory, without a copy.
//! Receivers must not write it: other receivers share it. (The interface
//! still says it is writable, because PyTorch refuses read-only arrays.) A
//! node sends a tensor by
//! passing a `GpuTensor`, any object with `__cuda_array_interface__` (a
//! PyTorch CUDA tensor, a CuPy array), which is copied into a ruro device
//! buffer, or a NumPy array, which makes a host tensor.

use std::sync::Arc;

use pyo3::IntoPyObjectExt;
use pyo3::exceptions::{PyAttributeError, PyRuntimeError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyTuple};
use ruro_gpu::{DType, Device, Stream, Tensor};
use ruro_node::Sample;

use crate::convert::PyMessage;

fn gpu_err(e: anyhow::Error) -> PyErr {
    PyRuntimeError::new_err(format!("{e:#}"))
}

enum Held {
    Received(Arc<Sample<Tensor>>),
    Owned(Tensor),
}

/// An n-dimensional array in GPU (or host) memory: a `gpu/Tensor` message.
#[pyclass(name = "GpuTensor", module = "ruro._native", frozen)]
pub struct PyGpuTensor {
    held: Held,
}

impl PyGpuTensor {
    fn tensor(&self) -> &Tensor {
        match &self.held {
            Held::Received(s) => &s.msg,
            Held::Owned(t) => t,
        }
    }
}

/// `__cuda_array_interface__` type strings.
fn typestr(d: DType) -> &'static str {
    match d {
        DType::U8 => "|u1",
        DType::I32 => "<i4",
        DType::I64 => "<i8",
        DType::F16 => "<f2",
        DType::F32 => "<f4",
        DType::F64 => "<f8",
    }
}

fn dtype_of_typestr(s: &str) -> PyResult<DType> {
    // `=` and `<` are both little-endian on every CUDA platform.
    Ok(match s.replace('=', "<").as_str() {
        "|u1" | "<u1" => DType::U8,
        "<i4" => DType::I32,
        "<i8" => DType::I64,
        "<f2" => DType::F16,
        "<f4" => DType::F32,
        "<f8" => DType::F64,
        other => {
            return Err(PyTypeError::new_err(format!(
                "element type {other} is not one of u8, i32, i64, f16, f32, f64"
            )));
        }
    })
}

/// Row-major byte strides of `shape`.
fn contiguous_byte_strides(shape: &[u64], item: usize) -> Vec<i64> {
    ruro_gpu::contiguous_strides(shape)
        .into_iter()
        .map(|s| s * item as i64)
        .collect()
}

#[pymethods]
impl PyGpuTensor {
    /// Message type name.
    #[classattr]
    const TYPE: &'static str = "gpu/Tensor";

    /// Element type: `u8`, `i32`, `i64`, `f16`, `f32` or `f64`.
    #[getter]
    fn dtype(&self) -> &'static str {
        self.tensor().dtype.name()
    }

    #[getter]
    fn shape<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(py, &self.tensor().shape)
    }

    /// Strides in elements.
    #[getter]
    fn strides<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(py, &self.tensor().strides)
    }

    /// `cuda:<n>` or `cpu`.
    #[getter]
    fn device(&self) -> String {
        self.tensor().device().to_string()
    }

    /// The CUDA array interface (version 3) of a tensor in device memory.
    #[getter]
    fn __cuda_array_interface__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let t = self.tensor();
        let Device::Cuda(_) = t.device() else {
            return Err(PyAttributeError::new_err(
                "a tensor in host memory has no __cuda_array_interface__; use numpy()",
            ));
        };
        let buf = t
            .buffer()
            .ok_or_else(|| PyRuntimeError::new_err("device tensor without a buffer"))?;
        let d = PyDict::new(py);
        d.set_item("shape", PyTuple::new(py, &t.shape)?)?;
        d.set_item("typestr", typestr(t.dtype))?;
        // Shared with every receiver of the message: not to be written. It
        // is not marked read-only because PyTorch refuses such arrays
        // ("the read only flag is not supported").
        d.set_item("data", (buf.device_ptr(), false))?;
        let item = t.dtype.size();
        let bytes: Vec<i64> = t.strides.iter().map(|s| s * item as i64).collect();
        if t.is_contiguous() {
            d.set_item("strides", py.None())?;
        } else {
            d.set_item("strides", PyTuple::new(py, bytes)?)?;
        }
        d.set_item("version", 3)?;
        // The producer finished writing before sending (ruro's rule): there
        // is nothing to wait for.
        d.set_item("stream", py.None())?;
        Ok(d)
    }

    /// A NumPy copy of the tensor, from wherever it lives.
    fn numpy<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let host = self.tensor().to_host().map_err(gpu_err)?;
        let bytes = host
            .host_bytes()
            .ok_or_else(|| PyRuntimeError::new_err("tensor not in host memory"))?;
        let np = py.import("numpy")?;
        let dtype = np.getattr("dtype")?.call1((typestr(host.dtype),))?;
        let flat = np.call_method1("frombuffer", (PyBytes::new(py, bytes), dtype))?;
        let shape = PyTuple::new(py, &host.shape)?;
        flat.call_method1("reshape", (shape,))?.call_method0("copy")
    }

    /// A tensor from a NumPy array (copied), on CUDA device `device`, or in
    /// host memory with `device=None`.
    #[staticmethod]
    #[pyo3(signature = (array, device = None))]
    fn from_numpy(array: &Bound<'_, PyAny>, device: Option<u32>) -> PyResult<Self> {
        let host = tensor_from_numpy(array)?;
        let t = match device {
            Some(d) => host.to_device(d).map_err(gpu_err)?,
            None => host,
        };
        Ok(Self {
            held: Held::Owned(t),
        })
    }

    fn __repr__(&self) -> String {
        let t = self.tensor();
        format!(
            "GpuTensor({} {:?} on {})",
            t.dtype.name(),
            t.shape,
            t.device()
        )
    }
}

fn tensor_from_numpy(array: &Bound<'_, PyAny>) -> PyResult<Tensor> {
    let py = array.py();
    let np = py.import("numpy")?;
    let a = np.call_method1("ascontiguousarray", (array,))?;
    let ts: String = a.getattr("dtype")?.getattr("str")?.extract()?;
    let dtype = dtype_of_typestr(&ts)?;
    let shape: Vec<u64> = a.getattr("shape")?.extract()?;
    let bytes: Vec<u8> = a.call_method0("tobytes")?.extract()?;
    Tensor::from_host(dtype, shape, bytes).map_err(gpu_err)
}

/// Copies an object with `__cuda_array_interface__` into a ruro tensor on
/// the same device.
fn tensor_from_cuda_array(obj: &Bound<'_, PyAny>) -> PyResult<Tensor> {
    let cai = obj.getattr("__cuda_array_interface__")?;
    let shape: Vec<u64> = cai.get_item("shape")?.extract()?;
    let ts: String = cai.get_item("typestr")?.extract()?;
    let dtype = dtype_of_typestr(&ts)?;
    let (ptr, _readonly): (u64, bool) = cai.get_item("data")?.extract()?;
    let strides = cai.get_item("strides").ok().filter(|s| !s.is_none());
    if let Some(strides) = strides {
        let strides: Vec<i64> = strides.extract()?;
        if strides != contiguous_byte_strides(&shape, dtype.size()) {
            return Err(PyValueError::new_err(
                "the array is not contiguous; make it so first (e.g. tensor.contiguous())",
            ));
        }
    }
    let cuda = ruro_gpu::cuda().map_err(gpu_err)?;
    let numel: u64 = shape.iter().product();
    let len = numel as usize * dtype.size();
    if len == 0 {
        return Tensor::from_host(dtype, shape, Vec::new()).map_err(gpu_err);
    }
    let device = cuda.device_of_pointer(ptr).map_err(gpu_err)?;
    // Wait for the producer: on the stream the interface names, as it asks
    // of consumers; on the whole device when it names none (PyTorch leaves
    // it out, and its streams do not order against the one the copy uses).
    match cai.get_item("stream").ok().filter(|s| !s.is_none()) {
        Some(stream) => {
            let handle: usize = stream.extract()?;
            // SAFETY: the interface promises a live stream of the array's
            // device, or 1 or 2 (the default streams).
            unsafe { cuda.synchronize_raw_stream(device, handle) }.map_err(gpu_err)?;
        }
        None => cuda.synchronize_device(device).map_err(gpu_err)?,
    }
    let mut t = Tensor::on_device(device, dtype, shape).map_err(gpu_err)?;
    let buf = t
        .buffer_mut()
        .ok_or_else(|| PyRuntimeError::new_err("new device tensor without a buffer"))?;
    // SAFETY: the interface describes `len` readable bytes at `ptr` on
    // `device`, and its producer is done writing them (synchronized above).
    unsafe { buf.copy_from_raw(ptr, len, &Stream::default_for(device)) }.map_err(gpu_err)?;
    Ok(t)
}

impl PyMessage for Tensor {
    fn to_py(py: Python<'_>, sample: Arc<Sample<Self>>) -> PyResult<Py<PyAny>> {
        PyGpuTensor {
            held: Held::Received(sample),
        }
        .into_py_any(py)
    }

    fn from_py(obj: &Bound<'_, PyAny>) -> PyResult<Self> {
        if let Ok(t) = obj.cast::<PyGpuTensor>() {
            return Ok(t.get().tensor().clone());
        }
        if obj.hasattr("__cuda_array_interface__")? {
            return tensor_from_cuda_array(obj);
        }
        if obj.hasattr("__array_interface__")? {
            return tensor_from_numpy(obj);
        }
        Err(PyTypeError::new_err(
            "expected a GpuTensor, an object with __cuda_array_interface__ (a PyTorch CUDA tensor, a CuPy array) or a NumPy array",
        ))
    }
}

/// Why CUDA cannot be used here, or `None`.
#[pyfunction]
pub fn cuda_unavailable() -> Option<String> {
    ruro_gpu::unavailable()
}
