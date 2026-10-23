//! Process nodes against a real program: `tests/fixtures/node.py`, which
//! speaks the protocol in plain Python.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ruro_msg::{Encoding, Image, Scalar, Text};
use ruro_node::testing::Harness;
use ruro_nodes_process::{Manifest, register};
use serde_json::json;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Registers `node.py` as `test/<name>` with `extra` appended to its
/// manifest.
fn node(name: &str, extra: &str) -> String {
    let kind = format!("test/{name}");
    let text = format!(
        "kind: {kind}
command: [python3, -I, node.py]
inputs: {{ input: std/Scalar, image: std/Image }}
outputs: {{ output: std/Scalar, info: std/Text, inverted: std/Image }}
params:
  k: {{ default: 2.0, min: -100, max: 100 }}
  mode: {{ default: scale }}
  locked: {{ default: 1 }}
start_timeout: 20
{extra}"
    );
    register(Manifest::parse(&text, &fixtures(), None).unwrap());
    kind
}

fn info(h: &mut Harness) -> String {
    let texts = h.take::<Text>("info").unwrap();
    texts.last().expect("an info message").0.clone()
}

fn pid_in(info: &str) -> i32 {
    let p = info.split_whitespace().next().unwrap();
    p.trim_start_matches("pid=").parse().unwrap()
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn wait_gone(pid: i32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn a_program_receives_inputs_and_parameters_and_sends_outputs() {
    let kind = node("scale", "");
    let mut h = Harness::new(&kind, json!({"k": 3.0})).unwrap();
    h.push("input", Scalar(1.0)).unwrap();
    h.push("input", Scalar(2.5)).unwrap();
    h.step().unwrap();
    let out: Vec<f64> = h
        .take::<Scalar>("output")
        .unwrap()
        .iter()
        .map(|s| s.0)
        .collect();
    assert_eq!(out, [3.0, 7.5]);

    // A change reaches the program; one it refuses is reported.
    h.set_param("k", json!(-1.0)).unwrap();
    h.push("input", Scalar(4.0)).unwrap();
    h.step().unwrap();
    assert_eq!(h.take::<Scalar>("output").unwrap()[0].0, -4.0);
    let e = format!("{:#}", h.set_param("locked", json!(2)).unwrap_err());
    assert!(e.contains("locked cannot change"), "{e}");
    let pid = pid_in(&info(&mut h));
    drop(h);
    assert!(wait_gone(pid), "the program is gone after the node stops");
}

#[test]
fn images_cross_as_base64() {
    let kind = node("image", "");
    let mut h = Harness::new(&kind, json!(null)).unwrap();
    let mut img = Image::new(4, 2, Encoding::Rgb8);
    for (i, b) in img.data.iter_mut().enumerate() {
        *b = i as u8;
    }
    h.push("image", img.clone()).unwrap();
    h.step().unwrap();
    let back = h.take::<Image>("inverted").unwrap();
    assert_eq!((back[0].width, back[0].height), (4, 2));
    let expected: Vec<u8> = img.data.iter().map(|b| 255 - b).collect();
    assert_eq!(back[0].data, expected);
}

#[test]
fn the_estop_reaches_the_program() {
    let kind = node("estop", "");
    let mut h = Harness::new(&kind, json!(null)).unwrap();
    h.step().unwrap();
    assert!(info(&mut h).contains("estops=0 estopped=False"));
    h.set_estop(true);
    h.step().unwrap();
    assert!(info(&mut h).contains("estops=1 estopped=True"));
}

#[test]
fn over_tcp_too() {
    let kind = node("tcp", "transport: tcp\n");
    let mut h = Harness::new(&kind, json!(null)).unwrap();
    h.push("input", Scalar(5.0)).unwrap();
    h.step().unwrap();
    assert_eq!(h.take::<Scalar>("output").unwrap()[0].0, 10.0);
}

#[test]
fn a_program_that_dies_is_started_again() {
    let mark = std::env::temp_dir().join(format!("ruro-process-crash-{}", std::process::id()));
    let _ = std::fs::remove_file(&mark);
    let kind = node(
        "crash",
        &format!("env: {{ TEST_MARK: \"{}\" }}\n", mark.display()),
    );
    let mut h = Harness::new(&kind, json!({"mode": "crash_once"})).unwrap();
    let e = format!("{:#}", h.step().unwrap_err());
    assert!(
        e.contains("exited (status 3)") && e.contains("restarting it in"),
        "{e}"
    );
    // Back after the backoff.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        std::thread::sleep(Duration::from_millis(100));
        match h.step() {
            Ok(()) => break,
            Err(e) => assert!(Instant::now() < deadline, "not restarted: {e:#}"),
        }
    }
    assert!(info(&mut h).contains("mode=crash_once"));
    let _ = std::fs::remove_file(&mark);
}

#[test]
fn a_program_that_hangs_is_killed() {
    let kind = node("hang", "step_timeout: 1\n");
    let mut h = Harness::new(&kind, json!(null)).unwrap();
    h.step().unwrap();
    let pid = pid_in(&info(&mut h));
    h.set_param("mode", json!("hang")).unwrap();
    let start = Instant::now();
    let e = format!("{:#}", h.step().unwrap_err());
    assert!(e.contains("did not answer within 1.0 s"), "{e}");
    assert!(start.elapsed() < Duration::from_secs(10));
    assert!(wait_gone(pid), "the hung program was killed");
}

#[test]
fn errors_and_bad_lines_are_reported() {
    let kind = node("complain", "");
    let mut h = Harness::new(&kind, json!({"mode": "complain", "k": 1.0})).unwrap();
    h.push("input", Scalar(1.0)).unwrap();
    let e = format!("{:#}", h.step().unwrap_err());
    assert!(e.contains("something is off"), "{e}");
    // Its outputs still went out.
    assert_eq!(h.take::<Scalar>("output").unwrap().len(), 1);

    let kind = node("garble", "");
    let mut h = Harness::new(&kind, json!({"mode": "garble"})).unwrap();
    let e = format!("{:#}", h.step().unwrap_err());
    assert!(e.contains("not JSON"), "{e}");

    let kind = node("refuse", "");
    let mut h = Harness::new(&kind, json!({"mode": "refuse"})).unwrap();
    let e = format!("{:#}", h.step().unwrap_err());
    assert!(e.contains("refusing to start"), "{e}");
}

#[test]
fn a_program_on_another_protocol_is_refused() {
    let kind = node("protocol", "env: { TEST_PROTOCOL: \"7\" }\n");
    let mut h = Harness::new(&kind, json!(null)).unwrap();
    let e = format!("{:#}", h.step().unwrap_err());
    assert!(e.contains("does not speak protocol 1"), "{e}");
}

#[test]
fn a_missing_program_is_reported() {
    let text = "kind: test/missing\ncommand: [./no-such-program]\n";
    register(Manifest::parse(text, &fixtures(), None).unwrap());
    let mut h = Harness::new("test/missing", json!(null)).unwrap();
    let e = format!("{:#}", h.step().unwrap_err());
    assert!(e.contains("no-such-program"), "{e}");
}

#[test]
fn manifests_in_a_directory_are_registered() {
    let dir = std::env::temp_dir().join(format!("ruro-process-dir-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.node.yaml"), "kind: dir/a\ncommand: [true]\n").unwrap();
    std::fs::write(dir.join("b.node.yml"), "kind: dir/b\ncommand: [true]\n").unwrap();
    std::fs::write(dir.join("graph.yaml"), "name: not a manifest\n").unwrap();
    let kinds = ruro_nodes_process::register_path(&dir).unwrap();
    assert_eq!(kinds, ["dir/a", "dir/b"]);
    let d = ruro_node::descriptor("dir/a").unwrap();
    assert!(d.source.unwrap().ends_with("a.node.yaml"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "a measurement: cargo test -p ruro-nodes-process --release -- --ignored --nocapture"]
fn step_round_trip() {
    let kind = node("bench", "");
    // Images are sent back unchanged: this measures the protocol, not Python.
    let mut h = Harness::new(&kind, json!({"mode": "echo"})).unwrap();
    h.step().unwrap();
    for (label, image) in [
        ("a scalar", None),
        (
            "a 640x480 RGB image",
            Some(Image::new(640, 480, Encoding::Rgb8)),
        ),
    ] {
        let n = if image.is_some() { 50 } else { 2000 };
        let start = Instant::now();
        for _ in 0..n {
            match &image {
                None => h.push("input", Scalar(1.0)).unwrap(),
                Some(img) => h.push("image", img.clone()).unwrap(),
            }
            h.step().unwrap();
            let _ = h.take::<Scalar>("output");
            let _ = h.take::<Image>("inverted");
        }
        let per = start.elapsed() / n;
        println!("{label}: {per:?} per step (Python program)");
    }
}
