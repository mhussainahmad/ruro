//! The ruro daemon: one per machine.
//!
//! It runs the Zenoh router every other ruro process connects to, validates
//! and deploys graphs, starts one host process per group and restarts them
//! according to their restart policy (or when they stop sending heartbeats).
//! It also latches the emergency stop, coordinates recording and replay, and
//! forwards the groups of multi-machine graphs to the daemons that run them.

// On Windows only the parts clients use are built into anything that runs.
#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

mod estop;
pub mod os;
mod session;
pub mod sign;
mod supervisor;
pub mod update;
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
pub use update::{TARGET, version};

/// Default port of the daemon's router.
pub const DEFAULT_PORT: u16 = 7447;

/// How long control requests to hosts and peer daemons may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct Config {
    /// Daemon name; defaults to the hostname.
    pub name: String,
    /// Zenoh endpoint to listen on, e.g. `tcp/127.0.0.1:7447` or
    /// `tls/[::]:7447`. Hosts always use a private Unix socket.
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
    /// File listing the public keys whose signed updates the daemon
    /// accepts; without it the daemon refuses updates.
    pub update_keys: Option<PathBuf>,
    /// The daemon's executable and the arguments it was started with: an
    /// update replaces the executable and starts it with the same
    /// arguments. Taken when the configuration is made, because once the
    /// file is replaced the running process can no longer name it.
    pub executable: PathBuf,
    pub args: Vec<std::ffi::OsString>,
    /// Shown in the daemon's status: how its last update went.
    pub update_note: Option<String>,
    /// How long an updated daemon must run before its update is kept: if
    /// it stops sooner (other than when asked to), its next start rolls
    /// the update back.
    pub update_probation: Duration,
    /// Start with the emergency stop engaged (as if engaged here).
    pub estopped: bool,
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
            update_keys: None,
            executable: std::env::current_exe().unwrap_or_default(),
            args: std::env::args_os().collect(),
            update_note: None,
            update_probation: Duration::from_secs(120),
            estopped: false,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new()
    }
}

