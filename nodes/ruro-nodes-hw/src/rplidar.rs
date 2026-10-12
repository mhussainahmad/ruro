//! Slamtec RPLidar A1/A2 serial protocol, standard scan mode, and the
//! `hw/rplidar` node.
//!
//! Requests are `A5 CMD` or, for commands with a payload,
//! `A5 CMD SIZE PAYLOAD.. CHECKSUM` with `CHECKSUM = A5 ^ CMD ^ SIZE ^ PAYLOAD..`.
//! Responses start with a 7-byte descriptor `A5 5A LEN(30 bits)|MODE(2 bits) TYPE`.
//! Standard scan measurements are 5 bytes:
//!
//! ```text
//! byte 0: quality(6) | !S | S        S = first sample of a new revolution
//! byte 1: angle_q6[6:0] | C          C = check bit, always 1
//! byte 2: angle_q6[14:7]             angle in degrees = angle_q6 / 64 (clockwise)
//! byte 3-4: distance_q2 (LE)          distance in mm = distance_q2 / 4
//! ```

use std::f32::consts::TAU;
use std::time::{Duration, Instant};

use ruro::prelude::*;

use crate::io::{SerialTransport, Transport, recv_exact};

pub mod cmd {
    pub const STOP: u8 = 0x25;
    pub const RESET: u8 = 0x40;
    pub const SCAN: u8 = 0x20;
    pub const FORCE_SCAN: u8 = 0x21;
    pub const GET_INFO: u8 = 0x50;
    pub const GET_HEALTH: u8 = 0x52;
    pub const SET_MOTOR_PWM: u8 = 0xF0;
}

/// Motor PWM used by the vendor SDK for A2/A3.
pub const DEFAULT_MOTOR_PWM: u16 = 660;

/// Builds a request packet.
pub fn request(cmd: u8, payload: &[u8]) -> Vec<u8> {
    let mut p = vec![0xA5, cmd];
    if !payload.is_empty() {
        p.push(payload.len() as u8);
        p.extend_from_slice(payload);
        let cs = p.iter().fold(0u8, |a, &b| a ^ b);
        p.push(cs);
    }
    p
}

pub fn motor_pwm(pwm: u16) -> Vec<u8> {
    request(cmd::SET_MOTOR_PWM, &pwm.to_le_bytes())
}

/// A response descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Descriptor {
    pub len: u32,
    /// 0 = single response, 1 = multiple (streamed) responses.
    pub mode: u8,
    pub data_type: u8,
}

pub fn parse_descriptor(b: &[u8; 7]) -> Option<Descriptor> {
    if b[0] != 0xA5 || b[1] != 0x5A {
        return None;
    }
    let v = u32::from_le_bytes([b[2], b[3], b[4], b[5]]);
    Some(Descriptor {
        len: v & 0x3FFF_FFFF,
        mode: (v >> 30) as u8,
        data_type: b[6],
    })
}

pub const SCAN_DESCRIPTOR: Descriptor = Descriptor {
    len: 5,
    mode: 1,
    data_type: 0x81,
};

/// One standard-scan sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// First sample of a new revolution.
    pub start: bool,
    pub quality: u8,
    /// Degrees, clockwise as seen from above.
    pub angle_deg: f32,
    /// Millimetres; 0 means no return.
    pub distance_mm: f32,
}

/// Decodes a 5-byte measurement, checking the redundancy bits.
pub fn parse_sample(b: &[u8; 5]) -> Option<Sample> {
    let s = b[0] & 1;
    let not_s = (b[0] >> 1) & 1;
    if s == not_s || b[1] & 1 != 1 {
        return None;
    }
    let angle_q6 = u16::from_le_bytes([b[1], b[2]]) >> 1;
    let dist_q2 = u16::from_le_bytes([b[3], b[4]]);
    Some(Sample {
        start: s == 1,
        quality: b[0] >> 2,
        angle_deg: angle_q6 as f32 / 64.0,
        distance_mm: dist_q2 as f32 / 4.0,
    })
}

