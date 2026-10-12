//! Linux SocketCAN (classic CAN 2.0) and the `hw/can` node.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use ruro::prelude::*;

/// A classic CAN frame.
#[derive(Message, Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
#[message(name = "hw/CanFrame")]
pub struct CanFrame {
    /// 11-bit standard or 29-bit extended identifier.
    pub id: u32,
    pub extended: bool,
    /// Remote transmission request.
    pub rtr: bool,
    /// Error frame (received only; `id` holds the error class bits).
    pub error: bool,
    /// Up to 8 bytes.
    pub data: Vec<u8>,
}

pub const CAN_EFF_FLAG: u32 = 0x8000_0000;
pub const CAN_RTR_FLAG: u32 = 0x4000_0000;
pub const CAN_ERR_FLAG: u32 = 0x2000_0000;
pub const CAN_SFF_MASK: u32 = 0x0000_07FF;
pub const CAN_EFF_MASK: u32 = 0x1FFF_FFFF;
/// `sizeof(struct can_frame)`.
pub const FRAME_SIZE: usize = 16;

const PF_CAN: libc::c_int = 29;
const CAN_RAW: libc::c_int = 1;
const SIOCGIFINDEX: libc::c_ulong = 0x8933;

impl CanFrame {
    /// Encodes as the kernel's `struct can_frame` (16 bytes, host byte order).
    pub fn to_raw(&self) -> Result<[u8; FRAME_SIZE], String> {
        if self.data.len() > 8 {
            return Err(format!(
                "CAN frame has {} data bytes (max 8)",
                self.data.len()
            ));
        }
        let mask = if self.extended {
            CAN_EFF_MASK
        } else {
            CAN_SFF_MASK
        };
        if self.id & !mask != 0 {
            return Err(format!(
                "CAN id {:#x} does not fit the frame format",
                self.id
            ));
        }
        let mut id = self.id;
        if self.extended {
            id |= CAN_EFF_FLAG;
        }
        if self.rtr {
            id |= CAN_RTR_FLAG;
        }
        let mut raw = [0u8; FRAME_SIZE];
        raw[0..4].copy_from_slice(&id.to_ne_bytes());
        raw[4] = self.data.len() as u8;
        raw[8..8 + self.data.len()].copy_from_slice(&self.data);
        Ok(raw)
    }

    /// Decodes a kernel `struct can_frame`.
    pub fn from_raw(raw: &[u8; FRAME_SIZE]) -> Self {
        let id = u32::from_ne_bytes(raw[0..4].try_into().unwrap());
        let extended = id & CAN_EFF_FLAG != 0;
        let len = (raw[4] as usize).min(8);
        CanFrame {
            id: id & if extended { CAN_EFF_MASK } else { CAN_SFF_MASK },
            extended,
            rtr: id & CAN_RTR_FLAG != 0,
            error: id & CAN_ERR_FLAG != 0,
            data: raw[8..8 + len].to_vec(),
        }
    }
}

#[repr(C)]
struct SockaddrCan {
    family: libc::sa_family_t,
    ifindex: libc::c_int,
    rx_id: u32,
    tx_id: u32,
    _pad: [u8; 8],
}

/// A raw CAN socket bound to one interface.
pub struct CanSocket {
    fd: OwnedFd,
}

impl CanSocket {
    pub fn open(interface: &str) -> io::Result<Self> {
        if interface.len() >= libc::IFNAMSIZ {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "interface name too long",
            ));
        }
        // SAFETY: plain socket(2) call; the result is checked before use.
        let fd = unsafe { libc::socket(PF_CAN, libc::SOCK_RAW | libc::SOCK_CLOEXEC, CAN_RAW) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is a fresh, valid descriptor we own.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };

        let mut ifr = [0u8; 40]; // struct ifreq
        ifr[..interface.len()].copy_from_slice(interface.as_bytes());
        // SAFETY: SIOCGIFINDEX reads the name and writes an int at offset 16 of the 40-byte ifreq.
        if unsafe { libc::ioctl(fd.as_raw_fd(), SIOCGIFINDEX, ifr.as_mut_ptr()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let ifindex = i32::from_ne_bytes(ifr[16..20].try_into().unwrap());
        let addr = SockaddrCan {
            family: PF_CAN as libc::sa_family_t,
            ifindex,
            rx_id: 0,
            tx_id: 0,
            _pad: [0; 8],
        };
        // SAFETY: `addr` is a properly initialised sockaddr_can of the size given.
        let r = unsafe {
            libc::bind(
                fd.as_raw_fd(),
                &addr as *const SockaddrCan as *const libc::sockaddr,
                std::mem::size_of::<SockaddrCan>() as libc::socklen_t,
            )
        };
        if r < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { fd })
    }

    pub fn send(&self, frame: &CanFrame) -> io::Result<()> {
        let raw = frame
            .to_raw()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        // SAFETY: writes FRAME_SIZE bytes from a valid buffer.
        let n = unsafe { libc::write(self.fd.as_raw_fd(), raw.as_ptr().cast(), FRAME_SIZE) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Receives one frame, waiting at most `timeout_ms` (0 = poll).
    pub fn recv(&self, timeout_ms: i32) -> io::Result<Option<CanFrame>> {
        let mut pfd = libc::pollfd {
            fd: self.fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        let r = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        if r < 0 {
            return Err(io::Error::last_os_error());
        }
        if r == 0 {
            return Ok(None);
        }
        let mut raw = [0u8; FRAME_SIZE];
        // SAFETY: reads at most FRAME_SIZE bytes into a buffer of that size.
        let n = unsafe { libc::read(self.fd.as_raw_fd(), raw.as_mut_ptr().cast(), FRAME_SIZE) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        if n as usize != FRAME_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "short CAN frame",
            ));
        }
        Ok(Some(CanFrame::from_raw(&raw)))
    }
}

