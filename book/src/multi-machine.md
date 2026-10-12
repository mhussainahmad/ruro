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

`ruro check` only knows the node types of the daemon it asks (or the local host), so a
graph that uses node types available only on a peer fails `check` but deploys.

What does not span machines yet:

- `ruro estop` reaches the groups of the daemon it is sent to; send it to every daemon.
- Recording and replay cover the groups of the daemon you talk to.

## Custom host binaries

```sh
ruro deploy --to robot --host-binary target/release/my-host
```

uploads a host binary (checked against its SHA-256) that groups select with
`groups.<name>.host: my-host`. `--host-name` stores it under another name (a valid
identifier); a graph can be given in the same command. Uploads live in the daemon's
runtime directory and are lost on reboot. Updating the daemon itself remotely is not
supported.

An uploaded [Python host](python.md) needs the `ruro` Python package installed on the
target machine.

How a group's `host` is resolved: a value with a directory part that exists on the
daemon's machine is used as a path (relative to the daemon's working directory); a plain
name must be an uploaded binary. Names are not looked up on `PATH`.
