use std::sync::atomic::{AtomicI32, AtomicU8, AtomicU64, Ordering};
use std::time::Instant;

use parking_lot::Mutex;
use ruro_proto::{NodeMetrics, NodeState};

/// Most step durations kept per metrics window.
const WINDOW_CAP: usize = 8192;

/// Live counters for one node, written by its thread and read by the metrics loop.
pub struct NodeStats {
    state: AtomicU8,
    steps: AtomicU64,
    errors: AtomicU64,
    last_error: Mutex<Option<String>>,
    durations_us: Mutex<Vec<u32>>,
    tid: AtomicI32,
}

impl Default for NodeStats {
    fn default() -> Self {
        Self {
            state: AtomicU8::new(NodeState::Starting as u8),
            steps: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            last_error: Mutex::new(None),
            durations_us: Mutex::new(Vec::with_capacity(256)),
            tid: AtomicI32::new(0),
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

    pub fn record_step(&self, micros: u32) {
        self.steps.fetch_add(1, Ordering::Relaxed);
        let mut d = self.durations_us.lock();
        if d.len() < WINDOW_CAP {
            d.push(micros);
        } else {
            // Overwrite pseudo-randomly so long windows stay representative.
            let i =
                (self.steps.load(Ordering::Relaxed) as usize).wrapping_mul(2654435761) % WINDOW_CAP;
            d[i] = micros;
        }
    }

    pub fn record_error(&self, err: String) {
        self.errors.fetch_add(1, Ordering::Relaxed);
        *self.last_error.lock() = Some(err);
    }

    pub fn steps(&self) -> u64 {
        self.steps.load(Ordering::Relaxed)
    }
}

/// Turns cumulative counters into per-window rates.
pub struct MetricsWindow {
    at: Instant,
    steps: u64,
    cpu_ticks: u64,
    age_sum: u64,
    age_count: u64,
}

impl MetricsWindow {
    pub fn new() -> Self {
        Self {
            at: Instant::now(),
            steps: 0,
            cpu_ticks: 0,
            age_sum: 0,
            age_count: 0,
        }
    }

    /// Produces metrics for the window since the last call.
    pub fn sample(
        &mut self,
        id: &str,
        stats: &NodeStats,
        (age_sum, age_count): (u64, u64),
        dropped: u64,
    ) -> NodeMetrics {
        let now = Instant::now();
        let dt = (now - self.at).as_secs_f64().max(1e-3);
        let steps = stats.steps();
        let tid = stats.tid.load(Ordering::Relaxed);
        let cpu_ticks = if tid > 0 {
            thread_cpu_ticks(tid).unwrap_or(self.cpu_ticks)
        } else {
            0
        };

        let mut durations = std::mem::take(&mut *stats.durations_us.lock());
        durations.sort_unstable();
        let pct = |p: f64| -> f64 {
            if durations.is_empty() {
                0.0
            } else {
                durations[((durations.len() - 1) as f64 * p).round() as usize] as f64
            }
        };
        let (p50, p99) = (pct(0.5), pct(0.99));
        durations.clear();
        *stats.durations_us.lock() = durations;

        let ages = age_count.saturating_sub(self.age_count);
        let metrics = NodeMetrics {
            id: id.to_owned(),
            state: stats.state(),
            hz: steps.saturating_sub(self.steps) as f64 / dt,
            step_p50_us: p50,
            step_p99_us: p99,
            input_age_ms: (ages > 0)
                .then(|| age_sum.saturating_sub(self.age_sum) as f64 / ages as f64 / 1e6),
            cpu_pct: cpu_ticks.saturating_sub(self.cpu_ticks) as f64 / clock_ticks_per_sec() / dt
                * 100.0,
            errors: stats.errors.load(Ordering::Relaxed),
            last_error: stats.last_error.lock().clone(),
            dropped,
        };
        *self = Self {
            at: now,
            steps,
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
        let m = w.sample("n", &stats, (2_000_000, 2), 0);
        assert_eq!(m.step_p50_us, 51.0);
        assert_eq!(m.step_p99_us, 99.0);
        assert!(m.hz > 0.0);
        assert_eq!(m.input_age_ms, Some(1.0));
        let m2 = w.sample("n", &stats, (2_000_000, 2), 0);
        assert_eq!(m2.input_age_ms, None);
        assert_eq!(m2.step_p99_us, 0.0);
    }
}
