# Performance

Measured with `cargo bench` on a 16-thread x86-64 Linux machine:

| Path | Payload | Time per hop |
|---|---|---|
| In-process | `Scalar` | 143 ns |
| In-process | 1080p RGB image (shared) | 191 ns |
| Shared memory | 8 B | 4.5 µs |
| Shared memory | 640×480 RGB | 26 µs |
| Shared memory | 1080p RGB | 0.7 ms |

These figures come from one development machine; they have not been re-measured since and
are not checked in CI. Reproduce them with:

```sh
cargo bench -p ruro-node -p ruro-transport
```

The `ruro-node` bench `local` measures in-process hops (`local_hop/*`, including a loaned
1080p frame); `ruro-transport` has `hop` (raw shared-memory bytes, no serialisation) and
`receive` (decoding). `scripts/bench.sh` saves and compares runs (see
[Benchmarks](runtime.md#benchmarks)).

## GPU buffers

Measured with `cargo bench -p ruro-gpu --bench gpu_hop` on an Intel Core i9-14900 with an
NVIDIA RTX 4090 (driver 610.57.04), while other builds were running on the machine; copy
times varied by up to a third between runs, and the table gives the range of two runs.

| Path | Payload | Time |
|---|---|---|
| In-process | 1080p RGB `gpu/Image` (loaned, shared) | 136 ns per hop |
| Two processes, round trip | `Scalar` and a one-byte reply | 6.5–7.2 µs |
| Two processes, round trip | 1080p RGB `gpu/Image` and a one-byte reply | 7.0–7.2 µs |
| Two processes, round trip | 1080p RGB `std/Image` and a one-byte reply | 355–362 µs |
| Upload | 1080p RGB from a `Vec` / from pinned memory | 330–383 µs / 265 µs |
| Download | 1080p RGB into a `Vec` / into pinned memory | 485–686 µs / 241 µs |
| `gpu/normalize` | 1080p RGB to a 640×640 float tensor | 8.3 µs |

A one-way hop is the round trip less half the scalar round trip: about 4 µs for a frame in
GPU memory, whatever its size, against about 0.36 ms for the same frame in host memory on
this machine (0.7 ms on the machine of the table above). The GPU figure includes pinning and
releasing the buffer; mapping an allocation the receiver has not seen before adds about
0.1 ms, once per allocation, and the first message a process receives also loads CUDA (about
80 ms). Upload and download are what `gpu/upload` and `gpu/download` cost, so keep frames
on the GPU between them.

## What the numbers mean

- **In-process** wires connect nodes in the same process group. A message is wrapped in an
  `Arc` once and shared with every receiver, so the cost does not depend on its size: a 1080p
  frame costs about the same as a single number.
- **Shared-memory** wires connect nodes in different groups. The message is serialised into an
  iceoryx2 shared-memory sample and deserialised on the other side, so the cost grows with the
  payload: small messages take a few microseconds; a full-HD frame, under a millisecond.

## Practical advice

- Put nodes that exchange large messages at high rates (camera to perception) in the **same
  group**, or keep the frames in [GPU memory](gpu.md), which crosses groups at the cost of
  a small message. Use separate groups for isolation: drivers that may crash, third-party
  code, or parts you want to restart independently.
- Watching outputs is cheap: taps produce previews only while someone watches.
- `ruro status` and the studio show each node's p50/p99 step latency and CPU use; start there
  when something is slow.
- For tight control loops, use a [real-time group](realtime.md), and measure timer jitter
  with `cargo bench -p ruro-host --bench jitter`.
