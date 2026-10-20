# ruro

Fast, lightweight robotics dataflow in Rust, with a native visual editor.

Build a robot's software as a graph of small nodes (drivers, perception, control) and run it
with zero-copy messaging between nodes in the same process and shared-memory transport
between processes. Design, run and inspect graphs in **ruro studio**, or drive everything from
the `ruro` CLI.

![ruro studio running a camera pipeline](book/src/images/studio.png)

## Features

- **Fast messaging.** In-process wires move a message in about 150 ns, whatever its size,
  because messages are shared rather than copied. Cross-process wires use iceoryx2 shared
  memory: a few microseconds per hop. Pooled sends avoid allocation in hot loops.
- **GPU buffers.** Images and tensors that stay on the GPU: shared without copying in a
  process, and passed between processes as CUDA IPC handles, so a 1080p frame crosses
  processes in a few microseconds. No CUDA toolkit needed to build.
- **Process groups.** Nodes in one group share a process; separate groups are separate OS
  processes, supervised and restarted on crash. Regrouping needs no rebuild.
- **Typed graphs.** YAML graph files, typed ports, validation before deployment, composite
  nodes (subgraphs), and message schemas with compatibility checks.
- **Simulation and the real robot in one file.** Overlays (`--overlay sim`,
  `--overlay real`) switch drivers, parameters and clocks without copying the graph.
- **Observability.** Per-node rate, latency, CPU, input age, drops, errors and timer jitter;
  live views of any output as an image, plot, value or 3D scene.
- **Studio.** A native egui editor: tabs, undo/redo, subgraphs, comparing versions, live
  parameter tuning, recording and replay timeline, an emergency-stop button (<kbd>F12</kbd>),
  fleet view with live metrics from every machine of a graph, ROS 2 topic browser, light and
  dark themes, plugins.
- **Real-time tier.** `SCHED_FIFO` priority, CPU pinning, memory locking, deterministic
  single-thread groups and per-node deadlines; simulated graph time; a software e-stop.
- **Record and replay.** MCAP recordings of every output of a graph, into one file even
  across machines, replay into a running graph, and lossless import of ROS 2 bags.
- **Multi-machine.** Daemons connect as peers; one graph can span several computers, with
  an emergency stop that reaches all of them, TLS, mutual TLS and token authentication, and
  signed over-the-air daemon updates with automatic rollback.
- **Node SDKs.** Rust (`#[derive(Node)]`, `ruro new`, `ruro build`) and Python (with
  NumPy views of images and hot reload).
- **ROS 2 topics, services and actions, any message type, and your launch files.** Over
  rmw_zenoh with ROS 2 QoS, without a ROS installation on the ruro side. Messages of any
  type pass through byte for byte; `ruro ros2 import` turns launch files into graphs, and
  `ros2/node` runs your existing ROS 2 nodes under ruro's supervision, so a ROS 2 system
  can move over piece by piece.
- **Node libraries.** Cameras (V4L2, MJPEG over HTTP, RTSP, Intel RealSense), serial, CAN,
  GPIO, I2C, gamepads, Dynamixel and Feetech servos (SO-100 arm), ODrive, WitMotion IMUs,
  RPLidar and Livox lidars; MuJoCo simulation, Gazebo and Isaac Sim examples, and URDF/MJCF
  kinematics; ONNX inference, YOLO decoding, policy execution and LeRobot dataset
  recording.

## Status

ruro is early: version 0.1.0, not released, interfaces may change. The runtime, SDKs, CLI,
daemon, studio, ROS 2 bridge (against an in-process router), overlays, updates and
record/replay are covered by tests that run in CI. Some parts were verified against the
real software outside CI:

- **ROS 2 interop** against ROS 2 Jazzy with rmw_zenoh in a container (services, actions,
  latched `/tf_static`, messages of types ruro does not bundle, launch import);
- **Gazebo Harmonic** headless in a container;
- **GPU buffers** on an RTX 4090;
- **RTSP** against the mediamtx server;
- **MuJoCo** against MuJoCo 3.15.0;
- **RealSense** with librealsense 2.56.3 on a D455 and two D455Fs.

Others are implemented to their specifications but not yet verified against the real thing:

- the **hardware drivers** are tested only against protocol fixtures and simulated devices
  on pseudo-terminals, never against the physical devices (RealSense excepted);
- **Isaac Sim** support is an example written from its documentation;
- **multi-machine graphs** have been tested with two daemons on one machine.

The book's [project status](book/src/status.md) page has the details and the known gaps.

