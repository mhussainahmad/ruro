//! A small decoder for the ROS 2 CDR encoding of the common message types.
//!
//! ROS 2 serializes messages as OMG CDR (XCDR1): a 4-byte encapsulation header
//! (`00 00` big endian or `00 01` little endian, then two option bytes),
//! followed by the fields in declaration order. Every primitive is aligned to
//! its own size, measured from the first byte after the header. Strings are a
//! `u32` length that counts a trailing NUL, then the bytes. Sequences are a
//! `u32` element count followed by the elements; fixed arrays have no count.
//!
//! Only the types [`decode`] lists are supported; this is not a general
//! ROS 2 bridge.

use anyhow::{Context, Result, bail, ensure};
use ruro_msg::{
    Encoding, Header, Image, Imu, LaserScan, Message, Odometry, PointCloud, Pose, Quat, Scalar,
    Transform, Transforms, Twist, Vec3,
};

/// Reads CDR primitives from a buffer.
pub struct CdrReader<'a> {
    /// Payload after the encapsulation header.
    buf: &'a [u8],
    pos: usize,
    little: bool,
}

impl<'a> CdrReader<'a> {
    /// Parses the encapsulation header of a serialized message.
    pub fn new(data: &'a [u8]) -> Result<Self> {
        ensure!(data.len() >= 4, "CDR payload shorter than its header");
        let little = match (data[0], data[1]) {
            (0, 0) => false,
            (0, 1) => true,
            (a, b) => bail!("unsupported CDR encapsulation {a:#04x}{b:02x}"),
        };
        Ok(Self {
            buf: &data[4..],
            pos: 0,
            little,
        })
    }

    fn align(&mut self, n: usize) {
        self.pos = self.pos.next_multiple_of(n);
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).context("length overflow")?;
        let bytes = self
            .buf
            .get(self.pos..end)
            .with_context(|| format!("CDR payload truncated at byte {}", self.pos))?;
        self.pos = end;
        Ok(bytes)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        self.align(N);
        let mut a: [u8; N] = self.take(N)?.try_into().expect("length checked");
        if !self.little {
            a.reverse();
        }
        Ok(a)
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub fn bool(&mut self) -> Result<bool> {
        Ok(self.u8()? != 0)
    }

    pub fn u32(&mut self) -> Result<u32> {
        self.array::<4>().map(u32::from_le_bytes)
    }

    pub fn i32(&mut self) -> Result<i32> {
        self.array::<4>().map(i32::from_le_bytes)
    }

    pub fn f32(&mut self) -> Result<f32> {
        self.array::<4>().map(f32::from_le_bytes)
    }

    pub fn f64(&mut self) -> Result<f64> {
        self.array::<8>().map(f64::from_le_bytes)
    }

    pub fn string(&mut self) -> Result<String> {
        let len = self.u32()? as usize;
        let bytes = self.take(len)?;
        // The length includes a NUL terminator; tolerate writers that omit it.
        let bytes = bytes.strip_suffix(&[0]).unwrap_or(bytes);
        Ok(String::from_utf8_lossy(bytes).into_owned())
    }

    /// Element count of a sequence, sanity-checked against the bytes left so
    /// a corrupt length cannot trigger a huge allocation.
    pub fn seq_len(&mut self, min_elem_size: usize) -> Result<usize> {
        let n = self.u32()? as usize;
        let left = self.buf.len().saturating_sub(self.pos);
        ensure!(
            n.saturating_mul(min_elem_size.max(1)) <= left,
            "sequence of {n} elements does not fit in {left} remaining bytes"
        );
        Ok(n)
    }

