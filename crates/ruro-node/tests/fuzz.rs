//! Property tests: decoders and the graph parser must reject bad input with
//! an error, never a panic. Bytes from other processes and graph files from
//! users are untrusted.

use proptest::prelude::*;
use ruro_graph::Graph;
use ruro_msg::*;

/// Decodes `bytes` as `M`, both fresh and into an existing value.
fn decode_all<M: Message + Default>(bytes: &[u8]) {
    let _ = M::decode(bytes);
    let mut m = M::default();
    let _ = m.decode_into(bytes);
}

fn decode_every_std_type(bytes: &[u8]) {
    decode_all::<Scalar>(bytes);
    decode_all::<Text>(bytes);
    decode_all::<Vec3>(bytes);
    decode_all::<Pose>(bytes);
    decode_all::<Twist>(bytes);
    decode_all::<JointState>(bytes);
    decode_all::<Image>(bytes);
    decode_all::<PointCloud>(bytes);
    decode_all::<Transform>(bytes);
    decode_all::<Transforms>(bytes);
    decode_all::<Imu>(bytes);
    decode_all::<LaserScan>(bytes);
    decode_all::<Odometry>(bytes);
    decode_all::<CameraInfo>(bytes);
    decode_all::<Joy>(bytes);
    decode_all::<Flag>(bytes);
    decode_all::<Bytes>(bytes);
    decode_all::<JointCommand>(bytes);
    decode_all::<Clock>(bytes);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn decoders_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        decode_every_std_type(&bytes);
    }

    /// Image headers with arbitrary sizes, including ones whose pixel count overflows.
    #[test]
    fn image_headers_never_panic(w in any::<u32>(), h in any::<u32>(), enc in any::<u8>(), tail in proptest::collection::vec(any::<u8>(), 0..64)) {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&w.to_le_bytes());
        bytes.extend_from_slice(&h.to_le_bytes());
        bytes.push(enc);
        bytes.extend_from_slice(&tail);
        let ok = Image::decode(&bytes).is_ok();
        let expected = (enc as usize) < 3 && (w as u128) * (h as u128) * [3, 4, 1][(enc as usize).min(2)] == tail.len() as u128;
        prop_assert_eq!(ok, expected);
    }

    /// Valid encodings with trailing garbage or truncation must not panic either.
    #[test]
    fn mutated_encodings_never_panic(
        positions in proptest::collection::vec(any::<f64>(), 0..8),
        cut in 0usize..200,
        flip in any::<(usize, u8)>(),
    ) {
        let js = JointState { position: positions.clone(), velocity: positions, effort: vec![] };
        let mut bytes = js.to_bytes();
        if !bytes.is_empty() {
            let i = flip.0 % bytes.len();
            bytes[i] ^= flip.1;
        }
        bytes.truncate(cut.min(bytes.len()));
        decode_every_std_type(&bytes);
    }

    #[test]
    fn graph_parser_never_panics(text in "\\PC{0,256}") {
        let _ = Graph::from_yaml(&text);
    }

    /// YAML-shaped input: graph keywords, punctuation and indentation in
    /// random order reach much deeper into the parser than arbitrary text.
    #[test]
    fn yaml_like_graphs_never_panic(tokens in proptest::collection::vec(
        prop_oneof![
            Just("name: g\n"), Just("nodes:\n"), Just("wires:\n"), Just("groups:\n"),
            Just("  a: { type: std/sine, rate: 10 }\n"), Just("  - a.value -> b.input\n"),
            Just("  - { from: a.x, to: b.y, depth: 4 }\n"), Just("    realtime: { priority: 80, cpus: [1] }\n"),
            Just("clock: sim\n"), Just("params: {"), Just("}"), Just("["), Just("]"), Just("- "),
            Just(": "), Just("\n"), Just("  "), Just("&a "), Just("*a "), Just("!!str "),
            Just("'"), Just("\""), Just("#"), Just("? "), Just("|\n"), Just("1e999"), Just("-1"),
        ],
        0..40,
    )) {
        let text: String = tokens.concat();
        let _ = Graph::from_yaml(&text);
    }
}

#[test]
fn deeply_nested_yaml_is_rejected_without_overflowing_the_stack() {
    for depth in [100usize, 10_000] {
        let flow = format!("name: x\nnodes: {}{}", "[".repeat(depth), "]".repeat(depth));
        // Run on a thread with a modest stack so unbounded recursion would crash the test.
        let r = std::thread::Builder::new()
            .stack_size(1 << 20)
            .spawn(move || Graph::from_yaml(&flow).is_err())
            .unwrap()
            .join();
        assert!(r.is_ok_and(|err| err), "nesting depth {depth}");
    }
}
