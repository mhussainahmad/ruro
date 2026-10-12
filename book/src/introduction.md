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
  deployment, and message schemas are checked for compatible changes.
- **Observable.** Every node reports its rate, step latency (p50/p99), CPU, input age, drops and
  errors. Any output can be watched live as an image, a plot, a value or a 3D scene. Watching
  costs nothing while nobody is looking.
- **Real-time capable.** Process groups can run with `SCHED_FIFO` priority, pinned to CPUs, or
  deterministically on one thread with per-node deadlines.
- **Connected.** Graphs can span several machines; recordings are MCAP files; a ROS 2 bridge
  talks to ROS 2 systems over rmw_zenoh.
- **Batteries included.** Drivers for cameras, serial, CAN, GPIO, I2C, gamepads, servos, IMUs
  and lidars; MuJoCo simulation; ONNX inference and LeRobot dataset recording; nodes in Rust or
  Python.
- **Native.** The studio is a single native Linux binary built with egui. No browser, no
  Electron, no runtime to install.

## Project status

ruro is early (0.1.0, unreleased). The runtime, node SDKs, CLI, daemon and studio are tested
in CI. Some parts are implemented to their specifications but not yet verified against the
real thing, most importantly the hardware drivers, which have only been tested against
protocol fixtures and simulated devices. [Project status](status.md) lists what is verified
and what is not.

## Where to go next

- [Install ruro](installation.md) and follow the [quick start](quick-start.md).
- Read [Concepts](concepts.md) for the vocabulary used throughout this book.
- [Write your own nodes](writing-nodes.md) in Rust, or in [Python](python.md).
- Look up commands in the [CLI reference](reference/cli.md) and the graph format in the
  [graph file reference](reference/graph-yaml.md).

ruro is dual-licensed under MIT or Apache-2.0.
