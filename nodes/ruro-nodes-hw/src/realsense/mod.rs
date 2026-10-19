//! Intel RealSense depth cameras and the `hw/realsense` node (feature
//! `realsense`).
//!
//! `librealsense2` is loaded at run time through its C API ([`ffi`]), so the
//! crate builds without it. Everything between the library's frames and
//! ruro messages lives here as plain functions: intrinsics to `CameraInfo`
//! (the mapping realsense-ros uses), Z16 depth to millimetres, colour and
//! infrared rows to images, and deprojection to a point cloud (a port of
//! `rs2_deproject_pixel_to_point`). Constants and struct layouts follow
//! `librealsense2/h/rs_sensor.h`, `rs_types.h` and `rs_frame.h`.

#[cfg(feature = "realsense")]
pub mod ffi;
#[cfg(feature = "realsense")]
mod node;
#[cfg(feature = "realsense")]
pub use node::RealSense;

use std::ffi::c_int;

use ruro::prelude::*;

/// A frame copied out of librealsense2.
#[derive(Debug, Clone, Default)]
pub struct RawFrame {
    /// `rs2_stream` and stream index (1 and 2 for the infrared pair).
    pub stream: i32,
    pub index: i32,
    /// `rs2_format`.
    pub format: i32,
    /// Zero for motion frames.
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    /// Host time, if the camera clock is mapped to it.
    pub stamp_ns: Option<u64>,
    pub intrinsics: Option<Intrinsics>,
    pub data: Vec<u8>,
}

/// `rs2_stream` values.
pub mod stream {
    pub const ANY: i32 = 0;
    pub const DEPTH: i32 = 1;
    pub const COLOR: i32 = 2;
    pub const INFRARED: i32 = 3;
    pub const GYRO: i32 = 5;
    pub const ACCEL: i32 = 6;
}

/// `rs2_format` values.
pub mod format {
    pub const ANY: i32 = 0;
    pub const Z16: i32 = 1;
    pub const RGB8: i32 = 5;
    pub const Y8: i32 = 9;
    pub const MOTION_XYZ32F: i32 = 16;
}

/// `rs2_distortion`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Distortion {
    None,
    ModifiedBrownConrady,
    InverseBrownConrady,
    FTheta,
    BrownConrady,
    KannalaBrandt4,
    Unknown(i32),
}

impl Distortion {
    pub fn from_raw(v: i32) -> Self {
        match v {
            0 => Distortion::None,
            1 => Distortion::ModifiedBrownConrady,
            2 => Distortion::InverseBrownConrady,
            3 => Distortion::FTheta,
            4 => Distortion::BrownConrady,
            5 => Distortion::KannalaBrandt4,
            other => Distortion::Unknown(other),
        }
    }
}

/// `struct rs2_intrinsics` (the enum is an `int` in the C ABI).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RawIntrinsics {
    pub width: c_int,
    pub height: c_int,
    pub ppx: f32,
    pub ppy: f32,
    pub fx: f32,
    pub fy: f32,
    pub model: c_int,
    pub coeffs: [f32; 5],
}

/// Pinhole intrinsics and distortion of one stream.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Intrinsics {
    pub width: u32,
    pub height: u32,
    /// Principal point, pixels from the top-left corner.
    pub ppx: f32,
    pub ppy: f32,
    /// Focal lengths in pixels.
    pub fx: f32,
    pub fy: f32,
    pub model: Distortion,
    /// `[k1, k2, p1, p2, k3]` for Brown-Conrady, `[k1, k2, k3, k4, 0]` for
    /// Kannala-Brandt and F-Theta.
    pub coeffs: [f32; 5],
}

impl From<RawIntrinsics> for Intrinsics {
    fn from(r: RawIntrinsics) -> Self {
        Self {
            width: r.width.max(0) as u32,
            height: r.height.max(0) as u32,
            ppx: r.ppx,
            ppy: r.ppy,
            fx: r.fx,
            fy: r.fy,
            model: Distortion::from_raw(r.model),
            coeffs: r.coeffs,
        }
    }
}

