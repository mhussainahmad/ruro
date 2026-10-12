use std::sync::Arc;
use std::time::Instant;

use parking_lot::{Condvar, Mutex};

use crate::Waker;

/// Where a host's nodes get graph time from: the wall clock, or a simulated
/// clock driven by the graph's `std/Clock` output.
#[derive(Clone, Default)]
pub struct GraphClock {
    sim: Option<Arc<SimClock>>,
}

impl GraphClock {
    /// Graph time is wall time.
    pub fn wall() -> Self {
        Self { sim: None }
    }

    /// Graph time follows [`SimClock::set`].
    pub fn sim() -> Self {
        Self {
            sim: Some(Arc::new(SimClock::default())),
        }
    }

    pub fn is_sim(&self) -> bool {
        self.sim.is_some()
    }

    pub fn sim_clock(&self) -> Option<&Arc<SimClock>> {
        self.sim.as_ref()
    }

    /// Current graph time in nanoseconds. Wall time is nanoseconds since the
    /// Unix epoch; simulated time is whatever the simulator publishes (zero
    /// until its first message).
    pub fn now_ns(&self) -> u64 {
        match &self.sim {
            Some(s) => s.now().unwrap_or(0),
            None => ruro_msg::now_ns(),
        }
    }
}

/// Simulated time shared by every node of a host.
#[derive(Default)]
pub struct SimClock {
    time: Mutex<Option<u64>>,
    cv: Condvar,
    listeners: Mutex<Vec<Arc<Waker>>>,
}

impl SimClock {
    /// Advances (or rewinds) simulated time and wakes timer-driven nodes.
    pub fn set(&self, time_ns: u64) {
        *self.time.lock() = Some(time_ns);
        self.cv.notify_all();
        for w in self.listeners.lock().iter() {
            w.notify_control();
        }
    }

    /// Current simulated time, or `None` before the first clock message.
    pub fn now(&self) -> Option<u64> {
        *self.time.lock()
    }

    /// Wakes `waker` (as control work) whenever time changes.
    pub fn subscribe(&self, waker: Arc<Waker>) {
        self.listeners.lock().push(waker);
    }

    /// Blocks until simulated time reaches `target_ns` or `deadline` passes.
    /// Returns the current time.
    pub fn wait_until(&self, target_ns: u64, deadline: Instant) -> Option<u64> {
        let mut t = self.time.lock();
        while t.is_none_or(|now| now < target_ns) {
            if self.cv.wait_until(&mut t, deadline).timed_out() {
                break;
            }
        }
        *t
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn wall_clock_tracks_system_time() {
        let c = GraphClock::wall();
        assert!(!c.is_sim());
        let a = c.now_ns();
        assert!(a.abs_diff(ruro_msg::now_ns()) < 1_000_000_000);
    }

    #[test]
    fn sim_clock_follows_set_and_wakes_listeners() {
        let c = GraphClock::sim();
        assert_eq!(c.now_ns(), 0);
        let sim = c.sim_clock().unwrap().clone();
        let waker = Arc::new(Waker::new());
        sim.subscribe(waker.clone());
        let s2 = sim.clone();
        let t = std::thread::spawn(move || {
            s2.wait_until(5_000, Instant::now() + Duration::from_secs(5))
        });
        std::thread::sleep(Duration::from_millis(10));
        sim.set(1_000);
        sim.set(5_000);
        assert_eq!(t.join().unwrap(), Some(5_000));
        assert_eq!(c.now_ns(), 5_000);
        assert!(waker.take_control());
        // Rewinding is allowed (a simulator reset).
        sim.set(10);
        assert_eq!(c.now_ns(), 10);
    }
}
