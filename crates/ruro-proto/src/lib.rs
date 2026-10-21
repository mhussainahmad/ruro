//! Control-plane protocol shared by the daemon, hosts, CLI and studio.
//!
//! Everything travels over Zenoh under `ruro/<daemon>/…`. The daemon runs a
//! Zenoh router; every other process connects to it as a client, so a studio
//! on a laptop can drive a daemon on a robot by pointing at its address.
//!
//! High-rate data (metrics, taps, logs) is postcard-encoded. Control requests
//! carry text (YAML graphs, JSON values) so they stay debuggable.

use anyhow::{Context, Result, anyhow, bail};
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
    /// Reply: JSON [`ruro_graph::Registry`] of the daemon's default host, or
    /// with a host name as payload, of that host (e.g. an uploaded one).
    Registry,
    /// Reply: YAML of the running graph (empty if none).
    Graph,
    /// Payload: group name. Kills the group process (for testing supervision).
    Kill,
    /// Reply: the daemon name. Used for discovery.
    Ping,
    /// Payload: path of an MCAP file to record every wire of the running
    /// graph into, or empty to stop recording. Groups on peers are recorded
    /// too: the daemon asks the peers it forwarded groups to (payload
    /// `from <daemon>`) and collects their part when the recording stops
    /// (`stop-from <daemon>`, reply: JSON [`RecordedPart`]).
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
    /// Payload: graph YAML. Reply: JSON list of the reasons the daemon would
    /// refuse to deploy it (empty if none), checked against the hosts each
    /// group would run on, here and on peers.
    Validate,
    /// Payload: JSON [`PartRequest`]. Reply (postcard, not a [`CtlReply`]):
    /// `Result<Vec<u8>, String>`, a chunk of a peer's part of a recording;
    /// empty at the end. Fetch with [`request_raw`].
    RecordPart,
    /// Payload: JSON [`ParamRequest`]. Checks the value against the node's
    /// descriptor, has the node's host apply it and replies once the node
    /// has accepted it. Forwarded to the machine that runs the node.
    Param,
    /// Reply: the daemon's clock, in nanoseconds since the Unix epoch. A
    /// daemon recording peers' groups estimates their clock offsets with it.
    Clock,
    /// Payload: postcard [`UploadChunk`] named after an [`UpdateRole`]
    /// (`daemon` or `host`). Stages a binary for [`Ctl::Update`] next to
    /// the one it replaces. Refused by daemons that trust no update keys.
    UpdateUpload,
    /// Payload: JSON [`UpdateRequest`]. Checks signed binaries against the
    /// daemon's trusted keys, version, target and state; unless only
    /// checking, tries the staged daemon binary, installs the binaries
    /// (keeping the previous ones) and restarts the daemon.
    Update,
}

impl Ctl {
    /// Every operation.
    pub const ALL: [Ctl; 17] = [
        Ctl::Deploy,
        Ctl::Stop,
        Ctl::Registry,
        Ctl::Graph,
        Ctl::Kill,
        Ctl::Ping,
        Ctl::Record,
        Ctl::Playback,
        Ctl::Estop,
        Ctl::DeployGroups,
        Ctl::Upload,
        Ctl::Validate,
        Ctl::Param,
        Ctl::RecordPart,
        Ctl::Clock,
        Ctl::UpdateUpload,
        Ctl::Update,
    ];

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
            Ctl::Validate => "validate",
            Ctl::Param => "param",
            Ctl::RecordPart => "record-part",
            Ctl::Clock => "clock",
            Ctl::UpdateUpload => "update-upload",
            Ctl::Update => "update",
        }
    }
}

/// What a binary sent with an update replaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateRole {
    /// The daemon's own executable (`ruro`).
    Daemon,
    /// The daemon's default host binary (`ruro-host`).
    Host,
}

impl UpdateRole {
    pub fn as_str(self) -> &'static str {
        match self {
            UpdateRole::Daemon => "daemon",
            UpdateRole::Host => "host",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "daemon" => Some(UpdateRole::Daemon),
            "host" => Some(UpdateRole::Host),
            _ => None,
        }
    }
}

