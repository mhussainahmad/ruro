//! Several daemons on one machine, with a stand-in host binary.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ruro_proto::{Ctl, DaemonStatus, Keys};
use zenoh::Wait;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A host that offers one node type and idles when asked to run a group.
fn fake_host(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("fake-host");
    std::fs::write(
        &path,
        "#!/bin/sh\nif [ \"$1\" = describe ]; then echo '{\"nodes\":{\"t/x\":{\"kind\":\"t/x\"}}}'; exit 0; fi\nexec sleep 30\n",
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn daemon(name: &str, port: u16, peers: Vec<String>, host: &Path) -> ruro_daemon::Daemon {
    let mut config = ruro_daemon::Config::new();
    config.name = name.into();
    config.listen = format!("tcp/127.0.0.1:{port}");
    config.peers = peers;
    config.host = host.to_path_buf();
    ruro_daemon::Daemon::start(config).expect("daemon starts")
}

/// Waits for a status of `daemon` that satisfies `done`.
fn wait_status(session: &zenoh::Session, daemon: &str, done: impl Fn(&DaemonStatus) -> bool) {
    let sub = session
        .declare_subscriber(Keys::new(daemon).status())
        .wait()
        .unwrap();
    let start = Instant::now();
    let mut last = None;
    while start.elapsed() < Duration::from_secs(10) {
        if let Ok(Some(s)) = sub.recv_timeout(Duration::from_millis(100)) {
            let st: DaemonStatus = ruro_proto::decode(&s.payload().to_bytes()).unwrap();
            if done(&st) {
                return;
            }
            last = Some(st);
        }
    }
    panic!("timed out; last status of `{daemon}`: {last:?}");
}

#[test]
fn the_emergency_stop_reaches_every_machine_of_the_graph() {
    let dir = std::env::temp_dir().join(format!("ruro-daemon-peers-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let host = fake_host(&dir);
    let (robot_port, laptop_port) = (free_port(), free_port());
    let tag = std::process::id();
    let (robot, laptop) = (format!("robot{tag}"), format!("laptop{tag}"));
    let robot_daemon = daemon(&robot, robot_port, Vec::new(), &host);
    let laptop_daemon = daemon(
        &laptop,
        laptop_port,
        vec![format!("tcp/127.0.0.1:{robot_port}")],
        &host,
    );

    let session = ruro_proto::connect(&format!("tcp/127.0.0.1:{laptop_port}")).unwrap();
    let request_to = |daemon: &str, op, payload: &str| {
        ruro_proto::request(
            &session,
            &Keys::new(daemon),
            op,
            payload,
            Duration::from_secs(10),
        )
        .unwrap()
    };
    let request = |op, payload: &str| request_to(&laptop, op, payload);
    // Engaged before the graph exists: the robot's part must come up stopped.
    request(Ctl::Estop, "engage");
    let graph = format!(
        "name: two\ngroups:\n  far: {{ machine: {robot} }}\nnodes:\n  a: {{ type: t/x, group: far, rate: 1 }}\n  b: {{ type: t/x, rate: 1 }}\n"
    );
    request(Ctl::Deploy, &graph);
    wait_status(&session, &robot, |s| {
        s.graph.as_deref() == Some("two") && s.estopped
    });

    request(Ctl::Estop, "release");
    wait_status(&session, &robot, |s| !s.estopped);
    request(Ctl::Estop, "engage");
    wait_status(&session, &robot, |s| s.estopped);

    // Engaged at the robot too: releasing from the laptop leaves it engaged.
    request_to(&robot, Ctl::Estop, "engage");
    request(Ctl::Estop, "release");
    std::thread::sleep(Duration::from_millis(600));
    wait_status(&session, &robot, |s| s.estopped);
    wait_status(&session, &laptop, |s| !s.estopped);
    request_to(&robot, Ctl::Estop, "release");
    wait_status(&session, &robot, |s| !s.estopped);

    // The robot now deploys a graph that spans the laptop: the laptop's own
    // graph is gone, and stops travel one way without bouncing back.
    let graph = format!(
        "name: back\ngroups:\n  near: {{ machine: {laptop} }}\nnodes:\n  a: {{ type: t/x, group: near, rate: 1 }}\n  b: {{ type: t/x, rate: 1 }}\n"
    );
    request_to(&robot, Ctl::Deploy, &graph);
    let start = Instant::now();
    request(Ctl::Estop, "engage");
    request_to(&robot, Ctl::Estop, "engage");
    request_to(&robot, Ctl::Estop, "release");
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
    wait_status(&session, &laptop, |s| s.estopped);
    wait_status(&session, &robot, |s| !s.estopped);

    request_to(&robot, Ctl::Stop, "");
    drop((robot_daemon, laptop_daemon));
    for name in [&robot, &laptop] {
        let _ = std::fs::remove_dir_all(ruro_daemon::runtime_dir(name));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_second_daemon_with_the_same_name_leaves_the_first_running() {
    let dir = std::env::temp_dir().join(format!("ruro-daemon-twin-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let host = fake_host(&dir);
    let name = format!("twin{}", std::process::id());
    let first = daemon(&name, free_port(), Vec::new(), &host);

    let mut config = ruro_daemon::Config::new();
    config.name = name.clone();
    config.listen = format!("tcp/127.0.0.1:{}", free_port());
    config.host = host.clone();
    let err = ruro_daemon::Daemon::start(config)
        .err()
        .expect("a second daemon with the same name is refused");
    assert!(format!("{err:#}").contains("already running"), "{err:#}");

    // Hosts still reach the first daemon through its socket.
    let socket = ruro_daemon::runtime_dir(&name).join("zenoh.sock");
    let session = ruro_proto::connect(&format!("unixsock-stream/{}", socket.display())).unwrap();
    let reply = ruro_proto::request(
        &session,
        &Keys::new(&name),
        Ctl::Ping,
        "",
        Duration::from_secs(5),
    );
    assert_eq!(reply.unwrap(), name);
    drop(first);
    let _ = std::fs::remove_dir_all(ruro_daemon::runtime_dir(&name));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unreachable_peer_does_not_keep_the_emergency_stop_from_the_others() {
    let dir = std::env::temp_dir().join(format!("ruro-daemon-gone-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let host = fake_host(&dir);
    let ports = [free_port(), free_port(), free_port()];
    let tag = std::process::id();
    // Peers are contacted in name order: the one that goes away comes first.
    let (gone, robot, laptop) = (
        format!("arm{tag}"),
        format!("base{tag}"),
        format!("console{tag}"),
    );
    let gone_daemon = daemon(&gone, ports[0], Vec::new(), &host);
    let robot_daemon = daemon(&robot, ports[1], Vec::new(), &host);
    let laptop_daemon = daemon(
        &laptop,
        ports[2],
        ports[..2]
            .iter()
            .map(|p| format!("tcp/127.0.0.1:{p}"))
            .collect(),
        &host,
    );
    let session = ruro_proto::connect(&format!("tcp/127.0.0.1:{}", ports[2])).unwrap();
    let keys = Keys::new(&laptop);
    let graph = format!(
        "name: three\ngroups:\n  a: {{ machine: {gone} }}\n  b: {{ machine: {robot} }}\nnodes:\n  x: {{ type: t/x, group: a, rate: 1 }}\n  y: {{ type: t/x, group: b, rate: 1 }}\n  z: {{ type: t/x, rate: 1 }}\n"
    );
    ruro_proto::request(
        &session,
        &keys,
        Ctl::Deploy,
        &graph,
        Duration::from_secs(10),
    )
    .unwrap();

    // One machine goes away while the laptop still runs the graph.
    drop(gone_daemon);
    let start = Instant::now();
    let reply = ruro_proto::request(
        &session,
        &keys,
        Ctl::Estop,
        "engage",
        Duration::from_secs(10),
    );
    let err = format!("{:#}", reply.expect_err("one machine did not get the stop"));
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
    assert!(err.contains("engaged here") && err.contains(&gone), "{err}");
    assert!(!err.contains(&robot), "{err}");
    wait_status(&session, &laptop, |s| s.estopped);
    wait_status(&session, &robot, |s| s.estopped);

    drop((robot_daemon, laptop_daemon));
    for name in [&gone, &robot, &laptop] {
        let _ = std::fs::remove_dir_all(ruro_daemon::runtime_dir(name));
    }
    let _ = std::fs::remove_dir_all(&dir);
}
