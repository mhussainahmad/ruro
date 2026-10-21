# Project status

ruro is at version 0.1.0 and has not had a release yet. Interfaces may still change. This
page says how far each part has been verified, so you know what to trust and what to test
yourself.

## Tested in CI

These run in every CI build (`cargo test --workspace`, the Python package's tests and the
optional ML and camera features):

- **Runtime:** in-process and shared-memory messaging, pooled sends, parameters, timers,
  simulated time, deterministic groups, deadlines and jitter metrics.
- **Graphs:** YAML format, validation, composite nodes and their flattening, and
  [overlays](sim-to-real.md) (merge rules, ordering, errors for overlays that do not apply).
- **Daemon and CLI:** deployment, supervision and restarts, heartbeats, the emergency stop,
  record and replay (including graphs spanning two daemons, recorded into one file and
  replayed into both), host binary uploads, TLS, mutual TLS and tokens.
- **Daemon updates:** signature, target, architecture and version checks; refusals while a
  graph runs; updating a running daemon in place, refusing a binary that fails its trial
  start, rolling back after a failed real start or a crash during the probation that
  follows it, and the restart path used under systemd,
  all with real daemon processes on one machine.
- **Node SDK:** derives, schemas and compatibility checks, `ruro new` packages.
- **Studio:** headless UI tests of editing, tabs, undo, subgraphs, comparing, recovery,
  overlays, the emergency stop, nodes on peer daemons and more. Its screenshots are
  rendered from the same tests.
- **Python nodes:** the `ruro` package's tests, and a daemon running Python and Rust nodes
  in one graph.
- **ROS 2 bridge:** encoding, type hashes (all bundled message, service and action types
  checked against ROS 2 Jazzy's), the QoS written into discovery tokens (checked against
  tokens captured from Jazzy nodes), publishing, subscribing, services, actions and
  transient-local durability through an in-process Zenoh router.
- **ROS 2 import:** messages of any type decoded and re-encoded byte for byte (125 types
  serialized by ROS 2 itself), hashes of definitions found at run time, a Jazzy bag
  imported with every byte kept, launch-file conversion, and `ros2/node` supervision with
  stand-in commands.
- **Robot descriptions:** URDF and MJCF parsing and forward kinematics.
- **Machine learning:** ONNX detection and policy models, YOLO decoding, LeRobot dataset
  writing (read back with a Parquet reader).
- **Hardware drivers:** protocol encoding and decoding against byte fixtures, the serial
  drivers end to end against simulated devices on pseudo-terminals, and the RTSP camera
  against an in-process RTSP server (H.264 and MJPEG, TCP and UDP, both authentication
  schemes, reconnection).
- **GPU buffers:** without a GPU, only that the GPU nodes refuse to start with a clear
  message.

## Verified outside CI

These were checked against the real software on a development machine, with opt-in tests
that skip in CI:

- **ROS 2 interop** with ROS 2 Jazzy and rmw_zenoh_cpp 0.2.11 in a container
  (`RURO_ROS2_CONTAINER`; image and script in `scripts/ros2/`): services in both
  directions, actions with feedback and cancel in both directions, `/tf_static` latching
  both ways, the generic `ros2/sub` and `ros2/pub` bridges with types ruro does not bundle
  (byte-identical with what crossed the router), type hashes of all 380 message and 310
  `.idl` types installed there, and demo nodes run from their launch file through
  `ros2/node`. See [ROS 2 bridge](ros2.md).
- **Latching with the generic bridge:** a message `ros2 topic pub` published once with
  transient-local durability reaches a `ros2/sub` started afterwards.
- **`ros2 param`** (Jazzy) against a deployed graph with `ros2: { parameters: true }`:
  `node list`, `param list`, `get`, `describe`, `set` (the node's output changes, and an
  out-of-range value is refused), `dump`, and the change on `/parameter_events`.
- **Multi-machine graphs** between two containers on their own network
  (`scripts/multi-machine/run.sh`): TLS and token refusals, a graph spanning both, the
  emergency stop from either side, 50 ms delay with 2% loss, a 20 s partition (the graph
  and a stop engaged meanwhile catch up after it), and a daemon restart or kill (its part of
  the graph comes back by itself).
- **Gazebo Harmonic** through the ROS 2 bridge, headless in a container
  (`RURO_GAZEBO_CONTAINER`): lidar scans and odometry reach the graph, the graph runs on
  Gazebo's `/clock`, and velocity commands move the rover. See [Gazebo and Isaac Sim](gazebo-isaac.md).
- **GPU buffers** on one machine with an RTX 4090 (driver 610.57.04): in one process, across
  two processes and under the daemon. See [GPU buffers](gpu.md).
- **RTSP cameras** against the mediamtx server (H.264 from ffmpeg, MJPEG from GStreamer),
  not against IP cameras.
- **MuJoCo** (`sim/mujoco`) against MuJoCo 3.15.0.
- **RealSense** (`hw/realsense`) with librealsense 2.56.3 on a D455 and two D455Fs: every
  output from each camera, 60 fps, and all three at once, five runs in a row with no USB
  errors in the kernel log. See [Hardware drivers](hardware.md#intel-realsense).
- **LeRobot datasets** with the Hugging Face `datasets` library when `RURO_LEROBOT_PYTHON`
  is set; not yet loaded by LeRobot itself.

## Implemented, not verified against the real thing

- **Hardware drivers** other than RealSense have not been run against the physical devices
  they drive. See [Hardware drivers](hardware.md) for what each one is tested against.
- **Isaac Sim**: the example graph is written from Isaac Sim's documentation and only
  checked to parse and validate.
- **Bridge nodes** `ros2/call-trigger`, `ros2/call-set-bool`, `ros2/serve-set-bool` and
  `ros2/fibonacci-goal`, through a local Zenoh router only.
- **`ros2/node`** behind a router that requires a token or TLS, composable-node loading, and
  `ruro ros2 import --eval` on complex Python launch files.
- **Multi-machine graphs** across a real network link between machines; they are tested
  between two containers on their own network (`scripts/multi-machine/run.sh`). Clock-offset
  correction for recordings is unit tested only.
- **Daemon updates** under a real systemd and across a network.
- **Real-time priority and memory locking** need privileges that CI does not have; their
  fallback (a warning) is what CI exercises.
- **Performance figures** in [Performance](performance.md) come from development machines.

## Known gaps

- ROS 2: deadline, lifespan and liveliness are announced but not enforced (as in
  rmw_zenoh); a ruro ROS node offers no type description service.
- GPU buffers work between processes on one machine only, and ONNX Runtime does not take
  them yet (its CUDA provider is not wired up). Python nodes cannot receive them.
- H.265 RTSP streams are refused.
- A daemon update that hangs, or fails after its two-minute probation, is not rolled back
  automatically.
- The crates are not published on crates.io, and there are no prebuilt packages.
