//! Motion JPEG over RTP (RFC 2435). Packets carry only the entropy-coded
//! scan; the JPEG headers are rebuilt from the RTP JPEG header: type 0/1
//! (4:2:2 / 4:2:0, 64/65 with restart markers), quantisation tables in band
//! (Q 128-255) or scaled from the standard tables (Q 1-99), and the standard
//! Huffman tables.

use super::rtp::RtpPacket;
use crate::pixel::DEFAULT_DHT;

/// Zig-zag scan order: natural index of the i-th coefficient.
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// ITU-T T.81 Tables K.1 and K.2 in natural order.
const LUMA_Q: [u8; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56,
    14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113,
    92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
];
const CHROMA_Q: [u8; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99, 99,
    47, 66, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
];

/// Luma and chroma tables in zig-zag order for Q 1-99 (RFC 2435 section 4.2,
/// the IJG scaling).
pub fn scaled_tables(q: u8) -> [[u8; 64]; 2] {
    let factor = q.clamp(1, 99) as u32;
    let s = if factor < 50 {
        5000 / factor
    } else {
        200 - factor * 2
    };
    let scale = |t: &[u8; 64]| -> [u8; 64] {
        std::array::from_fn(|i| ((t[ZIGZAG[i]] as u32 * s + 50) / 100).clamp(1, 255) as u8)
    };
    [scale(&LUMA_Q), scale(&CHROMA_Q)]
}

/// Builds the JPEG headers for a frame (SOI through SOS).
pub fn headers(kind: u8, width: u16, height: u16, tables: &[[u8; 64]], dri: u16) -> Vec<u8> {
    let mut h = vec![0xFF, 0xD8];
    for (i, t) in tables.iter().enumerate() {
        h.extend_from_slice(&[0xFF, 0xDB, 0, 67, i as u8]);
        h.extend_from_slice(t);
    }
    if dri != 0 {
        h.extend_from_slice(&[0xFF, 0xDD, 0, 4]);
        h.extend_from_slice(&dri.to_be_bytes());
    }
    let chroma_table = if tables.len() > 1 { 1 } else { 0 };
    h.extend_from_slice(&[0xFF, 0xC0, 0, 17, 8]);
    h.extend_from_slice(&height.to_be_bytes());
    h.extend_from_slice(&width.to_be_bytes());
    let luma_sampling = if kind & 0x3F == 0 { 0x21 } else { 0x22 };
    h.extend_from_slice(&[
        3,
        1,
        luma_sampling,
        0,
        2,
        0x11,
        chroma_table,
        3,
        0x11,
        chroma_table,
    ]);
    h.extend_from_slice(&DEFAULT_DHT);
    h.extend_from_slice(&[0xFF, 0xDA, 0, 12, 3, 1, 0x00, 2, 0x11, 3, 0x11, 0, 63, 0]);
    h
}

/// The frame parameters carried in every packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrameHeader {
    kind: u8,
    q: u8,
    width: u16,
    height: u16,
    dri: u16,
}

/// A complete JPEG file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JpegFrame {
    pub data: Vec<u8>,
    pub timestamp: u32,
}

#[derive(Default)]
pub struct Depacketizer {
    scan: Vec<u8>,
    header: Option<FrameHeader>,
    tables: Vec<[u8; 64]>,
    /// In-band tables by Q, for senders that send them once (length 0 later).
    cached: Vec<(u8, Vec<[u8; 64]>)>,
    timestamp: Option<u32>,
    last_seq: Option<u16>,
    broken: bool,
    pub dropped: u64,
    /// Why the last frame was dropped, for logging.
    pub last_error: Option<String>,
}

impl Depacketizer {
    pub fn new() -> Self {
        Self::default()
    }

    fn reset(&mut self) {
        self.scan.clear();
        self.header = None;
        self.broken = false;
        self.timestamp = None;
    }

    fn fail(&mut self, why: String) {
        self.broken = true;
        self.last_error = Some(why);
    }

    fn finish(&mut self, out: &mut Vec<JpegFrame>) {
        if let (Some(h), Some(ts), false) = (self.header, self.timestamp, self.broken)
            && !self.scan.is_empty()
            && !self.tables.is_empty()
        {
            let mut data = headers(h.kind, h.width, h.height, &self.tables, h.dri);
            data.extend_from_slice(&self.scan);
            if !self.scan.ends_with(&[0xFF, 0xD9]) {
                data.extend_from_slice(&[0xFF, 0xD9]);
            }
            out.push(JpegFrame {
                data,
                timestamp: ts,
            });
        } else if self.header.is_some() || self.broken {
            self.dropped += 1;
        }
        self.reset();
    }

