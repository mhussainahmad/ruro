//! `hw/rtsp-camera` against an in-process RTSP server: H.264 (encoded with
//! OpenH264 when the `h264` feature is on) and Motion JPEG, over TCP
//! interleaving and UDP, with Basic and Digest authentication, keep-alives
//! and reconnection after the server drops the connection.

use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use ruro::msg::{Bytes, Image};
use ruro::node::testing::Harness;
use ruro_nodes_hw::RtspCamera;
use ruro_nodes_hw::pixel::decode_jpeg;
use ruro_nodes_hw::rtsp::client::{Session, Transport, parse_url};
use ruro_nodes_hw::rtsp::{auth, h264, jpeg, rtp};
use serde_json::json;

const USER: &str = "viewer";
const PASS: &str = "s3cret";

#[derive(Clone)]
enum Stream {
    /// Frames as lists of NAL units. With `in_band`, SPS and PPS travel in
    /// the stream (aggregated in a STAP-A) instead of the SDP.
    H264 {
        sps: Vec<u8>,
        pps: Vec<u8>,
        frames: Vec<Vec<Vec<u8>>>,
        in_band: bool,
    },
    Jpeg(Vec<Vec<u8>>),
}

#[derive(Clone, Copy, PartialEq)]
enum Auth {
    None,
    Basic,
    Digest,
}

#[derive(Clone)]
struct Config {
    stream: Stream,
    auth: Auth,
    /// Largest RTP payload; small, so that frames fragment.
    mtu: usize,
    session_timeout: u64,
    /// Close the connection after this many frames.
    frames_per_connection: Option<usize>,
    /// With Digest, expire the nonce after this many authorized requests:
    /// later ones need the new nonce, given with `stale=true`.
    nonce_requests: Option<usize>,
}

impl Config {
    fn new(stream: Stream) -> Self {
        Self {
            stream,
            auth: Auth::None,
            mtu: 60,
            session_timeout: 60,
            frames_per_connection: None,
            nonce_requests: None,
        }
    }
}

struct Server {
    addr: SocketAddr,
    log: Arc<Mutex<Vec<String>>>,
    connections: Arc<AtomicUsize>,
}

impl Server {
    fn start(cfg: Config) -> Server {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let log = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(AtomicUsize::new(0));
        let (lg, conns) = (log.clone(), connections.clone());
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(s) = s else { break };
                conns.fetch_add(1, Ordering::Relaxed);
                let (cfg, lg) = (cfg.clone(), lg.clone());
                std::thread::spawn(move || serve(s, cfg, lg));
            }
        });
        Server {
            addr,
            log,
            connections,
        }
    }

    fn url(&self, auth: bool) -> String {
        if auth {
            format!("rtsp://{USER}:{PASS}@{}/cam", self.addr)
        } else {
            format!("rtsp://{}/cam", self.addr)
        }
    }

    fn requests(&self, method: &str) -> usize {
        self.log
            .lock()
            .iter()
            .filter(|l| l.starts_with(method))
            .count()
    }
}

struct Request {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
}

impl Request {
    fn header(&self, name: &str) -> &str {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map_or("", |(_, v)| v.as_str())
    }
}

fn read_request(r: &mut BufReader<TcpStream>) -> Option<Request> {
    let mut line = String::new();
    r.read_line(&mut line).ok().filter(|&n| n > 0)?;
    let mut parts = line.split_whitespace();
    let (method, url) = (parts.next()?.to_owned(), parts.next()?.to_owned());
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        r.read_line(&mut h).ok().filter(|&n| n > 0)?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        let (k, v) = h.split_once(':')?;
        headers.push((k.trim().to_owned(), v.trim().to_owned()));
    }
    Some(Request {
        method,
        url,
        headers,
    })
}

fn authorized(auth: Auth, req: &Request) -> bool {
    let h = req.header("Authorization");
    match auth {
        Auth::None => true,
        Auth::Basic => {
            h == format!(
                "Basic {}",
                ruro_nodes_hw::base64::encode(format!("{USER}:{PASS}").as_bytes())
            )
        }
        Auth::Digest => auth::verify_digest(h, &req.method, USER, PASS),
    }
}

