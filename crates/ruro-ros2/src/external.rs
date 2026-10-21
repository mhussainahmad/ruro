//! ROS 2 node processes as ruro runs them (`ros2/node`): what to start and
//! with which ROS arguments.
//!
//! A [`RosProcess`] is stored in a graph as the string parameters of a
//! `ros2/node` node ([`RosProcess::to_params`]), so lists are packed one
//! entry per line and parameter values as JSON:
//!
//! | parameter | meaning |
//! |---|---|
//! | `package`, `executable` | `ros2 run <package> <executable>` |
//! | `command` | any command instead, split like a shell would |
//! | `name`, `namespace` | node name and namespace (`__node`, `__ns`) |
//! | `remaps` | `from:=to`, one per line |
//! | `parameters` | a JSON object; nested objects become dotted names |
//! | `params_files` | parameter files, one per line |
//! | `arguments` | arguments before `--ros-args`, split like a shell would |
//! | `ros_args` | extra ROS arguments, split like a shell would |
//! | `env` | `NAME=value`, one per line |
//! | `prefix` | a command put in front, e.g. `gdb -ex run --args` |

use std::collections::BTreeMap;

use anyhow::{Result, anyhow, bail};
use serde_json::Value;

/// A ROS 2 node (or any program) to run.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RosProcess {
    pub package: String,
    pub executable: String,
    /// A command line used instead of `package`/`executable`.
    pub command: Vec<String>,
    pub name: String,
    pub namespace: String,
    pub remaps: Vec<(String, String)>,
    /// Parameter names (dotted for nested ones) and values.
    pub parameters: Vec<(String, Value)>,
    pub params_files: Vec<String>,
    pub arguments: Vec<String>,
    pub ros_args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub prefix: Vec<String>,
}

/// Splits a command line into words the way a POSIX shell does for quotes
/// and backslashes (no variables, globs or operators).
pub fn split_words(s: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => cur.push(c),
                        None => bail!("unterminated ' in `{s}`"),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '\\' | '$' | '`')) => cur.push(c),
                            Some(c) => {
                                cur.push('\\');
                                cur.push(c);
                            }
                            None => bail!("unterminated \" in `{s}`"),
                        },
                        Some(c) => cur.push(c),
                        None => bail!("unterminated \" in `{s}`"),
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(c) = chars.next() {
                    cur.push(c);
                }
            }
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    Ok(words)
}

/// Quotes a word so that [`split_words`] (or a shell) reads it back.
pub fn quote_word(w: &str) -> String {
    if !w.is_empty()
        && w.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=,+@%^".contains(c))
    {
        w.to_owned()
    } else {
        format!("'{}'", w.replace('\'', "'\\''"))
    }
}

