# Nodes in other languages

Any program can be a ruro node: a MATLAB or Octave script, a Julia or Go program, a
compiled tool, or a shell script. (For C and C++ code that should run inside the host's
process instead, without a process boundary, see [Nodes in C and C++](native-nodes.md).) A short YAML **manifest** declares the node type,
and the host runs the program for every node of that type in a graph, exchanging one JSON
line per step with it. In graphs, `ruro check`, `ruro nodes` and the studio, these node
types behave like any other: typed ports, parameters with ranges, live tuning, metrics,
the emergency stop, recording and replay.

> **Status:** implemented and tested in CI: a plain Python program speaking the protocol
> (inputs, outputs, parameters, images, the e-stop, both transports, a crash and restart,
> a hang, garbled output), a graph running such a node through a real daemon, and the
> MATLAB helper with the example nodes under GNU Octave 8.4, over standard input and over
> TCP. **MATLAB itself has not been tried**: the helper is written for both, and MATLAB
> uses the TCP transport, which Octave exercises.

## A node in MATLAB or Octave

The function gets the messages queued on each input since the last step and returns the
messages to send:

```matlab
% examples/process/gain.m
function [out, state] = gain(in, params, ctx, state)
out = struct();
if ~isempty(in.input)
    out.output = num2cell([in.input.msg] * params.k);
end
end
```

The manifest names the node type and how to start it:

```yaml
# examples/process/gain-octave.node.yaml
kind: example/octave-gain
doc: Multiplies its input by k, in Octave.
command: [octave-cli, --quiet, --norc, --eval, "addpath('../../sdk/matlab'); ruro_node(@gain)"]
inputs: { input: std/Scalar }
outputs: { output: std/Scalar }
params:
  k: { default: 2.0, min: -100, max: 100, doc: the gain }
start_timeout: 60
```

For MATLAB, start it with `matlab -batch` and use the TCP transport
(`examples/process/gain-matlab.node.yaml`): MATLAB cannot read its standard input when
another program starts it.

```yaml
command: [matlab, -batch, "addpath('../../sdk/matlab'); ruro_node(@gain)"]
transport: tcp
start_timeout: 180
```

Tell the host where the manifests are, then use the type in a graph:

```sh
RURO_PROCESS_NODES=examples/process ruro daemon
```

```yaml
name: matlab-demo
nodes:
  wave: { type: std/sine, rate: 50 }
  gain: { type: example/octave-gain, group: matlab, params: { k: 3.0 } }
wires:
  - { from: wave.value, to: gain.input, depth: 16 }
```

`sdk/matlab/ruro_node.m` handles the protocol. Besides the step function it takes
`'start'`, `'estop'` and `'stop'` handlers (`help ruro_node`). In the step function:

- `in.<input>` is a struct array of the queued messages (fields `seq`, `stamp_ns`, `msg`),
  or `[]` when none arrived;
- `params` holds the node's parameters, updated when they change while the graph runs;
- `ctx` has `steps`, `dt` and `time` (seconds), `now_ns` and `estopped`;
- `out.<output>` is a cell array of messages (a single message may be given as is);
- `state` carries whatever the function returned last time;
- an error it raises is reported as the step's error, and the node keeps running.

Images arrive as structs with base64 `data`; `ruro_bytes` converts them
(`examples/process/invert.m`):

```matlab
px = ruro_bytes(img);                       % height x width x channels uint8
img.data = ruro_bytes(255 - px, 'pixels');  % and back
```

The packages install the helper in `/usr/share/ruro/sdk/matlab`, next to the examples.

## Manifests

| Field | | Meaning |
|---|---|---|
| `kind` | required | Node type name, with a namespace: `acme/filter`. |
| `command` | required | The program and its arguments. A program given as a relative path with a `/` is relative to the manifest; a bare name is looked up on `PATH`. It runs in the manifest's directory, inheriting the host's environment. |
| `doc` | | One-line description, shown in the studio. |
| `inputs`, `outputs` | | Port names and message types: `{ image: std/Image }`. Any standard type (`ruro schema` lists them). |
| `params` | | Name to `{ default, min, max, doc, kind }`. `kind` (`int`, `float`, `bool`, `string`) is taken from the default if omitted: `2` is an integer, `2.0` a number. |
| `rate` | | Default trigger rate in Hz. Without one, the node steps when an input receives data. |
| `transport` | `stdio` | `stdio`, or `tcp` for programs that cannot read their standard input. |
| `env` | | Extra environment variables for the program. |
| `start_timeout` | 30 | Seconds the program gets to start and answer `start`. |
| `step_timeout` | 5 | Seconds it gets to answer any later request. |

