//! Conversions between ruro messages and ROS 2 messages.
//!
//! `*_from_ros` turn a received ROS message into the ruro message the
//! subscriber node outputs; `*_to_ros` build the ROS message a publisher node
//! sends. Fields ruro does not model (covariances of odometry, joint names,
//! camera binning and ROI) are dropped on the way in and zero on the way out.

use anyhow::{Result, bail};
use ruro::msg as r;
use ruro_ros2::msgs::{geometry_msgs as gm, nav_msgs, sensor_msgs as sm, std_msgs, tf2_msgs};
use ruro_ros2::time::{time_from_ns, time_to_ns};

/// What a publisher knows besides the ruro message.
#[derive(Debug, Clone, Default)]
pub struct PubContext {
    /// Frame for ROS messages that need one but whose ruro form has none.
    pub frame_id: String,
    /// Stamp for such messages: when the ruro message was sent.
    pub stamp_ns: u64,
}

fn header_from_ros(h: &std_msgs::msg::Header) -> r::Header {
    r::Header::new(h.frame_id.clone(), time_to_ns(&h.stamp))
}

fn header_to_ros(h: &r::Header) -> std_msgs::msg::Header {
    std_msgs::msg::Header {
        stamp: time_from_ns(h.stamp_ns),
        frame_id: h.frame_id.clone(),
    }
}

fn ctx_header(ctx: &PubContext) -> std_msgs::msg::Header {
    std_msgs::msg::Header {
        stamp: time_from_ns(ctx.stamp_ns),
        frame_id: ctx.frame_id.clone(),
    }
}

fn vec3_from_ros(v: &gm::msg::Vector3) -> r::Vec3 {
    r::Vec3 {
        x: v.x,
        y: v.y,
        z: v.z,
    }
}

fn vec3_to_ros(v: &r::Vec3) -> gm::msg::Vector3 {
    gm::msg::Vector3 {
        x: v.x,
        y: v.y,
        z: v.z,
    }
}

fn quat_from_ros(q: &gm::msg::Quaternion) -> r::Quat {
    r::Quat {
        x: q.x,
        y: q.y,
        z: q.z,
        w: q.w,
    }
}

fn quat_to_ros(q: &r::Quat) -> gm::msg::Quaternion {
    gm::msg::Quaternion {
        x: q.x,
        y: q.y,
        z: q.z,
        w: q.w,
    }
}

fn pose_from_ros(p: &gm::msg::Pose) -> r::Pose {
    r::Pose {
        position: r::Vec3 {
            x: p.position.x,
            y: p.position.y,
            z: p.position.z,
        },
        orientation: quat_from_ros(&p.orientation),
    }
}

fn pose_to_ros(p: &r::Pose) -> gm::msg::Pose {
    gm::msg::Pose {
        position: gm::msg::Point {
            x: p.position.x,
            y: p.position.y,
            z: p.position.z,
        },
        orientation: quat_to_ros(&p.orientation),
    }
}

pub fn twist_from_ros(t: gm::msg::Twist) -> Result<r::Twist> {
    Ok(r::Twist {
        linear: vec3_from_ros(&t.linear),
        angular: vec3_from_ros(&t.angular),
    })
}

pub fn twist_to_ros(t: &r::Twist, _: &PubContext) -> Result<gm::msg::Twist> {
    Ok(gm::msg::Twist {
        linear: vec3_to_ros(&t.linear),
        angular: vec3_to_ros(&t.angular),
    })
}

pub fn pose_stamped_from_ros(p: gm::msg::PoseStamped) -> Result<r::Pose> {
    Ok(pose_from_ros(&p.pose))
}

pub fn pose_stamped_to_ros(p: &r::Pose, ctx: &PubContext) -> Result<gm::msg::PoseStamped> {
    Ok(gm::msg::PoseStamped {
        header: ctx_header(ctx),
        pose: pose_to_ros(p),
    })
}

pub fn float64_from_ros(m: std_msgs::msg::Float64) -> Result<r::Scalar> {
    Ok(r::Scalar(m.data))
}

pub fn float64_to_ros(m: &r::Scalar, _: &PubContext) -> Result<std_msgs::msg::Float64> {
    Ok(std_msgs::msg::Float64 { data: m.0 })
}

pub fn string_from_ros(m: std_msgs::msg::String) -> Result<r::Text> {
    Ok(r::Text(m.data))
}

