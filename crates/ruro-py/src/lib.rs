//! Python node API for ruro.
//!
//! This crate is two things:
//!
//! - the native part (`ruro._native`) of the `ruro` Python package, built as
//!   an extension module by maturin (see `pyproject.toml`), which lets
//!   Python code define nodes and test them in-process;
//! - a library for host binaries that embed Python ([`embed::init`]), such
//!   as `ruro-host-py`, so graphs can mix Rust and Python nodes in one
//!   process with in-process wires between them.
//!
//! Python nodes use the standard message types and `gpu/Tensor`. Small
//! messages are converted to and from the dataclasses in `ruro.msg`; images
//! and point clouds are shared with Python through the buffer protocol, and
//! GPU tensors through `__cuda_array_interface__`, without copying.

mod convert;
pub mod embed;
mod gpu;
mod harness;
mod media;
mod node;
mod ports;

/// The message types Python nodes use, under one path for the macros.
pub(crate) mod msgs {
    pub use ruro_gpu::Tensor as GpuTensor;
    pub use ruro_msg::*;
}

use pyo3::prelude::*;

pub use convert::supported_types;

/// The `ruro._native` module.
#[pymodule]
#[pyo3(name = "_native")]
pub fn native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<media::PyImage>()?;
    m.add_class::<media::PyPointCloud>()?;
    m.add_class::<gpu::PyGpuTensor>()?;
    m.add_function(wrap_pyfunction!(gpu::cuda_unavailable, m)?)?;
    m.add_class::<ports::PyInput>()?;
    m.add_class::<ports::PyOutput>()?;
    m.add_class::<node::PyCtx>()?;
    m.add_class::<harness::PyHarness>()?;
    m.add_function(wrap_pyfunction!(node::register, m)?)?;
    m.add_function(wrap_pyfunction!(node::describe, m)?)?;
    m.add_function(wrap_pyfunction!(registry_json, m)?)?;
    m.add("SUPPORTED_TYPES", convert::supported_types())?;
    m.add("EMBEDDED", false)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}

/// Every node type known to this process (Rust and Python) as JSON.
#[pyfunction]
fn registry_json() -> String {
    serde_json::to_string(&ruro_node::registry()).expect("registry is serializable")
}
