//! `ruro schema`: print, compare and generate code from message schemas.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use ruro_msg::MessageSchema;

/// Arguments of `ruro schema`.
#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct SchemaArgs {
    #[command(subcommand)]
    cmd: Option<SchemaCmd>,
    /// Message type to print as JSON, e.g. `std/Pose` (or just `Pose`).
    ty: Option<String>,
    /// Print every schema as a JSON array.
    #[arg(long)]
    all: bool,
    /// Read schemas from a host binary built with `ruro build` (runs `<host> schemas`)
    /// instead of the types linked into this CLI.
    #[arg(long)]
    host: Option<PathBuf>,
    /// Generate Python dataclasses for the standard messages.
    #[arg(long)]
    python: bool,
}

#[derive(Debug, Subcommand)]
enum SchemaCmd {
    /// Compare two schema files and report wire compatibility. Exits with an
    /// error if any change is breaking.
    Diff {
        /// Schema JSON of the old version (one schema or an array).
        old: PathBuf,
        /// Schema JSON of the new version.
        new: PathBuf,
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
    },
}

/// Runs `ruro schema`.
pub fn run(args: SchemaArgs) -> Result<()> {
    if let Some(SchemaCmd::Diff { old, new, json }) = args.cmd {
        return diff_files(&old, &new, json);
    }
    if args.python {
        print!("{}", crate::python::generate_std());
        return Ok(());
    }
    let schemas = match &args.host {
        Some(host) => from_host(host)?,
        None => crate::linked(),
    };
    if args.all {
        println!("{}", serde_json::to_string_pretty(&schemas)?);
    } else if let Some(ty) = &args.ty {
        let schema = crate::find(&schemas, ty)?;
        println!("{}", crate::to_json(std::slice::from_ref(schema)));
    } else {
        for s in &schemas {
            let note = if s.schema.is_complete() {
                ""
            } else {
                "  (partly opaque)"
            };
            println!("{:<24} {}{note}", s.name, s.hash);
        }
    }
    Ok(())
}

/// Schemas reported by a host binary.
pub fn from_host(host: &Path) -> Result<Vec<MessageSchema>> {
    let out = std::process::Command::new(host)
        .arg("schemas")
        .output()
        .with_context(|| format!("running `{} schemas`", host.display()))?;
    if !out.status.success() {
        bail!(
            "`{} schemas` failed (hosts built with `ruro build` support it): {}",
            host.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    crate::from_json(&String::from_utf8_lossy(&out.stdout)).context("parsing host schemas")
}

fn diff_files(old: &Path, new: &Path, json: bool) -> Result<()> {
    let reports = crate::diff_sets(&crate::load(old)?, &crate::load(new)?);
    if json {
        println!("{}", serde_json::to_string_pretty(&reports)?);
    } else {
        for r in &reports {
            print!("{r}");
        }
    }
    let breaking: Vec<&str> = reports
        .iter()
        .filter(|r| r.is_breaking())
        .map(|r| r.name.as_str())
        .collect();
    if !breaking.is_empty() {
        bail!(
            "breaking change in {} (postcard encodes fields by position: adding, removing, \
             reordering or retyping a field breaks existing peers)",
            breaking.join(", ")
        );
    }
    Ok(())
}
