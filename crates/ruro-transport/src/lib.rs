//! Shared-memory transport for wires that cross process boundaries.
//!
//! Each exported output is an iceoryx2 publish-subscribe service carrying byte
//! slices, paired with an event service that wakes subscribers. A sample is a
//! 16-byte header (sequence number and send timestamp) followed by the message
//! encoding, which the sender writes directly into the loaned shared-memory
//! chunk. Queues keep the newest samples and overwrite the oldest, matching the
//! in-process keep-latest semantics.

use std::mem::MaybeUninit;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use iceoryx2::port::listener::Listener;
use iceoryx2::port::notifier::Notifier;
use iceoryx2::port::publisher::Publisher;
use iceoryx2::port::subscriber::Subscriber;
use iceoryx2::prelude::*;

type Svc = ipc_threadsafe::Service;

/// Bytes of per-sample header: `seq: u64` then `stamp_ns: u64`, little endian.
pub const HEADER_LEN: usize = 16;

/// Largest queue any subscriber of a topic may request.
const MAX_DEPTH: usize = 16;

/// Initial chunk size; chunks grow by powers of two as messages get larger.
const INITIAL_SLICE_LEN: usize = 4096;

/// iceoryx2 logs routine conditions (such as running without a config file)
/// as warnings; only errors are worth surfacing.
fn quiet() {
    iceoryx2::prelude::set_log_level(LogLevel::Error);
}

/// How long opening a service keeps retrying while another process is
/// creating or removing it.
const OPEN_RETRY: Duration = Duration::from_secs(10);

/// How often a retried open looks for resources left by dead processes.
const CLEANUP_EVERY: Duration = Duration::from_millis(250);

/// A service still being created after this long was left half-made by a
/// process that died while creating it. (iceoryx2 itself gives creation a
/// second before it reports the service as hanging.)
const HUNG_CREATION: Duration = Duration::from_secs(2);

/// Calls `open` until it succeeds, fails with a non-transient error, or
/// [`OPEN_RETRY`] passes. Processes of one graph start together, so one often
/// tries to open a service at the moment another is still creating it. While
/// retrying, resources of processes that died are removed now and then: a
/// host that was killed while creating a service leaves it hanging in
/// creation until then.
fn retry_while_in_flux<T, E>(
    mut open: impl FnMut() -> std::result::Result<T, E>,
    transient: impl Fn(&E) -> bool,
    mut cleanup: impl FnMut(),
) -> std::result::Result<T, E> {
    let start = std::time::Instant::now();
    let mut cleaned = start;
    let mut pause = Duration::from_millis(1);
    loop {
        match open() {
            Err(e) if transient(&e) && start.elapsed() < OPEN_RETRY => {
                if cleaned.elapsed() >= CLEANUP_EVERY {
                    cleanup();
                    cleaned = std::time::Instant::now();
                }
                std::thread::sleep(pause);
                pause = (pause * 2).min(Duration::from_millis(50));
            }
            result => return result,
        }
    }
}

/// Removes the records of services whose creation never finished: their
/// creator died after starting it. iceoryx2 marks a record finished by
/// making it read-only; one still writable long after it was written is
/// hung. Returns how many were removed.
fn remove_hung_services(older_than: Duration) -> usize {
    use std::os::unix::fs::PermissionsExt;
    let global = &Config::global_config().global;
    let dir = std::path::PathBuf::from(global.service_dir().to_string());
    let prefix = global.prefix.to_string();
    let suffix = global.service.static_config_storage_suffix.to_string();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(&prefix) || !name.ends_with(&suffix) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let unfinished = meta.is_file() && meta.permissions().mode() & 0o777 == 0o600;
        let old = meta
            .modified()
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age >= older_than);
        if unfinished && old && std::fs::remove_file(entry.path()).is_ok() {
            tracing::warn!(
                "removed shared-memory service {name}, left half-created by a process that died"
            );
            removed += 1;
        }
    }
    removed
}

/// A process-local handle to the shared-memory transport.
pub struct Shm {
    node: iceoryx2::node::Node<Svc>,
}

