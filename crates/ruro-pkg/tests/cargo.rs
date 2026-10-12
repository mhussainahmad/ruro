//! Compiles generated code with the real toolchain.
//!
//! `scaffold_type_checks` runs `cargo check` on a freshly scaffolded package
//! (a few seconds once the shared target directory is warm). The full
//! end-to-end build of a host is slow and runs only with `RURO_PKG_E2E=1`.

use std::path::{Path, PathBuf};
use std::process::Command;

use ruro_pkg::{BuildOptions, Source};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// A fresh directory inside the workspace's target dir.
fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Shared target dir so dependencies compile once across runs.
fn shared_target() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("ruro-pkg-target")
}

fn cargo(dir: &Path, args: &[&str]) {
    let status = Command::new(env!("CARGO"))
        .args(args)
        .arg("--offline")
        .current_dir(dir)
        .env("CARGO_TARGET_DIR", shared_target())
        .status()
        .unwrap();
    assert!(
        status.success(),
        "cargo {args:?} failed in {}",
        dir.display()
    );
}

fn scaffold(name: &str) -> PathBuf {
    let root = workspace_root();
    let dir = scratch(name);
    ruro_pkg::new_package(&dir, name, &Source::path(&root)).unwrap();
    ruro_pkg::new_node(&dir, &format!("{}/clamp", name.replace('-', "_"))).unwrap();
    // Same dependency versions as the workspace, and no network needed.
    std::fs::copy(root.join("Cargo.lock"), dir.join("Cargo.lock")).unwrap();
    dir
}

#[test]
fn scaffold_type_checks() {
    if std::env::var_os("RURO_SKIP_CARGO_TESTS").is_some() {
        return;
    }
    let dir = scaffold("check-nodes");
    cargo(&dir, &["check", "--tests", "--quiet"]);
}

#[test]
fn scaffold_builds_a_working_host() {
    if std::env::var_os("RURO_PKG_E2E").is_none() {
        eprintln!("skipped: set RURO_PKG_E2E=1 to build a host end to end");
        return;
    }
    let root = workspace_root();
    let dir = scaffold("e2e-nodes");
    cargo(&dir, &["test", "--quiet"]);

    let bin = ruro_pkg::build(
        &dir.join("ruro.toml"),
        &BuildOptions {
            debug: std::env::var_os("RURO_PKG_E2E_RELEASE").is_none(),
            target_dir: Some(shared_target()),
            seed_lockfile: Some(root.join("Cargo.lock")),
            offline: true,
        },
    )
    .unwrap();
    assert!(bin.ends_with("e2e-nodes-host"));

    let out = Command::new(&bin).arg("describe").output().unwrap();
    assert!(out.status.success());
    let registry = String::from_utf8(out.stdout).unwrap();
    for kind in ["e2e_nodes/scale", "e2e_nodes/clamp", "std/sine"] {
        assert!(registry.contains(kind), "{kind} missing from {registry}");
    }
    let out = Command::new(&bin).arg("schemas").output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("std/Scalar"));
}
