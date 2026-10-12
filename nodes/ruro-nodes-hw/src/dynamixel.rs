//! Robotis Dynamixel Protocol 2.0 and the `hw/dynamixel` node.
//!
//! Packet layout (instruction and status):
//!
//! ```text
//! FF FF FD 00 | ID | LEN_L LEN_H | INST | PARAMS... | CRC_L CRC_H
//! ```
//!
//! `LEN` counts the instruction byte, the (byte-stuffed) parameters and the
//! CRC. Status packets carry `INST = 0x55` followed by an error byte. Any
//! `FF FF FD` sequence inside the payload is escaped as `FF FF FD FD`.

use std::f64::consts::TAU;
use std::time::Duration;

use ruro::prelude::*;

use crate::io::{SerialTransport, Transport, parse_list};

pub const BROADCAST_ID: u8 = 0xFE;

pub mod inst {
    pub const PING: u8 = 0x01;
    pub const READ: u8 = 0x02;
    pub const WRITE: u8 = 0x03;
    pub const REG_WRITE: u8 = 0x04;
    pub const ACTION: u8 = 0x05;
    pub const FACTORY_RESET: u8 = 0x06;
    pub const REBOOT: u8 = 0x08;
    pub const STATUS: u8 = 0x55;
    pub const SYNC_READ: u8 = 0x82;
    pub const SYNC_WRITE: u8 = 0x83;
    pub const BULK_READ: u8 = 0x92;
    pub const BULK_WRITE: u8 = 0x93;
}

/// Control table of X-series servos (XL430, XM430, XM540, ...).
pub mod x_series {
    pub const OPERATING_MODE: u16 = 11;
    pub const TORQUE_ENABLE: u16 = 64;
    pub const GOAL_CURRENT: u16 = 102;
    pub const GOAL_VELOCITY: u16 = 104;
    pub const GOAL_POSITION: u16 = 116;
    /// Present current (2 bytes), velocity (4) and position (4) are contiguous.
    pub const PRESENT_CURRENT: u16 = 126;
    pub const PRESENT_VELOCITY: u16 = 128;
    pub const PRESENT_POSITION: u16 = 132;
    /// Ticks per revolution.
    pub const RESOLUTION: f64 = 4096.0;
    /// Ticks at the zero position.
    pub const CENTER: i32 = 2048;
    /// Velocity unit: 0.229 rev/min.
    pub const VELOCITY_UNIT_RPM: f64 = 0.229;
}

/// CRC-16 as specified for Protocol 2.0 (polynomial 0x8005, no reflection, init 0).
pub fn crc16(data: &[u8]) -> u16 {
    static TABLE: [u16; 256] = {
        let mut t = [0u16; 256];
        let mut i = 0;
        while i < 256 {
            let mut c = (i as u16) << 8;
            let mut b = 0;
            while b < 8 {
                c = if c & 0x8000 != 0 {
                    (c << 1) ^ 0x8005
                } else {
                    c << 1
                };
                b += 1;
            }
            t[i] = c;
            i += 1;
        }
        t
    };
    data.iter().fold(0u16, |crc, &b| {
        (crc << 8) ^ TABLE[(((crc >> 8) as u8) ^ b) as usize]
    })
}

/// Escapes `FF FF FD` in `payload` (instruction and parameters).
fn stuff(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 2);
    for &b in payload {
        out.push(b);
        let n = out.len();
        if n >= 3 && out[n - 3..] == [0xFF, 0xFF, 0xFD] {
            out.push(0xFD);
        }
    }
    out
}

/// Reverses [`stuff`].
fn unstuff(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len());
    let mut i = 0;
    while i < payload.len() {
        out.push(payload[i]);
        if i + 1 < payload.len()
            && payload[i] == 0xFD
            && payload[i + 1] == 0xFD
            && out.len() >= 3
            && out[out.len() - 3..] == [0xFF, 0xFF, 0xFD]
        {
            i += 1;
        }
        i += 1;
    }
    out
}

