//! Video4Linux2 capture with raw ioctls (no libv4l) and the `hw/v4l2-camera` node.
//!
//! Supports YUYV and MJPEG, the two formats virtually every UVC webcam offers,
//! using memory-mapped streaming I/O. Struct layouts are the 64-bit kernel
//! ABI; tests pin them to the sizes and offsets in `<linux/videodev2.h>`.

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;

use ruro::prelude::*;

use crate::pixel::{decode_jpeg, yuyv_to_rgb};

pub const fn fourcc(s: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*s)
}

pub const PIX_FMT_YUYV: u32 = fourcc(b"YUYV");
pub const PIX_FMT_MJPEG: u32 = fourcc(b"MJPG");
pub const PIX_FMT_JPEG: u32 = fourcc(b"JPEG");

pub const BUF_TYPE_VIDEO_CAPTURE: u32 = 1;
pub const MEMORY_MMAP: u32 = 1;
pub const FIELD_ANY: u32 = 0;
pub const CAP_VIDEO_CAPTURE: u32 = 0x0000_0001;
pub const CAP_STREAMING: u32 = 0x0400_0000;
pub const CAP_DEVICE_CAPS: u32 = 0x8000_0000;

const fn ioc(dir: u64, nr: u64, size: usize) -> libc::c_ulong {
    ((dir << 30) | ((size as u64) << 16) | ((b'V' as u64) << 8) | nr) as libc::c_ulong
}
const R: u64 = 2;
const W: u64 = 1;
const RW: u64 = 3;

pub const VIDIOC_QUERYCAP: libc::c_ulong = ioc(R, 0, size_of::<Capability>());
pub const VIDIOC_ENUM_FMT: libc::c_ulong = ioc(RW, 2, size_of::<FmtDesc>());
pub const VIDIOC_S_FMT: libc::c_ulong = ioc(RW, 5, size_of::<Format>());
pub const VIDIOC_REQBUFS: libc::c_ulong = ioc(RW, 8, size_of::<RequestBuffers>());
pub const VIDIOC_QUERYBUF: libc::c_ulong = ioc(RW, 9, size_of::<Buffer>());
pub const VIDIOC_QBUF: libc::c_ulong = ioc(RW, 15, size_of::<Buffer>());
pub const VIDIOC_DQBUF: libc::c_ulong = ioc(RW, 17, size_of::<Buffer>());
pub const VIDIOC_STREAMON: libc::c_ulong = ioc(W, 18, size_of::<i32>());
pub const VIDIOC_STREAMOFF: libc::c_ulong = ioc(W, 19, size_of::<i32>());
pub const VIDIOC_S_PARM: libc::c_ulong = ioc(RW, 22, size_of::<StreamParm>());

/// `struct v4l2_capability`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Capability {
    pub driver: [u8; 16],
    pub card: [u8; 32],
    pub bus_info: [u8; 32],
    pub version: u32,
    pub capabilities: u32,
    pub device_caps: u32,
    pub reserved: [u32; 3],
}

/// `struct v4l2_fmtdesc`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FmtDesc {
    pub index: u32,
    pub type_: u32,
    pub flags: u32,
    pub description: [u8; 32],
    pub pixelformat: u32,
    pub mbus_code: u32,
    pub reserved: [u32; 3],
}

/// `struct v4l2_pix_format`.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct PixFormat {
    pub width: u32,
    pub height: u32,
    pub pixelformat: u32,
    pub field: u32,
    pub bytesperline: u32,
    pub sizeimage: u32,
    pub colorspace: u32,
    pub priv_: u32,
    pub flags: u32,
    pub ycbcr_enc: u32,
    pub quantization: u32,
    pub xfer_func: u32,
}

/// `struct v4l2_format` (the union is 200 bytes and 8-byte aligned).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Format {
    pub type_: u32,
    _pad: u32,
    pub pix: PixFormat,
    _rest: [u8; 200 - size_of::<PixFormat>()],
}

