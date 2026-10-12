# Security

A client that can reach a daemon can deploy graphs, which run code from the node types
linked into the daemon's hosts, change parameters of running nodes and upload host
binaries. Protect daemons that listen on a network.

By default a daemon listens on `tcp/127.0.0.1:7447`, without encryption or authentication:
only processes on the same machine can reach it. To accept remote clients, use TLS and a
token:

```sh
ruro daemon --listen tls/0.0.0.0:7447 \
    --tls-cert robot.pem --tls-key robot-key.pem --token "$(cat token)"

ruro --connect tls/robot:7447 --tls-ca ca.pem --token "$(cat token)" status
```

- The certificate's name must match the host name clients connect to (`tls/robot:7447`
  needs a certificate for `robot`). A certificate without its key, or the reverse, is an
  error.
- `--token` (or `RURO_TOKEN`) enables Zenoh username/password authentication with the user
  `ruro`: sessions without the same token are refused, on every endpoint, including peer
  daemons. The daemon keeps the token in a file readable only by its user (`auth` in its
  runtime directory) and hands it to its hosts in their environment.
- Giving the daemon `--tls-ca` as well makes it require client certificates signed by that
  CA (mutual TLS); clients then pass their own `--tls-cert` and `--tls-key`.
- Peer daemons connect with the same options: start them with the same token (and, with
  TLS peers, a CA that trusts the other daemon's certificate).
- Hosts always talk to their daemon over a Unix socket in the daemon's runtime directory.

Every option has an environment variable (`RURO_TLS_CERT`, `RURO_TLS_KEY`, `RURO_TLS_CA`,
`RURO_TOKEN`), which is how the system service takes them (see
[Deploying on robots](deployment.md#system-service)). Programs built on `ruro-proto`
connect with `ruro_proto::connect_with` and a `Security` holding the certificates and
token.

> **The studio does not support TLS or tokens yet.** It connects without either, so it
> cannot reach a daemon started with TLS or a token; such daemons can only be used from the
> CLI for now. To use the studio with a remote robot, run the robot's daemon on its default
> `tcp/127.0.0.1:7447` without a token and reach it through an SSH tunnel (or a VPN), for
> example `ssh -L 7447:127.0.0.1:7447 robot` and
> `ruro-studio --connect tcp/127.0.0.1:7447`.

A plain `tcp/` listener on a network interface accepts anyone who can reach the port. If
you must use one, do so only on a network you trust (the robot's own network, a VPN), and
restrict it with a firewall.

The ROS 2 bridge nodes connect to their own Zenoh router (the `router` parameter) without
TLS or a token; keep that router on a trusted network too.

To report a vulnerability, see
[SECURITY.md](https://github.com/mhussainahmad/ruro/blob/main/SECURITY.md).
