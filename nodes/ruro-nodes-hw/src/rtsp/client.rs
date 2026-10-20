//! RTSP 1.0 client (RFC 2326): OPTIONS, DESCRIBE, SETUP, PLAY, keep-alive
//! and TEARDOWN for one video track, with RTP interleaved on the RTSP
//! connection (TCP) or on a pair of UDP ports.

use std::io::{self, BufRead, BufReader, Write};
use std::net::{IpAddr, Shutdown, TcpStream, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

use ruro::prelude::*;

use super::auth::Authenticator;
use super::sdp::{self, Codec, Media};

/// Parts of an `rtsp://[user[:password]@]host[:port]/path` URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtspUrl {
    pub host: String,
    pub port: u16,
    pub user: Option<String>,
    pub password: Option<String>,
    /// The URL without credentials, as sent in requests.
    pub url: String,
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Some(v) = std::str::from_utf8(&b[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn parse_url(url: &str) -> Result<RtspUrl> {
    let rest = url
        .strip_prefix("rtsp://")
        .ok_or_else(|| anyhow!("only rtsp:// URLs are supported, got `{url}`"))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (userinfo, hostport) = match authority.rsplit_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, authority),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        // `[::1]:8554` has a port; in `[::1]` the last colon is the address's.
        Some((h, p)) if !h.contains('[') || h.ends_with(']') => (
            h,
            p.parse().map_err(|_| anyhow!("bad port in `{hostport}`"))?,
        ),
        _ => (hostport, 554),
    };
    ensure!(!host.is_empty(), "no host in `{url}`");
    let (user, password) = match userinfo.map(|u| u.split_once(':').unwrap_or((u, ""))) {
        Some((u, p)) => (Some(percent_decode(u)), Some(percent_decode(p))),
        None => (None, None),
    };
    Ok(RtspUrl {
        host: host.trim_matches(['[', ']']).to_owned(),
        port,
        user,
        password,
        url: format!("rtsp://{hostport}{path}"),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// RTP interleaved on the RTSP connection (RFC 2326 section 10.12).
    Tcp,
    Udp,
}

impl Transport {
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "tcp" | "" => Ok(Transport::Tcp),
            "udp" => Ok(Transport::Udp),
            other => bail!("unknown transport `{other}` (expected tcp or udp)"),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Response {
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn headers_named(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .collect()
    }
}

/// Something read from the RTSP connection.
#[derive(Debug)]
pub enum Incoming {
    Response(Response),
    /// A request from the server (rare: some send `GET_PARAMETER` or
    /// `ANNOUNCE`); it is read and ignored.
    Request(String),
    /// An interleaved binary frame: `$`, channel, length, data.
    Data {
        channel: u8,
        data: Vec<u8>,
    },
}

fn read_line(r: &mut impl BufRead) -> io::Result<String> {
    let mut line = Vec::new();
    if r.read_until(b'\n', &mut line)? == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "connection closed",
        ));
    }
    if line.len() > 8192 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
    }
    let s = String::from_utf8_lossy(&line);
    Ok(s.trim_end_matches(['\r', '\n']).to_owned())
}

/// Reads the next message or interleaved frame.
pub fn read_incoming(r: &mut impl BufRead) -> io::Result<Incoming> {
    // Skip anything that is neither a frame nor a message start (padding
    // some servers leave between frames).
    loop {
        let buf = r.fill_buf()?;
        if buf.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed",
            ));
        }
        if buf[0] == b'$' || buf[0].is_ascii_alphabetic() {
            break;
        }
        r.consume(1);
    }
    if r.fill_buf()?[0] == b'$' {
        let mut h = [0u8; 4];
        r.read_exact(&mut h)?;
        let mut data = vec![0u8; u16::from_be_bytes([h[2], h[3]]) as usize];
        r.read_exact(&mut data)?;
        return Ok(Incoming::Data {
            channel: h[1],
            data,
        });
    }
    let start = read_line(r)?;
    let mut headers = Vec::new();
    loop {
        let line = read_line(r)?;
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_owned(), v.trim().to_owned()));
        }
        if headers.len() > 256 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "too many headers",
            ));
        }
    }
    let len = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);
    if len > 1 << 20 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "body too large"));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    if let Some(rest) = start.strip_prefix("RTSP/") {
        let mut parts = rest.splitn(3, ' ');
        let _version = parts.next();
        let status = parts.next().and_then(|s| s.parse().ok()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("bad status line `{start}`"),
            )
        })?;
        Ok(Incoming::Response(Response {
            status,
            reason: parts.next().unwrap_or("").to_owned(),
            headers,
            body,
        }))
    } else {
        Ok(Incoming::Request(start))
    }
}

