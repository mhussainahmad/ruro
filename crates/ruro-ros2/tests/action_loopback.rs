//! Action servers and clients through a local Zenoh router: goals,
//! feedback, results, cancellation and rejection, and the entities they
//! announce compared with a real fibonacci_action_server's
//! (tests/data/jazzy_tokens.txt).

use std::time::{Duration, Instant};

use ruro_ros2::action::{ActionClient, ActionServer, GoalStatus};
use ruro_ros2::discovery::{self, EntityKind};
use ruro_ros2::msgs::action_msgs::srv::CancelGoal_Response;
use ruro_ros2::msgs::example_interfaces::action::{
    Fibonacci, Fibonacci_Feedback, Fibonacci_Goal, Fibonacci_Result,
};
use ruro_ros2::rmw::{Endpoint, EndpointConfig};
use zenoh::Wait;

#[rustfmt::skip]
#[allow(dead_code)]
#[path = "fixtures/action_tutorials_interfaces.rs"]
mod tutorials;

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

fn endpoint(router: &str, node: &str, mode: &str) -> Endpoint {
    Endpoint::open(EndpointConfig {
        connect: vec![router.into()],
        mode: mode.into(),
        domain: 0,
        node_name: node.into(),
        namespace: "/".into(),
        credentials: Default::default(),
    })
    .unwrap()
}

/// Serves Fibonacci goals on a thread: one step every `step`, feedback
/// after each, honouring cancel requests. Rejects orders above 40.
fn spawn_server(node: Endpoint, step: Duration) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let server =
            ActionServer::<Fibonacci>::with_acceptor(&node, "fibonacci", |_, g| g.order <= 40)
                .unwrap();
        let start = Instant::now();
        let mut served = 0;
        while served < 2 && start.elapsed() < Duration::from_secs(30) {
            let Some(goal) = server.next_goal() else {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            };
            served += 1;
            goal.execute();
            let mut seq = vec![0, 1];
            let mut canceled = false;
            for i in 1..goal.goal().order {
                if goal.is_cancel_requested() {
                    canceled = true;
                    break;
                }
                seq.push(seq[i as usize] + seq[i as usize - 1]);
                goal.publish_feedback(Fibonacci_Feedback {
                    sequence: seq.clone(),
                })
                .unwrap();
                std::thread::sleep(step);
            }
            let result = Fibonacci_Result { sequence: seq };
            if canceled {
                goal.canceled(result);
            } else {
                goal.succeed(result);
            }
        }
        // Keep serving get_result for a moment.
        std::thread::sleep(Duration::from_millis(500));
    })
}

