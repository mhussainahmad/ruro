//! Python classes as ruro node types.
//!
//! The `@ruro.node` decorator calls [`register`] with the node's descriptor
//! and class. The factory registered with `ruro_node::register_dynamic`
//! declares typed ports from the descriptor, creates an instance of the class
//! and wraps it in a [`PyNode`] that the executor drives like any other node.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::anyhow;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};
use ruro_graph::{NodeDescriptor, ParamDesc, ParamKind, PortDesc};
use ruro_node::{Builder, Ctx, DynNode};
use serde_json::Value as Json;

use crate::convert::{py_to_value, supported_types, value_to_py};
use crate::ports::{AnyInput, AnyOutput, PyInput, PyOutput};

/// Converts a Python exception into an error carrying the traceback.
pub fn py_err(py: Python<'_>, e: PyErr) -> anyhow::Error {
    let tb = e
        .traceback(py)
        .and_then(|tb| tb.format().ok())
        .unwrap_or_default();
    anyhow!("{tb}{e}")
}

fn json_to_py(py: Python<'_>, v: &Json) -> PyResult<Py<PyAny>> {
    let v: serde_value::Value =
        serde_value::to_value(v).map_err(|e| PyValueError::new_err(e.to_string()))?;
    value_to_py(py, &v)
}

fn py_to_json(obj: &Bound<'_, PyAny>) -> PyResult<Json> {
    let v = py_to_value(obj)?;
    v.deserialize_into::<Json>()
        .map_err(|e| PyValueError::new_err(e.to_string()))
}

/// Per-step context handed to `step(self, ctx)`.
#[pyclass(name = "Ctx", module = "ruro._native", frozen)]
pub struct PyCtx {
    id: String,
    dt: f64,
    time: f64,
    steps: u64,
}

impl PyCtx {
    fn from(ctx: &Ctx) -> Self {
        Self {
            id: ctx.id().to_owned(),
            dt: ctx.dt().as_secs_f64(),
            time: ctx.time().as_secs_f64(),
            steps: ctx.steps(),
        }
    }
}

#[pymethods]
impl PyCtx {
    /// Node instance id in the graph.
    #[getter]
    fn id(&self) -> &str {
        &self.id
    }

    /// Seconds since the previous step (0 on the first).
    #[getter]
    fn dt(&self) -> f64 {
        self.dt
    }

    /// Seconds since the node started.
    #[getter]
    fn time(&self) -> f64 {
        self.time
    }

    /// Number of completed steps.
    #[getter]
    fn steps(&self) -> u64 {
        self.steps
    }

    /// Logs at debug level; shows up in `ruro logs` with the node id.
    fn debug(&self, msg: &str) {
        tracing::debug!(node = %self.id, "{msg}");
    }

    /// Logs at info level.
    fn info(&self, msg: &str) {
        tracing::info!(node = %self.id, "{msg}");
    }

    /// Logs at warning level.
    fn warn(&self, msg: &str) {
        tracing::warn!(node = %self.id, "{msg}");
    }

    /// Logs at error level.
    fn error(&self, msg: &str) {
        tracing::error!(node = %self.id, "{msg}");
    }

    fn __repr__(&self) -> String {
        format!(
            "Ctx(id={:?}, steps={}, dt={:.6})",
            self.id, self.steps, self.dt
        )
    }
}

/// What the decorator registered: descriptor, class and options.
struct Registered {
    desc: NodeDescriptor,
    class: Py<PyAny>,
    reload: bool,
}

/// Re-imports a node's module when its source file changes.
struct Reloader {
    module: String,
    class_name: String,
    file: PathBuf,
    mtime: Option<SystemTime>,
    last_check: Instant,
}

impl Reloader {
    const INTERVAL: Duration = Duration::from_millis(500);