    pub fn bytes(&mut self) -> Result<&'a [u8]> {
        let n = self.seq_len(1)?;
        self.take(n)
    }

    pub fn f32_seq(&mut self) -> Result<Vec<f32>> {
        let n = self.seq_len(4)?;
        (0..n).map(|_| self.f32()).collect()
    }

    pub fn f64_array<const N: usize>(&mut self) -> Result<[f64; N]> {
        let mut out = [0.0; N];
        for v in &mut out {
            *v = self.f64()?;
        }
        Ok(out)
    }

    fn header(&mut self) -> Result<Header> {
        let sec = self.i32()?;
        let nanosec = self.u32()?;
        let frame_id = self.string()?;
        let stamp_ns = (sec.max(0) as u64) * 1_000_000_000 + nanosec as u64;
        Ok(Header { stamp_ns, frame_id })
    }

    fn vec3(&mut self) -> Result<Vec3> {
        Ok(Vec3 {
            x: self.f64()?,
            y: self.f64()?,
            z: self.f64()?,
        })
    }

    fn quat(&mut self) -> Result<Quat> {
        Ok(Quat {
            x: self.f64()?,
            y: self.f64()?,
            z: self.f64()?,
            w: self.f64()?,
        })
    }

    fn pose(&mut self) -> Result<Pose> {
        Ok(Pose {
            position: self.vec3()?,
            orientation: self.quat()?,
        })
    }

    fn twist(&mut self) -> Result<Twist> {
        Ok(Twist {
            linear: self.vec3()?,
            angular: self.vec3()?,
        })
    }
}

/// A ROS 2 message converted to its ruro equivalent.
#[derive(Debug, Clone, PartialEq)]
pub struct Converted {
    /// ruro message type, e.g. `std/Image`.
    pub ty: &'static str,
    /// ruro encoding of the message, as it would cross shared memory.
    pub data: Vec<u8>,
    /// Header stamp of the ROS message, if it has one.
    pub stamp_ns: Option<u64>,
}

impl Converted {
    fn new<M: Message>(msg: &M, stamp_ns: Option<u64>) -> Self {
        Self {
            ty: M::TYPE,
            data: msg.to_bytes(),
            stamp_ns,
        }
    }
}

/// ROS 2 types [`decode`] understands, with the ruro type each becomes.
pub const SUPPORTED: &[(&str, &str)] = &[
    ("sensor_msgs/msg/Image", Image::TYPE),
    ("sensor_msgs/msg/PointCloud2", PointCloud::TYPE),
    ("sensor_msgs/msg/LaserScan", LaserScan::TYPE),
    ("sensor_msgs/msg/Imu", Imu::TYPE),
    ("geometry_msgs/msg/Twist", Twist::TYPE),
    ("geometry_msgs/msg/TwistStamped", Twist::TYPE),
    ("geometry_msgs/msg/PoseStamped", Pose::TYPE),
    ("std_msgs/msg/Float64", Scalar::TYPE),
    ("nav_msgs/msg/Odometry", Odometry::TYPE),
    ("tf2_msgs/msg/TFMessage", Transforms::TYPE),
];

/// Normalizes `pkg/Type` to `pkg/msg/Type`, as ROS 1 style names appear in some bags.
pub fn normalize_type(ros_type: &str) -> String {
    match ros_type.split('/').collect::<Vec<_>>()[..] {
        [pkg, ty] => format!("{pkg}/msg/{ty}"),
        _ => ros_type.to_owned(),
    }
}

