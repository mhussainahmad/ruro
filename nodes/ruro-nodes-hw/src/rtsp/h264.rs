//! H.264 over RTP (RFC 6184, packetization modes 0 and 1): single NAL unit
//! packets, STAP-A and FU-A, reassembled into Annex B access units. The
//! decoder (feature `h264`) wraps OpenH264.

use super::rtp::RtpPacket;

pub const NAL_IDR: u8 = 5;
pub const NAL_SPS: u8 = 7;
pub const NAL_PPS: u8 = 8;
const STAP_A: u8 = 24;
const FU_A: u8 = 28;
const START_CODE: [u8; 4] = [0, 0, 0, 1];

/// One complete frame as an Annex B byte stream (start code before every
/// NAL unit), decodable on its own if `keyframe`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessUnit {
    pub data: Vec<u8>,
    pub keyframe: bool,
    /// RTP timestamp (90 kHz).
    pub timestamp: u32,
}

/// Splits an Annex B stream into NAL units (without start codes).
pub fn split_annex_b(stream: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= stream.len() {
        if stream[i] == 0 && stream[i + 1] == 0 && stream[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(k, &s)| {
            let mut end = starts.get(k + 1).map_or(stream.len(), |&n| n - 3);
            // A four-byte start code leaves a trailing zero on the previous unit.
            while end > s && k + 1 < starts.len() && stream[end - 1] == 0 {
                end -= 1;
            }
            &stream[s..end]
        })
        .filter(|n| !n.is_empty())
        .collect()
}

pub fn nal_type(nal: &[u8]) -> u8 {
    nal.first().map_or(0, |b| b & 0x1F)
}

/// Reassembles RTP payloads into access units.
///
/// Frames are cut at the marker bit or a timestamp change. After packet
/// loss, frames are dropped until the next keyframe, so neither the decoder
/// nor a recording sees a corrupted stream; the same applies at start-up.
pub struct Depacketizer {
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    nals: Vec<Vec<u8>>,
    timestamp: Option<u32>,
    fu: Option<Vec<u8>>,
    last_seq: Option<u16>,
    /// The current frame lost data.
    broken: bool,
    need_keyframe: bool,
    /// Frames dropped because of loss or a missing keyframe.
    pub dropped: u64,
}

impl Depacketizer {
    /// `parameter_sets` are the SPS/PPS from the SDP, if any.
    pub fn new(parameter_sets: &[Vec<u8>]) -> Self {
        let mut d = Self {
            sps: None,
            pps: None,
            nals: Vec::new(),
            timestamp: None,
            fu: None,
            last_seq: None,
            broken: false,
            need_keyframe: true,
            dropped: 0,
        };
        for ps in parameter_sets {
            d.remember(ps);
        }
        d
    }

    fn remember(&mut self, nal: &[u8]) {
        match nal_type(nal) {
            NAL_SPS => self.sps = Some(nal.to_vec()),
            NAL_PPS => self.pps = Some(nal.to_vec()),
            _ => {}
        }
    }

    fn add_nal(&mut self, nal: Vec<u8>) {
        if nal.is_empty() {
            return;
        }
        self.remember(&nal);
        self.nals.push(nal);
    }

    /// Finishes the current frame.
    fn flush(&mut self, out: &mut Vec<AccessUnit>) {
        let nals = std::mem::take(&mut self.nals);
        let broken = std::mem::replace(&mut self.broken, false);
        self.fu = None;
        let Some(timestamp) = self.timestamp.take() else {
            return;
        };
        let keyframe = nals.iter().any(|n| nal_type(n) == NAL_IDR);
        if broken || (self.need_keyframe && !keyframe && !nals.is_empty()) {
            self.dropped += 1;
            self.need_keyframe = true;
            return;
        }
        if nals.is_empty() {
            return;
        }
        let has = |t: u8| nals.iter().any(|n| nal_type(n) == t);
        let mut data = Vec::new();
        let mut inserted = false;
        for n in &nals {
            // Repeat missing parameter sets before the first IDR slice, so
            // that every keyframe (and a recording cut at one) decodes on
            // its own.
            if nal_type(n) == NAL_IDR && !inserted {
                inserted = true;
                for (t, ps) in [(NAL_SPS, &self.sps), (NAL_PPS, &self.pps)] {
                    if let Some(ps) = ps.as_ref().filter(|_| !has(t)) {
                        data.extend_from_slice(&START_CODE);
                        data.extend_from_slice(ps);
                    }
                }
            }
            data.extend_from_slice(&START_CODE);
            data.extend_from_slice(n);
        }
        if keyframe {
            self.need_keyframe = false;
        }
        out.push(AccessUnit {
            data,
            keyframe,
            timestamp,
        });
    }

    /// Feeds one packet; completed frames are appended to `out`.
    pub fn push(&mut self, pkt: &RtpPacket, out: &mut Vec<AccessUnit>) {
        if let Some(last) = self.last_seq
            && pkt.sequence != last.wrapping_add(1)
        {
            self.broken = true;
            self.fu = None;
        }
        self.last_seq = Some(pkt.sequence);
        if self.timestamp.is_some_and(|t| t != pkt.timestamp) {
            self.flush(out);
        }
        self.timestamp = Some(pkt.timestamp);
        let p = pkt.payload;
        match nal_type(p) {
            0 => self.broken = true,
            1..=23 => self.add_nal(p.to_vec()),
            STAP_A => {
                let mut rest = &p[1..];
                while rest.len() >= 2 {
                    let n = u16::from_be_bytes([rest[0], rest[1]]) as usize;
                    if rest.len() < 2 + n {
                        self.broken = true;
                        break;
                    }
                    self.add_nal(rest[2..2 + n].to_vec());
                    rest = &rest[2 + n..];
                }
            }
            FU_A if p.len() >= 2 => {
                let (start, end) = (p[1] & 0x80 != 0, p[1] & 0x40 != 0);
                if start {
                    let mut nal = vec![(p[0] & 0xE0) | (p[1] & 0x1F)];
                    nal.extend_from_slice(&p[2..]);
                    self.fu = Some(nal);
                } else if let Some(fu) = self.fu.as_mut() {
                    fu.extend_from_slice(&p[2..]);
                } else {
                    self.broken = true;
                }
                if end && let Some(nal) = self.fu.take() {
                    self.add_nal(nal);
                }
            }
            // STAP-B, MTAP and FU-B belong to the interleaved mode.
            _ => self.broken = true,
        }
        if pkt.marker {
            self.flush(out);
        }
    }
}

/// Packetizes the NAL units of one frame (mode 1); used by test servers.
/// Units up to `mtu` bytes go out whole, larger ones as FU-A fragments, and
/// with `aggregate` consecutive small units share a STAP-A packet.
pub fn packetize(nals: &[&[u8]], mtu: usize, aggregate: bool) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut stap: Vec<u8> = Vec::new();
    let flush_stap = |stap: &mut Vec<u8>, out: &mut Vec<Vec<u8>>| {
        if !stap.is_empty() {
            out.push(std::mem::take(stap));
        }
    };
    for nal in nals {
        if nal.len() > mtu {
            flush_stap(&mut stap, &mut out);
            let (hdr, body) = (nal[0], &nal[1..]);
            let chunks: Vec<&[u8]> = body.chunks(mtu - 2).collect();
            for (i, c) in chunks.iter().enumerate() {
                let mut fu = vec![(hdr & 0xE0) | FU_A, hdr & 0x1F];
                if i == 0 {
                    fu[1] |= 0x80;
                }
                if i + 1 == chunks.len() {
                    fu[1] |= 0x40;
                }
                fu.extend_from_slice(c);
                out.push(fu);
            }
        } else if aggregate {
            if !stap.is_empty() && stap.len() + 2 + nal.len() > mtu {
                flush_stap(&mut stap, &mut out);
            }
            if stap.is_empty() {
                stap.push(STAP_A | (nal[0] & 0x60));
            }
            stap.extend_from_slice(&(nal.len() as u16).to_be_bytes());
            stap.extend_from_slice(nal);
        } else {
            out.push(nal.to_vec());
        }
    }
    flush_stap(&mut stap, &mut out);
    out
}

