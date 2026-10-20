//! Python launch files, evaluated by ROS 2 itself.
//!
//! `ros2 launch` runs the file with a launch prefix that records each
//! process's command line, environment and parameter files instead of
//! starting it. The recorded commands then become `ros2/node` nodes.

use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::{GROUP, ImportOptions, Imported, graph_name};
use crate::external::{RosProcess, flatten_params};

/// Stands in for every process `ros2 launch` starts: writes one JSON line
/// with what would have run, then waits to be stopped.
const RECORDER: &str = r#"
import json, os, signal, sys, time
argv = sys.argv[1:]
files = {}
for i, a in enumerate(argv):
    if a == '--params-file' and i + 1 < len(argv):
        try:
            with open(argv[i + 1]) as f:
                files[argv[i + 1]] = f.read()
        except OSError:
            pass
with open(os.environ['RURO_LAUNCH_RECORD'], 'a') as out:
    out.write(json.dumps({'argv': argv, 'env': dict(os.environ), 'files': files}) + '\n')
signal.signal(signal.SIGINT, lambda *a: sys.exit(0))
signal.signal(signal.SIGTERM, lambda *a: sys.exit(0))
while True:
    time.sleep(1)
"#;

/// Splits `.../lib/<package>/<executable>` into package and executable.
fn ros_executable(program: &str) -> Option<(String, String)> {
    let p = Path::new(program);
    let exe = p.file_name()?.to_str()?;
    let pkg_dir = p.parent()?;
    let pkg = pkg_dir.file_name()?.to_str()?;
    (pkg_dir.parent()?.file_name()? == "lib").then(|| (pkg.to_owned(), exe.to_owned()))
}

/// The parameters in a ROS parameter file: every `ros__parameters` map,
/// flattened. `None` if it does not parse.
fn params_from_file(text: &str) -> Option<Vec<(String, Value)>> {
    fn find(v: &Value, out: &mut Vec<(String, Value)>) {
        if let Value::Object(map) = v {
            for (k, v) in map {
                if k == "ros__parameters" {
                    flatten_params("", v, out);
                } else {
                    find(v, out);
                }
            }
        }
    }
    let v: Value = serde_saphyr::from_str(text).ok()?;
    let mut out = Vec::new();
    find(&v, &mut out);
    Some(out)
}

/// A recorded command line as a process: package and executable for ROS
/// executables, ROS arguments unpacked. Parameter files `launch_ros` wrote
/// to a temporary directory are inlined from their recorded contents.
pub(super) fn process_from_argv(
    argv: &[String],
    env: &[(String, String)],
    files: &BTreeMap<String, String>,
) -> RosProcess {
    let mut p = RosProcess {
        env: env.to_vec(),
        ..Default::default()
    };
    let Some((program, rest)) = argv.split_first() else {
        return p;
    };
    let ros_at = rest.iter().position(|a| a == "--ros-args");
    let (args, ros) = match ros_at {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, &[][..]),
    };
    match ros_executable(program) {
        Some((pkg, exe)) => {
            p.package = pkg;
            p.executable = exe;
            p.arguments = args.to_vec();
        }
        None => {
            p.command = std::iter::once(program.clone())
                .chain(args.iter().cloned())
                .collect();
        }
    }
    let mut i = 0;
    while i < ros.len() {
        let a = ros[i].as_str();
        let next = ros.get(i + 1).cloned();
        match (a, next) {
            ("-r" | "--remap", Some(r)) => {
                match r.split_once(":=") {
                    Some(("__node" | "__name", n)) => p.name = n.to_owned(),
                    Some(("__ns", n)) => p.namespace = n.to_owned(),
                    Some((f, t)) => p.remaps.push((f.to_owned(), t.to_owned())),
                    None => p.ros_args.extend([a.to_owned(), r]),
                }
                i += 2;
            }
            ("-p" | "--param", Some(kv)) => {
                match kv.split_once(":=") {
                    Some((k, v)) => p.parameters.push((k.to_owned(), super::param_value(v))),
                    None => p.ros_args.extend([a.to_owned(), kv]),
                }
                i += 2;
            }
            ("--params-file", Some(f)) => {
                let tmp = std::env::temp_dir();
                let inlined = Path::new(&f)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("launch_params_"))
                    || Path::new(&f).starts_with(&tmp);
                match (inlined, files.get(&f).and_then(|t| params_from_file(t))) {
                    (true, Some(params)) => p.parameters.extend(params),
                    _ => p.params_files.push(f),
                }
                i += 2;
            }
            _ => {
                p.ros_args.push(a.to_owned());
                i += 1;
            }
        }
    }
    p
}

