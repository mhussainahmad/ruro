//! Rust messages to and from the C structs of `ruro.h`.

use std::ffi::CString;

use anyhow::{Context, Result, bail, ensure};
use ruro_msg::{
    Bytes, CameraInfo, Encoding, Flag, Header, Image, Imu, JointCommand, JointState, Joy,
    LaserScan, Message, Odometry, PointCloud, Pose, Quat, Scalar, Text, Transform, Transforms,
    Twist, Vec3,
};

use crate::abi::*;

/// Memory a view points into besides the message itself: strings copied
/// with a terminating NUL, and the array of a `std/Transforms`.
#[derive(Default)]
pub struct Scratch {
    strings: Vec<CString>,
    transforms: Vec<ruro_transform>,
}

impl Scratch {
    pub fn clear(&mut self) {
        self.strings.clear();
        self.transforms.clear();
    }

    fn str(&mut self, s: &str) -> ruro_str {
        // A NUL inside the string would end it early for C; drop it.
        let c = CString::new(s).unwrap_or_else(|_| CString::new(s.replace('\0', "")).unwrap());
        let view = ruro_str {
            ptr: c.as_ptr(),
            len: c.as_bytes().len(),
        };
        // The CString's buffer does not move when the CString does.
        self.strings.push(c);
        view
    }
}

/// A message type with a C struct.
pub trait CMessage: Message {
    type C: Copy;

    /// A view of `self` for C; it points into `self` and `scratch`.
    fn view(&self, scratch: &mut Scratch) -> Self::C;

    /// Copies a message from C.
    ///
    /// # Safety
    /// The pointers in `c` must be valid for their lengths.
    unsafe fn from_c(c: &Self::C) -> Result<Self>;
}

fn f64s(v: &[f64]) -> ruro_f64s {
    ruro_f64s {
        ptr: v.as_ptr(),
        len: v.len(),
    }
}

fn f32s(v: &[f32]) -> ruro_f32s {
    ruro_f32s {
        ptr: v.as_ptr(),
        len: v.len(),
    }
}

/// # Safety
/// `ptr` must be valid for `len` elements unless `len` is 0.
unsafe fn to_vec<T: Copy>(ptr: *const T, len: usize, what: &str) -> Result<Vec<T>> {
    if len == 0 {
        return Ok(Vec::new());
    }
    ensure!(!ptr.is_null(), "{what}: NULL pointer with length {len}");
    // SAFETY: the caller's contract.
    Ok(unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec())
}

/// # Safety
/// As [`to_vec`].
unsafe fn string(s: ruro_str, what: &str) -> Result<String> {
    // SAFETY: the caller's contract.
    let bytes = unsafe { to_vec(s.ptr.cast::<u8>(), s.len, what)? };
    String::from_utf8(bytes).with_context(|| format!("{what} is not UTF-8"))
}

fn vec3(v: &Vec3) -> ruro_vec3 {
    ruro_vec3 {
        x: v.x,
        y: v.y,
        z: v.z,
    }
}

fn from_vec3(v: &ruro_vec3) -> Vec3 {
    Vec3 {
        x: v.x,
        y: v.y,
        z: v.z,
    }
}

fn quat(q: &Quat) -> ruro_quat {
    ruro_quat {
        x: q.x,
        y: q.y,
        z: q.z,
        w: q.w,
    }
}

fn from_quat(q: &ruro_quat) -> Quat {
    Quat {
        x: q.x,
        y: q.y,
        z: q.z,
        w: q.w,
    }
}

fn pose(p: &Pose) -> ruro_pose {
    ruro_pose {
        position: vec3(&p.position),
        orientation: quat(&p.orientation),
    }
}

fn from_pose(p: &ruro_pose) -> Pose {
    Pose {
        position: from_vec3(&p.position),
        orientation: from_quat(&p.orientation),
    }
}

fn twist(t: &Twist) -> ruro_twist {
    ruro_twist {
        linear: vec3(&t.linear),
        angular: vec3(&t.angular),
    }
}

fn from_twist(t: &ruro_twist) -> Twist {
    Twist {
        linear: from_vec3(&t.linear),
        angular: from_vec3(&t.angular),
    }
}

