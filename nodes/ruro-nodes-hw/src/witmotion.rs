//! WitMotion serial IMUs (WT901, WT61, HWT905, ...) using the standard
//! WitMotion protocol: 11-byte frames `55 TYPE D0..D7 SUM`, values as
//! little-endian `i16`, `SUM` the low byte of the sum of the first ten bytes.

use std::f64::consts::PI;
use std::time::Duration;

use ruro::prelude::*;

use crate::io::{SerialTransport, Transport};

pub const FRAME_LEN: usize = 11;
pub const STANDARD_GRAVITY: f64 = 9.80665;

pub mod kind {
    pub const TIME: u8 = 0x50;
    pub const ACCEL: u8 = 0x51;
    pub const GYRO: u8 = 0x52;
    pub const ANGLE: u8 = 0x53;
    pub const MAG: u8 = 0x54;
    pub const QUATERNION: u8 = 0x59;
}

/// One decoded frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Frame {
    /// m/s², and temperature in °C.
    Accel { xyz: [f64; 3], temperature: f64 },
    /// rad/s.
    Gyro { xyz: [f64; 3] },
    /// Roll, pitch, yaw in radians.
    Angle { rpy: [f64; 3] },
    /// Raw magnetometer counts.
    Mag { xyz: [i16; 3] },
    /// (w, x, y, z).
    Quaternion { wxyz: [f64; 4] },
    Other { kind: u8 },
}

fn words(d: &[u8]) -> [i16; 4] {
    std::array::from_fn(|i| i16::from_le_bytes([d[2 * i], d[2 * i + 1]]))
}

/// Decodes a complete, checksummed frame.
pub fn decode(frame: &[u8; FRAME_LEN]) -> Option<Frame> {
    let sum = frame[..10].iter().fold(0u8, |a, &b| a.wrapping_add(b));
    if frame[0] != 0x55 || sum != frame[10] {
        return None;
    }
    let w = words(&frame[2..10]);
    let s = |i: usize, full_scale: f64| w[i] as f64 / 32768.0 * full_scale;
    Some(match frame[1] {
        kind::ACCEL => Frame::Accel {
            xyz: [0, 1, 2].map(|i| s(i, 16.0 * STANDARD_GRAVITY)),
            temperature: w[3] as f64 / 100.0,
        },
        kind::GYRO => Frame::Gyro {
            xyz: [0, 1, 2].map(|i| s(i, 2000.0).to_radians()),
        },
        kind::ANGLE => Frame::Angle {
            rpy: [0, 1, 2].map(|i| s(i, PI)),
        },
        kind::MAG => Frame::Mag {
            xyz: [w[0], w[1], w[2]],
        },
        kind::QUATERNION => Frame::Quaternion {
            wxyz: [0, 1, 2, 3].map(|i| s(i, 1.0)),
        },
        other => Frame::Other { kind: other },
    })
}

/// Encodes a frame from raw words (for tests and simulators).
pub fn encode(kind: u8, words: [i16; 4]) -> [u8; FRAME_LEN] {
    let mut f = [0u8; FRAME_LEN];
    f[0] = 0x55;
    f[1] = kind;
    for (i, w) in words.iter().enumerate() {
        f[2 + 2 * i..4 + 2 * i].copy_from_slice(&w.to_le_bytes());
    }
    f[10] = f[..10].iter().fold(0u8, |a, &b| a.wrapping_add(b));
    f
}

/// Splits a byte stream into frames, resynchronising after corruption.
#[derive(Default)]
pub struct Decoder {
    buf: Vec<u8>,
    /// Frames dropped for a bad checksum.
    pub corrupt: u64,
}

impl Decoder {
    pub fn push(&mut self, bytes: &[u8], mut out: impl FnMut(Frame)) {
        self.buf.extend_from_slice(bytes);
        let mut i = 0;
        while i + FRAME_LEN <= self.buf.len() {
            if self.buf[i] != 0x55 {
                i += 1;
                continue;
            }
            let f: &[u8; FRAME_LEN] = self.buf[i..i + FRAME_LEN].try_into().unwrap();
            match decode(f) {
                Some(frame) => {
                    out(frame);
                    i += FRAME_LEN;
                }
                None => {
                    self.corrupt += 1;
                    i += 1;
                }
            }
        }
        self.buf.drain(..i);
    }
}