/// H.264 to RGB8 through OpenH264.
#[cfg(feature = "h264")]
pub struct Decoder {
    inner: openh264::decoder::Decoder,
}

#[cfg(feature = "h264")]
impl Decoder {
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            inner: openh264::decoder::Decoder::new().map_err(|e| e.to_string())?,
        })
    }

    /// Decodes one Annex B access unit; `None` while the decoder buffers.
    pub fn decode(&mut self, au: &[u8]) -> Result<Option<ruro::msg::Image>, String> {
        use openh264::formats::YUVSource;
        let Some(yuv) = self.inner.decode(au).map_err(|e| e.to_string())? else {
            return Ok(None);
        };
        let (w, h) = yuv.dimensions();
        let mut img = ruro::msg::Image::new(w as u32, h as u32, ruro::msg::Encoding::Rgb8);
        yuv.write_rgb8(&mut img.data);
        Ok(Some(img))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtsp::rtp;

    const SPS: &[u8] = &[0x67, 0x42, 0xC0, 0x1F, 0xDA];
    const PPS: &[u8] = &[0x68, 0xCE, 0x0F, 0xC8];

    fn nal(t: u8, len: usize) -> Vec<u8> {
        let mut n = vec![0x60 | t];
        n.extend((0..len - 1).map(|i| (i % 251) as u8 + 1));
        n
    }

    /// Feeds packets with consecutive sequence numbers, one frame per call.
    struct Feed {
        d: Depacketizer,
        seq: u16,
        ts: u32,
        out: Vec<AccessUnit>,
    }

    impl Feed {
        fn new(ps: &[Vec<u8>]) -> Self {
            Self {
                d: Depacketizer::new(ps),
                seq: 100,
                ts: 0,
                out: Vec::new(),
            }
        }

        fn frame(&mut self, payloads: &[Vec<u8>], skip: Option<usize>) {
            self.ts += 3000;
            for (i, p) in payloads.iter().enumerate() {
                self.seq = self.seq.wrapping_add(1);
                if skip == Some(i) {
                    continue;
                }
                let pkt = rtp::build(i + 1 == payloads.len(), 96, self.seq, self.ts, 1, p);
                self.d.push(&rtp::parse(&pkt).unwrap(), &mut self.out);
            }
        }
    }

    fn annex_b(nals: &[&[u8]]) -> Vec<u8> {
        nals.iter()
            .flat_map(|n| [&START_CODE[..], n].concat())
            .collect()
    }

    #[test]
    fn single_stap_and_fu_round_trip() {
        let idr = nal(NAL_IDR, 500);
        let p = nal(1, 40);
        let mut f = Feed::new(&[SPS.to_vec(), PPS.to_vec()]);
        // Keyframe: SPS and PPS aggregated, IDR fragmented.
        let pk = packetize(&[SPS, PPS, &idr], 100, true);
        assert_eq!(pk[0][0] & 0x1F, STAP_A);
        assert!(pk[1..].iter().all(|p| p[0] & 0x1F == FU_A));
        f.frame(&pk, None);
        f.frame(&packetize(&[&p], 100, false), None);
        assert_eq!(f.out.len(), 2);
        assert!(f.out[0].keyframe && !f.out[1].keyframe);
        assert_eq!(f.out[0].data, annex_b(&[SPS, PPS, &idr]));
        assert_eq!(f.out[1].data, annex_b(&[&p]));
        assert_eq!(split_annex_b(&f.out[0].data), [SPS, PPS, &idr[..]]);
    }

    #[test]
    fn parameter_sets_from_sdp_precede_keyframes() {
        let idr = nal(NAL_IDR, 60);
        let mut f = Feed::new(&[SPS.to_vec(), PPS.to_vec()]);
        f.frame(&packetize(&[&idr], 1400, false), None);
        assert_eq!(f.out[0].data, annex_b(&[SPS, PPS, &idr]));
        // In-band parameter sets replace the SDP ones.
        let sps2 = [0x67, 1, 2, 3];
        f.frame(&packetize(&[&sps2, &idr], 1400, false), None);
        assert_eq!(f.out[1].data, annex_b(&[&sps2, PPS, &idr]));
        f.frame(&packetize(&[&idr], 1400, false), None);
        assert_eq!(f.out[2].data, annex_b(&[&sps2, PPS, &idr]));
    }

    #[test]
    fn waits_for_a_keyframe_and_drops_after_loss() {
        let idr = nal(NAL_IDR, 300);
        let p = nal(1, 300);
        let mut f = Feed::new(&[]);
        f.frame(&packetize(&[&p], 100, false), None); // no keyframe yet
        assert!(f.out.is_empty());
        f.frame(&packetize(&[&idr], 100, false), None);
        f.frame(&packetize(&[&p], 100, false), Some(1)); // middle fragment lost
        f.frame(&packetize(&[&p], 100, false), None); // refers to the lost frame
        assert_eq!(f.out.len(), 1);
        f.frame(&packetize(&[&idr], 100, false), None);
        f.frame(&packetize(&[&p], 100, false), None);
        assert_eq!(f.out.len(), 3);
        assert_eq!(f.d.dropped, 3);
    }

    #[test]
    fn timestamp_change_ends_a_frame_without_marker() {
        let mut d = Depacketizer::new(&[]);
        let mut out = Vec::new();
        let idr = nal(NAL_IDR, 10);
        let pkt = rtp::build(false, 96, 1, 3000, 1, &idr);
        d.push(&rtp::parse(&pkt).unwrap(), &mut out);
        assert!(out.is_empty());
        let pkt = rtp::build(false, 96, 2, 6000, 1, &nal(1, 10));
        d.push(&rtp::parse(&pkt).unwrap(), &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].timestamp, 3000);
    }

    #[test]
    fn annex_b_with_three_and_four_byte_start_codes() {
        let s = [0, 0, 0, 1, 0x67, 1, 0, 0, 1, 0x68, 2, 0, 0, 0, 1, 0x65, 3];
        assert_eq!(split_annex_b(&s), [&[0x67, 1][..], &[0x68, 2], &[0x65, 3]]);
    }
}
