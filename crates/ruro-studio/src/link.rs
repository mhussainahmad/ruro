//! Connection to a daemon, run on a background thread so the UI never blocks.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};
use ruro_graph::Registry;
use ruro_proto::{
    Ctl, DaemonStatus, GroupMetrics, Keys, LogLine, PlaybackCommand, Security, SetParam, TapFrame,
};
use zenoh::Wait;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub enum Command {
    Deploy(String),
    Stop,
    Kill(String),
    SetParam {
        node: String,
        name: String,
        value: String,
    },
    Watch {
        node: String,
        port: String,
    },
    Unwatch {
        node: String,
        port: String,
    },
    FetchGraph,
    /// Start recording to a file on the daemon, or stop with an empty path.
    Record(String),
    Playback(PlaybackCommand),
    /// Talk to another daemon reachable through the same endpoint.
    SwitchDaemon(String),
}

pub enum Event {
    Connected {
        daemon: String,
        endpoint: String,
    },
    Offline(String),
    Registry(Registry),
    Status(DaemonStatus),
    Metrics(GroupMetrics),
    Tap {
        node: String,
        port: String,
        frame: TapFrame,
    },
    Log(LogLine),
    /// Outcome of a control request.
    Reply {
        op: &'static str,
        result: Result<String, String>,
    },
    /// YAML of the graph currently running on the daemon.
    RunningGraph(String),
    /// Status of any daemon on the network, for the fleet view.
    Fleet(DaemonStatus),
}

pub struct Link {
    tx: Sender<Command>,
    pub events: Receiver<Event>,
    /// What a disabled link was asked to send, for tests.
    #[cfg(test)]
    pub sent: Option<Receiver<Command>>,
}

impl Link {
    pub fn start(
        endpoint: String,
        daemon: Option<String>,
        security: Security,
        spawn_daemon: bool,
        ctx: egui::Context,
    ) -> Self {
        let (tx, commands) = unbounded();
        let (events_tx, events) = unbounded();
        let emit = Emitter { tx: events_tx, ctx };
        std::thread::Builder::new()
            .name("ruro-link".into())
            .spawn(move || run(endpoint, daemon, security, spawn_daemon, commands, emit))
            .expect("spawn link thread");
        Self {
            tx,
            events,
            #[cfg(test)]
            sent: None,
        }
    }

    /// A link that never connects, for tests and offline editing.
    pub fn disabled() -> Self {
        let (tx, _sent) = unbounded();
        let (_, events) = unbounded();
        Self {
            tx,
            events,
            #[cfg(test)]
            sent: Some(_sent),
        }
    }

    /// Commands sent through a disabled link since the last call.
    #[cfg(test)]
    pub fn take_sent(&self) -> Vec<Command> {
        self.sent
            .as_ref()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default()
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }
}

#[derive(Clone)]
struct Emitter {
    tx: Sender<Event>,
    ctx: egui::Context,
}

impl Emitter {
    fn emit(&self, e: Event) {
        let _ = self.tx.send(e);
        self.ctx.request_repaint();
    }
}

/// Why a session ended.
enum Exit {
    /// The daemon stopped answering.
    Lost,
    /// The user picked another daemon.
    Switch(String),
    /// The UI is gone.
    Quit,
}

