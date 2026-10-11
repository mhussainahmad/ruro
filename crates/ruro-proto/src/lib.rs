//! Control-plane protocol shared by the daemon, hosts, CLI and studio.
//!
//! Everything travels over Zenoh under `ruro/<daemon>/…`. The daemon runs a
//! Zenoh router; every other process connects to it as a client, so a studio
//! on a laptop can drive a daemon on a robot by pointing at its address.
//!
//! High-rate data (metrics, taps, logs) is postcard-encoded. Control requests
//! carry text (YAML graphs, JSON values) so they stay debuggable.

use anyhow::{Context, Result, anyhow};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub use ruro_msg::Preview;

/// Where clients look for a daemon by default.
pub const DEFAULT_ENDPOINT: &str = "tcp/127.0.0.1:7447";

/// Key expressions for one daemon.
#[derive(Debug, Clone)]
pub struct Keys {
    daemon: String,
    prefix: String,
}

impl Keys {
    pub fn new(daemon: &str) -> Self {
        Self {
            daemon: daemon.to_owned(),
            prefix: format!("ruro/{daemon}"),
        }
    }

    pub fn daemon(&self) -> &str {
        &self.daemon
    }

    /// Request/reply endpoint for a control operation.
    pub fn ctl(&self, op: Ctl) -> String {
        format!("{}/ctl/{}", self.prefix, op.as_str())
    }

    pub fn status(&self) -> String {
        format!("{}/status", self.prefix)
    }

    /// Matches the status of every daemon on the network.
    pub fn all_status() -> &'static str {
        "ruro/*/status"
    }

    pub fn metrics(&self, group: &str) -> String {
        format!("{}/metrics/{group}", self.prefix)
    }

    pub fn all_metrics(&self) -> String {
        format!("{}/metrics/*", self.prefix)
    }

    pub fn tap(&self, node: &str, port: &str) -> String {
        format!("{}/tap/{node}/{port}", self.prefix)
    }

    pub fn param(&self, node: &str) -> String {
        format!("{}/param/{node}", self.prefix)
    }

    pub fn log(&self, group: &str) -> String {
        format!("{}/log/{group}", self.prefix)
    }

    pub fn all_logs(&self) -> String {
        format!("{}/log/**", self.prefix)
    }

    /// Shared-memory topic name for an exported output port.
    pub fn shm_topic(&self, graph: &str, node: &str, port: &str) -> String {
        format!("ruro/{}/{graph}/{node}/{port}", self.daemon)
    }
}

/// Control operations served by the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ctl {
    /// Payload: graph YAML. Replaces the running graph.
    Deploy,
    /// Stops the running graph.
    Stop,
    /// Reply: JSON [`ruro_graph::Registry`].
    Registry,
    /// Reply: YAML of the running graph (empty if none).
    Graph,
    /// Payload: group name. Kills the group process (for testing supervision).
    Kill,
    /// Reply: the daemon name. Used for discovery.
    Ping,
}

impl Ctl {
    pub fn as_str(self) -> &'static str {
        match self {
            Ctl::Deploy => "deploy",
            Ctl::Stop => "stop",
            Ctl::Registry => "registry",
            Ctl::Graph => "graph",
            Ctl::Kill => "kill",
            Ctl::Ping => "ping",
        }
    }
}

/// Reply to every control request: `Ok(body)` or `Err(message)`.
pub type CtlReply = std::result::Result<String, String>;

/// Published by the daemon twice a second.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub name: String,
    pub version: String,
    pub pid: u32,
    pub uptime_s: f64,
    /// Name of the deployed graph, if any.
    pub graph: Option<String>,
    pub groups: Vec<GroupStatus>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupStatus {
    pub name: String,
    pub nodes: Vec<String>,
    pub state: GroupState,
    pub pid: Option<u32>,
    pub restarts: u32,
    /// Description of the last unexpected exit.
    pub last_exit: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GroupState {
    Running,
    /// Waiting before a restart.
    Backoff,
    /// Exited and will not be restarted.
    Failed,
    Stopped,
}

/// Published by every host twice a second.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupMetrics {
    pub group: String,
    pub pid: u32,
    pub nodes: Vec<NodeMetrics>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeMetrics {
    pub id: String,
    pub state: NodeState,
    /// Steps per second over the last window.
    pub hz: f64,
    pub step_p50_us: f64,
    pub step_p99_us: f64,
    /// Mean time from send to receive of consumed inputs, in milliseconds.
    pub input_age_ms: Option<f64>,
    /// CPU used by the node thread, in percent of one core.
    pub cpu_pct: f64,
    pub errors: u64,
    pub last_error: Option<String>,
    /// Messages dropped by full input queues since start.
    pub dropped: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeState {
    Starting,
    Running,
    /// `start` failed or the node panicked; it no longer steps.
    Failed,
    Stopped,
}

/// Parameter change sent to `param/<node>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetParam {
    pub name: String,
    /// JSON-encoded value.
    pub value: String,
}

/// One preview of a watched output port.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TapFrame {
    pub seq: u64,
    pub preview: Preview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogLine {
    pub ts_ns: u64,
    pub level: Level,
    pub group: String,
    /// Node that logged, if the line came from a node thread.
    pub node: Option<String>,
    pub message: String,
}

/// Encodes a high-rate payload.
pub fn encode<T: Serialize>(value: &T) -> Vec<u8> {
    postcard::to_allocvec(value).expect("protocol types are serializable")
}

/// Decodes a high-rate payload.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    postcard::from_bytes(bytes).context("malformed protocol message")
}

