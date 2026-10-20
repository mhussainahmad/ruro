//! Tests against a real GPU. They need the NVIDIA driver and a CUDA device,
//! and are skipped (with the reason printed) when either is missing, as on CI.

use std::time::{Duration, Instant};

use ruro_gpu::{
    DType, Device, DeviceBuffer, GpuImage, Normalize, Stream, Tensor, cuda, normalize, pool_stats,
};
use ruro_msg::{Encoding, Image, Message, Preview};

fn gpu() -> bool {
    match ruro_gpu::unavailable() {
        Some(why) => {
            eprintln!("skipping: {why}");
            false
        }
        None => true,
    }
}

/// A test image whose every byte depends on its position and `seed`.
fn pattern(width: u32, height: u32, encoding: Encoding, seed: u8) -> Image {
    let mut img = Image::new(width, height, encoding);
    for (i, b) in img.data.iter_mut().enumerate() {
        *b = (i as u32)
            .wrapping_mul(2_654_435_761)
            .rotate_left(seed as u32 % 31) as u8
            ^ seed;
    }
    img
}

#[test]
fn driver_reports_a_device() {
    if !gpu() {
        return;
    }
    let c = cuda().unwrap();
    assert!(c.device_count() >= 1);
    let (free, total) = c.memory_info(0).unwrap();
    assert!(free > 0 && free <= total);
    eprintln!(
        "CUDA driver {}, device 0: {} (sm_{}{}), {} MiB",
        c.driver_version(),
        c.device_name(0).unwrap(),
        c.compute_capability(0).unwrap().0,
        c.compute_capability(0).unwrap().1,
        total >> 20
    );
    assert!(c.device_name(c.device_count()).is_err());
}

#[test]
fn buffers_copy_both_ways() {
    if !gpu() {
        return;
    }
    let stream = Stream::new(0).unwrap();
    let src: Vec<u8> = (0..100_000u32).map(|i| (i * 7) as u8).collect();
    let mut a = DeviceBuffer::alloc(0, src.len()).unwrap();
    a.upload(&src, &stream).unwrap();
    let mut back = vec![0u8; src.len()];
    a.download(&mut back, &stream).unwrap();
    assert_eq!(back, src);

    let mut b = DeviceBuffer::alloc(0, src.len()).unwrap();
    b.fill(0xab, &stream).unwrap();
    b.download(&mut back, &stream).unwrap();
    assert!(back.iter().all(|&v| v == 0xab));
    b.copy_from(&a, &stream).unwrap();
    b.download(&mut back, &stream).unwrap();
    assert_eq!(back, src);

    // Every 10th row of 100-byte rows.
    let mut rows = vec![0u8; 100 * 100];
    a.download_rows(&mut rows, 100, 100, 10, 100, &stream)
        .unwrap();
    for r in 0..100 {
        assert_eq!(
            &rows[r * 100..r * 100 + 100],
            &src[r * 1000..r * 1000 + 100]
        );
    }
    assert!(a.upload(&vec![0; src.len() + 1], &stream).is_err());
}

#[test]
fn the_pool_reuses_allocations() {
    if !gpu() {
        return;
    }
    // A size no other test uses, so their buffers do not interfere.
    let len = 11 * 1024 * 1024 + 17;
    drop(DeviceBuffer::alloc(0, len).unwrap());
    let mut ptrs = std::collections::HashSet::new();
    for _ in 0..100 {
        let b = DeviceBuffer::alloc(0, len).unwrap();
        ptrs.insert(b.device_ptr());
    }
    // One allocation, handed out again and again. (The pool's counters are
    // shared with tests running in parallel, so they are not compared.)
    assert_eq!(ptrs.len(), 1);
    let stats = pool_stats(0).unwrap();
    assert!(
        stats.allocations >= 1 && stats.bytes >= len as u64,
        "{stats:?}"
    );
}

#[test]
fn images_roundtrip_and_preview() {
    if !gpu() {
        return;
    }
    let stream = Stream::new(0).unwrap();
    for enc in [Encoding::Rgb8, Encoding::Rgba8, Encoding::Mono8] {
        let img = pattern(1280, 720, enc, 3);
        let g = GpuImage::from_image(0, &img, &stream).unwrap();
        assert_eq!(
            (g.width, g.height, g.step),
            (1280, 720, img.stride() as u32)
        );
        assert_eq!(g.to_image().unwrap(), img);
        // The preview samples whole pixels from the device copy.
        let Preview::Image {
            width,
            height,
            rgba,
        } = g.preview()
        else {
            panic!("expected an image preview")
        };
        assert_eq!((width, height), (320, 180));
        let ch = enc.channels();
        let (x, y) = (17usize, 33usize);
        let px = &img.data[(y * 4) * img.stride() + x * 4 * ch..][..ch];
        let got = &rgba[(y * 320 + x) * 4..][..4];
        assert_eq!(got[0], px[0], "{enc:?}");
    }
    assert!(matches!(GpuImage::default().preview(), Preview::Text(_)));
}