/// A binary's signature, as written by `ruro sign` next to it
/// (`<binary>.sig`, JSON). The Ed25519 signature covers every other field
/// (see `ruro_daemon::sign`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedBinary {
    pub role: UpdateRole,
    /// Version the binary reports, e.g. `0.2.0`.
    pub version: String,
    /// Target triple it was built for.
    pub target: String,
    /// SHA-256 of the binary, lowercase hex.
    pub sha256: String,
    /// Public key that made the signature, base64.
    pub key: String,
    /// The signature, base64.
    pub signature: String,
}

/// Payload of [`Ctl::Update`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateRequest {
    /// At most one binary per role, uploaded first with
    /// [`Ctl::UpdateUpload`] unless only checking.
    pub binaries: Vec<SignedBinary>,
    /// Accept a daemon binary that is not newer than the running one.
    pub allow_downgrade: bool,
    /// Only check the signatures and whether the daemon would accept them.
    pub check: bool,
    /// How long the new daemon binary gets to start on trial.
    pub timeout_s: f64,
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

/// Published by the daemon twice a second. Decode it with
/// [`decode_status`], which also reads the status of older daemons.
///
/// New fields go at the end: postcard is not self-describing, and daemons
/// of different versions share a network while they are being updated.
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
    /// Target triple the daemon was built for, e.g.
    /// `aarch64-unknown-linux-gnu`. Empty for daemons that predate it.
    pub target: String,
    /// Outcome of the last update of this daemon, e.g. `updated from 0.1.0`
    /// or why an update was rolled back.
    pub update: Option<String>,
}

/// The status as published by daemons before `target` and `update`.
#[derive(Deserialize)]
struct StatusV1 {
    name: String,
    version: String,
    pid: u32,
    uptime_s: f64,
    graph: Option<String>,
    groups: Vec<GroupStatus>,
    recording: Option<String>,
    playback: Option<PlaybackStatus>,
    estopped: bool,
}

/// Decodes a [`DaemonStatus`], including one published by an older daemon.
/// (Older readers ignore the fields added since, which postcard appends.)
pub fn decode_status(bytes: &[u8]) -> Result<DaemonStatus> {
    decode(bytes).or_else(|e| {
        let Ok(s) = decode::<StatusV1>(bytes) else {
            return Err(e);
        };
        Ok(DaemonStatus {
            name: s.name,
            version: s.version,
            pid: s.pid,
            uptime_s: s.uptime_s,
            graph: s.graph,
            groups: s.groups,
            recording: s.recording,
            playback: s.playback,
            estopped: s.estopped,
            target: String::new(),
            update: None,
        })
    })
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
    /// Sent by the daemon replaying a recording to the peers that run some
    /// of the replayed nodes: they stop stepping (empty: resume).
    Mute {
        nodes: Vec<String>,
    },
}

/// A peer's part of a recording, waiting to be fetched with
/// [`Ctl::RecordPart`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordedPart {
    /// The file on the peer, if anything was recorded.
    pub file: Option<String>,
    pub size: u64,
    /// Groups recorded on the peer.
    pub groups: Vec<String>,
}

/// Payload of [`Ctl::RecordPart`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PartRequest {
    pub file: String,
    pub offset: u64,
}

/// Largest chunk of a recording sent in one [`Ctl::RecordPart`] reply.
pub const PART_CHUNK: usize = 1 << 20;

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
    /// Activity over the last window: steps per second, or messages per
    /// second on the node's busiest output when that is higher (nodes that
    /// publish from callbacks, such as ROS 2 subscriber bridges, and nodes
    /// whose outputs are being replayed).
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
    /// Sets a parameter of a node and replies once the node has applied it
    /// (or refused it). `value` is JSON.
    SetParam {
        node: String,
        name: String,
        value: String,
    },
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
            f => bail!("unknown data frame flags {f:#x}"),
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

