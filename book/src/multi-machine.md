# Multi-machine graphs

> **Status:** in development. This page describes the planned interface and will be verified
> before release.

Today one graph runs on one machine: the daemon you deploy to runs every process group. Many
robots have several computers (a real-time controller, a perception computer, an operator
station), and multi-machine graphs let one graph span them.

## Planned design

Each process group can name the machine that runs it, in the graph's `groups` section:

```yaml
groups:
  control:
    machine: arm-controller     # daemon name
  vision:
    machine: jetson
nodes:
  camera: { type: my_robot/camera, group: vision, rate: 30 }
  detect: { type: my_robot/detect, group: vision }
  ctrl:   { type: my_robot/ctrl,   group: control, rate: 500 }
wires:
  - detect.target -> ctrl.target
```

- `machine` is the name of a daemon (its `--name`, by default the hostname). Groups without one
  run on the daemon the graph is deployed to.
- The daemons of the fleet connect to each other through Zenoh; deploying to any of them
  starts each group on its machine.
- Wires within a machine keep using in-process sharing or shared memory. Wires between
  machines are carried over the network.
- `ruro status` and the studio show the whole graph, with each group's machine.

The `machine` field is already part of the graph format (see the
[graph file reference](../reference/graph-yaml.md#groups)).

In the studio, the inspector sets a process's machine from the daemons it has seen, and the
Fleet view lists every daemon on the network and switches between them (see
[Studio](../studio.md#fleet)).