#[test]
fn in_process_decode_shares_the_buffer_and_holds_it() {
    if !gpu() {
        return;
    }
    let stream = Stream::new(0).unwrap();
    let img = pattern(640, 480, Encoding::Rgb8, 9);
    let mut sent = GpuImage::from_image(0, &img, &stream).unwrap();
    let ptr = sent.buffer().unwrap().device_ptr();

    let bytes = sent.to_bytes();
    assert!(
        bytes.len() < 200,
        "only a descriptor travels: {} bytes",
        bytes.len()
    );
    let got = GpuImage::decode(&bytes).unwrap();
    assert_eq!(got.buffer().unwrap().device_ptr(), ptr, "same memory");
    assert!(got.buffer().unwrap().is_imported());
    assert_eq!(got.to_image().unwrap(), img);

    // While the receiver holds it, the producer cannot write it: reshaping
    // moves the producer to another buffer.
    sent.reshape(0, 640, 480, Encoding::Rgb8).unwrap();
    assert_ne!(sent.buffer().unwrap().device_ptr(), ptr);
    assert_eq!(
        got.to_image().unwrap(),
        img,
        "the receiver's frame is intact"
    );

    // A received buffer can be passed on (to a third process) as is.
    let again = GpuImage::decode(&got.to_bytes()).unwrap();
    assert_eq!(again.buffer().unwrap().device_ptr(), ptr);
    drop(again);
    drop(got);

    // Once released, the producer may reuse it, after a few newer buffers
    // (the ring that gives receivers time to pin a frame).
    let mut reused = false;
    for _ in 0..10 {
        sent.reshape(0, 640, 480, Encoding::Rgb8).unwrap();
        let _ = sent.to_bytes();
        reused |= sent.buffer().unwrap().device_ptr() == ptr;
    }
    assert!(reused);
}

#[test]
fn stale_descriptors_are_refused() {
    if !gpu() {
        return;
    }
    let stream = Stream::new(0).unwrap();
    let img = pattern(320, 240, Encoding::Mono8, 1);
    let mut g = GpuImage::from_image(0, &img, &stream).unwrap();
    let a = g.buffer().unwrap().device_ptr();
    let bytes = g.to_bytes();
    // The producer goes round its ring of buffers and writes this one again
    // before the receiver got to it.
    for _ in 0..10 {
        g.upload(0, &pattern(320, 240, Encoding::Mono8, 2), &stream)
            .unwrap();
        let _ = g.to_bytes();
        if g.buffer().unwrap().device_ptr() == a {
            break;
        }
    }
    assert_eq!(g.buffer().unwrap().device_ptr(), a);
    let err = GpuImage::decode(&bytes).unwrap_err().to_string();
    assert!(err.contains("stale"), "{err}");
    // The current descriptor works.
    assert_eq!(
        GpuImage::decode(&g.to_bytes()).unwrap().to_image().unwrap(),
        pattern(320, 240, Encoding::Mono8, 2)
    );
}

/// CPU reference of the normalize kernel.
fn normalize_cpu(img: &Image, p: &Normalize) -> Vec<f32> {
    let (dw, dh) = if p.width == 0 {
        (img.width, img.height)
    } else {
        (p.width, p.height)
    };
    let ch = img.encoding.channels();
    let out_ch = if img.encoding == Encoding::Mono8 {
        1
    } else {
        3
    };
    let mut out = vec![0f32; out_ch * (dw * dh) as usize];
    for y in 0..dh as usize {
        for x in 0..dw as usize {
            let sx = x * img.width as usize / dw as usize;
            let sy = y * img.height as usize / dh as usize;
            let px = &img.data[sy * img.stride() + sx * ch..];
            for c in 0..out_ch {
                let sc = if ch == 1 {
                    0
                } else if p.bgr {
                    2 - c
                } else {
                    c
                };
                let scale = 1.0 / (255.0 * p.std[c]);
                let bias = -p.mean[c] / p.std[c];
                out[c * (dw * dh) as usize + y * dw as usize + x] = px[sc] as f32 * scale + bias;
            }
        }
    }
    out
}

