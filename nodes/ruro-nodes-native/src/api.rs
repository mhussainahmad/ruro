//! The functions the host hands plugins (`ruro_api`), and registration.

use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use anyhow::{Context, Result, bail, ensure};
use ruro_graph::{NodeDescriptor, ParamDesc, ParamKind, PortDesc};
use ruro_node::{Builder, Ctx};
use serde_json::Value;

use crate::abi::*;
use crate::node::{NativeNode, NativeType};
use crate::ports::{CInput, COutput, SUPPORTED};

thread_local! {
    /// The error recorded by the callback running on this thread.
    static ERROR: RefCell<Option<String>> = const { RefCell::new(None) };
}

pub fn clear_error() {
    ERROR.with(|e| e.borrow_mut().take());
}

pub fn take_error() -> Option<String> {
    ERROR.with(|e| e.borrow_mut().take())
}

fn set_error_msg(msg: String) {
    ERROR.with(|e| *e.borrow_mut() = Some(msg));
}

/// Runs `f`, turning a panic into a recorded error and `fallback`: no unwind
/// may cross into the plugin's frames.
fn guard<R>(fallback: R, f: impl FnOnce() -> R) -> R {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|p| {
        let msg = p
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| p.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            .unwrap_or_else(|| "a panic in the host".to_owned());
        set_error_msg(msg);
        fallback
    })
}

/// # Safety
/// `p` must be NULL or a NUL-terminated string.
unsafe fn cstr<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        return None;
    }
    // SAFETY: the caller's contract.
    unsafe { CStr::from_ptr(p) }.to_str().ok()
}

/// What `create` works on: the Rust builder and the ports declared so far.
pub struct CBuilder<'a, 'b> {
    pub builder: &'a mut Builder<'b>,
    pub desc: &'a NodeDescriptor,
    pub id: CString,
    pub inputs: Vec<(String, Box<CInput>)>,
    pub outputs: Vec<(String, Box<COutput>)>,
    pub strings: Vec<CString>,
}

/// # Safety
/// `b` must be the builder of the running `create`.
unsafe fn builder<'x>(b: *mut ruro_builder) -> &'x mut CBuilder<'x, 'x> {
    // SAFETY: the caller's contract.
    unsafe { &mut *b.cast::<CBuilder<'x, 'x>>() }
}

/// # Safety
/// `c` must be the context of the running callback.
unsafe fn ctx<'x>(c: *mut ruro_ctx) -> &'x mut Ctx {
    // SAFETY: the caller's contract.
    unsafe { &mut *c.cast::<Ctx>() }
}

unsafe extern "C" fn set_error(msg: *const c_char) {
    guard((), || {
        // SAFETY: the plugin passes a C string.
        let m = unsafe { cstr(msg) }.unwrap_or("(an error without a message)");
        set_error_msg(m.to_owned());
    })
}

unsafe extern "C" fn log(level: c_int, msg: *const c_char) {
    guard((), || {
        // SAFETY: the plugin passes a C string.
        let m = unsafe { cstr(msg) }.unwrap_or("");
        match level {
            0 => tracing::error!("{m}"),
            1 => tracing::warn!("{m}"),
            3 => tracing::debug!("{m}"),
            _ => tracing::info!("{m}"),
        }
    })
}

unsafe extern "C" fn node_id(b: *mut ruro_builder) -> *const c_char {
    // SAFETY: called during create.
    guard(std::ptr::null(), || unsafe { builder(b) }.id.as_ptr())
}

unsafe extern "C" fn input(b: *mut ruro_builder, name: *const c_char) -> *mut ruro_input {
    guard(std::ptr::null_mut(), || {
        // SAFETY: called during create, with a C string.
        let (b, name) = unsafe { (builder(b), cstr(name)) };
        let Some(name) = name else {
            set_error_msg("input: the name is NULL or not UTF-8".into());
            return std::ptr::null_mut();
        };
        if let Some((_, i)) = b.inputs.iter_mut().find(|(n, _)| n == name) {
            return (&mut **i as *mut CInput).cast();
        }
        let Some(port) = b.desc.input(name) else {
            set_error_msg(format!("`{}` declares no input `{name}`", b.desc.kind));
            return std::ptr::null_mut();
        };
        match CInput::declare(b.builder, &port.ty, name) {
            Ok(i) => {
                b.inputs.push((name.to_owned(), Box::new(i)));
                (&mut *b.inputs.last_mut().unwrap().1 as *mut CInput).cast()
            }
            Err(e) => {
                set_error_msg(format!("{e:#}"));
                std::ptr::null_mut()
            }
        }
    })
}

