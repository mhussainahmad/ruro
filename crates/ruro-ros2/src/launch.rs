//! `ruro ros2 import`: ROS 2 launch files to ruro graphs.
//!
//! XML and YAML launch files are read without ROS: `arg`, `let`, the
//! substitutions `$(arg)`, `$(var)`, `$(env)`, `$(optenv)`,
//! `$(find-pkg-share)`, `$(find-pkg-prefix)`, `$(dirname)`, `$(filename)` and
//! `$(anon)`, `if`/`unless`, `group` (scoped or not), `push-ros-namespace`,
//! `set_parameter`, `set_remap`, `set_env`, `include` of other XML or YAML
//! files, `node` with `param` (values, nested groups and files), `remap` and
//! `env`, `executable`, and `node_container` with `composable_node`. Every
//! ROS node becomes a `ros2/node` node that runs it (see
//! [`crate::external`]); a composable node becomes a `ros2/node` running
//! `ros2 component load` into its container.
//!
//! Anything that cannot be converted is reported in
//! [`Imported::warnings`], never dropped silently. Python launch files need
//! ROS to evaluate them; see [`import`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use ruro_graph::{Graph, NodeInstance};
use serde_json::Value;

use crate::external::{RosProcess, find_package_share, split_words};

/// A launch file entity, from XML or YAML.
#[derive(Debug, Clone, PartialEq)]
struct Element {
    /// Tag with `-` written as `_`, e.g. `push_ros_namespace`.
    tag: String,
    /// Attributes with `-` written as `_`, in file order.
    attrs: Vec<(String, String)>,
    children: Vec<Element>,
    /// `file:line` for messages.
    origin: String,
}

impl Element {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

fn norm(s: &str) -> String {
    s.replace('-', "_")
}

fn parse_xml(text: &str, file: &str) -> Result<Element> {
    let doc = roxmltree::Document::parse(text).with_context(|| format!("parsing {file}"))?;
    fn convert(n: roxmltree::Node, doc: &roxmltree::Document, file: &str) -> Element {
        let line = doc.text_pos_at(n.range().start).row;
        Element {
            tag: norm(n.tag_name().name()),
            attrs: n
                .attributes()
                .map(|a| (norm(a.name()), a.value().to_owned()))
                .collect(),
            children: n
                .children()
                .filter(|c| c.is_element())
                .map(|c| convert(c, doc, file))
                .collect(),
            origin: format!("{file}:{line}"),
        }
    }
    let root = convert(doc.root_element(), &doc, file);
    if root.tag != "launch" {
        bail!("{file}: the root element is <{}>, not <launch>", root.tag);
    }
    Ok(root)
}

/// A YAML scalar as launch attributes hold it.
fn yaml_scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn yaml_entity(tag: &str, body: &Value, file: &str) -> Result<Element> {
    let mut e = Element {
        tag: norm(tag),
        attrs: Vec::new(),
        children: Vec::new(),
        origin: format!("{file}: {tag}"),
    };
    let Some(map) = body.as_object() else {
        // `- push-ros-namespace: ns` style shorthand is not part of the
        // format; accept a scalar as the main attribute.
        e.attrs.push(("value".into(), yaml_scalar(body)));
        return Ok(e);
    };
    for (k, v) in map {
        match v {
            Value::Array(items) if items.iter().all(Value::is_object) && !items.is_empty() => {
                if k == "children" {
                    for item in items {
                        e.children.extend(yaml_entities(item, file)?);
                    }
                } else {
                    for item in items {
                        e.children.push(yaml_entity(k, item, file)?);
                    }
                }
            }
            Value::Object(_) => e.children.push(yaml_entity(k, v, file)?),
            Value::Array(items) => {
                // A list value, e.g. a parameter array: keep it as YAML flow.
                let parts: Vec<String> = items.iter().map(|i| i.to_string()).collect();
                e.attrs.push((norm(k), format!("[{}]", parts.join(", "))));
            }
            v => e.attrs.push((norm(k), yaml_scalar(v))),
        }
    }
    Ok(e)
}

/// `- tag: {...}` list items.
fn yaml_entities(item: &Value, file: &str) -> Result<Vec<Element>> {
    let map = item
        .as_object()
        .ok_or_else(|| anyhow!("{file}: expected `- <entity>: {{...}}`, got {item}"))?;
    map.iter().map(|(k, v)| yaml_entity(k, v, file)).collect()
}

fn parse_yaml(text: &str, file: &str) -> Result<Element> {
    let v: Value = serde_saphyr::from_str(text).map_err(|e| anyhow!("parsing {file}: {e}"))?;
    let items = v
        .get("launch")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("{file}: expected a top-level `launch:` list"))?;
    let mut root = Element {
        tag: "launch".into(),
        attrs: Vec::new(),
        children: Vec::new(),
        origin: file.to_owned(),
    };
    for item in items {
        root.children.extend(yaml_entities(item, file)?);
    }
    Ok(root)
}

