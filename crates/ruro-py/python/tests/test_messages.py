import math

import numpy as np
import pytest

import ruro
from ruro.testing import harness


@ruro.node(
    kind="test/echo-all",
    inputs={name: getattr(ruro, name) for name in (
        "Scalar", "Text", "Flag", "Bytes", "Vec3", "Pose", "Twist", "JointState",
        "JointCommand", "Transform", "Transforms", "Imu", "LaserScan", "Odometry",
        "CameraInfo", "Joy", "Image", "PointCloud",
    )},
    outputs={f"{name}_out": getattr(ruro, name) for name in (
        "Scalar", "Text", "Flag", "Bytes", "Vec3", "Pose", "Twist", "JointState",
        "JointCommand", "Transform", "Transforms", "Imu", "LaserScan", "Odometry",
        "CameraInfo", "Joy", "Image", "PointCloud",
    )},
)
class EchoAll:
    """Forwards every input to the matching output."""

    def step(self, ctx):
        for name in self.inputs:
            for msg in self.inputs[name]:
                self.outputs[f"{name}_out"].send(msg)


def roundtrip(name, msg):
    h = harness(EchoAll)
    h.push(name, msg).step()
    (out,) = h.take(f"{name}_out")
    return out


@pytest.mark.parametrize(
    "name,msg",
    [
        ("Scalar", ruro.Scalar(2.5)),
        ("Text", ruro.Text("héllo")),
        ("Flag", ruro.Flag(True)),
        ("Bytes", ruro.Bytes(b"\x00\x01\xff")),
        ("Vec3", ruro.Vec3(1.0, -2.0, 3.5)),
        ("Pose", ruro.Pose(ruro.Vec3(1, 2, 3), ruro.Quat(0, 0, 0.7071, 0.7071))),
        ("Twist", ruro.Twist(linear=ruro.Vec3(x=0.5), angular=ruro.Vec3(z=-1.0))),
        ("JointState", ruro.JointState(position=[0.1, 0.2], velocity=[], effort=[1.0])),
        ("JointCommand", ruro.JointCommand(header=ruro.Header(5, "arm"), position=[1.0])),
        (
            "Transforms",
            ruro.Transforms(
                [ruro.Transform(ruro.Header(1, "map"), "odom", ruro.Vec3(1, 0, 0), ruro.Quat())]
            ),
        ),
        ("Imu", ruro.Imu(header=ruro.Header(7, "imu"), angular_velocity=ruro.Vec3(0.1, 0, 0))),
        ("Odometry", ruro.Odometry(header=ruro.Header(3, "odom"), child_frame_id="base")),
        ("CameraInfo", ruro.CameraInfo(width=640, height=480, d=[0.1] * 5)),
        ("Joy", ruro.Joy(axes=[0.5, -1.0], buttons=[1, 0, 1])),
    ],
)
def test_messages_roundtrip(name, msg):
    assert roundtrip(name, msg) == msg


def test_defaults_match_rust():
    assert ruro.Quat() == ruro.Quat(0.0, 0.0, 0.0, 1.0)
    assert ruro.Imu().orientation_covariance == [0.0] * 9
    assert ruro.Pose().orientation.w == 1.0


def test_non_finite_floats_survive():
    scan = ruro.LaserScan(ranges=[1.0, math.inf, math.nan], range_max=10.0)
    out = roundtrip("LaserScan", scan)
    assert out.ranges[0] == 1.0
    assert math.isinf(out.ranges[1])
    assert math.isnan(out.ranges[2])


def test_raw_forms_are_accepted():
    assert roundtrip("Scalar", 3) == ruro.Scalar(3.0)
    assert roundtrip("Twist", {"linear": {"x": 1, "y": 0, "z": 0}, "angular": {"x": 0, "y": 0, "z": 2}}) == ruro.Twist(
        ruro.Vec3(1, 0, 0), ruro.Vec3(0, 0, 2)
    )
    with pytest.raises(ValueError, match="not a valid std/Vec3"):
        roundtrip("Vec3", {"x": 1})


def test_image_is_a_zero_copy_numpy_view():
    pixels = np.arange(4 * 6 * 3, dtype=np.uint8).reshape(4, 6, 3)
    sent = ruro.Image.from_array(pixels)
    assert (sent.width, sent.height, sent.encoding) == (6, 4, "rgb8")
    assert not sent.shared

    got = roundtrip("Image", sent)
    assert got.shared, "received images wrap the shared sample"
    view = got.array()
    assert view.shape == (4, 6, 3)
    np.testing.assert_array_equal(view, pixels)
    # Views alias the message's memory instead of copying it...
    assert np.shares_memory(view, np.asarray(got))
    assert np.shares_memory(view, got.array())
    # ...and are read-only, because other nodes see the same bytes.
    assert not view.flags.writeable
    assert memoryview(got).readonly
    with pytest.raises(TypeError, match="read-only"):
        memoryview(got)[0] = 1


def test_image_outputs_accept_numpy_arrays():
    mono = np.full((3, 5), 7, dtype=np.uint8)
    got = roundtrip("Image", mono)
    assert got.encoding == "mono8"
    assert got.shape == (3, 5, 1)
    assert got.tobytes() == bytes([7] * 15)
    with pytest.raises(ValueError, match="C-contiguous"):
        ruro.Image.from_array(np.zeros((4, 4, 3), dtype=np.uint8)[:, ::2])
    with pytest.raises(ValueError, match="needs 12 bytes"):
        ruro.Image(2, 2, "rgb8", b"\x00" * 3)


def test_point_cloud_is_a_zero_copy_numpy_view():
    pts = np.array([[1, 2, 3], [4, 5, 6]], dtype=np.float32)
    got = roundtrip("PointCloud", pts)
    assert got.shared and len(got) == 2
    view = got.array()
    np.testing.assert_array_equal(view, pts)
    assert np.shares_memory(view, got.array())
    assert not view.flags.writeable
    assert got.tolist() == [(1.0, 2.0, 3.0), (4.0, 5.0, 6.0)]
    assert len(roundtrip("PointCloud", [(0, 0, 0)])) == 1