#[test]
fn sixteen_bit_images_roundtrip() {
    if !gpu() {
        return;
    }
    let stream = Stream::new(0).unwrap();
    let img = pattern(640, 480, Encoding::Mono16, 5);
    let g = GpuImage::from_image(0, &img, &stream).unwrap();
    assert_eq!(g.step, 640 * 2);
    assert_eq!(g.to_image().unwrap(), img);
    assert!(matches!(g.preview(), Preview::Image { .. }));
    // Wire form too.
    let back = GpuImage::decode(&g.to_bytes()).unwrap();
    assert_eq!(back.to_image().unwrap(), img);
}

#[test]
fn normalize_checks_the_image_against_its_buffer() {
    if !gpu() {
        return;
    }
    let stream = Stream::new(0).unwrap();
    let mut t = Tensor::default();
    let depth = GpuImage::from_image(0, &pattern(16, 16, Encoding::Mono16, 1), &stream).unwrap();
    assert!(normalize(&depth, &mut t, &Normalize::default(), &stream).is_err());
    let mut g = GpuImage::from_image(0, &pattern(16, 16, Encoding::Rgb8, 1), &stream).unwrap();
    (g.width, g.height) = (8192, 8192);
    assert!(normalize(&g, &mut t, &Normalize::default(), &stream).is_err());
    // The context is still usable.
    DeviceBuffer::alloc(0, 1024).unwrap();
}

#[test]
fn normalize_matches_the_cpu() {
    if !gpu() {
        return;
    }
    let stream = Stream::new(0).unwrap();
    let cases = [
        (Encoding::Rgb8, Normalize::default()),
        (
            Encoding::Rgb8,
            Normalize {
                width: 640,
                height: 640,
                mean: [0.485, 0.456, 0.406],
                std: [0.229, 0.224, 0.225],
                bgr: true,
            },
        ),
        (
            Encoding::Rgba8,
            Normalize {
                width: 100,
                height: 37,
                ..Default::default()
            },
        ),
        (
            Encoding::Mono8,
            Normalize {
                mean: [0.5; 3],
                std: [0.25; 3],
                ..Default::default()
            },
        ),
    ];
    let mut out = Tensor::default();
    for (enc, p) in cases {
        let img = pattern(1920, 1080, enc, 5);
        let g = GpuImage::from_image(0, &img, &stream).unwrap();
        normalize(&g, &mut out, &p, &stream).unwrap();
        assert_eq!(out.device(), Device::Cuda(0));
        assert_eq!(out.dtype, DType::F32);
        let expected = normalize_cpu(&img, &p);
        let got = out.to_f32_vec().unwrap();
        assert_eq!(got.len(), expected.len(), "{enc:?} {p:?}");
        let worst = got
            .iter()
            .zip(&expected)
            .map(|(a, b)| (a - b).abs())
            .fold(0f32, f32::max);
        assert!(worst < 1e-5, "{enc:?} {p:?}: max error {worst}");
    }
    assert_eq!(out.shape, vec![1, 1, 1080, 1920]);
}

#[test]
fn tensors_move_between_host_and_device() {
    if !gpu() {
        return;
    }
    let data: Vec<f32> = (0..24).map(|i| i as f32 * 0.5 - 3.0).collect();
    let host = Tensor::from_f32(vec![2, 3, 4], &data).unwrap();
    let dev = host.to_device(0).unwrap();
    assert_eq!(dev.device(), Device::Cuda(0));
    assert_eq!(dev.to_f32_vec().unwrap(), data);

    let bytes = dev.to_bytes();
    let got = Tensor::decode(&bytes).unwrap();
    assert_eq!(got.shape, vec![2, 3, 4]);
    assert_eq!(got.strides, vec![12, 4, 1]);
    assert_eq!(
        got.buffer().unwrap().device_ptr(),
        dev.buffer().unwrap().device_ptr()
    );
    assert_eq!(got.to_f32_vec().unwrap(), data);
    let Preview::Text(p) = got.preview() else {
        panic!()
    };
    assert!(p.contains("cuda:0") && p.contains("[-3, 8.5]"), "{p}");
}

// ---------------------------------------------------------------------------
// Two processes. The test binary runs itself as the receiving child; the two
// talk over ruro's shared-memory transport, as two host processes do.

const CHILD_ENV: &str = "RURO_GPU_TEST_CHILD";

