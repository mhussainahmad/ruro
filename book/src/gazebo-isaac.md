# Gazebo and Isaac Sim

ruro has no native Gazebo or Isaac Sim node. Both simulators talk ROS 2, so a graph reaches
them through [ruro's ROS 2 bridge](ros2.md): `ros2/sub-*` nodes bring the simulated
sensors in and `ros2/pub-twist` sends velocity commands back. The simulator's ROS 2 side
must use rmw_zenoh and connect to the same Zenoh router as the bridge nodes, which can be
the ruro daemon itself.

## Gazebo

`examples/gazebo/` drives a small differential-drive rover with a 360° lidar around a walled
arena in Gazebo Harmonic. `std/scan-avoid` turns the lidar's scans into velocity commands:

```yaml
nodes:
  lidar: { type: ros2/sub-laser-scan, group: robot, params: { topic: /scan, qos: best_effort } }
  odom:  { type: ros2/sub-odometry, group: robot, params: { topic: /odom } }
  drive: { type: ros2/pub-twist, group: robot, params: { topic: /cmd_vel } }
  avoid: { type: std/scan-avoid, params: { speed: 0.3, turn: 0.8, stop_distance: 0.7 } }
wires:
  - lidar.out -> avoid.scan
  - avoid.cmd -> drive.input
```

`examples/gazebo/run.sh` starts `gz sim` headless with the world in `rover.sdf` and
`ros_gz_bridge` for `/cmd_vel`, `/odom`, `/scan` and `/clock`, with
`RMW_IMPLEMENTATION=rmw_zenoh_cpp` connected to the daemon. A `Containerfile` next to it
builds an image with ROS 2 Jazzy, rmw_zenoh and Gazebo Harmonic (`ros-jazzy-ros-gz`).

```sh
ruro daemon
podman build -t ruro-gazebo -f examples/gazebo/Containerfile examples/gazebo
podman run --rm --init --network host ruro-gazebo /gazebo/run.sh tcp/127.0.0.1:7447
ruro run examples/gazebo/rover.yaml
```

The graph without overlays is the simulation (`--overlay sim` names it explicitly). Its
`real` overlay reads a real RPLidar with
`hw/rplidar` and runs the `robot` group on the rover's own daemon; velocity commands and
odometry then go through ROS 2 to the robot's base driver (see the example's README). It is
the same pattern as [the SO-100 arm](sim-to-real.md): one file, a variant per robot.

**Verified.** With Gazebo Harmonic 8.15 (`ros-jazzy-ros-gz` 1.0.24, rmw_zenoh 0.2.11) in a
container, headless with Mesa's software renderer: scans arrive at 10 Hz with 360 points,
odometry at 20 Hz, and the avoider's commands drive the rover around the arena.
`crates/ruro-daemon/tests/gazebo.rs` checks this; it runs when `RURO_GAZEBO_CONTAINER`
names a command that runs `run.sh` (see the example's README) and is skipped otherwise.

Two things were needed to make it work, and `run.sh` does both:

- The lidar is a rendered sensor (`gpu_lidar`), so even a server without a GUI needs a
  render engine: `gz sim -s --headless-rendering` renders through EGL.
- With host networking, Gazebo's transport picked a network interface on which
  `ros_gz_bridge` never saw its topics. `GZ_IP=127.0.0.1` keeps it on the loopback.

The ruro daemon logged "Received token with interest id from south-bound face" errors from
Zenoh while rmw_zenoh nodes connected; data flowed regardless. A bridge started before the
daemon kept its topics hidden after the daemon restarted: restart `run.sh` after the daemon.

## Isaac Sim

`examples/isaac-sim/carter.yaml` drives NVIDIA's Nova Carter the same way, steering by its
front 2D lidar (`/front_2d_lidar/scan`) and bringing in `/chassis/odom`, `/chassis/imu`, the
front left camera and `/tf`.

**Not verified.** Isaac Sim needs an NVIDIA RTX GPU, which was not available; only the graph
file is tested. According to the Isaac Sim 6.2 documentation, the ROS 2 libraries bundled
with Isaac Sim on Linux do not include rmw_zenoh: install ROS 2 Jazzy with
`ros-jazzy-rmw-zenoh-cpp` on Ubuntu 24.04, source it and set
`RMW_IMPLEMENTATION=rmw_zenoh_cpp` before starting Isaac Sim, and run `ruro daemon` as the
router in place of `rmw_zenohd`. `zenoh-bridge-ros2dds` does not help for an Isaac Sim on
Fast DDS: it maps topics to plain key expressions, while rmw_zenoh's (which ruro's bridge
speaks) carry the domain id, type and type hash, and the two do not interoperate without an
adapter. This was not tried.
`examples/isaac-sim/README.md` has the steps.

## Gaps

- **Simulated time.** Both simulators publish `/clock`, but the ROS 2 bridge has no
  `rosgraph_msgs/Clock` node, so a graph cannot follow simulated time (`clock: sim` needs a
  `std/Clock` output in the graph). The examples run on the wall clock, which matches as
  long as the simulator runs in real time.
- **Joint commands.** The bridge has no node for `std/JointCommand`, so arms in either
  simulator cannot be commanded yet; `ros2/pub-twist` covers velocity-driven bases.
