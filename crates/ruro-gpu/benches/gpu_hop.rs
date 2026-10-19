//! Cost of moving a 1080p frame in GPU memory between nodes, against the
//! CPU image path, plus host/device copies and the normalize kernel.
//!
//! - `gpu_local_hop`: in-process send and receive of a pooled `gpu/Image`.
//! - `gpu_cross_process`: a round trip to a second process (this binary,
//!   run as a child): the message is published into shared memory, decoded
//!   by a typed input on the other side (for `gpu/Image`: the buffer is
//!   pinned and mapped, not copied), dropped, and a one-byte reply comes
//!   back. `scalar` measures the round trip of two tiny messages, so a
//!   one-way hop is about `frame - scalar / 2`.
//! - `gpu_copy`: uploads and downloads of a 1080p RGB frame, from pageable
//!   (`Vec`) and pinned host memory.
//! - `gpu_kernel`: `gpu/normalize` of a 1080p frame to 640x640.
//!
//! Skipped, with the reason printed, without a CUDA device.

use std::sync::Arc;
use std::time::{Duration, Instant};

use criterion::{Criterion, Throughput, criterion_group};
use ruro_gpu::{GpuImage, Normalize, PinnedBuffer, Stream, Tensor};
use ruro_msg::{Encoding, Image, Message, Scalar};
use ruro_node::{Builder, ErasedInput, Input};
use ruro_transport::{Shm, ShmSubscriber};

const CHILD_ENV: &str = "RURO_GPU_BENCH_CHILD";
const W: u32 = 1920;
const H: u32 = 1080;

const KIND_GPU: u8 = 1;
const KIND_CPU: u8 = 2;
const KIND_SCALAR: u8 = 3;
const KIND_QUIT: u8 = 4;

struct Noop;
impl ruro_node::DynNode for Noop {
    fn start(&mut self, _: &mut ruro_node::Ctx) -> ruro_node::Result {
        Ok(())
    }
    fn step(&mut self, _: &mut ruro_node::Ctx) -> ruro_node::Result {
        Ok(())
    }
    fn stop(&mut self, _: &mut ruro_node::Ctx) {}
    fn set_param(&mut self, _: &str, _: &serde_json::Value) -> ruro_node::Result {
        Ok(())
    }
}

fn typed_input<T: Message>() -> (Input<T>, Arc<dyn ErasedInput>) {
    let params = Default::default();
    let mut b = Builder::new("bench", &params, Default::default());
    let input = b.input::<T>("in").unwrap();
    let erased = b.finish(Box::new(Noop)).inputs.remove(0).1;
    (input, erased)
}

fn wait_one(sub: &ShmSubscriber, mut f: impl FnMut(&[u8])) {
    loop {
        let mut n = 0;
        sub.drain(|_, _, body| {
            f(body);
            n += 1;
        })
        .unwrap();
        if n > 0 {
            return;
        }
        let _ = sub.wait(Duration::from_millis(100));
    }
}

/// The receiving process: decodes every message with a typed input, drops
/// it and replies.
fn child(topic: &str) {
    let shm = Shm::new("ruro-gpu-bench-child").unwrap();
    let rx = shm.subscriber(&format!("{topic}/ping"), 1).unwrap();
    let tx = shm.publisher(&format!("{topic}/pong")).unwrap();
    let (gpu_in, gpu_erased) = typed_input::<GpuImage>();
    let (cpu_in, cpu_erased) = typed_input::<Image>();
    let (scalar_in, scalar_erased) = typed_input::<Scalar>();
    let mut announced = false;
    let mut idle = Instant::now();
    loop {
        if !announced {
            tx.send(0, 0, 1, &mut |b| b[0] = 0).unwrap();
        }
        let mut quit = false;
        let n = rx
            .drain(|seq, stamp, body| {
                announced = true;
                let (kind, msg) = (body[0], &body[1..]);
                match kind {
                    KIND_GPU => {
                        gpu_erased.push_encoded(seq, stamp, msg).unwrap();
                        let s = gpu_in.recv().unwrap();
                        assert!(s.buffer().unwrap().len() >= (W * H * 3) as usize);
                    }
                    KIND_CPU => {
                        cpu_erased.push_encoded(seq, stamp, msg).unwrap();
                        assert_eq!(cpu_in.recv().unwrap().data.len(), (W * H * 3) as usize);
                    }
                    KIND_SCALAR => {
                        scalar_erased.push_encoded(seq, stamp, msg).unwrap();
                        scalar_in.recv().unwrap();
                    }
                    _ => quit = true,
                }
                tx.send(0, 0, 1, &mut |b| b[0] = kind).unwrap();
            })
            .unwrap();
        if quit {
            return;
        }
        if n > 0 {
            idle = Instant::now();
        } else if idle.elapsed() > Duration::from_secs(300) {
            return; // the parent went away
        }
        let _ = rx.wait(Duration::from_millis(100));
    }
}

