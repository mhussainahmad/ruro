# Graph files

A graph is a YAML file. The studio and the CLI read and write the same format.

```yaml
name: camera-demo
nodes:
  camera:
    type: std/fake-camera
    group: camera
    rate: 30
    params: { width: 640, height: 480 }
    pos: [0, 0]
  blur:
    type: std/blur
    group: vision
    params: { radius: 4 }
    pos: [400, 0]
  stats:
    type: std/image-stats
    group: vision
wires:
  - camera.image -> blur.image
  - from: blur.out
    to: stats.image
    depth: 4
```

## Top level

| Field | Type | Required | Meaning |
|---|---|---|---|
| `name` | string | yes | Graph name, an [identifier](#identifiers). |
| `nodes` | map of id to [node](#nodes) | no | Node instances, keyed by id. Order is preserved. |
| `wires` | list of [wires](#wires) | no | Connections between ports. |
| `groups` | map of name to [group settings](#groups) | no | Per-process-group settings. |
| `clock` | `wall` or `sim` | no | Graph time. Default `wall`; `sim` follows the graph's one `std/Clock` output (see [Graph time](../runtime.md#graph-time-and-simulation)). |
| `inputs`, `outputs` | map | no | Ports exposed when this graph is used as a [composite node](composites.md). |

### Identifiers

Graph names, node ids and group names must match `[A-Za-z0-9_-]+` and be at most 64
characters long. Unknown keys anywhere in the file are ignored rather than rejected, so
check the spelling of optional fields.

## Nodes

Each key under `nodes` is a node id (must match `[A-Za-z0-9_-]+`). The value:

| Field | Type | Default | Meaning |
|---|---|---|---|
| `type` | string | required | Node type, e.g. `std/blur`. `composite` and `graph:<file>` are [composite nodes](composites.md). |
| `group` | string | `main` | Process group the node runs in, an identifier. |
| `rate` | number (Hz) | node type's default | Run on a timer at this rate. Range (0, 100000]. Without a rate, the node runs whenever an input receives data. |
| `params` | map | type defaults | Parameter values. Unknown names, wrong types and out-of-range values are rejected by validation. |
| `restart` | `never`, `on-failure`, `always` | `on-failure` | What the daemon does when the node's process exits. |
| `pos` | `[x, y]` | | Position on the studio canvas. Ignored when running. |
| `graph` | graph | | Inline subgraph of a `type: composite` node. |

A node without inputs (a source) needs a `rate`, either in the graph or as the node type's
default. A node with inputs and a `rate` runs on its timer only. A `rate` on a composite node
is ignored.

### Restart policy

A process group is restarted according to the policies of the nodes in it: if any node says
`never`, the group is never restarted; otherwise, if any node says `always`, it is restarted
whenever it exits; otherwise (`on-failure`, the default) it is restarted only when it exits
abnormally.

Restarts back off exponentially. A process that ran for at least 10 s is restarted after
200 ms; one that exits sooner waits 400 ms, then 800 ms, 1.6 s, 3.2 s and at most 5 s. When
a group is stopped, its process gets `SIGTERM` and, 2 s later, `SIGKILL`. A process that
stops reporting is killed and counts as a failure (see
[Hung processes](../runtime.md#hung-processes)).

## Wires

A wire connects an output port to an input port. The short form is a string:

```yaml
wires:
  - camera.image -> blur.image
```

The long form sets the queue depth of the receiving input:

```yaml
wires:
  - from: camera.image
    to: blur.image
    depth: 4
```

| Field | Default | Meaning |
|---|---|---|
| `from` | required | Output port, `node.port`. |
| `to` | required | Input port, `node.port`. |
| `depth` | 1 | Messages the input queues. With 1, only the newest is kept. |

Both ports must carry the same message type. Duplicate wires are rejected. Within a group,
messages are passed by reference; between groups on one machine they go through shared
memory, and between machines over the network.

The queue belongs to the input port: if several wires feed one input, the depth of the
last one applies. Across processes, the shared-memory buffer that holds messages for a
slow reader is the depth capped at 16.

## Groups

Nodes are assigned to groups with their `group` field; every distinct group becomes one OS
process. The optional `groups` section configures how a group runs:

```yaml
groups:
  control:
    machine: robot-arm
    host: /opt/my-robot/bin/my-host
    realtime:
      priority: 80
      cpus: [2, 3]
      deterministic: true
      rate: 1000
      deadlines_us: { pid: 200 }
```

| Field | Type | Meaning |
|---|---|---|
| `machine` | string | Name of the daemon (machine) that runs the group. Default: the deploying daemon. See [Multi-machine graphs](../multi-machine.md). |
| `host` | string | Host program that executes the group: a path on the daemon's machine, or the name of an uploaded host binary. Default: the daemon's host. |
| `realtime.priority` | 1-99 | `SCHED_FIFO` priority of the group's threads; also locks memory. Default: normal scheduling. |
| `realtime.cpus` | list of integers | CPUs the group's threads are pinned to. |
| `realtime.deterministic` | bool | Run all nodes of the group on one thread in a fixed order each cycle, instead of one thread per node. Default `false`. |
| `realtime.rate` | number (Hz) | Cycle rate of a deterministic group. Default: the highest node rate in the group. |
| `realtime.deadlines_us` | map of node id to µs | Step deadlines; a longer step is reported as an error. |

See [Real-time scheduling](../realtime.md) for how these behave.

## Validation

`ruro check`, the studio and the daemon (before deploying) validate a graph against the
available node types. Checks include: valid identifiers; known node types; parameter names,
types and ranges; rates in range; sources have a rate; wire endpoints exist; port types match;
no duplicate wires; and, for composites, that subgraphs load and flatten.

Some checks only happen when the daemon deploys the graph:

- a `clock: sim` graph has exactly one `std/Clock` output;
- real-time settings: priority 1-99, cycle rate in (0, 100000], deadline nodes in the group;
- node types provided by a group's own `host` or by [peer daemons](../multi-machine.md).

`ruro check` asks the daemon it can reach for node types (otherwise the local host binary),
so graphs that rely on another host's or another machine's node types fail `check` but
deploy.
