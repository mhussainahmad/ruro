//! Import of ROS 2 bags (rosbag2) into ruro recordings.
//!
//! rosbag2 stores bags either in SQLite (`.db3`) or in MCAP. Only the MCAP
//! storage is supported: its channels carry CDR payloads, which
//! [`crate::cdr`] converts into ruro messages. Convert SQLite bags first with
//! `ros2 bag convert` (storage `mcap`).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::{McapWriter, Recording, cdr};

/// How imported topics are named.
#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    /// Explicit `ROS topic -> node.port` names. Other topics become
    /// `<topic with / replaced by _>.out`.
    pub map: HashMap<String, String>,
    /// Compress the output with zstd.
    pub compress: bool,
}

/// What an import did, per ROS topic.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImportReport {
    pub topics: Vec<TopicReport>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TopicReport {
    pub ros_topic: String,
    pub ros_type: String,
    /// Channel written in the ruro recording, if the type is supported.
    pub ruro_topic: Option<String>,
    pub converted: u64,
    pub skipped: u64,
    /// Why messages were skipped (first error seen).
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
            let report = reports.entry(topic.clone()).or_insert_with(|| TopicReport {
                ros_topic: topic.clone(),
                ros_type: m.ty().to_owned(),
                ..Default::default()
            });
            if m.encoding() != "cdr" {
                report.skipped += 1;
                report.reason.get_or_insert_with(|| {
                    format!("message encoding `{}` is not cdr", m.encoding())
                });
                continue;
            }
            match cdr::decode(m.ty(), &m.data) {
                Ok(Some(c)) => {
                    let name = opts
                        .map
                        .get(&topic)
                        .cloned()
                        .unwrap_or_else(|| default_topic(&topic));
                    let channel = writer.channel(&name, c.ty, crate::encoding_of(c.ty))?;
                    report.ruro_topic = Some(name);
                    report.converted += 1;
                    writer.write(channel, report.converted, m.log_time, &c.data)?;
                }
                Ok(None) => {
                    report.skipped += 1;
                    report
                        .reason
                        .get_or_insert_with(|| format!("type `{}` is not supported", m.ty()));
                }
                Err(e) => {
                    report.skipped += 1;
                    report.reason.get_or_insert_with(|| format!("{e:#}"));
                }
            }
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

    use ruro_msg::{Message, Scalar, Twist};

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
                b"Vector3 linear\nVector3 angular",
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
        w.finish().unwrap();
    }

    #[test]
    fn imports_supported_topics() {
        let d = dir();
        let bag = d.join("bag");
        std::fs::create_dir_all(&bag).unwrap();
        write_bag(&bag.join("bag_0.mcap"));
        let out = d.join("imported.mcap");
        let opts = ImportOptions {
            map: [("/cmd_vel".to_owned(), "base.cmd".to_owned())].into(),
            compress: true,
        };
        let report = import(&bag, &out, &opts).unwrap();
        let by_topic = |t: &str| report.topics.iter().find(|r| r.ros_topic == t).unwrap();
        assert_eq!(by_topic("/speed").converted, 5);
        assert_eq!(by_topic("/speed").ruro_topic.as_deref(), Some("speed.out"));
        assert_eq!(by_topic("/cmd_vel").ruro_topic.as_deref(), Some("base.cmd"));
        let chatter = by_topic("/chatter");
        assert_eq!((chatter.converted, chatter.skipped), (0, 1));
        assert!(chatter.reason.as_deref().unwrap().contains("not supported"));

        let rec = Recording::open(&out).unwrap();
        let info = rec.info().unwrap();
        assert_eq!(info.messages, 10);
        assert_eq!((info.start_ns, info.end_ns), (100, 145));
        let mut speeds = Vec::new();
        let mut cmds = Vec::new();
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
                other => panic!("unexpected topic {other}"),
            }
        }
        assert_eq!(speeds, [0.0, 0.5, 1.0, 1.5, 2.0]);
        assert_eq!(cmds, [0.0, 1.0, 2.0, 3.0, 4.0]);
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
