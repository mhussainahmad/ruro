//! Wakeup jitter of a 1 kHz control loop on this machine.
//!
//! Runs a small sine -> gain -> pid loop at 1 kHz for a few seconds in three
//! configurations: one thread per node, one deterministic thread, and a
//! deterministic thread pinned to a CPU with SCHED_FIFO requested (which
//! falls back to normal scheduling, with a warning, without privileges).
//! Prints the lateness of timer wakeups (p50, p99, max) and cycle overruns.
//!
//! Run with `cargo bench -p ruro-host --bench jitter`. Set
//! `RURO_JITTER_SECS` to change the duration (default 10 s per run).
//! The numbers depend heavily on the kernel (PREEMPT_RT or not) and on
//! what else the machine is doing.

use std::time::Duration;

use ruro_graph::Graph;
use ruro_host::{Executor, Remote};

fn run(label: &str, realtime: &str, secs: f64) {
    let yaml = format!(
        r#"
name: jitter
groups:
  main: {realtime}
nodes:
  target: {{ type: std/sine, rate: 1000, params: {{ frequency: 1.0 }} }}
  plant: {{ type: std/sine, rate: 1000, params: {{ frequency: 0.5 }} }}
  pid: {{ type: std/pid }}
  scale: {{ type: std/gain, params: {{ k: 0.5 }} }}
wires: [target.value -> pid.setpoint, plant.value -> pid.measured, pid.command -> scale.input]
"#
    );
    let graph = Graph::from_yaml(&yaml).unwrap();
    let exec = Executor::start(&graph, "main", Remote::Disabled).unwrap();
    // Discard start-up, then measure one long window.
    std::thread::sleep(Duration::from_millis(500));
    exec.metrics();
    std::thread::sleep(Duration::from_secs_f64(secs));
    let m = exec.metrics();
    exec.stop();
    let target = m.nodes.iter().find(|n| n.id == "target").unwrap();
    let t = target.timing.clone().unwrap_or_default();
    println!(
        "{label:<34} {:>8.1} Hz  jitter p50 {:>6.1} µs  p99 {:>7.1} µs  max {:>8.1} µs  overruns {}",
        target.hz, t.jitter_p50_us, t.jitter_p99_us, t.jitter_max_us, t.overruns
    );
}

fn main() {
    // Shows the warning when real-time scheduling is not permitted.
    tracing_subscriber::fmt()
        .with_env_filter("warn")
        .with_target(false)
        .init();
    // `cargo bench` passes harness flags such as --bench; they are ignored.
    let secs: f64 = std::env::var("RURO_JITTER_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10.0);
    let cpu = ruro_host::rt::current_affinity()
        .ok()
        .and_then(|cpus| cpus.last().copied())
        .unwrap_or(0);
    println!("1 kHz loop, {secs} s per run (kernel: {})", kernel());
    run("thread per node", "{}", secs);
    run(
        "deterministic",
        "{ realtime: { deterministic: true, rate: 1000 } }",
        secs,
    );
    run(
        &format!("deterministic, cpu {cpu}, fifo 80"),
        &format!(
            "{{ realtime: {{ deterministic: true, rate: 1000, cpus: [{cpu}], priority: 80 }} }}"
        ),
        secs,
    );
}

fn kernel() -> String {
    std::fs::read_to_string("/proc/sys/kernel/version")
        .map(|v| v.trim().to_owned())
        .unwrap_or_else(|_| "unknown".into())
}
