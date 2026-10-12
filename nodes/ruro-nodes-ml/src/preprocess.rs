//! Image to tensor conversion: resize (stretch or letterbox), scale,
//! normalise and lay out as NCHW float32.

use ruro::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resize {
    /// Scale each axis independently to the target size.
    Stretch,
    /// Keep the aspect ratio and pad with `pad_value` (YOLO-style).
    Letterbox,
}

impl Resize {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "stretch" => Ok(Resize::Stretch),
            "letterbox" => Ok(Resize::Letterbox),
            other => bail!("unknown resize mode `{other}` (expected stretch or letterbox)"),
        }
    }
}

/// Maps model-input pixel coordinates back to the source image.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mapping {
    pub scale_x: f32,
    pub scale_y: f32,
    pub pad_x: f32,
    pub pad_y: f32,
}

impl Mapping {
    pub const IDENTITY: Mapping = Mapping {
        scale_x: 1.0,
        scale_y: 1.0,
        pad_x: 0.0,
        pad_y: 0.0,
    };

    /// Source-image coordinates of model-input point `(x, y)`.
    pub fn to_source(&self, x: f32, y: f32) -> (f32, f32) {
        (
            (x - self.pad_x) / self.scale_x,
            (y - self.pad_y) / self.scale_y,
        )
    }
}

/// Preprocessing settings.
#[derive(Debug, Clone)]
pub struct Preprocess {
    pub width: usize,
    pub height: usize,
    pub resize: Resize,
    /// Applied to 0..255 pixel values first (typically 1/255).
    pub scale: f32,
    /// Per channel, after scaling: `(v - mean) / std`.
    pub mean: [f32; 3],
    pub std: [f32; 3],
    /// Channel order of the tensor: false = RGB, true = BGR.
    pub bgr: bool,
    /// Fill value for letterbox padding, in 0..255 pixel units.
    pub pad_value: f32,
}

impl Default for Preprocess {
    fn default() -> Self {
        Self {
            width: 640,
            height: 640,
            resize: Resize::Letterbox,
            scale: 1.0 / 255.0,
            mean: [0.0; 3],
            std: [1.0; 3],
            bgr: false,
            pad_value: 114.0,
        }
    }
}

