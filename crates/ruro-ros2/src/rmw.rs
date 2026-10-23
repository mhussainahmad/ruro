//! ROS 2 topics and services over Zenoh, following rmw_zenoh's conventions
//! (rmw_zenoh_cpp 0.2.11, the version ROS 2 Jazzy ships; see its
//! `docs/design.md`).
//!
//! What a ROS 2 node using `rmw_zenoh_cpp` expects from a peer:
//!
//! - **Data key**: `<domain>/<name without outer slashes>/<DDS type>/<hash>`,
//!   e.g. `0/chatter/std_msgs::msg::dds_::String_/RIHS01_df66…`, for topics
//!   and services alike. A service's DDS type and hash are those of the
//!   service type (`example_interfaces::srv::dds_::AddTwoInts_`).
//! - **Payload**: the CDR encoding with its 4-byte encapsulation header.
//! - **Attachment** on every sample, request and reply (ones without it are
//!   dropped): sequence number (`i64` LE), source timestamp (`i64` LE, ns
//!   since the epoch), then a 16-byte GID prefixed by its length (`0x10`).
//!   Publishers number samples from 1 and send their own GID. Clients number
//!   requests from 1 and send their GID; servers echo both in the reply,
//!   with a new timestamp.
//! - **Topics** are Zenoh (advanced) publishers and subscribers. Durability
//!   transient-local uses the advanced publisher's cache and the advanced
//!   subscriber's history queries, so latched topics such as `/tf_static`
//!   reach late joiners in both directions.
//! - **Services** are Zenoh queryables, declared complete; a call is a
//!   `get` with target all-complete and no consolidation.
//! - **Liveliness tokens** announce nodes and endpoints for graph discovery
//!   (`ros2 topic list`, `ros2 service list`, type lookup in `ros2 topic echo`):
//!   `@ros2_lv/<domain>/<zid>/<node id>/<entity id>/<kind>/<enclave>/<namespace>/<node>[/<name>/<type>/<hash>/<qos>]`
//!   with `/` inside names replaced by `%`; kinds `NN` (node), `MP`/`MS`
//!   (publisher, subscriber), `SS`/`SC` (service server, client).
//! - **GID**: XXH3-128 of the endpoint's liveliness key, low half first.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use zenoh::Wait;
use zenoh::liveliness::LivelinessToken;
use zenoh::qos::CongestionControl;
use zenoh::query::{ConsolidationMode, QueryTarget};
use zenoh_ext::{
    AdvancedPublisher, AdvancedPublisherBuilderExt, AdvancedSubscriber,
    AdvancedSubscriberBuilderExt, CacheConfig, HistoryConfig, MissDetectionConfig, RecoveryConfig,
};

use crate::cdr::{self, CdrError};
use crate::{RosMessage, RosService};

/// Default endpoint of a Zenoh router (rmw_zenohd, or a ruro daemon).
pub const DEFAULT_ROUTER: &str = "tcp/127.0.0.1:7447";

/// Liveliness key prefix.
pub const ADMIN_SPACE: &str = "@ros2_lv";

/// Delivery guarantee.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Reliability {
    #[default]
    Reliable,
    BestEffort,
}

impl std::str::FromStr for Reliability {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().replace('-', "_").as_str() {
            "reliable" => Ok(Self::Reliable),
            "best_effort" | "besteffort" | "sensor_data" => Ok(Self::BestEffort),
            other => Err(anyhow!(
                "unknown QoS `{other}` (expected reliable or best_effort)"
            )),
        }
    }
}

/// Whether late-joining subscribers get past samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Durability {
    /// Only samples published while a subscriber exists reach it.
    #[default]
    Volatile,
    /// The publisher keeps its last `depth` samples and late subscribers
    /// that ask for them (transient-local themselves) receive them, as
    /// `/tf_static` needs.
    TransientLocal,
}

impl std::str::FromStr for Durability {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().replace('-', "_").as_str() {
            "volatile" => Ok(Self::Volatile),
            "transient_local" | "transientlocal" | "latched" => Ok(Self::TransientLocal),
            other => Err(anyhow!(
                "unknown durability `{other}` (expected volatile or transient_local)"
            )),
        }
    }
}

/// How many samples are kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum History {
    /// The last `depth` samples.
    #[default]
    KeepLast,
    /// All of them; a reliable publisher blocks rather than drop samples
    /// when the network is congested.
    KeepAll,
}

/// How an endpoint's liveliness is asserted. rmw_zenoh only implements
/// automatic liveliness (through its tokens); other values are announced
/// but have no effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Liveliness {
    #[default]
    Automatic,
    ManualByTopic,
}

/// The QoS settings of an endpoint, as ROS 2 defines them.
///
/// On the wire they work as in rmw_zenoh: reliability picks Zenoh's
/// reliability, history keep-all with reliable delivery blocks a congested
/// publisher instead of dropping, transient-local durability turns on
/// Zenoh's advanced publisher cache (`depth` samples) and the advanced
/// subscriber's history queries. Deadline, lifespan and liveliness are
/// only announced in liveliness tokens (rmw_zenoh does not enforce them
/// either). Subscribers deliver every sample to their callback as it
/// arrives, so `depth` does not limit a subscriber's queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Qos {
    pub reliability: Reliability,
    /// Keep-last history depth; 0 means rmw_zenoh's default of 42.
    pub depth: usize,
    pub durability: Durability,
    pub history: History,
    /// `None` is infinite (the default).
    pub deadline: Option<Duration>,
    /// `None` is infinite (the default).
    pub lifespan: Option<Duration>,
    pub liveliness: Liveliness,
    /// `None` is infinite (the default).
    pub liveliness_lease: Option<Duration>,
}

impl Default for Qos {
    /// ROS 2's default profile: reliable, volatile, keep-last 10.
    fn default() -> Self {
        Self {
            reliability: Reliability::Reliable,
            depth: 10,
            durability: Durability::Volatile,
            history: History::KeepLast,
            deadline: None,
            lifespan: None,
            liveliness: Liveliness::Automatic,
            liveliness_lease: None,
        }
    }
}

