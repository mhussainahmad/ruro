//! ODrive motor controllers over the ASCII protocol (UART or USB CDC).
//!
//! Commands are text lines, for example `p 0 1.5 0 0` (position setpoint in
//! turns), `v 0 2.0 0` (velocity in turns/s), `c 0 0.3` (torque in Nm) and
//! `f 0` (feedback, answered with `"<pos> <vel>"`). Lines may carry a checksum
//! suffix `*NN`, NN being the decimal XOR of every byte before the `*`.

use std::f64::consts::TAU;
use std::time::{Duration, Instant};

use ruro::prelude::*;

use crate::io::{SerialTransport, Transport, parse_list};

/// Axis states used by the driver.
pub mod axis_state {
    pub const IDLE: u8 = 1;
    pub const CLOSED_LOOP_CONTROL: u8 = 8;
}

/// XOR of all bytes, the ASCII protocol checksum.
pub fn checksum(line: &str) -> u8 {
    line.bytes().fold(0, |a, b| a ^ b)
}

/// Terminates `line`, appending ` *NN` when `with_checksum` is set.
pub fn frame(line: &str, with_checksum: bool) -> String {
    if with_checksum {
        format!("{line} *{}\n", checksum(&format!("{line} ")))
    } else {
        format!("{line}\n")
    }
}

/// Strips and verifies an optional `*NN` checksum from a received line.
pub fn unframe(line: &str) -> Result<&str, String> {
    let line = line.trim_end_matches(['\r', '\n']);
    match line.rsplit_once('*') {
        Some((body, cs)) => {
            let cs: u8 = cs
                .trim()
                .parse()
                .map_err(|_| format!("bad checksum field in `{line}`"))?;
            if checksum(body) != cs {
                return Err(format!("checksum mismatch in `{line}`"));
            }
            Ok(body.trim_end())
        }
        None => Ok(line),
    }
}

/// How joint commands are turned into ODrive setpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Position,
    Velocity,
    Torque,
}

impl std::str::FromStr for Mode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "position" => Ok(Mode::Position),
            "velocity" => Ok(Mode::Velocity),
            "torque" => Ok(Mode::Torque),
            other => Err(format!(
                "unknown mode `{other}` (expected position, velocity or torque)"
            )),
        }
    }
}

/// Setpoint line for one axis. Positions and velocities are given in radians
/// and converted to turns.
pub fn setpoint(mode: Mode, axis: u8, cmd: &JointCommand, i: usize) -> Option<String> {
    let get = |v: &Vec<f64>| v.get(i).copied();
    let turns = |r: f64| r / TAU;
    match mode {
        Mode::Position => {
            let pos = turns(get(&cmd.position)?);
            let vel_ff = get(&cmd.velocity).map_or(0.0, turns);
            let torque_ff = get(&cmd.effort).unwrap_or(0.0);
            Some(format!("p {axis} {pos:.6} {vel_ff:.6} {torque_ff:.6}"))
        }
        Mode::Velocity => {
            let vel = turns(get(&cmd.velocity)?);
            let torque_ff = get(&cmd.effort).unwrap_or(0.0);
            Some(format!("v {axis} {vel:.6} {torque_ff:.6}"))
        }
        Mode::Torque => Some(format!("c {axis} {:.6}", get(&cmd.effort)?)),
    }
}

/// Parses the reply to `f <axis>`: position (turns) and velocity (turns/s).
/// The command that stops an axis in `mode`: hold `turns` (position), zero
/// velocity, or zero torque.
pub fn hold_setpoint(mode: Mode, axis: u8, turns: f64) -> String {
    match mode {
        Mode::Position => format!("p {axis} {turns:.6} 0.000000 0.000000"),
        Mode::Velocity => format!("v {axis} 0.000000 0.000000"),
        Mode::Torque => format!("c {axis} 0.000000"),
    }
}

pub fn parse_feedback(line: &str) -> Result<(f64, f64), String> {
    let body = unframe(line)?;
    let mut it = body.split_whitespace().map(str::parse::<f64>);
    match (it.next(), it.next()) {
        (Some(Ok(p)), Some(Ok(v))) => Ok((p, v)),
        _ => Err(format!("unexpected feedback `{body}`")),
    }
}

