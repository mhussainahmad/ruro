//! Feetech STS/SMS serial bus servos (STS3215 and friends) and the
//! `hw/so100-arm` node for the LeRobot SO-100/SO-101 arm.
//!
//! The protocol is close to Dynamixel Protocol 1.0:
//!
//! ```text
//! instruction: FF FF | ID | LEN | INST | PARAMS... | CHK
//! status:      FF FF | ID | LEN | ERR  | PARAMS... | CHK
//! ```
//!
//! `LEN` is the number of parameters plus two and the checksum is the bitwise
//! complement of the byte sum of ID, LEN, INST/ERR and PARAMS. STS servos store multi-byte values little-endian; signed
//! registers use sign-magnitude with a register-specific sign bit.

use std::f64::consts::TAU;
use std::time::{Duration, Instant};

use ruro::prelude::*;

use crate::io::{SerialTransport, Transport, parse_list};

pub const BROADCAST_ID: u8 = 0xFE;

pub mod inst {
    pub const PING: u8 = 0x01;
    pub const READ: u8 = 0x02;
    pub const WRITE: u8 = 0x03;
    pub const REG_WRITE: u8 = 0x04;
    pub const ACTION: u8 = 0x05;
    pub const SYNC_READ: u8 = 0x82;
    pub const SYNC_WRITE: u8 = 0x83;
}

/// STS/SMS series memory table (as used by LeRobot for the STS3215).
pub mod sts {
    pub const MIN_POSITION_LIMIT: u8 = 9;
    pub const MAX_POSITION_LIMIT: u8 = 11;
    pub const HOMING_OFFSET: u8 = 31;
    pub const OPERATING_MODE: u8 = 33;
    pub const TORQUE_ENABLE: u8 = 40;
    pub const ACCELERATION: u8 = 41;
    pub const GOAL_POSITION: u8 = 42;
    pub const GOAL_TIME: u8 = 44;
    pub const GOAL_VELOCITY: u8 = 46;
    pub const LOCK: u8 = 55;
    /// Present position (2), velocity (2), load (2), voltage (1) and temperature (1).
    pub const PRESENT_POSITION: u8 = 56;
    pub const PRESENT_VELOCITY: u8 = 58;
    pub const PRESENT_LOAD: u8 = 60;
    pub const PRESENT_VOLTAGE: u8 = 62;
    pub const PRESENT_TEMPERATURE: u8 = 63;
    /// Ticks per revolution.
    pub const RESOLUTION: f64 = 4096.0;
    pub const MODEL_STS3215: u16 = 777;
}

/// Sum-complement checksum over everything after the `FF FF` header.
pub fn checksum(body: &[u8]) -> u8 {
    !body.iter().fold(0u8, |a, &b| a.wrapping_add(b))
}

/// Builds an instruction packet.
pub fn instruction(id: u8, instruction: u8, params: &[u8]) -> Vec<u8> {
    assert!(params.len() <= 253, "packet too long");
    let mut p = Vec::with_capacity(params.len() + 6);
    p.extend_from_slice(&[0xFF, 0xFF, id, params.len() as u8 + 2, instruction]);
    p.extend_from_slice(params);
    p.push(checksum(&p[2..]));
    p
}

pub fn ping(id: u8) -> Vec<u8> {
    instruction(id, inst::PING, &[])
}

pub fn read(id: u8, address: u8, len: u8) -> Vec<u8> {
    instruction(id, inst::READ, &[address, len])
}

pub fn write(id: u8, address: u8, data: &[u8]) -> Vec<u8> {
    let mut params = vec![address];
    params.extend_from_slice(data);
    instruction(id, inst::WRITE, &params)
}

pub fn sync_read(address: u8, len: u8, ids: &[u8]) -> Vec<u8> {
    let mut params = vec![address, len];
    params.extend_from_slice(ids);
    instruction(BROADCAST_ID, inst::SYNC_READ, &params)
}

pub fn sync_write(address: u8, len: u8, items: &[(u8, &[u8])]) -> Vec<u8> {
    let mut params = vec![address, len];
    for (id, data) in items {
        debug_assert_eq!(data.len(), len as usize);
        params.push(*id);
        params.extend_from_slice(data);
    }
    instruction(BROADCAST_ID, inst::SYNC_WRITE, &params)
}

