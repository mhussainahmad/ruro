# Roadmap

ruro is meant for the same users as ROS 2: startups, researchers and companies running
robots in production. This page lists what still stands between ruro and that, roughly in
the order it will be done. An item is ticked only when a test proves it, and
[Project status](status.md) then says how it was verified.

## Small gaps

- [x] `ruro param` checks values against the node's declared range and type.
- [x] Python nodes get the emergency stop like Rust nodes.
- [ ] An emergency stop engaged on a peer daemon reaches the daemon that deployed the graph.

## Installing

- [ ] The quick start works from a fresh clone in a clean container.
- [ ] Every crate passes `cargo publish --dry-run`, in dependency order.
- [ ] A release workflow builds `ruro` and `rurod` for Linux x86_64 and aarch64.
- [ ] Cross-built aarch64 binaries pass the tests under emulation, in CI.

## ROS 2

- [ ] Simulated time follows a ROS `/clock`.
- [ ] ruro nodes offer ROS parameter services (`ros2 param get/set/list`).
- [ ] ruro nodes offer the type-description service.
- [ ] The generic bridges can be transient-local.

## Production

- [ ] Two daemons on separate network namespaces, with TLS, latency and packet loss.
- [ ] A daemon update that starts and later fails is rolled back automatically.
- [ ] An hour-long run of a realistic graph without growing memory or file descriptors.

## Later

- ONNX Runtime taking GPU buffers (its CUDA provider).
- GPU buffers in Python nodes.
- H.265 RTSP streams.
