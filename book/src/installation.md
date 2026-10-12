# Installation

ruro runs on Linux. It is developed on x86-64; 64-bit ARM binaries are cross-compiled in CI
(see [Cross-compiling for ARM](deployment.md#cross-compiling-for-arm)). There are several ways
to install it:

| Method | Contents | Best for |
|---|---|---|
| [From source](#from-source) | everything | development, custom hosts |
| [Debian package](#debian-package) | `ruro`, `ruro-host`, `ruro-studio`, systemd units | Debian/Ubuntu desktops and robots |
| [AppImage](#appimage) | the studio plus `ruro` and `ruro-host` | trying the studio on any distribution |
| [Docker](#docker) | `ruro` and `ruro-host` (no studio) | headless robots and servers |
| [cargo install](#cargo-install) | individual binaries | Rust users |

The three binaries are:

- `ruro`: the command-line tool, which also runs the daemon (`ruro daemon`);
- `ruro-host`: the process that executes one process group, with the standard nodes linked in;
- `ruro-studio`: the desktop editor.

The daemon uses the host named by `RURO_HOST` if set, otherwise `ruro-host` next to its own
executable, otherwise the one on `PATH`. The studio looks for `ruro` and `ruro-host` next to
itself, then on `PATH`. Keep the binaries together.

## From source

You need a recent stable Rust toolchain (install it with [rustup](https://rustup.rs)). The
command-line tools need Rust 1.89 or newer; the studio's GUI dependencies currently need 1.95
or newer, so simply use the latest stable release.

The studio needs the usual windowing development libraries. On Debian and Ubuntu:

```sh
sudo apt-get install libxkbcommon-dev libwayland-dev libgl1-mesa-dev
```

Then:

```sh
git clone https://github.com/mhussainahmad/ruro
cd ruro
cargo build --release
```

The binaries are in `target/release/`. To build only the headless tools (for example on a
robot without a display):

```sh
cargo build --release -p ruro-cli -p ruro-host
```

## Debian package

Build a `.deb` from a checkout with:

```sh
scripts/package-deb.sh
sudo apt install ./target/package/ruro_*_amd64.deb
```

The package installs:

- `/usr/bin/ruro`, `/usr/bin/ruro-host`, `/usr/bin/ruro-studio`;
- a desktop entry and icons, so ruro studio appears in your application menu;
- a system service `ruro-daemon.service` and a user service of the same name, both disabled
  (see [Deploying on robots](deployment.md));
- the example graphs in `/usr/share/doc/ruro/examples/`.

It also creates a `ruro` system user for the system service.

## AppImage

```sh
scripts/package-appimage.sh --fetch-tool
./target/package/ruro-studio-*-x86_64.AppImage
```

`--fetch-tool` downloads `appimagetool` into a temporary directory if it is not installed. The
AppImage starts the studio; the bundled tools are available as
`./ruro-studio-*.AppImage ruro <command>`.

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
