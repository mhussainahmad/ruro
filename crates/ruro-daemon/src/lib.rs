//! The ruro daemon: one per machine.
//!
//! It runs the Zenoh router every other ruro process connects to, validates
//! and deploys graphs, starts one host process per group and restarts them
//! according to their restart policy.

mod supervisor;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use parking_lot::Mutex;
use ruro_graph::{Graph, Registry};
use ruro_proto::{Ctl, CtlReply, DaemonStatus, Keys};
use zenoh::Wait;

use crate::supervisor::Supervisor;

/// Default port of the daemon's router.
pub const DEFAULT_PORT: u16 = 7447;

#[derive(Debug, Clone)]
pub struct Config {
    /// Daemon name; defaults to the hostname.
    pub name: String,
    /// Zenoh endpoint to listen on, e.g. `tcp/127.0.0.1:7447`.
    pub listen: String,
    /// Host binary used to run groups.
    pub host: PathBuf,
}

impl Config {
    pub fn new() -> Self {
        Self {
            name: ruro_proto::hostname(),
            listen: format!("tcp/127.0.0.1:{DEFAULT_PORT}"),
            host: default_host_binary(),
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

/// Endpoint that local child processes use to reach a router listening on `listen`.
fn local_endpoint(listen: &str) -> String {
    listen
        .replace("0.0.0.0", "127.0.0.1")
        .replace("[::]", "[::1]")
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
        let session = zenoh::open(ruro_proto::router_config(&config.listen)?)
            .wait()
            .map_err(|e| anyhow!("cannot listen on {}: {e}", config.listen))?;
        let keys = Keys::new(&config.name);
        let sup = Arc::new(Mutex::new(Supervisor::new(
            config.name.clone(),
            config.host.clone(),
            local_endpoint(&config.listen),
        )?));

        let queryable = session
            .declare_queryable(format!("ruro/{}/ctl/*", config.name))
            .wait()
            .map_err(|e| anyhow!("{e}"))?;
        let stop = Arc::new(AtomicBool::new(false));

        let ctl_sup = sup.clone();
        let ctl_keys = keys.clone();
        let ctl_registry = registry.clone();
        let host = config.host.clone();
        std::thread::Builder::new()
            .name("ruro-ctl".into())
            .spawn(move || {
                let mut registry = ctl_registry;
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
                        .map(|p| String::from_utf8_lossy(&p.to_bytes()).into_owned());
                    let payload = payload.unwrap_or_default();
                    if op == Ctl::Registry.as_str() {
                        // Re-read so newly built node types show up without a restart.
                        if let Ok(r) = load_registry(&host) {
                            registry = r;
                        }
                    }
                    let reply: CtlReply =
                        handle(&op, &payload, &ctl_sup, &registry).map_err(|e| format!("{e:#}"));
                    if let Err(e) = &reply {
                        tracing::warn!("{op}: {e}");
                    }
                    let _ = query
                        .reply(ctl_keys.ctl_key_for(&op), ruro_proto::reply_bytes(&reply))
                        .wait();
                }
            })?;

        let loop_stop = stop.clone();
        let name = config.name.clone();
        let started = Instant::now();
        let thread = std::thread::Builder::new()
            .name("ruro-supervisor".into())
            .spawn(move || {
                let mut last_status = Instant::now() - Duration::from_secs(1);
                while !loop_stop.load(Ordering::Relaxed) {
                    sup.lock().poll();
                    if last_status.elapsed() >= Duration::from_millis(500) {
                        last_status = Instant::now();
                        let s = sup.lock();
                        let status = DaemonStatus {
                            name: name.clone(),
                            version: env!("CARGO_PKG_VERSION").into(),
                            pid: std::process::id(),
                            uptime_s: started.elapsed().as_secs_f64(),
                            graph: s.graph().map(|g| g.name.clone()),
                            groups: s.status(),
                            recording: None,
                            playback: None,
                        };
                        drop(s);
                        let _ = session
                            .put(keys.status(), ruro_proto::encode(&status))
                            .wait();
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                sup.lock().stop_all();
                let _ = session.close().wait();
            })?;

        tracing::info!(
            "daemon `{}` listening on {} with {} node types (host: {})",
            config.name,
            config.listen,
            registry.nodes.len(),
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

fn handle(op: &str, payload: &str, sup: &Mutex<Supervisor>, registry: &Registry) -> Result<String> {
    match op {
        "deploy" => {
            let graph = Graph::from_yaml(payload)?;
            let issues = ruro_graph::validate(&graph, registry);
            if !issues.is_empty() {
                let list: Vec<String> = issues.iter().map(ToString::to_string).collect();
                bail!("graph is invalid:\n  {}", list.join("\n  "));
            }
            let groups = sup.lock().deploy(graph)?;
            Ok(format!("deployed {groups} group(s)"))
        }
        "stop" => {
            sup.lock().stop_all();
            Ok("stopped".into())
        }
        "registry" => Ok(serde_json::to_string(registry)?),
        "graph" => Ok(sup
            .lock()
            .graph()
            .map(|g| g.to_yaml())
            .transpose()?
            .unwrap_or_default()),
        "ping" => Ok(sup.lock().daemon().to_owned()),
        "kill" => {
            sup.lock().kill(payload.trim())?;
            Ok(format!("killed `{}`", payload.trim()))
        }
        other => bail!("unknown operation `{other}`"),
    }
}

trait CtlKey {
    fn ctl_key_for(&self, op: &str) -> String;
}

impl CtlKey for Keys {
    fn ctl_key_for(&self, op: &str) -> String {
        format!("ruro/{}/ctl/{op}", self.daemon())
    }
}
