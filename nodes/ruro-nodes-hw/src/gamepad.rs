//! Gamepads and joysticks through Linux evdev (`/dev/input/event*`) and the
//! `hw/gamepad` node. Devices are opened read-only and never grabbed, so other
//! programs keep receiving their events.

use std::fs::File;
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;

use ruro::prelude::*;

pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_ABS: u16 = 0x03;
pub const SYN_REPORT: u16 = 0;
pub const SYN_DROPPED: u16 = 3;
pub const BTN_JOYSTICK: u16 = 0x120;
pub const BTN_GAMEPAD: u16 = 0x130;
/// Last code of the joystick/gamepad button range (BTN_THUMBR).
pub const BTN_GAMEPAD_LAST: u16 = 0x13e;
pub const BTN_DPAD_UP: u16 = 0x220;
pub const BTN_DPAD_RIGHT: u16 = 0x223;
pub const BTN_TRIGGER_HAPPY: u16 = 0x2c0;
pub const BTN_TRIGGER_HAPPY_LAST: u16 = 0x2e7;
pub const KEY_MAX: u16 = 0x2ff;
pub const ABS_MAX: u16 = 0x3f;
/// `sizeof(struct input_event)` on 64-bit Linux.
pub const EVENT_SIZE: usize = 24;

const fn ioc_read(nr: u64, size: u64) -> libc::c_ulong {
    ((2u64 << 30) | (size << 16) | ((b'E' as u64) << 8) | nr) as libc::c_ulong
}

pub const fn eviocgname(len: u64) -> libc::c_ulong {
    ioc_read(0x06, len)
}

pub const fn eviocgbit(ev: u64, len: u64) -> libc::c_ulong {
    ioc_read(0x20 + ev, len)
}

pub const fn eviocgabs(abs: u64) -> libc::c_ulong {
    ioc_read(0x40 + abs, 24)
}

/// One `struct input_event` (timestamps dropped).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    pub kind: u16,
    pub code: u16,
    pub value: i32,
}

/// Decodes events from a buffer of whole `input_event` records.
pub fn parse_events(buf: &[u8]) -> impl Iterator<Item = Event> + '_ {
    buf.as_chunks::<EVENT_SIZE>().0.iter().map(|e| Event {
        kind: u16::from_ne_bytes([e[16], e[17]]),
        code: u16::from_ne_bytes([e[18], e[19]]),
        value: i32::from_ne_bytes(e[20..24].try_into().unwrap()),
    })
}

/// Encodes an event record (for tests and simulators).
pub fn encode_event(kind: u16, code: u16, value: i32) -> [u8; EVENT_SIZE] {
    let mut b = [0u8; EVENT_SIZE];
    b[16..18].copy_from_slice(&kind.to_ne_bytes());
    b[18..20].copy_from_slice(&code.to_ne_bytes());
    b[20..24].copy_from_slice(&value.to_ne_bytes());
    b
}

/// `struct input_absinfo` fields used for normalisation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbsInfo {
    pub minimum: i32,
    pub maximum: i32,
    pub flat: i32,
}

impl AbsInfo {
    /// Maps a raw value from `minimum..=maximum` to -1..1. Values within
    /// `flat` of the centre read as 0. Analog triggers therefore rest at -1.
    pub fn normalize(&self, v: i32) -> f32 {
        let span = (self.maximum - self.minimum) as f32;
        if span <= 0.0 {
            return 0.0;
        }
        let center = (self.maximum as f32 + self.minimum as f32) / 2.0;
        if (v as f32 - center).abs() <= self.flat as f32 {
            return 0.0;
        }
        ((v - self.minimum) as f32 / span * 2.0 - 1.0).clamp(-1.0, 1.0)
    }
}

/// Is `code` a button worth reporting on a gamepad?
pub fn is_button(code: u16) -> bool {
    (BTN_JOYSTICK..=BTN_GAMEPAD_LAST).contains(&code)
        || (BTN_DPAD_UP..=BTN_DPAD_RIGHT).contains(&code)
        || (BTN_TRIGGER_HAPPY..=BTN_TRIGGER_HAPPY_LAST).contains(&code)
}

/// Current state of a device, updated from events.
#[derive(Debug, Clone)]
pub struct Pad {
    /// Axis codes in output order, with their ranges.
    pub axes: Vec<(u16, AbsInfo)>,
    /// Button codes in output order.
    pub buttons: Vec<u16>,
    pub joy: Joy,
    changed: bool,
}

impl Pad {
    pub fn new(axes: Vec<(u16, AbsInfo)>, buttons: Vec<u16>) -> Self {
        let joy = Joy {
            header: Header::default(),
            axes: vec![0.0; axes.len()],
            buttons: vec![0; buttons.len()],
        };
        Self {
            axes,
            buttons,
            joy,
            changed: false,
        }
    }