/// Quaternion (x, y, z, w) from roll, pitch, yaw (Z-Y-X intrinsic).
pub fn quat_from_rpy(roll: f64, pitch: f64, yaw: f64) -> Quat {
    let (sr, cr) = (roll / 2.0).sin_cos();
    let (sp, cp) = (pitch / 2.0).sin_cos();
    let (sy, cy) = (yaw / 2.0).sin_cos();
    Quat {
        w: cr * cp * cy + sr * sp * sy,
        x: sr * cp * cy - cr * sp * sy,
        y: cr * sp * cy + sr * cp * sy,
        z: cr * cp * sy - sr * sp * cy,
    }
}

/// Assembles `Imu` messages from the frame stream. A message is complete when
/// an angle or quaternion frame arrives (devices send acceleration and rates
/// first in each output cycle).
#[derive(Default)]
pub struct ImuAssembler {
    imu: Imu,
    have_quaternion: bool,
}

impl ImuAssembler {
    pub fn apply(&mut self, f: Frame) -> Option<Imu> {
        match f {
            Frame::Accel { xyz, .. } => {
                self.imu.linear_acceleration = Vec3 {
                    x: xyz[0],
                    y: xyz[1],
                    z: xyz[2],
                };
                None
            }
            Frame::Gyro { xyz } => {
                self.imu.angular_velocity = Vec3 {
                    x: xyz[0],
                    y: xyz[1],
                    z: xyz[2],
                };
                None
            }
            Frame::Quaternion { wxyz } => {
                self.have_quaternion = true;
                self.imu.orientation = Quat {
                    w: wxyz[0],
                    x: wxyz[1],
                    y: wxyz[2],
                    z: wxyz[3],
                };
                Some(self.imu.clone())
            }
            Frame::Angle { rpy } if !self.have_quaternion => {
                self.imu.orientation = quat_from_rpy(rpy[0], rpy[1], rpy[2]);
                Some(self.imu.clone())
            }
            _ => None,
        }
    }
}

/// WitMotion IMU (WT901 and compatible) on a serial port, standard protocol.
/// Publishes one `Imu` per output cycle of the device; orientation comes from
/// the quaternion frame when enabled on the device, otherwise from Euler angles.
#[derive(Node)]
#[node(kind = "hw/witmotion-imu", rate = 200)]
pub struct WitMotionImu {
    #[output]
    imu: Output<Imu>,
    /// Serial device.
    #[param(default = "/dev/ttyUSB0")]
    device: String,
    /// Baud rate (factory default is 9600 on most models, 115200 on some).
    #[param(default = 9600, min = 4800, max = 921600)]
    baud: u32,
    /// Frame id stamped on messages.
    #[param(default = "imu")]
    frame_id: String,
    port: Option<Box<dyn Transport>>,
    decoder: Decoder,
    assembler: ImuAssembler,
}