/// Zenoh configuration for a daemon: a router listening on `listen`.
pub fn router_config(listen: &str) -> Result<zenoh::Config> {
    let mut c = zenoh::Config::default();
    set(&mut c, "mode", "\"router\"")?;
    set(&mut c, "listen/endpoints", &format!("[\"{listen}\"]"))?;
    set(&mut c, "scouting/multicast/enabled", "false")?;
    Ok(c)
}

/// Zenoh configuration for a client of the daemon at `endpoint`.
/// Opening fails immediately if nothing is listening.
pub fn client_config(endpoint: &str) -> Result<zenoh::Config> {
    let mut c = zenoh::Config::default();
    set(&mut c, "mode", "\"client\"")?;
    set(&mut c, "connect/endpoints", &format!("[\"{endpoint}\"]"))?;
    set(&mut c, "scouting/multicast/enabled", "false")?;
    Ok(c)
}

fn set(c: &mut zenoh::Config, key: &str, value: &str) -> Result<()> {
    c.insert_json5(key, value)
        .map_err(|e| anyhow!("zenoh config `{key}`: {e}"))
}

/// Sends a control request and waits for the reply.
pub fn request(
    session: &zenoh::Session,
    keys: &Keys,
    op: Ctl,
    payload: &str,
    timeout: std::time::Duration,
) -> Result<String> {
    use zenoh::Wait;
    let replies = session
        .get(keys.ctl(op))
        .payload(payload.as_bytes().to_vec())
        .timeout(timeout)
        .wait()
        .map_err(|e| anyhow!("request failed: {e}"))?;
    let reply = replies
        .recv()
        .map_err(|_| anyhow!("no reply from daemon `{}` (is it running?)", keys.daemon()))?;
    let sample = reply
        .result()
        .map_err(|e| anyhow!("daemon error: {:?}", e.payload().to_bytes()))?;
    let body: CtlReply =
        serde_json::from_slice(&sample.payload().to_bytes()).context("malformed reply")?;
    body.map_err(|e| anyhow!(e))
}

/// Opens a client session to the daemon at `endpoint`.
pub fn connect(endpoint: &str) -> Result<zenoh::Session> {
    use zenoh::Wait;
    zenoh::open(client_config(endpoint)?)
        .wait()
        .map_err(|e| anyhow!("no ruro daemon reachable at {endpoint} ({e})"))
}

/// Finds the daemon behind a session. With `name` set, only checks that it answers.
pub fn discover(
    session: &zenoh::Session,
    name: Option<&str>,
    timeout: std::time::Duration,
) -> Result<Keys> {
    use zenoh::Wait;
    let key = format!("ruro/{}/ctl/ping", name.unwrap_or("*"));
    let replies = session
        .get(key)
        .timeout(timeout)
        .wait()
        .map_err(|e| anyhow!("discovery failed: {e}"))?;
    let reply = replies.recv().map_err(|_| match name {
        Some(n) => anyhow!("daemon `{n}` did not answer"),
        None => anyhow!("no daemon answered"),
    })?;
    let sample = reply
        .result()
        .map_err(|_| anyhow!("daemon returned an error to ping"))?;
    let body: CtlReply =
        serde_json::from_slice(&sample.payload().to_bytes()).context("malformed reply")?;
    Ok(Keys::new(&body.map_err(|e| anyhow!(e))?))
}

/// Encodes a control reply.
pub fn reply_bytes(reply: &CtlReply) -> Vec<u8> {
    serde_json::to_vec(reply).expect("reply is serializable")
}

/// Hostname, used as the default daemon name.
pub fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| ruro_graph::is_valid_ident(s))
        .unwrap_or_else(|| "local".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        let k = Keys::new("bot");
        assert_eq!(k.ctl(Ctl::Deploy), "ruro/bot/ctl/deploy");
        assert_eq!(k.tap("cam", "image"), "ruro/bot/tap/cam/image");
        assert_eq!(
            k.shm_topic("demo", "cam", "image"),
            "ruro/bot/demo/cam/image"
        );
    }

    #[test]
    fn metrics_roundtrip() {
        let m = GroupMetrics {
            group: "main".into(),
            pid: 1,
            nodes: vec![NodeMetrics {
                id: "cam".into(),
                state: NodeState::Running,
                hz: 30.0,
                step_p50_us: 100.0,
                step_p99_us: 400.0,
                input_age_ms: Some(0.2),
                cpu_pct: 3.5,
                errors: 0,
                last_error: None,
                dropped: 0,
            }],
        };
        assert_eq!(decode::<GroupMetrics>(&encode(&m)).unwrap(), m);
    }

    #[test]
    fn configs_are_valid() {
        router_config("tcp/127.0.0.1:0").unwrap();
        client_config(DEFAULT_ENDPOINT).unwrap();
    }
}
