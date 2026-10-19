# Command line

```text
ruro [OPTIONS] <COMMAND>
```

`ruro` is the command-line client for the daemon, and also runs the daemon itself.

## Global options

These apply to every command.

| Option | Environment | Default | Meaning |
|---|---|---|---|
| `--connect <ENDPOINT>` | `RURO_CONNECT` | `tcp/127.0.0.1:7447` | Daemon endpoint (`tcp/...` or `tls/...`). |
| `--daemon <NAME>` | `RURO_DAEMON` | discovered | Daemon name, if several are reachable through one router. |
| `--tls-cert <PEM>` | `RURO_TLS_CERT` | | The daemon's certificate for `ruro daemon`; otherwise a client certificate for mutual TLS. |
| `--tls-key <PEM>` | `RURO_TLS_KEY` | | Private key of `--tls-cert`. |
| `--tls-ca <PEM>` | `RURO_TLS_CA` | | CA certificate that signed the other side's certificate. |
| `--token <TOKEN>` | `RURO_TOKEN` | | Shared access token. A daemon started with one only accepts clients and peers that present it. |
| `-h`, `--help` | | | Print help. |
| `-V`, `--version` | | | Print the version. |

See [Security](../security.md) for TLS and tokens.

Other environment variables:

| Variable | Meaning |
|---|---|
| `RURO_LOG` | Log filter in `tracing` syntax. Default `info,zenoh=warn`. Logs go to stderr. |
| `RURO_HOST` | Host binary used by the daemon (and by `ruro check`/`ruro nodes` without a daemon) when `--host` is not given. |

## Commands