fn header(h: &Header, s: &mut Scratch) -> ruro_header {
    ruro_header {
        stamp_ns: h.stamp_ns,
        frame_id: s.str(&h.frame_id),
    }
}

/// # Safety
/// As [`to_vec`].
unsafe fn from_header(h: &ruro_header) -> Result<Header> {
    Ok(Header {
        stamp_ns: h.stamp_ns,
        // SAFETY: the caller's contract.
        frame_id: unsafe { string(h.frame_id, "header.frame_id")? },
    })
}

fn transform(t: &Transform, s: &mut Scratch) -> ruro_transform {
    ruro_transform {
        header: header(&t.header, s),
        child_frame_id: s.str(&t.child_frame_id),
        translation: vec3(&t.translation),
        rotation: quat(&t.rotation),
    }
}

/// # Safety
/// As [`to_vec`].
unsafe fn from_transform(t: &ruro_transform) -> Result<Transform> {
    // SAFETY: the caller's contract.
    unsafe {
        Ok(Transform {
            header: from_header(&t.header)?,
            child_frame_id: string(t.child_frame_id, "child_frame_id")?,
            translation: from_vec3(&t.translation),
            rotation: from_quat(&t.rotation),
        })
    }
}

macro_rules! simple {
    ($t:ty, $c:ty, |$m:ident| $view:expr, |$cv:ident| $from:expr) => {
        impl CMessage for $t {
            type C = $c;

            fn view(&self, _: &mut Scratch) -> $c {
                let $m = self;
                $view
            }

            unsafe fn from_c($cv: &$c) -> Result<Self> {
                Ok($from)
            }
        }
    };
}

simple!(Scalar, ruro_scalar, |m| ruro_scalar { value: m.0 }, |c| {
    Scalar(c.value)
});
simple!(Flag, ruro_flag, |m| ruro_flag { value: m.0 }, |c| Flag(
    c.value
));
simple!(Vec3, ruro_vec3, |m| vec3(m), |c| from_vec3(c));
simple!(Pose, ruro_pose, |m| pose(m), |c| from_pose(c));
simple!(Twist, ruro_twist, |m| twist(m), |c| from_twist(c));

impl CMessage for Text {
    type C = ruro_text;

    fn view(&self, s: &mut Scratch) -> ruro_text {
        ruro_text {
            text: s.str(&self.0),
        }
    }

    unsafe fn from_c(c: &ruro_text) -> Result<Self> {
        // SAFETY: the caller's contract.
        Ok(Text(unsafe { string(c.text, "text")? }))
    }
}

impl CMessage for Bytes {
    type C = ruro_bytes;

    fn view(&self, _: &mut Scratch) -> ruro_bytes {
        ruro_bytes {
            data: ruro_u8s {
                ptr: self.0.as_ptr(),
                len: self.0.len(),
            },
        }
    }

    unsafe fn from_c(c: &ruro_bytes) -> Result<Self> {
        // SAFETY: the caller's contract.
        Ok(Bytes(unsafe { to_vec(c.data.ptr, c.data.len, "data")? }))
    }
}

impl CMessage for JointState {
    type C = ruro_joint_state;

    fn view(&self, _: &mut Scratch) -> ruro_joint_state {
        ruro_joint_state {
            position: f64s(&self.position),
            velocity: f64s(&self.velocity),
            effort: f64s(&self.effort),
        }
    }

    unsafe fn from_c(c: &ruro_joint_state) -> Result<Self> {
        // SAFETY: the caller's contract.
        unsafe {
            Ok(JointState {
                position: to_vec(c.position.ptr, c.position.len, "position")?,
                velocity: to_vec(c.velocity.ptr, c.velocity.len, "velocity")?,
                effort: to_vec(c.effort.ptr, c.effort.len, "effort")?,
            })
        }
    }
}

impl CMessage for JointCommand {
    type C = ruro_joint_command;

    fn view(&self, s: &mut Scratch) -> ruro_joint_command {
        ruro_joint_command {
            header: header(&self.header, s),
            position: f64s(&self.position),
            velocity: f64s(&self.velocity),
            effort: f64s(&self.effort),
        }
    }