    /// Applies one event. Returns true at a `SYN_REPORT` that ends a frame with changes.
    pub fn apply(&mut self, e: Event) -> bool {
        match e.kind {
            EV_ABS => {
                if let Some(i) = self.axes.iter().position(|(c, _)| *c == e.code) {
                    self.joy.axes[i] = self.axes[i].1.normalize(e.value);
                    self.changed = true;
                }
            }
            EV_KEY => {
                if let Some(i) = self.buttons.iter().position(|c| *c == e.code) {
                    self.joy.buttons[i] = (e.value != 0) as i32;
                    self.changed = true;
                }
            }
            EV_SYN if e.code == SYN_REPORT => return std::mem::take(&mut self.changed),
            _ => {}
        }
        false
    }
}

fn test_bit(bits: &[u8], n: usize) -> bool {
    bits.get(n / 8).is_some_and(|b| b & (1 << (n % 8)) != 0)
}

/// An opened evdev device.
pub struct Device {
    file: File,
    pub name: String,
    pub pad: Pad,
    buf: Vec<u8>,
}

impl Device {
    pub fn open(path: &str) -> io::Result<Self> {
        let file = File::options()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)?;
        let fd = file.as_raw_fd();
        let mut name = [0u8; 256];
        // SAFETY: EVIOCGNAME writes at most 256 bytes into `name`.
        unsafe { libc::ioctl(fd, eviocgname(256), name.as_mut_ptr()) };
        let name = String::from_utf8_lossy(name.split(|&c| c == 0).next().unwrap()).into_owned();

        let mut keys = [0u8; KEY_MAX as usize / 8 + 1];
        let mut abs = [0u8; ABS_MAX as usize / 8 + 1];
        // SAFETY: each EVIOCGBIT call writes at most the buffer length given.
        unsafe {
            libc::ioctl(
                fd,
                eviocgbit(EV_KEY as u64, keys.len() as u64),
                keys.as_mut_ptr(),
            );
            libc::ioctl(
                fd,
                eviocgbit(EV_ABS as u64, abs.len() as u64),
                abs.as_mut_ptr(),
            );
        }
        let buttons: Vec<u16> = (0..=KEY_MAX)
            .filter(|&c| test_bit(&keys, c as usize) && is_button(c))
            .collect();
        let mut axes = Vec::new();
        for code in 0..=ABS_MAX {
            if !test_bit(&abs, code as usize) {
                continue;
            }
            let mut info = [0i32; 6]; // value, minimum, maximum, fuzz, flat, resolution
            // SAFETY: EVIOCGABS writes a 24-byte struct input_absinfo.
            if unsafe { libc::ioctl(fd, eviocgabs(code as u64), info.as_mut_ptr()) } == 0 {
                axes.push((
                    code,
                    AbsInfo {
                        minimum: info[1],
                        maximum: info[2],
                        flat: info[4],
                    },
                ));
            }
        }
        if buttons.is_empty() && axes.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{path} ({name}) is not a joystick or gamepad"),
            ));
        }
        Ok(Self {
            file,
            name,
            pad: Pad::new(axes, buttons),
            buf: vec![0u8; EVENT_SIZE * 64],
        })
    }

    /// Reads pending events; returns true if a frame with changes completed.
    pub fn poll(&mut self) -> io::Result<bool> {
        let mut updated = false;
        loop {
            match self.file.read(&mut self.buf) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "device closed",
                    ));
                }
                Ok(n) => {
                    for e in parse_events(&self.buf[..n]) {
                        updated |= self.pad.apply(e);
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(updated),
                Err(e) => return Err(e),
            }
        }
    }
}

/// Finds the first evdev device with gamepad or joystick buttons.
pub fn find_gamepad() -> Option<String> {
    let mut paths: Vec<_> = std::fs::read_dir("/dev/input")
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("event"))
        })
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .find(|p| {
            Device::open(p).is_ok_and(|d| {
                d.pad
                    .buttons
                    .iter()
                    .any(|&b| (BTN_JOYSTICK..=BTN_GAMEPAD_LAST).contains(&b))
            })
        })
}

/// Gamepad or joystick via Linux evdev. Axes are normalised to -1..1 in
/// kernel axis-code order (left stick X/Y, triggers, right stick, hat);
/// buttons are 0/1 in button-code order (south, east, north, west, ...).
#[derive(Node)]
#[node(kind = "hw/gamepad", rate = 100)]
pub struct Gamepad {
    #[output]
    joy: Output<Joy>,
    /// Event device, e.g. `/dev/input/event5`; empty picks the first gamepad found.
    #[param(default = "")]
    device: String,
    /// Publish every step instead of only when the state changes.
    #[param(default = false)]
    continuous: bool,
    dev: Option<Device>,
}

