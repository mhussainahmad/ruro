# Writing nodes in Rust

Node authors depend on a single crate, `ruro`, which re-exports the derives, the node API and
the standard message types. Until the crates are published, depend on it by path or git:

```toml
[dependencies]
ruro = { git = "https://github.com/mhussainahmad/ruro" }
```

## A first node

```rust
use ruro::prelude::*;

/// Multiplies a signal by a constant.
#[derive(Node)]
#[node(kind = "my_robot/gain")]
pub struct Gain {
    #[input]
    input: Input<Scalar>,
    #[output]
    output: Output<Scalar>,
    /// Multiplier applied to every sample.
    #[param(default = 2.0, min = -10, max = 10)]
    k: f64,
}

impl Node for Gain {
    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        while let Some(sample) = self.input.recv() {
            self.output.send(Scalar(sample.0 * self.k))?;
        }
        Ok(())
    }
}
```

`#[derive(Node)]` generates the node's descriptor, its constructor and its parameter setter.
The doc comment on the struct becomes the node's description in the studio; doc comments on
parameters become their tooltips.

### Struct attributes

`#[node(...)]` on the struct:

| Key | Meaning |
|---|---|
| `kind = "..."` | Type name used in graphs, conventionally `<library>/<name>`. Required. |
| `rate = 30` | Default timer rate in Hz, used when the graph does not set `rate`. |
| `doc = "..."` | Description; defaults to the struct's doc comment. |

Generic structs are not supported.

### Field attributes

| Attribute | Field type | Meaning |
|---|---|---|
| `#[input]` | `Input<T>` | Input port named after the field. |
| `#[output]` | `Output<T>` | Output port named after the field. |
| `#[param(default = .., min = .., max = .., doc = "..")]` | integer, float, `bool` or `String` | Parameter, editable while running. All keys are optional. |
| *(none)* | anything implementing `Default` | Private state, initialised with `Default::default()`. |

Parameters set in the graph file override the default. When a parameter is changed at run
time (from the studio or `ruro param`), the new value is written to the field before the next
step.

## The `Node` trait

```rust
pub trait Node: Send + 'static {
    /// Called once on the node's thread before the first step.
    fn start(&mut self, ctx: &mut Ctx) -> Result { Ok(()) }

    /// Called whenever the node is triggered: on its timer if it has a rate,
    /// otherwise when any input receives data.
    fn step(&mut self, ctx: &mut Ctx) -> Result;

    /// Called once when the graph stops.
    fn stop(&mut self, ctx: &mut Ctx) {}
}
```

Returning an error from `step` is counted in the node's error metric and logged (at most once
per second); the node keeps running. A panic in `start` or `step` marks the node as failed and
stops it, while the other nodes in its process keep running. If the whole process dies, for
example on a segmentation fault, the daemon restarts it according to the `restart` policy.

A timer-driven node that falls behind skips the ticks it missed instead of running them in a
burst.

`Ctx` gives each step its timing:

| Method | Returns |
|---|---|
| `ctx.id()` | the node's instance id in the graph |
| `ctx.dt()` | time since the previous step (zero on the first) |
| `ctx.time()` | time since the node started |
| `ctx.steps()` | number of steps so far |

## Receiving and sending

`Input<T>`:

| Method | Behaviour |
|---|---|
| `recv()` | Takes the oldest queued message, or `None`. |
| `latest()` | Takes the newest queued message and discards the rest. |
| `len()`, `is_empty()` | Queue length. |

Both return `Option<Arc<Sample<T>>>`. A `Sample` dereferences to the message and also carries
`seq` (per-output sequence number, starting at 1) and `stamp_ns` (send time in nanoseconds
since the Unix epoch).

How many messages an input queues is set per wire with `depth` in the graph (default 1: only
the newest message is kept).

`Output<T>::send(msg)` publishes a message to every connected input. Inside a process the
message is shared, not copied, so sending a large image is as cheap as sending a number.

## Messages

The prelude includes the standard types: `Scalar`, `Text`, `Flag`, `Bytes`, `Vec3`, `Quat`,
`Pose`, `Twist`, `Transform`, `Transforms`, `JointState`, `JointCommand`, `Image` (with
`Encoding`), `CameraInfo`, `PointCloud`, `LaserScan`, `Imu`, `Odometry`, `Joy` and `Header`.

Custom messages are plain structs:

```rust
use ruro::prelude::*;

#[derive(Message, Serialize, Deserialize, Clone)]
#[message(name = "my_robot/Detection")]
pub struct Detection {
    pub x: f32,
    pub y: f32,
    pub score: f32,
}
```

The `name` identifies the type on wires: two ports can only be connected if their message
names match. Messages are serialised with `serde` when they cross a process boundary.

## Packaging nodes as a library

Put your nodes in a library crate. Registration happens at link time, so the crate needs a
function that hosts can call to make sure it is linked:

```rust
// my_robot_nodes/src/lib.rs
mod gain;
mod detector;

/// Forces this crate to be linked into a host.
pub fn link() {}
```

## Building a host

A host is the binary that executes a process group. Build one that links your node crates:

```toml
# my-host/Cargo.toml
[dependencies]
ruro-host = { git = "https://github.com/mhussainahmad/ruro" }
my_robot_nodes = { path = "../my_robot_nodes" }
```

```rust
// my-host/src/main.rs
fn main() {
    my_robot_nodes::link();
    ruro_host::main();
}
```

The standard nodes are always included. Point the daemon at your host:

```sh
cargo build --release -p my-host
ruro daemon --host target/release/my-host
```

`ruro nodes -v` (against that daemon) now lists your node types next to the standard ones, and
the studio shows them in its node library. The daemon also honours the `RURO_HOST`
environment variable.

## Testing nodes

Nodes are ordinary structs: keep the processing in plain functions and unit-test those, as the
standard nodes in `nodes/ruro-nodes-std` do. To check a graph that uses your nodes against
your host:

```sh
RURO_HOST=target/release/my-host ruro check my-graph.yaml
```

When a daemon is running, `ruro check` asks it for the node types instead, so the variable is
only needed without one.

## The standard nodes

| Type | Inputs | Outputs | Parameters |
|---|---|---|---|
| `std/fake-camera` (30 Hz) | | `image: Image` | `width`, `height`, `speed` |
| `std/blur` | `image: Image` | `out: Image` | `radius` |
| `std/grayscale` | `image: Image` | `out: Image` | |
| `std/image-stats` | `image: Image` | `brightness: Scalar` | |
| `std/sine` (100 Hz) | | `value: Scalar` | `frequency`, `amplitude`, `offset` |
| `std/gain` | `input: Scalar` | `output: Scalar` | `k` |
| `std/pid` | `setpoint: Scalar`, `measured: Scalar` | `command: Scalar` | `kp`, `ki`, `kd`, `limit` |
| `std/fake-arm` (200 Hz) | `velocity: Scalar` | `position: Scalar`, `state: JointState` | `lag`, `max_speed` |

Run `ruro nodes -v` for ranges, defaults and descriptions.