    fn new(py: Python<'_>, class: &Bound<'_, PyAny>) -> PyResult<Option<Self>> {
        let module: String = class.getattr("__module__")?.extract()?;
        let class_name: String = class.getattr("__qualname__")?.extract()?;
        let sys_modules = py.import("sys")?.getattr("modules")?;
        let Ok(m) = sys_modules.get_item(&module) else {
            return Ok(None);
        };
        let Ok(file) = m.getattr("__file__").and_then(|f| f.extract::<String>()) else {
            return Ok(None);
        };
        let file = PathBuf::from(file);
        let mtime = std::fs::metadata(&file).and_then(|m| m.modified()).ok();
        Ok(Some(Self {
            module,
            class_name,
            file,
            mtime,
            last_check: Instant::now(),
        }))
    }

    /// The reloaded class if the file changed since the last check.
    fn poll<'py>(&mut self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        if self.last_check.elapsed() < Self::INTERVAL {
            return Ok(None);
        }
        self.last_check = Instant::now();
        let mtime = std::fs::metadata(&self.file)
            .and_then(|m| m.modified())
            .ok();
        if mtime == self.mtime {
            return Ok(None);
        }
        self.mtime = mtime;
        let module = py
            .import("sys")?
            .getattr("modules")?
            .get_item(&self.module)?;
        let module = py.import("importlib")?.call_method1("reload", (module,))?;
        Ok(Some(module.getattr(self.class_name.as_str())?))
    }
}

/// A node implemented by a Python object.
pub struct PyNode {
    obj: Py<PyAny>,
    kind: String,
    params: Py<PyAny>,
    param_kinds: BTreeMap<String, ParamKind>,
    reloader: Option<Reloader>,
}

impl PyNode {
    fn call_hook(&self, py: Python<'_>, name: &str, ctx: &Ctx) -> anyhow::Result<()> {
        let obj = self.obj.bind(py);
        if obj.hasattr(name).map_err(|e| py_err(py, e))? {
            obj.call_method1(name, (PyCtx::from(ctx),))
                .map_err(|e| py_err(py, e))?;
        }
        Ok(())
    }

    /// Swaps in an instance of a reloaded class, keeping ports and parameters.
    fn reload(&mut self, py: Python<'_>, class: Bound<'_, PyAny>, ctx: &Ctx) -> anyhow::Result<()> {
        let old = self.obj.bind(py);
        let fresh = instantiate(
            py,
            &class,
            old.getattr("inputs").map_err(|e| py_err(py, e))?,
            old.getattr("outputs").map_err(|e| py_err(py, e))?,
            self.params.bind(py).clone(),
        )
        .map_err(|e| py_err(py, e))?;
        let _ = self.call_hook(py, "stop", ctx);
        self.obj = fresh.unbind();
        self.call_hook(py, "start", ctx)?;
        tracing::info!("{}: reloaded", self.kind);
        Ok(())
    }
}

impl DynNode for PyNode {
    fn start(&mut self, ctx: &mut Ctx) -> anyhow::Result<()> {
        Python::attach(|py| self.call_hook(py, "start", ctx))
    }

    fn step(&mut self, ctx: &mut Ctx) -> anyhow::Result<()> {
        Python::attach(|py| {
            let reloaded = match self.reloader.as_mut().map(|r| r.poll(py)) {
                Some(Ok(class)) => class,
                Some(Err(e)) => {
                    // Keep running the old code if the new one does not load.
                    tracing::error!("{}: reload failed: {:#}", self.kind, py_err(py, e));
                    None
                }
                None => None,
            };
            if let Some(class) = reloaded
                && let Err(e) = self.reload(py, class, ctx)
            {
                // Keep running the old code if the new one does not load.
                tracing::error!("{}: reload failed: {e:#}", self.kind);
            }
            self.obj
                .bind(py)
                .call_method1("step", (PyCtx::from(ctx),))
                .map(drop)
                .map_err(|e| py_err(py, e))
        })
    }

    fn stop(&mut self, ctx: &mut Ctx) {
        Python::attach(|py| {
            if let Err(e) = self.call_hook(py, "stop", ctx) {
                tracing::warn!("{}: stop failed: {e:#}", self.kind);
            }
        })
    }