pub fn string_to_ros(m: &r::Text, _: &PubContext) -> Result<std_msgs::msg::String> {
    Ok(std_msgs::msg::String { data: m.0.clone() })
}

pub fn bool_from_ros(m: std_msgs::msg::Bool) -> Result<r::Flag> {
    Ok(r::Flag(m.data))
}

pub fn bool_to_ros(m: &r::Flag, _: &PubContext) -> Result<std_msgs::msg::Bool> {
    Ok(std_msgs::msg::Bool { data: m.0 })
}

pub fn imu_from_ros(m: sm::msg::Imu) -> Result<r::Imu> {
    Ok(r::Imu {
        header: header_from_ros(&m.header),
        orientation: quat_from_ros(&m.orientation),
        orientation_covariance: m.orientation_covariance,
        angular_velocity: vec3_from_ros(&m.angular_velocity),
        angular_velocity_covariance: m.angular_velocity_covariance,
        linear_acceleration: vec3_from_ros(&m.linear_acceleration),
        linear_acceleration_covariance: m.linear_acceleration_covariance,
    })
}

pub fn imu_to_ros(m: &r::Imu, _: &PubContext) -> Result<sm::msg::Imu> {
    Ok(sm::msg::Imu {
        header: header_to_ros(&m.header),
        orientation: quat_to_ros(&m.orientation),
        orientation_covariance: m.orientation_covariance,
        angular_velocity: vec3_to_ros(&m.angular_velocity),
        angular_velocity_covariance: m.angular_velocity_covariance,
        linear_acceleration: vec3_to_ros(&m.linear_acceleration),
        linear_acceleration_covariance: m.linear_acceleration_covariance,
    })
}

pub fn laser_scan_from_ros(m: sm::msg::LaserScan) -> Result<r::LaserScan> {
    Ok(r::LaserScan {
        header: header_from_ros(&m.header),
        angle_min: m.angle_min,
        angle_max: m.angle_max,
        angle_increment: m.angle_increment,
        time_increment: m.time_increment,
        scan_time: m.scan_time,
        range_min: m.range_min,
        range_max: m.range_max,
        ranges: m.ranges,
        intensities: m.intensities,
    })
}

pub fn laser_scan_to_ros(m: &r::LaserScan, _: &PubContext) -> Result<sm::msg::LaserScan> {
    Ok(sm::msg::LaserScan {
        header: header_to_ros(&m.header),
        angle_min: m.angle_min,
        angle_max: m.angle_max,
        angle_increment: m.angle_increment,
        time_increment: m.time_increment,
        scan_time: m.scan_time,
        range_min: m.range_min,
        range_max: m.range_max,
        ranges: m.ranges.clone(),
        intensities: m.intensities.clone(),
    })
}

pub fn odometry_from_ros(m: nav_msgs::msg::Odometry) -> Result<r::Odometry> {
    Ok(r::Odometry {
        header: header_from_ros(&m.header),
        child_frame_id: m.child_frame_id,
        pose: pose_from_ros(&m.pose.pose),
        twist: r::Twist {
            linear: vec3_from_ros(&m.twist.twist.linear),
            angular: vec3_from_ros(&m.twist.twist.angular),
        },
    })
}

pub fn odometry_to_ros(m: &r::Odometry, _: &PubContext) -> Result<nav_msgs::msg::Odometry> {
    Ok(nav_msgs::msg::Odometry {
        header: header_to_ros(&m.header),
        child_frame_id: m.child_frame_id.clone(),
        pose: gm::msg::PoseWithCovariance {
            pose: pose_to_ros(&m.pose),
            ..Default::default()
        },
        twist: gm::msg::TwistWithCovariance {
            twist: twist_to_ros(&m.twist, &PubContext::default())?,
            ..Default::default()
        },
    })
}

pub fn tf_from_ros(m: tf2_msgs::msg::TFMessage) -> Result<r::Transforms> {
    Ok(r::Transforms(
        m.transforms
            .iter()
            .map(|t| r::Transform {
                header: header_from_ros(&t.header),
                child_frame_id: t.child_frame_id.clone(),
                translation: vec3_from_ros(&t.transform.translation),
                rotation: quat_from_ros(&t.transform.rotation),
            })
            .collect(),
    ))
}

