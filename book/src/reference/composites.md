# Composite nodes

> **Status:** the format, flattening and validation are implemented (`ruro_graph`), and the
> studio edits, validates and deploys composite graphs. Deploying them from the `ruro` CLI is
> in development: the CLI must flatten graphs (resolving `graph:` paths relative to the file)
> before sending them to the daemon. This note will be removed when it does.

A composite node is a subgraph used as a single node. It lets you package a pipeline (say,
"detect obstacles" made of five nodes) behind a few named ports and reuse it in several
graphs.

## Referencing a subgraph file

Set the node's type to `graph:` followed by the path of a graph file. The path is relative to
the file that contains the composite node.

```yaml
# robot.yaml
name: robot
nodes:
  camera: { type: std/fake-camera, group: camera, rate: 30 }
  vision:
    type: graph:vision.yaml
    group: vision
    params: { blur.radius: 6 }
wires:
  - camera.image -> vision.image
```

```yaml
# vision.yaml
name: vision
inputs:
  image: blur.image
outputs:
  brightness: stats.brightness
nodes:
  blur: { type: std/blur }
  stats: { type: std/image-stats }
wires:
  - blur.out -> stats.image
```

## Inline subgraphs

Alternatively, use `type: composite` and put the subgraph in the node's `graph` field:

```yaml
nodes:
  vision:
    type: composite
    params: { blur.radius: 6 }
    graph:
      name: vision
      inputs: { image: blur.image }
      outputs: { brightness: stats.brightness }
      nodes:
        blur: { type: std/blur }
        stats: { type: std/image-stats }
      wires: [blur.out -> stats.image]
wires:
  - camera.image -> vision.image
```

## Exposed ports

A subgraph declares the ports the composite node exposes:

| Field | Value | Meaning |
|---|---|---|
| `inputs` | map of name to `node.port`, or to a list of them | Each exposed input feeds one or more inner input ports. |
| `outputs` | map of name to `node.port` | Each exposed output carries one inner output port. |

An input that feeds several inner ports is written as a list:

```yaml
inputs:
  image: [blur.image, detector.image]
```

Outside, the composite's ports are wired like any other node's (`vision.image`,
`vision.brightness`) and carry the message types of the inner ports they map to.

## Parameters

Parameters set on the composite node are keyed `<inner node>.<parameter>` and override that
inner node's parameter. In the examples above, `blur.radius: 6` sets the `radius` of the inner
`blur` node, whatever the subgraph file says.

## Groups and restart policy

Inner nodes that do not set their own `group` or `restart` inherit the composite node's. A
subgraph can still split itself across several processes by giving its nodes explicit groups.

## Flattening

Before a graph is validated or deployed, composite nodes are expanded ("flattened") into plain
nodes and wires:

- inner node ids are prefixed with the composite's id and two underscores: `vision__blur`,
  `vision__stats`;
- wires to and from the composite's ports are rewired to the inner ports;
- composites may be nested, up to 16 levels; a subgraph that includes itself is rejected.

Logs, metrics and `ruro echo` / `ruro param` on the running graph use the flattened ids, for
example:

```sh
ruro echo vision__stats.brightness
ruro param vision__blur radius 8
```

`ruro check` validates composite nodes: the subgraph must load, every exposed port must refer
to an existing inner node, and the flattened ids must not collide with other nodes.