/// Converts one CDR-encoded ROS 2 message. Returns `Ok(None)` for types that
/// are not supported.
pub fn decode(ros_type: &str, data: &[u8]) -> Result<Option<Converted>> {
    let mut r = CdrReader::new(data)?;
    let out = match normalize_type(ros_type).as_str() {
        "sensor_msgs/msg/Image" => image(&mut r)?,
        "sensor_msgs/msg/PointCloud2" => point_cloud(&mut r)?,
        "sensor_msgs/msg/LaserScan" => {
            let header = r.header()?;
            let stamp = header.stamp_ns;
            let scan = LaserScan {
                header,
                angle_min: r.f32()?,
                angle_max: r.f32()?,
                angle_increment: r.f32()?,
                time_increment: r.f32()?,
                scan_time: r.f32()?,
                range_min: r.f32()?,
                range_max: r.f32()?,
                ranges: r.f32_seq()?,
                intensities: r.f32_seq()?,
            };
            Converted::new(&scan, Some(stamp))
        }
        "sensor_msgs/msg/Imu" => {
            let header = r.header()?;
            let stamp = header.stamp_ns;
            let imu = Imu {
                header,
                orientation: r.quat()?,
                orientation_covariance: r.f64_array()?,
                angular_velocity: r.vec3()?,
                angular_velocity_covariance: r.f64_array()?,
                linear_acceleration: r.vec3()?,
                linear_acceleration_covariance: r.f64_array()?,
            };
            Converted::new(&imu, Some(stamp))
        }
        "geometry_msgs/msg/Twist" => Converted::new(&r.twist()?, None),
        "geometry_msgs/msg/TwistStamped" => {
            let stamp = r.header()?.stamp_ns;
            Converted::new(&r.twist()?, Some(stamp))
        }
        "geometry_msgs/msg/PoseStamped" => {
            let stamp = r.header()?.stamp_ns;
            Converted::new(&r.pose()?, Some(stamp))
        }
        "std_msgs/msg/Float64" => Converted::new(&Scalar(r.f64()?), None),
        "nav_msgs/msg/Odometry" => {
            let header = r.header()?;
            let stamp = header.stamp_ns;
            let child_frame_id = r.string()?;
            let pose = r.pose()?;
            r.f64_array::<36>()?;
            let twist = r.twist()?;
            r.f64_array::<36>()?;
            let odom = Odometry {
                header,
                child_frame_id,
                pose,
                twist,
            };
            Converted::new(&odom, Some(stamp))
        }
        "tf2_msgs/msg/TFMessage" => {
            // Each TransformStamped is at least a header (12 bytes) and 7 doubles.
            let n = r.seq_len(12 + 56)?;
            let mut transforms = Vec::with_capacity(n);
            for _ in 0..n {
                let header = r.header()?;
                let child_frame_id = r.string()?;
                transforms.push(Transform {
                    header,
                    child_frame_id,
                    translation: r.vec3()?,
                    rotation: r.quat()?,
                });
            }
            let stamp = transforms.first().map(|t| t.header.stamp_ns);
            Converted::new(&Transforms(transforms), stamp)
        }
        _ => return Ok(None),
    };
    Ok(Some(out))
}

fn image(r: &mut CdrReader) -> Result<Converted> {
    let header = r.header()?;
    let height = r.u32()?;
    let width = r.u32()?;
    let encoding = r.string()?;
    let is_bigendian = r.u8()? != 0;
    let step = r.u32()? as usize;
    let data = r.bytes()?;
    let (out_enc, swap_rb) = match encoding.as_str() {
        "rgb8" | "8UC3" => (Encoding::Rgb8, false),
        "bgr8" => (Encoding::Rgb8, true),
        "rgba8" | "8UC4" => (Encoding::Rgba8, false),
        "bgra8" => (Encoding::Rgba8, true),
        "mono8" | "8UC1" => (Encoding::Mono8, false),
        "mono16" | "16UC1" => (Encoding::Mono16, false),
        other => bail!("unsupported image encoding `{other}`"),
    };
    let ch = out_enc.channels();
    let row = width as usize * out_enc.bytes_per_pixel();
    ensure!(
        step >= row,
        "image step {step} is smaller than a row ({row} bytes)"
    );
    let needed = step
        .checked_mul(height.saturating_sub(1) as usize)
        .and_then(|n| n.checked_add(row))
        .context("image size overflows")?;
    ensure!(
        height == 0 || data.len() >= needed,
        "image data has {} bytes, need {needed}",
        data.len()
    );
    let mut img = Image::new(width, height, out_enc);
    // Rows may be padded to `step` bytes; copy only the pixels.
    for (y, dst) in img.data.chunks_exact_mut(row.max(1)).enumerate() {
        dst.copy_from_slice(&data[y * step..y * step + row]);
        if swap_rb {
            dst.chunks_exact_mut(ch).for_each(|px| px.swap(0, 2));
        }
        if out_enc == Encoding::Mono16 && is_bigendian {
            dst.as_chunks_mut::<2>()
                .0
                .iter_mut()
                .for_each(|px| px.swap(0, 1));
        }
    }
    Ok(Converted::new(&img, Some(header.stamp_ns)))
}

/// `sensor_msgs/PointField` datatypes this decoder reads.
const FLOAT32: u8 = 7;
const FLOAT64: u8 = 8;

