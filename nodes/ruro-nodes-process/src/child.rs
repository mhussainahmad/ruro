//! A running program and the protocol lines exchanged with it.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::process::{Child as Process, Command, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde_json::Value;

use crate::manifest::{Manifest, Transport};
use crate::{PREFIX, PROTOCOL};

/// What the reader thread of the protocol stream passes on.
enum Event {
    Frame(Value),
    /// A protocol line that is not JSON.
    Garbled(String, String),
    /// The stream ended.
    Closed,
}

/// A program implementing one node instance.
pub struct Child {
    process: Process,
    /// Requests, as complete lines, for the writer thread.
    requests: Sender<Vec<u8>>,
    replies: Receiver<Event>,
    command: String,
}

impl Child {
    /// Starts the program for node `id` and waits for its hello.
    pub fn spawn(m: &Manifest, id: &str) -> Result<Self> {
        let program = m.program();
        let command = m.command.join(" ");
        let mut cmd = Command::new(&program);
        cmd.args(&m.command[1..])
            .current_dir(&m.dir)
            .envs(m.env.iter().map(|(k, v)| (k, v)))
            .env("RURO_NODE_ID", id)
            .env("RURO_NODE_PROTOCOL", PROTOCOL.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let listener = match m.transport {
            Transport::Stdio => {
                cmd.stdin(Stdio::piped());
                None
            }
            Transport::Tcp => {
                let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                    .context("listening for the program's connection")?;
                cmd.stdin(Stdio::null())
                    .env("RURO_NODE_ADDR", l.local_addr()?.to_string());
                Some(l)
            }
        };
        let token = token();
        cmd.env("RURO_NODE_TOKEN", &token);
        own_process_group(&mut cmd);
        let mut process = cmd
            .spawn()
            .with_context(|| format!("starting `{}`", program.display()))?;

        let (reply_tx, replies) = channel();
        let (requests, request_rx) = channel::<Vec<u8>>();
        let stdout = process.stdout.take().expect("piped");
        let stderr = process.stderr.take().expect("piped");
        let log_id = id.to_owned();
        spawn_thread("stderr", move || log_lines(stderr, &log_id));
        match listener {
            None => {
                let stdin = process.stdin.take().expect("piped");
                let id = id.to_owned();
                spawn_thread("stdout", move || read_protocol(stdout, &id, &reply_tx));
                spawn_thread("writer", move || write_requests(stdin, &request_rx));
            }
            Some(listener) => {
                let id2 = id.to_owned();
                spawn_thread("stdout", move || log_lines(stdout, &id2));
                let stream = match accept(&listener, &mut process, m.start_timeout) {
                    Ok(s) => s,
                    Err(e) => {
                        kill(&mut process);
                        return Err(e);
                    }
                };
                let reader = stream.try_clone()?;
                let id = id.to_owned();
                spawn_thread("reader", move || read_protocol(reader, &id, &reply_tx));
                spawn_thread("writer", move || write_requests(stream, &request_rx));
            }
        }
        let mut child = Self {
            process,
            requests,
            replies,
            command,
        };
        let hello = child
            .receive(m.start_timeout)
            .context("waiting for the program's hello")?;
        let version = hello.get("ruro").and_then(Value::as_u64);
        ensure!(
            version == Some(PROTOCOL),
            "the program's hello {hello} does not speak protocol {PROTOCOL} (expected `{PREFIX}{{\"ruro\": {PROTOCOL}}}` as its first protocol line)"
        );
        if m.transport == Transport::Tcp {
            ensure!(
                hello.get("token").and_then(Value::as_str) == Some(token.as_str()),
                "the connection did not present RURO_NODE_TOKEN"
            );
        }
        Ok(child)
    }

    /// Sends `request` and waits up to `timeout` for the reply.
    pub fn request(&mut self, request: &Value, timeout: Duration) -> Result<Value> {
        let mut line = Vec::from(PREFIX.as_bytes());
        serde_json::to_writer(&mut line, request)?;
        // An empty line after each request: programs ignore it, and line
        // readers that only return a line once the next one has begun
        // (Octave's fgetl on a pipe) return the request at once.
        line.extend_from_slice(b"\n\n");
        if self.requests.send(line).is_err() {
            return Err(self.ended());
        }
        self.receive(timeout)
    }

    fn receive(&mut self, timeout: Duration) -> Result<Value> {
        match self.replies.recv_timeout(timeout) {
            Ok(Event::Frame(v)) => Ok(v),
            Ok(Event::Garbled(line, e)) => {
                bail!(
                    "`{}` sent a protocol line that is not JSON ({e}): {line}",
                    self.command
                )
            }
            Ok(Event::Closed) | Err(RecvTimeoutError::Disconnected) => Err(self.ended()),
            Err(RecvTimeoutError::Timeout) => bail!(
                "`{}` did not answer within {:.1} s",
                self.command,
                timeout.as_secs_f64()
            ),
        }
    }

    /// Why the program stopped talking.
    fn ended(&mut self) -> anyhow::Error {
        // The stream can close just before the process is reaped.
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            match self.process.try_wait() {
                Ok(Some(status)) => {
                    return anyhow!("`{}` exited ({})", self.command, describe(status));
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                _ => return anyhow!("`{}` closed its protocol stream", self.command),
            }
        }
    }

    pub fn pid(&self) -> u32 {
        self.process.id()
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        kill(&mut self.process);
    }
}

fn spawn_thread(what: &str, f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .name(format!("process-node-{what}"))
        .spawn(f)
        .expect("spawning a thread");
}

/// Hands protocol lines to `tx` and logs everything else.
fn read_protocol(stream: impl Read, id: &str, tx: &Sender<Event>) {
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { break };
        let event = match line.strip_prefix(PREFIX) {
            Some(json) => match serde_json::from_str(json) {
                Ok(v) => Event::Frame(v),
                Err(e) => Event::Garbled(line.clone(), e.to_string()),
            },
            None => {
                log_line(id, &line);
                continue;
            }
        };
        if tx.send(event).is_err() {
            return;
        }
    }
    let _ = tx.send(Event::Closed);
}

