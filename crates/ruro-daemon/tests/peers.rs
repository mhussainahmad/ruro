//! Daemons on one machine, with stand-in host binaries.

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

    // Engaged at the robot too: releasing from the laptop leaves it engaged,
    // on both, since the robot's stop went back to the laptop.
    request_to(&robot, Ctl::Estop, "engage");
    let reply = request(Ctl::Estop, "release");
    assert!(
        reply.contains("stays engaged") && reply.contains(&robot),
        "{reply}"
    );
    std::thread::sleep(Duration::from_millis(600));
    wait_status(&session, &robot, |s| s.estopped);
    wait_status(&session, &laptop, |s| s.estopped);
    request_to(&robot, Ctl::Estop, "release");
    wait_status(&session, &robot, |s| !s.estopped);
    wait_status(&session, &laptop, |s| !s.estopped);

    // The robot now deploys a graph that spans the laptop: the laptop's own
    // graph is gone, and each stop reaches the other machine once.
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
    std::thread::sleep(Duration::from_millis(600));
    wait_status(&session, &laptop, |s| s.estopped);
    wait_status(&session, &robot, |s| s.estopped);
    request(Ctl::Estop, "release");
    wait_status(&session, &laptop, |s| !s.estopped);
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

#[test]
fn killing_a_group_reaches_the_machine_that_runs_it() {
    let dir = std::env::temp_dir().join(format!("ruro-daemon-kill-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let host = fake_host(&dir);
    let (robot_port, laptop_port) = (free_port(), free_port());
    let tag = std::process::id();
    let (robot, laptop) = (format!("krobot{tag}"), format!("klaptop{tag}"));
    let robot_daemon = daemon(&robot, robot_port, Vec::new(), &host);
    let laptop_daemon = daemon(
        &laptop,
        laptop_port,
        vec![format!("tcp/127.0.0.1:{robot_port}")],
        &host,
    );
    let session = ruro_proto::connect(&format!("tcp/127.0.0.1:{laptop_port}")).unwrap();
    let request = |op, payload: &str| {
        ruro_proto::request(
            &session,
            &Keys::new(&laptop),
            op,
            payload,
            Duration::from_secs(10),
        )
    };
    let graph = format!(
        "name: kill\ngroups:\n  far: {{ machine: {robot} }}\nnodes:\n  a: {{ type: t/x, group: far, rate: 1 }}\n  b: {{ type: t/x, rate: 1 }}\n"
    );
    request(Ctl::Deploy, &graph).unwrap();
    wait_status(&session, &robot, |s| {
        s.groups.iter().any(|g| g.name == "far" && g.pid.is_some())
    });

    let reply = request(Ctl::Kill, "far").unwrap();
    assert!(reply.contains("killed `far`"), "{reply}");
    wait_status(&session, &robot, |s| {
        s.groups
            .iter()
            .any(|g| g.name == "far" && g.restarts >= 1 && g.pid.is_some())
    });
    let err = request(Ctl::Kill, "nowhere").unwrap_err();
    assert!(format!("{err:#}").contains("no group `nowhere`"), "{err:#}");

    request(Ctl::Stop, "").unwrap();
    drop((robot_daemon, laptop_daemon));
    for name in [&robot, &laptop] {
        let _ = std::fs::remove_dir_all(ruro_daemon::runtime_dir(name));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_daemon_that_fails_to_start_leaves_no_runtime_directory() {
    let dir = std::env::temp_dir().join(format!("ruro-daemon-busy-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let host = fake_host(&dir);
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let name = format!("busy{}", std::process::id());
    let mut config = ruro_daemon::Config::new();
    config.name = name.clone();
    config.listen = format!("tcp/127.0.0.1:{}", taken.local_addr().unwrap().port());
    config.host = host;
    assert!(
        ruro_daemon::Daemon::start(config).is_err(),
        "the port is taken"
    );
    assert!(!ruro_daemon::runtime_dir(&name).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_daemon_given_another_graph_stops_the_groups_it_forwarded() {
    let dir = std::env::temp_dir().join(format!("ruro-daemon-replace-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let host = fake_host(&dir);
    let ports = [free_port(), free_port(), free_port()];
    let tag = std::process::id();
    let names = [
        format!("rarm{tag}"),
        format!("rbase{tag}"),
        format!("rconsole{tag}"),
    ];
    let [arm, base, console] = &names;
    let arm_daemon = daemon(arm, ports[0], Vec::new(), &host);
    let base_daemon = daemon(
        base,
        ports[1],
        vec![format!("tcp/127.0.0.1:{}", ports[0])],
        &host,
    );
    let console_daemon = daemon(
        console,
        ports[2],
        vec![format!("tcp/127.0.0.1:{}", ports[1])],
        &host,
    );
    let session = ruro_proto::connect(&format!("tcp/127.0.0.1:{}", ports[2])).unwrap();
    let request_to = |daemon: &str, op, payload: &str| {
        ruro_proto::request(
            &session,
            &Keys::new(daemon),
            op,
            payload,
            Duration::from_secs(10),
        )
    };

    // The base runs a graph with a group on the arm.
    let first = format!(
        "name: first\ngroups:\n  joints: {{ machine: {arm} }}\nnodes:\n  a: {{ type: t/x, group: joints, rate: 1 }}\n  b: {{ type: t/x, rate: 1 }}\n"
    );
    request_to(base, Ctl::Deploy, &first).unwrap();
    wait_status(&session, arm, |s| s.graph.as_deref() == Some("first"));

    // The console deploys a graph that uses the base but not the arm: the
    // base's previous graph, including its part on the arm, goes away.
    let second = format!(
        "name: second\ngroups:\n  drive: {{ machine: {base} }}\nnodes:\n  a: {{ type: t/x, group: drive, rate: 1 }}\n  b: {{ type: t/x, rate: 1 }}\n"
    );
    let start = Instant::now();
    request_to(console, Ctl::Deploy, &second).unwrap();
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
    wait_status(&session, base, |s| s.graph.as_deref() == Some("second"));
    wait_status(&session, arm, |s| s.graph.is_none());

    // A stop meant for a graph the arm no longer runs leaves its own alone.
    let own = "name: own\nnodes:\n  a: { type: t/x, rate: 1 }\n";
    request_to(arm, Ctl::Deploy, own).unwrap();
    let reply = request_to(arm, Ctl::Stop, &format!("from {base}")).unwrap();
    assert!(reply.contains("left running"), "{reply}");
    wait_status(&session, arm, |s| s.graph.as_deref() == Some("own"));

    request_to(console, Ctl::Stop, "").unwrap();
    wait_status(&session, base, |s| s.graph.is_none());
    request_to(arm, Ctl::Stop, "").unwrap();
    drop((arm_daemon, base_daemon, console_daemon));
    for name in &names {
        let _ = std::fs::remove_dir_all(ruro_daemon::runtime_dir(name));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn releases_lift_the_stops_of_their_own_origin_wherever_they_went() {
    let dir = std::env::temp_dir().join(format!("ruro-daemon-origins-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let host = fake_host(&dir);
    let (robot_port, laptop_port) = (free_port(), free_port());
    let tag = std::process::id();
    let (robot, laptop) = (format!("orobot{tag}"), format!("olaptop{tag}"));
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
    let spanning = format!(
        "name: span\ngroups:\n  far: {{ machine: {robot} }}\nnodes:\n  a: {{ type: t/x, group: far, rate: 1 }}\n  b: {{ type: t/x, rate: 1 }}\n"
    );
    request_to(&laptop, Ctl::Deploy, &spanning);
    request_to(&laptop, Ctl::Estop, "engage");
    wait_status(&session, &robot, |s| s.estopped);

    // A release at the robot lifts only the robot's own stop; the laptop's
    // stays, and the reply says whose it is.
    request_to(&robot, Ctl::Estop, "engage");
    let reply = request_to(&robot, Ctl::Estop, "release");
    assert!(
        reply.contains("stays engaged") && reply.contains(&laptop),
        "{reply}"
    );
    std::thread::sleep(Duration::from_millis(600));
    wait_status(&session, &robot, |s| s.estopped);
    // `--all` lifts it there, but not at the laptop.
    request_to(&robot, Ctl::Estop, "release-all");
    wait_status(&session, &robot, |s| !s.estopped);
    wait_status(&session, &laptop, |s| s.estopped);

    // A redeploy drops the robot from the graph while it holds the laptop's
    // stop; releasing at the laptop still reaches it.
    request_to(&laptop, Ctl::Estop, "engage");
    wait_status(&session, &robot, |s| s.estopped);
    request_to(
        &laptop,
        Ctl::Deploy,
        "name: alone\nnodes:\n  a: { type: t/x, rate: 1 }\n",
    );
    wait_status(&session, &robot, |s| s.graph.is_none() && s.estopped);
    let reply = request_to(&laptop, Ctl::Estop, "release");
    assert!(reply.contains("e-stop released"), "{reply}");
    wait_status(&session, &robot, |s| !s.estopped);
    wait_status(&session, &laptop, |s| !s.estopped);

    // A machine that left the graph and then went away does not make the
    // release fail; it is noted.
    request_to(&laptop, Ctl::Deploy, &spanning);
    request_to(&laptop, Ctl::Estop, "engage");
    request_to(
        &laptop,
        Ctl::Deploy,
        "name: alone\nnodes:\n  a: { type: t/x, rate: 1 }\n",
    );
    drop(robot_daemon);
    let reply = request_to(&laptop, Ctl::Estop, "release");
    assert!(
        reply.starts_with("e-stop released") && reply.contains("no longer in the graph"),
        "{reply}"
    );

    request_to(&laptop, Ctl::Stop, "");
    drop(laptop_daemon);
    for name in [&robot, &laptop] {
        let _ = std::fs::remove_dir_all(ruro_daemon::runtime_dir(name));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_stop_engaged_on_any_machine_stops_the_whole_graph() {
    let dir = std::env::temp_dir().join(format!("ruro-daemon-anywhere-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let host = fake_host(&dir);
    let ports = [free_port(), free_port(), free_port()];
    let tag = std::process::id();
    let (arm, base, laptop) = (
        format!("warm{tag}"),
        format!("wbase{tag}"),
        format!("wlaptop{tag}"),
    );
    let arm_daemon = daemon(&arm, ports[0], Vec::new(), &host);
    let base_daemon = daemon(&base, ports[1], Vec::new(), &host);
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
    let graph = format!(
        "name: three\ngroups:\n  a: {{ machine: {arm} }}\n  b: {{ machine: {base} }}\nnodes:\n  x: {{ type: t/x, group: a, rate: 1 }}\n  y: {{ type: t/x, group: b, rate: 1 }}\n  z: {{ type: t/x, rate: 1 }}\n"
    );
    request_to(&laptop, Ctl::Deploy, &graph);

    // Engaged on the base, the stop reaches the laptop that deployed the
    // graph and, through it, the arm.
    let reply = request_to(&base, Ctl::Estop, "engage");
    assert!(reply.starts_with("e-stop engaged"), "{reply}");
    for d in [&arm, &base, &laptop] {
        wait_status(&session, d, |s| s.estopped);
    }
    // Only the base's own release lifts it.
    let reply = request_to(&laptop, Ctl::Estop, "release");
    assert!(
        reply.contains("stays engaged") && reply.contains(&base),
        "{reply}"
    );
    std::thread::sleep(Duration::from_millis(600));
    wait_status(&session, &arm, |s| s.estopped);
    request_to(&base, Ctl::Estop, "release");
    for d in [&arm, &base, &laptop] {
        wait_status(&session, d, |s| !s.estopped);
    }

    // The base and the laptop engaging at the same time do not hold each
    // other up.
    let start = Instant::now();
    std::thread::scope(|s| {
        s.spawn(|| request_to(&base, Ctl::Estop, "engage"));
        s.spawn(|| request_to(&laptop, Ctl::Estop, "engage"));
    });
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "{:?}",
        start.elapsed()
    );
    for d in [&arm, &base, &laptop] {
        wait_status(&session, d, |s| s.estopped);
    }

    request_to(&laptop, Ctl::Stop, "");
    drop((arm_daemon, base_daemon, laptop_daemon));
    for name in [&arm, &base, &laptop] {
        let _ = std::fs::remove_dir_all(ruro_daemon::runtime_dir(name));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A host like [`fake_host`] whose node types exchange `t/Msg`, built with
/// the definition `hash`, plus `extra` node types of its own.
fn typed_host(dir: &Path, name: &str, hash: &str, extra: &str) -> PathBuf {
    let path = dir.join(name);
    let node = |kind: &str| {
        format!(
            r#""{kind}":{{"kind":"{kind}","default_rate":1,"inputs":[{{"name":"in","ty":"t/Msg"}}],"outputs":[{{"name":"out","ty":"t/Msg"}}]}}"#
        )
    };
    let mut nodes = vec![node("t/x")];
    nodes.extend(extra.split_whitespace().map(node));
    let registry = format!(
        r#"{{"nodes":{{{}}},"schema_hashes":{{"t/Msg":"{hash}"}}}}"#,
        nodes.join(",")
    );
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nif [ \"$1\" = describe ]; then echo '{registry}'; exit 0; fi\nexec sleep 30\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn deploys_check_the_hosts_each_group_runs_on() {
    let dir = std::env::temp_dir().join(format!("ruro-daemon-schemas-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let host = typed_host(&dir, "host-a", "aaaaaaaaaaaaaaaa", "");
    let other = typed_host(&dir, "host-b", "bbbbbbbbbbbbbbbb", "t/special");
    let (robot_port, laptop_port) = (free_port(), free_port());
    let tag = std::process::id();
    let (robot, laptop) = (format!("srobot{tag}"), format!("slaptop{tag}"));
    let robot_daemon = daemon(&robot, robot_port, Vec::new(), &host);
    let laptop_daemon = daemon(
        &laptop,
        laptop_port,
        vec![format!("tcp/127.0.0.1:{robot_port}")],
        &host,
    );
    let session = ruro_proto::connect(&format!("tcp/127.0.0.1:{laptop_port}")).unwrap();
    let request = |op, payload: &str| {
        ruro_proto::request(
            &session,
            &Keys::new(&laptop),
            op,
            payload,
            Duration::from_secs(10),
        )
    };
    let graph = |kind: &str| {
        format!(
            "name: schemas\ngroups:\n  far: {{ machine: {robot}, host: {} }}\nnodes:\n  a: {{ type: t/x }}\n  b: {{ type: {kind}, group: far }}\nwires: [a.out -> b.in]\n",
            other.display()
        )
    };

    // The robot's custom host was built with another definition of t/Msg.
    let issues: Vec<String> =
        serde_json::from_str(&request(Ctl::Validate, &graph("t/x")).unwrap()).unwrap();
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert!(
        issues[0].contains("a.out -> b.in") && issues[0].contains("t/Msg"),
        "{issues:?}"
    );
    let err = request(Ctl::Deploy, &graph("t/x")).unwrap_err();
    assert!(
        format!("{err:#}").contains("different definitions"),
        "{err:#}"
    );

    // A type only that host offers is known, through the robot.
    let reply = request(Ctl::Validate, &graph("t/special")).unwrap();
    let issues: Vec<String> = serde_json::from_str(&reply).unwrap();
    assert!(issues.iter().all(|i| !i.contains("unknown")), "{issues:?}");
    let registry = ruro_proto::request(
        &session,
        &Keys::new(&robot),
        Ctl::Registry,
        &other.display().to_string(),
        Duration::from_secs(10),
    )
    .unwrap();
    assert!(registry.contains("t/special"), "{registry}");

    drop((robot_daemon, laptop_daemon));
    for name in [&robot, &laptop] {
        let _ = std::fs::remove_dir_all(ruro_daemon::runtime_dir(name));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_host_that_fails_to_start_says_why() {
    let dir = std::env::temp_dir().join(format!("ruro-daemon-fail-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let host = dir.join("failing-host");
    std::fs::write(
        &host,
        "#!/bin/sh\nif [ \"$1\" = describe ]; then echo '{\"nodes\":{\"t/x\":{\"kind\":\"t/x\"}}}'; exit 0; fi\n\
         echo 'starting' >&2\necho 'failing-host: failed to open topic `ruro/x`: DoesNotExist' >&2\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o755)).unwrap();
    let port = free_port();
    let name = format!("failing{}", std::process::id());
    let d = daemon(&name, port, Vec::new(), &host);
    let session = ruro_proto::connect(&format!("tcp/127.0.0.1:{port}")).unwrap();
    let logs = session
        .declare_subscriber(Keys::new(&name).all_logs())
        .wait()
        .unwrap();
    ruro_proto::request(
        &session,
        &Keys::new(&name),
        Ctl::Deploy,
        "name: f\nnodes:\n  a: { type: t/x, rate: 1 }\n",
        Duration::from_secs(10),
    )
    .unwrap();
    wait_status(&session, &name, |s| {
        s.groups.first().and_then(|g| g.last_exit.as_deref())
            == Some("exit code 1: failed to open topic `ruro/x`: DoesNotExist")
    });
    let sample = logs
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .expect("the exit is logged");
    let line: ruro_proto::LogLine = ruro_proto::decode(&sample.payload().to_bytes()).unwrap();
    assert_eq!(line.level, ruro_proto::Level::Error);
    assert!(line.message.contains("failed to open topic"), "{line:?}");

    drop(d);
    let _ = std::fs::remove_dir_all(ruro_daemon::runtime_dir(&name));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_emergency_stop_does_not_wait_for_other_requests() {
    let dir = std::env::temp_dir().join(format!("ruro-daemon-quick-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let host = fake_host(&dir);
    let port = free_port();
    let tag = std::process::id();
    let (name, gone) = (format!("quick{tag}"), format!("quickghost{tag}"));
    let d = daemon(&name, port, Vec::new(), &host);
    let session = ruro_proto::connect(&format!("tcp/127.0.0.1:{port}")).unwrap();
    let keys = Keys::new(&name);
    // A machine that takes requests and never answers them.
    let held = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let keep = held.clone();
    let _ghost = session
        .declare_queryable(format!("ruro/{gone}/ctl/*"))
        .callback(move |q| keep.lock().unwrap().push(q))
        .wait()
        .unwrap();

    // A deploy to it keeps the daemon busy until the request times out.
    let graph = format!(
        "name: slow\ngroups:\n  far: {{ machine: {gone} }}\nnodes:\n  a: {{ type: t/x, group: far, rate: 1 }}\n"
    );
    std::thread::scope(|s| {
        s.spawn(|| {
            let started = Instant::now();
            let r = ruro_proto::request(
                &session,
                &keys,
                Ctl::Deploy,
                &graph,
                Duration::from_secs(30),
            );
            assert!(
                r.is_err() && started.elapsed() > Duration::from_secs(2),
                "{r:?}"
            );
        });
        std::thread::sleep(Duration::from_millis(300));
        let start = Instant::now();
        let reply = ruro_proto::request(
            &session,
            &keys,
            Ctl::Estop,
            "engage",
            Duration::from_secs(30),
        );
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "the stop waited {:?} for the deploy",
            start.elapsed()
        );
        assert_eq!(reply.unwrap(), "e-stop engaged");
    });

    // Requests sent back to back without waiting for replies apply in
    // order: the last one, an engage, wins, and what the daemon publishes
    // agrees with its latch.
    let flags = session.declare_subscriber(keys.estop()).wait().unwrap();
    let pending: Vec<_> = (0..40)
        .flat_map(|_| ["release", "engage"])
        .map(|op| {
            session
                .get(keys.ctl(Ctl::Estop))
                .payload(op)
                .timeout(Duration::from_secs(10))
                .wait()
                .unwrap()
        })
        .collect();
    for replies in pending {
        let _ = replies.recv();
    }
    wait_status(&session, &name, |s| s.estopped);
    // The flag is republished twice a second; the next ones must say engaged.
    let start = Instant::now();
    let mut seen = 0;
    while seen < 3 {
        let sample = flags.recv_timeout(Duration::from_secs(2)).unwrap().unwrap();
        let flag =
            <ruro_msg::Flag as ruro_msg::Message>::decode(&sample.payload().to_bytes()).unwrap();
        if start.elapsed() > Duration::from_millis(100) {
            assert!(flag.0, "published released while the daemon is engaged");
            seen += 1;
        }
    }

    drop(d);
    let _ = std::fs::remove_dir_all(ruro_daemon::runtime_dir(&name));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_machine_that_missed_the_stop_catches_up() {
    let dir = std::env::temp_dir().join(format!("ruro-daemon-missed-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let host = fake_host(&dir);
    let (robot_port, laptop_port) = (free_port(), free_port());
    let tag = std::process::id();
    let (robot, laptop) = (format!("mrobot{tag}"), format!("mlaptop{tag}"));
    let robot_daemon = daemon(&robot, robot_port, Vec::new(), &host);
    let laptop_daemon = daemon(
        &laptop,
        laptop_port,
        vec![format!("tcp/127.0.0.1:{robot_port}")],
        &host,
    );
    let session = ruro_proto::connect(&format!("tcp/127.0.0.1:{laptop_port}")).unwrap();
    let request = |op, payload: &str| {
        ruro_proto::request(
            &session,
            &Keys::new(&laptop),
            op,
            payload,
            Duration::from_secs(10),
        )
    };
    let graph = format!(
        "name: span\ngroups:\n  far: {{ machine: {robot} }}\nnodes:\n  a: {{ type: t/x, group: far, rate: 1 }}\n  b: {{ type: t/x, rate: 1 }}\n"
    );
    request(Ctl::Deploy, &graph).unwrap();

    // The robot is away (cut off, or restarting) when the stop is engaged.
    drop(robot_daemon);
    let _ = std::fs::remove_dir_all(ruro_daemon::runtime_dir(&robot));
    assert!(
        request(Ctl::Estop, "engage").is_err(),
        "the robot did not get it"
    );
    let _robot_back = daemon(&robot, robot_port, Vec::new(), &host);
    // It gets the stop once it is back, without anyone asking again.
    wait_status(&session, &robot, |s| s.estopped);

    // And a release it misses reaches it the same way.
    request(Ctl::Estop, "release").unwrap();
    wait_status(&session, &robot, |s| !s.estopped);

    drop(laptop_daemon);
    for name in [&robot, &laptop] {
        let _ = std::fs::remove_dir_all(ruro_daemon::runtime_dir(name));
    }
    let _ = std::fs::remove_dir_all(&dir);
}
