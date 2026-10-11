use std::time::{Duration, Instant};

/// Per-step context handed to a node.
#[derive(Debug)]
pub struct Ctx {
    id: String,
    started: Instant,
    last_step: Option<Instant>,
    dt: Duration,
    steps: u64,
}

impl Ctx {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            started: Instant::now(),
            last_step: None,
            dt: Duration::ZERO,
            steps: 0,
        }
    }

    /// Instance id of this node in the graph.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Time since the previous step (zero on the first step).
    pub fn dt(&self) -> Duration {
        self.dt
    }

    /// Time since the node started.
    pub fn time(&self) -> Duration {
        self.started.elapsed()
    }

    /// Number of completed steps.
    pub fn steps(&self) -> u64 {
        self.steps
    }

    #[doc(hidden)]
    pub fn begin_step(&mut self, now: Instant) {
        self.dt = self.last_step.map_or(Duration::ZERO, |t| now - t);
        self.last_step = Some(now);
    }

    #[doc(hidden)]
    pub fn end_step(&mut self) {
        self.steps += 1;
    }
}
