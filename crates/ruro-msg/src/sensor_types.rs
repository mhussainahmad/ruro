//! Sensor, navigation and input messages that carry a frame and timestamp.

use serde::{Deserialize, Serialize};

use crate::{Pose, Quat, Twist, Vec3, impl_postcard_message};

/// When and in which coordinate frame a measurement was taken.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Header {
    /// Measurement time in nanoseconds since the Unix epoch (or simulation epoch).
    pub stamp_ns: u64,
    /// Coordinate frame the data is expressed in, e.g. `base_link`.
    pub frame_id: String,
}

impl Header {
    pub fn new(frame_id: impl Into<String>, stamp_ns: u64) -> Self {
        Self {
            stamp_ns,
            frame_id: frame_id.into(),
        }
    }
}

/// A rigid transform from `header.frame_id` (parent) to `child_frame_id`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    pub header: Header,
    pub child_frame_id: String,
    pub translation: Vec3,
    pub rotation: Quat,
}
impl_postcard_message!(Transform, "std/Transform");

/// A batch of transforms, as published on a transform tree.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Transforms(pub Vec<Transform>);
impl_postcard_message!(Transforms, "std/Transforms");

/// Inertial measurement. Unknown fields are left at zero with a negative first
/// covariance entry, following the common convention.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Imu {
    pub header: Header,
    pub orientation: Quat,
    pub orientation_covariance: [f64; 9],
    /// rad/s
    pub angular_velocity: Vec3,
    pub angular_velocity_covariance: [f64; 9],
    /// m/s²
    pub linear_acceleration: Vec3,
    pub linear_acceleration_covariance: [f64; 9],
}
impl_postcard_message!(Imu, "std/Imu");

/// One sweep of a planar range finder.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LaserScan {
    pub header: Header,
    /// Angles in radians.
    pub angle_min: f32,
    pub angle_max: f32,
    pub angle_increment: f32,
    /// Seconds between measurements and between scans.
    pub time_increment: f32,
    pub scan_time: f32,
    /// Metres.
    pub range_min: f32,
    pub range_max: f32,
    pub ranges: Vec<f32>,
    pub intensities: Vec<f32>,
}
impl_postcard_message!(LaserScan, "std/LaserScan");

/// Estimated pose and velocity of a mobile base.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Odometry {
    pub header: Header,
    pub child_frame_id: String,
    pub pose: Pose,
    pub twist: Twist,
}
impl_postcard_message!(Odometry, "std/Odometry");

/// Pinhole camera calibration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CameraInfo {
    pub header: Header,
    pub width: u32,
    pub height: u32,
    /// Distortion model name, e.g. `plumb_bob`.
    pub distortion_model: String,
    pub d: Vec<f64>,
    /// Row-major 3x3 intrinsic matrix.
    pub k: [f64; 9],
    pub r: [f64; 9],
    /// Row-major 3x4 projection matrix.
    pub p: [f64; 12],
}
impl_postcard_message!(CameraInfo, "std/CameraInfo");

/// State of a joystick or gamepad.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Joy {
    pub header: Header,
    /// Normalised to [-1, 1].
    pub axes: Vec<f32>,
    pub buttons: Vec<i32>,
}
impl_postcard_message!(Joy, "std/Joy");

/// A boolean flag, e.g. an emergency stop.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Flag(pub bool);
impl_postcard_message!(Flag, "std/Flag");

/// Opaque bytes, e.g. raw serial frames.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Bytes(pub Vec<u8>);
impl_postcard_message!(Bytes, "std/Bytes");

/// Commanded joint targets. Vectors are index-aligned; empty means "not commanded".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct JointCommand {
    pub header: Header,
    pub position: Vec<f64>,
    pub velocity: Vec<f64>,
    pub effort: Vec<f64>,
}
impl_postcard_message!(JointCommand, "std/JointCommand");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Message;

    fn roundtrip<M: Message + PartialEq + std::fmt::Debug>(m: M) {
        assert_eq!(M::decode(&m.to_bytes()).unwrap(), m);
    }

    #[test]
    fn sensor_messages_roundtrip() {
        let h = Header::new("base_link", 42);
        roundtrip(Transform {
            header: h.clone(),
            child_frame_id: "camera".into(),
            ..Default::default()
        });
        roundtrip(Imu {
            header: h.clone(),
            angular_velocity: Vec3 {
                x: 0.1,
                y: 0.0,
                z: -0.2,
            },
            ..Default::default()
        });
        roundtrip(LaserScan {
            header: h.clone(),
            ranges: vec![1.0, 2.5, f32::INFINITY],
            ..Default::default()
        });
        roundtrip(Odometry {
            header: h.clone(),
            child_frame_id: "base".into(),
            ..Default::default()
        });
        roundtrip(CameraInfo {
            header: h.clone(),
            width: 640,
            height: 480,
            ..Default::default()
        });
        roundtrip(Joy {
            header: h.clone(),
            axes: vec![0.5],
            buttons: vec![1, 0],
        });
        roundtrip(JointCommand {
            header: h,
            position: vec![0.1, 0.2],
            ..Default::default()
        });
        roundtrip(Flag(true));
        roundtrip(Bytes(vec![1, 2, 3]));
    }
}
