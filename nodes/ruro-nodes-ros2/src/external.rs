//! `ros2/node`: an existing ROS 2 node, run and supervised by ruro.
//!
//! The node starts the ROS 2 executable (`lib/<package>/<executable>` of a
//! prefix in `AMENT_PREFIX_PATH`, as `ros2 run` finds it, or any command)
//! as a child process with `RMW_IMPLEMENTATION=rmw_zenoh_cpp` and a Zenoh
//! configuration that connects it to the router the bridge nodes use, so
//! ROS topics of the child and ruro's `ros2/*` nodes meet. The child's
//! output goes to the ruro log of this node, it is restarted according to
//! `respawn`, and it is stopped (SIGTERM to its process group, SIGKILL after
//! 5 s) when the graph stops. If the ruro host dies, the child gets SIGTERM.

use std::collections::BTreeMap;
use std::io::BufRead;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ruro::prelude::*;
use ruro_ros2::external::{RosProcess, find_executable, find_package_share};

/// When a child that exited is started again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Respawn {
    Never,
    OnFailure,
    Always,
}

impl std::str::FromStr for Respawn {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "never" | "false" | "no" => Ok(Self::Never),
            "on-failure" | "" => Ok(Self::OnFailure),
            "always" | "true" | "yes" => Ok(Self::Always),
            other => {
                bail!("parameter `respawn`: expected never, on-failure or always, got `{other}`")
            }
        }
    }
}

/// What to run, fully resolved.
#[derive(Debug, Clone, PartialEq)]
struct Launch {
    argv: Vec<String>,
    env: BTreeMap<String, String>,
    respawn: Respawn,
    /// Fixed delay before a restart; `None` backs off like the daemon.
    delay: Option<Duration>,
    label: String,
}

/// The environment of `ros_setup` sourced in bash, or this process's.
fn base_env(ros_setup: &str) -> Result<BTreeMap<String, String>> {
    if ros_setup.trim().is_empty() {
        return Ok(std::env::vars().collect());
    }
    let out = Command::new("bash")
        .arg("-c")
        .arg("source \"$0\" >/dev/null 2>&1 && env -0")
        .arg(ros_setup.trim())
        .stdin(Stdio::null())
        .output()
        .context("running bash to source `ros_setup`")?;
    if !out.status.success() {
        bail!("sourcing {} failed ({})", ros_setup, out.status);
    }
    Ok(out
        .stdout
        .split(|b| *b == 0)
        .filter_map(|kv| {
            let kv = String::from_utf8_lossy(kv);
            kv.split_once('=')
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
        })
        .collect())
}

