use std::any::Any;
use std::collections::VecDeque;
use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use parking_lot::{Mutex, RwLock};
use ruro_msg::{Message, Preview, now_ns};

use crate::{Result, Waker};

/// A message plus the metadata stamped on it when it was sent.
#[derive(Debug)]
pub struct Sample<T> {
    /// Per-output sequence number, starting at 1.
    pub seq: u64,
    /// Send time in nanoseconds since the Unix epoch.
    pub stamp_ns: u64,
    pub msg: T,
}

impl<T> Deref for Sample<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.msg
    }
}

/// Counters for one input port, read by the host for metrics.
#[derive(Default, Debug)]
pub struct InputStats {
    pub received: AtomicU64,
    pub dropped: AtomicU64,
    /// Sum and count of message ages (send to receive) observed by `recv`.
    pub age_ns_sum: AtomicU64,
    pub age_count: AtomicU64,
}

impl InputStats {
    fn observe_age(&self, stamp_ns: u64) {
        let age = now_ns().saturating_sub(stamp_ns);
        self.age_ns_sum.fetch_add(age, Ordering::Relaxed);
        self.age_count.fetch_add(1, Ordering::Relaxed);
    }
}

/// Shared state behind an [`Input`]: a bounded keep-latest queue.
pub(crate) struct InputPort<T> {
    queue: Mutex<VecDeque<Arc<Sample<T>>>>,
    depth: usize,
    waker: Arc<Waker>,
    stats: InputStats,
}

impl<T: Message> InputPort<T> {
    pub(crate) fn new(depth: usize, waker: Arc<Waker>) -> Self {
        let depth = depth.max(1);
        Self {
            queue: Mutex::new(VecDeque::with_capacity(depth)),
            depth,
            waker,
            stats: InputStats::default(),
        }
    }

    fn push(&self, sample: Arc<Sample<T>>) {
        {
            let mut q = self.queue.lock();
            if q.len() == self.depth {
                q.pop_front();
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
            }
            q.push_back(sample);
        }
        self.stats.received.fetch_add(1, Ordering::Relaxed);
        self.waker.notify();
    }
}

/// Receiving end of a wire.
pub struct Input<T> {
    port: Arc<InputPort<T>>,
}

impl<T: Message> Input<T> {
    pub(crate) fn new(port: Arc<InputPort<T>>) -> Self {
        Self { port }
    }

    /// Takes the oldest queued message.
    pub fn recv(&self) -> Option<Arc<Sample<T>>> {
        let s = self.port.queue.lock().pop_front()?;
        self.port.stats.observe_age(s.stamp_ns);
        Some(s)
    }

    /// Takes the newest queued message and discards the rest.
    pub fn latest(&self) -> Option<Arc<Sample<T>>> {
        let s = {
            let mut q = self.port.queue.lock();
            let s = q.pop_back()?;
            q.clear();
            s
        };
        self.port.stats.observe_age(s.stamp_ns);
        Some(s)
    }

    pub fn is_empty(&self) -> bool {
        self.port.queue.lock().is_empty()
    }

    pub fn len(&self) -> usize {
        self.port.queue.lock().len()
    }
}

/// Destination for messages that leave the process.
///
/// `write` must be called exactly once with a buffer of `len` bytes, which the
/// sink typically loans directly from shared memory.
pub trait RawSink: Send + Sync {
    fn send(&self, seq: u64, stamp_ns: u64, len: usize, write: &mut dyn FnMut(&mut [u8]))
    -> Result;
}

/// Receives previews of an output while a tool is watching it.
pub type TapSink = Arc<dyn Fn(Preview) + Send + Sync>;

/// Rate-limited preview hook on an output port.
pub struct Tap {
    active: AtomicBool,
    last_ns: AtomicU64,
    min_interval_ns: u64,
    sink: RwLock<Option<TapSink>>,
}

impl Default for Tap {
    fn default() -> Self {
        Self {
            active: AtomicBool::new(false),
            last_ns: AtomicU64::new(0),
            min_interval_ns: 1_000_000_000 / 30,
            sink: RwLock::new(None),
        }
    }
}

impl Tap {
    /// Installs the sink that receives previews.
    pub fn set_sink(&self, sink: TapSink) {
        *self.sink.write() = Some(sink);
    }