/// Launch configurations and settings that apply to the entities of a scope.
#[derive(Debug, Clone, Default)]
struct Scope {
    vars: BTreeMap<String, String>,
    namespace: String,
    remaps: Vec<(String, String)>,
    params: Vec<(String, Value)>,
    env: Vec<(String, String)>,
    /// The file being read, for `$(dirname)` and relative paths.
    file: PathBuf,
}

/// The result of an import.
#[derive(Debug, Clone)]
pub struct Imported {
    pub graph: Graph,
    /// What could not be converted, or was converted with a caveat.
    pub warnings: Vec<String>,
}

/// Options of an import.
#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    /// `name:=value` launch arguments.
    pub args: Vec<(String, String)>,
    /// Parameter files given to every node, as `--params-file`.
    pub params_files: Vec<PathBuf>,
    /// `AMENT_PREFIX_PATH` to resolve `$(find-pkg-share)` with; `None`
    /// takes the environment's.
    pub ament_prefix_path: Option<String>,
    /// Graph name; default: the file name.
    pub name: Option<String>,
    /// `ros2/sub` bridges to add: topic and type.
    pub subscribe: Vec<(String, String)>,
    /// `ros2/pub` bridges to add: topic and type.
    pub publish: Vec<(String, String)>,
    /// Zenoh router for every `ros2/*` node; `None` keeps their default.
    pub router: Option<String>,
}

/// Sets options that apply to every `ros2/*` node of an imported graph.
fn apply_common(graph: &mut Graph, opts: &ImportOptions) {
    if let Some(router) = &opts.router {
        for n in graph.nodes.values_mut() {
            if n.kind.starts_with("ros2/") {
                n.params.insert("router".into(), router.clone().into());
            }
        }
    }
}

struct Importer {
    opts: ImportOptions,
    ament: String,
    graph: Graph,
    warnings: Vec<String>,
    /// Include depth, to stop include loops.
    depth: usize,
}

const GROUP: &str = "ros2";

/// Truthiness of a launch condition.
fn truthy(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

/// Splits `$(cmd a b)` arguments, keeping quoted words together.
/// At most 60 characters of `base`, leaving room for the number
/// [`Graph::unique_id`] appends: cutting its result instead could make it
/// equal an existing id.
fn id_base(base: &str) -> String {
    base.chars().take(60).collect()
}

/// An environment variable as the launch sees it: set by `<set_env>`, else
/// inherited.
fn launch_env(scope: &Scope, name: &str) -> Option<String> {
    match scope.env.iter().rev().find(|(k, _)| k == name) {
        Some((_, v)) => Some(v.clone()),
        None => std::env::var(name).ok(),
    }
}

/// Shell-like words of `s`, with each `$(...)` kept whole inside its word.
fn split_keeping_substitutions(s: &str) -> Result<Vec<String>> {
    // Whitespace and quotes inside substitutions are hidden from the
    // splitter, then put back.
    const HIDE: [(char, char); 3] = [(' ', '\u{1}'), ('\'', '\u{2}'), ('"', '\u{3}')];
    let mut hidden = String::with_capacity(s.len());
    let mut depth = 0usize;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' && chars.peek() == Some(&'(') {
            depth += 1;
            hidden.push(c);
            hidden.push(chars.next().unwrap());
            continue;
        }
        match c {
            '(' if depth > 0 => depth += 1,
            ')' if depth > 0 => depth -= 1,
            _ => {}
        }
        let c = if c.is_whitespace() { ' ' } else { c };
        match HIDE.iter().find(|(from, _)| depth > 0 && *from == c) {
            Some(&(_, to)) => hidden.push(to),
            None => hidden.push(c),
        }
    }
    Ok(split_words(&hidden)?
        .into_iter()
        .map(|w| {
            HIDE.iter()
                .fold(w, |w, &(from, to)| w.replace(to, &from.to_string()))
        })
        .collect())
}

fn subst_args(s: &str) -> Vec<String> {
    split_words(s).unwrap_or_else(|_| s.split_whitespace().map(str::to_owned).collect())
}

impl Importer {
    fn warn(&mut self, origin: &str, msg: impl std::fmt::Display) {
        self.warnings.push(format!("{origin}: {msg}"));
    }

    /// Splits a command line into words as written, then substitutes each.
    fn words(&mut self, text: &str, scope: &Scope, origin: &str) -> Result<Vec<String>> {
        Ok(split_keeping_substitutions(text)?
            .iter()
            .map(|w| self.subst(w, scope, origin))
            .collect())
    }

