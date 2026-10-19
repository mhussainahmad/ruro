//! `ruro update` against real `ruro daemon` processes: an update to a newer
//! build (the same binary, with the running daemon made to report an older
//! version), the refusal of a binary that does not start, and the restart
//! through a service manager.
//!
//! Each test runs its daemon from a copy of the `ruro` binary in a
//! directory of its own, so the update never replaces cargo's build.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use ruro_proto::{DaemonStatus, Keys};
use zenoh::Wait;

const RURO: &str = env!("CARGO_BIN_EXE_ruro");
const OLD: &str = "0.0.1";
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Fleet {
    dir: PathBuf,
    name: String,
    endpoint: String,
    daemon: Option<Child>,
    /// Other daemons this one connects to.
    peers: Vec<String>,
}

impl Fleet {
    fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("ruro-cli-update-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::copy(RURO, dir.join("bin/ruro")).unwrap();
        let host = dir.join("bin/fake-host");
        std::fs::write(
            &host,
            "#!/bin/sh\nif [ \"$1\" = describe ]; then echo '{\"nodes\":{\"t/x\":{\"kind\":\"t/x\"}}}'; exit 0; fi\nexec sleep 30\n",
        )
        .unwrap();
        std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o755)).unwrap();
        let f = Self {
            name: format!("ota{tag}{}", std::process::id()),
            endpoint: format!("tcp/127.0.0.1:{}", free_port()),
            dir,
            daemon: None,
            peers: Vec::new(),
        };
        let out = f.cli(&["keygen", f.path("signing").to_str().unwrap()]);
        assert!(out.status.success(), "{}", text(&out));
        std::fs::copy(f.path("signing.pub"), f.path("update-keys")).unwrap();
        f
    }

    fn path(&self, p: &str) -> PathBuf {
        self.dir.join(p)
    }

    /// Starts the daemon from the copy in `bin`, reporting `version`.
    fn start(&mut self, version: Option<&str>, env: &[(&str, &str)]) {
        let mut cmd = Command::new(self.path("bin/ruro"));
        cmd.args(["daemon", "--name", &self.name, "--listen", &self.endpoint])
            .arg("--host")
            .arg(self.path("bin/fake-host"))
            .arg("--update-keys")
            .arg(self.path("update-keys"))
            .args(self.peers.iter().flat_map(|p| ["--peer", p.as_str()]))
            .env_remove("RURO_RESTART")
            .stdin(Stdio::null());
        if let Some(v) = version {
            cmd.env("RURO_VERSION_OVERRIDE", v);
        }
        cmd.envs(env.iter().copied());
        self.daemon = Some(cmd.spawn().unwrap());
        self.wait_status(|_| true);
    }

    /// Runs a client command against the daemon.
    fn cli(&self, args: &[&str]) -> Output {
        Command::new(RURO)
            .args(["--connect", &self.endpoint, "--daemon", &self.name])
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    /// A copy of `src` in `dir`, signed as `version` (or as what it reports).
    fn signed(&self, src: &Path, dir: &str, version: Option<&str>) -> PathBuf {
        std::fs::create_dir_all(self.path(dir)).unwrap();
        let bin = self.path(dir).join("ruro");
        std::fs::copy(src, &bin).unwrap();
        let mut args = vec![
            "sign".to_owned(),
            "--key".into(),
            self.path("signing.key").display().to_string(),
            bin.display().to_string(),
        ];
        if let Some(v) = version {
            args.extend(["--binary-version".into(), v.into()]);
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = self.cli(&args);
        assert!(out.status.success(), "{}", text(&out));
        bin
    }

    fn wait_status(&self, done: impl Fn(&DaemonStatus) -> bool) -> DaemonStatus {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut last = None;
        while Instant::now() < deadline {
            let Ok(session) = ruro_proto::connect(&self.endpoint) else {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            };
            let sub = session
                .declare_subscriber(Keys::new(&self.name).status())
                .wait()
                .unwrap();
            if let Ok(Some(s)) = sub.recv_timeout(Duration::from_secs(1)) {
                let st = ruro_proto::decode_status(&s.payload().to_bytes()).unwrap();
                if done(&st) {
                    return st;
                }
                last = Some(st);
            }
        }
        panic!("status never matched; last: {last:#?}");
    }
}

impl Drop for Fleet {
    fn drop(&mut self) {
        if let Some(mut d) = self.daemon.take() {
            let _ = d.kill();
            let _ = d.wait();
        }
        let _ = std::fs::remove_dir_all(ruro_daemon::runtime_dir(&self.name));
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn daemons_update_in_place_and_refuse_binaries_that_do_not_start() {
    let mut f = Fleet::new("inplace");
    f.start(Some(OLD), &[]);
    let pid = f.daemon.as_ref().unwrap().id();
    let new = f.signed(Path::new(RURO), "new", None);
    let new_bin = new.display().to_string();

    // Not while a graph runs.
    let graph = f.path("g.yaml");
    std::fs::write(&graph, "name: busy\nnodes:\n  a: { type: t/x, rate: 1 }\n").unwrap();
    let out = f.cli(&["deploy", graph.to_str().unwrap()]);
    assert!(out.status.success(), "{}", text(&out));
    let out = f.cli(&["update", &new_bin]);
    assert!(!out.status.success());
    assert!(
        text(&out).contains("graph `busy` is running"),
        "{}",
        text(&out)
    );
    assert!(f.cli(&["stop"]).status.success());

    // The same build, newer than what the daemon claims to run.
    let out = f.cli(&["update", &new_bin, "--timeout", "20"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains(&format!("runs {VERSION}")),
        "{}",
        text(&out)
    );
    let st = f.wait_status(|s| s.version == VERSION);
    assert_eq!(st.pid, pid, "executed in place");
    assert_eq!(st.target, ruro_daemon::TARGET);
    assert_eq!(
        st.update.as_deref(),
        Some(format!("updated from {OLD} to {VERSION}").as_str())
    );
    assert!(
        f.path("bin/ruro.prev").exists(),
        "the previous binary is kept"
    );
    let status = text(&f.cli(&["status"]));
    assert!(
        status.contains(&format!("v{VERSION} ({})", ruro_daemon::TARGET))
            && status.contains("last update: updated from"),
        "{status}"
    );

    // A binary that exits at once is tried and refused; nothing changes.
    let installed = std::fs::read(f.path("bin/ruro")).unwrap();
    let falsy = ["/bin/false", "/usr/bin/false"]
        .into_iter()
        .find(|p| Path::new(p).exists())
        .unwrap();
    let broken = f.signed(Path::new(falsy), "broken", Some("99.0.0"));
    let out = f.cli(&["update", broken.to_str().unwrap(), "--timeout", "10"]);
    assert!(!out.status.success());
    let t = text(&out);
    assert!(
        t.contains("failed its trial start")
            && t.contains("exited")
            && t.contains("nothing was replaced"),
        "{t}"
    );
    assert_eq!(std::fs::read(f.path("bin/ruro")).unwrap(), installed);
    let st = f.wait_status(|_| true);
    assert_eq!((st.version.as_str(), st.pid), (VERSION, pid));

    // Downgrades need saying so.
    let old = f.signed(Path::new(RURO), "old", Some(OLD));
    let out = f.cli(&["update", old.to_str().unwrap(), "--check"]);
    assert!(text(&out).contains("not newer"), "{}", text(&out));
    let out = f.cli(&[
        "update",
        old.to_str().unwrap(),
        "--check",
        "--allow-downgrade",
    ]);
    assert!(out.status.success(), "{}", text(&out));
}

#[test]
fn an_updated_daemon_that_cannot_start_puts_the_previous_binary_back() {
    let mut f = Fleet::new("rollback");
    // As left by a daemon that installed an update and executed it: the
    // installed binary differs from the kept one (by a trailing byte).
    let (bin, prev) = (f.path("bin/ruro"), f.path("bin/ruro.prev"));
    std::fs::copy(&bin, &prev).unwrap();
    let previous = std::fs::read(&prev).unwrap();
    let mut installed = previous.clone();
    installed.push(0);
    std::fs::write(&bin, &installed).unwrap();
    let handoff = ruro_daemon::runtime_dir(&f.name).join("update-handoff.json");
    std::fs::create_dir_all(handoff.parent().unwrap()).unwrap();
    let json = serde_json::json!({
        "from": OLD, "to": VERSION, "estop": false,
        "replaced": [[bin, prev]], "note": null,
    });
    std::fs::write(&handoff, json.to_string()).unwrap();

    // Its endpoint is taken, so it cannot start; nor can the previous
    // binary, which it then runs, and which reports why.
    let taken = std::net::TcpListener::bind(f.endpoint.trim_start_matches("tcp/")).unwrap();
    let out = Command::new(&bin)
        .args(["daemon", "--name", &f.name, "--listen", &f.endpoint])
        .arg("--host")
        .arg(f.path("bin/fake-host"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        text(&out).contains("rolled back to 0.0.1"),
        "{}",
        text(&out)
    );
    assert_eq!(
        std::fs::read(&bin).unwrap(),
        previous,
        "the previous binary is back"
    );
    assert!(!prev.exists());

    // Once it can start, it says what happened.
    drop(taken);
    f.start(None, &[]);
    let st = f.wait_status(|_| true);
    let note = st.update.unwrap_or_default();
    assert!(
        note.starts_with(&format!("rolled back to {OLD}: {VERSION} did not start")),
        "{note}"
    );
}

#[test]
fn under_a_service_manager_the_daemon_exits_to_be_restarted() {
    let mut f = Fleet::new("service");
    let service = [("RURO_RESTART", "exit")];
    f.start(Some(OLD), &service);
    // Engaged before the update: it stays engaged after.
    assert!(f.cli(&["estop"]).status.success());
    let new = f.signed(Path::new(RURO), "new", None);
    let update = Command::new(RURO)
        .args(["--connect", &f.endpoint, "--daemon", &f.name])
        .args(["update", new.to_str().unwrap(), "--timeout", "20"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    // Acting as systemd: the daemon exits with 75 and is started again.
    let mut daemon = f.daemon.take().unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(s) = daemon.try_wait().unwrap() {
            break s;
        }
        assert!(Instant::now() < deadline, "the daemon did not exit");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(status.code(), Some(ruro_daemon::update::EXIT_RESTART));
    f.start(None, &service);

    let out = update.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", text(&out));
    let st = f.wait_status(|s| s.version == VERSION);
    assert!(st.estopped, "the e-stop is carried over");
    assert!(
        st.update
            .as_deref()
            .unwrap_or_default()
            .contains("updated from"),
        "{st:?}"
    );
}

#[test]
fn peers_are_updated_before_the_daemon_they_are_reached_through() {
    let mut laptop = Fleet::new("laptop");
    let mut robot = Fleet::new("robot");
    // Both trust the laptop's key; the robot connects to the laptop.
    std::fs::copy(laptop.path("update-keys"), robot.path("update-keys")).unwrap();
    laptop.start(Some(OLD), &[]);
    robot.peers = vec![laptop.endpoint.clone()];
    robot.start(Some(OLD), &[]);
    let new = laptop.signed(Path::new(RURO), "new", None);

    let out = laptop.cli(&[
        "update",
        new.to_str().unwrap(),
        "--peers",
        "--timeout",
        "20",
    ]);
    let t = text(&out);
    assert!(out.status.success(), "{t}");
    let (r, l) = (
        t.find(&format!("`{}` runs {VERSION}", robot.name)),
        t.find(&format!("`{}` runs {VERSION}", laptop.name)),
    );
    assert!(r.is_some() && r < l, "the robot first: {t}");
    laptop.wait_status(|s| s.version == VERSION);
    robot.wait_status(|s| s.version == VERSION);
    // The robot is reachable through the restarted laptop again.
    let fleet = text(&laptop.cli(&["fleet"]));
    assert!(fleet.contains(&robot.name), "{fleet}");
}
