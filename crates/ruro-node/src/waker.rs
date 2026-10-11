use std::time::Instant;

use parking_lot::{Condvar, Mutex};

/// Wakes a node thread when one of its inputs receives data.
///
/// Every notification bumps a generation counter, so a waiter only sleeps if
/// nothing arrived since it last looked. This lets a node consume inputs
/// selectively without spinning.
#[derive(Default)]
pub struct Waker {
    generation: Mutex<u64>,
    cv: Condvar,
}

impl Waker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn notify(&self) {
        *self.generation.lock() += 1;
        self.cv.notify_one();
    }

    pub fn generation(&self) -> u64 {
        *self.generation.lock()
    }

    /// Blocks until the generation differs from `seen` or `deadline` passes.
    /// Returns the current generation.
    pub fn wait(&self, seen: u64, deadline: Option<Instant>) -> u64 {
        let mut generation = self.generation.lock();
        while *generation == seen {
            match deadline {
                Some(d) => {
                    if self.cv.wait_until(&mut generation, d).timed_out() {
                        break;
                    }
                }
                None => self.cv.wait(&mut generation),
            }
        }
        *generation
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
}