/// Session id and timeout from a `Session: id;timeout=60` header.
pub fn parse_session(h: &str) -> (String, Option<u64>) {
    let mut parts = h.split(';');
    let id = parts.next().unwrap_or("").trim().to_owned();
    let timeout = parts
        .filter_map(|p| p.trim().strip_prefix("timeout="))
        .find_map(|t| t.trim().parse().ok());
    (id, timeout)
}

/// A `key=a-b` pair from a Transport header.
pub fn transport_pair(h: &str, key: &str) -> Option<(u16, u16)> {
    h.split(';').find_map(|p| {
        let v = p.trim().strip_prefix(key)?.strip_prefix('=')?;
        let (a, b) = v.split_once('-').unwrap_or((v, v));
        Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
    })
}

/// Binds an even/odd pair of UDP ports for RTP and RTCP, of the camera's
/// address family.
fn bind_udp_pair(ipv6: bool) -> io::Result<(UdpSocket, UdpSocket)> {
    let any = if ipv6 { "::" } else { "0.0.0.0" };
    for _ in 0..50 {
        let rtp = UdpSocket::bind((any, 0))?;
        let port = rtp.local_addr()?.port();
        if port % 2 != 0 || port == u16::MAX {
            continue;
        }
        if let Ok(rtcp) = UdpSocket::bind((any, port + 1)) {
            return Ok((rtp, rtcp));
        }
    }
    Err(io::Error::other("no free even/odd UDP port pair"))
}

enum Channel {
    Interleaved(u8),
    Udp {
        rtp: UdpSocket,
        _rtcp: UdpSocket,
        server: IpAddr,
    },
}

/// A playing RTSP session for one video track.
pub struct Session {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    url: RtspUrl,
    cseq: u32,
    auth: Option<Authenticator>,
    session_id: Option<String>,
    play_url: String,
    keepalive_method: &'static str,
    keepalive_every: Duration,
    last_keepalive: Instant,
    last_packet: Instant,
    timeout: Duration,
    channel: Channel,
    pub media: Media,
    /// Keep-alive requests sent, for tests and logging.
    pub keepalives: u64,
}