unsafe extern "C" fn output(b: *mut ruro_builder, name: *const c_char) -> *mut ruro_output {
    guard(std::ptr::null_mut(), || {
        // SAFETY: called during create, with a C string.
        let (b, name) = unsafe { (builder(b), cstr(name)) };
        let Some(name) = name else {
            set_error_msg("output: the name is NULL or not UTF-8".into());
            return std::ptr::null_mut();
        };
        if let Some((_, o)) = b.outputs.iter_mut().find(|(n, _)| n == name) {
            return (&mut **o as *mut COutput).cast();
        }
        let Some(port) = b.desc.output(name) else {
            set_error_msg(format!("`{}` declares no output `{name}`", b.desc.kind));
            return std::ptr::null_mut();
        };
        match COutput::declare(b.builder, &port.ty, name) {
            Ok(o) => {
                b.outputs.push((name.to_owned(), Box::new(o)));
                (&mut *b.outputs.last_mut().unwrap().1 as *mut COutput).cast()
            }
            Err(e) => {
                set_error_msg(format!("{e:#}"));
                std::ptr::null_mut()
            }
        }
    })
}

/// A JSON parameter value for C; strings are kept in `strings`.
pub fn to_c_value(kind: ParamKind, v: &Value, strings: &mut Vec<CString>) -> ruro_value {
    let mut out = ruro_value {
        kind: RURO_INT,
        i: 0,
        f: 0.0,
        b: false,
        s: ruro_str::EMPTY,
    };
    match kind {
        ParamKind::Int => {
            out.kind = RURO_INT;
            out.i = v
                .as_i64()
                .or_else(|| v.as_f64().map(|f| f as i64))
                .unwrap_or(0);
            out.f = out.i as f64;
        }
        ParamKind::Float => {
            out.kind = RURO_FLOAT;
            out.f = v.as_f64().unwrap_or(0.0);
        }
        ParamKind::Bool => {
            out.kind = RURO_BOOL;
            out.b = v.as_bool().unwrap_or(false);
        }
        ParamKind::String => {
            out.kind = RURO_STRING;
            let s = v.as_str().unwrap_or("");
            let c = CString::new(s.replace('\0', "")).unwrap();
            out.s = ruro_str {
                ptr: c.as_ptr(),
                len: c.as_bytes().len(),
            };
            strings.push(c);
        }
    }
    out
}

unsafe extern "C" fn param(
    b: *mut ruro_builder,
    name: *const c_char,
    out: *mut ruro_value,
) -> c_int {
    guard(0, || {
        // SAFETY: called during create, with a C string.
        let (b, name) = unsafe { (builder(b), cstr(name)) };
        let Some(p) = name.and_then(|n| b.desc.param(n)) else {
            return 0;
        };
        let value = b
            .builder
            .raw_param(&p.name)
            .cloned()
            .unwrap_or(p.default.clone());
        let v = to_c_value(p.kind, &value, &mut b.strings);
        if !out.is_null() {
            // SAFETY: the plugin passes a ruro_value to fill.
            unsafe { *out = v };
        }
        1
    })
}

unsafe extern "C" fn dt(c: *mut ruro_ctx) -> f64 {
    // SAFETY: called during a callback.
    guard(0.0, || unsafe { ctx(c) }.dt().as_secs_f64())
}

unsafe extern "C" fn time(c: *mut ruro_ctx) -> f64 {
    // SAFETY: called during a callback.
    guard(0.0, || unsafe { ctx(c) }.time().as_secs_f64())
}

unsafe extern "C" fn steps(c: *mut ruro_ctx) -> u64 {
    // SAFETY: called during a callback.
    guard(0, || unsafe { ctx(c) }.steps())
}

unsafe extern "C" fn now_ns(c: *mut ruro_ctx) -> u64 {
    // SAFETY: called during a callback.
    guard(0, || unsafe { ctx(c) }.now_ns())
}

unsafe extern "C" fn estopped(c: *mut ruro_ctx) -> bool {
    // SAFETY: called during a callback.
    guard(false, || unsafe { ctx(c) }.estopped())
}