    /// Enables or disables preview generation. Disabled taps cost one atomic load per send.
    pub fn set_active(&self, active: bool) {
        self.active.store(active, Ordering::Relaxed);
    }

    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    fn offer<T: Message>(&self, msg: &T, now: u64) {
        if !self.is_active() {
            return;
        }
        let last = self.last_ns.load(Ordering::Relaxed);
        if now.saturating_sub(last) < self.min_interval_ns
            || self
                .last_ns
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_err()
        {
            return;
        }
        if let Some(sink) = self.sink.read().as_ref() {
            sink(msg.preview());
        }
    }
}

/// Shared state behind an [`Output`].
pub(crate) struct OutputPort<T> {
    seq: AtomicU64,
    local: RwLock<Vec<Arc<InputPort<T>>>>,
    remote: RwLock<Vec<Box<dyn RawSink>>>,
    tap: Tap,
}

impl<T: Message> OutputPort<T> {
    pub(crate) fn new() -> Self {
        Self {
            seq: AtomicU64::new(0),
            local: RwLock::new(Vec::new()),
            remote: RwLock::new(Vec::new()),
            tap: Tap::default(),
        }
    }
}

/// Sending end of a wire.
pub struct Output<T> {
    port: Arc<OutputPort<T>>,
}

/// Another handle to the same output, e.g. to send from a callback that a
/// driver library runs on its own thread. Both handles share the sequence
/// counter and the connected inputs.
impl<T> Clone for Output<T> {
    fn clone(&self) -> Self {
        Self {
            port: self.port.clone(),
        }
    }
}

impl<T: Message> Output<T> {
    pub(crate) fn new(port: Arc<OutputPort<T>>) -> Self {
        Self { port }
    }

    /// Publishes a message to every connected input.
    ///
    /// In-process inputs share one allocation; out-of-process inputs receive
    /// the encoding written once into shared memory.
    pub fn send(&self, msg: T) -> Result {
        let seq = self.port.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let stamp_ns = now_ns();
        let mut result = Ok(());
        for sink in self.port.remote.read().iter() {
            let r = sink.send(seq, stamp_ns, msg.encoded_len(), &mut |buf| {
                msg.encode_into(buf)
            });
            if r.is_err() && result.is_ok() {
                result = r;
            }
        }
        self.port.tap.offer(&msg, stamp_ns);
        let local = self.port.local.read();
        if !local.is_empty() {
            let sample = Arc::new(Sample { seq, stamp_ns, msg });
            for input in local.iter() {
                input.push(sample.clone());
            }
        }
        result
    }

    /// Number of messages sent so far.
    pub fn sent(&self) -> u64 {
        self.port.seq.load(Ordering::Relaxed)
    }
}

/// Maps a port field type to its message type name. Used by `#[derive(Node)]`.
pub trait PortType {
    const TY: &'static str;
}

impl<T: Message> PortType for Input<T> {
    const TY: &'static str = T::TYPE;
}

impl<T: Message> PortType for Output<T> {
    const TY: &'static str = T::TYPE;
}

/// Type-erased view of an input port, used by hosts to wire graphs.
pub trait ErasedInput: Send + Sync {
    fn ty(&self) -> &'static str;
    fn stats(&self) -> &InputStats;
    /// Decodes a message that arrived from another process and enqueues it.
    fn push_encoded(&self, seq: u64, stamp_ns: u64, bytes: &[u8]) -> Result;
    #[doc(hidden)]
    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync>;
}

