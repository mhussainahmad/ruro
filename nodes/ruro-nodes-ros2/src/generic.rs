//! Bridge nodes for ROS 2 messages of any type.
//!
//! `ros2/sub` and `ros2/pub` move messages between a ROS 2 topic and a
//! `ros2/Any` port without decoding them, so every type passes through
//! unchanged, including types ruro has never seen. They need the type's
//! RIHS01 hash, which is part of the topic's key: from the `type_hash`
//! parameter, else from the type's definition (see
//! [`ruro_ros2::search::TypeLibrary`]), else, for a subscriber, from what the
//! topic's publishers announce.
//!
//! `ros2/field` reads one field of a `ros2/Any` message, given the type's
//! definition, and `ros2/decode` renders the whole message as YAML.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ruro::msg::{Ros2Any, Ros2AnyPreviewer, Scalar, Text};
use ruro::prelude::*;
use ruro_ros2::dynamic::{self, FieldPath, MessageType, Value};
use ruro_ros2::idl::TypeName;
use ruro_ros2::rmw::{Endpoint, RawPublisher, Subscriber, qualify};
use ruro_ros2::search::TypeLibrary;

use crate::Settings;

/// Directories listed in an `interfaces` parameter (separated by `:` or `,`).
fn interface_dirs(param: &str) -> Vec<PathBuf> {
    param
        .split([':', ','])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Resolves a type in the process-wide library, searching `interfaces` first.
pub fn resolve(ty: &TypeName, interfaces: &str) -> Result<Arc<MessageType>> {
    let mut lib = TypeLibrary::global().lock().unwrap();
    for dir in interface_dirs(interfaces) {
        lib.add_dir(&dir);
    }
    lib.resolve(ty)
}

/// Parses a `type` parameter, accepting `pkg/Type` for `pkg/msg/Type`.
fn type_name(s: &str) -> Result<TypeName> {
    TypeName::parse(s.trim())
        .filter(|t| !t.package.is_empty() && !t.name.is_empty())
        .ok_or_else(|| anyhow::anyhow!("parameter `type`: expected `pkg/msg/Type`, got `{s}`"))
}

/// Previews `ros2/Any` messages as YAML when the type's definition is known.
fn preview(m: &Ros2Any) -> Option<ruro::msg::Preview> {
    let ty = TypeName::parse(&m.type_name)?;
    let t = TypeLibrary::global().lock().unwrap().resolve(&ty).ok()?;
    let mut text = match dynamic::decode(&t, &m.cdr) {
        Ok(d) => d.value.to_yaml(Some(16)),
        Err(e) => format!("# cannot decode as {ty}: {e}\n"),
    };
    if !m.type_hash.is_empty() && m.type_hash != t.hash {
        text.insert_str(
            0,
            &format!(
                "# the local definition of {ty} has hash {}, the message {}\n",
                t.hash, m.type_hash
            ),
        );
    }
    Some(ruro::msg::Preview::Text(text))
}

ruro::msg::__private::inventory::submit! {
    Ros2AnyPreviewer { preview }
}

/// Finds the hash publishers announce for `topic` with type `ty`.
fn announced_hash(endpoint: &Endpoint, topic: &str, ty: &TypeName) -> Result<Option<String>> {
    let topic = qualify(topic, "/");
    let ty = ty.to_string();
    let topics = ruro_ros2::list_topics(
        endpoint.session(),
        endpoint.domain(),
        Duration::from_millis(500),
    )?;
    let mut hashes: Vec<String> = topics
        .into_iter()
        .filter(|t| t.name == topic && t.ros_type == ty && t.publishers > 0)
        .map(|t| t.type_hash)
        .collect();
    hashes.dedup();
    match hashes.len() {
        0 => Ok(None),
        1 => Ok(hashes.pop()),
        _ => bail!("publishers of {topic} announce different hashes for {ty}: {hashes:?}"),
    }
}

/// What a generic bridge node was configured with.
#[derive(Debug, Clone, PartialEq)]
struct Want {
    settings: Settings,
    ty: TypeName,
    /// Explicit `type_hash` parameter, if any.
    hash: String,
    interfaces: String,
}

#[allow(clippy::too_many_arguments)]
fn want(
    ctx: &Ctx,
    topic: &str,
    ty: &str,
    hash: &str,
    interfaces: &str,
    domain_id: i64,
    qos: &str,
    depth: i64,
    router: &str,
    token: &str,
) -> Result<Want> {
    let mut settings = Settings::new(ctx.id(), topic, domain_id, qos, router, token)?;
    settings.qos.depth = usize::try_from(depth).context("parameter `depth`")?;
    let hash = hash.trim().to_owned();
    if !hash.is_empty() && !hash.starts_with("RIHS01_") {
        bail!("parameter `type_hash`: expected `RIHS01_<hex>`, got `{hash}`");
    }
    Ok(Want {
        settings,
        ty: type_name(ty)?,
        hash,
        interfaces: interfaces.to_owned(),
    })
}

/// The hash for a configured type: explicit, from the definition, or from
/// the network (`endpoint` given). `Ok(None)` if not known yet.
fn hash_for(want: &Want, endpoint: Option<&Endpoint>) -> Result<Option<String>> {
    if !want.hash.is_empty() {
        return Ok(Some(want.hash.clone()));
    }
    let local = resolve(&want.ty, &want.interfaces);
    match (local, endpoint) {
        (Ok(t), _) => Ok(Some(t.hash.clone())),
        (Err(e), None) => Err(e),
        (Err(e), Some(ep)) => match announced_hash(ep, &want.settings.topic, &want.ty)? {
            Some(h) => {
                info!(
                    "{}: no local definition of {} ({e:#}); using the hash its publishers announce",
                    want.settings.topic, want.ty
                );
                Ok(Some(h))
            }
            None => Err(e.context(format!(
                "no publisher of {} announces its hash yet; set `type_hash` or `interfaces`",
                want.settings.topic
            ))),
        },
    }
}

/// An active generic subscription.
pub struct AnySubState {
    want: Want,
    _endpoint: Arc<Endpoint>,
    _subscriber: Subscriber,
}

/// Subscribes to a ROS 2 topic of any type and outputs each message
/// unchanged as `ros2/Any`.
#[derive(Node)]
#[node(
    kind = "ros2/sub",
    rate = 1,
    doc = "Subscribes to a ROS 2 topic of any type and outputs each message, unchanged, as ros2/Any."
)]
pub struct AnySub {
    #[output]
    out: Output<Ros2Any>,
    /// ROS 2 topic name.
    #[param(default = "/chatter")]
    topic: String,
    /// ROS type, e.g. `sensor_msgs/msg/CompressedImage`.
    #[param(default = "std_msgs/msg/String")]
    r#type: String,
    /// RIHS01 hash of the type. Empty: from the type's definition, else from
    /// the topic's publishers.
    #[param(default = "")]
    type_hash: String,
    /// Directories with interface definitions (packages, directories of
    /// packages or install prefixes), separated by `:`. Searched before
    /// AMENT_PREFIX_PATH and the bundled definitions.
    #[param(default = "")]
    interfaces: String,
    /// ROS_DOMAIN_ID; -1 uses the environment variable (default 0).
    #[param(default = -1, min = -1, max = 232)]
    domain_id: i64,
    /// `reliable` or `best_effort`.
    #[param(default = "reliable")]
    qos: String,
    /// Keep-last history depth announced to ROS 2.
    #[param(default = 10, min = 1, max = 10000)]
    depth: i64,
    /// Zenoh router shared with the ROS 2 nodes.
    #[param(default = "tcp/127.0.0.1:7447")]
    router: String,
    /// Token of a router that requires one; empty uses RURO_TOKEN.
    #[param(default = "")]
    token: String,
    state: Option<AnySubState>,
}

