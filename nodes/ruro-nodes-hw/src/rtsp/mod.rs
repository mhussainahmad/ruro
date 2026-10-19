//! RTSP network cameras and the `hw/rtsp-camera` node.
//!
//! A plain-Rust RTSP 1.0 client ([`client`]) receives H.264 (RFC 6184) or
//! Motion JPEG (RFC 2435) over RTP, interleaved on the RTSP connection or
//! over UDP. Frames are published encoded (`encoded`, for recording without
//! decoding) and, unless `decode` is off, as RGB images. MJPEG decodes with
//! the same decoder as the other cameras; H.264 decoding needs the `h264`
//! cargo feature (OpenH264, built from its bundled source).

pub mod auth;
pub mod client;
pub mod h264;
pub mod jpeg;
pub mod rtp;
pub mod sdp;

use std::net::{Shutdown, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;
use std::time::Duration;

use parking_lot::Mutex;
use ruro::prelude::*;

use crate::calibration::Calibration;
use crate::pixel::decode_jpeg;
use client::{RtspUrl, Session, Transport};
use sdp::Codec;

/// A received frame: the encoded access unit and, if decoded, the image.
struct Frame {
    encoded: Vec<u8>,
    image: Option<Image>,
}

/// Network camera streaming H.264 or Motion JPEG over RTSP. A background
/// thread holds the session, reconnects with backoff, and decodes; each step
/// publishes every frame received since the last on `encoded` and the newest
/// one on `image`.
///
/// `encoded` carries one frame per message: an H.264 access unit as an
/// Annex B byte stream (SPS and PPS repeated before every keyframe, so a
/// recording decodes from any keyframe), or a complete JPEG file.
#[derive(Node)]
#[node(kind = "hw/rtsp-camera", rate = 30)]
pub struct RtspCamera {
    #[output]
    image: Output<Image>,
    #[output]
    encoded: Output<Bytes>,
    /// Published with each image when `calibration` is set.
    #[output]
    camera_info: Output<CameraInfo>,
    /// Stream URL, e.g. `rtsp://user:password@192.168.1.64:554/stream1`.
    #[param(default = "rtsp://127.0.0.1:8554/stream")]
    url: String,
    /// `tcp` (RTP interleaved on the RTSP connection, works through NAT) or `udp`.
    #[param(default = "tcp")]
    transport: String,
    /// Connect and read timeout in seconds.
    #[param(default = 5.0, min = 0.1, max = 60)]
    timeout: f64,
    /// Decode frames to `image`. Off publishes only `encoded`.
    #[param(default = true)]
    decode: bool,
    /// ROS `camera_calibration` YAML file; empty for no `camera_info`.
    #[param(default = "")]
    calibration: String,
    /// Frame id of `camera_info`.
    #[param(default = "camera")]
    frame_id: String,
    calib: Calibration,
    frames: Option<Receiver<Frame>>,
    worker: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    conn: Arc<Mutex<Option<TcpStream>>>,
}

/// Per-session depacketizer and decoder.
enum Pipeline {
    H264 {
        depack: h264::Depacketizer,
        #[cfg(feature = "h264")]
        decoder: Option<h264::Decoder>,
    },
    Jpeg(jpeg::Depacketizer),
}

impl Pipeline {
    fn new(s: &Session, decode: bool) -> Result<Self> {
        Ok(match s.media.codec {
            Codec::H264 => {
                let ps = s.media.sprop_parameter_sets().map_err(|e| anyhow!(e))?;
                #[cfg(not(feature = "h264"))]
                if decode {
                    warn!(
                        "H.264 stream, but this build has no H.264 decoder (cargo feature \
                         ruro-nodes-hw/h264); publishing only `encoded`"
                    );
                }
                Pipeline::H264 {
                    depack: h264::Depacketizer::new(&ps),
                    #[cfg(feature = "h264")]
                    decoder: if decode {
                        Some(h264::Decoder::new().map_err(|e| anyhow!(e))?)
                    } else {
                        None
                    },
                }
            }
            _ => Pipeline::Jpeg(jpeg::Depacketizer::new()),
        })
    }

    /// Feeds one RTP packet; completed frames go to `out`.
    fn push(&mut self, pkt: &rtp::RtpPacket, decode: bool, out: &mut Vec<Frame>) {
        match self {
            #[cfg_attr(not(feature = "h264"), allow(unused_variables))]
            Pipeline::H264 {
                depack,
                #[cfg(feature = "h264")]
                decoder,
            } => {
                let mut aus = Vec::new();
                depack.push(pkt, &mut aus);
                for au in aus {
                    #[cfg(feature = "h264")]
                    let image = decoder.as_mut().and_then(|d| match d.decode(&au.data) {
                        Ok(img) => img,
                        Err(e) => {
                            warn!("H.264 decode: {e}");
                            None
                        }
                    });
                    #[cfg(not(feature = "h264"))]
                    let image = None;
                    out.push(Frame {
                        encoded: au.data,
                        image,
                    });
                }
            }
            Pipeline::Jpeg(depack) => {
                let mut frames = Vec::new();
                let dropped = depack.dropped;
                depack.push(pkt, &mut frames);
                if depack.dropped > dropped {
                    debug!(
                        "RTP JPEG frame dropped: {}",
                        depack.last_error.as_deref().unwrap_or("incomplete")
                    );
                }
                for f in frames {
                    let image = if decode {
                        match decode_jpeg(&f.data) {
                            Ok((width, height, data)) => Some(Image {
                                width,
                                height,
                                encoding: Encoding::Rgb8,
                                data,
                            }),
                            Err(e) => {
                                warn!("RTP JPEG frame: {e}");
                                None
                            }
                        }
                    } else {
                        None
                    };
                    out.push(Frame {
                        encoded: f.data,
                        image,
                    });
                }
            }
        }
    }
}

struct Worker {
    url: RtspUrl,
    transport: Transport,
    timeout: Duration,
    decode: bool,
    tx: SyncSender<Frame>,
    stop: Arc<AtomicBool>,
    conn: Arc<Mutex<Option<TcpStream>>>,
}

impl Worker {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Streams one session until it fails or the node stops. Returns
    /// whether any frame arrived.
    fn play(&self, s: &mut Session, got_frames: &mut bool) -> Result<()> {
        let mut pipeline = Pipeline::new(s, self.decode)?;
        let pt = s.media.payload_type;
        let mut frames = Vec::new();
        while !self.stopped() {
            let Some(packet) = s.next_packet()? else {
                continue;
            };
            let pkt = match rtp::parse(&packet) {
                Ok(p) if p.payload_type == pt => p,
                Ok(_) => continue,
                Err(e) => {
                    debug!("bad RTP packet: {e}");
                    continue;
                }
            };
            pipeline.push(&pkt, self.decode, &mut frames);
            for f in frames.drain(..) {
                *got_frames = true;
                match self.tx.try_send(f) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_)) => debug!("RTSP frame dropped: node too slow"),
                    Err(TrySendError::Disconnected(_)) => return Ok(()),
                }
            }
        }
        Ok(())
    }

    fn run(self) {
        let name = format!("{}:{}", self.url.host, self.url.port);
        let mut backoff = Duration::from_millis(250);
        while !self.stopped() {
            match Session::open(&self.url, self.transport, self.timeout) {
                Ok(mut s) => {
                    info!(
                        "RTSP {name}: playing {} ({:?})",
                        s.media.encoding, self.transport
                    );
                    *self.conn.lock() = s.stop_handle().ok();
                    let mut got_frames = false;
                    let r = self.play(&mut s, &mut got_frames);
                    *self.conn.lock() = None;
                    s.teardown();
                    if got_frames {
                        backoff = Duration::from_millis(250);
                    }
                    match r {
                        Err(e) if !self.stopped() => warn!("RTSP {name}: {e:#}"),
                        _ => {}
                    }
                }
                Err(e) => warn!("cannot open RTSP stream {name}: {e:#}"),
            }
            // Wait before reconnecting, doubling up to 10 s.
            let until = std::time::Instant::now() + backoff;
            while !self.stopped() && std::time::Instant::now() < until {
                std::thread::sleep(Duration::from_millis(20));
            }
            backoff = (backoff * 2).min(Duration::from_secs(10));
        }
    }
}

