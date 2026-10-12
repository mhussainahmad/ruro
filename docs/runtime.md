# Runtime guide

How to use the runtime features beyond running a graph: zero-allocation
publishing, simulated time, real-time groups, the emergency stop, recording
and replay, graphs that span machines, and securing a daemon.

## Publishing without allocating

`Output::send` takes ownership of a message, so every send allocates it.
For large messages sent at a high rate, borrow a pooled one instead:

```rust
let mut frame = self.image.loan();          // a frame every receiver has dropped
frame.reshape(640, 480, Encoding::Rgb8);    // keeps the pixel buffer
render_into(&mut frame.data);
frame.send()?;
```

Once the pool is warm (a handful of messages) a `loan`/`send` loop does not
allocate. Inputs fed from other processes recycle their messages the same
way and decode into the existing buffers (`Message::decode_into`).

## Graph time and simulation

`Ctx::now_ns()` is graph time: wall time by default. With `clock: sim` the
graph follows its one `std/Clock` output (`ruro::msg::Clock { time_ns }`),
typically published by a simulator bridge:

```yaml
name: sim-demo
clock: sim
nodes:
  sim: { type: my/simulator, rate: 1000 }   # has an output of type std/Clock
  ctrl: { type: std/pid }
```

Timer-driven nodes then step on graph time: a `rate: 100` node steps every
10 ms of simulated time however fast the simulation runs, and `ctx.dt()` is
the graph-time step. The node that publishes the clock is itself paced by
the wall clock. Other groups receive the clock over shared memory (or the
network), so every process follows the same time.

## Real-time groups

```yaml
groups:
  control:
    realtime:
      priority: 80          # SCHED_FIFO 1-99; also locks memory (mlockall)
      cpus: [3]             # pin the group's threads
      deterministic: true   # one thread, fixed order, fixed rate
      rate: 1000            # cycle rate; defaults to the fastest node rate
      deadlines_us: { pid: 200 }
```

- **Pinning and priority** apply to every node thread of the group. Without
  `CAP_SYS_NICE` (or an `rtprio` limit, see `ulimit -r`) SCHED_FIFO is
  refused; the group then runs with normal scheduling and logs a warning that
  says so. The same goes for `mlockall` and the `memlock` limit.
- **Deterministic groups** run all their nodes on one thread. Each cycle steps
  the nodes in wire order (producers first; feedback loops are cut in
  declaration order), so data produced in a cycle is consumed in the same
  cycle without queueing. A node with `rate` r steps every `rate / r` cycles;
  input-triggered nodes step in cycles where their inputs received data.
  A cycle that runs past the start of the next one counts as an overrun.
- **Deadlines** are per node, in microseconds. A step that takes longer is
  reported as an error (`last_error` says by how much) and counted.

`ruro status` shows the p99 timer wakeup jitter of timer-driven nodes, and
deadline misses and overruns when there are any. The same numbers are in
`NodeMetrics.timing`.

PREEMPT_RT kernels give far better jitter than stock ones; measure your
machine with `cargo bench -p ruro-host --bench jitter`.

## Emergency stop

```sh
ruro estop            # engage
ruro estop --release
```

The daemon latches the state and republishes it twice a second, so hosts
that start or restart while it is engaged come up stopped. When it engages,
every node's `on_estop` hook runs once on the node's thread; nodes keep
stepping (sensors and logging stay live) and see `ctx.estopped() == true`.

The convention for nodes that drive actuators: command a safe state in
`on_estop` and keep commanding it while `ctx.estopped()` is true.

```rust
impl Node for Wheels {
    fn step(&mut self, ctx: &mut Ctx) -> Result {
        let cmd = if ctx.estopped() { Twist::default() } else { self.target };
        self.driver.command(cmd)
    }

    fn on_estop(&mut self, _ctx: &mut Ctx) {
        let _ = self.driver.brake();
    }
}
```

The daemon also restarts host processes that stop publishing metrics for
`--heartbeat-timeout` seconds (default 5), for example because they hang.

## Recording and replay

```sh
ruro record /data/run1.mcap      # every output of the running graph
ruro record --stop               # merge and write the file
ruro info /data/run1.mcap        # channels, counts, duration

ruro play /data/run1.mcap --rate 0.5 [--graph g.yaml]
ruro playback pause | resume | seek 12.5 | rate 2 | stop
```