/// Replaces `$(find-pkg-share P)` and `$(find-pkg-prefix P)`, which
/// `ruro ros2 import` leaves when it cannot resolve them.
fn resolve_packages(s: &str, ament: &str) -> Result<String> {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("$(") {
        out.push_str(&rest[..i]);
        let end = rest[i..]
            .find(')')
            .map(|e| i + e)
            .ok_or_else(|| anyhow::anyhow!("unterminated `$(` in `{s}`"))?;
        let words: Vec<&str> = rest[i + 2..end].split_whitespace().collect();
        let share = |pkg: &str| {
            find_package_share(ament, pkg).ok_or_else(|| {
                anyhow::anyhow!("package `{pkg}` is not in AMENT_PREFIX_PATH (`{s}`)")
            })
        };
        match words.as_slice() {
            ["find-pkg-share" | "find_pkg_share", pkg] => {
                out.push_str(&share(pkg)?.display().to_string())
            }
            ["find-pkg-prefix" | "find_pkg_prefix", pkg] => {
                let p = share(pkg)?;
                let prefix = p.parent().and_then(|p| p.parent()).unwrap_or(&p);
                out.push_str(&prefix.display().to_string());
            }
            _ => out.push_str(&rest[i..=end]),
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// The Zenoh session settings rmw_zenoh reads from `ZENOH_CONFIG_OVERRIDE`.
fn zenoh_override(router: &str, token: Option<&str>) -> String {
    let mut s = format!(
        "connect/endpoints=[{}];scouting/multicast/enabled=false",
        ruro::serde_json::to_string(router).expect("JSON")
    );
    if let Some(t) = token {
        s.push_str(&format!(
            ";transport/auth/usrpwd/user=\"ruro\";transport/auth/usrpwd/password={}",
            ruro::serde_json::to_string(t).expect("JSON")
        ));
    }
    s
}

/// Live state of a supervised child.
#[derive(Debug, Default)]
struct Status {
    pid: Option<u32>,
    restarts: u32,
    /// Why the child is not running, while it is not.
    problem: Option<String>,
    /// The child exited and will not be restarted.
    done: bool,
}

/// Runs and restarts a child on its own thread.
struct Supervisor {
    launch: Launch,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<Status>>,
    thread: Option<JoinHandle<()>>,
}

fn log_line(label: &str, line: &str, stderr: bool) {
    let upper = line.trim_start();
    if upper.starts_with("[ERROR]") || upper.starts_with("[FATAL]") {
        error!("{line}");
    } else if upper.starts_with("[WARN]") {
        warn!("{line}");
    } else if stderr && !upper.starts_with('[') {
        // Unformatted stderr (crash messages and the like).
        warn!("{label}: {line}");
    } else {
        info!("{line}");
    }
}

fn follow(
    thread_name: &str,
    label: String,
    pipe: impl std::io::Read + Send + 'static,
    stderr: bool,
) -> Option<JoinHandle<()>> {
    std::thread::Builder::new()
        .name(thread_name.to_owned())
        .spawn(move || {
            for line in std::io::BufReader::new(pipe).lines() {
                match line {
                    Ok(l) => log_line(&label, &l, stderr),
                    Err(_) => break,
                }
            }
        })
        .ok()
}

fn signal_group(pid: u32, sig: i32) {
    // SAFETY: kill(2) on the process group the child leads.
    unsafe {
        libc::kill(-(pid as i32), sig);
    }
}

/// Stops a child: SIGTERM to its group, SIGKILL after `grace`.
fn terminate(child: &mut Child, grace: Duration) {
    let pid = child.id();
    signal_group(pid, libc::SIGTERM);
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            // Leftover members of the group go too.
            signal_group(pid, libc::SIGKILL);
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    signal_group(pid, libc::SIGKILL);
    let _ = child.wait();
}

impl Supervisor {
    fn start(id: &str, launch: Launch) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let status = Arc::new(Mutex::new(Status::default()));
        let thread_name = format!("ruro:{id}");
        let (l, s, st) = (launch.clone(), stop.clone(), status.clone());
        let name = thread_name.clone();
        let thread = std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || supervise(&name, l, s, st))
            .context("starting the supervisor thread")?;
        Ok(Self {
            launch,
            stop,
            status,
            thread: Some(thread),
        })
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Sleeps up to `d`, returning early (true) when asked to stop.
fn pause(stop: &AtomicBool, d: Duration) -> bool {
    let end = Instant::now() + d;
    while Instant::now() < end {
        if stop.load(Ordering::Relaxed) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    stop.load(Ordering::Relaxed)
}

fn supervise(thread_name: &str, launch: Launch, stop: Arc<AtomicBool>, status: Arc<Mutex<Status>>) {
    let mut streak = 0u32;
    loop {
        let mut cmd = Command::new(&launch.argv[0]);
        cmd.args(&launch.argv[1..])
            .env_clear()
            .envs(&launch.env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        // SAFETY: prctl is async-signal-safe. The child gets SIGTERM if this
        // thread (and so the host) goes away.
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        let started = Instant::now();
        let outcome = match cmd.spawn() {
            Ok(mut child) => {
                let pid = child.id();
                info!(
                    "started {} (pid {pid}): {}",
                    launch.label,
                    launch.argv.join(" ")
                );
                {
                    let mut s = status.lock().unwrap();
                    s.pid = Some(pid);
                    s.problem = None;
                }
                let readers: Vec<JoinHandle<()>> = [
                    child
                        .stdout
                        .take()
                        .and_then(|p| follow(thread_name, launch.label.clone(), p, false)),
                    child
                        .stderr
                        .take()
                        .and_then(|p| follow(thread_name, launch.label.clone(), p, true)),
                ]
                .into_iter()
                .flatten()
                .collect();
                let exit = loop {
                    if stop.load(Ordering::Relaxed) {
                        terminate(&mut child, Duration::from_secs(5));
                        break None;
                    }
                    match child.try_wait() {
                        Ok(Some(st)) => {
                            // Children it left behind in its group.
                            signal_group(pid, libc::SIGTERM);
                            break Some(st);
                        }
                        Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                        Err(e) => {
                            warn!("waiting for {}: {e}", launch.label);
                            std::thread::sleep(Duration::from_millis(100));
                        }
                    }
                };
                for r in readers {
                    let _ = r.join();
                }
                status.lock().unwrap().pid = None;
                match exit {
                    None => {
                        info!("stopped {}", launch.label);
                        return;
                    }
                    Some(st) => Ok(st),
                }
            }
            Err(e) => Err(format!("cannot start {}: {e}", launch.argv[0])),
        };
        let failed = match &outcome {
            Ok(st) => !st.success(),
            Err(_) => true,
        };
        let what = match &outcome {
            Ok(st) => format!("{} exited ({st})", launch.label),
            Err(e) => e.clone(),
        };
        let restart = match launch.respawn {
            Respawn::Always => true,
            Respawn::OnFailure => failed,
            Respawn::Never => false,
        };
        if !restart {
            if failed {
                error!("{what}; not restarted (respawn: {:?})", launch.respawn);
            } else {
                info!("{what}");
            }
            let mut s = status.lock().unwrap();
            s.done = true;
            s.problem = failed.then_some(what);
            return;
        }
        // Back off like the daemon does for host processes.
        if started.elapsed() >= Duration::from_secs(10) {
            streak = 0;
        }
        streak += 1;
        let delay = launch.delay.unwrap_or_else(|| {
            if streak == 1 && started.elapsed() >= Duration::from_secs(10) {
                Duration::from_millis(200)
            } else {
                Duration::from_millis(200 * 2u64.pow(streak.min(5))).min(Duration::from_secs(5))
            }
        });
        warn!("{what}; restarting in {:.1} s", delay.as_secs_f64());
        {
            let mut s = status.lock().unwrap();
            s.restarts += 1;
            s.problem = Some(what);
        }
        if pause(&stop, delay) {
            return;
        }
    }
}

/// Runs a ROS 2 node (or any program) as a supervised child process.
#[derive(Node)]
#[node(
    kind = "ros2/node",
    rate = 1,
    doc = "Runs an existing ROS 2 node (`ros2 run <package> <executable>`, or any command) as a supervised child process connected to the bridge's Zenoh router through rmw_zenoh, with ROS arguments, restarts and its output in the ruro log."
)]
pub struct RosNode {
    /// ROS package of the executable.
    #[param(default = "")]
    package: String,
    /// Executable in `lib/<package>/` of a prefix in AMENT_PREFIX_PATH.
    #[param(default = "")]
    executable: String,
    /// Any command line to run instead of package/executable.
    #[param(default = "")]
    command: String,
    /// ROS node name (`-r __node:=`); empty keeps the executable's.
    #[param(default = "")]
    name: String,
    /// ROS namespace (`-r __ns:=`).
    #[param(default = "")]
    namespace: String,
    /// Remappings, `from:=to`, one per line.
    #[param(default = "")]
    remaps: String,
    /// ROS parameters as a JSON object; nested objects become dotted names.
    #[param(default = "")]
    parameters: String,
    /// ROS parameter files, one per line.
    #[param(default = "")]
    params_files: String,
    /// Arguments before `--ros-args`.
    #[param(default = "")]
    arguments: String,
    /// Extra ROS arguments, e.g. `--log-level debug`.
    #[param(default = "")]
    ros_args: String,
    /// Extra environment, `NAME=value`, one per line.
    #[param(default = "")]
    env: String,
    /// Command put in front of the executable, e.g. `gdb -ex run --args`.
    #[param(default = "")]
    prefix: String,
    /// When to restart the process after it exits: `never`, `on-failure`
    /// or `always`.
    #[param(default = "on-failure")]
    respawn: String,
    /// Seconds before a restart; 0 backs off from 0.2 s to 5 s.
    #[param(default = 0.0, min = 0.0)]
    respawn_delay: f64,
    /// A setup script to source for the ROS environment, e.g.
    /// `/opt/ros/jazzy/setup.bash` or a workspace's `install/setup.bash`.
    /// Empty uses the host's environment.
    #[param(default = "")]
    ros_setup: String,
    /// RMW implementation given to the node.
    #[param(default = "rmw_zenoh_cpp")]
    rmw: String,
    /// Zenoh router the node connects to (that of the bridge nodes).
    #[param(default = "tcp/127.0.0.1:7447")]
    router: String,
    /// ROS_DOMAIN_ID; -1 leaves the environment's.
    #[param(default = -1, min = -1, max = 232)]
    domain_id: i64,
    /// Token of a router that requires one; empty uses RURO_TOKEN.
    #[param(default = "")]
    token: String,
    supervisor: Option<Supervisor>,
    /// The parameters the supervisor was started with.
    started_with: String,
}

impl RosNode {
    /// Every parameter, to notice changes without re-reading the
    /// environment (sourcing `ros_setup`) on every step.
    fn key(&self) -> String {
        format!(
            "{:?}",
            (
                [
                    &self.package,
                    &self.executable,
                    &self.command,
                    &self.name,
                    &self.namespace,
                    &self.remaps,
                    &self.parameters,
                    &self.params_files,
                    &self.arguments,
                    &self.ros_args,
                    &self.env,
                    &self.prefix,
                    &self.respawn,
                    &self.ros_setup,
                    &self.rmw,
                    &self.router,
                    &self.token,
                ],
                self.respawn_delay,
                self.domain_id,
            )
        )
    }

    fn launch(&self) -> Result<Launch> {
        let mut p = RosProcess::from_params(
            &self.package,
            &self.executable,
            &self.command,
            &self.name,
            &self.namespace,
            &self.remaps,
            &self.parameters,
            &self.params_files,
            &self.arguments,
            &self.ros_args,
            &self.env,
            &self.prefix,
        )?;
        let respawn: Respawn = self.respawn.parse()?;
        let mut env = base_env(&self.ros_setup)?;
        let ament = env.get("AMENT_PREFIX_PATH").cloned().unwrap_or_default();
        for f in &mut p.params_files {
            *f = resolve_packages(f, &ament)?;
        }
        for a in &mut p.arguments {
            *a = resolve_packages(a, &ament)?;
        }
        for (_, v) in &mut p.parameters {
            if let ruro::serde_json::Value::String(s) = v {
                *s = resolve_packages(s, &ament)?;
            }
        }
        let program = if p.command.is_empty() {
            find_executable(&ament, &p.package, &p.executable)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "executable `{}` of package `{}` not found in AMENT_PREFIX_PATH ({}); \
                         set `ros_setup` to the setup.bash of the workspace that has it",
                        p.executable,
                        p.package,
                        if ament.is_empty() { "not set" } else { &ament }
                    )
                })?
                .display()
                .to_string()
        } else {
            String::new()
        };
        if !self.rmw.is_empty() {
            env.insert("RMW_IMPLEMENTATION".into(), self.rmw.clone());
        }
        let mut token = if self.token.is_empty() {
            crate::env_credentials()?.token
        } else {
            Some(self.token.clone())
        };
        if token.as_deref() == Some("") {
            token = None;
        }
        env.insert(
            "ZENOH_CONFIG_OVERRIDE".into(),
            zenoh_override(&self.router, token.as_deref()),
        );
        if self.domain_id >= 0 {
            env.insert("ROS_DOMAIN_ID".into(), self.domain_id.to_string());
        }
        // Plain, unbuffered output for the ruro log.
        env.entry("RCUTILS_COLORIZED_OUTPUT".into())
            .or_insert_with(|| "0".into());
        env.entry("RCUTILS_LOGGING_BUFFERED_STREAM".into())
            .or_insert_with(|| "0".into());
        env.entry("PYTHONUNBUFFERED".into())
            .or_insert_with(|| "1".into());
        for (k, v) in &p.env {
            env.insert(k.clone(), v.clone());
        }
        Ok(Launch {
            argv: p.argv(&program),
            env,
            respawn,
            delay: (self.respawn_delay > 0.0).then(|| Duration::from_secs_f64(self.respawn_delay)),
            label: p.label(),
        })
    }

    fn ensure(&mut self, ctx: &Ctx) -> Result {
        let key = self.key();
        if self.supervisor.is_some() && key == self.started_with {
            return Ok(());
        }
        let launch = self.launch()?;
        if self.supervisor.as_ref().is_none_or(|s| s.launch != launch) {
            // Stop the old process before starting the new one.
            self.supervisor = None;
            self.supervisor = Some(Supervisor::start(ctx.id(), launch)?);
        }
        self.started_with = key;
        Ok(())
    }
}

