//! A node instance implemented by a plugin.

use std::ffi::{CString, c_void};
use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use ruro_graph::NodeDescriptor;
use ruro_node::{Builder, Ctx, DynNode};
use serde_json::Value;

use crate::abi::{ruro_builder, ruro_ctx, ruro_node_type};
use crate::api::{API, CBuilder, clear_error, take_error, to_c_value};
use crate::ports::{CInput, COutput};

/// A node type a plugin registered.
pub struct NativeType {
    pub raw: ruro_node_type,
    pub desc: NodeDescriptor,
}

// SAFETY: the type's strings, functions and type_data belong to the loaded
// plugin, which is never unloaded; the plugin's functions are called under
// the rules ruro.h states.
unsafe impl Send for NativeType {}
unsafe impl Sync for NativeType {}

pub struct NativeNode {
    ty: Arc<NativeType>,
    instance: *mut c_void,
    // Held for the instance, which points into them; dropped after it.
    _inputs: Vec<(String, Box<CInput>)>,
    _outputs: Vec<(String, Box<COutput>)>,
}

// SAFETY: one instance is used from one thread at a time (the executor's
// contract), which is all ruro.h promises plugins.
unsafe impl Send for NativeNode {}

/// The error the plugin recorded, or a generic one.
fn failure(what: &str, kind: &str) -> anyhow::Error {
    match take_error() {
        Some(e) => anyhow!("{e}"),
        None => anyhow!("`{kind}`: {what} failed without saying why (call api->set_error)"),
    }
}

impl NativeNode {
    pub fn build(ty: Arc<NativeType>, b: &mut Builder) -> Result<Box<dyn DynNode>> {
        let id = CString::new(b.id().replace('\0', "")).unwrap();
        let mut cb = CBuilder {
            builder: b,
            desc: &ty.desc,
            id,
            inputs: Vec::new(),
            outputs: Vec::new(),
            strings: Vec::new(),
        };
        let create = ty.raw.create.expect("checked at registration");
        clear_error();
        // SAFETY: the plugin's create, with the builder it may use until it
        // returns.
        let instance = unsafe {
            create(
                &API,
                (&mut cb as *mut CBuilder).cast::<ruro_builder>(),
                ty.raw.type_data,
            )
        };
        if instance.is_null() {
            return Err(failure("create", &ty.desc.kind));
        }
        let (inputs, outputs) = (cb.inputs, cb.outputs);
        Ok(Box::new(Self {
            ty,
            instance,
            _inputs: inputs,
            _outputs: outputs,
        }))
    }

    fn call(&mut self, f: Option<crate::abi::CallFn>, ctx: &mut Ctx, what: &str) -> Result<()> {
        let Some(f) = f else { return Ok(()) };
        clear_error();
        // SAFETY: the plugin's function, with its instance and the context
        // of this call.
        let rc = unsafe { f(self.instance, (ctx as *mut Ctx).cast::<ruro_ctx>()) };
        if rc == 0 {
            Ok(())
        } else {
            Err(failure(what, &self.ty.desc.kind))
        }
    }

    fn call_void(&mut self, f: Option<crate::abi::VoidFn>, ctx: &mut Ctx) {
        if let Some(f) = f {
            clear_error();
            // SAFETY: as in `call`.
            unsafe { f(self.instance, (ctx as *mut Ctx).cast::<ruro_ctx>()) };
            if let Some(e) = take_error() {
                tracing::warn!("{e}");
            }
        }
    }
}

impl DynNode for NativeNode {
    fn start(&mut self, ctx: &mut Ctx) -> ruro_node::Result {
        self.call(self.ty.raw.start, ctx, "start")
    }

    fn step(&mut self, ctx: &mut Ctx) -> ruro_node::Result {
        self.call(self.ty.raw.step, ctx, "step")
    }

    fn stop(&mut self, ctx: &mut Ctx) {
        self.call_void(self.ty.raw.stop, ctx);
    }

    fn on_estop(&mut self, ctx: &mut Ctx) {
        self.call_void(self.ty.raw.on_estop, ctx);
    }

    fn set_param(&mut self, name: &str, value: &Value) -> ruro_node::Result {
        let Some(f) = self.ty.raw.set_param else {
            bail!(
                "`{}` does not take parameter changes while it runs",
                self.ty.desc.kind
            );
        };
        let Some(p) = self.ty.desc.param(name) else {
            bail!("`{}` has no parameter `{name}`", self.ty.desc.kind);
        };
        let mut strings = Vec::new();
        let v = to_c_value(p.kind, value, &mut strings);
        let name_c = CString::new(name).unwrap_or_default();
        clear_error();
        // SAFETY: the plugin's function, with its instance and values that
        // outlive the call.
        let rc = unsafe { f(self.instance, name_c.as_ptr(), &v) };
        if rc == 0 {
            Ok(())
        } else {
            Err(failure("set_param", &self.ty.desc.kind))
        }
    }
}

impl Drop for NativeNode {
    fn drop(&mut self) {
        if let Some(destroy) = self.ty.raw.destroy {
            // SAFETY: the instance create returned, destroyed once.
            unsafe { destroy(self.instance) };
        }
    }
}