/// Line-oriented client for the ASCII protocol.
pub struct Client<T> {
    pub transport: T,
    pub checksums: bool,
    pub timeout: Duration,
    rx: Vec<u8>,
}

impl<T: Transport> Client<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            checksums: false,
            timeout: Duration::from_millis(100),
            rx: Vec::new(),
        }
    }

    /// Sends a command that has no reply.
    pub fn command(&mut self, line: &str) -> std::io::Result<()> {
        self.transport.send(frame(line, self.checksums).as_bytes())
    }

    /// Sends a command and waits for its one-line reply.
    pub fn query(&mut self, line: &str) -> std::io::Result<String> {
        self.rx.clear();
        self.transport.clear_input()?;
        self.command(line)?;
        let deadline = Instant::now() + self.timeout;
        loop {
            if let Some(end) = self.rx.iter().position(|&b| b == b'\n') {
                let l: Vec<u8> = self.rx.drain(..=end).collect();
                let text = String::from_utf8_lossy(&l).trim().to_owned();
                if text.is_empty() {
                    continue;
                }
                return Ok(text);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("no reply to `{line}`"),
                ));
            }
            let mut buf = [0u8; 128];
            let n = self.transport.recv(&mut buf, left)?;
            self.rx.extend_from_slice(&buf[..n]);
        }
    }

    pub fn feedback(&mut self, axis: u8) -> Result<(f64, f64)> {
        let reply = self.query(&format!("f {axis}"))?;
        parse_feedback(&reply).map_err(|e| anyhow!(e))
    }

    pub fn set_state(&mut self, axis: u8, state: u8) -> std::io::Result<()> {
        self.command(&format!("w axis{axis}.requested_state {state}"))
    }
}

/// ODrive (firmware 0.5/0.6) over the ASCII protocol. Each joint is one axis.
/// Joint units are radians and radians/second; effort is torque in Nm.
///
/// E-stop: the axes stay in closed-loop control and are brought to rest. In
/// velocity mode the velocity setpoint is set to zero; in position mode the
/// present positions are read once and held; in torque mode the torque
/// setpoint is set to zero (the joint goes limp). The safe setpoint is sent
/// again every step and incoming commands are ignored while stopped. Putting
/// the axes in `idle` instead would also stop driving them, but lets the
/// motors coast and loaded joints drop, so it is not done here; wire your own
/// handling if your machine needs it. After the release the axes keep the
/// safe setpoint until a command arrives after the release (commands sent
/// during the stop are discarded).
#[derive(Node)]
#[node(kind = "hw/odrive", rate = 100)]
pub struct ODrive {
    /// Setpoints; which fields are used depends on `mode`.
    #[input]
    command: Input<JointCommand>,
    /// Encoder position (rad) and velocity (rad/s) per axis.
    #[output]
    state: Output<JointState>,
    /// Serial device (UART adapter or the ODrive's USB serial port).
    #[param(default = "/dev/ttyACM0")]
    device: String,
    #[param(default = 115200, min = 9600, max = 1000000)]
    baud: u32,
    /// Comma separated axis numbers, in joint order.
    #[param(default = "0")]
    axes: String,
    /// `position`, `velocity` or `torque`.
    #[param(default = "position")]
    mode: String,
    /// Put the axes in closed-loop control on start and idle them on stop.
    #[param(default = true)]
    manage_state: bool,
    /// Append `*NN` checksums to every command.
    #[param(default = false)]
    checksums: bool,
    client: Option<Client<Box<dyn Transport>>>,
    axis_ids: Vec<u8>,
    /// Safe setpoints latched when the e-stop engaged, one per axis.
    hold: Option<Vec<String>>,
    /// Stopped by the e-stop; commands are discarded until one arrives after the release.
    held: bool,
}

impl ODrive {
    /// Uses an existing transport instead of opening `device` (tests, custom links).
    pub fn attach(&mut self, transport: Box<dyn Transport>) {
        self.client = Some(Client::new(transport));
    }

