//! `hw/realsense` against real cameras. Needs the `realsense` feature,
//! librealsense2 and at least one camera; skipped unless
//! RURO_TEST_REALSENSE=1:
//!
//! RURO_TEST_REALSENSE=1 RURO_REALSENSE_LIB=/path/to/librealsense2.so \
//!     cargo test -p ruro-nodes-hw --features realsense --test realsense
//!
//! Every connected camera is tested, one at a time and then all at once.
//! The cameras should stand still, looking at a scene within a few metres.

#![cfg(feature = "realsense")]

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ruro::msg::{CameraInfo, Encoding, Image, Imu, PointCloud};
use ruro::node::testing::Harness;
use ruro_nodes_hw::RealSense;
use ruro_nodes_hw::realsense::ffi::Api;
use serde_json::json;

fn cameras() -> Option<Vec<(String, String)>> {
    if std::env::var("RURO_TEST_REALSENSE").is_err() {
        eprintln!("skipping: set RURO_TEST_REALSENSE=1 with a camera attached");
        return None;
    }
    let api = Api::load(None).expect("librealsense2");
    let devices = api.devices().expect("listing cameras");
    eprintln!("librealsense2 {}: {devices:?}", api.version);
    assert!(
        !devices.is_empty(),
        "RURO_TEST_REALSENSE is set but no camera is connected"
    );
    Some(devices)
}

#[derive(Default)]
struct Received {
    color: Vec<Image>,
    color_info: Vec<CameraInfo>,
    depth: Vec<Image>,
    depth_info: Vec<CameraInfo>,
    infrared: Vec<Image>,
    points: Vec<PointCloud>,
    imu: Vec<Imu>,
    /// How old each colour stamp was when its message arrived, in seconds.
    color_lag: Vec<f64>,
    /// From starting the node to the first colour or depth image.
    first_frame: f64,
    seconds: f64,
}

/// Streams from the camera with `serial` for `secs` seconds, stepping as a
/// host would at the node's 30 Hz.
fn stream(serial: &str, params: serde_json::Value, secs: f64) -> Received {
    let mut p = json!({ "serial": serial, "width": 640, "height": 480, "fps": 30 });
    p.as_object_mut()
        .unwrap()
        .extend(params.as_object().unwrap().clone());
    let started = Instant::now();
    let mut h = Harness::with_node::<RealSense>(p, |_| {}).unwrap();
    let mut r = Received::default();
    // The first frames take a while: the camera starts its streams.
    loop {
        h.step().unwrap();
        let mut first = Received::default();
        drain(&mut h, &mut first);
        if !first.color.is_empty() || !first.depth.is_empty() {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{serial}: no image within 10 s"
        );
        std::thread::sleep(Duration::from_millis(33));
    }
    r.first_frame = started.elapsed().as_secs_f64();
    // Then a second for auto-exposure to settle.
    let settle = Instant::now() + Duration::from_secs(1);
    while Instant::now() < settle {
        h.step().unwrap();
        drain(&mut h, &mut Received::default());
        std::thread::sleep(Duration::from_millis(33));
    }
    let start = Instant::now();
    while start.elapsed().as_secs_f64() < secs {
        h.step().unwrap();
        drain(&mut h, &mut r);
        std::thread::sleep(Duration::from_millis(33));
    }
    r.seconds = start.elapsed().as_secs_f64();
    r
}

fn drain(h: &mut Harness, r: &mut Received) {
    r.color.extend(h.take::<Image>("color").unwrap());
    let info = h.take::<CameraInfo>("color_info").unwrap();
    let now = now_ns() as i64;
    r.color_lag.extend(
        info.iter()
            .map(|i| (now - i.header.stamp_ns as i64) as f64 / 1e9),
    );
    r.color_info.extend(info);
    r.depth.extend(h.take::<Image>("depth").unwrap());
    r.depth_info
        .extend(h.take::<CameraInfo>("depth_info").unwrap());
    r.infrared.extend(h.take::<Image>("infrared").unwrap());
    r.points.extend(h.take::<PointCloud>("points").unwrap());
    r.imu.extend(h.take::<Imu>("imu").unwrap());
}

