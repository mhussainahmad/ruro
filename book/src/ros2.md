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

### Any message type

`ros2/sub` and `ros2/pub` bridge topics of any type. They carry `ros2/Any` messages: the
type name, its RIHS01 hash and the serialized message (CDR) exactly as it was on the
topic, so nothing is converted and nothing is lost.

| Parameter | Default | Meaning |
|---|---|---|
| `topic` | `/chatter` | ROS 2 topic name. |
| `type` | `std_msgs/msg/String` (sub), empty (pub) | ROS type, e.g. `sensor_msgs/msg/CompressedImage`. An empty type on `ros2/pub` takes the type of the first input. |
| `type_hash` | empty | RIHS01 hash. Empty: computed from the type's definition; a subscriber without a definition takes the hash the topic's publishers announce. |
| `interfaces` | empty | Directories with interface definitions, separated by `:` (see below). |
| `qos`, `depth` | `reliable`, 10 | Reliability and keep-last depth. |
| `domain_id`, `router`, `token` | | As for the typed bridges. |

`ros2/pub` drops (with a warning) input of another type than the one it publishes, and
input whose hash differs from the topic's.

Interface definitions are looked up in the `interfaces` directories, then in
`RURO_ROS2_INTERFACES` (colon separated), then in `share/<package>/` of every prefix of
`AMENT_PREFIX_PATH`, then among the bundled definitions. A directory can be a package
(with `msg/`, `srv/` or `action/`), a directory of packages, or an install prefix.
`.msg`, `.srv`, `.action` and `.idl` files are read; a service gives `<Srv>_Request`,
`<Srv>_Response` and `<Srv>_Event`, an action its goal, result, feedback, service and
feedback-message types.

With a definition, `ruro echo` and the studio's watch panel show `ros2/Any` messages as
YAML, the way `ros2 topic echo` prints them (arrays cut at 16 elements):

```text
$ ruro echo battery.out
voltage: 24.1
cells:
- 4.02
...
```

### Reading fields

`ros2/field` reads one field of each `ros2/Any` input, given its `path`
(`pose.position.x`, `ranges[0]`, `points[-1].y`): numbers and booleans are sent on its
`value` output (`std/Scalar`), strings on `text` (`std/Text`), and nested messages and
arrays on `text` as YAML or JSON. `ros2/decode` sends the whole message as YAML text
(`max_array` limits long arrays). Both take an `interfaces` parameter.

Decoding and encoding work for every ROS 2 type: all primitive types, bounded and unbounded
strings and wstrings, fixed, bounded and unbounded arrays, nested and empty messages,
constants and default values. A decoded message encodes back to exactly the same bytes,
including the uninitialised padding ROS 2's serializer leaves.

### Running ROS 2 nodes

`ros2/node` runs an existing ROS 2 node as a child process of the ruro host and supervises
it:

| Parameter | Default | Meaning |
|---|---|---|
| `package`, `executable` | | Runs `lib/<package>/<executable>` from a prefix of `AMENT_PREFIX_PATH`, as `ros2 run` does. |
| `command` | | Any command line instead, split like a shell would. |
| `name`, `namespace` | | Node name and namespace (`-r __node:=`, `-r __ns:=`). |
| `remaps` | | `from:=to`, one per line. |
| `parameters` | | ROS parameters as a JSON object; nested objects become dotted names. |
| `params_files` | | Parameter files, one per line. |
| `arguments`, `ros_args` | | Arguments before `--ros-args`, and extra ROS arguments. |
| `env` | | Extra environment, `NAME=value`, one per line. |
| `prefix` | | A command put in front, e.g. `gdb -ex run --args`. |
| `respawn` | `on-failure` | `never`, `on-failure` or `always`. |
| `respawn_delay` | 0 | Seconds before a restart; 0 backs off from 0.2 s to 5 s like the daemon. |
| `ros_setup` | | A `setup.bash` to source for the ROS environment; empty uses the host's. |
| `rmw` | `rmw_zenoh_cpp` | `RMW_IMPLEMENTATION` of the child. |
| `router`, `domain_id`, `token` | | The Zenoh router the child connects to (through `ZENOH_CONFIG_OVERRIDE`), `ROS_DOMAIN_ID`, and the router's token. |

The child's standard output and error appear in `ruro logs` under the node's id, with
`[WARN]` and `[ERROR]` lines at those levels. It runs in its own process group: stopping
the graph sends SIGTERM to the group and SIGKILL after 5 s, and the child gets SIGTERM if
the host process dies. While it is down, the node's steps report why (exit status,
restarts) as errors, which `ruro status` counts. `$(find-pkg-share P)` and
`$(find-pkg-prefix P)` in `params_files`, `arguments` and string parameters are resolved
when the node starts.

[Bringing a ROS 2 system to ruro](ros2-migration.md) puts these together.

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
ruro ros2 import robot.launch.xml [name:=value]... [--params FILE]... [-o graph.yaml]
    [--router R] [--sub TOPIC=TYPE]... [--pub TOPIC=TYPE]... [--eval] [--name N]
```

converts a ROS 2 launch file into a ruro graph with a [`ros2/node`](#running-ros-2-nodes)
per node (process group `ros2`), plus `ros2/sub` and `ros2/pub` bridges given with
`--sub` and `--pub`. XML and YAML launch files are read without ROS: `arg` and `let`;
the substitutions `$(var)`, `$(arg)`, `$(env)`, `$(optenv)`, `$(find-pkg-share)`,
`$(find-pkg-prefix)` (from `AMENT_PREFIX_PATH`), `$(dirname)`, `$(filename)` and
`$(anon)`; `if` and `unless`; `group` (scoped or not) and `push-ros-namespace`;
`set_parameter`, `set_remap` and `set_env`; `include` of XML and YAML files with `arg`;
`node` with `param` (values, `type="str"`, `value-sep`, nested groups and `from` files),
`remap`, `env`, `args`, `ros_args`, `launch-prefix`, `respawn` and `respawn_delay`;
`executable`; and `node_container` with `composable_node` (loaded by a `ros2/node`
running `ros2 component load`, which retries until the container answers). `--params`
files are given to every node. Anything else is listed as a warning, in a comment at the
top of the output and on the terminal. Python launch files need `--eval` and a sourced
ROS 2 workspace: `ros2 launch` evaluates them with a launch prefix that records each
process instead of starting it.

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

- Topics only: no services or actions. (ROS nodes run by `ros2/node` keep calling each
  other's services and actions through the router.)
- ROS nodes must use rmw_zenoh, also when ruro runs them.
- Durability is always volatile, so latched topics such as `/tf_static` are not received
  by late joiners.
- Type hashes follow REP 2011 (RIHS01), as used by Jazzy and later. Distributions without
  type hashes (Humble) are not supported.
- There is no bridge node for joint commands (`std/JointCommand`) or for `/clock`.
- The bridge connects to its router without TLS or authentication.
