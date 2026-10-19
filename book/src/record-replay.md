# Record and replay

Recording captures the messages flowing through a running graph so that you can replay
them later: to debug a run after the fact, to develop perception without the robot, or to
compare two versions of a node on the same input.

> **Status:** implemented and tested end to end through the daemon (record a graph, replay
> it, compare downstream outputs) and against a rosbag2 fixture.

## Recording

```sh
ruro record /data/run1.mcap      # every output of the groups running here
ruro record --stop               # merge and write the file
ruro info /data/run1.mcap        # channels, counts, duration
```

```text
$ ruro info run1.mcap
duration  12.532 s  (21:37:24.484 to 21:37:37.015)
messages  566
channels  4
TOPIC             TYPE        ENCODING       COUNT    HZ
blur.out          std/Image   ruro-raw          67   5.3
camera.image      std/Image   ruro-raw         365  29.0
gray.out          std/Image   ruro-raw          67   5.3
stats.brightness  std/Scalar  ruro-postcard     67   5.3
```

Recordings are [MCAP](https://mcap.dev) files with one channel per output port: topic
`node.port`, schema named after the message type, encoding `ruro-raw` (types with their own
layout, such as images) or `ruro-postcard`. Messages are the bytes that cross shared memory;
log time is the send stamp. Each host writes its own segment (under the daemon's runtime
directory) and the daemon merges them, so recording never moves messages between processes.

- A graph must be running, and the file's directory must exist.
- Only the groups running on the daemon you talk to are recorded. For a graph that spans
  [machines](multi-machine.md), the groups on other machines are not in the file; start a
  recording on each of their daemons as well.
- The path is on the daemon's machine. `ruro record` makes a relative path absolute against
  the directory you run it in, so use absolute paths when the daemon is remote.
- `ruro stop`, or deploying another graph, also ends the recording.
- `ruro info` reads a local file.

The studio has a record button and a recording indicator (see
[Studio](studio.md#recording-and-replay)).

## Replay

```sh
ruro play /data/run1.mcap --rate 0.5 [--graph g.yaml]
ruro playback pause | resume | seek 12.5 | rate 2 | stop
```

Replay feeds a recording into the running graph (or deploys `--graph` first). Nodes that
appear in the recording stop stepping and their recorded messages are published on their
outputs instead, so everything downstream (in any process) runs on the recorded data. To
replay only some nodes, record (or keep) only their channels. `ruro play` shows the
position until the replay ends; <kbd>Ctrl</kbd>+<kbd>C</kbd> stops it. `ruro playback`
controls it from another terminal, and the studio shows a timeline while it plays.

Recording and replay cover the process groups of the daemon you talk to. In a
[multi-machine graph](multi-machine.md), groups running on other machines are neither
recorded nor fed.

## ROS 2 bags

```sh
ruro bag import ~/bags/run_2024 --map /camera/image_raw=camera.image
```

converts a rosbag2 bag (a directory or an `.mcap` file, MCAP storage) into a ruro recording,
by default `<src>.ruro.mcap` (`-o` to choose). `--map` (repeatable) names a topic's channel,
so that replay feeds the graph's `camera` node. Supported types: `sensor_msgs/Image` (rgb8,
rgba8, bgr8, bgra8, mono8, mono16, and 8UC1/8UC3/8UC4/16UC1), `PointCloud2` (float x/y/z), `LaserScan`,
`Imu`, `geometry_msgs/Twist`, `TwistStamped`, `PoseStamped`, `std_msgs/Float64`,
`nav_msgs/Odometry` and `tf2_msgs/TFMessage`. Other topics are listed as skipped. Bags in
SQLite storage must first be converted with `ros2 bag convert` (storage `mcap`).
