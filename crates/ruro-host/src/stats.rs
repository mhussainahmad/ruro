use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU64, Ordering};
use std::time::Instant;

use parking_lot::Mutex;
use ruro_proto::{NodeMetrics, NodeState, TimingMetrics};

/// Most samples kept per metrics window.
const WINDOW_CAP: usize = 8192;

/// A bounded set of samples for percentiles over one metrics window.
struct Window {
    samples: Mutex<Vec<u32>>,
    count: AtomicU64,
}

impl Window {
    fn new() -> Self {
        Self {
            samples: Mutex::new(Vec::with_capacity(256)),
            count: AtomicU64::new(0),
        }
    }

    fn record(&self, v: u32) {
        let n = self.count.fetch_add(1, Ordering::Relaxed);
        let mut d = self.samples.lock();
        if d.len() < WINDOW_CAP {
            d.push(v);
        } else {
            // Overwrite pseudo-randomly so long windows stay representative.
            d[(n as usize).wrapping_mul(2654435761) % WINDOW_CAP] = v;
        }
    }

    /// `(p50, p99)` of the window, which is then cleared (keeping its buffer).
    fn take_percentiles(&self) -> (f64, f64) {
        let mut d = std::mem::take(&mut *self.samples.lock());
        d.sort_unstable();
        let pct = |p: f64| -> f64 {
            if d.is_empty() {
                0.0
            } else {
                d[((d.len() - 1) as f64 * p).round() as usize] as f64
            }
        };
        let out = (pct(0.5), pct(0.99));
        d.clear();
        // Hand the buffer back unless samples arrived meanwhile.
        let mut slot = self.samples.lock();
        if slot.is_empty() {
            *slot = d;
        }
        out
    }
}

/// Live counters for one node, written by its thread and read by the metrics loop.
pub struct NodeStats {
    state: AtomicU8,
    steps: AtomicU64,
    errors: AtomicU64,
    last_error: Mutex<Option<String>>,
    durations_us: Window,
    tid: AtomicI32,
    /// Timer wakeup lateness, for timer-driven nodes.
    jitter_us: Window,
    jitter_max_us: AtomicU64,
    deadline_misses: AtomicU64,
    /// Whether the node reports [`TimingMetrics`] (it has a timer or a deadline).
    timed: AtomicBool,
    /// Cycle overruns of the node's deterministic group, shared by its nodes.
    overruns: Mutex<Option<Arc<AtomicU64>>>,
}

impl Default for NodeStats {
    fn default() -> Self {
        Self {
            state: AtomicU8::new(NodeState::Starting as u8),
            steps: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            last_error: Mutex::new(None),
            durations_us: Window::new(),
            tid: AtomicI32::new(0),
            jitter_us: Window::new(),
            jitter_max_us: AtomicU64::new(0),
            deadline_misses: AtomicU64::new(0),
            timed: AtomicBool::new(false),
            overruns: Mutex::new(None),
        }
    }
}

impl NodeStats {
    pub fn set_state(&self, s: NodeState) {
        self.state.store(s as u8, Ordering::Relaxed);
    }

    pub fn state(&self) -> NodeState {
        match self.state.load(Ordering::Relaxed) {
            0 => NodeState::Starting,
            1 => NodeState::Running,
            2 => NodeState::Failed,
            _ => NodeState::Stopped,
        }
    }

    pub fn set_tid(&self, tid: i32) {
        self.tid.store(tid, Ordering::Relaxed);
    }

    /// Makes the node report timing metrics.
    pub fn set_timed(&self) {
        self.timed.store(true, Ordering::Relaxed);
    }

    /// Shares the overrun counter of the node's deterministic group.
    pub fn set_overruns(&self, counter: Arc<AtomicU64>) {
        self.set_timed();
        *self.overruns.lock() = Some(counter);
    }

    pub fn record_step(&self, micros: u32) {
        self.steps.fetch_add(1, Ordering::Relaxed);
        self.durations_us.record(micros);
    }

    /// Records how late a timer wakeup was.
    pub fn record_jitter(&self, micros: u32) {
        self.jitter_us.record(micros);
        self.jitter_max_us
            .fetch_max(micros as u64, Ordering::Relaxed);
    }

    pub fn record_deadline_miss(&self) {
        self.deadline_misses.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_error(&self, err: String) {
        self.errors.fetch_add(1, Ordering::Relaxed);
        *self.last_error.lock() = Some(err);
    }

    pub fn steps(&self) -> u64 {
        self.steps.load(Ordering::Relaxed)
    }

    pub fn deadline_misses(&self) -> u64 {
        self.deadline_misses.load(Ordering::Relaxed)
    }
}

/// Turns cumulative counters into per-window rates.
pub struct MetricsWindow {
    at: Instant,
    steps: u64,
    /// Messages published per output, as of the last sample.
    delivered: Vec<u64>,
    cpu_ticks: u64,
    age_sum: u64,
    age_count: u64,
}

impl MetricsWindow {
    pub fn new() -> Self {
        Self {
            at: Instant::now(),
            steps: 0,
            delivered: Vec::new(),
            cpu_ticks: 0,
            age_sum: 0,
            age_count: 0,
        }
    }