/// Message kinds from parent to child.
const DECODE_AND_HOLD: u8 = 1;
const STORE: u8 = 2;
const DECODE_STORED: u8 = 3;
const QUIT: u8 = 4;
const DECODE_AND_PIN: u8 = 5;
const UNPIN: u8 = 6;

fn wait_one(sub: &ruro_transport::ShmSubscriber, timeout: Duration) -> Vec<u8> {
    let start = Instant::now();
    loop {
        let mut got = None;
        sub.drain(|_, _, body| got = Some(body.to_vec())).unwrap();
        if let Some(g) = got {
            return g;
        }
        assert!(start.elapsed() < timeout, "no message within {timeout:?}");
        let _ = sub.wait(Duration::from_millis(50));
    }
}

#[test]
fn gpu_child_process() {
    let Ok(topic) = std::env::var(CHILD_ENV) else {
        return;
    };
    let shm = ruro_transport::Shm::new("ruro-gpu-test-child").unwrap();
    let rx = shm.subscriber(&format!("{topic}/to-child"), 16).unwrap();
    let tx = shm.publisher(&format!("{topic}/to-parent")).unwrap();
    let reply = |text: String| {
        tx.send(0, 0, text.len(), &mut |b| {
            b.copy_from_slice(text.as_bytes())
        })
        .unwrap()
    };
    // Announce until the parent answers.
    let mut held: Option<GpuImage> = None;
    let mut pinned: Option<GpuImage> = None;
    let mut stored: Option<Vec<u8>> = None;
    let mut first = true;
    let mut idle = Instant::now();
    loop {
        let msg = loop {
            if first {
                reply("ready".into());
            }
            let mut got = None;
            rx.drain(|_, _, body| got = Some(body.to_vec())).unwrap();
            if let Some(g) = got {
                break g;
            }
            // The parent died without saying goodbye.
            assert!(idle.elapsed() < Duration::from_secs(60), "parent went away");
            let _ = rx.wait(Duration::from_millis(50));
        };
        idle = Instant::now();
        first = false;
        let (kind, seed, body) = (msg[0], msg[1], &msg[2..]);
        let check = |bytes: &[u8]| -> Result<(GpuImage, Duration), String> {
            let start = Instant::now();
            let g = GpuImage::decode(bytes).map_err(|e| e.to_string())?;
            let took = start.elapsed();
            let expected = pattern(g.width, g.height, g.encoding, seed);
            let img = g.to_image().map_err(|e| e.to_string())?;
            if img != expected {
                return Err("pixels differ".into());
            }
            Ok((g, took))
        };
        match kind {
            DECODE_AND_HOLD => match check(body) {
                Ok((g, took)) => {
                    // Drop the previous frame only now: the parent checks
                    // that a held frame is not reused.
                    held = Some(g);
                    reply(format!("ok {}", took.as_micros()));
                }
                Err(e) => reply(format!("error {e}")),
            },
            DECODE_AND_PIN => match check(body) {
                Ok((g, took)) => {
                    pinned = Some(g);
                    reply(format!("ok {}", took.as_micros()));
                }
                Err(e) => reply(format!("error {e}")),
            },
            UNPIN => {
                pinned = None;
                reply("unpinned".into());
            }
            STORE => {
                stored = Some(body.to_vec());
                reply("stored".into());
            }
            DECODE_STORED => {
                let bytes = stored.take().unwrap();
                match check(&bytes) {
                    Ok(_) => reply("ok".into()),
                    Err(e) => reply(format!("error {e}")),
                }
            }
            QUIT => {
                drop(held.take());
                drop(pinned.take());
                reply("bye".into());
                return;
            }
            other => panic!("unknown message {other}"),
        }
    }
}

