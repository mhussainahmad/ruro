//! Standard node library (`std/*`).
//!
//! Synthetic sources and small, fast building blocks. They need no hardware,
//! which makes them useful for trying ruro out, for tests and for benchmarks.

mod control;
mod image;
mod signal;

pub use control::{FakeArm, Pid};
pub use image::{Blur, FakeCamera, Grayscale, ImageStats};
pub use signal::{Gain, Sine};

/// Forces this crate to be linked. Hosts call it so the node registrations
/// survive dead-code elimination.
pub fn link() {}