pub fn tf_to_ros(m: &r::Transforms, _: &PubContext) -> Result<tf2_msgs::msg::TFMessage> {
    Ok(tf2_msgs::msg::TFMessage {
        transforms: m
            .0
            .iter()
            .map(|t| gm::msg::TransformStamped {
                header: header_to_ros(&t.header),
                child_frame_id: t.child_frame_id.clone(),
                transform: gm::msg::Transform {
                    translation: vec3_to_ros(&t.translation),
                    rotation: quat_to_ros(&t.rotation),
                },
            })
            .collect(),
    })
}

pub fn joint_state_from_ros(m: sm::msg::JointState) -> Result<r::JointState> {
    Ok(r::JointState {
        position: m.position,
        velocity: m.velocity,
        effort: m.effort,
    })
}

pub fn joint_state_to_ros(m: &r::JointState, ctx: &PubContext) -> Result<sm::msg::JointState> {
    Ok(sm::msg::JointState {
        header: ctx_header(ctx),
        name: Vec::new(),
        position: m.position.clone(),
        velocity: m.velocity.clone(),
        effort: m.effort.clone(),
    })
}

pub fn joy_from_ros(m: sm::msg::Joy) -> Result<r::Joy> {
    Ok(r::Joy {
        header: header_from_ros(&m.header),
        axes: m.axes,
        buttons: m.buttons,
    })
}

pub fn joy_to_ros(m: &r::Joy, _: &PubContext) -> Result<sm::msg::Joy> {
    Ok(sm::msg::Joy {
        header: header_to_ros(&m.header),
        axes: m.axes.clone(),
        buttons: m.buttons.clone(),
    })
}

pub fn camera_info_from_ros(m: sm::msg::CameraInfo) -> Result<r::CameraInfo> {
    Ok(r::CameraInfo {
        header: header_from_ros(&m.header),
        width: m.width,
        height: m.height,
        distortion_model: m.distortion_model,
        d: m.d,
        k: m.k,
        r: m.r,
        p: m.p,
    })
}

pub fn camera_info_to_ros(m: &r::CameraInfo, _: &PubContext) -> Result<sm::msg::CameraInfo> {
    Ok(sm::msg::CameraInfo {
        header: header_to_ros(&m.header),
        width: m.width,
        height: m.height,
        distortion_model: m.distortion_model.clone(),
        d: m.d.clone(),
        k: m.k,
        r: m.r,
        p: m.p,
        ..Default::default()
    })
}

/// Accepts `rgb8`, `rgba8`, `mono8`/`8UC1`, and converts `bgr8`/`bgra8` to
/// RGB order. Row padding (`step` larger than a row) is removed.
pub fn image_from_ros(m: sm::msg::Image) -> Result<r::Image> {
    let (encoding, swap) = match m.encoding.as_str() {
        "rgb8" => (r::Encoding::Rgb8, false),
        "bgr8" => (r::Encoding::Rgb8, true),
        "rgba8" => (r::Encoding::Rgba8, false),
        "bgra8" => (r::Encoding::Rgba8, true),
        "mono8" | "8UC1" => (r::Encoding::Mono8, false),
        other => bail!("unsupported image encoding `{other}` (8-bit rgb/bgr/rgba/bgra/mono only)"),
    };
    let row = m.width as usize * encoding.channels();
    let step = m.step as usize;
    let rows = m.height as usize;
    if step < row || m.data.len() < step * rows.saturating_sub(1) + row {
        bail!(
            "image data too short: {}x{} {} with step {step} needs more than {} bytes",
            m.width,
            m.height,
            m.encoding,
            m.data.len()
        );
    }
    let mut data = if step == row && m.data.len() == row * rows {
        m.data
    } else {
        let mut d = Vec::with_capacity(row * rows);
        for y in 0..rows {
            d.extend_from_slice(&m.data[y * step..y * step + row]);
        }
        d
    };
    if swap {
        let ch = encoding.channels();
        for px in data.chunks_exact_mut(ch) {
            px.swap(0, 2);
        }
    }
    Ok(r::Image {
        width: m.width,
        height: m.height,
        encoding,
        data,
    })
}

pub fn image_to_ros(m: &r::Image, ctx: &PubContext) -> Result<sm::msg::Image> {
    let encoding = match m.encoding {
        r::Encoding::Rgb8 => "rgb8",
        r::Encoding::Rgba8 => "rgba8",
        r::Encoding::Mono8 => "mono8",
    };
    Ok(sm::msg::Image {
        header: ctx_header(ctx),
        height: m.height,
        width: m.width,
        encoding: encoding.into(),
        is_bigendian: 0,
        step: m.stride() as u32,
        data: m.data.clone(),
    })
}