    /// Produces metrics for the window since the last call. `delivered` is
    /// the number of messages published so far on each of the node's
    /// outputs (sent or replayed).
    pub fn sample(
        &mut self,
        id: &str,
        stats: &NodeStats,
        (age_sum, age_count): (u64, u64),
        dropped: u64,
        delivered: &[u64],
    ) -> NodeMetrics {
        let now = Instant::now();
        let dt = (now - self.at).as_secs_f64().max(1e-3);
        let steps = stats.steps();
        // Nodes that publish from callbacks (bridges) or whose outputs are
        // replayed step rarely or not at all: their busiest output shows
        // how active they are.
        let busiest = delivered
            .iter()
            .enumerate()
            .map(|(i, n)| n.saturating_sub(self.delivered.get(i).copied().unwrap_or(0)))
            .max()
            .unwrap_or(0);
        let tid = stats.tid.load(Ordering::Relaxed);
        let cpu_ticks = if tid > 0 {
            thread_cpu_ticks(tid).unwrap_or(self.cpu_ticks)
        } else {
            0
        };

        let (p50, p99) = stats.durations_us.take_percentiles();
        let (j50, j99) = stats.jitter_us.take_percentiles();
        let jmax = stats.jitter_max_us.swap(0, Ordering::Relaxed);
        let timing = stats.timed.load(Ordering::Relaxed).then(|| TimingMetrics {
            jitter_p50_us: j50,
            jitter_p99_us: j99,
            jitter_max_us: jmax as f64,
            deadline_misses: stats.deadline_misses(),
            overruns: stats
                .overruns
                .lock()
                .as_ref()
                .map_or(0, |c| c.load(Ordering::Relaxed)),
        });

        let ages = age_count.saturating_sub(self.age_count);
        let metrics = NodeMetrics {
            id: id.to_owned(),
            state: stats.state(),
            hz: steps.saturating_sub(self.steps).max(busiest) as f64 / dt,
            step_p50_us: p50,
            step_p99_us: p99,
            input_age_ms: (ages > 0)
                .then(|| age_sum.saturating_sub(self.age_sum) as f64 / ages as f64 / 1e6),
            cpu_pct: cpu_ticks.saturating_sub(self.cpu_ticks) as f64 / clock_ticks_per_sec() / dt
                * 100.0,
            errors: stats.errors.load(Ordering::Relaxed),
            last_error: stats.last_error.lock().clone(),
            dropped,
            timing,
        };
        *self = Self {
            at: now,
            steps,
            delivered: delivered.to_vec(),
            cpu_ticks,
            age_sum,
            age_count,
        };
        metrics
    }
}

fn clock_ticks_per_sec() -> f64 {
    // SAFETY: sysconf has no preconditions.
    let t = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if t > 0 { t as f64 } else { 100.0 }
}

/// utime + stime of a thread of this process, in clock ticks.
fn thread_cpu_ticks(tid: i32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/self/task/{tid}/stat")).ok()?;
    // The command name may contain spaces; fields resume after the last ')'.
    let rest = &stat[stat.rfind(')')? + 2..];
    let mut fields = rest.split_ascii_whitespace();
    // `rest` starts at field 3 (state); utime and stime are fields 14 and 15.
    let utime: u64 = fields.nth(11)?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    Some(utime + stime)
}

/// Kernel thread id of the calling thread.
pub fn current_tid() -> i32 {
    // SAFETY: gettid has no preconditions.
    unsafe { libc::gettid() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_own_thread_cpu() {
        let tid = current_tid();
        let start = thread_cpu_ticks(tid).expect("readable");
        let t = Instant::now();
        let mut x = 0u64;
        while t.elapsed().as_millis() < 50 {
            x = x.wrapping_add(std::hint::black_box(1));
        }
        assert!(thread_cpu_ticks(tid).unwrap() >= start);
        assert!(x > 0);
    }

    #[test]
    fn percentiles_and_rates() {
        let stats = NodeStats::default();
        let mut w = MetricsWindow::new();
        for us in 1..=100 {
            stats.record_step(us);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        let m = w.sample("n", &stats, (2_000_000, 2), 0, &[]);
        assert_eq!(m.step_p50_us, 51.0);
        assert_eq!(m.step_p99_us, 99.0);
        assert!(m.hz > 0.0);
        assert_eq!(m.input_age_ms, Some(1.0));
        assert_eq!(m.timing, None, "untimed nodes report no timing");
        let m2 = w.sample("n", &stats, (2_000_000, 2), 0, &[]);
        assert_eq!(m2.input_age_ms, None);
        assert_eq!(m2.step_p99_us, 0.0);
    }

    #[test]
    fn jitter_deadlines_and_overruns() {
        let stats = NodeStats::default();
        let overruns = Arc::new(AtomicU64::new(0));
        stats.set_overruns(overruns.clone());
        for us in [5, 10, 400, 7] {
            stats.record_jitter(us);
        }
        stats.record_deadline_miss();
        overruns.fetch_add(3, Ordering::Relaxed);
        let mut w = MetricsWindow::new();
        let t = w.sample("n", &stats, (0, 0), 0, &[]).timing.unwrap();
        assert_eq!(t.jitter_max_us, 400.0);
        assert_eq!(t.jitter_p50_us, 10.0);
        assert_eq!((t.deadline_misses, t.overruns), (1, 3));
        let t = w.sample("n", &stats, (0, 0), 0, &[]).timing.unwrap();
        assert_eq!((t.jitter_max_us, t.deadline_misses), (0.0, 1));
    }

    #[test]
    fn nodes_that_publish_more_than_they_step_report_the_message_rate() {
        let stats = NodeStats::default();
        let mut w = MetricsWindow::new();
        w.sample("n", &stats, (0, 0), 0, &[0, 0]);
        // One step, but 40 messages on the busier output, in ~0.1 s.
        stats.record_step(5);
        std::thread::sleep(std::time::Duration::from_millis(100));
        let m = w.sample("n", &stats, (0, 0), 0, &[3, 40]);
        assert!(m.hz > 200.0 && m.hz < 410.0, "{} Hz", m.hz);
        // Rates are per window: nothing new, nothing reported.
        let m = w.sample("n", &stats, (0, 0), 0, &[3, 40]);
        assert_eq!(m.hz, 0.0);
    }
}
