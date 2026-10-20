//! The subset of SDP (RFC 8866) an RTSP client needs: video media sections
//! with their payload type, codec, control URL and format parameters.

use crate::base64;

/// Video codecs the RTSP camera can depacketize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    /// H.264, RFC 6184.
    H264,
    /// Motion JPEG, RFC 2435.
    Jpeg,
    /// H.265 is recognised so the error message can say so.
    H265,
    Other,
}

/// One `m=video` section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Media {
    pub payload_type: u8,
    pub codec: Codec,
    /// Encoding name from `a=rtpmap`, e.g. `H264`.
    pub encoding: String,
    pub clock_rate: u32,
    /// `a=control`, possibly relative.
    pub control: Option<String>,
    /// `a=fmtp` parameters for this payload type.
    pub fmtp: Vec<(String, String)>,
}

impl Media {
    pub fn fmtp(&self, key: &str) -> Option<&str> {
        self.fmtp
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }

    /// H.264 parameter sets from `sprop-parameter-sets` (NAL units without
    /// start codes).
    pub fn sprop_parameter_sets(&self) -> Result<Vec<Vec<u8>>, String> {
        let Some(s) = self.fmtp("sprop-parameter-sets") else {
            return Ok(Vec::new());
        };
        s.split(',')
            .filter(|p| !p.trim().is_empty())
            .map(|p| base64::decode(p.trim()))
            .collect()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sdp {
    /// Session-level `a=control`.
    pub control: Option<String>,
    pub video: Vec<Media>,
}

pub fn parse(text: &str) -> Result<Sdp, String> {
    let mut sdp = Sdp::default();
    // Indices into `video` of the current media section (one per payload
    // type it lists), or `None` at session level or inside a non-video
    // section.
    let mut section: Option<Option<std::ops::Range<usize>>> = None;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        let Some((kind, value)) = line.split_once('=') else {
            continue;
        };
        match kind {
            "m" => {
                let mut parts = value.split_whitespace();
                let media = parts.next().unwrap_or("");
                let formats: Vec<u8> = parts.skip(2).filter_map(|p| p.parse().ok()).collect();
                section = Some(match media {
                    "video" if !formats.is_empty() => {
                        let start = sdp.video.len();
                        for pt in formats {
                            sdp.video.push(Media {
                                payload_type: pt,
                                // Static payload type 26 is JPEG without an rtpmap.
                                codec: if pt == 26 { Codec::Jpeg } else { Codec::Other },
                                encoding: if pt == 26 {
                                    "JPEG".into()
                                } else {
                                    String::new()
                                },
                                clock_rate: 90000,
                                control: None,
                                fmtp: Vec::new(),
                            });
                        }
                        Some(start..sdp.video.len())
                    }
                    _ => None,
                });
            }
            "a" => {
                let (attr, val) = value.split_once(':').unwrap_or((value, ""));
                match &section {
                    None if attr == "control" => sdp.control = Some(val.trim().to_owned()),
                    Some(Some(range)) => {
                        let media = &mut sdp.video[range.clone()];
                        if attr == "control" {
                            for m in media {
                                m.control = Some(val.trim().to_owned());
                            }
                            continue;
                        }
                        let (pt, rest) = val.split_once(' ').unwrap_or((val, ""));
                        let Some(m) = media
                            .iter_mut()
                            .find(|m| pt.trim().parse() == Ok(m.payload_type))
                        else {
                            continue;
                        };
                        match attr {
                            "rtpmap" => {
                                let mut e = rest.trim().split('/');
                                m.encoding = e.next().unwrap_or("").to_owned();
                                m.clock_rate =
                                    e.next().and_then(|r| r.parse().ok()).unwrap_or(90000);
                                m.codec = match m.encoding.to_ascii_uppercase().as_str() {
                                    "H264" => Codec::H264,
                                    "JPEG" => Codec::Jpeg,
                                    "H265" | "HEVC" => Codec::H265,
                                    _ => Codec::Other,
                                };
                            }
                            "fmtp" => {
                                m.fmtp = rest
                                    .split(';')
                                    .filter_map(|p| {
                                        let (k, v) = p.split_once('=')?;
                                        Some((k.trim().to_owned(), v.trim().to_owned()))
                                    })
                                    .collect();
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    Ok(sdp)
}

/// Resolves a control attribute against the base URL (RFC 2326 C.1.1).
pub fn resolve_control(base: &str, control: Option<&str>) -> String {
    match control.map(str::trim) {
        None | Some("") | Some("*") => base.to_owned(),
        Some(c) if c.contains("://") => c.to_owned(),
        // An absolute path keeps only the base's scheme and authority.
        Some(c) if c.starts_with('/') => {
            let authority_end = base
                .find("://")
                .and_then(|i| base[i + 3..].find('/').map(|j| i + 3 + j))
                .unwrap_or(base.len());
            format!("{}{c}", &base[..authority_end])
        }
        Some(c) => {
            if base.ends_with('/') {
                format!("{base}{c}")
            } else {
                format!("{base}/{c}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // As served by mediamtx for an H.264 stream, plus a JPEG section.
    const SDP: &str = "v=0\r\n\
        o=- 0 0 IN IP4 127.0.0.1\r\n\
        s=Stream\r\n\
        c=IN IP4 0.0.0.0\r\n\
        t=0 0\r\n\
        a=control:*\r\n\
        m=audio 0 RTP/AVP 97\r\n\
        a=control:trackID=9\r\n\
        a=rtpmap:97 MPEG4-GENERIC/48000/2\r\n\
        m=video 0 RTP/AVP 96\r\n\
        a=control:trackID=0\r\n\
        a=rtpmap:96 H264/90000\r\n\
        a=fmtp:96 packetization-mode=1; profile-level-id=42C01F; sprop-parameter-sets=Z0LAH9oBQBbsBEAAAAMAQAAADyPGDKg=,aM4PyA==\r\n\
        m=video 0 RTP/AVP 26\r\n\
        a=control:rtsp://other/track2\r\n";

    #[test]
    fn parses_video_sections() {
        let sdp = parse(SDP).unwrap();
        assert_eq!(sdp.control.as_deref(), Some("*"));
        assert_eq!(sdp.video.len(), 2);
        let h = &sdp.video[0];
        assert_eq!(
            (h.payload_type, h.codec, h.clock_rate),
            (96, Codec::H264, 90000)
        );
        assert_eq!(h.control.as_deref(), Some("trackID=0"));
        assert_eq!(h.fmtp("packetization-mode"), Some("1"));
        let ps = h.sprop_parameter_sets().unwrap();
        assert_eq!(ps.len(), 2);
        assert_eq!(ps[0][0] & 0x1F, 7, "SPS");
        assert_eq!(ps[1], [0x68, 0xCE, 0x0F, 0xC8], "PPS");
        let j = &sdp.video[1];
        assert_eq!((j.payload_type, j.codec), (26, Codec::Jpeg));
        assert!(
            parse("m=video 0 RTP/AVP 98\na=rtpmap:98 H265/90000\n")
                .unwrap()
                .video[0]
                .codec
                == Codec::H265
        );
    }

    #[test]
    fn control_urls() {
        let base = "rtsp://cam:554/live";
        assert_eq!(
            resolve_control(base, Some("trackID=0")),
            "rtsp://cam:554/live/trackID=0"
        );
        assert_eq!(
            resolve_control("rtsp://cam/live/", Some("trackID=0")),
            "rtsp://cam/live/trackID=0"
        );
        assert_eq!(resolve_control(base, Some("*")), base);
        assert_eq!(resolve_control(base, None), base);
        assert_eq!(resolve_control(base, Some("rtsp://x/y")), "rtsp://x/y");
        // An absolute path replaces the base's.
        assert_eq!(
            resolve_control(base, Some("/trackID=1")),
            "rtsp://cam:554/trackID=1"
        );
    }

    #[test]
    fn every_payload_type_of_a_section() {
        let sdp = parse(
            "m=video 0 RTP/AVP 98 96\na=control:trackID=0\n\
             a=rtpmap:98 H265/90000\na=rtpmap:96 H264/90000\n\
             a=fmtp:96 packetization-mode=1\n",
        )
        .unwrap();
        let codecs: Vec<_> = sdp
            .video
            .iter()
            .map(|m| (m.payload_type, m.codec))
            .collect();
        assert_eq!(codecs, [(98, Codec::H265), (96, Codec::H264)]);
        assert!(
            sdp.video
                .iter()
                .all(|m| m.control.as_deref() == Some("trackID=0"))
        );
        assert_eq!(sdp.video[1].fmtp("packetization-mode"), Some("1"));
    }
}