/// Decodes a sign-magnitude value whose sign is bit `sign_bit`.
pub fn decode_signed(raw: u16, sign_bit: u32) -> i32 {
    let magnitude = (raw & ((1u16 << sign_bit) - 1)) as i32;
    if raw & (1 << sign_bit) != 0 {
        -magnitude
    } else {
        magnitude
    }
}

/// Encodes a sign-magnitude value whose sign is bit `sign_bit`.
pub fn encode_signed(v: i32, sign_bit: u32) -> u16 {
    let max = (1i32 << sign_bit) - 1;
    let magnitude = v.unsigned_abs().min(max as u32) as u16;
    if v < 0 {
        magnitude | (1 << sign_bit)
    } else {
        magnitude
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub id: u8,
    pub error: u8,
    pub params: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    Incomplete,
    Corrupt { skip: usize, reason: &'static str },
}

/// Parses one status packet from the start of `buf`.
pub fn parse_status(buf: &[u8]) -> Result<(Status, usize), ParseError> {
    let Some(start) = buf.windows(2).position(|w| w == [0xFF, 0xFF]) else {
        return if buf.len() > 1 {
            Err(ParseError::Corrupt {
                skip: buf.len() - 1,
                reason: "no header",
            })
        } else {
            Err(ParseError::Incomplete)
        };
    };
    if start > 0 {
        return Err(ParseError::Corrupt {
            skip: start,
            reason: "garbage before header",
        });
    }
    if buf.len() < 4 {
        return Err(ParseError::Incomplete);
    }
    if buf[2] == 0xFF {
        // Three 0xFF in a row: the first one is noise.
        return Err(ParseError::Corrupt {
            skip: 1,
            reason: "extra 0xFF",
        });
    }
    let len = buf[3] as usize;
    if len < 2 {
        return Err(ParseError::Corrupt {
            skip: 2,
            reason: "length too short",
        });
    }
    let total = 4 + len;
    if buf.len() < total {
        return Err(ParseError::Incomplete);
    }
    if checksum(&buf[2..total - 1]) != buf[total - 1] {
        return Err(ParseError::Corrupt {
            skip: 2,
            reason: "checksum mismatch",
        });
    }
    Ok((
        Status {
            id: buf[2],
            error: buf[4],
            params: buf[5..total - 1].to_vec(),
        },
        total,
    ))
}

#[derive(Debug)]
pub enum BusError {
    Io(std::io::Error),
    Timeout { id: u8 },
    UnexpectedId { expected: u8, got: u8 },
}

impl std::fmt::Display for BusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BusError::Io(e) => write!(f, "bus I/O error: {e}"),
            BusError::Timeout { id } => write!(f, "servo {id} did not answer"),
            BusError::UnexpectedId { expected, got } => {
                write!(f, "expected a reply from servo {expected}, got {got}")
            }
        }
    }
}

impl std::error::Error for BusError {}

impl From<std::io::Error> for BusError {
    fn from(e: std::io::Error) -> Self {
        BusError::Io(e)
    }
}

/// A Feetech bus. Half-duplex adapters echo what they send; echoed
/// instruction packets are recognised and skipped.
pub struct Bus<T> {
    pub transport: T,
    pub timeout: Duration,
    rx: Vec<u8>,
    sent: Vec<u8>,
}