#[test]
fn goals_feedback_results_and_cancel() {
    let (_router, ep) = router();
    let server = spawn_server(
        endpoint(&ep, "fib_server", "client"),
        Duration::from_millis(20),
    );
    let node = endpoint(&ep, "fib_client", "peer");
    let client = ActionClient::<Fibonacci>::new(&node, "/fibonacci").unwrap();
    assert!(client.wait_for_server(Duration::from_secs(10)));

    // Rejected goals fail.
    let err = client
        .send_goal(Fibonacci_Goal { order: 99 }, Duration::from_secs(5))
        .err()
        .expect("rejected");
    assert!(err.to_string().contains("rejected"), "{err}");

    let goal = client
        .send_goal(Fibonacci_Goal { order: 6 }, Duration::from_secs(5))
        .unwrap();
    let (status, result) = goal.result(Duration::from_secs(10)).unwrap();
    assert_eq!(status, GoalStatus::Succeeded);
    assert_eq!(result.sequence, [0, 1, 1, 2, 3, 5, 8]);
    assert_eq!(goal.status(), GoalStatus::Succeeded);
    let feedback = goal.feedback();
    assert!(!feedback.is_empty(), "feedback arrived");
    assert!(feedback.last().unwrap().sequence.len() <= 7);

    // A long goal canceled midway.
    let goal = client
        .send_goal(Fibonacci_Goal { order: 40 }, Duration::from_secs(5))
        .unwrap();
    let start = Instant::now();
    while goal.feedback().is_empty() {
        assert!(start.elapsed() < Duration::from_secs(10), "no feedback");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(goal.cancel(Duration::from_secs(5)).unwrap());
    let (status, result) = goal.result(Duration::from_secs(10)).unwrap();
    assert_eq!(status, GoalStatus::Canceled);
    assert!(result.sequence.len() < 41);
    // Canceling a finished goal is refused.
    assert!(!goal.cancel(Duration::from_secs(5)).unwrap());
    server.join().unwrap();
}

#[test]
fn non_blocking_client_and_server_state() {
    let (_router, ep) = router();
    let node = endpoint(&ep, "fib_both", "client");
    let server = ActionServer::<Fibonacci>::new(&node, "/count").unwrap();
    let client = ActionClient::<Fibonacci>::new(&node, "count").unwrap();
    assert!(client.wait_for_server(Duration::from_secs(10)));
    let mut pending = client
        .send_goal_async(Fibonacci_Goal { order: 1 }, Duration::from_secs(5))
        .unwrap();
    let start = Instant::now();
    let goal = loop {
        if let Some(g) = pending.try_accepted() {
            break g.unwrap();
        }
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(pending.try_accepted().is_none(), "answer taken once");
    let handle = server.next_goal().expect("accepted goal");
    assert_eq!(handle.id(), goal.id());
    assert_eq!(server.goals(), [(goal.id(), GoalStatus::Accepted)]);
    assert!(goal.try_result().is_none());
    handle.execute();
    let start = Instant::now();
    while goal.status() != GoalStatus::Executing {
        assert!(start.elapsed() < Duration::from_secs(5), "status topic");
        std::thread::sleep(Duration::from_millis(5));
    }
    // Dropping an unfinished handle aborts the goal.
    drop(handle);
    let (status, _) = goal.result(Duration::from_secs(5)).unwrap();
    assert_eq!(status, GoalStatus::Aborted);
}

/// The five entities of an action server, compared from the name on with
/// what fibonacci_action_server announces.
#[test]
fn server_entities_match_rcl_action() {
    use tutorials::action_tutorials_interfaces::action::Fibonacci as TutorialFibonacci;
    let (router, ep) = router();
    let node = endpoint(&ep, "fibonacci_action_server", "client");
    let _server = ActionServer::<TutorialFibonacci>::new(&node, "fibonacci").unwrap();
    let real: Vec<String> = include_str!("data/jazzy_tokens.txt")
        .lines()
        .filter(|l| l.contains("/fibonacci_action_server/"))
        .map(|l| l.split('/').skip(9).collect::<Vec<_>>().join("/"))
        .collect();
    assert_eq!(real.len(), 5);
    let start = Instant::now();
    loop {
        let mut ours: Vec<String> = discovery::entities(&router, 0, Duration::from_millis(300))
            .unwrap()
            .into_iter()
            .filter(|e| {
                matches!(e.kind, EntityKind::Service | EntityKind::Publisher)
                    && e.topic.as_deref().is_some_and(|t| t.contains("/_action/"))
            })
            .map(|e| {
                format!(
                    "{}/{}/{}/{}",
                    ruro_ros2::rmw::mangle(&e.topic.unwrap()),
                    e.dds_type.unwrap(),
                    e.type_hash.unwrap(),
                    e.qos.unwrap()
                )
            })
            .collect();
        ours.sort();
        let mut want = real.clone();
        want.sort();
        if ours == want {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "ours: {ours:#?}\nreal: {want:#?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Endings follow rcl_action's state machine, and cancelling all goals of
/// an idle server is no error.
#[test]
fn goal_endings_follow_rcl_action() {
    let (_router, ep) = router();
    let node = endpoint(&ep, "fib_endings", "client");
    let server = ActionServer::<Fibonacci>::new(&node, "/endings").unwrap();
    let client = ActionClient::<Fibonacci>::new(&node, "endings").unwrap();
    assert!(client.wait_for_server(Duration::from_secs(10)));
    let none = client.cancel_all(Duration::from_secs(5)).unwrap();
    assert_eq!(none.return_code, CancelGoal_Response::ERROR_NONE);
    assert!(none.goals_canceling.is_empty());

    let next = || {
        let start = Instant::now();
        loop {
            if let Some(h) = server.next_goal() {
                return h;
            }
            assert!(start.elapsed() < Duration::from_secs(5), "no goal");
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    // Succeeding a goal never marked executing.
    let goal = client
        .send_goal(Fibonacci_Goal { order: 1 }, Duration::from_secs(5))
        .unwrap();
    next().succeed(Fibonacci_Result { sequence: vec![0] });
    let (status, _) = goal.result(Duration::from_secs(5)).unwrap();
    assert_eq!(status, GoalStatus::Succeeded);
    // Canceled without a cancel request is an abort.
    let goal = client
        .send_goal(Fibonacci_Goal { order: 1 }, Duration::from_secs(5))
        .unwrap();
    let handle = next();
    handle.execute();
    handle.canceled(Fibonacci_Result::default());
    let (status, _) = goal.result(Duration::from_secs(5)).unwrap();
    assert_eq!(status, GoalStatus::Aborted);
}