    fn set_param(&mut self, name: &str, value: &Json) -> anyhow::Result<()> {
        let kind = self
            .param_kinds
            .get(name)
            .ok_or_else(|| anyhow!("{} has no parameter `{name}`", self.kind))?;
        if !kind.accepts(value) {
            anyhow::bail!("parameter `{name}` expects a {kind:?} value, got {value}");
        }
        Python::attach(|py| {
            let v = json_to_py(py, &coerce(*kind, value)).map_err(|e| py_err(py, e))?;
            self.params
                .bind(py)
                .setattr(name, &v)
                .map_err(|e| py_err(py, e))?;
            let obj = self.obj.bind(py);
            if obj.hasattr("on_param").map_err(|e| py_err(py, e))? {
                obj.call_method1("on_param", (name, v))
                    .map_err(|e| py_err(py, e))?;
            }
            Ok(())
        })
    }
}

/// Floats declared as floats stay floats in Python even when set to `3`.
fn coerce(kind: ParamKind, v: &Json) -> Json {
    match (kind, v.as_f64()) {
        (ParamKind::Float, Some(f)) => Json::from(f),
        _ => v.clone(),
    }
}

/// Creates an instance of `class` without calling `__init__` with
/// arguments: ports and parameters are attached first, then `__init__()` runs.
fn instantiate<'py>(
    py: Python<'py>,
    class: &Bound<'py, PyAny>,
    inputs: Bound<'py, PyAny>,
    outputs: Bound<'py, PyAny>,
    params: Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let object = py.import("builtins")?.getattr("object")?;
    let obj = object.call_method1("__new__", (class,))?;
    obj.setattr("inputs", inputs)?;
    obj.setattr("outputs", outputs)?;
    obj.setattr("params", params)?;
    let init = class.getattr("__init__")?;
    if !init.is(object.getattr("__init__")?) {
        init.call1((&obj,))?;
    }
    Ok(obj)
}

fn build_node(b: &mut Builder, reg: &Registered) -> anyhow::Result<Box<dyn DynNode>> {
    let mut inputs = Vec::new();
    for p in &reg.desc.inputs {
        inputs.push(PyInput {
            name: p.name.clone(),
            port: AnyInput::declare(b, &p.ty, &p.name)?,
        });
    }
    let mut outputs = Vec::new();
    for p in &reg.desc.outputs {
        outputs.push(PyOutput {
            name: p.name.clone(),
            port: AnyOutput::declare(b, &p.ty, &p.name)?,
        });
    }
    let mut values = Vec::new();
    for p in &reg.desc.params {
        let v = b.raw_param(&p.name).cloned().unwrap_or(p.default.clone());
        if !p.kind.accepts(&v) {
            anyhow::bail!(
                "parameter `{}` expects a {:?} value, got {v}",
                p.name,
                p.kind
            );
        }
        values.push((p.name.clone(), coerce(p.kind, &v)));
    }
    let reload = reg.reload || std::env::var_os("RURO_PY_RELOAD").is_some_and(|v| v != "0");
    Python::attach(|py| -> PyResult<Box<dyn DynNode>> {
        let support = py.import("ruro.node")?;
        let in_dict = PyDict::new(py);
        for i in inputs {
            in_dict.set_item(i.name.clone(), Bound::new(py, i)?)?;
        }
        let out_dict = PyDict::new(py);
        for o in outputs {
            out_dict.set_item(o.name.clone(), Bound::new(py, o)?)?;
        }
        let param_dict = PyDict::new(py);
        for (name, v) in &values {
            param_dict.set_item(name, json_to_py(py, v)?)?;
        }
        let params = support.getattr("Params")?.call1((param_dict,))?;
        let class = reg.class.bind(py);
        let obj = instantiate(
            py,
            class,
            support.getattr("Ports")?.call1((in_dict,))?,
            support.getattr("Ports")?.call1((out_dict,))?,
            params.clone(),
        )?;
        let reloader = if reload {
            Reloader::new(py, class)?
        } else {
            None
        };
        Ok(Box::new(PyNode {
            obj: obj.unbind(),
            kind: reg.desc.kind.clone(),
            params: params.unbind(),
            param_kinds: reg
                .desc
                .params
                .iter()
                .map(|p| (p.name.clone(), p.kind))
                .collect(),
            reloader,
        }))
    })
    .map_err(|e| Python::attach(|py| py_err(py, e)))
}

