//! The standard message vocabulary (`std/*`).

use serde::{Deserialize, Serialize};

use crate::{DecodeError, Message, Preview, impl_postcard_message};

/// Longest edge, in pixels, of an image preview.
const PREVIEW_MAX_EDGE: u32 = 320;

/// A single floating point value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Scalar(pub f64);
impl_postcard_message!(Scalar, "std/Scalar");

/// A UTF-8 string.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Text(pub String);
impl_postcard_message!(Text, "std/Text");

/// A 3D vector.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Vec3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}
impl_postcard_message!(Vec3, "std/Vec3");

/// A unit quaternion.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Quat {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub w: f64,
}

impl Default for Quat {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            w: 1.0,
        }
    }
}

/// Position and orientation in 3D space.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Pose {
    pub position: Vec3,
    pub orientation: Quat,
}
impl_postcard_message!(Pose, "std/Pose", preview: |p: &Pose| Preview::Frames(vec![p.frame_preview("pose", "")]));

impl Pose {
    /// This pose as a named frame for 3D previews.
    pub fn frame_preview(&self, name: &str, parent: &str) -> crate::FramePreview {
        let (t, r) = (self.position, self.orientation);
        crate::FramePreview {
            name: name.to_owned(),
            parent: parent.to_owned(),
            translation: [t.x as f32, t.y as f32, t.z as f32],
            rotation: [r.x as f32, r.y as f32, r.z as f32, r.w as f32],
        }
    }
}

/// Linear and angular velocity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Twist {
    pub linear: Vec3,
    pub angular: Vec3,
}
impl_postcard_message!(Twist, "std/Twist");

/// State of a set of joints. Vectors are index-aligned.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct JointState {
    pub position: Vec<f64>,
    pub velocity: Vec<f64>,
    pub effort: Vec<f64>,
}
impl_postcard_message!(JointState, "std/JointState");

/// Pixel layout of an [`Image`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[repr(u8)]
pub enum Encoding {
    #[default]
    Rgb8 = 0,
    Rgba8 = 1,
    Mono8 = 2,
}

impl Encoding {
    pub fn channels(self) -> usize {
        match self {
            Encoding::Rgb8 => 3,
            Encoding::Rgba8 => 4,
            Encoding::Mono8 => 1,
        }
    }

    fn from_u8(v: u8) -> Result<Self, DecodeError> {
        match v {
            0 => Ok(Encoding::Rgb8),
            1 => Ok(Encoding::Rgba8),
            2 => Ok(Encoding::Mono8),
            other => Err(DecodeError::Invalid(format!(
                "unknown image encoding {other}"
            ))),
        }
    }
}

/// A dense, row-major 8-bit image.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub encoding: Encoding,
    pub data: Vec<u8>,
}

impl Image {
    /// Allocates a zeroed image.
    pub fn new(width: u32, height: u32, encoding: Encoding) -> Self {
        let len = width as usize * height as usize * encoding.channels();
        Self {
            width,
            height,
            encoding,
            data: vec![0; len],
        }
    }

    /// Bytes per row.
    pub fn stride(&self) -> usize {
        self.width as usize * self.encoding.channels()
    }
}

const IMAGE_HEADER: usize = 4 + 4 + 1;

impl Message for Image {
    const TYPE: &'static str = "std/Image";

    fn encoded_len(&self) -> usize {
        IMAGE_HEADER + self.data.len()
    }

    fn encode_into(&self, buf: &mut [u8]) {
        buf[0..4].copy_from_slice(&self.width.to_le_bytes());
        buf[4..8].copy_from_slice(&self.height.to_le_bytes());
        buf[8] = self.encoding as u8;
        buf[IMAGE_HEADER..].copy_from_slice(&self.data);
    }

    fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        if buf.len() < IMAGE_HEADER {
            return Err(DecodeError::TooShort {
                need: IMAGE_HEADER,
                got: buf.len(),
            });
        }
        let width = u32::from_le_bytes(buf[0..4].try_into().unwrap());
        let height = u32::from_le_bytes(buf[4..8].try_into().unwrap());
        let encoding = Encoding::from_u8(buf[8])?;
        let need = IMAGE_HEADER + width as usize * height as usize * encoding.channels();
        if buf.len() != need {
            return Err(DecodeError::TooShort {
                need,
                got: buf.len(),
            });
        }
        Ok(Self {
            width,
            height,
            encoding,
            data: buf[IMAGE_HEADER..].to_vec(),
        })
    }

    fn preview(&self) -> Preview {
        if self.width == 0 || self.height == 0 {
            return Preview::Text("empty image".into());
        }
        let scale = (self.width.max(self.height) as f32 / PREVIEW_MAX_EDGE as f32).max(1.0);
        let pw = ((self.width as f32 / scale) as u32).max(1);
        let ph = ((self.height as f32 / scale) as u32).max(1);
        let ch = self.encoding.channels();
        let stride = self.stride();
        let mut rgba = Vec::with_capacity(pw as usize * ph as usize * 4);
        for y in 0..ph {
            let sy = ((y as f32 * scale) as usize).min(self.height as usize - 1);
            let row = &self.data[sy * stride..(sy + 1) * stride];
            for x in 0..pw {
                let sx = ((x as f32 * scale) as usize).min(self.width as usize - 1);
                let px = &row[sx * ch..sx * ch + ch];
                match self.encoding {
                    Encoding::Rgb8 => rgba.extend_from_slice(&[px[0], px[1], px[2], 255]),
                    Encoding::Rgba8 => rgba.extend_from_slice(px),
                    Encoding::Mono8 => rgba.extend_from_slice(&[px[0], px[0], px[0], 255]),
                }
            }
        }
        Preview::Image {
            width: pw,
            height: ph,
            rgba,
        }
    }
}

/// An unordered set of 3D points.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PointCloud {
    pub points: Vec<[f32; 3]>,
}

impl Message for PointCloud {
    const TYPE: &'static str = "std/PointCloud";

    fn encoded_len(&self) -> usize {
        self.points.len() * 12
    }

    fn encode_into(&self, buf: &mut [u8]) {
        for (chunk, p) in buf.as_chunks_mut::<12>().0.iter_mut().zip(&self.points) {
            chunk[0..4].copy_from_slice(&p[0].to_le_bytes());
            chunk[4..8].copy_from_slice(&p[1].to_le_bytes());
            chunk[8..12].copy_from_slice(&p[2].to_le_bytes());
        }
    }

    fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        if !buf.len().is_multiple_of(12) {
            return Err(DecodeError::Invalid(
                "point cloud length is not a multiple of 12".into(),
            ));
        }
        let f = |b: &[u8]| f32::from_le_bytes(b.try_into().unwrap());
        let points = buf
            .as_chunks::<12>()
            .0
            .iter()
            .map(|c| [f(&c[0..4]), f(&c[4..8]), f(&c[8..12])])
            .collect();
        Ok(Self { points })
    }

    fn preview(&self) -> Preview {
        Preview::Points {
            frame: String::new(),
            points: crate::subsample_points(self.points.iter().copied()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<M: Message + PartialEq + std::fmt::Debug>(m: M) {
        let bytes = m.to_bytes();
        assert_eq!(bytes.len(), m.encoded_len());
        assert_eq!(M::decode(&bytes).unwrap(), m);
    }

    #[test]
    fn roundtrips() {
        roundtrip(Scalar(4.5));
        roundtrip(Text("hello".into()));
        roundtrip(Pose {
            position: Vec3 {
                x: 1.0,
                y: 2.0,
                z: 3.0,
            },
            ..Default::default()
        });
        roundtrip(JointState {
            position: vec![0.1, 0.2],
            velocity: vec![],
            effort: vec![1.0],
        });
        let mut img = Image::new(7, 3, Encoding::Rgb8);
        img.data
            .iter_mut()
            .enumerate()
            .for_each(|(i, b)| *b = i as u8);
        roundtrip(img);
        roundtrip(PointCloud {
            points: vec![[1.0, 2.0, 3.0], [-1.0, 0.5, 9.0]],
        });
    }

    #[test]
    fn image_decode_rejects_truncated() {
        let img = Image::new(4, 4, Encoding::Mono8);
        let bytes = img.to_bytes();
        assert!(Image::decode(&bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn image_preview_is_downscaled_rgba() {
        let img = Image::new(1280, 720, Encoding::Rgb8);
        let Preview::Image {
            width,
            height,
            rgba,
        } = img.preview()
        else {
            panic!()
        };
        assert_eq!((width, height), (320, 180));
        assert_eq!(rgba.len(), 320 * 180 * 4);
    }
}