impl Node for RtspCamera {
    fn start(&mut self, _ctx: &mut Ctx) -> Result {
        let url = client::parse_url(&self.url)?;
        let transport = Transport::parse(&self.transport)?;
        self.calib = Calibration::open(&self.calibration, &self.frame_id)?;
        let (tx, rx) = sync_channel(64);
        self.stop.store(false, Ordering::Relaxed);
        let worker = Worker {
            url,
            transport,
            timeout: Duration::from_secs_f64(self.timeout),
            decode: self.decode,
            tx,
            stop: self.stop.clone(),
            conn: self.conn.clone(),
        };
        self.worker = Some(
            std::thread::Builder::new()
                .name("ruro-rtsp".into())
                .spawn(move || worker.run())?,
        );
        self.frames = Some(rx);
        Ok(())
    }

    fn step(&mut self, ctx: &mut Ctx) -> Result {
        let rx = self.frames.as_ref().context("not started")?;
        let mut newest = None;
        for f in rx.try_iter() {
            self.encoded.send(Bytes(f.encoded))?;
            if f.image.is_some() {
                newest = f.image;
            }
        }
        if let Some(img) = newest {
            self.calib
                .publish(&self.camera_info, img.width, img.height, ctx.now_ns())?;
            self.image.send(img)?;
        }
        Ok(())
    }

    fn stop(&mut self, _ctx: &mut Ctx) {
        self.stop.store(true, Ordering::Relaxed);
        // Unblock a read waiting for the next packet.
        if let Some(c) = self.conn.lock().take() {
            let _ = c.shutdown(Shutdown::Read);
        }
        self.frames = None;
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}
