//! Drivers running inside a real executor, talking to simulated devices on
//! the other end of a pseudo-terminal.

use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use ruro::prelude::*;
use ruro_graph::Graph;
use ruro_host::{Executor, Remote};
use ruro_nodes_hw::{dynamixel, feetech, odrive};
use serialport::{SerialPort, TTYPort};

/// Publishes a fixed joint command.
#[derive(Node)]
#[node(kind = "test/joint-source", rate = 50)]
struct JointSource {
    #[output]
    command: Output<JointCommand>,
    #[param(default = "")]
    positions: String,
}

impl Node for JointSource {
    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        let position = self
            .positions
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|s| s.trim().parse().unwrap())
            .collect();
        self.command.send(JointCommand {
            position,
            ..Default::default()
        })
    }
}

static STATES: Mutex<Vec<(String, JointState)>> = Mutex::new(Vec::new());

/// Records joint states, tagged with its node id.
#[derive(Node)]
#[node(kind = "test/joint-probe")]
struct JointProbe {
    #[input]
    state: Input<JointState>,
}

impl Node for JointProbe {
    fn step(&mut self, ctx: &mut Ctx) -> Result {
        while let Some(s) = self.state.recv() {
            STATES.lock().push((ctx.id().to_owned(), s.msg.clone()));
        }
        Ok(())
    }
}

static BYTES: Mutex<Vec<u8>> = Mutex::new(Vec::new());

#[derive(Node)]
#[node(kind = "test/bytes-probe")]
struct BytesProbe {
    #[input]
    data: Input<Bytes>,
}

impl Node for BytesProbe {
    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        while let Some(b) = self.data.recv() {
            BYTES.lock().extend_from_slice(&b.0);
        }
        Ok(())
    }
}

#[derive(Node)]
#[node(kind = "test/bytes-source", rate = 20)]
struct BytesSource {
    #[output]
    data: Output<Bytes>,
}

impl Node for BytesSource {
    fn step(&mut self, ctx: &mut Ctx) -> Result {
        if ctx.steps() == 0 {
            self.data.send(Bytes(b"ping\xff\x00".to_vec()))?;
        }
        Ok(())
    }
}

fn wait_until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

/// A device simulator on the master side of a pty. `frame` returns the
/// length of the first complete packet in the buffer (or None), `handle`
/// produces the reply.
struct Device {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    _slave: TTYPort,
    path: String,
}

impl Device {
    fn spawn(
        mut frame: impl FnMut(&mut Vec<u8>) -> Option<Vec<u8>> + Send + 'static,
        mut handle: impl FnMut(&[u8]) -> Vec<u8> + Send + 'static,
    ) -> Self {
        let (mut master, slave) = TTYPort::pair().expect("openpty");
        let path = slave.name().expect("pty name");
        master.set_timeout(Duration::from_millis(10)).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 512];
            while !s.load(Ordering::Relaxed) {
                if let Ok(n) = master.read(&mut chunk) {
                    buf.extend_from_slice(&chunk[..n]);
                }
                while let Some(packet) = frame(&mut buf) {
                    let reply = handle(&packet);
                    if !reply.is_empty() {
                        master.write_all(&reply).unwrap();
                    }
                }
            }
        });
        Self {
            stop,
            thread: Some(thread),
            _slave: slave,
            path,
        }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn feetech_frame(buf: &mut Vec<u8>) -> Option<Vec<u8>> {
    let start = buf.windows(2).position(|w| w == [0xFF, 0xFF])?;
    buf.drain(..start);
    let total = 4 + *buf.get(3)? as usize;
    (buf.len() >= total).then(|| buf.drain(..total).collect())
}

fn dynamixel_frame(buf: &mut Vec<u8>) -> Option<Vec<u8>> {
    let start = buf.windows(4).position(|w| w == [0xFF, 0xFF, 0xFD, 0x00])?;
    buf.drain(..start);
    let len = u16::from_le_bytes([*buf.get(5)?, *buf.get(6)?]) as usize;
    (buf.len() >= 7 + len).then(|| buf.drain(..7 + len).collect())
}

fn line_frame(buf: &mut Vec<u8>) -> Option<Vec<u8>> {
    let end = buf.iter().position(|&b| b == b'\n')?;
    Some(buf.drain(..=end).collect())
}

fn latest_state(id: &str) -> Option<JointState> {
    STATES
        .lock()
        .iter()
        .rev()
        .find(|(n, _)| n == id)
        .map(|(_, s)| s.clone())
}

#[test]
fn hardware_nodes_are_registered() {
    ruro_nodes_hw::link();
    let reg = ruro::node::registry();
    for kind in [
        "hw/v4l2-camera",
        "hw/mjpeg-http-camera",
        "hw/serial",
        "hw/can",
        "hw/gpio-in",
        "hw/gpio-out",
        "hw/i2c",
        "hw/gamepad",
        "hw/dynamixel",
        "hw/odrive",
        "hw/witmotion-imu",
        "hw/rplidar",
        "hw/livox",
        "hw/so100-arm",
        "hw/joint-jog",
    ] {
        let d = reg.get(kind).unwrap_or_else(|| panic!("{kind} missing"));
        assert!(!d.doc.is_empty(), "{kind} has no documentation");
    }
    let can = reg.get("hw/can").unwrap();
    assert_eq!(can.inputs[0].ty, "hw/CanFrame");
}

