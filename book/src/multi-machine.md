# Multi-machine graphs

Many robots have several computers: a real-time controller, a perception computer, an
operator station. One graph can span them.

> **Status:** implemented and tested with two daemons on one machine (deploying a graph
> whose groups run on different daemons, uploading host binaries). It has not been
> exercised across a real network in CI.

## Connecting daemons

Start a daemon on each machine and connect them:

```sh
robot$  ruro daemon --name robot --listen tcp/0.0.0.0:7447
laptop$ ruro daemon --name laptop --peer tcp/robot:7447
```

`--peer` is repeatable. A daemon's name defaults to its hostname. On anything but a
trusted network, protect the daemons with a token and TLS (see [Security](security.md));
peers then need the same token.

`ruro fleet` lists every daemon reachable through the one you are connected to; the
studio's Fleet view shows the same.

## Placing groups

Name a machine per group; groups without one run on the daemon the graph is deployed to:

```yaml
name: rover
groups:
  drivers: { machine: robot }
nodes:
  cam: { type: std/fake-camera, group: drivers, rate: 30 }
  stats: { type: std/image-stats, group: vision }
wires:
  - cam.image -> stats.image
```

The deploying daemon validates the whole graph (asking peers for their node types), sends
each remote group to its daemon and starts its own. Wires between machines go over Zenoh,
lz4-compressed for messages of 4 KiB and up when that makes them smaller; wires between
groups on one machine stay in shared memory. `ruro stop` stops every machine's part.

`ruro check` asks the daemon to validate the graph, so node types that exist only on a peer
or in an uploaded host are known. Offline, unknown types in groups with `machine:` or
`host:` set are reported as warnings rather than errors.

Recording and replay span machines: each peer records its own groups, and stopping the
recording collects every machine's part into one file at the daemon you asked. Replay mutes
recorded nodes on the machines that run them. The reply to `ruro record --stop` names the
groups recorded on each machine and any machine that could not record. Peers' clocks are
measured when the recording starts and their parts are shifted onto the daemon's clock when
the offset is measurable; see
[Recordings across machines](record-replay.md#recordings-across-machines). A very large peer
part can take longer than the CLI's 10 s request timeout to collect.

`ruro kill`, `ruro echo` and `ruro param` reach the machine that runs the group or node.

The emergency stop spans machines too: engaged at the daemon you deployed to, it is passed
on to every machine of the graph (see [Emergency stop](runtime.md#emergency-stop)). A stop
engaged directly on a robot stays there, and `ruro status` lists every machine of the graph
whose stop is engaged. The
studio shows the live metrics of nodes on every machine of the graph, and watches and tunes
them on the daemon that runs them.

## Custom host binaries

```sh
ruro deploy --to robot --host-binary target/release/my-host
```

uploads a host binary (checked against its SHA-256) that groups select with
`groups.<name>.host: my-host`. `--host-name` stores it under another name (a valid
identifier); a graph can be given in the same command. Uploads live in the daemon's
runtime directory and are lost on reboot. To update the daemon itself, and its default
host, see [Updating daemons](updates.md).

An uploaded [Python host](python.md) needs the `ruro` Python package installed on the
target machine.

How a group's `host` is resolved: a value with a directory part that exists on the
daemon's machine is used as a path (relative to the daemon's working directory); a plain
name must be an uploaded binary. Names are not looked up on `PATH`.