impl<T: Transport> Bus<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            timeout: Duration::from_millis(50),
            rx: Vec::new(),
            sent: Vec::new(),
        }
    }

    fn send(&mut self, packet: Vec<u8>) -> Result<(), BusError> {
        self.rx.clear();
        self.transport.clear_input()?;
        self.transport.send(&packet)?;
        self.sent = packet;
        Ok(())
    }

    fn fill(&mut self, deadline: Instant, id: u8) -> Result<(), BusError> {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(BusError::Timeout { id });
        }
        let mut buf = [0u8; 256];
        let n = self.transport.recv(&mut buf, left)?;
        self.rx.extend_from_slice(&buf[..n]);
        Ok(())
    }

    fn receive(&mut self, id: u8) -> Result<Status, BusError> {
        let deadline = Instant::now() + self.timeout;
        loop {
            if !self.sent.is_empty() {
                let n = self.sent.len().min(self.rx.len());
                if self.rx[..n] != self.sent[..n] {
                    // No echo on this adapter.
                    self.sent.clear();
                } else if n == self.sent.len() {
                    self.rx.drain(..n);
                    self.sent.clear();
                    continue;
                } else {
                    self.fill(deadline, id)?;
                    continue;
                }
            }
            match parse_status(&self.rx) {
                Ok((status, used)) => {
                    self.rx.drain(..used);
                    if status.id != id {
                        return Err(BusError::UnexpectedId {
                            expected: id,
                            got: status.id,
                        });
                    }
                    return Ok(status);
                }
                Err(ParseError::Incomplete) => self.fill(deadline, id)?,
                Err(ParseError::Corrupt { skip, .. }) => {
                    self.rx.drain(..skip);
                }
            }
        }
    }

    /// Pings a servo and returns its error byte.
    pub fn ping(&mut self, id: u8) -> Result<u8, BusError> {
        self.send(ping(id))?;
        Ok(self.receive(id)?.error)
    }

    pub fn read(&mut self, id: u8, address: u8, len: u8) -> Result<Vec<u8>, BusError> {
        self.send(read(id, address, len))?;
        let s = self.receive(id)?;
        if s.params.len() != len as usize {
            return Err(BusError::Timeout { id });
        }
        Ok(s.params)
    }

    pub fn write(&mut self, id: u8, address: u8, data: &[u8]) -> Result<(), BusError> {
        self.send(write(id, address, data))?;
        self.receive(id).map(|_| ())
    }

    pub fn sync_read(
        &mut self,
        address: u8,
        len: u8,
        ids: &[u8],
    ) -> Result<Vec<Vec<u8>>, BusError> {
        self.send(sync_read(address, len, ids))?;
        ids.iter()
            .map(|&id| {
                let s = self.receive(id)?;
                if s.params.len() != len as usize {
                    return Err(BusError::Timeout { id });
                }
                Ok(s.params)
            })
            .collect()
    }

    pub fn sync_write(
        &mut self,
        address: u8,
        len: u8,
        items: &[(u8, &[u8])],
    ) -> Result<(), BusError> {
        // No replies; a half-duplex echo is discarded by the next `send`.
        self.transport.send(&sync_write(address, len, items))?;
        Ok(())
    }
}

/// A software model of STS3215 servos, for tests and dry runs.
pub struct VirtualServos {
    pub servos: Vec<(u8, [u8; 128])>,
    /// Echo every packet back first, like a half-duplex adapter.
    pub echo: bool,
}

impl VirtualServos {
    pub fn new(ids: &[u8]) -> Self {
        let servos = ids
            .iter()
            .map(|&id| {
                let mut t = [0u8; 128];
                t[3..5].copy_from_slice(&sts::MODEL_STS3215.to_le_bytes());
                t[5] = id;
                t[sts::PRESENT_POSITION as usize..][..2].copy_from_slice(&2048u16.to_le_bytes());
                t[sts::PRESENT_VOLTAGE as usize] = 120;
                t[sts::PRESENT_TEMPERATURE as usize] = 30;
                (id, t)
            })
            .collect();
        Self {
            servos,
            echo: false,
        }
    }

    fn status(id: u8, params: &[u8]) -> Vec<u8> {
        instruction(id, 0, params)
    }

    fn table(&mut self, id: u8) -> Option<&mut [u8; 128]> {
        self.servos
            .iter_mut()
            .find(|(s, _)| *s == id)
            .map(|(_, t)| t)
    }

