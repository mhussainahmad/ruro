//! Pixel format conversion for camera drivers: packed YUYV (YUY2) and
//! Motion-JPEG to RGB8.

use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

// BT.601 limited-range coefficients in 16.16 fixed point:
// R = 1.164 (Y-16) + 1.596 (V-128)
// G = 1.164 (Y-16) - 0.392 (U-128) - 0.813 (V-128)
// B = 1.164 (Y-16) + 2.017 (U-128)
const CY: i32 = 76284; // 1.164 * 65536
const CRV: i32 = 104595; // 1.596 * 65536
const CGU: i32 = 25690; // 0.392 * 65536
const CGV: i32 = 53281; // 0.813 * 65536
const CBU: i32 = 132186; // 2.017 * 65536

#[inline(always)]
fn clamp8(v: i32) -> u8 {
    (v >> 16).clamp(0, 255) as u8
}

/// Converts packed YUYV 4:2:2 (`Y0 U Y1 V` per pixel pair) to RGB8.
///
/// `stride` is the source row length in bytes (at least `2 * width`).
/// `rgb` must hold `width * height * 3` bytes. Odd widths are not supported
/// by the format.
pub fn yuyv_to_rgb(src: &[u8], width: usize, height: usize, stride: usize, rgb: &mut [u8]) {
    assert!(width.is_multiple_of(2), "YUYV width must be even");
    assert!(stride >= width * 2 && src.len() >= stride * (height - 1) + width * 2);
    assert!(rgb.len() >= width * height * 3);
    for y in 0..height {
        let row = &src[y * stride..y * stride + width * 2];
        let out = &mut rgb[y * width * 3..(y + 1) * width * 3];
        // Two pixels per iteration; fixed-size chunks let the compiler vectorise.
        for (s, d) in row
            .as_chunks::<4>()
            .0
            .iter()
            .zip(out.as_chunks_mut::<6>().0)
        {
            let u = s[1] as i32 - 128;
            let v = s[3] as i32 - 128;
            let r = CRV * v + 32768;
            let g = -CGU * u - CGV * v + 32768;
            let b = CBU * u + 32768;
            let y0 = CY * (s[0] as i32 - 16);
            let y1 = CY * (s[2] as i32 - 16);
            d[0] = clamp8(y0 + r);
            d[1] = clamp8(y0 + g);
            d[2] = clamp8(y0 + b);
            d[3] = clamp8(y1 + r);
            d[4] = clamp8(y1 + g);
            d[5] = clamp8(y1 + b);
        }
    }
}

/// The default Huffman tables of ITU-T T.81 Annex K.3, as one DHT segment.
/// UVC cameras commonly omit them from MJPEG frames.
pub(crate) const DEFAULT_DHT: [u8; 420] = {
    const BITS_DC_L: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
    const BITS_DC_C: [u8; 16] = [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0];
    const BITS_AC_L: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d];
    const BITS_AC_C: [u8; 16] = [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77];
    const VAL_DC: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
    const VAL_AC_L: [u8; 162] = [
        0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61,
        0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52,
        0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25,
        0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45,
        0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64,
        0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83,
        0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99,
        0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6,
        0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3,
        0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8,
        0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
    ];
    const VAL_AC_C: [u8; 162] = [
        0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61,
        0x71, 0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33,
        0x52, 0xf0, 0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18,
        0x19, 0x1a, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44,
        0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63,
        0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a,
        0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97,
        0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4,
        0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca,
        0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7,
        0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
    ];
    let mut out = [0u8; 420];
    out[0] = 0xFF;
    out[1] = 0xC4;
    out[2] = (418 >> 8) as u8;
    out[3] = (418 & 0xFF) as u8;
    let mut i = 4;
    // (class/id, bits, values)
    let tables: [(u8, &[u8; 16], &[u8]); 4] = [
        (0x00, &BITS_DC_L, &VAL_DC),
        (0x10, &BITS_AC_L, &VAL_AC_L),
        (0x01, &BITS_DC_C, &VAL_DC),
        (0x11, &BITS_AC_C, &VAL_AC_C),
    ];
    let mut t = 0;
    while t < 4 {
        let (tc, bits, vals) = tables[t];
        out[i] = tc;
        i += 1;
        let mut k = 0;
        while k < 16 {
            out[i] = bits[k];
            i += 1;
            k += 1;
        }
        let mut k = 0;
        while k < vals.len() {
            out[i] = vals[k];
            i += 1;
            k += 1;
        }
        t += 1;
    }
    assert!(i == 420);
    out
};

/// Returns `jpeg` with the standard Huffman tables inserted before the
/// start-of-scan marker if the stream has no DHT segment, otherwise `None`.
pub fn with_default_huffman_tables(jpeg: &[u8]) -> Option<Vec<u8>> {
    if jpeg.len() < 4 || jpeg[0] != 0xFF || jpeg[1] != 0xD8 {
        return None;
    }
    let mut i = 2;
    while i + 4 <= jpeg.len() {
        if jpeg[i] != 0xFF {
            return None;
        }
        match jpeg[i + 1] {
            0xC4 => return None, // has tables
            0xDA => {
                let mut out = Vec::with_capacity(jpeg.len() + DEFAULT_DHT.len());
                out.extend_from_slice(&jpeg[..i]);
                out.extend_from_slice(&DEFAULT_DHT);
                out.extend_from_slice(&jpeg[i..]);
                return Some(out);
            }
            0xFF => i += 1, // fill byte
            _ => {
                let len = u16::from_be_bytes([jpeg[i + 2], jpeg[i + 3]]) as usize;
                i += 2 + len;
            }
        }
    }
    None
}

