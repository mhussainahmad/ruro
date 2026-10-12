use std::f64::consts::FRAC_PI_2;

use ruro::msg::{Header, Transform, Transforms};
use ruro_tf::{Isometry, TfBuffer, TfError, Time};

const EPS: f64 = 1e-9;

fn tf(parent: &str, child: &str, stamp: u64, iso: Isometry) -> Transform {
    Transform {
        header: Header::new(parent, stamp),
        child_frame_id: child.into(),
        translation: iso.vec3(),
        rotation: iso.quat(),
    }
}

fn close(a: [f64; 3], b: [f64; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9)
}

/// world -> base (dynamic, moving along x) -> arm (static, +1 z, yawed 90°)
/// -> tool (static, +0.5 x); world -> map_marker (static).
fn robot() -> TfBuffer {
    let mut b = TfBuffer::new();
    for (t, x) in [(100, 0.0), (200, 1.0), (300, 2.0)] {
        b.set(
            &tf("world", "base", t, Isometry::from_translation(x, 0.0, 0.0)),
            false,
        )
        .unwrap();
    }
    b.set(
        &tf(
            "base",
            "arm",
            0,
            Isometry::from_rpy(0.0, 0.0, FRAC_PI_2).with_translation(0.0, 0.0, 1.0),
        ),
        true,
    )
    .unwrap();
    b.set(
        &tf("arm", "tool", 0, Isometry::from_translation(0.5, 0.0, 0.0)),
        true,
    )
    .unwrap();
    b.set(
        &tf(
            "world",
            "marker",
            0,
            Isometry::from_translation(0.0, 5.0, 0.0),
        ),
        true,
    )
    .unwrap();
    b
}

#[test]
fn chain_lookup_composes_edges() {
    let b = robot();
    // tool origin: base at x=2 (latest), arm 1 up, tool 0.5 along arm x = world y.
    let p = b
        .transform_point("world", "tool", Time::Latest, [0.0; 3])
        .unwrap();
    assert!(close(p, [2.0, 0.5, 1.0]), "{p:?}");
}

#[test]
fn inverse_lookup_is_the_inverse() {
    let b = robot();
    let fwd = b.lookup("world", "tool", Time::At(150)).unwrap();
    let inv = b.lookup("tool", "world", Time::At(150)).unwrap();
    assert!(fwd.compose(&inv).approx_eq(&Isometry::IDENTITY, EPS));
    let p = [0.3, 0.1, -2.0];
    assert!(close(inv.transform_point(fwd.transform_point(p)), p));
}

#[test]
fn lookup_across_branches_goes_through_common_ancestor() {
    let b = robot();
    // marker seen from base at t=200: base at x=1, marker at y=5.
    let p = b
        .transform_point("base", "marker", Time::At(200), [0.0; 3])
        .unwrap();
    assert!(close(p, [-1.0, 5.0, 0.0]), "{p:?}");
    // Same frame is identity.
    assert!(
        b.lookup("arm", "arm", Time::Latest)
            .unwrap()
            .approx_eq(&Isometry::IDENTITY, EPS)
    );
}

#[test]
fn dynamic_edges_interpolate() {
    let b = robot();
    let p = b
        .transform_point("world", "base", Time::At(250), [0.0; 3])
        .unwrap();
    assert!(close(p, [1.5, 0.0, 0.0]));
    // Exact sample times hit the sample.
    let p = b
        .transform_point("world", "base", Time::At(100), [0.0; 3])
        .unwrap();
    assert!(close(p, [0.0, 0.0, 0.0]));
}

#[test]
fn rotations_are_slerped() {
    let mut b = TfBuffer::new();
    b.set(&tf("w", "r", 1, Isometry::IDENTITY), false).unwrap();
    b.set(
        &tf("w", "r", 11, Isometry::from_rpy(0.0, 0.0, FRAC_PI_2)),
        false,
    )
    .unwrap();
    let mid = b.lookup("w", "r", Time::At(6)).unwrap();
    assert!(mid.approx_eq(&Isometry::from_rpy(0.0, 0.0, FRAC_PI_2 / 2.0), EPS));
}

#[test]
fn extrapolation_is_an_error() {
    let b = robot();
    let err = b.lookup("world", "tool", Time::At(301)).unwrap_err();
    assert!(
        matches!(
            err,
            TfError::Extrapolation {
                oldest: 100,
                newest: 300,
                ..
            }
        ),
        "{err}"
    );
    assert!(b.lookup("world", "tool", Time::At(99)).is_err());
    // Static-only paths are valid at any time.
    assert!(b.can_transform("base", "tool", Time::At(1)));
}

