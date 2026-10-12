"""Writes the rosbag2 fixture used by tests/rosbag_fixture.rs.

The bag is produced by the `rosbags` library (an independent CDR serializer,
https://gitlab.com/ternaris/rosbags), so the import test checks ruro's CDR
decoder against someone else's reading of the format rather than its own.

    python -m venv venv && venv/bin/pip install rosbags==0.11.6
    venv/bin/python make_rosbag.py rosbag2_fixture

Every message's values are chosen so the Rust test can check them exactly.
"""

import sys
from pathlib import Path

import numpy as np
from rosbags.rosbag2 import StoragePlugin, Writer
from rosbags.typesys import Stores, get_typestore

ts = get_typestore(Stores.ROS2_HUMBLE)
T = ts.types
Time = T["builtin_interfaces/msg/Time"]
Header = T["std_msgs/msg/Header"]
Vector3 = T["geometry_msgs/msg/Vector3"]
Quaternion = T["geometry_msgs/msg/Quaternion"]
Point = T["geometry_msgs/msg/Point"]
Pose = T["geometry_msgs/msg/Pose"]
Twist = T["geometry_msgs/msg/Twist"]


def header(sec, frame):
    return Header(stamp=Time(sec=sec, nanosec=500), frame_id=frame)


def messages():
    # 3x2 bgr8 image with 2 bytes of padding per row (step 11).
    rows = [bytes([1, 2, 3, 4, 5, 6, 7, 8, 9, 0xEE, 0xEE]), bytes([10, 11, 12, 13, 14, 15, 16, 17, 18, 0xEE, 0xEE])]
    image = T["sensor_msgs/msg/Image"](
        header=header(1, "camera"),
        height=2,
        width=3,
        encoding="bgr8",
        is_bigendian=0,
        step=11,
        data=np.frombuffer(b"".join(rows), dtype=np.uint8),
    )
    yield "/camera/image_raw", image

    Field = T["sensor_msgs/msg/PointField"]
    fields = [
        Field(name="x", offset=0, datatype=7, count=1),
        Field(name="y", offset=4, datatype=7, count=1),
        Field(name="z", offset=8, datatype=7, count=1),
        Field(name="intensity", offset=12, datatype=7, count=1),
    ]
    points = np.array([[1.0, 2.0, 3.0, 100.0], [-1.5, 0.25, 8.0, 50.0]], dtype="<f4")
    cloud = T["sensor_msgs/msg/PointCloud2"](
        header=header(2, "lidar"),
        height=1,
        width=2,
        fields=fields,
        is_bigendian=False,
        point_step=16,
        row_step=32,
        data=np.frombuffer(points.tobytes(), dtype=np.uint8),
        is_dense=True,
    )
    yield "/points", cloud

    scan = T["sensor_msgs/msg/LaserScan"](
        header=header(3, "laser"),
        angle_min=-1.0,
        angle_max=1.0,
        angle_increment=0.5,
        time_increment=0.0,
        scan_time=0.1,
        range_min=0.1,
        range_max=20.0,
        ranges=np.array([1.0, 2.0, 3.0, 4.0, 5.0], dtype=np.float32),
        intensities=np.array([], dtype=np.float32),
    )
    yield "/scan", scan

    imu = T["sensor_msgs/msg/Imu"](
        header=header(4, "imu"),
        orientation=Quaternion(x=0.0, y=0.0, z=0.0, w=1.0),
        orientation_covariance=np.full(9, 0.01),
        angular_velocity=Vector3(x=0.1, y=0.2, z=0.3),
        angular_velocity_covariance=np.zeros(9),
        linear_acceleration=Vector3(x=0.0, y=0.0, z=9.81),
        linear_acceleration_covariance=np.zeros(9),
    )
    yield "/imu", imu

    yield "/cmd_vel", Twist(linear=Vector3(x=0.5, y=0.0, z=0.0), angular=Vector3(x=0.0, y=0.0, z=-0.25))

    pose = T["geometry_msgs/msg/PoseStamped"](
        header=header(5, "map"),
        pose=Pose(position=Point(x=1.0, y=2.0, z=3.0), orientation=Quaternion(x=0.0, y=0.0, z=0.7071, w=0.7071)),
    )
    yield "/goal", pose

    yield "/speed", T["std_msgs/msg/Float64"](data=42.5)

    PoseCov = T["geometry_msgs/msg/PoseWithCovariance"]
    TwistCov = T["geometry_msgs/msg/TwistWithCovariance"]
    odom = T["nav_msgs/msg/Odometry"](
        header=header(6, "odom"),
        child_frame_id="base_link",
        pose=PoseCov(pose=Pose(position=Point(x=4.0, y=5.0, z=0.0), orientation=Quaternion(x=0.0, y=0.0, z=0.0, w=1.0)), covariance=np.zeros(36)),
        twist=TwistCov(twist=Twist(linear=Vector3(x=0.3, y=0.0, z=0.0), angular=Vector3(x=0.0, y=0.0, z=0.1)), covariance=np.zeros(36)),
    )
    yield "/odom", odom

    TS = T["geometry_msgs/msg/TransformStamped"]
    Transform = T["geometry_msgs/msg/Transform"]
    tf = T["tf2_msgs/msg/TFMessage"](
        transforms=[
            TS(header=header(7, "odom"), child_frame_id="base_link",
               transform=Transform(translation=Vector3(x=1.0, y=0.0, z=0.0), rotation=Quaternion(x=0.0, y=0.0, z=0.0, w=1.0))),
            TS(header=header(7, "base_link"), child_frame_id="camera",
               transform=Transform(translation=Vector3(x=0.1, y=0.0, z=0.5), rotation=Quaternion(x=0.0, y=0.0, z=0.0, w=1.0))),
        ]
    )
    yield "/tf", tf

    yield "/chatter", T["std_msgs/msg/String"](data="not imported")


def main(out):
    out = Path(out)
    with Writer(out, version=8, storage_plugin=StoragePlugin.MCAP) as writer:
        conns = {}
        for i, (topic, msg) in enumerate(messages()):
            if topic not in conns:
                conns[topic] = writer.add_connection(topic, msg.__msgtype__, typestore=ts)
            writer.write(conns[topic], 1_000_000_000 + i * 1_000_000, ts.serialize_cdr(msg, msg.__msgtype__))


if __name__ == "__main__":
    main(sys.argv[1])