#[test]
fn drivers_over_pseudo_terminals() {
    so100_arm_follows_commands();
    dynamixel_follows_commands();
    odrive_reports_feedback();
    serial_passes_bytes_both_ways();
}

fn so100_arm_follows_commands() {
    let mut servos = feetech::VirtualServos::new(&[1, 2, 3, 4, 5, 6]);
    servos.echo = true; // like the Waveshare/Feetech USB adapters
    let dev = Device::spawn(feetech_frame, move |p| servos.handle(p));
    let graph = Graph::from_yaml(&format!(
        r#"
name: so100
nodes:
  cmd: {{ type: test/joint-source, params: {{ positions: "0.1,-0.2,0.3,0,0,0.25" }} }}
  arm:
    type: hw/so100-arm
    rate: 50
    params:
      device: {dev}
      offsets: "2048,2048,2048,2048,2048,2000"
      directions: "1,-1,1,1,1,1"
  probe: {{ type: test/joint-probe }}
wires: [cmd.command -> arm.command, arm.state -> probe.state]
"#,
        dev = dev.path
    ))
    .unwrap();
    let exec = Executor::start(&graph, "main", Remote::Disabled).unwrap();
    let reached = wait_until(Duration::from_secs(5), || {
        latest_state("probe").is_some_and(|s| {
            let goal = [0.1, -0.2, 0.3, 0.0, 0.0, 0.25];
            s.position.len() == 6
                && s.position
                    .iter()
                    .zip(goal)
                    .all(|(p, g)| (p - g).abs() < 0.002)
        })
    });
    let m = exec.metrics();
    let arm = m.nodes.iter().find(|n| n.id == "arm").unwrap();
    assert!(
        reached,
        "arm did not reach the goal: {:?}, node error {:?}",
        latest_state("probe"),
        arm.last_error
    );
    exec.stop();
}

fn dynamixel_follows_commands() {
    let mut servos = dynamixel::VirtualServos::new(&[1, 2]);
    let dev = Device::spawn(dynamixel_frame, move |p| servos.handle(p));
    let graph = Graph::from_yaml(&format!(
        r#"
name: dxl
nodes:
  cmd: {{ type: test/joint-source, params: {{ positions: "1.0,-0.5" }} }}
  dxl: {{ type: hw/dynamixel, params: {{ device: {dev}, ids: "1,2", baud: 1000000 }} }}
  dxl_probe: {{ type: test/joint-probe }}
wires: [cmd.command -> dxl.command, dxl.state -> dxl_probe.state]
"#,
        dev = dev.path
    ))
    .unwrap();
    let exec = Executor::start(&graph, "main", Remote::Disabled).unwrap();
    let reached = wait_until(Duration::from_secs(5), || {
        latest_state("dxl_probe").is_some_and(|s| {
            s.position.len() == 2
                && (s.position[0] - 1.0).abs() < 0.002
                && (s.position[1] + 0.5).abs() < 0.002
        })
    });
    let m = exec.metrics();
    let n = m.nodes.iter().find(|n| n.id == "dxl").unwrap();
    assert!(
        reached,
        "{:?} {:?}",
        latest_state("dxl_probe"),
        n.last_error
    );
    exec.stop();
}

fn odrive_reports_feedback() {
    let mut drive = odrive::VirtualODrive::default();
    let dev = Device::spawn(line_frame, move |l| drive.handle(l));
    let graph = Graph::from_yaml(&format!(
        r#"
name: odrv
nodes:
  cmd: {{ type: test/joint-source, params: {{ positions: "3.14159265,-1.0" }} }}
  odrv: {{ type: hw/odrive, params: {{ device: {dev}, axes: "0,1", checksums: true }} }}
  odrv_probe: {{ type: test/joint-probe }}
wires: [cmd.command -> odrv.command, odrv.state -> odrv_probe.state]
"#,
        dev = dev.path
    ))
    .unwrap();
    let exec = Executor::start(&graph, "main", Remote::Disabled).unwrap();
    let ok = wait_until(Duration::from_secs(5), || {
        latest_state("odrv_probe").is_some_and(|s| {
            s.position.len() == 2
                && (s.position[0] - std::f64::consts::PI).abs() < 1e-4
                && (s.position[1] + 1.0).abs() < 1e-4
        })
    });
    let m = exec.metrics();
    let n = m.nodes.iter().find(|n| n.id == "odrv").unwrap();
    assert!(ok, "{:?} {:?}", latest_state("odrv_probe"), n.last_error);
    exec.stop();
}

fn serial_passes_bytes_both_ways() {
    // Echo device: replies to every received chunk with the bytes reversed.
    let dev = Device::spawn(
        |buf| (!buf.is_empty()).then(|| std::mem::take(buf)),
        |b| b.iter().rev().copied().collect(),
    );
    let graph = Graph::from_yaml(&format!(
        r#"
name: serial
nodes:
  src: {{ type: test/bytes-source }}
  port: {{ type: hw/serial, params: {{ device: {dev} }} }}
  sink: {{ type: test/bytes-probe }}
wires: [src.data -> port.tx, port.rx -> sink.data]
"#,
        dev = dev.path
    ))
    .unwrap();
    let exec = Executor::start(&graph, "main", Remote::Disabled).unwrap();
    assert!(
        wait_until(Duration::from_secs(5), || BYTES.lock().len() >= 6),
        "received {:?}",
        BYTES.lock()
    );
    assert_eq!(&BYTES.lock()[..6], b"\x00\xffgnip");
    exec.stop();
}
