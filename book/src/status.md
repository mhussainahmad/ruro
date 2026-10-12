# Project status

ruro is at version 0.1.0 and has not had a release yet. Interfaces may still change. This
page says how far each part has been verified, so you know what to trust and what to test
yourself.

## Tested in CI

These run in every CI build (`cargo test --workspace`, the Python package's tests and the
optional ML features):

- **Runtime:** in-process and shared-memory messaging, pooled sends, parameters, timers,
  simulated time, deterministic groups, deadlines and jitter metrics.
- **Graphs:** YAML format, validation, composite nodes and their flattening.
- **Daemon and CLI:** deployment, supervision and restarts, heartbeats, the emergency stop,
  record and replay, graphs spanning two daemons (on one machine), host binary uploads, TLS,
  mutual TLS and tokens.
- **Node SDK:** derives, schemas and compatibility checks, `ruro new` packages.
- **Studio:** headless UI tests of editing, tabs, undo, subgraphs, comparing, recovery,
  the emergency stop, nodes on peer daemons and more. Its screenshots are rendered from the same tests.
- **Python nodes:** the `ruro` package's tests, and a daemon running Python and Rust nodes
  in one graph.
- **ROS 2 bridge:** encoding, type hashes (checked against ROS 2 Jazzy's) and publishing and
  subscribing through an in-process Zenoh router.
- **Robot descriptions:** URDF and MJCF parsing and forward kinematics.
- **Machine learning:** ONNX detection and policy models, YOLO decoding, LeRobot dataset
  writing (read back with a Parquet reader).
- **Hardware drivers:** protocol encoding and decoding against byte fixtures, and the serial
  drivers end to end against simulated devices on pseudo-terminals.

## Implemented, not verified against the real thing

- **Hardware drivers** have not been run against the physical devices they drive. See
  [Hardware drivers](hardware.md) for what each one is tested against.
- **MuJoCo** (`sim/mujoco`) is tested against MuJoCo 3.15.0 on a development machine; CI
  has no MuJoCo, so those tests are skipped there.
- **ROS 2 interop** has not been tested in CI against a running ROS 2 installation with
  rmw_zenoh.
- **LeRobot datasets** are checked with the Hugging Face `datasets` library when
  `RURO_LEROBOT_PYTHON` is set, not in CI, and not yet loaded by LeRobot itself.
- **Multi-machine graphs** have been tested with two daemons on one machine, not across a
  network.
- **Real-time priority and memory locking** need privileges that CI does not have; their
  fallback (a warning) is what CI exercises.
- **Performance figures** in [Performance](performance.md) come from one development machine.

## Known gaps

- Python nodes have no e-stop hook. A stop engaged directly on a peer machine does not
  travel back to the daemon that deployed the graph.
- Recording and replay cover only the groups of the daemon you talk to.
- The daemon does not compare message schema hashes at deployment.
- `ruro param` does not check parameter ranges.
- ROS 2: topics only (no services or actions), volatile durability only, Jazzy-style type
  hashes only.
- Not implemented: GPU buffers, RealSense and RTSP cameras, camera calibration messages,
  Gazebo or Isaac Sim nodes (they connect through ROS 2), graph overlays for switching
  between simulation and the real robot.
- The crates are not published on crates.io, and there are no prebuilt packages.
