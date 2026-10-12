# Python nodes

> **Status:** in development. This page describes the planned interface and will be verified
> before release.

Python nodes let you use the Python ecosystem (NumPy, OpenCV, PyTorch) inside a ruro graph,
next to Rust nodes, with the same ports, parameters, metrics and studio support.

## Planned design

- A Python package exposes the node API: a node is a Python class that declares typed inputs,
  outputs and parameters and implements `step`, mirroring the Rust `Node` trait.
- Python node types are discovered when a host starts and registered at run time, so they
  appear in `ruro nodes`, in the studio's node library and in graph validation like any other
  type. The registry already supports run-time registration for this purpose
  (`ruro_node::register_dynamic`).
- Python nodes run inside a host process and exchange the standard message types with Rust
  nodes; images are expected to map to NumPy arrays.
- Because a node's process group is chosen in the graph file, Python nodes can be isolated in
  their own group, or share a process with Rust nodes.

Until this lands, the way to integrate Python code is to run it as a separate program and
bridge it to ruro through a custom Rust node.
