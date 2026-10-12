//! `ruro` — command-line interface.

mod fleet;
mod output;
mod recording;
mod soak;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use ruro_graph::{Graph, PortRef, Registry};
use ruro_proto::{Ctl, DaemonStatus, GroupMetrics, Keys, LogLine, Security, SetParam, TapFrame};
use zenoh::Wait;

const TIMEOUT: Duration = Duration::from_secs(10);
const DISCOVERY: Duration = Duration::from_millis(1500);

#[derive(Parser)]
#[command(
    name = "ruro",
    version,
    about = "Fast, lightweight robotics dataflow",
    propagate_version = true
)]
struct Cli {
    /// Daemon endpoint.
    #[arg(long, global = true, default_value = ruro_proto::DEFAULT_ENDPOINT, env = "RURO_CONNECT")]
    connect: String,
    /// Daemon name (discovered automatically if omitted).
    #[arg(long, global = true, env = "RURO_DAEMON")]
    daemon: Option<String>,
    #[command(flatten)]
    security: SecurityArgs,
    #[command(subcommand)]
    cmd: Cmd,
}

/// TLS and token options, shared by clients and the daemon.
#[derive(Args, Clone, Default)]
struct SecurityArgs {
    /// Certificate (PEM): the daemon's for `ruro daemon`, a client
    /// certificate for mutual TLS otherwise.
    #[arg(long, global = true, env = "RURO_TLS_CERT")]
    tls_cert: Option<PathBuf>,
    /// Private key (PEM) of `--tls-cert`.
    #[arg(long, global = true, env = "RURO_TLS_KEY")]
    tls_key: Option<PathBuf>,
    /// CA certificate (PEM) that signed the other side's certificate.
    #[arg(long, global = true, env = "RURO_TLS_CA")]
    tls_ca: Option<PathBuf>,
    /// Shared access token. A daemon started with one only accepts clients
    /// (and peer daemons) that present the same token. A token given here is
    /// visible to other users of the machine in `ps`; prefer RURO_TOKEN or
    /// --token-file.
    #[arg(long, global = true, env = "RURO_TOKEN", hide_env_values = true)]
    token: Option<String>,
    /// File holding the access token (its first line). Takes precedence over
    /// --token and RURO_TOKEN.
    #[arg(long, global = true, env = "RURO_TOKEN_FILE")]
    token_file: Option<PathBuf>,
}

