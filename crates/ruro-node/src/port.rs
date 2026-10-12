use std::any::Any;
use std::collections::VecDeque;
use std::ops::{Deref, DerefMut};
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

/// A set of reusable samples.
///
/// The pool keeps a reference to every sample it hands out. A sample is free
/// again once every other reference (input queues, nodes holding it) has been
/// dropped, which shows as a strong count of one. Reusing it then needs no
/// allocation, and its message keeps whatever buffers it owns.
struct SamplePool<T> {
    slots: Mutex<Vec<Arc<Sample<T>>>>,
    cap: usize,
}

impl<T> SamplePool<T> {
    fn new(cap: usize) -> Self {
        Self {
            slots: Mutex::new(Vec::with_capacity(cap)),
            cap,
        }
    }

    /// Takes a sample nobody else references, if there is one.
    fn take_free(&self) -> Option<Arc<Sample<T>>> {
        let mut slots = self.slots.lock();
        let i = slots.iter().position(|s| Arc::strong_count(s) == 1)?;
        Some(slots.swap_remove(i))
    }

    /// Keeps a reference to `sample` for later reuse, if there is room.
    fn keep(&self, sample: &Arc<Sample<T>>) {
        let mut slots = self.slots.lock();
        if slots.len() < self.cap {
            slots.push(sample.clone());
        }
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
    /// Samples decoded from other processes, reused once the node drops them.
    recycled: SamplePool<T>,
}

impl<T: Message> InputPort<T> {
    pub(crate) fn new(depth: usize, waker: Arc<Waker>) -> Self {
        let depth = depth.max(1);
        Self {
            queue: Mutex::new(VecDeque::with_capacity(depth)),
            depth,
            waker,
            stats: InputStats::default(),
            // The queue, one sample held by the node and one being decoded.
            recycled: SamplePool::new(depth + 2),
        }
    }

    fn push(&self, sample: Arc<Sample<T>>) {
        let evicted = {
            let mut q = self.queue.lock();
            let evicted = if q.len() == self.depth {
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
                q.pop_front()
            } else {
                None
            };
            q.push_back(sample);
            evicted
        };
        // Dropped outside the lock: the last reference may free a large message.
        drop(evicted);
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

/// Samples an output keeps for [`Output::loan`]. Enough for a frame in each
/// of a few depth-1 queues, one held by each consumer and one being written.
const LOAN_POOL: usize = 6;

/// Shared state behind an [`Output`].
pub(crate) struct OutputPort<T> {
    seq: AtomicU64,
    /// Messages handed to destinations: sent by the node or injected.
    delivered: AtomicU64,
    local: RwLock<Vec<Arc<InputPort<T>>>>,
    remote: RwLock<Vec<Box<dyn RawSink>>>,
    tap: Tap,
    pool: SamplePool<T>,
    recording: AtomicBool,
    recorder: RwLock<Option<Arc<dyn RawSink>>>,
}

impl<T: Message> OutputPort<T> {
    pub(crate) fn new() -> Self {
        Self {
            seq: AtomicU64::new(0),
            delivered: AtomicU64::new(0),
            local: RwLock::new(Vec::new()),
            remote: RwLock::new(Vec::new()),
            tap: Tap::default(),
            pool: SamplePool::new(LOAN_POOL),
            recording: AtomicBool::new(false),
            recorder: RwLock::new(None),
        }
    }

    /// Hands a stamped sample to every destination. Remote sinks and the
    /// recorder get `raw` if given (an encoding received from elsewhere),
    /// otherwise the message is encoded for them, and only if there are any.
    fn distribute(&self, sample: Arc<Sample<T>>, raw: Option<&[u8]>) -> Result {
        self.delivered.fetch_add(1, Ordering::Relaxed);
        let (seq, stamp_ns) = (sample.seq, sample.stamp_ns);
        let mut result = Ok(());
        let remote = self.remote.read();
        let recording = self.recording.load(Ordering::Relaxed);
        if !remote.is_empty() || recording {
            let msg = &sample.msg;
            // Sizing can cost as much as encoding (postcard sizes by
            // serializing), so it is only done here.
            let len = raw.map_or_else(|| msg.encoded_len(), <[u8]>::len);
            let mut encode = |buf: &mut [u8]| match raw {
                Some(bytes) => buf.copy_from_slice(bytes),
                None => msg.encode_into(buf),
            };
            for sink in remote.iter() {
                let r = sink.send(seq, stamp_ns, len, &mut encode);
                if r.is_err() && result.is_ok() {
                    result = r;
                }
            }
            if recording && let Some(rec) = self.recorder.read().as_ref() {
                // A full or failed recording must never fail the node's send.
                let _ = rec.send(seq, stamp_ns, len, &mut encode);
            }
        }
        drop(remote);
        self.tap.offer(&sample.msg, stamp_ns);
        let local = self.local.read();
        if let [rest @ .., last] = local.as_slice() {
            for input in rest {
                input.push(sample.clone());
            }
            last.push(sample);
        }
        result
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
        let sample = Arc::new(Sample {
            seq: 0,
            stamp_ns: 0,
            msg,
        });
        self.publish(sample, false)
    }

    /// Lends a message from this output's pool to fill in place, then
    /// publish with [`Loan::send`].
    ///
    /// The message is one sent earlier that every receiver has dropped since,
    /// with its buffers still allocated (or `T::default()` while the pool
    /// warms up). Overwrite every field you care about; for images,
    /// [`ruro_msg::Image::reshape`] keeps the pixel buffer. Once warm, a loop
    /// of `loan` and `send` does not allocate.
    pub fn loan(&self) -> Loan<'_, T>
    where
        T: Default,
    {
        let sample = self.port.pool.take_free().unwrap_or_else(|| {
            Arc::new(Sample {
                seq: 0,
                stamp_ns: 0,
                msg: T::default(),
            })
        });
        Loan { out: self, sample }
    }

    /// Stamps a sample nobody else holds yet and hands it to every
    /// destination. Pooled samples are kept for reuse by [`Output::loan`].
    fn publish(&self, mut sample: Arc<Sample<T>>, pooled: bool) -> Result {
        let seq = self.port.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let s = Arc::get_mut(&mut sample).expect("an unsent sample is not shared");
        s.seq = seq;
        s.stamp_ns = now_ns();
        if pooled {
            self.port.pool.keep(&sample);
        }
        self.port.distribute(sample, None)
    }

    /// Number of messages sent so far.
    pub fn sent(&self) -> u64 {
        self.port.seq.load(Ordering::Relaxed)
    }
}

/// A pooled message lent by [`Output::loan`]. Fill it through `DerefMut`,
/// then call [`Loan::send`]. A loan dropped without sending is discarded.
pub struct Loan<'a, T: Message> {
    out: &'a Output<T>,
    sample: Arc<Sample<T>>,
}

impl<T: Message> Loan<'_, T> {
    /// Publishes the message to every connected input.
    pub fn send(self) -> Result {
        self.out.publish(self.sample, true)
    }
}

impl<T: Message> Deref for Loan<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.sample.msg
    }
}

