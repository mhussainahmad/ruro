use ruro::graph::ParamKind;

#[test]
fn std_nodes_are_registered() {
    ruro_nodes_std::link();
    let reg = ruro::node::registry();
    let kinds: Vec<&str> = reg.iter().map(|d| d.kind.as_str()).collect();
    for kind in [
        "std/blur",
        "std/fake-arm",
        "std/fake-camera",
        "std/gain",
        "std/grayscale",
        "std/image-stats",
        "std/pid",
        "std/sine",
    ] {
        assert!(kinds.contains(&kind), "{kind} missing from {kinds:?}");
    }
}

#[test]
fn descriptors_carry_types_params_and_docs() {
    let reg = ruro::node::registry();
    let cam = reg.get("std/fake-camera").unwrap();
    assert_eq!(cam.default_rate, Some(30.0));
    assert_eq!(cam.outputs[0].ty, "std/Image");
    let width = cam.param("width").unwrap();
    assert_eq!(width.kind, ParamKind::Int);
    assert_eq!(width.default, serde_json::json!(640));
    assert_eq!((width.min, width.max), (Some(16.0), Some(3840.0)));
    assert_eq!(width.doc, "Frame width in pixels.");
    assert!(cam.doc.starts_with("Synthetic camera"));

    let pid = reg.get("std/pid").unwrap();
    assert_eq!(
        pid.inputs
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        ["setpoint", "measured"]
    );
    assert_eq!(pid.param("kp").unwrap().kind, ParamKind::Float);
}

#[test]
fn registry_carries_schema_hashes_of_port_types() {
    ruro_nodes_std::link();
    let reg = ruro::node::registry();
    let hash = reg.schema_hash("std/Image").expect("image schema hash");
    assert!(hash.starts_with("RS1_"));
    assert_eq!(
        reg.schema_hash("std/Scalar"),
        Some(
            <ruro::msg::Scalar as ruro::msg::Message>::message_schema()
                .hash
                .as_str()
        )
    );
    let json = serde_json::to_string(&reg).unwrap();
    let back: ruro::graph::Registry = serde_json::from_str(&json).unwrap();
    assert!(back.schema_mismatches(&reg).is_empty());
}
