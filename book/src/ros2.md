# ROS 2 bridge

The ROS 2 bridge connects a ruro graph to an existing ROS 2 system, in both directions, so
you can adopt ruro one subsystem at a time.

> **Status:** topics, services, actions and transient-local durability are tested against
> an in-process Zenoh router, with type hashes and liveliness tokens checked against ROS 2
> Jazzy's. An opt-in test runs them against real ROS 2 Jazzy nodes (rmw_zenoh_cpp 0.2.11,
> see [Checking against ROS 2](#checking-against-ros-2)); CI does not run it.

## How it works

The bridge speaks the protocol of [rmw_zenoh](https://github.com/ros2/rmw_zenoh), ROS 2's
Zenoh middleware, directly: CDR encoding, rmw_zenoh's key expressions and liveliness
tokens, and RIHS01 type hashes. Topics map to Zenoh publishers and subscribers, services to
Zenoh queryables and queries, and actions to the services and topics `rcl_action` makes
them of. It needs no ROS installation on the ruro side. In return, the ROS 2 side must use
rmw_zenoh:

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
| `qos` | `reliable` | `reliable` or `best_effort` (`sensor_data` is accepted as best effort). |
| `durability` | `volatile` | `volatile` or `transient_local` (latched). |
| `depth` | `10` | History depth (keep-last); 0 keeps all. |
| `router` | `tcp/127.0.0.1:7447` | Zenoh router to connect to. |
| `token` | empty | Token of a router that requires one; empty uses `RURO_TOKEN`. |
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
`rgb8`, `rgba8`, `mono8`/`8UC1`, `mono16`/`16UC1` (depth), `bgr8` or `bgra8` (BGR is converted to RGB); other
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

### Quality of service

The QoS parameters work as in ROS 2, and are announced in the liveliness tokens exactly as
rmw_zenoh announces them, so `ros2 topic info --verbose` reports them. On the wire they do
what rmw_zenoh does:

- `qos: best_effort` sends with Zenoh's best-effort reliability.
- `depth: 0` (keep all) with reliable delivery blocks a congested publisher instead of
  dropping messages.
- `durability: transient_local` gives the publisher a cache of its last `depth` messages
  (Zenoh's advanced publisher). A transient-local subscriber asks every such publisher for
  its cache when it starts, and when a publisher appears later.

Latched topics need transient-local on both sides, as in ROS 2. To mirror `/tf_static`
into ruro, or to publish static transforms that ROS nodes started later still receive:

```yaml
nodes:
  static_in:
    type: ros2/sub-tf
    params: { topic: /tf_static, durability: transient_local, depth: 1 }
  static_out:
    type: ros2/pub-tf
    params: { topic: /tf_static, durability: transient_local, depth: 1 }
```

Subscriber nodes deliver every message as it arrives, so `depth` does not limit their
queue; it sets how many cached messages they ask for. Deadline, lifespan and liveliness
are not enforced (rmw_zenoh does not enforce them either).

## Services

Service nodes call or serve `std_srvs` services from a graph:

| Node | Ports | Does |
|---|---|---|
| `ros2/call-trigger` | in `trigger: Flag`; out `success: Flag`, `message: Text` | Calls `std_srvs/Trigger` for every true input. |
| `ros2/call-set-bool` | in `data: Flag`; out `success: Flag`, `message: Text` | Calls `std_srvs/SetBool` with every input. |
| `ros2/serve-trigger` | out `triggered: Flag` | Serves `std_srvs/Trigger`; each request outputs true and is answered `success: true` with the `message` parameter. |
| `ros2/serve-set-bool` | out `data: Flag` | Serves `std_srvs/SetBool`; each request outputs its value and is answered `success: true`. |

They take `service` (the service name), `domain_id`, `router` and `token`; callers also
take `timeout_ms` (default 5000). A failed call (no server, timeout) outputs
`success: false` and the error as the message.

Services can start motion, so the nodes honour the [emergency stop](runtime.md#emergency-stop):
while it is engaged, callers drop their input instead of calling, and servers answer
`success: false` with the message `refused: ruro e-stop engaged` instead of passing the
request on. `SetBool` requests for `false`, the safe value by convention (motors off), still
go through.

From Rust, `Endpoint::service` (answer from the callback), `Endpoint::service_deferred`
(answer later, from any thread) and `Endpoint::client` work with any generated service type,
and `raw_service` and `raw_client` with CDR bytes:

```rust
use ruro_ros2::msgs::example_interfaces::srv::{AddTwoInts, AddTwoInts_Request, AddTwoInts_Response};
use ruro_ros2::rmw::Qos;

let _srv = endpoint.service::<AddTwoInts>("/add_two_ints", Qos::services(), |r| {
    AddTwoInts_Response { sum: r.a + r.b }
})?;
let client = endpoint.client::<AddTwoInts>("/add_two_ints", Qos::services())?;
client.raw().wait_for_server(Duration::from_secs(5));
let sum = client.call(&AddTwoInts_Request { a: 2, b: 3 }, Duration::from_secs(5))?.sum;
```

## Actions

An action is three services and two topics under `<action>/_action/`: `send_goal`,
`cancel_goal` and `get_result`, `feedback` and `status` (reliable, transient-local,
depth 1). Goals are named by a random UUID. ruro builds them the way `rcl_action` does, so
ruro action clients drive ROS 2 action servers and ROS 2 clients (`ros2 action send_goal`,
rclcpp, rclpy) drive ruro servers.

`ros2/fibonacci-goal` sends `example_interfaces/action/Fibonacci` goals. Every `order`
input (`Scalar`) sends a goal and cancels the one before; `feedback` and `result` come out
as space-separated sequences (`Text`), `succeeded` (`Flag`) tells how the goal ended and
`status` (`Text`) follows it (`accepted`, `executing`, `succeeded`, `canceled`, `aborted`,
`rejected`, `failed`). Parameters: `action` (default `/fibonacci`), `timeout_ms` for the
service calls, `domain_id`, `router`, `token`. When the e-stop engages it cancels its
active goal (by id, leaving other clients' goals alone), and it drops input while the stop
is engaged.

For other action types, `ruro_ros2::action` has `ActionClient` and `ActionServer` for any
generated action type, and `ruro_nodes_ros2::actions::GoalDriver` drives one goal at a
time from a node's step without blocking, with the same e-stop behaviour as
`ros2/fibonacci-goal`:

```rust
use ruro_ros2::action::{ActionClient, ActionServer};
use ruro_ros2::msgs::example_interfaces::action::{Fibonacci, Fibonacci_Goal, Fibonacci_Result};

let server = ActionServer::<Fibonacci>::new(&endpoint, "/fibonacci")?;
// In a loop: take accepted goals and drive them.
if let Some(goal) = server.next_goal() {
    goal.execute();
    goal.succeed(Fibonacci_Result { sequence: vec![0, 1, 1] });
}

let client = ActionClient::<Fibonacci>::new(&endpoint, "/fibonacci")?;
let goal = client.send_goal(Fibonacci_Goal { order: 5 }, Duration::from_secs(5))?;
let (status, result) = goal.result(Duration::from_secs(30))?;
```

Servers accept cancel requests for active goals and keep a finished goal's result for ten
seconds, as rclcpp does; a goal handle dropped before it ends aborts the goal.

## Command line

```sh
ruro ros2 topics [--router R] [--domain N] [--timeout MS] [--json] [--all]
```

lists the ROS 2 topics announced through rmw_zenoh, with their type and publisher and
subscriber counts. The topics actions are made of are left out unless `--all` is given.

```text
TOPIC        TYPE                                  PUB  SUB
/ruro/value  std_msgs/msg/Float64                    1    1
```

```sh
ruro ros2 services [--router R] [--domain N] [--timeout MS] [--json]
ruro ros2 actions  [--router R] [--domain N] [--timeout MS] [--json]
```

list services (with server and client counts, leaving out the services actions are made
of) and actions:

```text
ACTION      TYPE                                          SRV  CLI
/fibonacci  action_tutorials_interfaces/action/Fibonacci    1    0
```

```sh
ruro ros2 call /add_two_ints example_interfaces/srv/AddTwoInts '{a: 2, b: 3}' [--package DIR]
```

calls a service and prints the response as JSON. The request is YAML; fields left out take
their defaults. The type must be bundled or come from a `--package` directory. It waits up
to `--timeout` milliseconds (default 10000) for a server and for the reply.

```sh
ruro ros2 hash geometry_msgs/msg/Twist [PKG_DIR...] [--verbose]
```

prints the RIHS01 hash of a message, service or action type (with `--verbose`, the hashed
type description).

```sh
ruro ros2 gen path/to/my_msgs [-o src/ros.rs] [--root crate::ros]
```

generates Rust types from `.msg`, `.srv` and `.action` files, for using custom ROS 2
interfaces in your own nodes with `ruro_ros2::rmw::Endpoint` and `ruro_ros2::action`. Each
directory must be named after its ROS package and contain `msg/`, `srv/` and/or `action/`.
The generated code refers to the `ruro-ros2` crate, so add it as a dependency. A `.srv`
file produces its request, response and event messages and a marker type implementing
`RosService`; a `.action` file produces its goal, result and feedback messages, the
messages and services the action protocol wraps them in, and a marker type implementing
`RosAction`. `crates/ruro-ros2/examples/ros2_chat.rs` shows a talker and a listener.

These interfaces are bundled, copied from ROS 2 Jazzy: `builtin_interfaces`, `std_msgs`,
`std_srvs`, `geometry_msgs`, `sensor_msgs`, `nav_msgs`, `tf2_msgs`, `action_msgs`,
`unique_identifier_msgs`, `service_msgs` and `example_interfaces`. Their 214 type hashes,
service and action types included, are checked against the ones Jazzy publishes.

## Checking against ROS 2

`crates/ruro-ros2/tests/ros2_interop.rs` and `nodes/ruro-nodes-ros2/tests/ros2_interop.rs`
run real ROS 2 nodes against ruro. They are skipped unless `RURO_ROS2_CONTAINER` names a
script that runs a shell command in a ROS 2 Jazzy environment with rmw_zenoh_cpp,
`demo_nodes_cpp`, `action_tutorials_cpp` and `tf2_ros`, using host networking and the
Zenoh router given in `ZENOH_ROUTER`. For example, around a container image:

```sh
#!/bin/sh
# ros2-run.sh: runs "$*" in ROS 2 Jazzy, connected to $ZENOH_ROUTER.
exec podman run --rm --network host -e RMW_IMPLEMENTATION=rmw_zenoh_cpp \
  -e ZENOH_CONFIG_OVERRIDE="connect/endpoints=[\"$ZENOH_ROUTER\"];scouting/multicast/enabled=false" \
  my-ros-jazzy-zenoh-image bash -c "source /opt/ros/jazzy/setup.bash; $*"
```

```sh
RURO_ROS2_CONTAINER=$PWD/ros2-run.sh cargo test -p ruro-ros2 --test ros2_interop
RURO_ROS2_CONTAINER=$PWD/ros2-run.sh cargo test -p ruro-nodes-ros2 --test ros2_interop
```

Each test starts a Zenoh router in the test process. They check that:

- ruro calls `ros2 run demo_nodes_cpp add_two_ints_server`, and `ros2 service call`
  calls a ruro `AddTwoInts` server and a `ros2/serve-trigger` node;
- a ruro action client drives `ros2 run action_tutorials_cpp fibonacci_action_server`,
  feedback, result and cancellation included, and a ruro action server serves
  `ros2 action send_goal` and `ros2 run action_tutorials_cpp fibonacci_action_client`;
- a transform ruro latches on `/tf_static` reaches a `ros2 topic echo` started later, and
  one from `static_transform_publisher` reaches a ruro subscriber created after it was
  published.

Without a container, `ruro ros2 services`, `ruro ros2 actions` and `ruro ros2 call`
against a machine running ROS 2 with rmw_zenoh check the same things by hand.

## Limitations

- Deadline, lifespan and liveliness QoS are announced but not enforced, as in rmw_zenoh.
- The only action node is `ros2/fibonacci-goal`; other action types need Rust
  (`GoalDriver`). `ruro ros2 call` covers bundled and `--package` service types only.
- Service and action calls from nodes go through the node's step, adding up to 20 ms
  (servers) or 50 ms (callers) of latency per request.
- Type hashes follow REP 2011 (RIHS01), as used by Jazzy and later. Distributions without
  type hashes (Humble) are not supported.
- There is no bridge node for joint commands (`std/JointCommand`) or for `/clock`.
- The bridge's own ROS node does not offer the parameter or type description services
  rclcpp nodes have, so `ros2 param` and `ros2 node info` see little of it.
