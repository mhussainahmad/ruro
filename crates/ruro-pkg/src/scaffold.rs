//! `ruro new`: node package and node templates.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::manifest::{MANIFEST, Manifest, Project, Source, is_valid_crate_name, lib_name};

const CARGO_TOML: &str = r#"[package]
name = "{{name}}"
version = "0.1.0"
edition = "2024"
description = "Nodes for ruro"
publish = false

[dependencies]
ruro = {{ruro}}

# A standalone package. Remove this table to make it a member of an
# enclosing Cargo workspace.
[workspace]
"#;

const LIB_RS: &str = r#"//! {{name}}: nodes for ruro.
//!
//! Add a node with `ruro new node {{kind_prefix}}/<name>`, build a host that
//! links this package with `ruro build`, then start a daemon with the host
//! it prints: `ruro daemon --host <path>`.

mod {{module}};

pub use {{module}}::{{Struct}};

/// Forces this crate to be linked so its nodes are registered. Hosts call it.
pub fn link() {}
"#;

const NODE_RS: &str = r#"use ruro::prelude::*;

/// Multiplies every sample by a gain.
#[derive(Node)]
#[node(kind = "{{kind}}")]
pub struct {{Struct}} {
    #[input]
    input: Input<Scalar>,
    #[output]
    output: Output<Scalar>,
    /// Multiplier applied to every sample.
    #[param(default = 1.0, min = -100, max = 100)]
    gain: f64,
}

impl Node for {{Struct}} {
    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        while let Some(sample) = self.input.recv() {
            self.output.send(Scalar(sample.0 * self.gain))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use ruro::node::testing::Harness;
    use ruro::prelude::*;
    use ruro::serde_json::json;

    #[test]
    fn scales_samples() {
        let mut node = Harness::new("{{kind}}", json!({ "gain": 2.0 })).unwrap();
        node.push("input", Scalar(1.5)).unwrap();
        node.step().unwrap();
        assert_eq!(node.take::<Scalar>("output").unwrap(), [Scalar(3.0)]);
    }
}
"#;

const GITIGNORE: &str = "/target\n/.ruro\n";

/// Fills `{{key}}` placeholders.
fn render(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = template.to_owned();
    for (k, v) in vars {
        out = out.replace(&format!("{{{{{k}}}}}"), v);
    }
    out
}

/// `my-node` → `my_node`.
fn module_name(short: &str) -> String {
    short.replace('-', "_")
}

/// `my-node` → `MyNode`.
fn struct_name(short: &str) -> String {
    short
        .split(['-', '_'])
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut c = p.chars();
            c.next()
                .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
                .unwrap_or_default()
        })
        .collect()
}

/// Splits and checks a node kind such as `my_pkg/threshold`.
pub fn parse_kind(kind: &str) -> Result<(&str, &str)> {
    let Some((prefix, short)) = kind.split_once('/') else {
        bail!("node kind `{kind}` must look like `package/name`");
    };
    let ok = |s: &str| {
        s.chars().next().is_some_and(|c| c.is_ascii_lowercase())
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    };
    if !ok(prefix) || !ok(short) {
        bail!("node kind `{kind}`: use lowercase letters, digits, `-` and `_`");
    }
    Ok((prefix, short))
}

/// The `ruro` dependency entry for a package.
fn ruro_dependency(ruro: &Source) -> String {
    ruro.dependency("ruro", ruro.path.as_ref().map(|_| "crates/ruro"))
}

fn write_new(path: &Path, contents: &str, created: &mut Vec<PathBuf>) -> Result<()> {
    if path.exists() {
        bail!("{} already exists", path.display());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))?;
    created.push(path.to_owned());
    Ok(())
}

/// Creates a node package in `dir` (which must not exist or be empty): a
/// crate with one example node and its test, plus a `ruro.toml` that links it.
pub fn new_package(dir: &Path, name: &str, ruro: &Source) -> Result<Vec<PathBuf>> {
    if !is_valid_crate_name(name) {
        bail!("`{name}` is not a valid package name (letters, digits, `-`, `_`)");
    }
    if dir.exists() && std::fs::read_dir(dir)?.next().is_some() {
        bail!("{} is not empty", dir.display());
    }
    let kind_prefix = lib_name(name).to_ascii_lowercase();
    let kind = format!("{kind_prefix}/scale");
    let vars = [
        ("name", name),
        ("kind", kind.as_str()),
        ("kind_prefix", kind_prefix.as_str()),
        ("module", "scale"),
        ("Struct", "Scale"),
    ];
    let ruro_dep = ruro_dependency(ruro);
    let mut created = Vec::new();
    write_new(
        &dir.join("Cargo.toml"),
        &render(CARGO_TOML, &[("name", name), ("ruro", &ruro_dep)]),
        &mut created,
    )?;
    write_new(
        &dir.join("src/lib.rs"),
        &render(LIB_RS, &vars),
        &mut created,
    )?;
    write_new(
        &dir.join("src/scale.rs"),
        &render(NODE_RS, &vars),
        &mut created,
    )?;
    write_new(&dir.join(".gitignore"), GITIGNORE, &mut created)?;
    let manifest = Manifest {
        project: Project {
            name: name.to_owned(),
        },
        ruro: ruro.clone(),
        packages: [(name.to_owned(), Source::path("."))].into(),
        ..Default::default()
    };
    write_new(
        &dir.join(MANIFEST),
        &format!(
            "# Packages linked into this project's host. Build it with `ruro build`.\n\n{}",
            manifest.to_toml()
        ),
        &mut created,
    )?;
    Ok(created)
}

