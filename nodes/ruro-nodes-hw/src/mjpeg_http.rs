//! Motion-JPEG over HTTP (`multipart/x-mixed-replace`), the stream format of
//! most IP cameras, ESP32-CAM firmware and `mjpg-streamer`, and the
//! `hw/mjpeg-http-camera` node. Plain HTTP only.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;
use std::time::Duration;

use ruro::prelude::*;

use crate::pixel::decode_jpeg;

/// Parts of an `http://host[:port]/path` URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpUrl {
    pub host: String,
    pub port: u16,
    pub path: String,
    /// `user:password` for basic authentication, if present in the URL.
    pub userinfo: Option<String>,
}

pub fn parse_url(url: &str) -> Result<HttpUrl, String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| format!("only http:// URLs are supported, got `{url}`"))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (userinfo, hostport) = match authority.rsplit_once('@') {
        Some((u, h)) => (Some(u.to_owned()), h),
        None => (None, authority),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().map_err(|_| format!("bad port in `{url}`"))?),
        _ => (hostport, 80),
    };
    if host.is_empty() {
        return Err(format!("no host in `{url}`"));
    }
    Ok(HttpUrl {
        host: host.to_owned(),
        port,
        path: path.to_owned(),
        userinfo,
    })
}

fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in input.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Extracts the boundary from a `Content-Type: multipart/x-mixed-replace; boundary=...` value.
pub fn boundary_of(content_type: &str) -> Option<String> {
    let ct = content_type.trim();
    if !ct.to_ascii_lowercase().starts_with("multipart/") {
        return None;
    }
    ct.split(';').map(str::trim).find_map(|p| {
        let (k, v) = p.split_once('=')?;
        k.trim().eq_ignore_ascii_case("boundary").then(|| {
            let v = v.trim().trim_matches('"');
            v.strip_prefix("--").unwrap_or(v).to_owned()
        })
    })
}

fn read_line(r: &mut impl BufRead) -> io::Result<String> {
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "stream ended"));
    }
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}

/// Reads headers up to the blank line; returns (name lowercased, value) pairs.
fn read_headers(r: &mut impl BufRead) -> io::Result<Vec<(String, String)>> {
    let mut h = Vec::new();
    loop {
        let line = read_line(r)?;
        if line.is_empty() {
            return Ok(h);
        }
        if let Some((k, v)) = line.split_once(':') {
            h.push((k.trim().to_ascii_lowercase(), v.trim().to_owned()));
        }
    }
}

/// Reads JPEG parts from a multipart body.
pub struct PartReader<R> {
    reader: BufReader<R>,
    boundary: String,
}

impl<R: Read> PartReader<R> {
    pub fn new(reader: BufReader<R>, boundary: String) -> Self {
        Self { reader, boundary }
    }

    /// Returns the next part's body.
    pub fn next_part(&mut self) -> io::Result<Vec<u8>> {
        // Skip to the boundary line (some servers omit the leading "--").
        loop {
            let line = read_line(&mut self.reader)?;
            let l = line.trim();
            let b = l.strip_prefix("--").unwrap_or(l);
            if b == self.boundary {
                break;
            }
            if b == format!("{}--", self.boundary) {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "stream closed by server",
                ));
            }
        }
        let headers = read_headers(&mut self.reader)?;
        let len = headers
            .iter()
            .find(|(k, _)| k == "content-length")
            .and_then(|(_, v)| v.parse::<usize>().ok());
        match len {
            Some(n) => {
                let mut body = vec![0u8; n];
                self.reader.read_exact(&mut body)?;
                Ok(body)
            }
            None => self.read_until_eoi(),
        }
    }

    /// Without Content-Length: read until the JPEG end-of-image marker.
    fn read_until_eoi(&mut self) -> io::Result<Vec<u8>> {
        let mut body = Vec::new();
        loop {
            let mut b = [0u8; 1];
            self.reader.read_exact(&mut b)?;
            body.push(b[0]);
            if body.ends_with(&[0xFF, 0xD9]) {
                return Ok(body);
            }
            if body.len() > 64 << 20 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "part larger than 64 MiB",
                ));
            }
        }
    }
}

