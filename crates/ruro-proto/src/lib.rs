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

    /// Emergency stop state, a [`ruro_msg::Flag`] (postcard). The daemon
    /// latches it and republishes it with every status.
    pub fn estop(&self) -> String {
        format!("{}/estop", self.prefix)
    }

    /// Request/reply endpoint of one host process, for [`HostCommand`]s.
    pub fn host_ctl(&self, group: &str) -> String {
        format!("{}/host/{group}/ctl", self.prefix)
    }

    /// Replayed messages for an output port, as [`DataFrame`]s.
    pub fn inject(&self, node: &str, port: &str) -> String {
        format!("{}/inject/{node}/{port}", self.prefix)
    }

    /// Data-plane key of an output port whose consumers run on another
    /// machine. Not scoped to a daemon: both ends name it from the graph.
    pub fn data(graph: &str, node: &str, port: &str) -> String {
        format!("ruro/data/{graph}/{node}/{port}")
    }
}

/// Control operations served by the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ctl {
    /// Payload: graph YAML. Replaces the running graph.
    Deploy,
    /// Stops the running graph. Payload: empty, or `from <daemon>` (sent by
    /// daemons to their peers) to stop the graph only if that daemon
    /// forwarded it.
    Stop,
    /// Reply: JSON [`ruro_graph::Registry`].
    Registry,
    /// Reply: YAML of the running graph (empty if none).
    Graph,
    /// Payload: group name. Kills the group process (for testing supervision).
    Kill,
    /// Reply: the daemon name. Used for discovery.
    Ping,
    /// Payload: path of an MCAP file to record every wire of the running
    /// graph into, or empty to stop recording. Reply: the path.
    Record,
    /// Payload: JSON [`PlaybackCommand`]. Controls replay of a recording.
    Playback,
    /// Payload: `engage` or `release`. Latches the emergency stop.
    Estop,
    /// Payload: JSON [`GroupsDeploy`]. Sent by the daemon a graph was
    /// deployed to, asking a peer to run its groups.
    DeployGroups,
    /// Payload: postcard [`UploadChunk`]. Stores a host binary.
    Upload,
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
            Ctl::Record => "record",
            Ctl::Playback => "playback",
            Ctl::Estop => "estop",
            Ctl::DeployGroups => "deploy-groups",
            Ctl::Upload => "upload",
        }
    }
}

/// Payload of [`Ctl::DeployGroups`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupsDeploy {
    /// The daemon the graph was deployed to, which forwards it.
    pub origin: String,
    /// Graph YAML with `machine` set on every group.
    pub graph: String,
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
    /// File being recorded to, if recording.
    pub recording: Option<String>,
    /// Replay in progress, if any.
    pub playback: Option<PlaybackStatus>,
    /// True while the emergency stop is engaged.
    pub estopped: bool,
}

/// State of a replay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaybackStatus {
    pub file: String,
    /// Current position and total length, in seconds from the start of the recording.
    pub position_s: f64,
    pub duration_s: f64,
    pub paused: bool,
    /// Playback speed; 1.0 is real time.
    pub rate: f64,
}

/// A replay control request, sent as JSON with [`Ctl::Playback`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum PlaybackCommand {
    /// Replay `file` into the running graph, replacing the outputs of the
    /// nodes that recorded them.
    Start {
        file: String,
        rate: f64,
    },
    Pause,
    Resume,
    Seek {
        position_s: f64,
    },
    SetRate {
        rate: f64,
    },
    Stop,
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
    /// Scheduling quality, for timer-driven nodes and nodes with a deadline.
    pub timing: Option<TimingMetrics>,
}

/// Wakeup jitter, deadline misses and cycle overruns of one node.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TimingMetrics {
    /// Lateness of timer wakeups (actual minus scheduled) over the last
    /// window, in microseconds. Zero for nodes without a timer.
    pub jitter_p50_us: f64,
    pub jitter_p99_us: f64,
    pub jitter_max_us: f64,
    /// Steps that exceeded the node's deadline, since start.
    pub deadline_misses: u64,
    /// Cycles of the node's deterministic group that ran over their period,
    /// since start. Always zero outside deterministic groups.
    pub overruns: u64,
}

/// Control requests a daemon sends to one of its host processes (JSON).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum HostCommand {
    /// Record every output of the group into `path` (an MCAP segment), or
    /// stop recording and finish the file when `path` is `None`.
    Record { path: Option<String> },
    /// Nodes whose outputs are being replayed: they stop stepping and their
    /// outputs carry injected messages instead. Empty resumes every node.
    Mute { nodes: Vec<String> },
}

/// One chunk of an uploaded host binary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UploadChunk {
    /// Name groups use to request the binary (`groups.<g>.host`).
    pub name: String,
    pub offset: u64,
    pub total: u64,
    /// SHA-256 of the whole file, lowercase hex.
    pub sha256: String,
    pub data: Vec<u8>,
}