/// Encodes a measurement (for tests and simulators).
pub fn encode_sample(s: &Sample) -> [u8; 5] {
    let start = s.start as u8;
    let angle_q6 = (s.angle_deg * 64.0).round() as u16;
    let dist_q2 = (s.distance_mm * 4.0).round() as u16;
    let a = (angle_q6 << 1) | 1;
    let d = dist_q2.to_le_bytes();
    [
        (s.quality << 2) | ((1 - start) << 1) | start,
        a as u8,
        (a >> 8) as u8,
        d[0],
        d[1],
    ]
}

/// Splits the measurement stream into 5-byte samples, resynchronising on
/// check-bit failures by sliding one byte.
#[derive(Default)]
pub struct SampleStream {
    buf: Vec<u8>,
    pub rejected: u64,
}

impl SampleStream {
    pub fn push(&mut self, bytes: &[u8], mut out: impl FnMut(Sample)) {
        self.buf.extend_from_slice(bytes);
        let mut i = 0;
        while i + 5 <= self.buf.len() {
            match parse_sample(self.buf[i..i + 5].try_into().unwrap()) {
                Some(s) => {
                    out(s);
                    i += 5;
                }
                None => {
                    self.rejected += 1;
                    i += 1;
                }
            }
        }
        self.buf.drain(..i);
    }
}

/// Bins one revolution of samples into a `LaserScan`.
///
/// The output follows the usual right-handed convention: angles increase
/// counter-clockwise, `ranges[i]` is at `angle_min + i * angle_increment` with
/// `angle_min = -π`. Empty bins are `+inf`.
pub struct ScanBuilder {
    pub bins: usize,
    pub range_min: f32,
    pub range_max: f32,
    pub min_quality: u8,
    ranges: Vec<f32>,
    intensities: Vec<f32>,
    started: Option<Instant>,
    samples: usize,
}

impl ScanBuilder {
    pub fn new(bins: usize) -> Self {
        Self {
            bins,
            range_min: 0.15,
            range_max: 12.0,
            min_quality: 1,
            ranges: vec![f32::INFINITY; bins],
            intensities: vec![0.0; bins],
            started: None,
            samples: 0,
        }
    }

    /// Adds a sample; returns the finished previous revolution when `s.start` is set.
    pub fn push(&mut self, s: Sample, now: Instant, frame_id: &str) -> Option<LaserScan> {
        let finished = if s.start && self.samples > 0 {
            Some(self.finish(now, frame_id))
        } else {
            None
        };
        if s.start && self.started.is_none() || finished.is_some() {
            self.started = Some(now);
        }
        // Wait for the start of the first full revolution.
        self.started?;
        self.samples += 1;
        let r = s.distance_mm / 1000.0;
        if s.quality >= self.min_quality && r >= self.range_min && r <= self.range_max {
            // Clockwise degrees -> counter-clockwise radians in [-π, π).
            // Nearest bin, bin 0 at -π.
            let ccw = (-s.angle_deg.to_radians()).rem_euclid(TAU);
            let from_min = (ccw + TAU / 2.0).rem_euclid(TAU);
            let idx = (from_min / TAU * self.bins as f32).round() as usize % self.bins;
            if r < self.ranges[idx] {
                self.ranges[idx] = r;
                self.intensities[idx] = s.quality as f32;
            }
        }
        finished
    }

    fn finish(&mut self, now: Instant, frame_id: &str) -> LaserScan {
        let scan_time = self
            .started
            .map_or(0.0, |t| now.duration_since(t).as_secs_f32());
        let n = self.bins;
        let scan = LaserScan {
            header: Header::new(frame_id, ruro::msg::now_ns()),
            angle_min: -TAU / 2.0,
            angle_max: -TAU / 2.0 + TAU * (n - 1) as f32 / n as f32,
            angle_increment: TAU / n as f32,
            time_increment: scan_time / self.samples.max(1) as f32,
            scan_time,
            range_min: self.range_min,
            range_max: self.range_max,
            ranges: std::mem::replace(&mut self.ranges, vec![f32::INFINITY; n]),
            intensities: std::mem::replace(&mut self.intensities, vec![0.0; n]),
        };
        self.samples = 0;
        scan
    }
}