#[test]
fn frames_cross_processes_without_copies() {
    if !gpu() {
        return;
    }
    let topic = format!("ruro-gpu-test/{}", std::process::id());
    let shm = ruro_transport::Shm::new("ruro-gpu-test-parent").unwrap();
    let rx = shm.subscriber(&format!("{topic}/to-parent"), 16).unwrap();
    let tx = shm.publisher(&format!("{topic}/to-child")).unwrap();
    let mut child = Child(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "gpu_child_process", "--nocapture"])
            .env(CHILD_ENV, &topic)
            .spawn()
            .unwrap(),
    );
    let timeout = Duration::from_secs(20);
    assert_eq!(wait_one(&rx, timeout), b"ready");

    let send = |kind: u8, seed: u8, img: &GpuImage| -> String {
        let len = img.encoded_len();
        tx.send(0, 0, 2 + len, &mut |b| {
            b[0] = kind;
            b[1] = seed;
            img.encode_into(&mut b[2..]);
        })
        .unwrap();
        loop {
            let r = String::from_utf8(wait_one(&rx, timeout)).unwrap();
            // The child announces itself until its first message arrives.
            if r != "ready" {
                return r;
            }
        }
    };

    let stream = Stream::new(0).unwrap();
    // A frame size (size class) no other test in this binary uses.
    let (w, h) = (1920, 1200);
    let ptr = |g: &GpuImage| g.buffer().unwrap().device_ptr();
    // Frame A: mapped by the child, which keeps it until told otherwise.
    let mut g = GpuImage::from_image(0, &pattern(w, h, Encoding::Rgb8, 1), &stream).unwrap();
    let a_ptr = ptr(&g);
    let r = send(DECODE_AND_PIN, 1, &g);
    assert!(r.starts_with("ok"), "{r}");
    eprintln!(
        "first receive (loads CUDA, maps the allocation): {} µs",
        &r[3..]
    );

    // The producer sends frame after frame from one image; the child keeps
    // the latest. A is pinned, so it never comes back, and the others cycle
    // through a small ring of buffers.
    let mut buffers = std::collections::HashSet::new();
    let mut r = String::new();
    for seed in 2..42 {
        g.upload(0, &pattern(w, h, Encoding::Rgb8, seed), &stream)
            .unwrap();
        assert_ne!(ptr(&g), a_ptr, "a pinned buffer must not be overwritten");
        buffers.insert(ptr(&g));
        r = send(DECODE_AND_HOLD, seed, &g);
        assert!(r.starts_with("ok"), "{r}");
    }
    // A ring of four (see `RING` in memory.rs), plus slack for a slow child.
    eprintln!("40 frames used {} buffers", buffers.len());
    assert!(
        buffers.len() <= 6,
        "{} buffers for 40 frames",
        buffers.len()
    );
    eprintln!("receive of an already mapped allocation: {} µs", &r[3..]);

    // Once released, A is reused.
    assert_eq!(send(UNPIN, 0, &GpuImage::default()), "unpinned");
    let mut reused = false;
    for seed in 42..60 {
        g.upload(0, &pattern(w, h, Encoding::Rgb8, seed), &stream)
            .unwrap();
        reused |= ptr(&g) == a_ptr;
        let r = send(DECODE_AND_HOLD, seed, &g);
        assert!(r.starts_with("ok"), "{r}");
    }
    assert!(reused, "a released buffer is reused");

    // A frame whose buffer the producer reuses before the child decodes it
    // is refused.
    let mut d = GpuImage::from_image(0, &pattern(w, h, Encoding::Rgb8, 100), &stream).unwrap();
    let d_ptr = ptr(&d);
    assert_eq!(send(STORE, 100, &d), "stored");
    for seed in 101..130 {
        d.upload(0, &pattern(w, h, Encoding::Rgb8, seed), &stream)
            .unwrap();
        let _ = d.to_bytes(); // sent somewhere: the buffer is exported
        if ptr(&d) == d_ptr {
            break;
        }
    }
    assert_eq!(ptr(&d), d_ptr, "the producer went round its ring");
    let r = send(DECODE_STORED, 100, &d);
    assert!(r.contains("stale"), "{r}");

    assert_eq!(send(QUIT, 0, &GpuImage::default()), "bye");
    assert!(child.0.wait().unwrap().success());
}

/// Kills the child if the test fails.
struct Child(std::process::Child);

impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

const PRODUCER_ENV: &str = "RURO_GPU_TEST_PRODUCER";

/// Run in a child process: sends `RURO_GPU_TEST_FRAMES` frames, each in its
/// own allocation, waits for the parent's acknowledgement and exits.
#[test]
fn gpu_producer_child() {
    let Ok(topic) = std::env::var(PRODUCER_ENV) else {
        return;
    };
    let n: u8 = std::env::var("RURO_GPU_TEST_FRAMES")
        .unwrap()
        .parse()
        .unwrap();
    let shm = ruro_transport::Shm::new("ruro-gpu-test-producer").unwrap();
    let tx = shm.publisher(&format!("{topic}/frames")).unwrap();
    let rx = shm.subscriber(&format!("{topic}/acks"), 4).unwrap();
    let stream = Stream::new(0).unwrap();
    let frames: Vec<GpuImage> = (0..n)
        .map(|i| GpuImage::from_image(0, &pattern(64, 64, Encoding::Rgb8, i), &stream).unwrap())
        .collect();
    for f in &frames {
        tx.send(0, 0, f.encoded_len(), &mut |b| f.encode_into(b))
            .unwrap();
        // The parent's queue holds 16.
        std::thread::sleep(Duration::from_millis(5));
    }
    wait_one(&rx, Duration::from_secs(20));
    if std::env::var("RURO_GPU_TEST_FREE").is_ok() {
        // Past the pool's 64 idle allocations, the oldest are freed; then
        // one more frame tells the parent to look.
        drop(frames);
        let f = GpuImage::from_image(0, &pattern(64, 64, Encoding::Rgb8, 0), &stream).unwrap();
        tx.send(0, 0, f.encoded_len(), &mut |b| f.encode_into(b))
            .unwrap();
        wait_one(&rx, Duration::from_secs(20));
    }
}