/// Reads the `x`, `y`, `z` float32 fields of every point; points with a
/// non-finite coordinate are skipped.
pub fn point_cloud2_from_ros(m: sm::msg::PointCloud2) -> Result<r::PointCloud> {
    let offset = |name: &str| -> Result<usize> {
        let f = m
            .fields
            .iter()
            .find(|f| f.name == name)
            .ok_or_else(|| anyhow::anyhow!("point cloud has no `{name}` field"))?;
        if f.datatype != sm::msg::PointField::FLOAT32 {
            bail!("field `{name}` is not float32 (datatype {})", f.datatype);
        }
        Ok(f.offset as usize)
    };
    let (ox, oy, oz) = (offset("x")?, offset("y")?, offset("z")?);
    let step = m.point_step as usize;
    if step < ox.max(oy).max(oz) + 4 {
        bail!("point_step {step} is smaller than the xyz fields");
    }
    let n = m.width as usize * m.height as usize;
    let row_step = m.row_step as usize;
    let width = m.width as usize;
    let read = |at: usize| -> Option<f32> {
        let b: [u8; 4] = m.data.get(at..at + 4)?.try_into().ok()?;
        Some(if m.is_bigendian {
            f32::from_be_bytes(b)
        } else {
            f32::from_le_bytes(b)
        })
    };
    let mut points = Vec::with_capacity(n);
    for i in 0..n {
        let base = (i / width.max(1)) * row_step + (i % width.max(1)) * step;
        let (Some(x), Some(y), Some(z)) = (read(base + ox), read(base + oy), read(base + oz))
        else {
            bail!("point cloud data is shorter than its dimensions say");
        };
        if x.is_finite() && y.is_finite() && z.is_finite() {
            points.push([x, y, z]);
        }
    }
    Ok(r::PointCloud { points })
}

