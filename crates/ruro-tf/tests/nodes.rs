use ruro::msg::{Transform, Transforms};
use ruro::node::testing::Harness;
use ruro_tf::Isometry;
use serde_json::json;

#[test]
fn publisher_emits_static_transform_from_params() {
    ruro_tf::link();
    let mut h = Harness::new(
        "tf/publisher",
        json!({ "parent": "base_link", "child": "lidar", "x": 0.3, "z": 0.8, "yaw": 1.0 }),
    )
    .unwrap();
    h.step().unwrap();
    let out = h.take::<Transforms>("tf_static").unwrap();
    let t = &out[0].0[0];
    assert_eq!(t.header.frame_id, "base_link");
    assert_eq!(t.child_frame_id, "lidar");
    assert_eq!(t.header.stamp_ns, 0, "static transforms are stamped zero");
    let iso = Isometry::from_msg(t);
    assert!(iso.approx_eq(
        &Isometry::from_rpy(0.0, 0.0, 1.0).with_translation(0.3, 0.0, 0.8),
        1e-9
    ));

    h.set_param("x", json!(-1.0)).unwrap();
    h.step().unwrap();
    let t = &h.take::<Transforms>("tf_static").unwrap()[0].0[0];
    assert_eq!(t.translation.x, -1.0);
}

#[test]
fn lookup_node_chains_static_and_dynamic_streams() {
    ruro_tf::link();
    let mut mount = Harness::new(
        "tf/publisher",
        json!({ "parent": "base", "child": "cam", "x": 1.0 }),
    )
    .unwrap();
    mount.step().unwrap();
    let statics = mount.take::<Transforms>("tf_static").unwrap();

    let mut h = Harness::new("tf/lookup", json!({ "target": "odom", "source": "cam" })).unwrap();
    // Nothing known yet: no output, no error.
    h.step().unwrap();
    assert!(h.take::<Transform>("transform").unwrap().is_empty());

    h.push("tf_static", statics[0].clone()).unwrap();
    let odom = |stamp, x| Transform {
        header: ruro::msg::Header::new("odom", stamp),
        child_frame_id: "base".into(),
        translation: Isometry::from_translation(x, 0.0, 0.0).vec3(),
        ..Default::default()
    };
    h.push("tf", Transforms(vec![odom(10, 2.0), odom(20, 4.0)]))
        .unwrap();
    h.step().unwrap();
    let out = h.take::<Transform>("transform").unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].header.stamp_ns, 20);
    assert_eq!(out[0].header.frame_id, "odom");
    assert_eq!(out[0].child_frame_id, "cam");
    assert!((out[0].translation.x - 5.0).abs() < 1e-9);
}
