# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

First version, 0.1.0. Nothing has been released yet.

### Added

- **Runtime.** Graphs of typed nodes with zero-copy, reference-counted messaging between nodes
  in the same process (about 150 ns per hop regardless of message size) and iceoryx2
  shared-memory transport between processes. Pooled sends (`Output::loan`) and recycled
  receive buffers keep hot loops free of allocation.
- **Process groups.** Each node runs in a named group; each group is a separate OS process
  with crash isolation. Regrouping needs no rebuild.
- **Real-time tier.** Per-group `SCHED_FIFO` priority, CPU pinning and memory locking;
  deterministic groups that step all their nodes on one thread in wire order at a fixed
  rate; per-node deadlines; timer jitter, deadline-miss and overrun metrics; a 1 kHz jitter
  benchmark.
- **Graph time.** `clock: sim` graphs follow a `std/Clock` output instead of the wall clock.
- **Emergency stop.** `ruro estop` latches a software stop on the daemon; nodes get an
  `on_estop` hook and `ctx.estopped()`.
- **Node SDK.** `#[derive(Node)]`, `#[derive(Message)]` and `#[derive(Describe)]` in the
  `ruro` crate: typed ports, parameters with defaults, ranges and documentation, live
  parameter updates, timer- or data-driven execution, link-time registration of node types,
  and a test harness.
- **Messages.** Standard types `Scalar`, `Text`, `Flag`, `Bytes`, `Vec3`, `Pose`, `Twist`,
  `Transform`, `Transforms`, `JointState`, `JointCommand`, `Image`, `CameraInfo`,
  `PointCloud`, `LaserScan`, `Imu`, `Odometry`, `Joy` and `Clock`. Message schemas and
  schema hashes, `ruro schema` and `ruro schema diff` for compatibility checks.
- **Graph files.** YAML graph format shared by the CLI and the studio, with validation of node
  types, parameters, rates and wire types, queue depth per wire, and composite nodes
  (`type: graph:<file>` and inline `type: composite`) flattened for deployment by the CLI
  and the studio.
- **Daemon.** Per-machine supervisor and Zenoh router: deploys graphs, starts one host process
  per group, restarts crashed or hung groups with exponential backoff, and serves metrics,
  logs, live output previews and parameter changes. Peers (`--peer`) for graphs that span
  machines, host binary uploads (`ruro deploy --host-binary`), TLS, mutual TLS and token
  authentication.
- **Record and replay.** MCAP recordings of every output (`ruro record`), replay into a
  running graph with pause, seek and speed (`ruro play`, `ruro playback`), `ruro info`, and
  `ruro bag import` for ROS 2 bags in MCAP storage.
- **CLI.** `ruro run`, `daemon`, `deploy`, `stop`, `check`, `status`, `fleet`, `nodes`,
  `echo`, `param`, `kill`, `logs`, `estop`, `record`, `play`, `playback`, `info`, `bag`,
  `studio`, `schema`, `new`, `build`, `ros2` and a hidden `soak`, with `--connect` /
  `RURO_CONNECT` for remote daemons.
- **Node packages.** `ruro new package`, `ruro new node` and `ruro build`, which builds a
  host from a `ruro.toml` listing node packages.
- **Python nodes.** The `ruro` Python package (`@ruro.node`, message dataclasses, NumPy views
  of images and point clouds, a test harness) and `ruro-host-py`, a host with an embedded
  interpreter that loads node modules and entry points, with hot reload.
- **ROS 2 bridge.** `ros2/sub-*` and `ros2/pub-*` nodes for 14 common message types over
  rmw_zenoh, without a ROS installation; `ruro ros2 topics`, `gen` (Rust types from `.msg`
  and `.srv` files) and `hash` (RIHS01).
- **Hardware drivers.** V4L2 and MJPEG-over-HTTP cameras, serial, SocketCAN, GPIO, I2C,
  evdev gamepads and joint jogging, Dynamixel Protocol 2.0, Feetech STS3215 (SO-100/101 arm),
  ODrive, WitMotion IMUs, RPLidar and Livox lidars. Tested against protocol fixtures and
  simulated devices only.
- **Simulation.** `ruro-urdf` (URDF and MJCF parsing, forward kinematics), `sim/robot-state`,
  and `sim/mujoco`, which loads MuJoCo 3.15.0 at run time (cargo feature `mujoco`).
- **Machine learning.** `ml/onnx` and `ml/policy` on ONNX Runtime (feature `onnx`), YOLO
  decoding (`ml/yolo-decode`), and a LeRobot v2.1 dataset recorder (feature `lerobot`).
  Example graphs that switch between a simulated and a real SO-100 arm.
- **Transforms.** `ruro-tf`: a time-indexed transform buffer and the `tf/publisher` and
  `tf/lookup` nodes (linked into the Python host).
- **Observability.** Per-node rate, step latency (p50/p99), CPU, input age, drops, errors and
  timer jitter; live previews of any output that cost nothing while unwatched; `ruro soak`
  for long-running memory and latency checks; `scripts/bench.sh` for benchmark regressions.
- **Studio.** Native egui editor: node library, canvas with typed ports, inspector with live
  parameter editing, watch panel with image, plot, value, text and 3D views, problems,
  output, timing and process views, command palette, and remote operation against a
  robot's daemon.
  - Tabs for several open graphs, each with its own view and undo/redo history.
  - Copy, cut, paste and duplicate through the system clipboard; multi-selection with align and
    distribute.
  - Subgraphs: group nodes into a composite node, open it in its own tab, ungroup.
  - Compare a graph with its saved version or another file, with changes highlighted on the
    canvas.
  - Templates, native file dialogs through the XDG portal (with a built-in fallback), recent
    files, example graphs, persisted layout and settings, and autosave with recovery.
  - 3D view for point clouds, laser scans, poses and transform trees.
  - Recording and a replay timeline.
  - Fleet view of daemons on the network, and machine placement for process groups.
  - ROS 2 topic browser that adds bridge nodes.
  - Open a node's source in an editor.
  - Dark and light themes.
  - Visualizer and panel plugins, registered at link time.
- **Packaging and documentation.** Debian package and AppImage scripts, Flatpak manifest,
  Docker image, systemd units, cross-compilation setup for 64-bit ARM, CI, and a user guide
  built with mdBook.

### Known limitations

- The studio cannot connect to daemons that require TLS or a token.
- `ruro estop`, recording and replay act on the daemon they are sent to, not on peer
  daemons running other parts of a graph.
- The bundled hardware drivers do not implement the emergency-stop hook.
- Flattening composite nodes drops `clock: sim`.

[Unreleased]: https://github.com/mhussainahmad/ruro/commits/main
