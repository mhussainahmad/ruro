use ruro::prelude::*;

/// Synthetic camera: a drifting colour gradient with a bouncing ball.
#[derive(Node)]
#[node(kind = "std/fake-camera", rate = 30)]
pub struct FakeCamera {
    #[output]
    image: Output<Image>,
    /// Frame width in pixels.
    #[param(default = 640, min = 16, max = 3840)]
    width: u32,
    /// Frame height in pixels.
    #[param(default = 480, min = 16, max = 2160)]
    height: u32,
    /// Animation speed multiplier.
    #[param(default = 1.0, min = 0, max = 10)]
    speed: f64,
    phase: f64,
    row: Vec<u8>,
}

impl Node for FakeCamera {
    fn step(&mut self, ctx: &mut Ctx) -> Result {
        self.phase += ctx.dt().as_secs_f64() * self.speed;
        let (w, h) = (self.width as usize, self.height as usize);
        let mut img = Image::new(self.width, self.height, Encoding::Rgb8);

        // Background: one template row (red/blue gradient), green varies per row.
        let shift = (self.phase * 40.0) as usize;
        self.row.clear();
        self.row
            .extend((0..w).flat_map(|x| [((x + shift) % w * 255 / w) as u8, 0, 160]));
        for (y, row) in img.data.chunks_exact_mut(w * 3).enumerate() {
            row.copy_from_slice(&self.row);
            let g = (y * 255 / h) as u8;
            row.iter_mut().skip(1).step_by(3).for_each(|v| *v = g);
        }

        // Ball: only touch the rows and columns it covers.
        let (bx, by) = bounce(self.phase, w, h);
        let r = (w.min(h) / 10).max(2) as isize;
        for y in (by - r).max(0)..(by + r + 1).min(h as isize) {
            let dy = y - by;
            let half = ((r * r - dy * dy) as f64).sqrt() as isize;
            let (x0, x1) = (
                (bx - half).max(0) as usize,
                ((bx + half + 1).min(w as isize)).max(0) as usize,
            );
            if x0 < x1 {
                img.data[(y as usize * w + x0) * 3..(y as usize * w + x1) * 3].fill(250);
            }
        }
        self.image.send(img)
    }
}

/// Position of the ball: a Lissajous path inside the frame.
fn bounce(t: f64, w: usize, h: usize) -> (isize, isize) {
    let x = (0.5 + 0.4 * (t * 1.3).sin()) * w as f64;
    let y = (0.5 + 0.4 * (t * 1.7).cos()) * h as f64;
    (x as isize, y as isize)
}

/// Box blur with a configurable radius (separable, O(1) per pixel).
#[derive(Node)]
#[node(kind = "std/blur")]
pub struct Blur {
    #[input]
    image: Input<Image>,
    #[output]
    out: Output<Image>,
    /// Blur radius in pixels; 0 passes the image through.
    #[param(default = 4, min = 0, max = 32)]
    radius: u32,
    tmp: Vec<u8>,
    acc: Vec<u32>,
}

impl Node for Blur {
    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        let Some(sample) = self.image.latest() else {
            return Ok(());
        };
        let src: &Image = &sample;
        if self.radius == 0 || src.data.is_empty() {
            return self.out.send(src.clone());
        }
        let mut dst = Image::new(src.width, src.height, src.encoding);
        box_blur(
            src,
            &mut dst.data,
            self.radius as usize,
            &mut self.tmp,
            &mut self.acc,
        );
        self.out.send(dst)
    }
}

/// Blurs `src` into `dst` with a (2r+1)x(2r+1) box, clamping at the edges.
fn box_blur(src: &Image, dst: &mut [u8], r: usize, tmp: &mut Vec<u8>, acc: &mut Vec<u32>) {
    tmp.resize(src.data.len(), 0);
    let w = src.width as usize;
    match src.encoding.channels() {
        1 => blur_rows::<1>(&src.data, tmp, w, r),
        3 => blur_rows::<3>(&src.data, tmp, w, r),
        _ => blur_rows::<4>(&src.data, tmp, w, r),
    }
    blur_cols(tmp, dst, src.stride(), src.height as usize, r, acc);
}

/// Fixed-point reciprocal of the window size: `(sum * inv) >> 16 == sum / n`
/// for every sum a window of 8-bit values can produce (n <= 65).
fn reciprocal(r: usize) -> u32 {
    let n = (2 * r + 1) as u32;
    65536_u32.div_ceil(n)
}

fn blur_rows<const CH: usize>(src: &[u8], dst: &mut [u8], w: usize, r: usize) {
    let inv = reciprocal(r);
    for (srow, drow) in src.chunks_exact(w * CH).zip(dst.chunks_exact_mut(w * CH)) {
        let px = |i: usize| -> [u32; CH] {
            let i = i.min(w - 1) * CH;
            std::array::from_fn(|c| srow[i + c] as u32)
        };
        let mut acc = px(0).map(|v| v * (r as u32 + 1));
        for i in 1..=r {
            let p = px(i);
            for c in 0..CH {
                acc[c] += p[c];
            }
        }
        for (x, out) in drow.chunks_exact_mut(CH).enumerate() {
            for c in 0..CH {
                out[c] = ((acc[c] * inv) >> 16) as u8;
            }
            let (add, sub) = (px(x + r + 1), px(x.saturating_sub(r)));
            for c in 0..CH {
                acc[c] = acc[c] + add[c] - sub[c];
            }
        }
    }
}