/// Adds a node module to the package in `pkg_dir` and declares it in `src/lib.rs`.
pub fn new_node(pkg_dir: &Path, kind: &str) -> Result<Vec<PathBuf>> {
    let (_, short) = parse_kind(kind)?;
    let lib = pkg_dir.join("src/lib.rs");
    let source = std::fs::read_to_string(&lib)
        .with_context(|| format!("reading {} (is this a node package?)", lib.display()))?;
    let module = module_name(short);
    let strukt = struct_name(short);
    let vars = [
        ("kind", kind),
        ("module", module.as_str()),
        ("Struct", strukt.as_str()),
    ];
    let mut created = Vec::new();
    write_new(
        &pkg_dir.join(format!("src/{module}.rs")),
        &render(NODE_RS, &vars),
        &mut created,
    )?;
    std::fs::write(&lib, declare_module(&source, &module, &strukt))?;
    Ok(created)
}

/// Inserts `mod m;` and `pub use m::S;` after the last existing ones, or
/// before `pub fn link` if there are none.
fn declare_module(source: &str, module: &str, strukt: &str) -> String {
    let mut lines: Vec<String> = source.lines().map(str::to_owned).collect();
    let last = |lines: &[String], prefix: &str| lines.iter().rposition(|l| l.starts_with(prefix));
    let link = lines
        .iter()
        .position(|l| l.contains("pub fn link"))
        .map(|i| {
            // Keep the doc comment attached to `link`.
            let mut i = i;
            while i > 0 && lines[i - 1].trim_start().starts_with("///") {
                i -= 1;
            }
            i
        })
        .unwrap_or(lines.len());
    let at = last(&lines, "mod ").map_or(link, |i| i + 1);
    lines.insert(at, format!("mod {module};"));
    let at = last(&lines, "pub use ").map_or_else(
        || {
            lines.insert(at + 1, String::new());
            at + 2
        },
        |i| i + 1,
    );
    lines.insert(at, format!("pub use {module}::{strukt};"));
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parses(path: &Path) {
        let text = std::fs::read_to_string(path).unwrap();
        if let Err(e) = syn::parse_file(&text) {
            panic!("{} does not parse: {e}\n{text}", path.display());
        }
    }

    #[test]
    fn package_template_is_valid_rust_and_toml() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path().join("my-nodes");
        let files = new_package(&pkg, "my-nodes", &Source::path("/opt/ruro")).unwrap();
        assert_eq!(files.len(), 5);
        for f in files
            .iter()
            .filter(|f| f.extension().is_some_and(|e| e == "rs"))
        {
            parses(f);
        }
        let cargo: toml::Table =
            toml::from_str(&std::fs::read_to_string(pkg.join("Cargo.toml")).unwrap()).unwrap();
        assert_eq!(
            cargo["dependencies"]["ruro"]["path"].as_str(),
            Some("/opt/ruro/crates/ruro")
        );
        let manifest = Manifest::load(&pkg.join(MANIFEST)).unwrap();
        assert_eq!(manifest.packages["my-nodes"], Source::path("."));
        assert_eq!(manifest.host_name(), "my-nodes-host");
        let node = std::fs::read_to_string(pkg.join("src/scale.rs")).unwrap();
        assert!(node.contains("kind = \"my_nodes/scale\""));
        // Refuses to overwrite.
        assert!(new_package(&pkg, "my-nodes", &Source::default()).is_err());
    }

    #[test]
    fn new_node_adds_a_module() {
        let dir = tempfile::tempdir().unwrap();
        new_package(dir.path(), "bot", &Source::git("https://x/ruro")).unwrap();
        new_node(dir.path(), "bot/low-pass").unwrap();
        new_node(dir.path(), "bot/edge_detect").unwrap();
        let lib = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
        assert!(
            lib.contains("mod scale;\nmod low_pass;\nmod edge_detect;\n"),
            "{lib}"
        );
        assert!(lib.contains("pub use low_pass::LowPass;"));
        assert!(lib.contains("pub use edge_detect::EdgeDetect;"));
        parses(&dir.path().join("src/lib.rs"));
        parses(&dir.path().join("src/low_pass.rs"));
        assert!(new_node(dir.path(), "bot/low-pass").is_err(), "exists");
        assert!(new_node(dir.path(), "Bad Kind").is_err());
    }

    #[test]
    fn declares_module_in_a_lib_without_modules() {
        let out = declare_module("//! doc\n\n/// Links.\npub fn link() {}\n", "a", "A");
        assert_eq!(
            out,
            "//! doc\n\nmod a;\n\npub use a::A;\n/// Links.\npub fn link() {}\n"
        );
    }

    #[test]
    fn names() {
        assert_eq!(struct_name("low-pass_filter"), "LowPassFilter");
        assert!(parse_kind("pkg/node-1").is_ok());
        assert!(parse_kind("pkg").is_err());
        assert!(parse_kind("Pkg/x").is_err());
    }
}