    pub fn push(&mut self, pkt: &RtpPacket, out: &mut Vec<JpegFrame>) {
        if let Some(last) = self.last_seq
            && pkt.sequence != last.wrapping_add(1)
        {
            self.fail("packet loss".into());
        }
        self.last_seq = Some(pkt.sequence);
        if self.timestamp.is_some_and(|t| t != pkt.timestamp) {
            self.finish(out);
        }
        self.timestamp = Some(pkt.timestamp);
        if let Err(e) = self.parse(pkt.payload) {
            self.fail(e);
        }
        if pkt.marker {
            self.finish(out);
        }
    }

    fn parse(&mut self, p: &[u8]) -> Result<(), String> {
        if p.len() < 8 {
            return Err("short RTP JPEG header".into());
        }
        let offset = u32::from_be_bytes([0, p[1], p[2], p[3]]) as usize;
        let (kind, q) = (p[4], p[5]);
        let (width, height) = (p[6] as u16 * 8, p[7] as u16 * 8);
        let mut rest = &p[8..];
        if !matches!(kind, 0 | 1 | 64 | 65) {
            return Err(format!("unsupported RTP JPEG type {kind}"));
        }
        if width == 0 || height == 0 {
            return Err("frame larger than 2040 pixels".into());
        }
        let mut dri = 0;
        if kind >= 64 {
            if rest.len() < 4 {
                return Err("short restart marker header".into());
            }
            dri = u16::from_be_bytes([rest[0], rest[1]]);
            rest = &rest[4..];
        }
        if offset == 0 {
            self.scan.clear();
            self.broken = false;
            if q >= 128 {
                if rest.len() < 4 {
                    return Err("short quantization table header".into());
                }
                let (precision, len) = (rest[1], u16::from_be_bytes([rest[2], rest[3]]) as usize);
                rest = &rest[4..];
                if len == 0 {
                    self.tables = self
                        .cached
                        .iter()
                        .find(|(cq, _)| *cq == q)
                        .map(|(_, t)| t.clone())
                        .ok_or_else(|| format!("no quantization tables for Q {q} yet"))?;
                } else {
                    if precision != 0 {
                        return Err("16-bit quantization tables are not supported".into());
                    }
                    if rest.len() < len || len % 64 != 0 {
                        return Err(format!("bad quantization table length {len}"));
                    }
                    self.tables = rest[..len]
                        .as_chunks::<64>()
                        .0
                        .iter()
                        .take(2)
                        .copied()
                        .collect();
                    rest = &rest[len..];
                    self.cached.retain(|(cq, _)| *cq != q);
                    self.cached.push((q, self.tables.clone()));
                }
            } else {
                self.tables = scaled_tables(q).to_vec();
            }
            self.header = Some(FrameHeader {
                kind,
                q,
                width,
                height,
                dri,
            });
        } else if self.header.is_none() {
            return Err("frame started mid-way".into());
        }
        if offset != self.scan.len() {
            return Err(format!(
                "fragment offset {offset}, expected {}",
                self.scan.len()
            ));
        }
        self.scan.extend_from_slice(rest);
        Ok(())
    }
}

