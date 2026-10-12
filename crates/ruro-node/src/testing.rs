//! Drive a single node in a test, without an executor or a graph.
//!
//! ```ignore
//! let mut h = Harness::new("std/gain", json!({ "k": 2.0 }))?;
//! h.push("input", Scalar(1.5))?;
//! h.step()?;
//! assert_eq!(h.take::<Scalar>("output")?, [Scalar(3.0)]);
//! ```
//!
//! Inputs are fed and outputs captured through the same encoded path a
//! cross-process wire uses, so messages are copied; that keeps the harness
//! independent of the concrete port types.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use anyhow::{Context, anyhow, bail};
use parking_lot::Mutex;
use ruro_msg::{Message, now_ns};
use serde_json::Value;

use crate::{Builder, BuiltNode, Ctx, RawSink, Result};

/// Encoded messages captured from one output.
#[derive(Default)]
struct Captured(Mutex<Vec<Vec<u8>>>);

struct Capture(Arc<Captured>);

impl RawSink for Capture {
    fn send(
        &self,
        _seq: u64,
        _stamp_ns: u64,
        len: usize,
        write: &mut dyn FnMut(&mut [u8]),
    ) -> Result {
        let mut buf = vec![0; len];
        write(&mut buf);
        self.0.0.lock().push(buf);
        Ok(())
    }
}

/// A single node instance with test access to its ports.
pub struct Harness {
    built: BuiltNode,
    ctx: Ctx,
    captured: HashMap<String, Arc<Captured>>,
    seq: u64,
    started: bool,
}

impl Harness {
    /// Builds a node of a registered kind with the given parameters (a JSON
    /// object, or `null` for defaults) and an input queue depth of 64.
    pub fn new(kind: &str, params: Value) -> Result<Self> {
        let params: BTreeMap<String, Value> = match params {
            Value::Null => BTreeMap::new(),
            Value::Object(m) => m.into_iter().collect(),
            other => bail!("parameters must be a JSON object, got {other}"),
        };
        let desc = crate::descriptor(kind).ok_or_else(|| anyhow!("unknown node type `{kind}`"))?;
        let depths = desc.inputs.iter().map(|p| (p.name.clone(), 64)).collect();
        let mut b = Builder::new("test", &params, depths);
        let node = crate::build(kind, &mut b)
            .ok_or_else(|| anyhow!("unknown node type `{kind}`"))?
            .with_context(|| format!("building `{kind}`"))?;
        let built = b.finish(node);
        let mut captured = HashMap::new();
        for (name, out) in &built.outputs {
            let c = Arc::new(Captured::default());
            out.add_remote(Box::new(Capture(c.clone())));
            captured.insert(name.clone(), c);
        }
        Ok(Self {
            built,
            ctx: Ctx::new("test"),
            captured,
            seq: 0,
            started: false,
        })
    }

    /// Delivers a message to an input.
    pub fn push<T: Message>(&mut self, input: &str, msg: T) -> Result {
        let port = self
            .built
            .inputs
            .iter()
            .find(|(n, _)| n == input)
            .map(|(_, p)| p.clone())
            .ok_or_else(|| anyhow!("no input `{input}`"))?;
        if port.ty() != T::TYPE {
            bail!("input `{input}` takes {}, not {}", port.ty(), T::TYPE);
        }
        self.seq += 1;
        port.push_encoded(self.seq, now_ns(), &msg.to_bytes())
    }

    /// Runs one step (calling `start` first if it has not run yet).
    pub fn step(&mut self) -> Result {
        if !self.started {
            self.started = true;
            self.built.node.start(&mut self.ctx)?;
        }
        self.ctx.begin_step(std::time::Instant::now());
        let r = self.built.node.step(&mut self.ctx);
        self.ctx.end_step();
        r
    }

    /// Changes a parameter as the control plane would.
    pub fn set_param(&mut self, name: &str, value: Value) -> Result {
        self.built.node.set_param(name, &value)
    }

    /// Removes and decodes everything an output has sent so far.
    pub fn take<T: Message>(&mut self, output: &str) -> Result<Vec<T>> {
        let c = self
            .captured
            .get(output)
            .ok_or_else(|| anyhow!("no output `{output}`"))?;
        let ty = self
            .built
            .outputs
            .iter()
            .find(|(n, _)| n == output)
            .map(|(_, p)| p.ty())
            .unwrap_or_default();
        if ty != T::TYPE {
            bail!("output `{output}` sends {ty}, not {}", T::TYPE);
        }
        let raw = std::mem::take(&mut *c.0.lock());
        raw.iter()
            .map(|b| T::decode(b).map_err(Into::into))
            .collect()
    }

    /// The raw encoded messages an output has sent so far, removed.
    pub fn take_encoded(&mut self, output: &str) -> Result<Vec<Vec<u8>>> {
        let c = self
            .captured
            .get(output)
            .ok_or_else(|| anyhow!("no output `{output}`"))?;
        Ok(std::mem::take(&mut *c.0.lock()))
    }

    /// The built node and its ports.
    pub fn built(&self) -> &BuiltNode {
        &self.built
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if self.started {
            self.built.node.stop(&mut self.ctx);
        }
    }
}