fn sdp(cfg: &Config) -> String {
    let mut s = String::from(
        "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=ruro test\r\nt=0 0\r\na=control:*\r\n\
         m=audio 0 RTP/AVP 0\r\na=control:trackID=1\r\n",
    );
    match &cfg.stream {
        Stream::H264 {
            sps, pps, in_band, ..
        } => {
            s.push_str("m=video 0 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\n");
            if *in_band {
                s.push_str("a=fmtp:96 packetization-mode=1\r\n");
            } else {
                s.push_str(&format!(
                    "a=fmtp:96 packetization-mode=1; sprop-parameter-sets={},{}\r\n",
                    ruro_nodes_hw::base64::encode(sps),
                    ruro_nodes_hw::base64::encode(pps)
                ));
            }
        }
        Stream::Jpeg(_) => s.push_str("m=video 0 RTP/AVP 26\r\n"),
    }
    s.push_str("a=control:trackID=0\r\n");
    s
}

enum Sink {
    Tcp(Arc<Mutex<TcpStream>>),
    Udp(UdpSocket, SocketAddr),
}

impl Sink {
    fn send(&self, packet: &[u8]) -> std::io::Result<()> {
        match self {
            Sink::Tcp(w) => {
                let mut frame = vec![b'$', 0];
                frame.extend_from_slice(&(packet.len() as u16).to_be_bytes());
                frame.extend_from_slice(packet);
                w.lock().write_all(&frame)
            }
            Sink::Udp(s, to) => s.send_to(packet, to).map(|_| ()),
        }
    }
}

/// RTP payloads of each frame, in order.
fn packetized(cfg: &Config) -> (u8, Vec<Vec<Vec<u8>>>) {
    match &cfg.stream {
        Stream::H264 {
            sps,
            pps,
            frames,
            in_band,
        } => {
            let out = frames
                .iter()
                .map(|nals| {
                    let mut all: Vec<&[u8]> = Vec::new();
                    let key = nals.iter().any(|n| h264::nal_type(n) == h264::NAL_IDR);
                    if *in_band && key {
                        all.push(sps);
                        all.push(pps);
                    }
                    all.extend(nals.iter().map(|n| n.as_slice()));
                    h264::packetize(&all, cfg.mtu, true)
                })
                .collect();
            (96, out)
        }
        Stream::Jpeg(frames) => (
            26,
            frames
                .iter()
                .map(|f| jpeg::packetize(f, cfg.mtu).unwrap())
                .collect(),
        ),
    }
}