/// Decodes a (Motion-)JPEG frame to RGB8, returning `(width, height, pixels)`.
pub fn decode_jpeg(jpeg: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let patched = with_default_huffman_tables(jpeg);
    let data = patched.as_deref().unwrap_or(jpeg);
    let opts = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGB)
        .set_strict_mode(false);
    let mut d = JpegDecoder::new_with_options(ZCursor::new(data), opts);
    let pixels = d
        .decode()
        .map_err(|e| format!("JPEG decode failed: {e:?}"))?;
    let info = d.info().ok_or("JPEG decoder returned no header info")?;
    Ok((info.width as u32, info.height as u32, pixels))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yuyv_pixel_pair(y0: u8, u: u8, y1: u8, v: u8) -> [u8; 6] {
        let mut out = [0u8; 6];
        yuyv_to_rgb(&[y0, u, y1, v], 2, 1, 4, &mut out);
        out
    }

    #[test]
    fn yuyv_reference_colours() {
        // Limited-range black and white.
        assert_eq!(yuyv_pixel_pair(16, 128, 235, 128), [0, 0, 0, 255, 255, 255]);
        // BT.601 75% colour bars (Y, Cb, Cr) -> RGB 191.
        let check = |y, u, v, rgb: [u8; 3]| {
            let p = yuyv_pixel_pair(y, u, y, v);
            for c in 0..3 {
                assert!(
                    (p[c] as i32 - rgb[c] as i32).abs() <= 1,
                    "{y},{u},{v}: {p:?} vs {rgb:?}"
                );
            }
        };
        check(162, 44, 142, [191, 191, 0]); // yellow
        check(131, 156, 44, [0, 191, 191]); // cyan
        check(112, 72, 58, [0, 191, 0]); // green
        check(84, 184, 198, [191, 0, 191]); // magenta
        check(65, 100, 212, [191, 0, 0]); // red
        check(35, 212, 114, [0, 0, 191]); // blue
        // Out-of-range luma saturates instead of wrapping.
        assert_eq!(yuyv_pixel_pair(255, 128, 0, 128), [255, 255, 255, 0, 0, 0]);
        assert_eq!(yuyv_pixel_pair(255, 255, 255, 255)[..3], [255, 125, 255]);
    }

    #[test]
    fn yuyv_honours_stride() {
        // 2x2 image with 4 bytes of padding per row.
        let src = [16, 128, 16, 128, 9, 9, 9, 9, 235, 128, 235, 128, 9, 9, 9, 9];
        let mut out = [0u8; 12];
        yuyv_to_rgb(&src, 2, 2, 8, &mut out);
        assert_eq!(out, [0, 0, 0, 0, 0, 0, 255, 255, 255, 255, 255, 255]);
    }

    fn test_jpeg() -> Vec<u8> {
        // 16x8: left half red, right half blue.
        let mut rgb = Vec::new();
        for _y in 0..8 {
            for x in 0..16 {
                rgb.extend(if x < 8 { [255, 0, 0] } else { [0, 0, 255] });
            }
        }
        let mut out = Vec::new();
        jpeg_encoder::Encoder::new(&mut out, 95)
            .encode(&rgb, 16, 8, jpeg_encoder::ColorType::Rgb)
            .unwrap();
        out
    }

    fn strip_dht(jpeg: &[u8]) -> Vec<u8> {
        let mut out = jpeg[..2].to_vec();
        let mut i = 2;
        loop {
            let marker = jpeg[i + 1];
            if marker == 0xDA {
                out.extend_from_slice(&jpeg[i..]);
                return out;
            }
            let len = u16::from_be_bytes([jpeg[i + 2], jpeg[i + 3]]) as usize;
            if marker != 0xC4 {
                out.extend_from_slice(&jpeg[i..i + 2 + len]);
            }
            i += 2 + len;
        }
    }

    #[test]
    fn decodes_jpeg_with_and_without_huffman_tables() {
        let jpeg = test_jpeg();
        assert!(with_default_huffman_tables(&jpeg).is_none());
        let (w, h, px) = decode_jpeg(&jpeg).unwrap();
        assert_eq!((w, h, px.len()), (16, 8, 16 * 8 * 3));
        assert!(px[0] > 200 && px[2] < 60, "left is red: {:?}", &px[..3]);
        let right = &px[15 * 3..16 * 3];
        assert!(right[2] > 200 && right[0] < 60, "right is blue: {right:?}");

        // The encoder writes the standard Annex K tables, so a stripped stream
        // decodes identically once they are put back.
        let bare = strip_dht(&jpeg);
        assert!(bare.len() < jpeg.len());
        assert!(with_default_huffman_tables(&bare).is_some());
        let (_, _, px2) = decode_jpeg(&bare).unwrap();
        assert_eq!(px, px2);
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode_jpeg(&[0xFF, 0xD8, 0x00]).is_err());
        assert!(with_default_huffman_tables(b"not a jpeg").is_none());
    }
}
