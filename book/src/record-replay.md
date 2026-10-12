# Record and replay

> **Status:** in development. This page describes the planned interface and will be verified
> before release.

Recording captures the messages flowing through a running graph so that you can replay them
later: to debug a run after the fact, to develop perception without the robot, or to compare
two versions of a node on the same input.

## Planned design

- **Recording** writes every wire of the running graph to an [MCAP](https://mcap.dev) file,
  started and stopped on the daemon.
- **Replay** feeds a recording back into a running graph, replacing the outputs of the nodes
  that recorded them, so downstream nodes see the recorded data as if it were live.
- Replay can be paused, resumed, sought to a position and run faster or slower than real time.
- The daemon reports whether it is recording and the replay position, duration, speed and
  pause state, which the studio shows on a timeline.

The control operations exist in the daemon protocol (`ruro_proto::Ctl::Record` and
`Ctl::Playback`). The studio already drives them: a record button and recording indicator, and
a replay timeline with play/pause, seek and speed (see [Studio](../studio.md#recording-and-replay)).
The daemon side and the CLI commands will be documented here when they are released.
