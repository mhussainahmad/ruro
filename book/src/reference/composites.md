# Composite nodes

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
name: robot
nodes:
  camera: { type: std/fake-camera, group: camera, rate: 30 }
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
A subgraph's `groups` settings are merged into the outer graph's (the outer graph wins when
both configure a group). A `rate` set on the composite node itself is ignored; set rates on
the inner nodes.

## Flattening

Before a graph is validated or deployed, composite nodes are expanded ("flattened") into plain
nodes and wires:

- inner node ids are prefixed with the composite's id and two underscores: `vision__blur`,
  `vision__stats`;
- wires to and from the composite's ports are rewired to the inner ports;
- composites may be nested, up to 16 levels; a subgraph that includes itself is rejected;
- `graph:` paths are resolved relative to the file that contains them (for an inline
  composite, the outer file).

The CLI flattens a graph before sending it (`ruro run`, `deploy`, `play --graph` and
`soak`), and so does the studio, so the daemon always receives a self-contained graph and
subgraph files only need to exist where you deploy from.

Flattening currently drops the outer graph's `clock` setting, so a `clock: sim` graph that
contains composite nodes runs on wall time. A subgraph's own `clock` is always ignored.

Logs, metrics and `ruro echo` / `ruro param` on the running graph use the flattened ids, for
example:

```sh
ruro echo vision__stats.brightness
ruro param vision__blur radius 8
```

`ruro check` validates composite nodes: the subgraph must load, every exposed port must refer
to an existing inner node, parameter overrides must name an existing inner node, and the
flattened ids must not collide with other nodes. The node and wire counts it prints are
those of the file before flattening.
