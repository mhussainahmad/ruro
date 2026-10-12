# Switching between simulation and the real robot

`examples/arm-sim.yaml` and `examples/arm-real.yaml` are the same graph with different
robots:

| Node | arm-sim.yaml | arm-real.yaml |
|---|---|---|
| `arm` | `sim/mujoco` with `examples/models/so100.xml` | `hw/so100-arm` on `/dev/ttyACM0` |
| `camera` | `std/fake-camera` | `hw/v4l2-camera` on `/dev/video0` |
| `gamepad`, `jog`, `kinematics`, `recorder` | identical | identical |

Both robot nodes speak the same messages: `std/JointCommand` in (joint position
targets in radians, LeRobot joint order) and `std/JointState` out. Everything
downstream (gamepad teleoperation, forward kinematics, LeRobot recording, or a policy
in place of the gamepad) neither knows nor cares which one is running. The only other
difference is the dataset directory (`datasets/so100-sim` and `datasets/so100-real`), so
simulated and real episodes are not mixed in one dataset by accident.
`nodes/ruro-nodes-ml/tests/examples.rs` checks that only the robot nodes and the dataset
directory differ between the two files.

Both graphs need node features that the standard `ruro-host` leaves out, so build a host
with them and point the daemon at it:

```sh
# arm-sim.yaml: MuJoCo and LeRobot recording
cargo build --release -p ruro-host --features ruro-nodes-sim/mujoco,ruro-nodes-ml/lerobot
# arm-real.yaml: LeRobot recording
cargo build --release -p ruro-host --features ruro-nodes-ml/lerobot

ruro daemon --host target/release/ruro-host   # from the repository root
ruro run examples/arm-sim.yaml
```

The model path in the graphs (`examples/models/so100.xml`) is relative to the daemon's
working directory, so start the daemon from the repository root. `arm-sim.yaml` also
needs `RURO_MUJOCO_LIB` (see [Simulation](simulation.md#mujoco-simmujoco)).

To make switching work for your own graphs:

1. **Agree on units and zero.** The simulated model's zero pose must match the real
   arm's. For the SO-100, zero is the arm pointing straight up with the gripper closed:
   set the real arm's `offsets` to the servo readings in that pose and flip
   `directions` for joints that turn the other way than in the simulation.
2. **Put the robot in its own process group** (`group: robot`). Swapping the variant
   then changes one group; the rest of the graph is untouched, and a crash of the
   driver does not take the recorder down.
3. **Keep the joint order** of the model's actuators and of the servo `ids` the same.

To drive the arm with a trained policy instead of a gamepad, replace `gamepad` and
`jog` with an `ml/policy` node (`--features ruro-nodes-ml/onnx`) wired
`arm.state -> policy.state`, `camera.image -> policy.image`,
`policy.action -> arm.command`.

Selecting a variant from one file (graph overlays) is not supported yet; for now the
two files are kept side by side.
