//! Hardware driver nodes (`hw/*`).
//!
//! Every protocol is implemented in plain Rust on top of the [`io::Transport`]
//! trait or a thin syscall wrapper, so the byte-level logic is unit tested
//! against fixtures without hardware attached.

pub mod base64;
pub mod calibration;
pub mod can;
pub mod dynamixel;
pub mod feetech;
pub mod gamepad;
pub mod gpio;
pub mod i2c;
pub mod io;
pub mod jog;
pub mod livox;
pub mod mjpeg_http;
pub mod odrive;
pub mod pixel;
pub mod realsense;
pub mod rplidar;
pub mod rtsp;
pub mod serial;
pub mod v4l2;
pub mod witmotion;

pub use can::{Can, CanFrame};
pub use dynamixel::Dynamixel;
pub use feetech::So100Arm;
pub use gamepad::Gamepad;
pub use gpio::{GpioIn, GpioOut};
pub use i2c::I2c;
pub use jog::JointJog;
pub use livox::Livox;
pub use mjpeg_http::MjpegHttpCamera;
pub use odrive::ODrive;
#[cfg(feature = "realsense")]
pub use realsense::RealSense;
pub use rplidar::RpLidar;
pub use rtsp::RtspCamera;
pub use serial::Serial;
pub use v4l2::V4l2Camera;
pub use witmotion::WitMotionImu;

/// Forces this crate to be linked. Hosts call it so the node registrations
/// survive dead-code elimination.
pub fn link() {}
