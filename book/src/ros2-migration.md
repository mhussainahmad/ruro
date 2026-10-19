# Bringing a ROS 2 system to ruro

This page moves an existing ROS 2 system into ruro without changing it first: its nodes
keep running as they are, supervised by ruro; every topic, of any message type, can reach
ruro nodes; recorded bags become ruro recordings. You then replace ROS nodes with ruro
nodes one at a time, or not at all.

> **Status:** tested against ROS 2 Jazzy with rmw_zenoh in a container: ruro ran
> demo_nodes_cpp's talker and listener from their launch file, received `/chatter`, and
> exchanged `example_interfaces` and `rcl_interfaces` messages with ROS in both
> directions. Type hashes of all 1031 message types installed there match rosidl's. The
> CI tests use recorded fixtures and do not need ROS.

The pieces:

| Piece | What it does |
|---|---|
| [`ruro ros2 import`](#1-import-the-launch-file) | Turns an XML or YAML launch file into a ruro graph. |
| [`ros2/node`](ros2.md#running-ros-2-nodes) | Runs one ROS 2 node as a supervised child process. |
| [`ros2/sub`, `ros2/pub`](ros2.md#any-message-type) | Carry topics of any type, unchanged, as `ros2/Any`. |
| [`ros2/field`, `ros2/decode`](ros2.md#reading-fields) | Read fields of `ros2/Any` messages. |
| [`ruro bag import`](record-replay.md#ros-2-bags) | Keeps every topic of a bag. |

The ROS 2 side must use rmw_zenoh. Nodes that ruro starts get
`RMW_IMPLEMENTATION=rmw_zenoh_cpp` and the router address automatically. A ruro daemon is
a Zenoh router, so the ROS 2 nodes, the bridge nodes and the daemon can all use the
daemon's endpoint, and `rmw_zenohd` is not needed.

## Worked example

The example robot has this launch file, `robot.launch.xml`:

```xml
<launch>
  <arg name="robot" default="r1"/>
  <node pkg="demo_nodes_cpp" exec="talker" name="talker" namespace="$(var robot)" respawn="true">
    <param name="use_sim_time" value="false"/>
    <remap from="chatter" to="/$(var robot)/chatter"/>
  </node>
  <node pkg="demo_nodes_cpp" exec="listener" name="listener" namespace="$(var robot)">
    <remap from="chatter" to="/$(var robot)/chatter"/>
  </node>
  <node pkg="my_robot" exec="battery_monitor" name="battery">
    <param from="$(find-pkg-share my_robot)/config/battery.yaml"/>
  </node>
</launch>
```

`battery_monitor` publishes `/battery` with a message type from the robot's own package,
`my_robot_msgs/msg/BatteryStatus`, that ruro has never seen.

### 1. Import the launch file

In a shell where the ROS 2 workspace is sourced (so that `$(find-pkg-share)` resolves;
without it the substitution is left for `ros2/node` to resolve when it starts):

```sh
ruro ros2 import robot.launch.xml robot:=r1 --params common.yaml \
    --sub /battery=my_robot_msgs/msg/BatteryStatus -o robot.yaml
```

The result is a graph with one `ros2/node` node per ROS node, all in the process group
`ros2`, plus a `ros2/sub` bridge for `/battery`:

```yaml
# Imported from robot.launch.xml by `ruro ros2 import`.
# Everything was converted.
name: robot
nodes:
  r1_talker:
    type: ros2/node
    group: ros2
    params:
      package: demo_nodes_cpp
      executable: talker
      name: talker
      namespace: /r1
      parameters: '{"use_sim_time":false}'
      params_files: |
        /home/me/robot/common.yaml
      remaps: |
        chatter:=/r1/chatter
      respawn: always
  # ... r1_listener, battery
  sub_battery:
    type: ros2/sub
    params:
      topic: /battery
      type: my_robot_msgs/msg/BatteryStatus
```

Read the comment block at the top. Anything the importer could not convert (a `<timer>`
delay, an unsupported substitution such as `$(command ...)`, an unknown tag or attribute)
is listed there and on the terminal; nothing is dropped silently.

If the daemon does not run in a sourced shell, give the nodes the workspace to source:
set `ros_setup: /home/me/ws/install/setup.bash` on each `ros2/node`. If the daemon
listens somewhere other than `tcp/127.0.0.1:7447`, pass `--router` to the import.

Python launch files can only be evaluated by ROS 2. With a sourced workspace,
`ruro ros2 import --eval robot.launch.py` runs `ros2 launch` with a launch prefix that
records each process's command line instead of starting it, and converts those. Respawn
settings are not visible that way, so every node gets `respawn: never`; adjust them in
the graph.

### 2. Run it

```sh
ruro run robot.yaml
ruro status
```

```text
group ros2  running  pid 41
  r1_talker    running   1.0  ...
  r1_listener  running   1.0  ...
```

`ruro logs` shows each ROS node's output under its ruro node id. `ros2 node list` in
another shell lists `/r1/talker` and `/r1/listener` next to the bridge's
`/ruro_sub_battery`. Stopping the graph stops the ROS processes; a crashed one is
restarted according to its `respawn` parameter.

### 3. Look at any topic

`ros2/sub` needs the message definition only to compute the type's hash, and if it cannot
find one it takes the hash the topic's publishers announce. To see the values, ruro needs
the definition: it searches `AMENT_PREFIX_PATH` (a sourced workspace), then the
directories in `RURO_ROS2_INTERFACES` or in a node's `interfaces` parameter, then the
definitions bundled with ruro. Point `interfaces` at the package source if the daemon has
no ROS environment:

```yaml
  sub_battery:
    type: ros2/sub
    params:
      topic: /battery
      type: my_robot_msgs/msg/BatteryStatus
      interfaces: /home/me/ws/src/my_robot_msgs
```

```sh
ruro echo sub_battery.out
```

```text
header:
  stamp:
    sec: 1792426282
    nanosec: 28355981
  frame_id: 'base_link'
voltage: 24.1
cells:
- 4.02
- 4.01
...
```

### 4. Use fields in ruro nodes

`ros2/field` reads one field as a `std/Scalar` (numbers and booleans) or `std/Text`:

```yaml
  voltage:
    type: ros2/field
    params: { path: voltage, interfaces: /home/me/ws/src/my_robot_msgs }
  per_cell:
    type: std/gain
    params: { k: 0.1667 }
wires:
  - sub_battery.out -> voltage.input
  - voltage.value -> per_cell.input
```

Paths index arrays too: `cells[0]`, `poses[-1].position.x`.

### 5. Replace a ROS node

To replace `battery_monitor` with a ruro node, remove its `ros2/node` and add the ruro
node plus a `ros2/pub` for each topic that ROS nodes still read. Messages of a type ruro
has a Rust type for use the typed bridges (`ros2/pub-twist`, ...); any other type goes
through `ros2/pub` from a `ros2/Any` output.

### 6. Bring the bags

```sh
ruro bag import ~/bags/run_2024 --map /battery=sub_battery.out
```

Every topic is kept. Topics with a ruro equivalent become ruro messages; the others are
stored as `ros2/Any` with their exact bytes, type hash and the bag's definition. With the
mapping above, `ruro play` feeds the recorded `/battery` messages into the graph from the
`sub_battery` node's output, so `voltage` and everything after it run on the recording;
add `--raw` to keep every topic byte for byte, even convertible ones.

## What is not converted

- Python launch files without a ROS 2 environment (and their respawn settings with one).
- `$(eval)`, `$(command)` and other substitutions beyond those listed in
  [the ROS 2 bridge page](ros2.md#command-line); `<timer>` delays; `cwd` of nodes.
- Parameter files of composable nodes (`ros2 component load` takes `-p` only).
- `ros2/sub` and `ros2/pub` carry topics. ROS nodes run by `ros2/node` still call each
  other's services and actions through the router as before.
- DDS: ROS nodes must use rmw_zenoh.
