//! Livox SDK2 point cloud packets (Mid-360, HAP) and the `hw/livox` node.
//!
//! The lidar streams UDP datagrams to the host (Mid-360 default: port 56301).
//! Each starts with a packed little-endian 36-byte header:
//!
//! ```text
//!  0 version u8        1 length u16       3 time_interval u16 (0.1 µs)
//!  5 dot_num u16       7 udp_cnt u16      9 frame_cnt u8
//! 10 data_type u8     11 time_type u8    12 reserved [u8; 12]
//! 24 crc32 u32        28 timestamp u64 (ns)
//! ```
//!
//! followed by `dot_num` points whose layout depends on `data_type`:
//! 1 = Cartesian high (i32 x, y, z in mm, reflectivity, tag: 14 bytes),
//! 2 = Cartesian low (i16 x, y, z in cm, reflectivity, tag: 8 bytes),
//! 3 = spherical (u32 depth mm, u16 zenith and u16 azimuth in 0.01°,
//! reflectivity, tag: 10 bytes).
//!
//! The lidar has to be configured to send to this host (Livox Viewer or the
//! SDK's JSON config); this node only receives.

use std::net::UdpSocket;

use ruro::prelude::*;

pub const HEADER_LEN: usize = 36;

pub mod data_type {
    pub const IMU: u8 = 0;
    pub const CARTESIAN_HIGH: u8 = 1;
    pub const CARTESIAN_LOW: u8 = 2;
    pub const SPHERICAL: u8 = 3;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketHeader {
    pub version: u8,
    pub length: u16,
    /// Time between first and last point, in units of 0.1 µs.
    pub time_interval: u16,
    pub dot_num: u16,
    pub udp_cnt: u16,
    pub frame_cnt: u8,
    pub data_type: u8,
    pub time_type: u8,
    pub crc32: u32,
    /// Time of the first point in nanoseconds.
    pub timestamp: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    /// Metres, in the lidar frame.
    pub xyz: [f32; 3],
    pub reflectivity: u8,
    pub tag: u8,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PacketError {
    TooShort,
    UnknownDataType(u8),
    LengthMismatch { expected: usize, got: usize },
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(b[i..i + 4].try_into().unwrap())
}

pub fn parse_header(b: &[u8]) -> Result<PacketHeader, PacketError> {
    if b.len() < HEADER_LEN {
        return Err(PacketError::TooShort);
    }
    Ok(PacketHeader {
        version: b[0],
        length: u16_at(b, 1),
        time_interval: u16_at(b, 3),
        dot_num: u16_at(b, 5),
        udp_cnt: u16_at(b, 7),
        frame_cnt: b[9],
        data_type: b[10],
        time_type: b[11],
        crc32: u32_at(b, 24),
        timestamp: u64::from_le_bytes(b[28..36].try_into().unwrap()),
    })
}

/// Size of one point for a data type.
pub fn point_size(data_type: u8) -> Option<usize> {
    match data_type {
        data_type::CARTESIAN_HIGH => Some(14),
        data_type::CARTESIAN_LOW => Some(8),
        data_type::SPHERICAL => Some(10),
        _ => None,
    }
}

/// Decodes the points of one packet, appending them to `out`.
pub fn parse_points(b: &[u8], out: &mut Vec<Point>) -> Result<PacketHeader, PacketError> {
    let h = parse_header(b)?;
    let size = point_size(h.data_type).ok_or(PacketError::UnknownDataType(h.data_type))?;
    let expected = HEADER_LEN + size * h.dot_num as usize;
    if b.len() < expected {
        return Err(PacketError::LengthMismatch {
            expected,
            got: b.len(),
        });
    }
    out.reserve(h.dot_num as usize);
    for p in b[HEADER_LEN..expected].chunks_exact(size) {
        let point = match h.data_type {
            data_type::CARTESIAN_HIGH => {
                let c = |i| i32::from_le_bytes(p[i..i + 4].try_into().unwrap()) as f32 / 1000.0;
                Point {
                    xyz: [c(0), c(4), c(8)],
                    reflectivity: p[12],
                    tag: p[13],
                }
            }
            data_type::CARTESIAN_LOW => {
                let c = |i| i16::from_le_bytes([p[i], p[i + 1]]) as f32 / 100.0;
                Point {
                    xyz: [c(0), c(2), c(4)],
                    reflectivity: p[6],
                    tag: p[7],
                }
            }
            _ => {
                let depth = u32_at(p, 0) as f32 / 1000.0;
                let zenith = (u16_at(p, 4) as f32 / 100.0).to_radians();
                let azimuth = (u16_at(p, 6) as f32 / 100.0).to_radians();
                Point {
                    xyz: [
                        depth * zenith.sin() * azimuth.cos(),
                        depth * zenith.sin() * azimuth.sin(),
                        depth * zenith.cos(),
                    ],
                    reflectivity: p[8],
                    tag: p[9],
                }
            }
        };
        out.push(point);
    }
    Ok(h)
}

/// Builds a packet (for tests and replay tools). `points` holds raw point bytes.
pub fn build_packet(
    data_type: u8,
    dot_num: u16,
    frame_cnt: u8,
    timestamp: u64,
    points: &[u8],
) -> Vec<u8> {
    let mut b = vec![0u8; HEADER_LEN];
    b[1..3].copy_from_slice(&((HEADER_LEN + points.len()) as u16).to_le_bytes());
    b[5..7].copy_from_slice(&dot_num.to_le_bytes());
    b[9] = frame_cnt;
    b[10] = data_type;
    b[28..36].copy_from_slice(&timestamp.to_le_bytes());
    b.extend_from_slice(points);
    b
}

/// Livox SDK2 lidar (Mid-360, HAP) point cloud receiver. Accumulates the
/// points of every packet received between steps and publishes them as one
/// cloud, so the node rate sets the integration time (10 Hz is one full
/// Mid-360 scan pattern period).
#[derive(Node)]
#[node(kind = "hw/livox", rate = 10)]
pub struct Livox {
    #[output]
    cloud: Output<PointCloud>,
    /// Local address to receive point data on (Mid-360 sends to port 56301).
    #[param(default = "0.0.0.0:56301")]
    bind: String,
    /// Only accept packets from this lidar IP (empty accepts any).
    #[param(default = "")]
    lidar_ip: String,
    /// Drop points closer than this many metres (the body of the robot).
    #[param(default = 0.1, min = 0, max = 10)]
    min_range: f64,
    socket: Option<UdpSocket>,
    points: Vec<Point>,
    buf: Vec<u8>,
}

impl Livox {
    /// Reads every pending datagram; returns the number of buffered points.
    fn receive(&mut self) -> Result<usize> {
        let socket = self.socket.as_ref().context("socket not open")?;
        socket.set_nonblocking(true)?;
        loop {
            match socket.recv_from(&mut self.buf) {
                Ok((n, from)) => {
                    if !self.lidar_ip.is_empty() && from.ip().to_string() != self.lidar_ip {
                        continue;
                    }
                    match parse_points(&self.buf[..n], &mut self.points) {
                        Ok(_) | Err(PacketError::UnknownDataType(data_type::IMU)) => {}
                        Err(e) => debug!("ignoring packet from {from}: {e:?}"),
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(self.points.len())
    }
}

impl Node for Livox {
    fn start(&mut self, _ctx: &mut Ctx) -> Result {
        let s = UdpSocket::bind(&self.bind).with_context(|| format!("binding {}", self.bind))?;
        self.socket = Some(s);
        self.buf = vec![0u8; 65536];
        Ok(())
    }

    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        self.receive()?;
        let min2 = (self.min_range * self.min_range) as f32;
        let points = self
            .points
            .drain(..)
            .filter(|p| p.xyz.iter().map(|c| c * c).sum::<f32>() >= min2)
            .map(|p| p.xyz)
            .collect();
        self.cloud.send(PointCloud { points })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn header_layout() {
        let mut b = build_packet(1, 2, 7, 123_456_789, &[0u8; 28]);
        b[3..5].copy_from_slice(&500u16.to_le_bytes());
        b[7..9].copy_from_slice(&42u16.to_le_bytes());
        b[11] = 1;
        b[24..28].copy_from_slice(&0xDEADBEEFu32.to_le_bytes());
        let h = parse_header(&b).unwrap();
        assert_eq!(
            h,
            PacketHeader {
                version: 0,
                length: 64,
                time_interval: 500,
                dot_num: 2,
                udp_cnt: 42,
                frame_cnt: 7,
                data_type: 1,
                time_type: 1,
                crc32: 0xDEADBEEF,
                timestamp: 123_456_789,
            }
        );
        assert_eq!(parse_header(&b[..35]), Err(PacketError::TooShort));
    }

    #[test]
    fn cartesian_high_points() {
        let mut pts = Vec::new();
        for (x, y, z, r) in [(1000i32, -2000i32, 500i32, 80u8), (-1, 0, 12345, 255)] {
            pts.extend(x.to_le_bytes());
            pts.extend(y.to_le_bytes());
            pts.extend(z.to_le_bytes());
            pts.extend([r, 0]);
        }
        let b = build_packet(data_type::CARTESIAN_HIGH, 2, 0, 0, &pts);
        let mut out = Vec::new();
        parse_points(&b, &mut out).unwrap();
        assert_eq!(out[0].xyz, [1.0, -2.0, 0.5]);
        assert_eq!(out[0].reflectivity, 80);
        assert_eq!(out[1].xyz, [-0.001, 0.0, 12.345]);
        // Truncated packet.
        assert!(matches!(
            parse_points(&b[..b.len() - 1], &mut out),
            Err(PacketError::LengthMismatch { .. })
        ));
    }

    #[test]
    fn cartesian_low_and_spherical_points() {
        let mut pts = Vec::new();
        for c in [150i16, -20, 3] {
            pts.extend(c.to_le_bytes());
        }
        pts.extend([10, 1]);
        let mut out = Vec::new();
        parse_points(
            &build_packet(data_type::CARTESIAN_LOW, 1, 0, 0, &pts),
            &mut out,
        )
        .unwrap();
        assert_eq!(out[0].xyz, [1.5, -0.2, 0.03]);
        assert_eq!((out[0].reflectivity, out[0].tag), (10, 1));

        // 2 m at zenith 90° (horizontal), azimuth 90° -> (0, 2, 0).
        let mut sp = 2000u32.to_le_bytes().to_vec();
        sp.extend(9000u16.to_le_bytes());
        sp.extend(9000u16.to_le_bytes());
        sp.extend([0, 0]);
        out.clear();
        parse_points(&build_packet(data_type::SPHERICAL, 1, 0, 0, &sp), &mut out).unwrap();
        let [x, y, z] = out[0].xyz;
        assert!(x.abs() < 1e-6 && (y - 2.0).abs() < 1e-6 && z.abs() < 1e-6);
    }

    #[test]
    fn receives_packets_over_udp() {
        let params = Default::default();
        let mut b = ruro::node::Builder::new("lidar", &params, Default::default());
        let mut node = <Livox as ruro::NodeSpec>::build(&mut b).unwrap();
        node.bind = "127.0.0.1:0".into();
        node.min_range = 0.0;
        let mut ctx = Ctx::new("lidar");
        node.start(&mut ctx).unwrap();
        let addr = node.socket.as_ref().unwrap().local_addr().unwrap();

        let mut pts = Vec::new();
        for c in [1000i32, 0, 0] {
            pts.extend(c.to_le_bytes());
        }
        pts.extend([1, 0]);
        let tx = UdpSocket::bind("127.0.0.1:0").unwrap();
        for _ in 0..3 {
            tx.send_to(&build_packet(1, 1, 0, 0, &pts), addr).unwrap();
        }
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(node.receive().unwrap(), 3);
        assert_eq!(node.points[2].xyz, [1.0, 0.0, 0.0]);
        node.step(&mut ctx).unwrap();
        assert_eq!(node.points.len(), 0, "points are drained into the cloud");
        assert_eq!(node.cloud.sent(), 1);
    }
}