fn point_cloud(r: &mut CdrReader) -> Result<Converted> {
    let header = r.header()?;
    let height = r.u32()? as usize;
    let width = r.u32()? as usize;
    // PointField: name (string), offset u32, datatype u8, count u32.
    let n_fields = r.seq_len(13)?;
    let mut xyz: [Option<(usize, u8)>; 3] = [None; 3];
    for _ in 0..n_fields {
        let name = r.string()?;
        let offset = r.u32()? as usize;
        let datatype = r.u8()?;
        let _count = r.u32()?;
        let slot = match name.as_str() {
            "x" => 0,
            "y" => 1,
            "z" => 2,
            _ => continue,
        };
        xyz[slot] = Some((offset, datatype));
    }
    let big_endian = r.bool()?;
    let point_step = r.u32()? as usize;
    let row_step = r.u32()? as usize;
    let data = r.bytes()?;
    let _is_dense = r.bool()?;

    let fields: Vec<(usize, u8)> = xyz
        .iter()
        .map(|f| f.context("point cloud has no x, y and z fields"))
        .collect::<Result<_>>()?;
    for &(offset, datatype) in &fields {
        let size = match datatype {
            FLOAT32 => 4,
            FLOAT64 => 8,
            other => bail!("unsupported point field datatype {other}"),
        };
        ensure!(
            offset + size <= point_step,
            "point field at offset {offset} exceeds point step {point_step}"
        );
    }
    ensure!(
        width.saturating_mul(point_step) <= row_step || height <= 1,
        "row step {row_step} is smaller than a row of points"
    );
    let read = |p: &[u8], (offset, datatype): (usize, u8)| -> f32 {
        if datatype == FLOAT32 {
            let b: [u8; 4] = p[offset..offset + 4].try_into().unwrap();
            if big_endian {
                f32::from_be_bytes(b)
            } else {
                f32::from_le_bytes(b)
            }
        } else {
            let b: [u8; 8] = p[offset..offset + 8].try_into().unwrap();
            (if big_endian {
                f64::from_be_bytes(b)
            } else {
                f64::from_le_bytes(b)
            }) as f32
        }
    };
    let mut points = Vec::with_capacity(width.saturating_mul(height).min(data.len()));
    for row in 0..height {
        for col in 0..width {
            let start = row * row_step + col * point_step;
            let p = data
                .get(start..start + point_step)
                .context("point cloud data is shorter than width x height")?;
            points.push([read(p, fields[0]), read(p, fields[1]), read(p, fields[2])]);
        }
    }
    Ok(Converted::new(
        &PointCloud { points },
        Some(header.stamp_ns),
    ))
}

/// Builds CDR payloads. Used to make test fixtures.
#[cfg(test)]
pub(crate) mod writer {
    pub struct CdrWriter {
        pub buf: Vec<u8>,
    }

    impl CdrWriter {
        pub fn new() -> Self {
            Self {
                buf: vec![0, 1, 0, 0],
            }
        }

        fn align(&mut self, n: usize) {
            while !(self.buf.len() - 4).is_multiple_of(n) {
                self.buf.push(0);
            }
        }

        pub fn u8(&mut self, v: u8) -> &mut Self {
            self.buf.push(v);
            self
        }

        pub fn u32(&mut self, v: u32) -> &mut Self {
            self.align(4);
            self.buf.extend_from_slice(&v.to_le_bytes());
            self
        }

        pub fn i32(&mut self, v: i32) -> &mut Self {
            self.align(4);
            self.buf.extend_from_slice(&v.to_le_bytes());
            self
        }

        pub fn f32(&mut self, v: f32) -> &mut Self {
            self.align(4);
            self.buf.extend_from_slice(&v.to_le_bytes());
            self
        }

        pub fn f64(&mut self, v: f64) -> &mut Self {
            self.align(8);
            self.buf.extend_from_slice(&v.to_le_bytes());
            self
        }

        pub fn string(&mut self, s: &str) -> &mut Self {
            self.u32(s.len() as u32 + 1);
            self.buf.extend_from_slice(s.as_bytes());
            self.buf.push(0);
            self
        }

        pub fn bytes(&mut self, b: &[u8]) -> &mut Self {
            self.u32(b.len() as u32);
            self.buf.extend_from_slice(b);
            self
        }

        pub fn header(&mut self, sec: i32, nanosec: u32, frame: &str) -> &mut Self {
            self.i32(sec).u32(nanosec).string(frame)
        }

