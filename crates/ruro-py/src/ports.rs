//! Typed ports as seen from Python.

use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::types::PyList;
use ruro_msg::Message;
use ruro_node::{Builder, Input, Output};

use crate::convert::{PyMessage, with_message_types};

macro_rules! port_enums {
    ($($t:ident),* $(,)?) => {
        /// An input of any message type usable from Python.
        pub enum AnyInput {
            $($t(Input<ruro_msg::$t>),)*
        }

        /// An output of any message type usable from Python.
        pub enum AnyOutput {
            $($t(Output<ruro_msg::$t>),)*
        }

        impl AnyInput {
            /// Declares an input of message type `ty` on `b`.
            pub fn declare(b: &mut Builder, ty: &str, name: &str) -> anyhow::Result<Self> {
                Ok(match ty {
                    $(<ruro_msg::$t as Message>::TYPE => Self::$t(b.input(name)?),)*
                    other => anyhow::bail!("Python nodes do not support message type {other}"),
                })
            }

            fn recv(&self, py: Python<'_>, latest: bool) -> PyResult<Option<Py<PyAny>>> {
                match self {
                    $(Self::$t(i) => {
                        let s = if latest { i.latest() } else { i.recv() };
                        s.map(|s| <ruro_msg::$t as PyMessage>::to_py(py, s)).transpose()
                    })*
                }
            }

            fn len(&self) -> usize {
                match self {
                    $(Self::$t(i) => i.len(),)*
                }
            }

            fn ty(&self) -> &'static str {
                match self {
                    $(Self::$t(_) => <ruro_msg::$t as Message>::TYPE,)*
                }
            }
        }

        impl AnyOutput {
            /// Declares an output of message type `ty` on `b`.
            pub fn declare(b: &mut Builder, ty: &str, name: &str) -> anyhow::Result<Self> {
                Ok(match ty {
                    $(<ruro_msg::$t as Message>::TYPE => Self::$t(b.output(name)?),)*
                    other => anyhow::bail!("Python nodes do not support message type {other}"),
                })
            }

            fn send(&self, obj: &Bound<'_, PyAny>) -> PyResult<()> {
                match self {
                    $(Self::$t(o) => {
                        let msg = <ruro_msg::$t as PyMessage>::from_py(obj)?;
                        // Sending may encode into shared memory; release the
                        // interpreter while it does.
                        obj.py()
                            .detach(|| o.send(msg))
                            .map_err(|e| PyTypeError::new_err(format!("{e:#}")))
                    })*
                }
            }

            fn sent(&self) -> u64 {
                match self {
                    $(Self::$t(o) => o.sent(),)*
                }
            }

            fn ty(&self) -> &'static str {
                match self {
                    $(Self::$t(_) => <ruro_msg::$t as Message>::TYPE,)*
                }
            }
        }
    };
}

with_message_types!(port_enums);

/// A node input. `recv()` takes the oldest queued message, `latest()` the
/// newest (dropping older ones); iterating drains the queue.
#[pyclass(name = "InputPort", module = "ruro._native", frozen)]
pub struct PyInput {
    pub(crate) name: String,
    pub(crate) port: AnyInput,
}

#[pymethods]
impl PyInput {
    /// The oldest queued message, or `None`.
    fn recv(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        self.port.recv(py, false)
    }

    /// The newest queued message (older ones are discarded), or `None`.
    fn latest(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        self.port.recv(py, true)
    }

    /// Every queued message, oldest first.
    fn drain<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let list = PyList::empty(py);
        while let Some(m) = self.port.recv(py, false)? {
            list.append(m)?;
        }
        Ok(list)
    }

    /// Port name.
    #[getter]
    fn name(&self) -> &str {
        &self.name
    }

    /// Message type name.
    #[getter]
    fn r#type(&self) -> &'static str {
        self.port.ty()
    }

    fn __len__(&self) -> usize {
        self.port.len()
    }

    fn __bool__(&self) -> bool {
        self.port.len() > 0
    }

    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__(&self, py: Python<'_>) -> PyResult<Option<Py<PyAny>>> {
        self.port.recv(py, false)
    }

    fn __repr__(&self) -> String {
        format!(
            "<input {} ({}), {} queued>",
            self.name,
            self.port.ty(),
            self.port.len()
        )
    }
}

/// A node output. `send(msg)` accepts the message class, its raw form (e.g. a
/// float for `Scalar`) or, for images and point clouds, a numpy array.
#[pyclass(name = "OutputPort", module = "ruro._native", frozen)]
pub struct PyOutput {
    pub(crate) name: String,
    pub(crate) port: AnyOutput,
}

#[pymethods]
impl PyOutput {
    /// Publishes a message to every connected input.
    fn send(&self, msg: &Bound<'_, PyAny>) -> PyResult<()> {
        self.port.send(msg)
    }

    /// Number of messages sent so far.
    #[getter]
    fn sent(&self) -> u64 {
        self.port.sent()
    }

    /// Port name.
    #[getter]
    fn name(&self) -> &str {
        &self.name
    }

    /// Message type name.
    #[getter]
    fn r#type(&self) -> &'static str {
        self.port.ty()
    }

    fn __repr__(&self) -> String {
        format!("<output {} ({})>", self.name, self.port.ty())
    }
}