    /// Resolves the substitutions in `text`. Unresolvable ones are kept as
    /// written, with a warning.
    fn subst(&mut self, text: &str, scope: &Scope, origin: &str) -> String {
        let mut out = String::new();
        let mut rest = text;
        while let Some(start) = rest.find("$(") {
            out.push_str(&rest[..start]);
            // Find the matching parenthesis.
            let body_start = start + 2;
            let mut depth = 1;
            let mut end = None;
            for (i, c) in rest[body_start..].char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(body_start + i);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let Some(end) = end else {
                self.warn(origin, format!("unterminated substitution in `{text}`"));
                out.push_str(&rest[start..]);
                return out;
            };
            let inner = self.subst(&rest[body_start..end], scope, origin);
            out.push_str(&self.substitution(&inner, scope, origin));
            rest = &rest[end + 1..];
        }
        out.push_str(rest);
        out
    }

    fn substitution(&mut self, inner: &str, scope: &Scope, origin: &str) -> String {
        let literal = format!("$({inner})");
        let words = subst_args(inner);
        let (cmd, args) = match words.split_first() {
            Some((c, a)) => (c.as_str(), a),
            None => return literal,
        };
        match (cmd, args) {
            ("var" | "arg", [name]) => match scope.vars.get(name) {
                Some(v) => v.clone(),
                None => {
                    self.warn(origin, format!("launch argument `{name}` has no value"));
                    literal
                }
            },
            ("env", [name]) => match launch_env(scope, name) {
                Some(v) => v,
                None => {
                    self.warn(
                        origin,
                        format!("environment variable {name} is not set; kept `{literal}`"),
                    );
                    literal
                }
            },
            ("env" | "optenv", [name, default @ ..]) => {
                launch_env(scope, name).unwrap_or_else(|| default.join(" "))
            }
            ("optenv", []) => literal,
            ("find_pkg_share" | "find-pkg-share", [pkg]) => {
                match find_package_share(&self.ament, pkg) {
                    Some(p) => p.display().to_string(),
                    None => {
                        self.warn(
                            origin,
                            format!(
                                "package `{pkg}` was not found (AMENT_PREFIX_PATH); \
                                 kept `{literal}`, which ros2/node resolves when it starts"
                            ),
                        );
                        literal
                    }
                }
            }
            ("find_pkg_prefix" | "find-pkg-prefix", [pkg]) => {
                match find_package_share(&self.ament, pkg)
                    .and_then(|s| s.parent().and_then(Path::parent).map(Path::to_owned))
                {
                    Some(p) => p.display().to_string(),
                    None => {
                        self.warn(
                            origin,
                            format!(
                                "package `{pkg}` was not found (AMENT_PREFIX_PATH); \
                                 kept `{literal}`, which ros2/node resolves when it starts"
                            ),
                        );
                        literal
                    }
                }
            }
            ("dirname", []) => scope
                .file
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            ("filename", []) => scope.file.display().to_string(),
            ("anon", [name]) => {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                (scope.file.as_os_str(), name).hash(&mut h);
                format!("{name}_{:08x}", h.finish() as u32)
            }
            _ => {
                self.warn(
                    origin,
                    format!("substitution `{literal}` is not supported; kept as written"),
                );
                literal
            }
        }
    }

    /// Evaluates `if`/`unless`. Undecidable conditions keep the entity.
    fn enabled(&mut self, e: &Element, scope: &Scope) -> bool {
        for (attr, want) in [("if", true), ("unless", false)] {
            if let Some(c) = e.attr(attr) {
                let v = self.subst(c, scope, &e.origin);
                match truthy(&v) {
                    Some(b) if b != want => return false,
                    Some(_) => {}
                    None => self.warn(
                        &e.origin,
                        format!(
                            "condition {attr}=\"{c}\" evaluates to `{v}`, not true or false; \
                             <{}> kept",
                            e.tag
                        ),
                    ),
                }
            }
        }
        true
    }

    fn path_attr(&mut self, value: &str, scope: &Scope, origin: &str) -> String {
        let v = self.subst(value, scope, origin);
        if v.starts_with("$(") || Path::new(&v).is_absolute() {
            return v;
        }
        scope
            .file
            .parent()
            .map(|d| d.join(&v).display().to_string())
            .unwrap_or(v)
    }

    fn children(&mut self, parent: &Element, scope: &mut Scope) -> Result<()> {
        for e in &parent.children {
            self.entity(e, scope)?;
        }
        Ok(())
    }

