# Message classes for ruro, generated from the message schemas by
# `ruro schema --python`. Do not edit by hand.

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, ClassVar, Dict, List, Optional, Tuple

__all__ = ["Scalar", "Text", "Flag", "Bytes", "Vec3", "Quat", "Pose", "Twist", "JointState", "Header", "JointCommand", "Transform", "Transforms", "Imu", "LaserScan", "Odometry", "CameraInfo", "Joy"]

#: Message type name to class, for every message in this module.
MESSAGES: Dict[str, type] = {}


@dataclass
class Scalar:
    """``std/Scalar``"""

    TYPE: ClassVar[str] = "std/Scalar"

    value: float = 0.0

    def _to_raw(self) -> Any:
        return float(self.value)

    @classmethod
    def _from_raw(cls, raw: Any) -> "Scalar":
        return cls(float(raw))


@dataclass
class Text:
    """``std/Text``"""

    TYPE: ClassVar[str] = "std/Text"

    value: str = ""

    def _to_raw(self) -> Any:
        return str(self.value)

    @classmethod
    def _from_raw(cls, raw: Any) -> "Text":
        return cls(raw)


@dataclass
class Flag:
    """``std/Flag``"""

    TYPE: ClassVar[str] = "std/Flag"

    value: bool = False

    def _to_raw(self) -> Any:
        return bool(self.value)

    @classmethod
    def _from_raw(cls, raw: Any) -> "Flag":
        return cls(raw)


@dataclass
class Bytes:
    """``std/Bytes``"""

    TYPE: ClassVar[str] = "std/Bytes"

    value: bytes = b""

    def _to_raw(self) -> Any:
        return list(self.value)

    @classmethod
    def _from_raw(cls, raw: Any) -> "Bytes":
        return cls(bytes(raw))


@dataclass
class Vec3:
    """``std/Vec3``"""

    TYPE: ClassVar[str] = "std/Vec3"

    x: float = 0.0
    y: float = 0.0
    z: float = 0.0

    def _to_raw(self) -> Any:
        return {
            "x": float(self.x),
            "y": float(self.y),
            "z": float(self.z),
        }

    @classmethod
    def _from_raw(cls, raw: Any) -> "Vec3":
        return cls(
            x=float(raw["x"]),
            y=float(raw["y"]),
            z=float(raw["z"]),
        )


@dataclass
class Quat:
    """"""

    x: float = 0.0
    y: float = 0.0
    z: float = 0.0
    w: float = 1.0

    def _to_raw(self) -> Any:
        return {
            "x": float(self.x),
            "y": float(self.y),
            "z": float(self.z),
            "w": float(self.w),
        }

    @classmethod
    def _from_raw(cls, raw: Any) -> "Quat":
        return cls(
            x=float(raw["x"]),
            y=float(raw["y"]),
            z=float(raw["z"]),
            w=float(raw["w"]),
        )


@dataclass
class Pose:
    """``std/Pose``"""

    TYPE: ClassVar[str] = "std/Pose"

    position: Vec3 = field(default_factory=Vec3)
    orientation: Quat = field(default_factory=Quat)

    def _to_raw(self) -> Any:
        return {
            "position": self.position._to_raw(),
            "orientation": self.orientation._to_raw(),
        }

    @classmethod
    def _from_raw(cls, raw: Any) -> "Pose":
        return cls(
            position=Vec3._from_raw(raw["position"]),
            orientation=Quat._from_raw(raw["orientation"]),
        )


@dataclass
class Twist:
    """``std/Twist``"""

    TYPE: ClassVar[str] = "std/Twist"

    linear: Vec3 = field(default_factory=Vec3)
    """m/s"""
    angular: Vec3 = field(default_factory=Vec3)
    """rad/s"""

    def _to_raw(self) -> Any:
        return {
            "linear": self.linear._to_raw(),
            "angular": self.angular._to_raw(),
        }

    @classmethod
    def _from_raw(cls, raw: Any) -> "Twist":
        return cls(
            linear=Vec3._from_raw(raw["linear"]),
            angular=Vec3._from_raw(raw["angular"]),
        )


@dataclass
class JointState:
    """``std/JointState``"""

    TYPE: ClassVar[str] = "std/JointState"

    position: List[float] = field(default_factory=list)
    velocity: List[float] = field(default_factory=list)
    effort: List[float] = field(default_factory=list)

    def _to_raw(self) -> Any:
        return {
            "position": [float(x) for x in self.position],
            "velocity": [float(x) for x in self.velocity],
            "effort": [float(x) for x in self.effort],
        }

    @classmethod
    def _from_raw(cls, raw: Any) -> "JointState":
        return cls(
            position=list(raw["position"]),
            velocity=list(raw["velocity"]),
            effort=list(raw["effort"]),
        )


