//! `ruro-host-py` as a host binary: what it describes and where it finds
//! Python nodes.

use std::path::Path;

fn repo() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

#[test]
fn host_describes_python_nodes() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ruro-host-py"))
        .arg("describe")
        .env(
            "RURO_PY_MODULES",
            repo().join("examples/python/threshold.py"),
        )
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let registry: ruro_graph::Registry = serde_json::from_slice(&out.stdout).unwrap();
    let t = registry
        .get("py/threshold")
        .expect("python node registered");
    assert_eq!(t.inputs[0].ty, "std/Scalar");
    assert_eq!(t.params[0].name, "limit");
    assert!(registry.get("tf/lookup").is_some(), "tf nodes are linked");
    assert_eq!(
        registry.schema_hash("std/Flag"),
        Some(
            <ruro_msg::Flag as ruro_msg::Message>::message_schema()
                .hash
                .as_str()
        )
    );

    // A module that fails to import is reported but does not take the host down.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ruro-host-py"))
        .arg("describe")
        .env("RURO_PY_MODULES", "no_such_module_anywhere")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no_such_module_anywhere"));
}

#[test]
fn installed_distributions_provide_nodes_through_entry_points() {
    // A minimal installed distribution: a module plus dist-info metadata
    // declaring it in the `ruro.nodes` entry point group.
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("py-entry-points");
    let _ = std::fs::remove_dir_all(&dir);
    let info = dir.join("acme_nodes-0.1.dist-info");
    std::fs::create_dir_all(&info).unwrap();
    std::fs::write(
        dir.join("acme_nodes.py"),
        "import ruro\n\n\
         @ruro.node(kind=\"acme/double\", inputs={\"x\": ruro.Scalar}, outputs={\"y\": ruro.Scalar})\n\
         class Double:\n    \
             def step(self, ctx):\n        \
                 for s in self.inputs.x:\n            \
                     self.outputs.y.send(ruro.Scalar(2 * s.value))\n",
    )
    .unwrap();
    std::fs::write(
        info.join("METADATA"),
        "Metadata-Version: 2.1\nName: acme-nodes\nVersion: 0.1\n",
    )
    .unwrap();
    std::fs::write(
        info.join("entry_points.txt"),
        "[ruro.nodes]\nacme = acme_nodes\n",
    )
    .unwrap();

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ruro-host-py"))
        .arg("describe")
        .env("RURO_PY_PATH", &dir)
        .env_remove("RURO_PY_MODULES")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let registry: ruro_graph::Registry = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        registry.get("acme/double").is_some(),
        "{:?}",
        registry.nodes.keys()
    );
}
