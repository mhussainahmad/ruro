# Installation

ruro runs on Linux. It is developed on x86-64; 64-bit ARM binaries of the headless tools are
cross-compiled in CI (see [Cross-compiling for ARM](deployment.md#cross-compiling-for-arm)).
There are no prebuilt releases yet: build from source, or build one of the packages below
from a checkout.

| Method | Contents | Best for |
|---|---|---|
| [From source](#from-source) | everything | development, custom hosts, Python nodes |
| [Debian package](#debian-package) | `ruro`, `ruro-host`, `ruro-studio`, systemd units | Debian/Ubuntu desktops and robots |
| [AppImage](#appimage) | the studio plus `ruro` and `ruro-host` | trying the studio on any distribution |
| [Flatpak](#flatpak) | the studio plus `ruro` and `ruro-host` | sandboxed desktop installs |
| [Docker](#docker) | `ruro` and `ruro-host` (no studio) | headless robots and servers |
| [cargo install](#cargo-install) | individual binaries | Rust users |

The binaries are:

- `ruro`: the command-line tool, which also runs the daemon (`ruro daemon`);
- `ruro-host`: the process that executes one process group, with the standard node libraries
  linked in;
- `ruro-studio`: the desktop editor;
- `ruro-host-py`: a host with an embedded Python interpreter for [Python nodes](python.md)
  (source builds only).

The daemon uses the host given with `--host`, else the one named by `RURO_HOST`, else
`ruro-host` next to its own executable, else the one on `PATH`. The studio looks for `ruro`
and `ruro-host` next to itself, then on `PATH`. Keep the binaries together.

## From source

You need Rust 1.95 or newer (install it with [rustup](https://rustup.rs)) and a C compiler
(`build-essential` on Debian and Ubuntu). No other development libraries are needed to
build: the studio loads its windowing and OpenGL libraries at run time.

```sh
git clone https://github.com/mhussainahmad/ruro
cd ruro
cargo build --release -p ruro-cli -p ruro-host -p ruro-studio
```

The binaries are in `target/release/`. To build only the headless tools (for example on a
robot without a display):

```sh
cargo build --release -p ruro-cli -p ruro-host
```

### Python

`ruro-py` and `ruro-host-py` embed Python, so building them, and therefore building or
testing the whole workspace (`cargo build` or `cargo test` without `-p`), needs Python 3.11
or newer with its shared library: on Debian and Ubuntu, `python3-dev`. The build uses the
`python3` on `PATH`, or the interpreter named by `PYO3_PYTHON`. The binaries above do not
need Python.

```sh
cargo build --release -p ruro-host-py
```

### Optional node features

Some node types are behind cargo features of the standard host, because they pull in large
or external dependencies:

| Feature | Node types | Needs |
|---|---|---|
| `ruro-nodes-sim/mujoco` | `sim/mujoco` | MuJoCo 3.15.0 at run time ([Simulation](simulation.md)) |
| `ruro-nodes-ml/onnx` | `ml/onnx`, `ml/policy` | downloads ONNX Runtime at build time ([Machine learning](ml.md)) |
| `ruro-nodes-ml/lerobot` | `ml/lerobot-recorder` | nothing extra |

```sh
cargo build --release -p ruro-host --features ruro-nodes-ml/onnx,ruro-nodes-ml/lerobot
```

### Running the studio

The studio needs a Wayland or X11 session and OpenGL (EGL or GLX). On a minimal Debian or
Ubuntu system, the run-time libraries are `libegl1 libgl1 libwayland-client0
libwayland-egl1 libxkbcommon0 libxkbcommon-x11-0 libx11-6 libx11-xcb1 libxcursor1 libxi6
libxrandr2` (the Debian package depends on them). Native file dialogs go through the XDG
desktop portal (`xdg-desktop-portal` and a backend for your desktop); without one, the
studio uses its built-in file picker.

## Debian package

Build a `.deb` from a checkout with:

```sh
scripts/package-deb.sh
sudo apt install ./target/package/ruro_*_amd64.deb
```

The package installs:

- `/usr/bin/ruro`, `/usr/bin/ruro-host`, `/usr/bin/ruro-studio`;
- a desktop entry and icons (`io.github.mhussainahmad.ruro.Studio`, the studio's app id), so
  ruro studio appears in your application menu;
- a system service `ruro-daemon.service` and a user service of the same name, both disabled
  (see [Deploying on robots](deployment.md));
- the example graphs in `/usr/share/ruro/examples/` (also reachable as
  `/usr/share/doc/ruro/examples/`), which the studio lists under **File → Open Example**.

It also creates a `ruro` system user for the system service. It does not include the Python
host, which would tie the package to one Python version, or the optional node features. The
package's minimum glibc is that of the system it was built on; CI builds it on Ubuntu 22.04.

## AppImage

```sh
scripts/package-appimage.sh --fetch-tool
./target/package/ruro-studio-*-x86_64.AppImage
```

`--fetch-tool` downloads `appimagetool` into a temporary directory if it is not installed. The
AppImage starts the studio; the bundled tools are available as
`./ruro-studio-*.AppImage ruro <command>`. The windowing libraries come from the system.

## Flatpak

`packaging/flatpak/io.github.mhussainahmad.ruro.Studio.yml` builds the studio with `ruro`
and `ruro-host`; its header explains how (the crates have to be vendored first, because
Flatpak builds run offline). Inside the sandbox, the studio can only run graphs whose nodes
are linked into the bundled host; to work with a robot or a custom host, run the daemon
outside the sandbox and connect to it.

## Docker

The image contains the headless tools (no studio):

```sh
docker build -t ruro .
docker run --rm --shm-size=512m -p 7447:7447 ruro
```

The container runs `ruro daemon --listen tcp/0.0.0.0:7447`. Deploy to it from the host with
`ruro --connect tcp/127.0.0.1:7447 run graph.yaml`, or connect the studio with
`ruro-studio --connect tcp/127.0.0.1:7447`.

Cross-process wires use shared memory in `/dev/shm`. Docker's default of 64 MB is too small for
camera pipelines: raise it with `--shm-size`, or use `--ipc=host`. See
[Deploying on robots](deployment.md#docker) for real-time settings.

## cargo install

> **Status:** the crates are not yet published on crates.io. Until they are, install from a
> checkout with `--path` as shown.

```sh
cargo install --locked --path crates/ruro-cli     # ruro
cargo install --locked --path crates/ruro-host    # ruro-host
cargo install --locked --path crates/ruro-studio  # ruro-studio
```

Always install `ruro-host` alongside `ruro`: the daemon cannot run graphs without it.

## Checking the installation

```sh
ruro --version
ruro nodes          # lists the node types linked into ruro-host
```
