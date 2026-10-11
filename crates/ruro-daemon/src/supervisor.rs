//! Starts, watches and restarts host processes, one per graph group.

use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use ruro_graph::{Graph, RestartPolicy};
use ruro_proto::{GroupState, GroupStatus};

/// A group that stays up this long has its restart backoff reset.
const STABLE_AFTER: Duration = Duration::from_secs(10);
const BACKOFF_MIN: Duration = Duration::from_millis(200);
const BACKOFF_MAX: Duration = Duration::from_secs(5);
/// Grace period between SIGTERM and SIGKILL.
const STOP_GRACE: Duration = Duration::from_secs(2);

struct Group {
    name: String,
    nodes: Vec<String>,
    policy: RestartPolicy,
    child: Option<Child>,
    state: GroupState,
    restarts: u32,
    /// Consecutive quick failures, used for exponential backoff.
    streak: u32,
    started: Instant,
    retry_at: Option<Instant>,
    last_exit: Option<String>,
}

pub struct Supervisor {
    daemon: String,
    host: PathBuf,
    endpoint: String,
    dir: PathBuf,
    graph: Option<Graph>,
    graph_file: PathBuf,
    groups: Vec<Group>,
}

impl Supervisor {
    pub fn new(daemon: String, host: PathBuf, endpoint: String) -> Result<Self> {
        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let dir = base.join(format!("ruro-{daemon}"));
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(Self {
            daemon,
            host,
            endpoint,
            graph_file: dir.join("graph.yaml"),
            dir,
            graph: None,
            groups: Vec::new(),
        })
    }

    pub fn daemon(&self) -> &str {
        &self.daemon
    }

    pub fn graph(&self) -> Option<&Graph> {
        self.graph.as_ref()
    }

    /// Replaces the running graph. Returns the number of groups started.
    pub fn deploy(&mut self, graph: Graph) -> Result<usize> {
        self.stop_all();
        graph.save(&self.graph_file)?;
        for (name, nodes) in graph.groups() {
            let policy = group_policy(&graph, &nodes);
            let mut g = Group {
                name,
                nodes,
                policy,
                child: None,
                state: GroupState::Stopped,
                restarts: 0,
                streak: 0,
                started: Instant::now(),
                retry_at: None,
                last_exit: None,
            };
            self.spawn(&mut g)?;
            self.groups.push(g);
        }
        tracing::info!(
            "deployed graph `{}` ({} groups) from {}",
            graph.name,
            self.groups.len(),
            self.dir.display()
        );
        self.graph = Some(graph);
        Ok(self.groups.len())
    }

    fn spawn(&self, g: &mut Group) -> Result<()> {
        let mut cmd = Command::new(&self.host);
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
            .with_context(|| format!("starting {} for group `{}`", self.host.display(), g.name))?;
        tracing::info!("group `{}` started (pid {})", g.name, child.id());
        g.child = Some(child);
        g.state = GroupState::Running;
        g.started = Instant::now();
        g.retry_at = None;
        Ok(())
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
                let failed = !status.success();
                let desc = describe_exit(status);
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
}

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
        child: None,
        state: GroupState::Stopped,
        restarts: 0,
        streak: 0,
        started: Instant::now(),
        retry_at: None,
        last_exit: None,
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
}
