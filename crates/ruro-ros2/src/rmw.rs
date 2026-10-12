//! ROS 2 publish/subscribe over Zenoh, following rmw_zenoh's conventions.
//!
//! What a ROS 2 node using `rmw_zenoh_cpp` expects from a peer:
//!
//! - **Data key**: `<domain>/<topic without outer slashes>/<DDS type>/<hash>`,
//!   e.g. `0/chatter/std_msgs::msg::dds_::String_/RIHS01_df66…`.
//! - **Payload**: the CDR encoding with its 4-byte encapsulation header.
//! - **Attachment** on every sample (samples without one are dropped):
//!   sequence number (`i64` LE, from 1), source timestamp (`i64` LE, ns since
//!   the epoch), then the 16-byte publisher GID prefixed by its length (`0x10`).
//! - **Liveliness tokens** announce nodes and endpoints for graph discovery
//!   (`ros2 topic list`, `ros2 node list`, type lookup in `ros2 topic echo`):
//!   `@ros2_lv/<domain>/<zid>/<node id>/<entity id>/<kind>/<enclave>/<namespace>/<node>[/<topic>/<type>/<hash>/<qos>]`
//!   with `/` inside names replaced by `%`.
//! - **GID**: XXH3-128 of the endpoint's liveliness key, low half first.
//!
//! Only volatile durability is implemented: transient-local late joiners
//! (e.g. `/tf_static` subscribers that start after the publisher) need
//! zenoh's advanced publisher cache, which this module does not use. The
//! reliability setting is announced in liveliness tokens, so ROS 2 tools
//! report it, but samples are always sent with zenoh's default reliability
//! because zenoh only exposes that setting as an unstable API.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Weak};

use anyhow::{Context, Result, anyhow};
use zenoh::Wait;
use zenoh::liveliness::LivelinessToken;

use crate::RosMessage;
use crate::cdr::{self, CdrError};

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

/// The QoS settings ruro supports (history is always keep-last, durability
/// always volatile).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Qos {
    pub reliability: Reliability,
    /// Keep-last history depth.
    pub depth: usize,
}

impl Default for Qos {
    fn default() -> Self {
        Self {
            reliability: Reliability::Reliable,
            depth: 10,
        }
    }
}

impl Qos {
    /// rmw_zenoh's QoS rendering for liveliness keys. Values equal to its
    /// defaults (reliable, volatile, keep-last 42, infinite durations,
    /// automatic liveliness) are left empty.
    pub fn to_keyexpr(&self) -> String {
        let reliability = match self.reliability {
            Reliability::Reliable => "",
            Reliability::BestEffort => "2",
        };
        let depth = if self.depth == 42 || self.depth == 0 {
            String::new()
        } else {
            self.depth.to_string()
        };
        format!("{reliability}::,{depth}:,:,:,,")
    }

    /// Parses [`to_keyexpr`](Self::to_keyexpr) output (other fields ignored).
    pub fn from_keyexpr(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() != 6 {
            return None;
        }
        let reliability = match parts[0] {
            "" | "1" => Reliability::Reliable,
            "2" => Reliability::BestEffort,
            _ => return None,
        };
        let depth = parts[2]
            .split_once(',')
            .map(|(_, d)| d)
            .filter(|d| !d.is_empty())
            .map_or(Some(42), |d| d.parse().ok())?;
        Some(Self { reliability, depth })
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
/// publishers and subscribers from it.
pub struct Endpoint {
    shared: Arc<SharedSession>,
    cfg: EndpointConfig,
    zid: String,
    node_id: u32,
    _token: LivelinessToken,
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
        Ok(Self {
            shared,
            cfg,
            zid,
            node_id,
            _token: token,
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

    /// A publisher of raw CDR payloads for any type.
    pub fn raw_publisher(
        &self,
        topic: &str,
        dds_type: &str,
        type_hash: &str,
        qos: Qos,
    ) -> Result<RawPublisher> {
        let topic = qualify(topic, &self.cfg.namespace);
        let token_key = self.entity_key("MP", &topic, dds_type, type_hash, &qos);
        let key = topic_key(self.cfg.domain, &topic, dds_type, type_hash);
        let publisher = self
            .session()
            .declare_publisher(key.clone())
            .wait()
            .map_err(|e| anyhow!("declaring publisher on {key}: {e}"))?;
        let token = self
            .session()
            .liveliness()
            .declare_token(token_key.clone())
            .wait()
            .map_err(|e| anyhow!("declaring publisher token: {e}"))?;
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
        let topic = qualify(topic, &self.cfg.namespace);
        let token_key = self.entity_key("MS", &topic, dds_type, type_hash, &qos);
        let key = topic_key(self.cfg.domain, &topic, dds_type, type_hash);
        let subscriber = self
            .session()
            .declare_subscriber(key.clone())
            .callback(move |sample| {
                let info = sample
                    .attachment()
                    .and_then(|a| parse_attachment(&a.to_bytes()));
                callback(&sample.payload().to_bytes(), info);
            })
            .wait()
            .map_err(|e| anyhow!("declaring subscriber on {key}: {e}"))?;
        let token = self
            .session()
            .liveliness()
            .declare_token(token_key)
            .wait()
            .map_err(|e| anyhow!("declaring subscriber token: {e}"))?;
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
}

/// Publishes CDR payloads with rmw_zenoh attachments.
pub struct RawPublisher {
    publisher: zenoh::pubsub::Publisher<'static>,
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
    _subscriber: zenoh::pubsub::Subscriber<()>,
    _token: LivelinessToken,
    key: String,
}

impl Subscriber {
    /// The data key expression.
    pub fn key(&self) -> &str {
        &self.key
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
        let sensor = Qos {
            reliability: Reliability::BestEffort,
            depth: 5,
        };
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