/// Slamtec RPLidar A1/A2/A3 in standard scan mode (up to ~8k samples/s).
/// Publishes one `LaserScan` per revolution.
#[derive(Node)]
#[node(kind = "hw/rplidar", rate = 200)]
pub struct RpLidar {
    #[output]
    scan: Output<LaserScan>,
    /// Serial device.
    #[param(default = "/dev/ttyUSB0")]
    device: String,
    /// 115200 for A1 and A2M8, 256000 for A2M12, A3 and S1.
    #[param(default = 115200, min = 9600, max = 1000000)]
    baud: u32,
    /// Angular bins per revolution.
    #[param(default = 360, min = 90, max = 8192)]
    bins: u32,
    /// Motor PWM for models with motor control (A2/A3); 0 uses DTR only (A1).
    #[param(default = 660, min = 0, max = 1023)]
    motor_pwm: u32,
    /// Discard samples below this quality (0..63).
    #[param(default = 1, min = 0, max = 63)]
    min_quality: u32,
    /// Maximum range in metres.
    #[param(default = 12.0, min = 0.5, max = 40)]
    range_max: f64,
    #[param(default = "laser")]
    frame_id: String,
    port: Option<Box<dyn Transport>>,
    stream: SampleStream,
    builder: Option<ScanBuilder>,
}

/// Starts the motor and a standard scan; checks the response descriptor.
pub fn start_scan(port: &mut dyn Transport, pwm: u16) -> Result {
    port.send(&request(cmd::STOP, &[]))?;
    std::thread::sleep(Duration::from_millis(2));
    port.clear_input()?;
    // A1: the motor runs while DTR is deasserted. A2/A3: PWM command.
    port.set_dtr(false)?;
    if pwm > 0 {
        port.send(&motor_pwm(pwm))?;
    }
    port.send(&request(cmd::SCAN, &[]))?;
    let mut d = [0u8; 7];
    recv_exact(port, &mut d, Duration::from_secs(2)).context("no scan response descriptor")?;
    let desc = parse_descriptor(&d).context("malformed response descriptor")?;
    ensure!(
        desc == SCAN_DESCRIPTOR,
        "unexpected scan descriptor {desc:?}"
    );
    Ok(())
}

