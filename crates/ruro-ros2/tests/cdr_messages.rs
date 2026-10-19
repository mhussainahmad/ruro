//! Byte-level CDR layouts of generated messages, written out by hand from
//! the XCDR1 rules (alignment from the end of the encapsulation header).

use ruro_ros2::RosMessage;
use ruro_ros2::cdr::{from_slice, to_vec};
use ruro_ros2::msgs::builtin_interfaces::msg::Time;
use ruro_ros2::msgs::geometry_msgs::msg::{Quaternion, Twist, Vector3};
use ruro_ros2::msgs::sensor_msgs::msg::{Imu, PointCloud2, PointField};
use ruro_ros2::msgs::std_msgs::msg::{Bool, Empty, Float64, Header, String as RosString};

const HDR: [u8; 4] = [0, 1, 0, 0];

fn bytes(parts: &[&[u8]]) -> Vec<u8> {
    let mut v = HDR.to_vec();
    for p in parts {
        v.extend_from_slice(p);
    }
    v
}

#[test]
fn std_msgs_primitives() {
    let s = RosString {
        data: "hello".into(),
    };
    assert_eq!(to_vec(&s), bytes(&[&[6, 0, 0, 0], b"hello\0"]));
    assert_eq!(to_vec(&Bool { data: true }), bytes(&[&[1]]));
    assert_eq!(
        to_vec(&Float64 { data: 1.5 }),
        bytes(&[&1.5f64.to_le_bytes()])
    );
    // Empty messages carry one placeholder byte.
    assert_eq!(to_vec(&Empty {}), bytes(&[&[0]]));
    assert_eq!(from_slice::<Empty>(&bytes(&[&[0]])).unwrap(), Empty {});
}

#[test]
fn header_layout() {
    let h = Header {
        stamp: Time { sec: 1, nanosec: 2 },
        frame_id: "map".into(),
    };
    let expected = bytes(&[&[1, 0, 0, 0], &[2, 0, 0, 0], &[4, 0, 0, 0], b"map\0"]);
    assert_eq!(to_vec(&h), expected);
    assert_eq!(from_slice::<Header>(&expected).unwrap(), h);
}

#[test]
fn doubles_after_a_header_are_padded_to_eight() {
    // Header with frame "ab": 4 + 4 + 4 + 3 = 15 bytes, so the quaternion
    // that follows starts after one padding byte, at offset 16.
    let imu = Imu {
        header: Header {
            stamp: Time { sec: 0, nanosec: 0 },
            frame_id: "ab".into(),
        },
        orientation: Quaternion {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            w: 1.0,
        },
        ..Default::default()
    };
    let b = to_vec(&imu);
    let payload = &b[4..];
    assert_eq!(&payload[8..15], &[3, 0, 0, 0, b'a', b'b', 0]);
    assert_eq!(payload[15], 0, "padding");
    assert_eq!(&payload[16 + 24..16 + 32], &1.0f64.to_le_bytes(), "w");
    // 16 header+pad, 4 quaternion doubles, 9 covariance doubles, 3 vector
    // doubles, 9 covariance doubles, 3 vector doubles, 9 covariance doubles.
    assert_eq!(payload.len(), 16 + 8 * (4 + 9 + 3 + 9 + 3 + 9));
    assert_eq!(from_slice::<Imu>(&b).unwrap(), imu);
}

#[test]
fn quaternion_defaults_to_identity() {
    // geometry_msgs/Quaternion declares `float64 w 1`.
    assert_eq!(Quaternion::default().w, 1.0);
    assert_eq!(Imu::default().orientation.w, 1.0);
}

#[test]
fn twist_is_six_doubles() {
    let t = Twist {
        linear: Vector3 {
            x: 1.0,
            y: 2.0,
            z: 3.0,
        },
        angular: Vector3 {
            x: 4.0,
            y: 5.0,
            z: 6.0,
        },
    };
    let b = to_vec(&t);
    assert_eq!(b.len(), 4 + 48);
    for (i, v) in [1.0f64, 2.0, 3.0, 4.0, 5.0, 6.0].iter().enumerate() {
        assert_eq!(&b[4 + i * 8..12 + i * 8], &v.to_le_bytes());
    }
    assert_eq!(from_slice::<Twist>(&b).unwrap(), t);
}

#[test]
fn point_cloud2_with_fields_and_blob() {
    let pc = PointCloud2 {
        header: Header {
            stamp: Time { sec: 5, nanosec: 6 },
            frame_id: "l".into(),
        },
        height: 1,
        width: 1,
        fields: vec![PointField {
            name: "x".into(),
            offset: 0,
            datatype: PointField::FLOAT32,
            count: 1,
        }],
        is_bigendian: false,
        point_step: 4,
        row_step: 4,
        data: 2.0f32.to_le_bytes().to_vec(),
        is_dense: true,
    };
    let expected = bytes(&[
        &[5, 0, 0, 0, 6, 0, 0, 0],
        &[2, 0, 0, 0, b'l', 0],
        &[0, 0],                // pad to 4
        &[1, 0, 0, 0],          // height
        &[1, 0, 0, 0],          // width
        &[1, 0, 0, 0],          // fields.len()
        &[2, 0, 0, 0, b'x', 0], // name
        &[0, 0],                // pad to 4
        &[0, 0, 0, 0],          // offset
        &[7],                   // datatype FLOAT32
        &[0, 0, 0],             // pad to 4
        &[1, 0, 0, 0],          // count
        &[0],                   // is_bigendian
        &[0, 0, 0],             // pad to 4
        &[4, 0, 0, 0],          // point_step
        &[4, 0, 0, 0],          // row_step
        &[4, 0, 0, 0],          // data.len()
        &2.0f32.to_le_bytes(),
        &[1], // is_dense
    ]);
    assert_eq!(to_vec(&pc), expected);
    assert_eq!(from_slice::<PointCloud2>(&expected).unwrap(), pc);
}

#[test]
fn type_metadata() {
    assert_eq!(RosString::TYPE_NAME, "std_msgs/msg/String");
    assert_eq!(RosString::DDS_TYPE_NAME, "std_msgs::msg::dds_::String_");
    assert_eq!(
        RosString::TYPE_HASH,
        "RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18"
    );
    assert_eq!(
        Twist::TYPE_HASH,
        "RIHS01_9c45bf16fe0983d80e3cfe750d6835843d265a9a6c46bd2e609fcddde6fb8d2a"
    );
}

#[test]
fn wstrings_match_what_ros_2_puts_on_the_wire() {
    use ruro_ros2::cdr::{CdrDecode, CdrEncode, CdrReader, CdrWriter};
    use ruro_ros2::msgs::example_interfaces::msg::WString;
    // Serialized by ROS 2 Jazzy (tests/data/cdr_fixtures.jsonl): a count of
    // UTF-16 units, each as 4 bytes; the emoji is a surrogate pair. The last
    // four bytes are padding left uninitialised by the publisher.
    let wire: Vec<u8> = (0..112)
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&"000100000b000000650000006d0000006f0000006a00000069000000200000003dd8000000de0000200000006f0000006b0000002e736f00"[i..i + 2], 16).unwrap()
        })
        .collect();
    let mut r = CdrReader::new(&wire).unwrap();
    let msg = WString::decode(&mut r).unwrap();
    assert_eq!(msg.data, "emoji 😀 ok");
    let mut w = CdrWriter::new();
    msg.encode(&mut w);
    let bytes = w.finish();
    assert_eq!(bytes[..], wire[..52]);
}
