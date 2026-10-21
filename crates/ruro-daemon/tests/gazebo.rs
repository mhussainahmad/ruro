//! The rover example against Gazebo Harmonic through ros_gz_bridge and the
//! ROS 2 bridge: scans and odometry arrive, the graph follows Gazebo's
//! simulated time, and the avoider's velocity commands move the simulated
//! rover.
//!
//! Opt in with `RURO_GAZEBO_CONTAINER`: a command prefix that runs a program
//! with ROS 2 Jazzy, rmw_zenoh and ros_gz available, host networking and this
//! repository's `examples/gazebo` at the same path, for example
//! `podman run --rm --init --network host -v $PWD/examples:$PWD/examples:z ruro-gazebo`
//! (image from `examples/gazebo/Containerfile`), or `native` to run
//! `examples/gazebo/run.sh` directly. Needs `ruro-host` built in the target
//! directory (`cargo build -p ruro-host`) or named by `$RURO_HOST`.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use ruro_graph::Graph;
use ruro_proto::{Ctl, Keys, Preview, TapFrame};
use zenoh::Wait;

/// Stops the simulator when the test ends, however it ends.
struct Sim(Child);

impl Drop for Sim {
    fn drop(&mut self) {
        // SIGTERM lets `podman run` stop the container and run.sh its children.
        unsafe { libc::kill(self.0.id() as i32, libc::SIGTERM) };
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(15) {
            if let Ok(Some(_)) = self.0.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .canonicalize()
        .unwrap()
}

#[test]
fn the_rover_example_drives_in_gazebo() {
    let Some(prefix) = std::env::var_os("RURO_GAZEBO_CONTAINER") else {
        eprintln!("skipped: set RURO_GAZEBO_CONTAINER to run against Gazebo");
        return;
    };
    let prefix = prefix.to_string_lossy().into_owned();
    let host = std::env::var_os("RURO_HOST")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            // target/<profile>/deps/<test> -> target/<profile>/ruro-host
            let exe = std::env::current_exe().unwrap();
            exe.parent().unwrap().parent().unwrap().join("ruro-host")
        });
    assert!(host.exists(), "no host binary at {}", host.display());

    let port = free_port();
    let endpoint = format!("tcp/127.0.0.1:{port}");
    let name = format!("gazebo{}", std::process::id());
    let mut config = ruro_daemon::Config::new();
    config.name = name.clone();
    config.listen = endpoint.clone();
    config.host = host;
    // Dropped last, so the hosts stop after the simulator.
    let daemon = ruro_daemon::Daemon::start(config).expect("daemon starts");

    let run = examples().join("gazebo/run.sh");
    let command = if prefix == "native" {
        format!("exec {} {endpoint}", run.display())
    } else {
        format!("exec {prefix} {} {endpoint}", run.display())
    };
    let sim = Sim(Command::new("sh")
        .args(["-c", &command])
        .spawn()
        .expect("simulator starts"));

    // The example graph, with its bridge nodes pointed at this daemon.
    let mut graph = Graph::load(examples().join("gazebo/rover.yaml")).unwrap();
    for node in graph.nodes.values_mut() {
        if node.params.contains_key("router") {
            node.params
                .insert("router".into(), serde_json::json!(endpoint));
        }
    }
    let session = ruro_proto::connect(&endpoint).unwrap();
    let keys = Keys::new(&name);
    let reply = ruro_proto::request(
        &session,
        &keys,
        Ctl::Deploy,
        &graph.to_yaml().unwrap(),
        Duration::from_secs(10),
    )
    .expect("deploys");
    eprintln!("{reply}");

    let tap = |node: &str, port: &str| {
        session
            .declare_subscriber(keys.tap(node, port))
            .wait()
            .unwrap()
    };
    let (scans, odom, clock) = (tap("lidar", "out"), tap("odom", "out"), tap("clock", "out"));
    let frame = |s: &zenoh::sample::Sample| -> TapFrame {
        ruro_proto::decode(&s.payload().to_bytes()).unwrap()
    };

    // Gazebo takes a while to start, more so in a fresh container.
    let start = Instant::now();
    let mut scan_points = 0;
    let mut first: Option<[f32; 3]> = None;
    let mut moved = 0.0_f32;
    let mut sim_time: Option<f64> = None;
    while start.elapsed() < Duration::from_secs(120)
        && (scan_points == 0 || moved < 0.5 || sim_time.is_none())
    {
        while let Ok(Some(s)) = clock.try_recv() {
            if let Preview::Values(v) = frame(&s).preview
                && let Some((_, ns)) = v.first()
            {
                sim_time = Some(ns / 1e9);
            }
        }
        while let Ok(Some(s)) = scans.try_recv() {
            if let Preview::Points { points, .. } = frame(&s).preview {
                scan_points = scan_points.max(points.len());
            }
        }
        while let Ok(Some(s)) = odom.try_recv() {
            if let Preview::Frames(frames) = frame(&s).preview
                && let Some(f) = frames.first()
            {
                let p = f.translation;
                let p0 = *first.get_or_insert(p);
                moved = moved.max(((p[0] - p0[0]).powi(2) + (p[1] - p0[1]).powi(2)).sqrt());
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    eprintln!(
        "after {:.1} s: {scan_points} scan points, odometry from {first:?}, moved {moved:.2} m, \
         simulated time {sim_time:?} s",
        start.elapsed().as_secs_f32()
    );
    // Gazebo's time starts at zero when the world loads.
    let sim_time = sim_time.expect("no /clock arrived");
    assert!(sim_time < 600.0, "not simulated time: {sim_time} s");
    assert!(scan_points > 100, "no laser scans arrived");
    assert!(first.is_some(), "no odometry arrived");
    assert!(moved >= 0.5, "the rover did not move: {moved} m");

    let _ = ruro_proto::request(&session, &keys, Ctl::Stop, "", Duration::from_secs(10));
    drop(sim);
    daemon.shutdown();
}