Recordings are [MCAP](https://mcap.dev) files with one channel per output
port: topic `node.port`, schema named after the message type, encoding
`ruro-raw` (types with their own layout, such as images) or `ruro-postcard`.
Messages are the bytes that cross shared memory; log time is the send stamp.
Each host writes its own segment and the daemon merges them, so recording
never moves messages between processes. Paths are on the daemon's machine.

Replay feeds a recording into the running graph. Nodes that appear in the
recording stop stepping and their recorded messages are published on their
outputs instead, so everything downstream (in any process) runs on the
recorded data. To replay only some nodes, record (or keep) only their
channels.

### ROS 2 bags

```sh
ruro bag import ~/bags/run_2024 --map /camera/image_raw=camera.image
```

imports a rosbag2 bag in MCAP storage. Supported types: `sensor_msgs/Image`
(rgb8, rgba8, bgr8, bgra8, mono8), `PointCloud2` (float x/y/z), `LaserScan`,
`Imu`, `geometry_msgs/Twist`, `TwistStamped`, `PoseStamped`,
`std_msgs/Float64`, `nav_msgs/Odometry` and `tf2_msgs/TFMessage`. Other
topics are listed as skipped. Bags in SQLite storage must first be converted
with `ros2 bag convert` (storage `mcap`).

## Graphs across machines

Start a daemon on each machine and connect them:

```sh
robot$  ruro daemon --name robot --listen tcp/0.0.0.0:7447
laptop$ ruro daemon --name laptop --peer tcp/robot:7447
```

Then name a machine per group; groups without one run on the daemon the
graph is deployed to:

```yaml
groups:
  drivers: { machine: robot }
nodes:
  cam: { type: std/fake-camera, group: drivers }
  stats: { type: std/image-stats, group: vision }
```

The deploying daemon validates the whole graph (asking peers for their node
types), sends each remote group to its daemon and starts its own. Wires
between machines go over Zenoh, lz4-compressed for messages of 4 KiB and up
when that helps (hosts accept `--no-compress`); wires between groups on one
machine stay in shared memory. `ruro stop` stops every machine's part.
`ruro fleet` lists all reachable daemons.

### Custom host binaries

```sh
ruro deploy --to robot --host-binary target/release/my-host
```

uploads a host binary (checked against its SHA-256) that groups select with
`groups.<name>.host: my-host`. Uploads live in the daemon's runtime
directory and are lost on reboot. Updating the daemon itself remotely is not
supported.

## Securing a daemon

By default a daemon listens on `tcp/127.0.0.1:7447` and only local
processes can reach it. To accept remote clients, use TLS and a token:

```sh
ruro daemon --listen tls/0.0.0.0:7447 \
    --tls-cert robot.pem --tls-key robot-key.pem --token "$(cat token)"

ruro --connect tls/robot:7447 --tls-ca ca.pem --token "$(cat token)" status
```

- The certificate's name must match the host name clients connect to
  (`tls/robot:7447` needs a certificate for `robot`).
- `--token` (or `RURO_TOKEN`) enables Zenoh username/password authentication
  with the user `ruro`: sessions without the same token are refused, on every
  endpoint, including peer daemons. The daemon keeps the token in a file
  readable only by its user and hands it to its hosts in their environment.
- Giving the daemon `--tls-ca` as well makes it require client certificates
  signed by that CA (mutual TLS); clients then pass their own `--tls-cert`
  and `--tls-key`.
- Peer daemons connect with the same options: start them with the same token
  (and, with TLS peers, a CA that trusts the other daemon's certificate).
- Hosts always talk to their daemon over a Unix socket in the daemon's
  runtime directory.

Programs built on `ruro-proto` connect with `ruro_proto::connect_with` and a
`Security` holding the CA and token.

## Soak testing and benchmarks

```sh
ruro soak examples/camera.yaml --duration 3600 --max-rss-growth-mb 16
```

runs a graph (starting a daemon if none is reachable), samples memory of
every host process, step latency and drops, prints a summary and fails if
memory grew after the warm-up, a group restarted or a node failed.

```sh
scripts/bench.sh save main        # on the base branch
scripts/bench.sh compare main     # on a change; fails on regressions
```