impl Session {
    /// Connects, negotiates the first H.264 or JPEG video track and starts
    /// playing.
    pub fn open(url: &RtspUrl, transport: Transport, timeout: Duration) -> Result<Session> {
        let addr = (url.host.as_str(), url.port)
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| anyhow!("cannot resolve {}", url.host))?;
        let stream = TcpStream::connect_timeout(&addr, timeout)
            .with_context(|| format!("connecting to {addr}"))?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_nodelay(true)?;
        let auth = url
            .user
            .as_ref()
            .map(|u| Authenticator::new(u, url.password.as_deref().unwrap_or("")));
        let mut s = Session {
            reader: BufReader::with_capacity(256 << 10, stream.try_clone()?),
            writer: stream,
            url: url.clone(),
            cseq: 0,
            auth,
            session_id: None,
            play_url: url.url.clone(),
            keepalive_method: "OPTIONS",
            keepalive_every: Duration::from_secs(30),
            last_keepalive: Instant::now(),
            last_packet: Instant::now(),
            timeout,
            channel: Channel::Interleaved(0),
            media: Media {
                payload_type: 0,
                codec: Codec::Other,
                encoding: String::new(),
                clock_rate: 90000,
                control: None,
                fmtp: Vec::new(),
            },
            keepalives: 0,
        };
        let request_url = url.url.clone();
        if let Ok(r) = s.request("OPTIONS", &request_url, &[])
            && r.header("Public")
                .is_some_and(|p| p.contains("GET_PARAMETER"))
        {
            s.keepalive_method = "GET_PARAMETER";
        }
        let r = s.request("DESCRIBE", &request_url, &[("Accept", "application/sdp")])?;
        let description = sdp::parse(&String::from_utf8_lossy(&r.body)).map_err(|e| anyhow!(e))?;
        let base = r
            .header("Content-Base")
            .or(r.header("Content-Location"))
            .unwrap_or(&request_url)
            .to_owned();
        let media = description
            .video
            .iter()
            .find(|m| matches!(m.codec, Codec::H264 | Codec::Jpeg))
            .cloned()
            .ok_or_else(|| {
                if description.video.iter().any(|m| m.codec == Codec::H265) {
                    anyhow!("the stream is H.265, which is not supported yet; configure the camera for H.264 or MJPEG")
                } else {
                    let found: Vec<&str> =
                        description.video.iter().map(|m| m.encoding.as_str()).collect();
                    anyhow!("no H.264 or MJPEG video track in the stream (video tracks: {found:?})")
                }
            })?;
        let track_url = sdp::resolve_control(&base, media.control.as_deref());
        s.play_url = sdp::resolve_control(&base, description.control.as_deref());
        s.media = media;

