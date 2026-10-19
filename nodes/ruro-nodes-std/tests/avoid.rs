//! Obstacle avoidance from laser scans.

use ruro::msg::{LaserScan, Twist};
use ruro::node::testing::Harness;
use serde_json::json;

/// A 360-beam scan, one beam per degree from -180°, everything at `far`
/// except the beams within `width` degrees of `at` degrees, which see `near`.
fn scan(far: f32, at: f32, width: f32, near: f32) -> LaserScan {
    let ranges = (0..360)
        .map(|i| {
            let a = -180.0 + i as f32;
            if (a - at).abs() <= width { near } else { far }
        })
        .collect();
    LaserScan {
        angle_min: -std::f32::consts::PI,
        angle_max: std::f32::consts::PI,
        angle_increment: std::f32::consts::PI / 180.0,
        range_min: 0.1,
        range_max: 10.0,
        ranges,
        ..Default::default()
    }
}

fn command(h: &mut Harness, s: LaserScan) -> Twist {
    h.push("scan", s).unwrap();
    h.step().unwrap();
    let out: Vec<Twist> = h.take("cmd").unwrap();
    out.last().cloned().expect("a command per scan")
}

fn avoider() -> Harness {
    ruro_nodes_std::link();
    Harness::new(
        "std/scan-avoid",
        json!({ "speed": 0.5, "turn": 1.0, "stop_distance": 1.0 }),
    )
    .unwrap()
}

#[test]
fn drives_ahead_when_clear_and_slows_near_obstacles() {
    let mut h = avoider();
    let c = command(&mut h, scan(8.0, 0.0, 0.0, 8.0));
    assert_eq!((c.linear.x, c.angular.z), (0.5, 0.0));
    // 1.5 m ahead: half way between the stop distance and full speed.
    let c = command(&mut h, scan(8.0, 0.0, 10.0, 1.5));
    assert!((c.linear.x - 0.25).abs() < 1e-6, "{c:?}");
    // Obstacles behind or beside the sector do not matter.
    let c = command(&mut h, scan(8.0, 90.0, 20.0, 0.3));
    assert_eq!(c.linear.x, 0.5);
}

#[test]
fn turns_towards_the_side_with_more_room_and_keeps_turning() {
    let mut h = avoider();
    // Blocked ahead, a wall close on the right: turn left (counter-clockwise).
    let mut s = scan(8.0, 0.0, 10.0, 0.5);
    for (i, r) in s.ranges.iter_mut().enumerate() {
        if (100..170).contains(&i) {
            *r = 0.4; // -80° .. -10°
        }
    }
    let c = command(&mut h, s);
    assert_eq!((c.linear.x, c.angular.z), (0.0, 1.0));
    // Still blocked, now with more room on the right: no dithering.
    let mut s = scan(8.0, 0.0, 10.0, 0.5);
    for (i, r) in s.ranges.iter_mut().enumerate() {
        if (190..260).contains(&i) {
            *r = 0.4; // 10° .. 80°
        }
    }
    assert_eq!(command(&mut h, s.clone()).angular.z, 1.0);
    // Once clear, the next obstacle picks a direction afresh.
    assert_eq!(command(&mut h, scan(8.0, 0.0, 0.0, 8.0)).angular.z, 0.0);
    assert_eq!(command(&mut h, s).angular.z, -1.0);
}

#[test]
fn invalid_returns_are_ignored_and_stops_send_zero() {
    let mut h = avoider();
    // Below range_min, NaN and infinity are not obstacles.
    let mut s = scan(8.0, 0.0, 5.0, 0.05);
    s.ranges[178] = f32::NAN;
    s.ranges[182] = f32::INFINITY;
    assert_eq!(command(&mut h, s).linear.x, 0.5);
    h.set_estop(true);
    assert_eq!(
        command(&mut h, scan(8.0, 0.0, 0.0, 8.0)),
        Twist::default()
    );
}
