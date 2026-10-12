//! Linux GPIO character device, uAPI v2 (`/dev/gpiochipN`), and the
//! `hw/gpio-out` and `hw/gpio-in` nodes.
//!
//! Structures are encoded by hand into byte buffers with the exact layout of
//! `<linux/gpio.h>`; the tests pin sizes and offsets to the kernel header.

use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use ruro::prelude::*;

pub const GPIO_V2_LINES_MAX: usize = 64;
pub const GPIO_V2_LINE_NUM_ATTRS_MAX: usize = 10;
pub const GPIO_MAX_NAME_SIZE: usize = 32;

pub const LINE_REQUEST_SIZE: usize = 592;
const REQ_CONSUMER: usize = 256;
const REQ_CONFIG: usize = 288;
const REQ_NUM_LINES: usize = 560;
const REQ_EVENT_BUFFER_SIZE: usize = 564;
const REQ_FD: usize = 588;
const CONFIG_FLAGS: usize = 0;
const CONFIG_NUM_ATTRS: usize = 8;
const CONFIG_ATTRS: usize = 32;
#[cfg(test)]
const CONFIG_ATTR_SIZE: usize = 24;

pub const GPIO_GET_CHIPINFO_IOCTL: libc::c_ulong = 0x8044_b401;
pub const GPIO_V2_GET_LINE_IOCTL: libc::c_ulong = 0xc250_b407;
pub const GPIO_V2_LINE_GET_VALUES_IOCTL: libc::c_ulong = 0xc010_b40e;
pub const GPIO_V2_LINE_SET_VALUES_IOCTL: libc::c_ulong = 0xc010_b40f;

pub mod flags {
    pub const ACTIVE_LOW: u64 = 1 << 1;
    pub const INPUT: u64 = 1 << 2;
    pub const OUTPUT: u64 = 1 << 3;
    pub const BIAS_PULL_UP: u64 = 1 << 8;
    pub const BIAS_PULL_DOWN: u64 = 1 << 9;
    pub const BIAS_DISABLED: u64 = 1 << 10;
}

pub const ATTR_ID_OUTPUT_VALUES: u32 = 2;

/// Line bias for inputs.
pub fn bias_flags(bias: &str) -> Result<u64, String> {
    match bias {
        "" | "as-is" => Ok(0),
        "none" | "disabled" => Ok(flags::BIAS_DISABLED),
        "pull-up" => Ok(flags::BIAS_PULL_UP),
        "pull-down" => Ok(flags::BIAS_PULL_DOWN),
        other => Err(format!(
            "unknown bias `{other}` (expected as-is, none, pull-up or pull-down)"
        )),
    }
}

/// Encodes a `struct gpio_v2_line_request` for the given line offsets.
/// `initial` sets output values (bit i = line i) when requesting outputs.
pub fn line_request(offsets: &[u32], consumer: &str, flags: u64, initial: Option<u64>) -> Vec<u8> {
    assert!(offsets.len() <= GPIO_V2_LINES_MAX);
    let mut b = vec![0u8; LINE_REQUEST_SIZE];
    for (i, o) in offsets.iter().enumerate() {
        b[4 * i..4 * i + 4].copy_from_slice(&o.to_ne_bytes());
    }
    let name = consumer.as_bytes();
    let n = name.len().min(GPIO_MAX_NAME_SIZE - 1);
    b[REQ_CONSUMER..REQ_CONSUMER + n].copy_from_slice(&name[..n]);
    let cfg = REQ_CONFIG;
    b[cfg + CONFIG_FLAGS..cfg + CONFIG_FLAGS + 8].copy_from_slice(&flags.to_ne_bytes());
    if let Some(values) = initial {
        b[cfg + CONFIG_NUM_ATTRS..cfg + CONFIG_NUM_ATTRS + 4].copy_from_slice(&1u32.to_ne_bytes());
        // struct gpio_v2_line_config_attribute { attr { id, padding, values }, mask }
        let a = cfg + CONFIG_ATTRS;
        b[a..a + 4].copy_from_slice(&ATTR_ID_OUTPUT_VALUES.to_ne_bytes());
        b[a + 8..a + 16].copy_from_slice(&values.to_ne_bytes());
        let mask = if offsets.len() == 64 {
            u64::MAX
        } else {
            (1u64 << offsets.len()) - 1
        };
        b[a + 16..a + 24].copy_from_slice(&mask.to_ne_bytes());
    }
    b[REQ_NUM_LINES..REQ_NUM_LINES + 4].copy_from_slice(&(offsets.len() as u32).to_ne_bytes());
    b[REQ_EVENT_BUFFER_SIZE..REQ_EVENT_BUFFER_SIZE + 4].copy_from_slice(&0u32.to_ne_bytes());
    b
}

/// The line fd the kernel writes back into a request.
pub fn request_fd(req: &[u8]) -> i32 {
    i32::from_ne_bytes(req[REQ_FD..REQ_FD + 4].try_into().unwrap())
}

