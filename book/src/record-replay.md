# Record and replay

Recording captures the messages flowing through a running graph so that you can replay
them later: to debug a run after the fact, to develop perception without the robot, or to
compare two versions of a node on the same input.

> **Status:** implemented and tested end to end through the daemon (record a graph, replay
> it, compare downstream outputs), through two daemons on one machine (a graph spanning
> both, a peer that goes away mid-recording) and against a rosbag2 fixture. Clock-offset
> correction is unit-tested only: both test daemons share one clock.

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
- For a graph that spans [machines](multi-machine.md), every machine records its own groups
  and `ruro record --stop` collects their parts into the one file (see
  [below](#recordings-across-machines)).
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

## Recordings across machines

Recording and replay cover every machine of a [multi-machine graph](multi-machine.md).
Talk to the daemon the graph was deployed to:

- `ruro record` asks every peer running groups of the graph to record them. The reply names
  the groups on each machine, for example
  ``recording 3 group(s) to /data/run1.mcap: b; on `robot`: a, c``.
- `ruro record --stop` collects each peer's part and merges it into the one file. The reply
  names the groups recorded on each machine (``from `robot`: a, c``) and every machine that
  could not record (``not recorded: the groups on `robot` (...)``). A machine that goes away
  mid-recording loses its part; the file holds everything else.
- Replay mutes each recorded node on the machine that runs it and sends the recorded
  messages there, so nodes downstream on any machine are fed.

Log times are each machine's own clock. When a recording starts, the daemon measures each
peer's clock against its own (the peer's reading against the middle of a round trip, best of
eight). If the offset is larger than half the round trip, which is all the measurement can
resolve, the peer's part is shifted onto the daemon's clock when it is merged and the reply
says so: ``from `robot`: a (clock +41.2 ms ahead, corrected to within 0.3 ms)``. Otherwise
the times are left as they are. The correction is a single offset per recording: it does
not follow clocks that drift during a long recording, and on a busy network it is only as
good as the round trip. Keep the robot's computers synchronised with chrony (NTP) or PTP
when you compare their timestamps closely. A peer from an older version that cannot report
its clock is merged uncorrected, and the reply says `clock offset unknown`.

A large peer part can take longer than the CLI's 10 s request timeout to collect. The
daemon still writes the file.

## ROS 2 bags

```sh
ruro bag import ~/bags/run_2024 --map /camera/image_raw=camera.image [--raw]
```

converts a rosbag2 bag (a directory or an `.mcap` file, MCAP storage) into a ruro recording,
by default `<src>.ruro.mcap` (`-o` to choose). `--map` (repeatable) names a topic's channel,
so that replay feeds the graph's `camera` node.

Every topic is kept. Topics of these types become ruro messages: `sensor_msgs/Image` (rgb8,
rgba8, bgr8, bgra8, mono8, mono16, and 8UC1/8UC3/8UC4/16UC1), `PointCloud2` (float x/y/z), `LaserScan`,
`Imu`, `geometry_msgs/Twist`, `TwistStamped`, `PoseStamped`, `std_msgs/Float64`,
`nav_msgs/Odometry` and `tf2_msgs/TFMessage`. Every other topic becomes a `ros2/Any`
channel holding each message's exact bytes with its type name and RIHS01 hash (recorded by
rosbag2, or computed from the definition the bag holds, in `.msg` or `.idl` form); the
channel's metadata keeps that definition and the topic's QoS profiles. Map such a topic to
the output of a [`ros2/sub`](ros2.md#any-message-type) node and replay feeds the graph
from it, and a `ros2/pub` downstream publishes the messages on ROS 2 again. A message of a
convertible type that fails to convert is kept the same way, on a channel named
`<channel>_raw`. `--raw` keeps every topic as `ros2/Any`, even convertible ones.

The import prints, per topic, how many messages were converted, kept raw or skipped (only
messages that are not CDR are skipped) and why. Bags in SQLite storage must first be
converted with `ros2 bag convert` (storage `mcap`).
