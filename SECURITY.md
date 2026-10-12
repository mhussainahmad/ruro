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

ruro's control protocol (Zenoh, port 7447 by default) is not authenticated or encrypted. A
client that can reach a daemon can deploy graphs, which run code from the node types linked
into the daemon's host, and change parameters of running nodes. By default the daemon only
listens on `127.0.0.1`. If you make it listen on a network interface (`--listen
tcp/0.0.0.0:7447`), do so only on trusted networks, such as the robot's own network or a VPN,
and restrict access with a firewall.