    pub fn handle(&mut self, packet: &[u8]) -> Vec<u8> {
        let mut out = if self.echo {
            packet.to_vec()
        } else {
            Vec::new()
        };
        let Ok((p, _)) = parse_status(packet) else {
            return out;
        };
        // `parse_status` puts the instruction in `error` and the rest in `params`.
        let (id, instr, params) = (p.id, p.error, p.params);
        match instr {
            inst::PING => {
                if self.table(id).is_some() {
                    out.extend(Self::status(id, &[]));
                }
            }
            inst::READ => {
                let (a, n) = (params[0] as usize, params[1] as usize);
                if let Some(t) = self.table(id) {
                    let data = t[a..a + n].to_vec();
                    out.extend(Self::status(id, &data));
                }
            }
            inst::WRITE => {
                if let Some(t) = self.table(id) {
                    let a = params[0] as usize;
                    t[a..a + params.len() - 1].copy_from_slice(&params[1..]);
                    Self::settle(t);
                    out.extend(Self::status(id, &[]));
                }
            }
            inst::SYNC_WRITE => {
                let (a, n) = (params[0] as usize, params[1] as usize);
                for chunk in params[2..].chunks_exact(n + 1) {
                    if let Some(t) = self.table(chunk[0]) {
                        t[a..a + n].copy_from_slice(&chunk[1..]);
                        Self::settle(t);
                    }
                }
            }
            inst::SYNC_READ => {
                let (a, n) = (params[0] as usize, params[1] as usize);
                for &rid in &params[2..] {
                    if let Some(t) = self.table(rid) {
                        let data = t[a..a + n].to_vec();
                        out.extend(Self::status(rid, &data));
                    }
                }
            }
            _ => {}
        }
        out
    }

    /// Ideal servo: with torque on, the present position jumps to the goal.
    fn settle(t: &mut [u8; 128]) {
        if t[sts::TORQUE_ENABLE as usize] == 1 {
            let g = sts::GOAL_POSITION as usize;
            let goal = [t[g], t[g + 1]];
            t[sts::PRESENT_POSITION as usize..][..2].copy_from_slice(&goal);
        }
    }
}

/// Converts raw ticks to radians given a calibration offset (ticks at zero) and direction.
pub fn ticks_to_rad(ticks: i32, offset: i32, direction: f64) -> f64 {
    direction * (ticks - offset) as f64 * TAU / sts::RESOLUTION
}

/// Inverse of [`ticks_to_rad`], clamped to the encoder range.
pub fn rad_to_ticks(rad: f64, offset: i32, direction: f64) -> u16 {
    let t = (direction * rad * sts::RESOLUTION / TAU).round() as i32 + offset;
    t.clamp(0, 4095) as u16
}

/// LeRobot SO-100/SO-101 arm: six Feetech STS3215 servos on one bus
/// (shoulder_pan, shoulder_lift, elbow_flex, wrist_flex, wrist_roll, gripper).
/// Joint angles are in radians relative to the calibration offsets.
#[derive(Node)]
#[node(kind = "hw/so100-arm", rate = 50)]
pub struct So100Arm {
    /// Goal joint positions in radians (six values).
    #[input]
    command: Input<JointCommand>,
    /// Present position (rad), velocity (rad/s) and load (fraction of max torque, -1..1).
    #[output]
    state: Output<JointState>,
    /// Serial device of the bus adapter.
    #[param(default = "/dev/ttyACM0")]
    device: String,
    /// Bus baud rate (STS3215 default: 1000000).
    #[param(default = 1000000, min = 9600, max = 1000000)]
    baud: u32,
    /// Comma separated servo ids in joint order.
    #[param(default = "1,2,3,4,5,6")]
    ids: String,
    /// Calibration: raw tick reading at each joint's zero position.
    #[param(default = "2048,2048,2048,2048,2048,2048")]
    offsets: String,
    /// Calibration: +1 or -1 per joint to flip its direction.
    #[param(default = "1,1,1,1,1,1")]
    directions: String,
    /// Largest position change accepted per command, in radians (0 disables the check).
    #[param(default = 0.5, min = 0, max = 6.3)]
    max_step: f64,
    /// Enable torque on start and disable it on stop.
    #[param(default = true)]
    manage_torque: bool,
    bus: Option<Bus<Box<dyn Transport>>>,
    servo_ids: Vec<u8>,
    zero: Vec<i32>,
    dir: Vec<f64>,
    last_position: Vec<f64>,
}

impl So100Arm {
    fn configure(&mut self) -> Result {
        self.servo_ids = parse_list(&self.ids).map_err(|e| anyhow!("ids: {e}"))?;
        self.zero = parse_list(&self.offsets).map_err(|e| anyhow!("offsets: {e}"))?;
        self.dir = parse_list(&self.directions).map_err(|e| anyhow!("directions: {e}"))?;
        let n = self.servo_ids.len();
        ensure!(n > 0, "ids: no servos configured");
        ensure!(
            self.zero.len() == n && self.dir.len() == n,
            "need one offset and one direction per servo ({n})"
        );
        ensure!(
            self.dir.iter().all(|d| *d == 1.0 || *d == -1.0),
            "directions must be 1 or -1"
        );
        Ok(())
    }
}

