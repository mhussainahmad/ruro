//! Runs one process group of a graph: one thread per node, in-process wires
//! as shared queues, cross-process wires over shared memory.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use crossbeam_channel::{Receiver, Sender};
use ruro_graph::{Graph, PortRef};
use ruro_node::{Builder, BuiltNode, Ctx, DynNode, ErasedInput, ErasedOutput, RawSink, Waker};
use ruro_proto::{GroupMetrics, Keys, NodeState};
use ruro_transport::{Shm, ShmPublisher};
use serde_json::Value;

use crate::stats::{MetricsWindow, NodeStats, current_tid};

/// How cross-process wires are carried.
pub enum Remote {
    /// Shared memory, with topic names derived from the daemon keys.
    Shm(Keys),
    /// Cross-process wires are ignored (single-group tests).
    Disabled,
}

struct NodeHandle {
    id: String,
    thread: Option<JoinHandle<()>>,
    waker: Arc<Waker>,
    params: Sender<(String, Value)>,
    stats: Arc<NodeStats>,
    inputs: Vec<Arc<dyn ErasedInput>>,
    window: MetricsWindow,
}

/// A running process group.
pub struct Executor {
    group: String,
    nodes: Vec<NodeHandle>,
    outputs: HashMap<PortRef, Arc<dyn ErasedOutput>>,
    stop: Arc<AtomicBool>,
    receivers: Vec<JoinHandle<()>>,
    _shm: Option<Arc<Shm>>,
}

struct ShmSink(ShmPublisher);

impl RawSink for ShmSink {
    fn send(
        &self,
        seq: u64,
        stamp_ns: u64,
        len: usize,
        write: &mut dyn FnMut(&mut [u8]),
    ) -> ruro_node::Result {
        self.0.send(seq, stamp_ns, len, write)
    }
}

