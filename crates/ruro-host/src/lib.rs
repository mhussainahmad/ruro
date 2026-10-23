//! The ruro host: a process that executes one group of a graph.
//!
//! Every node crate linked into a host binary is available to graphs. The
//! stock `ruro-host` links the standard library; to ship your own nodes, make
//! a binary that depends on your node crates and calls [`main`]:
//!
//! ```ignore
//! fn main() {
//!     my_nodes::link();
//!     ruro_host::main();
//! }
//! ```
//!
//! and point the daemon at it with `ruro daemon --host path/to/binary`.

mod control;
mod executor;
mod logging;
mod record;
pub mod ros2_params;
pub mod rt;
mod run;
mod stats;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ruro_graph::Graph;
use ruro_proto::Keys;

pub use executor::{Executor, Options, Remote, clock_port};
pub use record::Recorder;
pub use run::step_order;

/// Environment variable carrying the daemon's access token to its hosts.
pub const TOKEN_ENV: &str = "RURO_TOKEN";

#[derive(Parser)]
#[command(
    name = "ruro-host",
    version,
    about = "Executes one group of a ruro graph"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print the descriptors of every linked node type as JSON.
    Describe,
    /// Run one group of a graph.
    Run {
        /// Graph YAML file.
        #[arg(long)]
        graph: PathBuf,
        /// Group to execute.
        #[arg(long)]
        group: String,
        /// Daemon this host belongs to.
        #[arg(long)]
        daemon: String,
        /// Daemon endpoint for metrics, taps, parameters and logs.
        #[arg(long, default_value = ruro_proto::DEFAULT_ENDPOINT)]
        connect: String,
        /// Start with the emergency stop engaged.
        #[arg(long)]
        estopped: bool,
        /// Start recording the group's outputs into this MCAP file.
        #[arg(long)]
        record: Option<PathBuf>,
        /// Nodes whose outputs are being replayed (comma separated).
        #[arg(long, value_delimiter = ',')]
        mute: Vec<String>,
        /// Send messages to other machines uncompressed.
        #[arg(long)]
        no_compress: bool,
    },
}

/// Descriptors of every node type linked into this binary, as JSON.
pub fn describe() -> String {
    ruro_nodes_std::link();
    ruro_nodes_hw::link();
    ruro_nodes_sim::link();
    ruro_nodes_ml::link();
    ruro_nodes_ros2::link();
    ruro_nodes_gpu::link();
    ruro_nodes_process::link();
    serde_json::to_string(&ruro_node::registry()).expect("registry is serializable")
}

/// Entry point for host binaries.
pub fn main() {
    ruro_nodes_std::link();
    ruro_nodes_hw::link();
    ruro_nodes_sim::link();
    ruro_nodes_ml::link();
    ruro_nodes_ros2::link();
    ruro_nodes_gpu::link();
    ruro_nodes_process::link();
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Describe => {
            println!("{}", describe());
            Ok(())
        }
        Cmd::Run {
            graph,
            group,
            daemon,
            connect,
            estopped,
            record,
            mute,
            no_compress,
        } => run(RunArgs {
            graph,
            group,
            daemon,
            connect,
            estopped,
            record,
            mute,
            compress: !no_compress,
        }),
    };
    if let Err(e) = result {
        eprintln!("ruro-host: {e:#}");
        std::process::exit(1);
    }
}

struct RunArgs {
    graph: PathBuf,
    group: String,
    daemon: String,
    connect: String,
    estopped: bool,
    record: Option<PathBuf>,
    mute: Vec<String>,
    compress: bool,
}

fn run(args: RunArgs) -> Result<()> {
    let graph =
        Graph::load(&args.graph).with_context(|| format!("loading {}", args.graph.display()))?;
    let keys = Keys::new(&args.daemon);
    let group = args.group.as_str();

    let security = ruro_proto::Security {
        token: std::env::var(TOKEN_ENV).ok().filter(|t| !t.is_empty()),
        ..Default::default()
    };
    let session = zenoh::open(ruro_proto::client_config_with(&args.connect, &security)?)
        .wait_session()
        .with_context(|| format!("connecting to daemon at {}", args.connect))?;
    logging::init(&session, &keys, group);

    let stop = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, stop.clone())?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, stop.clone())?;

    let exec = Arc::new(Executor::start_with(
        &graph,
        group,
        Options {
            remote: Remote::Shm(keys.clone()),
            session: Some(session.clone()),
            compress: args.compress,
            estopped: args.estopped,
            muted: args.mute.clone(),
        },
    )?);
    if let Some(path) = &args.record {
        exec.start_recording(path)?;
    }
    let control = control::attach(&session, &keys, &exec)?;
    // A ROS 2 network that cannot be reached must not stop the robot: the
    // parameter services are tried again until it can.
    let ros2_settings = graph.ros2.clone().filter(|r| r.parameters);
    let mut ros2 = None;
    let mut ros2_tried: Option<std::time::Instant> = None;
    let mut ros2_warned = false;
    tracing::info!(
        "group `{group}` running {} node(s){}",
        exec.node_ids().count(),
        if args.estopped { " (e-stopped)" } else { "" }
    );

    let metrics_key = keys.metrics(group);
    let mut tick = 0u32;
    while !stop.load(Ordering::Relaxed) {
        if let Some(r) = &ros2_settings
            && ros2.is_none()
            && ros2_tried.is_none_or(|t| t.elapsed() >= Duration::from_secs(5))
        {
            ros2_tried = Some(std::time::Instant::now());
            match ros2_params::attach(&exec, r) {
                Ok(s) => {
                    if ros2_warned {
                        tracing::info!("ROS 2 parameter services are up");
                    }
                    ros2 = Some(s);
                }
                Err(e) if !ros2_warned => {
                    tracing::warn!("no ROS 2 parameter services yet (trying again): {e:#}");
                    ros2_warned = true;
                }
                Err(_) => {}
            }
        }
        std::thread::sleep(Duration::from_millis(50));
        tick += 1;
        if tick.is_multiple_of(10) {
            let m = exec.metrics();
            let _ = zenoh::Wait::wait(session.put(&metrics_key, ruro_proto::encode(&m)));
        }
    }
    tracing::info!("group `{group}` stopping");
    drop(ros2);
    drop(control);
    exec.shutdown();
    Ok(())
}

trait WaitSession {
    fn wait_session(self) -> Result<zenoh::Session>;
}

impl<T: zenoh::Wait<To = zenoh::Result<zenoh::Session>>> WaitSession for T {
    fn wait_session(self) -> Result<zenoh::Session> {
        self.wait().map_err(|e| anyhow::anyhow!("{e}"))
    }
}