impl<T: Message> ErasedInput for InputPort<T> {
    fn ty(&self) -> &'static str {
        T::TYPE
    }

    fn stats(&self) -> &InputStats {
        &self.stats
    }

    fn push_encoded(&self, seq: u64, stamp_ns: u64, bytes: &[u8]) -> Result {
        let msg = T::decode(bytes)?;
        self.push(Arc::new(Sample { seq, stamp_ns, msg }));
        Ok(())
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

/// Type-erased view of an output port, used by hosts to wire graphs.
pub trait ErasedOutput: Send + Sync {
    fn ty(&self) -> &'static str;
    fn sent(&self) -> u64;
    fn tap(&self) -> &Tap;
    /// Connects to an input in the same process. Fails if the types differ.
    fn connect_local(&self, input: Arc<dyn ErasedInput>) -> Result;
    /// Adds a sink for inputs in other processes.
    fn add_remote(&self, sink: Box<dyn RawSink>);
}

impl<T: Message> ErasedOutput for OutputPort<T> {
    fn ty(&self) -> &'static str {
        T::TYPE
    }

    fn sent(&self) -> u64 {
        self.seq.load(Ordering::Relaxed)
    }

    fn tap(&self) -> &Tap {
        &self.tap
    }

    fn connect_local(&self, input: Arc<dyn ErasedInput>) -> Result {
        let ty = input.ty();
        let input = input
            .into_any()
            .downcast::<InputPort<T>>()
            .map_err(|_| anyhow::anyhow!("cannot connect {} output to {ty} input", T::TYPE))?;
        self.local.write().push(input);
        Ok(())
    }

    fn add_remote(&self, sink: Box<dyn RawSink>) {
        self.remote.write().push(sink);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruro_msg::{Image, Scalar};

    fn pair<T: Message>(depth: usize) -> (Output<T>, Input<T>, Arc<Waker>) {
        let waker = Arc::new(Waker::new());
        let out = Arc::new(OutputPort::<T>::new());
        let inp = Arc::new(InputPort::<T>::new(depth, waker.clone()));
        out.connect_local(inp.clone()).unwrap();
        (Output::new(out), Input::new(inp), waker)
    }

    #[test]
    fn keep_latest_queue_drops_oldest() {
        let (out, inp, waker) = pair::<Scalar>(2);
        for i in 0..5 {
            out.send(Scalar(i as f64)).unwrap();
        }
        assert_eq!(waker.generation(), 5);
        assert_eq!(inp.len(), 2);
        assert_eq!(inp.recv().unwrap().0, 3.0);
        assert_eq!(inp.recv().unwrap().seq, 5);
        assert!(inp.recv().is_none());
        assert_eq!(inp.port.stats.dropped.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn latest_drains_queue() {
        let (out, inp, _) = pair::<Scalar>(8);
        for i in 0..4 {
            out.send(Scalar(i as f64)).unwrap();
        }
        assert_eq!(inp.latest().unwrap().0, 3.0);
        assert!(inp.is_empty());
    }

    #[test]
    fn local_fanout_shares_one_allocation() {
        let waker = Arc::new(Waker::new());
        let out = Arc::new(OutputPort::<Image>::new());
        let a = Arc::new(InputPort::<Image>::new(1, waker.clone()));
        let b = Arc::new(InputPort::<Image>::new(1, waker));
        out.connect_local(a.clone()).unwrap();
        out.connect_local(b.clone()).unwrap();
        Output::new(out)
            .send(Image::new(64, 64, ruro_msg::Encoding::Rgb8))
            .unwrap();
        let sa = Input::new(a).recv().unwrap();
        let sb = Input::new(b).recv().unwrap();
        assert!(Arc::ptr_eq(&sa, &sb));
    }

    #[test]
    fn type_mismatch_is_rejected() {
        let out = OutputPort::<Scalar>::new();
        let inp = Arc::new(InputPort::<Image>::new(1, Arc::new(Waker::new())));
        assert!(out.connect_local(inp).is_err());
    }

    #[test]
    fn remote_sink_receives_encoding() {
        struct Capture(Mutex<Vec<u8>>);
        impl RawSink for Arc<Capture> {
            fn send(&self, _: u64, _: u64, len: usize, write: &mut dyn FnMut(&mut [u8])) -> Result {
                let mut buf = vec![0; len];
                write(&mut buf);
                *self.0.lock() = buf;
                Ok(())
            }
        }
        let cap = Arc::new(Capture(Mutex::new(Vec::new())));
        let port = Arc::new(OutputPort::<Scalar>::new());
        port.add_remote(Box::new(cap.clone()));
        Output::new(port).send(Scalar(2.5)).unwrap();

        let inp = Arc::new(InputPort::<Scalar>::new(1, Arc::new(Waker::new())));
        inp.push_encoded(1, now_ns(), &cap.0.lock()).unwrap();
        assert_eq!(Input::new(inp).recv().unwrap().0, 2.5);
    }

    #[test]
    fn tap_is_rate_limited() {
        let port = Arc::new(OutputPort::<Scalar>::new());
        let hits = Arc::new(AtomicU64::new(0));
        let h = hits.clone();
        port.tap.set_sink(Arc::new(move |_| {
            h.fetch_add(1, Ordering::Relaxed);
        }));
        let out = Output::new(port.clone());
        out.send(Scalar(0.0)).unwrap();
        assert_eq!(
            hits.load(Ordering::Relaxed),
            0,
            "inactive tap must not fire"
        );
        port.tap.set_active(true);
        for _ in 0..100 {
            out.send(Scalar(1.0)).unwrap();
        }
        assert_eq!(hits.load(Ordering::Relaxed), 1);
    }
}