impl Executor {
    /// Builds, wires and starts every node of `group`.
    pub fn start(graph: &Graph, group: &str, remote: Remote) -> Result<Self> {
        let ids: Vec<String> = graph
            .groups()
            .remove(group)
            .with_context(|| format!("no group `{group}` in graph"))?;
        let stop = Arc::new(AtomicBool::new(false));

        // Build every node; ports are created as the node asks for them.
        let mut built: Vec<(String, BuiltNode)> = Vec::new();
        for id in &ids {
            let inst = &graph.nodes[id];
            let depths: HashMap<String, usize> = graph
                .wires
                .iter()
                .filter(|w| &w.to.node == id)
                .map(|w| (w.to.port.clone(), w.depth()))
                .collect();
            let mut b = Builder::new(id, &inst.params, depths);
            let node = ruro_node::build(&inst.kind, &mut b)
                .with_context(|| format!("{id}: unknown node type `{}`", inst.kind))?
                .with_context(|| format!("{id}: failed to construct"))?;
            built.push((id.clone(), b.finish(node)));
        }

        let mut outputs: HashMap<PortRef, Arc<dyn ErasedOutput>> = HashMap::new();
        let mut inputs: HashMap<PortRef, Arc<dyn ErasedInput>> = HashMap::new();
        for (id, b) in &built {
            for (name, port) in &b.outputs {
                outputs.insert(PortRef::new(id, name), port.clone());
            }
            for (name, port) in &b.inputs {
                inputs.insert(PortRef::new(id, name), port.clone());
            }
        }

        let in_group = |n: &str| graph.group_of(n) == Some(group);
        let shm = match &remote {
            Remote::Shm(_) if graph.wires.iter().any(|w| graph.is_remote(w)) => {
                Shm::cleanup_stale();
                Some(Arc::new(Shm::new(&format!("ruro-{}-{group}", graph.name))?))
            }
            _ => None,
        };

        let mut receivers = Vec::new();
        for wire in &graph.wires {
            match (in_group(&wire.from.node), in_group(&wire.to.node)) {
                (true, true) => {
                    let out = outputs
                        .get(&wire.from)
                        .with_context(|| format!("no output `{}`", wire.from))?;
                    let inp = inputs
                        .get(&wire.to)
                        .with_context(|| format!("no input `{}`", wire.to))?;
                    out.connect_local(inp.clone())
                        .with_context(|| format!("wire `{wire}`"))?;
                }
                (false, true) => {
                    let (Some(shm), Remote::Shm(keys)) = (&shm, &remote) else {
                        continue;
                    };
                    let inp = inputs
                        .get(&wire.to)
                        .with_context(|| format!("no input `{}`", wire.to))?
                        .clone();
                    let topic = keys.shm_topic(&graph.name, &wire.from.node, &wire.from.port);
                    let sub = shm.subscriber(&topic, wire.depth())?;
                    let stop = stop.clone();
                    let name = format!("ruro-rx:{}", wire.to);
                    receivers.push(std::thread::Builder::new().name(name).spawn(move || {
                        while !stop.load(Ordering::Relaxed) {
                            if let Err(e) = sub.wait(Duration::from_millis(100)) {
                                tracing::warn!("shared-memory wait failed: {e:#}");
                                std::thread::sleep(Duration::from_millis(100));
                            }
                            let r = sub.drain(|seq, stamp, body| {
                                if let Err(e) = inp.push_encoded(seq, stamp, body) {
                                    tracing::warn!("dropping undecodable message: {e:#}");
                                }
                            });
                            if let Err(e) = r {
                                tracing::warn!("shared-memory receive failed: {e:#}");
                            }
                        }
                    })?);
                }
                _ => {}
            }
        }
        if let (Some(shm), Remote::Shm(keys)) = (&shm, &remote) {
            for port in graph.exported_outputs(group) {
                let out = outputs
                    .get(&port)
                    .with_context(|| format!("no output `{port}`"))?;
                let publisher =
                    shm.publisher(&keys.shm_topic(&graph.name, &port.node, &port.port))?;
                out.add_remote(Box::new(ShmSink(publisher)));
            }
        }

        let mut nodes = Vec::new();
        for (id, b) in built {
            let inst = &graph.nodes[&id];
            let desc = ruro_node::descriptor(&inst.kind).expect("built above");
            let rate = inst.rate.or(desc.default_rate);
            if desc.inputs.is_empty() && rate.is_none() {
                bail!("{id}: source node needs a rate");
            }
            let period = rate.map(|r| Duration::from_secs_f64(1.0 / r));
            let stats = Arc::new(NodeStats::default());
            let (params_tx, params_rx) = crossbeam_channel::unbounded();
            let BuiltNode {
                node,
                waker,
                inputs,
                ..
            } = b;
            let worker = Worker {
                ctx: Ctx::new(&id),
                node,
                waker: waker.clone(),
                params: params_rx,
                stats: stats.clone(),
                stop: stop.clone(),
                period,
            };
            let thread = std::thread::Builder::new()
                .name(format!("ruro:{id}"))
                .spawn(move || worker.run())
                .with_context(|| format!("{id}: failed to spawn thread"))?;
            nodes.push(NodeHandle {
                id,
                thread: Some(thread),
                waker,
                params: params_tx,
                stats,
                inputs: inputs.into_iter().map(|(_, p)| p).collect(),
                window: MetricsWindow::new(),
            });
        }

        Ok(Self {
            group: group.to_owned(),
            nodes,
            outputs,
            stop,
            receivers,
            _shm: shm,
        })
    }

    pub fn group(&self) -> &str {
        &self.group
    }

    /// Output ports of the nodes in this group.
    pub fn outputs(&self) -> &HashMap<PortRef, Arc<dyn ErasedOutput>> {
        &self.outputs
    }

    pub fn node_ids(&self) -> impl Iterator<Item = &str> {
        self.nodes.iter().map(|n| n.id.as_str())
    }

    /// Queues a parameter change; it is applied between steps.
    pub fn set_param(&self, node: &str, name: &str, value: Value) -> Result<()> {
        let send = self
            .param_sender(node)
            .ok_or_else(|| anyhow!("no node `{node}` in this group"))?;
        send(name.to_owned(), value);
        Ok(())
    }

