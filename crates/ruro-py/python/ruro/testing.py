"""Testing nodes in-process.

::

    from ruro.testing import harness

    def test_threshold():
        h = harness(Threshold, limit=0.5)
        h.push("value", ruro.Scalar(0.9))
        h.step()
        assert h.take("high") == [ruro.Flag(True)]

Messages pass through the same encoding that cross-process wires use, so a
node sees exactly what it would see in a running graph.
"""

from __future__ import annotations

from typing import Any, List, Union

from . import _native

__all__ = ["Harness", "harness"]


class Harness:
    """One node instance with direct access to its ports."""

    def __init__(self, node: Union[type, str], **params: Any) -> None:
        kind = node if isinstance(node, str) else getattr(node, "__ruro_kind__", None)
        if kind is None:
            raise TypeError(f"{node!r} is not a @ruro.node class or kind")
        self.kind: str = kind
        self._inner = _native.Harness(kind, params or None)

    def push(self, input: str, msg: Any) -> "Harness":
        """Delivers a message to an input. Returns self for chaining."""
        self._inner.push(input, msg)
        return self

    def step(self, times: int = 1) -> "Harness":
        """Runs ``times`` steps. Returns self for chaining."""
        for _ in range(times):
            self._inner.step()
        return self

    def take(self, output: str) -> List[Any]:
        """Removes and returns everything ``output`` has sent so far."""
        return list(self._inner.take(output))

    def set_param(self, name: str, value: Any) -> None:
        """Changes a parameter as the control plane would while a graph runs."""
        self._inner.set_param(name, value)


def harness(node: Union[type, str], **params: Any) -> Harness:
    """A :class:`Harness` for a node class (or registered kind) with parameter overrides."""
    return Harness(node, **params)
