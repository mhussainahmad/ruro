//! Starts, watches and restarts host processes, one per graph group.

use std::collections::VecDeque;
use std::io::{BufRead, Write};
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use anyhow::{Context, Result, bail};
use ruro_graph::{Graph, RestartPolicy};
use ruro_proto::{GroupState, GroupStatus};

/// A group that stays up this long has its restart backoff reset.
const STABLE_AFTER: Duration = Duration::from_secs(10);
const BACKOFF_MIN: Duration = Duration::from_millis(200);
const BACKOFF_MAX: Duration = Duration::from_secs(5);
/// Time a newly started host has to send its first heartbeat.
pub const STARTUP_GRACE: Duration = Duration::from_secs(30);
/// Grace period between SIGTERM and SIGKILL.
const STOP_GRACE: Duration = Duration::from_secs(2);
/// Lines of a host's stderr kept to explain its exit.
const STDERR_TAIL: usize = 20;
/// How long an exit waits for the rest of the host's stderr.
const STDERR_DRAIN: Duration = Duration::from_millis(300);
/// Longest error message taken from a host's stderr.
const REASON_MAX: usize = 400;

/// The end of a host's stderr. A thread copies the host's stderr to the
/// daemon's and keeps the last lines, so an exit can say why.
struct StderrTail {
    lines: Arc<Mutex<VecDeque<String>>>,
    /// Disconnected once the host's stderr is closed.
    closed: mpsc::Receiver<()>,
}

impl StderrTail {
    fn follow(group: &str, stderr: ChildStderr) -> Self {
        let lines = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL)));
        let (tx, closed) = mpsc::channel::<()>();
        let tail = lines.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("ruro-stderr:{group}"))
            .spawn(move || {
                let _tx = tx;
                let mut reader = std::io::BufReader::new(stderr);
                let mut buf = Vec::new();
                while matches!(reader.read_until(b'\n', &mut buf), Ok(n) if n > 0) {
                    let _ = std::io::stderr().lock().write_all(&buf);
                    let line = String::from_utf8_lossy(&buf).trim_end().to_owned();
                    buf.clear();
                    let mut tail = tail.lock();
                    if tail.len() == STDERR_TAIL {
                        tail.pop_front();
                    }
                    tail.push_back(line);
                }
            });
        if let Err(e) = spawned {
            tracing::warn!("cannot follow the output of group `{group}`: {e}");
        }
        Self { lines, closed }
    }

    /// The host's last error message, once its stderr is closed (or a
    /// short while has passed).
    fn reason(&self, host: &Path) -> Option<String> {
        let _ = self.closed.recv_timeout(STDERR_DRAIN);
        last_error(host, &self.lines.lock())
    }
}

/// The line of a host's stderr that most likely says why it exited: the
/// last one that is not routine logging or panic boilerplate, without the
/// program name the host prefixes fatal errors with.
fn last_error(host: &Path, lines: &VecDeque<String>) -> Option<String> {
    let program = host.file_name()?.to_string_lossy().into_owned();
    let routine = [" INFO ", " DEBUG ", " TRACE "];
    let line = lines.iter().rev().map(|l| strip_ansi(l)).find(|l| {
        let l = l.trim();
        !l.is_empty()
            && !routine.iter().any(|r| l.contains(r))
            && !l.starts_with("note: run with `RUST_BACKTRACE")
            && !l.starts_with("stack backtrace")
    })?;
    let line = line.trim();
    let line = line
        .strip_prefix(&format!("{program}: "))
        .unwrap_or(line)
        .to_owned();
    Some(match line.char_indices().nth(REASON_MAX) {
        Some((i, _)) => format!("{}…", &line[..i]),
        None => line,
    })
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // CSI sequences end with a letter.
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// A group whose host exited unexpectedly, and why.
pub struct Exit {
    pub group: String,
    pub description: String,
}

struct Group {
    name: String,
    nodes: Vec<String>,
    policy: RestartPolicy,
    host: PathBuf,
    child: Option<Child>,
    stderr: Option<StderrTail>,
    state: GroupState,
    restarts: u32,
    /// Consecutive quick failures, used for exponential backoff.
    streak: u32,
    started: Instant,
    retry_at: Option<Instant>,
    last_exit: Option<String>,
    /// When the running process last published metrics.
    last_seen: Option<Instant>,
    /// Set when the supervisor killed the process for missing heartbeats.
    heartbeat_lost: Option<Duration>,
}

/// State a restarted host must pick up.
#[derive(Default)]
pub struct HostState {
    /// The daemon's e-stop flag itself, so the stop never waits for the
    /// supervisor's lock.
    pub estopped: Arc<AtomicBool>,
    /// Directory receiving recording segments, while recording.
    pub record_dir: Option<PathBuf>,
    /// Nodes whose outputs are being replayed.
    pub muted: Vec<String>,
}

pub struct Supervisor {
    daemon: String,
    default_host: PathBuf,
    endpoint: String,
    dir: PathBuf,
    token: Option<String>,
    graph: Option<Graph>,
    graph_file: PathBuf,
    groups: Vec<Group>,
    pub host_state: HostState,
    segment: u32,
}

/// The daemon's runtime directory.
pub fn runtime_dir(daemon: &str) -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join(format!("ruro-{daemon}"))
}