/// A message on the network data plane (cross-machine wires and replay):
/// sequence number, send stamp and the message encoding, lz4-compressed when
/// that pays off.
#[derive(Debug, Clone, PartialEq)]
pub struct DataFrame {
    pub seq: u64,
    pub stamp_ns: u64,
    pub body: Vec<u8>,
}

/// Payloads at least this large are lz4-compressed when compression is on.
pub const COMPRESS_MIN: usize = 4096;

const FRAME_HEADER: usize = 17;
const FLAG_LZ4: u8 = 1;

impl DataFrame {
    /// Encodes a frame. With `compress`, bodies of at least [`COMPRESS_MIN`]
    /// bytes are lz4-compressed if that makes them smaller.
    pub fn encode(seq: u64, stamp_ns: u64, body: &[u8], compress: bool) -> Vec<u8> {
        let packed = (compress && body.len() >= COMPRESS_MIN)
            .then(|| lz4_flex::compress_prepend_size(body))
            .filter(|c| c.len() < body.len());
        let (flags, payload) = match &packed {
            Some(c) => (FLAG_LZ4, c.as_slice()),
            None => (0, body),
        };
        let mut out = Vec::with_capacity(FRAME_HEADER + payload.len());
        out.push(flags);
        out.extend_from_slice(&seq.to_le_bytes());
        out.extend_from_slice(&stamp_ns.to_le_bytes());
        out.extend_from_slice(payload);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        anyhow::ensure!(
            bytes.len() >= FRAME_HEADER,
            "data frame shorter than its header"
        );
        let seq = u64::from_le_bytes(bytes[1..9].try_into().unwrap());
        let stamp_ns = u64::from_le_bytes(bytes[9..17].try_into().unwrap());
        let payload = &bytes[FRAME_HEADER..];
        let body = match bytes[0] {
            0 => payload.to_vec(),
            FLAG_LZ4 => lz4_flex::decompress_size_prepended(payload)
                .map_err(|e| anyhow!("corrupt compressed frame: {e}"))?,
            f => anyhow::bail!("unknown data frame flags {f:#x}"),
        };
        Ok(Self {
            seq,
            stamp_ns,
            body,
        })
    }
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

/// Transport security for daemons and their clients.
///
/// TLS applies to `tls/...` endpoints: a daemon presents `cert`/`key`, and
/// clients verify it against `ca`. A daemon that is also given a `ca`
/// requires clients to present a certificate signed by it (mutual TLS),
/// which clients configure with their own `cert`/`key`. `token` turns on Zenoh's
/// username/password authentication with the fixed user `ruro`: a daemon
/// accepts only sessions that present the same token, on every endpoint.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Security {
    pub cert: Option<std::path::PathBuf>,
    pub key: Option<std::path::PathBuf>,
    pub ca: Option<std::path::PathBuf>,
    pub token: Option<String>,
}

/// User name presented with [`Security::token`].
pub const AUTH_USER: &str = "ruro";

impl Security {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    fn apply(&self, c: &mut zenoh::Config, router: bool) -> Result<()> {
        let path = |p: &std::path::Path| -> Result<String> {
            let p = std::fs::canonicalize(p).with_context(|| format!("{}", p.display()))?;
            Ok(serde_json::to_string(&p.to_string_lossy())?)
        };
        if let Some(ca) = &self.ca {
            set(c, "transport/link/tls/root_ca_certificate", &path(ca)?)?;
        }
        match (&self.cert, &self.key) {
            (Some(cert), Some(key)) => {
                let (cert, key) = (path(cert)?, path(key)?);
                // A daemon presents its certificate to clients, and to the
                // peers it connects to; a client presents it for mutual TLS.
                if router {
                    set(c, "transport/link/tls/listen_certificate", &cert)?;
                    set(c, "transport/link/tls/listen_private_key", &key)?;
                }
                set(c, "transport/link/tls/connect_certificate", &cert)?;
                set(c, "transport/link/tls/connect_private_key", &key)?;
            }
            (None, None) => {}
            _ => anyhow::bail!("a TLS certificate needs its private key and vice versa"),
        }
        // A daemon given a CA accepts only clients with a certificate it signed.
        let mtls = if router {
            self.ca.is_some() && self.cert.is_some()
        } else {
            self.cert.is_some()
        };
        if mtls {
            set(c, "transport/link/tls/enable_mtls", "true")?;
        }
        if let Some(token) = &self.token {
            set(c, "transport/auth/usrpwd/user", &format!("\"{AUTH_USER}\""))?;
            set(
                c,
                "transport/auth/usrpwd/password",
                &serde_json::to_string(token)?,
            )?;
        }
        Ok(())
    }
}

/// Zenoh configuration for a daemon: a router listening on `listen`.
pub fn router_config(listen: &str) -> Result<zenoh::Config> {
    router_config_with(&[listen.to_owned()], &[], &Security::default(), None)
}