impl Preprocess {
    /// Converts `img` to a `[1, 3, height, width]` tensor (bilinear resampling).
    pub fn run(&self, img: &Image) -> Result<(Vec<f32>, Mapping)> {
        ensure!(img.width > 0 && img.height > 0, "empty image");
        let (sw, sh) = (img.width as usize, img.height as usize);
        let (tw, th) = (self.width, self.height);
        let (sx, sy) = (tw as f32 / sw as f32, th as f32 / sh as f32);
        let map = match self.resize {
            Resize::Stretch => Mapping {
                scale_x: sx,
                scale_y: sy,
                pad_x: 0.0,
                pad_y: 0.0,
            },
            Resize::Letterbox => {
                let s = sx.min(sy);
                Mapping {
                    scale_x: s,
                    scale_y: s,
                    pad_x: ((tw as f32 - sw as f32 * s) / 2.0).floor(),
                    pad_y: ((th as f32 - sh as f32 * s) / 2.0).floor(),
                }
            }
        };
        let ch = img.encoding.channels();
        let stride = img.stride();
        let pixel = |x: usize, y: usize, c: usize| -> f32 {
            let i = y * stride + x * ch;
            match img.encoding {
                Encoding::Mono8 => img.data[i] as f32,
                _ => img.data[i + c] as f32,
            }
        };
        let plane = tw * th;
        let mut out = vec![0f32; 3 * plane];
        let norm: [(f32, f32); 3] = std::array::from_fn(|c| (self.mean[c], 1.0 / self.std[c]));
        let order: [usize; 3] = if self.bgr { [2, 1, 0] } else { [0, 1, 2] };
        let pad: [f32; 3] =
            std::array::from_fn(|c| (self.pad_value * self.scale - norm[c].0) * norm[c].1);

        // Precompute horizontal sample positions.
        let xs: Vec<Option<(usize, usize, f32)>> = (0..tw)
            .map(|x| {
                let fx = (x as f32 + 0.5 - map.pad_x) / map.scale_x - 0.5;
                if fx < -0.5 || fx > sw as f32 - 0.5 {
                    return None;
                }
                let fx = fx.clamp(0.0, (sw - 1) as f32);
                let x0 = fx.floor() as usize;
                Some((x0, (x0 + 1).min(sw - 1), fx - x0 as f32))
            })
            .collect();
        for y in 0..th {
            let fy = (y as f32 + 0.5 - map.pad_y) / map.scale_y - 0.5;
            let inside_y = fy >= -0.5 && fy <= sh as f32 - 0.5;
            let fy = fy.clamp(0.0, (sh - 1) as f32);
            let y0 = fy.floor() as usize;
            let (y1, wy) = ((y0 + 1).min(sh - 1), fy - y0 as f32);
            for (x, sample) in xs.iter().enumerate() {
                for (dst_c, &src_c) in order.iter().enumerate() {
                    let v = match (inside_y, sample) {
                        (true, Some((x0, x1, wx))) => {
                            let top =
                                pixel(*x0, y0, src_c) * (1.0 - wx) + pixel(*x1, y0, src_c) * wx;
                            let bot =
                                pixel(*x0, y1, src_c) * (1.0 - wx) + pixel(*x1, y1, src_c) * wx;
                            let v = top * (1.0 - wy) + bot * wy;
                            (v * self.scale - norm[src_c].0) * norm[src_c].1
                        }
                        _ => pad[src_c],
                    };
                    out[dst_c * plane + y * tw + x] = v;
                }
            }
        }
        Ok((out, map))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 3]) -> Image {
        let mut img = Image::new(w, h, Encoding::Rgb8);
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 3) as usize;
                img.data[i..i + 3].copy_from_slice(&f(x, y));
            }
        }
        img
    }

    #[test]
    fn identity_size_keeps_values_and_planes() {
        let img = image(4, 2, |x, y| [x as u8 * 10, y as u8 * 100, 255]);
        let p = Preprocess {
            width: 4,
            height: 2,
            resize: Resize::Stretch,
            scale: 1.0,
            ..Default::default()
        };
        let (t, map) = p.run(&img).unwrap();
        assert_eq!(map, Mapping::IDENTITY);
        assert_eq!(&t[0..4], &[0.0, 10.0, 20.0, 30.0]); // R plane, row 0
        assert_eq!(&t[8 + 4..8 + 8], &[100.0; 4]); // G plane, row 1
        assert!(t[16..].iter().all(|&v| v == 255.0)); // B plane
    }

    #[test]
    fn normalisation_and_bgr() {
        let img = image(2, 2, |_, _| [255, 0, 51]);
        let p = Preprocess {
            width: 2,
            height: 2,
            resize: Resize::Stretch,
            mean: [0.5, 0.5, 0.5],
            std: [0.5, 0.5, 0.5],
            bgr: true,
            ..Default::default()
        };
        let (t, _) = p.run(&img).unwrap();
        // Channel 0 is blue: (0.2 - 0.5) / 0.5 = -0.6; channel 2 is red: 1.0.
        assert!((t[0] + 0.6).abs() < 1e-6);
        assert!((t[8] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn letterbox_pads_and_maps_back() {
        // 8x4 image into 8x8: scale 1, 2 rows of padding top and bottom.
        let img = image(8, 4, |_, _| [200, 200, 200]);
        let p = Preprocess {
            width: 8,
            height: 8,
            scale: 1.0,
            ..Default::default()
        };
        let (t, map) = p.run(&img).unwrap();
        assert_eq!(map.pad_y, 2.0);
        assert_eq!(map.pad_x, 0.0);
        assert_eq!(&t[0..8], &[114.0; 8]); // padded first row
        assert_eq!(&t[2 * 8..3 * 8], &[200.0; 8]); // image starts at row 2
        assert_eq!(&t[6 * 8..7 * 8], &[114.0; 8]);
        assert_eq!(map.to_source(4.0, 4.0), (4.0, 2.0));

        // Downscale 1280x720 into 640x640.
        let big = image(1280, 720, |_, _| [0, 0, 0]);
        let p = Preprocess::default();
        let (t, map) = p.run(&big).unwrap();
        assert_eq!(t.len(), 3 * 640 * 640);
        assert_eq!((map.scale_x, map.pad_x, map.pad_y), (0.5, 0.0, 140.0));
        assert_eq!(map.to_source(320.0, 320.0), (640.0, 360.0));
    }

    #[test]
    fn bilinear_upsampling_interpolates() {
        let img = image(
            2,
            1,
            |x, _| if x == 0 { [0, 0, 0] } else { [100, 100, 100] },
        );
        let p = Preprocess {
            width: 4,
            height: 1,
            resize: Resize::Stretch,
            scale: 1.0,
            ..Default::default()
        };
        let (t, _) = p.run(&img).unwrap();
        assert_eq!(&t[0..4], &[0.0, 25.0, 75.0, 100.0]);
    }

    #[test]
    fn mono_images_fill_three_channels() {
        let mut img = Image::new(2, 2, Encoding::Mono8);
        img.data.fill(10);
        let p = Preprocess {
            width: 2,
            height: 2,
            resize: Resize::Stretch,
            scale: 1.0,
            ..Default::default()
        };
        let (t, _) = p.run(&img).unwrap();
        assert!(t.iter().all(|&v| v == 10.0));
    }
}
