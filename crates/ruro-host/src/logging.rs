//! Log routing: everything goes to stderr, and lines at info and above are
//! also published so tools can show them next to the graph.

use std::fmt::Write as _;

use crossbeam_channel::{Sender, bounded};
use ruro_proto::{Keys, Level, LogLine};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};
use zenoh::Wait;

struct Publish {
    group: String,
    tx: Sender<LogLine>,
}

#[derive(Default)]
struct Message(String);

impl Visit for Message {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0.push_str(value);
        } else {
            let _ = write!(self.0, " {}={value}", field.name());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.0, "{value:?}");
        } else {
            let _ = write!(self.0, " {}={value:?}", field.name());
        }
    }
}

impl<S: Subscriber> Layer<S> for Publish {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let level = match *event.metadata().level() {
            tracing::Level::ERROR => Level::Error,
            tracing::Level::WARN => Level::Warn,
            tracing::Level::INFO => Level::Info,
            tracing::Level::DEBUG => Level::Debug,
            tracing::Level::TRACE => Level::Trace,
        };
        if level < Level::Info
            || !event.metadata().target().starts_with("ruro") && level < Level::Warn
        {
            return;
        }
        let mut msg = Message::default();
        event.record(&mut msg);
        let node = std::thread::current()
            .name()
            .and_then(|n| n.strip_prefix("ruro:"))
            .map(str::to_owned);
        let _ = self.tx.try_send(LogLine {
            ts_ns: ruro_msg::now_ns(),
            level,
            group: self.group.clone(),
            node,
            message: msg.0,
        });
    }
}

/// Installs the global subscriber. Must be called once, before nodes start.
pub fn init(session: &zenoh::Session, keys: &Keys, group: &str) {
    let (tx, rx) = bounded::<LogLine>(1024);
    let key = keys.log(group);
    let session = session.clone();
    std::thread::Builder::new()
        .name("ruro-log".into())
        .spawn(move || {
            while let Ok(line) = rx.recv() {
                let _ = session.put(&key, ruro_proto::encode(&line)).wait();
            }
        })
        .expect("spawn log thread");

    let filter =
        EnvFilter::try_from_env("RURO_LOG").unwrap_or_else(|_| EnvFilter::new("info,zenoh=warn"));
    let stderr = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .with_target(false)
        .with_thread_names(true)
        .compact();
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(stderr)
        .with(Publish {
            group: group.to_owned(),
            tx,
        })
        .try_init();
}
