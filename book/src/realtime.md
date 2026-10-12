# Real-time scheduling

> **Status:** in development. This page describes the planned interface and will be verified
> before release.

By default every node runs on its own thread under the normal Linux scheduler, which is fine
for perception and most control at tens to hundreds of hertz. For kHz control loops a process
group can be configured for real-time execution.

## Configuration

Real-time settings belong to a process group, in the graph's `groups` section:

```yaml
groups:
  control:
    realtime:
      priority: 80         # SCHED_FIFO priority, 1-99
      cpus: [2, 3]         # pin the group's threads to these CPUs
      deterministic: true  # one thread, fixed node order each cycle
nodes:
  imu:   { type: my_robot/imu,   group: control, rate: 1000 }
  ctrl:  { type: my_robot/ctrl,  group: control }
  motor: { type: my_robot/motor, group: control }
```

| Field | Meaning |
|---|---|
| `priority` | Run the group's threads with `SCHED_FIFO` at this priority. |
| `cpus` | CPU affinity for the group's threads. Combine with isolated CPUs (`isolcpus`) for the lowest jitter. |
| `deterministic` | Instead of one thread per node, run all nodes of the group on one thread, in a fixed order each cycle, so a sensor-controller-actuator chain completes within one tick. |

The fields are already part of the graph format (see the
[graph file reference](../reference/graph-yaml.md#groups)); the scheduling itself is being
implemented in the host.

## System requirements

- Permission to use real-time priorities and to lock memory: the system service sets
  `LimitRTPRIO=95` and `LimitMEMLOCK=infinity`; for other setups, configure
  `/etc/security/limits.d/` or run Docker with
  `--cap-add=SYS_NICE --ulimit rtprio=95 --ulimit memlock=-1`.
- A `PREEMPT_RT` kernel is recommended for hard latency bounds.
