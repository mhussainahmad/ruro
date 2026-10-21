# Deploying on robots

On a robot, run one daemon per machine, usually as a service, and deploy graphs to it from
your workstation with the CLI or the studio.

## Running the daemon as a service

The Debian package installs two systemd units, both disabled by default.

### System service

`ruro-daemon.service` runs the daemon at boot as the `ruro` system user:

```sh
sudo systemctl enable --now ruro-daemon
journalctl -u ruro-daemon -f
```

Configure it in `/etc/default/ruro` (or with `sudo systemctl edit ruro-daemon`):

```sh
# /etc/default/ruro
RURO_LISTEN=tls/0.0.0.0:7447         # accept connections from the network
RURO_TLS_CERT=/etc/ruro/robot.pem    # with TLS and a token, see Security
RURO_TLS_KEY=/etc/ruro/robot-key.pem
RURO_TOKEN=change-me
RURO_HOST=/opt/my-robot/bin/my-host  # host binary with your own nodes
RURO_LOG=info,zenoh=warn
```

The key and the file holding the token should be readable only by root and the `ruro`
user. With this configuration every client needs the TLS and token options, including
`ruro status` run on the robot itself (`--connect tls/<robot>:7447 --tls-ca ca.pem
--token ...`), and the studio cannot connect at all (see [Security](security.md)). Keep the
default `tcp/127.0.0.1:7447` listener instead if you want to use the studio over an SSH
tunnel.

The unit:

- listens on `tcp/127.0.0.1:7447` unless `RURO_LISTEN` says otherwise;
- keeps deployed graphs and its runtime files in `/run/ruro/ruro-<name>/` and uses
  `/var/lib/ruro` as its working directory (relative paths in graphs, such as model files,
  are resolved there);
- raises `LimitRTPRIO` and `LimitMEMLOCK` so real-time process groups can use them;
- applies systemd hardening that leaves shared memory (`/dev/shm`) and real-time scheduling
  available to the host processes;
- restarts the daemon when it exits with status 75 after installing an update. Updating the
  daemon over the network takes a drop-in that runs it from a directory it can write; see
  [Updating daemons](updates.md#restarting).

Hardware access is granted through groups. Add the `ruro` user to the groups that own your
devices (`dialout` for serial ports, `video` for cameras, `input` for gamepads, and the
groups of `/dev/gpiochip*` and `/dev/i2c-*` on your system), for example:

```sh
sudo usermod -aG dialout,video,input ruro
sudo systemctl restart ruro-daemon
```

### User service

To run the daemon in your own session instead (handy on a development machine):

```sh
systemctl --user enable --now ruro-daemon
```

Override settings with `systemctl --user edit ruro-daemon`, for example
`Environment=RURO_LISTEN=tcp/0.0.0.0:7447`.

## Working remotely

With the daemon listening on the network, point any client at it:

```sh
ruro --connect tcp/robot.local:7447 status
ruro-studio --connect tcp/robot.local:7447 my-graph.yaml

# or once for the shell session:
export RURO_CONNECT=tcp/robot.local:7447
ruro deploy my-graph.yaml
ruro logs
```

If several daemons share one router, select one with `--daemon <name>` or `RURO_DAEMON`. A
daemon's name defaults to its hostname; set it with `ruro daemon --name`.

A daemon listening on `tcp/` accepts anyone who can reach it, without encryption. On any
network you do not fully trust, use TLS and a token (see [Security](security.md)): the CLI
then passes `--tls-ca` and `--token` (or `RURO_TLS_CA` and `RURO_TOKEN`). The studio does not
support TLS or tokens yet: for studio use, keep the robot's daemon on
`tcp/127.0.0.1:7447` without a token and reach it through an SSH tunnel.

To run one graph across several computers of a robot, see
[Multi-machine graphs](multi-machine.md).

## Docker

The repository's `Dockerfile` builds an image with `ruro` and `ruro-host` (no studio):

```sh
docker build -t ruro .
docker run -d --name ruro --restart unless-stopped \
    --shm-size=512m -p 7447:7447 ruro
```

- Cross-process wires use shared memory. Raise `--shm-size` for image pipelines, or use
  `--ipc=host`.
- For real-time process groups add
  `--cap-add=SYS_NICE --ulimit rtprio=95 --ulimit memlock=-1`.
- Give the container access to devices with `--device`, e.g. `--device /dev/ttyUSB0`.
- To use your own nodes, build an image from this one that adds your host binary and sets
  `RURO_HOST`. The image does not contain the Python host.
- The example graphs are in `/usr/share/ruro/examples/`.

## Cross-compiling for ARM

Most robot computers (Raspberry Pi 4/5, NVIDIA Jetson) are 64-bit ARM. The headless tools
cross-compile from an x86-64 machine:

```sh
sudo apt-get install gcc-aarch64-linux-gnu
rustup target add aarch64-unknown-linux-gnu
cargo build --release --target aarch64-unknown-linux-gnu -p ruro-cli -p ruro-host
```

The linker is configured in `.cargo/config.toml`. The binaries are in
`target/aarch64-unknown-linux-gnu/release/`; copy `ruro` and `ruro-host` (or your own host) to
the robot, side by side.

Alternatively, [`cross`](https://github.com/cross-rs/cross) builds in a container with the
toolchain preinstalled (configured in `Cross.toml`):

```sh
cargo install cross --locked
cross build --release --target aarch64-unknown-linux-gnu -p ruro-cli -p ruro-host
```

CI builds the ARM binaries, and runs the tests on them under emulation, for pull requests
and pushes to `main`.

## Checklist

- [ ] Daemon enabled as a service, listening on the robot's network with TLS and a token.
- [ ] Custom host installed and `RURO_HOST` set, if you have your own nodes.
- [ ] To update over the network: a signing key, its public key in the daemon's
  `--update-keys` file, and the daemon run from a directory it can write
  ([Updating daemons](updates.md)).
- [ ] Service user in the device groups (`dialout`, `video`, ...).
- [ ] Graph validated against the robot's daemon: `ruro --connect tcp/robot:7447 check graph.yaml`.
- [ ] Restart policies set on nodes that must stay up.
- [ ] A hardware emergency stop within reach: `ruro estop` is a software stop.