impl Intrinsics {
    /// The calibration as realsense-ros publishes it: `equidistant` with
    /// four coefficients for Kannala-Brandt, otherwise `plumb_bob` with
    /// five; R is identity and P is `[K | 0]`.
    pub fn camera_info(&self, frame_id: &str, stamp_ns: u64) -> CameraInfo {
        let (fx, fy, cx, cy) = (
            self.fx as f64,
            self.fy as f64,
            self.ppx as f64,
            self.ppy as f64,
        );
        let (model, n) = match self.model {
            Distortion::KannalaBrandt4 => ("equidistant", 4),
            _ => ("plumb_bob", 5),
        };
        CameraInfo {
            header: Header::new(frame_id, stamp_ns),
            width: self.width,
            height: self.height,
            distortion_model: model.into(),
            d: self.coeffs[..n].iter().map(|&c| c as f64).collect(),
            k: [fx, 0.0, cx, 0.0, fy, cy, 0.0, 0.0, 1.0],
            r: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            p: [fx, 0.0, cx, 0.0, 0.0, fy, cy, 0.0, 0.0, 0.0, 1.0, 0.0],
        }
    }

    /// The 3D point (metres, camera optical frame: x right, y down, z
    /// forward) seen at pixel `(px, py)` at `depth` metres. Port of
    /// `rs2_deproject_pixel_to_point` (librealsense `src/rs.cpp`); like it,
    /// forward-distorted images (modified Brown-Conrady) are not undistorted.
    pub fn deproject(&self, px: f32, py: f32, depth: f32) -> [f32; 3] {
        let c = &self.coeffs;
        let mut x = (px - self.ppx) / self.fx;
        let mut y = (py - self.ppy) / self.fy;
        let (xo, yo) = (x, y);
        let zero = c.iter().all(|v| v.abs() < f32::EPSILON);
        if !zero {
            match self.model {
                Distortion::InverseBrownConrady => {
                    for _ in 0..10 {
                        let r2 = x * x + y * y;
                        let icdist = 1.0 / (1.0 + ((c[4] * r2 + c[1]) * r2 + c[0]) * r2);
                        let (xq, yq) = (x / icdist, y / icdist);
                        let dx = 2.0 * c[2] * xq * yq + c[3] * (r2 + 2.0 * xq * xq);
                        let dy = 2.0 * c[3] * xq * yq + c[2] * (r2 + 2.0 * yq * yq);
                        x = (xo - dx) * icdist;
                        y = (yo - dy) * icdist;
                    }
                }
                Distortion::BrownConrady => {
                    for _ in 0..10 {
                        let r2 = x * x + y * y;
                        let icdist = 1.0 / (1.0 + ((c[4] * r2 + c[1]) * r2 + c[0]) * r2);
                        let dx = 2.0 * c[2] * x * y + c[3] * (r2 + 2.0 * x * x);
                        let dy = 2.0 * c[3] * x * y + c[2] * (r2 + 2.0 * y * y);
                        x = (xo - dx) * icdist;
                        y = (yo - dy) * icdist;
                    }
                }
                _ => {}
            }
        }
        match self.model {
            Distortion::KannalaBrandt4 => {
                let rd = (x * x + y * y).sqrt().max(f32::EPSILON);
                let mut theta = rd;
                let mut theta2 = rd * rd;
                for _ in 0..4 {
                    let f = theta
                        * (1.0
                            + theta2 * (c[0] + theta2 * (c[1] + theta2 * (c[2] + theta2 * c[3]))))
                        - rd;
                    if f.abs() < f32::EPSILON {
                        break;
                    }
                    let df = 1.0
                        + theta2
                            * (3.0 * c[0]
                                + theta2
                                    * (5.0 * c[1] + theta2 * (7.0 * c[2] + 9.0 * theta2 * c[3])));
                    theta -= f / df;
                    theta2 = theta * theta;
                }
                let r = theta.tan();
                x *= r / rd;
                y *= r / rd;
            }
            Distortion::FTheta => {
                let rd = (x * x + y * y).sqrt().max(f32::EPSILON);
                let r = if c[0].abs() < f32::EPSILON {
                    0.0
                } else {
                    (c[0] * rd).tan() / (2.0 * (c[0] / 2.0).tan()).atan()
                };
                x *= r / rd;
                y *= r / rd;
            }
            _ => {}
        }
        [depth * x, depth * y, depth]
    }
}