/// Connects and returns a reader positioned at the multipart body.
pub fn open_stream(url: &HttpUrl, timeout: Duration) -> io::Result<PartReader<TcpStream>> {
    let addr = (url.host.as_str(), url.port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "host not found"))?;
    let mut stream = TcpStream::connect_timeout(&addr, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    let auth = url
        .userinfo
        .as_ref()
        .map(|u| format!("Authorization: Basic {}\r\n", base64(u.as_bytes())))
        .unwrap_or_default();
    write!(
        stream,
        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: ruro\r\nAccept: multipart/x-mixed-replace, image/jpeg\r\n{auth}Connection: close\r\n\r\n",
        url.path, url.host
    )?;
    let mut reader = BufReader::with_capacity(64 << 10, stream);
    let status = read_line(&mut reader)?;
    let code = status.split_whitespace().nth(1).unwrap_or("");
    if code != "200" {
        return Err(io::Error::other(format!("server answered `{status}`")));
    }
    let headers = read_headers(&mut reader)?;
    let ct = headers
        .iter()
        .find(|(k, _)| k == "content-type")
        .map(|(_, v)| v.as_str())
        .unwrap_or("");
    let boundary = boundary_of(ct).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("not an MJPEG stream (Content-Type `{ct}`)"),
        )
    })?;
    Ok(PartReader::new(reader, boundary))
}

/// IP camera streaming Motion-JPEG over HTTP (ESP32-CAM, mjpg-streamer,
/// most network cameras). A background thread keeps the connection open and
/// reconnects on errors; each step decodes the newest frame.
#[derive(Node)]
#[node(kind = "hw/mjpeg-http-camera", rate = 30)]
pub struct MjpegHttpCamera {
    #[output]
    image: Output<Image>,
    /// Stream URL, e.g. `http://192.168.4.1:81/stream`. `user:pass@` is supported.
    #[param(default = "http://192.168.4.1:81/stream")]
    url: String,
    /// Connect and read timeout in seconds.
    #[param(default = 5.0, min = 0.1, max = 60)]
    timeout: f64,
    frames: Option<Receiver<Vec<u8>>>,
    worker: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
}