    unsafe fn from_c(c: &ruro_joint_command) -> Result<Self> {
        // SAFETY: the caller's contract.
        unsafe {
            Ok(JointCommand {
                header: from_header(&c.header)?,
                position: to_vec(c.position.ptr, c.position.len, "position")?,
                velocity: to_vec(c.velocity.ptr, c.velocity.len, "velocity")?,
                effort: to_vec(c.effort.ptr, c.effort.len, "effort")?,
            })
        }
    }
}

impl CMessage for Transform {
    type C = ruro_transform;

    fn view(&self, s: &mut Scratch) -> ruro_transform {
        transform(self, s)
    }

    unsafe fn from_c(c: &ruro_transform) -> Result<Self> {
        // SAFETY: the caller's contract.
        unsafe { from_transform(c) }
    }
}

impl CMessage for Transforms {
    type C = ruro_transforms;

    fn view(&self, s: &mut Scratch) -> ruro_transforms {
        let views: Vec<ruro_transform> = self.0.iter().map(|t| transform(t, s)).collect();
        s.transforms = views;
        ruro_transforms {
            ptr: s.transforms.as_ptr(),
            len: s.transforms.len(),
        }
    }

    unsafe fn from_c(c: &ruro_transforms) -> Result<Self> {
        // SAFETY: the caller's contract.
        unsafe {
            to_vec(c.ptr, c.len, "transforms")?
                .iter()
                .map(|t| from_transform(t))
                .collect::<Result<_>>()
                .map(Transforms)
        }
    }
}

impl CMessage for Imu {
    type C = ruro_imu;

    fn view(&self, s: &mut Scratch) -> ruro_imu {
        ruro_imu {
            header: header(&self.header, s),
            orientation: quat(&self.orientation),
            orientation_covariance: self.orientation_covariance,
            angular_velocity: vec3(&self.angular_velocity),
            angular_velocity_covariance: self.angular_velocity_covariance,
            linear_acceleration: vec3(&self.linear_acceleration),
            linear_acceleration_covariance: self.linear_acceleration_covariance,
        }
    }

    unsafe fn from_c(c: &ruro_imu) -> Result<Self> {
        Ok(Imu {
            // SAFETY: the caller's contract.
            header: unsafe { from_header(&c.header)? },
            orientation: from_quat(&c.orientation),
            orientation_covariance: c.orientation_covariance,
            angular_velocity: from_vec3(&c.angular_velocity),
            angular_velocity_covariance: c.angular_velocity_covariance,
            linear_acceleration: from_vec3(&c.linear_acceleration),
            linear_acceleration_covariance: c.linear_acceleration_covariance,
        })
    }
}

impl CMessage for LaserScan {
    type C = ruro_laser_scan;

    fn view(&self, s: &mut Scratch) -> ruro_laser_scan {
        ruro_laser_scan {
            header: header(&self.header, s),
            angle_min: self.angle_min,
            angle_max: self.angle_max,
            angle_increment: self.angle_increment,
            time_increment: self.time_increment,
            scan_time: self.scan_time,
            range_min: self.range_min,
            range_max: self.range_max,
            ranges: f32s(&self.ranges),
            intensities: f32s(&self.intensities),
        }
    }

    unsafe fn from_c(c: &ruro_laser_scan) -> Result<Self> {
        // SAFETY: the caller's contract.
        unsafe {
            Ok(LaserScan {
                header: from_header(&c.header)?,
                angle_min: c.angle_min,
                angle_max: c.angle_max,
                angle_increment: c.angle_increment,
                time_increment: c.time_increment,
                scan_time: c.scan_time,
                range_min: c.range_min,
                range_max: c.range_max,
                ranges: to_vec(c.ranges.ptr, c.ranges.len, "ranges")?,
                intensities: to_vec(c.intensities.ptr, c.intensities.len, "intensities")?,
            })
        }
    }
}

impl CMessage for Odometry {
    type C = ruro_odometry;

