# Quick start

This walk-through uses the example graphs in `examples/` and the standard nodes, so it needs
no hardware. It assumes you built from source (`cargo build --release`); with a package
installed, drop the `./target/release/` prefix.

## In the studio

```sh
./target/release/ruro-studio examples/camera.yaml
```

The studio starts a local daemon automatically. Then:

1. Press **Run** (or <kbd>Ctrl</kbd>+<kbd>Enter</kbd>). The graph is validated and deployed: the
   camera runs in one process, the vision nodes in another.
2. Click the eye next to an output port to watch it live: images, plots or values.
3. Select a node to see its parameters in the inspector, and change them while the graph runs.
4. Press <kbd>Ctrl</kbd>+<kbd>K</kbd> for the command palette.

The [Studio guide](studio.md) covers the editor in detail.

## From a terminal

`ruro run` deploys a graph and keeps running in the foreground. If no daemon is reachable it
starts one for the duration of the command.

```sh
./target/release/ruro run examples/camera.yaml   # deploys, streams logs, Ctrl-C stops
```

In a second terminal:

```sh
./target/release/ruro status                     # daemon, groups and live per-node metrics
./target/release/ruro echo stats.brightness      # print an output's values
./target/release/ruro param blur radius 12       # retune a running node
./target/release/ruro kill camera                # crash a process; watch it come back
```

`ruro kill camera` kills the process running the `camera` group. The daemon notices and
restarts it, and the rest of the graph keeps running.

## A closed loop

`examples/arm.yaml` connects a sine-wave target, a PID controller and a simulated joint:

```sh
./target/release/ruro run examples/arm.yaml
```

Watch `arm.position` and `target.value` in the studio, or tune the controller from a terminal:

```sh
./target/release/ruro param pid kp 8
./target/release/ruro param pid kd 0.2
```

## Writing a graph

Graphs are YAML files. A minimal one:

```yaml
name: my-first-graph
nodes:
  wave: { type: std/sine, rate: 50, params: { frequency: 1.0 } }
  amp:  { type: std/gain, params: { k: 3.0 } }
wires:
  - wave.value -> amp.input
```

Check it against the available node types, then run it:

```sh
ruro check my-first-graph.yaml
ruro run my-first-graph.yaml
```

`ruro nodes -v` lists every node type with its ports and parameters. The
[graph file reference](reference/graph-yaml.md) documents every field.