fn stream_frames(cfg: Config, sink: Sink, conn: TcpStream) {
    let (pt, frames) = packetized(&cfg);
    let (mut seq, mut ts) = (0xFFF0u16, 0u32);
    let mut sent = 0;
    for frame in frames.iter().cycle() {
        for (i, p) in frame.iter().enumerate() {
            let packet = rtp::build(i + 1 == frame.len(), pt, seq, ts, 0x1234, p);
            seq = seq.wrapping_add(1);
            if sink.send(&packet).is_err() {
                return;
            }
        }
        ts = ts.wrapping_add(3000);
        sent += 1;
        if cfg.frames_per_connection == Some(sent) {
            let _ = conn.shutdown(Shutdown::Both);
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn serve(stream: TcpStream, cfg: Config, log: Arc<Mutex<Vec<String>>>) {
    let peer = stream.peer_addr().unwrap();
    let writer = Arc::new(Mutex::new(stream.try_clone().unwrap()));
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut sink = None;
    let mut authorized_requests = 0;
    while let Some(req) = read_request(&mut reader) {
        log.lock().push(format!("{} {}", req.method, req.url));
        let renewed = cfg.nonce_requests.is_some_and(|n| authorized_requests >= n);
        let nonce = if renewed { "b7e2c4" } else { "8f3a91" };
        let cseq = req.header("CSeq").to_owned();
        let respond = |status: &str, headers: &str, body: &str| {
            let r = format!(
                "RTSP/1.0 {status}\r\nCSeq: {cseq}\r\n{headers}Content-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ = writer.lock().write_all(r.as_bytes());
        };
        let fresh = cfg.auth != Auth::Digest
            || req
                .header("Authorization")
                .contains(&format!("nonce=\"{nonce}\""));
        if req.method != "OPTIONS" && !(authorized(cfg.auth, &req) && fresh) {
            let challenge = match cfg.auth {
                Auth::Basic => "WWW-Authenticate: Basic realm=\"ruro\"\r\n".to_owned(),
                _ => format!(
                    "WWW-Authenticate: Digest realm=\"ruro\", nonce=\"{nonce}\", qop=\"auth\", opaque=\"x1\"{}\r\n",
                    if renewed { ", stale=true" } else { "" }
                ),
            };
            respond("401 Unauthorized", &challenge, "");
            continue;
        }
        if req.method != "OPTIONS" {
            authorized_requests += 1;
            if renewed {
                log.lock().push(format!("RENEWED {}", req.method));
            }
        }
        let session = format!("Session: 5EC7ED;timeout={}\r\n", cfg.session_timeout);
        match req.method.as_str() {
            "OPTIONS" => respond(
                "200 OK",
                "Public: OPTIONS, DESCRIBE, SETUP, PLAY, TEARDOWN, GET_PARAMETER\r\n",
                "",
            ),
            "DESCRIBE" => {
                let base = format!(
                    "Content-Base: rtsp://{}/cam/\r\n",
                    stream.local_addr().unwrap()
                );
                respond(
                    "200 OK",
                    &format!("{base}Content-Type: application/sdp\r\n"),
                    &sdp(&cfg),
                )
            }
            "SETUP" => {
                assert!(req.url.ends_with("/cam/trackID=0"), "SETUP {}", req.url);
                let t = req.header("Transport").to_owned();
                if t.contains("TCP") {
                    sink = Some(Sink::Tcp(writer.clone()));
                    respond(
                        "200 OK",
                        &format!("{session}Transport: RTP/AVP/TCP;unicast;interleaved=0-1\r\n"),
                        "",
                    );
                } else {
                    let (a, b) =
                        ruro_nodes_hw::rtsp::client::transport_pair(&t, "client_port").unwrap();
                    let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
                    let sp = udp.local_addr().unwrap().port();
                    sink = Some(Sink::Udp(udp, SocketAddr::new(peer.ip(), a)));
                    respond(
                        "200 OK",
                        &format!(
                            "{session}Transport: RTP/AVP;unicast;client_port={a}-{b};server_port={sp}-{}\r\n",
                            sp + 1
                        ),
                        "",
                    );
                }
            }
            "PLAY" => {
                respond("200 OK", &session, "");
                let sink = sink.take().expect("PLAY before SETUP");
                let (cfg, conn) = (cfg.clone(), stream.try_clone().unwrap());
                std::thread::spawn(move || stream_frames(cfg, sink, conn));
            }
            "GET_PARAMETER" => respond("200 OK", &session, ""),
            "TEARDOWN" => {
                respond("200 OK", &session, "");
                break;
            }
            _ => respond("501 Not Implemented", "", ""),
        }
    }
}

/// Steps the camera until `done` returns true for everything received so far.
fn run_until(
    h: &mut Harness,
    timeout: Duration,
    mut done: impl FnMut(&[Image], &[Bytes]) -> bool,
) -> (Vec<Image>, Vec<Bytes>) {
    let (mut images, mut encoded) = (Vec::new(), Vec::new());
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        h.step().unwrap();
        images.extend(h.take::<Image>("image").unwrap());
        encoded.extend(h.take::<Bytes>("encoded").unwrap());
        if done(&images, &encoded) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    (images, encoded)
}

fn camera(url: String, transport: &str, decode: bool) -> Harness {
    Harness::with_node::<RtspCamera>(
        json!({ "url": url, "transport": transport, "decode": decode, "timeout": 2.0 }),
        |_| {},
    )
    .unwrap()
}

fn test_jpeg(i: usize) -> Vec<u8> {
    let (w, h) = (32u16, 24u16);
    let rgb: Vec<u8> = (0..w as usize * h as usize)
        .flat_map(|p| [(i * 40) as u8, (p % w as usize * 8) as u8, 200])
        .collect();
    let mut out = Vec::new();
    let mut e = jpeg_encoder::Encoder::new(&mut out, 85);
    e.set_sampling_factor(jpeg_encoder::SamplingFactor::F_2_2);
    e.encode(&rgb, w, h, jpeg_encoder::ColorType::Rgb).unwrap();
    out
}

fn check_mjpeg(transport: &str, auth: Auth) {
    let frames: Vec<Vec<u8>> = (0..4).map(test_jpeg).collect();
    let decoded: Vec<_> = frames.iter().map(|f| decode_jpeg(f).unwrap()).collect();
    let mut cfg = Config::new(Stream::Jpeg(frames.clone()));
    cfg.auth = auth;
    let server = Server::start(cfg);
    let mut h = camera(server.url(auth != Auth::None), transport, true);
    let (images, encoded) = run_until(&mut h, Duration::from_secs(10), |i, _| i.len() >= 5);
    assert!(
        images.len() >= 5,
        "{transport}: got {} images",
        images.len()
    );
    for img in &images {
        // Bit-exact with decoding the original file: same tables, same scan.
        assert!(
            decoded
                .iter()
                .any(|(w, hh, px)| (*w, *hh) == (img.width, img.height) && *px == img.data),
            "{transport}: image differs from every source frame"
        );
    }
    assert!(encoded.iter().all(|e| decode_jpeg(&e.0).is_ok()));
    drop(h);
    assert!(server.requests("PLAY") >= 1);
    if auth != Auth::None {
        // The first DESCRIBE is challenged, the retry carries credentials.
        assert!(server.requests("DESCRIBE") >= 2);
    }
}

#[test]
fn mjpeg_over_tcp_with_basic_auth() {
    check_mjpeg("tcp", Auth::Basic);
}

#[test]
fn mjpeg_over_udp_with_digest_auth() {
    check_mjpeg("udp", Auth::Digest);
}

#[test]
fn calibration_is_published_with_each_image() {
    let path = std::env::temp_dir().join(format!("ruro-rtsp-calib-{}.yaml", std::process::id()));
    std::fs::write(
        &path,
        "image_width: 32\nimage_height: 24\ncamera_matrix: {rows: 3, cols: 3, data: [30, 0, 16, 0, 30, 12, 0, 0, 1]}\n\
         distortion_model: plumb_bob\ndistortion_coefficients: {rows: 1, cols: 5, data: [0.1, 0, 0, 0, 0]}\n",
    )
    .unwrap();
    let server = Server::start(Config::new(Stream::Jpeg(vec![test_jpeg(1)])));
    let mut h = Harness::with_node::<RtspCamera>(
        json!({ "url": server.url(false), "calibration": path.to_str().unwrap(), "frame_id": "front" }),
        |_| {},
    )
    .unwrap();
    let (images, _) = run_until(&mut h, Duration::from_secs(10), |i, _| i.len() >= 3);
    // `run_until` takes only images and frames; the infos are still queued.
    let infos = h.take::<ruro::msg::CameraInfo>("camera_info").unwrap();
    std::fs::remove_file(&path).unwrap();
    assert!(images.len() >= 3);
    assert_eq!(infos.len(), images.len(), "one CameraInfo per image");
    let c = &infos[0];
    assert_eq!(
        (c.width, c.height, c.header.frame_id.as_str()),
        (32, 24, "front")
    );
    assert_eq!((c.k[0], c.d[0]), (30.0, 0.1));
    assert!(c.header.stamp_ns > 0);
    // ROS pairs an image with its camera_info by stamp: they are the same.
    let start = Instant::now();
    let mut pairs = Vec::new();
    while pairs.len() < 3 {
        assert!(start.elapsed() < Duration::from_secs(10), "no more images");
        h.step().unwrap();
        let images = h.take_stamped::<ruro::msg::Image>("image").unwrap();
        let infos = h.take::<ruro::msg::CameraInfo>("camera_info").unwrap();
        assert_eq!(images.len(), infos.len());
        pairs.extend(
            images
                .iter()
                .zip(&infos)
                .map(|((s, _), i)| (*s, i.header.stamp_ns)),
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(pairs.iter().all(|(a, b)| a == b), "{pairs:?}");
}

#[test]
fn reconnects_after_the_server_drops_the_connection() {
    let mut cfg = Config::new(Stream::Jpeg((0..2).map(test_jpeg).collect()));
    cfg.frames_per_connection = Some(3);
    let server = Server::start(cfg);
    let mut h = camera(server.url(false), "tcp", true);
    let conns = server.connections.clone();
    let (images, encoded) = run_until(&mut h, Duration::from_secs(15), |_, e| {
        conns.load(Ordering::Relaxed) >= 3 && e.len() >= 7
    });
    let n = server.connections.load(Ordering::Relaxed);
    assert!(n >= 3, "{n} connections");
    // Three frames per connection: the third session delivered frames too.
    assert!(encoded.len() >= 7, "{} frames", encoded.len());
    assert!(!images.is_empty());
}

#[test]
fn keepalives_are_sent_within_the_session_timeout() {
    let mut cfg = Config::new(Stream::Jpeg(vec![test_jpeg(0)]));
    cfg.session_timeout = 2; // keep-alive every second
    let server = Server::start(cfg);
    let mut h = camera(server.url(false), "tcp", false);
    let start = Instant::now();
    let (_, encoded) = run_until(&mut h, Duration::from_secs(10), |_, _| {
        start.elapsed() > Duration::from_millis(2500)
    });
    // Responses to the keep-alives arrive between RTP frames; the stream
    // keeps flowing past them.
    assert!(server.requests("GET_PARAMETER") >= 2);
    assert!(encoded.len() > 50, "{} frames", encoded.len());
    drop(h);
    let deadline = Instant::now() + Duration::from_secs(2);
    while server.requests("TEARDOWN") == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(server.requests("TEARDOWN"), 1);
}

/// A camera that expires its Digest nonce mid-session: the keep-alive
/// answered 401 (stale) is sent again with the new nonce.
#[test]
fn keepalives_follow_an_expired_nonce() {
    let mut cfg = Config::new(Stream::Jpeg(vec![test_jpeg(0)]));
    cfg.auth = Auth::Digest;
    cfg.session_timeout = 2; // keep-alive every second
    cfg.nonce_requests = Some(4); // DESCRIBE, SETUP, PLAY and one keep-alive
    let server = Server::start(cfg);
    let mut h = camera(server.url(true), "tcp", false);
    let start = Instant::now();
    run_until(&mut h, Duration::from_secs(10), |_, _| {
        start.elapsed() > Duration::from_millis(3500)
    });
    assert!(
        server.requests("RENEWED GET_PARAMETER") >= 1,
        "{:?}",
        server.log.lock()
    );
}

/// Opt-in check against a real server, e.g. mediamtx fed by ffmpeg:
/// `RURO_TEST_RTSP_URL=rtsp://127.0.0.1:8554/stream cargo test -p ruro-nodes-hw
/// --features h264 --test rtsp real_server -- --nocapture`. Both transports
/// must deliver decoded frames. `RURO_TEST_RTSP_SECONDS=n` keeps each
/// session open for n seconds (past the server's session timeout, to check
/// keep-alives) and requires frames to still be arriving at the end.
/// `RURO_TEST_RTSP_DUMP=dir` writes the last image of each as a PPM file and
/// the encoded frames as one stream.
#[test]
fn real_server() {
    let Ok(url) = std::env::var("RURO_TEST_RTSP_URL") else {
        eprintln!("skipped: set RURO_TEST_RTSP_URL to test against a real RTSP server");
        return;
    };
    let hold: f64 = std::env::var("RURO_TEST_RTSP_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);
    for transport in ["tcp", "udp"] {
        let mut h = camera(url.clone(), transport, true);
        let start = Instant::now();
        let (mut seen, mut last_new) = (0, Instant::now());
        let (images, encoded) = run_until(&mut h, Duration::from_secs_f64(hold + 20.0), |i, _| {
            if i.len() > seen {
                (seen, last_new) = (i.len(), Instant::now());
            }
            i.len() >= 30 && start.elapsed().as_secs_f64() >= hold
        });
        let secs = start.elapsed().as_secs_f64();
        drop(h);
        assert!(!images.is_empty(), "{transport}: no images from {url}");
        assert!(
            last_new.elapsed() < Duration::from_secs(2),
            "{transport}: frames stopped {:.1} s before the end",
            last_new.elapsed().as_secs_f64()
        );
        let last = images.last().unwrap();
        let mean = last.data.iter().map(|&v| v as f64).sum::<f64>() / last.data.len() as f64;
        eprintln!(
            "{transport}: {} images and {} encoded frames in {secs:.1} s, {}x{}, mean level {mean:.0}",
            images.len(),
            encoded.len(),
            last.width,
            last.height
        );
        if let Ok(dir) = std::env::var("RURO_TEST_RTSP_DUMP") {
            let mut ppm = format!("P6\n{} {}\n255\n", last.width, last.height).into_bytes();
            ppm.extend_from_slice(&last.data);
            std::fs::write(format!("{dir}/rtsp-{transport}.ppm"), ppm).unwrap();
            let raw: Vec<u8> = encoded.iter().flat_map(|e| e.0.iter().copied()).collect();
            std::fs::write(format!("{dir}/rtsp-{transport}.bin"), raw).unwrap();
        }
    }
}

#[test]
fn wrong_credentials_and_missing_streams_are_reported() {
    let mut cfg = Config::new(Stream::Jpeg(vec![test_jpeg(0)]));
    cfg.auth = Auth::Digest;
    let server = Server::start(cfg);
    let bad = parse_url(&format!("rtsp://{USER}:nope@{}/cam", server.addr)).unwrap();
    let err = Session::open(&bad, Transport::Tcp, Duration::from_secs(2))
        .err()
        .unwrap();
    assert!(format!("{err:#}").contains("401"), "{err:#}");
    let anon = parse_url(&server.url(false)).unwrap();
    let err = Session::open(&anon, Transport::Tcp, Duration::from_secs(2))
        .err()
        .unwrap();
    assert!(
        format!("{err:#}").contains("requires authentication"),
        "{err:#}"
    );
    let closed = parse_url("rtsp://127.0.0.1:1/x").unwrap();
    assert!(Session::open(&closed, Transport::Tcp, Duration::from_secs(1)).is_err());
}

/// Encoded output needs no decoder: fake NAL units pass straight through.
#[test]
fn h264_encoded_output_without_decoding() {
    let nal = |t: u8, len: usize| -> Vec<u8> {
        let mut n = vec![0x60 | t];
        n.extend((1..len).map(|i| (i % 200) as u8 + 1));
        n
    };
    let (sps, pps) = (
        vec![0x67, 0x42, 0xC0, 0x1F, 0xDA],
        vec![0x68, 0xCE, 0x0F, 0xC8],
    );
    let frames = vec![
        vec![nal(5, 300)],
        vec![nal(1, 90)],
        vec![nal(1, 20), nal(1, 20)],
    ];
    let server = Server::start(Config::new(Stream::H264 {
        sps: sps.clone(),
        pps: pps.clone(),
        frames: frames.clone(),
        in_band: false,
    }));
    let mut h = camera(server.url(false), "tcp", false);
    let (images, encoded) = run_until(&mut h, Duration::from_secs(10), |_, e| e.len() >= 6);
    assert!(images.is_empty());
    assert!(encoded.len() >= 6);
    let units: Vec<Vec<&[u8]>> = encoded.iter().map(|e| h264::split_annex_b(&e.0)).collect();
    let i = units
        .iter()
        .position(|u| h264::nal_type(u[0]) == 7)
        .unwrap();
    assert_eq!(units[i], [&sps[..], &pps[..], &frames[0][0][..]]);
    assert_eq!(units[i + 1], [&frames[1][0][..]]);
    assert_eq!(units[i + 2], [&frames[2][0][..], &frames[2][1][..]]);
}

#[cfg(feature = "h264")]
mod decoded {
    use super::*;
    use openh264::encoder::Encoder;
    use openh264::formats::{RgbSliceU8, YUVBuffer};

    const COLOURS: [[u8; 3]; 6] = [
        [220, 40, 40],
        [40, 200, 40],
        [40, 40, 220],
        [200, 200, 40],
        [40, 200, 200],
        [200, 40, 200],
    ];

    /// Encodes solid-colour 64x48 frames; returns SPS, PPS and each frame's
    /// other NAL units.
    fn encode() -> (Vec<u8>, Vec<u8>, Vec<Vec<Vec<u8>>>) {
        let (w, h) = (64, 48);
        let mut enc = Encoder::new().unwrap();
        let (mut sps, mut pps, mut frames) = (Vec::new(), Vec::new(), Vec::new());
        for c in COLOURS {
            let rgb: Vec<u8> = (0..w * h).flat_map(|_| c).collect();
            let yuv = YUVBuffer::from_rgb_source(RgbSliceU8::new(&rgb, (w, h)));
            let bits = enc.encode(&yuv).unwrap().to_vec();
            let mut nals = Vec::new();
            for n in h264::split_annex_b(&bits) {
                match h264::nal_type(n) {
                    h264::NAL_SPS => sps = n.to_vec(),
                    h264::NAL_PPS => pps = n.to_vec(),
                    _ => nals.push(n.to_vec()),
                }
            }
            frames.push(nals);
        }
        assert!(!sps.is_empty() && !pps.is_empty());
        assert_eq!(h264::nal_type(&frames[0][0]), h264::NAL_IDR);
        (sps, pps, frames)
    }

    fn check(transport: &str, in_band: bool, auth: Auth) {
        let (sps, pps, frames) = encode();
        let mut cfg = Config::new(Stream::H264 {
            sps,
            pps,
            frames,
            in_band,
        });
        cfg.auth = auth;
        let server = Server::start(cfg);
        let mut h = camera(server.url(auth != Auth::None), transport, true);
        let (images, encoded) = run_until(&mut h, Duration::from_secs(10), |i, _| i.len() >= 8);
        assert!(images.len() >= 8, "{transport}: {} images", images.len());
        for img in &images {
            assert_eq!((img.width, img.height), (64, 48));
            let px = &img.data[(24 * 64 + 32) * 3..][..3];
            assert!(
                COLOURS
                    .iter()
                    .any(|c| (0..3).all(|k| (c[k] as i32 - px[k] as i32).abs() < 30)),
                "{transport}: centre pixel {px:?} matches no source colour"
            );
        }
        // Every keyframe carries its parameter sets.
        let key = encoded
            .iter()
            .map(|e| h264::split_annex_b(&e.0))
            .find(|u| u.iter().any(|n| h264::nal_type(n) == h264::NAL_IDR))
            .unwrap();
        let types: Vec<u8> = key.iter().map(|n| h264::nal_type(n)).collect();
        assert_eq!(&types[..3], &[7, 8, 5]);
    }

    #[test]
    fn h264_over_tcp_with_digest_auth() {
        check("tcp", false, Auth::Digest);
    }

    #[test]
    fn h264_over_udp_with_in_band_parameter_sets() {
        check("udp", true, Auth::None);
    }
}
