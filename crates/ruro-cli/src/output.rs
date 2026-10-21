//! Terminal formatting.

use ruro_graph::{Graph, Registry};
use ruro_proto::{DaemonStatus, GroupMetrics, GroupState, Level, LogLine, NodeState, Preview};

/// Prints a line from a background callback. Exits quietly when stdout is
/// closed, e.g. when piped into `head`.
pub fn emit(line: &str) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    if writeln!(out, "{line}").and_then(|_| out.flush()).is_err() {
        std::process::exit(0);
    }
}

pub fn print_registry(registry: &Registry, verbose: bool) {
    let width = registry.iter().map(|d| d.kind.len()).max().unwrap_or(0);
    for d in registry.iter() {
        println!("{:width$}  {}", d.kind, d.doc);
        if verbose {
            for p in &d.inputs {
                println!("{:width$}    in   {:<12} {}", "", p.name, p.ty);
            }
            for p in &d.outputs {
                println!("{:width$}    out  {:<12} {}", "", p.name, p.ty);
            }
            for p in &d.params {
                let range = match (p.min, p.max) {
                    (Some(a), Some(b)) => format!(" [{a}, {b}]"),
                    _ => String::new(),
                };
                println!(
                    "{:width$}    param {:<11} = {}{range}  {}",
                    "", p.name, p.default, p.doc
                );
            }
            if let Some(r) = d.default_rate {
                println!("{:width$}    rate {r} Hz", "");
            }
            println!();
        }
    }
}

pub fn preview_line(p: &Preview) -> String {
    match p {
        Preview::Image { width, height, .. } => format!("image {width}x{height} (preview)"),
        Preview::Values(values) => values
            .iter()
            .map(|(k, v)| format!("{k}={v:.4}"))
            .collect::<Vec<_>>()
            .join("  "),
        Preview::Text(t) => t.clone(),
        Preview::Points { frame, points } => format!("{} points in `{frame}`", points.len()),
        Preview::Frames(frames) => frames
            .iter()
            .map(|f| {
                let [x, y, z] = f.translation;
                format!("{} ({x:.3}, {y:.3}, {z:.3})", f.name)
            })
            .collect::<Vec<_>>()
            .join("  "),
    }
}

/// Whether output gets ANSI colours: on a terminal, unless `NO_COLOR` is set.
fn color() -> bool {
    use std::io::IsTerminal;
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none())
}

/// `text` in the SGR style `sgr` (e.g. `1;31`) when colouring.
fn paint(sgr: &str, text: &str) -> String {
    styled(color(), sgr, text)
}

fn styled(on: bool, sgr: &str, text: &str) -> String {
    if on {
        format!("\x1b[{sgr}m{text}\x1b[0m")
    } else {
        text.to_owned()
    }
}

pub fn log_line(l: &LogLine) -> String {
    log_line_styled(l, color())
}

fn log_line_styled(l: &LogLine, on: bool) -> String {
    let paint = |sgr: &str, text: &str| styled(on, sgr, text);
    let level = match l.level {
        Level::Error => paint("31", "ERROR"),
        Level::Warn => paint("33", " WARN"),
        Level::Info => paint("32", " INFO"),
        Level::Debug => "DEBUG".into(),
        Level::Trace => "TRACE".into(),
    };
    let source = match &l.node {
        Some(n) => format!("{}/{n}", l.group),
        None => l.group.clone(),
    };
    format!(
        "{} {level} {} {}",
        clock(l.ts_ns),
        paint("2", &source),
        l.message
    )
}

/// Local wall-clock time `HH:MM:SS.mmm` (UTC on Windows).
pub fn clock(ts_ns: u64) -> String {
    let t = ruro_proto::local_time((ts_ns / 1_000_000_000) as i64);
    let millis = (ts_ns / 1_000_000) % 1000;
    format!("{:02}:{:02}:{:02}.{millis:03}", t.hour, t.minute, t.second)
}