/// `RMW_DURATION_INFINITE`, as the `(sec, nsec)` pair rmw_zenoh compares
/// each component of a duration with.
const INFINITE: (u64, u64) = (9_223_372_036, 854_775_807);

/// rmw_zenoh's default depth when a profile asks for 0.
const DEFAULT_DEPTH: usize = 42;

fn duration_to_keyexpr(d: Option<Duration>) -> String {
    let (s, ns) = d.map_or(INFINITE, |d| (d.as_secs(), u64::from(d.subsec_nanos())));
    let part = |v: u64, inf: u64| {
        if v == inf {
            String::new()
        } else {
            v.to_string()
        }
    };
    format!("{},{}", part(s, INFINITE.0), part(ns, INFINITE.1))
}

fn duration_from_keyexpr(s: &str) -> Option<Option<Duration>> {
    let (sec, nsec) = s.split_once(',')?;
    let num = |v: &str, inf: u64| {
        if v.is_empty() {
            Some(inf)
        } else {
            v.parse().ok()
        }
    };
    let (sec, nsec) = (num(sec, INFINITE.0)?, num(nsec, INFINITE.1)?);
    Some(if (sec, nsec) == INFINITE {
        None
    } else {
        Some(Duration::from_secs(sec).checked_add(Duration::from_nanos(nsec))?)
    })
}

impl Qos {
    /// ROS 2's sensor data profile: best effort, keep-last 5.
    pub fn sensor_data() -> Self {
        Self {
            reliability: Reliability::BestEffort,
            depth: 5,
            ..Self::default()
        }
    }

    /// ROS 2's default profile for services: reliable, volatile, keep-last 10.
    pub fn services() -> Self {
        Self::default()
    }

    /// A latched topic such as `/tf_static`: reliable, transient-local,
    /// keep-last `depth`.
    pub fn transient_local(depth: usize) -> Self {
        Self {
            durability: Durability::TransientLocal,
            depth,
            ..Self::default()
        }
    }

    /// The depth rmw_zenoh uses: `depth`, or 42 if it is 0.
    pub fn effective_depth(&self) -> usize {
        if self.depth == 0 {
            DEFAULT_DEPTH
        } else {
            self.depth
        }
    }

    /// rmw_zenoh's QoS rendering for liveliness keys
    /// (`qos_to_keyexpr` in `liveliness_utils.cpp`):
    /// `<reliability>:<durability>:<history>,<depth>:<deadline s>,<ns>:<lifespan s>,<ns>:<liveliness>,<lease s>,<ns>`
    /// with the `rmw_qos_*_policy_e` numbers, and every value equal to
    /// rmw_zenoh's defaults (reliable, volatile, keep-last 42, infinite
    /// durations, automatic liveliness) left empty.
    pub fn to_keyexpr(&self) -> String {
        let reliability = match self.reliability {
            Reliability::Reliable => "",
            Reliability::BestEffort => "2",
        };
        let durability = match self.durability {
            Durability::Volatile => "",
            Durability::TransientLocal => "1",
        };
        let history = match self.history {
            History::KeepLast => "",
            History::KeepAll => "2",
        };
        let depth = match self.effective_depth() {
            DEFAULT_DEPTH => String::new(),
            d => d.to_string(),
        };
        let liveliness = match self.liveliness {
            Liveliness::Automatic => "",
            Liveliness::ManualByTopic => "3",
        };
        format!(
            "{reliability}:{durability}:{history},{depth}:{}:{}:{liveliness},{}",
            duration_to_keyexpr(self.deadline),
            duration_to_keyexpr(self.lifespan),
            duration_to_keyexpr(self.liveliness_lease),
        )
    }

    /// Parses [`to_keyexpr`](Self::to_keyexpr) output, as rmw_zenoh's
    /// `keyexpr_to_qos` does.
    pub fn from_keyexpr(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() != 6 {
            return None;
        }
        let reliability = match parts[0] {
            "" | "0" | "1" => Reliability::Reliable,
            "2" => Reliability::BestEffort,
            _ => return None,
        };
        let durability = match parts[1] {
            "" | "0" | "2" => Durability::Volatile,
            "1" => Durability::TransientLocal,
            _ => return None,
        };
        let (history, depth) = parts[2].split_once(',')?;
        let history = match history {
            "" | "0" | "1" => History::KeepLast,
            "2" => History::KeepAll,
            _ => return None,
        };
        let depth = if depth.is_empty() {
            DEFAULT_DEPTH
        } else {
            depth.parse().ok()?
        };
        let (liveliness, lease) = parts[5].split_once(',')?;
        let liveliness = match liveliness {
            "" | "0" | "1" => Liveliness::Automatic,
            "3" => Liveliness::ManualByTopic,
            _ => return None,
        };
        Some(Self {
            reliability,
            depth,
            durability,
            history,
            deadline: duration_from_keyexpr(parts[3])?,
            lifespan: duration_from_keyexpr(parts[4])?,
            liveliness,
            liveliness_lease: duration_from_keyexpr(lease)?,
        })
    }

    fn congestion_control(&self) -> CongestionControl {
        if self.reliability == Reliability::Reliable && self.history == History::KeepAll {
            CongestionControl::Block
        } else {
            CongestionControl::Drop
        }
    }

    fn zenoh_reliability(&self) -> zenoh::qos::Reliability {
        match self.reliability {
            Reliability::Reliable => zenoh::qos::Reliability::Reliable,
            Reliability::BestEffort => zenoh::qos::Reliability::BestEffort,
        }
    }
}

/// `/` → `%`, as in liveliness keys.
pub fn mangle(name: &str) -> String {
    name.replace('/', "%")
}

/// `%` → `/`.
pub fn unmangle(name: &str) -> String {
    name.replace('%', "/")
}

/// Removes at most one leading and one trailing `/`.
pub fn strip_slashes(s: &str) -> &str {
    let s = s.strip_prefix('/').unwrap_or(s);
    s.strip_suffix('/').unwrap_or(s)
}

/// A fully qualified topic name: relative names are resolved in `namespace`.
pub fn qualify(topic: &str, namespace: &str) -> String {
    if topic.starts_with('/') {
        topic.to_owned()
    } else {
        let ns = namespace.trim_end_matches('/');
        format!("{ns}/{topic}")
    }
}

