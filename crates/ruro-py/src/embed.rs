//! Embedding Python in a host binary.
//!
//! [`init`] starts the interpreter, makes this crate's native module the one
//! `import ruro` uses (so Python nodes register into the host's own node
//! registry, not a second copy loaded from a wheel), and imports the modules
//! that define nodes:
//!
//! - every module named in `RURO_PY_MODULES` (comma-separated module names
//!   or paths to `.py` files), and
//! - every entry point in the `ruro.nodes` group of installed distributions.
//!
//! The active virtual environment (`VIRTUAL_ENV`) is added to the module
//! search path, as are the directories in `RURO_PY_PATH`.

use std::path::Path;

use anyhow::Result;
use pyo3::prelude::*;
use pyo3::types::{IntoPyDict, PyList};

/// Entry point group scanned for node modules.
pub const ENTRY_POINT_GROUP: &str = "ruro.nodes";

/// Where the `ruro` Python package lives in the source tree, used when it is
/// not installed.
const SOURCE_PACKAGE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/python");

/// What [`init`] loaded.
#[derive(Debug, Default)]
pub struct Loaded {
    /// Modules imported successfully.
    pub modules: Vec<String>,
    /// Modules that failed, with the error.
    pub errors: Vec<(String, String)>,
}

fn site_packages(venv: &Path, py: Python<'_>) -> PyResult<String> {
    let v = py.version_info();
    Ok(venv
        .join(format!("lib/python{}.{}/site-packages", v.major, v.minor))
        .to_string_lossy()
        .into_owned())
}

fn prepare_path(py: Python<'_>) -> PyResult<()> {
    let sys = py.import("sys")?;
    let path = sys.getattr("path")?.cast_into::<PyList>()?;
    if let Some(venv) = std::env::var_os("VIRTUAL_ENV") {
        let dir = site_packages(Path::new(&venv), py)?;
        py.import("site")?.call_method1("addsitedir", (dir,))?;
    }
    if let Some(extra) = std::env::var_os("RURO_PY_PATH") {
        for (i, dir) in std::env::split_paths(&extra).enumerate() {
            path.insert(i, dir.to_string_lossy().into_owned())?;
        }
    }
    // Fall back to the package in the source tree if `ruro` is not installed.
    let util = py.import("importlib.util")?;
    if util.call_method1("find_spec", ("ruro",))?.is_none()
        && Path::new(SOURCE_PACKAGE_DIR).is_dir()
    {
        path.append(SOURCE_PACKAGE_DIR)?;
    }
    Ok(())
}

/// Imports `spec`: a module name, or a path to a `.py` file.
fn import_one(py: Python<'_>, spec: &str) -> PyResult<()> {
    if spec.ends_with(".py") {
        let file = Path::new(spec);
        let dir = file.parent().filter(|d| !d.as_os_str().is_empty());
        let dir = dir.unwrap_or(Path::new(".")).canonicalize()?;
        let stem = file
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let path = py.import("sys")?.getattr("path")?.cast_into::<PyList>()?;
        path.insert(0, dir.to_string_lossy().into_owned())?;
        py.import(stem.as_str())?;
    } else {
        py.import(spec)?;
    }
    Ok(())
}

/// Starts the interpreter and imports node modules. Import failures are
/// reported in the result rather than aborting, so the host still offers its
/// Rust nodes.
pub fn init() -> Result<Loaded> {
    Python::initialize();
    Python::attach(|py| -> PyResult<Loaded> {
        let native = pyo3::wrap_pymodule!(crate::native)(py);
        native.bind(py).setattr("EMBEDDED", true)?;
        py.import("sys")?
            .getattr("modules")?
            .set_item("ruro._native", native)?;
        prepare_path(py)?;
        py.import("ruro")?;

        let mut loaded = Loaded::default();
        let mut record = |name: String, r: PyResult<()>| match r {
            Ok(()) => loaded.modules.push(name),
            Err(e) => loaded
                .errors
                .push((name, crate::node::py_err(py, e).to_string())),
        };
        if let Ok(list) = std::env::var("RURO_PY_MODULES") {
            for spec in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                record(spec.to_owned(), import_one(py, spec));
            }
        }
        let eps = py.import("importlib.metadata")?.call_method(
            "entry_points",
            (),
            Some(&[("group", ENTRY_POINT_GROUP)].into_py_dict(py)?),
        )?;
        for ep in eps.try_iter()? {
            let ep = ep?;
            let name: String = ep.getattr("value")?.extract()?;
            record(name, ep.call_method0("load").map(drop));
        }
        Ok(loaded)
    })
    .map_err(|e| Python::attach(|py| crate::node::py_err(py, e)))
}