| Command | Summary |
|---|---|
| [`run`](#ruro-run) | Run a graph in the foreground. Starts a daemon if none is reachable. |
| [`daemon`](#ruro-daemon) | Run the per-machine daemon in the foreground. |
| [`deploy`](#ruro-deploy) | Deploy a graph to the daemon and return; upload host binaries. |
| [`stop`](#ruro-stop) | Stop the running graph. |
| [`check`](#ruro-check) | Validate a graph against the available node types. |
| [`overlays`](#ruro-overlays) | List the overlays of a graph file and what each changes. |
| [`status`](#ruro-status) | Show the daemon, its groups and live node metrics. |
| [`fleet`](#ruro-fleet) | List every daemon reachable through the connected one. |
| [`nodes`](#ruro-nodes) | List available node types. |
| [`echo`](#ruro-echo) | Print live values of an output port. |
| [`param`](#ruro-param) | Change a node parameter while the graph runs. |
| [`kill`](#ruro-kill) | Kill a group process to exercise supervision. |
| [`logs`](#ruro-logs) | Stream logs from the running graph. |
| [`estop`](#ruro-estop) | Engage or release the emergency stop. |
| [`record`](#ruro-record) | Record the outputs of the groups running on the daemon into an MCAP file. |
| [`play`](#ruro-play) | Replay a recording into the running graph. |
| [`playback`](#ruro-playback) | Control a running replay. |
| [`info`](#ruro-info) | Show the channels, message counts and duration of a recording. |
| [`bag import`](#ruro-bag-import) | Convert a ROS 2 bag into a ruro recording. |
| [`studio`](#ruro-studio) | Open the desktop studio. |
| [`schema`](#ruro-schema) | Print message schemas, check compatibility, generate Python types. |
| [`new`](#ruro-new) | Create a node package or add a node to one. |
| [`build`](#ruro-build) | Build a host binary that links the packages listed in `ruro.toml`. |
| [`ros2`](#ruro-ros2) | List ROS 2 topics, generate message types, compute type hashes. |
| [`soak`](#ruro-soak) | Run a graph for a while and check memory, latency and drops (hidden). |

### ruro run

```text
ruro run <GRAPH> [--overlay <NAME>]...
```

Deploys the graph file (with its subgraph files expanded), prints the graph's logs and stays
in the foreground until <kbd>Ctrl</kbd>+<kbd>C</kbd>, then stops the graph. If no daemon is
reachable at `--connect`, `run` starts one in-process, listening on that endpoint (named
after `--daemon` or the hostname, with the default host), and shuts it down on exit.

`--overlay` (short `-o`) applies one of the graph's [overlays](graph-yaml.md#overlays) before
deploying; repeat it to apply several, in the order given. `deploy`, `check`, `play --graph`
and `soak` take it too. An unknown overlay name is an error.

### ruro daemon

```text
ruro daemon [--name <NAME>] [--listen <ENDPOINT>] [--host <HOST>] [--peer <ENDPOINT>]...
            [--heartbeat-timeout <SECONDS>]
```

| Option | Default | Meaning |
|---|---|---|
| `--name <NAME>` | hostname | Name other tools use to address this daemon. |
| `--listen <ENDPOINT>` | `tcp/127.0.0.1:7447` | Endpoint to listen on. Use `tcp/0.0.0.0:7447`, or `tls/0.0.0.0:7447` with `--tls-cert`/`--tls-key`, to accept remote clients. |
| `--host <HOST>` | `RURO_HOST`, else `ruro-host` next to `ruro`, else `PATH` | Host binary used to run graph groups. |
| `--peer <ENDPOINT>` | | Another daemon to connect to, e.g. `tcp/robot2:7447`. Repeatable. See [Multi-machine graphs](../multi-machine.md). |
| `--heartbeat-timeout <SECONDS>` | 5 | Seconds without metrics after which a host is restarted (at least 0.5). |

Runs until <kbd>Ctrl</kbd>+<kbd>C</kbd> or `SIGTERM`, then stops the running graph. The
daemon's runtime directory is `$XDG_RUNTIME_DIR/ruro-<name>/` (or `ruro-<name>/` in the
system temporary directory). It holds the deployed `graph.yaml`, the Unix socket hosts
connect to, the token file, uploaded host binaries and recording segments.

### ruro deploy

```text
ruro deploy [GRAPH] [--overlay <NAME>]... [--to <NAME>] [--host-binary <PATH>]
            [--host-name <NAME>]
```

Sends the graph to the daemon (with its subgraph files expanded), which validates it and
starts one process per group, replacing any graph already running. Returns immediately.
`--to` names the daemon (same as `--daemon`). `--host-binary` first uploads a host binary
that groups can select with `host: <name>`, stored under its file name or `--host-name`.
Give a graph, a binary or both.

### ruro stop

```text
ruro stop
```

Stops the running graph, on every machine it spans.

### ruro check

```text
ruro check <GRAPH> [--overlay <NAME>]...
```

Validates the graph: node ids, types, parameters and their ranges, rates, wires and message
types, and composite nodes. The node types come from the daemon if one is reachable,
otherwise from the local host binary. Prints a summary and exits with status 0 if the graph
is valid; otherwise prints each problem and exits with status 1. Some checks only happen at
deployment (see [Validation](graph-yaml.md#validation)). With `--overlay`, the graph is
checked with the overlays applied; without, the base graph is checked, along with whether
each overlay applies to it.

```text
$ ruro check examples/camera.yaml
examples/camera.yaml: ok (4 nodes, 3 wires, 2 process groups)
```

### ruro overlays

```text
ruro overlays <GRAPH>
```

Lists the graph's [overlays](graph-yaml.md#overlays) and, under each, what it changes. Works
without a daemon.

```text
$ ruro overlays examples/pid.yaml
stiff
    pid: params kd, kp
scaled
    - wire target.value -> pid.setpoint
    half: type std/gain; params k
    arm: params max_speed
    + wire target.value -> half.input
    + wire half.output -> pid.setpoint
```

### ruro status

```text
ruro status
```

Prints the daemon, the deployed graph's process groups (state, pid, restarts, last exit) and
live per-node metrics: state, rate, step time p50/p99, input age, CPU, errors and timer
jitter p99. It also shows the emergency stop, recording and replay when active.

### ruro fleet

```text
ruro fleet
```

Lists every daemon reachable through the connected one: name, version, graph, running
groups, uptime, CPU and state.

### ruro nodes

```text
ruro nodes [-v]
```

Lists the node types the daemon's host (or the local host binary) provides. `-v`,
`--verbose` also shows ports and parameters.

### ruro echo

```text
ruro echo <NODE.PORT>
```

Prints live values of an output port until <kbd>Ctrl</kbd>+<kbd>C</kbd>, for example
`ruro echo cam.image`. Images are summarised; numbers and small messages are printed as
values.

### ruro param

```text
ruro param <NODE> <NAME> <VALUE>
```

Changes a parameter of a running node. `VALUE` is parsed as JSON when possible (`12`, `0.5`,
`true`, `"text"`), otherwise it is sent as a string. The change is not written back to the
graph file. The value is not checked against the parameter's range, and an unknown node or
parameter is only reported in the graph's logs.

### ruro kill

```text
ruro kill <GROUP>
```

Kills the process running a group (`SIGKILL`), to exercise supervision. The daemon restarts
it according to the group's restart policy.

### ruro logs

```text
ruro logs
```

Streams new log lines from every process of the running graph until
<kbd>Ctrl</kbd>+<kbd>C</kbd>.

### ruro estop

```text
ruro estop [--release [--all]]
```

Engages the emergency stop on the daemon and the machines running the rest of its graph.
`--release` releases the stop engaged at this daemon, there and on those machines; with
`--all` it also lifts stops that other daemons engaged here. See
[Emergency stop](../runtime.md#emergency-stop).

### ruro record

```text
ruro record <FILE>
ruro record --stop
```

Starts recording every output of the groups running on the daemon (not groups on other
machines of a multi-machine graph) into an MCAP file on the daemon's machine, or stops the recording and writes the file. See
[Record and replay](../record-replay.md).

### ruro play

```text
ruro play <FILE> [--rate <RATE>] [--graph <GRAPH> [--overlay <NAME>]...]
```

Replays a recording into the running graph (or deploys `--graph` first): the recorded nodes
stop and their recorded outputs feed the rest of the graph. `--rate` is the speed (default
1, real time).

### ruro playback

```text
ruro playback pause | resume | seek <SECONDS> | rate <RATE> | stop
```

Controls a running replay.

### ruro info

```text
ruro info <FILE>
```

Shows the channels, message counts and duration of a local recording.

### ruro bag import

```text
ruro bag import <SRC> [-o <OUT>] [--map <TOPIC=NODE.PORT>]...
```

Converts a rosbag2 bag (directory or `.mcap` file, MCAP storage) into a ruro recording,
by default `<SRC>.ruro.mcap`. `--map` names a topic's channel so that replay feeds the
right node. See [ROS 2 bags](../record-replay.md#ros-2-bags).

### ruro studio

```text
ruro studio [ARGS]...
```

Starts `ruro-studio` (from next to `ruro`, else from `PATH`), passing the remaining arguments
through. See [Studio](../studio.md) for its options.

### ruro schema

```text
ruro schema [TYPE] [--all] [--host <HOST>] [--python]
ruro schema diff <OLD> <NEW> [--json]
```

Without arguments, lists the message types and their schema hashes. With a type (`std/Pose`
or `Pose`), prints its schema as JSON; `--all` prints every schema as a JSON array.
`--host` reads the schemas of a host built with `ruro build` instead of the types linked
into the CLI. `--python` generates the Python dataclasses for the standard messages.
`diff` compares two schema files and exits with an error if any change is breaking. See
[Message schemas](../packages.md#message-schemas).

### ruro new

```text
ruro new package <NAME> [--path <DIR>] [--ruro-path <DIR>] [--ruro-git <URL>]
ruro new node <KIND> [--package <DIR>]
```

Creates a node package (a crate with an example node, its test and a `ruro.toml`), or adds
a node to the package in the current directory. See
[Node packages and hosts](../packages.md).

### ruro build

```text
ruro build [--manifest <FILE>] [--debug] [--target-dir <DIR>] [--lockfile <FILE>] [--offline]
```

Builds a host binary that links the packages listed in `ruro.toml` (default manifest), in
`.ruro/host/target` unless `--target-dir` says otherwise. `--debug` builds without
optimisations; `--lockfile` gives a `Cargo.lock` to start from; `--offline` does not access
the network.

### ruro ros2

```text
ruro ros2 topics [--router <ENDPOINT>] [--domain <ID>] [--timeout <MS>] [--json]
ruro ros2 gen <PACKAGE_DIR>... [-o <FILE>] [--root <PATH>]
ruro ros2 hash <TYPE> [PACKAGE_DIR]... [--verbose]
```

Lists ROS 2 topics announced through rmw_zenoh (router default `tcp/127.0.0.1:7447`,
domain from `ROS_DOMAIN_ID` or 0, timeout 1000 ms), generates Rust types from `.msg`/`.srv`
definitions (module path `crate::ros` by default), and prints the RIHS01 hash of a message
type such as `geometry_msgs/msg/Twist`. See [ROS 2 bridge](../ros2.md).

### ruro soak

```text
ruro soak <GRAPH> [--overlay <NAME>]... [--duration <S>] [--warmup <S>] [--interval <S>]
                  [--max-rss-growth-mb <MIB>] [--csv <FILE>]
```

Runs a graph (starting a daemon if none is reachable) and fails on memory growth, restarts
or failed nodes. Hidden from `ruro --help`. See [Soak tests](../runtime.md#soak-tests).

## ruro-host

`ruro-host` is started by the daemon; you rarely run it yourself.

```text
ruro-host describe
ruro-host run --graph <FILE> --group <GROUP> --daemon <NAME> [--connect <ENDPOINT>]
              [--estopped] [--record <MCAP>] [--mute <NODES>] [--no-compress]
```

`describe` prints the descriptors of every linked node type as JSON. `run` executes one
group of a graph and reports to the named daemon; `--estopped` starts with the emergency
stop engaged, `--record` records the group's outputs, `--mute` lists nodes whose outputs are
being replayed, and `--no-compress` sends messages to other machines uncompressed.
`ruro-host-py` (the [Python host](../python.md)) takes the same commands.
