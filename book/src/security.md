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
- Peer daemons connect with the same options: start them with the same token. A daemon
  that connects to a TLS peer needs `--tls-ca` to trust that peer's certificate, but a daemon
  that has a certificate of its own *and* `--tls-ca` requires client certificates from
  everyone (previous point). So between two daemons, give `--tls-cert`/`--tls-key` to the
  one that listens on TLS and `--tls-ca` to the one that connects to it:

  ```sh
  # robot: listens on TLS
  ruro daemon --listen tls/0.0.0.0:7447 --tls-cert robot.pem --tls-key robot-key.pem \
      --token-file token
  # laptop: connects to it
  ruro daemon --peer tls/robot:7447 --tls-ca ca.pem --token-file token
  ```

  To use mutual TLS between daemons, give each a certificate and the CA, and give every
  client a certificate too.
- Hosts always talk to their daemon over a Unix socket in the daemon's runtime directory.

`--token` is visible to other users in `ps`; prefer `--token-file <path>` (or
`RURO_TOKEN_FILE`), whose first line is the token. It overrides `--token`.

Every option has an environment variable (`RURO_TLS_CERT`, `RURO_TLS_KEY`, `RURO_TLS_CA`,
`RURO_TOKEN`, `RURO_TOKEN_FILE`), which is how the system service takes them (see
[Deploying on robots](deployment.md#system-service)). Programs built on `ruro-proto`
connect with `ruro_proto::connect_with` and a `Security` holding the certificates and
token.

The studio takes the same options (`--tls-ca`, `--tls-cert`, `--tls-key`, `--token`,
`--token-file` and their environment variables):

```sh
ruro-studio --connect tls/robot:7447 --tls-ca ca.pem --token-file token
```

It only starts a local daemon when nothing listens at its endpoint, never over a daemon
that refused it.

`ruro update` replaces a daemon's own binary (see [Updating daemons](updates.md)). A daemon
accepts an update only if it was started with `--update-keys <file>` (or
`RURO_UPDATE_KEYS`) and the binary carries an Ed25519 signature by one of the public keys in
that file. The token and TLS decide who may send an update; the signature decides what may
be installed, so a leaked token alone cannot install a binary you did not sign. It can
install an older release you did sign, because `--allow-downgrade` comes from the client:
to retire old releases, sign new ones with a new key and remove the old key from the
daemons. Keep the secret signing key (`ruro keygen`) off the robots, and the keys file
writable only by root.

A plain `tcp/` listener on a network interface accepts anyone who can reach the port. If
you must use one, do so only on a network you trust (the robot's own network, a VPN), and
restrict it with a firewall.

The ROS 2 bridge nodes connect to the Zenoh router named by their `router` parameter. They
take a token from their `token` parameter or from `RURO_TOKEN` / `RURO_TOKEN_FILE`, and TLS
settings from `RURO_TLS_CA`, `RURO_TLS_CERT` and `RURO_TLS_KEY`, so they can use a
protected daemon as their router. A token taken from the environment is retried without it
when the router does not authenticate, as `rmw_zenohd` does by default.

To report a vulnerability, see
[SECURITY.md](https://github.com/mhussainahmad/ruro/blob/main/SECURITY.md).