impl Supervisor {
    pub fn new(
        daemon: String,
        host: PathBuf,
        endpoint: String,
        token: Option<String>,
    ) -> Result<Self> {
        let dir = runtime_dir(&daemon);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(Self {
            daemon,
            default_host: host,
            endpoint,
            graph_file: dir.join("graph.yaml"),
            dir,
            token,
            graph: None,
            groups: Vec::new(),
            host_state: HostState::default(),
            segment: 0,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn graph(&self) -> Option<&Graph> {
        self.graph.as_ref()
    }

    /// Directory of host binaries uploaded with `ruro deploy --host-binary`.
    pub fn hosts_dir(&self) -> PathBuf {
        self.dir.join("hosts")
    }

    /// Host binary for a group's `host` setting: a path, or the name of an
    /// uploaded binary.
    pub fn resolve_host(&self, host: Option<&str>) -> Result<PathBuf> {
        let Some(name) = host else {
            return Ok(self.default_host.clone());
        };
        let path = Path::new(name);
        if path.components().count() > 1 && path.exists() {
            return Ok(path.to_owned());
        }
        let uploaded = self.hosts_dir().join(name);
        if ruro_graph::is_valid_ident(name) && uploaded.exists() {
            return Ok(uploaded);
        }
        bail!("host `{name}` is neither a path nor an uploaded host binary")
    }

    /// Replaces the running graph with the groups of `graph` that run on this
    /// machine. Returns the number of groups started.
    pub fn deploy(&mut self, graph: Graph) -> Result<usize> {
        self.stop_all();
        graph.save(&self.graph_file)?;
        for (name, nodes) in graph.groups() {
            let config = graph.group_config(&name);
            if config.machine.as_deref().is_some_and(|m| m != self.daemon) {
                continue;
            }
            let host = self.resolve_host(config.host.as_deref())?;
            let policy = group_policy(&graph, &nodes);
            let mut g = Group {
                name,
                nodes,
                policy,
                host,
                child: None,
                stderr: None,
                state: GroupState::Stopped,
                restarts: 0,
                streak: 0,
                started: Instant::now(),
                retry_at: None,
                last_exit: None,
                last_seen: None,
                heartbeat_lost: None,
            };
            self.spawn(&mut g)?;
            self.groups.push(g);
        }
        tracing::info!(
            "deployed graph `{}` ({} local groups) from {}",
            graph.name,
            self.groups.len(),
            self.dir.display()
        );
        self.graph = Some(graph);
        Ok(self.groups.len())
    }

    fn spawn(&mut self, g: &mut Group) -> Result<()> {
        let mut cmd = Command::new(&g.host);
        cmd.args([
            "run",
            "--group",
            &g.name,
            "--daemon",
            &self.daemon,
            "--connect",
            &self.endpoint,
        ])
        .arg("--graph")
        .arg(&self.graph_file)
        .stdin(Stdio::null())
        .stderr(Stdio::piped());
        if let Some(token) = &self.token {
            cmd.env(TOKEN_ENV, token);
        }
        // The host's stderr goes through the daemon's: keep its colours.
        if std::io::IsTerminal::is_terminal(&std::io::stderr()) {
            cmd.env("RURO_LOG_COLOR", "1");
        }
        if self.host_state.estopped.load(Ordering::Relaxed) {
            cmd.arg("--estopped");
        }
        if let Some(dir) = &self.host_state.record_dir {
            self.segment += 1;
            cmd.arg("--record")
                .arg(dir.join(format!("{}-{}.mcap", g.name, self.segment)));
        }
        let muted: Vec<&String> = self
            .host_state
            .muted
            .iter()
            .filter(|n| g.nodes.contains(n))
            .collect();
        if !muted.is_empty() {
            cmd.arg("--mute").arg(
                muted
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
            );
        }
        // SAFETY: prctl is async-signal-safe. The child is killed if the daemon dies,
        // so a crashed daemon never leaves orphaned robots running.
        #[cfg(target_os = "linux")]
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("starting {} for group `{}`", g.host.display(), g.name))?;
        tracing::info!("group `{}` started (pid {})", g.name, child.id());
        g.stderr = child
            .stderr
            .take()
            .map(|stderr| StderrTail::follow(&g.name, stderr));
        g.child = Some(child);
        g.state = GroupState::Running;
        g.started = Instant::now();
        g.retry_at = None;
        g.last_seen = None;
        g.heartbeat_lost = None;
        Ok(())
    }

    /// Names of the groups whose host process is running.
    pub fn running_groups(&self) -> Vec<String> {
        self.groups
            .iter()
            .filter(|g| g.child.is_some())
            .map(|g| g.name.clone())
            .collect()
    }

    /// Notes that the host of `group` with process id `pid` is alive.
    pub fn heartbeat(&mut self, group: &str, pid: u32) {
        if let Some(g) = self
            .groups
            .iter_mut()
            .find(|g| g.name == group && g.child.as_ref().is_some_and(|c| c.id() == pid))
        {
            g.last_seen = Some(Instant::now());
        }
    }

    /// Kills hosts that published no metrics for `timeout`; they are then
    /// restarted like crashed ones. A host that has not reported yet gets at
    /// least [`STARTUP_GRACE`] to start, since loading node libraries and
    /// joining the router can be slow on a busy or small machine.
    pub fn check_heartbeats(&mut self, timeout: Duration) {
        let now = Instant::now();
        for g in &mut self.groups {
            let Some(child) = &mut g.child else { continue };
            if g.heartbeat_lost.is_some() {
                continue;
            }
            let (last, limit) = match g.last_seen {
                Some(seen) => (seen, timeout),
                None => (g.started, timeout.max(STARTUP_GRACE)),
            };
            let silent = now.saturating_duration_since(last);
            if silent > limit {
                tracing::warn!(
                    "group `{}` sent no heartbeat for {:.1} s; killing it",
                    g.name,
                    silent.as_secs_f64()
                );
                g.heartbeat_lost = Some(silent);
                let _ = child.kill();
            }
        }
    }

    /// Reaps exited children and performs due restarts. Call periodically.
    /// Reaps exited hosts and restarts groups whose backoff has passed.
    /// Returns the groups that exited unexpectedly.
    pub fn poll(&mut self) -> Vec<Exit> {
        let now = Instant::now();
        let mut to_spawn = Vec::new();
        let mut exits = Vec::new();
        for (i, g) in self.groups.iter_mut().enumerate() {
            if let Some(child) = &mut g.child {
                let Ok(Some(status)) = child.try_wait() else {
                    continue;
                };
                g.child = None;
                let reason = g.stderr.take().and_then(|t| t.reason(&g.host));
                let (failed, desc) = match g.heartbeat_lost.take() {
                    Some(silent) => (
                        true,
                        format!("no heartbeat for {:.1} s", silent.as_secs_f64()),
                    ),
                    None => {
                        let desc = describe_exit(status);
                        // Killed hosts left no last word; their last line is
                        // whatever they logged before.
                        match reason.filter(|_| status.code().is_some_and(|c| c != 0)) {
                            Some(r) => (true, format!("{desc}: {r}")),
                            None => (!status.success(), desc),
                        }
                    }
                };
                tracing::warn!("group `{}` exited: {desc}", g.name);
                exits.push(Exit {
                    group: g.name.clone(),
                    description: desc.clone(),
                });
                g.last_exit = Some(desc);
                let restart = match g.policy {
                    RestartPolicy::Never => false,
                    RestartPolicy::OnFailure => failed,
                    RestartPolicy::Always => true,
                };
                if !restart {
                    g.state = if failed {
                        GroupState::Failed
                    } else {
                        GroupState::Stopped
                    };
                    continue;
                }
                g.streak = if g.started.elapsed() >= STABLE_AFTER {
                    0
                } else {
                    g.streak + 1
                };
                let delay = BACKOFF_MIN
                    .saturating_mul(1 << g.streak.min(5))
                    .min(BACKOFF_MAX);
                g.state = GroupState::Backoff;
                g.retry_at = Some(now + delay);
                tracing::info!("restarting group `{}` in {delay:?}", g.name);
            } else if g.state == GroupState::Backoff && g.retry_at.is_some_and(|t| t <= now) {
                to_spawn.push(i);
            }
        }
        for i in to_spawn {
            let mut g = std::mem::replace(&mut self.groups[i], placeholder());
            g.restarts += 1;
            if let Err(e) = self.spawn(&mut g) {
                tracing::error!("{e:#}");
                g.state = GroupState::Failed;
                g.last_exit = Some(format!("{e:#}"));
                exits.push(Exit {
                    group: g.name.clone(),
                    description: format!("{e:#}"),
                });
            }
            self.groups[i] = g;
        }
        exits
    }

    /// Sends SIGKILL to a group, as a crash would.
    pub fn kill(&mut self, group: &str) -> Result<()> {
        let Some(g) = self.groups.iter_mut().find(|g| g.name == group) else {
            bail!("no group `{group}`");
        };
        let Some(child) = &mut g.child else {
            bail!("group `{group}` is not running")
        };
        child.kill()?;
        Ok(())
    }

    /// Stops every group: SIGTERM, then SIGKILL after a grace period.
    pub fn stop_all(&mut self) {
        for g in &mut self.groups {
            if let Some(child) = &mut g.child {
                // SAFETY: plain signal delivery to our own child.
                #[cfg(unix)]
                unsafe {
                    libc::kill(child.id() as i32, libc::SIGTERM)
                };
                #[cfg(not(unix))]
                let _ = child.kill();
            }
        }
        let deadline = Instant::now() + STOP_GRACE;
        for g in &mut self.groups {
            if let Some(mut child) = g.child.take() {
                loop {
                    match child.try_wait() {
                        Ok(Some(_)) | Err(_) => break,
                        Ok(None) if Instant::now() >= deadline => {
                            tracing::warn!("group `{}` ignored SIGTERM; killing", g.name);
                            let _ = child.kill();
                            let _ = child.wait();
                            break;
                        }
                        Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                    }
                }
            }
        }
        if self.graph.take().is_some() {
            tracing::info!("graph stopped");
        }
        self.groups.clear();
        self.host_state.muted.clear();
    }

    pub fn status(&self) -> Vec<GroupStatus> {
        self.groups
            .iter()
            .map(|g| GroupStatus {
                name: g.name.clone(),
                nodes: g.nodes.clone(),
                state: g.state,
                pid: g.child.as_ref().map(Child::id),
                restarts: g.restarts,
                last_exit: g.last_exit.clone(),
            })
            .collect()
    }

    /// Next segment file of `group` in the recording directory.
    pub fn next_segment(&mut self, group: &str) -> Option<PathBuf> {
        let dir = self.host_state.record_dir.as_ref()?;
        self.segment += 1;
        Some(dir.join(format!("{group}-{}.mcap", self.segment)))
    }
}

/// Environment variable hosts read the access token from.
const TOKEN_ENV: &str = "RURO_TOKEN";

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.stop_all();
    }
}