/// `struct v4l2_streamparm` with the capture member of its union.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct StreamParm {
    pub type_: u32,
    pub capability: u32,
    pub capturemode: u32,
    pub timeperframe_numerator: u32,
    pub timeperframe_denominator: u32,
    pub extendedmode: u32,
    pub readbuffers: u32,
    _rest: [u8; 200 - 24],
}

/// `struct v4l2_requestbuffers`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct RequestBuffers {
    pub count: u32,
    pub type_: u32,
    pub memory: u32,
    pub capabilities: u32,
    pub flags: u8,
    pub reserved: [u8; 3],
}

/// `struct v4l2_buffer` (64-bit layout).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Buffer {
    pub index: u32,
    pub type_: u32,
    pub bytesused: u32,
    pub flags: u32,
    pub field: u32,
    _pad: u32,
    pub timestamp_sec: i64,
    pub timestamp_usec: i64,
    pub timecode: [u32; 4],
    pub sequence: u32,
    pub memory: u32,
    /// Union: `offset` for MMAP buffers (low 32 bits).
    pub m: u64,
    pub length: u32,
    pub reserved2: u32,
    pub request_fd: i32,
    _pad2: u32,
}

fn zeroed<T: Copy>() -> T {
    // SAFETY: only used for the plain-old-data ioctl structs above, for which
    // all-zero bytes are a valid value.
    unsafe { std::mem::zeroed() }
}

fn cstr(b: &[u8]) -> String {
    String::from_utf8_lossy(b.split(|&c| c == 0).next().unwrap_or(b)).into_owned()
}

