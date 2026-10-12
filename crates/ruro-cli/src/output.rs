//! Terminal formatting.

use ruro_graph::Registry;
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

pub fn log_line(l: &LogLine) -> String {
    let level = match l.level {
        Level::Error => "\x1b[31mERROR\x1b[0m",
        Level::Warn => "\x1b[33m WARN\x1b[0m",
        Level::Info => "\x1b[32m INFO\x1b[0m",
        Level::Debug => "DEBUG",
        Level::Trace => "TRACE",
    };
    let source = match &l.node {
        Some(n) => format!("{}/{n}", l.group),
        None => l.group.clone(),
    };
    format!(
        "{} {level} \x1b[2m{source}\x1b[0m {}",
        clock(l.ts_ns),
        l.message
    )
}

/// Local wall-clock time `HH:MM:SS.mmm`.
pub fn clock(ts_ns: u64) -> String {
    let secs = (ts_ns / 1_000_000_000) as libc::time_t;
    let millis = (ts_ns / 1_000_000) % 1000;
    // SAFETY: localtime_r writes only into the provided struct.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&secs, &mut tm);
        tm
    };
    format!(
        "{:02}:{:02}:{:02}.{millis:03}",
        tm.tm_hour, tm.tm_min, tm.tm_sec
    )
}

pub fn print_status(status: &DaemonStatus, metrics: &[GroupMetrics]) {
    println!(
        "daemon {}  v{}  pid {}  up {}",
        status.name,
        status.version,
        status.pid,
        duration(status.uptime_s)
    );
    let Some(graph) = &status.graph else {
        println!("no graph deployed");
        return;
    };
    println!("graph  {graph}\n");
    println!(
        "{:<14} {:<10} {:>7} {:>9} {:>9} {:>9} {:>6} {:>7}",
        "NODE", "STATE", "HZ", "P50", "P99", "AGE", "CPU", "ERRORS"
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
            "\x1b[1mgroup {}\x1b[0m  {state}  pid {pid}{restarts}",
            g.name
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
                        "  {:<12} {:<10} {:>7.1} {:>9} {:>9} {:>9} {:>5.1}% {:>7}",
                        id,
                        state,
                        n.hz,
                        micros(n.step_p50_us),
                        micros(n.step_p99_us),
                        n.input_age_ms.map_or("-".into(), |a| micros(a * 1000.0)),
                        n.cpu_pct,
                        n.errors
                    );
                    if let Some(e) = &n.last_error {
                        println!("  {:<12} \x1b[31m{e}\x1b[0m", "");
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

fn duration(s: f64) -> String {
    let s = s as u64;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m{}s", s / 60, s % 60),
        _ => format!("{}h{}m", s / 3600, s / 60 % 60),
    }
}
