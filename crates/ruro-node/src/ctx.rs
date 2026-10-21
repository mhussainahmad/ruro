use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::GraphClock;

/// Per-step context handed to a node.
pub struct Ctx {
    id: String,
    started: Instant,
    last_step: Option<Instant>,
    dt: Duration,
    steps: u64,
    clock: GraphClock,
    estop: Arc<AtomicBool>,
    /// Graph time of the first and of the current step (simulated clocks only).
    first_graph_ns: Option<u64>,
    last_graph_ns: Option<u64>,
}

impl std::fmt::Debug for Ctx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ctx")
            .field("id", &self.id)
            .field("steps", &self.steps)
            .field("dt", &self.dt)
            .field("sim", &self.clock.is_sim())
            .field("estopped", &self.estopped())
            .finish()
    }
}

impl Ctx {
    /// A context on the wall clock, never e-stopped.
    pub fn new(id: impl Into<String>) -> Self {
        Self::with_runtime(id, GraphClock::wall(), Arc::new(AtomicBool::new(false)))
    }

    /// A context following `clock` and the host's e-stop flag.
    #[doc(hidden)]
    pub fn with_runtime(id: impl Into<String>, clock: GraphClock, estop: Arc<AtomicBool>) -> Self {
        Self {
            id: id.into(),
            started: Instant::now(),
            last_step: None,
            dt: Duration::ZERO,
            steps: 0,
            clock,
            estop,
            first_graph_ns: None,
            last_graph_ns: None,
        }
    }

    /// Instance id of this node in the graph.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Graph time of the previous step to this one (zero on the first step).
    pub fn dt(&self) -> Duration {
        self.dt
    }

    /// Graph time elapsed since the node's first step (since it started, on
    /// the wall clock).
    pub fn time(&self) -> Duration {
        match (self.first_graph_ns, self.last_graph_ns) {
            (Some(first), Some(now)) if self.clock.is_sim() => {
                Duration::from_nanos(now.saturating_sub(first))
            }
            _ => self.started.elapsed(),
        }
    }

    /// Current graph time in nanoseconds: wall time (since the Unix epoch) by
    /// default, or simulated time when the graph sets `clock: sim`.
    pub fn now_ns(&self) -> u64 {
        self.clock.now_ns()
    }

    /// True when graph time is simulated (`clock: sim`).
    pub fn sim_time(&self) -> bool {
        self.clock.is_sim()
    }

    /// True while the emergency stop is engaged. Nodes that drive actuators
    /// must command a safe state (for example zero velocity) while it is set.
    pub fn estopped(&self) -> bool {
        self.estop.load(Ordering::Relaxed)
    }

    /// Number of completed steps.
    pub fn steps(&self) -> u64 {
        self.steps
    }

    #[doc(hidden)]
    pub fn begin_step(&mut self, now: Instant) {
        self.begin_step_at(now, None);
    }

    /// Starts a step. `graph_ns` is the scheduled graph time of the step on a
    /// simulated clock; `None` means "now".
    #[doc(hidden)]
    pub fn begin_step_at(&mut self, now: Instant, graph_ns: Option<u64>) {
        if self.clock.is_sim() {
            let t = graph_ns.unwrap_or_else(|| self.clock.now_ns());
            // A rewound clock (simulator reset) yields a zero step, never a negative one.
            self.dt = self.last_graph_ns.map_or(Duration::ZERO, |last| {
                Duration::from_nanos(t.saturating_sub(last))
            });
            self.first_graph_ns.get_or_insert(t);
            self.last_graph_ns = Some(t);
        } else {
            self.dt = self.last_step.map_or(Duration::ZERO, |t| now - t);
        }
        self.last_step = Some(now);
    }

    #[doc(hidden)]
    pub fn end_step(&mut self) {
        self.steps += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sim_dt_follows_graph_time() {
        let clock = GraphClock::sim();
        let sim = clock.sim_clock().unwrap().clone();
        let estop = Arc::new(AtomicBool::new(false));
        let mut ctx = Ctx::with_runtime("n", clock, estop.clone());
        sim.set(1_000_000_000);
        ctx.begin_step(Instant::now());
        assert_eq!(ctx.dt(), Duration::ZERO);
        ctx.begin_step_at(Instant::now(), Some(1_010_000_000));
        assert_eq!(ctx.dt(), Duration::from_millis(10));
        assert_eq!(ctx.time(), Duration::from_millis(10));
        sim.set(500);
        ctx.begin_step(Instant::now());
        assert_eq!(ctx.dt(), Duration::ZERO, "rewinds never go negative");
        assert_eq!(ctx.now_ns(), 500);
        assert!(!ctx.estopped());
        estop.store(true, Ordering::Relaxed);
        assert!(ctx.estopped());
    }
}
