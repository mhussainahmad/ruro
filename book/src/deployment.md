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
RURO_LISTEN=tcp/0.0.0.0:7447       # accept connections from the network
RURO_HOST=/opt/my-robot/bin/my-host  # host binary with your own nodes
RURO_LOG=info,zenoh=warn
```

The unit:

- listens on `tcp/127.0.0.1:7447` unless `RURO_LISTEN` says otherwise;
- keeps deployed graphs in `/run/ruro` and uses `/var/lib/ruro` as its working directory;
- raises `LimitRTPRIO` and `LimitMEMLOCK` so real-time process groups can use them;
- applies systemd hardening that leaves shared memory (`/dev/shm`) and real-time scheduling
  available to the host processes.

Hardware access is granted through groups. Add the `ruro` user to the groups that own your
devices, for example:

```sh
sudo usermod -aG dialout,video ruro
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

> The control protocol is not authenticated or encrypted. Only listen on networks you trust,
> for example the robot's own network or a VPN.

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
  `RURO_HOST`.

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

CI builds the ARM binaries on every push.

## Checklist

- [ ] Daemon enabled as a service, listening on the robot's network.
- [ ] Custom host installed and `RURO_HOST` set, if you have your own nodes.
- [ ] Service user in the device groups (`dialout`, `video`, ...).
- [ ] Graph validated against the robot's daemon: `ruro --connect tcp/robot:7447 check graph.yaml`.
- [ ] Restart policies set on nodes that must stay up.
