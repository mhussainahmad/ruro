//! `ruro soak`: run a graph for a long time and check it stays healthy.
//!
//! Every interval it samples each group's resident memory (VmRSS of the host
//! process), the step latency p99 and drop counts the hosts report, and group
//! restarts. At the end it prints a summary and fails if memory grew by more
//! than the allowed amount after the warm-up, or if any group restarted or
//! any node failed.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use clap::Args;
use parking_lot::Mutex;
use ruro_proto::{Ctl, DaemonStatus, GroupMetrics, NodeState, Security};
use zenoh::Wait;

#[derive(Args)]
pub struct SoakArgs {
    graph: PathBuf,
    /// Seconds to run.
    #[arg(long, default_value_t = 600.0)]
    duration: f64,
    /// Seconds to run before the memory baseline is taken.
    #[arg(long, default_value_t = 60.0)]
    warmup: f64,
    /// Seconds between samples.
    #[arg(long, default_value_t = 5.0)]
    interval: f64,
    /// Fail if a group's memory grows by more than this many MiB after the warm-up.
    #[arg(long, default_value_t = 16.0)]
    max_rss_growth_mb: f64,
    /// Also write every sample to this CSV file.
    #[arg(long)]
    csv: Option<PathBuf>,
}

/// Resident set size of a process, in KiB.
pub fn rss_kib(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

#[derive(Default)]
struct NodeSummary {
    p99s: Vec<f64>,
    hz: Vec<f64>,
    dropped: u64,
    errors: u64,
    failed: bool,
    jitter_p99s: Vec<f64>,
}

#[derive(Default)]
struct GroupSummary {
    /// `(seconds since start, pid, RSS KiB)`.
    rss: Vec<(f64, u32, u64)>,
    restarts: u32,
}

fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

pub fn soak(endpoint: &str, name: Option<&str>, security: &Security, args: SoakArgs) -> Result<()> {
    if args.duration <= args.warmup {
        bail!("--duration must be longer than --warmup");
    }
    let (daemon, c) = crate::connect_or_start(endpoint, name, security)?;
    println!("{}", crate::deploy(&c, &args.graph)?);

    let latest: Arc<Mutex<HashMap<String, GroupMetrics>>> = Arc::default();
    let l = latest.clone();
    let _metrics = c
        .session
        .declare_subscriber(c.keys.all_metrics())
        .callback(move |s| {
            if let Ok(m) = ruro_proto::decode::<GroupMetrics>(&s.payload().to_bytes()) {
                l.lock().insert(m.group.clone(), m);
            }
        })
        .wait()
        .map_err(|e| anyhow!("{e}"))?;
    let status: Arc<Mutex<Option<DaemonStatus>>> = Arc::default();
    let s2 = status.clone();
    let _status = c
        .session
        .declare_subscriber(c.keys.status())
        .callback(move |s| {
            if let Ok(st) = ruro_proto::decode_status(&s.payload().to_bytes()) {
                *s2.lock() = Some(st);
            }
        })
        .wait()
        .map_err(|e| anyhow!("{e}"))?;

    let mut csv = match &args.csv {
        Some(p) => {
            let mut f =
                std::fs::File::create(p).with_context(|| format!("creating {}", p.display()))?;
            writeln!(
                f,
                "t_s,group,pid,rss_kib,node,hz,step_p99_us,jitter_p99_us,dropped,errors"
            )?;
            Some(f)
        }
        None => None,
    };

    let stop = crate::ctrl_c_flag()?;
    let start = Instant::now();
    let interval = Duration::from_secs_f64(args.interval.max(0.5));
    let mut groups: BTreeMap<String, GroupSummary> = BTreeMap::new();
    let mut nodes: BTreeMap<String, NodeSummary> = BTreeMap::new();
    let mut next = start + interval;
    while start.elapsed().as_secs_f64() < args.duration && !stop.load(Ordering::Relaxed) {
        if Instant::now() < next {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        next += interval;
        let t = start.elapsed().as_secs_f64();
        let Some(st) = status.lock().clone() else {
            continue;
        };
        let metrics = latest.lock().clone();
        let mut total_rss = 0;
        for g in &st.groups {
            let summary = groups.entry(g.name.clone()).or_default();
            summary.restarts = g.restarts;
            let Some(pid) = g.pid else { continue };
            let Some(rss) = rss_kib(pid) else { continue };
            total_rss += rss;
            summary.rss.push((t, pid, rss));
            let Some(m) = metrics.get(&g.name) else {
                continue;
            };
            for n in &m.nodes {
                let s = nodes.entry(n.id.clone()).or_default();
                // The first windows include start-up.
                if t >= args.interval * 2.0 {
                    s.p99s.push(n.step_p99_us);
                    s.hz.push(n.hz);
                    if let Some(timing) = &n.timing {
                        s.jitter_p99s.push(timing.jitter_p99_us);
                    }
                }
                s.dropped = n.dropped;
                s.errors = n.errors;
                s.failed |= n.state == NodeState::Failed;
                if let Some(f) = &mut csv {
                    writeln!(
                        f,
                        "{t:.1},{},{pid},{rss},{},{:.2},{:.1},{:.1},{},{}",
                        g.name,
                        n.id,
                        n.hz,
                        n.step_p99_us,
                        n.timing.as_ref().map_or(0.0, |t| t.jitter_p99_us),
                        n.dropped,
                        n.errors
                    )?;
                }
            }
        }
        let worst = metrics
            .values()
            .flat_map(|m| m.nodes.iter())
            .map(|n| n.step_p99_us)
            .fold(0.0, f64::max);
        let dropped: u64 = nodes.values().map(|n| n.dropped).sum();
        let restarts: u32 = groups.values().map(|g| g.restarts).sum();
        eprintln!(
            "[{t:>6.0} s] rss {:>7.1} MiB  worst p99 {worst:>8.0} µs  dropped {dropped}  restarts {restarts}",
            total_rss as f64 / 1024.0
        );
    }
    let ran = start.elapsed().as_secs_f64();
    let _ = c.request(Ctl::Stop, "");
    if let Some(d) = daemon {
        d.shutdown();
    }

    // Summary.
    println!(
        "\nsoak of {} for {ran:.0} s (warm-up {:.0} s)\n",
        args.graph.display(),
        args.warmup
    );
    let mut failures = Vec::new();
    println!(
        "{:<12} {:>10} {:>10} {:>10} {:>10} {:>9}",
        "GROUP", "BASE MiB", "END MiB", "MAX MiB", "GROWTH", "RESTARTS"
    );
    let window = 60.0f64.min((ran - args.warmup) / 2.0).max(args.interval);
    for (name, g) in &groups {
        let mib = |kib: u64| kib as f64 / 1024.0;
        let mut base: Vec<f64> = g
            .rss
            .iter()
            .filter(|(t, ..)| *t >= args.warmup && *t < args.warmup + window)
            .map(|(.., r)| mib(*r))
            .collect();
        let mut end: Vec<f64> = g
            .rss
            .iter()
            .filter(|(t, ..)| *t >= ran - window)
            .map(|(.., r)| mib(*r))
            .collect();
        let max = g.rss.iter().map(|(.., r)| mib(*r)).fold(0.0, f64::max);
        let (base, end) = (median(&mut base), median(&mut end));
        let growth = end - base;
        println!(
            "{name:<12} {base:>10.1} {end:>10.1} {max:>10.1} {growth:>+10.2} {:>9}",
            g.restarts
        );
        if base == 0.0 || end == 0.0 {
            failures.push(format!("group `{name}`: not enough memory samples"));
        } else if growth > args.max_rss_growth_mb {
            failures.push(format!(
                "group `{name}`: memory grew {growth:.2} MiB after warm-up (limit {} MiB)",
                args.max_rss_growth_mb
            ));
        }
        if g.restarts > 0 {
            failures.push(format!("group `{name}` restarted {} time(s)", g.restarts));
        }
    }
    println!(
        "\n{:<12} {:>8} {:>12} {:>12} {:>12} {:>9} {:>7}",
        "NODE", "HZ", "P99 (med)", "P99 (max)", "JITTER P99", "DROPPED", "ERRORS"
    );
    for (id, n) in &mut nodes {
        let p99_max = n.p99s.iter().copied().fold(0.0, f64::max);
        let jitter = if n.jitter_p99s.is_empty() {
            "-".to_owned()
        } else {
            format!("{:.0} µs", median(&mut n.jitter_p99s))
        };
        println!(
            "{id:<12} {:>8.1} {:>9.0} µs {p99_max:>9.0} µs {jitter:>12} {:>9} {:>7}",
            median(&mut n.hz),
            median(&mut n.p99s),
            n.dropped,
            n.errors
        );
        if n.failed {
            failures.push(format!("node `{id}` failed"));
        }
    }
    if failures.is_empty() {
        println!("\nPASS");
        Ok(())
    } else {
        for f in &failures {
            println!("FAIL: {f}");
        }
        bail!("soak failed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_own_rss() {
        let rss = rss_kib(std::process::id()).unwrap();
        assert!(rss > 100, "{rss} KiB");
        assert!(rss_kib(u32::MAX).is_none());
    }
}