fn reader_loop(url: HttpUrl, timeout: Duration, tx: SyncSender<Vec<u8>>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        match open_stream(&url, timeout) {
            Ok(mut parts) => loop {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                match parts.next_part() {
                    Ok(jpeg) => match tx.try_send(jpeg) {
                        Ok(()) | Err(TrySendError::Full(_)) => {}
                        Err(TrySendError::Disconnected(_)) => return,
                    },
                    Err(e) => {
                        warn!("MJPEG stream {}:{}: {e}", url.host, url.port);
                        break;
                    }
                }
            },
            Err(e) => warn!("cannot open MJPEG stream {}:{}: {e}", url.host, url.port),
        }
        for _ in 0..10 {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Node for MjpegHttpCamera {
    fn start(&mut self, _ctx: &mut Ctx) -> Result {
        let url = parse_url(&self.url).map_err(|e| anyhow!(e))?;
        let (tx, rx) = sync_channel(2);
        let stop = self.stop.clone();
        let timeout = Duration::from_secs_f64(self.timeout);
        self.worker = Some(
            std::thread::Builder::new()
                .name("ruro-mjpeg".into())
                .spawn(move || reader_loop(url, timeout, tx, stop))?,
        );
        self.frames = Some(rx);
        Ok(())
    }

    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        let rx = self.frames.as_ref().context("not started")?;
        // Only the newest frame matters.
        let Some(jpeg) = rx.try_iter().last() else {
            return Ok(());
        };
        let (width, height, data) = decode_jpeg(&jpeg).map_err(|e| anyhow!(e))?;
        self.image.send(Image {
            width,
            height,
            encoding: Encoding::Rgb8,
            data,
        })
    }

    fn stop(&mut self, _ctx: &mut Ctx) {
        self.stop.store(true, Ordering::Relaxed);
        self.frames = None;
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn urls() {
        assert_eq!(
            parse_url("http://192.168.4.1:81/stream").unwrap(),
            HttpUrl {
                host: "192.168.4.1".into(),
                port: 81,
                path: "/stream".into(),
                userinfo: None
            }
        );
        let u = parse_url("http://admin:secret@cam.local").unwrap();
        assert_eq!(
            (u.host.as_str(), u.port, u.path.as_str()),
            ("cam.local", 80, "/")
        );
        assert_eq!(u.userinfo.as_deref(), Some("admin:secret"));
        assert!(parse_url("https://x/").is_err());
        assert!(parse_url("http://:80/").is_err());
        assert_eq!(base64(b"admin:secret"), "YWRtaW46c2VjcmV0");
        assert_eq!(base64(b"ab"), "YWI=");
    }

    #[test]
    fn boundaries() {
        assert_eq!(
            boundary_of("multipart/x-mixed-replace;boundary=123456789000000000000987654321")
                .as_deref(),
            Some("123456789000000000000987654321")
        );
        assert_eq!(
            boundary_of("multipart/x-mixed-replace; boundary=\"--frame\"").as_deref(),
            Some("frame")
        );
        assert_eq!(boundary_of("image/jpeg"), None);
    }

    fn jpeg(r: u8, g: u8, b: u8) -> Vec<u8> {
        let rgb: Vec<u8> = (0..8 * 8).flat_map(|_| [r, g, b]).collect();
        let mut out = Vec::new();
        jpeg_encoder::Encoder::new(&mut out, 90)
            .encode(&rgb, 8, 8, jpeg_encoder::ColorType::Rgb)
            .unwrap();
        out
    }

    /// Consumes the request head so closing the socket does not reset it.
    fn read_request(s: &mut TcpStream) {
        let mut req = Vec::new();
        let mut b = [0u8; 1];
        while !req.ends_with(b"\r\n\r\n") && s.read(&mut b).unwrap_or(0) == 1 {
            req.push(b[0]);
        }
    }

    /// Serves a short multipart stream: one part with Content-Length, one without.
    fn serve(frames: Vec<Vec<u8>>) -> std::net::SocketAddr {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            read_request(&mut s);
            s.write_all(b"HTTP/1.0 200 OK\r\nContent-Type: multipart/x-mixed-replace;boundary=frame\r\n\r\n")
                .unwrap();
            for (i, f) in frames.iter().enumerate() {
                if i % 2 == 0 {
                    write!(
                        s,
                        "--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
                        f.len()
                    )
                    .unwrap();
                } else {
                    write!(s, "--frame\r\nContent-Type: image/jpeg\r\n\r\n").unwrap();
                }
                s.write_all(f).unwrap();
                s.write_all(b"\r\n").unwrap();
            }
            s.write_all(b"--frame--\r\n").unwrap();
        });
        addr
    }

    #[test]
    fn reads_parts_from_a_local_server() {
        let frames = vec![jpeg(255, 0, 0), jpeg(0, 255, 0), jpeg(0, 0, 255)];
        let addr = serve(frames.clone());
        let url = parse_url(&format!("http://{addr}/stream")).unwrap();
        let mut parts = open_stream(&url, Duration::from_secs(2)).unwrap();
        for f in &frames {
            assert_eq!(&parts.next_part().unwrap(), f);
        }
        assert!(parts.next_part().is_err());
        let (w, h, px) = decode_jpeg(&frames[1]).unwrap();
        assert_eq!((w, h), (8, 8));
        assert!(px[1] > 200 && px[0] < 60);
    }

    #[test]
    fn rejects_non_multipart_responses() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            read_request(&mut s);
            s.write_all(b"HTTP/1.1 404 Not Found\r\n\r\n").unwrap();
        });
        let url = parse_url(&format!("http://{addr}/")).unwrap();
        let err = open_stream(&url, Duration::from_secs(2)).err().unwrap();
        assert!(err.to_string().contains("404"));
    }
}
