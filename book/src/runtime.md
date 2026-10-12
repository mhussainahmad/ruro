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
  output. `ruro check` does not.

None of the bundled nodes publishes `std/Clock` yet (the MuJoCo node publishes its time as
a `std/Scalar`), so `clock: sim` is for your own simulator nodes for now. Composite nodes
and `clock: sim` do not mix yet: flattening a graph that contains a composite drops the
`clock` setting, so such a graph runs on wall time.

## Emergency stop

```sh
ruro estop            # engage
ruro estop --release
```

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

Limits to be aware of:

- **It is a software stop.** It is no substitute for a hardware emergency stop.
- **The bundled hardware drivers do not implement `on_estop` yet**, and none of them checks
  `ctx.estopped()`: engaging the stop does not stop a `hw/dynamixel`, `hw/so100-arm` or
  `hw/odrive` node.
- **It is per daemon.** `ruro estop` reaches the groups of the daemon it is sent to, not
  groups that run on [other machines](multi-machine.md). Send it to each daemon.
- The studio has no e-stop control.

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