        let (header, udp) = match transport {
            Transport::Tcp => ("RTP/AVP/TCP;unicast;interleaved=0-1".to_owned(), None),
            Transport::Udp => {
                let (rtp, rtcp) = bind_udp_pair(s.writer.peer_addr()?.is_ipv6())?;
                let p = rtp.local_addr()?.port();
                (
                    format!("RTP/AVP;unicast;client_port={}-{}", p, p + 1),
                    Some((rtp, rtcp)),
                )
            }
        };
        let r = s.request("SETUP", &track_url, &[("Transport", &header)])?;
        let session = r
            .header("Session")
            .ok_or_else(|| anyhow!("SETUP response has no Session header"))?;
        let (id, timeout_s) = parse_session(session);
        s.session_id = Some(id);
        s.keepalive_every = Duration::from_secs((timeout_s.unwrap_or(60) / 2).max(1));
        let t = r.header("Transport").unwrap_or("").to_owned();
        s.channel = match udp {
            None => {
                ensure!(
                    t.is_empty() || t.contains("interleaved") || t.contains("TCP"),
                    "server refused TCP interleaving (Transport: {t}); try transport: udp"
                );
                Channel::Interleaved(transport_pair(&t, "interleaved").map_or(0, |p| p.0 as u8))
            }
            Some((rtp, rtcp)) => {
                rtp.set_read_timeout(Some(Duration::from_millis(200)))?;
                let server = t
                    .split(';')
                    .find_map(|p| p.trim().strip_prefix("source=")?.parse().ok())
                    .unwrap_or(addr.ip());
                Channel::Udp {
                    rtp,
                    _rtcp: rtcp,
                    server,
                }
            }
        };
        let play_url = s.play_url.clone();
        s.request("PLAY", &play_url, &[("Range", "npt=0.000-")])?;
        s.last_keepalive = Instant::now();
        s.last_packet = Instant::now();
        Ok(s)
    }

    fn send(&mut self, method: &str, url: &str, extra: &[(&str, &str)]) -> io::Result<u32> {
        self.cseq += 1;
        let mut req = format!(
            "{method} {url} RTSP/1.0\r\nCSeq: {}\r\nUser-Agent: ruro\r\n",
            self.cseq
        );
        if let Some(id) = &self.session_id {
            req.push_str(&format!("Session: {id}\r\n"));
        }
        if let Some(h) = self.auth.as_mut().and_then(|a| a.header(method, url)) {
            req.push_str(&format!("Authorization: {h}\r\n"));
        }
        for (k, v) in extra {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("\r\n");
        self.writer.write_all(req.as_bytes())?;
        Ok(self.cseq)
    }

    /// Sends a request and waits for its response, answering one
    /// authentication challenge.
    fn request(&mut self, method: &str, url: &str, extra: &[(&str, &str)]) -> Result<Response> {
        for attempt in 0..2 {
            let cseq = self.send(method, url, extra)?;
            let r = loop {
                match read_incoming(&mut self.reader)? {
                    Incoming::Response(r)
                        if r.header("CSeq").and_then(|c| c.trim().parse().ok()) == Some(cseq)
                            || r.header("CSeq").is_none() =>
                    {
                        break r;
                    }
                    _ => {}
                }
            };
            if r.status == 401 && attempt == 0 {
                let challenges = r.headers_named("WWW-Authenticate");
                match self.auth.as_mut() {
                    None => bail!(
                        "{method}: the camera requires authentication; put user:password@ in the URL"
                    ),
                    Some(a) => {
                        ensure!(
                            a.challenged(&challenges),
                            "{method}: unsupported authentication scheme ({challenges:?})"
                        );
                        continue;
                    }
                }
            }
            ensure!(
                r.status == 200,
                "{method} {url}: {} {}{}",
                r.status,
                r.reason,
                if r.status == 401 {
                    " (wrong user or password?)"
                } else {
                    ""
                }
            );
            return Ok(r);
        }
        unreachable!()
    }

    fn keepalive_if_due(&mut self) -> Result<()> {
        if self.last_keepalive.elapsed() < self.keepalive_every {
            return Ok(());
        }
        self.last_keepalive = Instant::now();
        self.keepalives += 1;
        let (method, url) = (self.keepalive_method, self.play_url.clone());
        match self.channel {
            // The response arrives between RTP frames and is skipped there.
            Channel::Interleaved(_) => {
                self.send(method, &url, &[])?;
            }
            Channel::Udp { .. } => {
                self.request(method, &url, &[])?;
            }
        }
        Ok(())
    }

    /// The next RTP packet of the video track. Over UDP, returns `None`
    /// after 200 ms without one so that the caller can check for a stop.
    pub fn next_packet(&mut self) -> Result<Option<Vec<u8>>> {
        loop {
            self.keepalive_if_due()?;
            match &self.channel {
                Channel::Interleaved(ch) => {
                    let ch = *ch;
                    match read_incoming(&mut self.reader)? {
                        Incoming::Data { channel, data } if channel == ch => return Ok(Some(data)),
                        Incoming::Response(r)
                            if r.status == 401
                                && self.auth.as_mut().is_some_and(|a| {
                                    a.challenged(&r.headers_named("WWW-Authenticate"))
                                }) =>
                        {
                            // A new challenge (an expired nonce, typically):
                            // send the keep-alive again with it now.
                            self.last_keepalive = Instant::now()
                                .checked_sub(self.keepalive_every)
                                .unwrap_or(self.last_keepalive);
                        }
                        Incoming::Response(r) if r.status != 200 => {
                            warn!("RTSP keep-alive answered {} {}", r.status, r.reason)
                        }
                        _ => {}
                    }
                }
                Channel::Udp { rtp, server, .. } => {
                    let mut buf = vec![0u8; 65536];
                    match rtp.recv_from(&mut buf) {
                        Ok((n, from)) if from.ip() == *server => {
                            buf.truncate(n);
                            self.last_packet = Instant::now();
                            return Ok(Some(buf));
                        }
                        Ok(_) => {}
                        Err(e)
                            if matches!(
                                e.kind(),
                                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                            ) =>
                        {
                            ensure!(
                                self.last_packet.elapsed() < self.timeout,
                                "no RTP packets over UDP for {:?} (a firewall or NAT may be dropping them; try transport: tcp)",
                                self.timeout
                            );
                            return Ok(None);
                        }
                        Err(e) => return Err(e.into()),
                    }
                }
            }
        }
    }

    /// A handle that unblocks a pending read when shut down.
    pub fn stop_handle(&self) -> io::Result<TcpStream> {
        self.writer.try_clone()
    }

    /// Ends the session (best effort, without waiting for the answer).
    pub fn teardown(&mut self) {
        let url = self.play_url.clone();
        let _ = self.send("TEARDOWN", &url, &[]);
        let _ = self.writer.shutdown(Shutdown::Both);
    }

    pub fn url(&self) -> &RtspUrl {
        &self.url
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        let u = parse_url("rtsp://admin:p%40ss@192.168.1.10:8554/Streaming/101?tcp").unwrap();
        assert_eq!((u.host.as_str(), u.port), ("192.168.1.10", 8554));
        assert_eq!(
            (u.user.as_deref(), u.password.as_deref()),
            (Some("admin"), Some("p@ss"))
        );
        assert_eq!(u.url, "rtsp://192.168.1.10:8554/Streaming/101?tcp");
        let u = parse_url("rtsp://cam.local").unwrap();
        assert_eq!(
            (u.port, u.url.as_str(), u.user),
            (554, "rtsp://cam.local/", None)
        );
        assert!(parse_url("http://cam/").is_err());
        assert!(parse_url("rtsp:///x").is_err());
        assert!(parse_url("rtsp://cam:x/").is_err());
        for (url, port) in [
            ("rtsp://[::1]:8554/x", 8554),
            ("rtsp://[::1]/x", 554),
            ("rtsp://user@[fe80::1]/x", 554),
        ] {
            let u = parse_url(url).unwrap();
            assert_eq!(
                (u.host.as_str(), u.port),
                (
                    &url[url.find('[').unwrap() + 1..url.find(']').unwrap()],
                    port
                ),
                "{url}"
            );
        }
    }

    #[test]
    fn headers() {
        assert_eq!(
            parse_session("12345678;timeout=60"),
            ("12345678".into(), Some(60))
        );
        assert_eq!(parse_session(" abc "), ("abc".into(), None));
        let t = "RTP/AVP;unicast;client_port=5000-5001;server_port=6970-6971;ssrc=1";
        assert_eq!(transport_pair(t, "server_port"), Some((6970, 6971)));
        assert_eq!(
            transport_pair("RTP/AVP/TCP;interleaved=2-3", "interleaved"),
            Some((2, 3))
        );
        assert_eq!(transport_pair(t, "interleaved"), None);
        assert_eq!(Transport::parse("UDP").unwrap(), Transport::Udp);
        assert!(Transport::parse("http").is_err());
    }

    #[test]
    fn reads_responses_between_interleaved_frames() {
        let mut wire = Vec::new();
        wire.extend_from_slice(&[b'$', 0, 0, 3, 1, 2, 3]);
        wire.extend_from_slice(b"RTSP/1.0 200 OK\r\nCSeq: 7\r\nContent-Length: 2\r\n\r\nhi");
        wire.extend_from_slice(&[0, 0]); // stray padding
        wire.extend_from_slice(&[b'$', 1, 0, 1, 9]);
        wire.extend_from_slice(b"GET_PARAMETER rtsp://x RTSP/1.0\r\nCSeq: 1\r\n\r\n");
        let mut r = BufReader::new(&wire[..]);
        assert!(
            matches!(read_incoming(&mut r).unwrap(), Incoming::Data { channel: 0, data } if data == [1, 2, 3])
        );
        match read_incoming(&mut r).unwrap() {
            Incoming::Response(resp) => {
                assert_eq!((resp.status, resp.header("cseq")), (200, Some("7")));
                assert_eq!(resp.body, b"hi");
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            read_incoming(&mut r).unwrap(),
            Incoming::Data { channel: 1, .. }
        ));
        assert!(
            matches!(read_incoming(&mut r).unwrap(), Incoming::Request(m) if m.starts_with("GET_PARAMETER"))
        );
        assert!(read_incoming(&mut r).is_err());
    }
}