impl Node for So100Arm {
    fn start(&mut self, _ctx: &mut Ctx) -> Result {
        self.configure()?;
        if self.bus.is_none() {
            let t = SerialTransport::open(&self.device, self.baud)
                .with_context(|| format!("opening {}", self.device))?;
            self.bus = Some(Bus::new(Box::new(t)));
        }
        let bus = self.bus.as_mut().unwrap();
        for &id in &self.servo_ids {
            bus.ping(id)
                .with_context(|| format!("pinging servo {id}"))?;
        }
        if self.manage_torque {
            // Hold the current pose: goal = present before enabling torque.
            let present = bus.sync_read(sts::PRESENT_POSITION, 2, &self.servo_ids)?;
            let items: Vec<(u8, &[u8])> = self
                .servo_ids
                .iter()
                .zip(&present)
                .map(|(&id, p)| (id, p.as_slice()))
                .collect();
            bus.sync_write(sts::GOAL_POSITION, 2, &items)?;
            for &id in &self.servo_ids {
                bus.write(id, sts::TORQUE_ENABLE, &[1])?;
            }
        }
        Ok(())
    }

    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        let bus = self.bus.as_mut().context("bus not open")?;
        let raw = bus.sync_read(sts::PRESENT_POSITION, 6, &self.servo_ids)?;
        let mut state = JointState::default();
        for (i, r) in raw.iter().enumerate() {
            let pos = decode_signed(u16::from_le_bytes([r[0], r[1]]), 15);
            let vel = decode_signed(u16::from_le_bytes([r[2], r[3]]), 15);
            let load = decode_signed(u16::from_le_bytes([r[4], r[5]]), 10);
            state
                .position
                .push(ticks_to_rad(pos, self.zero[i], self.dir[i]));
            state
                .velocity
                .push(self.dir[i] * vel as f64 * TAU / sts::RESOLUTION);
            state.effort.push(self.dir[i] * load as f64 / 1000.0);
        }
        self.last_position.clone_from(&state.position);

