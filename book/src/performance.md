# Performance

Measured with `cargo bench` on a 16-thread x86-64 Linux machine:

| Path | Payload | Time per hop |
|---|---|---|
| In-process | `Scalar` | 143 ns |
| In-process | 1080p RGB image (shared) | 191 ns |
| Shared memory | 8 B | 4.5 µs |
| Shared memory | 640×480 RGB | 26 µs |
| Shared memory | 1080p RGB | 0.7 ms |

Reproduce with:

```sh
cargo bench -p ruro-node -p ruro-transport
```

## What the numbers mean

- **In-process** wires connect nodes in the same process group. A message is wrapped in an
  `Arc` once and shared with every receiver, so the cost does not depend on its size: a 1080p
  frame costs about the same as a single number.
- **Shared-memory** wires connect nodes in different groups. The message is serialised into an
  iceoryx2 shared-memory sample and deserialised on the other side, so the cost grows with the
  payload: small messages take a few microseconds; a full-HD frame, under a millisecond.

## Practical advice

- Put nodes that exchange large messages at high rates (camera to perception) in the **same
  group**. Use separate groups for isolation: drivers that may crash, third-party code, or
  parts you want to restart independently.
- Watching outputs is cheap: taps produce previews only while someone watches.
- `ruro status` and the studio show each node's p50/p99 step latency and CPU use; start there
  when something is slow.
- Real-time scheduling for tight control loops is [in development](upcoming/realtime.md).