        pub fn f64s(&mut self, vs: &[f64]) -> &mut Self {
            vs.iter().for_each(|&v| {
                self.f64(v);
            });
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::writer::CdrWriter;
    use super::*;

    fn decode_as<M: Message>(ros_type: &str, w: &CdrWriter) -> (M, Option<u64>) {
        let c = decode(ros_type, &w.buf).unwrap().expect("supported type");
        assert_eq!(c.ty, M::TYPE);
        (M::decode(&c.data).unwrap(), c.stamp_ns)
    }

    #[test]
    fn float64_and_alignment_after_header() {
        // A double right after the encapsulation header is at offset 0: no padding.
        let mut w = CdrWriter::new();
        w.f64(2.5);
        assert_eq!(w.buf.len(), 12);
        let (s, stamp) = decode_as::<Scalar>("std_msgs/msg/Float64", &w);
        assert_eq!((s, stamp), (Scalar(2.5), None));
    }

    #[test]
    fn big_endian_payloads() {
        let mut buf = vec![0, 0, 0, 0];
        buf.extend_from_slice(&(-1.25f64).to_be_bytes());
        let c = decode("std_msgs/Float64", &buf).unwrap().unwrap();
        assert_eq!(Scalar::decode(&c.data).unwrap(), Scalar(-1.25));
    }

    #[test]
    fn image_with_padding_and_bgr() {
        // 2x2 bgr8 with 2 bytes of padding per row.
        let mut w = CdrWriter::new();
        w.header(3, 500, "cam")
            .u32(2)
            .u32(2)
            .string("bgr8")
            .u8(0)
            .u32(8);
        w.bytes(&[1, 2, 3, 4, 5, 6, 0, 0, 7, 8, 9, 10, 11, 12, 0, 0]);
        let (img, stamp) = decode_as::<Image>("sensor_msgs/msg/Image", &w);
        assert_eq!(stamp, Some(3_000_000_500));
        assert_eq!(
            (img.width, img.height, img.encoding),
            (2, 2, Encoding::Rgb8)
        );
        assert_eq!(img.data, [3, 2, 1, 6, 5, 4, 9, 8, 7, 12, 11, 10]);
    }

    #[test]
    fn image_16uc1_depth() {
        // 2x1 big-endian depth, 2 bytes of padding.
        let mut w = CdrWriter::new();
        w.header(0, 0, "depth")
            .u32(1)
            .u32(2)
            .string("16UC1")
            .u8(1)
            .u32(6);
        w.bytes(&[0x03, 0xE8, 0x00, 0x01, 0, 0]);
        let (img, _) = decode_as::<Image>("sensor_msgs/msg/Image", &w);
        assert_eq!(img.encoding, Encoding::Mono16);
        assert_eq!(img.data, [0xE8, 0x03, 0x01, 0x00]);
    }

    #[test]
    fn image_rejects_short_data_and_unknown_encodings() {
        let mut w = CdrWriter::new();
        w.header(0, 0, "")
            .u32(2)
            .u32(2)
            .string("mono8")
            .u8(0)
            .u32(2)
            .bytes(&[1, 2, 3]);
        assert!(decode("sensor_msgs/msg/Image", &w.buf).is_err());
        let mut w = CdrWriter::new();
        w.header(0, 0, "")
            .u32(1)
            .u32(1)
            .string("yuv422")
            .u8(0)
            .u32(2)
            .bytes(&[1, 2]);
        assert!(decode("sensor_msgs/msg/Image", &w.buf).is_err());
    }

    #[test]
    fn point_cloud2_xyz_float32() {
        let mut w = CdrWriter::new();
        w.header(1, 0, "lidar").u32(1).u32(2);
        w.u32(4);
        for (name, offset) in [("x", 0), ("y", 4), ("z", 8), ("intensity", 12)] {
            w.string(name).u32(offset).u8(FLOAT32).u32(1);
        }
        w.u8(0).u32(16).u32(32);
        let mut data = Vec::new();
        for p in [[1.0f32, 2.0, 3.0, 9.0], [-1.0, 0.5, 0.25, 9.0]] {
            p.iter()
                .for_each(|v| data.extend_from_slice(&v.to_le_bytes()));
        }
        w.bytes(&data).u8(1);
        let (pc, _) = decode_as::<PointCloud>("sensor_msgs/msg/PointCloud2", &w);
        assert_eq!(pc.points, [[1.0, 2.0, 3.0], [-1.0, 0.5, 0.25]]);
    }

    #[test]
    fn laser_scan() {
        let mut w = CdrWriter::new();
        w.header(0, 7, "laser");
        for v in [-1.5f32, 1.5, 0.5, 0.0, 0.1, 0.05, 10.0] {
            w.f32(v);
        }
        w.u32(3).f32(1.0).f32(2.0).f32(f32::INFINITY).u32(0);
        let (scan, stamp) = decode_as::<LaserScan>("sensor_msgs/msg/LaserScan", &w);
        assert_eq!(stamp, Some(7));
        assert_eq!(scan.header.frame_id, "laser");
        assert_eq!(scan.angle_increment, 0.5);
        assert_eq!(scan.range_max, 10.0);
        assert_eq!(scan.ranges, [1.0, 2.0, f32::INFINITY]);
        assert!(scan.intensities.is_empty());
    }

    #[test]
    fn imu_twist_pose_odometry() {
        let mut w = CdrWriter::new();
        // Frame id "imu" ends at offset 16; the quaternion is aligned to 8 -> 16.
        w.header(0, 0, "imu")
            .f64s(&[0.0, 0.0, 0.0, 1.0])
            .f64s(&[0.1; 9]);
        w.f64s(&[1.0, 2.0, 3.0])
            .f64s(&[0.0; 9])
            .f64s(&[0.0, 0.0, 9.81])
            .f64s(&[0.0; 9]);
        let (imu, _) = decode_as::<Imu>("sensor_msgs/msg/Imu", &w);
        assert_eq!(imu.orientation.w, 1.0);
        assert_eq!(imu.orientation_covariance, [0.1; 9]);
        assert_eq!(imu.angular_velocity.y, 2.0);
        assert_eq!(imu.linear_acceleration.z, 9.81);

        let mut w = CdrWriter::new();
        w.f64s(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.5]);
        let (t, _) = decode_as::<Twist>("geometry_msgs/msg/Twist", &w);
        assert_eq!((t.linear.x, t.angular.z), (1.0, 0.5));

        let mut w = CdrWriter::new();
        w.header(2, 0, "map")
            .f64s(&[1.0, 2.0, 3.0, 0.0, 0.0, 0.0, 1.0]);
        let (p, stamp) = decode_as::<Pose>("geometry_msgs/msg/PoseStamped", &w);
        assert_eq!(stamp, Some(2_000_000_000));
        assert_eq!(p.position.z, 3.0);

        let mut w = CdrWriter::new();
        w.header(0, 0, "odom").string("base_link");
        w.f64s(&[4.0, 5.0, 6.0, 0.0, 0.0, 0.0, 1.0])
            .f64s(&[0.0; 36]);
        w.f64s(&[0.5, 0.0, 0.0, 0.0, 0.0, 0.1]).f64s(&[0.0; 36]);
        let (o, _) = decode_as::<Odometry>("nav_msgs/msg/Odometry", &w);
        assert_eq!(o.child_frame_id, "base_link");
        assert_eq!(o.pose.position.x, 4.0);
        assert_eq!(o.twist.angular.z, 0.1);
    }

