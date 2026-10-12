//! Images and point clouds, shared with Python without copying.
//!
//! A received image stays in the reference-counted sample it arrived in; the
//! Python object exposes its pixels through the buffer protocol, so
//! `numpy.asarray(img)` (or `img.array()`) is a read-only view of the same
//! memory other nodes see. The view is read-only because that memory is
//! shared with every other subscriber of the wire.

use std::ffi::{c_int, c_void};
use std::sync::Arc;

use pyo3::buffer::PyBuffer;
use pyo3::exceptions::{PyBufferError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use pyo3::{IntoPyObjectExt, ffi};
use ruro_msg::{Encoding, Image, PointCloud};
use ruro_node::Sample;

use crate::convert::PyMessage;

/// Data that is either shared with a received sample or owned.
enum Shared<T> {
    Received(Arc<Sample<T>>),
    Owned(T),
}

impl<T> Shared<T> {
    fn get(&self) -> &T {
        match self {
            Shared::Received(s) => &s.msg,
            Shared::Owned(m) => m,
        }
    }
}

fn encoding_name(e: Encoding) -> &'static str {
    match e {
        Encoding::Rgb8 => "rgb8",
        Encoding::Rgba8 => "rgba8",
        Encoding::Mono8 => "mono8",
    }
}

fn parse_encoding(s: &str) -> PyResult<Encoding> {
    match s.to_ascii_lowercase().as_str() {
        "rgb8" | "rgb" => Ok(Encoding::Rgb8),
        "rgba8" | "rgba" => Ok(Encoding::Rgba8),
        "mono8" | "mono" | "gray" | "l" => Ok(Encoding::Mono8),
        other => Err(PyValueError::new_err(format!(
            "unknown encoding `{other}` (expected rgb8, rgba8 or mono8)"
        ))),
    }
}

/// Fills a read-only, one-dimensional byte buffer view over `data`.
///
/// # Safety
/// `view` must be the pointer passed to `__getbuffer__`, and `data` must stay
/// valid and unchanged for as long as `owner` is alive.
unsafe fn fill_view(
    owner: &Bound<'_, PyAny>,
    data: &[u8],
    view: *mut ffi::Py_buffer,
    flags: c_int,
) -> PyResult<()> {
    if flags & ffi::PyBUF_WRITABLE == ffi::PyBUF_WRITABLE {
        return Err(PyBufferError::new_err(
            "message data is shared with other nodes and is read-only; copy it to modify",
        ));
    }
    let rc = unsafe {
        ffi::PyBuffer_FillInfo(
            view,
            owner.as_ptr(),
            data.as_ptr() as *mut c_void,
            data.len() as ffi::Py_ssize_t,
            1,
            flags,
        )
    };
    if rc == -1 {
        return Err(PyErr::fetch(owner.py()));
    }
    Ok(())
}

/// A dense, row-major 8-bit image (`std/Image`).
///
/// Supports the buffer protocol: `numpy.asarray(img)` gives the raw bytes
/// without copying, and `img.array()` a `(height, width, channels)` view.
#[pyclass(name = "Image", module = "ruro._native", frozen)]
pub struct PyImage {
    data: Shared<Image>,
}

impl PyImage {
    /// The wrapped image.
    pub fn image(&self) -> &Image {
        self.data.get()
    }
}

#[pymethods]
impl PyImage {
    /// Message type name.
    #[classattr]
    const TYPE: &'static str = "std/Image";

