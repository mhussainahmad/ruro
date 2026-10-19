//! Finding interface definitions at run time.
//!
//! A [`TypeLibrary`] looks message types up, in this order, in:
//!
//! 1. directories added with [`TypeLibrary::add_dir`] (a graph's
//!    `interfaces:` setting, a node's `interfaces` parameter), most recent
//!    first, then those in `RURO_ROS2_INTERFACES` (colon separated);
//! 2. `share/<package>/` of every prefix in `AMENT_PREFIX_PATH`, in overlay
//!    order (what a sourced ROS 2 workspace sets);
//! 3. the definitions bundled with ruro.
//!
//! A directory can be a package (it has `msg/`, `srv/` or `action/`), a
//! directory of packages, or an install prefix (it has `share/`). Packages
//! are parsed when one of their types is first needed; a file that does not
//! parse is skipped with a warning rather than failing the whole package.
//! `.srv` files give `<Srv>_Request`, `<Srv>_Response` and `<Srv>_Event`;
//! `.action` files give the goal, result and feedback messages and the
//! service and feedback messages derived from them, as `rosidl` does.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Result, anyhow};

use crate::dynamic::MessageType;
use crate::idl::{BaseType, Interfaces, TypeName};

/// Message types found on disk, in the environment and bundled with ruro.
#[derive(Debug, Default)]
pub struct TypeLibrary {
    /// Search roots, highest precedence first, each with the packages in it.
    roots: Vec<(PathBuf, BTreeMap<String, PathBuf>)>,
    interfaces: Interfaces,
    /// Packages already parsed (or found missing).
    loaded: BTreeSet<String>,
    compiled: BTreeMap<TypeName, Arc<MessageType>>,
    warnings: Vec<String>,
}

/// The package directories under a search root.
fn packages_in(root: &Path) -> BTreeMap<String, PathBuf> {
    let is_package = |d: &Path| ["msg", "srv", "action"].iter().any(|s| d.join(s).is_dir());
    let mut out = BTreeMap::new();
    if is_package(root) {
        if let Some(name) = root.file_name().and_then(|n| n.to_str()) {
            out.insert(name.to_owned(), root.to_owned());
        }
        return out;
    }
    let dir = if root.join("share").is_dir() {
        root.join("share")
    } else {
        root.to_owned()
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        if is_package(&p)
            && let Some(name) = p.file_name().and_then(|n| n.to_str())
        {
            out.insert(name.to_owned(), p);
        }
    }
    out
}

/// Parses every definition of the package at `dir` into `interfaces`.
/// Files that fail are skipped and reported in the returned warnings.
pub fn load_package(name: &str, dir: &Path, interfaces: &mut Interfaces) -> Vec<String> {
    let mut warnings = Vec::new();
    for sub in ["msg", "srv", "action"] {
        let Ok(entries) = std::fs::read_dir(dir.join(sub)) else {
            continue;
        };
        let all: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        // A `.idl` is used only where there is no `.msg`/`.srv`/`.action`
        // (rosidl installs both; some packages are written in IDL).
        let mut files: Vec<PathBuf> = all
            .iter()
            .filter(|p| {
                p.extension().is_some_and(|e| e == sub)
                    || (p.extension().is_some_and(|e| e == "idl")
                        && !all.contains(&p.with_extension(sub)))
            })
            .cloned()
            .collect();
        files.sort();
        for path in files {
            let file = path.display().to_string();
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    warnings.push(format!("skipped {file}: {e}"));
                    continue;
                }
            };
            if path.extension().is_some_and(|e| e == "idl") {
                match crate::idl_file::parse_idl(&text) {
                    Ok(specs) => {
                        for s in specs.into_iter().filter(|s| s.name.package == name) {
                            interfaces.add(s);
                        }
                    }
                    Err(e) => warnings.push(format!("skipped {file}: {e:#}")),
                }
                continue;
            }
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            if let Err(e) = interfaces.add_text(name, sub, &stem, &text, &file) {
                warnings.push(format!("skipped {file}: {e}"));
            }
        }
    }
    warnings
}

