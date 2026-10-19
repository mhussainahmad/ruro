//! RTSP authentication: Basic and Digest (RFC 2617, MD5, with and without
//! `qop=auth`), as RTSP 1.0 servers and IP cameras use them.

use crate::base64;

/// MD5 (RFC 1321). Only for Digest authentication, which mandates it.
pub fn md5(input: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    // K[i] = floor(abs(sin(i + 1)) * 2^32).
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];
    let mut msg = input.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((input.len() as u64).wrapping_mul(8)).to_le_bytes());
    let mut h = [0x67452301u32, 0xefcdab89, 0x98badcfe, 0x10325476];
    for block in msg.as_chunks::<64>().0 {
        let m: [u32; 16] = std::array::from_fn(|i| {
            u32::from_le_bytes(block[i * 4..i * 4 + 4].try_into().unwrap())
        });
        let [mut a, mut b, mut c, mut d] = h;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(K[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        h = [
            h[0].wrapping_add(a),
            h[1].wrapping_add(b),
            h[2].wrapping_add(c),
            h[3].wrapping_add(d),
        ];
    }
    let mut out = [0u8; 16];
    for (o, w) in out.as_chunks_mut::<4>().0.iter_mut().zip(h) {
        o.copy_from_slice(&w.to_le_bytes());
    }
    out
}

fn md5_hex(s: &str) -> String {
    md5(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A parsed `WWW-Authenticate` challenge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Challenge {
    Basic,
    Digest {
        realm: String,
        nonce: String,
        opaque: Option<String>,
        /// The server offered `qop=auth`.
        qop_auth: bool,
    },
}

/// Splits `k=v, k="quoted, v"` into pairs (keys lowercased).
fn auth_params(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = s.trim();
    while !rest.is_empty() {
        let Some(eq) = rest.find('=') else { break };
        let key = rest[..eq]
            .trim()
            .trim_start_matches(',')
            .trim()
            .to_ascii_lowercase();
        rest = rest[eq + 1..].trim_start();
        let value;
        if let Some(r) = rest.strip_prefix('"') {
            let mut v = String::new();
            let mut chars = r.char_indices();
            let mut end = r.len();
            while let Some((i, c)) = chars.next() {
                match c {
                    '\\' => {
                        if let Some((_, n)) = chars.next() {
                            v.push(n);
                        }
                    }
                    '"' => {
                        end = i + 1;
                        break;
                    }
                    c => v.push(c),
                }
            }
            value = v;
            rest = &r[end.min(r.len())..];
        } else {
            let end = rest.find(',').unwrap_or(rest.len());
            value = rest[..end].trim().to_owned();
            rest = &rest[end..];
        }
        rest = rest.trim_start().trim_start_matches(',').trim_start();
        out.push((key, value));
    }
    out
}

impl Challenge {
    /// Parses one `WWW-Authenticate` value; `None` for unsupported schemes
    /// (and Digest with an algorithm other than MD5).
    pub fn parse(header: &str) -> Option<Challenge> {
        let header = header.trim();
        let (scheme, rest) = header.split_once(' ').unwrap_or((header, ""));
        if scheme.eq_ignore_ascii_case("basic") {
            return Some(Challenge::Basic);
        }
        if !scheme.eq_ignore_ascii_case("digest") {
            return None;
        }
        let params = auth_params(rest);
        let get = |k: &str| {
            params
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
        };
        if get("algorithm").is_some_and(|a| !a.eq_ignore_ascii_case("md5")) {
            return None;
        }
        Some(Challenge::Digest {
            realm: get("realm").unwrap_or_default(),
            nonce: get("nonce")?,
            opaque: get("opaque"),
            qop_auth: get("qop").is_some_and(|q| q.split(',').any(|q| q.trim() == "auth")),
        })
    }
}

/// Produces `Authorization` headers once the server has challenged.
#[derive(Debug, Clone)]
pub struct Authenticator {
    user: String,
    password: String,
    challenge: Option<Challenge>,
    nc: u32,
    cnonce: String,
}

impl Authenticator {
    pub fn new(user: &str, password: &str) -> Self {
        // The client nonce only needs to be unpredictable enough to vary
        // between sessions.
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        Self {
            user: user.to_owned(),
            password: password.to_owned(),
            challenge: None,
            nc: 0,
            cnonce: md5_hex(&format!("{seed}{}", std::process::id()))[..16].to_owned(),
        }
    }

    /// Adopts the strongest supported challenge from a 401 response.
    /// Returns false if none is supported.
    pub fn challenged(&mut self, headers: &[&str]) -> bool {
        let challenges: Vec<Challenge> =
            headers.iter().filter_map(|h| Challenge::parse(h)).collect();
        let best = challenges
            .iter()
            .find(|c| matches!(c, Challenge::Digest { .. }))
            .or(challenges.first());
        match best {
            Some(c) => {
                self.challenge = Some(c.clone());
                self.nc = 0;
                true
            }
            None => false,
        }
    }

    pub fn has_challenge(&self) -> bool {
        self.challenge.is_some()
    }

    /// The `Authorization` value for a request, if challenged.
    pub fn header(&mut self, method: &str, uri: &str) -> Option<String> {
        match self.challenge.as_ref()? {
            Challenge::Basic => Some(format!(
                "Basic {}",
                base64::encode(format!("{}:{}", self.user, self.password).as_bytes())
            )),
            Challenge::Digest {
                realm,
                nonce,
                opaque,
                qop_auth,
            } => {
                let ha1 = md5_hex(&format!("{}:{realm}:{}", self.user, self.password));
                let ha2 = md5_hex(&format!("{method}:{uri}"));
                let mut h = format!(
                    "Digest username=\"{}\", realm=\"{realm}\", nonce=\"{nonce}\", uri=\"{uri}\"",
                    self.user
                );
                let response = if *qop_auth {
                    self.nc += 1;
                    let nc = format!("{:08x}", self.nc);
                    h.push_str(&format!(", qop=auth, nc={nc}, cnonce=\"{}\"", self.cnonce));
                    md5_hex(&format!("{ha1}:{nonce}:{nc}:{}:auth:{ha2}", self.cnonce))
                } else {
                    md5_hex(&format!("{ha1}:{nonce}:{ha2}"))
                };
                h.push_str(&format!(", response=\"{response}\""));
                if let Some(o) = opaque {
                    h.push_str(&format!(", opaque=\"{o}\""));
                }
                Some(h)
            }
        }
    }
}

/// Checks a Digest `Authorization` header against the expected credentials;
/// for test servers.
pub fn verify_digest(header: &str, method: &str, user: &str, password: &str) -> bool {
    let Some(rest) = header.strip_prefix("Digest ") else {
        return false;
    };
    let p = auth_params(rest);
    let get = |k: &str| {
        p.iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.as_str())
            .unwrap_or("")
    };
    let ha1 = md5_hex(&format!("{user}:{}:{password}", get("realm")));
    let ha2 = md5_hex(&format!("{method}:{}", get("uri")));
    let expected = if get("qop") == "auth" {
        md5_hex(&format!(
            "{ha1}:{}:{}:{}:auth:{ha2}",
            get("nonce"),
            get("nc"),
            get("cnonce")
        ))
    } else {
        md5_hex(&format!("{ha1}:{}:{ha2}", get("nonce")))
    };
    get("username") == user && get("response") == expected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_rfc1321_vectors() {
        for (input, hex) in [
            ("", "d41d8cd98f00b204e9800998ecf8427e"),
            ("a", "0cc175b9c0f1b6a831c399e269772661"),
            ("abc", "900150983cd24fb0d6963f7d28e17f72"),
            ("message digest", "f96b697d7cb7938d525a2f31aaf161d0"),
            (
                "abcdefghijklmnopqrstuvwxyz",
                "c3fcd3d76192e4007dfb496cca67e13b",
            ),
            (
                "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
                "d174ab98d277d9f5a5611c2c9f419d9f",
            ),
            (
                "12345678901234567890123456789012345678901234567890123456789012345678901234567890",
                "57edf4a22be3c955ac49da2e2107b67a",
            ),
        ] {
            assert_eq!(md5_hex(input), hex, "{input}");
        }
    }

    #[test]
    fn digest_rfc2617_example() {
        let mut a = Authenticator::new("Mufasa", "Circle Of Life");
        a.cnonce = "0a4f113b".into();
        assert!(a.challenged(&[
            "Basic realm=\"testrealm@host.com\"",
            "Digest realm=\"testrealm@host.com\", qop=\"auth,auth-int\", \
             nonce=\"dcd98b7102dd2f0e8b11d0f600bfb0c093\", \
             opaque=\"5ccc069c403ebaf9f0171e9517f40e41\"",
        ]));
        let h = a.header("GET", "/dir/index.html").unwrap();
        assert!(
            h.contains("response=\"6629fae49393a05397450978507c4ef1\""),
            "{h}"
        );
        assert!(
            h.contains("nc=00000001") && h.contains("opaque=\"5ccc069c403ebaf9f0171e9517f40e41\"")
        );
        assert!(verify_digest(&h, "GET", "Mufasa", "Circle Of Life"));
        assert!(!verify_digest(&h, "GET", "Mufasa", "wrong"));
        // The nonce count advances with every request.
        assert!(a.header("GET", "/").unwrap().contains("nc=00000002"));
    }

    #[test]
    fn digest_without_qop_and_basic() {
        let mut a = Authenticator::new("admin", "12345");
        assert!(a.header("DESCRIBE", "rtsp://cam/").is_none());
        assert!(a.challenged(&["Digest realm=\"IP Camera\", nonce=\"abc\", stale=\"FALSE\""]));
        let h = a.header("DESCRIBE", "rtsp://cam/").unwrap();
        assert!(!h.contains("qop") && !h.contains("opaque"));
        assert!(verify_digest(&h, "DESCRIBE", "admin", "12345"));
        // RFC 2617 without qop: MD5(HA1:nonce:HA2).
        let ha1 = md5_hex("admin:IP Camera:12345");
        let ha2 = md5_hex("DESCRIBE:rtsp://cam/");
        assert!(h.contains(&md5_hex(&format!("{ha1}:abc:{ha2}"))));

        let mut b = Authenticator::new("admin", "secret");
        assert!(b.challenged(&["Basic realm=\"cam\""]));
        assert_eq!(b.header("PLAY", "x").unwrap(), "Basic YWRtaW46c2VjcmV0");
        assert!(!b.challenged(&[
            "Bearer x",
            "Digest realm=\"r\", nonce=\"n\", algorithm=SHA-256"
        ]));
    }
}