@dataclass
class Header:
    """"""

    stamp_ns: int = 0
    """Measurement time in nanoseconds since the Unix epoch."""
    frame_id: str = ""
    """Coordinate frame the data is expressed in."""

    def _to_raw(self) -> Any:
        return {
            "stamp_ns": int(self.stamp_ns),
            "frame_id": str(self.frame_id),
        }

    @classmethod
    def _from_raw(cls, raw: Any) -> "Header":
        return cls(
            stamp_ns=raw["stamp_ns"],
            frame_id=raw["frame_id"],
        )


@dataclass
class JointCommand:
    """``std/JointCommand``"""

    TYPE: ClassVar[str] = "std/JointCommand"

    header: Header = field(default_factory=Header)
    position: List[float] = field(default_factory=list)
    velocity: List[float] = field(default_factory=list)
    effort: List[float] = field(default_factory=list)

    def _to_raw(self) -> Any:
        return {
            "header": self.header._to_raw(),
            "position": [float(x) for x in self.position],
            "velocity": [float(x) for x in self.velocity],
            "effort": [float(x) for x in self.effort],
        }

    @classmethod
    def _from_raw(cls, raw: Any) -> "JointCommand":
        return cls(
            header=Header._from_raw(raw["header"]),
            position=list(raw["position"]),
            velocity=list(raw["velocity"]),
            effort=list(raw["effort"]),
        )


@dataclass
class Transform:
    """``std/Transform``"""

    TYPE: ClassVar[str] = "std/Transform"

    header: Header = field(default_factory=Header)
    child_frame_id: str = ""
    translation: Vec3 = field(default_factory=Vec3)
    rotation: Quat = field(default_factory=Quat)

    def _to_raw(self) -> Any:
        return {
            "header": self.header._to_raw(),
            "child_frame_id": str(self.child_frame_id),
            "translation": self.translation._to_raw(),
            "rotation": self.rotation._to_raw(),
        }

    @classmethod
    def _from_raw(cls, raw: Any) -> "Transform":
        return cls(
            header=Header._from_raw(raw["header"]),
            child_frame_id=raw["child_frame_id"],
            translation=Vec3._from_raw(raw["translation"]),
            rotation=Quat._from_raw(raw["rotation"]),
        )


@dataclass
class Transforms:
    """``std/Transforms``"""

    TYPE: ClassVar[str] = "std/Transforms"

    value: List[Transform] = field(default_factory=list)

    def _to_raw(self) -> Any:
        return [x._to_raw() for x in self.value]

    @classmethod
    def _from_raw(cls, raw: Any) -> "Transforms":
        return cls([Transform._from_raw(x) for x in raw])


@dataclass
class Imu:
    """``std/Imu``"""

    TYPE: ClassVar[str] = "std/Imu"

    header: Header = field(default_factory=Header)
    orientation: Quat = field(default_factory=Quat)
    orientation_covariance: List[float] = field(default_factory=lambda: [0.0] * 9)
    angular_velocity: Vec3 = field(default_factory=Vec3)
    """rad/s"""
    angular_velocity_covariance: List[float] = field(default_factory=lambda: [0.0] * 9)
    linear_acceleration: Vec3 = field(default_factory=Vec3)
    """m/s²"""
    linear_acceleration_covariance: List[float] = field(default_factory=lambda: [0.0] * 9)

    def _to_raw(self) -> Any:
        return {
            "header": self.header._to_raw(),
            "orientation": self.orientation._to_raw(),
            "orientation_covariance": [float(x) for x in self.orientation_covariance],
            "angular_velocity": self.angular_velocity._to_raw(),
            "angular_velocity_covariance": [float(x) for x in self.angular_velocity_covariance],
            "linear_acceleration": self.linear_acceleration._to_raw(),
            "linear_acceleration_covariance": [float(x) for x in self.linear_acceleration_covariance],
        }

    @classmethod
    def _from_raw(cls, raw: Any) -> "Imu":
        return cls(
            header=Header._from_raw(raw["header"]),
            orientation=Quat._from_raw(raw["orientation"]),
            orientation_covariance=list(raw["orientation_covariance"]),
            angular_velocity=Vec3._from_raw(raw["angular_velocity"]),
            angular_velocity_covariance=list(raw["angular_velocity_covariance"]),
            linear_acceleration=Vec3._from_raw(raw["linear_acceleration"]),
            linear_acceleration_covariance=list(raw["linear_acceleration_covariance"]),
        )


