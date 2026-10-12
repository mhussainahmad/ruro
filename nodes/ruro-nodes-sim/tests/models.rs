//! The example SO-100 model parses and has the LeRobot joint layout.

use ruro_urdf::Robot;

pub fn so100_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/models/so100.xml")
}

#[test]
fn so100_model_has_six_joints_in_lerobot_order() {
    let robot = Robot::from_file(so100_path()).unwrap();
    let names: Vec<&str> = robot.active_joints().map(|j| j.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "shoulder_pan",
            "shoulder_lift",
            "elbow_flex",
            "wrist_flex",
            "wrist_roll",
            "gripper"
        ]
    );
    // Straight up at zero: the hand sits 0.05 + 0.04 + 0.11 + 0.13 + 0.05 m above the floor.
    let fk = robot.forward_kinematics(&[0.0; 6]).unwrap();
    let hand = fk.link_pose("hand").unwrap().translation;
    assert!((hand[2] - 0.38).abs() < 1e-12, "{hand:?}");
}
