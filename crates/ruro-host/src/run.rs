//! Stepping nodes: one [`Runner`] per node, driven either by its own thread
//! ([`run_threaded`]) or, for deterministic groups, by one thread that steps
//! every node of the group in a fixed order each cycle ([`run_deterministic`]).

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use parking_lot::Mutex;
use ruro_graph::Realtime;
use ruro_node::{Ctx, DynNode, GraphClock, Waker};
use ruro_proto::NodeState;
use serde_json::Value;

use crate::rt;
use crate::stats::{NodeStats, current_tid};

/// Longest a thread sleeps before re-checking for shutdown.
pub const IDLE_WAIT: Duration = Duration::from_millis(100);

/// Most missed timer ticks a node catches up on after simulated time jumps.
const MAX_CATCH_UP: u32 = 100;

/// A parameter change queued for a node, applied between its steps.
pub struct ParamChange {
    pub name: String,
    pub value: Value,
    /// Told whether the node accepted the value.
    pub reply: Option<Sender<Result<(), String>>>,
}

/// One node and everything needed to step it.
pub struct Runner {
    pub id: String,
    ctx: Ctx,
    node: Box<dyn DynNode>,
    pub waker: Arc<Waker>,
    params: Receiver<ParamChange>,
    /// Current parameter values, updated as changes are applied.
    values: Arc<Mutex<BTreeMap<String, Value>>>,
    pub stats: Arc<NodeStats>,
    /// Timer period; `None` for input-triggered nodes.
    pub period: Option<Duration>,
    deadline: Option<Duration>,
    muted: Arc<AtomicBool>,
    estop: Arc<AtomicBool>,
    estop_handled: bool,
    /// Clock the node is scheduled by (the graph clock, or the wall clock for
    /// the node that publishes simulated time).
    clock: GraphClock,
    failed: bool,
    /// Data generation consumed by the last step.
    seen: u64,
    last_logged: Option<Instant>,
}

/// Everything a [`Runner`] is built from.
pub struct RunnerParts {
    pub id: String,
    pub node: Box<dyn DynNode>,
    pub waker: Arc<Waker>,
    pub params: Receiver<ParamChange>,
    pub values: Arc<Mutex<BTreeMap<String, Value>>>,
    pub stats: Arc<NodeStats>,
    pub period: Option<Duration>,
    pub deadline: Option<Duration>,
    pub muted: Arc<AtomicBool>,
    pub estop: Arc<AtomicBool>,
    pub clock: GraphClock,
}

impl Runner {
    pub fn new(p: RunnerParts) -> Self {
        if p.period.is_some() || p.deadline.is_some() {
            p.stats.set_timed();
        }
        Self {
            ctx: Ctx::with_runtime(&p.id, p.clock.clone(), p.estop.clone()),
            clock: p.clock,
            id: p.id,
            node: p.node,
            waker: p.waker,
            params: p.params,
            values: p.values,
            stats: p.stats,
            period: p.period,
            deadline: p.deadline,
            muted: p.muted,
            estop: p.estop,
            estop_handled: false,
            failed: false,
            seen: 0,
            last_logged: None,
        }
    }

    pub fn failed(&self) -> bool {
        self.failed
    }

    /// Calls the node's `start`. Returns false if it failed.
    pub fn start(&mut self) -> bool {
        match catch_unwind(AssertUnwindSafe(|| self.node.start(&mut self.ctx))) {
            Ok(Ok(())) => {
                self.stats.set_state(NodeState::Running);
                true
            }
            Ok(Err(e)) => {
                self.fail(format!("start failed: {e:#}"));
                false
            }
            Err(p) => {
                self.fail(format!("panicked in start: {}", panic_message(&p)));
                false
            }
        }
    }

    /// Applies pending parameter changes and reacts to the e-stop.
    pub fn poll_control(&mut self) {
        while let Ok(change) = self.params.try_recv() {
            let (name, value) = (&change.name, &change.value);
            let result = self.node.set_param(name, value);
            match &result {
                Ok(()) => {
                    tracing::info!("{}.{name} = {value}", self.id);
                    self.values.lock().insert(name.clone(), value.clone());
                }
                Err(e) => tracing::warn!("cannot set {}.{name}: {e:#}", self.id),
            }
            if let Some(reply) = change.reply {
                let _ = reply.send(result.map_err(|e| format!("{e:#}")));
            }
        }
        let estopped = self.estop.load(Ordering::Relaxed);
        if estopped && !self.estop_handled && !self.failed {
            tracing::warn!("{}: emergency stop", self.id);
            if let Err(p) = catch_unwind(AssertUnwindSafe(|| self.node.on_estop(&mut self.ctx))) {
                self.fail(format!("panicked in on_estop: {}", panic_message(&p)));
            }
        }
        self.estop_handled = estopped;
    }

    /// True (once) if an input received data since the last call.
    pub fn take_data(&mut self) -> bool {
        let g = self.waker.generation();
        let fresh = g != self.seen;
        self.seen = g;
        fresh
    }