impl Shm {
    /// Opens the transport. `name` identifies this process in diagnostics.
    pub fn new(name: &str) -> Result<Self> {
        quiet();
        let node = NodeBuilder::new()
            .name(&NodeName::new(name).map_err(|e| anyhow!("invalid node name `{name}`: {e:?}"))?)
            .signal_handling_mode(SignalHandlingMode::Disabled)
            .create::<Svc>()
            .map_err(|e| anyhow!("failed to create shared-memory node: {e:?}"))?;
        Ok(Self { node })
    }

    /// Releases resources left behind by processes that crashed: their
    /// ports and nodes, and services they died while creating.
    pub fn cleanup_stale() {
        quiet();
        let _ = iceoryx2::node::Node::<Svc>::list(Config::global_config(), |state| {
            if let NodeState::Dead(view) = state {
                let _ = view.blocking_remove_stale_resources(Duration::from_millis(100));
            }
            CallbackProgression::Continue
        });
        remove_hung_services(HUNG_CREATION);
    }

    fn service_name(topic: &str) -> Result<ServiceName> {
        ServiceName::new(topic).map_err(|e| anyhow!("invalid topic `{topic}`: {e:?}"))
    }

    fn event_name(topic: &str) -> Result<ServiceName> {
        Self::service_name(&format!("{topic}/notify"))
    }

    fn data_service(
        &self,
        topic: &str,
    ) -> Result<iceoryx2::service::port_factory::publish_subscribe::PortFactory<Svc, [u8], ()>>
    {
        use iceoryx2::service::builder::publish_subscribe::{
            PublishSubscribeCreateError as C, PublishSubscribeOpenError as O,
            PublishSubscribeOpenOrCreateError as E,
        };
        let name = Self::service_name(topic)?;
        retry_while_in_flux(
            || {
                self.node
                    .service_builder(&name)
                    .publish_subscribe::<[u8]>()
                    .enable_safe_overflow(true)
                    .subscriber_max_buffer_size(MAX_DEPTH)
                    .history_size(0)
                    .max_publishers(4)
                    .max_subscribers(32)
                    .open_or_create()
            },
            |e| {
                matches!(
                    e,
                    E::SystemInFlux
                        | E::PublishSubscribeOpenError(
                            O::DoesNotExist
                                | O::HangsInCreation
                                | O::IsMarkedForDestruction
                                | O::Interrupt
                        )
                        | E::PublishSubscribeCreateError(
                            C::AlreadyExists
                                | C::IsBeingCreatedByAnotherInstance
                                | C::HangsInCreation
                                | C::Interrupt
                        )
                )
            },
            Self::cleanup_stale,
        )
        .map_err(|e| anyhow!("failed to open topic `{topic}`: {e:?}"))
    }

    fn event_service(
        &self,
        topic: &str,
    ) -> Result<iceoryx2::service::port_factory::event::PortFactory<Svc>> {
        use iceoryx2::service::builder::event::{
            EventCreateError as C, EventOpenError as O, EventOpenOrCreateError as E,
        };
        let name = Self::event_name(topic)?;
        retry_while_in_flux(
            || {
                self.node
                    .service_builder(&name)
                    .event()
                    .max_notifiers(8)
                    .max_listeners(32)
                    .open_or_create()
            },
            |e| {
                matches!(
                    e,
                    E::SystemInFlux
                        | E::EventOpenError(O::DoesNotExist | O::HangsInCreation | O::Interrupt)
                        | E::EventCreateError(
                            C::AlreadyExists | C::IsBeingCreatedByAnotherInstance | C::Interrupt
                        )
                )
            },
            Self::cleanup_stale,
        )
        .map_err(|e| anyhow!("failed to open event for `{topic}`: {e:?}"))
    }

    pub fn publisher(&self, topic: &str) -> Result<ShmPublisher> {
        let publisher = self
            .data_service(topic)?
            .publisher_builder()
            .initial_max_slice_len(INITIAL_SLICE_LEN)
            .allocation_strategy(AllocationStrategy::PowerOfTwo)
            .create()
            .map_err(|e| anyhow!("failed to create publisher on `{topic}`: {e:?}"))?;
        let notifier = self
            .event_service(topic)?
            .notifier_builder()
            .create()
            .map_err(|e| anyhow!("failed to create notifier on `{topic}`: {e:?}"))?;
        Ok(ShmPublisher {
            publisher,
            notifier,
        })
    }

