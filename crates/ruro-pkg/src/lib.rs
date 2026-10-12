//! Node packages: scaffolding, `ruro.toml` manifests and host builds.
//!
//! A *node package* is an ordinary Rust crate that depends on `ruro`, defines
//! nodes with `#[derive(Node)]` and exposes a `link()` function. A project's
//! `ruro.toml` lists the packages its host should link; `ruro build` turns
//! that into a host binary for `ruro daemon --host`.

pub mod cli;
pub mod host;
pub mod manifest;
pub mod scaffold;

use std::path::{Path, PathBuf};

pub use host::{BuildOptions, build, generate};
pub use manifest::{MANIFEST, Manifest, Source};
pub use scaffold::{new_node, new_package};

/// The ruro checkout a binary was built from, if it runs from
/// `<checkout>/target/<profile>/`. Used as the default ruro source so that
/// packages created by a development build use the same tree.
pub fn detect_checkout(exe: &Path) -> Option<PathBuf> {
    let root = exe.parent()?.parent()?.parent()?;
    let looks_like_ruro = root.join("crates/ruro/Cargo.toml").is_file()
        && root.join("crates/ruro-host/Cargo.toml").is_file();
    looks_like_ruro.then(|| root.to_owned())
}

/// The ruro source new packages use: `$RURO_PATH`, else the checkout this
/// binary was built from, else the git repository.
pub fn default_ruro_source() -> Source {
    if let Some(p) = std::env::var_os("RURO_PATH") {
        return Source::path(p);
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| detect_checkout(&exe))
        .map(Source::path)
        .unwrap_or_else(|| Source::git(manifest::RURO_GIT))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_the_workspace_checkout() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let exe = root.join("target/debug/ruro");
        let found = detect_checkout(&exe).unwrap();
        assert!(found.join("crates/ruro-pkg").exists());
        assert!(detect_checkout(Path::new("/usr/bin/ruro")).is_none());
    }
}