impl SecurityArgs {
    fn security(&self) -> Result<Security> {
        let token = match &self.token_file {
            Some(path) => Some(ruro_proto::read_token_file(path)?),
            None => self.token.clone().filter(|t| !t.is_empty()),
        };
        Ok(Security {
            cert: self.tls_cert.clone(),
            key: self.tls_key.clone(),
            ca: self.tls_ca.clone(),
            token,
        })
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Run a graph in the foreground. Starts a daemon if none is reachable.
    Run { graph: PathBuf },
    /// Run the per-machine daemon in the foreground.
    Daemon {
        /// Name other tools use to address this daemon (default: hostname).
        #[arg(long)]
        name: Option<String>,
        /// Endpoint to listen on. Use tcp/0.0.0.0:7447 (or tls/0.0.0.0:7447
        /// with --tls-cert/--tls-key) to accept remote studios.
        #[arg(long, default_value = "tcp/127.0.0.1:7447")]
        listen: String,
        /// Host binary used to run graph groups.
        #[arg(long)]
        host: Option<PathBuf>,
        /// Another daemon to connect to, e.g. tcp/robot2:7447. Repeatable.
        #[arg(long = "peer")]
        peers: Vec<String>,
        /// Seconds without metrics after which a host is restarted.
        #[arg(long, default_value_t = 5.0)]
        heartbeat_timeout: f64,
    },
    /// Deploy a graph to the daemon and return. With --host-binary, first
    /// upload a host binary that groups can request with `host: <name>`.
    Deploy {
        graph: Option<PathBuf>,
        /// Daemon to deploy to (same as --daemon).
        #[arg(long)]
        to: Option<String>,
        /// Host binary to upload.
        #[arg(long)]
        host_binary: Option<PathBuf>,
        /// Name to store the binary under (default: its file name).
        #[arg(long, requires = "host_binary")]
        host_name: Option<String>,
    },
    /// Stop the running graph.
    Stop,
    /// Validate a graph against the available node types.
    Check { graph: PathBuf },
    /// Show the daemon, its groups and live node metrics.
    Status,
    /// List every daemon reachable through the connected one.
    Fleet,
    /// List available node types.
    Nodes {
        /// Show ports and parameters.
        #[arg(short, long)]
        verbose: bool,
    },
    /// Print live values of an output port, e.g. `ruro echo cam.image`.
    Echo { port: String },
    /// Change a node parameter while the graph runs.
    Param {
        node: String,
        name: String,
        /// JSON, or a plain string. Negative numbers need no `--`.
        #[arg(allow_hyphen_values = true)]
        value: String,
    },
    /// Kill a group process to exercise supervision.
    Kill { group: String },
    /// Stream logs from the running graph.
    Logs,
    /// Engage the emergency stop: actuator nodes command a safe state.
    Estop {
        /// Release the emergency stop instead: the stop engaged at this
        /// daemon, here and on every machine it was passed on to.
        #[arg(long)]
        release: bool,
        /// With --release, also lift stops engaged at other daemons and
        /// passed on to this one.
        #[arg(long, requires = "release")]
        all: bool,
    },
    /// Record every output of the running graph into an MCAP file.
    Record {
        /// File to write (on the daemon's machine).
        file: Option<PathBuf>,
        /// Stop the current recording and write the file.
        #[arg(long, conflicts_with = "file")]
        stop: bool,
    },
    /// Replay a recording into the running graph: the recorded nodes stop
    /// and their recorded outputs feed the rest of the graph.
    Play {
        file: PathBuf,
        /// Playback speed; 1 is real time.
        #[arg(long, default_value_t = 1.0)]
        rate: f64,
        /// Deploy this graph first.
        #[arg(long)]
        graph: Option<PathBuf>,
    },
    /// Control a running replay.
    Playback {
        #[command(subcommand)]
        cmd: recording::PlaybackCmd,
    },
    /// Show the channels, message counts and duration of a recording.
    Info { file: PathBuf },
    /// ROS 2 bag tools.
    Bag {
        #[command(subcommand)]
        cmd: recording::BagCmd,
    },
    /// Run a graph for a while and check memory, latency and drops.
    #[command(hide = true)]
    Soak(soak::SoakArgs),
    /// Open the desktop studio.
    Studio {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Print message schemas, check compatibility, generate bindings.
    Schema(ruro_schema::cli::SchemaArgs),
    /// Create a node package or add a node to one.
    New(ruro_pkg::cli::NewArgs),
    /// Build a host binary that links the packages listed in ruro.toml.
    Build(ruro_pkg::cli::BuildArgs),
    /// ROS 2 interop: list topics, generate message types, compute type hashes.
    Ros2(ruro_ros2::cli::Ros2Args),
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("RURO_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,zenoh=warn")),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .compact()
        .init();
    let cli = Cli::parse();
    // Rust ignores SIGPIPE, so printing into a closed pipe (`ruro status |
    // head`) panics. Commands that only print get the default: they end
    // quietly. Commands that may run a daemon keep ignoring it, so a peer
    // closing a connection cannot kill the daemon.
    if !matches!(cli.cmd, Cmd::Daemon { .. } | Cmd::Run { .. } | Cmd::Soak(_)) {
        // SAFETY: no other thread exists yet.
        unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    }
    if let Err(e) = run(cli) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

/// A connection to one daemon.
pub struct Client {
    pub session: zenoh::Session,
    pub keys: Keys,
}

impl Client {
    pub fn request(&self, op: Ctl, payload: &str) -> Result<String> {
        ruro_proto::request(&self.session, &self.keys, op, payload, TIMEOUT)
    }

    pub fn status(&self) -> Result<DaemonStatus> {
        let sub = self
            .session
            .declare_subscriber(self.keys.status())
            .wait()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let sample = sub
            .recv_timeout(Duration::from_secs(2))
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .context("daemon sent no status")?;
        ruro_proto::decode(&sample.payload().to_bytes())
    }
}

fn run(cli: Cli) -> Result<()> {
    let security = cli.security.security()?;
    let connect = |name: Option<&str>| -> Result<Client> {
        let session = ruro_proto::connect_with(&cli.connect, &security)?;
        let keys = ruro_proto::discover(&session, name.or(cli.daemon.as_deref()), DISCOVERY)?;
        Ok(Client { session, keys })
    };
    let client = || connect(None);
    match cli.cmd {
        Cmd::Daemon {
            name,
            listen,
            host,
            peers,
            heartbeat_timeout,
        } => {
            let mut config = ruro_daemon::Config::new();
            config.listen = listen;
            config.peers = peers;
            config.security = security.clone();
            config.heartbeat_timeout = Duration::from_secs_f64(heartbeat_timeout.max(0.5));
            if let Some(n) = name {
                config.name = n;
            }
            if let Some(h) = host {
                config.host = h;
            }
            let daemon = ruro_daemon::Daemon::start(config)?;
            wait_for_ctrl_c()?;
            daemon.shutdown();
            Ok(())
        }
        Cmd::Run { graph } => run_graph(&cli.connect, cli.daemon.as_deref(), &security, &graph),
        Cmd::Deploy {
            graph,
            to,
            host_binary,
            host_name,
        } => {
            if graph.is_none() && host_binary.is_none() {
                bail!("nothing to deploy: give a graph, --host-binary, or both");
            }
            let c = connect(to.as_deref())?;
            if let Some(bin) = host_binary {
                println!("{}", fleet::upload_host(&c, &bin, host_name.as_deref())?);
            }
            if let Some(graph) = graph {
                println!("{}", deploy(&c, &graph)?);
            }
            Ok(())
        }
        Cmd::Stop => {
            println!("{}", client()?.request(Ctl::Stop, "")?);
            Ok(())
        }
        Cmd::Check { graph: path } => {
            let graph =
                Graph::load(&path).with_context(|| format!("loading {}", path.display()))?;
            let registry =
                local_or_remote_registry(&cli.connect, cli.daemon.as_deref(), &security)?;
            let issues = ruro_graph::validate_in(&graph, &registry, &base_dir(&path));
            if issues.is_empty() {
                let groups = graph.groups();
                println!(
                    "{}: ok ({} nodes, {} wires, {} process group{})",
                    path.display(),
                    graph.nodes.len(),
                    graph.wires.len(),
                    groups.len(),
                    if groups.len() == 1 { "" } else { "s" }
                );
                Ok(())
            } else {
                for i in &issues {
                    eprintln!("  {i}");
                }
                bail!("{} issue(s) in {}", issues.len(), path.display());
            }
        }
        Cmd::Status => status(&client()?),
        Cmd::Fleet => {
            let session = ruro_proto::connect_with(&cli.connect, &security)?;
            fleet::fleet(&session)
        }
        Cmd::Nodes { verbose } => {
            let registry =
                local_or_remote_registry(&cli.connect, cli.daemon.as_deref(), &security)?;
            output::print_registry(&registry, verbose);
            Ok(())
        }
        Cmd::Echo { port } => {
            let c = client()?;
            let port: PortRef = port.parse()?;
            // Watching may start before the graph does: then wait on this daemon.
            let keys = daemon_of(&c, &port.node).unwrap_or_else(|e| {
                eprintln!("{e:#}; waiting for it on `{}`", c.keys.daemon());
                c.keys.clone()
            });
            let _sub = c
                .session
                .declare_subscriber(keys.tap(&port.node, &port.port))
                .callback(|sample| {
                    if let Ok(frame) = ruro_proto::decode::<TapFrame>(&sample.payload().to_bytes())
                    {
                        output::emit(&output::preview_line(&frame.preview));
                    }
                })
                .wait()
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            eprintln!("watching {port} (ctrl-c to stop)");
            wait_for_ctrl_c()
        }
        Cmd::Param { node, name, value } => {
            let c = client()?;
            let keys = daemon_of(&c, &node)?;
            // Accept bare words as strings: `ruro param cam mode fast`.
            let json = serde_json::from_str::<serde_json::Value>(&value)
                .unwrap_or(serde_json::Value::String(value))
                .to_string();
            let msg = SetParam {
                name: name.clone(),
                value: json.clone(),
            };
            c.session
                .put(keys.param(&node), ruro_proto::encode(&msg))
                .wait()
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            // Give the publication a moment to leave before the session closes.
            std::thread::sleep(Duration::from_millis(100));
            println!("{node}.{name} = {json}");
            Ok(())
        }
        Cmd::Kill { group } => {
            println!("{}", client()?.request(Ctl::Kill, &group)?);
            Ok(())
        }
        Cmd::Logs => {
            let c = client()?;
            stream_logs(&c.session, &c.keys)?;
            wait_for_ctrl_c()
        }
        Cmd::Estop { release, all } => {
            let payload = match (release, all) {
                (false, _) => "engage",
                (true, false) => "release",
                (true, true) => "release-all",
            };
            println!("{}", client()?.request(Ctl::Estop, payload)?);
            Ok(())
        }
        Cmd::Record { file, stop } => recording::record(&client()?, file, stop),
        Cmd::Play { file, rate, graph } => {
            let c = client()?;
            if let Some(g) = graph {
                println!("{}", deploy(&c, &g)?);
            }
            recording::play(&c, &file, rate)
        }
        Cmd::Playback { cmd } => recording::control(&client()?, cmd),
        Cmd::Info { file } => recording::info(&file),
        Cmd::Bag { cmd } => recording::bag(cmd),
        Cmd::Soak(args) => soak::soak(&cli.connect, cli.daemon.as_deref(), &security, args),
        Cmd::Studio { args } => {
            let exe = std::env::current_exe()?.with_file_name("ruro-studio");
            let exe = if exe.exists() {
                exe
            } else {
                PathBuf::from("ruro-studio")
            };
            let err = exec(&exe, &args);
            bail!("failed to start {}: {err}", exe.display())
        }
        Cmd::Schema(args) => ruro_schema::cli::run(args),
        Cmd::New(args) => ruro_pkg::cli::run_new(args),
        Cmd::Build(args) => ruro_pkg::cli::run_build(args),
        Cmd::Ros2(args) => ruro_ros2::cli::run(args),
    }
}

fn deploy(c: &Client, path: &Path) -> Result<String> {
    c.request(Ctl::Deploy, &flattened_yaml(path)?)
}

/// Loads a graph and expands its subgraphs, resolving `graph:` file
/// references relative to the graph's own directory. The daemon may run on
/// another machine, so it must receive a self-contained graph.
fn flattened_yaml(path: &Path) -> Result<String> {
    let graph = Graph::load(path).with_context(|| format!("loading {}", path.display()))?;
    let flat = graph
        .flatten_in(&base_dir(path))
        .with_context(|| format!("expanding subgraphs of {}", path.display()))?;
    Ok(flat.to_yaml()?)
}

fn base_dir(path: &Path) -> PathBuf {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Connects to the daemon at `endpoint`, starting one in this process if
/// none is reachable.
pub fn connect_or_start(
    endpoint: &str,
    name: Option<&str>,
    security: &Security,
) -> Result<(Option<ruro_daemon::Daemon>, Client)> {
    let (daemon, session) = match ruro_proto::connect_with(endpoint, security) {
        Ok(session) => (None, session),
        // A daemon that refuses the session (wrong certificate or token)
        // is reported; starting another on its endpoint would fail anyway.
        Err(e)
            if ruro_proto::ConnectError::failure_of(&e)
                != Some(ruro_proto::ConnectFailure::NoDaemon) =>
        {
            return Err(e);
        }
        Err(_) => {
            let mut config = ruro_daemon::Config::new();
            config.listen = endpoint.to_owned();
            config.security = security.clone();
            if let Some(n) = name {
                config.name = n.to_owned();
            }
            let daemon = ruro_daemon::Daemon::start(config)?;
            (Some(daemon), ruro_proto::connect_with(endpoint, security)?)
        }
    };
    let keys = ruro_proto::discover(&session, name, DISCOVERY)?;
    Ok((daemon, Client { session, keys }))
}

/// `ruro run`: deploy to a reachable daemon, or start one in this process.
fn run_graph(endpoint: &str, name: Option<&str>, security: &Security, path: &Path) -> Result<()> {
    let (daemon, c) = connect_or_start(endpoint, name, security)?;
    println!("{}", deploy(&c, path)?);
    if daemon.is_none() {
        // Host output goes to the remote daemon's terminal; mirror it here.
        stream_logs(&c.session, &c.keys)?;
    }
    wait_for_ctrl_c()?;
    let _ = c.request(Ctl::Stop, "");
    if let Some(d) = daemon {
        d.shutdown();
    }
    Ok(())
}

fn stream_logs(session: &zenoh::Session, keys: &Keys) -> Result<()> {
    session
        .declare_subscriber(keys.all_logs())
        .callback(|sample| {
            if let Ok(line) = ruro_proto::decode::<LogLine>(&sample.payload().to_bytes()) {
                output::emit(&output::log_line(&line));
            }
        })
        .background()
        .wait()
        .map_err(|e| anyhow::anyhow!("{e}"))
}

/// Latest metrics of every running group of the daemon.
/// Keys of the daemon that runs `node`: the connected one or, for a graph
/// that spans machines, the peer running the node's group.
fn daemon_of(c: &Client, node: &str) -> Result<Keys> {
    let status = c.status()?;
    let Some(graph) = &status.graph else {
        bail!("no graph is running on `{}`", status.name);
    };
    if runs_node(&status, node) {
        return Ok(c.keys.clone());
    }
    let sub = c
        .session
        .declare_subscriber(Keys::all_status())
        .wait()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let deadline = std::time::Instant::now() + DISCOVERY;
    while std::time::Instant::now() < deadline {
        if let Ok(Some(s)) = sub.recv_timeout(Duration::from_millis(100))
            && let Ok(peer) = ruro_proto::decode::<DaemonStatus>(&s.payload().to_bytes())
            && peer.graph.as_ref() == Some(graph)
            && runs_node(&peer, node)
        {
            return Ok(Keys::new(&peer.name));
        }
    }
    bail!("no node `{node}` in graph `{graph}`")
}

fn runs_node(status: &DaemonStatus, node: &str) -> bool {
    status
        .groups
        .iter()
        .any(|g| g.nodes.iter().any(|n| n == node))
}

pub fn group_metrics(c: &Client, status: &DaemonStatus) -> Result<Vec<GroupMetrics>> {
    let metrics_sub = c
        .session
        .declare_subscriber(c.keys.all_metrics())
        .wait()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let expected = status.groups.iter().filter(|g| g.pid.is_some()).count();
    let mut metrics: Vec<GroupMetrics> = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_millis(1200);
    while metrics.len() < expected && std::time::Instant::now() < deadline {
        if let Ok(Some(s)) = metrics_sub.recv_timeout(Duration::from_millis(100))
            && let Ok(m) = ruro_proto::decode::<GroupMetrics>(&s.payload().to_bytes())
        {
            metrics.retain(|x| x.group != m.group);
            metrics.push(m);
        }
    }
    Ok(metrics)
}

fn status(c: &Client) -> Result<()> {
    // Daemons publish their status twice a second; listen while metrics
    // are collected to find the other machines of the graph.
    let others = c
        .session
        .declare_subscriber(Keys::all_status())
        .wait()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let listening = std::time::Instant::now();
    let status = c.status()?;
    let metrics = group_metrics(c, &status)?;
    let mut peers: std::collections::BTreeMap<String, DaemonStatus> = Default::default();
    loop {
        while let Ok(Some(s)) = others.try_recv() {
            if let Ok(peer) = ruro_proto::decode::<DaemonStatus>(&s.payload().to_bytes())
                && peer.name != status.name
                && peer.graph.is_some()
                && peer.graph == status.graph
            {
                peers.insert(peer.name.clone(), peer);
            }
        }
        if status.graph.is_none() || listening.elapsed() >= Duration::from_millis(700) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let peers: Vec<DaemonStatus> = peers.into_values().collect();
    output::print_status(&status, &metrics, &peers);
    Ok(())
}

fn local_or_remote_registry(
    endpoint: &str,
    name: Option<&str>,
    security: &Security,
) -> Result<Registry> {
    if let Ok(session) = ruro_proto::connect_with(endpoint, security)
        && let Ok(keys) = ruro_proto::discover(&session, name, DISCOVERY)
    {
        let body = ruro_proto::request(&session, &keys, Ctl::Registry, "", TIMEOUT)?;
        return Ok(serde_json::from_str(&body)?);
    }
    ruro_daemon::load_registry(&ruro_daemon::default_host_binary())
}

/// Blocks until SIGINT or SIGTERM.
pub fn wait_for_ctrl_c() -> Result<()> {
    let stop = ctrl_c_flag()?;
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

/// A flag set by SIGINT or SIGTERM.
pub fn ctrl_c_flag() -> Result<Arc<AtomicBool>> {
    let stop = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGINT, stop.clone())?;
    signal_hook::flag::register(signal_hook::consts::SIGTERM, stop.clone())?;
    Ok(stop)
}

fn exec(exe: &PathBuf, args: &[String]) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    std::process::Command::new(exe).args(args).exec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_nodes_in_the_groups_a_daemon_runs() {
        let status = DaemonStatus {
            name: "robot".into(),
            version: String::new(),
            pid: 1,
            uptime_s: 0.0,
            graph: Some("g".into()),
            groups: vec![ruro_proto::GroupStatus {
                name: "drivers".into(),
                nodes: vec!["camera".into(), "imu".into()],
                state: ruro_proto::GroupState::Running,
                pid: Some(2),
                restarts: 0,
                last_exit: None,
            }],
            recording: None,
            playback: None,
            estopped: false,
        };
        assert!(runs_node(&status, "imu"));
        assert!(!runs_node(&status, "drivers"));
        assert!(!runs_node(&status, "stats"));
    }

    #[test]
    fn tokens_come_from_files_before_flags() {
        let path = std::env::temp_dir().join(format!("ruro-cli-token-{}", std::process::id()));
        std::fs::write(&path, "from-file\n").unwrap();
        let cli = Cli::try_parse_from([
            "ruro",
            "--token",
            "from-flag",
            "--token-file",
            path.to_str().unwrap(),
            "status",
        ])
        .unwrap();
        let token = cli.security.security().unwrap().token;
        assert_eq!(token.as_deref(), Some("from-file"));
        let _ = std::fs::remove_file(&path);
        let cli = Cli::try_parse_from(["ruro", "--token-file", path.to_str().unwrap(), "status"])
            .unwrap();
        assert!(
            cli.security.security().is_err(),
            "a missing file is an error"
        );
    }

    #[test]
    fn releasing_every_stop_needs_release() {
        assert!(Cli::try_parse_from(["ruro", "estop", "--all"]).is_err());
        let cli = Cli::try_parse_from(["ruro", "estop", "--release", "--all"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Estop {
                release: true,
                all: true
            }
        ));
    }

    #[test]
    fn parameters_can_be_set_to_negative_values() {
        let cli = Cli::try_parse_from(["ruro", "param", "pid", "kp", "-1.5"]).unwrap();
        assert!(matches!(cli.cmd, Cmd::Param { value, .. } if value == "-1.5"));
    }

    #[test]
    fn deploys_send_graphs_with_file_subgraphs_expanded() {
        let dir = std::env::temp_dir().join(format!("ruro-cli-flatten-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("parts")).unwrap();
        std::fs::write(
            dir.join("parts/source.yaml"),
            "name: source\noutputs:\n  value: s.value\nnodes:\n  s: { type: std/sine, rate: 10 }\n",
        )
        .unwrap();
        let top = dir.join("top.yaml");
        std::fs::write(
            &top,
            "name: top\nnodes:\n  src: { type: graph:parts/source.yaml }\n  g: { type: std/gain }\nwires: [src.value -> g.input]\n",
        )
        .unwrap();

        // Resolved against the graph's directory, not the working directory.
        let flat = Graph::from_yaml(&flattened_yaml(&top).unwrap()).unwrap();
        let kinds: Vec<&str> = flat.nodes.values().map(|n| n.kind.as_str()).collect();
        assert_eq!(kinds, ["std/sine", "std/gain"]);
        assert_eq!(flat.wires.len(), 1);
        assert_eq!(flat.wires[0].from.port, "value");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
