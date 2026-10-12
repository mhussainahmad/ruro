# Concepts

| | |
|---|---|
| **Node** | A struct with typed input and output ports and parameters. Implements `step`. |
| **Graph** | Node instances and wires, stored as YAML. The studio and the CLI read and write the same file. |
| **Process group** | Nodes that share an OS process. Same group: zero-copy. Different groups: shared memory, crash isolation. |
| **Host** | The binary that runs a group. Every node crate linked into it is available to graphs. |
| **Daemon** | One per machine. Validates and deploys graphs, supervises hosts, serves metrics, logs and live values. |

## Nodes

A node type is a Rust struct annotated with `#[derive(Node)]`. It declares:

- **inputs** (`Input<T>`) and **outputs** (`Output<T>`), each carrying one message type;
- **parameters**: plain fields with a default value and optional range, editable while the
  graph runs;
- **state**: any other fields, which start as `Default::default()`.

Its type name, such as `std/blur`, is how graphs refer to it. The derive generates a
*descriptor* (ports, parameter ranges, documentation from doc comments) that the studio uses
for its node library, port colours, type checks and parameter editors.

A node runs on a timer when it has a `rate`, otherwise whenever any of its inputs receives
data. Each running node gets its own thread, except in
[deterministic groups](realtime.md), which step all their nodes on one thread.

## Messages

Ports carry typed messages. ruro ships standard types (`Scalar`, `Image`, `PointCloud`,
`Pose`, `JointState`, `Imu`, `LaserScan`, `Odometry`, ...) and you can define your own with
`#[derive(Message)]`.

Within a process, a message is sent once and shared between all receivers by reference count:
a 1080p frame costs the same to pass along as a number. Between processes, messages are
serialised into iceoryx2 shared memory; between machines, they travel over Zenoh.

## Graphs and wires

A graph names node instances and connects output ports to input ports with wires:

```yaml
wires:
  - camera.image -> blur.image
```

A wire is only valid between ports of the same message type. Each input keeps the newest
message by default; set `depth` on a wire to queue more. One output can feed any number of
inputs, and one input can receive from several outputs.

## Process groups

Every node instance has a `group` (default `main`). All nodes in a group run in one OS process:

- **same group**: messages pass by reference, about 150 ns per hop;
- **different groups**: messages cross through shared memory, a few microseconds per hop for
  small messages, and a crash in one group does not take down the others.

A group can also be configured to run with real-time priority, pinned to CPUs or
deterministically ([Real-time scheduling](realtime.md)), with a different host binary, or on
another machine ([Multi-machine graphs](multi-machine.md)).

Grouping is decided in the graph file, not in code, so you can move a flaky driver into its
own process, or merge a perception pipeline into one, without rebuilding anything.

## Hosts

A *host* is the executable that runs one process group. The standard `ruro-host` links the
standard node libraries: `std/*` (synthetic sources, image processing, control),
[`hw/*`](hardware.md), [`sim/*`](simulation.md), [`ml/*`](ml.md) and [`ros2/*`](ros2.md).
`ruro-host-py` adds [Python nodes](python.md). To use your own nodes, build a host that
links your node crates (`ruro build`, see [Node packages and hosts](packages.md)) and point
the daemon at it with `ruro daemon --host`.

`ruro-host describe` prints the descriptors of every linked node type as JSON; the daemon,
CLI and studio use it to learn which node types exist.

## The daemon

One daemon runs per machine (`ruro daemon`). It:

- runs the Zenoh router every other ruro process connects to;
- validates and deploys graphs, starting one host process per group;
- supervises those processes and restarts them according to each node's `restart` policy,
  with exponential backoff;
- relays metrics, logs, live output previews ("taps") and parameter changes between hosts and
  clients.

The CLI and the studio are clients. They connect to `tcp/127.0.0.1:7447` by default; pass
`--connect` (or set `RURO_CONNECT`) to work with a daemon on another machine. Host processes
are tied to their daemon: if the daemon exits, they are stopped too. Daemons on several
machines can be connected so that one graph spans them.

## Observability

Every node reports its state, rate, step latency (p50/p99), CPU usage, input age, drops and
errors, and timer-driven nodes their wakeup jitter (and deadline misses and overruns when
configured). `ruro status` prints them; the studio shows them on the canvas and in the
inspector.

Any output port can be *tapped*: the host sends a preview (an image thumbnail, a value or a
plot sample) only while someone is watching, so unwatched taps cost nothing.