#[test]
fn unknown_and_disconnected_frames() {
    let mut b = robot();
    assert_eq!(
        b.lookup("world", "ghost", Time::Latest).unwrap_err(),
        TfError::UnknownFrame("ghost".into())
    );
    b.set(&tf("other_root", "island", 0, Isometry::IDENTITY), true)
        .unwrap();
    assert!(matches!(
        b.lookup("world", "island", Time::Latest).unwrap_err(),
        TfError::NotConnected(..)
    ));
}

#[test]
fn latest_uses_common_time_of_the_path() {
    let mut b = robot();
    // A second dynamic edge that lags behind: only known until t=200.
    for (t, y) in [(100, 0.0), (200, 2.0)] {
        b.set(
            &tf("tool", "finger", t, Isometry::from_translation(0.0, y, 0.0)),
            false,
        )
        .unwrap();
    }
    let latest = b.lookup("world", "finger", Time::Latest).unwrap();
    let at_200 = b.lookup("world", "finger", Time::At(200)).unwrap();
    assert!(latest.approx_eq(&at_200, EPS));
    let msg = b.lookup_msg("world", "finger", Time::Latest).unwrap();
    assert_eq!(msg.header.stamp_ns, 200);
    assert_eq!(msg.header.frame_id, "world");
    assert_eq!(msg.child_frame_id, "finger");
}

#[test]
fn cycles_and_self_parents_are_rejected() {
    let mut b = robot();
    assert!(matches!(
        b.set(&tf("tool", "world", 0, Isometry::IDENTITY), true),
        Err(TfError::Cycle { .. })
    ));
    assert!(matches!(
        b.set(&tf("a", "a", 0, Isometry::IDENTITY), true),
        Err(TfError::SelfParent(_))
    ));
    assert_eq!(
        b.set(&tf("", "a", 0, Isometry::IDENTITY), true),
        Err(TfError::EmptyFrame)
    );
}

#[test]
fn reparenting_replaces_the_edge() {
    let mut b = robot();
    b.set(&tf("marker", "tool", 0, Isometry::IDENTITY), true)
        .unwrap();
    assert_eq!(b.parent("tool"), Some("marker"));
    let p = b
        .transform_point("world", "tool", Time::Latest, [0.0; 3])
        .unwrap();
    assert!(close(p, [0.0, 5.0, 0.0]));
}

#[test]
fn history_is_pruned_to_cache_length() {
    let mut b = TfBuffer::new().with_cache_ns(100);
    for t in [0u64, 50, 100, 150, 200].map(|t| t + 1000) {
        b.set(&tf("w", "r", t, Isometry::IDENTITY), false).unwrap();
    }
    assert!(b.lookup("w", "r", Time::At(1050)).is_err());
    assert!(b.lookup("w", "r", Time::At(1100)).is_ok());
}

#[test]
fn out_of_order_samples_are_sorted_and_duplicates_replaced() {
    let mut b = TfBuffer::new();
    b.set(
        &tf("w", "r", 30, Isometry::from_translation(3.0, 0.0, 0.0)),
        false,
    )
    .unwrap();
    b.set(
        &tf("w", "r", 10, Isometry::from_translation(1.0, 0.0, 0.0)),
        false,
    )
    .unwrap();
    b.set(
        &tf("w", "r", 10, Isometry::from_translation(0.0, 0.0, 0.0)),
        false,
    )
    .unwrap();
    let p = b.transform_point("w", "r", Time::At(20), [0.0; 3]).unwrap();
    assert!(close(p, [1.5, 0.0, 0.0]));
}

#[test]
fn ingest_applies_all_valid_transforms() {
    let mut b = TfBuffer::new();
    let msg = Transforms(vec![
        tf("w", "a", 0, Isometry::IDENTITY),
        tf("x", "x", 0, Isometry::IDENTITY),
        tf("a", "b", 0, Isometry::IDENTITY),
    ]);
    assert!(b.ingest(&msg, true).is_err());
    assert_eq!(b.frames(), ["a", "b", "w"]);
    assert!(b.can_transform("w", "b", Time::Latest));
}

#[test]
fn leading_slashes_are_ignored() {
    let mut b = TfBuffer::new();
    b.set(
        &tf(
            "/map",
            "/odom",
            0,
            Isometry::from_translation(1.0, 0.0, 0.0),
        ),
        true,
    )
    .unwrap();
    assert!(b.can_transform("map", "/odom", Time::Latest));
}