fn log_lines(stream: impl Read, id: &str) {
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { break };
        log_line(id, &line);
    }
}

/// Logs a line of the program's output at the level its first word names
/// (`error`, `warning`, `debug`, optionally in brackets or followed by a
/// colon); info otherwise.
fn log_line(id: &str, line: &str) {
    let word: String = line
        .trim_start()
        .trim_start_matches('[')
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_lowercase();
    match word.as_str() {
        "error" | "fatal" | "critical" => tracing::error!("{id}: {line}"),
        "warn" | "warning" => tracing::warn!("{id}: {line}"),
        "debug" | "trace" => tracing::debug!("{id}: {line}"),
        _ => tracing::info!("{id}: {line}"),
    }
}

fn write_requests(mut sink: impl Write, rx: &Receiver<Vec<u8>>) {
    while let Ok(line) = rx.recv() {
        if sink.write_all(&line).and_then(|()| sink.flush()).is_err() {
            return;
        }
    }
}

/// Waits for the program to connect, while it runs.
fn accept(listener: &TcpListener, process: &mut Process, timeout: Duration) -> Result<TcpStream> {
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false)?;
                stream.set_nodelay(true)?;
                return Ok(stream);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e).context("accepting the program's connection"),
        }
        if let Some(status) = process.try_wait()? {
            bail!(
                "the program exited ({}) before connecting to RURO_NODE_ADDR",
                describe(status)
            );
        }
        ensure!(
            Instant::now() < deadline,
            "the program did not connect to RURO_NODE_ADDR within {:.0} s",
            timeout.as_secs_f64()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn describe(status: ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return format!("signal {sig}");
        }
    }
    match status.code() {
        Some(c) => format!("status {c}"),
        None => status.to_string(),
    }
}

/// A secret the program presents when it connects over TCP, so that other
/// local programs cannot take its place.
fn token() -> String {
    let mut bytes = [0u8; 16];
    let read = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes));
    if read.is_err() {
        // Not a secret then, but still unique to this attempt.
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        bytes = (t ^ u128::from(std::process::id())).to_le_bytes();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The program and whatever it starts get a process group of their own,
/// which is signalled as a whole, and on Linux die with the host.
fn own_process_group(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
        #[cfg(target_os = "linux")]
        // SAFETY: prctl is async-signal-safe and touches no Rust state.
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
    }
    #[cfg(not(unix))]
    let _ = cmd;
}

/// Terminates the program's process group: SIGTERM, then SIGKILL after a
/// second.
fn kill(process: &mut Process) {
    if let Ok(Some(_)) = process.try_wait() {
        signal_group(process, true);
        return;
    }
    signal_group(process, false);
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        if let Ok(Some(_)) = process.try_wait() {
            signal_group(process, true);
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    signal_group(process, true);
    let _ = process.kill();
    let _ = process.wait();
}

fn signal_group(process: &mut Process, hard: bool) {
    #[cfg(unix)]
    {
        let sig = if hard { libc::SIGKILL } else { libc::SIGTERM };
        let pgid = process.id() as libc::pid_t;
        // SAFETY: plain signal delivery to the group this module created.
        unsafe { libc::kill(-pgid, sig) };
    }
    #[cfg(not(unix))]
    if hard {
        let _ = process.kill();
    }
}