    fn entity(&mut self, e: &Element, scope: &mut Scope) -> Result<()> {
        if !self.enabled(e, scope) {
            return Ok(());
        }
        match e.tag.as_str() {
            "arg" => {
                let Some(name) = e.attr("name") else {
                    self.warn(&e.origin, "<arg> without a name ignored");
                    return Ok(());
                };
                let name = name.to_owned();
                if !scope.vars.contains_key(&name) {
                    match e.attr("default").or(e.attr("value")) {
                        Some(d) => {
                            let v = self.subst(d, scope, &e.origin);
                            scope.vars.insert(name, v);
                        }
                        None => self.warn(
                            &e.origin,
                            format!(
                                "launch argument `{name}` has no default; give it as \
                                 `{name}:=<value>`"
                            ),
                        ),
                    }
                }
            }
            "let" => {
                if let (Some(name), Some(value)) = (e.attr("name"), e.attr("value")) {
                    let v = self.subst(value, scope, &e.origin);
                    scope.vars.insert(name.to_owned(), v);
                }
            }
            "group" => {
                let scoped = e.attr("scoped").and_then(truthy).unwrap_or(true);
                let ns = e
                    .attr("ns")
                    .or(e.attr("namespace"))
                    .map(|ns| self.subst(ns, scope, &e.origin));
                if scoped {
                    let mut inner = scope.clone();
                    if let Some(ns) = ns {
                        scope_push_ns(&mut inner, &ns);
                    }
                    self.children(e, &mut inner)?;
                } else {
                    if let Some(ns) = ns {
                        scope_push_ns(scope, &ns);
                    }
                    self.children(e, scope)?;
                }
            }
            "push_ros_namespace" | "push_namespace" => {
                let ns = e.attr("namespace").or(e.attr("ns")).or(e.attr("value"));
                match ns {
                    Some(ns) => {
                        let ns = self.subst(ns, scope, &e.origin);
                        scope_push_ns(scope, &ns);
                    }
                    None => self.warn(&e.origin, "<push-ros-namespace> without a namespace"),
                }
            }
            "set_parameter" => {
                if let (Some(name), Some(value)) = (e.attr("name"), e.attr("value")) {
                    let v = self.subst(value, scope, &e.origin);
                    scope.params.push((name.to_owned(), param_value(&v)));
                }
            }
            "set_remap" => {
                if let (Some(f), Some(t)) = (e.attr("from"), e.attr("to")) {
                    let f = self.subst(f, scope, &e.origin);
                    let t = self.subst(t, scope, &e.origin);
                    scope.remaps.push((f, t));
                }
            }
            "set_env" => {
                if let (Some(n), Some(v)) = (e.attr("name"), e.attr("value")) {
                    let v = self.subst(v, scope, &e.origin);
                    scope.env.push((n.to_owned(), v));
                }
            }
            "include" => self.include(e, scope)?,
            "node" => {
                let p = self.node(e, scope)?;
                self.add_process(e, p, scope)?;
            }
            "executable" => {
                let Some(cmd) = e.attr("cmd") else {
                    self.warn(&e.origin, "<executable> without cmd ignored");
                    return Ok(());
                };
                let cmd = self.subst(cmd, scope, &e.origin);
                let mut p = RosProcess {
                    command: split_words(&cmd)?,
                    name: e.attr("name").unwrap_or_default().to_owned(),
                    env: scope.env.clone(),
                    ..Default::default()
                };
                for c in &e.children {
                    match c.tag.as_str() {
                        "env" => self.env_child(c, scope, &mut p),
                        other => self.warn(&c.origin, format!("<{other}> in <executable> ignored")),
                    }
                }
                // `name` of an executable is a label, not a ROS node name.
                let label = std::mem::take(&mut p.name);
                self.add_process_named(e, p, &label, scope)?;
            }
            "node_container" => {
                let container = self.node(e, scope)?;
                let target = container.label();
                self.add_process(e, container, scope)?;
                for c in &e.children {
                    if c.tag == "composable_node" && self.enabled(c, scope) {
                        self.composable(c, &target, scope)?;
                    }
                }
            }
            "load_composable_node" => {
                let target = e
                    .attr("target")
                    .map(|t| self.subst(t, scope, &e.origin))
                    .unwrap_or_default();
                for c in &e.children {
                    if c.tag == "composable_node" && self.enabled(c, scope) {
                        let target = crate::rmw::qualify(&target, &scope_ns(scope));
                        self.composable(c, &target, scope)?;
                    }
                }
            }
            "timer" => {
                self.warn(
                    &e.origin,
                    format!(
                        "<timer period=\"{}\">: the delay is dropped; its entities start \
                         with the others",
                        e.attr("period").unwrap_or("?")
                    ),
                );
                let mut inner = scope.clone();
                self.children(e, &mut inner)?;
            }
            other => self.warn(
                &e.origin,
                format!("<{other}> is not supported and was skipped"),
            ),
        }
        Ok(())
    }