/// Where to release the stop latched at daemon `at` by `by`: a stop is
/// released where it was engaged, or at any daemon with `--all`.
pub fn release_hint(at: &str, by: &[String], here: &str) -> String {
    let daemon = |d: &str| {
        if d == here {
            String::new()
        } else {
            format!("--daemon {d} ")
        }
    };
    match by {
        // Daemons that do not say who engaged it.
        [] => format!("release with `ruro {}estop --release`", daemon(at)),
        [only] => format!("release with `ruro {}estop --release`", daemon(only)),
        _ => {
            let who: Vec<String> = by.iter().map(|o| format!("`{o}`")).collect();
            format!(
                "engaged by {}: release each there, or all with `ruro {}estop --release --all`",
                who.join(", "),
                daemon(at)
            )
        }
    }
}

/// Lines about the e-stop of the other machines running the graph, as
/// seen from daemon `here`.
pub fn peer_estop_lines(peers: &[DaemonStatus], here: &str) -> Vec<String> {
    peers
        .iter()
        .filter(|p| p.estopped)
        .map(|p| {
            format!(
                "emergency stop engaged on `{}` ({})",
                p.name,
                release_hint(&p.name, &p.estop_by, here)
            )
        })
        .collect()
}

/// `peers` are the statuses of the other daemons running parts of the graph.
/// `v0.1.0 (x86_64-unknown-linux-gnu)`; daemons that predate the target
/// show only the version.
pub fn version_cell(status: &DaemonStatus) -> String {
    if status.target.is_empty() {
        format!("v{}", status.version)
    } else {
        format!("v{} ({})", status.version, status.target)
    }
}

pub fn print_status(status: &DaemonStatus, metrics: &[GroupMetrics], peers: &[DaemonStatus]) {
    println!(
        "daemon {}  {}  pid {}  up {}",
        status.name,
        version_cell(status),
        status.pid,
        duration(status.uptime_s)
    );
    if let Some(u) = &status.update {
        println!("last update: {u}");
    }
    if !peers.is_empty() {
        let others: Vec<String> = peers
            .iter()
            .map(|p| format!("{} {}", p.name, version_cell(p)))
            .collect();
        println!("other machines: {}", others.join(", "));
    }
    if !status.unreachable.is_empty() {
        let names: Vec<String> = status
            .unreachable
            .iter()
            .map(|m| format!("`{m}`"))
            .collect();
        println!(
            "{}",
            paint(
                "1;31",
                &format!(
                    "not answering: {} (their part of the graph is deployed again when they are back)",
                    names.join(", ")
                )
            )
        );
    }
    if status.estopped {
        println!(
            "{} ({})",
            paint("1;31", "EMERGENCY STOP ENGAGED"),
            release_hint(&status.name, &status.estop_by, &status.name)
        );
    }
    for line in peer_estop_lines(peers, &status.name) {
        println!("{}", paint("1;31", &line));
    }
    if let Some(r) = &status.recording {
        println!("recording to {r}");
    }
    if let Some(p) = &status.playback {
        println!(
            "playing {}  {:.1}/{:.1} s  x{}{}",
            p.file,
            p.position_s,
            p.duration_s,
            p.rate,
            if p.paused { "  paused" } else { "" }
        );
    }
    let Some(graph) = &status.graph else {
        println!("no graph deployed");
        return;
    };
    println!("graph  {graph}\n");
    println!(
        "{:<14} {:<10} {:>7} {:>9} {:>9} {:>9} {:>7} {:>6} {:>7} {:>9}",
        "NODE", "STATE", "HZ", "P50", "P99", "AGE", "DROPS", "CPU", "ERRORS", "JIT P99"
    );
    for g in &status.groups {
        let state = match g.state {
            GroupState::Running => "running",
            GroupState::Backoff => "restarting",
            GroupState::Failed => "failed",
            GroupState::Stopped => "stopped",
        };
        let pid = g.pid.map_or("-".to_string(), |p| p.to_string());
        let restarts = if g.restarts > 0 {
            format!("  restarts {}", g.restarts)
        } else {
            String::new()
        };
        println!(
            "{}  {state}  pid {pid}{restarts}",
            paint("1", &format!("group {}", g.name))
        );
        let m = metrics.iter().find(|m| m.group == g.name);
        for id in &g.nodes {
            match m.and_then(|m| m.nodes.iter().find(|n| &n.id == id)) {
                Some(n) => {
                    let state = match n.state {
                        NodeState::Starting => "starting",
                        NodeState::Running => "running",
                        NodeState::Failed => "FAILED",
                        NodeState::Stopped => "stopped",
                    };
                    println!(
                        "  {:<12} {:<10} {:>7.1} {:>9} {:>9} {:>9} {:>7} {:>5.1}% {:>7} {:>9}",
                        id,
                        state,
                        n.hz,
                        micros(n.step_p50_us),
                        micros(n.step_p99_us),
                        n.input_age_ms.map_or("-".into(), |a| micros(a * 1000.0)),
                        n.dropped,
                        n.cpu_pct,
                        n.errors,
                        n.timing
                            .as_ref()
                            .map_or("-".into(), |t| micros(t.jitter_p99_us))
                    );
                    if let Some(t) = &n.timing
                        && (t.deadline_misses > 0 || t.overruns > 0)
                    {
                        println!(
                            "  {:<12} jitter max {}  deadline misses {}  cycle overruns {}",
                            "",
                            micros(t.jitter_max_us),
                            t.deadline_misses,
                            t.overruns
                        );
                    }
                    if let Some(e) = &n.last_error {
                        println!("  {:<12} {}", "", paint("31", e));
                    }
                }
                None => println!("  {id:<12} -"),
            }
        }
        if let Some(exit) = &g.last_exit {
            println!("  last exit: {exit}");
        }
    }
}

