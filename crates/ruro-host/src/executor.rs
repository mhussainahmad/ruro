//! Runs one process group of a graph: one thread per node (or one thread for
//! the whole group when it is deterministic), in-process wires as shared
//! queues, cross-process wires over shared memory and wires to other machines
//! over Zenoh.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use crossbeam_channel::Sender;
use parking_lot::Mutex;
use ruro_graph::{ClockSource, Graph, PortRef};
use ruro_msg::Message;
use ruro_node::{Builder, BuiltNode, ErasedInput, ErasedOutput, GraphClock, RawSink, Waker};
use ruro_proto::{DataFrame, GroupMetrics, Keys};
use ruro_transport::{Shm, ShmPublisher};
use serde_json::Value;
use zenoh::Wait;

use crate::record::Recorder;
use crate::rt;
use crate::run::{
    Cycle, ParamChange, Runner, RunnerParts, run_deterministic, run_threaded, step_order,
};
use crate::stats::{MetricsWindow, NodeStats};

/// How cross-process wires are carried.
pub enum Remote {
    /// Shared memory between groups on this machine, with topic names derived
    /// from the daemon keys; the daemon's name is this machine's name.
    Shm(Keys),
    /// Cross-process wires are ignored (single-group tests).
    Disabled,
}

/// Everything [`Executor::start_with`] can be configured with.
pub struct Options {
    pub remote: Remote,
    /// Session for wires to groups on other machines and for replay. Without
    /// one those wires are left unconnected.
    pub session: Option<zenoh::Session>,
    /// Compress large messages on wires to other machines (lz4).
    pub compress: bool,
    /// Start with the emergency stop engaged.
    pub estopped: bool,
    /// Nodes that start muted: their outputs are being replayed.
    pub muted: Vec<String>,
}

impl Options {
    pub fn new(remote: Remote) -> Self {
        Self {
            remote,
            session: None,
            compress: true,
            estopped: false,
            muted: Vec::new(),
        }
    }
}

struct NodeHandle {
    id: String,
    kind: String,
    waker: Arc<Waker>,
    params: Sender<ParamChange>,
    stats: Arc<NodeStats>,
    inputs: Vec<Arc<dyn ErasedInput>>,
    window: Mutex<MetricsWindow>,
    muted: Arc<AtomicBool>,
}