/// The host binary given by `$RURO_HOST`, else `ruro-host` next to the
/// current executable, else `ruro-host` looked up on `PATH`.
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
    let mut attempts = 0;
    let out = loop {
        match std::process::Command::new(host).arg("describe").output() {
            // A binary just written (an upload, a test's stand-in) is busy
            // while a process forked meanwhile still has it open.
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) && attempts < 50 => {
                attempts += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
            r => break r.with_context(|| format!("running `{} describe`", host.display()))?,
        }
    };
    if !out.status.success() {
        bail!(
            "`{} describe` failed: {}",
            host.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    serde_json::from_slice(&out.stdout).context("parsing node registry")
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
    pub estop: Arc<AtomicBool>,
    pub estop_latch: Mutex<estop::Latch>,
    pub recording: Mutex<Option<RecordSession>>,
    /// This daemon's part of a peer's recording, until the peer fetches it:
    /// `(peer, part)`.
    pub record_part: Mutex<Option<(String, ruro_proto::RecordedPart)>>,
    pub playback: Mutex<Option<Playback>>,
    /// Daemons running groups of the graph deployed here.
    pub peers: Mutex<BTreeSet<String>>,
    /// Last metrics seen per group: `(pid, when)`.
    pub heartbeats: Mutex<HashMap<String, (u32, Instant)>>,
    /// The last metrics each group published, as published, answered on
    /// request.
    pub metrics: Mutex<HashMap<String, Vec<u8>>>,
    /// The graph as deployed by the client (before flattening and machine
    /// assignment), returned by `Ctl::Graph`.
    pub deployed: Mutex<Option<String>>,
    /// The daemon whose graph this one runs part of; `None` when the graph
    /// was deployed here (or nothing runs).
    pub origin: Mutex<Option<String>>,
    /// What this daemon sent the peers running parts of its graph
    /// (`Ctl::DeployGroups` payload), to send again to one that comes back
    /// without it.
    pub forwarded: Mutex<Option<String>>,
    /// Peers of the graph that do not answer.
    pub unreachable: Mutex<BTreeSet<String>>,
    /// Held while the graph is deployed or stopped, so that a peer is not
    /// given its part again meanwhile.
    pub graph_ops: Mutex<()>,
    /// Set when the daemon trusts update keys.
    pub updates: Option<update::Updates>,
    /// An installed update, waiting for the reply to its request to go out.
    pub restart: Mutex<Option<update::Restart>>,
    /// Set once that reply is out: the daemon may stop and restart.
    pub restart_ready: AtomicBool,
    pub update_note: Option<String>,
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

    /// The machine that runs `group` of the running graph, if there is
    /// such a group. Deploys pin every group to a machine.
    pub fn machine_of_group(&self, group: &str) -> Option<String> {
        let sup = self.sup.lock();
        let graph = sup.graph()?;
        graph.groups().contains_key(group).then(|| {
            graph
                .group_config(group)
                .machine
                .unwrap_or_else(|| self.name.clone())
        })
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
    state: Arc<State>,
}

impl Daemon {
    /// Starts the daemon on background threads.
    /// Starts the daemon. It needs Linux or macOS: its hosts reach it over
    /// a Unix socket.
    #[cfg(not(unix))]
    pub fn start(config: Config) -> Result<Self> {
        bail!(
            "`{}`: the ruro daemon runs on Linux and macOS; from here, connect to one with --connect",
            config.name
        )
    }

    #[cfg(unix)]
    pub fn start(config: Config) -> Result<Self> {
        let dir = runtime_dir(&config.name);
        let created = !dir.exists();
        Self::start_in(config, &dir).inspect_err(|_| {
            // Leave nothing behind for a daemon that never ran. A directory
            // that existed may hold uploads, and one with a socket that
            // answers belongs to a daemon of the same name that started
            // meanwhile; only an empty directory is removed.
            let socket = dir.join("zenoh.sock");
            if created && std::os::unix::net::UnixStream::connect(&socket).is_err() {
                let _ = std::fs::remove_file(&socket);
                let _ = std::fs::remove_file(dir.join("auth"));
                let _ = std::fs::remove_dir(&dir);
            }
        })
    }

    #[cfg(unix)]
    fn start_in(config: Config, dir: &Path) -> Result<Self> {
        let registry = load_registry(&config.host)?;
        let updates = update::Updates::from_config(&config)?;
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;

        // Hosts reach the router over a private Unix socket, so they need
        // neither the network endpoint nor TLS certificates.
        let socket = dir.join("zenoh.sock");
        // A socket that answers belongs to a daemon of the same name;
        // removing it would cut that daemon off from its hosts.
        if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
            bail!(
                "a daemon named `{}` is already running (socket {})",
                config.name,
                socket.display()
            );
        }
        let _ = std::fs::remove_file(&socket);
        let local = format!("unixsock-stream/{}", socket.display());
        let dictionary = match &config.security.token {
            Some(token) => Some(write_dictionary(dir, token)?),
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
        let mut sup = Supervisor::new(
            config.name.clone(),
            config.host.clone(),
            local,
            config.security.token.clone(),
        )?;
        let estop = Arc::new(AtomicBool::new(false));
        sup.host_state.estopped = estop.clone();
        let state = Arc::new(State {
            name: config.name.clone(),
            keys: keys.clone(),
            session: session.clone(),
            sup: Mutex::new(sup),
            host: config.host.clone(),
            registry: Mutex::new(registry),
            estop,
            estop_latch: Mutex::new(estop::Latch::default()),
            recording: Mutex::new(None),
            record_part: Mutex::new(None),
            playback: Mutex::new(None),
            peers: Mutex::new(BTreeSet::new()),
            heartbeats: Mutex::new(HashMap::new()),
            metrics: Mutex::new(HashMap::new()),
            deployed: Mutex::new(None),
            origin: Mutex::new(None),
            forwarded: Mutex::new(None),
            unreachable: Mutex::new(BTreeSet::new()),
            graph_ops: Mutex::new(()),
            updates,
            restart: Mutex::new(None),
            restart_ready: AtomicBool::new(false),
            update_note: config.update_note.clone(),
        });
        if config.estopped {
            estop::handle(&state, "engage")?;
        }

        // One queryable per operation rather than one for `ctl/*`: a query
        // is answered only once every queryable it matches has finished
        // with it, and the e-stop must not wait for this loop.
        let (ctl_tx, queries) = std::sync::mpsc::channel();
        let mut queryables = Vec::new();
        for op in Ctl::ALL.into_iter().filter(|op| *op != Ctl::Estop) {
            let tx = ctl_tx.clone();
            queryables.push(
                session
                    .declare_queryable(keys.ctl(op))
                    .callback(move |q| {
                        let _ = tx.send(q);
                    })
                    .wait()
                    .map_err(|e| anyhow!("{e}"))?,
            );
        }
        // The e-stop has a queryable and a thread of its own: it never
        // waits behind a deploy or an update. Requests apply in the order
        // they arrive; passing them on to peers, which may take a while,
        // happens alongside.
        let estop_queryable = session
            .declare_queryable(keys.ctl(Ctl::Estop))
            .wait()
            .map_err(|e| anyhow!("{e}"))?;
        let estop_state = state.clone();
        std::thread::Builder::new()
            .name("ruro-estop".into())
            .spawn(move || {
                let st = estop_state;
                while let Ok(query) = estop_queryable.recv() {
                    let text = query
                        .payload()
                        .map(|p| String::from_utf8_lossy(&p.to_bytes()).into_owned())
                        .unwrap_or_default();
                    let key = format!("ruro/{}/ctl/estop", st.name);
                    let reply = move |r: Result<String>| {
                        let r: CtlReply = r.map_err(|e| format!("{e:#}"));
                        if let Err(e) = &r {
                            tracing::warn!("estop: {e}");
                        }
                        let _ = query.reply(key, ruro_proto::reply_bytes(&r)).wait();
                    };
                    match estop::begin(&st, &text) {
                        Ok((done, Some(rest))) => {
                            let st = st.clone();
                            std::thread::spawn(move || reply(rest.finish(&st, done)));
                        }
                        Ok((done, None)) => reply(Ok(done)),
                        Err(e) => reply(Err(e)),
                    }
                }
            })?;
        let stop = Arc::new(AtomicBool::new(false));
        // Stops passed on again, for machines that missed them.
        let (resend_state, resend_stop) = (state.clone(), stop.clone());
        std::thread::Builder::new()
            .name("ruro-estop-resend".into())
            .spawn(move || {
                let mut last = Instant::now();
                while !resend_stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(100));
                    if last.elapsed() >= estop::RESEND_INTERVAL {
                        estop::resend(&resend_state);
                        watch_peers(&resend_state);
                        last = Instant::now();
                    }
                }
            })?;

        let ctl_state = state.clone();
        std::thread::Builder::new()
            .name("ruro-ctl".into())
            .spawn(move || {
                let _queryables = queryables;
                while let Ok(query) = queries.recv() {
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
                    let bytes = if op == Ctl::RecordPart.as_str() {
                        // Binary chunks of a recording, not a text reply.
                        let reply: std::result::Result<Vec<u8>, String> =
                            session::serve_part(&ctl_state, &payload).map_err(|e| format!("{e:#}"));
                        ruro_proto::encode(&reply)
                    } else {
                        let reply: CtlReply =
                            handle(&ctl_state, &op, &payload).map_err(|e| format!("{e:#}"));
                        if let Err(e) = &reply {
                            tracing::warn!("{op}: {e}");
                        }
                        ruro_proto::reply_bytes(&reply)
                    };
                    let _ = query
                        .reply(format!("ruro/{}/ctl/{op}", ctl_state.name), bytes)
                        .wait();
                    drop(query);
                    if ctl_state.restart.lock().is_some() {
                        // Give the reply time to leave before the daemon
                        // closes its session.
                        std::thread::sleep(Duration::from_millis(300));
                        ctl_state.restart_ready.store(true, Ordering::Relaxed);
                    }
                }
            })?;

        // Heartbeats: every host publishes metrics twice a second.
        let hb = state.clone();
        let heartbeat_sub = session
            .declare_subscriber(keys.all_metrics())
            .callback(move |sample| {
                let bytes = sample.payload().to_bytes();
                if let Ok(m) = ruro_proto::decode::<GroupMetrics>(&bytes) {
                    hb.metrics
                        .lock()
                        .insert(m.group.clone(), bytes.into_owned());
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

        // The status is also answered on request, so a client need not wait
        // for the next publication.
        let status_queryable = session
            .declare_queryable(keys.status())
            .wait()
            .map_err(|e| anyhow!("{e}"))?;
        // Likewise the groups' last metrics, which hosts publish twice a
        // second.
        let metrics_queryable = session
            .declare_queryable(keys.all_metrics())
            .wait()
            .map_err(|e| anyhow!("{e}"))?;
        let status_state = state.clone();
        std::thread::Builder::new()
            .name("ruro-status".into())
            .spawn(move || {
                while let Ok(query) = status_queryable.recv() {
                    let status = status(&status_state, started);
                    let _ = query
                        .reply(status_state.keys.status(), ruro_proto::encode(&status))
                        .wait();
                }
            })?;
        let metrics_state = state.clone();
        std::thread::Builder::new()
            .name("ruro-metrics".into())
            .spawn(move || {
                while let Ok(query) = metrics_queryable.recv() {
                    let latest: Vec<(String, Vec<u8>)> = metrics_state
                        .metrics
                        .lock()
                        .iter()
                        .map(|(g, m)| (g.clone(), m.clone()))
                        .collect();
                    for (group, bytes) in latest {
                        let _ = query
                            .reply(metrics_state.keys.metrics(&group), bytes)
                            .wait();
                    }
                }
            })?;
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
                        let exits = sup.poll();
                        drop(sup);
                        // Next to the group's own logs, so tools that follow
                        // them see why it stopped.
                        for exit in exits {
                            let line = ruro_proto::LogLine {
                                ts_ns: ruro_msg::now_ns(),
                                level: ruro_proto::Level::Error,
                                group: exit.group.clone(),
                                node: None,
                                message: format!("exited: {}", exit.description),
                            };
                            let _ = st
                                .session
                                .put(st.keys.log(&exit.group), ruro_proto::encode(&line))
                                .wait();
                        }
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
            "daemon `{}` {} ({}) listening on {} with {} node types (host: {})",
            config.name,
            version(),
            TARGET,
            config.listen,
            state.registry.lock().nodes.len(),
            config.host.display()
        );
        if let Some(note) = &config.update_note {
            tracing::warn!("{note}");
        }
        Ok(Self {
            stop,
            thread: Some(thread),
            state,
        })
    }

    /// An update installed through this daemon, once the daemon may stop
    /// to start it: stop the daemon, then call [`update::Restart::exec`].
    pub fn take_restart(&self) -> Option<update::Restart> {
        if !self.state.restart_ready.load(Ordering::Relaxed) {
            return None;
        }
        self.state.restart_ready.store(false, Ordering::Relaxed);
        // The e-stop as it is now, not as it was when the update was
        // installed: requests were still served since.
        let mut restart = self.state.restart.lock().take()?;
        restart.set_estop(self.state.estop.load(Ordering::Relaxed));
        Some(restart)
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
#[cfg(unix)]
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
        version: version().into(),
        pid: std::process::id(),
        uptime_s: started.elapsed().as_secs_f64(),
        graph: s.graph().map(|g| g.name.clone()),
        groups: s.status(),
        recording: st.recording.lock().as_ref().map(|r| r.describe()),
        playback: st.playback.lock().as_ref().map(Playback::status),
        estopped: st.estop.load(Ordering::Relaxed),
        target: TARGET.into(),
        update: st.update_note.clone(),
        estop_by: st.estop_latch.lock().origins(),
        unreachable: st.unreachable.lock().iter().cloned().collect(),
    }
}

fn handle(st: &Arc<State>, op: &str, payload: &[u8]) -> Result<String> {
    let text = || String::from_utf8_lossy(payload).into_owned();
    match op {
        "deploy" => deploy(st, &text()),
        "deploy-groups" => {
            let req: ruro_proto::GroupsDeploy =
                serde_json::from_slice(payload).context("bad forwarded deploy")?;
            let graph = Graph::from_yaml(&req.graph)?;
            // Part of another daemon's graph now: the groups this daemon
            // forwarded for its own previous graph must go, except on
            // machines the new graph uses (they get their part from the
            // origin, which waits for this reply and must not be asked).
            let mut keep: BTreeSet<String> = graph
                .groups
                .values()
                .filter_map(|c| c.machine.clone())
                .collect();
            keep.insert(req.origin.clone());
            let _ops = st.graph_ops.lock();
            *st.forwarded.lock() = None;
            st.unreachable.lock().clear();
            session::stop_playback(st);
            st.sup.lock().stop_all();
            // Before the peers stop: a recording collects their parts.
            session::finish_recording(st, false)?;
            let peers = std::mem::take(&mut *st.peers.lock());
            stop_peers(st, peers, &keep);
            let n = st.sup.lock().deploy(graph)?;
            *st.deployed.lock() = Some(req.graph);
            *st.origin.lock() = Some(req.origin);
            Ok(format!("deployed {n} group(s) on `{}`", st.name))
        }
        "stop" => {
            let text = text();
            if let Some(from) = text.trim().strip_prefix("from ") {
                // A daemon stopping what it forwarded; this daemon may have
                // been given another graph since.
                let origin = st.origin.lock().clone();
                if origin.as_deref() != Some(from) {
                    return Ok(format!(
                        "left running: `{}` runs no graph from `{from}`",
                        st.name
                    ));
                }
            }
            Ok(match stop_graph(st)? {
                Some(r) => format!("stopped; {r}"),
                None => "stopped".into(),
            })
        }
        "registry" => {
            let host = text();
            let host = host.trim();
            if !host.is_empty() {
                // A custom host groups can select, such as an uploaded one.
                let path = st.sup.lock().resolve_host(Some(host))?;
                return Ok(serde_json::to_string(&load_registry(&path)?)?);
            }
            // Re-read so newly built node types show up without a restart.
            if let Ok(r) = load_registry(&st.host) {
                *st.registry.lock() = r;
            }
            Ok(serde_json::to_string(&*st.registry.lock())?)
        }
        "param" => {
            let req: ruro_proto::ParamRequest =
                serde_json::from_slice(payload).context("bad parameter request")?;
            set_param(st, &req)
        }
        "validate" => {
            let graph = pinned(st, &text())?;
            Ok(serde_json::to_string(&check(st, &graph)?)?)
        }
        "graph" => Ok(match st.sup.lock().graph() {
            Some(_) => st.deployed.lock().clone().unwrap_or_default(),
            None => String::new(),
        }),
        "ping" => {
            // A client looking for the daemon it connected to names it.
            if let Some(zid) = text().strip_prefix(ruro_proto::PING_ZID)
                && zid.trim() != st.session.zid().to_string()
            {
                bail!("not this daemon");
            }
            Ok(st.name.clone())
        }
        "clock" => Ok(ruro_msg::now_ns().to_string()),
        "kill" => {
            let group = text().trim().to_owned();
            // Groups of a graph that spans machines run where it pins them.
            if let Some(machine) = st.machine_of_group(&group)
                && machine != st.name
            {
                return st.peer_request(&machine, Ctl::Kill, &group);
            }
            st.sup.lock().kill(&group)?;
            Ok(format!("killed `{group}`"))
        }
        "record" => session::record(st, &text()),
        "playback" => {
            let cmd = serde_json::from_slice(payload).context("bad playback command")?;
            session::playback(st, cmd)
        }
        "upload" => {
            let chunk: ruro_proto::UploadChunk = ruro_proto::decode(payload)?;
            let dir = st.sup.lock().hosts_dir();
            upload::receive(&dir, &chunk)
        }
        "update-upload" => update::receive(st, &ruro_proto::decode(payload)?),
        "update" => {
            let req = serde_json::from_slice(payload).context("bad update request")?;
            update::update(st, &req)
        }
        other => bail!("unknown operation `{other}`"),
    }
}

/// Stops the running graph, here and on the peers running parts of it,
/// with any replay and recording. Returns what the recording wrote.
fn stop_graph(st: &Arc<State>) -> Result<Option<String>> {
    let _ops = st.graph_ops.lock();
    *st.forwarded.lock() = None;
    st.unreachable.lock().clear();
    session::stop_playback(st);
    st.sup.lock().stop_all();
    *st.origin.lock() = None;
    // Peers stop even if the recording could not be finished.
    let recorded = session::finish_recording(st, false);
    let peers = std::mem::take(&mut *st.peers.lock());
    stop_peers(st, peers, &BTreeSet::new());
    recorded
}

/// Checks on the peers running parts of this daemon's graph: notes those
/// that do not answer, and gives one that answers without a graph (it was
/// restarted, or the machine rebooted) its part again, with the e-stop
/// first if it is engaged. A peer running another graph is left alone.
fn watch_peers(st: &Arc<State>) {
    let Some(forward) = st.forwarded.lock().clone() else {
        return;
    };
    let peers = st.peers.lock().clone();
    for m in peers {
        let reply = ruro_proto::request(
            &st.session,
            &Keys::new(&m),
            Ctl::Graph,
            "",
            Duration::from_secs(2),
        );
        let Ok(graph) = reply else {
            if st.unreachable.lock().insert(m.clone()) {
                tracing::warn!("`{m}`, which runs part of the graph, does not answer");
            }
            continue;
        };
        if st.unreachable.lock().remove(&m) {
            tracing::info!("`{m}` answers again");
        }
        if !graph.trim().is_empty() {
            continue;
        }
        let _ops = st.graph_ops.lock();
        // Still the same graph, and still a machine of it?
        if st.forwarded.lock().as_deref() != Some(forward.as_str()) || !st.peers.lock().contains(&m)
        {
            return;
        }
        tracing::warn!("`{m}` runs nothing: deploying its part of the graph again");
        let one = BTreeSet::from([m.clone()]);
        let sent = estop::forward_on_deploy(st, &one)
            .and_then(|()| st.peer_request(&m, Ctl::DeployGroups, &forward));
        if let Err(e) = sent {
            tracing::warn!("deploying the part of `{m}` again: {e:#}");
        }
    }
}

/// Validates `yaml`, sends the groups of other machines to their daemons and
/// starts the local ones.
fn deploy(st: &Arc<State>, yaml: &str) -> Result<String> {
    let graph = pinned(st, yaml)?;
    let issues = check(st, &graph)?;
    if !issues.is_empty() {
        bail!("graph is invalid:\n  {}", issues.join("\n  "));
    }
    let machines = peer_machines(st, &graph);

    let _ops = st.graph_ops.lock();
    *st.forwarded.lock() = None;
    st.unreachable.lock().clear();
    session::stop_playback(st);
    st.sup.lock().stop_all();
    session::finish_recording(st, false)?;
    let peers = std::mem::take(&mut *st.peers.lock());
    stop_peers(st, peers, &machines);
    *st.origin.lock() = None;
    let forward = serde_json::to_string(&ruro_proto::GroupsDeploy {
        origin: st.name.clone(),
        graph: graph.to_yaml()?,
    })?;
    // The machines join the graph before the stop is checked: a stop
    // engaged from here on is passed on to them, and one engaged before is
    // passed on now, so their hosts start stopped.
    st.peers.lock().extend(machines.iter().cloned());
    estop::forward_on_deploy(st, &machines)?;
    for m in &machines {
        st.peer_request(m, Ctl::DeployGroups, &forward)?;
    }
    let n = st.sup.lock().deploy(graph)?;
    *st.deployed.lock() = Some(yaml.to_owned());
    if !machines.is_empty() {
        *st.forwarded.lock() = Some(forward);
    }
    Ok(if machines.is_empty() {
        format!("deployed {n} group(s)")
    } else {
        format!(
            "deployed {n} group(s) here and the rest on {}",
            machines.into_iter().collect::<Vec<_>>().join(", ")
        )
    })
}

/// Checks a parameter change against the descriptor of the node's type, as
/// linked into the host of the node's group, and has that host apply it.
/// Nodes on other machines are left to their daemon.
fn set_param(st: &State, req: &ruro_proto::ParamRequest) -> Result<String> {
    let (kind, group, config) = {
        let sup = st.sup.lock();
        let graph = sup.graph().context("no graph is running")?;
        let node = graph
            .nodes
            .get(&req.node)
            .with_context(|| format!("no node `{}` in graph `{}`", req.node, graph.name))?;
        let group = node.group().to_owned();
        (node.kind.clone(), group.clone(), graph.group_config(&group))
    };
    if let Some(machine) = config.machine.filter(|m| *m != st.name) {
        return st.peer_request(&machine, Ctl::Param, &serde_json::to_string(req)?);
    }
    let registry = match &config.host {
        None => st.registry.lock().clone(),
        Some(h) => load_registry(&st.sup.lock().resolve_host(Some(h))?)?,
    };
    let desc = registry
        .get(&kind)
        .with_context(|| format!("the host of group `{group}` has no node type `{kind}`"))?;
    let node = &req.node;
    let value = desc
        .param_or_suggest(&req.name)
        .and_then(|p| p.parse(&req.value))
        .map_err(|e| anyhow!("{node}: {e}"))?;
    let cmd = ruro_proto::HostCommand::SetParam {
        node: node.clone(),
        name: req.name.clone(),
        value: value.to_string(),
    };
    st.host_request(&group, &cmd)
}

/// Parses and flattens a graph and pins every group to a machine (this
/// daemon unless it names one), so each daemon knows exactly what it runs.
fn pinned(st: &State, yaml: &str) -> Result<Graph> {
    let mut graph = Graph::from_yaml(yaml)?.flatten()?;
    for group in graph.groups().into_keys() {
        graph
            .groups
            .entry(group)
            .or_default()
            .machine
            .get_or_insert_with(|| st.name.clone());
    }
    Ok(graph)
}

/// The other machines a pinned graph runs groups on.
fn peer_machines(st: &State, graph: &Graph) -> BTreeSet<String> {
    graph
        .groups
        .values()
        .filter_map(|c| c.machine.clone())
        .filter(|m| *m != st.name)
        .collect()
}

/// The node types of the host that runs a group: `(machine, host)`, where
/// `host` is `None` for the machine's default host.
type HostKey = (String, Option<String>);

/// Everything that would keep a pinned graph from running: invalid nodes
/// and wires, nodes placed on hosts that lack their type, wires between
/// hosts built with different message definitions, and the runtime's
/// conventions. Asks peers for the node types of the hosts they would use.
fn check(st: &State, graph: &Graph) -> Result<Vec<String>> {
    let hosts: BTreeSet<HostKey> = graph
        .groups()
        .into_keys()
        .map(|g| {
            let c = graph.group_config(&g);
            (c.machine.unwrap_or_else(|| st.name.clone()), c.host)
        })
        .collect();
    let mut registries: HashMap<HostKey, Registry> = HashMap::new();
    let mut issues = Vec::new();
    for key in hosts {
        let (machine, host) = &key;
        let registry = if *machine == st.name {
            match host {
                None => Ok(st.registry.lock().clone()),
                Some(h) => st
                    .sup
                    .lock()
                    .resolve_host(Some(h))
                    .and_then(|p| load_registry(&p)),
            }
        } else {
            st.peer_request(machine, Ctl::Registry, host.as_deref().unwrap_or_default())
                .and_then(|body| serde_json::from_str(&body).context("peer registry"))
        };
        match registry {
            Ok(r) => {
                registries.insert(key, r);
            }
            Err(e) if host.is_none() => return Err(e),
            Err(e) => issues.push(format!(
                "host `{}` on `{machine}`: {e:#}",
                host.as_deref().unwrap_or_default()
            )),
        }
    }
    // Every type any of the hosts offers; whether a node's own host offers
    // it is checked by placement.
    let all = Registry::new(registries.values().flat_map(Registry::iter).cloned());
    let offered = |group: &str| {
        let c = graph.group_config(group);
        registries.get(&(c.machine.unwrap_or_else(|| st.name.clone()), c.host))
    };
    let found = ruro_graph::validate(graph, &all)
        .into_iter()
        .chain(ruro_graph::runtime_issues(graph, &all))
        .chain(ruro_graph::placement_issues(graph, &all, offered))
        .chain(ruro_graph::schema_issues(graph, offered));
    issues.extend(found.map(|i| i.to_string()));
    Ok(issues)
}

/// Stops the groups this daemon forwarded to `peers`, except on the
/// machines in `keep`. A peer that has since been given a graph by another
/// daemon keeps it.
fn stop_peers(st: &State, peers: BTreeSet<String>, keep: &BTreeSet<String>) {
    let from = format!("from {}", st.name);
    for p in peers.difference(keep) {
        if let Err(e) = st.peer_request(p, Ctl::Stop, &from) {
            tracing::warn!("{e:#}");
        }
    }
}
