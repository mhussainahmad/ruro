# Switching between simulation and the real robot

`examples/arm.yaml` is one graph for the SO-100 arm in two variants. The graph itself is the
simulation. Its `real` overlay swaps the robot nodes for the hardware:

| Node | `arm.yaml` | `arm.yaml --overlay real` |
|---|---|---|
| `arm` | `sim/mujoco` with `examples/models/so100.xml` | `hw/so100-arm` on `/dev/ttyACM0` |
| `camera` | `std/fake-camera` | `hw/v4l2-camera` on `/dev/video0` |
| `recorder` | dataset `datasets/so100-sim` | dataset `datasets/so100-real` |
| `gamepad`, `jog`, `kinematics` | identical | identical |
| `clock` | `sim` (MuJoCo's time) | `wall` |

Both robot nodes speak the same messages: `std/JointCommand` in (joint position targets in
radians, LeRobot joint order) and `std/JointState` out. Everything downstream (gamepad
teleoperation, forward kinematics, LeRobot recording, or a policy in place of the gamepad)
neither knows nor cares which one is running. The datasets differ so that simulated and real
episodes are not mixed by accident. `nodes/ruro-nodes-ml/tests/examples.rs` checks that
nothing else differs.

```sh
ruro overlays examples/arm.yaml                 # list the overlays and what they change
ruro run examples/arm.yaml                      # MuJoCo
ruro run examples/arm.yaml --overlay real       # the real arm and camera
ruro run examples/arm.yaml -o real -o policy    # a trained policy drives the real arm
```

In the studio, pick the overlay next to **Run** (see [Studio](studio.md#overlays)).

The simulation is the base graph on purpose: running the file without an overlay never
moves hardware.

Both variants need node features that the standard `ruro-host` leaves out, so build a host
with them and point the daemon at it:

```sh
# simulation: MuJoCo and LeRobot recording
cargo build --release -p ruro-host --features ruro-nodes-sim/mujoco,ruro-nodes-ml/lerobot
# real only: LeRobot recording
cargo build --release -p ruro-host --features ruro-nodes-ml/lerobot

ruro daemon --host target/release/ruro-host   # from the repository root
ruro run examples/arm.yaml
```

The model path in the graph (`examples/models/so100.xml`) is relative to the daemon's
working directory, so start the daemon from the repository root. The simulation also needs
`RURO_MUJOCO_LIB` (see [Simulation](simulation.md#mujoco-simmujoco)).

To make switching work for your own graphs:

1. **Agree on units and zero.** The simulated model's zero pose must match the real
   arm's. For the SO-100, zero is the arm pointing straight up with the gripper closed:
   set the real arm's `offsets` to the servo readings in that pose and flip
   `directions` for joints that turn the other way than in the simulation.
2. **Put the robot in its own process group** (`group: robot`). Swapping the variant
   then changes one group; the rest of the graph is untouched, and a crash of the
   driver does not take the recorder down.
3. **Keep the joint order** of the model's actuators and of the servo `ids` the same.

The `policy` overlay replaces `gamepad` and `jog` with an `ml/policy` node
(`--features ruro-nodes-ml/onnx`) wired `arm.state -> policy.state`,
`camera.image -> policy.image` and `policy.action -> arm.command`. Combined with `real`, the
policy drives the real arm; alone, it drives the simulation.

## Overlays

An overlay is a named patch kept in the graph file under `overlays:`. The `real` overlay of
`arm.yaml`:

```yaml
overlays:
  real:
    clock: wall
    nodes:
      arm:
        type: hw/so100-arm        # a new type: the simulator's params are dropped
        params: { device: /dev/ttyACM0, baud: 1000000, ids: "1,2,3,4,5,6" }
      camera:
        type: hw/v4l2-camera
        params: { device: /dev/video0, width: 640, height: 480, fps: 30 }
      recorder:
        params: { root: datasets/so100-real }   # same type: merged into the base params
```

Overlays are applied in the order they are selected, before subgraphs are flattened and
before validation, so the result is checked like any graph. An overlay can change the clock,
group settings (such as which `machine` a group runs on), nodes and wires; the exact rules
are in [Graph files](reference/graph-yaml.md#overlays). Every mistake is an error: selecting
an overlay the graph does not have, patching a node that does not exist without giving its
`type`, or removing a node or wire that is not there. `ruro check` and the studio also report
overlays that do not apply, even when they are not selected.

`examples/pid.yaml` has two overlays that need nothing beyond the standard host, for trying
them out: `stiff` changes the controller's gains, and `scaled` puts a `std/gain` node
between the target and the controller.
