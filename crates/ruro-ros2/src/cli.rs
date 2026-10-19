//! `ruro ros2`: inspect a ROS 2 system and generate message types.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use crate::idl::TypeName;
use crate::rmw::{DEFAULT_ROUTER, EndpointConfig};

/// Arguments of `ruro ros2`.
#[derive(Debug, Args)]
pub struct Ros2Args {
    #[command(subcommand)]
    cmd: Ros2Cmd,
}

#[derive(Debug, Subcommand)]
enum Ros2Cmd {
    /// List ROS 2 topics announced through rmw_zenoh liveliness tokens.
    Topics {
        /// Zenoh router to query (rmw_zenohd or a ruro daemon).
        #[arg(long, default_value = DEFAULT_ROUTER)]
        router: String,
        /// ROS domain id (default: $ROS_DOMAIN_ID or 0).
        #[arg(long)]
        domain: Option<u32>,
        /// How long to wait for answers, in milliseconds.
        #[arg(long, default_value_t = 1000)]
        timeout: u64,
        /// Print JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Generate Rust types from .msg/.srv definitions.
    Gen {
        /// Package directories (named after the package, containing msg/ and/or srv/).
        #[arg(required = true)]
        packages: Vec<PathBuf>,
        /// Output file (default: stdout).
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Module path the output will live at.
        #[arg(long, default_value = "crate::ros")]
        root: String,
    },
    /// Print the RIHS01 hash of a message type, e.g. `geometry_msgs/msg/Twist`.
    Hash {
        ty: String,
        /// Extra package directories to load besides the bundled ones.
        packages: Vec<PathBuf>,
        /// Also print the hashed type description.
        #[arg(long)]
        verbose: bool,
    },
}

/// Runs `ruro ros2`.
pub fn run(args: Ros2Args) -> Result<()> {
    match args.cmd {
        Ros2Cmd::Topics {
            router,
            domain,
            timeout,
            json,
        } => {
            let cfg = EndpointConfig {
                connect: vec![router.clone()],
                ..Default::default()
            };
            let domain = domain.unwrap_or(cfg.domain);
            let session = zenoh::Wait::wait(zenoh::open(cfg.zenoh_config()?))
                .map_err(|e| anyhow::anyhow!("cannot reach Zenoh at {router}: {e}"))?;
            let topics =
                crate::discovery::list_topics(&session, domain, Duration::from_millis(timeout))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&topics)?);
            } else if topics.is_empty() {
                eprintln!("no ROS 2 topics in domain {domain} via {router}");
            } else {
                let w = topics
                    .iter()
                    .map(|t| t.name.len())
                    .max()
                    .unwrap_or(5)
                    .max(5);
                println!("{:<w$}  {:<36}  PUB  SUB", "TOPIC", "TYPE");
                for t in &topics {
                    println!(
                        "{:<w$}  {:<36}  {:>3}  {:>3}",
                        t.name, t.ros_type, t.publishers, t.subscribers
                    );
                }
            }
            Ok(())
        }
        Ros2Cmd::Gen {
            packages,
            output,
            root,
        } => {
            let mut interfaces = crate::bundled_interfaces();
            let mut names = Vec::new();
            for dir in &packages {
                // Drop bundled definitions of a package being regenerated.
                let name = dir
                    .canonicalize()
                    .with_context(|| format!("{}", dir.display()))?
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                interfaces.remove_package(&name);
                interfaces.load_package(&dir.canonicalize()?)?;
                names.push(name);
            }
            let externs: BTreeMap<String, String> = crate::bundled_packages()
                .into_iter()
                .filter(|p| !names.contains(p))
                .map(|p| (p.clone(), format!("ruro_ros2::msgs::{p}")))
                .collect();
            let code = crate::codegen::generate(
                &interfaces,
                &crate::codegen::Options {
                    packages: names,
                    root,
                    runtime: "ruro_ros2".into(),
                    externs,
                },
            )?;
            match output {
                Some(path) => {
                    std::fs::write(&path, code)?;
                    eprintln!("wrote {}", path.display());
                }
                None => print!("{code}"),
            }
            Ok(())
        }
        Ros2Cmd::Hash {
            ty,
            packages,
            verbose,
        } => {
            let mut interfaces = crate::bundled_interfaces();
            for dir in &packages {
                interfaces.load_package(&dir.canonicalize()?)?;
            }
            let Some(name) = TypeName::parse(&ty) else {
                bail!("expected a type like `pkg/msg/Type`, got `{ty}`");
            };
            let hash = crate::hash::type_hash(&interfaces, &name)
                .map_err(|missing| anyhow::anyhow!("unknown message type {missing}"))?;
            println!("{hash}");
            if verbose {
                println!(
                    "{}",
                    crate::hash::hashable_description(&interfaces, &name).unwrap_or_default()
                );
            }
            Ok(())
        }
    }
}
