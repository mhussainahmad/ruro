//! `ruro.toml`: which node packages a project's host links, and where ruro
//! itself comes from.
//!
//! ```toml
//! [project]
//! name = "my-robot"
//!
//! [ruro]                      # source of the ruro crates (one of path, git, version)
//! git = "https://github.com/mhussainahmad/ruro"
//!
//! [packages]
//! my_nodes = { path = "." }
//! vision = { git = "https://example.com/vision.git", rev = "4f2a9c1" }
//! lidar = { version = "0.3", package = "acme-lidar-nodes" }
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// Default file name of a project manifest.
pub const MANIFEST: &str = "ruro.toml";

/// Where ruro's repository lives when no source is configured.
pub const RURO_GIT: &str = "https://github.com/mhussainahmad/ruro";

/// A parsed `ruro.toml`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    #[serde(default)]
    pub project: Project,
    /// Source of the ruro crates. Defaults to the ruro git repository.
    #[serde(default)]
    pub ruro: Source,
    /// Node packages to link, keyed by crate name.
    #[serde(default)]
    pub packages: BTreeMap<String, Source>,
    #[serde(default)]
    pub host: HostConfig,
}

/// `[project]`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    #[serde(default)]
    pub name: String,
}

/// `[host]`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    /// Binary name; defaults to `<project>-host`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Where a crate comes from, in Cargo dependency terms.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// Local directory. For `[ruro]`, a ruro checkout (the workspace root).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,
    /// crates.io version requirement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Crate name, when it differs from the key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
}

impl Source {
    /// A local path source.
    pub fn path(p: impl Into<PathBuf>) -> Self {
        Self {
            path: Some(p.into()),
            ..Default::default()
        }
    }

    /// A git source.
    pub fn git(url: impl Into<String>) -> Self {
        Self {
            git: Some(url.into()),
            ..Default::default()
        }
    }

    fn check(&self, what: &str) -> Result<()> {
        let kinds = [
            self.path.is_some(),
            self.git.is_some(),
            self.version.is_some(),
        ];
        if kinds.iter().filter(|k| **k).count() > 1 {
            bail!("{what}: set only one of `path`, `git` and `version`");
        }
        let refs = [&self.branch, &self.tag, &self.rev];
        if self.git.is_none() && refs.iter().any(|r| r.is_some()) {
            bail!("{what}: `branch`, `tag` and `rev` need `git`");
        }
        if refs.iter().filter(|r| r.is_some()).count() > 1 {
            bail!("{what}: set only one of `branch`, `tag` and `rev`");
        }
        Ok(())
    }

    /// This source with a relative path resolved against `base`.
    pub fn resolved(&self, base: &Path) -> Source {
        let mut s = self.clone();
        if let Some(p) = &s.path
            && p.is_relative()
        {
            let joined = base.join(p);
            s.path = Some(joined.canonicalize().unwrap_or(joined));
        }
        s
    }

    /// The Cargo dependency table for crate `krate` from this source. For a
    /// path source `subdir` is appended (ruro crates live in `crates/<name>`).
    pub fn dependency(&self, krate: &str, subdir: Option<&str>) -> String {
        let mut parts = Vec::new();
        if let Some(p) = &self.path {
            let p = match subdir {
                Some(sub) => p.join(sub),
                None => p.clone(),
            };
            parts.push(format!("path = {}", quote(&p.to_string_lossy())));
        } else if let Some(g) = &self.git {
            parts.push(format!("git = {}", quote(g)));
            for (k, v) in [("branch", &self.branch), ("tag", &self.tag), ("rev", &self.rev)] {
                if let Some(v) = v {
                    parts.push(format!("{k} = {}", quote(v)));
                }
            }
        } else {
            let v = self.version.as_deref().unwrap_or("*");
            parts.push(format!("version = {}", quote(v)));
        }
        let package = self.package.as_deref().unwrap_or(krate);
        if package != krate || subdir.is_some() {
            parts.push(format!("package = {}", quote(package)));
        }
        if !self.features.is_empty() {
            let f: Vec<String> = self.features.iter().map(|f| quote(f)).collect();
            parts.push(format!("features = [{}]", f.join(", ")));
        }
        format!("{{ {} }}", parts.join(", "))
    }

    /// Whether no source is configured.
    pub fn is_unset(&self) -> bool {
        self.path.is_none() && self.git.is_none() && self.version.is_none()
    }
}

/// A TOML basic string.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Whether `name` is usable as a Cargo package name.
pub fn is_valid_crate_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && name.len() <= 64
}

/// The Rust identifier of a crate (`my-nodes` → `my_nodes`).
pub fn lib_name(krate: &str) -> String {
    krate.replace('-', "_")
}

impl Manifest {
    /// Parses manifest text.
    pub fn parse(text: &str) -> Result<Self> {
        let m: Manifest = toml::from_str(text)?;
        m.ruro.check("[ruro]")?;
        for (name, src) in &m.packages {
            if !is_valid_crate_name(name) {
                bail!("[packages]: `{name}` is not a valid crate name");
            }
            src.check(&format!("[packages] {name}"))?;
        }
        Ok(m)
    }

    /// Reads and parses a manifest file.
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("in {}", path.display()))
    }

    /// Serializes to TOML.
    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).expect("manifest is serializable")
    }

    /// The source of the ruro crates, falling back to the git repository.
    pub fn ruro_source(&self) -> Source {
        if self.ruro.is_unset() {
            Source::git(RURO_GIT)
        } else {
            self.ruro.clone()
        }
    }

    /// The host binary name.
    pub fn host_name(&self) -> String {
        if let Some(n) = &self.host.name {
            return n.clone();
        }
        let project = if self.project.name.is_empty() {
            "ruro-project"
        } else {
            &self.project.name
        };
        format!("{project}-host")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_source_kinds() {
        let m = Manifest::parse(
            r#"
[project]
name = "bot"

[ruro]
path = "../ruro"

[packages]
my_nodes = { path = "." }
vision = { git = "https://example.com/v.git", rev = "abc" }
lidar = { version = "0.3", package = "acme-lidar", features = ["fast"] }
"#,
        )
        .unwrap();
        assert_eq!(m.host_name(), "bot-host");
        assert_eq!(
            m.packages["vision"].dependency("vision", None),
            r#"{ git = "https://example.com/v.git", rev = "abc" }"#
        );
        assert_eq!(
            m.packages["lidar"].dependency("lidar", None),
            r#"{ version = "0.3", package = "acme-lidar", features = ["fast"] }"#
        );
        assert_eq!(
            m.ruro.dependency("ruro-host", Some("crates/ruro-host")),
            r#"{ path = "../ruro/crates/ruro-host", package = "ruro-host" }"#
        );
        assert_eq!(Manifest::parse(&m.to_toml()).unwrap(), m);
    }

    #[test]
    fn rejects_conflicting_sources_and_bad_names() {
        assert!(Manifest::parse("[packages]\na = { path = \".\", git = \"x\" }").is_err());
        assert!(Manifest::parse("[packages]\na = { rev = \"x\" }").is_err());
        assert!(Manifest::parse("[packages]\n\"1bad\" = { path = \".\" }").is_err());
        assert!(Manifest::parse("[unknown]\n").is_err());
    }

    #[test]
    fn defaults_to_the_ruro_repository() {
        let m = Manifest::parse("").unwrap();
        assert_eq!(m.ruro_source().git.as_deref(), Some(RURO_GIT));
        assert_eq!(m.host_name(), "ruro-project-host");
    }

    #[test]
    fn quoting_escapes() {
        assert_eq!(quote(r#"a"b\c"#), r#""a\"b\\c""#);
    }
}