fn micros(us: f64) -> String {
    if us >= 1000.0 {
        format!("{:.2}ms", us / 1000.0)
    } else {
        format!("{us:.0}µs")
    }
}

pub fn duration(s: f64) -> String {
    let s = s as u64;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m{}s", s / 60, s % 60),
        _ => format!("{}h{}m", s / 3600, s / 60 % 60),
    }
}

/// `ruro overlays`: each overlay of `graph` and what it changes.
pub fn overlays(graph: &Graph) -> String {
    if graph.overlays.is_empty() {
        return format!("{} has no overlays\n", graph.name);
    }
    let mut out = String::new();
    for (name, overlay) in &graph.overlays {
        out.push_str(name);
        out.push('\n');
        for line in overlay.summary() {
            out.push_str(&format!("    {line}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlays_are_listed_with_their_changes() {
        let g = Graph::from_yaml(
            "name: g\nnodes:\n  a: { type: t/a }\noverlays:\n  sim: { clock: sim }\n  real: { nodes: { a: { type: t/b } } }\n",
        )
        .unwrap();
        assert_eq!(overlays(&g), "sim\n    clock sim\nreal\n    a: type t/b\n");
        assert_eq!(overlays(&Graph::new("p")), "p has no overlays\n");
    }

    #[test]
    fn colour_codes_only_when_colouring() {
        let line = LogLine {
            ts_ns: 0,
            level: Level::Error,
            group: "vision".into(),
            node: Some("blur".into()),
            message: "failed".into(),
        };
        let plain = log_line_styled(&line, false);
        assert!(plain.ends_with(" ERROR vision/blur failed"), "{plain:?}");
        assert!(log_line_styled(&line, true).contains("\x1b[31mERROR\x1b[0m"));
    }

    #[test]
    fn stops_on_other_machines_are_shown() {
        let peer = |name: &str, estopped| DaemonStatus {
            name: name.into(),
            version: String::new(),
            pid: 1,
            uptime_s: 0.0,
            graph: Some("g".into()),
            groups: Vec::new(),
            recording: None,
            playback: None,
            estopped,
            target: String::new(),
            update: None,
            estop_by: Vec::new(),
            unreachable: Vec::new(),
        };
        let lines = peer_estop_lines(&[peer("robot", true), peer("arm", false)], "laptop");
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("`robot`"), "{lines:?}");

        // Released where it was engaged, which may be neither machine.
        let by = |o: &[&str]| o.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            release_hint("robot", &by(&["laptop"]), "laptop"),
            "release with `ruro estop --release`"
        );
        assert_eq!(
            release_hint("laptop", &by(&["robot"]), "laptop"),
            "release with `ruro --daemon robot estop --release`"
        );
        assert_eq!(
            release_hint("robot", &by(&["laptop", "console"]), "laptop"),
            "engaged by `laptop`, `console`: release each there, or all with \
             `ruro --daemon robot estop --release --all`"
        );
    }
}