    fn view(&self, s: &mut Scratch) -> ruro_odometry {
        ruro_odometry {
            header: header(&self.header, s),
            child_frame_id: s.str(&self.child_frame_id),
            pose: pose(&self.pose),
            twist: twist(&self.twist),
        }
    }

    unsafe fn from_c(c: &ruro_odometry) -> Result<Self> {
        // SAFETY: the caller's contract.
        unsafe {
            Ok(Odometry {
                header: from_header(&c.header)?,
                child_frame_id: string(c.child_frame_id, "child_frame_id")?,
                pose: from_pose(&c.pose),
                twist: from_twist(&c.twist),
            })
        }
    }
}

impl CMessage for CameraInfo {
    type C = ruro_camera_info;

    fn view(&self, s: &mut Scratch) -> ruro_camera_info {
        ruro_camera_info {
            header: header(&self.header, s),
            width: self.width,
            height: self.height,
            distortion_model: s.str(&self.distortion_model),
            d: f64s(&self.d),
            k: self.k,
            r: self.r,
            p: self.p,
        }
    }

    unsafe fn from_c(c: &ruro_camera_info) -> Result<Self> {
        // SAFETY: the caller's contract.
        unsafe {
            Ok(CameraInfo {
                header: from_header(&c.header)?,
                width: c.width,
                height: c.height,
                distortion_model: string(c.distortion_model, "distortion_model")?,
                d: to_vec(c.d.ptr, c.d.len, "d")?,
                k: c.k,
                r: c.r,
                p: c.p,
            })
        }
    }
}

impl CMessage for Joy {
    type C = ruro_joy;

    fn view(&self, s: &mut Scratch) -> ruro_joy {
        ruro_joy {
            header: header(&self.header, s),
            axes: f32s(&self.axes),
            buttons: ruro_i32s {
                ptr: self.buttons.as_ptr(),
                len: self.buttons.len(),
            },
        }
    }

    unsafe fn from_c(c: &ruro_joy) -> Result<Self> {
        // SAFETY: the caller's contract.
        unsafe {
            Ok(Joy {
                header: from_header(&c.header)?,
                axes: to_vec(c.axes.ptr, c.axes.len, "axes")?,
                buttons: to_vec(c.buttons.ptr, c.buttons.len, "buttons")?,
            })
        }
    }
}

impl CMessage for Image {
    type C = ruro_image;

    fn view(&self, _: &mut Scratch) -> ruro_image {
        ruro_image {
            width: self.width,
            height: self.height,
            encoding: self.encoding as u32,
            data: ruro_u8s {
                ptr: self.data.as_ptr(),
                len: self.data.len(),
            },
        }
    }

    unsafe fn from_c(c: &ruro_image) -> Result<Self> {
        let encoding = match u8::try_from(c.encoding) {
            Ok(e) => Encoding::from_u8(e).map_err(|e| anyhow::anyhow!("{e}"))?,
            Err(_) => bail!("unknown image encoding {}", c.encoding),
        };
        let expected = c.width as usize * c.height as usize * encoding.bytes_per_pixel();
        ensure!(
            c.data.len == expected,
            "image data has {} bytes; a {}x{} image of encoding {} has {expected}",
            c.data.len,
            c.width,
            c.height,
            c.encoding
        );
        Ok(Image {
            width: c.width,
            height: c.height,
            encoding,
            // SAFETY: the caller's contract.
            data: unsafe { to_vec(c.data.ptr, c.data.len, "data")? },
        })
    }
}

impl CMessage for PointCloud {
    type C = ruro_point_cloud;

    fn view(&self, _: &mut Scratch) -> ruro_point_cloud {
        ruro_point_cloud {
            xyz: self.points.as_ptr().cast::<f32>(),
            len: self.points.len(),
        }
    }

    unsafe fn from_c(c: &ruro_point_cloud) -> Result<Self> {
        // SAFETY: the caller's contract: `xyz` holds 3 * len floats.
        let flat = unsafe { to_vec(c.xyz, c.len * 3, "xyz")? };
        Ok(PointCloud {
            points: flat.as_chunks::<3>().0.to_vec(),
        })
    }
}