    #[test]
    fn tf_message() {
        let mut w = CdrWriter::new();
        w.u32(2);
        for (child, x) in [("base", 1.0), ("camera", 2.0)] {
            w.header(5, 0, "odom").string(child);
            w.f64s(&[x, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0]);
        }
        let (tf, stamp) = decode_as::<Transforms>("tf2_msgs/msg/TFMessage", &w);
        assert_eq!(stamp, Some(5_000_000_000));
        assert_eq!(tf.0.len(), 2);
        assert_eq!(tf.0[1].child_frame_id, "camera");
        assert_eq!(tf.0[1].translation.x, 2.0);
        assert_eq!(tf.0[0].header.frame_id, "odom");
    }

    #[test]
    fn unsupported_and_truncated() {
        let w = CdrWriter::new();
        assert!(decode("std_msgs/msg/String", &w.buf).unwrap().is_none());
        assert!(decode("std_msgs/msg/Float64", &w.buf).is_err());
        assert!(decode("std_msgs/msg/Float64", &[0, 1]).is_err());
        // A sequence length far beyond the payload must fail, not allocate.
        let mut w = CdrWriter::new();
        w.u32(u32::MAX);
        assert!(decode("tf2_msgs/msg/TFMessage", &w.buf).is_err());
    }
}
