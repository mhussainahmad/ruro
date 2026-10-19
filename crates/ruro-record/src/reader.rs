use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap, HashMap};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use memmap2::Mmap;

use crate::McapWriter;

/// An MCAP file opened for reading.
pub struct Recording {
    path: PathBuf,
    map: Mmap,
}

/// One message read from a recording.
pub struct RecordedMessage {
    pub channel: Arc<mcap::Channel<'static>>,
    pub seq: u32,
    /// Nanoseconds since the Unix epoch.
    pub log_time: u64,
    pub data: std::borrow::Cow<'static, [u8]>,
}

impl RecordedMessage {
    pub fn topic(&self) -> &str {
        &self.channel.topic
    }

    /// Schema (message type) name, or empty if the channel has no schema.
    pub fn ty(&self) -> &str {
        self.channel.schema.as_ref().map_or("", |s| s.name.as_str())
    }

    pub fn encoding(&self) -> &str {
        &self.channel.message_encoding
    }
}

/// Summary of a recording.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Info {
    pub channels: Vec<ChannelInfo>,
    pub messages: u64,
    /// Log time of the first and last message, in nanoseconds since the Unix epoch.
    pub start_ns: u64,
    pub end_ns: u64,
}

impl Info {
    pub fn duration_s(&self) -> f64 {
        self.end_ns.saturating_sub(self.start_ns) as f64 / 1e9
    }
}

/// One channel of a recording.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelInfo {
    pub id: u16,
    pub topic: String,
    /// Schema name: the ruro message type, or a ROS type for imported bags.
    pub ty: String,
    pub encoding: String,
    pub count: u64,
}

