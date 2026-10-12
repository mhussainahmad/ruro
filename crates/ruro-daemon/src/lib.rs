//! The ruro daemon: one per machine.
//!
//! It runs the Zenoh router every other ruro process connects to, validates
//! and deploys graphs, starts one host process per group and restarts them
//! according to their restart policy (or when they stop sending heartbeats).
//! It also latches the emergency stop, coordinates recording and replay, and
//! forwards the groups of multi-machine graphs to the daemons that run them.

mod session;
mod supervisor;
mod upload;

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use parking_lot::Mutex;
use ruro_graph::{Graph, Registry};
use ruro_proto::{Ctl, CtlReply, DaemonStatus, GroupMetrics, Keys, Security};
use zenoh::Wait;

use crate::session::{Playback, RecordSession};
use crate::supervisor::Supervisor;

pub use supervisor::runtime_dir;

/// Default port of the daemon's router.
pub const DEFAULT_PORT: u16 = 7447;

/// How long control requests to hosts and peer daemons may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct Config {
    /// Daemon name; defaults to the hostname.
    pub name: String,
    /// Zenoh endpoint to listen on, e.g. `tcp/127.0.0.1:7447` or
    /// `tls/0.0.0.0:7447`. Hosts always use a private Unix socket.
    pub listen: String,
    /// Host binary used to run groups.
    pub host: PathBuf,
    /// Other daemons to connect to, e.g. `tcp/robot2:7447`, so graphs can
    /// span machines.
    pub peers: Vec<String>,
    /// TLS certificates and access token.
    pub security: Security,
    /// Hosts that publish no metrics for this long are killed and restarted.
    pub heartbeat_timeout: Duration,
}

impl Config {
    pub fn new() -> Self {
        Self {
            name: ruro_proto::hostname(),
            listen: format!("tcp/127.0.0.1:{DEFAULT_PORT}"),
            host: default_host_binary(),
            peers: Vec::new(),
            security: Security::default(),
            heartbeat_timeout: Duration::from_secs(5),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new()
    }
}

/// `ruro-host` next to the current executable, else `$RURO_HOST`, else `PATH`.
pub fn default_host_binary() -> PathBuf {
    if let Some(p) = std::env::var_os("RURO_HOST") {
        return p.into();
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join("ruro-host")))
        .filter(|p| p.exists())
        .unwrap_or_else(|| "ruro-host".into())
}