/// The most conservative policy of the group's nodes wins.
fn group_policy(graph: &Graph, nodes: &[String]) -> RestartPolicy {
    let policies: Vec<RestartPolicy> = nodes
        .iter()
        .map(|n| graph.nodes[n].restart.unwrap_or_default())
        .collect();
    if policies.contains(&RestartPolicy::Never) {
        RestartPolicy::Never
    } else if policies.contains(&RestartPolicy::Always) {
        RestartPolicy::Always
    } else {
        RestartPolicy::OnFailure
    }
}

fn describe_exit(status: ExitStatus) -> String {
    #[cfg(unix)]
    let signal = status.signal();
    #[cfg(not(unix))]
    let signal: Option<i32> = None;
    match (status.code(), signal) {
        (Some(code), _) => format!("exit code {code}"),
        (None, Some(sig)) => format!("killed by signal {sig}"),
        _ => "unknown exit".into(),
    }
}

fn placeholder() -> Group {
    Group {
        name: String::new(),
        nodes: Vec::new(),
        policy: RestartPolicy::Never,
        host: PathBuf::new(),
        child: None,
        stderr: None,
        state: GroupState::Stopped,
        restarts: 0,
        streak: 0,
        started: Instant::now(),
        retry_at: None,
        last_exit: None,
        last_seen: None,
        heartbeat_lost: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_precedence() {
        let g = Graph::from_yaml(
            "name: p\nnodes:\n  a: { type: x, restart: always }\n  b: { type: x }\n  c: { type: x, restart: never, group: other }\n  d: { type: x, group: other, restart: always }\n",
        )
        .unwrap();
        assert_eq!(
            group_policy(&g, &["a".into(), "b".into()]),
            RestartPolicy::Always
        );
        assert_eq!(group_policy(&g, &["b".into()]), RestartPolicy::OnFailure);
        assert_eq!(
            group_policy(&g, &["c".into(), "d".into()]),
            RestartPolicy::Never
        );
    }

    #[test]
    fn exits_are_explained_by_the_last_error_line() {
        let host = Path::new("/opt/bin/ruro-host");
        let lines = |ls: &[&str]| ls.iter().map(|l| l.to_string()).collect::<VecDeque<_>>();
        assert_eq!(
            last_error(
                host,
                &lines(&[
                    "\x1b[2m12:00:00\x1b[0m \x1b[32m WARN\x1b[0m slow start",
                    "ruro-host: failed to open topic `ruro/x`: DoesNotExist",
                    "2026-10-11T12:00:00Z  INFO main group `main` stopping",
                    "",
                ])
            )
            .as_deref(),
            Some("failed to open topic `ruro/x`: DoesNotExist")
        );
        let panic = lines(&[
            "thread 'main' panicked at src/main.rs:3:5:",
            "boom",
            "note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace",
        ]);
        assert_eq!(last_error(host, &panic).as_deref(), Some("boom"));
        assert_eq!(last_error(host, &lines(&[])), None);
        let long = "x".repeat(1000);
        let cut = last_error(host, &lines(&[&long])).unwrap();
        assert_eq!(cut.chars().count(), REASON_MAX + 1);
    }

    #[test]
    fn hosts_resolve_to_paths_or_uploads() {
        let name = format!("resolve-test-{}", std::process::id());
        let sup = Supervisor::new(name, "/bin/true".into(), String::new(), None).unwrap();
        assert_eq!(sup.resolve_host(None).unwrap(), PathBuf::from("/bin/true"));
        assert_eq!(
            sup.resolve_host(Some("/bin/sh")).unwrap(),
            PathBuf::from("/bin/sh")
        );
        assert!(sup.resolve_host(Some("missing")).is_err());
        std::fs::create_dir_all(sup.hosts_dir()).unwrap();
        std::fs::write(sup.hosts_dir().join("custom"), b"").unwrap();
        assert_eq!(
            sup.resolve_host(Some("custom")).unwrap(),
            sup.hosts_dir().join("custom")
        );
        let _ = std::fs::remove_dir_all(sup.dir());
    }
}