/// Converts a Z16 frame (`depth_scale` metres per unit) to a 16-bit image in
/// millimetres; 0 stays "no measurement" and far values saturate.
pub fn depth_image(
    data: &[u8],
    width: u32,
    height: u32,
    stride: usize,
    depth_scale: f32,
) -> Result<Image> {
    let row = width as usize * 2;
    ensure!(
        stride >= row,
        "depth stride {stride} is less than a row ({row} bytes)"
    );
    ensure!(
        height == 0 || data.len() >= stride * (height as usize - 1) + row,
        "depth frame has {} bytes, need {}",
        data.len(),
        stride * height as usize
    );
    let mut img = Image::new(width, height, Encoding::Mono16);
    // The D400 series default is exactly 1 mm per unit.
    let unity = (depth_scale - 0.001).abs() < 1e-9;
    let scale = depth_scale * 1000.0;
    for (y, out) in img.data.chunks_exact_mut(row.max(1)).enumerate() {
        let src = &data[y * stride..y * stride + row];
        if unity {
            out.copy_from_slice(src);
        } else {
            for (o, s) in out
                .as_chunks_mut::<2>()
                .0
                .iter_mut()
                .zip(src.as_chunks::<2>().0)
            {
                let mm = (u16::from_le_bytes(*s) as f32 * scale)
                    .round()
                    .min(u16::MAX as f32);
                *o = (mm as u16).to_le_bytes();
            }
        }
    }
    Ok(img)
}

/// Copies an RGB8 or Y8 frame into an image, dropping row padding.
pub fn video_image(data: &[u8], width: u32, height: u32, stride: usize, fmt: i32) -> Result<Image> {
    let encoding = match fmt {
        format::RGB8 => Encoding::Rgb8,
        format::Y8 => Encoding::Mono8,
        other => bail!("unexpected RealSense video format {other}"),
    };
    let mut img = Image::new(width, height, encoding);
    let row = img.stride();
    ensure!(
        stride >= row,
        "stride {stride} is less than a row ({row} bytes)"
    );
    ensure!(
        height == 0 || data.len() >= stride * (height as usize - 1) + row,
        "frame has {} bytes, need {}",
        data.len(),
        stride * height as usize
    );
    for (y, out) in img.data.chunks_exact_mut(row.max(1)).enumerate() {
        out.copy_from_slice(&data[y * stride..y * stride + row]);
    }
    Ok(img)
}

/// Deprojects every `step`-th pixel of a millimetre depth image with the
/// intrinsics of the image it is registered to. Pixels without depth or
/// beyond `max_range` metres are skipped.
pub fn point_cloud(depth_mm: &Image, intr: &Intrinsics, step: usize, max_range: f32) -> PointCloud {
    let step = step.max(1);
    let (w, h) = (depth_mm.width as usize, depth_mm.height as usize);
    let mut points = Vec::with_capacity(w.div_ceil(step) * h.div_ceil(step));
    if depth_mm.encoding != Encoding::Mono16 {
        return PointCloud { points };
    }
    for v in (0..h).step_by(step) {
        let row = &depth_mm.data[v * w * 2..(v + 1) * w * 2];
        for u in (0..w).step_by(step) {
            let mm = u16::from_le_bytes([row[u * 2], row[u * 2 + 1]]);
            let z = mm as f32 / 1000.0;
            if mm == 0 || z > max_range {
                continue;
            }
            points.push(intr.deproject(u as f32, v as f32, z));
        }
    }
    PointCloud { points }
}

/// One IMU message from a gyro sample (rad/s) and the latest accelerometer
/// sample (m/s²), both in the IMU's optical frame. Orientation is unknown.
pub fn imu(gyro: [f32; 3], accel: [f32; 3], frame_id: &str, stamp_ns: u64) -> Imu {
    let v = |a: [f32; 3]| Vec3 {
        x: a[0] as f64,
        y: a[1] as f64,
        z: a[2] as f64,
    };
    let mut m = Imu {
        header: Header::new(frame_id, stamp_ns),
        angular_velocity: v(gyro),
        linear_acceleration: v(accel),
        ..Default::default()
    };
    m.orientation_covariance[0] = -1.0;
    m
}