/// The data key expression of a topic.
pub fn topic_key(domain: u32, topic: &str, dds_type: &str, hash: &str) -> String {
    format!("{domain}/{}/{dds_type}/{hash}", strip_slashes(topic))
}

/// The publisher GID rmw_zenoh derives from a liveliness key.
pub fn gid(liveliness_key: &str) -> [u8; 16] {
    xxhash_rust::xxh3::xxh3_128(liveliness_key.as_bytes()).to_le_bytes()
}

/// Encodes a sample attachment.
pub fn attachment(seq: i64, timestamp_ns: i64, gid: &[u8; 16]) -> Vec<u8> {
    let mut a = Vec::with_capacity(33);
    a.extend_from_slice(&seq.to_le_bytes());
    a.extend_from_slice(&timestamp_ns.to_le_bytes());
    a.push(16);
    a.extend_from_slice(gid);
    a
}

/// What an attachment says about a sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleInfo {
    pub sequence: i64,
    pub source_timestamp_ns: i64,
    pub publisher_gid: [u8; 16],
}

/// Decodes a sample attachment.
pub fn parse_attachment(a: &[u8]) -> Option<SampleInfo> {
    if a.len() != 33 || a[16] != 16 {
        return None;
    }
    Some(SampleInfo {
        sequence: i64::from_le_bytes(a[0..8].try_into().ok()?),
        source_timestamp_ns: i64::from_le_bytes(a[8..16].try_into().ok()?),
        publisher_gid: a[17..33].try_into().ok()?,
    })
}

/// How to reach a Zenoh network and how to appear in the ROS graph.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EndpointConfig {
    /// Router or peer endpoints to connect to.
    pub connect: Vec<String>,
    /// `client` (through a router) or `peer`.
    pub mode: String,
    /// `ROS_DOMAIN_ID`.
    pub domain: u32,
    pub node_name: String,
    /// Node namespace, `/` for the root.
    pub namespace: String,
    /// Token and TLS settings for a router that requires them.
    pub credentials: Credentials,
}

/// Credentials for a Zenoh router that requires them, such as a ruro daemon
/// started with a token or listening on TLS.
#[derive(Clone, Default, PartialEq, Eq, Hash)]
pub struct Credentials {
    /// Token for Zenoh's user/password authentication, presented as the
    /// user `ruro`, as ruro daemons expect. Routers that do not
    /// authenticate ignore it.
    pub token: Option<String>,
    /// CA certificate (PEM) that signed the router's certificate, for
    /// `tls/` endpoints.
    pub tls_ca: Option<std::path::PathBuf>,
    /// Client certificate and its key (PEM), for routers that require
    /// mutual TLS.
    pub tls_cert: Option<std::path::PathBuf>,
    pub tls_key: Option<std::path::PathBuf>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("token", &self.token.as_ref().map(|_| "<hidden>"))
            .field("tls_ca", &self.tls_ca)
            .field("tls_cert", &self.tls_cert)
            .field("tls_key", &self.tls_key)
            .finish()
    }
}

/// User name ruro daemons expect with a token.
const TOKEN_USER: &str = "ruro";

impl Credentials {
    /// The credentials ruro processes share through the environment:
    /// `RURO_TOKEN` (or the first line of the file `RURO_TOKEN_FILE`),
    /// `RURO_TLS_CA`, `RURO_TLS_CERT` and `RURO_TLS_KEY`. Daemons give
    /// their hosts the token this way.
    pub fn from_env() -> Result<Self> {
        let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
        let token = match var("RURO_TOKEN_FILE") {
            Some(path) => {
                let text = std::fs::read_to_string(&path).with_context(|| {
                    format!("reading the token file {}", path.to_string_lossy())
                })?;
                Some(text.lines().next().unwrap_or_default().trim().to_owned())
            }
            None => var("RURO_TOKEN").map(|t| t.to_string_lossy().into_owned()),
        };
        Ok(Self {
            token: token.filter(|t| !t.is_empty()),
            tls_ca: var("RURO_TLS_CA").map(Into::into),
            tls_cert: var("RURO_TLS_CERT").map(Into::into),
            tls_key: var("RURO_TLS_KEY").map(Into::into),
        })
    }

    fn apply(&self, c: &mut zenoh::Config) -> Result<()> {
        let set = |c: &mut zenoh::Config, k: &str, v: String| {
            c.insert_json5(k, &v)
                .map_err(|e| anyhow!("zenoh config `{k}`: {e}"))
        };
        let path = |p: &std::path::Path| -> Result<String> {
            let p = std::fs::canonicalize(p).with_context(|| p.display().to_string())?;
            Ok(serde_json::to_string(&p.to_string_lossy())?)
        };
        if let Some(ca) = &self.tls_ca {
            set(c, "transport/link/tls/root_ca_certificate", path(ca)?)?;
        }
        match (&self.tls_cert, &self.tls_key) {
            (Some(cert), Some(key)) => {
                set(c, "transport/link/tls/connect_certificate", path(cert)?)?;
                set(c, "transport/link/tls/connect_private_key", path(key)?)?;
                set(c, "transport/link/tls/enable_mtls", "true".into())?;
            }
            (None, None) => {}
            _ => anyhow::bail!("a TLS certificate needs its private key and vice versa"),
        }
        if let Some(token) = &self.token {
            set(c, "transport/auth/usrpwd/user", format!("{TOKEN_USER:?}"))?;
            set(
                c,
                "transport/auth/usrpwd/password",
                serde_json::to_string(token)?,
            )?;
        }
        Ok(())
    }
}

impl Default for EndpointConfig {
    fn default() -> Self {
        Self {
            connect: vec![DEFAULT_ROUTER.into()],
            mode: "client".into(),
            domain: std::env::var("ROS_DOMAIN_ID")
                .ok()
                .and_then(|d| d.parse().ok())
                .unwrap_or(0),
            node_name: "ruro".into(),
            namespace: "/".into(),
            credentials: Credentials::default(),
        }
    }
}

