//! The MATLAB helper (`sdk/matlab`) and the example nodes in
//! `examples/process`, run by GNU Octave. Skipped when `octave-cli` is not on
//! `PATH`, unless `RURO_REQUIRE_OCTAVE` is set (as in CI's Octave job).

use std::path::{Path, PathBuf};

use ruro_msg::{Encoding, Image, Scalar};
use ruro_node::testing::Harness;
use ruro_nodes_process::{Manifest, Transport, register};
use serde_json::json;

fn examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/process")
}

fn have_octave() -> bool {
    let found = std::process::Command::new("octave-cli")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !found {
        assert!(
            std::env::var_os("RURO_REQUIRE_OCTAVE").is_none(),
            "octave-cli is not on PATH"
        );
        eprintln!("skipped: octave-cli is not on PATH");
    }
    found
}

fn load(file: &str) -> Manifest {
    Manifest::load(&examples().join(file)).unwrap()
}

fn gain(kind: &str, transport: Transport) {
    let mut m = load("gain-octave.node.yaml");
    m.descriptor.kind = kind.to_owned();
    m.transport = transport;
    register(m);
    let mut h = Harness::new(kind, json!({"k": 3.0})).unwrap();
    h.push("input", Scalar(1.0)).unwrap();
    h.push("input", Scalar(-2.0)).unwrap();
    h.step().unwrap();
    let out: Vec<f64> = h
        .take::<Scalar>("output")
        .unwrap()
        .iter()
        .map(|s| s.0)
        .collect();
    assert_eq!(out, [3.0, -6.0]);

    h.set_param("k", json!(0.5)).unwrap();
    h.push("input", Scalar(4.0)).unwrap();
    h.step().unwrap();
    assert_eq!(h.take::<Scalar>("output").unwrap()[0].0, 2.0);

    // A step with nothing queued sends nothing.
    h.step().unwrap();
    assert!(h.take::<Scalar>("output").unwrap().is_empty());

    h.set_estop(true);
    h.push("input", Scalar(4.0)).unwrap();
    h.step().unwrap();
    assert!(
        h.take::<Scalar>("output").unwrap().is_empty(),
        "held during the e-stop"
    );
}

#[test]
fn an_octave_node_over_stdio() {
    if have_octave() {
        gain("example/octave-gain-stdio", Transport::Stdio);
    }
}

#[test]
fn an_octave_node_over_tcp() {
    // As MATLAB nodes connect; needs Octave's instrument-control package.
    if have_octave() {
        gain("example/octave-gain-tcp", Transport::Tcp);
    }
}

#[test]
fn an_octave_node_processes_images() {
    if !have_octave() {
        return;
    }
    register(load("invert-octave.node.yaml"));
    let mut h = Harness::new("example/octave-invert", json!(null)).unwrap();
    let mut img = Image::new(3, 2, Encoding::Rgb8);
    for (i, b) in img.data.iter_mut().enumerate() {
        *b = (i * 13) as u8;
    }
    h.push("image", img.clone()).unwrap();
    h.step().unwrap();
    let back = h.take::<Image>("inverted").unwrap();
    assert_eq!(
        (back[0].width, back[0].height, back[0].encoding),
        (3, 2, Encoding::Rgb8)
    );
    let expected: Vec<u8> = img.data.iter().map(|b| 255 - b).collect();
    assert_eq!(
        back[0].data, expected,
        "pixel order survives the round trip"
    );
}

#[test]
fn the_matlab_manifest_parses() {
    let m = load("gain-matlab.node.yaml");
    assert_eq!(m.transport, Transport::Tcp);
    assert_eq!(m.command[0], "matlab");
}