    fn include(&mut self, e: &Element, scope: &Scope) -> Result<()> {
        let Some(file) = e.attr("file") else {
            self.warn(&e.origin, "<include> without file ignored");
            return Ok(());
        };
        let path = self.path_attr(file, scope, &e.origin);
        if path.contains("$(") {
            self.warn(
                &e.origin,
                format!("cannot include `{path}`: the path is unresolved"),
            );
            return Ok(());
        }
        let mut inner = scope.clone();
        for c in &e.children {
            if c.tag == "arg"
                && let (Some(n), Some(v)) = (c.attr("name"), c.attr("value"))
            {
                let v = self.subst(v, scope, &c.origin);
                inner.vars.insert(n.to_owned(), v);
            }
        }
        if self.depth > 32 {
            bail!("{}: includes nested more than 32 deep", e.origin);
        }
        let path = PathBuf::from(path);
        match load_file(&path) {
            Ok(root) => {
                inner.file = path;
                self.depth += 1;
                let r = self.children(&root, &mut inner);
                self.depth -= 1;
                r
            }
            Err(err) => {
                self.warn(
                    &e.origin,
                    format!("cannot include {}: {err:#}", path.display()),
                );
                Ok(())
            }
        }
    }

    fn env_child(&mut self, c: &Element, scope: &Scope, p: &mut RosProcess) {
        if let (Some(n), Some(v)) = (c.attr("name"), c.attr("value")) {
            let v = self.subst(v, scope, &c.origin);
            p.env.push((n.to_owned(), v));
        }
    }

