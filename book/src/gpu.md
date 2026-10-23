# GPU buffers

> **Status:** tested on one machine with an NVIDIA RTX 4090 (driver 610.57.04), in one
> process and between two processes. CI has no GPU, so the GPU tests are skipped there; they
> print why. `ml/onnx-cuda` feeds `gpu/Tensor`s to ONNX Runtime's CUDA provider (see
> [Machine learning](ml.md#onnx-runtime-on-nvidia-gpus---features-ruro-nodes-mlonnx-cuda)).

Images and tensors can live in GPU memory and move between nodes without being copied.
Within a process group, receivers share the device buffer, as with every ruro message.
Between process groups on one machine, only a small descriptor of the buffer travels
through shared memory, and the receiving process maps the same GPU memory. A 1080p frame
crosses processes in about 4 µs this way, against about 0.36 ms for the same frame in host
memory (see [Performance](performance.md#gpu-buffers)).

## Requirements

An NVIDIA GPU and its driver. ruro uses only the driver library (`libcuda.so.1`), which it
loads when a GPU node starts: building needs no CUDA toolkit, and kernels are PTX compiled
by the driver. Set `RURO_CUDA_LIB` to load the library from another path. Without a driver
or a GPU, the GPU nodes fail to start with a message saying why; everything else runs as
usual.

The GPU nodes are part of the standard `ruro-host`.

## Messages

`crates/ruro-gpu` defines two messages:

- `gpu/Image`: `width`, `height`, `encoding` (`Rgb8`, `Rgba8`, `Mono8`), `step` (bytes per
  row) and a device buffer.
- `gpu/Tensor`: `dtype` (`U8`, `I32`, `I64`, `F16`, `F32`, `F64`), `shape`, `strides` (in
  elements) and its data, either in host memory or in a device buffer. `device()` says which:
  `Cpu` or `Cuda(n)`.

## Nodes

| Node | Input | Output | Parameters |
|---|---|---|---|
| `gpu/upload` | `image`: `std/Image` | `out`: `gpu/Image` | `device` (CUDA ordinal, default 0) |
| `gpu/download` | `image`: `gpu/Image` | `out`: `std/Image` | |
| `gpu/normalize` | `image`: `gpu/Image` | `tensor`: `gpu/Tensor` | `width`, `height`, `mean`, `std`, `bgr` |
| `gpu/tensor-to-cpu` | `tensor`: `gpu/Tensor` | `out`: `ml/Tensor` | |

`gpu/normalize` turns an image into model input on the GPU: a float32 `[1, C, H, W]` tensor,
resized to `width`×`height` (nearest neighbour; 0 keeps the image size), scaled to 0..1, then
`(v - mean) / std` per channel. C is 3 for colour images (alpha is dropped) and 1 for mono.
It runs a hand-written PTX kernel; the pixels never leave the GPU. `gpu/tensor-to-cpu` hands
the result to nodes that run on the CPU, such as `ml/yolo-decode`.

`examples/gpu.yaml` uploads 1080p frames in one process and normalizes and downloads them in
another:

```sh
ruro run examples/gpu.yaml
ruro echo normalize.tensor   # tensor f32 [1, 3, 640, 640] on cuda:0, values in [...]
```

## Watching GPU outputs

The studio's watch panel and `ruro echo` work on GPU outputs. A `gpu/Image` preview
downloads only every n-th row and column, and only while someone watches; a `gpu/Tensor`
preview shows its shape, device and value range.

## Writing GPU nodes in Rust

Depend on `ruro-gpu`. Fill pooled messages with `Output::loan`, as with host images:

```rust
use ruro::prelude::*;
use ruro_gpu::{GpuImage, Stream};

#[derive(Node)]
#[node(kind = "my/upload")]
struct MyUpload {
    #[input]
    image: Input<Image>,
    #[output]
    out: Output<GpuImage>,
    stream: Option<Stream>,
}

impl Node for MyUpload {
    fn start(&mut self, _ctx: &mut Ctx) -> Result {
        self.stream = Some(Stream::new(0)?);
        Ok(())
    }

    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        let stream = self.stream.as_ref().unwrap();
        while let Some(img) = self.image.recv() {
            let mut frame = self.out.loan();
            frame.upload(0, &img, stream)?; // or reshape(), then write buffer_mut()
            frame.send()?;
        }
        Ok(())
    }
}
```

`GpuImage::reshape` and `Tensor::reshape_on` make the buffer writable: they keep it when no
one else holds it, in this process or another, and take another one from the device's pool
otherwise. `buffer_mut()` then gives a `DeviceBuffer` whose `device_ptr_mut()` you can pass
to your own kernels. `ruro_gpu::cuda()` reports why CUDA is unavailable, if it is.

Two rules keep data consistent, because ruro does not track work queued on streams:

- **Finish writing before sending.** Synchronize the stream that wrote the buffer. The
  helpers (`upload`, `normalize`, ...) return only when their work is done.
- **Finish reading before dropping.** A received buffer may be reused by its producer as soon
  as every receiver has dropped it.

## How sharing works

Every device buffer comes from a per-device pool of whole `cuMemAlloc` allocations, so a node
that produces a frame per step stops allocating once warm.

Within a process, a message holds its buffer in an `Arc`. Sending it to three nodes shares
one allocation.

Across processes, the message is encoded as a 128-byte descriptor: the allocation's CUDA IPC
handle, the producer's process id and boot id, and a lease slot with a generation number. The
receiver maps the allocation with `cuIpcOpenMemHandle` the first time it sees it (about
0.1 ms), keeps the mapping, and reads the memory in place.

The producer must not overwrite a buffer that a receiver is reading, and a receiver must not
read a buffer the producer has already overwritten. Each producer process keeps a lease table,
a small file in `/dev/shm` with one 64-bit word per exported allocation: a generation number
and a count of readers.

- A receiver *pins* a buffer by adding a reader, but only if the generation still matches its
  descriptor (one compare-and-swap). Dropping the received buffer removes the reader.
- The producer *reclaims* a buffer before writing it again by swapping "generation g, no
  readers" for "generation g + 1, no readers". That fails while anyone reads it; once it
  succeeds, old descriptors can no longer pin it.
- A buffer that has been sent to another process goes back to the pool and is reused only
  after three newer ones of its size. Receivers therefore have a few frame periods to pin a
  frame. A frame that arrives later than that is dropped as stale, with a warning in the log.

The producer never waits for receivers: its pool skips buffers that are still pinned and
allocates another. A received buffer can be passed on to a third process unchanged.

## Limitations

- **One machine.** Descriptors are only valid on the machine (and boot) that made them.
  Wires to other machines and recordings would carry them, so a receiver there refuses them.
  Put `gpu/download` before such wires and before recorded outputs.
- **Crashes.** A receiver that crashes while holding buffers keeps them pinned until the
  producer exits: each crash of a receiving group costs its producer a few frames' worth of
  GPU memory (about 18 MiB for 1080p frames in `examples/gpu.yaml`). A receiver releases the
  mappings of a producer that exited when it meets the restarted one.
- **Synchronization** is done by the producer before sending; IPC events
  (`cuIpcGetEventHandle`) would let receivers wait on the device instead.
- **ONNX Runtime** takes `gpu/Tensor`s in `ml/onnx-cuda` (feature `onnx-cuda`), which
  returns its output in host memory; outputs that stay on the device are not supported yet.
  `ml/onnx` and `ml/policy` run on the CPU.
- **Python nodes** cannot receive GPU messages.
- One CUDA device per buffer; there are no peer copies between GPUs.