fn xioctl<T>(fd: i32, req: libc::c_ulong, arg: &mut T) -> io::Result<()> {
    loop {
        // SAFETY: `arg` is the struct type matching `req` (sizes are encoded in `req`).
        let r = unsafe { libc::ioctl(fd, req, arg as *mut T) };
        if r >= 0 {
            return Ok(());
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

/// Device identity and capture formats, from QUERYCAP and ENUM_FMT only.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub driver: String,
    pub card: String,
    pub bus_info: String,
    pub capabilities: u32,
    /// (fourcc, description)
    pub formats: Vec<(u32, String)>,
}

impl DeviceInfo {
    pub fn can_capture(&self) -> bool {
        self.capabilities & CAP_VIDEO_CAPTURE != 0 && self.capabilities & CAP_STREAMING != 0
    }
}

/// Queries a device without configuring or streaming it.
pub fn query(file: &File) -> io::Result<DeviceInfo> {
    let fd = file.as_raw_fd();
    let mut cap: Capability = zeroed();
    xioctl(fd, VIDIOC_QUERYCAP, &mut cap)?;
    let capabilities = if cap.capabilities & CAP_DEVICE_CAPS != 0 {
        cap.device_caps
    } else {
        cap.capabilities
    };
    let mut formats = Vec::new();
    for index in 0.. {
        let mut d: FmtDesc = zeroed();
        d.index = index;
        d.type_ = BUF_TYPE_VIDEO_CAPTURE;
        if xioctl(fd, VIDIOC_ENUM_FMT, &mut d).is_err() {
            break;
        }
        formats.push((d.pixelformat, cstr(&d.description)));
    }
    Ok(DeviceInfo {
        driver: cstr(&cap.driver),
        card: cstr(&cap.card),
        bus_info: cstr(&cap.bus_info),
        capabilities,
        formats,
    })
}

/// Picks the pixel format: `"yuyv"`, `"mjpeg"` or `"auto"` (MJPEG if offered,
/// since it reaches higher resolutions and frame rates over USB 2.0).
pub fn choose_format(requested: &str, offered: &[u32]) -> Result<u32> {
    let has = |f: u32| offered.contains(&f);
    let mjpeg = [PIX_FMT_MJPEG, PIX_FMT_JPEG].into_iter().find(|&f| has(f));
    match requested {
        "yuyv" if has(PIX_FMT_YUYV) => Ok(PIX_FMT_YUYV),
        "mjpeg" => mjpeg.context("camera does not offer MJPEG"),
        "auto" => mjpeg
            .or(has(PIX_FMT_YUYV).then_some(PIX_FMT_YUYV))
            .context("camera offers neither MJPEG nor YUYV"),
        "yuyv" => bail!("camera does not offer YUYV"),
        other => bail!("unknown format `{other}` (expected auto, yuyv or mjpeg)"),
    }
}

struct Mapped {
    ptr: *mut libc::c_void,
    len: usize,
}

/// An open, streaming capture device.
pub struct Capture {
    file: File,
    buffers: Vec<Mapped>,
    pub format: PixFormat,
    streaming: bool,
}

// SAFETY: the mapped buffers are owned by this struct and only accessed through &mut self.
unsafe impl Send for Capture {}

impl Capture {
    /// Opens, configures and starts streaming.
    pub fn open(path: &str, width: u32, height: u32, fps: u32, format: &str) -> Result<Self> {
        let file = File::options()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .with_context(|| format!("opening {path}"))?;
        let fd = file.as_raw_fd();
        let info = query(&file)?;
        ensure!(
            info.can_capture(),
            "{path} ({}) cannot stream video capture",
            info.card
        );
        let offered: Vec<u32> = info.formats.iter().map(|f| f.0).collect();
        let pixfmt = choose_format(format, &offered)?;

        let mut fmt: Format = zeroed();
        fmt.type_ = BUF_TYPE_VIDEO_CAPTURE;
        fmt.pix.width = width;
        fmt.pix.height = height;
        fmt.pix.pixelformat = pixfmt;
        fmt.pix.field = FIELD_ANY;
        xioctl(fd, VIDIOC_S_FMT, &mut fmt).context("VIDIOC_S_FMT")?;
        ensure!(
            fmt.pix.pixelformat == pixfmt,
            "driver changed the pixel format"
        );
        if (fmt.pix.width, fmt.pix.height) != (width, height) {
            warn!(
                "{path}: {width}x{height} not supported, using {}x{}",
                fmt.pix.width, fmt.pix.height
            );
        }

        let mut parm: StreamParm = zeroed();
        parm.type_ = BUF_TYPE_VIDEO_CAPTURE;
        parm.timeperframe_numerator = 1;
        parm.timeperframe_denominator = fps.max(1);
        if let Err(e) = xioctl(fd, VIDIOC_S_PARM, &mut parm) {
            warn!("{path}: cannot set frame rate: {e}");
        }

        let mut req = RequestBuffers {
            count: 4,
            type_: BUF_TYPE_VIDEO_CAPTURE,
            memory: MEMORY_MMAP,
            ..Default::default()
        };
        xioctl(fd, VIDIOC_REQBUFS, &mut req).context("VIDIOC_REQBUFS")?;
        ensure!(req.count >= 2, "driver granted only {} buffers", req.count);

        let mut cap = Capture {
            file,
            buffers: Vec::new(),
            format: fmt.pix,
            streaming: false,
        };
        for index in 0..req.count {
            let mut b = Buffer {
                index,
                type_: BUF_TYPE_VIDEO_CAPTURE,
                memory: MEMORY_MMAP,
                ..Default::default()
            };
            xioctl(fd, VIDIOC_QUERYBUF, &mut b).context("VIDIOC_QUERYBUF")?;
            // SAFETY: maps the driver buffer at the offset it reported.
            let ptr = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    b.length as usize,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    fd,
                    b.m as libc::off_t,
                )
            };
            if ptr == libc::MAP_FAILED {
                return Err(io::Error::last_os_error()).context("mmap");
            }
            cap.buffers.push(Mapped {
                ptr,
                len: b.length as usize,
            });
            xioctl(fd, VIDIOC_QBUF, &mut b).context("VIDIOC_QBUF")?;
        }
        let mut ty = BUF_TYPE_VIDEO_CAPTURE as i32;
        xioctl(fd, VIDIOC_STREAMON, &mut ty).context("VIDIOC_STREAMON")?;
        cap.streaming = true;
        Ok(cap)
    }

    /// Waits up to `timeout_ms` for a frame and converts it to RGB8.
    pub fn next_frame(&mut self, timeout_ms: i32) -> Result<Option<Image>> {
        let fd = self.file.as_raw_fd();
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        if unsafe { libc::poll(&mut pfd, 1, timeout_ms) } <= 0 {
            return Ok(None);
        }
        let mut b = Buffer {
            type_: BUF_TYPE_VIDEO_CAPTURE,
            memory: MEMORY_MMAP,
            ..Default::default()
        };
        match xioctl(fd, VIDIOC_DQBUF, &mut b) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(None),
            Err(e) => return Err(e).context("VIDIOC_DQBUF"),
        }
        let m = &self.buffers[b.index as usize];
        let used = (b.bytesused as usize).min(m.len);
        // SAFETY: the dequeued buffer belongs to us until it is queued again.
        let data = unsafe { std::slice::from_raw_parts(m.ptr as *const u8, used) };
        let f = self.format;
        let image = match f.pixelformat {
            PIX_FMT_YUYV => {
                let mut img = Image::new(f.width, f.height, Encoding::Rgb8);
                let stride = (f.bytesperline as usize).max(f.width as usize * 2);
                if data.len() >= stride * (f.height as usize - 1) + f.width as usize * 2 {
                    yuyv_to_rgb(
                        data,
                        f.width as usize,
                        f.height as usize,
                        stride,
                        &mut img.data,
                    );
                    Some(img)
                } else {
                    None
                }
            }
            _ => match decode_jpeg(data) {
                Ok((w, h, px)) => Some(Image {
                    width: w,
                    height: h,
                    encoding: Encoding::Rgb8,
                    data: px,
                }),
                Err(e) => {
                    debug!("dropping corrupt MJPEG frame: {e}");
                    None
                }
            },
        };
        xioctl(fd, VIDIOC_QBUF, &mut b).context("VIDIOC_QBUF")?;
        Ok(image)
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let fd = self.file.as_raw_fd();
        if self.streaming {
            let mut ty = BUF_TYPE_VIDEO_CAPTURE as i32;
            let _ = xioctl(fd, VIDIOC_STREAMOFF, &mut ty);
        }
        for m in &self.buffers {
            // SAFETY: unmapping exactly what we mapped.
            unsafe { libc::munmap(m.ptr, m.len) };
        }
    }
}