/// Builds an instruction packet.
pub fn instruction(id: u8, instruction: u8, params: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(params.len() + 1);
    payload.push(instruction);
    payload.extend_from_slice(params);
    let body = stuff(&payload);
    let len = (body.len() + 2) as u16;
    let mut p = Vec::with_capacity(body.len() + 9);
    p.extend_from_slice(&[0xFF, 0xFF, 0xFD, 0x00, id]);
    p.extend_from_slice(&len.to_le_bytes());
    p.extend_from_slice(&body);
    let crc = crc16(&p);
    p.extend_from_slice(&crc.to_le_bytes());
    p
}

pub fn ping(id: u8) -> Vec<u8> {
    instruction(id, inst::PING, &[])
}

pub fn read(id: u8, address: u16, len: u16) -> Vec<u8> {
    let [a0, a1] = address.to_le_bytes();
    let [l0, l1] = len.to_le_bytes();
    instruction(id, inst::READ, &[a0, a1, l0, l1])
}

pub fn write(id: u8, address: u16, data: &[u8]) -> Vec<u8> {
    let mut params = address.to_le_bytes().to_vec();
    params.extend_from_slice(data);
    instruction(id, inst::WRITE, &params)
}

pub fn sync_read(address: u16, len: u16, ids: &[u8]) -> Vec<u8> {
    let mut params = address.to_le_bytes().to_vec();
    params.extend_from_slice(&len.to_le_bytes());
    params.extend_from_slice(ids);
    instruction(BROADCAST_ID, inst::SYNC_READ, &params)
}

/// `items` pairs a servo id with exactly `len` bytes of data.
pub fn sync_write(address: u16, len: u16, items: &[(u8, &[u8])]) -> Vec<u8> {
    let mut params = address.to_le_bytes().to_vec();
    params.extend_from_slice(&len.to_le_bytes());
    for (id, data) in items {
        debug_assert_eq!(data.len(), len as usize);
        params.push(*id);
        params.extend_from_slice(data);
    }
    instruction(BROADCAST_ID, inst::SYNC_WRITE, &params)
}

/// A decoded status packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub id: u8,
    /// Error field; bit 7 is the hardware alert flag, bits 0..6 the error number.
    pub error: u8,
    pub params: Vec<u8>,
}

impl Status {
    /// The error number (0 = no error), ignoring the hardware alert bit.
    pub fn error_number(&self) -> u8 {
        self.error & 0x7F
    }