@dataclass
class LaserScan:
    """``std/LaserScan``"""

    TYPE: ClassVar[str] = "std/LaserScan"

    header: Header = field(default_factory=Header)
    angle_min: float = 0.0
    """rad"""
    angle_max: float = 0.0
    """rad"""
    angle_increment: float = 0.0
    """rad"""
    time_increment: float = 0.0
    """s"""
    scan_time: float = 0.0
    """s"""
    range_min: float = 0.0
    """m"""
    range_max: float = 0.0
    """m"""
    ranges: List[float] = field(default_factory=list)
    intensities: List[float] = field(default_factory=list)

    def _to_raw(self) -> Any:
        return {
            "header": self.header._to_raw(),
            "angle_min": float(self.angle_min),
            "angle_max": float(self.angle_max),
            "angle_increment": float(self.angle_increment),
            "time_increment": float(self.time_increment),
            "scan_time": float(self.scan_time),
            "range_min": float(self.range_min),
            "range_max": float(self.range_max),
            "ranges": [float(x) for x in self.ranges],
            "intensities": [float(x) for x in self.intensities],
        }

    @classmethod
    def _from_raw(cls, raw: Any) -> "LaserScan":
        return cls(
            header=Header._from_raw(raw["header"]),
            angle_min=float(raw["angle_min"]),
            angle_max=float(raw["angle_max"]),
            angle_increment=float(raw["angle_increment"]),
            time_increment=float(raw["time_increment"]),
            scan_time=float(raw["scan_time"]),
            range_min=float(raw["range_min"]),
            range_max=float(raw["range_max"]),
            ranges=list(raw["ranges"]),
            intensities=list(raw["intensities"]),
        )


@dataclass
class Odometry:
    """``std/Odometry``"""

    TYPE: ClassVar[str] = "std/Odometry"

    header: Header = field(default_factory=Header)
    child_frame_id: str = ""
    pose: Pose = field(default_factory=Pose)
    twist: Twist = field(default_factory=Twist)

    def _to_raw(self) -> Any:
        return {
            "header": self.header._to_raw(),
            "child_frame_id": str(self.child_frame_id),
            "pose": self.pose._to_raw(),
            "twist": self.twist._to_raw(),
        }

    @classmethod
    def _from_raw(cls, raw: Any) -> "Odometry":
        return cls(
            header=Header._from_raw(raw["header"]),
            child_frame_id=raw["child_frame_id"],
            pose=Pose._from_raw(raw["pose"]),
            twist=Twist._from_raw(raw["twist"]),
        )


@dataclass
class CameraInfo:
    """``std/CameraInfo``"""

    TYPE: ClassVar[str] = "std/CameraInfo"

    header: Header = field(default_factory=Header)
    width: int = 0
    height: int = 0
    distortion_model: str = ""
    d: List[float] = field(default_factory=list)
    k: List[float] = field(default_factory=lambda: [0.0] * 9)
    """Row-major 3x3 intrinsic matrix."""
    r: List[float] = field(default_factory=lambda: [0.0] * 9)
    p: List[float] = field(default_factory=lambda: [0.0] * 12)
    """Row-major 3x4 projection matrix."""

    def _to_raw(self) -> Any:
        return {
            "header": self.header._to_raw(),
            "width": int(self.width),
            "height": int(self.height),
            "distortion_model": str(self.distortion_model),
            "d": [float(x) for x in self.d],
            "k": [float(x) for x in self.k],
            "r": [float(x) for x in self.r],
            "p": [float(x) for x in self.p],
        }

    @classmethod
    def _from_raw(cls, raw: Any) -> "CameraInfo":
        return cls(
            header=Header._from_raw(raw["header"]),
            width=raw["width"],
            height=raw["height"],
            distortion_model=raw["distortion_model"],
            d=list(raw["d"]),
            k=list(raw["k"]),
            r=list(raw["r"]),
            p=list(raw["p"]),
        )


@dataclass
class Joy:
    """``std/Joy``"""

    TYPE: ClassVar[str] = "std/Joy"

    header: Header = field(default_factory=Header)
    axes: List[float] = field(default_factory=list)
    buttons: List[int] = field(default_factory=list)

    def _to_raw(self) -> Any:
        return {
            "header": self.header._to_raw(),
            "axes": [float(x) for x in self.axes],
            "buttons": [int(x) for x in self.buttons],
        }

    @classmethod
    def _from_raw(cls, raw: Any) -> "Joy":
        return cls(
            header=Header._from_raw(raw["header"]),
            axes=list(raw["axes"]),
            buttons=list(raw["buttons"]),
        )

MESSAGES[Scalar.TYPE] = Scalar
MESSAGES[Text.TYPE] = Text
MESSAGES[Flag.TYPE] = Flag
MESSAGES[Bytes.TYPE] = Bytes
MESSAGES[Vec3.TYPE] = Vec3
MESSAGES[Pose.TYPE] = Pose
MESSAGES[Twist.TYPE] = Twist
MESSAGES[JointState.TYPE] = JointState
MESSAGES[JointCommand.TYPE] = JointCommand
MESSAGES[Transform.TYPE] = Transform
MESSAGES[Transforms.TYPE] = Transforms
MESSAGES[Imu.TYPE] = Imu
MESSAGES[LaserScan.TYPE] = LaserScan
MESSAGES[Odometry.TYPE] = Odometry
MESSAGES[CameraInfo.TYPE] = CameraInfo
MESSAGES[Joy.TYPE] = Joy
