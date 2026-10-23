//! Node manifests: a YAML file that declares a node type implemented by a
//! program.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use indexmap::IndexMap;
use ruro_graph::{NodeDescriptor, ParamDesc, ParamKind, PortDesc};
use serde::Deserialize;
use serde_json::Value;

use crate::message;

/// How the host and the program exchange protocol lines.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    /// The program's standard input and output.
    #[default]
    Stdio,
    /// A TCP connection to the host on the loopback interface, for programs
    /// that cannot read their standard input (MATLAB).
    Tcp,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    kind: String,
    #[serde(default)]
    doc: String,
    command: Vec<String>,
    #[serde(default)]
    transport: Transport,
    #[serde(default)]
    rate: Option<f64>,
    #[serde(default)]
    inputs: IndexMap<String, String>,
    #[serde(default)]
    outputs: IndexMap<String, String>,
    #[serde(default)]
    params: IndexMap<String, RawParam>,
    #[serde(default)]
    env: IndexMap<String, String>,
    #[serde(default)]
    start_timeout: Option<f64>,
    #[serde(default)]
    step_timeout: Option<f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawParam {
    default: Value,
    #[serde(default)]
    kind: Option<ParamKind>,
    #[serde(default)]
    min: Option<f64>,
    #[serde(default)]
    max: Option<f64>,
    #[serde(default)]
    doc: String,
}

/// A node type implemented by a program, as declared by its manifest.
#[derive(Debug, Clone)]
pub struct Manifest {
    pub descriptor: NodeDescriptor,
    /// The program and its arguments.
    pub command: Vec<String>,
    pub transport: Transport,
    /// Extra environment variables; the program also inherits the host's.
    pub env: Vec<(String, String)>,
    /// The directory the program runs in: the manifest's.
    pub dir: PathBuf,
    /// How long the program gets to start and say hello.
    pub start_timeout: Duration,
    /// How long the program gets to answer each request after that.
    pub step_timeout: Duration,
}

const DEFAULT_START: Duration = Duration::from_secs(30);
const DEFAULT_STEP: Duration = Duration::from_secs(5);

impl Manifest {
    /// Reads the manifest at `path`.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).context("reading the manifest")?;
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let dir = if dir.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            dir
        };
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        Self::parse(&text, &dir, Some(path))
    }

    /// Parses a manifest whose program runs in `dir`.
    pub fn parse(text: &str, dir: &Path, source: Option<&Path>) -> Result<Self> {
        let raw: Raw = serde_saphyr::from_str(text).map_err(|e| anyhow::anyhow!("{e}"))?;
        ensure!(
            raw.kind.contains('/') && !raw.kind.starts_with('/') && !raw.kind.ends_with('/'),
            "kind `{}` needs a namespace, e.g. `acme/{}`",
            raw.kind,
            raw.kind
        );
        ensure!(!raw.command.is_empty(), "`command` is empty");
        if let Some(r) = raw.rate {
            ensure!(r.is_finite() && r > 0.0, "`rate` must be a positive number");
        }
        let ports = |ports: IndexMap<String, String>, what: &str| -> Result<Vec<PortDesc>> {
            ports
                .into_iter()
                .map(|(name, ty)| {
                    ensure!(
                        message::supported(&ty),
                        "{what} `{name}`: message type `{ty}` is not supported by process nodes (supported: {})",
                        message::SUPPORTED.join(", ")
                    );
                    Ok(PortDesc { name, ty })
                })
                .collect()
        };
        let inputs = ports(raw.inputs, "input")?;
        let outputs = ports(raw.outputs, "output")?;
        let params = raw
            .params
            .into_iter()
            .map(|(name, p)| {
                let kind = match p.kind {
                    Some(k) => k,
                    None => infer_kind(&p.default)
                        .with_context(|| format!("parameter `{name}`: give its `kind`"))?,
                };
                let desc = ParamDesc {
                    name: name.clone(),
                    kind,
                    default: p.default,
                    min: p.min,
                    max: p.max,
                    doc: p.doc,
                };
                desc.check(&desc.default)
                    .map_err(|e| anyhow::anyhow!("default of parameter `{name}`: {e}"))?;
                Ok(desc)
            })
            .collect::<Result<Vec<_>>>()?;
        let timeout = |t: Option<f64>, default: Duration, what: &str| -> Result<Duration> {
            match t {
                None => Ok(default),
                Some(s) if s.is_finite() && s > 0.0 => Ok(Duration::from_secs_f64(s)),
                Some(_) => bail!("`{what}` must be a positive number of seconds"),
            }
        };
        Ok(Self {
            descriptor: NodeDescriptor {
                kind: raw.kind,
                doc: raw.doc,
                default_rate: raw.rate,
                inputs,
                outputs,
                params,
                source: source.map(|p| p.display().to_string()),
            },
            command: raw.command,
            transport: raw.transport,
            env: raw.env.into_iter().collect(),
            dir: dir.to_path_buf(),
            start_timeout: timeout(raw.start_timeout, DEFAULT_START, "start_timeout")?,
            step_timeout: timeout(raw.step_timeout, DEFAULT_STEP, "step_timeout")?,
        })
    }

    /// The program to run: a relative path (one with a `/`) is relative to
    /// the manifest's directory; a bare name is looked up on `PATH`.
    pub fn program(&self) -> PathBuf {
        let p = Path::new(&self.command[0]);
        if p.is_relative() && self.command[0].contains('/') {
            self.dir.join(p)
        } else {
            p.to_path_buf()
        }
    }
}

