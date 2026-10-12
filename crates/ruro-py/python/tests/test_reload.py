import importlib
import os
import sys
import textwrap
import time

import ruro
from ruro.testing import harness

SOURCE = """
import ruro

@ruro.node(kind="test/reloadable", outputs={{"out": ruro.Scalar}}, reload=True)
class Reloadable:
    def step(self, ctx):
        self.outputs.out.send(ruro.Scalar({value}))
"""


def write(path, value, mtime):
    path.write_text(textwrap.dedent(SOURCE.format(value=value)))
    os.utime(path, (mtime, mtime))


def test_node_module_is_reloaded_when_its_file_changes(tmp_path, monkeypatch):
    module = tmp_path / "reloadable_nodes.py"
    write(module, 1.0, 1_000_000)
    monkeypatch.syspath_prepend(str(tmp_path))
    importlib.invalidate_caches()
    import reloadable_nodes  # noqa: F401

    h = harness("test/reloadable")
    h.step()
    assert h.take("out") == [ruro.Scalar(1.0)]

    write(module, 2.0, 2_000_000)
    time.sleep(0.6)  # reload checks are rate limited
    h.step()
    assert h.take("out") == [ruro.Scalar(2.0)]

    # A broken edit keeps the previous code running.
    module.write_text("this is not python")
    os.utime(module, (3_000_000, 3_000_000))
    time.sleep(0.6)
    h.step()
    assert h.take("out") == [ruro.Scalar(2.0)]
    sys.modules.pop("reloadable_nodes", None)