    /// A handle that queues parameter changes for `node` from any thread.
    pub fn param_sender(
        &self,
        node: &str,
    ) -> Option<impl Fn(String, Value) + Send + Sync + 'static> {
        let n = self.nodes.iter().find(|n| n.id == node)?;
        let (tx, waker) = (n.params.clone(), n.waker.clone());
        Some(move |name, value| {
            if tx.send((name, value)).is_ok() {
                waker.notify();
            }
        })
    }

    /// Metrics for the window since the previous call.
    pub fn metrics(&mut self) -> GroupMetrics {
        let nodes = self
            .nodes
            .iter_mut()
            .map(|n| {
                let (mut sum, mut count, mut dropped) = (0, 0, 0);
                for i in &n.inputs {
                    let s = i.stats();
                    sum += s.age_ns_sum.load(Ordering::Relaxed);
                    count += s.age_count.load(Ordering::Relaxed);
                    dropped += s.dropped.load(Ordering::Relaxed);
                }
                n.window.sample(&n.id, &n.stats, (sum, count), dropped)
            })
            .collect();
        GroupMetrics {
            group: self.group.clone(),
            pid: std::process::id(),
            nodes,
        }
    }

    /// Stops all nodes and waits for their threads.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for n in &self.nodes {
            n.waker.notify();
        }
        for n in &mut self.nodes {
            if let Some(t) = n.thread.take() {
                let _ = t.join();
            }
        }
        for t in self.receivers.drain(..) {
            let _ = t.join();
        }
    }
}

impl Drop for Executor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct Worker {
    ctx: Ctx,
    node: Box<dyn DynNode>,
    waker: Arc<Waker>,
    params: Receiver<(String, Value)>,
    stats: Arc<NodeStats>,
    stop: Arc<AtomicBool>,
    period: Option<Duration>,
}

/// Longest a node thread sleeps before re-checking for shutdown.
const IDLE_WAIT: Duration = Duration::from_millis(100);

impl Worker {
    fn run(mut self) {
        self.stats.set_tid(current_tid());
        let id = self.ctx.id().to_owned();
        match catch_unwind(AssertUnwindSafe(|| self.node.start(&mut self.ctx))) {
            Ok(Ok(())) => self.stats.set_state(NodeState::Running),
            Ok(Err(e)) => return self.fail(&id, format!("start failed: {e:#}")),
            Err(p) => return self.fail(&id, format!("panicked in start: {}", panic_message(&p))),
        }

        let mut next = Instant::now();
        let mut seen = 0;
        let mut last_logged = None::<Instant>;
        while !self.stop.load(Ordering::Relaxed) {
            self.apply_params(&id);
            match self.period {
                Some(period) => {
                    let now = Instant::now();
                    if now < next {
                        let g = self.waker.generation();
                        self.waker.wait(g, Some(next.min(now + IDLE_WAIT)));
                        continue;
                    }
                    next += period;
                    if next < now {
                        // Fell behind: skip missed ticks instead of bursting.
                        next = now + period;
                    }
                }
                None => {
                    let g = self.waker.wait(seen, Some(Instant::now() + IDLE_WAIT));
                    if g == seen {
                        continue;
                    }
                    seen = g;
                }
            }

            let t0 = Instant::now();
            self.ctx.begin_step(t0);
            let result = catch_unwind(AssertUnwindSafe(|| self.node.step(&mut self.ctx)));
            self.stats
                .record_step(t0.elapsed().as_micros().min(u32::MAX as u128) as u32);
            self.ctx.end_step();
            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    let msg = format!("{e:#}");
                    if last_logged.is_none_or(|t| t.elapsed() >= Duration::from_secs(1)) {
                        tracing::warn!("step failed: {msg}");
                        last_logged = Some(Instant::now());
                    }
                    self.stats.record_error(msg);
                }
                Err(p) => return self.fail(&id, format!("panicked: {}", panic_message(&p))),
            }
        }

        let _ = catch_unwind(AssertUnwindSafe(|| self.node.stop(&mut self.ctx)));
        self.stats.set_state(NodeState::Stopped);
    }

    fn apply_params(&mut self, id: &str) {
        while let Ok((name, value)) = self.params.try_recv() {
            match self.node.set_param(&name, &value) {
                Ok(()) => tracing::info!("{id}.{name} = {value}"),
                Err(e) => tracing::warn!("cannot set {id}.{name}: {e:#}"),
            }
        }
    }

    /// Marks the node failed and parks until shutdown; sibling nodes keep running.
    fn fail(&mut self, id: &str, msg: String) {
        tracing::error!("{id} {msg}");
        self.stats.record_error(msg);
        self.stats.set_state(NodeState::Failed);
        while !self.stop.load(Ordering::Relaxed) {
            let g = self.waker.generation();
            self.waker.wait(g, Some(Instant::now() + IDLE_WAIT));
        }
    }
}

fn panic_message(p: &Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}
