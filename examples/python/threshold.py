"""Python nodes used by examples/python-threshold.yaml.

Run with the Python host:

    RURO_PY_MODULES=examples/python/threshold.py \\
        ruro daemon --host target/release/ruro-host-py
    ruro run examples/python-threshold.yaml
"""

import ruro


@ruro.node(
    kind="py/threshold",
    inputs={"value": ruro.Scalar},
    outputs={"high": ruro.Flag},
    params={"limit": ruro.param(0.5, min=-10, max=10, doc="Values above this are high.")},
)
class Threshold:
    """Flags samples above a limit."""

    def step(self, ctx):
        for sample in self.inputs.value:
            self.outputs.high.send(ruro.Flag(sample.value > self.params.limit))


@ruro.node(
    kind="py/brightness",
    inputs={"image": ruro.Image},
    outputs={"mean": ruro.Scalar},
)
class Brightness:
    """Mean pixel value of an image, computed with numpy on the shared buffer."""

    def step(self, ctx):
        image = self.inputs.image.latest()
        if image is not None:
            self.outputs.mean.send(ruro.Scalar(float(image.array().mean())))