pub fn point_cloud2_to_ros(m: &r::PointCloud, ctx: &PubContext) -> Result<sm::msg::PointCloud2> {
    let field = |name: &str, offset: u32| sm::msg::PointField {
        name: name.into(),
        offset,
        datatype: sm::msg::PointField::FLOAT32,
        count: 1,
    };
    let mut data = Vec::with_capacity(m.points.len() * 12);
    for p in &m.points {
        for c in p {
            data.extend_from_slice(&c.to_le_bytes());
        }
    }
    Ok(sm::msg::PointCloud2 {
        header: ctx_header(ctx),
        height: 1,
        width: m.points.len() as u32,
        fields: vec![field("x", 0), field("y", 4), field("z", 8)],
        is_bigendian: false,
        point_step: 12,
        row_step: 12 * m.points.len() as u32,
        data,
        is_dense: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> PubContext {
        PubContext {
            frame_id: "base".into(),
            stamp_ns: 5_000_000_007,
        }
    }

    #[test]
    fn stamped_messages_roundtrip() {
        let h = r::Header::new("imu_link", 1_700_000_000_000_000_001);
        let imu = r::Imu {
            header: h.clone(),
            angular_velocity: r::Vec3 {
                x: 0.1,
                y: 0.2,
                z: 0.3,
            },
            orientation_covariance: [-1.0; 9],
            ..Default::default()
        };
        assert_eq!(
            imu_from_ros(imu_to_ros(&imu, &ctx()).unwrap()).unwrap(),
            imu
        );
        let scan = r::LaserScan {
            header: h.clone(),
            ranges: vec![1.0, f32::INFINITY],
            angle_increment: 0.01,
            ..Default::default()
        };
        assert_eq!(
            laser_scan_from_ros(laser_scan_to_ros(&scan, &ctx()).unwrap()).unwrap(),
            scan
        );
        let odom = r::Odometry {
            header: h.clone(),
            child_frame_id: "base".into(),
            ..Default::default()
        };
        assert_eq!(
            odometry_from_ros(odometry_to_ros(&odom, &ctx()).unwrap()).unwrap(),
            odom
        );
        let tf = r::Transforms(vec![r::Transform {
            header: h.clone(),
            child_frame_id: "laser".into(),
            ..Default::default()
        }]);
        assert_eq!(tf_from_ros(tf_to_ros(&tf, &ctx()).unwrap()).unwrap(), tf);
        let joy = r::Joy {
            header: h.clone(),
            axes: vec![0.5],
            buttons: vec![1],
        };
        assert_eq!(
            joy_from_ros(joy_to_ros(&joy, &ctx()).unwrap()).unwrap(),
            joy
        );
        let info = r::CameraInfo {
            header: h,
            width: 640,
            k: [1.0; 9],
            ..Default::default()
        };
        assert_eq!(
            camera_info_from_ros(camera_info_to_ros(&info, &ctx()).unwrap()).unwrap(),
            info
        );
    }

    #[test]
    fn unstamped_messages_take_frame_and_stamp_from_context() {
        let p = pose_stamped_to_ros(&r::Pose::default(), &ctx()).unwrap();
        assert_eq!(p.header.frame_id, "base");
        assert_eq!((p.header.stamp.sec, p.header.stamp.nanosec), (5, 7));
        assert_eq!(pose_stamped_from_ros(p).unwrap(), r::Pose::default());
        let js = r::JointState {
            position: vec![1.0],
            ..Default::default()
        };
        assert_eq!(
            joint_state_from_ros(joint_state_to_ros(&js, &ctx()).unwrap()).unwrap(),
            js
        );
        assert_eq!(
            float64_from_ros(float64_to_ros(&r::Scalar(2.5), &ctx()).unwrap()).unwrap(),
            r::Scalar(2.5)
        );
        assert_eq!(
            string_from_ros(string_to_ros(&r::Text("hi".into()), &ctx()).unwrap()).unwrap(),
            r::Text("hi".into())
        );
        assert_eq!(
            bool_from_ros(bool_to_ros(&r::Flag(true), &ctx()).unwrap()).unwrap(),
            r::Flag(true)
        );
    }

    #[test]
    fn images_handle_bgr_and_row_padding() {
        let ros = sm::msg::Image {
            width: 2,
            height: 2,
            encoding: "bgr8".into(),
            step: 8, // 6 bytes of pixels + 2 bytes of padding per row
            data: vec![1, 2, 3, 4, 5, 6, 0, 0, 7, 8, 9, 10, 11, 12, 0, 0],
            ..Default::default()
        };
        let img = image_from_ros(ros).unwrap();
        assert_eq!(img.encoding, r::Encoding::Rgb8);
        assert_eq!(img.data, [3, 2, 1, 6, 5, 4, 9, 8, 7, 12, 11, 10]);
        let back = image_to_ros(&img, &ctx()).unwrap();
        assert_eq!((back.step, back.encoding.as_str()), (6, "rgb8"));
        assert_eq!(image_from_ros(back).unwrap(), img);
        let bad = sm::msg::Image {
            width: 1,
            height: 1,
            encoding: "32FC1".into(),
            ..Default::default()
        };
        assert!(image_from_ros(bad).is_err());
    }

    #[test]
    fn point_clouds_read_xyz_fields() {
        let pc = r::PointCloud {
            points: vec![[1.0, 2.0, 3.0], [-1.0, 0.0, 9.5]],
        };
        let ros = point_cloud2_to_ros(&pc, &ctx()).unwrap();
        assert_eq!(ros.data.len(), 24);
        assert_eq!(point_cloud2_from_ros(ros).unwrap(), pc);

        // xyz plus intensity, 16-byte points, one NaN point that is skipped.
        let mut data = Vec::new();
        for p in [[1.0f32, 2.0, 3.0, 0.5], [f32::NAN, 0.0, 0.0, 0.0]] {
            for c in p {
                data.extend_from_slice(&c.to_le_bytes());
            }
        }
        let field = |name: &str, offset| sm::msg::PointField {
            name: name.into(),
            offset,
            datatype: sm::msg::PointField::FLOAT32,
            count: 1,
        };
        let ros = sm::msg::PointCloud2 {
            height: 1,
            width: 2,
            fields: vec![
                field("x", 0),
                field("y", 4),
                field("z", 8),
                field("intensity", 12),
            ],
            point_step: 16,
            row_step: 32,
            data,
            ..Default::default()
        };
        assert_eq!(
            point_cloud2_from_ros(ros).unwrap().points,
            [[1.0, 2.0, 3.0]]
        );
    }
}