impl EndpointConfig {
    /// The Zenoh session configuration for these settings.
    pub fn zenoh_config(&self) -> Result<zenoh::Config> {
        let mut c = zenoh::Config::default();
        let set = |c: &mut zenoh::Config, k: &str, v: String| {
            c.insert_json5(k, &v)
                .map_err(|e| anyhow!("zenoh config `{k}`: {e}"))
        };
        set(&mut c, "mode", format!("{:?}", self.mode))?;
        set(
            &mut c,
            "connect/endpoints",
            serde_json::to_string(&self.connect)?,
        )?;
        set(&mut c, "scouting/multicast/enabled", "false".into())?;
        // As rmw_zenoh's default session configuration.
        set(
            &mut c,
            "timestamping/enabled",
            "{ router: true, peer: true, client: true }".into(),
        )?;
        self.credentials.apply(&mut c)?;
        Ok(c)
    }
}

/// Zenoh sessions shared by every endpoint of a process with the same
/// connection settings: endpoints, mode and credentials.
type SessionEntry = ((Vec<String>, String, Credentials), Weak<SharedSession>);

static SESSIONS: Mutex<Vec<SessionEntry>> = Mutex::new(Vec::new());

struct SharedSession {
    session: zenoh::Session,
    /// rmw_zenoh numbers nodes and entities per context from zero.
    next_id: AtomicU32,
}

fn shared_session(cfg: &EndpointConfig) -> Result<Arc<SharedSession>> {
    let mut sessions = SESSIONS.lock().unwrap();
    sessions.retain(|(_, w)| w.strong_count() > 0);
    let key = (
        cfg.connect.clone(),
        cfg.mode.clone(),
        cfg.credentials.clone(),
    );
    if let Some(s) = sessions
        .iter()
        .find(|(k, _)| *k == key)
        .and_then(|(_, w)| w.upgrade())
    {
        return Ok(s);
    }
    let session = zenoh::open(cfg.zenoh_config()?).wait().map_err(|e| {
        let hint = if cfg.credentials.token.is_none() {
            " (a router that requires a token needs one, e.g. from RURO_TOKEN)"
        } else {
            ""
        };
        anyhow!("cannot reach Zenoh at {:?}: {e}{hint}", cfg.connect)
    })?;
    let shared = Arc::new(SharedSession {
        session,
        next_id: AtomicU32::new(0),
    });
    sessions.push((key, Arc::downgrade(&shared)));
    Ok(shared)
}

/// A ROS 2 node: a Zenoh session plus the node's liveliness token. Create
/// publishers, subscribers, service servers and clients from it (and
/// action servers and clients with [`crate::action`]).
pub struct Endpoint {
    shared: Arc<SharedSession>,
    cfg: EndpointConfig,
    zid: String,
    node_id: u32,
    _token: LivelinessToken,
    /// `~/get_type_description`, which every ROS 2 node offers.
    _type_description: Option<RawService>,
}

impl Endpoint {
    /// Connects (sharing a session with other endpoints of this process that
    /// use the same settings) and announces the node.
    pub fn open(cfg: EndpointConfig) -> Result<Self> {
        let shared = shared_session(&cfg)?;
        let zid = shared.session.zid().to_string();
        let node_id = shared.next_id.fetch_add(1, Ordering::Relaxed);
        let key = format!(
            "{ADMIN_SPACE}/{}/{zid}/{node_id}/{node_id}/NN/%/{}/{}",
            cfg.domain,
            mangle(&cfg.namespace),
            mangle(&cfg.node_name)
        );
        let token = shared
            .session
            .liveliness()
            .declare_token(key)
            .wait()
            .map_err(|e| anyhow!("declaring node token: {e}"))?;
        let mut endpoint = Self {
            shared,
            cfg,
            zid,
            node_id,
            _token: token,
            _type_description: None,
        };
        endpoint._type_description = Some(endpoint.type_description_service()?);
        Ok(endpoint)
    }

    /// Answers `~/get_type_description` (REP 2011) for the types the
    /// process knows: the bundled ones and those found on the search path.
    fn type_description_service(&self) -> Result<RawService> {
        use crate::msgs::type_description_interfaces::srv::{
            GetTypeDescription, GetTypeDescription_Response,
        };
        let ns = self.cfg.namespace.trim_end_matches('/');
        let name = format!("{ns}/{}/get_type_description", self.cfg.node_name);
        self.service::<GetTypeDescription>(&name, Qos::services(), |req| {
            let fail = |why: String| GetTypeDescription_Response {
                successful: false,
                failure_reason: why,
                ..Default::default()
            };
            let Some(name) = crate::idl::TypeName::parse(&req.type_name) else {
                return fail(format!("`{}` is not a type name", req.type_name));
            };
            let mut lib = crate::search::TypeLibrary::global()
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if let Err(e) = lib.load(&name) {
                return fail(format!("{e:#}"));
            }
            let interfaces = lib.interfaces();
            let hash = match crate::hash::type_hash(interfaces, &name) {
                Ok(h) => h,
                Err(missing) => return fail(format!("`{missing}` is not known here")),
            };
            if !req.type_hash.is_empty() && req.type_hash != hash {
                return fail(format!(
                    "`{}` has hash {hash} here, not {}",
                    req.type_name, req.type_hash
                ));
            }
            match crate::hash::type_description(interfaces, &name) {
                Ok(d) => GetTypeDescription_Response {
                    successful: true,
                    type_description: d,
                    ..Default::default()
                },
                Err(missing) => fail(format!("`{missing}` is not known here")),
            }
        })
    }

    /// The underlying Zenoh session.
    pub fn session(&self) -> &zenoh::Session {
        &self.shared.session
    }

    /// The ROS domain.
    pub fn domain(&self) -> u32 {
        self.cfg.domain
    }

    /// The settings this endpoint was opened with.
    pub fn config(&self) -> &EndpointConfig {
        &self.cfg
    }

    /// A fully qualified name for `name`, resolved in the node's namespace.
    pub fn qualify(&self, name: &str) -> String {
        qualify(name, &self.cfg.namespace)
    }

