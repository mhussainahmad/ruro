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
- The studio connects with TLS, a client certificate (for daemons that require one) and a
  token through **File > Connect to Daemon…**, or with the same options as the CLI
  (`--tls-ca`, `--tls-cert`, `--tls-key`, `--token-file`). It remembers the endpoint and the
  certificate paths, never the token.
- Plugins (`RURO_PLUGINS`) run inside the host process, and the programs of process nodes
  (`RURO_PROCESS_NODES`) run as the host's user, with its environment: both are code the
  daemon's operator chose to trust. Keep those variables, and the files they name, writable
  only by that operator.
- The ROS 2 bridge nodes connect to their Zenoh router without TLS or authentication.

## Supply chain

- `cargo deny check` (configured in [`deny.toml`](deny.toml)) runs on every push and every
  week: it fails on crates with known vulnerabilities in the
  [RustSec advisory database](https://rustsec.org), on yanked crates, on licenses other than
  the permissive ones listed there (no GPL, LGPL or AGPL code is linked), and on crates from
  other sources than crates.io. Advisories that do not apply are listed in `deny.toml` with
  the reason.
- Releases attach a [CycloneDX](https://cyclonedx.org) software bill of materials for each
  binary (`ruro-<version>.cdx.json`, `ruro-host-…`, `ruro-studio-…`), listing every crate in it
  with its version and license, and SHA-256 sums of every file (`SHA256SUMS`).
- Dependencies are pinned by `Cargo.lock`, and every build in CI and every release uses it
  (`--locked`).
