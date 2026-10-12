# Security policy

## Supported versions

ruro is in early development and has not had a stable release yet. Security fixes are made on
the `main` branch and included in the next release.

| Version | Supported |
|---|---|
| `main` | yes |
| 0.1.x | yes, until 0.2.0 is released |
| older | no |

## Reporting a vulnerability

Please do not report security vulnerabilities through public GitHub issues, discussions or
pull requests.

Report them privately through GitHub's private vulnerability reporting:

**<https://github.com/mhussainahmad/ruro/security/advisories/new>**

Please include as much of the following as you can:

- the affected component (`ruro` CLI or daemon, `ruro-host`, `ruro-studio`, a node library, a
  package or container image) and version or commit;
- the kind of issue (for example remote code execution, denial of service, privilege
  escalation, information disclosure);
- steps to reproduce, a proof of concept, or a graph file that triggers the problem;
- the impact as you understand it.

You should receive an acknowledgement within a few days. We will keep you informed while we
investigate and fix the issue, and credit you in the advisory unless you prefer otherwise.

## Deployment notes

A client that can reach a ruro daemon (Zenoh, port 7447 by default) can deploy graphs, which
run code from the node types linked into the daemon's hosts, change parameters of running
nodes and upload host binaries.

- By default the daemon only listens on `127.0.0.1`, without encryption or authentication.
- To accept remote clients, listen on `tls/...` with a certificate and key, and set a shared
  token (`--token` or `RURO_TOKEN`); add `--tls-ca` to require client certificates. See the
  [Security](book/src/security.md) page of the book.
- A plain `tcp/` listener on a network interface accepts anyone who can reach it. Use one
  only on trusted networks, such as the robot's own network or a VPN, and restrict access
  with a firewall.
- The studio cannot use TLS or tokens yet, so daemons that require them can only be used
  from the CLI. For studio use, keep the daemon on `127.0.0.1` without a token and reach it
  through an SSH tunnel.
- The ROS 2 bridge nodes connect to their Zenoh router without TLS or authentication.
