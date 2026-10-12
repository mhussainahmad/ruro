//! Records a group's outputs into an MCAP segment.
//!
//! Each output gets a [`RawSink`] that copies the message encoding (the same
//! bytes shared memory would carry) into a queue; one writer thread drains
//! the queue into the file, so node threads never wait for the disk. If the
//! writer falls behind by more than [`MAX_QUEUED_BYTES`], messages are
//! dropped and counted instead of growing memory without bound.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::thread::JoinHandle;

use anyhow::{Context, Result, anyhow};
use crossbeam_channel::{Sender, unbounded};
use ruro_graph::PortRef;
use ruro_node::{ErasedOutput, RawSink};
use ruro_record::McapWriter;

/// Most bytes waiting for the writer before messages are dropped.
pub const MAX_QUEUED_BYTES: usize = 512 << 20;

struct Entry {
    channel: u16,
    seq: u64,
    stamp_ns: u64,
    data: Vec<u8>,
}

struct Shared {
    queued: AtomicUsize,
    written: AtomicU64,
    dropped: AtomicU64,
}

/// An open recording of some outputs.
pub struct Recorder {
    path: PathBuf,
    tx: Option<Sender<Entry>>,
    thread: Option<JoinHandle<Result<()>>>,
    shared: Arc<Shared>,
    outputs: Vec<Arc<dyn ErasedOutput>>,
}

struct Sink {
    channel: u16,
    tx: Sender<Entry>,
    shared: Arc<Shared>,
}

impl RawSink for Sink {
    fn send(
        &self,
        seq: u64,
        stamp_ns: u64,
        len: usize,
        write: &mut dyn FnMut(&mut [u8]),
    ) -> ruro_node::Result {
        if self.shared.queued.load(Ordering::Relaxed) + len > MAX_QUEUED_BYTES {
            self.shared.dropped.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
        let mut data = vec![0; len];
        write(&mut data);
        self.shared.queued.fetch_add(len, Ordering::Relaxed);
        let entry = Entry {
            channel: self.channel,
            seq,
            stamp_ns,
            data,
        };
        if self.tx.send(entry).is_err() {
            self.shared.dropped.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }
}

impl Recorder {
    /// Starts recording every port in `outputs` into a new file at `path`.
    pub fn start<'a>(
        path: &Path,
        outputs: impl IntoIterator<Item = (&'a PortRef, &'a Arc<dyn ErasedOutput>)>,
    ) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let mut writer = McapWriter::create(path, false)?;
        let (tx, rx) = unbounded::<Entry>();
        let shared = Arc::new(Shared {
            queued: AtomicUsize::new(0),
            written: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
        });
        let mut sinks = Vec::new();
        let mut ports: Vec<_> = outputs.into_iter().collect();
        ports.sort_by(|a, b| a.0.cmp(b.0));
        for (port, out) in ports {
            let topic = ruro_record::topic(&port.node, &port.port);
            let channel = writer.channel(&topic, out.ty(), ruro_record::encoding_of(out.ty()))?;
            sinks.push((out.clone(), channel));
        }
        let worker = shared.clone();
        let thread = std::thread::Builder::new()
            .name("ruro-record".into())
            .spawn(move || -> Result<()> {
                let mut error = None;
                for e in rx {
                    worker.queued.fetch_sub(e.data.len(), Ordering::Relaxed);
                    if error.is_none() {
                        match writer.write(e.channel, e.seq, e.stamp_ns, &e.data) {
                            Ok(()) => {
                                worker.written.fetch_add(1, Ordering::Relaxed);
                            }
                            Err(err) => {
                                tracing::error!("recording failed: {err:#}");
                                error = Some(err);
                            }
                        }
                    }
                }
                match error {
                    Some(e) => Err(e),
                    None => writer.finish(),
                }
            })?;
        let outputs: Vec<_> = sinks.iter().map(|(o, _)| o.clone()).collect();
        for (out, channel) in sinks {
            out.set_recorder(Some(Arc::new(Sink {
                channel,
                tx: tx.clone(),
                shared: shared.clone(),
            })));
        }
        tracing::info!(
            "recording {} output(s) to {}",
            outputs.len(),
            path.display()
        );
        Ok(Self {
            path: path.to_owned(),
            tx: Some(tx),
            thread: Some(thread),
            shared,
            outputs,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Stops recording, writes everything queued and closes the file.
    /// Returns `(written, dropped)` message counts.
    pub fn finish(mut self) -> Result<(u64, u64)> {
        self.close()
    }

    fn close(&mut self) -> Result<(u64, u64)> {
        for out in &self.outputs {
            out.set_recorder(None);
        }
        // Dropping the sinks (with the outputs' recorders) and our sender
        // ends the writer's loop.
        self.tx.take();
        if let Some(t) = self.thread.take() {
            t.join()
                .map_err(|_| anyhow!("recording thread panicked"))??;
        }
        let written = self.shared.written.load(Ordering::Relaxed);
        let dropped = self.shared.dropped.load(Ordering::Relaxed);
        if dropped > 0 {
            tracing::warn!(
                "recording {}: dropped {dropped} message(s) because the disk could not keep up",
                self.path.display()
            );
        }
        Ok((written, dropped))
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if self.thread.is_some()
            && let Err(e) = self.close()
        {
            tracing::error!("finishing {}: {e:#}", self.path.display());
        }
    }
}