    /// A new image. Without `data` the pixels are zero; with it, `data` is any
    /// buffer (bytes, a numpy array) of exactly `width * height * channels`
    /// bytes, which is copied.
    #[new]
    #[pyo3(signature = (width, height, encoding = "rgb8", data = None))]
    fn new(
        width: u32,
        height: u32,
        encoding: &str,
        data: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Self> {
        let mut img = Image::new(width, height, parse_encoding(encoding)?);
        if let Some(data) = data {
            let bytes = buffer_bytes(data)?;
            if bytes.len() != img.data.len() {
                return Err(PyValueError::new_err(format!(
                    "{width}x{height} {encoding} needs {} bytes, got {}",
                    img.data.len(),
                    bytes.len()
                )));
            }
            img.data = bytes;
        }
        Ok(Self {
            data: Shared::Owned(img),
        })
    }

    /// An image from a C-contiguous `uint8` array of shape `(h, w)` (mono8),
    /// `(h, w, 3)` (rgb8) or `(h, w, 4)` (rgba8). The pixels are copied once.
    #[staticmethod]
    #[pyo3(signature = (array, encoding = None))]
    fn from_array(array: &Bound<'_, PyAny>, encoding: Option<&str>) -> PyResult<Self> {
        let buf: PyBuffer<u8> = PyBuffer::get(array).map_err(|_| {
            PyTypeError::new_err("expected a uint8 array (numpy.ndarray with dtype=uint8)")
        })?;
        if !buf.is_c_contiguous() {
            return Err(PyValueError::new_err(
                "array must be C-contiguous; use numpy.ascontiguousarray",
            ));
        }
        let shape = buf.shape();
        let (h, w, c) = match shape {
            [h, w] => (*h, *w, 1),
            [h, w, c] => (*h, *w, *c),
            _ => {
                return Err(PyValueError::new_err(format!(
                    "expected a 2D or 3D array, got shape {shape:?}"
                )));
            }
        };
        let enc = match (encoding, c) {
            (Some(e), _) => parse_encoding(e)?,
            (None, 1) => Encoding::Mono8,
            (None, 3) => Encoding::Rgb8,
            (None, 4) => Encoding::Rgba8,
            (None, c) => {
                return Err(PyValueError::new_err(format!(
                    "cannot infer an encoding for {c} channels"
                )));
            }
        };
        if enc.channels() != c {
            return Err(PyValueError::new_err(format!(
                "{} has {} channels, array has {c}",
                encoding_name(enc),
                enc.channels()
            )));
        }
        let data = buf.to_vec(array.py())?;
        Ok(Self {
            data: Shared::Owned(Image {
                width: w as u32,
                height: h as u32,
                encoding: enc,
                data,
            }),
        })
    }

    #[getter]
    fn width(&self) -> u32 {
        self.image().width
    }

    #[getter]
    fn height(&self) -> u32 {
        self.image().height
    }

    /// `rgb8`, `rgba8` or `mono8`.
    #[getter]
    fn encoding(&self) -> &'static str {
        encoding_name(self.image().encoding)
    }

    #[getter]
    fn channels(&self) -> usize {
        self.image().encoding.channels()
    }

    /// `(height, width, channels)`.
    #[getter]
    fn shape(&self) -> (u32, u32, usize) {
        let i = self.image();
        (i.height, i.width, i.encoding.channels())
    }

    /// Whether the pixels are shared with a received message (not copied).
    #[getter]
    fn shared(&self) -> bool {
        matches!(self.data, Shared::Received(_))
    }

    /// A read-only numpy view `(height, width, channels)` of the pixels.
    fn array<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyAny>> {
        let (h, w, c) = slf.get().shape();
        let np = slf.py().import("numpy")?;
        np.call_method1("frombuffer", (slf, "uint8"))?
            .call_method1("reshape", (h, w, c))
    }

    /// A copy of the pixels as bytes.
    fn tobytes<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.image().data)
    }

    fn __len__(&self) -> usize {
        self.image().data.len()
    }

    fn __repr__(&self) -> String {
        let i = self.image();
        format!(
            "Image({}x{}, {})",
            i.width,
            i.height,
            encoding_name(i.encoding)
        )
    }

    /// # Safety
    /// Called by the interpreter with a valid view pointer.
    unsafe fn __getbuffer__(
        slf: Bound<'_, Self>,
        view: *mut ffi::Py_buffer,
        flags: c_int,
    ) -> PyResult<()> {
        let data = slf.get().image().data.as_slice();
        // The pixels live as long as `slf`, which the view holds a reference to.
        unsafe { fill_view(slf.as_any(), data, view, flags) }
    }
}

/// Copies any byte buffer.
fn buffer_bytes(obj: &Bound<'_, PyAny>) -> PyResult<Vec<u8>> {
    if let Ok(b) = obj.cast::<PyBytes>() {
        return Ok(b.as_bytes().to_vec());
    }
    let buf: PyBuffer<u8> =
        PyBuffer::get(obj).map_err(|_| PyTypeError::new_err("expected bytes or a uint8 buffer"))?;
    buf.to_vec(obj.py())
}

impl PyMessage for Image {
    fn to_py(py: Python<'_>, sample: Arc<Sample<Self>>) -> PyResult<Py<PyAny>> {
        PyImage {
            data: Shared::Received(sample),
        }
        .into_py_any(py)
    }

