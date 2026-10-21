"""Defining nodes in Python.

A node is a class decorated with :func:`node`. Its ports and parameters are
declared in the decorator; ruro attaches them to every instance as
``self.inputs``, ``self.outputs`` and ``self.params`` before ``__init__`` runs::

    import ruro

    @ruro.node(
        kind="py/threshold",
        inputs={"value": ruro.Scalar},
        outputs={"high": ruro.Flag},
        params={"limit": ruro.param(0.5, min=0, max=1, doc="Threshold.")},
    )
    class Threshold:
        \"\"\"Flags samples above a limit.\"\"\"

        def step(self, ctx):
            for sample in self.inputs.value:
                self.outputs.high.send(ruro.Flag(sample.value > self.params.limit))

Optional hooks: ``start(self, ctx)``, ``stop(self, ctx)``,
``on_param(self, name, value)`` (called after a parameter changes while the
graph runs) and ``on_estop(self, ctx)`` (called once when the emergency stop
engages; ``ctx.estopped`` stays true until it is released).
"""

from __future__ import annotations

import inspect
from typing import Any, Callable, Dict, Iterator, NamedTuple, Optional, TypeVar, Union

from . import _native

__all__ = ["node", "param", "Param", "Params", "Ports", "registered"]

T = TypeVar("T", bound=type)

#: Node classes registered in this interpreter, by kind.
_REGISTERED: Dict[str, type] = {}


class Param(NamedTuple):
    """A parameter declaration with a range and documentation."""

    default: Union[bool, int, float, str]
    min: Optional[float] = None
    max: Optional[float] = None
    doc: str = ""


def param(
    default: Union[bool, int, float, str],
    *,
    min: Optional[float] = None,
    max: Optional[float] = None,
    doc: str = "",
) -> Param:
    """Declares a parameter. Its type (bool, int, float or str) follows ``default``."""
    return Param(default, min, max, doc)


class Params:
    """A node's current parameter values, as attributes: ``self.params.limit``."""

    def __init__(self, values: Dict[str, Any]) -> None:
        object.__setattr__(self, "_values", dict(values))

    def __getattr__(self, name: str) -> Any:
        try:
            return self._values[name]
        except KeyError:
            raise AttributeError(f"no parameter {name!r}") from None

    def __setattr__(self, name: str, value: Any) -> None:
        self._values[name] = value

    def __getitem__(self, name: str) -> Any:
        return self._values[name]

    def __contains__(self, name: str) -> bool:
        return name in self._values

    def as_dict(self) -> Dict[str, Any]:
        """A copy of all values."""
        return dict(self._values)

    def __repr__(self) -> str:
        items = ", ".join(f"{k}={v!r}" for k, v in self._values.items())
        return f"Params({items})"


class Ports:
    """A node's inputs or outputs, as attributes: ``self.inputs.value``."""

    def __init__(self, ports: Dict[str, Any]) -> None:
        object.__setattr__(self, "_ports", dict(ports))

    def __getattr__(self, name: str) -> Any:
        try:
            return self._ports[name]
        except KeyError:
            raise AttributeError(f"no port {name!r}") from None

    def __setattr__(self, name: str, value: Any) -> None:
        raise AttributeError("ports are declared in @ruro.node")

    def __getitem__(self, name: str) -> Any:
        return self._ports[name]

    def __iter__(self) -> Iterator[str]:
        return iter(self._ports)

    def __len__(self) -> int:
        return len(self._ports)

    def __repr__(self) -> str:
        return f"Ports({', '.join(self._ports)})"


def _type_name(t: Any) -> str:
    if isinstance(t, str):
        return t
    name = getattr(t, "TYPE", None)
    if isinstance(name, str):
        return name
    raise TypeError(f"{t!r} is not a ruro message type (expected e.g. ruro.Scalar)")


def _doc(cls: type, doc: Optional[str]) -> str:
    if doc is not None:
        return doc
    text = inspect.getdoc(cls) or ""
    # First paragraph, on one line.
    return " ".join(text.split("\n\n")[0].split())


def node(
    kind: str,
    *,
    inputs: Optional[Dict[str, Any]] = None,
    outputs: Optional[Dict[str, Any]] = None,
    params: Optional[Dict[str, Any]] = None,
    rate: Optional[float] = None,
    doc: Optional[str] = None,
    reload: bool = False,
) -> Callable[[T], T]:
    """Registers a class as a ruro node type.

    Args:
        kind: Node type name, ``"category/name"``.
        inputs: Port name to message class (``ruro.Scalar``, ``ruro.Image``, ...).
        outputs: Port name to message class.
        params: Parameter name to default value or :func:`param`.
        rate: Default timer rate in Hz. Without one the node steps whenever
            an input receives data.
        doc: One-line description; defaults to the class docstring.
        reload: Re-import the class's module when its file changes (also
            enabled for every node by ``RURO_PY_RELOAD=1``).
    """
    if "/" not in kind:
        raise ValueError(f"node kind {kind!r} must look like 'category/name'")

    def register(cls: T) -> T:
        if not inspect.isclass(cls):
            raise TypeError("@ruro.node decorates a class")
        if not callable(getattr(cls, "step", None)):
            raise TypeError(f"{cls.__name__} must define step(self, ctx)")
        ins = [(n, _type_name(t)) for n, t in (inputs or {}).items()]
        outs = [(n, _type_name(t)) for n, t in (outputs or {}).items()]
        ps = []
        for name, p in (params or {}).items():
            if not isinstance(p, Param):
                p = Param(p)
            ps.append((name, p.default, p.min, p.max, p.doc))
        _native.register(
            kind,
            cls,
            ins,
            outs,
            ps,
            doc=_doc(cls, doc),
            rate=None if rate is None else float(rate),
            reload=reload,
        )
        cls.__ruro_kind__ = kind  # type: ignore[attr-defined]
        _REGISTERED[kind] = cls
        return cls

    return register


def registered() -> Dict[str, type]:
    """Node classes registered in this interpreter, by kind."""
    return dict(_REGISTERED)
