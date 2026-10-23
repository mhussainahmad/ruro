# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Nodes in other languages.** A manifest (`*.node.yaml`, listed in `RURO_PROCESS_NODES`)
  makes any program a node type, with typed ports, parameters, the e-stop and supervision:
  the host runs it per node and exchanges one JSON line per step over its standard input
  and output, or over TCP. `sdk/matlab` has a helper for MATLAB and GNU Octave, and
  `examples/process` example nodes. See the book's "Nodes in other languages".
- **Nodes in C and C++.** Plugins (`RURO_PLUGINS`) built against `sdk/include/ruro/ruro.h`,
  or `ruro.hpp` for C++17, run in the host's process with typed ports over C structs of
  every standard message type, parameters, the e-stop and errors (C++ exceptions included);
  the host refuses plugins built against another ABI. `examples/native` has a C and a C++
  node with CMake and make builds. See the book's "Nodes in C and C++".
- A daemon update that hangs during its probation (stops answering, or its supervisor stops
  running, for a quarter of it) is rolled back like one that crashes.
- **Python nodes from a package.** `ruro-python` (Ubuntu 24.04, Python 3.12) installs
  `ruro-host-py` and the `ruro` Python package; releases attach it, and CI installs it with
  `ruro` and runs the Python example through the daemon. The studio's Connect to Daemon
  dialog takes a client certificate for daemons that require one.
- **ROS 2 type descriptions.** Every ruro ROS node offers `~/get_type_description`
  (REP 2011); its answer for `tf2_msgs/msg/TFMessage` is the same as a Jazzy node's.

### Changed

- `ruro status` and `ruro fleet` answer in milliseconds: daemons reply to requests for their
  status and their groups' metrics instead of clients waiting up to 1.5 s for the next
  publication.

### Fixed

- Numbers parsed from JSON (process-node messages, parameter values) are the exact double
  they were printed from; the last bit could differ before.

### Security

- `rustls` 0.23.46, `rustls-webpki` 0.103.16 and `webpki-roots` 1.0.10 (RUSTSEC-2026-0346 to
  0349, 0381, 0382), and `lz4_flex` 0.12 for ruro's message compression (RUSTSEC-2026-0041).
- `cargo deny` checks advisories, licenses and sources on every push and weekly; releases
  attach a CycloneDX SBOM per binary. See SECURITY.md.

## [0.1.0] - 2026-10-22

The first release.

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
  `on_estop` hook and `ctx.estopped()`. Every bundled actuator node (`std/fake-arm`,
  `hw/dynamixel`, `hw/so100-arm`, `hw/odrive`, `hw/joint-jog`, `ml/policy`,
  `ros2/pub-twist`, `sim/mujoco`) goes to a documented safe state. Python nodes get
  `on_estop` and `ctx.estopped` too. A stop engaged on any machine of a multi-machine graph
  stops all of them.
- **Node SDK.** `#[derive(Node)]`, `#[derive(Message)]` and `#[derive(Describe)]` in the
  `ruro` crate: typed ports, parameters with defaults, ranges and documentation, live
  parameter updates, timer- or data-driven execution, link-time registration of node types,
  and a test harness.
- **Messages.** Standard types `Scalar`, `Text`, `Flag`, `Bytes`, `Vec3`, `Pose`, `Twist`,
  `Transform`, `Transforms`, `JointState`, `JointCommand`, `Image` (including 16-bit depth,
  `mono16`), `CameraInfo`,
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
- **Record and replay.** MCAP recordings of every output of a graph (`ruro record`), into one
  file even when the graph spans machines, with peers' clock offsets corrected; replay into
  a running graph, on every machine of it, with pause, seek and speed (`ruro play`,
  `ruro playback`), `ruro info`, and `ruro bag import` for ROS 2 bags in MCAP storage, which
  keeps every topic (types with no ruro equivalent as `ros2/Any`, byte for byte; `--raw`).
- **Graph overlays.** Named variants of a graph in the same file (`overlays:`), such as `sim`
  and `real`, selected with `--overlay` on `run`, `deploy`, `check`, `play` and `soak`,
  listed with `ruro overlays`, and picked in the studio, which shows what each one changes.
- **Daemon updates.** `ruro update` installs a new daemon (and host) binary on a daemon and
  its peers over the network: Ed25519-signed (`ruro keygen`, `ruro sign`), checked for
  target, architecture and version, refused while a graph runs unless the e-stop is
  engaged, tried before it is installed, and rolled back if it fails to start. Daemon
  status shows each machine's version, target and last update.
- **CLI.** `ruro run`, `daemon`, `deploy`, `stop`, `check`, `status`, `fleet`, `nodes`,
  `echo`, `param`, `kill`, `logs`, `estop`, `record`, `play`, `playback`, `info`, `bag`,
  `overlays`, `update`, `keygen`, `sign`, `studio`, `schema`, `new`, `build`, `ros2` and a
  hidden `soak`, with `--connect` /
  `RURO_CONNECT` for remote daemons.
- **Node packages.** `ruro new package`, `ruro new node` and `ruro build`, which builds a
  host from a `ruro.toml` listing node packages.
- **Python nodes.** The `ruro` Python package (`@ruro.node`, message dataclasses, NumPy views
  of images and point clouds, a test harness) and `ruro-host-py`, a host with an embedded
  interpreter that loads node modules and entry points, with hot reload.