unsafe extern "C" fn queued(i: *mut ruro_input) -> usize {
    // SAFETY: a handle from `input`.
    guard(0, || unsafe { &*i.cast::<CInput>() }.queued())
}

unsafe fn take(
    i: *mut ruro_input,
    view: *mut c_void,
    sample: *mut ruro_sample,
    latest: bool,
) -> c_int {
    guard(-1, || {
        if i.is_null() || view.is_null() {
            set_error_msg("recv: the input or the view is NULL".into());
            return -1;
        }
        // SAFETY: a handle from `input`, and the view of its type.
        c_int::from(unsafe { (*i.cast::<CInput>()).take(latest, view, sample) })
    })
}

unsafe extern "C" fn recv(
    i: *mut ruro_input,
    view: *mut c_void,
    sample: *mut ruro_sample,
) -> c_int {
    // SAFETY: passed on.
    unsafe { take(i, view, sample, false) }
}

unsafe extern "C" fn latest(
    i: *mut ruro_input,
    view: *mut c_void,
    sample: *mut ruro_sample,
) -> c_int {
    // SAFETY: passed on.
    unsafe { take(i, view, sample, true) }
}

unsafe extern "C" fn send(o: *mut ruro_output, msg: *const c_void) -> c_int {
    guard(-1, || {
        if o.is_null() {
            set_error_msg("send: the output is NULL".into());
            return -1;
        }
        // SAFETY: a handle from `output`, and the struct of its type.
        match unsafe { (*o.cast::<COutput>()).send(msg) } {
            Ok(()) => 0,
            Err(e) => {
                set_error_msg(format!("{e:#}"));
                -1
            }
        }
    })
}

/// What `ruro_plugin_init` registers into.
pub struct Registrar {
    pub library: String,
    pub kinds: Vec<String>,
    pub refused: Vec<String>,
}

unsafe extern "C" fn register_node(
    reg: *mut ruro_registrar,
    abi_version: u32,
    layout: *const u32,
    n_layout: usize,
    ty: *const ruro_node_type,
) -> c_int {
    guard(-1, || {
        // SAFETY: the registrar passed to ruro_plugin_init.
        let reg = unsafe { &mut *reg.cast::<Registrar>() };
        // SAFETY: the plugin's own layout table and node type.
        match unsafe { check_and_register(&reg.library, abi_version, layout, n_layout, ty) } {
            Ok(kind) => {
                reg.kinds.push(kind);
                0
            }
            Err(e) => {
                let msg = format!("{e:#}");
                set_error_msg(msg.clone());
                reg.refused.push(msg);
                -1
            }
        }
    })
}

/// # Safety
/// `layout` must hold `n_layout` entries and `ty` be a valid node type.
unsafe fn check_and_register(
    library: &str,
    abi_version: u32,
    layout: *const u32,
    n_layout: usize,
    ty: *const ruro_node_type,
) -> Result<String> {
    ensure!(
        abi_version == ABI_VERSION,
        "built against ruro.h of ABI version {abi_version}; this host has version {ABI_VERSION}: rebuild it with this ruro's sdk/include"
    );
    ensure!(
        !layout.is_null() && !ty.is_null(),
        "NULL layout or node type"
    );
    // SAFETY: the caller's contract.
    let theirs = unsafe { std::slice::from_raw_parts(layout, n_layout) };
    ensure!(
        theirs.len() == LAYOUT.len(),
        "its ruro.h has {} structs, this host's {}: rebuild it with this ruro's sdk/include",
        theirs.len(),
        LAYOUT.len()
    );
    for ((name, ours), theirs) in LAYOUT_NAMES.iter().zip(LAYOUT).zip(theirs) {
        ensure!(
            ours == *theirs,
            "sizeof({name}) is {theirs} in the plugin but {ours} in this host: compiled with another ruro.h, or for another architecture"
        );
    }
    // SAFETY: the caller's contract.
    let raw = unsafe { *ty };
    // SAFETY: the plugin's strings, NUL-terminated.
    let desc = unsafe { descriptor(&raw, library)? };
    ensure!(
        raw.create.is_some(),
        "`{}` has no create function",
        desc.kind
    );
    ensure!(raw.step.is_some(), "`{}` has no step function", desc.kind);
    ensure!(
        raw.destroy.is_some(),
        "`{}` has no destroy function",
        desc.kind
    );
    let kind = desc.kind.clone();
    let native = Arc::new(NativeType { raw, desc });
    ruro_node::register_dynamic(
        native.desc.clone(),
        Arc::new(move |b| NativeNode::build(native.clone(), b)),
    );
    Ok(kind)
}