/// Zenoh configuration for a daemon listening on several endpoints and
/// connecting to peer daemons. `dictionary` is the `user:password` file that
/// enforces [`Security::token`]; the caller writes it with private permissions.
pub fn router_config_with(
    listen: &[String],
    peers: &[String],
    security: &Security,
    dictionary: Option<&std::path::Path>,
) -> Result<zenoh::Config> {
    let mut c = zenoh::Config::default();
    set(&mut c, "mode", "\"router\"")?;
    set(&mut c, "listen/endpoints", &serde_json::to_string(listen)?)?;
    set(&mut c, "connect/endpoints", &serde_json::to_string(peers)?)?;
    set(&mut c, "scouting/multicast/enabled", "false")?;
    security.apply(&mut c, true)?;
    if let Some(d) = dictionary {
        set(
            &mut c,
            "transport/auth/usrpwd/dictionary_file",
            &serde_json::to_string(&d.to_string_lossy())?,
        )?;
    }
    Ok(c)
}

/// Zenoh configuration for a client of the daemon at `endpoint`.
/// Opening fails immediately if nothing is listening.
pub fn client_config(endpoint: &str) -> Result<zenoh::Config> {
    client_config_with(endpoint, &Security::default())
}

/// Like [`client_config`], with TLS and token authentication.
pub fn client_config_with(endpoint: &str, security: &Security) -> Result<zenoh::Config> {
    let mut c = zenoh::Config::default();
    set(&mut c, "mode", "\"client\"")?;
    set(&mut c, "connect/endpoints", &format!("[\"{endpoint}\"]"))?;
    set(&mut c, "scouting/multicast/enabled", "false")?;
    security.apply(&mut c, false)?;
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
    request_bytes(session, keys, op, payload.as_bytes().to_vec(), timeout)
}

/// Like [`request`], with a binary payload.
pub fn request_bytes(
    session: &zenoh::Session,
    keys: &Keys,
    op: Ctl,
    payload: Vec<u8>,
    timeout: std::time::Duration,
) -> Result<String> {
    request_key(session, &keys.ctl(op), payload, timeout).map_err(|e| {
        if e.to_string().starts_with("no reply") {
            anyhow!("no reply from daemon `{}` (is it running?)", keys.daemon())
        } else {
            e
        }
    })
}

/// Sends a request to any queryable that answers with a [`CtlReply`]
/// (daemons and hosts) and waits for the reply.
pub fn request_key(
    session: &zenoh::Session,
    key: &str,
    payload: Vec<u8>,
    timeout: std::time::Duration,
) -> Result<String> {
    use zenoh::Wait;
    let replies = session
        .get(key)
        .payload(payload)
        .timeout(timeout)
        .wait()
        .map_err(|e| anyhow!("request failed: {e}"))?;
    let reply = replies.recv().map_err(|_| anyhow!("no reply from {key}"))?;
    let sample = reply
        .result()
        .map_err(|e| anyhow!("daemon error: {:?}", e.payload().to_bytes()))?;
    let body: CtlReply =
        serde_json::from_slice(&sample.payload().to_bytes()).context("malformed reply")?;
    body.map_err(|e| anyhow!(e))
}

/// Opens a client session to the daemon at `endpoint`.
pub fn connect(endpoint: &str) -> Result<zenoh::Session> {
    connect_with(endpoint, &Security::default())
}

/// Opens a client session with TLS and token authentication.
pub fn connect_with(endpoint: &str, security: &Security) -> Result<zenoh::Session> {
    use zenoh::Wait;
    zenoh::open(client_config_with(endpoint, security)?)
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
                timing: Some(TimingMetrics {
                    jitter_p99_us: 12.0,
                    deadline_misses: 2,
                    ..Default::default()
                }),
            }],
        };
        assert_eq!(decode::<GroupMetrics>(&encode(&m)).unwrap(), m);
    }

    #[test]
    fn data_frames_roundtrip_and_compress() {
        let small = DataFrame::encode(3, 9, b"abc", true);
        assert_eq!(small.len(), 17 + 3, "small payloads are sent as is");
        assert_eq!(
            DataFrame::decode(&small).unwrap(),
            DataFrame {
                seq: 3,
                stamp_ns: 9,
                body: b"abc".to_vec()
            }
        );
        let image = vec![7u8; 640 * 480];
        let packed = DataFrame::encode(1, 2, &image, true);
        assert!(packed.len() < image.len() / 10);
        assert_eq!(DataFrame::decode(&packed).unwrap().body, image);
        let plain = DataFrame::encode(1, 2, &image, false);
        assert_eq!(plain.len(), 17 + image.len());
        assert!(DataFrame::decode(&packed[..10]).is_err());
        let mut bad = packed.clone();
        bad[0] = 9;
        assert!(DataFrame::decode(&bad).is_err());
    }

    #[test]
    fn host_commands_are_json() {
        let c = HostCommand::Mute {
            nodes: vec!["src".into()],
        };
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, r#"{"cmd":"mute","nodes":["src"]}"#);
        assert_eq!(serde_json::from_str::<HostCommand>(&json).unwrap(), c);
    }

    #[test]
    fn configs_are_valid() {
        router_config("tcp/127.0.0.1:0").unwrap();
        client_config(DEFAULT_ENDPOINT).unwrap();
    }
}
