//! Linux I2C character devices (`/dev/i2c-N`) and the `hw/i2c` node.
//!
//! Register reads use one combined `I2C_RDWR` transaction (write the register
//! address, repeated start, read N bytes), which is what most sensors expect.

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;

use ruro::prelude::*;

pub const I2C_SLAVE: libc::c_ulong = 0x0703;
pub const I2C_RDWR: libc::c_ulong = 0x0707;
pub const I2C_M_RD: u16 = 0x0001;

/// `struct i2c_msg`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct I2cMsg {
    pub addr: u16,
    pub flags: u16,
    pub len: u16,
    pub buf: *mut u8,
}

/// `struct i2c_rdwr_ioctl_data`.
#[repr(C)]
pub struct I2cRdwrData {
    pub msgs: *mut I2cMsg,
    pub nmsgs: u32,
}

/// The two messages of a register read: write `reg`, then read into `out`.
pub fn register_read_msgs(addr: u16, reg: &mut [u8; 1], out: &mut [u8]) -> [I2cMsg; 2] {
    [
        I2cMsg {
            addr,
            flags: 0,
            len: 1,
            buf: reg.as_mut_ptr(),
        },
        I2cMsg {
            addr,
            flags: I2C_M_RD,
            len: out.len() as u16,
            buf: out.as_mut_ptr(),
        },
    ]
}

/// An I2C bus with a fixed target address.
pub struct I2cDevice {
    file: File,
    addr: u16,
}

impl I2cDevice {
    pub fn open(bus: &str, addr: u16) -> io::Result<Self> {
        if addr > 0x7F {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "only 7-bit addresses are supported",
            ));
        }
        let file = File::options().read(true).write(true).open(bus)?;
        // SAFETY: I2C_SLAVE takes the address as an integer argument.
        if unsafe { libc::ioctl(file.as_raw_fd(), I2C_SLAVE, addr as libc::c_ulong) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { file, addr })
    }

    /// Reads `out.len()` bytes starting at register `reg`.
    pub fn read_register(&self, reg: u8, out: &mut [u8]) -> io::Result<()> {
        let mut r = [reg];
        let mut msgs = register_read_msgs(self.addr, &mut r, out);
        let mut data = I2cRdwrData {
            msgs: msgs.as_mut_ptr(),
            nmsgs: 2,
        };
        // SAFETY: both messages point at live buffers of the stated lengths.
        if unsafe { libc::ioctl(self.file.as_raw_fd(), I2C_RDWR, &mut data) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Writes raw bytes (typically a register address followed by data).
    pub fn write(&self, data: &[u8]) -> io::Result<()> {
        use std::io::Write;
        (&self.file).write_all(data)
    }
}

/// Reads a block of registers from an I2C device at the node rate and/or
/// writes raw bytes to it. Each `write` message is sent as one I2C write
/// (first byte is usually the register address).
#[derive(Node)]
#[node(kind = "hw/i2c", rate = 50)]
pub struct I2c {
    /// Bytes to write in one transaction.
    #[input]
    write: Input<Bytes>,
    /// Register block read every step.
    #[output]
    data: Output<Bytes>,
    /// Bus device.
    #[param(default = "/dev/i2c-1")]
    bus: String,
    /// 7-bit device address.
    #[param(default = 104, min = 3, max = 119)]
    address: u16,
    /// First register to read.
    #[param(default = 0, min = 0, max = 255)]
    register: u8,
    /// Number of bytes to read each step (0 disables reading).
    #[param(default = 1, min = 0, max = 255)]
    length: u32,
    device: Option<I2cDevice>,
}

impl Node for I2c {
    fn start(&mut self, _ctx: &mut Ctx) -> Result {
        self.device = Some(
            I2cDevice::open(&self.bus, self.address)
                .with_context(|| format!("opening {} address {:#04x}", self.bus, self.address))?,
        );
        Ok(())
    }

    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        let dev = self.device.as_ref().context("device not open")?;
        while let Some(w) = self.write.recv() {
            dev.write(&w.0)?;
        }
        if self.length > 0 {
            let mut buf = vec![0u8; self.length as usize];
            dev.read_register(self.register, &mut buf)?;
            self.data.send(Bytes(buf))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_layout_matches_kernel_headers() {
        // From <linux/i2c.h> and <linux/i2c-dev.h> on x86_64.
        assert_eq!(std::mem::size_of::<I2cMsg>(), 16);
        assert_eq!(std::mem::size_of::<I2cRdwrData>(), 16);
        assert_eq!((I2C_SLAVE, I2C_RDWR, I2C_M_RD), (0x703, 0x707, 1));
    }

    #[test]
    fn register_read_is_write_then_read() {
        let mut reg = [0x3B];
        let mut out = [0u8; 6];
        let m = register_read_msgs(0x68, &mut reg, &mut out);
        assert_eq!((m[0].addr, m[0].flags, m[0].len), (0x68, 0, 1));
        assert_eq!((m[1].addr, m[1].flags, m[1].len), (0x68, I2C_M_RD, 6));
        assert_eq!(m[0].buf, reg.as_mut_ptr());
        assert_eq!(m[1].buf, out.as_mut_ptr());
    }

    #[test]
    fn rejects_ten_bit_addresses() {
        assert!(I2cDevice::open("/dev/null", 0x150).is_err());
    }
}