impl<T: Message> DerefMut for Loan<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut Arc::get_mut(&mut self.sample)
            .expect("a loaned sample is not shared")
            .msg
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
        let sample = match self.recycled.take_free() {
            Some(mut sample) => {
                let s = Arc::get_mut(&mut sample).expect("a free sample is not shared");
                s.msg.decode_into(bytes)?;
                s.seq = seq;
                s.stamp_ns = stamp_ns;
                sample
            }
            None => Arc::new(Sample {
                seq,
                stamp_ns,
                msg: T::decode(bytes)?,
            }),
        };
        self.recycled.keep(&sample);
        self.push(sample);
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
    /// Messages published so far, sent by the node or injected (replayed).
    fn delivered(&self) -> u64 {
        self.sent()
    }
    fn tap(&self) -> &Tap;
    /// Connects to an input in the same process. Fails if the types differ.
    fn connect_local(&self, input: Arc<dyn ErasedInput>) -> Result;
    /// Adds a sink for inputs in other processes.
    fn add_remote(&self, sink: Box<dyn RawSink>);
    /// Starts (`Some`) or stops (`None`) copying every sent message's
    /// encoding into `sink`. Failures of the recorder never fail a send.
    fn set_recorder(&self, _sink: Option<Arc<dyn RawSink>>) {}
    /// Publishes an encoded message as if the node had sent it: decoded once
    /// for in-process inputs, passed through as bytes to other processes.
    /// Used to replay recordings in place of the node.
    fn inject_encoded(&self, _seq: u64, _stamp_ns: u64, _bytes: &[u8]) -> Result {
        anyhow::bail!("this output does not support injection")
    }
}