impl Recording {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        if file.metadata()?.len() == 0 {
            bail!("{} is empty", path.display());
        }
        // SAFETY: the mapping is read-only. Recordings are not modified while
        // they are read; a concurrent truncation would at worst fault this
        // process, which is the same failure mode as reading a corrupt file.
        let map =
            unsafe { Mmap::map(&file) }.with_context(|| format!("mapping {}", path.display()))?;
        Ok(Self {
            path: path.to_owned(),
            map,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn bytes(&self) -> &[u8] {
        &self.map
    }

    /// Every message in file order (time order for files written by ruro).
    pub fn messages(&self) -> Result<impl Iterator<Item = Result<RecordedMessage>> + '_> {
        let stream = mcap::MessageStream::new(&self.map)
            .with_context(|| format!("reading {}", self.path.display()))?;
        Ok(stream.map(|m| {
            let m = m.context("corrupt message")?;
            Ok(RecordedMessage {
                channel: m.channel,
                seq: m.sequence,
                log_time: m.log_time,
                data: m.data,
            })
        }))
    }

    /// Channels, counts and time range. Uses the summary section when the
    /// file has one, otherwise scans every message.
    pub fn info(&self) -> Result<Info> {
        if let Ok(Some(summary)) = mcap::Summary::read(&self.map)
            && let Some(stats) = &summary.stats
        {
            let mut channels: Vec<ChannelInfo> = summary
                .channels
                .values()
                .map(|c| channel_info(c, stats.channel_message_counts.get(&c.id).copied()))
                .collect();
            channels.sort_by(|a, b| a.topic.cmp(&b.topic));
            return Ok(Info {
                channels,
                messages: stats.message_count,
                start_ns: stats.message_start_time,
                end_ns: stats.message_end_time,
            });
        }
        let mut channels: BTreeMap<u16, ChannelInfo> = BTreeMap::new();
        let mut info = Info {
            start_ns: u64::MAX,
            ..Default::default()
        };
        for m in self.messages()? {
            let m = m?;
            channels
                .entry(m.channel.id)
                .or_insert_with(|| channel_info(&m.channel, Some(0)))
                .count += 1;
            info.messages += 1;
            info.start_ns = info.start_ns.min(m.log_time);
            info.end_ns = info.end_ns.max(m.log_time);
        }
        if info.messages == 0 {
            info.start_ns = 0;
        }
        info.channels = channels.into_values().collect();
        info.channels.sort_by(|a, b| a.topic.cmp(&b.topic));
        Ok(info)
    }
}

fn channel_info(c: &mcap::Channel, count: Option<u64>) -> ChannelInfo {
    ChannelInfo {
        id: c.id,
        topic: c.topic.clone(),
        ty: c
            .schema
            .as_ref()
            .map(|s| s.name.clone())
            .unwrap_or_default(),
        encoding: c.message_encoding.clone(),
        count: count.unwrap_or(0),
    }
}

/// Interleaves several recordings (for example one segment per host process)
/// into one file ordered by log time. Channels with the same topic, type and
/// encoding are merged.
pub fn merge(inputs: &[PathBuf], output: &Path, compress: bool) -> Result<Info> {
    let recordings: Vec<Recording> = inputs
        .iter()
        .map(|p| Recording::open(p))
        .collect::<Result<_>>()?;
    let mut streams = Vec::new();
    for r in &recordings {
        streams.push(r.messages()?);
    }
    let mut writer = McapWriter::create(output, compress)?;
    let mut ids: HashMap<(usize, u16), u16> = HashMap::new();
    let mut heads: Vec<Option<RecordedMessage>> = Vec::new();
    let mut heap = BinaryHeap::new();
    // A segment cut short (its process was killed) ends at its first bad record.
    let next = |i: usize, s: &mut dyn Iterator<Item = Result<RecordedMessage>>| match s.next() {
        Some(Ok(m)) => Some(m),
        Some(Err(e)) => {
            tracing::warn!(
                "{}: {e:#}; using the messages before it",
                inputs[i].display()
            );
            None
        }
        None => None,
    };
    for (i, s) in streams.iter_mut().enumerate() {
        let head = next(i, s);
        if let Some(m) = &head {
            heap.push(Reverse((m.log_time, i)));
        }
        heads.push(head);
    }
    while let Some(Reverse((_, i))) = heap.pop() {
        let m = heads[i].take().expect("heap entries have a head");
        let id = match ids.get(&(i, m.channel.id)) {
            Some(&id) => id,
            None => {
                let id = writer.channel_with_metadata(
                    m.topic(),
                    m.ty(),
                    m.encoding(),
                    &m.channel.metadata,
                )?;
                ids.insert((i, m.channel.id), id);
                id
            }
        };
        writer.write(id, m.seq as u64, m.log_time, &m.data)?;
        if let Some(m) = next(i, &mut streams[i]) {
            heap.push(Reverse((m.log_time, i)));
            heads[i] = Some(m);
        }
    }
    writer.finish()?;
    Recording::open(output)?.info()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ruro-record-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn write_read_and_info() {
        let path = tmp("basic.mcap");
        let mut w = McapWriter::create(&path, true).unwrap();
        let a = w
            .channel("src.value", "std/Scalar", crate::ENCODING_POSTCARD)
            .unwrap();
        let b = w
            .channel("cam.image", "std/Image", crate::ENCODING_RAW)
            .unwrap();
        assert_eq!(
            w.channel("src.value", "std/Scalar", crate::ENCODING_POSTCARD)
                .unwrap(),
            a
        );
        w.write(a, 1, 100, &[1, 2, 3]).unwrap();
        w.write(b, 1, 150, &[9; 64]).unwrap();
        w.write(a, 2, 200, &[4]).unwrap();
        w.finish().unwrap();

        let r = Recording::open(&path).unwrap();
        let msgs: Vec<_> = r.messages().unwrap().map(Result::unwrap).collect();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].topic(), "src.value");
        assert_eq!(msgs[0].ty(), "std/Scalar");
        assert_eq!(&*msgs[2].data, &[4]);
        let info = r.info().unwrap();
        assert_eq!(info.messages, 3);
        assert_eq!((info.start_ns, info.end_ns), (100, 200));
        let src = info
            .channels
            .iter()
            .find(|c| c.topic == "src.value")
            .unwrap();
        assert_eq!((src.count, src.ty.as_str()), (2, "std/Scalar"));
    }

    #[test]
    fn merge_interleaves_by_time() {
        let (p1, p2, out) = (tmp("seg1.mcap"), tmp("seg2.mcap"), tmp("merged.mcap"));
        for (path, topic, times) in [(&p1, "a.out", [10, 30, 50]), (&p2, "b.out", [20, 40, 60])] {
            let mut w = McapWriter::create(path, false).unwrap();
            let c = w
                .channel(topic, "std/Scalar", crate::ENCODING_POSTCARD)
                .unwrap();
            for (i, t) in times.into_iter().enumerate() {
                w.write(c, i as u64 + 1, t, &[i as u8]).unwrap();
            }
            w.finish().unwrap();
        }
        let info = merge(&[p1, p2], &out, true).unwrap();
        assert_eq!(info.messages, 6);
        assert_eq!(info.channels.len(), 2);
        let r = Recording::open(&out).unwrap();
        let times: Vec<u64> = r.messages().unwrap().map(|m| m.unwrap().log_time).collect();
        assert_eq!(times, [10, 20, 30, 40, 50, 60]);
    }

    #[test]
    fn empty_file_is_an_error() {
        let path = tmp("empty.mcap");
        std::fs::write(&path, b"").unwrap();
        assert!(Recording::open(&path).is_err());
        std::fs::write(&path, b"not an mcap file at all").unwrap();
        let r = Recording::open(&path).unwrap();
        assert!(r.messages().is_err() || r.messages().unwrap().any(|m| m.is_err()));
    }
}
