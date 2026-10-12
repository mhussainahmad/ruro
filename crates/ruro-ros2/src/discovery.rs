//! The ROS 2 graph as seen through rmw_zenoh liveliness tokens.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde::Serialize;
use zenoh::Wait;

use crate::idl::TypeName;
use crate::rmw::{ADMIN_SPACE, Qos, unmangle};

/// What a liveliness token announces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    Node,
    Publisher,
    Subscriber,
    Service,
    Client,
}

/// One parsed liveliness token.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Entity {
    pub kind: EntityKind,
    pub domain: u32,
    /// Zenoh session id of the process.
    pub zid: String,
    pub node_id: u32,
    pub entity_id: u32,
    pub enclave: String,
    pub namespace: String,
    pub node: String,
    /// Fully qualified topic or service name (not set for nodes).
    pub topic: Option<String>,
    /// ROS type name, e.g. `std_msgs/msg/String`.
    pub ros_type: Option<String>,
    pub dds_type: Option<String>,
    pub type_hash: Option<String>,
    pub qos: Option<String>,
}

impl Entity {
    /// Parses a liveliness key expression; `None` if it is not an rmw_zenoh token.
    pub fn parse(key: &str) -> Option<Self> {
        let parts: Vec<&str> = key.split('/').collect();
        if parts.len() < 9 || parts[0] != ADMIN_SPACE {
            return None;
        }
        let kind = match parts[5] {
            "NN" => EntityKind::Node,
            "MP" => EntityKind::Publisher,
            "MS" => EntityKind::Subscriber,
            "SS" => EntityKind::Service,
            "SC" => EntityKind::Client,
            _ => return None,
        };
        let full = kind != EntityKind::Node;
        if full && parts.len() < 13 {
            return None;
        }
        let at = |i: usize| full.then(|| parts[i].to_owned());
        Some(Self {
            kind,
            domain: parts[1].parse().ok()?,
            zid: parts[2].to_owned(),
            node_id: parts[3].parse().ok()?,
            entity_id: parts[4].parse().ok()?,
            enclave: unmangle(parts[6]),
            namespace: unmangle(parts[7]),
            node: unmangle(parts[8]),
            topic: at(9).map(|t| unmangle(&t)),
            ros_type: at(10).map(|t| TypeName::from_dds(&t).map(|n| n.to_string()).unwrap_or(t)),
            dds_type: at(10),
            type_hash: at(11),
            qos: at(12),
        })
    }

    /// The node's fully qualified name, e.g. `/robot1/talker`.
    pub fn node_fqn(&self) -> String {
        let ns = self.namespace.trim_end_matches('/');
        format!("{ns}/{}", self.node)
    }
}

/// A topic and who uses it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TopicInfo {
    /// Fully qualified name, e.g. `/chatter`.
    pub name: String,
    /// ROS type name, e.g. `std_msgs/msg/String`.
    pub ros_type: String,
    /// RIHS01 hash announced by the endpoints.
    pub type_hash: String,
    pub publishers: usize,
    pub subscribers: usize,
    /// Fully qualified names of the nodes publishing or subscribing.
    pub nodes: Vec<String>,
    /// Reliability of the first publisher, if any announced one.
    pub best_effort: bool,
}

/// Groups publisher and subscriber tokens into topics, sorted by name.
/// A topic used with two different types appears once per type.
pub fn topics(entities: &[Entity]) -> Vec<TopicInfo> {
    let mut by_topic: BTreeMap<(String, String, String), TopicInfo> = BTreeMap::new();
    for e in entities {
        let (Some(topic), Some(ty), Some(hash)) = (&e.topic, &e.ros_type, &e.type_hash) else {
            continue;
        };
        if !matches!(e.kind, EntityKind::Publisher | EntityKind::Subscriber) {
            continue;
        }
        let info = by_topic
            .entry((topic.clone(), ty.clone(), hash.clone()))
            .or_insert_with(|| TopicInfo {
                name: topic.clone(),
                ros_type: ty.clone(),
                type_hash: hash.clone(),
                publishers: 0,
                subscribers: 0,
                nodes: Vec::new(),
                best_effort: false,
            });
        match e.kind {
            EntityKind::Publisher => {
                if info.publishers == 0 {
                    info.best_effort = e
                        .qos
                        .as_deref()
                        .and_then(Qos::from_keyexpr)
                        .is_some_and(|q| q.reliability == crate::Reliability::BestEffort);
                }
                info.publishers += 1;
            }
            _ => info.subscribers += 1,
        }
        let node = e.node_fqn();
        if !info.nodes.contains(&node) {
            info.nodes.push(node);
        }
    }
    by_topic.into_values().collect()
}