    fn entity_key(&self, kind: &str, topic: &str, dds: &str, hash: &str, qos: &Qos) -> String {
        let eid = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        format!(
            "{ADMIN_SPACE}/{}/{}/{}/{eid}/{kind}/%/{}/{}/{}/{dds}/{hash}/{}",
            self.cfg.domain,
            self.zid,
            self.node_id,
            mangle(&self.cfg.namespace),
            mangle(&self.cfg.node_name),
            mangle(topic),
            qos.to_keyexpr()
        )
    }

    fn token(&self, key: &str, what: &str) -> Result<LivelinessToken> {
        self.session()
            .liveliness()
            .declare_token(key.to_owned())
            .wait()
            .map_err(|e| anyhow!("declaring {what} token: {e}"))
    }

    /// A publisher of raw CDR payloads for any type.
    pub fn raw_publisher(
        &self,
        topic: &str,
        dds_type: &str,
        type_hash: &str,
        qos: Qos,
    ) -> Result<RawPublisher> {
        let topic = self.qualify(topic);
        let token_key = self.entity_key("MP", &topic, dds_type, type_hash, &qos);
        let key = topic_key(self.cfg.domain, &topic, dds_type, type_hash);
        // As rmw_zenoh: always an advanced publisher; transient-local adds
        // a cache that late subscribers query, and reliable transient-local
        // heartbeats so subscribers notice missed samples.
        let mut builder = self
            .session()
            .declare_publisher(key.clone())
            .congestion_control(qos.congestion_control())
            .reliability(qos.zenoh_reliability())
            .advanced();
        if qos.durability == Durability::TransientLocal {
            builder = builder
                .publisher_detection()
                .cache(CacheConfig::default().max_samples(qos.effective_depth()));
            if qos.reliability == Reliability::Reliable {
                builder = builder.sample_miss_detection(
                    MissDetectionConfig::default().sporadic_heartbeat(HEARTBEAT_PERIOD),
                );
            }
        }
        let publisher = builder
            .wait()
            .map_err(|e| anyhow!("declaring publisher on {key}: {e}"))?;
        let token = self.token(&token_key, "publisher")?;
        Ok(RawPublisher {
            publisher,
            gid: gid(&token_key),
            seq: AtomicI64::new(0),
            key,
            _token: token,
        })
    }

    /// A typed publisher.
    pub fn publisher<T: RosMessage>(&self, topic: &str, qos: Qos) -> Result<Publisher<T>> {
        Ok(Publisher {
            raw: self.raw_publisher(topic, T::DDS_TYPE_NAME, T::TYPE_HASH, qos)?,
            _type: std::marker::PhantomData,
        })
    }

    /// A subscriber delivering raw CDR payloads to `callback`.
    pub fn raw_subscriber(
        &self,
        topic: &str,
        dds_type: &str,
        type_hash: &str,
        qos: Qos,
        callback: impl Fn(&[u8], Option<SampleInfo>) + Send + Sync + 'static,
    ) -> Result<Subscriber> {
        let topic = self.qualify(topic);
        let token_key = self.entity_key("MS", &topic, dds_type, type_hash, &qos);
        let key = topic_key(self.cfg.domain, &topic, dds_type, type_hash);
        let mut builder = self
            .session()
            .declare_subscriber(key.clone())
            .advanced()
            .callback(move |sample| {
                let info = sample
                    .attachment()
                    .and_then(|a| parse_attachment(&a.to_bytes()));
                callback(&sample.payload().to_bytes(), info);
            });
        // As rmw_zenoh: a transient-local subscriber asks publishers that
        // keep a cache (including ones that appear later) for their history.
        if qos.durability == Durability::TransientLocal {
            builder = builder
                .subscriber_detection()
                .query_timeout(FOREVER)
                .history(
                    HistoryConfig::default()
                        .detect_late_publishers()
                        .max_samples(qos.effective_depth()),
                );
            if qos.reliability == Reliability::Reliable {
                builder = builder.recovery(RecoveryConfig::default().heartbeat());
            }
        }
        let subscriber = builder
            .wait()
            .map_err(|e| anyhow!("declaring subscriber on {key}: {e}"))?;
        let token = self.token(&token_key, "subscriber")?;
        Ok(Subscriber {
            _subscriber: subscriber,
            _token: token,
            key,
        })
    }

    /// A typed subscriber. Payloads that fail to decode are passed to
    /// `callback` as errors.
    pub fn subscriber<T: RosMessage>(
        &self,
        topic: &str,
        qos: Qos,
        callback: impl Fn(Result<T, CdrError>, Option<SampleInfo>) + Send + Sync + 'static,
    ) -> Result<Subscriber> {
        self.raw_subscriber(
            topic,
            T::DDS_TYPE_NAME,
            T::TYPE_HASH,
            qos,
            move |b, info| callback(cdr::from_slice::<T>(b), info),
        )
    }

    /// A service server for raw CDR payloads of any type. `callback` gets
    /// every request; reply with [`ServiceRequest::reply`], from the
    /// callback or later from any thread. Requests without a valid
    /// rmw_zenoh attachment are ignored, as rmw_zenoh ignores them.
    pub fn raw_service(
        &self,
        name: &str,
        dds_type: &str,
        type_hash: &str,
        qos: Qos,
        callback: impl Fn(ServiceRequest) + Send + Sync + 'static,
    ) -> Result<RawService> {
        let name = self.qualify(name);
        let token_key = self.entity_key("SS", &name, dds_type, type_hash, &qos);
        let key = topic_key(self.cfg.domain, &name, dds_type, type_hash);
        let reply_key = key.clone();
        // Complete: rmw_zenoh clients only query complete queryables.
        let queryable = self
            .session()
            .declare_queryable(key.clone())
            .complete(true)
            .callback(move |query| {
                let Some(info) = query
                    .attachment()
                    .and_then(|a| parse_attachment(&a.to_bytes()))
                else {
                    return;
                };
                let payload = query
                    .payload()
                    .map(|p| p.to_bytes().into_owned())
                    .unwrap_or_default();
                callback(ServiceRequest {
                    query,
                    payload,
                    info,
                    key: reply_key.clone(),
                });
            })
            .wait()
            .map_err(|e| anyhow!("declaring service on {key}: {e}"))?;
        let token = self.token(&token_key, "service")?;
        Ok(RawService {
            _queryable: queryable,
            _token: token,
            key,
        })
    }

