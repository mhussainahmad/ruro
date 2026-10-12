//! Running a single node from Python tests.

use pyo3::exceptions::{PyKeyError, PyRuntimeError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use ruro_node::testing::Harness;

use crate::convert::{decode, encode, py_to_value};

fn runtime(e: anyhow::Error) -> PyErr {
    PyRuntimeError::new_err(format!("{e:#}"))
}

/// One node instance with direct access to its ports, for tests.
///
/// Messages are pushed and taken through the encoded path that cross-process
/// wires use, so the node sees exactly what it would see in a graph.
#[pyclass(name = "Harness", module = "ruro._native", unsendable)]
pub struct PyHarness {
    inner: Harness,
}

#[pymethods]
impl PyHarness {
    /// Builds a node of a registered kind with optional parameter overrides.
    #[new]
    #[pyo3(signature = (kind, params = None))]
    fn new(kind: &str, params: Option<&Bound<'_, PyDict>>) -> PyResult<Self> {
        let params = match params {
            Some(p) => py_to_value(p.as_any())?
                .deserialize_into::<serde_json::Value>()
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?,
            None => serde_json::Value::Null,
        };
        Ok(Self {
            inner: Harness::new(kind, params).map_err(runtime)?,
        })
    }

    /// Delivers a message to an input.
    fn push(&mut self, input: &str, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        let ty = self
            .inner
            .port_type(input)
            .ok_or_else(|| PyKeyError::new_err(format!("no port `{input}`")))?;
        let bytes = encode(ty, msg)?;
        self.inner.push_encoded(input, &bytes).map_err(runtime)
    }

    /// Runs one step (and `start` before the first).
    fn step(&mut self) -> PyResult<()> {
        self.inner.step().map_err(runtime)
    }

    /// Removes and returns everything an output has sent so far.
    fn take<'py>(&mut self, py: Python<'py>, output: &str) -> PyResult<Bound<'py, PyList>> {
        let ty = self
            .inner
            .port_type(output)
            .ok_or_else(|| PyKeyError::new_err(format!("no port `{output}`")))?;
        let list = PyList::empty(py);
        for bytes in self.inner.take_encoded(output).map_err(runtime)? {
            list.append(decode(py, ty, &bytes)?)?;
        }
        Ok(list)
    }

    /// Changes a parameter as the control plane would while a graph runs.
    fn set_param(&mut self, name: &str, value: &Bound<'_, PyAny>) -> PyResult<()> {
        let v = py_to_value(value)?
            .deserialize_into::<serde_json::Value>()
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        self.inner.set_param(name, v).map_err(runtime)
    }
}