    /// Sends the safe setpoints, working them out first if nothing is latched yet.
    fn hold(&mut self) -> Result {
        let mode: Mode = self.mode.parse().map_err(|e: String| anyhow!(e))?;
        let c = self.client.as_mut().context("not connected")?;
        if self.hold.is_none() {
            let mut lines = Vec::with_capacity(self.axis_ids.len());
            for &a in &self.axis_ids {
                let turns = if mode == Mode::Position {
                    c.feedback(a)?.0
                } else {
                    0.0
                };
                lines.push(hold_setpoint(mode, a, turns));
            }
            self.hold = Some(lines);
        }
        for line in self.hold.as_ref().expect("latched above") {
            c.command(line)?;
        }
        Ok(())
    }
}

impl Node for ODrive {
    fn start(&mut self, _ctx: &mut Ctx) -> Result {
        self.axis_ids = parse_list(&self.axes).map_err(|e| anyhow!("axes: {e}"))?;
        ensure!(!self.axis_ids.is_empty(), "axes: no axes configured");
        self.mode.parse::<Mode>().map_err(|e| anyhow!(e))?;
        if self.client.is_none() {
            let t = SerialTransport::open(&self.device, self.baud)
                .with_context(|| format!("opening {}", self.device))?;
            self.client = Some(Client::new(Box::new(t)));
        }
        let c = self.client.as_mut().unwrap();
        c.checksums = self.checksums;
        let vbus = c.query("r vbus_voltage").context("ODrive did not answer")?;
        info!("ODrive bus voltage {} V", unframe(&vbus).unwrap_or(&vbus));
        if self.manage_state {
            for &a in &self.axis_ids {
                c.set_state(a, axis_state::CLOSED_LOOP_CONTROL)?;
            }
        }
        Ok(())
    }

    fn step(&mut self, ctx: &mut Ctx) -> Result {
        if ctx.estopped() || self.held {
            // Commands queued while stopped are stale.
            while self.command.recv().is_some() {}
            if ctx.estopped() {
                self.held = true;
                self.hold()?;
            } else {
                self.held = false;
                self.hold = None;
            }
        }
        let mode: Mode = self.mode.parse().map_err(|e: String| anyhow!(e))?;
        let c = self.client.as_mut().context("not connected")?;
        if let Some(cmd) = self.command.latest() {
            for (i, &a) in self.axis_ids.iter().enumerate() {
                if let Some(line) = setpoint(mode, a, &cmd, i) {
                    c.command(&line)?;
                }
            }
        }
        let mut state = JointState::default();
        for &a in &self.axis_ids {
            let (pos, vel) = c.feedback(a)?;
            state.position.push(pos * TAU);
            state.velocity.push(vel * TAU);
        }
        self.state.send(state)
    }

    fn on_estop(&mut self, _ctx: &mut Ctx) {
        self.held = true;
        self.hold = None;
        if self.client.is_some()
            && let Err(e) = self.hold()
        {
            // The next step tries again.
            warn!("e-stop: could not stop the axes: {e:#}");
        }
    }

    fn stop(&mut self, _ctx: &mut Ctx) {
        if let (true, Some(c)) = (self.manage_state, self.client.as_mut()) {
            for &a in &self.axis_ids {
                if let Err(e) = c.set_state(a, axis_state::IDLE) {
                    warn!("could not idle axis {a}: {e}");
                }
            }
        }
    }
}

/// A minimal ODrive stand-in that integrates `p`/`v` setpoints, for tests.
#[derive(Default)]
pub struct VirtualODrive {
    /// Per axis: (position turns, velocity turns/s, closed loop).
    pub axes: [(f64, f64, bool); 2],
    pending: Vec<u8>,
}

