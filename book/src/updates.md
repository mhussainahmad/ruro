# Updating daemons

`ruro update` replaces the `ruro` binary of running daemons, and optionally their host
binary, over the network. The daemon only accepts binaries signed with a key it trusts, tries
the new binary before it installs it, and keeps the previous one.

> **Status:** implemented and tested end to end with `ruro daemon` processes on one machine:
> an update executed in place, the restart through a service manager (the test plays
> systemd's part), updating a peer before the daemon it is reached through, a binary that
> exits at once (refused, nothing replaced) and an updated daemon that cannot start (the
> previous binary is put back). Signature, version, target and graph checks have their own
> tests. It has not been run under a real systemd or across a real network.

## Keys

Make a signing key once, on the machine that builds releases:

```sh
ruro keygen release            # writes release.key (secret) and release.pub
```

`release.key` is readable only by you; keep it off the robots. Each daemon trusts the
public keys listed in a file, one per line (blank lines and `#` comments are ignored):

```sh
ruro daemon --update-keys /etc/ruro/update-keys      # or RURO_UPDATE_KEYS
```

A daemon started without `--update-keys` refuses every update and every upload of one.
List more than one key to rotate keys: add the new one, update with it, then remove the
old one.

## Signing a binary

```sh
cargo build --release -p ruro-cli -p ruro-host
ruro sign --key release.key target/release/ruro
ruro sign --key release.key --host target/release/ruro-host
```

`ruro sign` writes `<binary>.sig` next to the binary. The Ed25519 signature covers the
binary's SHA-256 and what it claims to be: daemon or host, its version and the target triple
it was built for. The version is read from the binary (`--version`) and the target is this
machine's triple with the architecture of the binary's ELF header. For a binary that does not
run here, such as an ARM build, the version must be given:

```sh
ruro sign --key release.key --binary-version 0.2.0 \
    target/aarch64-unknown-linux-gnu/release/ruro
```

`--target` sets the triple when the guess is wrong (32-bit ARM, musl builds).

## Updating

```sh
ruro --connect tls/robot:7447 ... update target/release/ruro
ruro update target/release/ruro --host-binary target/release/ruro-host
ruro update target/release/ruro --peers        # every daemon reachable through this one
ruro update target/release/ruro --check        # only ask whether they would accept it
```

| Option | Meaning |
|---|---|
| `--host-binary <PATH>` | Also replace the daemon's default host binary. |
| `--to <NAME>` | Daemon to update (same as `--daemon`). |
| `--peers` | Also update every other daemon that answers through the connected one, before it. |
| `--allow-downgrade` | Install a daemon binary that is not newer than the running one. |
| `--timeout <SECONDS>` | How long the new daemon gets to start, on trial and after the restart (default 30). |
| `--check` | Only check; upload nothing. |

Every daemon is checked before any is updated. With `--peers` the connected daemon goes
last, because the others are reached through it. Each update goes through these steps:

1. The daemon checks the signatures, versions, targets and its state. Refusals say what to
   do:
   - the signature must be by a trusted key and match the binary, version and target;
   - the target triple must be the daemon's own, and the binary an ELF executable for its
     architecture;
   - the daemon binary must be newer than the running daemon (semantic versioning), unless
     `--allow-downgrade` is given. A host binary must carry the version of the daemon it
     goes with: the one installed alongside, or the running one;
   - no graph may be running, unless the e-stop is engaged. The update restarts the daemon,
     which stops the graph: stop it with `ruro stop` or engage the stop with `ruro estop`.
     A graph that spans machines is stopped everywhere;
   - the daemon must be able to write the directory of the binary it replaces.
2. The client uploads the binaries in 1 MiB chunks. Each is stored next to the binary it
   replaces as `.ruro.update` (`.ruro-host.update`), synced to disk.
3. The daemon checks the uploaded files against the signatures, then starts the new daemon
   binary on trial: with the daemon's own arguments, but under the name `<name>-trial`, on a
   free loopback port and without peers. The trial must start (load its host's node types,
   open its endpoint) and report the signed version and target within the timeout. A new
   host binary must answer `describe`. If anything fails, the uploaded files are removed and
   nothing else changes:

   ```text
   error: `robot`: nothing was replaced; `robot` still runs 0.1.0: the new daemon binary failed
   its trial start: it exited (exit status: 1) before it was up
   ```

4. The daemon keeps the current binaries as `ruro.prev` (`ruro-host.prev`), renames the new
   ones into place, stops the graph (finishing any recording) and restarts.
5. The client waits for the daemon to come back and checks its version:

   ```text
   uploading daemon to `robot`: 21843968/21843968 bytes
   installed daemon 0.2.0 at /var/lib/ruro/bin/ruro; `robot` restarts now
   `robot` runs 0.2.0 (updated from 0.1.0 to 0.2.0)
   ```

A host binary updated on its own needs no restart: groups use it from the next deploy. Your
own host (`RURO_HOST`) may print its own crate version for `--version`; sign it with
`--binary-version` set to the daemon version it goes with.

After an update the daemon runs no graph; deploy it again. If the e-stop was engaged, the
new daemon starts with it engaged, as if engaged there: release it with
`ruro estop --release`. `ruro status`, `ruro fleet` and the studio's Fleet view show each
daemon's version and target triple, and how its last update went.

## Restarting

A daemon run from a terminal or a script executes the new binary in place: same process id,
same arguments and environment. Before it restarts, it leaves `.ruro.handoff-<name>.json`
next to the binary: the new process reads it to report the update, to start with the e-stop
engaged if it was, and to know which binaries to put back if it cannot start.

Under a service manager, set `RURO_RESTART=exit`: the daemon then exits with status 75 and
the service manager starts the new binary. The packaged systemd units set it, with
`RestartForceExitStatus=75` so the restart happens whatever `Restart=` says.

The packaged system service runs `/usr/bin/ruro`, which belongs to root and is read-only to
the service, so updates are refused there. To update it over the air, run the daemon from
copies in a directory the `ruro` user owns, with the drop-in the package installs:

```sh
sudo install -d -o ruro -g ruro /var/lib/ruro/bin
sudo install -o ruro -g ruro /usr/bin/ruro /usr/bin/ruro-host /var/lib/ruro/bin/
sudo install -Dm644 /usr/share/ruro/systemd/ruro-daemon-updates.conf \
    /etc/systemd/system/ruro-daemon.service.d/updates.conf
sudo install -Dm644 release.pub /etc/ruro/update-keys
sudo systemctl daemon-reload && sudo systemctl restart ruro-daemon
```

The drop-in starts `/var/lib/ruro/bin/ruro` and sets `RURO_UPDATE_KEYS`. The host binary
next to it is the default host, so `--host-binary` updates it too. Upgrading the Debian
package no longer changes the daemon that runs; update it with `ruro update`.

## Rollback

- A new binary that fails its trial is never installed (step 3).
- If the installed binary then fails to start for real, it puts the previous binaries back
  and starts them (under systemd: exits, and systemd starts the previous binary). The daemon
  reports `rolled back to 0.1.0: 0.2.0 did not start (...)` in its status, and `ruro update`
  fails with it.
- Once started, the new daemon is on probation for `--update-probation` seconds (default
  120, or `RURO_UPDATE_PROBATION`). If it stops in that time without being asked to (it
  crashes, or the kernel kills it) and is started again, it puts the previous binaries back
  and starts them, which report `rolled back to 0.1.0: 0.2.0 stopped within 120 s of
  starting`. Under systemd (`Restart=on-failure`) that happens on its own; a daemon run by
  hand is rolled back when you next start it. A daemon stopped on purpose (SIGTERM, Ctrl-C,
  a reboot) keeps the update, and so does one that outlives its probation.
- To go back by hand, rename `ruro.prev` over `ruro` and restart the daemon, or install the
  older build with `ruro update --allow-downgrade`.

Not covered: a new daemon that hangs rather than exits, or that fails after its probation.
The previous binary is still there as `ruro.prev`.

## Limitations

- Versions are only compared with the running daemon. A daemon does not remember the
  highest version it ran, so after a manual rollback an update to any version newer than
  the running one is accepted.
- Daemons of different versions can share a network while a fleet is being updated, and
  status messages of older daemons are still read. Other protocol changes between versions
  are not guaranteed to be compatible: update every machine of a graph together
  (`--peers`).
- The upload goes through the daemon's control channel, so it needs the same TLS and token
  options as any other client (see [Security](security.md)).
- Updates replace `ruro` and the default host. Hosts uploaded with
  `ruro deploy --host-binary` are replaced by uploading them again.