    /// A service server answering each request with `handler`, called on a
    /// Zenoh thread: keep it short, and use [`service_deferred`] for
    /// handlers that block or wait. Requests that fail to decode are not
    /// answered.
    ///
    /// [`service_deferred`]: Self::service_deferred
    pub fn service<S: RosService>(
        &self,
        name: &str,
        qos: Qos,
        handler: impl Fn(S::Request) -> S::Response + Send + Sync + 'static,
    ) -> Result<RawService> {
        self.raw_service(name, S::DDS_TYPE_NAME, S::TYPE_HASH, qos, move |req| {
            if let Ok(r) = cdr::from_slice::<S::Request>(req.payload()) {
                let _ = req.reply(cdr::to_vec(&handler(r)));
            }
        })
    }

    /// A service server handing each request to `callback` as a
    /// [`PendingRequest`] to answer whenever ready, from any thread.
    pub fn service_deferred<S: RosService>(
        &self,
        name: &str,
        qos: Qos,
        callback: impl Fn(PendingRequest<S>) + Send + Sync + 'static,
    ) -> Result<RawService> {
        self.raw_service(name, S::DDS_TYPE_NAME, S::TYPE_HASH, qos, move |req| {
            if let Ok(request) = cdr::from_slice::<S::Request>(req.payload()) {
                callback(PendingRequest {
                    request,
                    raw: req,
                    _type: std::marker::PhantomData,
                });
            }
        })
    }

    /// A service client for raw CDR payloads of any type.
    pub fn raw_client(
        &self,
        name: &str,
        dds_type: &str,
        type_hash: &str,
        qos: Qos,
    ) -> Result<RawClient> {
        let name = self.qualify(name);
        let token_key = self.entity_key("SC", &name, dds_type, type_hash, &qos);
        let key = topic_key(self.cfg.domain, &name, dds_type, type_hash);
        let token = self.token(&token_key, "client")?;
        Ok(RawClient {
            session: self.session().clone(),
            domain: self.cfg.domain,
            name,
            dds_type: dds_type.to_owned(),
            key,
            gid: gid(&token_key),
            seq: AtomicI64::new(0),
            _token: token,
        })
    }

    /// A typed service client.
    pub fn client<S: RosService>(&self, name: &str, qos: Qos) -> Result<Client<S>> {
        Ok(Client {
            raw: self.raw_client(name, S::DDS_TYPE_NAME, S::TYPE_HASH, qos)?,
            _type: std::marker::PhantomData,
        })
    }
}

/// Period of the heartbeats a reliable transient-local publisher sends so
/// subscribers notice missed samples (rmw_zenoh's
/// `SAMPLE_MISS_DETECTION_HEARTBEAT_PERIOD`).
const HEARTBEAT_PERIOD: Duration = Duration::from_millis(500);

/// A timeout long enough to never expire in practice, for queries that
/// rmw_zenoh makes without one. (`Duration::MAX` would overflow when Zenoh
/// adds it to the current time.)
pub const FOREVER: Duration = Duration::from_secs(10 * 365 * 24 * 3600);

/// Publishes CDR payloads with rmw_zenoh attachments.
pub struct RawPublisher {
    publisher: AdvancedPublisher<'static>,
    gid: [u8; 16],
    seq: AtomicI64,
    key: String,
    _token: LivelinessToken,
}

impl RawPublisher {
    /// Publishes an encoded message (encapsulation header included).
    pub fn put(&self, payload: Vec<u8>) -> Result<()> {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let now = ruro_now_ns() as i64;
        self.publisher
            .put(payload)
            .attachment(attachment(seq, now, &self.gid))
            .wait()
            .map_err(|e| anyhow!("publishing on {}: {e}", self.key))
    }

    /// The data key expression.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The publisher GID.
    pub fn gid(&self) -> [u8; 16] {
        self.gid
    }
}

/// Publishes messages of one type on one topic.
pub struct Publisher<T> {
    raw: RawPublisher,
    _type: std::marker::PhantomData<fn(T)>,
}

impl<T: RosMessage> Publisher<T> {
    /// Encodes and publishes a message.
    pub fn publish(&self, msg: &T) -> Result<()> {
        self.raw.put(cdr::to_vec(msg))
    }

    /// The underlying raw publisher.
    pub fn raw(&self) -> &RawPublisher {
        &self.raw
    }
}

/// An active subscription; dropping it unsubscribes.
pub struct Subscriber {
    _subscriber: AdvancedSubscriber<()>,
    _token: LivelinessToken,
    key: String,
}

impl Subscriber {
    /// The data key expression.
    pub fn key(&self) -> &str {
        &self.key
    }
}

/// A request received by a service server, answered with
/// [`reply`](Self::reply). Dropping it unanswered leaves the client
/// waiting until its timeout.
pub struct ServiceRequest {
    query: zenoh::query::Query,
    payload: Vec<u8>,
    info: SampleInfo,
    key: String,
}

impl ServiceRequest {
    /// The encoded request (encapsulation header included).
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// The request's attachment: the client's sequence number, its source
    /// timestamp and the client GID.
    pub fn info(&self) -> SampleInfo {
        self.info
    }

    /// Sends the encoded response, with the attachment rmw_zenoh expects:
    /// the request's sequence number and client GID and a new timestamp.
    pub fn reply(self, payload: Vec<u8>) -> Result<()> {
        let a = attachment(
            self.info.sequence,
            ruro_now_ns() as i64,
            &self.info.publisher_gid,
        );
        self.query
            .reply(self.key.clone(), payload)
            .attachment(a)
            .wait()
            .map_err(|e| anyhow!("replying on {}: {e}", self.key))
    }
}

/// A decoded request of service type `S`, to answer when ready.
pub struct PendingRequest<S: RosService> {
    /// The request.
    pub request: S::Request,
    raw: ServiceRequest,
    _type: std::marker::PhantomData<fn(S)>,
}

impl<S: RosService> PendingRequest<S> {
    /// The request's attachment.
    pub fn info(&self) -> SampleInfo {
        self.raw.info
    }

    /// Encodes and sends the response.
    pub fn respond(self, response: &S::Response) -> Result<()> {
        self.raw.reply(cdr::to_vec(response))
    }
}