`RURO_PROCESS_NODES` lists manifest files, or directories whose `*.node.yaml` files are
manifests, separated by `:`. Hosts read it when they start (the daemon's hosts inherit the
daemon's environment), so restart the daemon after changing it. A manifest with a mistake
is reported on the host's standard error and skipped.

Each node instance runs its own copy of the program. Give process nodes a
[group](concepts.md) of their own: a step waits for the program's answer, and a slow
program then delays only its own group.

## The protocol

Programs in languages without a helper speak it directly; `nodes/ruro-nodes-process/tests/fixtures/node.py`
is a complete implementation in about 110 lines of plain Python.

Every protocol line is `@ruro ` followed by one JSON object, in both directions. Other
lines the program prints are logged at the level their first word names (`error`,
`warning`, `debug`, else info), as is everything on its standard error; lines from the
host without the prefix (it sends an empty line after each request) are to be ignored.

With `transport: stdio`, the lines go over the program's standard input and output. With
`transport: tcp`, the program connects to `RURO_NODE_ADDR` (`127.0.0.1:<port>`) and the lines
go over that connection; standard output is then only logged.

The program starts by sending its hello: `{"ruro": 1}` (over TCP, with
`"token": <RURO_NODE_TOKEN>`). Then the host sends requests, one at a time, and the program
answers each with one line:

| Request | Answer |
|---|---|
| `{"op": "start", "node": id, "kind": kind, "params": {...}, "estopped": bool}` | `{}` |
| `{"op": "step", "steps": n, "dt": s, "time": s, "now_ns": n, "estopped": bool, "inputs": {"<input>": [{"seq": n, "stamp_ns": n, "msg": ...}, ...]}}` | `{"outputs": {"<output>": [msg, ...]}}` |
| `{"op": "param", "name": name, "value": v}` | `{}` |
| `{"op": "estop"}` (when the emergency stop is engaged) | `{}` |
| `{"op": "stop"}` | `{}`, then exit |

Any answer may carry `"error": "<message>"`: for `start` the node fails to start, for
`step` the error is counted and logged (outputs sent with it still go out), for `param` the
change is refused. The program also gets `RURO_NODE_ID` (the node's id in the graph) and
`RURO_NODE_PROTOCOL` in its environment.

The host keeps the program in check:

- an answer that does not come within the timeout, or a program that exits, fails that
  step; the host kills the program and starts it again on a later step (after 0.5 s, then
  doubling up to 10 s), sending `start` with the current parameters;
- the program runs in a process group of its own, which is terminated (`SIGTERM`, then
  `SIGKILL` after a second) when the node stops or the program is restarted; on Linux it
  also gets `SIGTERM` if its host dies.

## Messages as JSON

Messages are their serde form, the field names `ruro schema <type>` prints: a `std/Scalar`
is a number, `std/Text` a string, `std/Flag` a boolean, `std/Pose`
`{"position": {"x", "y", "z"}, "orientation": {"x", "y", "z", "w"}}`, `std/JointState`
`{"position": [...], "velocity": [...], "effort": [...]}`. Bytes travel as base64:

| Type | JSON |
|---|---|
| `std/Bytes` | a base64 string |
| `std/Image` | `{"width", "height", "encoding", "data"}`: `encoding` is `rgb8`, `rgba8`, `mono8` or `mono16`; `data` is the rows, top first, in base64 |
| `std/PointCloud` | `{"points"}`: the points' x, y, z as little-endian 32-bit floats, in base64 (a list of `[x, y, z]` is accepted from programs) |

## Costs and limits

One step costs a round trip to the program: about 30 µs with a scalar, and about 5.5 ms
with a 640x480 RGB image in each direction (base64 and JSON), measured with the Python test
program on a development machine (`cargo test -p ruro-nodes-process --release -- --ignored
--nocapture`). That suits controllers, planners and image processing at camera rates; for
the per-message cost of in-process nodes, write the node in Rust, Python, or
[C or C++](native-nodes.md).

- Only the standard message types can be used, not types defined in node packages (the
  same holds for [C and C++ nodes](native-nodes.md)).
- Programs send outputs in answer to a step. A program that produces data at its own pace,
  such as a driver, is polled by giving the node a `rate`.
- The host is a Linux (or macOS) program, so the programs run there, not on a Windows or
  macOS client.