/// A parameter change requested with [`Ctl::Param`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamRequest {
    pub node: String,
    pub name: String,
    /// The value as typed: JSON (`1.5`, `true`, `"text"`), or a bare string.
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
            _ => bail!("a TLS certificate needs its private key and vice versa"),
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
    let sample = reply.result().map_err(|e| {
        anyhow!(
            "daemon error: {}",
            String::from_utf8_lossy(&e.payload().to_bytes())
        )
    })?;
    let body: CtlReply =
        serde_json::from_slice(&sample.payload().to_bytes()).context("malformed reply")?;
    body.map_err(|e| anyhow!(e))
}

/// Opens a client session to the daemon at `endpoint`.
pub fn connect(endpoint: &str) -> Result<zenoh::Session> {
    connect_with(endpoint, &Security::default())
}

/// Opens a client session with TLS and token authentication. On failure
/// the error is a [`ConnectError`], which tells a daemon that is not
/// running from one that refused the session.
pub fn connect_with(endpoint: &str, security: &Security) -> Result<zenoh::Session> {
    use zenoh::Wait;
    let config = client_config_with(endpoint, security)?;
    zenoh::open(config).wait().map_err(|e| {
        let detail = e.to_string();
        anyhow::Error::new(ConnectError {
            endpoint: endpoint.to_owned(),
            failure: probe(endpoint),
            tls: endpoint.starts_with("tls/"),
            has_ca: security.ca.is_some(),
            has_token: security.token.is_some(),
            detail,
        })
    })
}

/// Why a client could not open a session with a daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectFailure {
    /// Nothing listens at the endpoint, or its address cannot be resolved
    /// or reached.
    NoDaemon,
    /// Something listens there but refused the session: a TLS certificate
    /// that is not trusted, a missing or wrong client certificate or token,
    /// or a service that is not a ruro daemon.
    Refused,
    /// The endpoint's protocol cannot be probed.
    Unknown,
}

/// A failure of [`connect_with`].
#[derive(Debug, Clone)]
pub struct ConnectError {
    pub endpoint: String,
    pub failure: ConnectFailure,
    tls: bool,
    has_ca: bool,
    has_token: bool,
    /// Zenoh's description of the failure.
    pub detail: String,
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let endpoint = &self.endpoint;
        match self.failure {
            ConnectFailure::NoDaemon => write!(
                f,
                "no ruro daemon is listening at {endpoint} (start one with `ruro daemon`)"
            ),
            ConnectFailure::Refused => {
                let hint = if self.tls && !self.has_ca {
                    "a tls/ endpoint needs --tls-ca with the CA that signed the daemon's certificate"
                        .to_owned()
                } else {
                    let mut checks = Vec::new();
                    if self.tls {
                        checks.push(
                            "that --tls-ca signed the daemon's certificate and the certificate \
                             names the host you connect to",
                        );
                        checks.push("the client certificate, if the daemon requires one");
                    } else {
                        checks.push("whether the daemon listens on tls/ rather than tcp/");
                    }
                    checks.push(if self.has_token {
                        "that the token is the daemon's"
                    } else {
                        "whether the daemon requires a token (--token-file or RURO_TOKEN)"
                    });
                    format!("check {}", checks.join(", "))
                };
                write!(
                    f,
                    "the daemon at {endpoint} refused the connection; {hint} ({})",
                    self.detail
                )
            }
            ConnectFailure::Unknown => {
                write!(f, "cannot connect to {endpoint} ({})", self.detail)
            }
        }
    }
}

impl std::error::Error for ConnectError {}

impl ConnectError {
    /// The failure behind an error returned by [`connect_with`], if it is one.
    pub fn failure_of(e: &anyhow::Error) -> Option<ConnectFailure> {
        e.downcast_ref::<ConnectError>().map(|c| c.failure)
    }
}