/// An active service server; dropping it withdraws the service.
pub struct RawService {
    _queryable: zenoh::query::Queryable<()>,
    _token: LivelinessToken,
    key: String,
}

impl RawService {
    /// The service's key expression.
    pub fn key(&self) -> &str {
        &self.key
    }
}

/// Calls a service with raw CDR payloads the way rmw_zenoh clients do: a
/// Zenoh `get` on the service key with target all-complete, no
/// consolidation, and an attachment carrying a per-client sequence number
/// (from 1), a timestamp and the client GID.
pub struct RawClient {
    session: zenoh::Session,
    domain: u32,
    name: String,
    dds_type: String,
    key: String,
    gid: [u8; 16],
    seq: AtomicI64,
    _token: LivelinessToken,
}

impl RawClient {
    /// Sends an encoded request; the reply arrives on the returned
    /// [`PendingCall`]. The call fails if no reply comes within `timeout`.
    pub fn send(&self, payload: Vec<u8>, timeout: Duration) -> Result<PendingCall> {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let replies = self
            .session
            .get(self.key.clone())
            .payload(payload)
            .attachment(attachment(seq, ruro_now_ns() as i64, &self.gid))
            .target(QueryTarget::AllComplete)
            .consolidation(ConsolidationMode::None)
            .timeout(timeout)
            .wait()
            .map_err(|e| anyhow!("calling {}: {e}", self.name))?;
        Ok(PendingCall {
            replies,
            sequence: seq,
            name: self.name.clone(),
        })
    }

    /// Sends an encoded request and waits for the encoded response.
    pub fn call(&self, payload: Vec<u8>, timeout: Duration) -> Result<(Vec<u8>, SampleInfo)> {
        self.send(payload, timeout)?.wait()
    }

    /// Whether a server for this service (same name and type) is in the
    /// ROS graph, waiting up to `timeout` for liveliness answers.
    pub fn server_available(&self, timeout: Duration) -> bool {
        let pattern = format!(
            "{ADMIN_SPACE}/{}/*/*/*/SS/*/*/*/{}/{}/*/*",
            self.domain,
            mangle(&self.name),
            self.dds_type
        );
        let Ok(replies) = self
            .session
            .liveliness()
            .get(pattern)
            .timeout(timeout)
            .wait()
        else {
            return false;
        };
        while let Ok(reply) = replies.recv() {
            if reply.result().is_ok() {
                return true;
            }
        }
        false
    }

