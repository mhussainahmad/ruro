# Runtime features

How to use the runtime beyond running a graph: publishing without allocating, simulated
time, the emergency stop, hang detection, and soak tests and benchmarks. Real-time
scheduling, recording and graphs that span machines have their own pages:
[Real-time scheduling](realtime.md), [Record and replay](record-replay.md) and
[Multi-machine graphs](multi-machine.md).

## Publishing without allocating

`Output::send` takes ownership of a message, so every send allocates it. For large
messages sent at a high rate, borrow a pooled one instead:

```rust
let mut frame = self.image.loan();          // a frame every receiver has dropped
frame.reshape(640, 480, Encoding::Rgb8);    // keeps the pixel buffer
render_into(&mut frame.data);
frame.send()?;
```

`loan()` needs a message type that implements `Default`. The pool keeps six messages, so
once it is warm a `loan`/`send` loop does not allocate. A loan dropped without `send` is
discarded.

Inputs fed from other processes keep a small pool of received messages too (the wire's
`depth` plus two). `Image` and `PointCloud` decode into the buffers of a recycled message
(`Message::decode_into`); other types decode into a new value.

`Output` is also `Clone`: a clone shares the sequence counter, which is handy for sending
from a driver's callback thread.

## Graph time and simulation

`Ctx::now_ns()` is graph time: wall time by default. With `clock: sim` the graph follows
its one `std/Clock` output (`ruro::msg::Clock { time_ns }`, not in the prelude), typically
published by a simulator bridge:

```yaml
name: sim-demo
clock: sim
nodes:
  sim: { type: my/simulator, rate: 1000 }   # has an output of type std/Clock
  ctrl: { type: std/pid }
```

Timer-driven nodes then step on graph time: a `rate: 100` node steps every 10 ms of
simulated time however fast the simulation runs, and `ctx.dt()` is the graph-time step.
The node that publishes the clock is itself paced by the wall clock. Other groups receive
the clock over shared memory (or the network), so every process follows the same time.

- Until the first clock message arrives, `now_ns()` is 0 and timer-driven nodes do not
  step.
- A node that falls behind the simulated clock catches up at most 100 ticks at once; a
  clock that goes backwards gives `dt` = 0.
- Message stamps (`Sample::stamp_ns`) stay wall time.
- The daemon checks at deploy time that a `clock: sim` graph has exactly one `std/Clock`
  output, and so does `ruro check` when it can reach a daemon.

The MuJoCo node (`sim/mujoco`) publishes its simulation time as a `std/Clock`; see
`examples/arm.yaml`, whose `real` overlay switches the graph back to the wall clock. Flattening composite nodes keeps the graph's `clock` setting.

## Emergency stop

```sh
ruro estop                  # engage
ruro estop --release        # release the stop engaged at this daemon
ruro estop --release --all  # also lift stops engaged at other daemons
```

The studio has an **E-STOP** button and <kbd>F12</kbd> for the same (see
[Studio](studio.md#emergency-stop)).

The daemon latches the state and republishes it twice a second, so hosts that start or
restart while it is engaged come up stopped. When it engages, every node's `on_estop` hook
runs once on the node's thread (again after each release and re-engage); nodes keep
stepping (sensors and logging stay live) and see `ctx.estopped() == true`.

The convention for nodes that drive actuators: command a safe state in `on_estop` and keep
commanding it while `ctx.estopped()` is true.

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

The bundled nodes that command actuators follow this convention. While the stop is
engaged they ignore incoming commands, and after the release they keep their safe state
until a command arrives that was sent after the release:

| Node | Safe state |
|---|---|
| `std/fake-arm` | commanded velocity zero; the joint slows down with its usual lag |
| `std/scan-avoid` | sends a zero twist for every scan |
| `hw/dynamixel`, `hw/so100-arm` | the servos hold their present position; torque stays enabled so the arm does not fall |
| `hw/odrive` | axes stay in closed-loop control: zero velocity (velocity mode), hold the present position (position mode) or zero torque (torque mode, the joint goes limp) |
| `hw/joint-jog` | stick input is ignored and the targets follow the measured joint state, so the arm holds where it is |
| `ml/policy` | no actions are sent and the current action chunk is dropped; after the release the policy runs on the latest observations |
| `ros2/pub-twist` | publishes a zero twist when the stop engages and in place of every input while stopped |
| `sim/mujoco` | zero velocity or effort controls, or hold the present joint positions for position actuators; the physics keeps running |

Each node's documentation (`ruro nodes -v`) describes its behaviour in detail. Python nodes
have no e-stop hook yet.

Limits to be aware of:

- **It is a software stop.** It is no substitute for a hardware emergency stop, and the
  hardware drivers have not yet been tested on the physical devices.
- **It stops the whole graph, wherever it is engaged.** A stop engaged at a daemon is
  passed on to the daemons running the rest of its graph on
  [other machines](multi-machine.md). A stop engaged directly on one of those machines
  goes back to the daemon that deployed the graph, which passes it on to the others. Each
  stop is latched under the daemon it was engaged at, and a release lifts only that
  daemon's own stop, on every machine; `--release --all` lifts every stop latched at one
  daemon, there only. `ruro status` and the studio show which machines are stopped.
- **A machine that cannot be reached is reported, not waited for.** The others still stop,
  and the command fails naming the machine.

## Hung processes

The daemon also restarts host processes that stop publishing metrics for
`--heartbeat-timeout` seconds (default 5, at least 0.5), for example because they hang. A
host that has just started gets at least 30 seconds before its first report. Such a
restart counts as a failure for the [restart policy](reference/graph-yaml.md#restart-policy).

## Soak tests

```sh
ruro soak examples/camera.yaml --duration 3600 --max-rss-growth-mb 16
```

runs a graph (starting a daemon if none is reachable), samples the memory of every host
process, step latency and drops every `--interval` seconds (default 5), prints a summary
and fails if a group's memory grew by more than `--max-rss-growth-mb` after the
`--warmup` (default 60 s), a group restarted or a node failed. `--duration` defaults to
600 s; `--csv <file>` also writes every sample. The command is hidden from `ruro --help`.

## Benchmarks

```sh
cargo bench -p ruro-node -p ruro-transport     # message passing, see Performance
cargo bench -p ruro-host --bench jitter        # timer wakeup jitter at 1 kHz

scripts/bench.sh save main        # on the base branch
scripts/bench.sh compare main     # on a change; fails on regressions
```

The jitter bench runs for `RURO_JITTER_SECS` seconds (default 10). `scripts/bench.sh`
compares Criterion results and fails when a benchmark is more than `BENCH_THRESHOLD`
percent slower (default 10); `BENCH_PACKAGES` (default `ruro-node ruro-transport`) and
`BENCH_ARGS` select what runs.
