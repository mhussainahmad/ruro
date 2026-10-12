# Introduction

ruro is a fast, lightweight robotics dataflow platform written in Rust, with a native visual
editor.

You build a robot's software as a graph of small nodes (drivers, perception, control) and run
it with zero-copy messaging between nodes in the same process and shared-memory transport
between processes. Graphs are designed, run and inspected in **ruro studio**, or driven entirely
from the `ruro` command line.

![ruro studio running a camera pipeline](images/studio.png)

## Why ruro

- **Fast.** In-process wires move a message in about 150 ns, whether it is a number or a 1080p
  frame, because messages are shared rather than copied. Cross-process wires use iceoryx2
  shared memory: a few microseconds per hop. See [Performance](performance.md).
- **Isolated when you want it.** Every node belongs to a *process group*. Nodes in one group
  share a process; separate groups run as separate OS processes, supervised and restarted on
  crash. Regrouping needs no rebuild.
- **Typed.** Ports carry typed messages. Invalid wires are rejected in the editor and before
  deployment.
- **Observable.** Every node reports its rate, step latency (p50/p99), CPU, input age, drops and
  errors. Any output can be watched live as an image, a plot or a value. Watching costs nothing
  while nobody is looking.
- **Remote-ready.** The daemon is a Zenoh router: point the studio at a robot on the network and
  work with it as if it were local.
- **Native.** The studio is a single native Linux binary built with egui. No browser, no
  Electron, no runtime to install.

## Project status

ruro is early. The core runtime, node SDK, CLI, daemon and studio work end to end. Bridges to
ROS 2, simulators and hardware, a Python SDK, record and replay, real-time scheduling and
multi-machine graphs are in development; their pages are collected under *In development* and
are marked as such.

## Where to go next

- [Install ruro](installation.md) and follow the [quick start](quick-start.md).
- Read [Concepts](concepts.md) for the vocabulary used throughout this book.
- [Write your own nodes](writing-nodes.md) in Rust.
- Look up commands in the [CLI reference](reference/cli.md) and the graph format in the
  [graph file reference](reference/graph-yaml.md).

ruro is dual-licensed under MIT or Apache-2.0.
