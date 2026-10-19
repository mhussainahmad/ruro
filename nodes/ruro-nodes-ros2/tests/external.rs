//! `ros2/node` supervising stand-in programs (shell scripts), so that it is
//! tested without ROS: arguments and environment, restarts, process group
//! shutdown and the lookup of `lib/<package>/<executable>`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ruro::node::testing::Harness;
use serde_json::json;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ruro-ros2-node-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn wait_for_file(path: &Path) -> String {
    let start = Instant::now();
    loop {
        if let Ok(s) = std::fs::read_to_string(path)
            && s.ends_with('\n')
        {
            return s;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "{} never written",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[test]
fn ros_arguments_and_environment_reach_the_process() {
    ruro_nodes_ros2::link();
    let d = dir("args");
    let out = d.join("out");
    let script = format!(
        "printf '%s|' \"$@\" > {o}.tmp; echo \"$RMW_IMPLEMENTATION|$ZENOH_CONFIG_OVERRIDE|$ROS_DOMAIN_ID|$EXTRA\" >> {o}.tmp; mv {o}.tmp {o}; sleep 30",
        o = out.display()
    );
    let mut h = Harness::new(
        "ros2/node",
        json!({
            "command": format!("sh -c {} stub", ruro_ros2::external::quote_word(&script)),
            "name": "talker",
            "namespace": "/robot",
            "remaps": "chatter:=/out\n",
            "parameters": "{\"rate\": 2, \"mode\": \"true\"}",
            "env": "EXTRA=yes",
            "router": "tcp/127.0.0.1:7999",
            "domain_id": 7
        }),
    )
    .unwrap();
    h.step().unwrap();
    // One line: the arguments, then the environment.
    let text = wait_for_file(&out);
    let args_line = text.trim_end();
    assert!(
        args_line.starts_with(
            "--ros-args|-r|__node:=talker|-r|__ns:=/robot|-r|chatter:=/out|-p|rate:=2|-p|mode:=\"true\"|"
        ),
        "{text}"
    );
    assert!(
        args_line.ends_with(
            "rmw_zenoh_cpp|connect/endpoints=[\"tcp/127.0.0.1:7999\"];scouting/multicast/enabled=false|7|yes"
        ),
        "{text}"
    );
    // Still running: steps report no problem.
    h.step().unwrap();
}

#[test]
fn crashes_are_restarted_and_stop_kills_the_process_group() {
    ruro_nodes_ros2::link();
    let d = dir("restart");
    let count = d.join("count");
    // Exits with an error twice, then stays up with a grandchild.
    let script = format!(
        "echo run >> {c}; n=$(wc -l < {c}); if [ $n -lt 3 ]; then echo crash >&2; exit 3; fi; \
         sleep 300 & echo $$ $! > {c}.pids; wait",
        c = count.display()
    );
    let pids_file = d.join("count.pids");
    let mut h = Harness::new(
        "ros2/node",
        json!({
            "command": format!("sh -c {}", ruro_ros2::external::quote_word(&script)),
            "respawn": "on-failure",
            "respawn_delay": 0.1
        }),
    )
    .unwrap();
    h.step().unwrap();
    let start = Instant::now();
    let mut saw_error = false;
    let pids = loop {
        if h.step().is_err_and(|e| format!("{e:#}").contains("exited")) {
            saw_error = true;
        }
        if let Ok(p) = std::fs::read_to_string(&pids_file)
            && p.ends_with('\n')
        {
            break p;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "never stayed up");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(saw_error, "steps report the crashes");
    assert_eq!(std::fs::read_to_string(&count).unwrap().lines().count(), 3);
    let pids: Vec<i32> = pids
        .split_whitespace()
        .map(|p| p.parse().unwrap())
        .collect();
    assert!(pids.iter().all(|p| alive(*p)));
    h.step().unwrap();
    drop(h);
    // Both the process and the child it started are gone.
    let start = Instant::now();
    while pids.iter().any(|p| alive(*p)) {
        assert!(start.elapsed() < Duration::from_secs(10), "still running");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn executables_are_found_in_a_sourced_workspace() {
    ruro_nodes_ros2::link();
    let d = dir("ws");
    let exe_dir = d.join("install/lib/my_pkg");
    std::fs::create_dir_all(&exe_dir).unwrap();
    let out = d.join("out");
    let exe = exe_dir.join("my_node");
    std::fs::write(
        &exe,
        format!(
            "#!/bin/sh\necho \"$0 $* $FROM_SETUP\" > {o}.tmp; mv {o}.tmp {o}\n",
            o = out.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let setup = d.join("setup.bash");
    std::fs::write(
        &setup,
        format!(
            "export AMENT_PREFIX_PATH={}\nexport FROM_SETUP=sourced\n",
            d.join("install").display()
        ),
    )
    .unwrap();
    let mut h = Harness::new(
        "ros2/node",
        json!({
            "package": "my_pkg", "executable": "my_node",
            "arguments": "--flag", "ros_setup": setup.display().to_string(),
            "respawn": "never"
        }),
    )
    .unwrap();
    h.step().unwrap();
    assert_eq!(
        wait_for_file(&out),
        format!("{} --flag sourced\n", exe.display())
    );
    // A clean exit with respawn: never is not an error.
    std::thread::sleep(Duration::from_millis(300));
    h.step().unwrap();

    let mut missing = Harness::new(
        "ros2/node",
        json!({ "package": "nope", "executable": "x", "ros_setup": setup.display().to_string() }),
    )
    .unwrap();
    let err = format!("{:#}", missing.step().unwrap_err());
    assert!(err.contains("not found in AMENT_PREFIX_PATH"), "{err}");
    assert!(
        Harness::new("ros2/node", json!({}))
            .unwrap()
            .step()
            .is_err()
    );
}
