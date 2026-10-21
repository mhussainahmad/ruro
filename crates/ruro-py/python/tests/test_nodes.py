import json

import pytest

import ruro
from ruro.testing import harness


@ruro.node(
    kind="test/threshold",
    inputs={"value": ruro.Scalar},
    outputs={"high": ruro.Flag},
    params={"limit": ruro.param(0.5, min=0, max=1, doc="Threshold.")},
)
class Threshold:
    """Flags samples above a limit.

    Longer description that is not part of the one-line doc.
    """

    def step(self, ctx):
        for sample in self.inputs.value:
            self.outputs.high.send(ruro.Flag(sample.value > self.params.limit))


@ruro.node(
    kind="test/lifecycle",
    inputs={"tick": ruro.Scalar},
    outputs={"log": ruro.Text},
    params={"name": "a", "count": 3, "gain": 1.5, "enabled": True},
    rate=20,
)
class Lifecycle:
    def __init__(self):
        self.events = ["init"]

    def start(self, ctx):
        self.events.append("start")

    def step(self, ctx):
        self.events.append(f"step{ctx.steps}")
        self.outputs.log.send(",".join(self.events))

    def on_param(self, name, value):
        self.events.append(f"{name}={value!r}")


def test_descriptor_comes_from_the_decorator():
    desc = json.loads(ruro._native.describe("test/threshold"))
    assert desc["doc"] == "Flags samples above a limit."
    assert desc["inputs"] == [{"name": "value", "ty": "std/Scalar"}]
    assert desc["outputs"] == [{"name": "high", "ty": "std/Flag"}]
    (limit,) = desc["params"]
    assert limit == {
        "name": "limit",
        "kind": "float",
        "default": 0.5,
        "min": 0.0,
        "max": 1.0,
        "doc": "Threshold.",
    }
    kinds = {p["name"]: p["kind"] for p in json.loads(ruro._native.describe("test/lifecycle"))["params"]}
    assert kinds == {"name": "string", "count": "int", "gain": "float", "enabled": "bool"}
    assert json.loads(ruro._native.describe("test/lifecycle"))["default_rate"] == 20.0
    assert "test/threshold" in json.loads(ruro._native.registry_json())["nodes"]


def test_threshold_flags_values_above_the_limit():
    h = harness(Threshold)
    for v in (0.2, 0.7, 0.5):
        h.push("value", ruro.Scalar(v))
    h.step()
    assert h.take("high") == [ruro.Flag(False), ruro.Flag(True), ruro.Flag(False)]


def test_parameters_from_graph_and_at_run_time():
    h = harness(Threshold, limit=0.1)
    h.push("value", 0.2).step()  # raw values are accepted too
    assert h.take("high") == [ruro.Flag(True)]
    h.set_param("limit", 0.9)
    h.push("value", ruro.Scalar(0.2)).step()
    assert h.take("high") == [ruro.Flag(False)]
    with pytest.raises(RuntimeError, match="expects a Float"):
        h.set_param("limit", "high")
    with pytest.raises(RuntimeError, match="no parameter"):
        h.set_param("nope", 1)


def test_lifecycle_hooks_and_on_param():
    h = harness(Lifecycle, count=5)
    h.step()
    h.set_param("gain", 2)
    h.step()
    logs = [t.value for t in h.take("log")]
    assert logs == ["init,start,step0", "init,start,step0,gain=2.0,step1"]


def test_python_errors_carry_a_traceback():
    @ruro.node(kind="test/broken", outputs={"out": ruro.Scalar})
    class Broken:
        def step(self, ctx):
            raise ValueError("bad sensor")

    h = harness(Broken)
    with pytest.raises(RuntimeError) as err:
        h.step()
    assert "bad sensor" in str(err.value)
    assert "in step" in str(err.value)


def test_sending_the_wrong_type_is_rejected():
    @ruro.node(kind="test/wrong", outputs={"out": ruro.Scalar})
    class Wrong:
        def step(self, ctx):
            self.outputs.out.send(ruro.Text("not a number"))

    with pytest.raises(RuntimeError, match="expected std/Scalar, got std/Text"):
        harness(Wrong).step()


def test_decorator_validation():
    with pytest.raises(ValueError):
        ruro.node(kind="no-category")
    with pytest.raises(TypeError, match="must define step"):

        @ruro.node(kind="test/nostep")
        class NoStep:
            pass

    with pytest.raises(TypeError, match="not a ruro message type"):

        @ruro.node(kind="test/badport", inputs={"x": float})
        class BadPort:
            def step(self, ctx):
                pass

    with pytest.raises(ValueError, match="do not support message type"):

        @ruro.node(kind="test/unsupported", inputs={"x": "my/Custom"})
        class Unsupported:
            def step(self, ctx):
                pass


def test_ports_and_ctx_are_introspectable():
    seen = {}

    @ruro.node(
        kind="test/introspect",
        inputs={"a": ruro.Twist, "b": ruro.Image},
        outputs={"c": ruro.PointCloud},
    )
    class Introspect:
        def step(self, ctx):
            seen["inputs"] = list(self.inputs)
            seen["type"] = self.inputs.a.type
            seen["empty"] = len(self.inputs.a)
            seen["ctx"] = (ctx.id, ctx.steps, ctx.dt >= 0)
            ctx.info("hello from python")

    harness(Introspect).step()
    assert seen == {
        "inputs": ["a", "b"],
        "type": "std/Twist",
        "empty": 0,
        "ctx": ("test", 0, True),
    }


@ruro.node(kind="test/motor", outputs={"cmd": ruro.Scalar}, rate=50)
class Motor:
    def __init__(self):
        self.stops = 0

    def on_estop(self, ctx):
        self.stops += 1
        self.outputs.cmd.send(ruro.Scalar(0.0))

    def step(self, ctx):
        speed = 0.0 if ctx.estopped else 1.0
        self.outputs.cmd.send(ruro.Scalar(speed))


def test_emergency_stop_reaches_python_nodes():
    h = harness(Motor)
    h.step()
    assert [s.value for s in h.take("cmd")] == [1.0]
    h.set_estop(True)
    # on_estop runs once, right away; steps then see ctx.estopped.
    assert [s.value for s in h.take("cmd")] == [0.0]
    h.step()
    h.set_estop(True)
    assert [s.value for s in h.take("cmd")] == [0.0]
    h.set_estop(False)
    h.step()
    assert [s.value for s in h.take("cmd")] == [1.0]


def test_errors_in_on_estop_do_not_stop_the_node():
    @ruro.node(kind="test/bad-estop", outputs={"out": ruro.Scalar})
    class BadEstop:
        def on_estop(self, ctx):
            raise ValueError("no brakes")

        def step(self, ctx):
            self.outputs.out.send(ruro.Scalar(1.0))

    h = harness(BadEstop)
    h.step()
    h.set_estop(True)
    h.step()
    assert len(h.take("out")) == 2
