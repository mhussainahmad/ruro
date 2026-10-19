# Simulation

> **Status:** URDF/MJCF parsing and forward kinematics are tested in CI. The MuJoCo node is
> tested against MuJoCo 3.15.0 when `RURO_MUJOCO_LIB` points at it; CI does not run those
> tests, as it has no MuJoCo.

## Robot descriptions: `ruro-urdf`

`crates/ruro-urdf` parses URDF and MJCF (MuJoCo XML) into one model: links with
visual and collision geometry (boxes, cylinders, capsules, spheres, ellipsoids,
planes, mesh references), joints with type, axis, limits and origin, and inertials.
`Robot::forward_kinematics` turns joint positions into every joint transform and every
link pose.

- URDF: `revolute`, `continuous`, `prismatic`, `fixed`, `floating` and `mimic` joints;
  `rpy` is fixed-axis roll-pitch-yaw. `planar` joints are parsed but treated as fixed.
- MJCF: default classes (`class`, `childclass`), `<frame>`, all orientation forms
  (`quat`, `axisangle`, `euler` with `eulerseq`, `xyaxes`, `zaxis`), `angle="degree"`,
  bodies with several joints, joint anchors (`pos`), `ref`, ball and free joints (positions
  in MuJoCo `qpos` order), `fromto` geoms, `<asset>` meshes and `meshdir`. `<include>`,
  `<replicate>`, attachments and composites are not expanded.

The MJCF kinematics have been checked against MuJoCo itself: for a model mixing a
free joint, two hinges on one body, an anchored hinge, a slide joint inside a rotated
frame and a ball joint, the body poses agree with MuJoCo's to 1e-6
(`nodes/ruro-nodes-sim/tests/mujoco.rs`).

The `sim/robot-state` node publishes these transforms for every incoming joint state, like
ROS's robot_state_publisher:

| Port | Type | |
|---|---|---|
| `joints` (in) | `std/JointState` | positions matched to the model's movable joints in document order (MJCF: `qpos` order) |
| `transforms` (out) | `std/Transforms` | parent-to-child transform of every joint |

Parameters: `model` (path to a `.urdf` or MJCF `.xml` file) and `prefix` (prepended to
frame names). It is part of the standard `ruro-host`; it needs no MuJoCo.

## MuJoCo: `sim/mujoco`

The node loads `libmujoco` at run time with `libloading`, so ruro needs no MuJoCo
headers or libraries to build. It is behind the `mujoco` cargo feature:

```sh
cargo build --release -p ruro-host --features ruro-nodes-sim/mujoco
```

Get the library from the `mujoco` Python wheel (no system install needed) and point
`RURO_MUJOCO_LIB` at it, or set the node's `library` parameter. Without either, the node
loads `libmujoco.so` from the system library path.

```sh
python -m venv ~/.venvs/mujoco && ~/.venvs/mujoco/bin/pip install mujoco==3.15.0
export RURO_MUJOCO_LIB=$(ls ~/.venvs/mujoco/lib/python3*/site-packages/mujoco/libmujoco.so.*)
```

| Port | Type | |
|---|---|---|
| `command` (in) | `std/JointCommand` | copied into `ctrl`, one value per actuator; `command_field` picks position, velocity or effort |
| `state` (out) | `std/JointState` | position and velocity of every hinge and slide joint |
| `bodies` (out) | `std/Transforms` | world pose of every body |
| `sensors` (out) | `sim/SensorReadings` | all sensors, by name (only published if the model has sensors) |
| `clock` (out) | `std/Clock` | simulation time, sent after the other outputs |

`real_time_factor` scales simulated time against wall time (0 steps as fast as
`max_steps` allows). The node does not render camera images yet; the example graphs use
`std/fake-camera` in simulation.

With [`clock: sim`](runtime.md#graph-time-and-simulation) in the graph, the `clock`
output drives graph time: every timer-driven node steps on simulated time, so a slow or
paused simulation slows the whole graph down with it. No wire is needed; the runtime finds
the graph's one `std/Clock` output. `examples/arm.yaml` runs this way. Message stamps,
including those of `bodies`, stay wall time.

**Emergency stop.** The physics keeps running and commands are ignored while stopped. With
`command_field: velocity` or `effort` the controls are set to zero. With
`command_field: position`, each actuator holds the present position of its joint (actuator
*i* driving the *i*-th hinge or slide joint); models with a different number of actuators
and joints keep their last controls instead. After the release, the controls stay until a
command arrives (commands sent during the stop are discarded).

**Version support.** MuJoCo has no stable ABI for `mjModel`/`mjData` fields, so the
node reads them through a layout table keyed by `mj_version()` and refuses versions it
does not know, and every load cross-checks the table against `mj_stateSize`. The table
currently covers MuJoCo 3.15.0 (x86_64 Linux). To add a release, run
`nodes/ruro-nodes-sim/tools/mujoco_layout.c` against its headers (instructions in the
file) and add the printed entry to `src/mujoco/ffi.rs`.

**Tests** (skipped unless `RURO_MUJOCO_LIB` is set):

```sh
RURO_MUJOCO_LIB=... cargo test -p ruro-nodes-sim --features mujoco -- --nocapture
```

They check that an undamped RK4 pendulum swinging at 0.3 rad has the period predicted by
the large-amplitude formula (T₀(1 + θ²/16 + 11θ⁴/3072), within 1e-4 relative) and
conserves energy (drift below 1e-6 relative over 20 s), that damping settles it and a
motor torque holds it at the expected angle, that sensors match the state, that the
example SO-100 model tracks position targets, and that the node keeps scaled real time
inside an executor. With MuJoCo 3.15.0 the measured period was 1.804534 s against
1.804534 s expected, with an energy drift of about 5e-15 J.

## Gazebo and Isaac Sim

There is no native Gazebo or Isaac Sim node. Both simulators integrate through ROS 2, so
the route is ROS 2 → [ruro's ROS 2 bridge](ros2.md) → ruro:

- **Gazebo (gz sim).** Run `ros_gz_bridge` to expose the simulated robot's topics
  (`/joint_states`, camera images, `/tf`, lidar scans) on ROS 2, with
  `RMW_IMPLEMENTATION=rmw_zenoh_cpp`, then bring them into a graph with `ros2/sub-*`
  nodes.
- **Isaac Sim.** Enable its ROS 2 bridge extension and publish joint states, cameras and
  TF from the action graph, again over rmw_zenoh.

Not tried yet, and with known gaps:

- The bridge has no node for joint commands (`std/JointCommand`), so commands cannot be
  sent back to the simulator's controllers yet; `ros2/pub-twist` covers velocity-driven
  mobile bases.
- There is no `/clock` bridge, so the graph cannot follow simulated time.
- Whether Isaac Sim's bridge works with rmw_zenoh depends on the Isaac Sim version.

The intended setup keeps the downstream nodes of `examples/arm-sim.yaml` as they are and
swaps only the `arm` and `camera` nodes for bridge nodes. No example graph is shipped for
this path yet.