    /// `<param>` children, recursively for nested groups.
    fn params(&mut self, prefix: &str, e: &Element, scope: &Scope, p: &mut RosProcess) {
        for c in &e.children {
            if c.tag != "param" {
                continue;
            }
            if let Some(from) = c.attr("from") {
                let path = self.path_attr(from, scope, &c.origin);
                p.params_files.push(path);
                continue;
            }
            let Some(name) = c.attr("name") else {
                self.warn(&c.origin, "<param> without name or from ignored");
                continue;
            };
            let full = if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix}.{name}")
            };
            if !c.children.is_empty() {
                self.params(&full, c, scope, p);
                continue;
            }
            let Some(value) = c.attr("value") else {
                self.warn(&c.origin, format!("parameter `{full}` has no value"));
                continue;
            };
            let v = self.subst(value, scope, &c.origin);
            let v = match (c.attr("value_sep"), c.attr("type")) {
                (Some(sep), _) => {
                    Value::Array(v.split(sep).map(|s| param_value(s.trim())).collect())
                }
                (_, Some("str")) => Value::String(v),
                _ => param_value(&v),
            };
            p.parameters.push((full, v));
        }
    }

    /// A `<node>` or `<node_container>`.
    fn node(&mut self, e: &Element, scope: &Scope) -> Result<RosProcess> {
        let get = |me: &mut Self, k: &str| -> String {
            e.attr(k)
                .map(|v| me.subst(v, scope, &e.origin))
                .unwrap_or_default()
        };
        let package = get(self, "pkg");
        let mut executable = get(self, "exec");
        if executable.is_empty() {
            executable = get(self, "executable");
        }
        let node_ns = get(self, "namespace");
        let node_ns = if node_ns.is_empty() {
            get(self, "ns")
        } else {
            node_ns
        };
        let namespace = if node_ns.starts_with('/') {
            node_ns
        } else {
            join_ns(&scope_ns(scope), &node_ns)
        };
        let mut p = RosProcess {
            package,
            executable,
            name: get(self, "name"),
            namespace: if namespace == "/" {
                String::new()
            } else {
                namespace
            },
            remaps: scope.remaps.clone(),
            parameters: scope.params.clone(),
            params_files: self
                .opts
                .params_files
                .iter()
                .map(|f| f.display().to_string())
                .collect(),
            env: scope.env.clone(),
            ..Default::default()
        };
        // Split as written, then substitute: a substitution stays one word
        // whatever its value, as in ros2 launch.
        p.arguments = self
            .words(e.attr("args").unwrap_or(""), scope, &e.origin)
            .with_context(|| format!("{}: args", e.origin))?;
        p.ros_args = self
            .words(e.attr("ros_args").unwrap_or(""), scope, &e.origin)
            .with_context(|| format!("{}: ros_args", e.origin))?;
        let prefix = get(self, "launch_prefix");
        p.prefix = split_words(&prefix).with_context(|| format!("{}: launch-prefix", e.origin))?;
        self.params("", e, scope, &mut p);
        for c in &e.children {
            match c.tag.as_str() {
                "param" | "composable_node" => {}
                "remap" => {
                    if let (Some(f), Some(t)) = (c.attr("from"), c.attr("to")) {
                        let f = self.subst(f, scope, &c.origin);
                        let t = self.subst(t, scope, &c.origin);
                        p.remaps.push((f, t));
                    }
                }
                "env" => self.env_child(c, scope, &mut p),
                other => self.warn(&c.origin, format!("<{other}> in <{}> ignored", e.tag)),
            }
        }
        const KNOWN: &[&str] = &[
            "pkg",
            "exec",
            "executable",
            "name",
            "namespace",
            "ns",
            "args",
            "ros_args",
            "launch_prefix",
            "respawn",
            "respawn_delay",
            "if",
            "unless",
            // Output always goes to the ruro log.
            "output",
            "emulate_tty",
            "cwd",
            "sigterm_timeout",
            "sigkill_timeout",
            "exec_name",
        ];
        for (k, _) in &e.attrs {
            if !KNOWN.contains(&k.as_str()) {
                self.warn(&e.origin, format!("attribute `{k}` of <{}> ignored", e.tag));
            }
        }
        for (attr, why) in [
            ("cwd", "the working directory is the host's"),
            ("sigterm_timeout", "ros2/node waits 5 s"),
            ("sigkill_timeout", "ros2/node waits 5 s"),
            ("exec_name", "logs use the node id"),
        ] {
            if e.attr(attr).is_some() {
                self.warn(&e.origin, format!("`{attr}` ignored: {why}"));
            }
        }
        if p.package.is_empty() || p.executable.is_empty() {
            bail!("{}: <{}> needs pkg and exec", e.origin, e.tag);
        }
        Ok(p)
    }

    fn composable(&mut self, c: &Element, container: &str, scope: &Scope) -> Result<()> {
        let get = |me: &mut Self, k: &str| -> String {
            c.attr(k)
                .map(|v| me.subst(v, scope, &c.origin))
                .unwrap_or_default()
        };
        let pkg = get(self, "pkg");
        let plugin = get(self, "plugin");
        let name = get(self, "name");
        let ns = get(self, "namespace");
        let ns = if ns.starts_with('/') {
            ns
        } else {
            join_ns(&scope_ns(scope), &ns)
        };
        let mut params = RosProcess::default();
        self.params("", c, scope, &mut params);
        let mut cmd: Vec<String> = ["ros2", "component", "load"].map(String::from).into();
        if !name.is_empty() {
            cmd.extend(["--node-name".into(), name.clone()]);
        }
        if ns != "/" && !ns.is_empty() {
            cmd.extend(["--node-namespace".into(), ns]);
        }
        for (k, v) in scope.params.iter().chain(&params.parameters) {
            cmd.extend(["-p".into(), format!("{k}:={v}")]);
        }
        if !params.params_files.is_empty() {
            self.warn(
                &c.origin,
                "parameter files of a composable node are not passed to `ros2 component load`",
            );
        }
        // The scope's remaps first, as for nodes: the first matching rule wins.
        for (f, t) in &scope.remaps {
            cmd.extend(["-r".into(), format!("{f}:={t}")]);
        }
        for r in &c.children {
            match r.tag.as_str() {
                "remap" => {
                    if let (Some(f), Some(t)) = (r.attr("from"), r.attr("to")) {
                        let f = self.subst(f, scope, &r.origin);
                        let t = self.subst(t, scope, &r.origin);
                        cmd.extend(["-r".into(), format!("{f}:={t}")]);
                    }
                }
                "extra_arg" => {
                    if let (Some(n), Some(v)) = (r.attr("name"), r.attr("value")) {
                        let v = self.subst(v, scope, &r.origin);
                        cmd.extend(["-e".into(), format!("{n}:={v}")]);
                    }
                }
                _ => {}
            }
        }
        cmd.extend([container.to_owned(), pkg, plugin.clone()]);
        self.warn(
            &c.origin,
            format!(
                "composable node {plugin} is loaded into {container} by `ros2 component load` \
                 (needs the ros2 command); it retries until the container answers"
            ),
        );
        let p = RosProcess {
            command: cmd,
            env: scope.env.clone(),
            ..Default::default()
        };
        let label = if name.is_empty() {
            format!("load_{plugin}")
        } else {
            format!("load_{name}")
        };
        let id = self.add_process_named(c, p, &label, scope)?;
        // Loading is a one-shot command: retry on failure, stop on success.
        if let Some(n) = self.graph.nodes.get_mut(&id) {
            n.params.insert("respawn".into(), "on-failure".into());
        }
        Ok(())
    }

    fn add_process(&mut self, e: &Element, p: RosProcess, scope: &Scope) -> Result<String> {
        let label = p.label();
        self.add_process_named(e, p, &label, scope)
    }

    fn add_process_named(
        &mut self,
        e: &Element,
        p: RosProcess,
        label: &str,
        scope: &Scope,
    ) -> Result<String> {
        let base = label.trim_start_matches('/').replace('/', "_");
        let base: String = if base.is_empty() {
            "ros2_node".into()
        } else {
            base
        };
        let id = self.graph.unique_id(&id_base(&base));
        let mut node = NodeInstance::new("ros2/node");
        node.group = Some(GROUP.into());
        node.params = p.to_params();
        match e.attr("respawn").map(|r| self.subst(r, scope, &e.origin)) {
            Some(r) if truthy(&r) == Some(true) => {
                node.params.insert("respawn".into(), "always".into());
                if let Some(d) = e.attr("respawn_delay") {
                    let d = self.subst(d, scope, &e.origin);
                    if let Ok(secs) = d.parse::<f64>() {
                        node.params.insert("respawn_delay".into(), secs.into());
                    }
                }
            }
            Some(r) if truthy(&r) == Some(false) => {
                // ROS 2 launch does not restart by default.
                node.params.insert("respawn".into(), "never".into());
            }
            Some(r) => self.warn(&e.origin, format!("respawn=\"{r}\" is not true or false")),
            None => {
                node.params.insert("respawn".into(), "never".into());
            }
        }
        let n = self.graph.nodes.len();
        node.pos = Some([(n % 4) as f32 * 260.0, (n / 4) as f32 * 140.0]);
        self.graph.nodes.insert(id.clone(), node);
        Ok(id)
    }
}