    /// Steps the node once. `scheduled` is when a timer step was due (for
    /// jitter), `graph_ns` its graph time on a simulated clock.
    pub fn step(&mut self, scheduled: Option<Instant>, graph_ns: Option<u64>) {
        if self.failed || self.muted.load(Ordering::Relaxed) {
            return;
        }
        let t0 = Instant::now();
        if let Some(s) = scheduled {
            let late = t0.saturating_duration_since(s).as_micros();
            self.stats.record_jitter(late.min(u32::MAX as u128) as u32);
        }
        self.ctx.begin_step_at(t0, graph_ns);
        let result = catch_unwind(AssertUnwindSafe(|| self.node.step(&mut self.ctx)));
        let took = t0.elapsed();
        self.stats
            .record_step(took.as_micros().min(u32::MAX as u128) as u32);
        self.ctx.end_step();
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => self.report(format!("{e:#}")),
            Err(p) => return self.fail(format!("panicked: {}", panic_message(&p))),
        }
        if let Some(deadline) = self.deadline
            && took > deadline
        {
            self.stats.record_deadline_miss();
            self.report(format!(
                "deadline missed: step took {} µs, deadline {} µs",
                took.as_micros(),
                deadline.as_micros()
            ));
        }
    }

    /// Records an error; logs at most once a second.
    fn report(&mut self, msg: String) {
        if self
            .last_logged
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(1))
        {
            tracing::warn!("step failed: {msg}");
            self.last_logged = Some(Instant::now());
        }
        self.stats.record_error(msg);
    }

    /// Marks the node failed; it no longer steps but siblings keep running.
    fn fail(&mut self, msg: String) {
        tracing::error!("{} {msg}", self.id);
        self.stats.record_error(msg);
        self.stats.set_state(NodeState::Failed);
        self.failed = true;
    }

    /// Calls the node's `stop`.
    pub fn stop(&mut self) {
        let _ = catch_unwind(AssertUnwindSafe(|| self.node.stop(&mut self.ctx)));
        if !self.failed {
            self.stats.set_state(NodeState::Stopped);
        }
    }
}

fn panic_message(p: &Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}

/// Runs one node on the calling thread until `stop` is set.
pub fn run_threaded(mut r: Runner, stop: Arc<AtomicBool>, rt: Option<Realtime>) {
    let clock = r.clock.clone();
    if let Some(rt) = &rt {
        rt::apply_thread(rt, &r.id);
    }
    r.stats.set_tid(current_tid());
    if r.period.is_some() {
        rt::precise_timers();
    }
    if r.start() {
        // Pick up state that predates the thread, such as a latched e-stop.
        r.poll_control();
        if let Some(sim) = clock.sim_clock() {
            sim.subscribe(r.waker.clone());
        }
        match (r.period, clock.is_sim()) {
            (Some(period), false) => wall_timer(&mut r, &stop, period),
            (Some(period), true) => sim_timer(&mut r, &stop, period, &clock),
            (None, _) => on_input(&mut r, &stop),
        }
    }
    // A failed node parks until shutdown so its thread is joined normally.
    while !stop.load(Ordering::Relaxed) {
        r.waker.wait_control(Some(Instant::now() + IDLE_WAIT));
        r.waker.take_control();
    }
    r.stop();
}

fn wall_timer(r: &mut Runner, stop: &AtomicBool, period: Duration) {
    let mut next = Instant::now();
    while !stop.load(Ordering::Relaxed) && !r.failed() {
        if r.waker.take_control() {
            r.poll_control();
        }
        let now = Instant::now();
        if now < next {
            r.waker.wait_control(Some(next.min(now + IDLE_WAIT)));
            continue;
        }
        let scheduled = next;
        next += period;
        if next < now {
            // Fell behind: skip missed ticks instead of bursting.
            next = now + period;
        }
        r.step(Some(scheduled), None);
    }
}

fn sim_timer(r: &mut Runner, stop: &AtomicBool, period: Duration, clock: &GraphClock) {
    let period = period.as_nanos() as u64;
    let sim = clock.sim_clock().expect("simulated clock");
    let mut next: Option<u64> = None;
    while !stop.load(Ordering::Relaxed) && !r.failed() {
        r.waker.wait_control(Some(Instant::now() + IDLE_WAIT));
        r.waker.take_control();
        r.poll_control();
        let Some(now) = sim.now() else { continue };
        let due = next.get_or_insert(now);
        if now.saturating_add(period) < *due {
            // The simulation was reset to an earlier time.
            *due = now;
        }
        let mut caught_up = 0;
        while now >= *due && caught_up < MAX_CATCH_UP {
            let t = *due;
            *due += period;
            caught_up += 1;
            r.step(None, Some(t));
        }
        if now >= *due {
            *due = now + period;
        }
    }
}