/// Messages from one batch of frames; the newest frame of each stream wins.
#[derive(Debug, Default)]
pub struct Messages {
    pub color: Option<(Image, CameraInfo)>,
    pub depth: Option<(Image, CameraInfo)>,
    pub infrared: Option<(Image, CameraInfo)>,
    pub points: Option<PointCloud>,
    pub imu: Vec<Imu>,
}

/// Turns copied frames into messages, with realsense-ros frame names
/// (`<prefix>_color_optical_frame`, `<prefix>_depth_optical_frame`,
/// `<prefix>_infra1_optical_frame`, `<prefix>_imu_optical_frame`).
pub struct Converter {
    /// Metres per depth unit, from the depth sensor.
    pub depth_scale: f32,
    pub prefix: String,
    /// Depth is aligned to colour: it is published in the colour frame with
    /// the colour intrinsics, and deprojected with them.
    pub aligned: bool,
    /// Deproject every n-th pixel (in both directions); 0 for no cloud.
    pub point_step: usize,
    /// Points farther than this (metres) are dropped.
    pub max_range: f32,
    accel: Option<[f32; 3]>,
    color_intrinsics: Option<Intrinsics>,
}

fn motion(f: &RawFrame) -> Option<[f32; 3]> {
    let d = f.data.get(..12)?;
    Some(std::array::from_fn(|i| {
        f32::from_le_bytes(d[i * 4..i * 4 + 4].try_into().unwrap())
    }))
}

impl Converter {
    pub fn new(
        depth_scale: f32,
        prefix: &str,
        aligned: bool,
        point_step: usize,
        max_range: f32,
    ) -> Self {
        Self {
            depth_scale,
            prefix: prefix.to_owned(),
            aligned,
            point_step,
            max_range,
            accel: None,
            color_intrinsics: None,
        }
    }

    fn info(&self, f: &RawFrame, intr: Option<&Intrinsics>, frame: &str, stamp: u64) -> CameraInfo {
        let id = format!("{}_{frame}_optical_frame", self.prefix);
        match intr {
            Some(i) => i.camera_info(&id, stamp),
            None => CameraInfo {
                header: Header::new(id, stamp),
                width: f.width,
                height: f.height,
                ..Default::default()
            },
        }
    }