/// Registers a Python class as node type `kind`.
///
/// `inputs` and `outputs` are `(name, message type)` pairs; `params` are
/// `(name, default, min, max, doc)` tuples whose kind follows the default's
/// Python type.
#[pyfunction]
#[pyo3(signature = (kind, cls, inputs, outputs, params, doc = String::new(), rate = None, reload = false))]
#[allow(clippy::too_many_arguments)]
pub fn register(
    py: Python<'_>,
    kind: String,
    cls: Py<PyAny>,
    inputs: Vec<(String, String)>,
    outputs: Vec<(String, String)>,
    params: Vec<Bound<'_, PyTuple>>,
    doc: String,
    rate: Option<f64>,
    reload: bool,
) -> PyResult<()> {
    let supported = supported_types();
    let port = |(name, ty): &(String, String)| -> PyResult<PortDesc> {
        if !supported.contains(&ty.as_str()) {
            return Err(PyValueError::new_err(format!(
                "port `{name}`: Python nodes do not support message type {ty}"
            )));
        }
        Ok(PortDesc {
            name: name.clone(),
            ty: ty.clone(),
        })
    };
    let mut param_descs = Vec::new();
    for p in params {
        let (name, default, min, max, pdoc): (
            String,
            Bound<'_, PyAny>,
            Option<f64>,
            Option<f64>,
            String,
        ) = p.extract()?;
        let kind = if default.is_instance_of::<pyo3::types::PyBool>() {
            ParamKind::Bool
        } else if default.is_instance_of::<pyo3::types::PyInt>() {
            ParamKind::Int
        } else if default.is_instance_of::<pyo3::types::PyFloat>() {
            ParamKind::Float
        } else if default.is_instance_of::<pyo3::types::PyString>() {
            ParamKind::String
        } else {
            return Err(PyValueError::new_err(format!(
                "parameter `{name}`: default must be a bool, int, float or str"
            )));
        };
        param_descs.push(ParamDesc {
            name,
            kind,
            default: py_to_json(&default)?,
            min,
            max,
            doc: pdoc,
        });
    }
    let desc = NodeDescriptor {
        kind: kind.clone(),
        doc,
        default_rate: rate,
        inputs: inputs.iter().map(port).collect::<PyResult<_>>()?,
        outputs: outputs.iter().map(port).collect::<PyResult<_>>()?,
        params: param_descs,
        source: source_location(py, &cls),
    };
    let reg = Arc::new(Registered {
        desc: desc.clone(),
        class: cls.clone_ref(py),
        reload,
    });
    ruro_node::register_dynamic(desc, Arc::new(move |b| build_node(b, &reg)));
    Ok(())
}

/// Descriptor of a registered node type as JSON, for tooling and tests.
#[pyfunction]
pub fn describe(kind: &str) -> PyResult<Option<String>> {
    Ok(ruro_node::descriptor(kind)
        .map(|d| serde_json::to_string(&d).expect("descriptor is serializable")))
}

/// `file:line` of a node class, so tools can open its source.
fn source_location(py: Python<'_>, cls: &Py<PyAny>) -> Option<String> {
    let inspect = py.import("inspect").ok()?;
    let file: String = inspect
        .call_method1("getsourcefile", (cls,))
        .ok()?
        .extract()
        .ok()?;
    let line: usize = inspect
        .call_method1("getsourcelines", (cls,))
        .ok()?
        .get_item(1)
        .ok()?
        .extract()
        .ok()?;
    Some(format!("{file}:{line}"))
}
