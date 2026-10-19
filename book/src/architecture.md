# Architecture

ruro is a Cargo workspace of small crates:

```text
crates/
  ruro            facade crate for node authors: derives, prelude, testing harness
  ruro-msg        Message trait, schemas and standard types (Image, PointCloud, Pose, ...)
  ruro-macros     #[derive(Node)], #[derive(Message)], #[derive(Describe)]
  ruro-node       ports, queues, parameters, link-time node registry
  ruro-graph      graph YAML, composite flattening, node descriptors, validation
  ruro-transport  iceoryx2 shared-memory topics
  ruro-proto      control protocol over Zenoh (deploy, metrics, taps, params, logs), TLS
  ruro-host       executes one process group: threads, wiring, metrics, real-time setup
  ruro-daemon     per-machine supervisor and Zenoh router, peers, uploads, recording,
                  updates
  ruro-record     MCAP recording, replay and rosbag2 import
  ruro-schema     message schema export, compatibility checks, Python codegen
  ruro-pkg        `ruro new` and `ruro build`: node packages and generated hosts
  ruro-ros2       ROS 2 interop: CDR, RIHS01 hashes, rmw_zenoh topics, services
                  and actions, codegen, dynamic types, launch import
  ruro-tf         transform buffer and tf nodes
  ruro-urdf       URDF and MJCF parsing, forward kinematics
  ruro-gpu        CUDA driver loaded at run time, device buffers, IPC handles
  ruro-py         Python node API (pyo3), also the `ruro` Python package
  ruro-host-py    ruro-host with an embedded Python interpreter
  ruro-cli        `ruro`
  ruro-studio     `ruro-studio`, the desktop editor
nodes/
  ruro-nodes-std   standard nodes: synthetic sources, image processing, control
  ruro-nodes-hw    hardware drivers
  ruro-nodes-sim   simulation: MuJoCo, robot state
  ruro-nodes-ml    ONNX inference, YOLO decoding, policies, LeRobot recording
  ruro-nodes-gpu   GPU upload, download and preprocessing
  ruro-nodes-ros2  ROS 2 bridge nodes
```

## Processes

```text
 ruro-studio / ruro (clients)
        |
        |  Zenoh: deploy, status, metrics, logs, taps, parameters
        v
   ruro daemon  (one per machine, Zenoh router)
        |
        |  starts and supervises one process per group
        v
 ruro-host --group camera      ruro-host --group vision
   [camera]  ---- iceoryx2 shared memory ---->  [blur] -> [stats]
                                                 (in-process, by reference)
```

- **Clients** (the CLI and the studio) talk to a daemon over Zenoh. They never touch the hosts
  directly. Because the daemon is a Zenoh router, a client on another machine works exactly
  like a local one.
- **The daemon** validates a graph, writes it to its runtime directory and starts one host
  process per group. It restarts processes that exit according to their restart policy, and
  stops them when it exits itself (host processes are tied to the daemon with
  `PR_SET_PDEATHSIG`).
- **Hosts** build the nodes of their group from the link-time registry, connect wires
  in-process or through iceoryx2, and run one thread per node (one thread for a
  deterministic group, plus a receive thread per imported shared-memory topic). They publish metrics and logs,
  answer tap requests and apply parameter changes, all through the daemon's router.

## Data plane and control plane

On one machine, messages between nodes never go through Zenoh. Within a process, an output
holds subscribers' queues directly and sends an `Arc` of the message to each of them.
Between processes, an output that has remote readers publishes on an iceoryx2 topic; each
receiving process subscribes and feeds its local inputs. Between machines, and for replayed
messages and a simulated clock that comes from another machine, messages travel over Zenoh
(lz4-compressed when that helps).

Zenoh also carries the control plane: deployment, status and metrics, logs, parameter updates
and *taps*, the low-rate previews used by `ruro echo` and the studio's watch panel. A tap is
only serialised while a client is watching it.

## Node registry

`#[derive(Node)]` registers each node type at link time (with `inventory`). A host therefore
provides exactly the node types of the crates linked into it. `ruro-host describe` prints their
descriptors, which is how the daemon, the CLI and the studio learn which types exist and how
to validate graphs.
