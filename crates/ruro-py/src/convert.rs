//! Moving messages between Rust and Python.
//!
//! Small messages cross as their "raw" serde form: nested dicts, lists and
//! scalars, which the generated dataclasses in `ruro.msg` build themselves
//! from (`_from_raw`) and turn back into (`_to_raw`). Images and point clouds
//! are native classes that share the received buffer instead.

use std::collections::BTreeMap;
use std::sync::Arc;

use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBool, PyBytes, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};
use ruro_msg::Message;
use ruro_node::Sample;
use serde_value::Value;

/// A message type that can be handed to and taken from Python.
pub trait PyMessage: Message {
    /// Converts a received sample. May keep `sample` alive to share its data.
    fn to_py(py: Python<'_>, sample: Arc<Sample<Self>>) -> PyResult<Py<PyAny>>;
    /// Converts a value from Python, accepting the message class or its raw form.
    fn from_py(obj: &Bound<'_, PyAny>) -> PyResult<Self>;
}

static MESSAGES: PyOnceLock<Py<PyDict>> = PyOnceLock::new();

/// `ruro.msg.MESSAGES`: message type name to generated class.
fn message_classes(py: Python<'_>) -> PyResult<&Bound<'_, PyDict>> {
    MESSAGES
        .get_or_try_init(py, || -> PyResult<Py<PyDict>> {
            let m = py.import("ruro.msg")?;
            Ok(m.getattr("MESSAGES")?.cast_into::<PyDict>()?.unbind())
        })
        .map(|d| d.bind(py))
}

/// The generated Python class of a message type.
pub fn message_class<'py>(py: Python<'py>, ty: &str) -> PyResult<Bound<'py, PyAny>> {
    message_classes(py)?
        .get_item(ty)?
        .ok_or_else(|| PyTypeError::new_err(format!("no Python class for message type {ty}")))
}

/// serde value to Python objects.
pub fn value_to_py(py: Python<'_>, v: &Value) -> PyResult<Py<PyAny>> {
    Ok(match v {
        Value::Bool(b) => PyBool::new(py, *b).to_owned().into_any().unbind(),
        Value::U8(n) => n.into_pyobject(py)?.into_any().unbind(),
        Value::U16(n) => n.into_pyobject(py)?.into_any().unbind(),
        Value::U32(n) => n.into_pyobject(py)?.into_any().unbind(),
        Value::U64(n) => n.into_pyobject(py)?.into_any().unbind(),
        Value::I8(n) => n.into_pyobject(py)?.into_any().unbind(),
        Value::I16(n) => n.into_pyobject(py)?.into_any().unbind(),
        Value::I32(n) => n.into_pyobject(py)?.into_any().unbind(),
        Value::I64(n) => n.into_pyobject(py)?.into_any().unbind(),
        Value::F32(f) => PyFloat::new(py, f64::from(*f)).into_any().unbind(),
        Value::F64(f) => PyFloat::new(py, *f).into_any().unbind(),
        Value::Char(c) => PyString::new(py, &c.to_string()).into_any().unbind(),
        Value::String(s) => PyString::new(py, s).into_any().unbind(),
        Value::Unit | Value::Option(None) => py.None(),
        Value::Option(Some(inner)) | Value::Newtype(inner) => value_to_py(py, inner)?,
        Value::Seq(items) => {
            let list = PyList::empty(py);
            for i in items {
                list.append(value_to_py(py, i)?)?;
            }
            list.into_any().unbind()
        }
        Value::Map(map) => {
            let dict = PyDict::new(py);
            for (k, v) in map {
                dict.set_item(value_to_py(py, k)?, value_to_py(py, v)?)?;
            }
            dict.into_any().unbind()
        }
        Value::Bytes(b) => PyBytes::new(py, b).into_any().unbind(),
    })
}

/// Python objects to a serde value. Bytes become a sequence of `u8`, which
/// is how `Vec<u8>` fields are encoded.
pub fn py_to_value(obj: &Bound<'_, PyAny>) -> PyResult<Value> {
    if obj.is_none() {
        return Ok(Value::Option(None));
    }
    if let Ok(b) = obj.cast::<PyBool>() {
        return Ok(Value::Bool(b.is_true()));
    }
    if obj.is_instance_of::<PyInt>() {
        if let Ok(n) = obj.extract::<i64>() {
            return Ok(Value::I64(n));
        }
        return Ok(Value::U64(obj.extract::<u64>()?));
    }
    if obj.is_instance_of::<PyFloat>() {
        return Ok(Value::F64(obj.extract::<f64>()?));
    }
    if let Ok(s) = obj.cast::<PyString>() {
        return Ok(Value::String(s.to_str()?.to_owned()));
    }
    if let Ok(b) = obj.cast::<PyBytes>() {
        return Ok(Value::Seq(
            b.as_bytes().iter().map(|x| Value::U8(*x)).collect(),
        ));
    }
    if let Ok(d) = obj.cast::<PyDict>() {
        let mut map = BTreeMap::new();
        for (k, v) in d.iter() {
            map.insert(py_to_value(&k)?, py_to_value(&v)?);
        }
        return Ok(Value::Map(map));
    }
    if obj.is_instance_of::<PyList>() || obj.is_instance_of::<PyTuple>() {
        let items: PyResult<Vec<Value>> = obj.try_iter()?.map(|i| py_to_value(&i?)).collect();
        return Ok(Value::Seq(items?));
    }
    // numpy scalars and other number-likes.
    if let Ok(f) = obj.extract::<f64>() {
        return Ok(Value::F64(f));
    }
    // Any other iterable, e.g. a numpy array.
    if let Ok(iter) = obj.try_iter() {
        let items: PyResult<Vec<Value>> = iter.map(|i| py_to_value(&i?)).collect();
        return Ok(Value::Seq(items?));
    }
    Err(PyTypeError::new_err(format!(
        "cannot convert {} to a message field",
        obj.get_type().name()?
    )))
}

/// The raw form of `obj` for message type `ty`: calls `_to_raw` on message
/// classes (checking their type) and takes anything else as already raw.
fn raw_of(obj: &Bound<'_, PyAny>, ty: &str) -> PyResult<Value> {
    if obj.hasattr("_to_raw")? {
        if let Ok(t) = obj.getattr("TYPE")
            && let Ok(t) = t.extract::<String>()
            && t != ty
        {
            return Err(PyTypeError::new_err(format!("expected {ty}, got {t}")));
        }
        return py_to_value(&obj.call_method0("_to_raw")?);
    }
    py_to_value(obj)
}

/// Implements [`PyMessage`] through the raw serde form.
macro_rules! raw_messages {
    ($($t:ident),* $(,)?) => {$(
        impl PyMessage for ruro_msg::$t {
            fn to_py(py: Python<'_>, sample: Arc<Sample<Self>>) -> PyResult<Py<PyAny>> {
                let raw = serde_value::to_value(&sample.msg)
                    .map_err(|e| PyValueError::new_err(e.to_string()))?;
                let class = message_class(py, <Self as Message>::TYPE)?;
                Ok(class.call_method1("_from_raw", (value_to_py(py, &raw)?,))?.unbind())
            }

            fn from_py(obj: &Bound<'_, PyAny>) -> PyResult<Self> {
                let raw = raw_of(obj, <Self as Message>::TYPE)?;
                serde::Deserialize::deserialize(raw).map_err(|e: serde_value::DeserializerError| {
                    PyValueError::new_err(format!(
                        "not a valid {}: {e}",
                        <Self as Message>::TYPE
                    ))
                })
            }
        }
    )*};
}

raw_messages!(
    Scalar,
    Text,
    Flag,
    Bytes,
    Vec3,
    Pose,
    Twist,
    JointState,
    JointCommand,
    Transform,
    Transforms,
    Imu,
    LaserScan,
    Odometry,
    CameraInfo,
    Joy,
);

/// Calls `$mac!` with every message type usable from Python.
macro_rules! with_message_types {
    ($mac:ident) => {
        ruro_msg::for_each_std_message! { $mac }
    };
}
pub(crate) use with_message_types;

macro_rules! type_list {
    ($($t:ident),* $(,)?) => {
        /// Type names of every message usable from Python.
        pub fn supported_types() -> Vec<&'static str> {
            vec![$(<ruro_msg::$t as Message>::TYPE),*]
        }

        /// Encodes a Python value as message type `ty`.
        pub fn encode(ty: &str, obj: &Bound<'_, PyAny>) -> PyResult<Vec<u8>> {
            match ty {
                $(<ruro_msg::$t as Message>::TYPE => {
                    Ok(<ruro_msg::$t as PyMessage>::from_py(obj)?.to_bytes())
                })*
                other => Err(PyTypeError::new_err(format!("unsupported message type {other}"))),
            }
        }

        /// Decodes bytes of message type `ty` into a Python value.
        pub fn decode(py: Python<'_>, ty: &str, bytes: &[u8]) -> PyResult<Py<PyAny>> {
            match ty {
                $(<ruro_msg::$t as Message>::TYPE => {
                    let msg = <ruro_msg::$t as Message>::decode(bytes)
                        .map_err(|e| PyValueError::new_err(e.to_string()))?;
                    <ruro_msg::$t as PyMessage>::to_py(
                        py,
                        Arc::new(Sample { seq: 0, stamp_ns: 0, msg }),
                    )
                })*
                other => Err(PyTypeError::new_err(format!("unsupported message type {other}"))),
            }
        }
    };
}

with_message_types!(type_list);
