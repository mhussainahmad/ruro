# ROS 2 bridge

The ROS 2 bridge connects a ruro graph to an existing ROS 2 system, in both directions, so
you can adopt ruro one subsystem at a time.

> **Status:** implemented and tested against an in-process Zenoh router, with type hashes
> checked against ROS 2 Jazzy's. It has not yet been tested in CI against a running ROS 2
> installation.

## How it works

The bridge speaks the protocol of [rmw_zenoh](https://github.com/ros2/rmw_zenoh), ROS 2's
Zenoh middleware, directly: CDR encoding, rmw_zenoh's key expressions and liveliness
tokens, and RIHS01 type hashes. It needs no ROS installation on the ruro side. In return,
the ROS 2 side must use rmw_zenoh:

```sh
export RMW_IMPLEMENTATION=rmw_zenoh_cpp
ros2 run rmw_zenoh_cpp rmw_zenohd      # the Zenoh router, unless one is already running
```

Bridge nodes connect to a Zenoh router (`router` parameter, default
`tcp/127.0.0.1:7447`). That can be `rmw_zenohd` or a ruro daemon, which is itself a Zenoh
router; ROS 2 nodes and the bridge must reach the same one. ROS 2 nodes that use a DDS
middleware are not reachable.

## Bridge nodes

`ros2/sub-<type>` nodes subscribe to a ROS 2 topic and publish each message on their `out`
port; `ros2/pub-<type>` nodes publish every message on their `input` port to a ROS 2 topic.
They are part of the standard `ruro-host`.

| Parameter | Default | Meaning |
|---|---|---|
| `topic` | per type, see below | ROS 2 topic name. |
| `domain_id` | `-1` | ROS domain id; -1 takes `ROS_DOMAIN_ID`, else 0. |
| `qos` | `reliable` | `reliable` or `best_effort` (`sensor_data` is accepted as best effort). History is keep-last 10. |
| `router` | `tcp/127.0.0.1:7447` | Zenoh router to connect to. |
| `frame_id` (pub only) | `base_link` | `frame_id` for message types whose ruro counterpart has no header (PoseStamped, Image, JointState, PointCloud2). |

`ros2/pub-twist` publishes a zero twist when the [emergency stop](runtime.md#emergency-stop)
engages and in place of every input while it is engaged.

In the ROS graph the nodes appear as `ruro_<node id>`. Subscriber nodes publish from the
network callback and step once a second only to apply parameter changes, so their rate in
`ruro status` stays near zero while data flows.

### Message types

| ruro type | ROS 2 type | Node suffix | Default topic |
|---|---|---|---|
| `std/Twist` | `geometry_msgs/Twist` | `twist` | `/cmd_vel` |
| `std/Pose` | `geometry_msgs/PoseStamped` | `pose-stamped` | `/pose` |
| `std/Image` | `sensor_msgs/Image` | `image` | `/image_raw` |
| `std/CameraInfo` | `sensor_msgs/CameraInfo` | `camera-info` | `/camera_info` |
| `std/LaserScan` | `sensor_msgs/LaserScan` | `laser-scan` | `/scan` |
| `std/Imu` | `sensor_msgs/Imu` | `imu` | `/imu` |
| `std/JointState` | `sensor_msgs/JointState` | `joint-state` | `/joint_states` |
| `std/Joy` | `sensor_msgs/Joy` | `joy` | `/joy` |
| `std/PointCloud` | `sensor_msgs/PointCloud2` (float32 x, y, z) | `point-cloud2` | `/points` |
| `std/Odometry` | `nav_msgs/Odometry` | `odometry` | `/odom` |
| `std/Transforms` | `tf2_msgs/TFMessage` | `tf` | `/tf` |
| `std/Scalar` | `std_msgs/Float64` | `float64` | `/value` |
| `std/Text` | `std_msgs/String` | `string` | `/chatter` |
| `std/Flag` | `std_msgs/Bool` | `bool` | `/flag` |

Fields ruro does not model are dropped on the way in and zeroed on the way out (odometry
covariances, joint names, camera binning and region of interest). Incoming images must be
`rgb8`, `rgba8`, `mono8`/`8UC1`, `bgr8` or `bgra8` (BGR is converted to RGB); other
encodings are dropped with a warning. Incoming point clouds need float32 x, y and z fields;
non-finite points are skipped.

A graph that sends velocity commands to a ROS 2 robot and watches its odometry:

```yaml
name: teleop
nodes:
  odom:
    type: ros2/sub-odometry
    params: { topic: /odom }
  cmd:
    type: ros2/pub-twist
    params: { topic: /cmd_vel, qos: reliable }
```

The studio's [ROS 2 Topics](studio.md#ros-2-topics) view discovers the topics of a running
ROS 2 system and adds bridge nodes for them.

## Command line

```sh
ruro ros2 topics [--router R] [--domain N] [--timeout MS] [--json]
```

lists the ROS 2 topics announced through rmw_zenoh, with their type and publisher and
subscriber counts:

```text
TOPIC        TYPE                                  PUB  SUB
/ruro/value  std_msgs/msg/Float64                    1    1
```

```sh
ruro ros2 hash geometry_msgs/msg/Twist [PKG_DIR...] [--verbose]
```

prints a message type's RIHS01 hash (with `--verbose`, the hashed type description).

```sh
ruro ros2 gen path/to/my_msgs [-o src/ros.rs] [--root crate::ros]
```

generates Rust types from `.msg` and `.srv` files, for using custom ROS 2 messages in your
own nodes with `ruro_ros2::rmw::Endpoint` publishers and subscribers. Each directory must
be named after its ROS package and contain `msg/` and/or `srv/`. The generated code refers
to the `ruro-ros2` crate, so add it as a dependency; a `.srv` file produces its request and
response message types. `crates/ruro-ros2/examples/ros2_chat.rs` shows a talker and a
listener.

The standard interfaces the bridge uses (`builtin_interfaces`, `std_msgs`,
`geometry_msgs`, `sensor_msgs`, `nav_msgs`, `tf2_msgs`) are bundled, copied from ROS 2
Jazzy.

## Limitations

- Topics only: no services or actions.
- Durability is always volatile, so latched topics such as `/tf_static` are not received
  by late joiners.
- Type hashes follow REP 2011 (RIHS01), as used by Jazzy and later. Distributions without
  type hashes (Humble) are not supported.
- There is no bridge node for joint commands (`std/JointCommand`) or for `/clock`.
- The bridge connects to its router without TLS or authentication.