/// Receives the frames of one producer child, which then exits.
fn receive_from_producer(topic: &str, n: u8) {
    let shm = ruro_transport::Shm::new("ruro-gpu-test-consumer").unwrap();
    let rx = shm.subscriber(&format!("{topic}/frames"), 16).unwrap();
    let tx = shm.publisher(&format!("{topic}/acks")).unwrap();
    let mut child = Child(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "gpu_producer_child"])
            .env(PRODUCER_ENV, topic)
            .env("RURO_GPU_TEST_FRAMES", n.to_string())
            .spawn()
            .unwrap(),
    );
    let mut got = Vec::new();
    let start = Instant::now();
    while got.len() < n as usize {
        assert!(start.elapsed() < Duration::from_secs(20), "frames missing");
        let _ = rx.wait(Duration::from_millis(50));
        rx.drain(|_, _, body| got.push(GpuImage::decode(body).unwrap()))
            .unwrap();
    }
    for (i, g) in got.iter().enumerate() {
        assert!(g.to_image().unwrap() == pattern(64, 64, Encoding::Rgb8, i as u8));
    }
    drop(got);
    tx.send(0, 0, 0, &mut |_| {}).unwrap();
    assert!(child.0.wait().unwrap().success());
}

/// Run alone (`--ignored`): it counts this process's mappings, which the
/// other cross-process tests change.
#[test]
#[ignore = "counts mappings: run with --ignored --exact"]
fn mappings_of_freed_allocations_are_released() {
    if !gpu() {
        return;
    }
    let topic = format!("ruro-gpu-test/{}/freed", std::process::id());
    let shm = ruro_transport::Shm::new("ruro-gpu-test-consumer").unwrap();
    let rx = shm.subscriber(&format!("{topic}/frames"), 128).unwrap();
    let tx = shm.publisher(&format!("{topic}/acks")).unwrap();
    let mut child = Child(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "gpu_producer_child"])
            .env(PRODUCER_ENV, &topic)
            .env("RURO_GPU_TEST_FRAMES", "70")
            .env("RURO_GPU_TEST_FREE", "1")
            .spawn()
            .unwrap(),
    );
    let receive = |n: usize| {
        let mut got = Vec::new();
        let start = Instant::now();
        while got.len() < n {
            assert!(start.elapsed() < Duration::from_secs(20), "frames missing");
            let _ = rx.wait(Duration::from_millis(50));
            rx.drain(|_, _, body| got.push(GpuImage::decode(body).unwrap()))
                .unwrap();
        }
        got
    };
    drop(receive(70));
    assert_eq!(ruro_gpu::ipc::mapped_allocations(), 70);
    tx.send(0, 0, 0, &mut |_| {}).unwrap();
    let last = receive(1);
    // The six the producer freed are no longer mapped here.
    assert_eq!(ruro_gpu::ipc::mapped_allocations(), 64);
    drop(last);
    tx.send(0, 0, 0, &mut |_| {}).unwrap();
    assert!(child.0.wait().unwrap().success());
}

#[test]
fn mappings_of_exited_producers_are_released() {
    if !gpu() {
        return;
    }
    // This test is the only one that maps other processes' memory in this
    // process (the child of `frames_cross_processes_without_copies` is the
    // receiver there).
    let topic = format!("ruro-gpu-test/{}/producers", std::process::id());
    receive_from_producer(&format!("{topic}/1"), 3);
    assert_eq!(ruro_gpu::ipc::mapped_allocations(), 3);
    // Meeting the next producer lets go of the exited one's allocations.
    receive_from_producer(&format!("{topic}/2"), 1);
    assert_eq!(ruro_gpu::ipc::mapped_allocations(), 1);
}