/// # Safety
/// The strings and arrays of `t` must be valid.
unsafe fn descriptor(t: &ruro_node_type, library: &str) -> Result<NodeDescriptor> {
    // SAFETY: the caller's contract, for every string below.
    let text = |p: *const c_char, what: &str| -> Result<String> {
        unsafe { cstr(p) }
            .map(str::to_owned)
            .with_context(|| format!("{what} is NULL or not UTF-8"))
    };
    let kind = text(t.kind, "the kind")?;
    ensure!(
        kind.contains('/') && !kind.starts_with('/') && !kind.ends_with('/'),
        "kind `{kind}` needs a namespace, e.g. `acme/{kind}`"
    );
    let doc = if t.doc.is_null() {
        String::new()
    } else {
        text(t.doc, "the doc")?
    };
    let ports = |p: *const ruro_port_desc, n: usize, what: &str| -> Result<Vec<PortDesc>> {
        if n == 0 {
            return Ok(Vec::new());
        }
        ensure!(!p.is_null(), "{what}: NULL with {n} entries");
        // SAFETY: the caller's contract.
        let ports = unsafe { std::slice::from_raw_parts(p, n) };
        ports
            .iter()
            .map(|port| {
                let name = text(port.name, &format!("the name of an {what}"))?;
                let ty = text(port.ty, &format!("the type of {what} `{name}`"))?;
                ensure!(
                    SUPPORTED.contains(&ty.as_str()),
                    "{what} `{name}`: message type `{ty}` is not supported by C nodes (supported: {})",
                    SUPPORTED.join(", ")
                );
                Ok(PortDesc { name, ty })
            })
            .collect()
    };
    let inputs = ports(t.inputs, t.n_inputs, "input")?;
    let outputs = ports(t.outputs, t.n_outputs, "output")?;
    let mut params = Vec::new();
    if t.n_params > 0 {
        ensure!(
            !t.params.is_null(),
            "params: NULL with {} entries",
            t.n_params
        );
        // SAFETY: the caller's contract.
        for p in unsafe { std::slice::from_raw_parts(t.params, t.n_params) } {
            let name = text(p.name, "a parameter name")?;
            let d = &p.default_value;
            let (kind, default) = match d.kind {
                RURO_INT => (ParamKind::Int, Value::from(d.i)),
                RURO_FLOAT => (ParamKind::Float, Value::from(d.f)),
                RURO_BOOL => (ParamKind::Bool, Value::from(d.b)),
                RURO_STRING => {
                    let s = if d.s.ptr.is_null() {
                        String::new()
                    } else {
                        // SAFETY: the caller's contract.
                        let bytes =
                            unsafe { std::slice::from_raw_parts(d.s.ptr.cast::<u8>(), d.s.len) };
                        String::from_utf8(bytes.to_vec())
                            .with_context(|| format!("the default of `{name}` is not UTF-8"))?
                    };
                    (ParamKind::String, Value::from(s))
                }
                other => bail!("parameter `{name}`: unknown kind {other}"),
            };
            let desc = ParamDesc {
                name: name.clone(),
                kind,
                default,
                min: p.has_min.then_some(p.min),
                max: p.has_max.then_some(p.max),
                doc: if p.doc.is_null() {
                    String::new()
                } else {
                    text(p.doc, "a parameter doc")?
                },
            };
            desc.check(&desc.default)
                .map_err(|e| anyhow::anyhow!("default of parameter `{name}`: {e}"))?;
            params.push(desc);
        }
    }
    ensure!(
        t.rate >= 0.0 && t.rate.is_finite(),
        "rate must be 0 (none) or positive"
    );
    Ok(NodeDescriptor {
        kind,
        doc,
        default_rate: (t.rate > 0.0).then_some(t.rate),
        inputs,
        outputs,
        params,
        source: Some(library.to_owned()),
    })
}

/// The table handed to every plugin.
pub static API: ruro_api = ruro_api {
    size: std::mem::size_of::<ruro_api>() as u32,
    abi_version: ABI_VERSION,
    set_error,
    log,
    node_id,
    input,
    output,
    param,
    dt,
    time,
    steps,
    now_ns,
    estopped,
    queued,
    recv,
    latest,
    send,
    register_node,
};