/// Checks whether anything accepts connections at a Zenoh endpoint.
fn probe(endpoint: &str) -> ConnectFailure {
    use std::net::ToSocketAddrs;
    let Some((proto, rest)) = endpoint.split_once('/') else {
        return ConnectFailure::Unknown;
    };
    // Endpoints may carry `?metadata` and `#config` after the address.
    let addr = rest.split(['?', '#']).next().unwrap_or_default();
    match proto {
        "tcp" | "tls" => {
            let Ok(addrs) = addr.to_socket_addrs() else {
                return ConnectFailure::NoDaemon;
            };
            let timeout = std::time::Duration::from_secs(1);
            if addrs
                .into_iter()
                .any(|a| std::net::TcpStream::connect_timeout(&a, timeout).is_ok())
            {
                ConnectFailure::Refused
            } else {
                ConnectFailure::NoDaemon
            }
        }
        "unixsock-stream" => match std::os::unix::net::UnixStream::connect(addr) {
            Ok(_) => ConnectFailure::Refused,
            Err(_) => ConnectFailure::NoDaemon,
        },
        _ => ConnectFailure::Unknown,
    }
}

/// Reads an access token from a file: its first line, without surrounding
/// whitespace. Unlike a token on the command line, it does not show up in
/// `ps`.
pub fn read_token_file(path: &std::path::Path) -> Result<String> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading the token file {}", path.display()))?;
    let token = text.lines().next().unwrap_or_default().trim();
    anyhow::ensure!(
        !token.is_empty(),
        "the token file {} is empty",
        path.display()
    );
    Ok(token.to_owned())
}

/// Sends a request whose reply is binary (postcard `Result<Vec<u8>,
/// String>`), such as [`Ctl::RecordPart`].
pub fn request_raw(
    session: &zenoh::Session,
    key: &str,
    payload: Vec<u8>,
    timeout: std::time::Duration,
) -> Result<Vec<u8>> {
    use zenoh::Wait;
    let replies = session
        .get(key)
        .payload(payload)
        .timeout(timeout)
        .wait()
        .map_err(|e| anyhow!("request failed: {e}"))?;
    let reply = replies.recv().map_err(|_| anyhow!("no reply from {key}"))?;
    let sample = reply.result().map_err(|e| {
        anyhow!(
            "daemon error: {}",
            String::from_utf8_lossy(&e.payload().to_bytes())
        )
    })?;
    let body: std::result::Result<Vec<u8>, String> = decode(&sample.payload().to_bytes())?;
    body.map_err(|e| anyhow!(e))
}

/// Finds the daemon behind a session. With `name` set, only checks that it answers.
///
/// Daemons that peer with each other all answer a ping, so without a name
/// the ping carries the id of the router the session is connected to, and
/// only the daemon that is that router answers: the one at the endpoint
/// the client connected to. If none does (a plain Zenoh router in between,
/// or daemons of an older version), any daemon that answers is taken, as
/// long as only one does.
pub fn discover(
    session: &zenoh::Session,
    name: Option<&str>,
    timeout: std::time::Duration,
) -> Result<Keys> {
    use zenoh::Wait;
    let ping = |key: String, payload: String| -> Result<Vec<String>> {
        let replies = session
            .get(key)
            .payload(payload)
            .timeout(timeout)
            .wait()
            .map_err(|e| anyhow!("discovery failed: {e}"))?;
        let mut names = Vec::new();
        while let Ok(reply) = replies.recv() {
            let Ok(sample) = reply.result() else { continue };
            if let Ok(Ok(name)) = serde_json::from_slice::<CtlReply>(&sample.payload().to_bytes()) {
                // Answering a named ping is all a named daemon must do.
                names.push(name);
                break;
            }
        }
        Ok(names)
    };
    if let Some(n) = name {
        return match ping(format!("ruro/{n}/ctl/ping"), String::new())?.pop() {
            Some(name) => Ok(Keys::new(&name)),
            None => bail!("daemon `{n}` did not answer"),
        };
    }
    let routers: Vec<String> = session
        .info()
        .routers_zid()
        .wait()
        .map(|z| z.to_string())
        .collect();
    for router in &routers {
        if let Some(name) = ping("ruro/*/ctl/ping".into(), format!("{PING_ZID}{router}"))?.pop() {
            return Ok(Keys::new(&name));
        }
    }
    let replies = session
        .get("ruro/*/ctl/ping")
        .timeout(timeout)
        .wait()
        .map_err(|e| anyhow!("discovery failed: {e}"))?;
    let mut names = std::collections::BTreeSet::new();
    while let Ok(reply) = replies.recv() {
        if let Ok(sample) = reply.result()
            && let Ok(Ok(name)) = serde_json::from_slice::<CtlReply>(&sample.payload().to_bytes())
        {
            names.insert(name);
        }
    }
    match names.len() {
        0 => bail!("no daemon answered"),
        1 => Ok(Keys::new(names.first().expect("one name"))),
        _ => {
            let all: Vec<String> = names.iter().map(|n| format!("`{n}`")).collect();
            bail!(
                "several daemons answer ({}); choose one with --daemon",
                all.join(", ")
            )
        }
    }
}

