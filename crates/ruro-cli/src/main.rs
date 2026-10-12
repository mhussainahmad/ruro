//! `ruro` — command-line interface.

mod output;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use ruro_graph::{Graph, PortRef, Registry};
use ruro_proto::{Ctl, DaemonStatus, GroupMetrics, Keys, LogLine, SetParam, TapFrame};
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
    #[command(subcommand)]
    cmd: Cmd,
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
        /// Endpoint to listen on. Use tcp/0.0.0.0:7447 to accept remote studios.
        #[arg(long, default_value = "tcp/127.0.0.1:7447")]
        listen: String,
        /// Host binary used to run graph groups.
        #[arg(long)]
        host: Option<PathBuf>,
    },
    /// Deploy a graph to the daemon and return.
    Deploy { graph: PathBuf },
    /// Stop the running graph.
    Stop,
    /// Validate a graph against the available node types.
    Check { graph: PathBuf },
    /// Show the daemon, its groups and live node metrics.
    Status,
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
        value: String,
    },
    /// Kill a group process to exercise supervision.
    Kill { group: String },
    /// Stream logs from the running graph.
    Logs,
    /// Open the desktop studio.
    Studio {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Print message schemas, check compatibility, generate bindings.
    Schema(ruro_schema::cli::SchemaArgs),
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("RURO_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,zenoh=warn")),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .compact()
        .init();
    if let Err(e) = run(Cli::parse()) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let client = || -> Result<(zenoh::Session, Keys)> {
        let session = ruro_proto::connect(&cli.connect).context("start one with `ruro daemon`")?;
        let keys = ruro_proto::discover(&session, cli.daemon.as_deref(), DISCOVERY)?;
        Ok((session, keys))
    };
    match cli.cmd {
        Cmd::Daemon { name, listen, host } => {
            let mut config = ruro_daemon::Config::new();
            config.listen = listen;
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
        Cmd::Run { graph } => run_graph(&cli.connect, cli.daemon.as_deref(), &graph),
        Cmd::Deploy { graph } => {
            let (session, keys) = client()?;
            println!("{}", deploy(&session, &keys, &graph)?);
            Ok(())
        }
        Cmd::Stop => {
            let (session, keys) = client()?;
            println!(
                "{}",
                ruro_proto::request(&session, &keys, Ctl::Stop, "", TIMEOUT)?
            );
            Ok(())
        }
        Cmd::Check { graph: path } => {
            let graph =
                Graph::load(&path).with_context(|| format!("loading {}", path.display()))?;
            let registry = local_or_remote_registry(&cli.connect, cli.daemon.as_deref())?;
            let issues = ruro_graph::validate(&graph, &registry);
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
        Cmd::Status => {
            let (session, keys) = client()?;
            status(&session, &keys)
        }
        Cmd::Nodes { verbose } => {
            let registry = local_or_remote_registry(&cli.connect, cli.daemon.as_deref())?;
            output::print_registry(&registry, verbose);
            Ok(())
        }
        Cmd::Echo { port } => {
            let (session, keys) = client()?;
            let port: PortRef = port.parse()?;
            let _sub = session
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
            let (session, keys) = client()?;
            // Accept bare words as strings: `ruro param cam mode fast`.
            let json = serde_json::from_str::<serde_json::Value>(&value)
                .unwrap_or(serde_json::Value::String(value))
                .to_string();
            let msg = SetParam {
                name: name.clone(),
                value: json.clone(),
            };
            session
                .put(keys.param(&node), ruro_proto::encode(&msg))
                .wait()
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            // Give the publication a moment to leave before the session closes.
            std::thread::sleep(Duration::from_millis(100));
            println!("{node}.{name} = {json}");
            Ok(())
        }
        Cmd::Kill { group } => {
            let (session, keys) = client()?;
            println!(
                "{}",
                ruro_proto::request(&session, &keys, Ctl::Kill, &group, TIMEOUT)?
            );
            Ok(())
        }
        Cmd::Logs => {
            let (session, keys) = client()?;
            stream_logs(&session, &keys)?;
            wait_for_ctrl_c()
        }
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
    }
}

fn deploy(session: &zenoh::Session, keys: &Keys, path: &PathBuf) -> Result<String> {
    let yaml =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    ruro_proto::request(session, keys, Ctl::Deploy, &yaml, TIMEOUT)
}

/// `ruro run`: deploy to a reachable daemon, or start one in this process.
fn run_graph(endpoint: &str, name: Option<&str>, path: &PathBuf) -> Result<()> {
    let (daemon, session) = match ruro_proto::connect(endpoint) {
        Ok(session) => (None, session),
        Err(_) => {
            let mut config = ruro_daemon::Config::new();
            config.listen = endpoint.to_owned();
            if let Some(n) = name {
                config.name = n.to_owned();
            }
            let daemon = ruro_daemon::Daemon::start(config)?;
            (Some(daemon), ruro_proto::connect(endpoint)?)
        }
    };
    let keys = ruro_proto::discover(&session, name, DISCOVERY)?;
    println!("{}", deploy(&session, &keys, path)?);
    if daemon.is_none() {
        // Host output goes to the remote daemon's terminal; mirror it here.
        stream_logs(&session, &keys)?;
    }
    wait_for_ctrl_c()?;
    let _ = ruro_proto::request(&session, &keys, Ctl::Stop, "", TIMEOUT);
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

fn status(session: &zenoh::Session, keys: &Keys) -> Result<()> {
    let status_sub = session
        .declare_subscriber(keys.status())
        .wait()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let metrics_sub = session
        .declare_subscriber(keys.all_metrics())
        .wait()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let sample = status_sub
        .recv_timeout(Duration::from_secs(2))
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .context("daemon sent no status")?;
    let status: DaemonStatus = ruro_proto::decode(&sample.payload().to_bytes())?;
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
    output::print_status(&status, &metrics);
    Ok(())
}

fn local_or_remote_registry(endpoint: &str, name: Option<&str>) -> Result<Registry> {
    if let Ok(session) = ruro_proto::connect(endpoint)
        && let Ok(keys) = ruro_proto::discover(&session, name, DISCOVERY)
    {
        let body = ruro_proto::request(&session, &keys, Ctl::Registry, "", TIMEOUT)?;
        return Ok(serde_json::from_str(&body)?);
    }
    ruro_daemon::load_registry(&ruro_daemon::default_host_binary())
}

fn wait_for_ctrl_c() -> Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGINT, stop.clone())?;
    signal_hook::flag::register(signal_hook::consts::SIGTERM, stop.clone())?;
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

fn exec(exe: &PathBuf, args: &[String]) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    std::process::Command::new(exe).args(args).exec()
}
