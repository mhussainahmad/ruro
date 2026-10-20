//! `gpu/Image`: an image in GPU memory.

use std::sync::Arc;

use anyhow::{Result, ensure};
use ruro_msg::schema::{Describe, Field, Schema};
use ruro_msg::{DecodeError, Encoding, Image, Message, Preview};

use crate::ipc::{DESCRIPTOR_LEN, Descriptor};
use crate::memory::{DeviceBuffer, Stream, writable_buffer};

/// Longest edge, in pixels, of a preview.
const PREVIEW_MAX_EDGE: u32 = 320;

/// A dense 8-bit image in GPU memory, rows `step` bytes apart.
///
/// Within a process, receivers share the device buffer (no copy). Across
/// process groups on one machine, only a descriptor of the buffer travels
/// and the receiver maps the same memory (see [`crate::ipc`]).
///
/// Producers must finish writing (synchronize their stream) before sending;
/// receivers must finish reading before dropping the image. The helpers
/// here all wait for their copies.
#[derive(Clone, Default)]
pub struct GpuImage {
    pub width: u32,
    pub height: u32,
    pub encoding: Encoding,
    /// Bytes per row.
    pub step: u32,
    buffer: Option<Arc<DeviceBuffer>>,
}

impl std::fmt::Debug for GpuImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("encoding", &self.encoding)
            .field("step", &self.step)
            .field("buffer", &self.buffer)
            .finish()
    }
}

impl GpuImage {
    /// An image on `device` with tightly packed rows and unspecified pixels.
    pub fn new(device: u32, width: u32, height: u32, encoding: Encoding) -> Result<GpuImage> {
        let mut img = GpuImage::default();
        img.reshape(device, width, height, encoding)?;
        Ok(img)
    }

    /// Uploads `image` to `device`.
    pub fn from_image(device: u32, image: &Image, stream: &Stream) -> Result<GpuImage> {
        let mut img = GpuImage::default();
        img.upload(device, image, stream)?;
        Ok(img)
    }

    /// Bytes of pixel data.
    pub fn byte_len(&self) -> usize {
        self.step as usize * self.height as usize
    }

    /// Changes size and layout and makes the buffer writable, keeping it
    /// when nobody else holds it (in this process or another) and it is
    /// large enough. Pixel contents are unspecified afterwards. Use with
    /// `Output::loan` so a steady stream of frames does not allocate.
    pub fn reshape(
        &mut self,
        device: u32,
        width: u32,
        height: u32,
        encoding: Encoding,
    ) -> Result<()> {
        let step = width as usize * encoding.bytes_per_pixel();
        let len = step
            .checked_mul(height as usize)
            .filter(|&n| n <= u32::MAX as usize * 4)
            .ok_or_else(|| anyhow::anyhow!("image size {width}x{height} overflows"))?;
        writable_buffer(&mut self.buffer, device, len)?;
        self.width = width;
        self.height = height;
        self.encoding = encoding;
        self.step = step as u32;
        Ok(())
    }

    /// The device buffer, if the image has pixels.
    pub fn buffer(&self) -> Option<&DeviceBuffer> {
        self.buffer.as_deref()
    }

    /// The device buffer for writing. `None` if it is shared (call
    /// [`GpuImage::reshape`] first) or absent.
    pub fn buffer_mut(&mut self) -> Option<&mut DeviceBuffer> {
        self.buffer.as_mut().and_then(Arc::get_mut)
    }

    /// Device the pixels are on.
    pub fn device(&self) -> Option<u32> {
        self.buffer.as_ref().map(|b| b.device())
    }

    /// True if two images share one device buffer.
    pub fn shares_buffer_with(&self, other: &GpuImage) -> bool {
        match (&self.buffer, &other.buffer) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }

    /// Replaces the contents with `image`, uploaded to `device`.
    pub fn upload(&mut self, device: u32, image: &Image, stream: &Stream) -> Result<()> {
        self.reshape(device, image.width, image.height, image.encoding)?;
        let len = self.byte_len();
        ensure!(image.data.len() == len, "image data has the wrong length");
        if len == 0 {
            return Ok(());
        }
        let buf = self.buffer_mut().expect("reshape made it writable");
        buf.upload(&image.data, stream)
    }

    /// Downloads into `image`, reusing its allocation.
    pub fn download_into(&self, image: &mut Image, stream: &Stream) -> Result<()> {
        image.reshape(self.width, self.height, self.encoding);
        let row = image.stride();
        let Some(buf) = self.buffer() else {
            ensure!(image.data.is_empty(), "GPU image has no buffer");
            return Ok(());
        };
        if self.step as usize == row {
            buf.download(&mut image.data, stream)
        } else {
            buf.download_rows(
                &mut image.data,
                self.step as usize,
                row,
                1,
                self.height as usize,
                stream,
            )
        }
    }

    /// Downloads into a new image, on the device's default stream.
    pub fn to_image(&self) -> Result<Image> {
        let mut image = Image::default();
        let stream = Stream::default_for(self.device().unwrap_or(0));
        self.download_into(&mut image, &stream)?;
        Ok(image)
    }