impl AnySub {
    fn ensure(&mut self, ctx: &Ctx) -> Result {
        let want = want(
            ctx,
            &self.topic,
            &self.r#type,
            &self.type_hash,
            &self.interfaces,
            self.domain_id,
            &self.qos,
            self.depth,
            &self.router,
            &self.token,
        )?;
        if self.state.as_ref().is_some_and(|s| s.want == want) {
            return Ok(());
        }
        self.state = None;
        let endpoint = want.settings.endpoint()?;
        let hash = hash_for(&want, Some(&endpoint))?.expect("hash or error");
        let out = self.out.clone();
        let type_name = want.ty.to_string();
        let type_hash = hash.clone();
        let subscriber = endpoint.raw_subscriber(
            &want.settings.topic,
            &want.ty.dds_name(),
            &hash,
            want.settings.qos,
            move |bytes, _| {
                let _ = out.send(Ros2Any {
                    type_name: type_name.clone(),
                    type_hash: type_hash.clone(),
                    cdr: bytes.to_vec(),
                });
            },
        )?;
        info!(
            "subscribed to ROS 2 {} ({} {hash}) in domain {}",
            want.settings.topic, want.ty, want.settings.domain
        );
        self.state = Some(AnySubState {
            want,
            _endpoint: endpoint,
            _subscriber: subscriber,
        });
        Ok(())
    }
}