    fn from_py(obj: &Bound<'_, PyAny>) -> PyResult<Self> {
        if let Ok(img) = obj.cast::<PyImage>() {
            return Ok(img.get().image().clone());
        }
        Ok(PyImage::from_array(obj, None)?.image().clone())
    }
}

/// An unordered set of 3D points (`std/PointCloud`).
///
/// Supports the buffer protocol over the packed `float32` x, y, z values:
/// `pc.array()` is a read-only `(n, 3)` numpy view.
#[pyclass(name = "PointCloud", module = "ruro._native", frozen)]
pub struct PyPointCloud {
    data: Shared<PointCloud>,
}

impl PyPointCloud {
    fn cloud(&self) -> &PointCloud {
        self.data.get()
    }

    fn bytes(&self) -> &[u8] {
        let points = &self.cloud().points;
        // [f32; 3] is 12 bytes with no padding.
        unsafe { std::slice::from_raw_parts(points.as_ptr().cast::<u8>(), points.len() * 12) }
    }
}

#[pymethods]
impl PyPointCloud {
    /// Message type name.
    #[classattr]
    const TYPE: &'static str = "std/PointCloud";

    /// A point cloud from an `(n, 3)` float array or a sequence of `(x, y, z)`.
    #[new]
    #[pyo3(signature = (points = None))]
    fn new(points: Option<&Bound<'_, PyAny>>) -> PyResult<Self> {
        let points = match points {
            None => Vec::new(),
            Some(p) => points_from(p)?,
        };
        Ok(Self {
            data: Shared::Owned(PointCloud { points }),
        })
    }

    /// A point cloud from a C-contiguous `float32` array of shape `(n, 3)`.
    #[staticmethod]
    fn from_array(array: &Bound<'_, PyAny>) -> PyResult<Self> {
        Self::new(Some(array))
    }

    /// A read-only numpy view `(n, 3)` of the points.
    fn array<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyAny>> {
        let np = slf.py().import("numpy")?;
        np.call_method1("frombuffer", (slf, "float32"))?
            .call_method1("reshape", (-1, 3))
    }

    /// Whether the points are shared with a received message (not copied).
    #[getter]
    fn shared(&self) -> bool {
        matches!(self.data, Shared::Received(_))
    }

    /// The points as a list of `(x, y, z)` tuples.
    fn tolist(&self) -> Vec<(f32, f32, f32)> {
        self.cloud()
            .points
            .iter()
            .map(|p| (p[0], p[1], p[2]))
            .collect()
    }

    fn __len__(&self) -> usize {
        self.cloud().points.len()
    }

    fn __repr__(&self) -> String {
        format!("PointCloud({} points)", self.cloud().points.len())
    }

    /// # Safety
    /// Called by the interpreter with a valid view pointer.
    unsafe fn __getbuffer__(
        slf: Bound<'_, Self>,
        view: *mut ffi::Py_buffer,
        flags: c_int,
    ) -> PyResult<()> {
        let data = slf.get().bytes();
        unsafe { fill_view(slf.as_any(), data, view, flags) }
    }
}

fn points_from(obj: &Bound<'_, PyAny>) -> PyResult<Vec<[f32; 3]>> {
    if let Ok(pc) = obj.cast::<PyPointCloud>() {
        return Ok(pc.get().cloud().points.clone());
    }
    if let Ok(buf) = PyBuffer::<f32>::get(obj) {
        if !buf.is_c_contiguous() {
            return Err(PyValueError::new_err(
                "array must be C-contiguous; use numpy.ascontiguousarray",
            ));
        }
        let ok = match buf.shape() {
            [_, 3] => true,
            [n] => n % 3 == 0,
            _ => false,
        };
        if !ok {
            return Err(PyValueError::new_err(format!(
                "expected shape (n, 3), got {:?}",
                buf.shape()
            )));
        }
        let flat = buf.to_vec(obj.py())?;
        return Ok(flat.as_chunks::<3>().0.to_vec());
    }
    let mut points = Vec::new();
    for item in obj.try_iter()? {
        let (x, y, z): (f32, f32, f32) = item?.extract()?;
        points.push([x, y, z]);
    }
    Ok(points)
}

impl PyMessage for PointCloud {
    fn to_py(py: Python<'_>, sample: Arc<Sample<Self>>) -> PyResult<Py<PyAny>> {
        PyPointCloud {
            data: Shared::Received(sample),
        }
        .into_py_any(py)
    }

    fn from_py(obj: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(PointCloud {
            points: points_from(obj)?,
        })
    }
}
