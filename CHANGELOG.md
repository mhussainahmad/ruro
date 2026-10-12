# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Runtime.** Graphs of typed nodes with zero-copy, reference-counted messaging between nodes
  in the same process (about 150 ns per hop regardless of message size) and iceoryx2
  shared-memory transport between processes.
- **Process groups.** Each node runs in a named group; each group is a separate OS process
  with crash isolation. Regrouping needs no rebuild.
- **Node SDK.** `#[derive(Node)]` and `#[derive(Message)]` in the `ruro` crate: typed input
  and output ports, parameters with defaults, ranges and documentation, live parameter
  updates, timer- or data-driven execution, and link-time registration of node types.
- **Messages.** Standard types including `Scalar`, `Text`, `Image`, `CameraInfo`,
  `PointCloud`, `LaserScan`, `Imu`, `Pose`, `Twist`, `Transform`, `Odometry`, `JointState`,
  `JointCommand` and `Joy`.
- **Graph files.** YAML graph format shared by the CLI and the studio, with validation of node
  types, parameters, rates and wire types, and queue depth per wire.
- **Daemon.** Per-machine supervisor and Zenoh router: deploys graphs, starts one host process
  per group, restarts crashed groups according to their restart policy with exponential
  backoff, and serves metrics, logs, live output previews and parameter changes.
- **Observability.** Per-node rate, step latency (p50/p99), CPU, input age, drops and errors;
  live previews of any output that cost nothing while unwatched.
- **CLI.** `ruro run`, `daemon`, `deploy`, `stop`, `check`, `status`, `nodes`, `echo`, `param`,
  `kill`, `logs` and `studio`, with `--connect` / `RURO_CONNECT` for remote daemons.
- **Studio.** Native egui editor: node library, canvas with typed ports, inspector with live
  parameter editing, watch panel with image, plot and value previews, problems, output and
  process views, command palette, and remote operation against a robot's daemon.
  - Tabs for several open graphs, each with its own view and undo/redo history.
  - Copy, cut, paste and duplicate through the system clipboard; multi-selection with align and
    distribute.
  - Subgraphs: group nodes into a composite node, open it in its own tab, ungroup.
  - Compare a graph with its saved version or another file, with changes highlighted on the
    canvas.
  - Templates, native file dialogs through the XDG portal, recent files, persisted layout and
    settings, and autosave with recovery.
  - 3D view for point clouds, laser scans, poses and transform trees.
  - Record button, recording indicator and replay timeline (needs daemon support).
  - Fleet view of daemons on the network, and machine placement for process groups.
  - Open a node's source in an editor.
  - Light theme.
  - Visualizer and panel plugins, registered at link time.
- **Composite nodes.** `type: graph:<file>` and inline `type: composite` subgraphs with exposed
  ports, flattened recursively for deployment with cycle detection and validation.
- **Standard nodes.** `std/fake-camera`, `std/blur`, `std/grayscale`, `std/image-stats`,
  `std/sine`, `std/gain`, `std/pid` and `std/fake-arm`.
- **Packaging and documentation.** Debian package and AppImage scripts, Flatpak manifest,
  Docker image, systemd units, cross-compilation setup for 64-bit ARM, and a user guide built
  with mdBook.

### In progress

- Deploying composite graphs from the CLI (flattening before deployment).
- Python node SDK.
- ROS 2 bridge.
- Hardware drivers and simulator bridges.
- Record and replay in the daemon and CLI.
- Real-time scheduling for process groups.
- Multi-machine graphs.

[Unreleased]: https://github.com/mhussainahmad/ruro/commits/main
