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
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, anyhow, bail};
use parking_lot::Mutex;
use ruro_msg::{Message, now_ns};
use serde_json::Value;

use crate::{Builder, BuiltNode, Ctx, GraphClock, Node, NodeSpec, RawSink, Result};

/// Encoded messages captured from one output, with their stamps.
#[derive(Default)]
struct Captured(Mutex<Vec<(u64, Vec<u8>)>>);

struct Capture(Arc<Captured>);

impl RawSink for Capture {
    fn send(
        &self,
        _seq: u64,
        stamp_ns: u64,
        len: usize,
        write: &mut dyn FnMut(&mut [u8]),
    ) -> Result {
        let mut buf = vec![0; len];
        write(&mut buf);
        self.0.0.lock().push((stamp_ns, buf));
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
    estop: Arc<AtomicBool>,
    /// The e-stop was engaged and `on_estop` has not run yet.
    estop_pending: bool,
    /// Set by [`Harness::set_sim_time`].
    sim: Option<GraphClock>,
}

impl Harness {
    /// Builds a node of a registered kind with the given parameters (a JSON
    /// object, or `null` for defaults) and an input queue depth of 64.
    pub fn new(kind: &str, params: Value) -> Result<Self> {
        let params = param_map(params)?;
        let desc = crate::descriptor(kind).ok_or_else(|| anyhow!("unknown node type `{kind}`"))?;
        let mut b = Builder::new("test", &params, deep_queues(&desc));
        let node = crate::build(kind, &mut b)
            .ok_or_else(|| anyhow!("unknown node type `{kind}`"))?
            .with_context(|| format!("building `{kind}`"))?;
        Ok(Self::from_built(b.finish(node)))
    }

    /// Builds a node of type `T` and lets `setup` adjust it before it is
    /// wrapped, e.g. to attach a mock transport in place of a device.
    pub fn with_node<T: Node + NodeSpec>(
        params: Value,
        setup: impl FnOnce(&mut T),
    ) -> Result<Self> {
        let params = param_map(params)?;
        let mut b = Builder::new("test", &params, deep_queues(&T::descriptor()));
        let mut node = T::build(&mut b)?;
        setup(&mut node);
        Ok(Self::from_built(b.finish(Box::new(node))))
    }

    fn from_built(built: BuiltNode) -> Self {
        let mut captured = HashMap::new();
        for (name, out) in &built.outputs {
            let c = Arc::new(Captured::default());
            out.add_remote(Box::new(Capture(c.clone())));
            captured.insert(name.clone(), c);
        }
        let estop = Arc::new(AtomicBool::new(false));
        Self {
            built,
            ctx: Ctx::with_runtime("test", GraphClock::wall(), estop.clone()),
            captured,
            seq: 0,
            started: false,
            estop,
            estop_pending: false,
            sim: None,
        }
    }

    /// Puts the node on a simulated clock at `time_ns`, as in a graph with
    /// `clock: sim`, and moves that clock on later calls. The first call
    /// must come before the first step.
    pub fn set_sim_time(&mut self, time_ns: u64) {
        let clock = match &self.sim {
            Some(c) => c.clone(),
            None => {
                assert!(!self.started, "set_sim_time before the first step");
                let c = GraphClock::sim();
                self.ctx = Ctx::with_runtime("test", c.clone(), self.estop.clone());
                self.sim = Some(c.clone());
                c
            }
        };
        clock.sim_clock().expect("a simulated clock").set(time_ns);
    }

    /// Engages or releases the emergency stop. Like a host, engaging runs the
    /// node's `on_estop` once (right away if the node has started, otherwise
    /// after `start`), and `ctx.estopped()` reports the state on every step.
    pub fn set_estop(&mut self, engaged: bool) {
        let was = self.estop.swap(engaged, Ordering::Relaxed);
        if engaged && !was {
            self.estop_pending = true;
            self.run_estop();
        } else if !engaged {
            self.estop_pending = false;
        }
    }

    fn run_estop(&mut self) {
        if self.started && std::mem::take(&mut self.estop_pending) {
            self.built.node.on_estop(&mut self.ctx);
        }
    }

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

    /// Delivers an already encoded message to an input.
    pub fn push_encoded(&mut self, input: &str, bytes: &[u8]) -> Result {
        let port = self
            .built
            .inputs
            .iter()
            .find(|(n, _)| n == input)
            .map(|(_, p)| p.clone())
            .ok_or_else(|| anyhow!("no input `{input}`"))?;
        self.seq += 1;
        port.push_encoded(self.seq, now_ns(), bytes)
    }

    /// Message type of an input or output port.
    pub fn port_type(&self, port: &str) -> Option<&'static str> {
        let b = &self.built;
        b.inputs
            .iter()
            .find(|(n, _)| n == port)
            .map(|(_, p)| p.ty())
            .or_else(|| {
                b.outputs
                    .iter()
                    .find(|(n, _)| n == port)
                    .map(|(_, p)| p.ty())
            })
    }

    /// Runs one step (calling `start` first if it has not run yet).
    pub fn step(&mut self) -> Result {
        if !self.started {
            self.started = true;
            self.built.node.start(&mut self.ctx)?;
            self.run_estop();
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
        Ok(self
            .take_stamped(output)?
            .into_iter()
            .map(|(_, m)| m)
            .collect())
    }

    /// Like [`take`](Self::take), with each message's stamp (the sample's,
    /// which a ROS bridge puts in headerless messages).
    pub fn take_stamped<T: Message>(&mut self, output: &str) -> Result<Vec<(u64, T)>> {
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
            .map(|(stamp, b)| Ok((*stamp, T::decode(b)?)))
            .collect()
    }

    /// The raw encoded messages an output has sent so far, removed.
    pub fn take_encoded(&mut self, output: &str) -> Result<Vec<Vec<u8>>> {
        let c = self
            .captured
            .get(output)
            .ok_or_else(|| anyhow!("no output `{output}`"))?;
        Ok(std::mem::take(&mut *c.0.lock())
            .into_iter()
            .map(|(_, b)| b)
            .collect())
    }

    /// The built node and its ports.
    pub fn built(&self) -> &BuiltNode {
        &self.built
    }
}

fn param_map(params: Value) -> Result<BTreeMap<String, Value>> {
    match params {
        Value::Null => Ok(BTreeMap::new()),
        Value::Object(m) => Ok(m.into_iter().collect()),
        other => bail!("parameters must be a JSON object, got {other}"),
    }
}

/// Queue depths that keep every pushed message until the node takes it.
fn deep_queues(desc: &crate::NodeDescriptor) -> HashMap<String, usize> {
    desc.inputs.iter().map(|p| (p.name.clone(), 64)).collect()
}

impl Drop for Harness {
    fn drop(&mut self) {
        if self.started {
            self.built.node.stop(&mut self.ctx);
        }
    }
}
