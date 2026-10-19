//! Service and action nodes, and transient-local bridges, through a local
//! Zenoh router; the e-stop behaviour of each.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use ruro::msg::{Flag, Scalar, Text, Transform, Transforms};
use ruro::node::testing::Harness;
use ruro_nodes_ros2::services::ESTOP_REFUSAL;
use ruro_ros2::action::ActionServer;
use ruro_ros2::msgs::example_interfaces::action::{
    Fibonacci, Fibonacci_Feedback, Fibonacci_Result,
};
use ruro_ros2::rmw::{Endpoint, EndpointConfig};
use serde_json::json;
use zenoh::Wait;

fn router() -> (zenoh::Session, String) {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let endpoint = format!("tcp/127.0.0.1:{port}");
    let mut c = zenoh::Config::default();
    c.insert_json5("mode", "\"router\"").unwrap();
    c.insert_json5("listen/endpoints", &format!("[\"{endpoint}\"]"))
        .unwrap();
    c.insert_json5("scouting/multicast/enabled", "false")
        .unwrap();
    (zenoh::open(c).wait().unwrap(), endpoint)
}

/// Steps every harness until `done` holds.
fn step_until(
    harnesses: &mut [&mut Harness],
    what: &str,
    mut done: impl FnMut(&mut [&mut Harness]) -> bool,
) {
    let start = Instant::now();
    loop {
        for h in harnesses.iter_mut() {
            h.step().unwrap();
        }
        if done(harnesses) {
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(15), "{what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Calls through `caller` until `server` has answered once (discovery done).
fn first_call(
    caller: &mut Harness,
    server: &mut Harness,
    input: &str,
    msg: Flag,
) -> (bool, String) {
    let start = Instant::now();
    loop {
        caller.push(input, msg).unwrap();
        let mut success = Vec::new();
        let mut message = Vec::new();
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline && success.is_empty() {
            caller.step().unwrap();
            server.step().unwrap();
            success.extend(caller.take::<Flag>("success").unwrap());
            message.extend(caller.take::<Text>("message").unwrap());
            std::thread::sleep(Duration::from_millis(10));
        }
        // Calls made before the server was discovered fail; retry.
        if let (Some(s), Some(m)) = (success.first(), message.first())
            && !m.0.contains("no reply")
        {
            return (s.0, m.0.clone());
        }
        assert!(start.elapsed() < Duration::from_secs(15), "no answer");
    }
}

#[test]
fn trigger_nodes_call_and_serve() {
    ruro_nodes_ros2::link();
    let (_router, endpoint) = router();
    let params = json!({ "service": "/start", "router": endpoint, "domain_id": 5 });
    let mut server = Harness::new(
        "ros2/serve-trigger",
        json!({ "service": "/start", "router": endpoint, "domain_id": 5, "message": "started" }),
    )
    .unwrap();
    server.step().unwrap();
    let mut caller = Harness::new("ros2/call-trigger", params).unwrap();

    assert_eq!(
        first_call(&mut caller, &mut server, "trigger", Flag(true)),
        (true, "started".into())
    );
    assert!(!server.take::<Flag>("triggered").unwrap().is_empty());

    // A stopped server refuses and outputs nothing.
    server.set_estop(true);
    caller.push("trigger", Flag(true)).unwrap();
    let mut got = Vec::new();
    step_until(&mut [&mut caller, &mut server], "refusal", |h| {
        got.extend(h[0].take::<Text>("message").unwrap());
        !got.is_empty()
    });
    assert_eq!(got[0].0, ESTOP_REFUSAL);
    assert!(server.take::<Flag>("triggered").unwrap().is_empty());
    server.set_estop(false);
    caller.take::<Flag>("success").unwrap();

    // A stopped caller does not call; false inputs never call.
    caller.set_estop(true);
    caller.push("trigger", Flag(true)).unwrap();
    caller.push("trigger", Flag(false)).unwrap();
    for _ in 0..20 {
        caller.step().unwrap();
        server.step().unwrap();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(server.take::<Flag>("triggered").unwrap().is_empty());
    assert!(caller.take::<Flag>("success").unwrap().is_empty());
}

#[test]
fn set_bool_nodes_let_only_false_through_an_estop() {
    ruro_nodes_ros2::link();
    let (_router, endpoint) = router();
    let params = json!({ "service": "/motors", "router": endpoint, "domain_id": 6 });
    let mut server = Harness::new("ros2/serve-set-bool", params.clone()).unwrap();
    server.step().unwrap();
    let mut caller = Harness::new("ros2/call-set-bool", params).unwrap();
    assert!(first_call(&mut caller, &mut server, "data", Flag(true)).0);
    assert_eq!(server.take::<Flag>("data").unwrap(), [Flag(true)]);

    server.set_estop(true);
    let (ok, msg) = first_call(&mut caller, &mut server, "data", Flag(true));
    assert!(!ok);
    assert_eq!(msg, ESTOP_REFUSAL);
    assert!(first_call(&mut caller, &mut server, "data", Flag(false)).0);
    assert_eq!(server.take::<Flag>("data").unwrap(), [Flag(false)]);
    server.set_estop(false);

    caller.set_estop(true);
    caller.push("data", Flag(true)).unwrap();
    for _ in 0..20 {
        caller.step().unwrap();
        server.step().unwrap();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        server.take::<Flag>("data").unwrap().is_empty(),
        "true dropped"
    );
    assert!(first_call(&mut caller, &mut server, "data", Flag(false)).0);
}

/// Serves Fibonacci goals, 50 ms a step, until `stop` is set.
/// Each ended goal's order and whether it was canceled goes to `ended`.
fn serve(
    endpoint: String,
    stop: Arc<AtomicBool>,
    ended: mpsc::Sender<(i32, bool)>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let node = Endpoint::open(EndpointConfig {
            connect: vec![endpoint],
            domain: 7,
            node_name: "fib_server".into(),
            ..EndpointConfig::default()
        })
        .unwrap();
        let server = ActionServer::<Fibonacci>::new(&node, "/fib").unwrap();
        while !stop.load(Ordering::Relaxed) {
            let Some(goal) = server.next_goal() else {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            };
            goal.execute();
            let mut seq = vec![0, 1];
            let mut canceled = false;
            for i in 1..goal.goal().order as usize {
                if goal.is_cancel_requested() {
                    canceled = true;
                    break;
                }
                seq.push(seq[i] + seq[i - 1]);
                goal.publish_feedback(Fibonacci_Feedback {
                    sequence: seq.clone(),
                })
                .unwrap();
                std::thread::sleep(Duration::from_millis(50));
            }
            let result = Fibonacci_Result { sequence: seq };
            let _ = ended.send((goal.goal().order, canceled));
            if canceled {
                goal.canceled(result);
            } else {
                goal.succeed(result);
            }
        }
    })
}

#[test]
fn fibonacci_goal_node_and_its_estop() {
    ruro_nodes_ros2::link();
    let (_router, endpoint) = router();
    let stop = Arc::new(AtomicBool::new(false));
    let (ended, _) = mpsc::channel();
    let server = serve(endpoint.clone(), stop.clone(), ended);
    let mut node = Harness::new(
        "ros2/fibonacci-goal",
        json!({ "action": "/fib", "router": endpoint, "domain_id": 7 }),
    )
    .unwrap();
    node.step().unwrap();

    // Goals sent before the server is discovered are rejected (no answer);
    // retry until one is accepted.
    let start = Instant::now();
    let mut status = Vec::new();
    while !status.iter().any(|s: &Text| s.0 == "accepted") {
        node.push("order", Scalar(5.0)).unwrap();
        let deadline = Instant::now() + Duration::from_millis(800);
        while Instant::now() < deadline {
            node.step().unwrap();
            status.extend(node.take::<Text>("status").unwrap());
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(start.elapsed() < Duration::from_secs(15), "never accepted");
    }
    let mut result = Vec::new();
    step_until(&mut [&mut node], "result", |h| {
        result.extend(h[0].take::<Text>("result").unwrap());
        !result.is_empty()
    });
    assert_eq!(result[0].0, "0 1 1 2 3 5");
    assert_eq!(
        node.take::<Flag>("succeeded").unwrap().last(),
        Some(&Flag(true))
    );
    let feedback = node.take::<Text>("feedback").unwrap();
    assert!(feedback.iter().any(|f| f.0 == "0 1 1 2"), "{feedback:?}");

    // A long goal is canceled by the e-stop.
    node.push("order", Scalar(200.0)).unwrap();
    let mut status = Vec::new();
    step_until(&mut [&mut node], "executing", |h| {
        status.extend(h[0].take::<Text>("status").unwrap());
        status.iter().any(|s| s.0 == "executing")
    });
    node.set_estop(true);
    let mut status = Vec::new();
    step_until(&mut [&mut node], "canceled", |h| {
        status.extend(h[0].take::<Text>("status").unwrap());
        status.iter().any(|s| s.0 == "canceled")
    });
    assert_eq!(
        node.take::<Flag>("succeeded").unwrap().last(),
        Some(&Flag(false))
    );

    // While stopped, input sends nothing.
    node.push("order", Scalar(3.0)).unwrap();
    for _ in 0..20 {
        node.step().unwrap();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(node.take::<Text>("status").unwrap().is_empty());
    drop(node);
    stop.store(true, Ordering::Relaxed);
    server.join().unwrap();
}

#[test]
fn a_new_goal_cancels_one_awaiting_acceptance() {
    ruro_nodes_ros2::link();
    let (_router, endpoint) = router();
    let stop = Arc::new(AtomicBool::new(false));
    let (ended, ended_rx) = mpsc::channel();
    let server = serve(endpoint.clone(), stop.clone(), ended);
    let mut node = Harness::new(
        "ros2/fibonacci-goal",
        json!({ "action": "/fib", "router": endpoint, "domain_id": 7 }),
    )
    .unwrap();
    // Warm up until a goal gets through.
    let start = Instant::now();
    loop {
        node.push("order", Scalar(1.0)).unwrap();
        let deadline = Instant::now() + Duration::from_millis(800);
        while Instant::now() < deadline {
            node.step().unwrap();
            std::thread::sleep(Duration::from_millis(20));
        }
        if node
            .take::<Text>("result")
            .unwrap()
            .iter()
            .any(|r| r.0 == "0 1")
        {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(15), "never served");
    }
    while ended_rx.try_recv().is_ok() {}

    // Two goals in one step: the first is still awaiting acceptance when
    // the second replaces it.
    node.push("order", Scalar(200.0)).unwrap();
    node.push("order", Scalar(3.0)).unwrap();
    let mut result = Vec::new();
    step_until(&mut [&mut node], "second goal's result", |h| {
        result.extend(h[0].take::<Text>("result").unwrap());
        !result.is_empty()
    });
    assert_eq!(result[0].0, "0 1 1 2");
    let ended: Vec<_> = ended_rx.try_iter().collect();
    assert_eq!(ended, [(200, true), (3, false)]);
    drop(node);
    stop.store(true, Ordering::Relaxed);
    server.join().unwrap();
}

#[test]
fn transient_local_bridges_latch() {
    ruro_nodes_ros2::link();
    let (_router, endpoint) = router();
    let params = json!({
        "topic": "/tf_static", "router": endpoint, "domain_id": 8,
        "durability": "transient_local", "depth": 1,
    });
    let mut publ = Harness::new("ros2/pub-tf", params.clone()).unwrap();
    let mut t = Transform {
        child_frame_id: "base".into(),
        ..Default::default()
    };
    t.header.frame_id = "world".into();
    let t = Transforms(vec![t]);
    publ.push("input", t.clone()).unwrap();
    publ.step().unwrap();
    std::thread::sleep(Duration::from_millis(300));

    // Started after the only message was published.
    let mut sub = Harness::new("ros2/sub-tf", params).unwrap();
    sub.step().unwrap();
    let mut got = Vec::new();
    step_until(&mut [&mut sub], "latched transform", |h| {
        got.extend(h[0].take::<Transforms>("out").unwrap());
        !got.is_empty()
    });
    assert_eq!(got[0].0[0].child_frame_id, "base");
    assert!(publ.set_param("durability", json!("sometimes")).is_ok());
    assert!(publ.step().is_err(), "bad durability is reported");
}

#[test]
fn rpc_kinds_are_registered() {
    ruro_nodes_ros2::link();
    let registry = ruro::node::registry();
    for kind in [
        "ros2/call-trigger",
        "ros2/call-set-bool",
        "ros2/serve-trigger",
        "ros2/serve-set-bool",
        "ros2/fibonacci-goal",
    ] {
        assert!(registry.get(kind).is_some(), "{kind} not registered");
    }
}
