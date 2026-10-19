//! RTP fixed header (RFC 3550).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtpPacket<'a> {
    pub marker: bool,
    pub payload_type: u8,
    pub sequence: u16,
    pub timestamp: u32,
    pub ssrc: u32,
    pub payload: &'a [u8],
}

pub fn parse(buf: &[u8]) -> Result<RtpPacket<'_>, String> {
    if buf.len() < 12 {
        return Err(format!("RTP packet of {} bytes is too short", buf.len()));
    }
    if buf[0] >> 6 != 2 {
        return Err(format!("RTP version {} is not 2", buf[0] >> 6));
    }
    let padding = buf[0] & 0x20 != 0;
    let extension = buf[0] & 0x10 != 0;
    let csrc = (buf[0] & 0x0F) as usize;
    let mut start = 12 + 4 * csrc;
    if extension {
        if buf.len() < start + 4 {
            return Err("truncated RTP header extension".into());
        }
        let words = u16::from_be_bytes([buf[start + 2], buf[start + 3]]) as usize;
        start += 4 + 4 * words;
    }
    let mut end = buf.len();
    if padding {
        let pad = *buf.last().unwrap() as usize;
        end = end.checked_sub(pad).ok_or("bad RTP padding")?;
    }
    if start > end {
        return Err("truncated RTP packet".into());
    }
    Ok(RtpPacket {
        marker: buf[1] & 0x80 != 0,
        payload_type: buf[1] & 0x7F,
        sequence: u16::from_be_bytes([buf[2], buf[3]]),
        timestamp: u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]),
        ssrc: u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]),
        payload: &buf[start..end],
    })
}

/// Builds an RTP packet; used by the test servers.
pub fn build(marker: bool, pt: u8, seq: u16, ts: u32, ssrc: u32, payload: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(12 + payload.len());
    p.push(0x80);
    p.push(pt | if marker { 0x80 } else { 0 });
    p.extend_from_slice(&seq.to_be_bytes());
    p.extend_from_slice(&ts.to_be_bytes());
    p.extend_from_slice(&ssrc.to_be_bytes());
    p.extend_from_slice(payload);
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_fields() {
        let p = build(true, 96, 65535, 90000, 0xDEADBEEF, &[1, 2, 3]);
        let r = parse(&p).unwrap();
        assert!(r.marker);
        assert_eq!(
            (r.payload_type, r.sequence, r.timestamp, r.ssrc),
            (96, 65535, 90000, 0xDEADBEEF)
        );
        assert_eq!(r.payload, [1, 2, 3]);

        // One CSRC, a one-word extension and two bytes of padding.
        let mut q = vec![0xB1, 26, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
        q.extend_from_slice(&[9, 9, 9, 9]); // CSRC
        q.extend_from_slice(&[0xBE, 0xDE, 0, 1, 7, 7, 7, 7]); // extension
        q.extend_from_slice(&[5, 6, 0, 2]); // payload + padding
        let r = parse(&q).unwrap();
        assert_eq!((r.payload_type, r.marker), (26, false));
        assert_eq!(r.payload, [5, 6]);

        assert!(parse(&[0x80; 8]).is_err());
        assert!(parse(&[0x40; 12]).is_err());
    }
}
