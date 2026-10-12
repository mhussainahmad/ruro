# Real-time scheduling

By default every node runs on its own thread under the normal Linux scheduler, which is fine
for perception and most control at tens to hundreds of hertz. For kHz control loops a process
group can be configured for real-time execution.

> **Status:** implemented and covered by tests that run without special privileges. Jitter
> figures depend on the kernel and the machine; measure yours (see below). Scheduling
> priority and memory locking need permissions that CI does not have, so their success path
> is only exercised on machines set up for it.

## Configuration

Real-time settings belong to a process group, in the graph's `groups` section:

```yaml
name: arm-control
groups:
  control:
    realtime:
      priority: 80          # SCHED_FIFO 1-99; also locks memory (mlockall)
      cpus: [3]             # pin the group's threads
      deterministic: true   # one thread, fixed order, fixed rate
      rate: 1000            # cycle rate; defaults to the fastest node rate
      deadlines_us: { pid: 200 }
nodes:
  target: { type: std/sine, group: control, rate: 1000 }
  pid:    { type: std/pid, group: control }
  arm:    { type: std/fake-arm, group: control, rate: 1000 }
wires:
  - target.value -> pid.setpoint
  - arm.position -> pid.measured
  - pid.command -> arm.velocity
```

| Field | Meaning |
|---|---|
| `priority` | Run the group's threads with `SCHED_FIFO` at this priority (1-99), and lock the process's memory with `mlockall`. |
| `cpus` | CPU affinity for the group's threads. Combine with isolated CPUs (`isolcpus`) for the lowest jitter. |
| `deterministic` | Instead of one thread per node, run all nodes of the group on one thread, in a fixed order each cycle. |
| `rate` | Cycle rate of a deterministic group in Hz. Defaults to the highest node rate in the group. |
| `deadlines_us` | Per-node step deadlines in microseconds. Every node named must be in the group. |

- **Pinning and priority** apply to every node thread of the group (and to the cycle thread
  of a deterministic group), not to the threads that receive messages from other processes.
  Without `CAP_SYS_NICE` (or an `rtprio` limit, see `ulimit -r`) `SCHED_FIFO` is refused;
  the group then runs with normal scheduling and logs a warning that says so. The same goes
  for `mlockall` and the `memlock` limit.
- **Deterministic groups** run all their nodes on one thread. Each cycle steps the nodes in
  wire order (producers first; feedback loops are cut in declaration order), so data
  produced in a cycle is consumed in the same cycle without queueing, and a
  sensor-controller-actuator chain completes within one tick. A node with a `rate` steps
  every *n* cycles, *n* being the cycle rate divided by the node rate, rounded (at least
  1); input-triggered nodes step in cycles where their inputs received data. A cycle that
  runs past the start of the next one counts as an overrun. A deterministic group needs a
  rate: its own or one of its nodes'.
- **Deadlines** work with or without `deterministic`. A step that takes longer is reported as
  an error (`deadline missed: step took X µs, deadline Y µs`) and counted.

The daemon checks the priority range, the cycle rate (0 to 100000 Hz) and that deadline
nodes belong to the group when the graph is deployed. `ruro check` does not.

## Measuring

`ruro status` shows the p99 timer wakeup jitter of timer-driven nodes (column `JIT P99`),
and deadline misses and overruns when there are any. The same numbers, plus p50 and maximum
jitter, are in `NodeMetrics.timing`.

PREEMPT_RT kernels give far better jitter than stock ones. Measure your machine with:

```sh
cargo bench -p ruro-host --bench jitter      # RURO_JITTER_SECS=60 for a longer run
```

## System requirements

- Permission to use real-time priorities and to lock memory. The system service sets
  `LimitRTPRIO=95` and `LimitMEMLOCK=infinity` (see [Deploying on robots](deployment.md));
  for other setups, configure `/etc/security/limits.d/` or run Docker with
  `--cap-add=SYS_NICE --ulimit rtprio=95 --ulimit memlock=-1`.
- A `PREEMPT_RT` kernel is recommended for hard latency bounds.
