"""Write ruro nodes in Python.

Define a node with :func:`ruro.node`, test it in-process with
:mod:`ruro.testing`, and run it in a graph with the ``ruro-host-py`` host::

    RURO_PY_MODULES=my_nodes ruro daemon --host ruro-host-py

Messages: the dataclasses in :mod:`ruro.msg` (``Scalar``, ``Twist``,
``Imu``...) plus :class:`Image` and :class:`PointCloud`, whose data is shared
with numpy without copying (``img.array()``, ``cloud.array()``), and
:class:`GpuTensor` (``gpu/Tensor``), whose device memory PyTorch and CuPy
read in place through ``__cuda_array_interface__``.
"""

from . import _native
from ._native import Ctx, GpuTensor, Image, InputPort, OutputPort, PointCloud
from .msg import (
    MESSAGES,
    Bytes,
    CameraInfo,
    Flag,
    Header,
    Imu,
    JointCommand,
    JointState,
    Joy,
    LaserScan,
    Odometry,
    Pose,
    Quat,
    Scalar,
    Text,
    Transform,
    Transforms,
    Twist,
    Vec3,
)
from .node import Param, Params, Ports, node, param, registered

MESSAGES[Image.TYPE] = Image
MESSAGES[PointCloud.TYPE] = PointCloud
MESSAGES[GpuTensor.TYPE] = GpuTensor

__version__: str = _native.__version__

__all__ = [
    "Bytes",
    "CameraInfo",
    "Ctx",
    "Flag",
    "GpuTensor",
    "Header",
    "Image",
    "Imu",
    "InputPort",
    "JointCommand",
    "JointState",
    "Joy",
    "LaserScan",
    "MESSAGES",
    "Odometry",
    "OutputPort",
    "Param",
    "Params",
    "PointCloud",
    "Ports",
    "Pose",
    "Quat",
    "Scalar",
    "Text",
    "Transform",
    "Transforms",
    "Twist",
    "Vec3",
    "node",
    "param",
    "registered",
]