impl VirtualODrive {
    pub fn handle(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.pending.extend_from_slice(bytes);
        let mut out = String::new();
        while let Some(end) = self.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line).to_string();
            let with_cs = line.contains('*');
            let Ok(body) = unframe(&line) else { continue };
            let w: Vec<&str> = body.split_whitespace().collect();
            let axis = |i: usize| {
                w.get(i)
                    .and_then(|a| a.parse::<usize>().ok())
                    .filter(|a| *a < 2)
            };
            let num = |i: usize| w.get(i).and_then(|a| a.parse::<f64>().ok());
            let reply = match w.as_slice() {
                ["r", "vbus_voltage"] => Some("24.000000".to_owned()),
                ["f", ..] => {
                    axis(1).map(|a| format!("{:.6} {:.6}", self.axes[a].0, self.axes[a].1))
                }
                ["p", ..] => {
                    if let (Some(a), Some(p)) = (axis(1), num(2)) {
                        self.axes[a].0 = p;
                    }
                    None
                }
                ["v", ..] => {
                    if let (Some(a), Some(v)) = (axis(1), num(2)) {
                        self.axes[a].1 = v;
                    }
                    None
                }
                ["w", prop, v] => {
                    if let Some(a) = prop
                        .strip_prefix("axis")
                        .and_then(|r| r.strip_suffix(".requested_state"))
                        .and_then(|a| a.parse::<usize>().ok())
                        .filter(|a| *a < 2)
                    {
                        self.axes[a].2 = *v == "8";
                    }
                    None
                }
                _ => Some("unknown command".to_owned()),
            };
            if let Some(r) = reply {
                if with_cs {
                    let cs = checksum(&format!("{r} "));
                    out.push_str(&format!("{r} *{cs}\r\n"));
                } else {
                    out.push_str(&r);
                    out.push_str("\r\n");
                }
            }
        }
        out.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::MockTransport;

    #[test]
    fn checksums_follow_the_documented_example() {
        // From the ODrive ASCII protocol documentation: `r vbus_voltage *93`.
        assert_eq!(checksum("r vbus_voltage "), 93);
        assert_eq!(frame("r vbus_voltage", true), "r vbus_voltage *93\n");
        assert_eq!(unframe("r vbus_voltage *93\r\n").unwrap(), "r vbus_voltage");
        assert!(unframe("r vbus_voltage *94").is_err());
        assert_eq!(unframe("1.5 0.25\r\n").unwrap(), "1.5 0.25");
    }

    #[test]
    fn setpoints_convert_radians_to_turns() {
        let cmd = JointCommand {
            position: vec![TAU, -TAU / 2.0],
            velocity: vec![TAU / 4.0],
            effort: vec![0.5],
            ..Default::default()
        };
        assert_eq!(
            setpoint(Mode::Position, 0, &cmd, 0).unwrap(),
            "p 0 1.000000 0.250000 0.500000"
        );
        assert_eq!(
            setpoint(Mode::Position, 1, &cmd, 1).unwrap(),
            "p 1 -0.500000 0.000000 0.000000"
        );
        assert_eq!(
            setpoint(Mode::Velocity, 0, &cmd, 0).unwrap(),
            "v 0 0.250000 0.500000"
        );
        assert_eq!(setpoint(Mode::Torque, 0, &cmd, 0).unwrap(), "c 0 0.500000");
        assert_eq!(setpoint(Mode::Velocity, 1, &cmd, 1), None);
        assert!("speed".parse::<Mode>().is_err());
    }

    #[test]
    fn feedback_parses_with_and_without_checksum() {
        assert_eq!(parse_feedback("1.25 -0.5\r\n").unwrap(), (1.25, -0.5));
        let cs = checksum("1.25 -0.5 ");
        assert_eq!(
            parse_feedback(&format!("1.25 -0.5 *{cs}")).unwrap(),
            (1.25, -0.5)
        );
        assert!(parse_feedback("invalid").is_err());
    }

    #[test]
    fn client_talks_to_a_virtual_odrive() {
        for checksums in [false, true] {
            let mut dev = VirtualODrive::default();
            let mut c = Client::new(MockTransport::with_responder(move |b| dev.handle(b)));
            c.checksums = checksums;
            assert_eq!(
                unframe(&c.query("r vbus_voltage").unwrap()).unwrap(),
                "24.000000"
            );
            c.command("p 0 2.5 0 0").unwrap();
            c.command("v 1 -1.0 0").unwrap();
            assert_eq!(c.feedback(0).unwrap(), (2.5, 0.0));
            assert_eq!(c.feedback(1).unwrap(), (0.0, -1.0));
        }
    }
}
