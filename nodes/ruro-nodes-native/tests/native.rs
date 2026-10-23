//! C and C++ plugins: `tests/fixtures/*.c` and `*.cpp`, compiled here with
//! `$RURO_TEST_CC` and `$RURO_TEST_CXX` (default `cc` and `c++`) against
//! `sdk/include`, loaded, and run through the test harness.
//!
//! Skipped, with a message, when the compiler builds for another
//! architecture than this test's (the emulated aarch64 CI job sets the cross
//! compilers instead), unless `RURO_REQUIRE_PLUGINS` is set.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use ruro_msg::*;
use ruro_node::testing::Harness;
use serde_json::json;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn out_dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("ruro-native-tests-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The ELF machine of a library, or None if it is not ELF (macOS).
fn elf_machine(path: &Path) -> Option<u16> {
    let bytes = std::fs::read(path).ok()?;
    (bytes.get(..4)? == b"\x7fELF").then(|| u16::from_le_bytes([bytes[18], bytes[19]]))
}

fn this_machine() -> Option<u16> {
    match std::env::consts::ARCH {
        "x86_64" => Some(62),
        "aarch64" => Some(183),
        _ => None,
    }
}

/// Compiles `fixture` into a shared library, or returns None when it would
/// not load here.
fn build(fixture: &str, cxx: bool, defines: &[&str], name: &str) -> Option<PathBuf> {
    let (var, default, std) = if cxx {
        ("RURO_TEST_CXX", "c++", "-std=c++17")
    } else {
        ("RURO_TEST_CC", "cc", "-std=c11")
    };
    let compiler = std::env::var(var).unwrap_or_else(|_| default.to_owned());
    let out = out_dir().join(format!("lib{name}.so"));
    let mut cmd = Command::new(&compiler);
    cmd.args([
        std,
        "-Wall",
        "-Wextra",
        "-Wpedantic",
        "-Werror",
        "-shared",
        "-fPIC",
        "-O1",
    ])
    .arg("-I")
    .arg(root().join("sdk/include"))
    .args(defines)
    .arg("-o")
    .arg(&out)
    .arg(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(fixture),
    );
    let result = cmd
        .output()
        .unwrap_or_else(|e| panic!("running {compiler}: {e}"));
    assert!(
        result.status.success(),
        "{compiler} failed on {fixture}:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    if let (Some(lib), Some(here)) = (elf_machine(&out), this_machine())
        && lib != here
    {
        assert!(
            std::env::var_os("RURO_REQUIRE_PLUGINS").is_none(),
            "{compiler} builds for ELF machine {lib}, this test runs on {here}"
        );
        eprintln!("skipped: {compiler} builds for another architecture (set {var})");
        return None;
    }
    Some(out)
}

/// Builds and loads the two test plugins once.
fn plugins() -> bool {
    static LOADED: OnceLock<bool> = OnceLock::new();
    *LOADED.get_or_init(|| {
        let (Some(c), Some(cpp)) = (
            build("ctest.c", false, &[], "ctest"),
            build("cpptest.cpp", true, &[], "cpptest"),
        ) else {
            return false;
        };
        let mut kinds = ruro_nodes_native::load(&c).unwrap();
        kinds.extend(ruro_nodes_native::load(&cpp).unwrap());
        assert_eq!(
            kinds,
            [
                "ctest/echo",
                "ctest/probe",
                "cpptest/gain",
                "cpptest/invert",
                "cpptest/thrower",
                "cpptest/mistyped"
            ]
        );
        true
    })
}

fn header(n: u64) -> Header {
    Header {
        stamp_ns: 1_000 + n,
        frame_id: format!("frame{n}"),
    }
}

fn vec3(b: f64) -> Vec3 {
    Vec3 {
        x: b + 0.1,
        y: b + 0.2,
        z: b + 0.3,
    }
}

fn quat(b: f64) -> Quat {
    Quat {
        x: b + 0.4,
        y: b + 0.5,
        z: b + 0.6,
        w: b + 0.7,
    }
}

fn cov(b: f64) -> [f64; 9] {
    std::array::from_fn(|i| b + i as f64)
}

fn transform(n: u64) -> Transform {
    Transform {
        header: header(n),
        child_frame_id: format!("child{n}"),
        translation: vec3(n as f64),
        rotation: quat(n as f64 * 2.0),
    }
}

/// Pushes `msg` into the echo node's port `port`, steps, and checks that the
/// same message comes back.
fn echo<T: Message + PartialEq + std::fmt::Debug + Clone>(h: &mut Harness, port: &str, msg: T) {
    h.push(port, msg.clone()).unwrap();
    h.step().unwrap();
    let back = h.take::<T>(port).unwrap();
    assert_eq!(back, [msg], "{port}");
}

#[test]
fn every_standard_type_crosses_into_c_and_back() {
    if !plugins() {
        return;
    }
    let mut h = Harness::new("ctest/echo", json!(null)).unwrap();
    echo(&mut h, "scalar", Scalar(-1.25));
    echo(&mut h, "text", Text("héllo, C".into()));
    echo(&mut h, "flag", Flag(true));
    echo(&mut h, "bytes", Bytes(vec![0, 1, 254, 255]));
    echo(&mut h, "vec3", vec3(1.0));
    echo(
        &mut h,
        "pose",
        Pose {
            position: vec3(2.0),
            orientation: quat(3.0),
        },
    );
    echo(
        &mut h,
        "twist",
        Twist {
            linear: vec3(4.0),
            angular: vec3(5.0),
        },
    );
    echo(
        &mut h,
        "joint_state",
        JointState {
            position: vec![1.0, 2.0],
            velocity: vec![3.0],
            effort: vec![],
        },
    );
    echo(
        &mut h,
        "joint_command",
        JointCommand {
            header: header(6),
            position: vec![6.5],
            velocity: vec![],
            effort: vec![7.5, 8.5, 9.5],
        },
    );
    echo(&mut h, "transform", transform(7));
    echo(
        &mut h,
        "transforms",
        Transforms(vec![transform(8), transform(9), transform(10)]),
    );
    echo(
        &mut h,
        "imu",
        Imu {
            header: header(11),
            orientation: quat(11.0),
            orientation_covariance: cov(100.0),
            angular_velocity: vec3(12.0),
            angular_velocity_covariance: cov(200.0),
            linear_acceleration: vec3(13.0),
            linear_acceleration_covariance: cov(300.0),
        },
    );
    echo(
        &mut h,
        "laser_scan",
        LaserScan {
            header: header(14),
            angle_min: -1.5,
            angle_max: 1.5,
            angle_increment: 0.01,
            time_increment: 0.001,
            scan_time: 0.1,
            range_min: 0.2,
            range_max: 30.0,
            ranges: vec![1.0, 2.0, 3.0],
            intensities: vec![9.0],
        },
    );
    echo(
        &mut h,
        "odometry",
        Odometry {
            header: header(15),
            child_frame_id: "base".into(),
            pose: Pose {
                position: vec3(15.0),
                orientation: quat(16.0),
            },
            twist: Twist {
                linear: vec3(17.0),
                angular: vec3(18.0),
            },
        },
    );
    echo(
        &mut h,
        "camera_info",
        CameraInfo {
            header: header(19),
            width: 640,
            height: 480,
            distortion_model: "plumb_bob".into(),
            d: vec![0.1, -0.2, 0.0, 0.0, 0.05],
            k: cov(400.0),
            r: cov(500.0),
            p: std::array::from_fn(|i| 600.0 + i as f64),
        },
    );
    echo(
        &mut h,
        "joy",
        Joy {
            header: header(20),
            axes: vec![0.5, -0.5],
            buttons: vec![1, 0, -3],
        },
    );
    let mut img = Image::new(3, 2, Encoding::Rgba8);
    img.data
        .iter_mut()
        .enumerate()
        .for_each(|(i, b)| *b = i as u8);
    echo(&mut h, "image", img);
    let mut deep = Image::new(2, 2, Encoding::Mono16);
    deep.data
        .iter_mut()
        .enumerate()
        .for_each(|(i, b)| *b = 200 + i as u8);
    echo(&mut h, "image", deep);
    echo(
        &mut h,
        "point_cloud",
        PointCloud {
            points: vec![[1.0, 2.0, 3.0], [-4.0, 5.5, 6.25]],
        },
    );
    // Empty values, where pointers may be NULL.
    echo(&mut h, "text", Text(String::new()));
    echo(&mut h, "transforms", Transforms(vec![]));
    echo(&mut h, "point_cloud", PointCloud::default());
}

#[test]
fn a_c_node_gets_its_parameters_timing_and_the_estop() {
    if !plugins() {
        return;
    }
    let d = ruro_node::descriptor("ctest/probe").unwrap();
    assert_eq!(d.default_rate, Some(10.0));
    assert!(d.source.unwrap().ends_with("libctest.so"));
    let kinds: Vec<_> = d.params.iter().map(|p| format!("{:?}", p.kind)).collect();
    assert_eq!(kinds, ["Int", "Float", "Bool", "String"]);
    assert_eq!(d.params[0].max, Some(100.0));

    let mut h = Harness::new("ctest/probe", json!({"count": 7, "mode": "quiet"})).unwrap();
    h.push("input", Scalar(1.0)).unwrap();
    h.push("input", Scalar(2.0)).unwrap();
    h.step().unwrap();
    let report = &h.take::<Text>("report").unwrap()[0].0;
    assert!(
        report.starts_with("id=test count=7 gain=1.5 enabled=1 mode=quiet sum=3 seq=2 steps=0"),
        "{report}"
    );

    h.set_param("gain", json!(4.0)).unwrap();
    let e = format!("{:#}", h.set_param("mode", json!("loud")).unwrap_err());
    assert!(e.contains("only gain and count change"), "{e}");
    std::thread::sleep(std::time::Duration::from_millis(5));
    h.set_estop(true);
    h.step().unwrap();
    let report = &h.take::<Text>("report").unwrap()[0].0;
    assert!(report.contains("gain=4 "), "{report}");
    assert!(
        report.contains("dt_positive=1 estopped=1 estops=1"),
        "{report}"
    );

    // A failing step reports the plugin's error.
    h.set_param("gain", json!(-1.0)).unwrap();
    let e = format!("{:#}", h.step().unwrap_err());
    assert_eq!(e, "gain is negative");
}

#[test]
fn failures_in_c_are_reported() {
    if !plugins() {
        return;
    }
    let e = Harness::new("ctest/probe", json!({"mode": "fail-create"}))
        .err()
        .unwrap();
    assert!(format!("{e:#}").contains("refusing to be created"), "{e:#}");

    let mut h = Harness::new("ctest/probe", json!({"mode": "fail-start"})).unwrap();
    let e = format!("{:#}", h.step().unwrap_err());
    assert_eq!(e, "refusing to start");

    // A message the host cannot accept is refused with the reason.
    let mut h = Harness::new("ctest/probe", json!({"mode": "bad-image"})).unwrap();
    let e = format!("{:#}", h.step().unwrap_err());
    assert!(
        e.contains("image data has 3 bytes") && e.contains("has 12"),
        "{e}"
    );
}

#[test]
fn a_cpp_node_runs_and_takes_parameter_changes() {
    if !plugins() {
        return;
    }
    let mut h = Harness::new("cpptest/gain", json!({"k": 3.0})).unwrap();
    h.push("input", Scalar(2.0)).unwrap();
    h.push("input", Scalar(-1.0)).unwrap();
    h.step().unwrap();
    let out: Vec<f64> = h
        .take::<Scalar>("output")
        .unwrap()
        .iter()
        .map(|s| s.0)
        .collect();
    assert_eq!(out, [6.0, -3.0]);
    h.set_param("k", json!(0.5)).unwrap();
    h.push("input", Scalar(4.0)).unwrap();
    h.step().unwrap();
    assert_eq!(h.take::<Scalar>("output").unwrap()[0].0, 2.0);

    let mut h = Harness::new("cpptest/invert", json!(null)).unwrap();
    let mut img = Image::new(4, 3, Encoding::Rgb8);
    img.data
        .iter_mut()
        .enumerate()
        .for_each(|(i, b)| *b = (i * 7) as u8);
    h.push("image", img.clone()).unwrap();
    h.step().unwrap();
    let back = h.take::<Image>("inverted").unwrap();
    let expected: Vec<u8> = img.data.iter().map(|b| 255 - b).collect();
    assert_eq!(back[0].data, expected);
    assert_eq!((back[0].width, back[0].height), (4, 3));
}

#[test]
fn cpp_exceptions_become_errors() {
    if !plugins() {
        return;
    }
    let e = Harness::new("cpptest/thrower", json!({"in_constructor": true}))
        .err()
        .unwrap();
    assert!(
        format!("{e:#}").ends_with(": thrown in the constructor"),
        "{e:#}"
    );

    let mut h = Harness::new("cpptest/thrower", json!({"name": "t"})).unwrap();
    h.step().unwrap();
    assert_eq!(h.take::<Text>("out").unwrap()[0].0, "t step 0");
    assert_eq!(format!("{:#}", h.step().unwrap_err()), "thrown in step 1");
    let e = format!("{:#}", h.step().unwrap_err());
    assert!(e.contains("not a std::exception"), "{e}");
    // It keeps running.
    h.step().unwrap();
    assert_eq!(h.take::<Text>("out").unwrap()[0].0, "t step 3");

    let e = format!("{:#}", h.set_param("name", json!("u")).unwrap_err());
    assert!(e.contains("does not take changes to `name`"), "{e}");

    let e = Harness::new("cpptest/mistyped", json!(null)).err().unwrap();
    assert!(
        format!("{e:#}").contains("input `image` is std/Image, not std/Scalar"),
        "{e:#}"
    );
}

#[test]
fn plugins_built_against_another_ruro_h_are_refused() {
    let Some(abi) = build("refused.c", false, &["-DCASE=1"], "refused_abi") else {
        return;
    };
    let e = format!("{:#}", ruro_nodes_native::load(&abi).unwrap_err());
    assert!(
        e.contains("ABI version 2") && e.contains("version 1"),
        "{e}"
    );

    let layout = build("refused.c", false, &["-DCASE=2"], "refused_layout").unwrap();
    let e = format!("{:#}", ruro_nodes_native::load(&layout).unwrap_err());
    assert!(e.contains("sizeof(ruro_imu)"), "{e}");

    let none = build("refused.c", false, &["-DCASE=3"], "refused_entry").unwrap();
    let e = format!("{:#}", ruro_nodes_native::load(&none).unwrap_err());
    assert!(e.contains("does not export ruro_plugin_init"), "{e}");

    let e = format!(
        "{:#}",
        ruro_nodes_native::load(Path::new("/nonexistent/lib.so")).unwrap_err()
    );
    assert!(e.contains("/nonexistent/lib.so"), "{e}");
}

#[test]
#[ignore = "a measurement: cargo test -p ruro-nodes-native --release -- --ignored --nocapture"]
fn step_cost() {
    if !plugins() {
        return;
    }
    ruro_nodes_std::link();
    for kind in ["cpptest/gain", "std/gain"] {
        let mut h = Harness::new(kind, json!({"k": 2.0})).unwrap();
        let n = 200_000;
        let start = std::time::Instant::now();
        for i in 0..n {
            h.push("input", Scalar(i as f64)).unwrap();
            h.step().unwrap();
            let _ = h.take::<Scalar>("output");
        }
        println!("{kind}: {:?} per step with a scalar", start.elapsed() / n);
    }
    let mut h = Harness::new("cpptest/invert", json!(null)).unwrap();
    let img = Image::new(640, 480, Encoding::Rgb8);
    let n = 500;
    let start = std::time::Instant::now();
    for _ in 0..n {
        h.push("image", img.clone()).unwrap();
        h.step().unwrap();
        let _ = h.take::<Image>("inverted");
    }
    println!(
        "cpptest/invert: {:?} per 640x480 RGB image (inverting it included)",
        start.elapsed() / n
    );
}

/// Builds `examples/native` as its Makefile does.
fn build_examples() -> Option<PathBuf> {
    let cc = std::env::var("RURO_TEST_CC").unwrap_or_else(|_| "cc".to_owned());
    let cxx = std::env::var("RURO_TEST_CXX").unwrap_or_else(|_| "c++".to_owned());
    let dir = root().join("examples/native");
    let out = out_dir();
    let run = |cmd: &mut Command| {
        let r = cmd.output().unwrap();
        assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    };
    let include = root().join("sdk/include");
    run(Command::new(&cc)
        .args([
            "-std=c11",
            "-fPIC",
            "-fvisibility=hidden",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-c",
            "-I",
        ])
        .arg(&include)
        .arg("-o")
        .arg(out.join("lowpass.o"))
        .arg(dir.join("lowpass.c")));
    run(Command::new(&cxx)
        .args([
            "-std=c++17",
            "-fPIC",
            "-fvisibility=hidden",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-c",
            "-I",
        ])
        .arg(&include)
        .arg("-o")
        .arg(out.join("threshold.o"))
        .arg(dir.join("threshold.cpp")));
    let lib = out.join("libexample_nodes.so");
    run(Command::new(&cxx)
        .arg("-shared")
        .arg("-o")
        .arg(&lib)
        .arg(out.join("lowpass.o"))
        .arg(out.join("threshold.o")));
    match (elf_machine(&lib), this_machine()) {
        (Some(l), Some(h)) if l != h => {
            assert!(std::env::var_os("RURO_REQUIRE_PLUGINS").is_none());
            eprintln!("skipped: the compilers build for another architecture");
            None
        }
        _ => Some(lib),
    }
}

#[test]
fn the_examples_build_and_run() {
    let Some(lib) = build_examples() else {
        return;
    };
    let kinds = ruro_nodes_native::load(&lib).unwrap();
    assert_eq!(kinds, ["example/c-lowpass", "example/cpp-threshold"]);

    let mut h = Harness::new("example/c-lowpass", json!({"alpha": 0.5})).unwrap();
    for v in [0.0, 10.0, 10.0] {
        h.push("input", Scalar(v)).unwrap();
    }
    h.step().unwrap();
    let out: Vec<f64> = h
        .take::<Scalar>("output")
        .unwrap()
        .iter()
        .map(|s| s.0)
        .collect();
    assert_eq!(out, [0.0, 5.0, 7.5]);
    h.set_param("alpha", json!(1.0)).unwrap();
    h.push("input", Scalar(20.0)).unwrap();
    h.step().unwrap();
    assert_eq!(h.take::<Scalar>("output").unwrap()[0].0, 20.0);

    let mut h = Harness::new("example/cpp-threshold", json!({"level": 100})).unwrap();
    let mut img = Image::new(2, 1, Encoding::Rgb8);
    img.data.copy_from_slice(&[200, 200, 200, 50, 50, 50]);
    h.push("image", img).unwrap();
    h.step().unwrap();
    let mask = h.take::<Image>("mask").unwrap();
    assert_eq!(
        (mask[0].encoding, mask[0].data.clone()),
        (Encoding::Mono8, vec![255, 0])
    );
    h.set_param("level", json!(250)).unwrap();
    let mut img = Image::new(1, 1, Encoding::Mono8);
    img.data[0] = 200;
    h.push("image", img).unwrap();
    h.step().unwrap();
    assert_eq!(h.take::<Image>("mask").unwrap()[0].data, [0]);
}