/// Every rmw_zenoh entity alive in `domain`, queried through `session`.
pub fn entities(session: &zenoh::Session, domain: u32, timeout: Duration) -> Result<Vec<Entity>> {
    let replies = session
        .liveliness()
        .get(format!("{ADMIN_SPACE}/{domain}/**"))
        .timeout(timeout)
        .wait()
        .map_err(|e| anyhow!("liveliness query failed: {e}"))?;
    let mut out = Vec::new();
    while let Ok(reply) = replies.recv() {
        if let Ok(sample) = reply.result()
            && let Some(e) = Entity::parse(sample.key_expr().as_str())
        {
            out.push(e);
        }
    }
    Ok(out)
}

/// The topics of a ROS 2 system reachable through `session`.
pub fn list_topics(
    session: &zenoh::Session,
    domain: u32,
    timeout: Duration,
) -> Result<Vec<TopicInfo>> {
    Ok(topics(&entities(session, domain, timeout)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TALKER: &str = "@ros2_lv/0/8b20917502ee955ac4476e0266340d5c/0/10/MP/%/%/talker/%chatter/std_msgs::msg::dds_::String_/RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18/::,7:,:,:,,";

    #[test]
    fn parses_the_design_document_example() {
        let e = Entity::parse(TALKER).unwrap();
        assert_eq!(e.kind, EntityKind::Publisher);
        assert_eq!(e.zid, "8b20917502ee955ac4476e0266340d5c");
        assert_eq!((e.node_id, e.entity_id), (0, 10));
        assert_eq!(e.namespace, "/");
        assert_eq!(e.node_fqn(), "/talker");
        assert_eq!(e.topic.as_deref(), Some("/chatter"));
        assert_eq!(e.ros_type.as_deref(), Some("std_msgs/msg/String"));
        assert_eq!(
            Qos::from_keyexpr(e.qos.as_deref().unwrap()).unwrap().depth,
            7
        );

        let node = Entity::parse("@ros2_lv/0/abc/3/3/NN/%/%robot1/driver").unwrap();
        assert_eq!(node.kind, EntityKind::Node);
        assert_eq!(node.node_fqn(), "/robot1/driver");
        assert_eq!(node.topic, None);
        assert!(Entity::parse("@ros2_lv/0/abc/3/3/MP/%/%/x").is_none());
        assert!(Entity::parse("ruro/x/status").is_none());
    }

    #[test]
    fn topics_aggregate_endpoints() {
        let listener = TALKER
            .replace("/0/10/MP/", "/1/11/MS/")
            .replace("/talker/", "/listener/");
        let other = "@ros2_lv/0/ff/2/5/MP/%/%/cam/%image/sensor_msgs::msg::dds_::Image_/RIHS01_x/2::,5:,:,:,,";
        let es: Vec<Entity> = [TALKER, &listener, other]
            .iter()
            .map(|k| Entity::parse(k).unwrap())
            .collect();
        let t = topics(&es);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].name, "/chatter");
        assert_eq!((t[0].publishers, t[0].subscribers), (1, 1));
        assert_eq!(t[0].nodes, ["/talker", "/listener"]);
        assert_eq!(t[1].ros_type, "sensor_msgs/msg/Image");
        assert!(t[1].best_effort);
    }
}