- **ROS 2 bridge.** Over rmw_zenoh, without a ROS installation:
  - `ros2/sub-*` and `ros2/pub-*` nodes for 15 common message types, with full QoS
    (reliability, durability including transient-local, so `/tf_static` latches, history
    depth). `ros2/sub-clock` lets a `clock: sim` graph follow Gazebo's, Isaac Sim's or a
    bag's `/clock`; on a simulated clock the publishers stamp with simulated time.
  - With `ros2: { parameters: true }`, every node is a ROS node with the parameter
    services, so `ros2 param` and rqt read and tune it.
  - Services and actions: a Rust API for any generated type, and the nodes
    `ros2/call-trigger`, `ros2/call-set-bool`, `ros2/serve-trigger`, `ros2/serve-set-bool`
    and `ros2/fibonacci-goal`, which honour the e-stop.
  - Messages of any type, found at run time from `.msg`, `.srv`, `.action` and `.idl` files:
    the `ros2/Any` message (type name, hash and the exact bytes), the generic `ros2/sub` and
    `ros2/pub` bridges, `ros2/field` and `ros2/decode`. `ruro echo` and the studio show them
    as YAML.
  - `ruro ros2 import` converts XML and YAML launch files (and Python ones in a ROS
    environment) into graphs, listing anything it cannot convert; `ros2/node` runs existing
    ROS 2 nodes as supervised processes.
  - `ruro ros2 topics`, `services`, `actions`, `call`, `gen` (Rust types from `.msg`, `.srv`
    and `.action` files) and `hash` (RIHS01). Bundled `std_srvs`, `example_interfaces`,
    `action_msgs`, `unique_identifier_msgs` and `service_msgs`.
- **Hardware drivers.** V4L2, MJPEG-over-HTTP and RTSP cameras (H.264 behind the `h264`
  feature, and MJPEG; TCP or UDP; Basic and Digest authentication), Intel RealSense depth
  cameras (feature `realsense`, librealsense2 loaded at run time), camera calibration from
  ROS calibration files, serial, SocketCAN, GPIO, I2C,
  evdev gamepads and joint jogging, Dynamixel Protocol 2.0, Feetech STS3215 (SO-100/101 arm),
  ODrive, WitMotion IMUs, RPLidar and Livox lidars. Tested against protocol fixtures and
  simulated devices only.
- **Simulation.** `ruro-urdf` (URDF and MJCF parsing, forward kinematics), `sim/robot-state`,
  and `sim/mujoco`, which loads MuJoCo 3.15.0 at run time (cargo feature `mujoco`) and
  publishes a `std/Clock` that drives `clock: sim` graphs. A Gazebo Harmonic rover example
  and an Isaac Sim example through the ROS 2 bridge, and `std/scan-avoid`.
- **GPU buffers.** `gpu/Image` and `gpu/Tensor` messages, shared without copying within a
  process and through CUDA IPC handles between process groups; the CUDA driver is loaded at
  run time, so no toolkit is needed. Nodes `gpu/upload`, `gpu/download`, `gpu/normalize`
  and `gpu/tensor-to-cpu`.
- **Machine learning.** `ml/onnx` and `ml/policy` on ONNX Runtime (feature `onnx`), YOLO
  decoding (`ml/yolo-decode`), and a LeRobot v2.1 dataset recorder (feature `lerobot`).
  An SO-100 arm example graph with `sim`, `real` and `policy` overlays.
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
  - An emergency-stop button in the title bar (and <kbd>F12</kbd>), with a banner and a
    release while the stop is engaged on the connected daemon or a machine of its graph.
  - Overlay picker next to Run; the canvas marks the nodes an overlay adds, changes or
    removes.
  - Fleet view of daemons on the network, with each one's version and last update, and
    machine placement for process groups. Nodes
    of a graph that run on peer daemons show live metrics and can be watched and tuned.
  - ROS 2 topic browser that adds bridge nodes, generic ones for types without a typed bridge.
  - Open a node's source in an editor.
  - Dark and light themes.
  - Visualizer and panel plugins, registered at link time.
- **Packaging and documentation.** Debian package and AppImage scripts, Flatpak manifest,
  Docker image, systemd units, cross-compilation setup for 64-bit ARM, CI, and a user guide
  built with mdBook. The desktop file, icons and AppStream metadata are named after the
  studio's app id, `io.github.mhussainahmad.ruro.Studio`, which its window also uses. The
  Debian package installs the example graphs in `/usr/share/ruro/examples`.
- **Windows and macOS clients.** The studio and the CLI build for Windows (x86-64) and macOS
  (Apple silicon) and connect to a daemon on a Linux machine (**File > Connect to
  Daemon…**, or `--connect`); the daemon and hosts stay on Linux. CI builds a zip and a
  `.dmg`, and releases attach them with the Linux binaries, the Debian package and the
  AppImage. `scripts/remote-check.sh` checks a client against a daemon on another machine.

### Known limitations

See [Project status](book/src/status.md#known-gaps).

[Unreleased]: https://github.com/mhussainahmad/ruro/compare/v0.1.0...main
[0.1.0]: https://github.com/mhussainahmad/ruro/releases/tag/v0.1.0