/// SocketCAN interface (`can0`, `vcan0`, ...). Sends frames from `tx` and
/// publishes every received frame on `rx`. Bring the interface up first, e.g.
/// `ip link set can0 up type can bitrate 500000`.
#[derive(Node)]
#[node(kind = "hw/can", rate = 1000)]
pub struct Can {
    #[input]
    tx: Input<CanFrame>,
    #[output]
    rx: Output<CanFrame>,
    /// Network interface name.
    #[param(default = "can0")]
    interface: String,
    socket: Option<CanSocket>,
}

impl Node for Can {
    fn start(&mut self, _ctx: &mut Ctx) -> Result {
        self.socket = Some(
            CanSocket::open(&self.interface)
                .with_context(|| format!("opening {}", self.interface))?,
        );
        Ok(())
    }

    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        let s = self.socket.as_ref().context("socket not open")?;
        while let Some(f) = self.tx.recv() {
            s.send(&f)?;
        }
        while let Some(f) = s.recv(0)? {
            self.rx.send(f)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_matches_kernel_headers() {
        // Values from <linux/can.h> on x86_64.
        assert_eq!(std::mem::size_of::<SockaddrCan>(), 24);
        assert_eq!(CAN_EFF_FLAG, 0x8000_0000);
        assert_eq!(CAN_RTR_FLAG, 0x4000_0000);
        assert_eq!(CAN_ERR_FLAG, 0x2000_0000);
    }

    #[test]
    fn standard_frame_encoding() {
        let f = CanFrame {
            id: 0x123,
            data: vec![0xDE, 0xAD, 0xBE, 0xEF],
            ..Default::default()
        };
        let raw = f.to_raw().unwrap();
        assert_eq!(&raw[0..4], &0x123u32.to_ne_bytes());
        assert_eq!(raw[4], 4);
        assert_eq!(&raw[8..12], &[0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(&raw[12..], &[0; 4]);
        assert_eq!(CanFrame::from_raw(&raw), f);
    }

    #[test]
    fn extended_rtr_and_error_frames() {
        let f = CanFrame {
            id: 0x1ABC_DEF0,
            extended: true,
            rtr: true,
            ..Default::default()
        };
        let raw = f.to_raw().unwrap();
        assert_eq!(
            u32::from_ne_bytes(raw[0..4].try_into().unwrap()),
            0x1ABC_DEF0 | CAN_EFF_FLAG | CAN_RTR_FLAG
        );
        assert_eq!(CanFrame::from_raw(&raw), f);

        let mut err = [0u8; FRAME_SIZE];
        err[0..4].copy_from_slice(&(CAN_ERR_FLAG | 0x004).to_ne_bytes());
        err[4] = 8;
        let e = CanFrame::from_raw(&err);
        assert!(e.error && !e.extended);
        assert_eq!(e.id, 0x004);
        assert_eq!(e.data.len(), 8);
    }

    #[test]
    fn invalid_frames_are_rejected() {
        let too_long = CanFrame {
            id: 1,
            data: vec![0; 9],
            ..Default::default()
        };
        assert!(too_long.to_raw().is_err());
        let big_id = CanFrame {
            id: 0x800,
            ..Default::default()
        };
        assert!(big_id.to_raw().is_err());
    }

    #[test]
    fn message_roundtrip() {
        use ruro::msg::Message as _;
        let f = CanFrame {
            id: 0x7FF,
            data: vec![1, 2, 3],
            ..Default::default()
        };
        assert_eq!(CanFrame::decode(&f.to_bytes()).unwrap(), f);
        assert_eq!(CanFrame::TYPE, "hw/CanFrame");
    }

    /// Loopback through a virtual CAN interface. Needs `vcan0` to exist and be
    /// up (`ip link add vcan0 type vcan && ip link set vcan0 up`, as root):
    /// RURO_TEST_VCAN=vcan0 cargo test -p ruro-nodes-hw vcan
    #[test]
    fn vcan_loopback() {
        let Ok(iface) = std::env::var("RURO_TEST_VCAN") else {
            return;
        };
        let a = CanSocket::open(&iface).unwrap();
        let b = CanSocket::open(&iface).unwrap();
        let f = CanFrame {
            id: 0x42,
            data: vec![9, 8, 7],
            ..Default::default()
        };
        a.send(&f).unwrap();
        assert_eq!(b.recv(1000).unwrap(), Some(f));
    }

    #[test]
    fn missing_interface_fails_cleanly() {
        assert!(CanSocket::open("ruro-no-such-can").is_err());
    }
}
