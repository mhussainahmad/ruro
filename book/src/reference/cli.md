# Command line

```text
ruro [OPTIONS] <COMMAND>
```

`ruro` is the command-line client for the daemon, and also runs the daemon itself.

## Global options

These apply to every command.

| Option | Environment | Default | Meaning |
|---|---|---|---|
| `--connect <ENDPOINT>` | `RURO_CONNECT` | `tcp/127.0.0.1:7447` | Daemon endpoint. |
| `--daemon <NAME>` | `RURO_DAEMON` | discovered | Daemon name, if several are reachable through one router. |
| `-h`, `--help` | | | Print help. |
| `-V`, `--version` | | | Print the version. |

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
| [`deploy`](#ruro-deploy) | Deploy a graph to the daemon and return. |
| [`stop`](#ruro-stop) | Stop the running graph. |
| [`check`](#ruro-check) | Validate a graph against the available node types. |
| [`status`](#ruro-status) | Show the daemon, its groups and live node metrics. |
| [`nodes`](#ruro-nodes) | List available node types. |
| [`echo`](#ruro-echo) | Print live values of an output port. |
| [`param`](#ruro-param) | Change a node parameter while the graph runs. |
| [`kill`](#ruro-kill) | Kill a group process to exercise supervision. |
| [`logs`](#ruro-logs) | Stream logs from the running graph. |
| [`studio`](#ruro-studio) | Open the desktop studio. |

### ruro run

```text
ruro run <GRAPH>
```

Deploys the graph file, prints the graph's logs and stays in the foreground until
<kbd>Ctrl</kbd>+<kbd>C</kbd>, then stops the graph. If no daemon is reachable at `--connect`,
`run` starts one in-process, listening on that endpoint, and shuts it down on exit.

### ruro daemon

```text
ruro daemon [--name <NAME>] [--listen <ENDPOINT>] [--host <HOST>]
```

| Option | Default | Meaning |
|---|---|---|
| `--name <NAME>` | hostname | Name other tools use to address this daemon. |
| `--listen <ENDPOINT>` | `tcp/127.0.0.1:7447` | Endpoint to listen on. Use `tcp/0.0.0.0:7447` to accept remote studios. |
| `--host <HOST>` | `RURO_HOST`, else `ruro-host` next to `ruro`, else `PATH` | Host binary used to run graph groups. |

Runs until <kbd>Ctrl</kbd>+<kbd>C</kbd> or `SIGTERM`, then stops the running graph. Deployed
graphs are written to `$XDG_RUNTIME_DIR/ruro-<name>/` (or the system temporary directory).

### ruro deploy

```text
ruro deploy <GRAPH>
```

Sends the graph to the daemon, which validates it and starts one process per group, replacing
any graph already running. Returns immediately.

### ruro stop

```text
ruro stop
```

Stops the running graph.

### ruro check

```text
ruro check <GRAPH>
```

Validates the graph: node ids, types, parameters and their ranges, rates, wires and message
types. The node types come from the daemon if one is reachable, otherwise from the local host
binary. Prints a summary and exits with status 0 if the graph is valid; otherwise prints each
problem and exits with status 1.

```text
$ ruro check examples/camera.yaml
examples/camera.yaml: ok (4 nodes, 3 wires, 2 process groups)
```

### ruro status

```text
ruro status
```

Prints the daemon, the deployed graph's process groups (state, pid, restarts) and live per-node
metrics.

### ruro nodes

```text
ruro nodes [-v]
```

Lists the node types the daemon's host (or the local host binary) provides. `-v`, `--verbose`
also shows ports and parameters.

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
graph file.

### ruro kill

```text
ruro kill <GROUP>
```

Kills the process running a group, to exercise supervision. The daemon restarts it according
to the group's restart policy.

### ruro logs

```text
ruro logs
```

Streams log lines from every process of the running graph until <kbd>Ctrl</kbd>+<kbd>C</kbd>.

### ruro studio

```text
ruro studio [ARGS]...
```

Starts `ruro-studio` (from next to `ruro`, else from `PATH`), passing the remaining arguments
through. See [Studio](../studio.md) for its options.

## ruro-host

`ruro-host` is started by the daemon; you rarely run it yourself.

```text
ruro-host describe
ruro-host run --graph <FILE> --group <GROUP> --daemon <NAME> [--connect <ENDPOINT>]
```

`describe` prints the descriptors of every linked node type as JSON. `run` executes one group
of a graph and reports to the named daemon.