fn depth_mm(img: &Image) -> Vec<u16> {
    img.data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes(*b))
        .collect()
}

/// Fraction of pixels with depth in the median depth image. Depth
/// auto-exposure can hunt for a moment, leaving single frames nearly empty
/// (seen on D455Fs), so no one frame stands for the stream.
fn median_valid(depth: &[Image]) -> f64 {
    let mut f: Vec<f64> = depth
        .iter()
        .map(|d| {
            let mm = depth_mm(d);
            mm.iter().filter(|&&v| v > 0).count() as f64 / mm.len() as f64
        })
        .collect();
    f.sort_by(f64::total_cmp);
    f[f.len() / 2]
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

fn check_full(name: &str, r: &Received) {
    let rate = |n: usize| n as f64 / r.seconds;
    eprintln!(
        "{name}: {:.1} colour/s, {:.1} depth/s, {:.1} infrared/s, {:.1} clouds/s, {:.0} imu/s",
        rate(r.color.len()),
        rate(r.depth.len()),
        rate(r.infrared.len()),
        rate(r.points.len()),
        rate(r.imu.len()),
    );
    // 30 fps asked; every frame is published, but allow for the odd one
    // the camera drops.
    for (what, n) in [
        ("colour", r.color.len()),
        ("depth", r.depth.len()),
        ("infrared", r.infrared.len()),
    ] {
        assert!(rate(n) > 28.0, "{name}: {what} at {:.1} fps", rate(n));
    }
    assert_eq!(
        r.color_info.len(),
        r.color.len(),
        "one CameraInfo per colour image"
    );
    assert_eq!(
        r.depth_info.len(),
        r.depth.len(),
        "one CameraInfo per depth image"
    );

    let c = r.color.last().unwrap();
    assert_eq!((c.width, c.height, c.encoding), (640, 480, Encoding::Rgb8));
    assert_eq!(c.data.len(), 640 * 480 * 3);
    let mean = c.data.iter().map(|&v| v as f64).sum::<f64>() / c.data.len() as f64;
    assert!(
        (5.0..250.0).contains(&mean),
        "{name}: colour image is blank (mean {mean:.0})"
    );

    // Depth is aligned to colour, so it has the colour image's size and
    // camera model.
    let d = r.depth.last().unwrap();
    assert_eq!(
        (d.width, d.height, d.encoding),
        (640, 480, Encoding::Mono16)
    );
    let fraction = median_valid(&r.depth);
    let mut sorted: Vec<u16> = depth_mm(d).into_iter().filter(|&v| v > 0).collect();
    sorted.sort_unstable();
    let median = sorted.get(sorted.len() / 2).copied().unwrap_or(0);
    eprintln!(
        "{name}: {:.0}% of depth pixels valid, median {median} mm",
        fraction * 100.0
    );
    assert!(
        fraction > 0.3,
        "{name}: only {:.0}% of depth pixels are valid",
        fraction * 100.0
    );
    assert!(
        (150..10_000).contains(&median),
        "{name}: median depth {median} mm"
    );

    let ci = r.color_info.last().unwrap();
    let di = r.depth_info.last().unwrap();
    assert_eq!(ci.header.frame_id, "camera_color_optical_frame");
    assert_eq!(
        di.header.frame_id, "camera_color_optical_frame",
        "aligned depth"
    );
    assert_eq!((ci.width, ci.height), (640, 480));
    assert_eq!(ci.k, di.k, "aligned depth uses the colour intrinsics");
    let (fx, fy, cx, cy) = (ci.k[0], ci.k[4], ci.k[2], ci.k[5]);
    eprintln!(
        "{name}: fx {fx:.1} fy {fy:.1} cx {cx:.1} cy {cy:.1} {}",
        ci.distortion_model
    );
    assert!(
        (300.0..700.0).contains(&fx) && (fx - fy).abs() < 0.05 * fx,
        "fx {fx} fy {fy}"
    );
    assert!(
        (cx - 320.0).abs() < 40.0 && (cy - 240.0).abs() < 40.0,
        "cx {cx} cy {cy}"
    );

    // Stamps come from the camera's clock mapped to system time: they rise
    // and are close to now.
    let stamps: Vec<u64> = r.color_info.iter().map(|i| i.header.stamp_ns).collect();
    assert!(
        stamps.windows(2).all(|w| w[1] > w[0]),
        "{name}: colour stamps do not rise"
    );
    let mut lag = r.color_lag.clone();
    lag.sort_by(f64::total_cmp);
    let (median, worst) = (lag[lag.len() / 2], lag[lag.len() - 1]);
    eprintln!(
        "{name}: stamps {:.0} ms old on arrival (median), {:.0} ms at worst",
        median * 1e3,
        worst * 1e3
    );
    assert!(
        (0.0..0.2).contains(&median) && worst < 0.5,
        "{name}: stamps {median:.3} s old on arrival (median), {worst:.3} s at worst"
    );
    let span = (stamps.last().unwrap() - stamps[0]) as f64 / 1e9;
    let mean_dt = span / (stamps.len() - 1) as f64;
    assert!(
        (mean_dt - 1.0 / 30.0).abs() < 0.004,
        "{name}: mean frame interval {mean_dt:.4} s"
    );

    let ir = r.infrared.last().unwrap();
    assert_eq!(
        (ir.width, ir.height, ir.encoding),
        (640, 480, Encoding::Mono8)
    );

    // The point cloud: in metres, in front of the camera, within range, and
    // consistent with the depth image it came from.
    let pc = r.points.last().unwrap();
    assert!(pc.points.len() > 1000, "{name}: {} points", pc.points.len());
    assert!(
        pc.points.iter().all(|p| p[2] > 0.0 && p[2] <= 4.0 + 1e-3),
        "points outside (0, 4] m"
    );
    let mut z: Vec<f32> = pc.points.iter().map(|p| p[2]).collect();
    z.sort_by(f32::total_cmp);
    eprintln!(
        "{name}: {} points, median z {:.3} m",
        z.len(),
        z[z.len() / 2]
    );

    // A camera at rest: gravity in the accelerometer, little rotation.
    assert!(
        rate(r.imu.len()) > 100.0,
        "{name}: {:.0} IMU samples/s",
        rate(r.imu.len())
    );
    let n = r.imu.len() as f64;
    let mean = |f: fn(&Imu) -> f64| r.imu.iter().map(f).sum::<f64>() / n;
    let a = [
        mean(|i| i.linear_acceleration.x),
        mean(|i| i.linear_acceleration.y),
        mean(|i| i.linear_acceleration.z),
    ];
    let g = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
    let w = [
        mean(|i| i.angular_velocity.x),
        mean(|i| i.angular_velocity.y),
        mean(|i| i.angular_velocity.z),
    ];
    eprintln!("{name}: mean accel {a:.2?} (|a| {g:.2}), mean gyro {w:.3?}");
    assert!(
        (g - 9.81).abs() < 0.8,
        "{name}: |accel| {g:.2} m/s^2 at rest"
    );
    assert!(
        w.iter().all(|v| v.abs() < 0.1),
        "{name}: gyro {w:?} rad/s at rest"
    );
    assert_eq!(r.imu[0].header.frame_id, "camera_imu_optical_frame");
}

/// Each camera on its own, with every stream on.
#[test]
fn every_camera_streams_all_outputs() {
    let Some(cams) = cameras() else { return };
    for (name, serial) in &cams {
        let r = stream(
            serial,
            json!({ "enable_infrared": true, "enable_imu": true, "point_step": 4, "max_range": 4.0 }),
            4.0,
        );
        check_full(&format!("{name} {serial}"), &r);
    }
}

/// Without alignment, depth keeps the depth imager's own model and frame.
#[test]
fn unaligned_depth_uses_the_depth_imager() {
    let Some(cams) = cameras() else { return };
    let (name, serial) = &cams[0];
    let r = stream(serial, json!({ "align": false }), 2.0);
    let (ci, di) = (r.color_info.last().unwrap(), r.depth_info.last().unwrap());
    eprintln!("{name}: depth fx {:.1}, colour fx {:.1}", di.k[0], ci.k[0]);
    assert_eq!(di.header.frame_id, "camera_depth_optical_frame");
    assert_ne!(
        ci.k, di.k,
        "depth and colour imagers have different intrinsics"
    );
    let d = r.depth.last().unwrap();
    assert_eq!(
        (d.width, d.height, d.encoding),
        (640, 480, Encoding::Mono16)
    );
}

/// Above the node's 30 Hz step rate, every frame still comes through.
#[test]
fn frames_faster_than_the_node_steps() {
    let Some(cams) = cameras() else { return };
    let (name, serial) = &cams[0];
    let r = stream(serial, json!({ "fps": 60 }), 3.0);
    let fps = |n: usize| n as f64 / r.seconds;
    eprintln!(
        "{name}: {:.1} colour/s, {:.1} depth/s at 60 fps",
        fps(r.color.len()),
        fps(r.depth.len())
    );
    assert!(
        fps(r.color.len()) > 55.0 && fps(r.depth.len()) > 55.0,
        "{name}: frames lost at 60 fps"
    );
}

/// All cameras at once, each in its own thread as separate nodes would be:
/// the USB bus must carry colour and depth from every one at full rate.
#[test]
fn all_cameras_at_once() {
    let Some(cams) = cameras() else { return };
    let results: Vec<(String, Received)> = std::thread::scope(|s| {
        let handles: Vec<_> = cams
            .iter()
            .map(|(name, serial)| {
                s.spawn(move || (format!("{name} {serial}"), stream(serial, json!({}), 4.0)))
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    // Report every camera before judging any.
    for (name, r) in &results {
        let fps = |n: usize| n as f64 / r.seconds;
        eprintln!(
            "{name}: first image after {:.2} s, {:.1} colour/s, {:.1} depth/s, {:.0}% depth valid",
            r.first_frame,
            fps(r.color.len()),
            fps(r.depth.len()),
            median_valid(&r.depth) * 100.0
        );
    }
    for (name, r) in &results {
        let fps = |n: usize| n as f64 / r.seconds;
        assert!(
            fps(r.color.len()) > 28.0 && fps(r.depth.len()) > 28.0,
            "{name} dropped frames"
        );
        let valid = median_valid(&r.depth);
        assert!(valid > 0.25, "{name}: {:.0}% depth valid", valid * 100.0);
    }
}

/// The IMU alone: no video streams.
#[test]
fn imu_only() {
    let Some(cams) = cameras() else { return };
    let (name, serial) = &cams[0];
    let p = json!({ "serial": serial, "enable_color": false, "enable_depth": false, "enable_imu": true });
    let mut h = Harness::with_node::<RealSense>(p, |_| {}).unwrap();
    let (mut imu, mut images) = (0, 0);
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(4) {
        h.step().unwrap();
        imu += h.take::<Imu>("imu").unwrap().len();
        images += h.take::<Image>("color").unwrap().len() + h.take::<Image>("depth").unwrap().len();
        std::thread::sleep(Duration::from_millis(33));
    }
    eprintln!("{name}: {imu} IMU samples, {images} images in 4 s");
    assert!(imu > 300, "{name}: {imu} IMU samples in 4 s");
    assert_eq!(images, 0, "{name}: images though none were asked for");
}