fn join_ns(base: &str, ns: &str) -> String {
    let base = base.trim_end_matches('/');
    let ns = ns.trim_matches('/');
    match (base.is_empty(), ns.is_empty()) {
        (true, true) => "/".into(),
        (true, false) => format!("/{ns}"),
        (false, true) => base.to_owned(),
        (false, false) => format!("{base}/{ns}"),
    }
}

fn scope_ns(scope: &Scope) -> String {
    if scope.namespace.is_empty() {
        "/".into()
    } else {
        scope.namespace.clone()
    }
}

fn scope_push_ns(scope: &mut Scope, ns: &str) {
    scope.namespace = if ns.starts_with('/') {
        join_ns("", ns)
    } else {
        join_ns(&scope.namespace, ns)
    };
}

/// A parameter value written in a launch file: YAML, like ROS reads it.
fn param_value(s: &str) -> Value {
    match serde_saphyr::from_str::<Value>(s) {
        Ok(Value::Null) if !s.trim().is_empty() && s.trim() != "null" && s.trim() != "~" => {
            Value::String(s.to_owned())
        }
        Ok(v @ (Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_))) => v,
        _ => Value::String(s.to_owned()),
    }
}

fn load_file(path: &Path) -> Result<Element> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let file = path.display().to_string();
    match path.extension().and_then(|e| e.to_str()) {
        Some("xml") => parse_xml(&text, &file),
        Some("yaml" | "yml") => parse_yaml(&text, &file),
        Some("py") => bail!("{}", python_message(path)),
        _ if text.trim_start().starts_with('<') => parse_xml(&text, &file),
        _ => parse_yaml(&text, &file),
    }
}

/// True if a ROS 2 installation is usable from this process.
pub fn ros_environment() -> bool {
    std::env::var_os("AMENT_PREFIX_PATH").is_some_and(|v| !v.is_empty())
        || std::env::var_os("PATH")
            .is_some_and(|path| std::env::split_paths(&path).any(|d| d.join("ros2").is_file()))
}

fn python_message(path: &Path) -> String {
    let base = format!(
        "{} is a Python launch file; ruro reads XML and YAML launch files without ROS, \
         but Python launch files can only be evaluated by ROS 2 itself.",
        path.display()
    );
    if ros_environment() {
        format!(
            "{base} A ROS 2 environment is available here: run `ruro ros2 import --eval {}` \
             to evaluate it with `ros2 launch` (its processes are recorded instead of run)",
            path.display()
        )
    } else {
        format!(
            "{base} Run `ruro ros2 import --eval` in a shell where a ROS 2 workspace is \
             sourced (setup.bash), or port the file to XML or YAML launch format"
        )
    }
}

