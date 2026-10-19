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
    let before = pool_stats(0).unwrap().allocations;
    let mut ptrs = std::collections::HashSet::new();
    for _ in 0..100 {
        let b = DeviceBuffer::alloc(0, len).unwrap();
        ptrs.insert(b.device_ptr());
    }
    assert_eq!(
        pool_stats(0).unwrap().allocations,
        before,
        "no new allocations"
    );
    assert_eq!(ptrs.len(), 1);
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

    // Once released, the producer may reuse it.
    let mut reused = GpuImage::new(0, 640, 480, Encoding::Rgb8).unwrap();
    let mut tries = 0;
    while reused.buffer().unwrap().device_ptr() != ptr && tries < 70 {
        // Other buffers of this size may be ahead in the pool.
        reused = GpuImage::new(0, 640, 480, Encoding::Rgb8).unwrap();
        tries += 1;
    }
    assert_eq!(reused.buffer().unwrap().device_ptr(), ptr);
}

#[test]
fn stale_descriptors_are_refused() {
    if !gpu() {
        return;
    }
    let stream = Stream::new(0).unwrap();
    let img = pattern(320, 240, Encoding::Mono8, 1);
    let mut g = GpuImage::from_image(0, &img, &stream).unwrap();
    let bytes = g.to_bytes();
    // The producer writes the buffer again before the receiver got to it.
    g.upload(0, &pattern(320, 240, Encoding::Mono8, 2), &stream)
        .unwrap();
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
    // Frame A: mapped by the child, which keeps it.
    let mut a = GpuImage::from_image(0, &pattern(w, h, Encoding::Rgb8, 1), &stream).unwrap();
    let a_ptr = a.buffer().unwrap().device_ptr();
    let r = send(DECODE_AND_HOLD, 1, &a);
    assert!(r.starts_with("ok"), "{r}");
    eprintln!(
        "first receive (loads CUDA, maps the allocation): {} µs",
        &r[3..]
    );

    // The child holds A, so writing a new frame moves to another buffer.
    a.upload(0, &pattern(w, h, Encoding::Rgb8, 2), &stream)
        .unwrap();
    let b_ptr = a.buffer().unwrap().device_ptr();
    assert_ne!(b_ptr, a_ptr, "a held buffer must not be overwritten");
    // Frame B replaces A in the child, which releases A.
    let r = send(DECODE_AND_HOLD, 2, &a);
    assert!(r.starts_with("ok"), "{r}");
    eprintln!(
        "first receive of another allocation (maps it): {} µs",
        &r[3..]
    );

    // A is free again: the pool hands it out (oldest first) without allocating.
    let before = pool_stats(0).unwrap().allocations;
    let mut c = GpuImage::new(0, w, h, Encoding::Rgb8).unwrap();
    assert_eq!(c.buffer().unwrap().device_ptr(), a_ptr);
    assert_eq!(pool_stats(0).unwrap().allocations, before);
    c.upload(0, &pattern(w, h, Encoding::Rgb8, 3), &stream)
        .unwrap();
    // Received again (now cached mapping): fast.
    let r = send(DECODE_AND_HOLD, 3, &c);
    assert!(r.starts_with("ok"), "{r}");
    eprintln!("receive of an already mapped allocation: {} µs", &r[3..]);

    // A frame reused by the producer before the child decodes it is refused.
    let mut d = GpuImage::from_image(0, &pattern(w, h, Encoding::Rgb8, 4), &stream).unwrap();
    assert_eq!(send(STORE, 4, &d), "stored");
    d.upload(0, &pattern(w, h, Encoding::Rgb8, 5), &stream)
        .unwrap();
    let r = send(DECODE_STORED, 4, &d);
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