impl Node for RpLidar {
    fn start(&mut self, _ctx: &mut Ctx) -> Result {
        if self.port.is_none() {
            let t = SerialTransport::open(&self.device, self.baud)
                .with_context(|| format!("opening {}", self.device))?;
            self.port = Some(Box::new(t));
        }
        let mut b = ScanBuilder::new(self.bins as usize);
        b.min_quality = self.min_quality as u8;
        b.range_max = self.range_max as f32;
        self.builder = Some(b);
        start_scan(self.port.as_deref_mut().unwrap(), self.motor_pwm as u16)
    }

    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        let port = self.port.as_mut().context("port not open")?;
        let builder = self.builder.as_mut().context("not started")?;
        let mut buf = [0u8; 1024];
        let n = port.recv(&mut buf, Duration::from_millis(2))?;
        let now = Instant::now();
        let mut done = Vec::new();
        let frame_id = &self.frame_id;
        self.stream
            .push(&buf[..n], |s| done.extend(builder.push(s, now, frame_id)));
        for scan in done {
            self.scan.send(scan)?;
        }
        Ok(())
    }

    fn stop(&mut self, _ctx: &mut Ctx) {
        if let Some(p) = self.port.as_mut() {
            let _ = p.send(&request(cmd::STOP, &[]));
            if self.motor_pwm > 0 {
                let _ = p.send(&motor_pwm(0));
            }
            let _ = p.set_dtr(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::MockTransport;

    #[test]
    fn requests_and_descriptors() {
        assert_eq!(request(cmd::SCAN, &[]), [0xA5, 0x20]);
        assert_eq!(request(cmd::STOP, &[]), [0xA5, 0x25]);
        // A5 ^ F0 ^ 02 ^ 94 ^ 02 = 0xC1
        assert_eq!(motor_pwm(660), [0xA5, 0xF0, 0x02, 0x94, 0x02, 0xC1]);
        // Legacy express scan request from the protocol document.
        assert_eq!(
            request(0x82, &[0, 0, 0, 0, 0]),
            [0xA5, 0x82, 0x05, 0, 0, 0, 0, 0, 0x22]
        );
        assert_eq!(
            parse_descriptor(&[0xA5, 0x5A, 0x05, 0x00, 0x00, 0x40, 0x81]),
            Some(SCAN_DESCRIPTOR)
        );
        assert_eq!(
            parse_descriptor(&[0xA5, 0x5A, 0x14, 0x00, 0x00, 0x00, 0x04]),
            Some(Descriptor {
                len: 20,
                mode: 0,
                data_type: 4
            })
        );
        assert_eq!(parse_descriptor(&[0xA5, 0x5B, 0, 0, 0, 0, 0]), None);
    }

    #[test]
    fn sample_bits() {
        // Start flag, quality 47, angle 90.0° (q6 5760), distance 1234.5 mm (q2 4938).
        let raw = [0xBD, 0x01, 0x2D, 0x4A, 0x13];
        let s = parse_sample(&raw).unwrap();
        assert!(s.start);
        assert_eq!(s.quality, 47);
        assert_eq!(s.angle_deg, 90.0);
        assert_eq!(s.distance_mm, 1234.5);
        assert_eq!(encode_sample(&s), raw);
        // S == !S and missing check bit are rejected.
        assert_eq!(parse_sample(&[0xBF, 0x01, 0x2D, 0, 0]), None);
        assert_eq!(parse_sample(&[0xBD, 0x00, 0x2D, 0, 0]), None);
    }

    fn revolution(start_deg: f32, n: usize, dist_mm: f32) -> Vec<u8> {
        (0..n)
            .flat_map(|i| {
                encode_sample(&Sample {
                    start: i == 0,
                    quality: 30,
                    angle_deg: (start_deg + i as f32 * 360.0 / n as f32) % 360.0,
                    distance_mm: dist_mm,
                })
            })
            .collect()
    }

    #[test]
    fn revolutions_become_scans() {
        let mut stream = SampleStream::default();
        let mut b = ScanBuilder::new(360);
        let mut bytes = vec![0x00, 0x13]; // garbage before the first sample
        bytes.extend(revolution(0.0, 720, 2000.0));
        bytes.extend(revolution(0.0, 720, 3000.0));
        bytes.extend(revolution(0.0, 10, 1000.0));
        let t0 = Instant::now();
        let mut scans = Vec::new();
        for chunk in bytes.chunks(37) {
            stream.push(chunk, |s| scans.extend(b.push(s, t0, "laser")));
        }
        assert_eq!(scans.len(), 2);
        assert!(scans[0].ranges.iter().all(|&r| r == 2.0));
        assert!(scans[1].ranges.iter().all(|&r| r == 3.0));
        assert_eq!(scans[0].ranges.len(), 360);
        assert!(stream.rejected >= 1);
    }

    #[test]
    fn clockwise_angles_map_to_counter_clockwise_bins() {
        let mut b = ScanBuilder::new(4); // bins at -180°, -90°, 0°, 90°
        let t = Instant::now();
        let mk = |start, angle_deg, distance_mm| Sample {
            start,
            quality: 10,
            angle_deg,
            distance_mm,
        };
        b.push(mk(true, 0.0, 1000.0), t, "l");
        b.push(mk(false, 90.0, 2000.0), t, "l"); // clockwise 90° = right = -90°
        b.push(mk(false, 180.0, 3000.0), t, "l");
        b.push(mk(false, 270.0, 4000.0), t, "l"); // left = +90°
        let scan = b.push(mk(true, 0.0, 1000.0), t, "l").unwrap();
        assert_eq!(scan.ranges, [3.0, 2.0, 1.0, 4.0]);
        assert_eq!(scan.angle_min, -std::f32::consts::PI);
        let pts: Vec<[f32; 3]> = scan.points().collect();
        // Bin 3 (+90°) at 4 m lies on +y.
        assert!((pts[3][1] - 4.0).abs() < 1e-5 && pts[3][0].abs() < 1e-5);
    }

    #[test]
    fn start_sequence() {
        let mut port = MockTransport::with_responder(|w| {
            if w == [0xA5, 0x20] {
                vec![0xA5, 0x5A, 0x05, 0x00, 0x00, 0x40, 0x81]
            } else {
                vec![]
            }
        });
        start_scan(&mut port, 660).unwrap();
        assert_eq!(
            port.sent,
            [0xA5, 0x25, 0xA5, 0xF0, 0x02, 0x94, 0x02, 0xC1, 0xA5, 0x20]
        );
        assert_eq!(port.dtr, Some(false));

        let mut silent = MockTransport::new();
        assert!(start_scan(&mut silent, 0).is_err());
    }
}