impl Node for WitMotionImu {
    fn start(&mut self, _ctx: &mut Ctx) -> Result {
        if self.port.is_none() {
            let t = SerialTransport::open(&self.device, self.baud)
                .with_context(|| format!("opening {}", self.device))?;
            self.port = Some(Box::new(t));
        }
        Ok(())
    }

    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        let port = self.port.as_mut().context("port not open")?;
        let mut buf = [0u8; 512];
        let n = port.recv(&mut buf, Duration::from_millis(2))?;
        let mut ready = Vec::new();
        let assembler = &mut self.assembler;
        self.decoder.push(&buf[..n], |f| ready.extend(assembler.apply(f)));
        // Publish only the newest sample of this step; older ones are stale.
        if let Some(mut imu) = ready.pop() {
            imu.header = Header::new(self.frame_id.clone(), ruro::msg::now_ns());
            // The device does not report covariances; mark them unknown.
            imu.orientation_covariance[0] = -1.0;
            imu.angular_velocity_covariance[0] = -1.0;
            imu.linear_acceleration_covariance[0] = -1.0;
            self.imu.send(imu)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_reference_frames() {
        // Device lying flat: az = +1 g (2048 counts at ±16 g), 25.00 °C.
        let f = encode(kind::ACCEL, [0, 0, 2048, 2500]);
        assert_eq!(f[10], (0x55u32 + 0x51 + 0x08 + 0xC4 + 0x09) as u8);
        let Some(Frame::Accel { xyz, temperature }) = decode(&f) else {
            panic!()
        };
        assert!((xyz[2] - STANDARD_GRAVITY).abs() < 1e-9);
        assert_eq!(temperature, 25.0);

        // 1000 °/s about z is 16384 counts.
        let Some(Frame::Gyro { xyz }) = decode(&encode(kind::GYRO, [0, 0, 16384, 0])) else {
            panic!()
        };
        assert!((xyz[2] - 1000f64.to_radians()).abs() < 1e-9);

        // Yaw of -90°.
        let Some(Frame::Angle { rpy }) = decode(&encode(kind::ANGLE, [0, 0, -16384, 0])) else {
            panic!()
        };
        assert!((rpy[2] + PI / 2.0).abs() < 1e-9);

        let mut bad = encode(kind::ANGLE, [1, 2, 3, 4]);
        bad[10] = bad[10].wrapping_add(1);
        assert_eq!(decode(&bad), None);
    }

    #[test]
    fn decoder_resyncs_and_assembles_imu() {
        let mut stream = vec![0x00, 0x55, 0x13]; // noise, including a fake header
        stream.extend(encode(kind::ACCEL, [0, 0, 2048, 0]));
        stream.extend(encode(kind::GYRO, [0, 0, 0, 0]));
        let mut corrupt = encode(kind::MAG, [1, 1, 1, 0]);
        corrupt[10] ^= 0xFF;
        stream.extend(corrupt);
        stream.extend(encode(kind::ANGLE, [0, 0, 16384, 0]));

        let mut d = Decoder::default();
        let mut a = ImuAssembler::default();
        let mut out = Vec::new();
        // Feed in awkward chunk sizes.
        for chunk in stream.chunks(7) {
            d.push(chunk, |f| out.extend(a.apply(f)));
        }
        assert_eq!(out.len(), 1);
        let imu = &out[0];
        assert!((imu.linear_acceleration.z - STANDARD_GRAVITY).abs() < 1e-9);
        // Yaw +90°: q = (0, 0, sin 45°, cos 45°).
        let q = imu.orientation;
        assert!((q.z - (PI / 4.0).sin()).abs() < 1e-9 && (q.w - (PI / 4.0).cos()).abs() < 1e-9);
        assert!(d.corrupt >= 1);
    }

    #[test]
    fn quaternion_frames_take_precedence() {
        let mut a = ImuAssembler::default();
        let q = a
            .apply(decode(&encode(kind::QUATERNION, [16384, 0, 0, 16384])).unwrap())
            .unwrap()
            .orientation;
        assert_eq!((q.w, q.x, q.y, q.z), (0.5, 0.0, 0.0, 0.5));
        // Angle frames no longer override the orientation.
        assert!(a.apply(decode(&encode(kind::ANGLE, [0, 0, 0, 0])).unwrap()).is_none());
    }

    #[test]
    fn rpy_quaternion_matches_axis_rotations() {
        let q = quat_from_rpy(PI / 2.0, 0.0, 0.0);
        assert!((q.x - (PI / 4.0).sin()).abs() < 1e-12 && q.y.abs() < 1e-12);
        let q = quat_from_rpy(0.0, PI / 2.0, 0.0);
        assert!((q.y - (PI / 4.0).sin()).abs() < 1e-12 && q.x.abs() < 1e-12);
    }
}
