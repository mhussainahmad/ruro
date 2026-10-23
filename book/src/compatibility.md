# Compatibility and support

What each release promises, on which systems ruro runs, and how versions work together.

## Platforms

| Part | Tested in CI on | Packages |
|---|---|---|
| Daemon, hosts, CLI | Linux x86-64 (Ubuntu 22.04 and 24.04); Linux 64-bit ARM cross-built and tested under emulation | tarballs, `.deb`, AppImage, Docker |
| Studio | Linux x86-64; Windows x86-64 and macOS on Apple silicon (GitHub's Windows Server and macOS 14 runners), as clients of a daemon on Linux | AppImage, `.deb`, zip, `.dmg` (macOS 11 or newer) |
| Python nodes | Linux x86-64, Python 3.11 and 3.12 | `ruro-python` `.deb` for Ubuntu 24.04 |
| C and C++ nodes | Linux x86-64 and 64-bit ARM (GCC) | headers in `.deb`, `sdk/include` |
| Nodes in other languages | Linux; MATLAB-style helper under GNU Octave 8.4 | `sdk/matlab` |
| ROS 2 bridge | ROS 2 Jazzy with rmw_zenoh (checked in a container, outside CI) | |
| GPU buffers, `ml/onnx-cuda` | one NVIDIA RTX 4090 machine, outside CI; `ml/onnx-cuda` needs glibc 2.38 or newer | |

The daemon also builds on macOS, but nothing tests it there. The release binaries for Linux
need glibc 2.34 or newer: they are built on Ubuntu 22.04.

## Versions

ruro follows [semantic versioning](https://semver.org). Before 1.0:

- a release that increases the minor version (0.1 to 0.2) may change the graph format, the
  CLI, the node APIs (Rust, Python, C and C++, the process protocol) and the protocol between
  daemons, hosts and clients; [CHANGELOG.md](https://github.com/mhussainahmad/ruro/blob/main/CHANGELOG.md)
  lists every such change and how to adapt;
- a patch release (0.1.0 to 0.1.1) fixes bugs and security issues only, and keeps all of the
  above compatible.

From 1.0, breaking changes wait for the next major version, and what is deprecated keeps
working, with a warning, for at least one minor version.

The versioned interfaces carry their own version numbers, which change only with a
breaking change: the C ABI (`RURO_ABI_VERSION`, checked when a plugin loads), the process
protocol (`"ruro": 1` in a program's hello), and the message schemas (`ruro schema diff`
reports whether a change of a message type is wire compatible).

## Mixing versions

- **Daemons, hosts and clients** of the same minor version work together. Use the hosts
  that came with a daemon (an update replaces both).
- **During an update** of a fleet, daemons of two versions can share a network: status
  messages of older daemons are still read, and `ruro status` and `ruro fleet` still list
  0.1.0 daemons. Update every machine of a graph together (`ruro update --peers`); see
  [Updating daemons](updates.md).
- **Recordings** (MCAP) store each channel's message schema with the data, so tools can
  read them without ruro; a release that cannot read older recordings says so in the
  changelog.
- **Graph files** are read by later patch releases unchanged; a minor release that changes
  them says so in the changelog.

## Rust version

The workspace needs Rust 1.95 or newer (`rust-version` in `Cargo.toml`; CI checks it). The
minimum moves forward only in minor releases, to a version at least six months old.

## Security fixes

See [SECURITY.md](https://github.com/mhussainahmad/ruro/blob/main/SECURITY.md): reports are
made privately through GitHub, fixes land on `main` and in a patch release of the latest
minor version, and every push and every week are checked against the RustSec advisory
database.
