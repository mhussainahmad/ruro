//! `ruro new` and `ruro build`.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};

use crate::{BuildOptions, MANIFEST, Source};

/// Arguments of `ruro new`.
#[derive(Debug, Args)]
pub struct NewArgs {
    #[command(subcommand)]
    what: NewCmd,
}

#[derive(Debug, Subcommand)]
enum NewCmd {
    /// Create a node package: a crate with an example node, its test and a ruro.toml.
    Package {
        /// Package (crate) name.
        name: String,
        /// Directory to create (default: ./<name>).
        #[arg(long)]
        path: Option<PathBuf>,
        /// Use a local ruro checkout instead of the git repository.
        #[arg(long, conflicts_with = "ruro_git")]
        ruro_path: Option<PathBuf>,
        /// Use ruro from this git URL.
        #[arg(long)]
        ruro_git: Option<String>,
    },
    /// Add a node to the package in the current directory.
    Node {
        /// Node kind, e.g. `my_pkg/threshold`.
        kind: String,
        /// Package directory (default: current directory).
        #[arg(long, default_value = ".")]
        package: PathBuf,
    },
}

/// Runs `ruro new`.
pub fn run_new(args: NewArgs) -> Result<()> {
    match args.what {
        NewCmd::Package {
            name,
            path,
            ruro_path,
            ruro_git,
        } => {
            let dir = path.unwrap_or_else(|| PathBuf::from(&name));
            let ruro = match (ruro_path, ruro_git) {
                (Some(p), _) => Source::path(std::path::absolute(p)?),
                (None, Some(g)) => Source::git(g),
                (None, None) => crate::default_ruro_source(),
            };
            let created = crate::new_package(&dir, &name, &ruro)?;
            for f in &created {
                println!("created {}", f.display());
            }
            println!(
                "\nnext:\n  cd {}\n  cargo test        # run the example node's test\n  ruro build        # build a host that links this package",
                dir.display()
            );
            Ok(())
        }
        NewCmd::Node { kind, package } => {
            for f in crate::new_node(&package, &kind)? {
                println!("created {}", f.display());
            }
            println!("declared in {}", package.join("src/lib.rs").display());
            Ok(())
        }
    }
}

/// Arguments of `ruro build`.
#[derive(Debug, Args)]
pub struct BuildArgs {
    /// Project manifest.
    #[arg(long, default_value = MANIFEST)]
    manifest: PathBuf,
    /// Build without optimizations.
    #[arg(long)]
    debug: bool,
    /// Cargo target directory (default: .ruro/host/target).
    #[arg(long)]
    target_dir: Option<PathBuf>,
    /// Lockfile to start from when the host has none yet.
    #[arg(long)]
    lockfile: Option<PathBuf>,
    /// Do not access the network.
    #[arg(long)]
    offline: bool,
}

/// Runs `ruro build`.
pub fn run_build(args: BuildArgs) -> Result<()> {
    let bin = crate::build(
        &args.manifest,
        &BuildOptions {
            debug: args.debug,
            target_dir: args.target_dir,
            seed_lockfile: args.lockfile,
            offline: args.offline,
        },
    )?;
    println!("built {}", bin.display());
    println!("run graphs with its nodes:\n  ruro daemon --host {}", bin.display());
    Ok(())
}
