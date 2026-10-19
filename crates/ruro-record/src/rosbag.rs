//! Import of ROS 2 bags (rosbag2) into ruro recordings.
//!
//! rosbag2 stores bags either in SQLite (`.db3`) or in MCAP. Only the MCAP
//! storage is supported: convert SQLite bags first with `ros2 bag convert`
//! (storage `mcap`).
//!
//! Nothing is dropped. Topics of a type [`crate::cdr`] can convert become
//! channels of the ruro type; every other topic (and, with
//! [`ImportOptions::raw`], every topic) becomes a `ros2/Any` channel that
//! keeps each message's CDR bytes exactly, with the type name and RIHS01
//! hash, so that replay can feed a `ros2/sub` node's output and a `ros2/pub`
//! can publish them again. A message of a convertible type that fails to
//! convert goes to a `ros2/Any` channel named `<channel>_raw`. Raw channels
//! carry the bag's type definition and QoS profiles in their metadata.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ruro_msg::{Message, Ros2Any};

use crate::{McapWriter, Recording, cdr};

/// How imported topics are named and converted.
#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    /// Explicit `ROS topic -> node.port` names. Other topics become
    /// `<topic with / replaced by _>.out`.
    pub map: HashMap<String, String>,
    /// Compress the output with zstd.
    pub compress: bool,
    /// Keep every topic as `ros2/Any`, even those with a ruro equivalent.
    pub raw: bool,
}

/// What an import did, per ROS topic.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImportReport {
    pub topics: Vec<TopicReport>,
}