/// Splits a baseline JPEG file into RFC 2435 payloads with in-band tables
/// (Q 255); used by test servers. Only files this depacketizer can rebuild
/// are accepted: three components, 4:2:2 or 4:2:0, standard Huffman tables.
pub fn packetize(jpeg: &[u8], mtu: usize) -> Result<Vec<Vec<u8>>, String> {
    let mut tables: Vec<[u8; 64]> = Vec::new();
    let (mut kind, mut width, mut height) = (None, 0u16, 0u16);
    let mut i = 2;
    let scan = loop {
        if i + 4 > jpeg.len() || jpeg[i] != 0xFF {
            return Err("malformed JPEG".into());
        }
        let marker = jpeg[i + 1];
        let len = u16::from_be_bytes([jpeg[i + 2], jpeg[i + 3]]) as usize;
        let seg = &jpeg[i + 4..i + 2 + len];
        match marker {
            0xDB => {
                for t in seg.chunks(65) {
                    if t[0] >> 4 != 0 {
                        return Err("16-bit quantization table".into());
                    }
                    tables.push(t[1..65].try_into().unwrap());
                }
            }
            0xC0 => {
                height = u16::from_be_bytes([seg[1], seg[2]]);
                width = u16::from_be_bytes([seg[3], seg[4]]);
                kind = match (seg[5], seg[7]) {
                    (3, 0x21) => Some(0u8),
                    (3, 0x22) => Some(1),
                    _ => return Err("only 4:2:2 or 4:2:0 YCbCr is supported".into()),
                };
            }
            0xC1..=0xCF if marker != 0xC4 && marker != 0xC8 && marker != 0xCC => {
                return Err("not a baseline JPEG".into());
            }
            0xDA => break &jpeg[i + 2 + len..],
            _ => {}
        }
        i += 2 + len;
    };
    let kind = kind.ok_or("no SOF0 segment")?;
    if width % 8 != 0 || height % 8 != 0 || width > 2040 || height > 2040 {
        return Err(format!("{width}x{height} cannot be sent in RFC 2435"));
    }
    let scan = scan.strip_suffix(&[0xFF, 0xD9]).unwrap_or(scan);
    let mut qt = vec![0, 0];
    qt.extend_from_slice(&((tables.len() * 64) as u16).to_be_bytes());
    tables.iter().for_each(|t| qt.extend_from_slice(t));
    let mut out = Vec::new();
    let mut offset = 0;
    while offset < scan.len() {
        let mut p = vec![0];
        p.extend_from_slice(&(offset as u32).to_be_bytes()[1..]);
        p.extend_from_slice(&[kind, 255, (width / 8) as u8, (height / 8) as u8]);
        if offset == 0 {
            p.extend_from_slice(&qt);
        }
        let n = (mtu.saturating_sub(p.len()))
            .max(1)
            .min(scan.len() - offset);
        p.extend_from_slice(&scan[offset..offset + n]);
        offset += n;
        out.push(p);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pixel::decode_jpeg;
    use crate::rtsp::rtp;

    fn encode(w: u16, h: u16, sampling: jpeg_encoder::SamplingFactor) -> Vec<u8> {
        let rgb: Vec<u8> = (0..w as usize * h as usize)
            .flat_map(|i| [(i % w as usize * 7) as u8, (i / w as usize * 5) as u8, 128])
            .collect();
        let mut out = Vec::new();
        let mut e = jpeg_encoder::Encoder::new(&mut out, 80);
        e.set_sampling_factor(sampling);
        e.encode(&rgb, w, h, jpeg_encoder::ColorType::Rgb).unwrap();
        out
    }

    fn depacketize(payloads: &[Vec<u8>], seq0: u16) -> (Vec<JpegFrame>, Depacketizer) {
        let mut d = Depacketizer::new();
        let mut out = Vec::new();
        for (i, p) in payloads.iter().enumerate() {
            let last = i + 1 == payloads.len();
            let pkt = rtp::build(last, 26, seq0.wrapping_add(i as u16), 1234, 7, p);
            d.push(&rtp::parse(&pkt).unwrap(), &mut out);
        }
        (out, d)
    }

    #[test]
    fn rebuilt_frames_decode_like_the_original() {
        use jpeg_encoder::SamplingFactor::{F_2_1, F_2_2};
        for (sampling, kind) in [(F_2_1, 0), (F_2_2, 1)] {
            let jpeg = encode(48, 32, sampling);
            let payloads = packetize(&jpeg, 200).unwrap();
            assert!(payloads.len() > 1);
            assert_eq!(payloads[0][4], kind);
            let (frames, _) = depacketize(&payloads, 65530);
            assert_eq!(frames.len(), 1);
            assert_eq!(frames[0].timestamp, 1234);
            assert_eq!(decode_jpeg(&frames[0].data), decode_jpeg(&jpeg));
        }
    }

    #[test]
    fn losses_and_bad_offsets_drop_the_frame() {
        let jpeg = encode(48, 32, jpeg_encoder::SamplingFactor::F_2_2);
        let mut payloads = packetize(&jpeg, 120).unwrap();
        payloads.remove(1);
        // Sequence numbers stay consecutive, so the offset check catches it.
        let (frames, d) = depacketize(&payloads, 0);
        assert!(frames.is_empty());
        assert_eq!(d.dropped, 1);
        assert!(d.last_error.unwrap().contains("offset"));
    }

    #[test]
    fn standard_tables_scale_with_q() {
        let [l50, c50] = scaled_tables(50);
        // Q 50 is the unscaled table, in zig-zag order.
        assert_eq!(&l50[..8], &[16, 11, 12, 14, 12, 10, 16, 14]);
        assert_eq!(&c50[..6], &[17, 18, 18, 24, 21, 24]);
        let [l25, _] = scaled_tables(25); // scale 200%
        assert_eq!(l25[0], 32);
        let [l90, _] = scaled_tables(90); // scale 20%
        assert_eq!(l90[0], 3);
        assert!(scaled_tables(99)[0].iter().all(|&v| v >= 1));

        // A Q 50 frame without tables rebuilds with them.
        let mut p = vec![0, 0, 0, 0, 1, 50, 2, 2];
        p.extend_from_slice(&[0x12, 0x34]);
        let (frames, _) = depacketize(&[p], 0);
        let h = headers(1, 16, 16, &scaled_tables(50), 0);
        assert_eq!(frames[0].data[..h.len()], h[..]);
        assert!(frames[0].data.ends_with(&[0x12, 0x34, 0xFF, 0xD9]));
    }
}