    pub fn convert(&mut self, frames: &[RawFrame], now_ns: u64) -> Result<Messages> {
        let mut m = Messages::default();
        let stamp = |f: &RawFrame| f.stamp_ns.unwrap_or(now_ns);
        // Colour first: aligned depth borrows its intrinsics.
        for f in frames.iter().filter(|f| f.stream == stream::COLOR) {
            let img = video_image(&f.data, f.width, f.height, f.stride, f.format)?;
            if f.intrinsics.is_some() {
                self.color_intrinsics = f.intrinsics;
            }
            m.color = Some((img, self.info(f, f.intrinsics.as_ref(), "color", stamp(f))));
        }
        for f in frames.iter().filter(|f| f.stream == stream::INFRARED) {
            let img = video_image(&f.data, f.width, f.height, f.stride, f.format)?;
            let name = format!("infra{}", f.index.max(1));
            m.infrared = Some((img, self.info(f, f.intrinsics.as_ref(), &name, stamp(f))));
        }
        for f in frames.iter().filter(|f| f.stream == stream::DEPTH) {
            ensure!(
                f.format == format::Z16,
                "unexpected depth format {}",
                f.format
            );
            let img = depth_image(&f.data, f.width, f.height, f.stride, self.depth_scale)?;
            let (intr, name) = match (self.aligned, self.color_intrinsics) {
                (true, Some(c)) if (c.width, c.height) == (f.width, f.height) => (Some(c), "color"),
                _ => (f.intrinsics, "depth"),
            };
            if self.point_step > 0
                && let Some(i) = &intr
            {
                m.points = Some(point_cloud(&img, i, self.point_step, self.max_range));
            }
            m.depth = Some((img, self.info(f, intr.as_ref(), name, stamp(f))));
        }
        let imu_frame = format!("{}_imu_optical_frame", self.prefix);
        for f in frames {
            match f.stream {
                stream::ACCEL => self.accel = motion(f).or(self.accel),
                // Gyro samples before the first accelerometer sample are dropped.
                stream::GYRO => {
                    if let (Some(g), Some(a)) = (motion(f), self.accel) {
                        m.imu.push(imu(g, a, &imu_frame, stamp(f)));
                    }
                }
                _ => {}
            }
        }
        Ok(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A D435-like colour stream: 640x480, inverse Brown-Conrady, zero coefficients.
    fn d435_color() -> Intrinsics {
        RawIntrinsics {
            width: 640,
            height: 480,
            ppx: 321.5,
            ppy: 245.25,
            fx: 615.0,
            fy: 615.5,
            model: 2,
            coeffs: [0.0; 5],
        }
        .into()
    }

    #[test]
    fn intrinsics_layout_matches_rs_types_h() {
        // int, int, 4 floats, enum (int), float[5].
        assert_eq!(size_of::<RawIntrinsics>(), 48);
        assert_eq!(std::mem::offset_of!(RawIntrinsics, model), 24);
        assert_eq!(std::mem::offset_of!(RawIntrinsics, coeffs), 28);
        assert_eq!(Distortion::from_raw(5), Distortion::KannalaBrandt4);
        assert_eq!(Distortion::from_raw(9), Distortion::Unknown(9));
    }

    #[test]
    fn camera_info_follows_realsense_ros() {
        let ci = d435_color().camera_info("camera_color_optical_frame", 7);
        assert_eq!(
            (ci.width, ci.height, ci.distortion_model.as_str()),
            (640, 480, "plumb_bob")
        );
        assert_eq!(ci.k, [615.0, 0.0, 321.5, 0.0, 615.5, 245.25, 0.0, 0.0, 1.0]);
        assert_eq!(
            ci.p,
            [
                615.0, 0.0, 321.5, 0.0, 0.0, 615.5, 245.25, 0.0, 0.0, 0.0, 1.0, 0.0
            ]
        );
        assert_eq!(ci.d.len(), 5);
        assert_eq!(
            (ci.header.frame_id.as_str(), ci.header.stamp_ns),
            ("camera_color_optical_frame", 7)
        );
        let mut fisheye = d435_color();
        fisheye.model = Distortion::KannalaBrandt4;
        fisheye.coeffs = [0.1, 0.2, 0.3, 0.4, 0.0];
        let ci = fisheye.camera_info("f", 0);
        assert_eq!(ci.distortion_model, "equidistant");
        assert_eq!(ci.d.len(), 4);
        assert!((ci.d[3] - 0.4).abs() < 1e-6);
    }

    #[test]
    fn deprojection_without_distortion_is_the_pinhole_model() {
        let i = d435_color();
        assert_eq!(i.deproject(321.5, 245.25, 2.0), [0.0, 0.0, 2.0]);
        // 61.5 px right of centre at f = 615 is a tenth of the depth.
        let p = i.deproject(321.5 + 61.5, 245.25 - 123.1, 2.0);
        assert!(
            (p[0] - 0.2).abs() < 1e-6 && (p[1] + 0.4).abs() < 1e-6,
            "{p:?}"
        );
    }

    #[test]
    fn deprojection_inverts_brown_conrady() {
        let mut i = d435_color();
        i.model = Distortion::BrownConrady;
        i.coeffs = [0.1, -0.05, 0.001, -0.002, 0.01];
        // Distort a known normalised point forward (OpenCV's model), then
        // check deprojection recovers it.
        let (x, y) = (0.3f32, -0.2f32);
        let c = i.coeffs;
        let r2 = x * x + y * y;
        let radial = 1.0 + c[0] * r2 + c[1] * r2 * r2 + c[4] * r2 * r2 * r2;
        let xd = x * radial + 2.0 * c[2] * x * y + c[3] * (r2 + 2.0 * x * x);
        let yd = y * radial + 2.0 * c[3] * x * y + c[2] * (r2 + 2.0 * y * y);
        let p = i.deproject(xd * i.fx + i.ppx, yd * i.fy + i.ppy, 1.5);
        assert!(
            (p[0] - x * 1.5).abs() < 1e-4 && (p[1] - y * 1.5).abs() < 1e-4,
            "{p:?}"
        );
        assert_eq!(p[2], 1.5);
    }

    #[test]
    fn deprojection_inverts_kannala_brandt() {
        let mut i = d435_color();
        i.model = Distortion::KannalaBrandt4;
        i.coeffs = [-0.01, 0.002, 0.0, 0.0, 0.0];
        // A ray 30 degrees off axis: r_d = theta (1 + k1 theta^2 + k2 theta^4).
        let theta = 30f32.to_radians();
        let t2 = theta * theta;
        let rd = theta * (1.0 + t2 * (i.coeffs[0] + t2 * i.coeffs[1]));
        let p = i.deproject(i.ppx + rd * i.fx, i.ppy, 1.0);
        assert!(
            (p[0] - theta.tan()).abs() < 1e-4 && p[1].abs() < 1e-6,
            "{p:?}"
        );
    }

    #[test]
    fn depth_in_millimetres() {
        // 2x2 with 2 bytes of row padding; units of 1 mm and of 0.25 mm (L515).
        let raw: Vec<u8> = [1000u16, 0, 0xFFFF, 4000]
            .chunks(2)
            .flat_map(|r| {
                let mut v: Vec<u8> = r.iter().flat_map(|d| d.to_le_bytes()).collect();
                v.extend_from_slice(&[0xAA, 0xAA]);
                v
            })
            .collect();
        let mm = |img: &Image| -> Vec<u16> {
            img.data
                .as_chunks::<2>()
                .0
                .iter()
                .map(|p| u16::from_le_bytes(*p))
                .collect()
        };
        let img = depth_image(&raw, 2, 2, 6, 0.001).unwrap();
        assert_eq!(
            (img.encoding, mm(&img)),
            (Encoding::Mono16, vec![1000, 0, 0xFFFF, 4000])
        );
        let img = depth_image(&raw, 2, 2, 6, 0.00025).unwrap();
        assert_eq!(mm(&img), vec![250, 0, 16384, 1000]);
        // Coarser units saturate instead of wrapping.
        let img = depth_image(&raw, 2, 2, 6, 0.01).unwrap();
        assert_eq!(mm(&img), vec![10000, 0, 65535, 40000]);
        assert!(depth_image(&raw, 2, 2, 3, 0.001).is_err());
        assert!(depth_image(&raw[..8], 2, 2, 6, 0.001).is_err());
    }

    #[test]
    fn video_frames_drop_padding() {
        let raw = [1, 2, 3, 4, 5, 6, 0, 0, 7, 8, 9, 10, 11, 12, 0, 0];
        let img = video_image(&raw, 2, 2, 8, format::RGB8).unwrap();
        assert_eq!(img.data, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
        let ir = video_image(&[1, 2, 9, 3, 4, 9], 2, 2, 3, format::Y8).unwrap();
        assert_eq!((ir.encoding, ir.data), (Encoding::Mono8, vec![1, 2, 3, 4]));
        assert!(video_image(&raw, 2, 2, 8, format::Z16).is_err());
    }

    #[test]
    fn point_clouds_skip_holes_and_far_points() {
        let i = Intrinsics {
            width: 4,
            height: 2,
            ppx: 1.5,
            ppy: 0.5,
            fx: 2.0,
            fy: 2.0,
            model: Distortion::None,
            coeffs: [0.0; 5],
        };
        let mut depth = Image::new(4, 2, Encoding::Mono16);
        for (k, d) in [1000u16, 0, 2000, 9000, 1000, 1000, 1000, 1000]
            .iter()
            .enumerate()
        {
            depth.data[k * 2..k * 2 + 2].copy_from_slice(&d.to_le_bytes());
        }
        let pc = point_cloud(&depth, &i, 1, 5.0);
        assert_eq!(pc.points.len(), 6);
        assert_eq!(pc.points[0], [-0.75, -0.25, 1.0]);
        assert_eq!(pc.points[1], [0.5, -0.5, 2.0]);
        // Every other row and column: (0, 0) and (2, 0).
        assert_eq!(point_cloud(&depth, &i, 2, 5.0).points.len(), 2);
    }

    fn video(
        stream: i32,
        fmt: i32,
        w: u32,
        h: u32,
        bpp: usize,
        intr: Option<Intrinsics>,
    ) -> RawFrame {
        RawFrame {
            stream,
            index: if stream == stream::INFRARED { 1 } else { 0 },
            format: fmt,
            width: w,
            height: h,
            stride: w as usize * bpp,
            stamp_ns: Some(42),
            intrinsics: intr,
            data: vec![1; w as usize * h as usize * bpp],
        }
    }

    fn motion_frame(stream: i32, v: [f32; 3]) -> RawFrame {
        RawFrame {
            stream,
            format: format::MOTION_XYZ32F,
            data: v.iter().flat_map(|x| x.to_le_bytes()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn converts_a_frameset() {
        let mut color = d435_color();
        color.width = 4;
        color.height = 2;
        let mut depth_intr = color;
        depth_intr.fx = 400.0;
        let frames = vec![
            video(stream::DEPTH, format::Z16, 4, 2, 2, Some(depth_intr)),
            video(stream::COLOR, format::RGB8, 4, 2, 3, Some(color)),
            video(stream::INFRARED, format::Y8, 4, 2, 1, None),
            motion_frame(stream::GYRO, [1.0, 2.0, 3.0]), // before any accel: dropped
            motion_frame(stream::ACCEL, [0.0, -9.8, 0.0]),
            motion_frame(stream::GYRO, [0.1, 0.2, 0.3]),
        ];
        let mut c = Converter::new(0.001, "cam", true, 1, 10.0);
        let m = c.convert(&frames, 99).unwrap();
        let (img, info) = m.color.unwrap();
        assert_eq!(
            (img.encoding, info.header.frame_id.as_str()),
            (Encoding::Rgb8, "cam_color_optical_frame")
        );
        assert_eq!(info.header.stamp_ns, 42);
        // Aligned depth: colour frame and intrinsics.
        let (d, dinfo) = m.depth.unwrap();
        assert_eq!(d.encoding, Encoding::Mono16);
        assert_eq!(
            (dinfo.header.frame_id.as_str(), dinfo.k[0]),
            ("cam_color_optical_frame", 615.0)
        );
        // Raw value 0x0101 = 257 units of 1 mm, all pixels valid.
        let pc = m.points.unwrap();
        assert_eq!(pc.points.len(), 8);
        assert!((pc.points[0][2] - 0.257).abs() < 1e-6);
        let (ir, irinfo) = m.infrared.unwrap();
        assert_eq!(
            (ir.encoding, irinfo.header.frame_id.as_str()),
            (Encoding::Mono8, "cam_infra1_optical_frame")
        );
        assert_eq!((irinfo.width, irinfo.height), (4, 2));
        assert_eq!(m.imu.len(), 1);
        assert!((m.imu[0].angular_velocity.z - 0.3).abs() < 1e-6);
        assert_eq!(m.imu[0].header.frame_id, "cam_imu_optical_frame");
        assert_eq!(m.imu[0].header.stamp_ns, 99, "no host stamp: node time");

        // Unaligned depth keeps its own frame and intrinsics.
        let mut c = Converter::new(0.001, "cam", false, 0, 10.0);
        let m = c.convert(&frames, 0).unwrap();
        let (_, dinfo) = m.depth.unwrap();
        assert_eq!(
            (dinfo.header.frame_id.as_str(), dinfo.k[0]),
            ("cam_depth_optical_frame", 400.0)
        );
        assert!(m.points.is_none());
    }

    #[test]
    fn imu_messages() {
        let m = imu(
            [0.1, 0.2, 0.3],
            [0.0, -9.8, 0.0],
            "camera_imu_optical_frame",
            5,
        );
        assert!((m.angular_velocity.y - 0.2).abs() < 1e-6);
        assert!((m.linear_acceleration.y + 9.8).abs() < 1e-5);
        assert_eq!(m.orientation_covariance[0], -1.0);
    }
}