impl TypeLibrary {
    /// A library of the bundled definitions only.
    pub fn bundled() -> Self {
        Self::default()
    }

    /// The bundled definitions plus `RURO_ROS2_INTERFACES` and
    /// `AMENT_PREFIX_PATH`.
    pub fn from_env() -> Self {
        let mut lib = Self::default();
        let split = |var: &str| -> Vec<PathBuf> {
            std::env::var_os(var)
                .map(|v| {
                    std::env::split_paths(&v)
                        .filter(|p| !p.as_os_str().is_empty())
                        .collect()
                })
                .unwrap_or_default()
        };
        for prefix in split("AMENT_PREFIX_PATH").into_iter().rev() {
            lib.add_dir(&prefix.join("share"));
        }
        for dir in split("RURO_ROS2_INTERFACES").into_iter().rev() {
            lib.add_dir(&dir);
        }
        lib
    }

    /// The process-wide library, created with [`from_env`](Self::from_env)
    /// on first use. Nodes add their `interfaces` directories to it.
    pub fn global() -> &'static Mutex<TypeLibrary> {
        static GLOBAL: OnceLock<Mutex<TypeLibrary>> = OnceLock::new();
        GLOBAL.get_or_init(|| Mutex::new(TypeLibrary::from_env()))
    }

    /// Searches `dir` before every directory added so far. Adding a
    /// directory that is already searched does nothing. Types resolved
    /// before stay as they were resolved.
    pub fn add_dir(&mut self, dir: &Path) {
        if self.roots.iter().any(|(r, _)| r == dir) {
            return;
        }
        let packages = packages_in(dir);
        // Packages the new directory provides are looked up again.
        for p in packages.keys() {
            if self.loaded.remove(p) {
                self.interfaces.remove_package(p);
            }
        }
        self.roots.insert(0, (dir.to_owned(), packages));
    }

    /// The directories searched, highest precedence first.
    pub fn dirs(&self) -> Vec<&Path> {
        self.roots.iter().map(|(r, _)| r.as_path()).collect()
    }

    /// Where a package's definitions come from: a directory, or `None` for
    /// bundled or unknown packages.
    pub fn package_dir(&self, package: &str) -> Option<&Path> {
        self.roots
            .iter()
            .find_map(|(_, pkgs)| pkgs.get(package))
            .map(PathBuf::as_path)
    }

    /// Files that could not be parsed so far.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    fn ensure_package(&mut self, package: &str) {
        if !self.loaded.insert(package.to_owned()) {
            return;
        }
        if let Some(dir) = self.package_dir(package).map(Path::to_owned) {
            let warnings = load_package(package, &dir, &mut self.interfaces);
            self.warnings.extend(warnings);
            return;
        }
        for (pkg, kind, name, text) in crate::bundled::BUNDLED {
            if *pkg == package {
                self.interfaces
                    .add_text(pkg, kind, name, text, name)
                    .expect("bundled definitions parse");
            }
        }
    }

    /// The parsed definitions loaded so far.
    pub fn interfaces(&self) -> &Interfaces {
        &self.interfaces
    }

    /// Loads the definitions of `name` and of every type it references.
    pub fn load(&mut self, name: &TypeName) -> Result<()> {
        let mut seen = BTreeSet::new();
        let mut stack = vec![name.clone()];
        while let Some(n) = stack.pop() {
            if !seen.insert(n.clone()) {
                continue;
            }
            self.ensure_package(&n.package);
            let spec = self.interfaces.get(&n).ok_or_else(|| {
                let mut msg = if &n == name {
                    format!("unknown message type {n}")
                } else {
                    format!("{name} refers to unknown message type {n}")
                };
                if self.package_dir(&n.package).is_none() {
                    msg.push_str(&format!(
                        "; package `{}` was not found in {}",
                        n.package,
                        self.describe_search()
                    ));
                }
                anyhow!(msg)
            })?;
            for f in &spec.fields {
                if let BaseType::Named(t) = &f.ty.base {
                    stack.push(t.clone());
                }
            }
        }
        Ok(())
    }

    /// Compiles a message type for encoding and decoding.
    pub fn resolve(&mut self, name: &TypeName) -> Result<Arc<MessageType>> {
        if let Some(t) = self.compiled.get(name) {
            return Ok(t.clone());
        }
        self.load(name)?;
        if self.interfaces.is_description(name) {
            anyhow::bail!(
                "{name} is a service or action, not a message; use its request, response, \
                 goal, result or feedback message type"
            );
        }
        crate::dynamic::compile(&self.interfaces, name, &mut self.compiled)
    }

    /// [`resolve`](Self::resolve) for a name written `pkg/msg/Type` or
    /// `pkg/Type`.
    pub fn resolve_str(&mut self, name: &str) -> Result<Arc<MessageType>> {
        let t = TypeName::parse(name)
            .ok_or_else(|| anyhow!("expected a type like `pkg/msg/Type`, got `{name}`"))?;
        self.resolve(&t)
    }

    fn describe_search(&self) -> String {
        let mut places: Vec<String> = self
            .roots
            .iter()
            .map(|(r, _)| r.display().to_string())
            .collect();
        places.push("the bundled definitions".into());
        let mut s = places.join(", ");
        if std::env::var_os("AMENT_PREFIX_PATH").is_none() {
            s.push_str(
                " (AMENT_PREFIX_PATH is not set: source a ROS 2 workspace, or point \
                 `interfaces` at the package's directory)",
            );
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ruro-search-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn directories_take_precedence_over_bundled_definitions() {
        let root = temp("prec");
        let pkg = root.join("std_msgs/msg");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("String.msg"), "string data\nint32 extra\n").unwrap();
        std::fs::write(pkg.join("Broken.msg"), "int32[x] bad\n").unwrap();
        let mut lib = TypeLibrary::bundled();
        let bundled = lib.resolve_str("std_msgs/msg/String").unwrap();
        assert_eq!(bundled.fields.len(), 1);
        lib.add_dir(&root);
        let ours = lib.resolve_str("std_msgs/msg/String").unwrap();
        assert_eq!(ours.fields.len(), 1, "resolved types are kept");
        let mut lib = TypeLibrary::bundled();
        lib.add_dir(&root);
        let ours = lib.resolve_str("std_msgs/msg/String").unwrap();
        assert_eq!(ours.fields.len(), 2);
        assert_eq!(lib.warnings().len(), 1, "{:?}", lib.warnings());
        // The rest of std_msgs now comes from the directory, which lacks it.
        let err = lib.resolve_str("std_msgs/msg/Header").unwrap_err();
        assert!(
            format!("{err:#}").contains("unknown message type"),
            "{err:#}"
        );
    }

    #[test]
    fn packages_prefixes_and_package_lists_are_found() {
        let root = temp("layout");
        std::fs::create_dir_all(root.join("prefix/share/a_msgs/msg")).unwrap();
        std::fs::create_dir_all(root.join("list/b_msgs/srv")).unwrap();
        std::fs::create_dir_all(root.join("c_msgs/action")).unwrap();
        assert!(packages_in(&root.join("prefix")).contains_key("a_msgs"));
        assert!(packages_in(&root.join("list")).contains_key("b_msgs"));
        assert!(packages_in(&root.join("c_msgs")).contains_key("c_msgs"));
    }

    #[test]
    fn missing_types_explain_where_they_were_looked_for() {
        let mut lib = TypeLibrary::bundled();
        let err = lib.resolve_str("nope_msgs/msg/Thing").unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("package `nope_msgs` was not found"), "{text}");
        assert!(lib.resolve_str("not a type").is_err());
    }
}
