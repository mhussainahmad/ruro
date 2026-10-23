"""GPU tensors (``gpu/Tensor``) in Python nodes.

Needs an NVIDIA GPU and its driver; skipped otherwise. The device memory is
checked with the CUDA driver API through ctypes, so no GPU library (PyTorch,
CuPy) is needed.
"""

import ctypes
import os

import pytest

import ruro
from ruro.testing import harness

np = pytest.importorskip("numpy")

if ruro._native.cuda_unavailable() and not os.environ.get("RURO_REQUIRE_CUDA"):
    pytest.skip(ruro._native.cuda_unavailable(), allow_module_level=True)


class Driver:
    """The few CUDA driver calls the test needs."""

    def __init__(self):
        self.lib = ctypes.CDLL(os.environ.get("RURO_CUDA_LIB", "libcuda.so.1"))
        self.check(self.lib.cuInit(0))
        dev = ctypes.c_int()
        self.check(self.lib.cuDeviceGet(ctypes.byref(dev), 0))
        ctx = ctypes.c_void_p()
        self.check(self.lib.cuDevicePrimaryCtxRetain(ctypes.byref(ctx), dev))
        self.check(self.lib.cuCtxSetCurrent(ctx))

    @staticmethod
    def check(rc):
        assert rc == 0, f"CUDA error {rc}"

    def read(self, ptr, nbytes):
        out = ctypes.create_string_buffer(nbytes)
        self.check(self.lib.cuMemcpyDtoH_v2(out, ctypes.c_uint64(ptr), ctypes.c_size_t(nbytes)))
        return out.raw

    def alloc(self, data: bytes):
        ptr = ctypes.c_uint64()
        self.check(self.lib.cuMemAlloc_v2(ctypes.byref(ptr), ctypes.c_size_t(len(data))))
        self.check(self.lib.cuMemcpyHtoD_v2(ptr, data, ctypes.c_size_t(len(data))))
        return ptr.value

    def free(self, ptr):
        self.check(self.lib.cuMemFree_v2(ctypes.c_uint64(ptr)))


class ForeignArray:
    """Device memory owned by "another library", as PyTorch or CuPy expose it."""

    def __init__(self, driver, array):
        self.driver = driver
        self.ptr = driver.alloc(array.tobytes())
        self.__cuda_array_interface__ = {
            "shape": array.shape,
            "typestr": array.dtype.str,
            "data": (self.ptr, False),
            "strides": None,
            "version": 3,
            # The legacy default stream: the consumer waits on it.
            "stream": 1,
        }

    def __del__(self):
        self.driver.free(self.ptr)


@pytest.fixture(scope="module")
def driver():
    return Driver()


def test_a_tensor_on_the_device(driver):
    a = np.arange(12, dtype=np.float32).reshape(3, 4)
    t = ruro.GpuTensor.from_numpy(a, device=0)
    assert (t.dtype, t.shape, t.strides, t.device) == ("f32", (3, 4), (4, 1), "cuda:0")
    cai = t.__cuda_array_interface__
    assert cai["shape"] == (3, 4) and cai["typestr"] == "<f4" and cai["version"] == 3
    ptr, readonly = cai["data"]
    assert readonly, "shared memory is read-only"
    # The memory the interface points at holds the array.
    assert driver.read(ptr, a.nbytes) == a.tobytes()
    np.testing.assert_array_equal(t.numpy(), a)

    host = ruro.GpuTensor.from_numpy(a.astype(np.uint8))
    assert host.device == "cpu" and host.dtype == "u8"
    assert not hasattr(host, "__cuda_array_interface__")


@ruro.node(
    kind="test/gpu-double",
    inputs={"x": ruro.GpuTensor},
    outputs={"doubled": ruro.GpuTensor, "same": ruro.GpuTensor},
)
class Double:
    """Doubles a tensor the way a GPU library would hand back its result."""

    def start(self, ctx):
        self.driver = Driver()

    def step(self, ctx):
        for t in self.inputs.x:
            cai = t.__cuda_array_interface__
            x = np.frombuffer(
                self.driver.read(cai["data"][0], int(np.prod(t.shape)) * 4), dtype=np.float32
            ).reshape(t.shape)
            # A result in device memory another library owns...
            self.outputs.doubled.send(ForeignArray(self.driver, x * 2))
            # ...and the received tensor passed on unchanged.
            self.outputs.same.send(t)


def test_a_node_receives_and_sends_device_tensors():
    a = np.linspace(-1, 1, 24, dtype=np.float32).reshape(2, 3, 4)
    h = harness(Double)
    h.push("x", ruro.GpuTensor.from_numpy(a, device=0))
    h.step()
    (doubled,) = h.take("doubled")
    assert doubled.device == "cuda:0" and doubled.shape == (2, 3, 4)
    np.testing.assert_array_equal(doubled.numpy(), a * 2)
    (same,) = h.take("same")
    np.testing.assert_array_equal(same.numpy(), a)


def test_non_contiguous_arrays_are_refused(driver):
    a = np.zeros((4, 4), dtype=np.float32)
    f = ForeignArray(driver, a)
    f.__cuda_array_interface__ = dict(f.__cuda_array_interface__, strides=(4, 16))
    h = harness(Double)

    @ruro.node(kind="test/gpu-forward", outputs={"out": ruro.GpuTensor}, rate=10)
    class Forward:
        def step(self, ctx):
            self.outputs.out.send(f)

    h = harness(Forward)
    with pytest.raises(RuntimeError, match="not contiguous"):
        h.step()