/// Evaluates a Python launch file with `ros2 launch` and converts the
/// processes it would start.
pub fn eval_python(path: &Path, opts: &ImportOptions) -> Result<Imported> {
    if !super::ros_environment() {
        bail!("{}", super::python_message(path));
    }
    let dir = std::env::temp_dir().join(format!("ruro-launch-eval-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let recorder = dir.join("record.py");
    let record = dir.join("record.jsonl");
    std::fs::write(&recorder, RECORDER)?;
    let _ = std::fs::remove_file(&record);
    let mut cmd = Command::new("ros2");
    cmd.arg("launch")
        .arg(path)
        .args(opts.args.iter().map(|(k, v)| format!("{k}:={v}")))
        .arg("--launch-prefix")
        .arg(format!("python3 {}", recorder.display()))
        .env("RURO_LAUNCH_RECORD", &record)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .context("running `ros2 launch` (is a ROS 2 workspace sourced?)")?;
    let stderr = child.stderr.take().expect("piped");
    let errors = std::thread::spawn(move || {
        std::io::BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
            .collect::<Vec<_>>()
    });
    let stdout = child.stdout.take().expect("piped");
    let output = std::thread::spawn(move || {
        std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
            .collect::<Vec<_>>()
    });
    // Wait until the recorded processes stop growing in number.
    let start = Instant::now();
    let mut last = (0, Instant::now());
    let exited = loop {
        std::thread::sleep(Duration::from_millis(200));
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        let n = std::fs::read_to_string(&record)
            .map(|t| t.lines().count())
            .unwrap_or(0);
        if n != last.0 {
            last = (n, Instant::now());
        }
        let quiet = last.1.elapsed() > Duration::from_secs(3);
        if (n > 0 && quiet) || start.elapsed() > Duration::from_secs(30) {
            break None;
        }
    };
    if exited.is_none() {
        // SIGINT lets ros2 launch stop the recorders.
        // SAFETY: kill(2) on our own child's pid.
        unsafe {
            libc::kill(child.id() as i32, libc::SIGINT);
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while child.try_wait()?.is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
    let errors = errors.join().unwrap_or_default();
    let _ = output.join();
    let text = std::fs::read_to_string(&record).unwrap_or_default();
    if text.trim().is_empty() {
        bail!(
            "`ros2 launch {}` started no process{}",
            path.display(),
            if errors.is_empty() {
                String::new()
            } else {
                format!(":\n{}", errors.join("\n"))
            }
        );
    }
    let base_env: BTreeMap<String, String> = std::env::vars().collect();
    let mut graph = ruro_graph::Graph::new(graph_name(path, opts));
    let mut warnings = vec![format!(
        "{}: evaluated with `ros2 launch`; conditions and substitutions were resolved for \
         this machine's environment and these arguments",
        path.display()
    )];
    if exited.is_none() {
        // Delays and event handlers can start processes at any time; the
        // launch was stopped once none had started for 3 s.
        let source = std::fs::read_to_string(path).unwrap_or_default();
        let late: Vec<&str> = ["TimerAction", "RegisterEventHandler"]
            .into_iter()
            .filter(|a| source.contains(a))
            .collect();
        warnings.push(format!(
            "{}: stopped after 3 s without a new process; processes started later{} are \
             not in the graph",
            path.display(),
            if late.is_empty() {
                String::new()
            } else {
                format!(" (it uses {})", late.join(" and "))
            }
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for line in text.lines() {
        let rec: Value = serde_json::from_str(line).context("reading the recorded processes")?;
        let argv: Vec<String> = rec["argv"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        if !seen.insert(argv.clone()) {
            continue;
        }
        let env: Vec<(String, String)> = rec["env"]
            .as_object()
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                    .filter(|(k, v)| k != "RURO_LAUNCH_RECORD" && base_env.get(k) != Some(v))
                    .collect()
            })
            .unwrap_or_default();
        let files: BTreeMap<String, String> = rec["files"]
            .as_object()
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                    .collect()
            })
            .unwrap_or_default();
        let mut p = process_from_argv(&argv, &env, &files);
        let mut params = opts
            .params_files
            .iter()
            .map(|f| f.display().to_string())
            .collect::<Vec<_>>();
        params.append(&mut p.params_files);
        p.params_files = params;
        let label = p.label();
        let base = label.trim_start_matches('/').replace('/', "_");
        let mut id = graph.unique_id(if base.is_empty() { "ros2_node" } else { &base });
        id.truncate(64);
        let mut node = ruro_graph::NodeInstance::new("ros2/node");
        node.group = Some(GROUP.into());
        node.params = p.to_params();
        node.params.insert("respawn".into(), "never".into());
        let n = graph.nodes.len();
        node.pos = Some([(n % 4) as f32 * 260.0, (n / 4) as f32 * 140.0]);
        graph.nodes.insert(id, node);
    }
    warnings.push(format!(
        "respawn settings of Python launch files are not visible to the recorder; every node \
         was given respawn: never ({} node(s))",
        graph.nodes.len()
    ));
    let mut imported = Imported { graph, warnings };
    let mut extra = super::Importer {
        ament: String::new(),
        opts: opts.clone(),
        graph: std::mem::take(&mut imported.graph),
        warnings: Vec::new(),
        depth: 0,
    };
    extra.add_bridges();
    imported.graph = extra.graph;
    super::apply_common(&mut imported.graph, opts);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(imported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_command_lines_become_processes() {
        let argv: Vec<String> = [
            "/opt/ros/jazzy/lib/demo_nodes_cpp/talker",
            "--extra",
            "--ros-args",
            "-r",
            "__node:=talker",
            "-r",
            "__ns:=/robot",
            "-r",
            "chatter:=out",
            "-p",
            "rate:=2",
            "--params-file",
            "/tmp/launch_params_abc",
            "--params-file",
            "/etc/robot.yaml",
            "--log-level",
            "debug",
        ]
        .map(String::from)
        .into();
        let files: BTreeMap<String, String> = [(
            "/tmp/launch_params_abc".to_owned(),
            "/**:\n  ros__parameters:\n    mode: fast\n    pid:\n      kp: 1.5\n".to_owned(),
        )]
        .into();
        let p = process_from_argv(&argv, &[("A".into(), "1".into())], &files);
        assert_eq!(
            (p.package.as_str(), p.executable.as_str()),
            ("demo_nodes_cpp", "talker")
        );
        assert_eq!(p.arguments, ["--extra"]);
        assert_eq!(
            (p.name.as_str(), p.namespace.as_str()),
            ("talker", "/robot")
        );
        assert_eq!(p.remaps, [("chatter".into(), "out".into())]);
        assert_eq!(
            p.parameters,
            [
                ("rate".to_owned(), Value::from(2)),
                ("mode".to_owned(), Value::from("fast")),
                ("pid.kp".to_owned(), Value::from(1.5))
            ]
        );
        assert_eq!(p.params_files, ["/etc/robot.yaml"]);
        assert_eq!(p.ros_args, ["--log-level", "debug"]);
        assert_eq!(p.env, [("A".to_owned(), "1".to_owned())]);

        let other = process_from_argv(&["/usr/bin/env".into(), "x".into()], &[], &files);
        assert_eq!(other.command, ["/usr/bin/env", "x"]);
    }
}
