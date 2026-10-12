use std::time::Instant;

use parking_lot::{Condvar, Mutex};

/// Wakes a node thread when one of its inputs receives data, or when the host
/// has control work for it (a parameter change, an e-stop, shutdown).
///
/// Data notifications bump a generation counter, so a waiter only sleeps if
/// nothing arrived since it last looked. This lets a node consume inputs
/// selectively without spinning. Control notifications set a separate flag:
/// they wake the thread without changing the generation, so an input-triggered
/// node applies a parameter change without running an extra step.
#[derive(Default)]
pub struct Waker {
    state: Mutex<State>,
    cv: Condvar,
}

#[derive(Default)]
struct State {
    generation: u64,
    control: bool,
}

impl Waker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Signals that an input received data.
    pub fn notify(&self) {
        self.state.lock().generation += 1;
        self.cv.notify_one();
    }

    /// Signals control work without counting as new data.
    pub fn notify_control(&self) {
        self.state.lock().control = true;
        self.cv.notify_one();
    }

    /// Number of data notifications so far.
    pub fn generation(&self) -> u64 {
        self.state.lock().generation
    }

    /// Clears and returns the pending control flag.
    pub fn take_control(&self) -> bool {
        std::mem::take(&mut self.state.lock().control)
    }

    /// Blocks until the generation differs from `seen`, control work is
    /// pending, or `deadline` passes. Returns the current generation.
    pub fn wait(&self, seen: u64, deadline: Option<Instant>) -> u64 {
        self.wait_for(Some(seen), deadline)
    }

    /// Like [`Waker::wait`], but data notifications do not wake the caller:
    /// only control work or the deadline do. Used by timer-driven nodes.
    pub fn wait_control(&self, deadline: Option<Instant>) -> u64 {
        self.wait_for(None, deadline)
    }

    fn wait_for(&self, seen: Option<u64>, deadline: Option<Instant>) -> u64 {
        let mut state = self.state.lock();
        while !state.control && seen.is_none_or(|s| state.generation == s) {
            match deadline {
                Some(d) => {
                    if self.cv.wait_until(&mut state, d).timed_out() {
                        break;
                    }
                }
                None => self.cv.wait(&mut state),
            }
        }
        state.generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn wait_returns_immediately_if_already_notified() {
        let w = Waker::new();
        w.notify();
        assert_eq!(w.wait(0, None), 1);
    }

    #[test]
    fn wait_times_out() {
        let w = Waker::new();
        let start = Instant::now();
        assert_eq!(w.wait(0, Some(start + Duration::from_millis(20))), 0);
        assert!(start.elapsed() >= Duration::from_millis(20));
    }

    #[test]
    fn wait_wakes_on_notify() {
        let w = Arc::new(Waker::new());
        let w2 = w.clone();
        let t = std::thread::spawn(move || w2.wait(0, None));
        std::thread::sleep(Duration::from_millis(10));
        w.notify();
        assert_eq!(t.join().unwrap(), 1);
    }

    #[test]
    fn control_wakes_without_bumping_the_generation() {
        let w = Arc::new(Waker::new());
        let w2 = w.clone();
        let t = std::thread::spawn(move || w2.wait(0, None));
        std::thread::sleep(Duration::from_millis(10));
        w.notify_control();
        assert_eq!(t.join().unwrap(), 0, "a control wakeup is not new data");
        assert!(w.take_control());
        assert!(!w.take_control(), "the flag is cleared once taken");
    }

    #[test]
    fn timer_waits_ignore_data() {
        let w = Waker::new();
        w.notify();
        let start = Instant::now();
        w.wait_control(Some(start + Duration::from_millis(20)));
        assert!(start.elapsed() >= Duration::from_millis(20));
        w.notify_control();
        w.wait_control(None);
    }
}