/// A ping payload `zid <id>`: only the daemon whose session has that Zenoh
/// id answers.
pub const PING_ZID: &str = "zid ";

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

    /// The status layout of daemons before `target` and `update`.
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct OldStatus {
        name: String,
        version: String,
        pid: u32,
        uptime_s: f64,
        graph: Option<String>,
        groups: Vec<GroupStatus>,
        recording: Option<String>,
        playback: Option<PlaybackStatus>,
        estopped: bool,
    }

    #[test]
    fn statuses_of_old_and_new_daemons_decode_both_ways() {
        let old = OldStatus {
            name: "robot".into(),
            version: "0.1.0".into(),
            pid: 7,
            uptime_s: 1.5,
            graph: Some("g".into()),
            groups: Vec::new(),
            recording: None,
            playback: None,
            estopped: true,
        };
        let new = decode_status(&encode(&old)).unwrap();
        assert_eq!(
            (new.name.as_str(), new.pid, new.estopped),
            ("robot", 7, true)
        );
        assert_eq!((new.target.as_str(), new.update), ("", None));

        let current = DaemonStatus {
            target: "x86_64-unknown-linux-gnu".into(),
            update: Some("updated from 0.0.9".into()),
            version: "0.2.0".into(),
            ..new
        };
        assert_eq!(decode_status(&encode(&current)).unwrap(), current);
        // An old reader ignores the new fields.
        let seen: OldStatus = decode(&encode(&current)).unwrap();
        assert_eq!(seen.version, "0.2.0");
        assert!(seen.estopped);
    }

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
    fn a_missing_daemon_is_told_apart_from_a_refused_session() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let endpoint = format!("tcp/127.0.0.1:{port}");
        assert_eq!(probe(&endpoint), ConnectFailure::NoDaemon);
        let err = connect(&endpoint).expect_err("nothing listens");
        assert_eq!(
            ConnectError::failure_of(&err),
            Some(ConnectFailure::NoDaemon)
        );
        assert!(format!("{err:#}").contains("ruro daemon"), "{err:#}");

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        assert_eq!(probe(&format!("tls/{addr}")), ConnectFailure::Refused);
        assert_eq!(
            probe("tcp/no-such-host.invalid:7447"),
            ConnectFailure::NoDaemon
        );
        assert_eq!(probe("udp/127.0.0.1:1"), ConnectFailure::Unknown);
        let refused = ConnectError {
            endpoint: format!("tls/{addr}"),
            failure: ConnectFailure::Refused,
            tls: true,
            has_ca: false,
            has_token: false,
            detail: String::new(),
        };
        assert!(refused.to_string().contains("--tls-ca"), "{refused}");
    }

    #[test]
    fn token_files_hold_the_first_line() {
        let path = std::env::temp_dir().join(format!("ruro-token-{}", std::process::id()));
        std::fs::write(&path, "  s3cret \nsecond line\n").unwrap();
        assert_eq!(read_token_file(&path).unwrap(), "s3cret");
        std::fs::write(&path, "\n").unwrap();
        assert!(read_token_file(&path).is_err());
        let _ = std::fs::remove_file(&path);
        assert!(read_token_file(&path).is_err());
    }

    #[test]
    fn configs_are_valid() {
        router_config("tcp/127.0.0.1:0").unwrap();
        client_config(DEFAULT_ENDPOINT).unwrap();
    }
}
