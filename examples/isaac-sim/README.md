# Nova Carter in Isaac Sim

> **Not verified.** This example was written from the Isaac Sim documentation and has never
> been run against Isaac Sim, which needs an NVIDIA RTX GPU and was not available. Only the
> graph file is tested: it parses and validates against ruro's node types. The same path
> through the ROS 2 bridge was verified with Gazebo (see `../gazebo`).

`carter.yaml` drives NVIDIA's Nova Carter robot in Isaac Sim with `std/scan-avoid`, steering
by the front 2D lidar, and brings in odometry, the chassis IMU, the front left camera and
`/tf` for watching in the studio.

| Node | ROS 2 topic | Type |
|---|---|---|
| `scan` | `/front_2d_lidar/scan` | `sensor_msgs/LaserScan` |
| `odom` | `/chassis/odom` | `nav_msgs/Odometry` |
| `imu` | `/chassis/imu` | `sensor_msgs/Imu` |
| `camera` | `/front_stereo_camera/left/image_raw` | `sensor_msgs/Image` |
| `tf` | `/tf` | `tf2_msgs/TFMessage` |
| `drive` | `/cmd_vel` | `geometry_msgs/Twist` |

The sensor topic names are those of the Nova Carter asset in the Isaac Sim 5.1 and 6.x
documentation ("Nova Carter" asset page). That page does not list `/cmd_vel`; the robot
takes velocity commands on it in Isaac Sim's ROS 2 navigation sample. Which sensors publish depends on the scene: the
ROS 2 navigation sample enables some of them in its action graphs, and others have to be
enabled. Check with `ruro ros2 topics` and adjust the `topic` parameters.

## Connecting Isaac Sim to ruro

ruro's ROS 2 bridge speaks rmw_zenoh, so Isaac Sim must use rmw_zenoh too. According to the
Isaac Sim 6.2 ROS 2 installation guide:

- The ROS 2 libraries bundled with Isaac Sim on Linux include Fast DDS (the default) and
  Cyclone DDS but **not Zenoh**. Install ROS 2 Jazzy on Ubuntu 24.04 with
  `ros-jazzy-rmw-zenoh-cpp`, and source it before starting Isaac Sim so that it uses the
  system libraries.
- Set `RMW_IMPLEMENTATION=rmw_zenoh_cpp` before starting Isaac Sim.
- The guide starts `ros2 run rmw_zenoh_cpp rmw_zenohd` as the router. A ruro daemon is
  itself a Zenoh router, so run `ruro daemon` instead (or point the graph's `router`
  parameters at `rmw_zenohd`, which listens on the same default port, 7447).
- Zenoh with Humble on Ubuntu 22.04 and under WSL is marked untested there.

```sh
ruro daemon                                    # tcp/127.0.0.1:7447
# in the shell that starts Isaac Sim:
source /opt/ros/jazzy/setup.bash
export RMW_IMPLEMENTATION=rmw_zenoh_cpp
./isaac-sim.sh                                 # open the Nova Carter ROS 2 sample, press Play
# then:
ruro ros2 topics                               # the topics above should be listed
ruro run examples/isaac-sim/carter.yaml
```

When Isaac Sim runs on another machine than the ruro daemon, set
`ZENOH_CONFIG_OVERRIDE='connect/endpoints=["tcp/<daemon-host>:7447"]'` for Isaac Sim, start
the daemon with `--listen tcp/0.0.0.0:7447`, and leave the graph's `router` at the daemon's
own address.

**Isaac Sim on Fast DDS.** `zenoh-bridge-ros2dds` connects DDS to Zenoh, but it maps topics
to plain key expressions (`chatter`, or `<namespace>/chatter`), while rmw_zenoh's carry the
domain id, the message type and its hash (`<domain>/chatter/<type>/<hash>`), which is what
ruro's bridge nodes use. The two do not interoperate without an adapter (as discussed in
"Using RMW_ZENOH with free_fleet", discourse.openrobotics.org; the bridge's README only
mentions testing with Cyclone DDS). So ruro cannot reach a DDS-only Isaac Sim this way;
switch it to rmw_zenoh as above. This was not tried.

## Known gaps

- The graph follows Isaac Sim's `/clock` through `ros2/sub-clock` (`clock: sim`), which
  needs a ROS 2 clock publisher in the scene (Isaac Sim's "ROS2 Publish Clock" OmniGraph
  node). Without one, timer-driven nodes do not step; remove `clock: sim` and the `clock`
  node to run on the wall clock instead.
- The 3D lidar (`/front_3d_lidar/lidar_points`, `sensor_msgs/PointCloud2`) can be added
  with a `ros2/sub-point-cloud2` node if its fields are float32 x, y and z.
- Camera images must be `rgb8`, `rgba8`, `bgr8`, `bgra8` or `mono8` for the bridge to
  accept them.