    pub fn hardware_alert(&self) -> bool {
        self.error & 0x80 != 0
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    /// More bytes are needed.
    Incomplete,
    /// The packet at the start of the buffer has a bad CRC or is malformed;
    /// skip `skip` bytes and try again.
    Corrupt { skip: usize, reason: &'static str },
}

/// Parses one status packet from the start of `buf` (after resynchronising to a
/// header). Returns the packet and the number of bytes consumed.
pub fn parse_status(buf: &[u8]) -> Result<(Status, usize), ParseError> {
    let Some(start) = buf.windows(4).position(|w| w == [0xFF, 0xFF, 0xFD, 0x00]) else {
        // Keep a possible partial header at the end.
        let keep = buf.len().min(3);
        return if buf.len() > 3 {
            Err(ParseError::Corrupt {
                skip: buf.len() - keep,
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
    if buf.len() < 7 {
        return Err(ParseError::Incomplete);
    }
    let len = u16::from_le_bytes([buf[5], buf[6]]) as usize;
    if len < 4 {
        return Err(ParseError::Corrupt {
            skip: 1,
            reason: "length too short for a status packet",
        });
    }
    let total = 7 + len;
    if buf.len() < total {
        return Err(ParseError::Incomplete);
    }
    let crc = u16::from_le_bytes([buf[total - 2], buf[total - 1]]);
    if crc16(&buf[..total - 2]) != crc {
        return Err(ParseError::Corrupt {
            skip: 1,
            reason: "CRC mismatch",
        });
    }
    if buf[7] != inst::STATUS {
        return Err(ParseError::Corrupt {
            skip: total,
            reason: "not a status packet",
        });
    }
    let payload = unstuff(&buf[8..total - 2]);
    Ok((
        Status {
            id: buf[4],
            error: payload[0],
            params: payload[1..].to_vec(),
        },
        total,
    ))
}

#[derive(Debug)]
pub enum BusError {
    Io(std::io::Error),
    Timeout {
        id: u8,
    },
    /// A servo answered with an error number.
    Servo {
        id: u8,
        error: u8,
    },
    UnexpectedId {
        expected: u8,
        got: u8,
    },
}

impl std::fmt::Display for BusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BusError::Io(e) => write!(f, "bus I/O error: {e}"),
            BusError::Timeout { id } => write!(f, "servo {id} did not answer"),
            BusError::Servo { id, error } => write!(f, "servo {id} reported error {error:#04x}"),
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

/// A Protocol 2.0 bus on top of any transport.
pub struct Bus<T> {
    pub transport: T,
    pub timeout: Duration,
    rx: Vec<u8>,
}

impl<T: Transport> Bus<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            timeout: Duration::from_millis(50),
            rx: Vec::new(),
        }
    }

    fn receive(&mut self, id: u8) -> Result<Status, BusError> {
        let deadline = std::time::Instant::now() + self.timeout;
        loop {
            match parse_status(&self.rx) {
                Ok((status, used)) => {
                    self.rx.drain(..used);
                    if status.id != id {
                        return Err(BusError::UnexpectedId {
                            expected: id,
                            got: status.id,
                        });
                    }
                    if status.error_number() != 0 {
                        return Err(BusError::Servo {
                            id,
                            error: status.error,
                        });
                    }
                    return Ok(status);
                }
                Err(ParseError::Corrupt { skip, .. }) => {
                    self.rx.drain(..skip);
                }
                Err(ParseError::Incomplete) => {
                    let left = deadline.saturating_duration_since(std::time::Instant::now());
                    if left.is_zero() {
                        return Err(BusError::Timeout { id });
                    }
                    let mut buf = [0u8; 256];
                    let n = self.transport.recv(&mut buf, left)?;
                    self.rx.extend_from_slice(&buf[..n]);
                }
            }
        }
    }

    fn transact(&mut self, packet: &[u8], id: u8) -> Result<Status, BusError> {
        self.rx.clear();
        self.transport.clear_input()?;
        self.transport.send(packet)?;
        self.receive(id)
    }

    /// Pings a servo; returns `(model number, firmware version)`.
    pub fn ping(&mut self, id: u8) -> Result<(u16, u8), BusError> {
        let s = self.transact(&ping(id), id)?;
        if s.params.len() < 3 {
            return Err(BusError::Timeout { id });
        }
        Ok((u16::from_le_bytes([s.params[0], s.params[1]]), s.params[2]))
    }

    pub fn read(&mut self, id: u8, address: u16, len: u16) -> Result<Vec<u8>, BusError> {
        Ok(self.transact(&read(id, address, len), id)?.params)
    }

    pub fn write(&mut self, id: u8, address: u16, data: &[u8]) -> Result<(), BusError> {
        self.transact(&write(id, address, data), id).map(|_| ())
    }

    /// Reads `len` bytes at `address` from each servo, in order.
    pub fn sync_read(
        &mut self,
        address: u16,
        len: u16,
        ids: &[u8],
    ) -> Result<Vec<Vec<u8>>, BusError> {
        self.rx.clear();
        self.transport.clear_input()?;
        self.transport.send(&sync_read(address, len, ids))?;
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

    /// Writes without waiting for replies (sync write has none).
    pub fn sync_write(
        &mut self,
        address: u16,
        len: u16,
        items: &[(u8, &[u8])],
    ) -> Result<(), BusError> {
        self.transport.send(&sync_write(address, len, items))?;
        Ok(())
    }
}

/// Converts X-series position ticks to radians (2048 = 0).
pub fn ticks_to_rad(ticks: i32) -> f64 {
    (ticks - x_series::CENTER) as f64 * TAU / x_series::RESOLUTION
}

/// Converts radians to X-series position ticks (2048 = 0).
pub fn rad_to_ticks(rad: f64) -> i32 {
    (rad * x_series::RESOLUTION / TAU).round() as i32 + x_series::CENTER
}

/// Dynamixel X-series servos over Protocol 2.0 (XL430, XM430, XM540, ...).
/// Commands goal positions with one sync write and reads back position,
/// velocity and current with one sync read per step. Servos must be in
/// position control mode (the factory default).
#[derive(Node)]
#[node(kind = "hw/dynamixel", rate = 100)]
pub struct Dynamixel {
    /// Goal positions in radians, one per servo id (0 rad = tick 2048).
    #[input]
    command: Input<JointCommand>,
    /// Present position (rad), velocity (rad/s) and current (A) per servo.
    #[output]
    state: Output<JointState>,
    /// Serial device of the U2D2 or similar adapter.
    #[param(default = "/dev/ttyUSB0")]
    device: String,
    /// Baud rate configured on the servos.
    #[param(default = 57600, min = 9600, max = 4500000)]
    baud: u32,
    /// Comma separated servo ids, in joint order.
    #[param(default = "1")]
    ids: String,
    /// Current unit of the servo model in milliamps (2.69 for XM430/XM540).
    #[param(default = 2.69, min = 0, max = 100)]
    current_unit_ma: f64,
    /// Enable torque on start and disable it on stop.
    #[param(default = true)]
    manage_torque: bool,
    bus: Option<Bus<Box<dyn Transport>>>,
    servo_ids: Vec<u8>,
}

impl Dynamixel {
    /// Uses an existing transport instead of opening `device` (tests, custom links).
    pub fn attach(&mut self, transport: Box<dyn Transport>) {
        self.bus = Some(Bus::new(transport));
    }
}

impl Node for Dynamixel {
    fn start(&mut self, _ctx: &mut Ctx) -> Result {
        self.servo_ids = parse_list(&self.ids).map_err(|e| anyhow!("ids: {e}"))?;
        ensure!(!self.servo_ids.is_empty(), "ids: no servos configured");
        if self.bus.is_none() {
            let t = SerialTransport::open(&self.device, self.baud)
                .with_context(|| format!("opening {}", self.device))?;
            self.attach(Box::new(t));
        }
        let bus = self.bus.as_mut().unwrap();
        for &id in &self.servo_ids {
            let (model, fw) = bus
                .ping(id)
                .with_context(|| format!("pinging servo {id}"))?;
            info!("dynamixel {id}: model {model}, firmware {fw}");
            if self.manage_torque {
                bus.write(id, x_series::TORQUE_ENABLE, &[1])?;
            }
        }
        Ok(())
    }

    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        let bus = self.bus.as_mut().context("bus not open")?;
        if let Some(cmd) = self.command.latest() {
            ensure!(
                cmd.position.len() == self.servo_ids.len(),
                "command has {} positions for {} servos",
                cmd.position.len(),
                self.servo_ids.len()
            );
            let data: Vec<[u8; 4]> = cmd
                .position
                .iter()
                .map(|&p| rad_to_ticks(p).clamp(0, 4095).to_le_bytes())
                .collect();
            let items: Vec<(u8, &[u8])> = self
                .servo_ids
                .iter()
                .zip(&data)
                .map(|(&id, d)| (id, d.as_slice()))
                .collect();
            bus.sync_write(x_series::GOAL_POSITION, 4, &items)?;
        }
        let replies = bus.sync_read(x_series::PRESENT_CURRENT, 10, &self.servo_ids)?;
        let mut state = JointState::default();
        for r in replies {
            let current = i16::from_le_bytes([r[0], r[1]]);
            let velocity = i32::from_le_bytes([r[2], r[3], r[4], r[5]]);
            let position = i32::from_le_bytes([r[6], r[7], r[8], r[9]]);
            state.position.push(ticks_to_rad(position));
            state
                .velocity
                .push(velocity as f64 * x_series::VELOCITY_UNIT_RPM * TAU / 60.0);
            state
                .effort
                .push(current as f64 * self.current_unit_ma / 1000.0);
        }
        self.state.send(state)
    }

    fn stop(&mut self, _ctx: &mut Ctx) {
        if let (true, Some(bus)) = (self.manage_torque, self.bus.as_mut()) {
            for &id in &self.servo_ids {
                if let Err(e) = bus.write(id, x_series::TORQUE_ENABLE, &[0]) {
                    warn!("could not disable torque on servo {id}: {e}");
                }
            }
        }
    }
}

/// A software model of X-series servos answering on a bus. Useful for tests
/// and for running graphs without hardware.
pub struct VirtualServos {
    /// (id, control table) pairs.
    pub servos: Vec<(u8, Vec<u8>)>,
}

impl VirtualServos {
    pub fn new(ids: &[u8]) -> Self {
        let servos = ids
            .iter()
            .map(|&id| {
                let mut table = vec![0u8; 256];
                table[0..2].copy_from_slice(&1020u16.to_le_bytes()); // XM430-W350
                table[x_series::PRESENT_POSITION as usize..][..4]
                    .copy_from_slice(&2048i32.to_le_bytes());
                (id, table)
            })
            .collect();
        Self { servos }
    }

    /// A status packet without error; status packets share the instruction framing.
    pub fn status(id: u8, params: &[u8]) -> Vec<u8> {
        let mut payload = vec![0];
        payload.extend_from_slice(params);
        instruction(id, inst::STATUS, &payload)
    }

    /// Handles one instruction packet and returns the status bytes to send back.
    pub fn handle(&mut self, packet: &[u8]) -> Vec<u8> {
        if packet.len() < 10
            || crc16(&packet[..packet.len() - 2])
                != u16::from_le_bytes([packet[packet.len() - 2], packet[packet.len() - 1]])
        {
            return Vec::new();
        }
        let id = packet[4];
        let payload = unstuff(&packet[7..packet.len() - 2]);
        let (instr, p) = (payload[0], &payload[1..]);
        let addr = |p: &[u8]| u16::from_le_bytes([p[0], p[1]]) as usize;
        let mut out = Vec::new();
        match instr {
            inst::PING => {
                for (sid, t) in &self.servos {
                    if id == *sid || id == BROADCAST_ID {
                        out.extend(Self::status(*sid, &[t[0], t[1], 52]));
                    }
                }
            }
            inst::READ => {
                let (a, n) = (addr(p), addr(&p[2..]));
                if let Some((sid, t)) = self.servos.iter().find(|(s, _)| *s == id) {
                    out.extend(Self::status(*sid, &t[a..a + n]));
                }
            }
            inst::WRITE => {
                let a = addr(p);
                if let Some((sid, t)) = self.servos.iter_mut().find(|(s, _)| *s == id) {
                    t[a..a + p.len() - 2].copy_from_slice(&p[2..]);
                    if a == x_series::GOAL_POSITION as usize {
                        let goal = t[a..a + 4].to_vec();
                        t[x_series::PRESENT_POSITION as usize..][..4].copy_from_slice(&goal);
                    }
                    out.extend(Self::status(*sid, &[]));
                }
            }
            inst::SYNC_WRITE => {
                let (a, n) = (addr(p), addr(&p[2..]));
                for chunk in p[4..].chunks_exact(n + 1) {
                    if let Some((_, t)) = self.servos.iter_mut().find(|(s, _)| *s == chunk[0]) {
                        t[a..a + n].copy_from_slice(&chunk[1..]);
                        if a == x_series::GOAL_POSITION as usize {
                            // Ideal servo: reaches the goal immediately.
                            t[x_series::PRESENT_POSITION as usize..][..4]
                                .copy_from_slice(&chunk[1..5]);
                        }
                    }
                }
            }
            inst::SYNC_READ => {
                let (a, n) = (addr(p), addr(&p[2..]));
                for &rid in &p[4..] {
                    if let Some((sid, t)) = self.servos.iter().find(|(s, _)| *s == rid) {
                        out.extend(Self::status(*sid, &t[a..a + n]));
                    }
                }
            }
            _ => {}
        }
        out
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

    // Reference packets captured from the Robotis Python SDK
    // (tools/servo_fixtures.py); the ping and read packets also appear in the
    // Protocol 2.0 e-Manual.
    #[test]
    fn packets_match_vendor_sdk() {
        assert_eq!(ping(1), hex("ff ff fd 00 01 03 00 01 19 4e"));
        assert_eq!(
            read(1, 132, 4),
            hex("ff ff fd 00 01 07 00 02 84 00 04 00 1d 15")
        );
        assert_eq!(
            write(1, 116, &512u32.to_le_bytes()),
            hex("ff ff fd 00 01 09 00 03 74 00 00 02 00 00 ca 89")
        );
        assert_eq!(
            write(1, 64, &[1]),
            hex("ff ff fd 00 01 06 00 03 40 00 01 db 66")
        );
        assert_eq!(
            sync_write(
                116,
                4,
                &[(1, &150u32.to_le_bytes()), (2, &170u32.to_le_bytes())]
            ),
            hex("ff ff fd 00 fe 11 00 83 74 00 04 00 01 96 00 00 00 02 aa 00 00 00 82 87")
        );
        assert_eq!(
            sync_read(132, 4, &[1, 2]),
            hex("ff ff fd 00 fe 09 00 82 84 00 04 00 01 02 ce fa")
        );
    }

    // Status packets from the Protocol 2.0 e-Manual examples.
    #[test]
    fn manual_status_packets_parse() {
        let (s, _) = parse_status(&hex("ff ff fd 00 01 07 00 55 00 06 04 26 65 5d")).unwrap();
        assert_eq!((s.id, s.error), (1, 0));
        assert_eq!(s.params, [0x06, 0x04, 0x26]); // model 1030, firmware 38
        let (s, _) = parse_status(&hex("ff ff fd 00 01 08 00 55 00 a6 00 00 00 8c c0")).unwrap();
        assert_eq!(i32::from_le_bytes(s.params.try_into().unwrap()), 166);
        let (s, _) = parse_status(&hex("ff ff fd 00 02 08 00 55 00 1f 08 00 00 ba be")).unwrap();
        assert_eq!(
            (s.id, i32::from_le_bytes(s.params.try_into().unwrap())),
            (2, 2079)
        );
        let (s, _) = parse_status(&hex("ff ff fd 00 01 04 00 55 00 a1 0c")).unwrap();
        assert!(s.params.is_empty());
    }

    #[test]
    fn crc_of_status_packet_matches_sdk() {
        let status = hex("ff ff fd 00 01 08 00 55 00 a6 01 00 00");
        assert_eq!(crc16(&status), 0x409b);
    }

    #[test]
    fn header_sequences_are_stuffed() {
        // Expected bytes from the SDK's addStuffing + updateCRC.
        let p = write(1, 116, &[0xFF, 0xFF, 0xFD, 0x00]);
        assert_eq!(p, hex("ff ff fd 00 01 0a 00 03 74 00 ff ff fd fd 00 21 e7"));
        assert_eq!(
            unstuff(&p[7..p.len() - 2]),
            [0x03, 0x74, 0x00, 0xFF, 0xFF, 0xFD, 0x00]
        );
        // A lone FD or FF FD is not escaped.
        assert_eq!(stuff(&[0xFF, 0xFD, 0xFD]), [0xFF, 0xFD, 0xFD]);
    }

    #[test]
    fn status_parse_handles_noise_and_fragments() {
        let pkt = VirtualServos::status(1, &[0xA6, 0x01, 0x00, 0x00]);
        assert_eq!(
            &pkt[..13],
            &hex("ff ff fd 00 01 08 00 55 00 a6 01 00 00")[..]
        );
        assert_eq!(&pkt[13..], &0x409bu16.to_le_bytes());

        let mut buf = vec![0x00, 0x13, 0xFF];
        buf.extend_from_slice(&pkt);
        assert_eq!(
            parse_status(&buf),
            Err(ParseError::Corrupt {
                skip: 3,
                reason: "garbage before header"
            })
        );
        let (s, used) = parse_status(&buf[3..]).unwrap();
        assert_eq!(used, pkt.len());
        assert_eq!(s.id, 1);
        assert_eq!(s.params, [0xA6, 0x01, 0, 0]);

        assert_eq!(parse_status(&pkt[..9]), Err(ParseError::Incomplete));
        let mut bad = pkt.clone();
        bad[9] ^= 1;
        assert!(matches!(
            parse_status(&bad),
            Err(ParseError::Corrupt {
                reason: "CRC mismatch",
                ..
            })
        ));
    }

    #[test]
    fn bus_reads_and_reports_servo_errors() {
        let mut servos = VirtualServos::new(&[1, 2]);
        let mut bus = Bus::new(MockTransport::with_responder(move |p| servos.handle(p)));
        assert_eq!(bus.ping(2).unwrap(), (1020, 52));
        bus.write(1, x_series::GOAL_POSITION, &3000i32.to_le_bytes())
            .unwrap();
        let r = bus.read(1, x_series::PRESENT_POSITION, 4).unwrap();
        assert_eq!(i32::from_le_bytes(r.try_into().unwrap()), 3000);
        assert!(matches!(bus.ping(7), Err(BusError::Timeout { id: 7 })));

        // Error reply: error number 0x07 (access error).
        let mut t = MockTransport::new();
        let pkt = instruction(3, inst::STATUS, &[0x07]);
        t.responder = Some(Box::new(move |_| pkt.clone()));
        let mut bus = Bus::new(t);
        assert!(matches!(
            bus.write(3, 64, &[1]),
            Err(BusError::Servo { id: 3, error: 7 })
        ));
    }

    #[test]
    fn sync_read_collects_every_servo() {
        let mut servos = VirtualServos::new(&[1, 2, 3]);
        let mut bus = Bus::new(MockTransport::with_responder(move |p| servos.handle(p)));
        let items: Vec<(u8, [u8; 4])> = vec![
            (1, 100i32.to_le_bytes()),
            (2, 2048i32.to_le_bytes()),
            (3, 4000i32.to_le_bytes()),
        ];
        let refs: Vec<(u8, &[u8])> = items.iter().map(|(i, d)| (*i, d.as_slice())).collect();
        bus.sync_write(x_series::GOAL_POSITION, 4, &refs).unwrap();
        let r = bus
            .sync_read(x_series::PRESENT_POSITION, 4, &[1, 2, 3])
            .unwrap();
        let pos: Vec<i32> = r
            .iter()
            .map(|b| i32::from_le_bytes(b[..4].try_into().unwrap()))
            .collect();
        assert_eq!(pos, [100, 2048, 4000]);
    }

    #[test]
    fn units() {
        assert_eq!(rad_to_ticks(0.0), 2048);
        assert_eq!(rad_to_ticks(std::f64::consts::PI), 4096);
        assert!((ticks_to_rad(1024) + std::f64::consts::FRAC_PI_2).abs() < 1e-12);
        assert_eq!(rad_to_ticks(ticks_to_rad(1234)), 1234);
    }
}