        if let Some(cmd) = self.command.latest() {
            let n = self.servo_ids.len();
            ensure!(
                cmd.position.len() == n,
                "command has {} positions for {n} joints",
                cmd.position.len()
            );
            if self.max_step > 0.0 {
                for (j, (goal, now)) in cmd.position.iter().zip(&self.last_position).enumerate() {
                    ensure!(
                        (goal - now).abs() <= self.max_step,
                        "joint {j}: command {goal:.3} rad is more than max_step from {now:.3} rad"
                    );
                }
            }
            let data: Vec<[u8; 2]> = cmd
                .position
                .iter()
                .enumerate()
                .map(|(i, &p)| rad_to_ticks(p, self.zero[i], self.dir[i]).to_le_bytes())
                .collect();
            let items: Vec<(u8, &[u8])> = self
                .servo_ids
                .iter()
                .zip(&data)
                .map(|(&id, d)| (id, d.as_slice()))
                .collect();
            bus.sync_write(sts::GOAL_POSITION, 2, &items)?;
        }
        self.state.send(state)
    }

    fn stop(&mut self, _ctx: &mut Ctx) {
        if let (true, Some(bus)) = (self.manage_torque, self.bus.as_mut()) {
            for &id in &self.servo_ids {
                if let Err(e) = bus.write(id, sts::TORQUE_ENABLE, &[0]) {
                    warn!("could not disable torque on servo {id}: {e}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::MockTransport;

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace()
            .map(|b| u8::from_str_radix(b, 16).unwrap())
            .collect()
    }

    // Reference packets captured from Feetech's scservo_sdk (tools/servo_fixtures.py).
    #[test]
    fn packets_match_vendor_sdk() {
        assert_eq!(ping(1), hex("ff ff 01 02 01 fb"));
        assert_eq!(read(1, 56, 2), hex("ff ff 01 04 02 38 02 be"));
        assert_eq!(
            write(1, 42, &2048u16.to_le_bytes()),
            hex("ff ff 01 05 03 2a 00 08 c4")
        );
        assert_eq!(write(1, 40, &[1]), hex("ff ff 01 04 03 28 01 ce"));
        let goals: Vec<[u8; 2]> = [2048u16, 1024, 3072, 0, 4095, 100]
            .iter()
            .map(|g| g.to_le_bytes())
            .collect();
        let items: Vec<(u8, &[u8])> = goals
            .iter()
            .enumerate()
            .map(|(i, g)| (i as u8 + 1, g.as_slice()))
            .collect();
        assert_eq!(
            sync_write(42, 2, &items),
            hex("ff ff fe 16 83 2a 02 01 00 08 02 00 04 03 00 0c 04 00 00 05 ff 0f 06 64 00 9d")
        );
        assert_eq!(
            sync_read(56, 2, &[1, 2, 3, 4, 5, 6]),
            hex("ff ff fe 0a 82 38 02 01 02 03 04 05 06 26")
        );
    }

    #[test]
    fn sign_magnitude_matches_sdk() {
        assert_eq!(encode_signed(-100, 15), 0x8064);
        assert_eq!(decode_signed(0x8064, 15), -100);
        assert_eq!(decode_signed(0x0064, 15), 100);
        // Present load uses bit 10.
        assert_eq!(decode_signed(0x0400 | 500, 10), -500);
        assert_eq!(encode_signed(5000, 10), 1023);
    }

    #[test]
    fn status_parsing_skips_noise_and_rejects_bad_checksums() {
        let reply = VirtualServos::status(3, &[0x00, 0x08]);
        let mut buf = vec![0x12, 0xFF];
        buf.extend_from_slice(&reply);
        assert!(matches!(
            parse_status(&buf),
            Err(ParseError::Corrupt { skip: 1, .. })
        ));
        // FF FF FF ...: first FF is noise.
        assert!(matches!(
            parse_status(&buf[1..]),
            Err(ParseError::Corrupt { skip: 1, .. })
        ));
        let (s, used) = parse_status(&buf[2..]).unwrap();
        assert_eq!((s.id, s.error, used), (3, 0, reply.len()));
        assert_eq!(s.params, [0x00, 0x08]);
        let mut bad = reply.clone();
        bad[5] ^= 0x40;
        assert!(matches!(
            parse_status(&bad),
            Err(ParseError::Corrupt {
                reason: "checksum mismatch",
                ..
            })
        ));
        assert_eq!(parse_status(&reply[..4]), Err(ParseError::Incomplete));
    }

    #[test]
    fn bus_works_through_a_half_duplex_echo() {
        for echo in [false, true] {
            let mut servos = VirtualServos::new(&[1, 2]);
            servos.echo = echo;
            let mut bus = Bus::new(MockTransport::with_responder(move |p| servos.handle(p)));
            assert_eq!(bus.ping(2).unwrap(), 0);
            let model = bus.read(1, 3, 2).unwrap();
            assert_eq!(u16::from_le_bytes([model[0], model[1]]), 777);
            bus.write(1, sts::TORQUE_ENABLE, &[1]).unwrap();
            bus.write(1, sts::GOAL_POSITION, &1000u16.to_le_bytes())
                .unwrap();
            let r = bus.sync_read(sts::PRESENT_POSITION, 2, &[1, 2]).unwrap();
            assert_eq!(
                r,
                [
                    1000u16.to_le_bytes().to_vec(),
                    2048u16.to_le_bytes().to_vec()
                ],
                "echo={echo}"
            );
            assert!(matches!(bus.ping(9), Err(BusError::Timeout { id: 9 })));
        }
    }

    #[test]
    fn calibration_round_trips() {
        assert_eq!(ticks_to_rad(2048, 2048, 1.0), 0.0);
        assert!((ticks_to_rad(3072, 2048, 1.0) - std::f64::consts::FRAC_PI_2).abs() < 1e-12);
        assert!((ticks_to_rad(3072, 2048, -1.0) + std::f64::consts::FRAC_PI_2).abs() < 1e-12);
        for (off, dir) in [(2048, 1.0), (1000, -1.0), (3500, 1.0)] {
            for t in [0u16, 500, 2047, 4095] {
                let rad = ticks_to_rad(t as i32, off, dir);
                assert_eq!(rad_to_ticks(rad, off, dir), t);
            }
        }
        assert_eq!(rad_to_ticks(100.0, 2048, 1.0), 4095);
        assert_eq!(rad_to_ticks(-100.0, 2048, 1.0), 0);
    }
}