fn on_input(r: &mut Runner, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) && !r.failed() {
        r.waker.wait(r.seen, Some(Instant::now() + IDLE_WAIT));
        if r.waker.take_control() {
            r.poll_control();
        }
        if r.take_data() {
            r.step(None, None);
        }
    }
}

/// Order in which a deterministic group steps its nodes: producers before
/// consumers along in-group wires. Feedback loops are broken by declaration
/// order, so the result is always a permutation of `0..n`.
pub fn step_order(n: usize, edges: &[(usize, usize)]) -> Vec<usize> {
    let mut indegree = vec![0usize; n];
    for &(_, to) in edges {
        indegree[to] += 1;
    }
    let mut done = vec![false; n];
    let mut order = Vec::with_capacity(n);
    while order.len() < n {
        // The first ready node in declaration order, or, inside a cycle, the
        // first remaining one.
        let next = (0..n)
            .find(|&i| !done[i] && indegree[i] == 0)
            .or_else(|| (0..n).find(|&i| !done[i]))
            .expect("nodes remain");
        done[next] = true;
        order.push(next);
        for &(from, to) in edges {
            if from == next && !done[to] {
                indegree[to] = indegree[to].saturating_sub(1);
            }
        }
    }
    order
}

/// Settings of a deterministic group.
pub struct Cycle {
    pub period: Duration,
    pub overruns: Arc<AtomicU64>,
    pub rt: Option<Realtime>,
}

/// Runs every node on the calling thread: each cycle, in `order`, timer
/// nodes step on their share of cycles and input-triggered nodes step if
/// data arrived. In-group wires stay zero-copy queues, so data produced
/// earlier in a cycle is consumed later in the same cycle.
pub fn run_deterministic(
    mut runners: Vec<Runner>,
    order: Vec<usize>,
    cycle: Cycle,
    stop: Arc<AtomicBool>,
    clock: GraphClock,
) {
    if let Some(rt) = &cycle.rt {
        rt::apply_thread(rt, "deterministic group");
    }
    let tid = current_tid();
    rt::precise_timers();
    // A timer node with rate r steps every `round(cycle rate / r)` cycles.
    let mut every: Vec<u64> = Vec::with_capacity(runners.len());
    for r in &mut runners {
        r.stats.set_tid(tid);
        r.stats.set_overruns(cycle.overruns.clone());
        r.start();
        every.push(r.period.map_or(0, |p| {
            (p.as_secs_f64() / cycle.period.as_secs_f64())
                .round()
                .max(1.0) as u64
        }));
    }
    let period_ns = cycle.period.as_nanos() as u64;
    let sim = clock.sim_clock().cloned();
    let mut n: u64 = 0;
    let mut next = Instant::now();
    let mut next_graph: Option<u64> = None;

    while !stop.load(Ordering::Relaxed) {
        // Wait for the cycle to be due.
        let (scheduled, graph_ns) = match &sim {
            None => {
                let now = Instant::now();
                if now < next {
                    std::thread::sleep((next - now).min(IDLE_WAIT));
                    continue;
                }
                (Some(next), None)
            }
            Some(sim) => {
                let target = next_graph.unwrap_or(0);
                match sim.wait_until(target, Instant::now() + IDLE_WAIT) {
                    Some(now) if now >= target => {
                        let t = *next_graph.get_or_insert(now);
                        if now.saturating_add(period_ns) < t {
                            next_graph = Some(now);
                            continue;
                        }
                        (None, Some(t))
                    }
                    _ => continue,
                }
            }
        };

        for &i in &order {
            let r = &mut runners[i];
            r.waker.take_control();
            r.poll_control();
            let due = match every[i] {
                0 => r.take_data(),
                k => n.is_multiple_of(k),
            };
            if due {
                let timer = every[i] > 0;
                r.step(scheduled.filter(|_| timer), graph_ns);
            }
        }
        n += 1;

        match (&sim, scheduled) {
            (None, Some(s)) => {
                next = s + cycle.period;
                let now = Instant::now();
                if now > next {
                    // The cycle ran past the start of the next one.
                    cycle.overruns.fetch_add(1, Ordering::Relaxed);
                    let missed = (now - next).as_nanos() as u64 / period_ns;
                    next += cycle.period * (missed as u32 + 1);
                }
            }
            _ => {
                next_graph = graph_ns.map(|t| t + period_ns);
            }
        }
    }
    for r in &mut runners {
        r.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_follows_wires_and_breaks_cycles_by_declaration() {
        // c -> a -> b
        assert_eq!(step_order(3, &[(2, 0), (0, 1)]), [2, 0, 1]);
        // a <-> b feedback, c independent.
        assert_eq!(step_order(3, &[(0, 1), (1, 0)]), [2, 0, 1]);
        // A loop fed from outside: source first, then the loop.
        assert_eq!(step_order(3, &[(0, 1), (1, 2), (2, 1)]), [0, 1, 2]);
        assert_eq!(step_order(0, &[]), Vec::<usize>::new());
    }
}