fn infer_kind(v: &Value) -> Result<ParamKind> {
    Ok(match v {
        Value::Bool(_) => ParamKind::Bool,
        Value::String(_) => ParamKind::String,
        Value::Number(n) if n.is_i64() || n.is_u64() => ParamKind::Int,
        Value::Number(_) => ParamKind::Float,
        other => bail!("cannot tell the kind of {other}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCALE: &str = r#"
kind: acme/scale
doc: Multiplies by k.
command: [python3, scale.py, --verbose]
inputs: { input: std/Scalar }
outputs: { output: std/Scalar }
params:
  k: { default: 2.0, min: 0, max: 10, doc: gain }
  steps: { default: 3 }
  name: { default: hello }
env: { MODE: fast }
step_timeout: 1.5
"#;

    #[test]
    fn a_manifest_becomes_a_node_type() {
        let m = Manifest::parse(SCALE, Path::new("/opt/acme"), None).unwrap();
        let d = &m.descriptor;
        assert_eq!(d.kind, "acme/scale");
        assert_eq!(
            d.inputs,
            [PortDesc {
                name: "input".into(),
                ty: "std/Scalar".into()
            }]
        );
        let kinds: Vec<ParamKind> = d.params.iter().map(|p| p.kind).collect();
        assert_eq!(kinds, [ParamKind::Float, ParamKind::Int, ParamKind::String]);
        assert_eq!(d.params[0].max, Some(10.0));
        assert_eq!(m.transport, Transport::Stdio);
        assert_eq!(m.step_timeout, Duration::from_millis(1500));
        assert_eq!(m.start_timeout, DEFAULT_START);
        assert_eq!(m.env, [("MODE".to_owned(), "fast".to_owned())]);
        assert_eq!(m.program(), Path::new("python3"));
    }

    #[test]
    fn relative_programs_are_found_next_to_the_manifest() {
        let text = "kind: a/b\ncommand: [./bin/node]\n";
        let m = Manifest::parse(text, Path::new("/opt/acme"), None).unwrap();
        assert_eq!(m.program(), Path::new("/opt/acme/./bin/node"));
    }

    #[test]
    fn mistakes_are_explained() {
        let bad = |text: &str| {
            format!(
                "{:#}",
                Manifest::parse(text, Path::new("."), None).unwrap_err()
            )
        };
        assert!(bad("kind: scale\ncommand: [x]\n").contains("needs a namespace"));
        assert!(bad("kind: a/b\ncommand: []\n").contains("`command` is empty"));
        let e = bad("kind: a/b\ncommand: [x]\ninputs: { i: acme/Thing }\n");
        assert!(
            e.contains("not supported") && e.contains("std/Image"),
            "{e}"
        );
        let e = bad("kind: a/b\ncommand: [x]\nparams: { k: { default: 20, max: 10 } }\n");
        assert!(e.contains("default of parameter `k`"), "{e}");
        assert!(bad("kind: a/b\ncommand: [x]\nrat: 3\n").contains("rat"));
    }
}