fn blur_cols(src: &[u8], dst: &mut [u8], stride: usize, h: usize, r: usize, acc: &mut Vec<u32>) {
    let inv = reciprocal(r);
    let row = |y: usize| &src[y.min(h - 1) * stride..][..stride];
    acc.clear();
    acc.extend(row(0).iter().map(|&v| v as u32 * (r as u32 + 1)));
    for y in 1..=r {
        for (a, &v) in acc.iter_mut().zip(row(y)) {
            *a += v as u32;
        }
    }
    for (y, out) in dst.chunks_exact_mut(stride).enumerate() {
        for (o, &a) in out.iter_mut().zip(acc.iter()) {
            *o = ((a * inv) >> 16) as u8;
        }
        let (add, sub) = (row(y + r + 1), row(y.saturating_sub(r)));
        for ((a, &p), &m) in acc.iter_mut().zip(add).zip(sub) {
            *a = *a + p as u32 - m as u32;
        }
    }
}

/// Converts an image to 8-bit grayscale (Rec. 601 luma).
#[derive(Node)]
#[node(kind = "std/grayscale")]
pub struct Grayscale {
    #[input]
    image: Input<Image>,
    #[output]
    out: Output<Image>,
}

impl Node for Grayscale {
    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        let Some(sample) = self.image.latest() else {
            return Ok(());
        };
        let src: &Image = &sample;
        let mut dst = Image::new(src.width, src.height, Encoding::Mono8);
        match src.encoding {
            Encoding::Mono8 => dst.data.copy_from_slice(&src.data),
            enc => {
                for (d, px) in dst
                    .data
                    .iter_mut()
                    .zip(src.data.chunks_exact(enc.channels()))
                {
                    *d = luma(px);
                }
            }
        }
        self.out.send(dst)
    }
}

fn luma(px: &[u8]) -> u8 {
    ((px[0] as u32 * 299 + px[1] as u32 * 587 + px[2] as u32 * 114) / 1000) as u8
}

/// Mean brightness of an image, from 0 (black) to 1 (white).
#[derive(Node)]
#[node(kind = "std/image-stats")]
pub struct ImageStats {
    #[input]
    image: Input<Image>,
    #[output]
    brightness: Output<Scalar>,
}

impl Node for ImageStats {
    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        let Some(sample) = self.image.latest() else {
            return Ok(());
        };
        let img: &Image = &sample;
        let ch = img.encoding.channels();
        let pixels = (img.data.len() / ch).max(1) as f64;
        let sum: u64 = match img.encoding {
            Encoding::Mono8 => img.data.iter().map(|&v| v as u64).sum(),
            _ => img.data.chunks_exact(ch).map(|px| luma(px) as u64).sum(),
        };
        self.brightness.send(Scalar(sum as f64 / pixels / 255.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blur(img: &Image, r: usize) -> Image {
        let mut out = img.clone();
        box_blur(img, &mut out.data, r, &mut Vec::new(), &mut Vec::new());
        out
    }

    /// Straightforward reference implementation.
    fn blur_naive(img: &Image, r: usize) -> Image {
        let (w, h, ch) = (
            img.width as isize,
            img.height as isize,
            img.encoding.channels(),
        );
        let mut out = img.clone();
        let n = ((2 * r + 1) * (2 * r + 1)) as u32;
        for y in 0..h {
            for x in 0..w {
                for c in 0..ch {
                    let mut sum = 0u32;
                    for dy in -(r as isize)..=r as isize {
                        for dx in -(r as isize)..=r as isize {
                            let sx = (x + dx).clamp(0, w - 1) as usize;
                            let sy = (y + dy).clamp(0, h - 1) as usize;
                            sum += img.data[(sy * w as usize + sx) * ch + c] as u32;
                        }
                    }
                    out.data[(y * w + x) as usize * ch + c] = (sum / n) as u8;
                }
            }
        }
        out
    }

    #[test]
    fn blur_matches_reference_within_rounding() {
        for enc in [Encoding::Mono8, Encoding::Rgb8, Encoding::Rgba8] {
            let mut img = Image::new(23, 11, enc);
            let mut seed = 12345u32;
            for b in &mut img.data {
                seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
                *b = (seed >> 16) as u8;
            }
            for r in [1, 2, 5] {
                let (fast, slow) = (blur(&img, r), blur_naive(&img, r));
                for (a, b) in fast.data.iter().zip(&slow.data) {
                    // Two rounding steps (rows then columns) may lose a little.
                    assert!(
                        (*a as i32 - *b as i32).abs() <= 2,
                        "{enc:?} r={r}: {a} vs {b}"
                    );
                }
            }
        }
    }

    #[test]
    fn blur_preserves_flat_images() {
        let mut img = Image::new(17, 9, Encoding::Rgb8);
        img.data.fill(123);
        assert_eq!(blur(&img, 3).data, img.data);
    }

    #[test]
    fn blur_spreads_a_spike() {
        let mut img = Image::new(9, 9, Encoding::Mono8);
        img.data[4 * 9 + 4] = 225;
        let out = blur(&img, 1);
        // 3x3 box: the spike is spread evenly over 9 pixels.
        assert_eq!(out.data[4 * 9 + 4], 25);
        assert_eq!(out.data[3 * 9 + 3], 25);
        assert_eq!(out.data[2 * 9 + 2], 0);
    }

    #[test]
    fn blur_radius_larger_than_image() {
        let mut img = Image::new(3, 2, Encoding::Rgb8);
        img.data.fill(10);
        assert_eq!(blur(&img, 20).data, img.data);
    }

    #[test]
    fn luma_extremes() {
        assert_eq!(luma(&[0, 0, 0]), 0);
        assert_eq!(luma(&[255, 255, 255]), 255);
    }
}