/// Encodes a `struct gpio_v2_line_values { bits, mask }`.
pub fn line_values(bits: u64, mask: u64) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[..8].copy_from_slice(&bits.to_ne_bytes());
    b[8..].copy_from_slice(&mask.to_ne_bytes());
    b
}

/// Chip name, label and line count from GPIO_GET_CHIPINFO_IOCTL.
pub fn chip_info(chip: &str) -> io::Result<(String, String, u32)> {
    let f = File::open(chip)?;
    let mut b = [0u8; 68];
    // SAFETY: the ioctl writes a 68-byte struct gpiochip_info into `b`.
    if unsafe { libc::ioctl(f.as_raw_fd(), GPIO_GET_CHIPINFO_IOCTL, b.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let s = |r: &[u8]| String::from_utf8_lossy(r.split(|&c| c == 0).next().unwrap()).into_owned();
    Ok((
        s(&b[..32]),
        s(&b[32..64]),
        u32::from_ne_bytes(b[64..68].try_into().unwrap()),
    ))
}

/// Requested GPIO lines.
pub struct Lines {
    fd: OwnedFd,
    count: usize,
}

impl Lines {
    pub fn request(
        chip: &str,
        offsets: &[u32],
        consumer: &str,
        flags: u64,
        initial: Option<u64>,
    ) -> io::Result<Self> {
        let f = File::options().read(true).write(true).open(chip)?;
        let mut req = line_request(offsets, consumer, flags, initial);
        // SAFETY: `req` is a correctly sized gpio_v2_line_request buffer.
        if unsafe { libc::ioctl(f.as_raw_fd(), GPIO_V2_GET_LINE_IOCTL, req.as_mut_ptr()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = request_fd(&req);
        if fd < 0 {
            return Err(io::Error::other("kernel returned no line fd"));
        }
        // SAFETY: the kernel handed us ownership of a fresh line descriptor.
        Ok(Self {
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
            count: offsets.len(),
        })
    }

    fn mask(&self) -> u64 {
        if self.count == 64 {
            u64::MAX
        } else {
            (1 << self.count) - 1
        }
    }

    pub fn get(&self) -> io::Result<u64> {
        let mut v = line_values(0, self.mask());
        // SAFETY: 16-byte gpio_v2_line_values buffer.
        if unsafe {
            libc::ioctl(
                self.fd.as_raw_fd(),
                GPIO_V2_LINE_GET_VALUES_IOCTL,
                v.as_mut_ptr(),
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(u64::from_ne_bytes(v[..8].try_into().unwrap()))
    }

    pub fn set(&self, bits: u64) -> io::Result<()> {
        let mut v = line_values(bits, self.mask());
        // SAFETY: 16-byte gpio_v2_line_values buffer.
        if unsafe {
            libc::ioctl(
                self.fd.as_raw_fd(),
                GPIO_V2_LINE_SET_VALUES_IOCTL,
                v.as_mut_ptr(),
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// Drives one GPIO line from a boolean input. The line is requested as an
/// output on start (at `initial`) and released on stop.
#[derive(Node)]
#[node(kind = "hw/gpio-out")]
pub struct GpioOut {
    /// Logical level to drive (`true` = active).
    #[input]
    value: Input<Flag>,
    /// GPIO chip device.
    #[param(default = "/dev/gpiochip0")]
    chip: String,
    /// Line offset on the chip.
    #[param(default = 0, min = 0, max = 511)]
    line: u32,
    /// Invert the line (active = low voltage).
    #[param(default = false)]
    active_low: bool,
    /// Level set when the line is requested.
    #[param(default = false)]
    initial: bool,
    lines: Option<Lines>,
}

impl Node for GpioOut {
    fn start(&mut self, ctx: &mut Ctx) -> Result {
        let mut f = flags::OUTPUT;
        if self.active_low {
            f |= flags::ACTIVE_LOW;
        }
        let lines = Lines::request(
            &self.chip,
            &[self.line],
            ctx.id(),
            f,
            Some(self.initial as u64),
        )
        .with_context(|| format!("requesting {} line {}", self.chip, self.line))?;
        self.lines = Some(lines);
        Ok(())
    }

    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        if let Some(v) = self.value.latest() {
            self.lines
                .as_ref()
                .context("line not requested")?
                .set(v.0 as u64)?;
        }
        Ok(())
    }
}

/// Reads one GPIO line at the node rate and publishes its logical level.
#[derive(Node)]
#[node(kind = "hw/gpio-in", rate = 100)]
pub struct GpioIn {
    /// Logical level (`true` = active).
    #[output]
    value: Output<Flag>,
    /// GPIO chip device.
    #[param(default = "/dev/gpiochip0")]
    chip: String,
    /// Line offset on the chip.
    #[param(default = 0, min = 0, max = 511)]
    line: u32,
    /// Invert the line (active = low voltage).
    #[param(default = false)]
    active_low: bool,
    /// `as-is`, `none`, `pull-up` or `pull-down`.
    #[param(default = "as-is")]
    bias: String,
    /// Publish only when the level changes.
    #[param(default = true)]
    on_change: bool,
    lines: Option<Lines>,
    last: Option<bool>,
}

impl Node for GpioIn {
    fn start(&mut self, ctx: &mut Ctx) -> Result {
        let mut f = flags::INPUT | bias_flags(&self.bias).map_err(|e| anyhow!(e))?;
        if self.active_low {
            f |= flags::ACTIVE_LOW;
        }
        let lines = Lines::request(&self.chip, &[self.line], ctx.id(), f, None)
            .with_context(|| format!("requesting {} line {}", self.chip, self.line))?;
        self.lines = Some(lines);
        Ok(())
    }

    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        let v = self.lines.as_ref().context("line not requested")?.get()? & 1 == 1;
        if !self.on_change || self.last != Some(v) {
            self.last = Some(v);
            self.value.send(Flag(v))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_layout_matches_kernel_header() {
        // Sizes, offsets and ioctl numbers printed from <linux/gpio.h> (x86_64).
        assert_eq!(LINE_REQUEST_SIZE, 592);
        assert_eq!(
            (
                REQ_CONSUMER,
                REQ_CONFIG,
                REQ_NUM_LINES,
                REQ_EVENT_BUFFER_SIZE,
                REQ_FD
            ),
            (256, 288, 560, 564, 588)
        );
        assert_eq!(
            (CONFIG_NUM_ATTRS, CONFIG_ATTRS, CONFIG_ATTR_SIZE),
            (8, 32, 24)
        );
        assert_eq!(
            CONFIG_ATTRS + GPIO_V2_LINE_NUM_ATTRS_MAX * CONFIG_ATTR_SIZE,
            272
        );
        assert_eq!(
            GPIO_V2_GET_LINE_IOCTL & 0x3FFF_0000,
            (LINE_REQUEST_SIZE as libc::c_ulong) << 16
        );
        assert_eq!(
            (flags::INPUT, flags::OUTPUT, flags::ACTIVE_LOW),
            (0x4, 0x8, 0x2)
        );
        assert_eq!(
            (
                flags::BIAS_PULL_UP,
                flags::BIAS_PULL_DOWN,
                flags::BIAS_DISABLED
            ),
            (0x100, 0x200, 0x400)
        );
    }

    #[test]
    fn output_request_encoding() {
        let r = line_request(
            &[17, 27],
            "ruro-test",
            flags::OUTPUT | flags::ACTIVE_LOW,
            Some(0b10),
        );
        assert_eq!(&r[0..8], &[17, 0, 0, 0, 27, 0, 0, 0]);
        assert_eq!(&r[256..265], b"ruro-test");
        assert_eq!(r[265], 0);
        assert_eq!(u64::from_ne_bytes(r[288..296].try_into().unwrap()), 0xA);
        assert_eq!(u32::from_ne_bytes(r[296..300].try_into().unwrap()), 1); // num_attrs
        let attr = 288 + 32;
        assert_eq!(
            u32::from_ne_bytes(r[attr..attr + 4].try_into().unwrap()),
            ATTR_ID_OUTPUT_VALUES
        );
        assert_eq!(
            u64::from_ne_bytes(r[attr + 8..attr + 16].try_into().unwrap()),
            0b10
        );
        assert_eq!(
            u64::from_ne_bytes(r[attr + 16..attr + 24].try_into().unwrap()),
            0b11
        );
        assert_eq!(u32::from_ne_bytes(r[560..564].try_into().unwrap()), 2);
        assert_eq!(request_fd(&r), 0);
    }

    #[test]
    fn input_request_has_no_attributes() {
        let r = line_request(
            &[4],
            &"x".repeat(64),
            flags::INPUT | flags::BIAS_PULL_UP,
            None,
        );
        assert_eq!(u64::from_ne_bytes(r[288..296].try_into().unwrap()), 0x104);
        assert_eq!(u32::from_ne_bytes(r[296..300].try_into().unwrap()), 0);
        // Consumer is truncated and NUL terminated.
        assert_eq!(r[256 + 31], 0);
        assert_eq!(r[256 + 30], b'x');
    }

    #[test]
    fn values_and_bias() {
        let v = line_values(0b101, 0b111);
        assert_eq!(u64::from_ne_bytes(v[..8].try_into().unwrap()), 5);
        assert_eq!(u64::from_ne_bytes(v[8..].try_into().unwrap()), 7);
        assert_eq!(bias_flags("pull-down").unwrap(), flags::BIAS_PULL_DOWN);
        assert!(bias_flags("sideways").is_err());
    }

    /// Read-only query of a chip's name and line count. Never requests lines.
    /// RURO_TEST_GPIOCHIP=/dev/gpiochip0 cargo test -p ruro-nodes-hw gpio_chip_info
    #[test]
    fn gpio_chip_info() {
        let Ok(chip) = std::env::var("RURO_TEST_GPIOCHIP") else {
            return;
        };
        let (name, label, lines) = chip_info(&chip).unwrap();
        println!("{chip}: {name} [{label}] with {lines} lines");
        assert!(!name.is_empty());
    }
}
