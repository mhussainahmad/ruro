//! Nodes in C and C++: plugins loaded into the host process.
//!
//! A plugin is a shared library built against `sdk/include/ruro/ruro.h` (or
//! `ruro.hpp` for C++) that exports `ruro_plugin_init`. Hosts load the
//! libraries listed in `RURO_PLUGINS` when they start; their node types then
//! run in the host's process, next to Rust nodes, with typed ports,
//! parameters, metrics and the e-stop. Messages cross as C structs that point
//! into the host's messages; see the book's "Nodes in C and C++".

mod abi;
mod api;
mod convert;
mod node;
mod ports;

use std::ffi::{CString, c_int};
use std::path::{Path, PathBuf};
use std::sync::Once;

use anyhow::{Context, Result, anyhow, bail};

pub use abi::ABI_VERSION;
pub use ports::SUPPORTED;

/// Plugin libraries, or directories of them, separated by `:`.
pub const ENV: &str = "RURO_PLUGINS";

/// Loads the plugins in [`ENV`]. Hosts call it when they start; later calls
/// do nothing. A plugin that cannot be loaded is reported on standard error
/// and skipped.
pub fn link() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let Some(list) = std::env::var_os(ENV) else {
            return;
        };
        for path in std::env::split_paths(&list) {
            if path.as_os_str().is_empty() {
                continue;
            }
            if let Err(e) = load_path(&path) {
                eprintln!("ruro-host: {ENV}: {}: {e:#}", path.display());
            }
        }
    });
}

fn is_library(p: &Path) -> bool {
    p.extension().is_some_and(|e| e == "so" || e == "dylib")
}

/// Loads the plugin at `path`, or every `*.so` (`*.dylib`) in the directory
/// `path`. Returns the node types registered.
pub fn load_path(path: &Path) -> Result<Vec<String>> {
    let files: Vec<PathBuf> = if path.is_dir() {
        let mut files: Vec<PathBuf> = std::fs::read_dir(path)
            .context("reading the directory")?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| is_library(p))
            .collect();
        files.sort();
        files
    } else {
        vec![path.to_path_buf()]
    };
    let mut kinds = Vec::new();
    let mut errors = Vec::new();
    for f in files {
        match load(&f) {
            Ok(k) => kinds.extend(k),
            Err(e) => errors.push(format!("{}: {e:#}", f.display())),
        }
    }
    if !errors.is_empty() {
        bail!("{}", errors.join("; "));
    }
    Ok(kinds)
}

type InitFn = unsafe extern "C" fn(*const abi::ruro_api, *mut abi::ruro_registrar) -> c_int;

/// Loads one plugin library and lets it register its node types. The
/// library stays loaded for the life of the process.
pub fn load(path: &Path) -> Result<Vec<String>> {
    let path = std::fs::canonicalize(path).with_context(|| format!("{}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let c = CString::new(path.as_os_str().as_bytes())?;
        // SAFETY: dlopen with a valid path; its constructors run here.
        let handle = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if handle.is_null() {
            return Err(anyhow!("cannot load it: {}", dlerror()));
        }
        // SAFETY: looking up a symbol in the library just loaded.
        let sym = unsafe { libc::dlsym(handle, c"ruro_plugin_init".as_ptr()) };
        if sym.is_null() {
            bail!("it does not export ruro_plugin_init (see RURO_PLUGIN_INIT in ruro.h)");
        }
        // SAFETY: ruro.h declares ruro_plugin_init with this signature.
        let init: InitFn = unsafe { std::mem::transmute(sym) };
        let mut reg = api::Registrar {
            library: path.display().to_string(),
            kinds: Vec::new(),
            refused: Vec::new(),
        };
        api::clear_error();
        // SAFETY: the plugin's entry point, with the host's table and a
        // registrar valid for the call.
        let rc = unsafe { init(&api::API, (&mut reg as *mut api::Registrar).cast()) };
        if !reg.refused.is_empty() {
            bail!("{}", reg.refused.join("; "));
        }
        if rc != 0 {
            let why = api::take_error().unwrap_or_else(|| format!("returned {rc}"));
            bail!("ruro_plugin_init failed: {why}");
        }
        Ok(reg.kinds)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        bail!("plugins are loaded on Linux and macOS hosts")
    }
}

#[cfg(unix)]
fn dlerror() -> String {
    // SAFETY: dlerror returns NULL or a C string valid until the next call.
    let e = unsafe { libc::dlerror() };
    if e.is_null() {
        "unknown error".to_owned()
    } else {
        // SAFETY: see above.
        unsafe { std::ffi::CStr::from_ptr(e) }
            .to_string_lossy()
            .into_owned()
    }
}