/// Arguments of `ruro ros2 import`.
#[derive(Debug, clap::Args)]
pub struct ImportArgs {
    /// Launch file: XML or YAML (Python with --eval).
    file: PathBuf,
    /// Launch arguments, `name:=value`.
    args: Vec<String>,
    /// Parameter file given to every node (repeatable).
    #[arg(long = "params", value_name = "FILE")]
    params: Vec<PathBuf>,
    /// Output graph file (default: stdout).
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Graph name (default: the launch file's name).
    #[arg(long)]
    name: Option<String>,
    /// Add a ros2/sub bridge, e.g. `/odom=nav_msgs/msg/Odometry` (repeatable).
    #[arg(long = "sub", value_name = "TOPIC=TYPE")]
    subscribe: Vec<String>,
    /// Add a ros2/pub bridge, e.g. `/cmd_vel=geometry_msgs/msg/Twist` (repeatable).
    #[arg(long = "pub", value_name = "TOPIC=TYPE")]
    publish: Vec<String>,
    /// Zenoh router for the ROS nodes and bridges (default: theirs,
    /// tcp/127.0.0.1:7447).
    #[arg(long)]
    router: Option<String>,
    /// Evaluate a Python launch file with `ros2 launch` (needs a sourced ROS 2
    /// workspace); its processes are recorded, not run.
    #[arg(long)]
    eval: bool,
}

fn topic_type(s: &str) -> Result<(String, String)> {
    let (t, ty) = s
        .split_once('=')
        .ok_or_else(|| anyhow!("expected TOPIC=TYPE, got `{s}`"))?;
    Ok((t.to_owned(), ty.to_owned()))
}

/// Runs `ruro ros2 import`.
pub fn run(a: ImportArgs) -> Result<()> {
    let mut opts = ImportOptions {
        name: a.name,
        router: a.router,
        ..Default::default()
    };
    for arg in &a.args {
        let (k, v) = arg
            .split_once(":=")
            .ok_or_else(|| anyhow!("launch arguments are `name:=value`, got `{arg}`"))?;
        opts.args.push((k.to_owned(), v.to_owned()));
    }
    for p in &a.params {
        opts.params_files.push(std::path::absolute(p)?);
    }
    opts.subscribe = a
        .subscribe
        .iter()
        .map(|s| topic_type(s))
        .collect::<Result<_>>()?;
    opts.publish = a
        .publish
        .iter()
        .map(|s| topic_type(s))
        .collect::<Result<_>>()?;
    let is_python = a.file.extension().is_some_and(|e| e == "py");
    let imported = if is_python && a.eval {
        eval::eval_python(&a.file, &opts)?
    } else {
        import(&a.file, &opts)?
    };
    let text = render(&imported, &a.file)?;
    match &a.output {
        Some(path) => {
            std::fs::write(path, &text)?;
            eprintln!(
                "wrote {} with {} node(s)",
                path.display(),
                imported.graph.nodes.len()
            );
        }
        None => print!("{text}"),
    }
    if !imported.warnings.is_empty() {
        eprintln!("{} warning(s):", imported.warnings.len());
        for w in &imported.warnings {
            eprintln!("  {w}");
        }
    }
    Ok(())
}

/// Converts the launch file at `path` into a ruro graph.
pub fn import(path: &Path, opts: &ImportOptions) -> Result<Imported> {
    let root = load_file(path)?;
    let mut imp = Importer {
        ament: opts
            .ament_prefix_path
            .clone()
            .unwrap_or_else(|| std::env::var("AMENT_PREFIX_PATH").unwrap_or_default()),
        opts: opts.clone(),
        graph: Graph::new(graph_name(path, opts)),
        warnings: Vec::new(),
        depth: 0,
    };
    let mut scope = Scope {
        vars: opts.args.iter().cloned().collect(),
        file: std::path::absolute(path).unwrap_or_else(|_| path.to_owned()),
        ..Default::default()
    };
    imp.children(&root, &mut scope)?;
    imp.add_bridges();
    apply_common(&mut imp.graph, opts);
    Ok(Imported {
        graph: imp.graph,
        warnings: imp.warnings,
    })
}

fn graph_name(path: &Path, opts: &ImportOptions) -> String {
    let raw = opts.name.clone().unwrap_or_else(|| {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
            .split('.')
            .next()
            .unwrap_or_default()
            .to_owned()
    });
    let mut name: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    name.truncate(64);
    if name.is_empty() {
        name.push_str("ros2");
    }
    name
}

impl Importer {
    fn add_bridges(&mut self) {
        let subs = self.opts.subscribe.clone();
        let pubs = self.opts.publish.clone();
        for (kind, list) in [("ros2/sub", subs), ("ros2/pub", pubs)] {
            for (topic, ty) in list {
                let dir = kind.trim_start_matches("ros2/");
                let base = format!("{dir}_{}", topic.trim_start_matches('/').replace('/', "_"));
                let id = self.graph.unique_id(&id_base(&base));
                let mut node = NodeInstance::new(kind);
                node.params.insert("topic".into(), topic.into());
                node.params.insert("type".into(), ty.into());
                let n = self.graph.nodes.len();
                node.pos = Some([(n % 4) as f32 * 260.0, (n / 4) as f32 * 140.0]);
                self.graph.nodes.insert(id, node);
            }
        }
    }
}

/// The graph YAML with the warnings as a comment block at the top.
pub fn render(imported: &Imported, source: &Path) -> Result<String> {
    let mut out = format!(
        "# Imported from {} by `ruro ros2 import`.\n",
        source.display()
    );
    if imported.warnings.is_empty() {
        out.push_str("# Everything was converted.\n");
    } else {
        out.push_str("# Warnings:\n");
        for w in &imported.warnings {
            for (i, line) in w.lines().enumerate() {
                out.push_str(if i == 0 { "# - " } else { "#   " });
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    out.push_str(&imported.graph.to_yaml()?);
    Ok(out)
}

mod eval;

#[cfg(test)]
mod tests;