## What's next

- A first release: crates on crates.io, the Python package on PyPI, prebuilt packages
- Validating the hardware drivers on the physical devices, starting with the SO-100 arm
- Verifying real-time behaviour on a PREEMPT_RT kernel
- An e-stop hook for Python nodes, and peer e-stops that travel back
- GPU tensors straight into ONNX Runtime's CUDA provider
- Following a ROS 2 `/clock` for simulated time

## Quick start

Requires Linux, Rust 1.95 or newer and a C compiler.

```sh
git clone https://github.com/mhussainahmad/ruro
cd ruro
cargo build --release -p ruro-cli -p ruro-host -p ruro-studio

# Open the studio. It starts a local daemon automatically.
./target/release/ruro-studio examples/camera.yaml
```

Press **Run** (or <kbd>Ctrl</kbd>+<kbd>Enter</kbd>). Click the eye next to an output to watch
it live, select a node to tune its parameters while it runs, and press
<kbd>Ctrl</kbd>+<kbd>K</kbd> for the command palette.

Or from a terminal:

```sh
./target/release/ruro run examples/camera.yaml   # deploys, streams logs, Ctrl-C stops
./target/release/ruro status                     # live per-node metrics
./target/release/ruro echo stats.brightness      # print an output's values
./target/release/ruro param blur radius 12       # retune a running node
./target/release/ruro kill camera                # crash a process; watch it come back
```

Building the whole workspace (`cargo build` without `-p`), including the Python host, also
needs Python 3.11 or newer with its shared library (`python3-dev` on Debian and Ubuntu).
See [Installation](book/src/installation.md) for the Debian package, AppImage, Flatpak and
Docker image.

## A graph and a node

```yaml
name: camera-demo
nodes:
  camera: { type: std/fake-camera, group: camera, rate: 30 }
  blur:   { type: std/blur, group: vision, params: { radius: 4 } }
  stats:  { type: std/image-stats, group: vision }
wires:
  - camera.image -> blur.image
  - blur.out -> stats.image
```

```rust
use ruro::prelude::*;

/// Multiplies every sample by a gain.
#[derive(Node)]
#[node(kind = "my_robot/gain")]
pub struct Gain {
    #[input]
    input: Input<Scalar>,
    #[output]
    output: Output<Scalar>,
    #[param(default = 2.0, min = -10, max = 10)]
    k: f64,
}

impl Node for Gain {
    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        while let Some(sample) = self.input.recv() {
            self.output.send(Scalar(sample.0 * self.k))?;
        }
        Ok(())
    }
}
```

`ruro new package my_nodes` scaffolds a package like this with a test, and `ruro build`
builds a host that links it.

## Documentation

The user guide is an [mdBook](https://rust-lang.github.io/mdBook/) in [`book/`](book/src/SUMMARY.md)
(`mdbook serve book` to read it locally):

- [Installation](book/src/installation.md), [Quick start](book/src/quick-start.md),
  [Concepts](book/src/concepts.md)
- [Writing nodes in Rust](book/src/writing-nodes.md),
  [Node packages and hosts](book/src/packages.md), [Python nodes](book/src/python.md)
- [Studio](book/src/studio.md), [Studio plugins](book/src/studio-plugins.md)
- [Runtime features](book/src/runtime.md), [Real-time scheduling](book/src/realtime.md),
  [Record and replay](book/src/record-replay.md)
- [Hardware drivers](book/src/hardware.md), [Simulation](book/src/simulation.md),
  [Simulation and the real robot](book/src/sim-to-real.md),
  [Gazebo and Isaac Sim](book/src/gazebo-isaac.md), [Machine learning](book/src/ml.md),
  [GPU buffers](book/src/gpu.md)
- [ROS 2 bridge](book/src/ros2.md), [Bringing a ROS 2 system to ruro](book/src/ros2-migration.md)
- [Deploying on robots](book/src/deployment.md),
  [Multi-machine graphs](book/src/multi-machine.md), [Security](book/src/security.md),
  [Updating daemons](book/src/updates.md)
- [Command line](book/src/reference/cli.md), [Graph files](book/src/reference/graph-yaml.md),
  [Architecture](book/src/architecture.md), [Performance](book/src/performance.md)

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the full set of checks CI runs, and
[SECURITY.md](SECURITY.md) for reporting vulnerabilities.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
The studio bundles Inter and JetBrains Mono (SIL OFL 1.1) and Lucide icons (ISC); their
licences are in `crates/ruro-studio/assets`.
