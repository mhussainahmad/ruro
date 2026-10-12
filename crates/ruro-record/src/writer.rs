use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use anyhow::{Context, Result};

use crate::SCHEMA_ENCODING;

/// Writes ruro channels into an MCAP file.
pub struct McapWriter {
    inner: mcap::Writer<BufWriter<File>>,
    schemas: HashMap<String, u16>,
    channels: HashMap<(String, String, String), u16>,
}

impl McapWriter {
    /// Creates `path`. `compress` enables zstd chunk compression, which
    /// shrinks images a lot but costs CPU while writing.
    pub fn create(path: &Path, compress: bool) -> Result<Self> {
        let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
        let compression = compress.then_some(mcap::Compression::Zstd);
        let inner = mcap::WriteOptions::new()
            .compression(compression)
            .profile("ruro")
            .library(concat!("ruro-record/", env!("CARGO_PKG_VERSION")))
            .create(BufWriter::with_capacity(1 << 20, file))
            .context("starting MCAP file")?;
        Ok(Self {
            inner,
            schemas: HashMap::new(),
            channels: HashMap::new(),
        })
    }

    /// Returns the id of the channel `topic` carrying message type `ty`,
    /// declaring it on first use.
    pub fn channel(&mut self, topic: &str, ty: &str, encoding: &str) -> Result<u16> {
        let key = (topic.to_owned(), ty.to_owned(), encoding.to_owned());
        if let Some(&id) = self.channels.get(&key) {
            return Ok(id);
        }
        let schema = match self.schemas.get(ty) {
            Some(&id) => id,
            None => {
                let id = self
                    .inner
                    .add_schema(ty, SCHEMA_ENCODING, ty.as_bytes())
                    .context("adding schema")?;
                self.schemas.insert(ty.to_owned(), id);
                id
            }
        };
        let id = self
            .inner
            .add_channel(schema, topic, encoding, &BTreeMap::new())
            .context("adding channel")?;
        self.channels.insert(key, id);
        Ok(id)
    }

    /// Appends one message. `log_time` is in nanoseconds since the Unix epoch.
    pub fn write(&mut self, channel: u16, seq: u64, log_time: u64, data: &[u8]) -> Result<()> {
        self.inner
            .write_to_known_channel(
                &mcap::records::MessageHeader {
                    channel_id: channel,
                    sequence: seq as u32,
                    log_time,
                    publish_time: log_time,
                },
                data,
            )
            .context("writing message")
    }

    /// Writes the summary and closes the file.
    pub fn finish(mut self) -> Result<()> {
        self.inner.finish().context("finishing MCAP file")?;
        Ok(())
    }
}
