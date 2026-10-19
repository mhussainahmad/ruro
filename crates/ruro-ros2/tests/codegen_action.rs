//! An action package generated the way users generate theirs:
//! `action_tutorials_interfaces` (copied from ROS 2 Jazzy, see
//! `fixtures/action_tutorials_interfaces/NOTICE.md`). The checked-in output
//! compiles, matches the generator, and carries the hashes real ROS 2 nodes
//! announce.

use std::collections::BTreeMap;
use std::path::Path;

#[rustfmt::skip]
#[path = "fixtures/action_tutorials_interfaces.rs"]
mod tutorials;

use ruro_ros2::{RosAction, RosMessage, RosService};
use tutorials::action_tutorials_interfaces::action::{
    Fibonacci, Fibonacci_Feedback, Fibonacci_Goal,
};

fn generate() -> String {
    let dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/action_tutorials_interfaces");
    let mut interfaces = ruro_ros2::bundled_interfaces();
    interfaces.load_package(&dir).unwrap();
    let externs: BTreeMap<String, String> = ruro_ros2::bundled_packages()
        .into_iter()
        .map(|p| (p.clone(), format!("ruro_ros2::msgs::{p}")))
        .collect();
    ruro_ros2::codegen::generate(
        &interfaces,
        &ruro_ros2::codegen::Options {
            packages: vec!["action_tutorials_interfaces".into()],
            root: "crate::tutorials".into(),
            runtime: "ruro_ros2".into(),
            externs,
        },
    )
    .unwrap()
}

#[test]
fn fixture_is_current() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/action_tutorials_interfaces.rs");
    let code = generate();
    if std::env::var_os("RURO_REGEN").is_some() {
        std::fs::write(&path, &code).unwrap();
        return;
    }
    assert!(
        std::fs::read_to_string(&path).unwrap() == code,
        "fixture is stale; regenerate with RURO_REGEN=1"
    );
}

#[test]
fn hashes_match_the_ones_ros_announces() {
    // From the liveliness tokens of `ros2 run action_tutorials_cpp
    // fibonacci_action_server` (tests/data/jazzy_tokens.txt).
    type SendGoal = <Fibonacci as RosAction>::SendGoal;
    type GetResult = <Fibonacci as RosAction>::GetResult;
    type FeedbackMessage = <Fibonacci as RosAction>::FeedbackMessage;
    assert_eq!(
        SendGoal::DDS_TYPE_NAME,
        "action_tutorials_interfaces::action::dds_::Fibonacci_SendGoal_"
    );
    assert_eq!(
        SendGoal::TYPE_HASH,
        "RIHS01_a0603060ed69fe2dfbd1a6f3b982a1749957ef346e4a4d2b311a05e305ec37bb"
    );
    assert_eq!(
        GetResult::TYPE_HASH,
        "RIHS01_8b47e383f1e31f6d8df6417ab54957e7d5ea24dad315646ad711ac3fdea81d58"
    );
    assert_eq!(
        FeedbackMessage::TYPE_HASH,
        "RIHS01_50fc26b9cac313652ecbeab3adf9b5414d59fd4d4d5f9058ddcc7525169927f1"
    );
}

#[test]
fn action_glue_builds_and_splits_messages() {
    let id = [9u8; 16];
    let goal = Fibonacci_Goal { order: 5 };
    let req = Fibonacci::send_goal_request(id, goal.clone());
    assert_eq!(req.goal_id.uuid, id);
    assert_eq!(Fibonacci::split_send_goal_request(req), (id, goal));
    let fb = Fibonacci::feedback_message(
        id,
        Fibonacci_Feedback {
            partial_sequence: vec![0, 1, 1],
        },
    );
    assert_eq!(
        Fibonacci::split_feedback_message(fb)
            .1
            .partial_sequence
            .len(),
        3
    );
}
