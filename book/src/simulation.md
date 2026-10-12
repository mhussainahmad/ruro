# Simulation

## Robot descriptions: `ruro-urdf`

`crates/ruro-urdf` parses URDF and MJCF (MuJoCo XML) into one model: links with
visual and collision geometry (boxes, cylinders, capsules, spheres, ellipsoids,
planes, mesh references), joints with type, axis, limits and origin, and inertials.
`Robot::forward_kinematics` turns joint positions into every joint transform and every
link pose.

- URDF: `revolute`, `continuous`, `prismatic`, `fixed`, `floating` and `mimic` joints;
  `rpy` is fixed-axis roll-pitch-yaw.
- MJCF: default classes (`class`, `childclass`), `<frame>`, all orientation forms
  (`quat`, `axisangle`, `euler` with `eulerseq`, `xyaxes`, `zaxis`), `angle="degree"`,
  bodies with several joints, joint anchors (`pos`), `ref`, ball and free joints (positions
  in MuJoCo `qpos` order), `fromto` geoms and `<asset>` meshes. `<include>`,
  `<replicate>`, attachments and composites are not expanded.

The MJCF kinematics have been checked against MuJoCo itself: for a model mixing a
free joint, two hinges on one body, an anchored hinge, a slide joint inside a rotated
frame and a ball joint, the body poses agree with MuJoCo's to 1e-6
(`nodes/ruro-nodes-sim/tests/mujoco.rs`).

The `sim/robot-state` node publishes these transforms (`std/Transforms`) for every
incoming `std/JointState`, like ROS's robot_state_publisher.

## MuJoCo: `sim/mujoco`

The node loads `libmujoco` at run time with `libloading`, so ruro needs no MuJoCo
headers or libraries to build. It is behind the `mujoco` cargo feature:

```sh
cargo build --release -p ruro-host --features ruro-nodes-sim/mujoco
```

Get the library from the `mujoco` Python wheel (no system install needed) and point
`RURO_MUJOCO_LIB` at it, or set the node's `library` parameter:

```sh
python -m venv ~/.venvs/mujoco && ~/.venvs/mujoco/bin/pip install mujoco==3.15.0
export RURO_MUJOCO_LIB=$(ls ~/.venvs/mujoco/lib/python3*/site-packages/mujoco/libmujoco.so.*)
```

| Port | Type | |
|---|---|---|
| `command` (in) | `std/JointCommand` | copied into `ctrl`, one value per actuator; `command_field` picks position, velocity or effort |
| `state` (out) | `std/JointState` | position and velocity of every hinge and slide joint |
| `bodies` (out) | `std/Transforms` | world pose of every body |
| `sensors` (out) | `sim/SensorReadings` | all sensors, by name |
| `sim_time` (out) | `std/Scalar` | simulation time in seconds (there is no clock message yet) |

`real_time_factor` scales simulated time against wall time (0 steps as fast as
`max_steps` allows). The node does not render camera images yet; the example graphs use
`std/fake-camera` in simulation.

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

There is no native Gazebo or Isaac Sim node: gz-transport and the Isaac Sim Python
runtime are not available to link against here. Both simulators integrate through
ROS 2, so the route is ROS 2 bridge → ruro:

- **Gazebo (gz sim).** Run `ros_gz_bridge` to expose the simulated robot's topics
  (`/joint_states`, `/cmd_vel`, camera images, `/tf`, lidar scans) on ROS 2, then
  connect them to a ruro graph with the ROS 2 bridge nodes. Joint commands flow back the
  same way to the controllers loaded in Gazebo (`ros2_control` with `gz_ros2_control`).
- **Isaac Sim.** Enable the ROS 2 bridge extension (`omni.isaac.ros2_bridge` /
  `isaacsim.ros2.bridge`), publish joint states, cameras and TF from the action graph,
  and subscribe to joint commands, then connect through the same ROS 2 bridge nodes.

The intended setup keeps the downstream nodes of `examples/arm-sim.yaml` as they are
and swaps only the `arm` and `camera` nodes for bridge nodes. This has not been tried
yet: the bridge node types do not exist on this branch, so no example graph is
shipped for this path, and the extension and package names above should be checked
against the Gazebo and Isaac Sim versions in use.
