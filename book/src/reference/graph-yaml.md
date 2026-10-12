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
| `name` | string | yes | Graph name. Must match `[A-Za-z0-9_-]+`. |
| `nodes` | map of id to [node](#nodes) | no | Node instances, keyed by id. Order is preserved. |
| `wires` | list of [wires](#wires) | no | Connections between ports. |
| `groups` | map of name to [group settings](#groups) | no | Per-process-group settings. |
| `inputs`, `outputs` | map | no | Ports exposed when this graph is used as a [composite node](composites.md). |

## Nodes

Each key under `nodes` is a node id (must match `[A-Za-z0-9_-]+`). The value:

| Field | Type | Default | Meaning |
|---|---|---|---|
| `type` | string | required | Node type, e.g. `std/blur`. `composite` and `graph:<file>` are [composite nodes](composites.md). |
| `group` | string | `main` | Process group the node runs in. Must match `[A-Za-z0-9_-]+`. |
| `rate` | number (Hz) | node type's default | Run on a timer at this rate. Range (0, 100000]. Without a rate, the node runs whenever an input receives data. |
| `params` | map | type defaults | Parameter values. Unknown names, wrong types and out-of-range values are rejected. |
| `restart` | `never`, `on-failure`, `always` | `on-failure` | What the daemon does when the node's process exits. |
| `pos` | `[x, y]` | | Position on the studio canvas. Ignored when running. |
| `graph` | graph | | Inline subgraph of a `type: composite` node. |

A node without inputs (a source) needs a `rate`, either in the graph or as the node type's
default.

### Restart policy

A process group is restarted according to the policies of the nodes in it: if any node says
`never`, the group is never restarted; otherwise, if any node says `always`, it is restarted
whenever it exits; otherwise (`on-failure`, the default) it is restarted only when it exits
abnormally.

Restarts back off exponentially from 200 ms up to 5 s; a process that stays up for 10 s has its
backoff reset.

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
| `depth` | 1 | Messages the input queues from this wire. With 1, only the newest is kept. |

Both ports must carry the same message type. Duplicate wires are rejected. A wire between
nodes in different groups goes through shared memory; within a group, messages are passed by
reference.

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
```

> **Status:** in development. These settings are parsed and preserved, and are being wired
> through the daemon and host. See [Real-time scheduling](../upcoming/realtime.md) and
> [Multi-machine graphs](../upcoming/multi-machine.md).

| Field | Type | Meaning |
|---|---|---|
| `machine` | string | Name of the daemon (machine) that runs the group. Default: the deploying daemon. |
| `host` | string | Host program that executes the group, e.g. one with extra node libraries linked in. Default: the daemon's host. |
| `realtime.priority` | 1-99 | `SCHED_FIFO` priority of the group's threads. Default: normal scheduling. |
| `realtime.cpus` | list of integers | CPUs the group's threads are pinned to. |
| `realtime.deterministic` | bool | Run all nodes of the group on one thread in a fixed order each cycle, instead of one thread per node. Default `false`. |

## Validation

`ruro check`, the studio and the daemon (before deploying) validate a graph against the
available node types. Checks include: valid identifiers; known node types; parameter names,
types and ranges; rates in range; sources have a rate; wire endpoints exist; port types match;
no duplicate wires; and, for composites, that subgraphs load and flatten.
