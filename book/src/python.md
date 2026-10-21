# Python nodes

Python nodes let you use the Python ecosystem (NumPy, OpenCV, PyTorch) inside a ruro graph,
next to Rust nodes, with the same ports, parameters, metrics and studio support. They run in
`ruro-host-py`, a host with an embedded Python interpreter that also carries every node of
the standard `ruro-host` (plus the `tf/*` transform nodes).

> **Status:** implemented and tested: the Python package's tests (pytest) and Rust tests
> that run Python and Rust nodes in one graph through a daemon. The Python host is not part
> of the Debian package, AppImage or Docker image; build it from source.

## A Python node

```python
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
```

`@ruro.node(kind, inputs=..., outputs=..., params=..., rate=None, doc=None, reload=False)`
registers the class as a node type:

- `kind` must contain a `/`; `py/` is a convention, not a requirement.
- `inputs` and `outputs` map port names to message types. `params` maps names to a default
  value, or to `ruro.param(default, min=..., max=..., doc=...)`.
- `rate` is the default timer rate; without one, the node runs when an input receives data.
- The class must define `step(self, ctx)`. Optional hooks: `start(self, ctx)`,
  `stop(self, ctx)`, `on_param(self, name, value)` and `on_estop(self, ctx)`. `on_estop`
  runs once when the emergency stop engages; a node driving actuators should command a safe
  state there and keep doing so while `ctx.estopped` is true. An exception in it is logged
  and the node keeps stepping. In tests, `harness.set_estop(True)` engages the stop.
- The class docstring becomes the node's description in the studio.

Inputs (`self.inputs.<name>`) can be iterated (oldest first) and have `recv()`, `latest()`
and `drain()`. Outputs (`self.outputs.<name>`) have `send(msg)`. Parameters are attributes
of `self.params`. `ctx` has `id`, `dt`, `time`, `steps`, `estopped` and logging methods
`debug`, `info`, `warn` and `error`, which go to the graph's logs.

## Messages

`ruro.msg` has a dataclass for each standard message (`Scalar`, `Text`, `Flag`, `Bytes`,
`Vec3`, `Quat`, `Pose`, `Twist`, `JointState`, `JointCommand`, `Transform`, `Transforms`,
`Imu`, `LaserScan`, `Odometry`, `CameraInfo`, `Joy`, `Header`); they are re-exported from
`ruro`. The module is generated from the Rust definitions with `ruro schema --python`.

`ruro.Image` and `ruro.PointCloud` are native types whose data is shared with NumPy without
copying: `image.array()` is a read-only `(height, width, channels)` view, and
`ruro.Image.from_array(array)` builds an image from a `uint8` array.

```python
@ruro.node(kind="py/brightness", inputs={"image": ruro.Image}, outputs={"mean": ruro.Scalar})
class Brightness:
    """Mean pixel value of an image, computed with numpy on the shared buffer."""

    def step(self, ctx):
        image = self.inputs.image.latest()
        if image is not None:
            self.outputs.mean.send(ruro.Scalar(float(image.array().mean())))
```

## Testing

`ruro.testing` runs a node in-process. Messages pass through the same encoding that
cross-process wires use, so a node sees exactly what it would see in a running graph:

```python
from ruro.testing import harness

def test_threshold():
    h = harness(Threshold, limit=0.5)
    h.push("value", ruro.Scalar(0.9))
    h.step()
    assert h.take("high") == [ruro.Flag(True)]
```

`harness(...)` also has `set_param(name, value)`, and `step(n)` steps several times.

## Building

Python 3.11 or newer is required, with its shared library (on Debian and Ubuntu,
`python3-dev`). The host links the Python it was built against: `pyo3` finds `python3` on
`PATH`, or the interpreter named by `PYO3_PYTHON`.

```sh
cargo build --release -p ruro-host-py
```

The host carries the package's native module, but takes its Python files from an
installed `ruro` package or, failing that, from `crates/ruro-py/python` in the source tree
it was built from (the path is recorded at build time). Running the host from its checkout
therefore needs no installation. When the host runs anywhere else (copied to a robot,
uploaded with `ruro deploy --host-binary`, or after the checkout moved), install the `ruro`
package into the environment the host uses, as below. NumPy is needed for `array()`;
install it in that environment too.

To install the package, for example to run node tests with pytest, build it in a virtual
environment with [maturin](https://www.maturin.rs):

```sh
python3 -m venv .venv && source .venv/bin/activate
pip install 'maturin>=1.9,<2'
maturin develop -m crates/ruro-py/Cargo.toml --extras test   # numpy and pytest
pytest crates/ruro-py                                        # the package's own tests
```

## Running Python nodes

The host imports node modules when it starts:

- `RURO_PY_MODULES`: comma-separated module names or `.py` file paths;
- installed Python distributions that declare a `ruro.nodes` entry point, for example in
  `pyproject.toml`:

  ```toml
  [project.entry-points."ruro.nodes"]
  acme = "acme_nodes"
  ```

It adds the active virtual environment's `site-packages` (`$VIRTUAL_ENV`) to the module
search path, and the entries of `RURO_PY_PATH` before it. A module that fails to import is
reported (`ruro-host-py: failed to import ...`) and the host still offers its other nodes.

Point the daemon at the Python host; hosts inherit the daemon's environment:

```sh
RURO_PY_MODULES=examples/python/threshold.py \
    ruro daemon --host target/release/ruro-host-py
ruro run examples/python-threshold.yaml
```

`ruro nodes` and the studio's node library then list the Python node types, and
`ruro check` validates graphs that use them. Alternatively, run only some process groups
with the Python host through `groups.<name>.host: /path/to/ruro-host-py`; the daemon's node
list then does not include the Python types, so `ruro check` against it reports them as
unknown, but the graph deploys.

A Python node can share a process with Rust nodes (as in the example) or have its own
process group, which isolates it: a crash or a hang in Python code then only restarts that
group.

### Reloading on change

With `reload=True` on a node, or `RURO_PY_RELOAD=1` for all of them, the host checks the
node's module file at most every half second. When it changes, the module is re-imported and
the node is replaced by a new instance with the same ports and parameters (`stop` is called
on the old one and `start` on the new one).