impl Node for AnySub {
    fn start(&mut self, ctx: &mut Ctx) -> Result {
        // A hash that cannot be found yet (no publisher) is retried on
        // every step rather than failing the node.
        if let Err(e) = self.ensure(ctx) {
            type_name(&self.r#type)?;
            warn!("{e:#}");
        }
        Ok(())
    }

    fn step(&mut self, ctx: &mut Ctx) -> Result {
        self.ensure(ctx)
    }

    fn stop(&mut self, _ctx: &mut Ctx) {
        self.state = None;
    }
}

/// An active generic publisher.
pub struct AnyPubState {
    want: Want,
    /// The type actually published (the parameter, or the first message's).
    ty: TypeName,
    hash: String,
    _endpoint: Arc<Endpoint>,
    publisher: RawPublisher,
}

/// Publishes every `ros2/Any` input unchanged on a ROS 2 topic.
#[derive(Node)]
#[node(
    kind = "ros2/pub",
    doc = "Publishes every ros2/Any input message, unchanged, on a ROS 2 topic."
)]
pub struct AnyPub {
    #[input]
    input: Input<Ros2Any>,
    /// ROS 2 topic name.
    #[param(default = "/chatter")]
    topic: String,
    /// ROS type, e.g. `sensor_msgs/msg/CompressedImage`. Empty: the type of
    /// the first message received. Messages of another type are dropped.
    #[param(default = "")]
    r#type: String,
    /// RIHS01 hash of the type. Empty: the messages' own hash, else the
    /// type's definition.
    #[param(default = "")]
    type_hash: String,
    /// Directories with interface definitions, separated by `:`.
    #[param(default = "")]
    interfaces: String,
    /// ROS_DOMAIN_ID; -1 uses the environment variable (default 0).
    #[param(default = -1, min = -1, max = 232)]
    domain_id: i64,
    /// `reliable` or `best_effort`.
    #[param(default = "reliable")]
    qos: String,
    /// Keep-last history depth announced to ROS 2.
    #[param(default = 10, min = 1, max = 10000)]
    depth: i64,
    /// Zenoh router shared with the ROS 2 nodes.
    #[param(default = "tcp/127.0.0.1:7447")]
    router: String,
    /// Token of a router that requires one; empty uses RURO_TOKEN.
    #[param(default = "")]
    token: String,
    state: Option<AnyPubState>,
}

impl AnyPub {
    /// The publisher for `msg`, created on first use.
    fn publisher_for(&mut self, ctx: &Ctx, msg: &Ros2Any) -> Result<&AnyPubState> {
        let ty_param = if self.r#type.trim().is_empty() {
            msg.type_name.as_str()
        } else {
            self.r#type.as_str()
        };
        let want = want(
            ctx,
            &self.topic,
            ty_param,
            &self.type_hash,
            &self.interfaces,
            self.domain_id,
            &self.qos,
            self.depth,
            &self.router,
            &self.token,
        )?;
        let stale = self.state.as_ref().is_none_or(|s| {
            // Following the input's type: a new type is a new publisher.
            s.want.settings != want.settings
                || s.want.hash != want.hash
                || s.want.interfaces != want.interfaces
                || (!self.r#type.trim().is_empty() && s.want.ty != want.ty)
        });
        if stale {
            self.state = None;
            let hash = if want.hash.is_empty() && !msg.type_hash.is_empty() {
                msg.type_hash.clone()
            } else {
                hash_for(&want, None)?.expect("hash or error")
            };
            let endpoint = want.settings.endpoint()?;
            let publisher = endpoint.raw_publisher(
                &want.settings.topic,
                &want.ty.dds_name(),
                &hash,
                want.settings.qos,
            )?;
            info!(
                "publishing ROS 2 {} ({} {hash}) in domain {}",
                want.settings.topic, want.ty, want.settings.domain
            );
            self.state = Some(AnyPubState {
                ty: want.ty.clone(),
                want,
                hash,
                _endpoint: endpoint,
                publisher,
            });
        }
        Ok(self.state.as_ref().expect("created"))
    }
}

impl Node for AnyPub {
    fn start(&mut self, _ctx: &mut Ctx) -> Result {
        if !self.r#type.trim().is_empty() {
            type_name(&self.r#type)?;
        }
        Ok(())
    }

    fn step(&mut self, ctx: &mut Ctx) -> Result {
        while let Some(sample) = self.input.recv() {
            let msg = &sample.msg;
            let state = self.publisher_for(ctx, msg)?;
            let msg_ty = type_name(&msg.type_name).ok();
            if msg_ty.as_ref() != Some(&state.ty) {
                warn!(
                    "{}: dropped a {} message; this topic carries {}",
                    state.want.settings.topic, msg.type_name, state.ty
                );
                continue;
            }
            if !msg.type_hash.is_empty() && msg.type_hash != state.hash {
                warn!(
                    "{}: dropped a {} message with hash {}; the topic uses {}",
                    state.want.settings.topic, msg.type_name, msg.type_hash, state.hash
                );
                continue;
            }
            state.publisher.put(msg.cdr.clone())?;
        }
        Ok(())
    }