    /// `depth` is the number of samples kept when the reader falls behind.
    pub fn subscriber(&self, topic: &str, depth: usize) -> Result<ShmSubscriber> {
        let subscriber = self
            .data_service(topic)?
            .subscriber_builder()
            .buffer_size(depth.clamp(1, MAX_DEPTH))
            .create()
            .map_err(|e| anyhow!("failed to create subscriber on `{topic}`: {e:?}"))?;
        let listener = self
            .event_service(topic)?
            .listener_builder()
            .create()
            .map_err(|e| anyhow!("failed to create listener on `{topic}`: {e:?}"))?;
        Ok(ShmSubscriber {
            subscriber,
            listener,
        })
    }
}

/// Writing end of a shared-memory topic.
pub struct ShmPublisher {
    publisher: Publisher<Svc, [u8], ()>,
    notifier: Notifier<Svc>,
}

impl ShmPublisher {
    /// Loans a chunk of `HEADER_LEN + len` bytes, lets `write` fill the body
    /// in place, publishes it and wakes subscribers.
    pub fn send(
        &self,
        seq: u64,
        stamp_ns: u64,
        len: usize,
        write: &mut dyn FnMut(&mut [u8]),
    ) -> Result<()> {
        let mut sample = self
            .publisher
            .loan_slice_uninit(HEADER_LEN + len)
            .map_err(|e| anyhow!("loan failed: {e:?}"))?;
        let chunk: &mut [MaybeUninit<u8>] = sample.payload_mut();
        // SAFETY: the data segment is a kernel-zeroed shared-memory mapping, so
        // every byte of a chunk is initialized; `MaybeUninit<u8>` and `u8` have
        // the same layout. The whole slice is overwritten below before sending.
        let bytes = unsafe { &mut *(chunk as *mut [MaybeUninit<u8>] as *mut [u8]) };
        bytes[..8].copy_from_slice(&seq.to_le_bytes());
        bytes[8..HEADER_LEN].copy_from_slice(&stamp_ns.to_le_bytes());
        write(&mut bytes[HEADER_LEN..]);
        // SAFETY: fully written above.
        let sample = unsafe { sample.assume_init() };
        sample.send().map_err(|e| anyhow!("send failed: {e:?}"))?;
        self.notifier
            .notify()
            .map_err(|e| anyhow!("notify failed: {e:?}"))?;
        Ok(())
    }
}

/// Reading end of a shared-memory topic.
pub struct ShmSubscriber {
    subscriber: Subscriber<Svc, [u8], ()>,
    listener: Listener<Svc>,
}

impl ShmSubscriber {
    /// Blocks until a publisher signals new data or `timeout` passes.
    pub fn wait(&self, timeout: Duration) -> Result<()> {
        self.listener
            .timed_wait(|_| {}, timeout)
            .map_err(|e| anyhow!("wait failed: {e:?}"))?;
        Ok(())
    }