/// Joins words into a command line.
pub fn join_words(words: &[String]) -> String {
    words
        .iter()
        .map(|w| quote_word(w))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A parameter value as ROS reads it after `name:=`: YAML, of which JSON is
/// a subset. Strings are quoted so that `"true"` stays a string.
pub fn param_value_text(v: &Value) -> String {
    v.to_string()
}

/// Flattens nested objects into dotted parameter names.
pub fn flatten_params(prefix: &str, v: &Value, out: &mut Vec<(String, Value)>) {
    match v {
        Value::Object(map) => {
            for (k, v) in map {
                let name = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten_params(&name, v, out);
            }
        }
        v => out.push((prefix.to_owned(), v.clone())),
    }
}

fn lines(s: &str) -> impl Iterator<Item = &str> {
    s.lines().map(str::trim).filter(|l| !l.is_empty())
}

impl RosProcess {
    /// The `ros2/node` parameters describing this process.
    pub fn to_params(&self) -> BTreeMap<String, Value> {
        let mut p = BTreeMap::new();
        let mut put = |k: &str, v: String| {
            if !v.is_empty() {
                p.insert(k.to_owned(), Value::String(v));
            }
        };
        put("package", self.package.clone());
        put("executable", self.executable.clone());
        put("command", join_words(&self.command));
        put("name", self.name.clone());
        put("namespace", self.namespace.clone());
        put(
            "remaps",
            self.remaps
                .iter()
                .map(|(f, t)| format!("{f}:={t}\n"))
                .collect(),
        );
        if !self.parameters.is_empty() {
            let mut obj = serde_json::Map::new();
            for (k, v) in &self.parameters {
                obj.insert(k.clone(), v.clone());
            }
            put(
                "parameters",
                serde_json::to_string(&Value::Object(obj)).expect("JSON"),
            );
        }
        put(
            "params_files",
            self.params_files.iter().map(|f| format!("{f}\n")).collect(),
        );
        put("arguments", join_words(&self.arguments));
        put("ros_args", join_words(&self.ros_args));
        put(
            "env",
            self.env.iter().map(|(k, v)| format!("{k}={v}\n")).collect(),
        );
        put("prefix", join_words(&self.prefix));
        p
    }

    /// Reads the `ros2/node` parameters back.
    #[allow(clippy::too_many_arguments)]
    pub fn from_params(
        package: &str,
        executable: &str,
        command: &str,
        name: &str,
        namespace: &str,
        remaps: &str,
        parameters: &str,
        params_files: &str,
        arguments: &str,
        ros_args: &str,
        env: &str,
        prefix: &str,
    ) -> Result<Self> {
        let mut remap_list = Vec::new();
        for l in lines(remaps).flat_map(|l| l.split(';')).map(str::trim) {
            if l.is_empty() {
                continue;
            }
            let (f, t) = l
                .split_once(":=")
                .ok_or_else(|| anyhow!("remap `{l}` is not `from:=to`"))?;
            remap_list.push((f.trim().to_owned(), t.trim().to_owned()));
        }
        let mut params = Vec::new();
        if !parameters.trim().is_empty() {
            let v: Value = serde_json::from_str(parameters)
                .map_err(|e| anyhow!("`parameters` is not a JSON object: {e}"))?;
            if !v.is_object() {
                bail!("`parameters` is not a JSON object");
            }
            flatten_params("", &v, &mut params);
        }
        let mut env_list = Vec::new();
        for l in lines(env) {
            let (k, v) = l
                .split_once('=')
                .ok_or_else(|| anyhow!("env entry `{l}` is not `NAME=value`"))?;
            env_list.push((k.trim().to_owned(), v.to_owned()));
        }
        let p = Self {
            package: package.trim().to_owned(),
            executable: executable.trim().to_owned(),
            command: split_words(command)?,
            name: name.trim().to_owned(),
            namespace: namespace.trim().to_owned(),
            remaps: remap_list,
            parameters: params,
            params_files: lines(params_files).map(str::to_owned).collect(),
            arguments: split_words(arguments)?,
            ros_args: split_words(ros_args)?,
            env: env_list,
            prefix: split_words(prefix)?,
        };
        if p.command.is_empty() && (p.package.is_empty() || p.executable.is_empty()) {
            bail!("set `package` and `executable`, or `command`");
        }
        Ok(p)
    }

    /// Arguments after the program: `arguments`, then `--ros-args` with the
    /// node name, namespace, remaps, parameter files and parameters, then
    /// `ros_args`.
    pub fn args(&self) -> Vec<String> {
        let mut a = self.arguments.clone();
        let mut ros = Vec::new();
        if !self.name.is_empty() {
            ros.extend(["-r".into(), format!("__node:={}", self.name)]);
        }
        if !self.namespace.is_empty() {
            ros.extend(["-r".into(), format!("__ns:={}", self.namespace)]);
        }
        for (f, t) in &self.remaps {
            ros.extend(["-r".into(), format!("{f}:={t}")]);
        }
        for f in &self.params_files {
            ros.extend(["--params-file".into(), f.clone()]);
        }
        for (k, v) in &self.parameters {
            ros.extend(["-p".into(), format!("{k}:={}", param_value_text(v))]);
        }
        ros.extend(self.ros_args.iter().cloned());
        if !ros.is_empty() {
            a.push("--ros-args".into());
            a.extend(ros);
        }
        a
    }

    /// The full command line for an executable found at `program`.
    pub fn argv(&self, program: &str) -> Vec<String> {
        let mut v = self.prefix.clone();
        if self.command.is_empty() {
            v.push(program.to_owned());
            v.extend(self.args());
        } else {
            v.extend(self.command.iter().cloned());
            // A command gets the ROS arguments only if any are set.
            let args = self.args();
            v.extend(args);
        }
        v
    }

    /// A short name for logs: the node name, else the executable or command.
    pub fn label(&self) -> String {
        if !self.name.is_empty() {
            let ns = self.namespace.trim_end_matches('/');
            if ns.is_empty() {
                format!("/{}", self.name)
            } else {
                format!("{ns}/{}", self.name)
            }
        } else if !self.executable.is_empty() {
            format!("{}/{}", self.package, self.executable)
        } else {
            self.command.first().cloned().unwrap_or_default()
        }
    }
}

/// Finds `lib/<package>/<executable>` in the prefixes of `ament_prefix_path`
/// (colon separated), as `ros2 run` does.
pub fn find_executable(
    ament_prefix_path: &str,
    package: &str,
    executable: &str,
) -> Option<std::path::PathBuf> {
    ament_prefix_path
        .split(':')
        .filter(|p| !p.is_empty())
        .map(|p| {
            std::path::Path::new(p)
                .join("lib")
                .join(package)
                .join(executable)
        })
        .find(|p| std::fs::metadata(p).is_ok_and(|m| m.is_file() && executable_mode(&m)))
}

#[cfg(unix)]
fn executable_mode(m: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    m.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable_mode(_: &std::fs::Metadata) -> bool {
    true
}

/// Finds `share/<package>` in the prefixes of `ament_prefix_path`.
pub fn find_package_share(ament_prefix_path: &str, package: &str) -> Option<std::path::PathBuf> {
    ament_prefix_path
        .split(':')
        .filter(|p| !p.is_empty())
        .map(|p| std::path::Path::new(p).join("share").join(package))
        .find(|p| p.join("package.xml").is_file() || p.is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_split_and_join_like_a_shell() {
        assert_eq!(
            split_words(r#"a 'b c' "d \"e\"" f\ g"#).unwrap(),
            ["a", "b c", "d \"e\"", "f g"]
        );
        assert!(split_words("'open").is_err());
        let words: Vec<String> = ["x", "it's", "", "a b"].map(String::from).into();
        assert_eq!(split_words(&join_words(&words)).unwrap(), words);
    }

    #[test]
    fn parameters_roundtrip_and_build_ros_args() {
        let p = RosProcess {
            package: "demo_nodes_cpp".into(),
            executable: "talker".into(),
            name: "talker".into(),
            namespace: "/robot".into(),
            remaps: vec![("chatter".into(), "/out".into())],
            parameters: vec![
                ("rate".into(), 2.5.into()),
                ("mode".into(), "true".into()),
                ("pid.kp".into(), serde_json::json!([1, 2])),
            ],
            params_files: vec!["/etc/p.yaml".into()],
            arguments: vec!["--verbose".into()],
            env: vec![("RCUTILS_COLORIZED_OUTPUT".into(), "0".into())],
            ..Default::default()
        };
        let params = p.to_params();
        let s = |k: &str| params.get(k).and_then(Value::as_str).unwrap_or("");
        let back = RosProcess::from_params(
            s("package"),
            s("executable"),
            s("command"),
            s("name"),
            s("namespace"),
            s("remaps"),
            s("parameters"),
            s("params_files"),
            s("arguments"),
            s("ros_args"),
            s("env"),
            s("prefix"),
        )
        .unwrap();
        assert_eq!(back, p);
        assert_eq!(
            p.argv("/opt/ros/lib/demo_nodes_cpp/talker"),
            [
                "/opt/ros/lib/demo_nodes_cpp/talker",
                "--verbose",
                "--ros-args",
                "-r",
                "__node:=talker",
                "-r",
                "__ns:=/robot",
                "-r",
                "chatter:=/out",
                "--params-file",
                "/etc/p.yaml",
                "-p",
                "rate:=2.5",
                "-p",
                "mode:=\"true\"",
                "-p",
                "pid.kp:=[1,2]"
            ]
        );
        assert_eq!(p.label(), "/robot/talker");
    }

    #[test]
    fn nested_parameters_flatten_and_bad_input_is_refused() {
        let p = RosProcess::from_params(
            "p",
            "e",
            "",
            "",
            "",
            "a:=b; c:=d",
            r#"{"pid": {"kp": 1, "ki": 0.5}}"#,
            "",
            "",
            "",
            "",
            "",
        )
        .unwrap();
        assert_eq!(p.remaps.len(), 2);
        assert_eq!(
            p.parameters,
            [
                ("pid.kp".to_owned(), Value::from(1)),
                ("pid.ki".to_owned(), Value::from(0.5))
            ]
        );
        let bad = |remaps: &str, params: &str, pkg: &str| {
            RosProcess::from_params(pkg, "e", "", "", "", remaps, params, "", "", "", "", "")
                .is_err()
        };
        assert!(bad("nope", "", "p"));
        assert!(bad("", "[1]", "p"));
        assert!(bad("", "", ""));
    }
}