impl ImportReport {
    /// Messages written, converted or raw.
    pub fn written(&self) -> u64 {
        self.topics.iter().map(|t| t.converted + t.raw).sum()
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TopicReport {
    pub ros_topic: String,
    pub ros_type: String,
    /// RIHS01 hash of the type, recorded in the bag or computed from the
    /// definition it holds; empty if neither is available.
    pub type_hash: String,
    /// Channel of the ruro type, if messages were converted.
    pub ruro_topic: Option<String>,
    pub converted: u64,
    /// Channel of `ros2/Any` messages, if messages were kept raw.
    pub raw_topic: Option<String>,
    pub raw: u64,
    /// Messages that could not be imported at all (not CDR).
    pub skipped: u64,
    /// Why messages were skipped or kept raw after a failed conversion, or
    /// why the hash is unknown (first reason seen).
    pub reason: Option<String>,
}

/// Default ruro channel name for a ROS topic: `/camera/image_raw` becomes
/// `camera_image_raw.out`.
pub fn default_topic(ros_topic: &str) -> String {
    let mut node: String = ros_topic
        .trim_start_matches('/')
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    node.truncate(64);
    if node.is_empty() {
        node.push_str("topic");
    }
    format!("{node}.out")
}

/// The MCAP files of a bag: `src` itself, or every `.mcap` in a bag directory.
pub fn bag_files(src: &Path) -> Result<Vec<PathBuf>> {
    if src.is_file() {
        if src.extension().is_some_and(|e| e == "db3") {
            bail!(
                "{} is a SQLite rosbag2 file, which is not supported; convert it with \
                 `ros2 bag convert` to the mcap storage first",
                src.display()
            );
        }
        return Ok(vec![src.to_owned()]);
    }
    let mut files = Vec::new();
    let mut sqlite = false;
    for entry in std::fs::read_dir(src).with_context(|| format!("reading {}", src.display()))? {
        let path = entry?.path();
        match path.extension().and_then(|e| e.to_str()) {
            Some("mcap") => files.push(path),
            Some("db3") => sqlite = true,
            _ => {}
        }
    }
    files.sort();
    if files.is_empty() {
        if sqlite {
            bail!(
                "{} uses the SQLite storage, which is not supported; convert it with \
                 `ros2 bag convert` to the mcap storage first",
                src.display()
            );
        }
        bail!("no .mcap files in {}", src.display());
    }
    Ok(files)
}

/// The RIHS01 hash of a bag channel's type: the `topic_type_hash` rosbag2
/// (Jazzy and later) records, else computed from its definition.
pub fn channel_type_hash(channel: &mcap::Channel<'_>) -> Result<String, String> {
    if let Some(h) = channel
        .metadata
        .get("topic_type_hash")
        .filter(|h| !h.is_empty())
    {
        return Ok(h.clone());
    }
    schema_type_hash(channel)
}

/// The RIHS01 hash computed from a channel's `ros2msg` or `ros2idl`
/// definition.
pub fn schema_type_hash(channel: &mcap::Channel<'_>) -> Result<String, String> {
    let schema = channel.schema.as_ref().ok_or("the channel has no schema")?;
    let name = ruro_ros2::idl::TypeName::parse(&cdr::normalize_type(&schema.name))
        .ok_or_else(|| format!("bad type name `{}`", schema.name))?;
    let text = std::str::from_utf8(&schema.data).map_err(|_| "the schema is not UTF-8")?;
    let specs = ruro_ros2::dynamic::parse_schema(&name, &schema.encoding, text)
        .map_err(|e| format!("{e:#}"))?;
    let mut interfaces = ruro_ros2::idl::Interfaces::default();
    for s in specs {
        interfaces.add(s);
    }
    ruro_ros2::hash::type_hash(&interfaces, &name)
        .map_err(|missing| format!("the definition lacks {missing}"))
}

/// Metadata of a raw channel: where its messages came from and the bag's
/// definition of their type.
fn raw_metadata(channel: &mcap::Channel<'_>, hash: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    m.insert("ros2_topic".into(), channel.topic.clone());
    if let Some(s) = &channel.schema {
        m.insert("ros2_type".into(), cdr::normalize_type(&s.name));
        m.insert("ros2_schema_encoding".into(), s.encoding.clone());
        m.insert(
            "ros2_schema".into(),
            String::from_utf8_lossy(&s.data).into_owned(),
        );
    }
    if !hash.is_empty() {
        m.insert("ros2_type_hash".into(), hash.to_owned());
    }
    if let Some(q) = channel.metadata.get("offered_qos_profiles") {
        m.insert("offered_qos_profiles".into(), q.clone());
    }
    m
}

/// Converts the bag at `src` (a rosbag2 directory or `.mcap` file) into a
/// ruro recording at `out`.
pub fn import(src: &Path, out: &Path, opts: &ImportOptions) -> Result<ImportReport> {
    let mut writer = McapWriter::create(out, opts.compress)?;
    let mut reports: BTreeMap<String, TopicReport> = BTreeMap::new();
    for file in bag_files(src)? {
        let rec = Recording::open(&file)?;
        for m in rec.messages()? {
            let m = m?;
            let topic = m.topic().to_owned();
            let ros_type = cdr::normalize_type(m.ty());
            let report = reports.entry(topic.clone()).or_insert_with(|| {
                let (type_hash, reason) = match channel_type_hash(&m.channel) {
                    Ok(h) => (h, None),
                    Err(e) => (String::new(), Some(format!("type hash unknown: {e}"))),
                };
                TopicReport {
                    ros_topic: topic.clone(),
                    ros_type: ros_type.clone(),
                    type_hash,
                    reason,
                    ..Default::default()
                }
            });
            if m.encoding() != "cdr" {
                report.skipped += 1;
                report.reason.get_or_insert_with(|| {
                    format!("message encoding `{}` is not cdr", m.encoding())
                });
                continue;
            }
            let name = opts
                .map
                .get(&topic)
                .cloned()
                .unwrap_or_else(|| default_topic(&topic));
            let mut raw_name = name.clone();
            if !opts.raw {
                match cdr::decode(&ros_type, &m.data) {
                    Ok(Some(c)) => {
                        let mut meta = BTreeMap::new();
                        meta.insert("ros2_topic".to_owned(), topic.clone());
                        meta.insert("ros2_type".to_owned(), ros_type.clone());
                        let channel = writer.channel_with_metadata(
                            &name,
                            c.ty,
                            crate::encoding_of(c.ty),
                            &meta,
                        )?;
                        report.ruro_topic = Some(name);
                        report.converted += 1;
                        writer.write(channel, report.converted, m.log_time, &c.data)?;
                        continue;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        // Kept beside the converted channel.
                        report
                            .reason
                            .get_or_insert_with(|| format!("kept raw: {e:#}"));
                        raw_name = format!("{name}_raw");
                    }
                }
            }
            let msg = Ros2Any {
                type_name: ros_type,
                type_hash: report.type_hash.clone(),
                cdr: m.data.to_vec(),
            };
            let channel = writer.channel_with_metadata(
                &raw_name,
                Ros2Any::TYPE,
                crate::encoding_of(Ros2Any::TYPE),
                &raw_metadata(&m.channel, &report.type_hash),
            )?;
            report.raw_topic = Some(raw_name);
            report.raw += 1;
            writer.write(channel, report.raw, m.log_time, &msg.to_bytes())?;
        }
    }
    writer.finish()?;
    Ok(ImportReport {
        topics: reports.into_values().collect(),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ruro_msg::{Message, Ros2Any, Scalar, Twist};

    use super::*;
    use crate::cdr::writer::CdrWriter;

    fn dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("ruro-rosbag-test-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Writes a bag the way rosbag2's MCAP storage does: ros2msg schemas,
    /// cdr channels, zstd chunks.
    fn write_bag(path: &Path) {
        let file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
        let mut w = mcap::WriteOptions::new()
            .compression(Some(mcap::Compression::Zstd))
            .profile("ros2")
            .create(file)
            .unwrap();
        let none = BTreeMap::new();
        let f64_schema = w
            .add_schema("std_msgs/msg/Float64", "ros2msg", b"float64 data")
            .unwrap();
        let twist_schema = w
            .add_schema(
                "geometry_msgs/msg/Twist",
                "ros2msg",
                b"Vector3 linear\nVector3 angular\n\
                  ================================================================================\n\
                  MSG: geometry_msgs/Vector3\nfloat64 x\nfloat64 y\nfloat64 z\n",
            )
            .unwrap();
        let str_schema = w
            .add_schema("std_msgs/msg/String", "ros2msg", b"string data")
            .unwrap();
        let speed = w.add_channel(f64_schema, "/speed", "cdr", &none).unwrap();
        let cmd = w
            .add_channel(twist_schema, "/cmd_vel", "cdr", &none)
            .unwrap();
        let chatter = w.add_channel(str_schema, "/chatter", "cdr", &none).unwrap();
        let mut write = |channel: u16, t: u64, data: &[u8]| {
            let header = mcap::records::MessageHeader {
                channel_id: channel,
                sequence: 0,
                log_time: t,
                publish_time: t,
            };
            w.write_to_known_channel(&header, data).unwrap();
        };
        for i in 0..5 {
            write(
                speed,
                100 + i * 10,
                &CdrWriter::new().f64(i as f64 * 0.5).buf,
            );
            write(
                cmd,
                105 + i * 10,
                &CdrWriter::new()
                    .f64s(&[i as f64, 0.0, 0.0, 0.0, 0.0, 0.1])
                    .buf,
            );
        }
        write(chatter, 150, &CdrWriter::new().string("hello").buf);
        // A Float64 too short to convert.
        write(speed, 160, &[0, 1, 0, 0, 1, 2]);
        w.finish().unwrap();
    }

    #[test]
    fn imports_every_topic() {
        let d = dir();
        let bag = d.join("bag");
        std::fs::create_dir_all(&bag).unwrap();
        write_bag(&bag.join("bag_0.mcap"));
        let out = d.join("imported.mcap");
        let opts = ImportOptions {
            map: [("/cmd_vel".to_owned(), "base.cmd".to_owned())].into(),
            compress: true,
            raw: false,
        };
        let report = import(&bag, &out, &opts).unwrap();
        let by_topic = |t: &str| report.topics.iter().find(|r| r.ros_topic == t).unwrap();
        assert_eq!(by_topic("/speed").converted, 5);
        assert_eq!(by_topic("/speed").ruro_topic.as_deref(), Some("speed.out"));
        assert_eq!(by_topic("/cmd_vel").ruro_topic.as_deref(), Some("base.cmd"));
        // The hash comes from the schema when the bag records none.
        assert_eq!(
            by_topic("/cmd_vel").type_hash,
            "RIHS01_9c45bf16fe0983d80e3cfe750d6835843d265a9a6c46bd2e609fcddde6fb8d2a"
        );
        // A type with no ruro equivalent is kept, as ros2/Any.
        let chatter = by_topic("/chatter");
        assert_eq!((chatter.converted, chatter.raw), (0, 1));
        assert_eq!(chatter.raw_topic.as_deref(), Some("chatter.out"));
        assert_eq!(
            chatter.type_hash,
            "RIHS01_df668c740482bbd48fb39d76a70dfd4bd59db1288021743503259e948f6b1a18"
        );
        // A message that fails to convert is kept beside the converted ones.
        let speed = by_topic("/speed");
        assert_eq!(speed.raw, 1);
        assert_eq!(speed.raw_topic.as_deref(), Some("speed.out_raw"));
        assert!(speed.reason.as_deref().unwrap().contains("kept raw"));
        assert_eq!(report.written(), 12);

        let rec = Recording::open(&out).unwrap();
        let info = rec.info().unwrap();
        assert_eq!(info.messages, 12);
        assert_eq!((info.start_ns, info.end_ns), (100, 160));
        let mut speeds = Vec::new();
        let mut cmds = Vec::new();
        let mut raw = Vec::new();
        for m in rec.messages().unwrap() {
            let m = m.unwrap();
            match m.topic() {
                "speed.out" => {
                    assert_eq!(
                        (m.ty(), m.encoding()),
                        ("std/Scalar", crate::ENCODING_POSTCARD)
                    );
                    speeds.push(Scalar::decode(&m.data).unwrap().0);
                }
                "base.cmd" => cmds.push(Twist::decode(&m.data).unwrap().linear.x),
                "chatter.out" | "speed.out_raw" => {
                    assert_eq!(m.ty(), "ros2/Any");
                    assert_eq!(m.channel.metadata["ros2_schema_encoding"], "ros2msg");
                    raw.push(Ros2Any::decode(&m.data).unwrap());
                }
                other => panic!("unexpected topic {other}"),
            }
        }
        assert_eq!(speeds, [0.0, 0.5, 1.0, 1.5, 2.0]);
        assert_eq!(cmds, [0.0, 1.0, 2.0, 3.0, 4.0]);
        assert_eq!(raw[0].type_name, "std_msgs/msg/String");
        assert_eq!(raw[0].cdr, CdrWriter::new().string("hello").buf);
        assert_eq!(raw[1].cdr, [0, 1, 0, 0, 1, 2]);
    }

    #[test]
    fn raw_import_keeps_every_message_as_is() {
        let d = dir().join("raw");
        std::fs::create_dir_all(&d).unwrap();
        write_bag(&d.join("bag.mcap"));
        let out = d.join("raw.mcap");
        let opts = ImportOptions {
            raw: true,
            ..Default::default()
        };
        let report = import(&d.join("bag.mcap"), &out, &opts).unwrap();
        assert!(report.topics.iter().all(|t| t.converted == 0));
        assert_eq!(report.written(), 12);
        let rec = Recording::open(&out).unwrap();
        for m in rec.messages().unwrap() {
            assert_eq!(m.unwrap().ty(), "ros2/Any");
        }
    }

    #[test]
    fn sqlite_bags_are_rejected_clearly() {
        let d = dir().join("sqlite_bag");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("bag_0.db3"), b"").unwrap();
        let err = import(&d, &dir().join("never.mcap"), &ImportOptions::default()).unwrap_err();
        assert!(format!("{err:#}").contains("ros2 bag convert"));
    }

    #[test]
    fn default_names_are_valid_ruro_ids() {
        assert_eq!(default_topic("/camera/image_raw"), "camera_image_raw.out");
        assert_eq!(default_topic("/tf"), "tf.out");
        assert_eq!(default_topic("/"), "topic.out");
    }
}