    fn stop(&mut self, _ctx: &mut Ctx) {
        self.state = None;
    }
}

/// Reads one field of `ros2/Any` messages: numbers and booleans on `value`,
/// strings (and other fields, as YAML) on `text`.
#[derive(Node)]
#[node(
    kind = "ros2/field",
    doc = "Reads one field of ros2/Any messages, given a path such as pose.position.x or data[0]: numbers and booleans are output on `value`, strings and other fields (as YAML) on `text`."
)]
pub struct Field {
    #[input]
    input: Input<Ros2Any>,
    #[output]
    value: Output<Scalar>,
    #[output]
    text: Output<Text>,
    /// Field path, e.g. `pose.position.x`, `ranges[0]` or `points[-1].y`.
    #[param(default = "data")]
    path: String,
    /// Directories with interface definitions, separated by `:`.
    #[param(default = "")]
    interfaces: String,
    /// Compiled path per type.
    compiled: Option<(String, String, Arc<MessageType>, FieldPath)>,
}

impl Field {
    fn plan(&mut self, type_name_: &str) -> Result<(Arc<MessageType>, FieldPath)> {
        if let Some((t, p, ty, path)) = &self.compiled
            && t == type_name_
            && p == &self.path
        {
            return Ok((ty.clone(), path.clone()));
        }
        let ty = resolve(&type_name(type_name_)?, &self.interfaces)?;
        let path = FieldPath::compile(&ty, &self.path)?;
        self.compiled = Some((
            type_name_.to_owned(),
            self.path.clone(),
            ty.clone(),
            path.clone(),
        ));
        Ok((ty, path))
    }
}

impl Node for Field {
    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        while let Some(sample) = self.input.recv() {
            let msg = &sample.msg;
            let (ty, path) = self.plan(&msg.type_name)?;
            let decoded = dynamic::decode(&ty, &msg.cdr)
                .with_context(|| format!("decoding {}", msg.type_name))?;
            let Some(v) = path.get(&decoded.value) else {
                warn!("{}: `{}` is out of range", msg.type_name, self.path);
                continue;
            };
            match &v {
                Value::String(s) | Value::WString(s) => self.text.send(Text(s.clone()))?,
                Value::Message(m) => self.text.send(Text(m.to_yaml(None)))?,
                Value::Array(_) => {
                    let json = v.to_json();
                    self.text.send(Text(json.to_string()))?
                }
                scalar => {
                    let x = scalar.as_f64().expect("numeric");
                    self.value.send(Scalar(x))?
                }
            }
        }
        Ok(())
    }
}

/// Renders `ros2/Any` messages as YAML text.
#[derive(Node)]
#[node(
    kind = "ros2/decode",
    doc = "Renders each ros2/Any message as YAML, as `ros2 topic echo` prints it."
)]
pub struct Decode {
    #[input]
    input: Input<Ros2Any>,
    #[output]
    out: Output<Text>,
    /// Directories with interface definitions, separated by `:`.
    #[param(default = "")]
    interfaces: String,
    /// Arrays longer than this are cut short; 0 prints them whole.
    #[param(default = 64, min = 0)]
    max_array: i64,
}

impl Node for Decode {
    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        while let Some(sample) = self.input.recv() {
            let msg = &sample.msg;
            let ty = resolve(&type_name(&msg.type_name)?, &self.interfaces)?;
            let decoded = dynamic::decode(&ty, &msg.cdr)
                .with_context(|| format!("decoding {}", msg.type_name))?;
            let max = usize::try_from(self.max_array).ok().filter(|m| *m > 0);
            self.out.send(Text(decoded.value.to_yaml(max)))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interface_lists_split_on_colons_and_commas() {
        assert_eq!(
            interface_dirs(" /a:/b , /c "),
            [
                PathBuf::from("/a"),
                PathBuf::from("/b"),
                PathBuf::from("/c")
            ]
        );
        assert!(interface_dirs("").is_empty());
    }

    #[test]
    fn type_names_accept_the_short_form() {
        assert_eq!(
            type_name("std_msgs/String").unwrap().to_string(),
            "std_msgs/msg/String"
        );
        assert!(type_name("String").is_err());
    }

    #[test]
    fn previews_are_yaml_when_the_type_is_known() {
        let m = Ros2Any {
            type_name: "std_msgs/msg/String".into(),
            type_hash: String::new(),
            cdr: vec![0, 1, 0, 0, 3, 0, 0, 0, b'h', b'i', 0],
        };
        assert_eq!(
            preview(&m),
            Some(ruro::msg::Preview::Text("data: 'hi'\n".into()))
        );
        let unknown = Ros2Any {
            type_name: "nope_msgs/msg/X".into(),
            ..m
        };
        assert_eq!(preview(&unknown), None);
    }
}
