//! Hardware driver nodes (`hw/*`).
//!
//! Every protocol is implemented in plain Rust on top of the [`io::Transport`]
//! trait or a thin syscall wrapper, so the byte-level logic is unit tested
//! against fixtures without hardware attached.

pub mod dynamixel;
pub mod feetech;
pub mod io;
pub mod odrive;
pub mod rplidar;
pub mod witmotion;

pub use dynamixel::Dynamixel;
pub use feetech::So100Arm;
pub use odrive::ODrive;
pub use rplidar::RpLidar;
pub use witmotion::WitMotionImu;

/// Forces this crate to be linked. Hosts call it so the node registrations
/// survive dead-code elimination.
pub fn link() {}