impl<T: Message> ErasedOutput for OutputPort<T> {
    fn ty(&self) -> &'static str {
        T::TYPE
    }

    fn sent(&self) -> u64 {
        self.seq.load(Ordering::Relaxed)
    }

    fn delivered(&self) -> u64 {
        self.delivered.load(Ordering::Relaxed)
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

    fn set_recorder(&self, sink: Option<Arc<dyn RawSink>>) {
        let on = sink.is_some();
        *self.recorder.write() = sink;
        self.recording.store(on, Ordering::Relaxed);
    }

    fn inject_encoded(&self, seq: u64, stamp_ns: u64, bytes: &[u8]) -> Result {
        let sample = Arc::new(Sample {
            seq,
            stamp_ns,
            msg: T::decode(bytes)?,
        });
        self.distribute(sample, Some(bytes))
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

    struct Capture(Mutex<Vec<(u64, Vec<u8>)>>);
    impl RawSink for Arc<Capture> {
        fn send(&self, seq: u64, _: u64, len: usize, write: &mut dyn FnMut(&mut [u8])) -> Result {
            let mut buf = vec![0; len];
            write(&mut buf);
            self.0.lock().push((seq, buf));
            Ok(())
        }
    }

    fn capture() -> Arc<Capture> {
        Arc::new(Capture(Mutex::new(Vec::new())))
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
        let cap = capture();
        let port = Arc::new(OutputPort::<Scalar>::new());
        port.add_remote(Box::new(cap.clone()));
        Output::new(port).send(Scalar(2.5)).unwrap();

        let inp = Arc::new(InputPort::<Scalar>::new(1, Arc::new(Waker::new())));
        inp.push_encoded(1, now_ns(), &cap.0.lock()[0].1).unwrap();
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

    #[test]
    fn loans_reuse_buffers_once_receivers_drop_them() {
        let (out, inp, _) = pair::<Image>(1);
        let mut first = out.loan();
        first.reshape(32, 16, ruro_msg::Encoding::Rgb8);
        first.data[0] = 7;
        let ptr = first.data.as_ptr();
        first.send().unwrap();

        // Still held by the input queue: the next loan must be a fresh one.
        let held = out.loan();
        assert!(held.data.is_empty());
        drop(held);

        let got = inp.recv().unwrap();
        assert_eq!((got.width, got.data[0], got.seq), (32, 7, 1));
        drop(got);
        let mut again = out.loan();
        assert_eq!(again.data.as_ptr(), ptr, "the released frame is reused");
        again.data[0] = 9;
        again.send().unwrap();
        let got = inp.recv().unwrap();
        assert_eq!((got.data[0], got.seq), (9, 2));
    }

    #[test]
    fn received_samples_are_recycled() {
        let inp = Arc::new(InputPort::<Image>::new(1, Arc::new(Waker::new())));
        let input = Input::new(inp.clone());
        let bytes = Image::new(8, 8, ruro_msg::Encoding::Mono8).to_bytes();
        inp.push_encoded(1, 0, &bytes).unwrap();
        let first = input.recv().unwrap();
        let ptr = first.data.as_ptr();
        drop(first);
        inp.push_encoded(2, 0, &bytes).unwrap();
        let second = input.recv().unwrap();
        assert_eq!(second.seq, 2);
        assert_eq!(second.data.as_ptr(), ptr);
        // A bad message is rejected without poisoning the pool.
        assert!(inp.push_encoded(3, 0, &bytes[..5]).is_err());
        drop(second);
        inp.push_encoded(4, 0, &bytes).unwrap();
        assert_eq!(input.recv().unwrap().seq, 4);
    }

    #[test]
    fn recorder_sees_every_send_until_removed() {
        let (out, _inp, _) = pair::<Scalar>(1);
        let rec = capture();
        out.port.set_recorder(Some(Arc::new(rec.clone())));
        out.send(Scalar(1.0)).unwrap();
        out.loan().send().unwrap();
        out.port.set_recorder(None);
        out.send(Scalar(3.0)).unwrap();
        let got = rec.0.lock();
        assert_eq!(got.len(), 2);
        assert_eq!(Scalar::decode(&got[0].1).unwrap(), Scalar(1.0));
        assert_eq!(
            (got[1].0, Scalar::decode(&got[1].1).unwrap()),
            (2, Scalar(0.0))
        );
    }

    #[test]
    fn injection_reaches_local_and_remote_inputs() {
        let (out, inp, _) = pair::<Scalar>(4);
        let remote = capture();
        out.port.add_remote(Box::new(remote.clone()));
        let bytes = Scalar(4.0).to_bytes();
        out.port.inject_encoded(17, 5, &bytes).unwrap();
        let got = inp.recv().unwrap();
        assert_eq!((got.seq, got.stamp_ns, got.0), (17, 5, 4.0));
        assert_eq!(remote.0.lock()[0], (17, bytes.clone()));
        assert!(out.port.inject_encoded(18, 5, &[1, 2, 3]).is_err());
        assert_eq!(out.sent(), 0, "injected messages are not the node's");
        assert_eq!(ErasedOutput::delivered(&*out.port), 1);
        out.send(Scalar(1.0)).unwrap();
        out.clone().loan().send().unwrap();
        assert_eq!(ErasedOutput::delivered(&*out.port), 3);
    }
}