    /// Polls [`server_available`](Self::server_available) until it holds or
    /// `timeout` passes.
    pub fn wait_for_server(&self, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if self.server_available(Duration::from_millis(200)) {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The service's key expression.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The client GID.
    pub fn gid(&self) -> [u8; 16] {
        self.gid
    }
}

/// A request in flight.
pub struct PendingCall {
    replies: zenoh::handlers::FifoChannelHandler<zenoh::query::Reply>,
    sequence: i64,
    name: String,
}

impl PendingCall {
    /// The request's sequence number.
    pub fn sequence(&self) -> i64 {
        self.sequence
    }

    fn accept(&self, reply: zenoh::query::Reply) -> Option<Result<(Vec<u8>, SampleInfo)>> {
        match reply.result() {
            Ok(sample) => {
                let info = sample
                    .attachment()
                    .and_then(|a| parse_attachment(&a.to_bytes()))?;
                // Replies to another request of this client are not ours.
                (info.sequence == self.sequence)
                    .then(|| Ok((sample.payload().to_bytes().into_owned(), info)))
            }
            Err(e) => Some(Err(anyhow!(
                "{} failed: {}",
                self.name,
                e.payload().try_to_string().unwrap_or_default()
            ))),
        }
    }

    /// The response if it has arrived (`None` while waiting); an error if
    /// the call failed or timed out.
    pub fn try_recv(&self) -> Option<Result<(Vec<u8>, SampleInfo)>> {
        loop {
            match self.replies.try_recv() {
                Ok(Some(reply)) => {
                    if let Some(r) = self.accept(reply) {
                        return Some(r);
                    }
                }
                Ok(None) => return None,
                Err(_) => return Some(Err(self.no_reply())),
            }
        }
    }

    /// Waits for the response.
    pub fn wait(self) -> Result<(Vec<u8>, SampleInfo)> {
        while let Ok(reply) = self.replies.recv() {
            if let Some(r) = self.accept(reply) {
                return r;
            }
        }
        Err(self.no_reply())
    }

    fn no_reply(&self) -> anyhow::Error {
        anyhow!("no reply from service {} (timed out)", self.name)
    }
}

/// Calls a service of type `S`.
pub struct Client<S> {
    raw: RawClient,
    _type: std::marker::PhantomData<fn(S)>,
}

impl<S: RosService> Client<S> {
    /// Sends a request; the response arrives on the returned
    /// [`PendingResponse`].
    pub fn send(&self, request: &S::Request, timeout: Duration) -> Result<PendingResponse<S>> {
        Ok(PendingResponse {
            call: self.raw.send(cdr::to_vec(request), timeout)?,
            _type: std::marker::PhantomData,
        })
    }

    /// Sends a request and waits for the response.
    pub fn call(&self, request: &S::Request, timeout: Duration) -> Result<S::Response> {
        self.send(request, timeout)?.wait()
    }

    /// The underlying raw client.
    pub fn raw(&self) -> &RawClient {
        &self.raw
    }
}

/// A typed request in flight.
pub struct PendingResponse<S: RosService> {
    call: PendingCall,
    _type: std::marker::PhantomData<fn(S)>,
}

impl<S: RosService> PendingResponse<S> {
    /// The response if it has arrived (`None` while waiting).
    pub fn try_recv(&self) -> Option<Result<S::Response>> {
        self.call
            .try_recv()
            .map(|r| r.and_then(|(b, _)| Ok(cdr::from_slice(&b)?)))
    }

    /// Waits for the response.
    pub fn wait(self) -> Result<S::Response> {
        let (b, _) = self.call.wait()?;
        Ok(cdr::from_slice(&b)?)
    }
}

fn ruro_now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Endpoint configurations keyed for reuse, so several bridges with the
/// same settings share one ROS node.
pub fn endpoint_cache() -> &'static Mutex<HashMap<EndpointConfig, Weak<Endpoint>>> {
    static CACHE: std::sync::OnceLock<Mutex<HashMap<EndpointConfig, Weak<Endpoint>>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// An endpoint for `cfg`, shared with other users of the same settings.
pub fn shared_endpoint(cfg: EndpointConfig) -> Result<Arc<Endpoint>> {
    let mut cache = endpoint_cache().lock().unwrap();
    cache.retain(|_, w| w.strong_count() > 0);
    if let Some(e) = cache.get(&cfg).and_then(Weak::upgrade) {
        return Ok(e);
    }
    let e = Arc::new(Endpoint::open(cfg.clone()).context("opening ROS 2 endpoint")?);
    cache.insert(cfg, Arc::downgrade(&e));
    Ok(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qos_strings_match_rmw_zenoh() {
        assert_eq!(Qos::default().to_keyexpr(), "::,10:,:,:,,");
        let sensor = Qos::sensor_data();
        assert_eq!(sensor.to_keyexpr(), "2::,5:,:,:,,");
        assert_eq!(
            Qos {
                depth: 42,
                ..Qos::default()
            }
            .to_keyexpr(),
            "::,:,:,:,,"
        );
        assert_eq!(Qos::from_keyexpr("2::,5:,:,:,,"), Some(sensor));
        assert_eq!(Qos::from_keyexpr(":1:,1:,:,:,,").unwrap().depth, 1);
        assert_eq!(Qos::from_keyexpr("::,:,:,:,,").unwrap().depth, 42);
        assert!("best_effort".parse::<Reliability>().is_ok());
        assert!("sometimes".parse::<Reliability>().is_err());
        assert_eq!(
            "transient_local".parse::<Durability>().unwrap(),
            Durability::TransientLocal
        );
        assert!("forever".parse::<Durability>().is_err());

        // Every field, round trip.
        let all = Qos {
            reliability: Reliability::BestEffort,
            depth: 7,
            durability: Durability::TransientLocal,
            history: History::KeepAll,
            deadline: Some(Duration::from_millis(1500)),
            lifespan: Some(Duration::from_secs(10)),
            liveliness: Liveliness::ManualByTopic,
            liveliness_lease: Some(Duration::from_nanos(5)),
        };
        assert_eq!(all.to_keyexpr(), "2:1:2,7:1,500000000:10,0:3,0,5");
        assert_eq!(Qos::from_keyexpr(&all.to_keyexpr()), Some(all));
        assert_eq!(Qos::from_keyexpr("::,10:,:,:,"), None);
        assert_eq!(Qos::from_keyexpr("9::,10:,:,:,,"), None);
    }

    /// QoS strings of real ROS 2 Jazzy endpoints (tests/data/jazzy_tokens.txt).
    #[test]
    fn qos_strings_of_real_ros_endpoints() {
        let tokens = include_str!("../tests/data/jazzy_tokens.txt");
        let mut n = 0;
        for key in tokens.lines().filter(|l| l.starts_with(ADMIN_SPACE)) {
            let qos = key.rsplit('/').next().unwrap();
            let parsed = Qos::from_keyexpr(qos).unwrap_or_else(|| panic!("{key}"));
            assert_eq!(parsed.to_keyexpr(), qos, "{key}");
            n += 1;
        }
        assert!(n >= 10);
        // Services and action feedback: the services default profile.
        assert_eq!(Qos::services().to_keyexpr(), "::,10:,:,:,,");
        // Action status and /tf_static: reliable, transient-local, depth 1.
        assert_eq!(Qos::transient_local(1).to_keyexpr(), ":1:,1:,:,:,,");
        // `ros2 topic pub --qos-durability transient_local --qos-depth 3
        // --qos-reliability best_effort`.
        let latched = Qos {
            reliability: Reliability::BestEffort,
            ..Qos::transient_local(3)
        };
        assert_eq!(latched.to_keyexpr(), "2:1:,3:,:,:,,");
        // /rosout: transient-local, depth 1000, lifespan 10 s.
        let rosout = Qos::from_keyexpr(":1:,1000:,:10,0:,,").unwrap();
        assert_eq!(rosout.lifespan, Some(Duration::from_secs(10)));
        assert_eq!(rosout.deadline, None);
        assert_eq!(rosout.depth, 1000);
    }

    #[test]
    fn keys_follow_the_design_document() {
        assert_eq!(
            topic_key(
                0,
                "/chatter",
                "std_msgs::msg::dds_::String_",
                "RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18"
            ),
            "0/chatter/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18"
        );
        assert_eq!(qualify("chatter", "/"), "/chatter");
        assert_eq!(qualify("chatter", "/robot1"), "/robot1/chatter");
        assert_eq!(qualify("/abs", "/robot1"), "/abs");
        assert_eq!(strip_slashes("/robot1/chatter/"), "robot1/chatter");
        assert_eq!(mangle("/robot1/chatter"), "%robot1%chatter");
        assert_eq!(unmangle("%robot1%chatter"), "/robot1/chatter");
    }

    #[test]
    fn attachment_layout() {
        let gid = [7u8; 16];
        let a = attachment(1, 2, &gid);
        assert_eq!(a.len(), 33);
        assert_eq!(&a[..8], &1i64.to_le_bytes());
        assert_eq!(&a[8..16], &2i64.to_le_bytes());
        assert_eq!(a[16], 16);
        assert_eq!(
            parse_attachment(&a),
            Some(SampleInfo {
                sequence: 1,
                source_timestamp_ns: 2,
                publisher_gid: gid
            })
        );
        assert_eq!(parse_attachment(&a[..32]), None);
    }

    #[test]
    fn gid_is_xxh3_128_low_half_first() {
        // XXH3-128 of the empty input, from the xxHash test vectors:
        // 99aa06d3014798d86001c324468d497f (high64 then low64).
        let g = gid("");
        assert_eq!(&g[..8], &0x6001c324468d497fu64.to_le_bytes());
        assert_eq!(&g[8..], &0x99aa06d3014798d8u64.to_le_bytes());
    }
}