fn gpu_benches(c: &mut Criterion) {
    if let Some(why) = ruro_gpu::unavailable() {
        eprintln!("skipping GPU benchmarks: {why}");
        return;
    }
    let cuda = ruro_gpu::cuda().unwrap();
    eprintln!(
        "CUDA driver {}, {}",
        cuda.driver_version(),
        cuda.device_name(0).unwrap()
    );
    let stream = Stream::new(0).unwrap();
    let img = Image::new(W, H, Encoding::Rgb8);
    let len = img.data.len();

    // In-process hop of a pooled GPU frame.
    let params = Default::default();
    let mut b = Builder::new("bench", &params, Default::default());
    let out = b.output::<GpuImage>("out").unwrap();
    let inp = b.input::<GpuImage>("in").unwrap();
    let built = b.finish(Box::new(Noop));
    built.outputs[0]
        .1
        .connect_local(built.inputs[0].1.clone())
        .unwrap();
    c.bench_function("gpu_local_hop/1080p_gpu_image_loaned", |b| {
        b.iter(|| {
            let mut frame = out.loan();
            frame.reshape(0, W, H, Encoding::Rgb8).unwrap();
            frame.send().unwrap();
            inp.recv().unwrap()
        })
    });

    // Round trips to a second process.
    let topic = format!("ruro-gpu-bench/{}", std::process::id());
    let shm = Shm::new("ruro-gpu-bench").unwrap();
    let pong = shm.subscriber(&format!("{topic}/pong"), 1).unwrap();
    let ping = shm.publisher(&format!("{topic}/ping")).unwrap();
    let mut child_proc = std::process::Command::new(std::env::current_exe().unwrap())
        .env(CHILD_ENV, &topic)
        .spawn()
        .unwrap();
    wait_one(&pong, |_| {});
    let round_trip = |kind: u8, msg: &dyn Fn(&mut [u8]), len: usize| {
        ping.send(1, 0, 1 + len, &mut |b| {
            b[0] = kind;
            msg(&mut b[1..]);
        })
        .unwrap();
        let mut got = 0;
        while got != kind {
            wait_one(&pong, |b| got = b[0]);
        }
    };
    let mut group = c.benchmark_group("gpu_cross_process");
    let s = Scalar(1.0);
    group.bench_function("scalar", |b| {
        b.iter(|| round_trip(KIND_SCALAR, &|buf| s.encode_into(buf), s.encoded_len()))
    });
    let mut frame = GpuImage::new(0, W, H, Encoding::Rgb8).unwrap();
    group.bench_function("1080p_gpu_image", |b| {
        b.iter(|| {
            // The producer takes its frame back (the child has dropped it)
            // and sends it again.
            frame.reshape(0, W, H, Encoding::Rgb8).unwrap();
            round_trip(KIND_GPU, &|buf| frame.encode_into(buf), frame.encoded_len())
        })
    });
    group.throughput(Throughput::Bytes(len as u64));
    group.bench_function("1080p_cpu_image", |b| {
        b.iter(|| round_trip(KIND_CPU, &|buf| img.encode_into(buf), img.encoded_len()))
    });
    group.finish();
    ping.send(1, 0, 1, &mut |b| b[0] = KIND_QUIT).unwrap();
    let _ = child_proc.wait();

    // Copies and the kernel.
    let mut group = c.benchmark_group("gpu_copy");
    group.throughput(Throughput::Bytes(len as u64));
    let mut g = GpuImage::new(0, W, H, Encoding::Rgb8).unwrap();
    group.bench_function("upload_1080p_pageable", |b| {
        b.iter(|| g.upload(0, &img, &stream).unwrap())
    });
    let mut pinned = PinnedBuffer::new(len).unwrap();
    group.bench_function("upload_1080p_pinned", |b| {
        b.iter(|| g.buffer_mut().unwrap().upload(&pinned, &stream).unwrap())
    });
    let mut back = Image::default();
    group.bench_function("download_1080p_pageable", |b| {
        b.iter(|| g.download_into(&mut back, &stream).unwrap())
    });
    group.bench_function("download_1080p_pinned", |b| {
        b.iter(|| g.buffer().unwrap().download(&mut pinned, &stream).unwrap())
    });
    group.finish();

    let mut group = c.benchmark_group("gpu_kernel");
    let mut t = Tensor::default();
    let p = Normalize {
        width: 640,
        height: 640,
        ..Default::default()
    };
    group.bench_function("normalize_1080p_to_640", |b| {
        b.iter(|| ruro_gpu::normalize(&g, &mut t, &p, &stream).unwrap())
    });
    group.finish();
}

criterion_group!(benches, gpu_benches);

fn main() {
    if let Ok(topic) = std::env::var(CHILD_ENV) {
        child(&topic);
        return;
    }
    benches();
    Criterion::default().configure_from_args().final_summary();
}
