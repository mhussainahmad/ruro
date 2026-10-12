//! Starts, watches and restarts host processes, one per graph group.

use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

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

struct Group {
    name: String,
    nodes: Vec<String>,
    policy: RestartPolicy,
    host: PathBuf,
    child: Option<Child>,
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
    pub estopped: bool,
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
        .stdin(Stdio::null());
        if let Some(token) = &self.token {
            cmd.env(TOKEN_ENV, token);
        }
        if self.host_state.estopped {
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
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        let child = cmd
            .spawn()
            .with_context(|| format!("starting {} for group `{}`", g.host.display(), g.name))?;
        tracing::info!("group `{}` started (pid {})", g.name, child.id());
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
    pub fn poll(&mut self) {
        let now = Instant::now();
        let mut to_spawn = Vec::new();
        for (i, g) in self.groups.iter_mut().enumerate() {
            if let Some(child) = &mut g.child {
                let Ok(Some(status)) = child.try_wait() else {
                    continue;
                };
                g.child = None;
                let (failed, desc) = match g.heartbeat_lost.take() {
                    Some(silent) => (
                        true,
                        format!("no heartbeat for {:.1} s", silent.as_secs_f64()),
                    ),
                    None => (!status.success(), describe_exit(status)),
                };
                tracing::warn!("group `{}` exited: {desc}", g.name);
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
            }
            self.groups[i] = g;
        }
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
            if let Some(child) = &g.child {
                // SAFETY: plain signal delivery to our own child.
                unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
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
    match (status.code(), status.signal()) {
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