    /// Hands every pending sample to `f` as `(seq, stamp_ns, body)`, oldest first.
    pub fn drain(&self, mut f: impl FnMut(u64, u64, &[u8])) -> Result<usize> {
        let mut n = 0;
        while let Some(sample) = self
            .subscriber
            .receive()
            .map_err(|e| anyhow!("receive failed: {e:?}"))?
        {
            let bytes = sample.payload();
            let (header, body) = bytes
                .split_at_checked(HEADER_LEN)
                .context("sample shorter than header")?;
            let seq = u64::from_le_bytes(header[..8].try_into().unwrap());
            let stamp = u64::from_le_bytes(header[8..].try_into().unwrap());
            f(seq, stamp, body);
            n += 1;
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_open_failures_are_retried_and_others_are_not() {
        let mut calls = 0;
        let r: std::result::Result<u8, &str> = retry_while_in_flux(
            || {
                calls += 1;
                if calls < 4 { Err("in flux") } else { Ok(7) }
            },
            |e| *e == "in flux",
            || {},
        );
        assert_eq!((r, calls), (Ok(7), 4));

        let mut calls = 0;
        let r: std::result::Result<u8, &str> = retry_while_in_flux(
            || {
                calls += 1;
                Err("incompatible")
            },
            |e| *e == "in flux",
            || {},
        );
        assert_eq!((r, calls), (Err("incompatible"), 1));
    }

    #[test]
    fn processes_opening_a_topic_at_once_all_succeed() {
        let topic = format!("ruro-test/{}/race", std::process::id());
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let topic = topic.clone();
                std::thread::spawn(move || {
                    let shm = Shm::new(&format!("ruro-race-{i}")).unwrap();
                    if i % 2 == 0 {
                        shm.publisher(&topic).map(|_| ())
                    } else {
                        shm.subscriber(&topic, 1).map(|_| ())
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap().unwrap();
        }
    }

    /// Run in a child process: starts creating many topics and is killed
    /// meanwhile, so one is likely left half-created.
    #[test]
    fn creates_topics_until_killed() {
        if let Ok(topic) = std::env::var("RURO_TEST_CREATOR") {
            let shm = Shm::new("ruro-test-creator").unwrap();
            for i in 0..10_000 {
                let _p = shm.publisher(&format!("{topic}/{i}")).unwrap();
            }
        }
    }

    /// Service records of this machine's iceoryx2 that are still being
    /// created.
    fn unfinished_services() -> std::collections::BTreeSet<std::ffi::OsString> {
        use std::os::unix::fs::PermissionsExt;
        let dir = Config::global_config().global.service_dir().to_string();
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| {
                e.metadata()
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o777 == 0o600)
            })
            .map(|e| e.file_name())
            .collect()
    }

    #[test]
    fn topics_a_killed_process_was_creating_open_quickly() {
        let before = unfinished_services();
        // Kill processes busy creating topics until one leaves a topic
        // half-created (most do).
        let mut hung = None;
        for attempt in 0..40 {
            let topic = format!("ruro-test/{}/killed{attempt}", std::process::id());
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "tests::creates_topics_until_killed"])
                .env("RURO_TEST_CREATOR", &topic)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            std::thread::sleep(Duration::from_millis(250 + 37 * (attempt % 12)));
            child.kill().unwrap();
            child.wait().unwrap();
            if !unfinished_services().is_subset(&before) {
                hung = Some(topic);
                break;
            }
        }
        let topic = hung.expect("no process died while creating a topic");
        let shm = Shm::new("ruro-test-after").unwrap();
        // The topics it created, the one it was creating and some after.
        // Opening the half-created one waits for it to be recognised as
        // hung, not for the whole retry period.
        for i in 0..30 {
            let start = std::time::Instant::now();
            shm.subscriber(&format!("{topic}/{i}"), 1).unwrap();
            let took = start.elapsed();
            assert!(took < Duration::from_secs(6), "topic {i} took {took:?}");
        }
    }

    fn topic(name: &str) -> String {
        format!("ruro-test/{}/{name}", std::process::id())
    }

    #[test]
    fn roundtrip_with_header() {
        let shm = Shm::new("ruro-test").unwrap();
        let t = topic("roundtrip");
        let sub = shm.subscriber(&t, 4).unwrap();
        let publ = shm.publisher(&t).unwrap();
        publ.send(7, 99, 5, &mut |b| b.copy_from_slice(b"hello"))
            .unwrap();
        sub.wait(Duration::from_secs(1)).unwrap();
        let mut got = Vec::new();
        sub.drain(|seq, stamp, body| got.push((seq, stamp, body.to_vec())))
            .unwrap();
        assert_eq!(got, vec![(7, 99, b"hello".to_vec())]);
    }

    #[test]
    fn grows_for_large_messages() {
        let shm = Shm::new("ruro-test").unwrap();
        let t = topic("large");
        let sub = shm.subscriber(&t, 1).unwrap();
        let publ = shm.publisher(&t).unwrap();
        let big = vec![42u8; 1920 * 1080 * 3];
        publ.send(1, 0, big.len(), &mut |b| b.copy_from_slice(&big))
            .unwrap();
        let mut len = 0;
        sub.drain(|_, _, body| len = body.len()).unwrap();
        assert_eq!(len, big.len());
    }

    #[test]
    fn overflow_keeps_newest() {
        let shm = Shm::new("ruro-test").unwrap();
        let t = topic("overflow");
        let sub = shm.subscriber(&t, 2).unwrap();
        let publ = shm.publisher(&t).unwrap();
        for i in 1..=5u64 {
            publ.send(i, 0, 0, &mut |_| {}).unwrap();
        }
        let mut seqs = Vec::new();
        sub.drain(|seq, _, _| seqs.push(seq)).unwrap();
        assert_eq!(seqs, vec![4, 5]);
    }
}