/// USB/V4L2 camera. Captures YUYV or MJPEG with memory-mapped streaming and
/// publishes RGB8 images. Runs at the node rate; set it to the camera `fps`.
#[derive(Node)]
#[node(kind = "hw/v4l2-camera", rate = 30)]
pub struct V4l2Camera {
    #[output]
    image: Output<Image>,
    /// Video device.
    #[param(default = "/dev/video0")]
    device: String,
    /// Requested width; the driver may pick the nearest supported size.
    #[param(default = 640, min = 16, max = 7680)]
    width: u32,
    #[param(default = 480, min = 16, max = 4320)]
    height: u32,
    /// Requested frame rate.
    #[param(default = 30, min = 1, max = 240)]
    fps: u32,
    /// `auto`, `yuyv` or `mjpeg`.
    #[param(default = "auto")]
    format: String,
    capture: Option<Capture>,
}

impl Node for V4l2Camera {
    fn start(&mut self, _ctx: &mut Ctx) -> Result {
        let cap = Capture::open(
            &self.device,
            self.width,
            self.height,
            self.fps,
            &self.format,
        )?;
        info!(
            "{}: {}x{} {}",
            self.device,
            cap.format.width,
            cap.format.height,
            String::from_utf8_lossy(&cap.format.pixelformat.to_le_bytes())
        );
        self.capture = Some(cap);
        Ok(())
    }

    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        let cap = self.capture.as_mut().context("camera not open")?;
        if let Some(img) = cap.next_frame(100)? {
            self.image.send(img)?;
        }
        Ok(())
    }

    fn stop(&mut self, _ctx: &mut Ctx) {
        self.capture = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_matches_kernel_headers() {
        // Sizes and ioctl numbers printed from <linux/videodev2.h> on x86_64.
        assert_eq!(size_of::<Capability>(), 104);
        assert_eq!(size_of::<FmtDesc>(), 64);
        assert_eq!(size_of::<PixFormat>(), 48);
        assert_eq!(size_of::<Format>(), 208);
        assert_eq!(size_of::<StreamParm>(), 204);
        assert_eq!(size_of::<RequestBuffers>(), 20);
        assert_eq!(size_of::<Buffer>(), 88);
        assert_eq!(std::mem::offset_of!(Format, pix), 8);
        assert_eq!(std::mem::offset_of!(Buffer, timestamp_sec), 24);
        assert_eq!(std::mem::offset_of!(Buffer, sequence), 56);
        assert_eq!(std::mem::offset_of!(Buffer, memory), 60);
        assert_eq!(std::mem::offset_of!(Buffer, m), 64);
        assert_eq!(std::mem::offset_of!(Buffer, length), 72);
        assert_eq!(std::mem::offset_of!(Buffer, request_fd), 80);
        assert_eq!(std::mem::offset_of!(FmtDesc, pixelformat), 44);
        assert_eq!(std::mem::offset_of!(Capability, device_caps), 88);
        assert_eq!(VIDIOC_QUERYCAP, 0x8068_5600);
        assert_eq!(VIDIOC_ENUM_FMT, 0xc040_5602);
        assert_eq!(VIDIOC_S_FMT, 0xc0d0_5605);
        assert_eq!(VIDIOC_REQBUFS, 0xc014_5608);
        assert_eq!(VIDIOC_QUERYBUF, 0xc058_5609);
        assert_eq!(VIDIOC_QBUF, 0xc058_560f);
        assert_eq!(VIDIOC_DQBUF, 0xc058_5611);
        assert_eq!(VIDIOC_STREAMON, 0x4004_5612);
        assert_eq!(VIDIOC_STREAMOFF, 0x4004_5613);
        assert_eq!(VIDIOC_S_PARM, 0xc0cc_5616);
        assert_eq!((PIX_FMT_YUYV, PIX_FMT_MJPEG), (0x5659_5559, 0x4750_4a4d));
    }

    #[test]
    fn format_choice() {
        let both = [PIX_FMT_YUYV, PIX_FMT_MJPEG];
        assert_eq!(choose_format("auto", &both).unwrap(), PIX_FMT_MJPEG);
        assert_eq!(choose_format("yuyv", &both).unwrap(), PIX_FMT_YUYV);
        assert_eq!(
            choose_format("auto", &[PIX_FMT_YUYV]).unwrap(),
            PIX_FMT_YUYV
        );
        assert_eq!(
            choose_format("mjpeg", &[PIX_FMT_JPEG]).unwrap(),
            PIX_FMT_JPEG
        );
        assert!(choose_format("mjpeg", &[PIX_FMT_YUYV]).is_err());
        assert!(choose_format("auto", &[fourcc(b"NV12")]).is_err());
        assert!(choose_format("h264", &both).is_err());
    }

    /// Opens a camera only to read its capabilities and format list
    /// (QUERYCAP and ENUM_FMT). Nothing is configured, mapped or streamed.
    /// RURO_TEST_V4L2_QUERY=/dev/video0 cargo test -p ruro-nodes-hw v4l2_query -- --nocapture
    #[test]
    fn v4l2_query() {
        let Ok(path) = std::env::var("RURO_TEST_V4L2_QUERY") else {
            return;
        };
        let file = File::options().read(true).open(&path).unwrap();
        let info = query(&file).unwrap();
        println!(
            "{path}: {} ({}) at {}",
            info.card, info.driver, info.bus_info
        );
        for (f, d) in &info.formats {
            println!("  {} {d}", String::from_utf8_lossy(&f.to_le_bytes()));
        }
        assert!(!info.driver.is_empty());
    }
}