    /// A downscaled preview: only every n-th row and column is downloaded.
    fn try_preview(&self) -> Result<Preview> {
        let Some(buf) = self.buffer() else {
            return Ok(Preview::Text("empty GPU image".into()));
        };
        if self.width == 0 || self.height == 0 {
            return Ok(Preview::Text("empty GPU image".into()));
        }
        let n = self
            .width
            .max(self.height)
            .div_ceil(PREVIEW_MAX_EDGE)
            .max(1) as usize;
        // Bytes of one pixel (two for 16-bit depth).
        let ch = self.encoding.bytes_per_pixel();
        let rows = (self.height as usize).div_ceil(n);
        let row_bytes = self.width as usize * ch;
        let mut sampled = vec![0u8; rows * row_bytes];
        let stream = Stream::default_for(buf.device());
        buf.download_rows(
            &mut sampled,
            self.step as usize,
            row_bytes,
            n,
            rows,
            &stream,
        )?;
        let cols = (self.width as usize).div_ceil(n);
        let mut small = Image::new(cols as u32, rows as u32, self.encoding);
        for (dst, src) in small
            .data
            .chunks_exact_mut(cols * ch)
            .zip(sampled.chunks_exact(row_bytes))
        {
            for (x, px) in dst.chunks_exact_mut(ch).enumerate() {
                px.copy_from_slice(&src[x * n * ch..x * n * ch + ch]);
            }
        }
        Ok(small.preview())
    }
}

/// `version, width, height, encoding, step, buffer flag`.
const HEADER: usize = 1 + 4 + 4 + 1 + 4 + 1;
const VERSION: u8 = 1;

/// Buffer flags in the encoding.
pub(crate) const NO_BUFFER: u8 = 0;
pub(crate) const SHARED_BUFFER: u8 = 1;
pub(crate) const EXPORT_FAILED: u8 = 2;

/// Writes `flag` and the descriptor (zeros if it could not be made).
pub(crate) fn encode_buffer(buffer: &DeviceBuffer, out: &mut [u8]) {
    match Descriptor::of(buffer) {
        Ok(d) => {
            out[0] = SHARED_BUFFER;
            d.write(&mut out[1..1 + DESCRIPTOR_LEN]);
        }
        Err(e) => {
            tracing::warn!("{e:#}");
            out[0] = EXPORT_FAILED;
            out[1..1 + DESCRIPTOR_LEN].fill(0);
        }
    }
}

/// Reads what [`encode_buffer`] wrote, mapping the buffer.
pub(crate) fn decode_buffer(flag: u8, rest: &[u8]) -> Result<DeviceBuffer, DecodeError> {
    match flag {
        SHARED_BUFFER => Descriptor::read(rest)
            .and_then(|d| d.import())
            .map_err(|e| DecodeError::Invalid(format!("{e:#}"))),
        EXPORT_FAILED => Err(DecodeError::Invalid(
            "the sender could not share this GPU buffer (see its log)".into(),
        )),
        other => Err(DecodeError::Invalid(format!(
            "unknown GPU buffer flag {other}"
        ))),
    }
}

impl Message for GpuImage {
    const TYPE: &'static str = "gpu/Image";

    fn schema() -> Schema {
        <Self as Describe>::describe()
    }

    fn encoded_len(&self) -> usize {
        HEADER
            + if self.buffer.is_some() {
                DESCRIPTOR_LEN
            } else {
                0
            }
    }

    fn encode_into(&self, buf: &mut [u8]) {
        buf[0] = VERSION;
        buf[1..5].copy_from_slice(&self.width.to_le_bytes());
        buf[5..9].copy_from_slice(&self.height.to_le_bytes());
        buf[9] = self.encoding as u8;
        buf[10..14].copy_from_slice(&self.step.to_le_bytes());
        match &self.buffer {
            None => buf[14] = NO_BUFFER,
            Some(b) => encode_buffer(b, &mut buf[14..]),
        }
    }

    fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        if buf.len() < HEADER {
            return Err(DecodeError::TooShort {
                need: HEADER,
                got: buf.len(),
            });
        }
        if buf[0] != VERSION {
            return Err(DecodeError::Invalid(format!(
                "unknown gpu/Image version {}",
                buf[0]
            )));
        }
        let u32_at = |i: usize| u32::from_le_bytes(buf[i..i + 4].try_into().unwrap());
        let (width, height, step) = (u32_at(1), u32_at(5), u32_at(10));
        let encoding = Encoding::from_u8(buf[9])?;
        let flag = buf[14];
        let buffer = if flag == NO_BUFFER {
            None
        } else {
            if buf.len() < HEADER + DESCRIPTOR_LEN {
                return Err(DecodeError::TooShort {
                    need: HEADER + DESCRIPTOR_LEN,
                    got: buf.len(),
                });
            }
            let b = decode_buffer(flag, &buf[HEADER..])?;
            let need = step as u64 * height as u64;
            if (b.len() as u64) < need
                || (step as u64) < width as u64 * encoding.bytes_per_pixel() as u64
            {
                return Err(DecodeError::Invalid(
                    "GPU image is larger than its buffer".into(),
                ));
            }
            Some(Arc::new(b))
        };
        Ok(GpuImage {
            width,
            height,
            encoding,
            step,
            buffer,
        })
    }

    fn preview(&self) -> Preview {
        self.try_preview().unwrap_or_else(|e| {
            Preview::Text(format!("GPU image {}x{}: {e:#}", self.width, self.height))
        })
    }
}

impl Describe for GpuImage {
    fn describe() -> Schema {
        Schema::Custom {
            name: "GpuImage".into(),
            encoding: "ruro/gpu-image-v1: u8 version (1), u32 LE width, u32 LE height, \
                       u8 encoding (0 rgb8, 1 rgba8, 2 mono8), u32 LE step, u8 buffer flag \
                       (0 none, 1 shared, 2 export failed), then for flags 1 and 2 a 128-byte \
                       CUDA IPC descriptor valid only on the sending machine"
                .into(),
            fields: vec![
                Field::new("width", Schema::U32),
                Field::new("height", Schema::U32),
                Field::new("encoding", Encoding::describe()),
                Field::new("step", Schema::U32).doc("Bytes per row."),
                Field::new("buffer", Schema::seq(Schema::U8))
                    .doc("Device memory, described by its CUDA IPC handle."),
            ],
        }
    }
}

ruro_msg::register_schema!(GpuImage);