impl Node for RosNode {
    fn start(&mut self, ctx: &mut Ctx) -> Result {
        self.ensure(ctx)
    }

    fn step(&mut self, ctx: &mut Ctx) -> Result {
        self.ensure(ctx)?;
        let sup = self.supervisor.as_ref().expect("started");
        let s = sup.status.lock().unwrap();
        match &s.problem {
            Some(p) if s.done => bail!("{p}"),
            Some(p) => bail!("{p} ({} restart(s))", s.restarts),
            None => Ok(()),
        }
    }

    fn stop(&mut self, _ctx: &mut Ctx) {
        self.supervisor = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zenoh_settings_for_rmw_zenoh() {
        assert_eq!(
            zenoh_override("tcp/127.0.0.1:7447", None),
            "connect/endpoints=[\"tcp/127.0.0.1:7447\"];scouting/multicast/enabled=false"
        );
        assert!(zenoh_override("tcp/h:1", Some("s\"x")).ends_with(
            ";transport/auth/usrpwd/user=\"ruro\";transport/auth/usrpwd/password=\"s\\\"x\""
        ));
    }

    #[test]
    fn package_substitutions_resolve_at_start() {
        let dir = std::env::temp_dir().join(format!("ruro-ros2-node-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("share/cfg")).unwrap();
        let ament = dir.display().to_string();
        assert_eq!(
            resolve_packages("$(find-pkg-share cfg)/a.yaml", &ament).unwrap(),
            format!("{ament}/share/cfg/a.yaml")
        );
        assert_eq!(
            resolve_packages("x $(find-pkg-prefix cfg) $(other)", &ament).unwrap(),
            format!("x {ament} $(other)")
        );
        assert!(resolve_packages("$(find-pkg-share nope)", &ament).is_err());
        assert_eq!("always".parse::<Respawn>().unwrap(), Respawn::Always);
        assert!("sometimes".parse::<Respawn>().is_err());
    }
}
