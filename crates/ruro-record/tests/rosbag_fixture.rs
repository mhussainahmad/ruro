//! Imports a rosbag2 bag written by an independent serializer (the Python
//! `rosbags` library, see fixtures/make_rosbag.py) and checks every value.

use std::collections::HashMap;
use std::path::Path;

use ruro_msg::*;
use ruro_record::Recording;
use ruro_record::rosbag::{ImportOptions, import};

fn by_topic(path: &Path) -> HashMap<String, (String, Vec<u8>, u64)> {
    Recording::open(path)
        .unwrap()
        .messages()
        .unwrap()
        .map(Result::unwrap)
        .map(|m| {
            (
                m.topic().to_owned(),
                (m.ty().to_owned(), m.data.to_vec(), m.log_time),
            )
        })
        .collect()
}

#[test]
fn imports_a_bag_written_by_rosbags() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rosbag2_fixture");
    let out = std::env::temp_dir().join(format!("ruro-fixture-{}.mcap", std::process::id()));
    let opts = ImportOptions {
        map: [("/camera/image_raw".to_owned(), "camera.image".to_owned())].into(),
        compress: false,
    };
    let report = import(&src, &out, &opts).unwrap();
    let chatter = report
        .topics
        .iter()
        .find(|t| t.ros_topic == "/chatter")
        .unwrap();
    assert_eq!((chatter.converted, chatter.skipped), (0, 1));
    let converted: u64 = report.topics.iter().map(|t| t.converted).sum();
    assert_eq!(converted, 9, "{report:#?}");

    let msgs = by_topic(&out);
    let get = |topic: &str, ty: &str| -> Vec<u8> {
        let (t, data, _) = &msgs[topic];
        assert_eq!(t, ty, "{topic}");
        data.clone()
    };

    // bgr8 with row padding becomes tightly packed rgb8.
    let img = Image::decode(&get("camera.image", "std/Image")).unwrap();
    assert_eq!(
        (img.width, img.height, img.encoding),
        (3, 2, Encoding::Rgb8)
    );
    assert_eq!(
        img.data,
        [
            3, 2, 1, 6, 5, 4, 9, 8, 7, 12, 11, 10, 15, 14, 13, 18, 17, 16
        ]
    );
    assert_eq!(msgs["camera.image"].2, 1_000_000_000, "log time is kept");

    let cloud = PointCloud::decode(&get("points.out", "std/PointCloud")).unwrap();
    assert_eq!(cloud.points, [[1.0, 2.0, 3.0], [-1.5, 0.25, 8.0]]);

    let scan = LaserScan::decode(&get("scan.out", "std/LaserScan")).unwrap();
    assert_eq!(scan.header.frame_id, "laser");
    assert_eq!(scan.header.stamp_ns, 3_000_000_500);
    assert_eq!(scan.ranges, [1.0, 2.0, 3.0, 4.0, 5.0]);
    assert_eq!(
        (scan.angle_min, scan.angle_increment, scan.range_max),
        (-1.0, 0.5, 20.0)
    );

    let imu = Imu::decode(&get("imu.out", "std/Imu")).unwrap();
    assert_eq!(imu.orientation.w, 1.0);
    assert_eq!(imu.orientation_covariance, [0.01; 9]);
    assert_eq!(
        imu.angular_velocity,
        Vec3 {
            x: 0.1,
            y: 0.2,
            z: 0.3
        }
    );
    assert_eq!(imu.linear_acceleration.z, 9.81);

    let twist = Twist::decode(&get("cmd_vel.out", "std/Twist")).unwrap();
    assert_eq!((twist.linear.x, twist.angular.z), (0.5, -0.25));

    let pose = Pose::decode(&get("goal.out", "std/Pose")).unwrap();
    assert_eq!(
        pose.position,
        Vec3 {
            x: 1.0,
            y: 2.0,
            z: 3.0
        }
    );
    assert_eq!(pose.orientation.z, pose.orientation.w);
    assert!(pose.orientation.z > 0.7 && pose.orientation.x == 0.0);

    let speed = Scalar::decode(&get("speed.out", "std/Scalar")).unwrap();
    assert_eq!(speed, Scalar(42.5));

    let odom = Odometry::decode(&get("odom.out", "std/Odometry")).unwrap();
    assert_eq!(odom.child_frame_id, "base_link");
    assert_eq!(odom.header.frame_id, "odom");
    assert_eq!((odom.pose.position.x, odom.pose.position.y), (4.0, 5.0));
    assert_eq!((odom.twist.linear.x, odom.twist.angular.z), (0.3, 0.1));

    let tf = Transforms::decode(&get("tf.out", "std/Transforms")).unwrap();
    assert_eq!(tf.0.len(), 2);
    assert_eq!(tf.0[1].header.frame_id, "base_link");
    assert_eq!(tf.0[1].child_frame_id, "camera");
    assert_eq!(
        tf.0[1].translation,
        Vec3 {
            x: 0.1,
            y: 0.0,
            z: 0.5
        }
    );
    let _ = std::fs::remove_file(&out);
}