impl Node for Gamepad {
    fn start(&mut self, _ctx: &mut Ctx) -> Result {
        let path = if self.device.is_empty() {
            find_gamepad()
                .context("no gamepad found in /dev/input (check permissions: input group)")?
        } else {
            self.device.clone()
        };
        let dev = Device::open(&path).with_context(|| format!("opening {path}"))?;
        info!(
            "gamepad `{}` at {path}: {} axes, {} buttons",
            dev.name,
            dev.pad.axes.len(),
            dev.pad.buttons.len()
        );
        self.dev = Some(dev);
        Ok(())
    }

    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        let dev = self.dev.as_mut().context("device not open")?;
        if dev.poll()? || self.continuous {
            let mut joy = dev.pad.joy.clone();
            joy.header = Header::new("", ruro::msg::now_ns());
            self.joy.send(joy)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_matches_kernel_headers() {
        // Values printed from <linux/input.h> on x86_64.
        assert_eq!(eviocgname(256), 0x8100_4506);
        assert_eq!(eviocgbit(0, 32), 0x8020_4520);
        assert_eq!(eviocgbit(EV_KEY as u64, 96), 0x8060_4521);
        assert_eq!(eviocgbit(EV_ABS as u64, 8), 0x8008_4523);
        assert_eq!(eviocgabs(0), 0x8018_4540);
        assert_eq!(
            (BTN_GAMEPAD, BTN_JOYSTICK, KEY_MAX, ABS_MAX),
            (0x130, 0x120, 0x2ff, 0x3f)
        );
    }

    fn xbox_like() -> Pad {
        let stick = AbsInfo {
            minimum: -32768,
            maximum: 32767,
            flat: 128,
        };
        let trigger = AbsInfo {
            minimum: 0,
            maximum: 1023,
            flat: 0,
        };
        // ABS_X, ABS_Y, ABS_Z (trigger), ABS_RX; BTN_SOUTH, BTN_EAST, BTN_THUMBL.
        Pad::new(
            vec![(0, stick), (1, stick), (2, trigger), (3, stick)],
            vec![0x130, 0x131, 0x13d],
        )
    }

    #[test]
    fn events_update_joy_on_sync() {
        let mut pad = xbox_like();
        let mut stream = Vec::new();
        stream.extend(encode_event(EV_ABS, 0, 32767));
        stream.extend(encode_event(EV_ABS, 1, -32768));
        stream.extend(encode_event(EV_ABS, 2, 1023));
        stream.extend(encode_event(EV_ABS, 3, 100)); // inside the dead zone
        stream.extend(encode_event(EV_KEY, 0x131, 1));
        stream.extend(encode_event(EV_KEY, 0x2c, 1)); // a keyboard key: ignored
        stream.extend(encode_event(EV_SYN, SYN_REPORT, 0));
        let updates: Vec<bool> = parse_events(&stream).map(|e| pad.apply(e)).collect();
        assert_eq!(updates.iter().filter(|u| **u).count(), 1);
        assert!(*updates.last().unwrap());
        assert_eq!(pad.joy.axes, [1.0, -1.0, 1.0, 0.0]);
        assert_eq!(pad.joy.buttons, [0, 1, 0]);

        // Release; a sync with no change reports nothing.
        assert!(!pad.apply(Event {
            kind: EV_SYN,
            code: SYN_REPORT,
            value: 0
        }));
        pad.apply(Event {
            kind: EV_KEY,
            code: 0x131,
            value: 0,
        });
        assert!(pad.apply(Event {
            kind: EV_SYN,
            code: SYN_REPORT,
            value: 0
        }));
        assert_eq!(pad.joy.buttons, [0, 0, 0]);
    }

    #[test]
    fn normalisation() {
        let a = AbsInfo {
            minimum: 0,
            maximum: 255,
            flat: 15,
        };
        assert_eq!(a.normalize(0), -1.0);
        assert_eq!(a.normalize(255), 1.0);
        assert_eq!(a.normalize(130), 0.0);
        assert!((a.normalize(192) - 0.5058824).abs() < 1e-5);
        assert_eq!(
            AbsInfo {
                minimum: 5,
                maximum: 5,
                flat: 0
            }
            .normalize(5),
            0.0
        );
    }

    #[test]
    fn button_ranges() {
        assert!(is_button(0x130) && is_button(0x13e) && is_button(0x220) && is_button(0x2c0));
        assert!(!is_button(0x110) && !is_button(0x1e) && !is_button(0x13f));
    }

    #[test]
    fn bit_tests() {
        let bits = [0b0000_0010u8, 0b1000_0000];
        assert!(test_bit(&bits, 1) && test_bit(&bits, 15));
        assert!(!test_bit(&bits, 0) && !test_bit(&bits, 99));
    }
}
