//! Nodes implemented by other programs, in any language.
//!
//! A manifest (YAML) declares a node type: its kind, the command that runs
//! it, its typed inputs and outputs and its parameters. Hosts register the
//! manifests listed in `RURO_PROCESS_NODES` when they start, so the types
//! appear in `ruro nodes`, `ruro check` and the studio like any other.
//!
//! Each node instance runs its program and exchanges one JSON object per line
//! with it, prefixed with `@ruro ` (other output is logged): the program's
//! hello, then a request from the host and a reply from the program for
//! `start`, every `step`, `param` changes, the `estop` and `stop`. The book's
//! "Nodes in other languages" chapter specifies the protocol.

mod child;
mod manifest;
mod message;
mod node;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Once};

use anyhow::{Context, Result};

pub use manifest::{Manifest, Transport};
pub use message::{JsonMessage, SUPPORTED};

/// The protocol version this host speaks.
pub const PROTOCOL: u64 = 1;

/// Marks protocol lines, in both directions.
pub const PREFIX: &str = "@ruro ";

/// Manifests, or directories of `*.node.yaml` manifests, separated by `:`.
pub const ENV: &str = "RURO_PROCESS_NODES";

/// Registers the node types of the manifests in [`ENV`]. Hosts call it when
/// they start; later calls do nothing. A manifest that cannot be read is
/// reported on standard error and skipped.
pub fn link() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let Some(list) = std::env::var_os(ENV) else {
            return;
        };
        for path in std::env::split_paths(&list) {
            if path.as_os_str().is_empty() {
                continue;
            }
            if let Err(e) = register_path(&path) {
                eprintln!("ruro-host: {ENV}: {}: {e:#}", path.display());
            }
        }
    });
}

/// Registers the manifest at `path`, or every `*.node.yaml` (or `.yml`)
/// manifest in the directory `path`. Returns the kinds registered.
pub fn register_path(path: &Path) -> Result<Vec<String>> {
    let files: Vec<PathBuf> = if path.is_dir() {
        let mut files: Vec<PathBuf> = std::fs::read_dir(path)
            .context("reading the directory")?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                name.ends_with(".node.yaml") || name.ends_with(".node.yml")
            })
            .collect();
        files.sort();
        files
    } else {
        vec![path.to_path_buf()]
    };
    let mut kinds = Vec::new();
    for file in files {
        let m = Manifest::load(&file).with_context(|| format!("{}", file.display()))?;
        kinds.push(m.descriptor.kind.clone());
        register(m);
    }
    Ok(kinds)
}

/// Registers the node type `manifest` declares.
pub fn register(manifest: Manifest) {
    let manifest = Arc::new(manifest);
    let descriptor = manifest.descriptor.clone();
    ruro_node::register_dynamic(
        descriptor,
        Arc::new(move |b| node::ProcessNode::build(manifest.clone(), b)),
    );
}