/// A running process group.
pub struct Executor {
    group: String,
    graph_name: String,
    nodes: Vec<NodeHandle>,
    outputs: HashMap<PortRef, Arc<dyn ErasedOutput>>,
    stop: Arc<AtomicBool>,
    estop: Arc<AtomicBool>,
    threads: Mutex<Vec<JoinHandle<()>>>,
    recorder: Mutex<Option<Recorder>>,
    clock: GraphClock,
    _shm: Option<Arc<Shm>>,
    _subscribers: Vec<zenoh::pubsub::Subscriber<()>>,
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

/// Carries an output to other machines over Zenoh.
struct NetSink {
    publisher: zenoh::pubsub::Publisher<'static>,
    compress: bool,
}

impl RawSink for NetSink {
    fn send(
        &self,
        seq: u64,
        stamp_ns: u64,
        len: usize,
        write: &mut dyn FnMut(&mut [u8]),
    ) -> ruro_node::Result {
        let mut body = vec![0; len];
        write(&mut body);
        let frame = DataFrame::encode(seq, stamp_ns, &body, self.compress);
        self.publisher
            .put(frame)
            .wait()
            .map_err(|e| anyhow!("network send failed: {e}"))
    }
}

/// Feeds the graph's `std/Clock` output into the simulated clock.
struct ClockSink(GraphClock);

impl RawSink for ClockSink {
    fn send(
        &self,
        _: u64,
        _: u64,
        len: usize,
        write: &mut dyn FnMut(&mut [u8]),
    ) -> ruro_node::Result {
        let mut buf = vec![0; len];
        write(&mut buf);
        set_sim_time(&self.0, &buf);
        Ok(())
    }
}

fn set_sim_time(clock: &GraphClock, bytes: &[u8]) {
    match ruro_msg::Clock::decode(bytes) {
        Ok(c) => {
            if let Some(sim) = clock.sim_clock() {
                sim.set(c.time_ns);
            }
        }
        Err(e) => tracing::warn!("ignoring malformed clock message: {e}"),
    }
}

/// The output port that drives simulated time, if the graph has one.
pub fn clock_port(graph: &Graph) -> Result<Option<PortRef>> {
    if graph.clock != ClockSource::Sim {
        return Ok(None);
    }
    let mut ports = Vec::new();
    for (id, inst) in &graph.nodes {
        let Some(desc) = ruro_node::descriptor(&inst.kind) else {
            continue;
        };
        for p in desc
            .outputs
            .iter()
            .filter(|p| p.ty == ruro_msg::Clock::TYPE)
        {
            ports.push(PortRef::new(id, &p.name));
        }
    }
    match ports.len() {
        0 => bail!("the graph uses `clock: sim` but no node has a std/Clock output"),
        1 => Ok(ports.pop()),
        _ => bail!(
            "the graph uses `clock: sim` but has several std/Clock outputs: {}",
            ports
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Where data reaches a group from another group.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Link {
    Shm,
    Net,
}

impl Executor {
    /// Builds, wires and starts every node of `group`.
    pub fn start(graph: &Graph, group: &str, remote: Remote) -> Result<Self> {
        Self::start_with(graph, group, Options::new(remote))
    }

    /// Like [`Executor::start`], with every option.
    pub fn start_with(graph: &Graph, group: &str, opts: Options) -> Result<Self> {
        let ids: Vec<String> = graph
            .groups()
            .remove(group)
            .with_context(|| format!("no group `{group}` in graph"))?;
        let config = graph.group_config(group);
        let realtime = config.realtime.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let estop = Arc::new(AtomicBool::new(opts.estopped));
        let clock = match graph.clock {
            ClockSource::Wall => GraphClock::wall(),
            ClockSource::Sim => GraphClock::sim(),
        };

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

        // Machines: groups without one run where the graph was deployed,
        // which for a host is the daemon that started it.
        let (keys, here) = match &opts.remote {
            Remote::Shm(keys) => (Some(keys.clone()), keys.daemon().to_owned()),
            Remote::Disabled => (None, String::new()),
        };
        let machine_of = |g: &str| {
            graph
                .group_config(g)
                .machine
                .unwrap_or_else(|| here.clone())
        };
        let link_to = |g: &str| {
            if machine_of(g) == machine_of(group) {
                Link::Shm
            } else {
                Link::Net
            }
        };
        let in_group = |n: &str| graph.group_of(n) == Some(group);

        let clock_port = clock_port(graph)?;
        // Data that leaves this group: (port, link) for each consumer group.
        let mut exports: BTreeSet<(PortRef, bool)> = BTreeSet::new();
        for w in &graph.wires {
            if in_group(&w.from.node) && !in_group(&w.to.node) {
                let to = graph.group_of(&w.to.node).unwrap_or_default();
                exports.insert((w.from.clone(), link_to(to) == Link::Net));
            }
        }
        if let Some(cp) = &clock_port
            && in_group(&cp.node)
        {
            let out = outputs
                .get(cp)
                .with_context(|| format!("no clock output `{cp}`"))?;
            out.add_remote(Box::new(ClockSink(clock.clone())));
            for g in graph.groups().keys().filter(|g| *g != group) {
                exports.insert((cp.clone(), link_to(g) == Link::Net));
            }
        }

        let needs_shm = keys.is_some()
            && (exports.iter().any(|(_, net)| !net)
                || graph.wires.iter().any(|w| {
                    in_group(&w.to.node)
                        && !in_group(&w.from.node)
                        && link_to(graph.group_of(&w.from.node).unwrap_or_default()) == Link::Shm
                })
                || clock_port.as_ref().is_some_and(|cp| {
                    !in_group(&cp.node)
                        && link_to(graph.group_of(&cp.node).unwrap_or_default()) == Link::Shm
                }));
        let shm = if needs_shm {
            Shm::cleanup_stale();
            Some(Arc::new(Shm::new(&format!("ruro-{}-{group}", graph.name))?))
        } else {
            None
        };

        let mut threads = Vec::new();
        let mut subscribers = Vec::new();
        // Delivers bytes from another group to an input (or the clock).
        type Deliver = Arc<dyn Fn(u64, u64, &[u8]) + Send + Sync>;
        let mut imports: Vec<(PortRef, Link, usize, Deliver)> = Vec::new();
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
                    let inp = inputs
                        .get(&wire.to)
                        .with_context(|| format!("no input `{}`", wire.to))?
                        .clone();
                    let from_group = graph.group_of(&wire.from.node).unwrap_or_default();
                    let deliver: Deliver = Arc::new(move |seq, stamp, body| {
                        if let Err(e) = inp.push_encoded(seq, stamp, body) {
                            tracing::warn!("dropping undecodable message: {e:#}");
                        }
                    });
                    imports.push((
                        wire.from.clone(),
                        link_to(from_group),
                        wire.depth(),
                        deliver,
                    ));
                }
                _ => {}
            }
        }
        if let Some(cp) = &clock_port
            && !in_group(&cp.node)
        {
            let c = clock.clone();
            let from_group = graph.group_of(&cp.node).unwrap_or_default();
            imports.push((
                cp.clone(),
                link_to(from_group),
                1,
                Arc::new(move |_, _, body| set_sim_time(&c, body)),
            ));
        }

        if let Some(keys) = &keys {
            for (port, link, depth, deliver) in imports {
                match link {
                    Link::Shm => {
                        let shm = shm.as_ref().expect("created above");
                        let topic = keys.shm_topic(&graph.name, &port.node, &port.port);
                        let sub = shm.subscriber(&topic, depth)?;
                        let stop = stop.clone();
                        let name = format!("ruro-rx:{port}");
                        threads.push(std::thread::Builder::new().name(name).spawn(move || {
                            while !stop.load(Ordering::Relaxed) {
                                if let Err(e) = sub.wait(Duration::from_millis(100)) {
                                    tracing::warn!("shared-memory wait failed: {e:#}");
                                    std::thread::sleep(Duration::from_millis(100));
                                }
                                if let Err(e) =
                                    sub.drain(|seq, stamp, body| deliver(seq, stamp, body))
                                {
                                    tracing::warn!("shared-memory receive failed: {e:#}");
                                }
                            }
                        })?);
                    }
                    Link::Net => {
                        let Some(session) = &opts.session else {
                            tracing::warn!(
                                "{port}: no network session; wire from another machine left unconnected"
                            );
                            continue;
                        };
                        let key = Keys::data(&graph.name, &port.node, &port.port);
                        let sub = session
                            .declare_subscriber(key)
                            .callback(move |sample| {
                                match DataFrame::decode(&sample.payload().to_bytes()) {
                                    Ok(f) => deliver(f.seq, f.stamp_ns, &f.body),
                                    Err(e) => tracing::warn!("dropping bad network frame: {e:#}"),
                                }
                            })
                            .wait()
                            .map_err(|e| anyhow!("subscribing to {port}: {e}"))?;
                        subscribers.push(sub);
                    }
                }
            }
            for (port, net) in &exports {
                let out = outputs
                    .get(port)
                    .with_context(|| format!("no output `{port}`"))?;
                if *net {
                    let Some(session) = &opts.session else {
                        tracing::warn!(
                            "{port}: no network session; wire to another machine left unconnected"
                        );
                        continue;
                    };
                    let publisher = session
                        .declare_publisher(Keys::data(&graph.name, &port.node, &port.port))
                        .wait()
                        .map_err(|e| anyhow!("publishing {port}: {e}"))?;
                    out.add_remote(Box::new(NetSink {
                        publisher,
                        compress: opts.compress,
                    }));
                } else {
                    let shm = shm.as_ref().expect("created above");
                    let publisher =
                        shm.publisher(&keys.shm_topic(&graph.name, &port.node, &port.port))?;
                    out.add_remote(Box::new(ShmSink(publisher)));
                }
            }
            // Replayed messages for this group's outputs.
            if let Some(session) = &opts.session {
                let outs = outputs.clone();
                let sub = session
                    .declare_subscriber(keys.inject("*", "*"))
                    .callback(move |sample| {
                        let key = sample.key_expr().as_str();
                        let mut parts = key.rsplit('/');
                        let (Some(port), Some(node)) = (parts.next(), parts.next()) else {
                            return;
                        };
                        let Some(out) = outs.get(&PortRef::new(node, port)) else {
                            return;
                        };
                        let r = DataFrame::decode(&sample.payload().to_bytes())
                            .and_then(|f| out.inject_encoded(f.seq, f.stamp_ns, &f.body));
                        if let Err(e) = r {
                            tracing::warn!("dropping replayed {node}.{port}: {e:#}");
                        }
                    })
                    .wait()
                    .map_err(|e| anyhow!("subscribing to replay: {e}"))?;
                subscribers.push(sub);
            }
        }

        // Turn built nodes into runners.
        let deadline_of = |id: &str| {
            realtime
                .as_ref()
                .and_then(|rt| rt.deadlines_us.get(id))
                .map(|&us| Duration::from_micros(us))
        };
        let mut nodes = Vec::new();
        let mut runners = Vec::new();
        let mut max_rate: Option<f64> = None;
        for (id, b) in built {
            let inst = &graph.nodes[&id];
            let desc = ruro_node::descriptor(&inst.kind).expect("built above");
            let rate = inst.rate.or(desc.default_rate);
            if desc.inputs.is_empty() && rate.is_none() {
                bail!("{id}: source node needs a rate");
            }
            if let Some(r) = rate {
                max_rate = Some(max_rate.map_or(r, |m: f64| m.max(r)));
            }
            let stats = Arc::new(NodeStats::default());
            let (params_tx, params_rx) = crossbeam_channel::unbounded();
            let muted = Arc::new(AtomicBool::new(opts.muted.contains(&id)));
            // The node that publishes simulated time is itself paced by the wall clock.
            let node_clock = if clock_port.as_ref().is_some_and(|cp| cp.node == id) {
                GraphClock::wall()
            } else {
                clock.clone()
            };
            let BuiltNode {
                node,
                waker,
                inputs,
                ..
            } = b;
            runners.push(Runner::new(RunnerParts {
                id: id.clone(),
                node,
                waker: waker.clone(),
                params: params_rx,
                stats: stats.clone(),
                period: rate.map(|r| Duration::from_secs_f64(1.0 / r)),
                deadline: deadline_of(&id),
                muted: muted.clone(),
                estop: estop.clone(),
                clock: node_clock,
            }));
            nodes.push(NodeHandle {
                kind: inst.kind.clone(),
                id,
                waker,
                params: params_tx,
                stats,
                inputs: inputs.into_iter().map(|(_, p)| p).collect(),
                window: Mutex::new(MetricsWindow::new()),
                muted,
            });
        }

        if let Some(rt) = &realtime {
            rt::apply_process(rt);
        }
        let rt_threads = realtime.clone().filter(rt::wants_realtime);
        match realtime.as_ref().filter(|rt| rt.deterministic) {
            Some(rt) => {
                let rate = rt
                    .rate
                    .or(max_rate)
                    .context("a deterministic group needs a `rate` (or a node with one)")?;
                if !(rate.is_finite() && rate > 0.0) {
                    bail!("deterministic rate {rate} Hz is invalid");
                }
                let index: HashMap<&str, usize> = ids
                    .iter()
                    .enumerate()
                    .map(|(i, id)| (id.as_str(), i))
                    .collect();
                let edges: Vec<(usize, usize)> = graph
                    .wires
                    .iter()
                    .filter_map(|w| {
                        Some((
                            *index.get(w.from.node.as_str())?,
                            *index.get(w.to.node.as_str())?,
                        ))
                    })
                    .collect();
                let order = step_order(ids.len(), &edges);
                let cycle = Cycle {
                    period: Duration::from_secs_f64(1.0 / rate),
                    overruns: Arc::new(AtomicU64::new(0)),
                    rt: rt_threads,
                };
                // A group that contains the time source cannot wait for it.
                let owns_clock = clock_port.as_ref().is_some_and(|cp| in_group(&cp.node));
                let cycle_clock = if owns_clock {
                    GraphClock::wall()
                } else {
                    clock.clone()
                };
                let (stop, clock) = (stop.clone(), cycle_clock);
                tracing::info!(
                    "group `{group}` runs deterministically at {rate} Hz: {}",
                    order
                        .iter()
                        .map(|&i| ids[i].as_str())
                        .collect::<Vec<_>>()
                        .join(" -> ")
                );
                threads.push(
                    std::thread::Builder::new()
                        .name(format!("ruro-cycle:{group}"))
                        .spawn(move || run_deterministic(runners, order, cycle, stop, clock))
                        .context("failed to spawn the cycle thread")?,
                );
            }
            None => {
                for r in runners {
                    let (stop, rt) = (stop.clone(), rt_threads.clone());
                    let name = format!("ruro:{}", r.id);
                    let id = r.id.clone();
                    threads.push(
                        std::thread::Builder::new()
                            .name(name)
                            .spawn(move || run_threaded(r, stop, rt))
                            .with_context(|| format!("{id}: failed to spawn thread"))?,
                    );
                }
            }
        }

        Ok(Self {
            group: group.to_owned(),
            graph_name: graph.name.clone(),
            nodes,
            outputs,
            stop,
            estop,
            threads: Mutex::new(threads),
            recorder: Mutex::new(None),
            clock,
            _shm: shm,
            _subscribers: subscribers,
        })
    }

    pub fn group(&self) -> &str {
        &self.group
    }

    pub fn graph_name(&self) -> &str {
        &self.graph_name
    }

    /// Output ports of the nodes in this group.
    pub fn outputs(&self) -> &HashMap<PortRef, Arc<dyn ErasedOutput>> {
        &self.outputs
    }

    pub fn node_ids(&self) -> impl Iterator<Item = &str> {
        self.nodes.iter().map(|n| n.id.as_str())
    }

    /// The clock nodes of this group follow.
    pub fn clock(&self) -> &GraphClock {
        &self.clock
    }

    /// Checks a parameter change against the node's descriptor and queues
    /// it; it is applied between steps.
    pub fn set_param(&self, node: &str, name: &str, value: Value) -> Result<()> {
        self.queue_param(node, name, value, None)
    }

    /// Like [`Executor::set_param`], and waits up to `timeout` for the node
    /// to apply the value, returning its verdict.
    pub fn apply_param(
        &self,
        node: &str,
        name: &str,
        value: Value,
        timeout: Duration,
    ) -> Result<()> {
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.queue_param(node, name, value, Some(tx))?;
        match rx.recv_timeout(timeout) {
            Ok(result) => result.map_err(|e| anyhow!("{node}: {e}")),
            Err(_) => bail!(
                "{node} did not apply `{name}` within {:.1} s; it will when it next steps",
                timeout.as_secs_f64()
            ),
        }
    }

    fn queue_param(
        &self,
        node: &str,
        name: &str,
        value: Value,
        reply: Option<Sender<std::result::Result<(), String>>>,
    ) -> Result<()> {
        let n = self
            .nodes
            .iter()
            .find(|n| n.id == node)
            .ok_or_else(|| anyhow!("no node `{node}` in this group"))?;
        if let Some(desc) = ruro_node::descriptor(&n.kind) {
            desc.param_or_suggest(name)
                .and_then(|p| p.check(&value))
                .map_err(|e| anyhow!("{node}: {e}"))?;
        }
        if n.stats.state() == ruro_proto::NodeState::Failed {
            bail!("{node} has failed and no longer takes parameters");
        }
        let change = ParamChange {
            name: name.to_owned(),
            value,
            reply,
        };
        if n.params.send(change).is_ok() {
            n.waker.notify_control();
        }
        Ok(())
    }

    /// Engages or releases the emergency stop for every node of the group.
    pub fn set_estop(&self, engaged: bool) {
        if self.estop.swap(engaged, Ordering::Relaxed) != engaged {
            for n in &self.nodes {
                n.waker.notify_control();
            }
        }
    }

    pub fn estopped(&self) -> bool {
        self.estop.load(Ordering::Relaxed)
    }

    /// Stops stepping `nodes` (their outputs are being replayed); every other
    /// node of the group steps normally.
    pub fn set_muted(&self, nodes: &[String]) {
        for n in &self.nodes {
            n.muted.store(nodes.contains(&n.id), Ordering::Relaxed);
        }
    }

    /// Publishes an encoded message on `port` as if its node had sent it.
    pub fn inject(&self, port: &PortRef, seq: u64, stamp_ns: u64, bytes: &[u8]) -> Result<()> {
        self.outputs
            .get(port)
            .with_context(|| format!("no output `{port}` in this group"))?
            .inject_encoded(seq, stamp_ns, bytes)
    }

    /// Records every output of the group into an MCAP file at `path`.
    pub fn start_recording(&self, path: &Path) -> Result<()> {
        let mut rec = self.recorder.lock();
        if let Some(r) = rec.take() {
            r.finish()?;
        }
        *rec = Some(Recorder::start(path, &self.outputs)?);
        Ok(())
    }

    /// Finishes the current recording, if any. Returns `(written, dropped)`.
    pub fn stop_recording(&self) -> Result<Option<(u64, u64)>> {
        self.recorder
            .lock()
            .take()
            .map(Recorder::finish)
            .transpose()
    }

    /// Metrics for the window since the previous call.
    pub fn metrics(&self) -> GroupMetrics {
        let nodes = self
            .nodes
            .iter()
            .map(|n| {
                let (mut sum, mut count, mut dropped) = (0, 0, 0);
                for i in &n.inputs {
                    let s = i.stats();
                    sum += s.age_ns_sum.load(Ordering::Relaxed);
                    count += s.age_count.load(Ordering::Relaxed);
                    dropped += s.dropped.load(Ordering::Relaxed);
                }
                n.window
                    .lock()
                    .sample(&n.id, &n.stats, (sum, count), dropped)
            })
            .collect();
        GroupMetrics {
            group: self.group.clone(),
            pid: std::process::id(),
            nodes,
        }
    }

    /// Stops all nodes and waits for their threads.
    pub fn stop(self) {
        drop(self);
    }

    /// Stops all nodes, waits for their threads and finishes any recording.
    /// Idempotent; also done on drop.
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Relaxed);
        for n in &self.nodes {
            n.waker.notify_control();
        }
        for t in self.threads.lock().drain(..) {
            let _ = t.join();
        }
        // Nodes are stopped: everything they sent is queued for the recorder.
        if let Err(e) = self.stop_recording() {
            tracing::error!("finishing the recording: {e:#}");
        }
    }
}

impl Drop for Executor {
    fn drop(&mut self) {
        self.shutdown();
    }
}