/// Asks a host binary for the node types it links.
pub fn load_registry(host: &Path) -> Result<Registry> {
    let out = std::process::Command::new(host)
        .arg("describe")
        .output()
        .with_context(|| format!("running `{} describe`", host.display()))?;
    if !out.status.success() {
        bail!(
            "`{} describe` failed: {}",
            host.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    serde_json::from_slice(&out.stdout).context("parsing node registry")
}

/// E-stop requests forwarded from the daemon that deployed the graph to the
/// peers running parts of it. Peers do not forward them further.
const FORWARDED_ENGAGE: &str = "engage-forwarded";
const FORWARDED_RELEASE: &str = "release-forwarded";

/// Who engaged the emergency stop: someone at this daemon, or the daemon
/// whose graph this one runs part of. A release forwarded from there does
/// not lift a stop engaged here.
#[derive(Debug, Default)]
pub(crate) struct EstopBy {
    here: bool,
    forwarded: bool,
}

impl EstopBy {
    fn apply(&mut self, engage: bool, forwarded: bool) {
        match (engage, forwarded) {
            (true, false) => self.here = true,
            (true, true) => self.forwarded = true,
            // A release at this daemon lifts every stop.
            (false, false) => *self = Self::default(),
            (false, true) => self.forwarded = false,
        }
    }

    fn engaged(&self) -> bool {
        self.here || self.forwarded
    }
}

/// Everything the daemon's threads share.
pub(crate) struct State {
    pub name: String,
    pub keys: Keys,
    pub session: zenoh::Session,
    pub sup: Mutex<Supervisor>,
    pub host: PathBuf,
    pub registry: Mutex<Registry>,
    /// Whether the emergency stop is engaged, by anyone.
    pub estop: AtomicBool,
    pub estop_by: Mutex<EstopBy>,
    pub recording: Mutex<Option<RecordSession>>,
    pub playback: Mutex<Option<Playback>>,
    /// Daemons running groups of the graph deployed here.
    pub peers: Mutex<BTreeSet<String>>,
    /// Last metrics seen per group: `(pid, when)`.
    pub heartbeats: Mutex<HashMap<String, (u32, Instant)>>,
    /// The graph as deployed by the client (before flattening and machine
    /// assignment), returned by `Ctl::Graph`.
    pub deployed: Mutex<Option<String>>,
}

impl State {
    /// Sends a command to the host running `group` and waits for its reply.
    pub fn host_request(&self, group: &str, cmd: &ruro_proto::HostCommand) -> Result<String> {
        let payload = serde_json::to_vec(cmd)?;
        ruro_proto::request_key(
            &self.session,
            &self.keys.host_ctl(group),
            payload,
            REQUEST_TIMEOUT,
        )
        .with_context(|| format!("group `{group}`"))
    }

    /// Sends a control request to another daemon.
    pub fn peer_request(&self, daemon: &str, op: Ctl, payload: &str) -> Result<String> {
        ruro_proto::request(
            &self.session,
            &Keys::new(daemon),
            op,
            payload,
            REQUEST_TIMEOUT,
        )
        .with_context(|| format!("daemon `{daemon}`"))
    }

    fn publish_estop(&self) {
        let flag = ruro_msg::Flag(self.estop.load(Ordering::Relaxed));
        let _ = self
            .session
            .put(self.keys.estop(), ruro_msg::Message::to_bytes(&flag))
            .wait();
    }
}

/// A running daemon. Dropping it stops the deployed graph.
pub struct Daemon {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Daemon {
    /// Starts the daemon on background threads.
    pub fn start(config: Config) -> Result<Self> {
        let registry = load_registry(&config.host)?;
        let dir = runtime_dir(&config.name);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

        // Hosts reach the router over a private Unix socket, so they need
        // neither the network endpoint nor TLS certificates.
        let socket = dir.join("zenoh.sock");
        let _ = std::fs::remove_file(&socket);
        let local = format!("unixsock-stream/{}", socket.display());
        let dictionary = match &config.security.token {
            Some(token) => Some(write_dictionary(&dir, token)?),
            None => None,
        };
        let zconfig = ruro_proto::router_config_with(
            &[config.listen.clone(), local.clone()],
            &config.peers,
            &config.security,
            dictionary.as_deref(),
        )?;
        let session = zenoh::open(zconfig)
            .wait()
            .map_err(|e| anyhow!("cannot listen on {}: {e}", config.listen))?;
        let keys = Keys::new(&config.name);
        let sup = Supervisor::new(
            config.name.clone(),
            config.host.clone(),
            local,
            config.security.token.clone(),
        )?;
        let state = Arc::new(State {
            name: config.name.clone(),
            keys: keys.clone(),
            session: session.clone(),
            sup: Mutex::new(sup),
            host: config.host.clone(),
            registry: Mutex::new(registry),
            estop: AtomicBool::new(false),
            estop_by: Mutex::new(EstopBy::default()),
            recording: Mutex::new(None),
            playback: Mutex::new(None),
            peers: Mutex::new(BTreeSet::new()),
            heartbeats: Mutex::new(HashMap::new()),
            deployed: Mutex::new(None),
        });

        let queryable = session
            .declare_queryable(format!("ruro/{}/ctl/*", config.name))
            .wait()
            .map_err(|e| anyhow!("{e}"))?;
        let stop = Arc::new(AtomicBool::new(false));

        let ctl_state = state.clone();
        std::thread::Builder::new()
            .name("ruro-ctl".into())
            .spawn(move || {
                while let Ok(query) = queryable.recv() {
                    let op = query
                        .key_expr()
                        .as_str()
                        .rsplit('/')
                        .next()
                        .unwrap_or_default()
                        .to_owned();
                    let payload = query
                        .payload()
                        .map(|p| p.to_bytes().into_owned())
                        .unwrap_or_default();
                    let reply: CtlReply =
                        handle(&ctl_state, &op, &payload).map_err(|e| format!("{e:#}"));
                    if let Err(e) = &reply {
                        tracing::warn!("{op}: {e}");
                    }
                    let _ = query
                        .reply(
                            format!("ruro/{}/ctl/{op}", ctl_state.name),
                            ruro_proto::reply_bytes(&reply),
                        )
                        .wait();
                }
            })?;

        // Heartbeats: every host publishes metrics twice a second.
        let hb = state.clone();
        let heartbeat_sub = session
            .declare_subscriber(keys.all_metrics())
            .callback(move |sample| {
                if let Ok(m) = ruro_proto::decode::<GroupMetrics>(&sample.payload().to_bytes()) {
                    hb.heartbeats
                        .lock()
                        .insert(m.group, (m.pid, Instant::now()));
                }
            })
            .wait()
            .map_err(|e| anyhow!("{e}"))?;

        let loop_stop = stop.clone();
        let started = Instant::now();
        let heartbeat_timeout = config.heartbeat_timeout;
        let st = state.clone();
        let thread = std::thread::Builder::new()
            .name("ruro-supervisor".into())
            .spawn(move || {
                let _heartbeat_sub = heartbeat_sub;
                let mut last_status = Instant::now() - Duration::from_secs(1);
                while !loop_stop.load(Ordering::Relaxed) {
                    {
                        let beats: Vec<_> = st.heartbeats.lock().drain().collect();
                        let mut sup = st.sup.lock();
                        for (group, (pid, _)) in beats {
                            sup.heartbeat(&group, pid);
                        }
                        sup.check_heartbeats(heartbeat_timeout);
                        sup.poll();
                    }
                    session::reap_playback(&st);
                    if last_status.elapsed() >= Duration::from_millis(500) {
                        last_status = Instant::now();
                        let status = status(&st, started);
                        let _ = st
                            .session
                            .put(st.keys.status(), ruro_proto::encode(&status))
                            .wait();
                        // Republished so hosts that missed a change converge.
                        st.publish_estop();
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                session::stop_playback(&st);
                st.sup.lock().stop_all();
                if let Err(e) = session::finish_recording(&st, false) {
                    tracing::warn!("{e:#}");
                }
                let _ = st.session.close().wait();
            })?;

        tracing::info!(
            "daemon `{}` listening on {} with {} node types (host: {})",
            config.name,
            config.listen,
            state.registry.lock().nodes.len(),
            config.host.display()
        );
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }

    /// Stops the deployed graph and the daemon.
    pub fn shutdown(mut self) {
        self.join();
    }

    fn join(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.join();
    }
}

/// Writes the `user:password` file Zenoh checks tokens against.
fn write_dictionary(dir: &Path, token: &str) -> Result<PathBuf> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let path = dir.join("auth");
    let _ = std::fs::remove_file(&path);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("writing {}", path.display()))?;
    writeln!(f, "{}:{token}", ruro_proto::AUTH_USER)?;
    Ok(path)
}

fn status(st: &State, started: Instant) -> DaemonStatus {
    let s = st.sup.lock();
    DaemonStatus {
        name: st.name.clone(),
        version: env!("CARGO_PKG_VERSION").into(),
        pid: std::process::id(),
        uptime_s: started.elapsed().as_secs_f64(),
        graph: s.graph().map(|g| g.name.clone()),
        groups: s.status(),
        recording: st
            .recording
            .lock()
            .as_ref()
            .map(|r| r.output.display().to_string()),
        playback: st.playback.lock().as_ref().map(Playback::status),
        estopped: st.estop.load(Ordering::Relaxed),
    }
}

fn handle(st: &Arc<State>, op: &str, payload: &[u8]) -> Result<String> {
    let text = || String::from_utf8_lossy(payload).into_owned();
    match op {
        "deploy" => deploy(st, &text()),
        "deploy-groups" => {
            let graph = Graph::from_yaml(&text())?;
            // Part of another daemon's graph now: none of its own to forward to.
            st.peers.lock().clear();
            session::stop_playback(st);
            st.sup.lock().stop_all();
            session::finish_recording(st, false)?;
            let n = st.sup.lock().deploy(graph)?;
            *st.deployed.lock() = Some(text());
            Ok(format!("deployed {n} group(s) on `{}`", st.name))
        }
        "stop" => {
            session::stop_playback(st);
            st.sup.lock().stop_all();
            let recorded = session::finish_recording(st, false)?;
            let peers = std::mem::take(&mut *st.peers.lock());
            for p in peers {
                if let Err(e) = st.peer_request(&p, Ctl::Stop, "") {
                    tracing::warn!("{e:#}");
                }
            }
            Ok(match recorded {
                Some(r) => format!("stopped; {r}"),
                None => "stopped".into(),
            })
        }
        "registry" => {
            // Re-read so newly built node types show up without a restart.
            if let Ok(r) = load_registry(&st.host) {
                *st.registry.lock() = r;
            }
            Ok(serde_json::to_string(&*st.registry.lock())?)
        }
        "graph" => Ok(match st.sup.lock().graph() {
            Some(_) => st.deployed.lock().clone().unwrap_or_default(),
            None => String::new(),
        }),
        "ping" => Ok(st.name.clone()),
        "kill" => {
            let group = text();
            st.sup.lock().kill(group.trim())?;
            Ok(format!("killed `{}`", group.trim()))
        }
        "record" => {
            let path = text();
            let path = path.trim();
            if path.is_empty() {
                session::finish_recording(st, true)?.context("not recording")
            } else {
                session::start_recording(st, Path::new(path))
            }
        }
        "playback" => {
            let cmd = serde_json::from_slice(payload).context("bad playback command")?;
            session::playback(st, cmd)
        }
        "estop" => {
            let (engage, forwarded) = match text().trim() {
                "engage" | "" => (true, false),
                "release" => (false, false),
                FORWARDED_ENGAGE => (true, true),
                FORWARDED_RELEASE => (false, true),
                other => bail!("expected `engage` or `release`, got `{other}`"),
            };
            let engaged = {
                let mut by = st.estop_by.lock();
                by.apply(engage, forwarded);
                by.engaged()
            };
            st.estop.store(engaged, Ordering::Relaxed);
            st.sup.lock().host_state.estopped = engaged;
            st.publish_estop();
            tracing::warn!(
                "emergency stop {}",
                if engaged { "engaged" } else { "released" }
            );
            let done = match (engage, engaged) {
                (true, _) => "e-stop engaged",
                (false, false) => "e-stop released",
                (false, true) => "e-stop stays engaged: it was engaged here",
            };
            if forwarded {
                return Ok(done.into());
            }
            // The machines running the rest of the graph follow, without
            // passing it further.
            let peers: Vec<String> = st.peers.lock().iter().cloned().collect();
            let op = if engage {
                FORWARDED_ENGAGE
            } else {
                FORWARDED_RELEASE
            };
            let replies: Vec<String> = peers
                .iter()
                .map(|p| match st.peer_request(p, Ctl::Estop, op) {
                    Ok(reply) => Ok(format!("{p}: {reply}")),
                    Err(e) => Err(format!("{e:#}")),
                })
                .collect::<Result<_, _>>()
                .map_err(|e| anyhow!("{done} here, but not on {e}"))?;
            Ok(std::iter::once(done.to_owned())
                .chain(replies)
                .collect::<Vec<_>>()
                .join("; "))
        }
        "upload" => {
            let chunk: ruro_proto::UploadChunk = ruro_proto::decode(payload)?;
            let dir = st.sup.lock().hosts_dir();
            upload::receive(&dir, &chunk)
        }
        other => bail!("unknown operation `{other}`"),
    }
}

/// Validates `yaml`, sends the groups of other machines to their daemons and
/// starts the local ones.
fn deploy(st: &Arc<State>, yaml: &str) -> Result<String> {
    let mut graph = Graph::from_yaml(yaml)?.flatten()?;
    // Pin every group to a machine, so each daemon knows exactly what it runs.
    for group in graph.groups().into_keys() {
        graph
            .groups
            .entry(group)
            .or_default()
            .machine
            .get_or_insert_with(|| st.name.clone());
    }
    let machines: BTreeSet<String> = graph
        .groups
        .values()
        .filter_map(|c| c.machine.clone())
        .filter(|m| *m != st.name)
        .collect();

    // Node types: this daemon's hosts plus whatever the peers offer.
    let local = st.registry.lock().clone();
    let custom_hosts: BTreeSet<String> = graph
        .groups
        .values()
        .filter(|c| c.machine.as_deref() == Some(st.name.as_str()))
        .filter_map(|c| c.host.clone())
        .collect();
    let mut hosts = HashMap::new();
    for h in custom_hosts {
        let path = st.sup.lock().resolve_host(Some(&h))?;
        hosts.insert(h, load_registry(&path)?);
    }
    let mut peers = HashMap::new();
    for m in &machines {
        let body = st.peer_request(m, Ctl::Registry, "")?;
        let r: Registry = serde_json::from_str(&body).context("peer registry")?;
        peers.insert(m.clone(), r);
    }
    let registry = Registry::new(
        local
            .iter()
            .chain(hosts.values().flat_map(Registry::iter))
            .chain(peers.values().flat_map(Registry::iter))
            .cloned(),
    );
    let mut issues: Vec<String> = ruro_graph::validate(&graph, &registry)
        .iter()
        .map(ToString::to_string)
        .collect();
    issues.extend(runtime_issues(&graph, &registry));
    // The types a group's host links. Peers only report their default host.
    let offered = |group: &str| {
        let config = graph.group_config(group);
        match (config.machine.as_deref(), config.host.as_deref()) {
            (Some(m), None) if m != st.name => peers.get(m),
            (Some(m), Some(_)) if m != st.name => None,
            (_, Some(h)) => hosts.get(h),
            (_, None) => Some(&local),
        }
    };
    issues.extend(placement_issues(&graph, &registry, offered));
    if !issues.is_empty() {
        bail!("graph is invalid:\n  {}", issues.join("\n  "));
    }

    session::stop_playback(st);
    st.sup.lock().stop_all();
    session::finish_recording(st, false)?;
    let previous = std::mem::take(&mut *st.peers.lock());
    for p in previous.difference(&machines) {
        if let Err(e) = st.peer_request(p, Ctl::Stop, "") {
            tracing::warn!("{e:#}");
        }
    }
    let normalized = graph.to_yaml()?;
    for m in &machines {
        // A peer's hosts start stopped when the stop is engaged here.
        if st.estop.load(Ordering::Relaxed) {
            st.peer_request(m, Ctl::Estop, FORWARDED_ENGAGE)?;
        }
        st.peer_request(m, Ctl::DeployGroups, &normalized)?;
        st.peers.lock().insert(m.clone());
    }
    let n = st.sup.lock().deploy(graph)?;
    *st.deployed.lock() = Some(yaml.to_owned());
    Ok(if machines.is_empty() {
        format!("deployed {n} group(s)")
    } else {
        format!(
            "deployed {n} group(s) here and the rest on {}",
            machines.into_iter().collect::<Vec<_>>().join(", ")
        )
    })
}

/// Nodes placed in a group whose host does not link their type, which
/// would fail to start there. `offered` gives a group's host types, or
/// `None` when they are not known here.
fn placement_issues<'a>(
    graph: &Graph,
    registry: &Registry,
    offered: impl Fn(&str) -> Option<&'a Registry>,
) -> Vec<String> {
    let mut issues = Vec::new();
    for (id, node) in &graph.nodes {
        // Unknown everywhere: already reported by validation.
        if registry.get(&node.kind).is_none() {
            continue;
        }
        let group = node.group();
        if offered(group).is_some_and(|r| r.get(&node.kind).is_none()) {
            issues.push(format!(
                "node `{id}`: type `{}` is not linked into the host of group `{group}`",
                node.kind
            ));
        }
    }
    issues
}

/// Checks that need the runtime's conventions rather than node descriptors.
fn runtime_issues(graph: &Graph, registry: &Registry) -> Vec<String> {
    let mut issues = Vec::new();
    if graph.clock == ruro_graph::ClockSource::Sim {
        let clocks = graph
            .nodes
            .values()
            .filter_map(|n| registry.get(&n.kind))
            .flat_map(|d| d.outputs.iter())
            .filter(|p| p.ty == "std/Clock")
            .count();
        if clocks != 1 {
            issues.push(format!(
                "`clock: sim` needs exactly one std/Clock output, found {clocks}"
            ));
        }
    }
    let groups = graph.groups();
    for (name, config) in &graph.groups {
        let Some(rt) = &config.realtime else { continue };
        for node in rt.deadlines_us.keys() {
            if !groups.get(name).is_some_and(|ns| ns.contains(node)) {
                issues.push(format!(
                    "group `{name}`: deadline for `{node}`, which is not in the group"
                ));
            }
        }
        if let Some(p) = rt.priority
            && !(1..=99).contains(&p)
        {
            issues.push(format!("group `{name}`: priority {p} is outside 1-99"));
        }
        if let Some(r) = rt.rate
            && !(r.is_finite() && r > 0.0 && r <= 100_000.0)
        {
            issues.push(format!(
                "group `{name}`: rate {r} Hz is out of range (0, 100000]"
            ));
        }
    }
    issues
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nodes_must_run_in_a_group_whose_host_links_their_type() {
        let desc = |kind: &str| ruro_graph::NodeDescriptor {
            kind: kind.into(),
            ..Default::default()
        };
        let std_host = Registry::new([desc("std/sine")]);
        let custom_host = Registry::new([desc("std/sine"), desc("my/scale")]);
        let all = Registry::new([desc("std/sine"), desc("my/scale")]);
        let g = Graph::from_yaml(
            "name: x\ngroups:\n  custom: { host: my-host }\n  far: { machine: robot, host: other }\nnodes:\n  a: { type: std/sine, group: custom }\n  b: { type: my/scale }\n  c: { type: my/scale, group: custom }\n  d: { type: my/scale, group: far }\n",
        )
        .unwrap();
        let offered = |group: &str| match group {
            "custom" => Some(&custom_host),
            "far" => None,
            _ => Some(&std_host),
        };
        let issues = placement_issues(&g, &all, offered);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(issues[0].contains("`b`") && issues[0].contains("`main`"));
    }

    #[test]
    fn a_forwarded_release_leaves_a_local_stop_engaged() {
        let mut by = EstopBy::default();
        by.apply(true, false);
        by.apply(true, true);
        by.apply(false, true);
        assert!(by.engaged());
        by.apply(true, true);
        by.apply(false, false);
        assert!(!by.engaged(), "a local release lifts both");
        by.apply(true, true);
        by.apply(false, true);
        assert!(!by.engaged());
    }

    #[test]
    fn runtime_checks() {
        let registry = Registry::new([]);
        let g = Graph::from_yaml(
            "name: x\nclock: sim\ngroups:\n  a:\n    realtime: { priority: 120, rate: -1, deadlines_us: { ghost: 5 } }\nnodes:\n  n: { type: t/x, group: a }\n",
        )
        .unwrap();
        let issues = runtime_issues(&g, &registry);
        assert_eq!(issues.len(), 4, "{issues:?}");
        assert!(issues[0].contains("std/Clock"));
    }
}