fn run(
    endpoint: String,
    mut daemon: Option<String>,
    security: Security,
    spawn_daemon: bool,
    commands: Receiver<Command>,
    emit: Emitter,
) {
    let mut spawned: Option<std::process::Child> = None;
    let mut announced_offline = false;
    loop {
        let session = match ruro_proto::connect_with(&endpoint, &security) {
            Ok(s) => s,
            Err(e) => {
                if spawn_daemon && spawned.is_none() && is_local(&endpoint) {
                    spawned = start_local_daemon(&endpoint, security.token.as_deref());
                    std::thread::sleep(Duration::from_millis(300));
                    continue;
                }
                if !announced_offline {
                    emit.emit(Event::Offline(format!("{e:#}")));
                    announced_offline = true;
                }
                if let Some(name) = drain_while_offline(&commands, &emit) {
                    daemon = Some(name);
                }
                continue;
            }
        };
        let keys = match ruro_proto::discover(&session, daemon.as_deref(), Duration::from_secs(2)) {
            Ok(k) => k,
            Err(e) => {
                emit.emit(Event::Offline(format!("{e:#}")));
                announced_offline = true;
                if let Some(name) = drain_while_offline(&commands, &emit) {
                    daemon = Some(name);
                }
                continue;
            }
        };
        emit.emit(Event::Connected {
            daemon: keys.daemon().to_owned(),
            endpoint: endpoint.clone(),
        });
        match serve(&session, &keys, &commands, &emit) {
            Exit::Quit => break,
            Exit::Switch(name) => {
                daemon = Some(name);
                announced_offline = false;
            }
            Exit::Lost => {
                emit.emit(Event::Offline("lost connection to daemon".into()));
                announced_offline = true;
            }
        }
    }
    if let Some(mut child) = spawned {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Serves one session until the daemon disappears, the user switches to
/// another daemon or the UI quits.
fn serve(
    session: &zenoh::Session,
    keys: &Keys,
    commands: &Receiver<Command>,
    emit: &Emitter,
) -> Exit {
    let last_status = std::sync::Arc::new(heartbeat::Heartbeat::now());

    let e = emit.clone();
    let seen = last_status.clone();
    let _status = session
        .declare_subscriber(keys.status())
        .callback(move |s| {
            if let Ok(st) = ruro_proto::decode::<DaemonStatus>(&s.payload().to_bytes()) {
                seen.touch();
                e.emit(Event::Status(st));
            }
        })
        .wait();
    let e = emit.clone();
    let _metrics = session
        .declare_subscriber(keys.all_metrics())
        .callback(move |s| {
            if let Ok(m) = ruro_proto::decode::<GroupMetrics>(&s.payload().to_bytes()) {
                e.emit(Event::Metrics(m));
            }
        })
        .wait();
    // Every daemon on the network, for the fleet view.
    let e = emit.clone();
    let _fleet = session
        .declare_subscriber(Keys::all_status())
        .callback(move |s| {
            if let Ok(st) = ruro_proto::decode::<DaemonStatus>(&s.payload().to_bytes()) {
                e.emit(Event::Fleet(st));
            }
        })
        .wait();
    let e = emit.clone();
    let _logs = session
        .declare_subscriber(keys.all_logs())
        .callback(move |s| {
            if let Ok(l) = ruro_proto::decode::<LogLine>(&s.payload().to_bytes()) {
                e.emit(Event::Log(l));
            }
        })
        .wait();

    match ruro_proto::request(session, keys, Ctl::Registry, "", REQUEST_TIMEOUT)
        .and_then(|body| Ok(serde_json::from_str::<Registry>(&body)?))
    {
        Ok(r) => emit.emit(Event::Registry(r)),
        Err(err) => emit.emit(Event::Reply {
            op: "registry",
            result: Err(format!("{err:#}")),
        }),
    }

    let mut taps: HashMap<(String, String), zenoh::pubsub::Subscriber<()>> = HashMap::new();
    loop {
        match commands.recv_timeout(Duration::from_millis(250)) {
            Ok(Command::SwitchDaemon(name)) => {
                if name != keys.daemon() {
                    return Exit::Switch(name);
                }
            }
            Ok(cmd) => handle(session, keys, cmd, &mut taps, emit),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return Exit::Quit,
        }
        if last_status.elapsed() > Duration::from_secs(3) {
            return Exit::Lost;
        }
    }
}

fn handle(
    session: &zenoh::Session,
    keys: &Keys,
    cmd: Command,
    taps: &mut HashMap<(String, String), zenoh::pubsub::Subscriber<()>>,
    emit: &Emitter,
) {
    let request = |op: Ctl, payload: &str| {
        ruro_proto::request(session, keys, op, payload, REQUEST_TIMEOUT)
            .map_err(|e| format!("{e:#}"))
    };
    match cmd {
        Command::Deploy(yaml) => emit.emit(Event::Reply {
            op: "deploy",
            result: request(Ctl::Deploy, &yaml),
        }),
        Command::Stop => emit.emit(Event::Reply {
            op: "stop",
            result: request(Ctl::Stop, ""),
        }),
        Command::Kill(group) => emit.emit(Event::Reply {
            op: "kill",
            result: request(Ctl::Kill, &group),
        }),
        Command::FetchGraph => match request(Ctl::Graph, "") {
            Ok(yaml) if !yaml.trim().is_empty() => emit.emit(Event::RunningGraph(yaml)),
            Ok(_) => {}
            Err(e) => emit.emit(Event::Reply {
                op: "graph",
                result: Err(e),
            }),
        },
        Command::SetParam { node, name, value } => {
            let msg = SetParam { name, value };
            let _ = session
                .put(keys.param(&node), ruro_proto::encode(&msg))
                .wait();
        }
        Command::Watch { node, port } => {
            let e = emit.clone();
            let (n, p) = (node.clone(), port.clone());
            if let Ok(sub) = session
                .declare_subscriber(keys.tap(&node, &port))
                .callback(move |s| {
                    if let Ok(frame) = ruro_proto::decode::<TapFrame>(&s.payload().to_bytes()) {
                        e.emit(Event::Tap {
                            node: n.clone(),
                            port: p.clone(),
                            frame,
                        });
                    }
                })
                .wait()
            {
                taps.insert((node, port), sub);
            }
        }
        Command::Record(path) => emit.emit(Event::Reply {
            op: record_op(&path),
            result: request(Ctl::Record, &path),
        }),
        Command::Playback(c) => emit.emit(Event::Reply {
            op: "playback",
            result: serde_json::to_string(&c)
                .map_err(|e| e.to_string())
                .and_then(|json| request(Ctl::Playback, &json)),
        }),
        Command::SwitchDaemon(_) => {}
        Command::Unwatch { node, port } => {
            taps.remove(&(node, port));
        }
    }
}

/// Answers commands while offline. Returns a daemon the user switched to, if any.
fn drain_while_offline(commands: &Receiver<Command>, emit: &Emitter) -> Option<String> {
    let until = Instant::now() + Duration::from_secs(1);
    let mut switch = None;
    while let Ok(cmd) = commands.recv_deadline(until) {
        let op = match cmd {
            Command::Deploy(_) => "deploy",
            Command::Stop => "stop",
            Command::Kill(_) => "kill",
            Command::Record(path) => record_op(&path),
            Command::Playback(_) => "playback",
            Command::SwitchDaemon(name) => {
                switch = Some(name);
                continue;
            }
            _ => continue,
        };
        emit.emit(Event::Reply {
            op,
            result: Err("not connected to a daemon".into()),
        });
    }
    switch
}

/// Reply operation of a record request: an empty path stops recording.
fn record_op(path: &str) -> &'static str {
    if path.is_empty() {
        "stop recording"
    } else {
        "record"
    }
}

fn is_local(endpoint: &str) -> bool {
    endpoint.contains("127.0.0.1") || endpoint.contains("localhost") || endpoint.contains("[::1]")
}

/// Starts `ruro daemon` next to this executable, so the studio works out of the box.
fn start_local_daemon(endpoint: &str, token: Option<&str>) -> Option<std::process::Child> {
    let exe = std::env::current_exe().ok()?.with_file_name("ruro");
    let exe = if exe.exists() {
        exe
    } else {
        PathBuf::from("ruro")
    };
    let listen = endpoint.replace("localhost", "127.0.0.1");
    let mut cmd = std::process::Command::new(&exe);
    cmd.args(["daemon", "--listen", &listen])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null());
    if let Some(token) = token {
        // The daemon then accepts only sessions with the same token.
        cmd.env("RURO_TOKEN", token);
    }
    // SAFETY: prctl is async-signal-safe. The daemon (and the graph it runs)
    // stops when the studio that started it exits.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    match cmd.spawn() {
        Ok(child) => {
            tracing::info!(
                "started local daemon ({} daemon, pid {})",
                exe.display(),
                child.id()
            );
            Some(child)
        }
        Err(e) => {
            tracing::warn!("could not start {}: {e}", exe.display());
            None
        }
    }
}

mod heartbeat {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    /// Time of the last sign of life, updated from zenoh callbacks.
    pub struct Heartbeat {
        base: Instant,
        offset_ms: AtomicU64,
    }

    impl Heartbeat {
        pub fn now() -> Self {
            Self {
                base: Instant::now(),
                offset_ms: AtomicU64::new(0),
            }
        }

        pub fn touch(&self) {
            self.offset_ms
                .store(self.base.elapsed().as_millis() as u64, Ordering::Relaxed);
        }

        pub fn elapsed(&self) -> Duration {
            self.base.elapsed().saturating_sub(Duration::from_millis(
                self.offset_ms.load(Ordering::Relaxed),
            ))
        }
    }
}
