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
mod stats;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ruro_graph::Graph;
use ruro_proto::Keys;

pub use executor::{Executor, Remote};

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
    },
}

/// Descriptors of every node type linked into this binary, as JSON.
pub fn describe() -> String {
    ruro_nodes_std::link();
    ruro_nodes_hw::link();
    ruro_nodes_sim::link();
    ruro_nodes_ml::link();
    ruro_nodes_ros2::link();
    serde_json::to_string(&ruro_node::registry()).expect("registry is serializable")
}

/// Entry point for host binaries.
pub fn main() {
    ruro_nodes_std::link();
    ruro_nodes_hw::link();
    ruro_nodes_sim::link();
    ruro_nodes_ml::link();
    ruro_nodes_ros2::link();
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
        } => run(&graph, &group, &daemon, &connect),
    };
    if let Err(e) = result {
        eprintln!("ruro-host: {e:#}");
        std::process::exit(1);
    }
}

fn run(graph_path: &PathBuf, group: &str, daemon: &str, endpoint: &str) -> Result<()> {
    let graph =
        Graph::load(graph_path).with_context(|| format!("loading {}", graph_path.display()))?;
    let keys = Keys::new(daemon);

    let session = zenoh::open(ruro_proto::client_config(endpoint)?)
        .wait_session()
        .with_context(|| format!("connecting to daemon at {endpoint}"))?;
    logging::init(&session, &keys, group);

    let stop = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, stop.clone())?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, stop.clone())?;

    let mut exec = Executor::start(&graph, group, Remote::Shm(keys.clone()))?;
    let _control = control::attach(&session, &keys, &exec)?;
    tracing::info!(
        "group `{group}` running {} node(s)",
        exec.node_ids().count()
    );

    let metrics_key = keys.metrics(group);
    let mut tick = 0u32;
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(50));
        tick += 1;
        if tick.is_multiple_of(10) {
            let m = exec.metrics();
            let _ = zenoh::Wait::wait(session.put(&metrics_key, ruro_proto::encode(&m)));
        }
    }
    tracing::info!("group `{group}` stopping");
    exec.stop();
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
