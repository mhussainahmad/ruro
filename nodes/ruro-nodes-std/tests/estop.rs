//! E-stop behaviour of the simulated actuators.

use std::time::Duration;

use ruro::msg::{JointState, Scalar};
use ruro::node::testing::Harness;
use serde_json::json;

/// Steps once (after a short pause, so `dt` is not zero) and returns the
/// commanded velocity and the velocity. The node reports `command - velocity`
/// as its effort.
fn step(h: &mut Harness) -> (f64, f64) {
    std::thread::sleep(Duration::from_millis(2));
    h.step().unwrap();
    let s: Vec<JointState> = h.take("state").unwrap();
    let s = s.last().unwrap();
    (s.effort[0] + s.velocity[0], s.velocity[0])
}

#[test]
fn fake_arm_decelerates_to_rest_and_ignores_commands_while_stopped() {
    ruro_nodes_std::link();
    let mut h = Harness::new("std/fake-arm", json!({ "lag": 0.01 })).unwrap();
    h.push("velocity", Scalar(2.0)).unwrap();
    let (cmd, _) = step(&mut h);
    assert_eq!(cmd, 2.0);
    for _ in 0..20 {
        step(&mut h);
    }
    let (_, moving) = step(&mut h);
    assert!(moving > 1.0, "the joint is moving: {moving}");

    h.set_estop(true);
    h.push("velocity", Scalar(3.0)).unwrap();
    let (cmd, vel) = step(&mut h);
    assert_eq!(cmd, 0.0, "zero velocity is commanded while stopped");
    assert!(vel < moving, "the joint slows down with its lag: {vel}");
    // 100 steps of at least 2 ms is 20 lag time constants, even when sleeps are exact.
    for _ in 0..100 {
        h.push("velocity", Scalar(3.0)).unwrap();
        assert_eq!(step(&mut h).0, 0.0);
    }
    assert!(step(&mut h).1.abs() < 1e-3, "the joint comes to rest");

    // A command queued during the stop is not applied after the release.
    h.push("velocity", Scalar(-1.5)).unwrap();
    h.set_estop(false);
    assert_eq!(step(&mut h).0, 0.0, "stale command was discarded");
    assert_eq!(step(&mut h).0, 0.0, "nothing new arrived");
    h.push("velocity", Scalar(1.0)).unwrap();
    assert_eq!(step(&mut h).0, 1.0, "fresh commands are followed");
}
