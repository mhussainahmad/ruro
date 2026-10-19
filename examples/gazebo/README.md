# Rover in Gazebo

A small differential-drive rover with a 360° lidar wanders around a walled arena in
Gazebo Harmonic, steered by a ruro graph that avoids obstacles. Gazebo talks to ROS 2
through `ros_gz_bridge`; ruro talks to ROS 2 through its [ROS 2 bridge](../../book/src/ros2.md),
which speaks rmw_zenoh. No ROS installation is needed on the ruro side.

```text
gz sim (rover.sdf) --gz transport--> ros_gz_bridge --rmw_zenoh--> ruro daemon (Zenoh router)
                                                                   |
                       /scan, /odom  -> ros2/sub-laser-scan, ros2/sub-odometry
                       /cmd_vel      <- ros2/pub-twist <- std/scan-avoid
```

| File | What it is |
|---|---|
| `rover.yaml` | The graph. Without an overlay it drives the Gazebo rover; the `real` overlay reads a real RPLidar and runs the robot group on the rover's computer. |
| `rover.sdf` | The world: arena, two boxes, the rover with a `gpu_lidar` and the `DiffDrive` system. |
| `run.sh` | Starts `gz sim` headless and `ros_gz_bridge` for `/cmd_vel`, `/odom`, `/scan` and `/clock`. |
| `Containerfile` | ROS 2 Jazzy with `ros-jazzy-rmw-zenoh-cpp` and `ros-jazzy-ros-gz` (Gazebo Harmonic), for `run.sh`. |

## Running it

The ruro daemon is the Zenoh router that rmw_zenoh connects to, so start it first:

```sh
cargo build --release -p ruro-cli -p ruro-host
./target/release/ruro daemon                       # listens on tcp/127.0.0.1:7447

# Gazebo and the bridge, in a container:
podman build -t ruro-gazebo -f examples/gazebo/Containerfile examples/gazebo
podman run --rm --init --network host ruro-gazebo /gazebo/run.sh tcp/127.0.0.1:7447

./target/release/ruro run examples/gazebo/rover.yaml
./target/release/ruro echo odom.out                # the rover's position
```

Rootless podman without subordinate ids (no entry in `/etc/subuid`) fails the build with
`setgroups ... Operation not permitted` from apt; adding `-o APT::Sandbox::User=root` to
the `apt-get` commands works around it. The example was verified with an image built that
way from `ros:jazzy-ros-base` with the same two packages, not with this exact file.

With ROS 2 Jazzy, rmw_zenoh and ros_gz installed natively, run `examples/gazebo/run.sh`
instead of the container. To see the simulation, run `gz sim -g` on the same machine (with
the same `GZ_IP`), or drop `-s` and `--headless-rendering` from `run.sh`.

The lidar is a rendered sensor, so the server needs a render engine even without a GUI:
`run.sh` uses `--headless-rendering` (EGL), which also works with Mesa's software renderer
when there is no GPU. `run.sh` sets `GZ_IP=127.0.0.1`: with host networking, Gazebo's
transport otherwise picked an interface on which the bridge never saw its topics.

## The real robot

`ruro run examples/gazebo/rover.yaml --overlay real` swaps the simulated lidar for ruro's
`hw/rplidar` driver on `/dev/ttyUSB0` (its output is `scan`, so the overlay rewires it) and
runs the `robot` group on the daemon named `rover`. ruro has no wheel driver that takes a
`std/Twist`, so on the real robot `/cmd_vel` and `/odom` still go through ROS 2: to the base
driver of the robot (for example a TurtleBot or iRobot Create 3 base) running on the rover's
computer with rmw_zenoh, connected to the `rover` daemon. `ruro overlays
examples/gazebo/rover.yaml` lists the changes. The real overlay has not been run on a robot.

## Simulated time

Gazebo publishes `/clock` and `run.sh` bridges it, but the graph runs on the wall clock: the
ROS 2 bridge has no node for `rosgraph_msgs/Clock` yet, so `clock: sim` cannot follow
Gazebo's time. Gazebo runs at real-time factor 1, so the two agree as long as the
simulation keeps up.

## Test

`crates/ruro-daemon/tests/gazebo.rs` starts a daemon, runs `run.sh` through
`RURO_GAZEBO_CONTAINER`, deploys `rover.yaml` and checks that scans and odometry arrive and
that the rover moves at least 0.5 m:

```sh
cargo build -p ruro-host
RURO_GAZEBO_CONTAINER="podman run --rm --init --network host -v $PWD/examples:$PWD/examples:z ruro-gazebo" \
    cargo test -p ruro-daemon --test gazebo -- --nocapture
```

`RURO_GAZEBO_CONTAINER=native` runs `run.sh` directly. Without the variable the test is
skipped.
