use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;
use parking_lot::{Condvar, Mutex};

use crate::{Info, RecordedMessage, Recording};

/// Shared controls of a running [`Player`]: pause, seek, rate and stop can be
/// changed from any thread while [`Player::run`] streams messages.
pub struct PlaybackControl {
    state: Mutex<State>,
    cv: Condvar,
}

struct State {
    paused: bool,
    rate: f64,
    seek_to_ns: Option<u64>,
    stop: bool,
    finished: bool,
    /// Offset from the start of the recording of the last delivered message.
    position_ns: u64,
    /// Length of the recording, once [`Player::run`] has started. Seeks and
    /// positions are clamped to it.
    duration_ns: Option<u64>,
    /// Bumped by every command so a pacing wait can notice it.
    epoch: u64,
}

/// Slowest and fastest supported playback rates.
const RATE_RANGE: (f64, f64) = (0.01, 1000.0);

impl PlaybackControl {
    pub fn new(rate: f64) -> Self {
        Self {
            state: Mutex::new(State {
                paused: false,
                rate: clamp_rate(rate),
                seek_to_ns: None,
                stop: false,
                finished: false,
                position_ns: 0,
                duration_ns: None,
                epoch: 0,
            }),
            cv: Condvar::new(),
        }
    }

    fn update(&self, f: impl FnOnce(&mut State)) {
        let mut s = self.state.lock();
        f(&mut s);
        s.epoch += 1;
        self.cv.notify_all();
    }

    pub fn pause(&self) {
        self.update(|s| s.paused = true);
    }

    pub fn resume(&self) {
        self.update(|s| s.paused = false);
    }

    /// Jumps to `position_s` seconds from the start of the recording. A
    /// position past the end is taken as the end.
    pub fn seek(&self, position_s: f64) {
        let ns = if position_s.is_finite() {
            (position_s.max(0.0) * 1e9) as u64
        } else if position_s > 0.0 {
            u64::MAX
        } else {
            0
        };
        self.update(|s| {
            let ns = s.duration_ns.map_or(ns, |d| ns.min(d));
            s.seek_to_ns = Some(ns);
            s.position_ns = ns;
        });
    }

    /// Playback speed; 1.0 is real time. Clamped to [0.01, 1000].
    pub fn set_rate(&self, rate: f64) {
        self.update(|s| s.rate = clamp_rate(rate));
    }

    pub fn stop(&self) {
        self.update(|s| s.stop = true);
    }

    pub fn position_s(&self) -> f64 {
        self.state.lock().position_ns as f64 / 1e9
    }

    pub fn rate(&self) -> f64 {
        self.state.lock().rate
    }

    pub fn is_paused(&self) -> bool {
        self.state.lock().paused
    }

    /// True once [`Player::run`] has returned.
    pub fn is_finished(&self) -> bool {
        self.state.lock().finished
    }
}

fn clamp_rate(rate: f64) -> f64 {
    if rate.is_finite() {
        rate.clamp(RATE_RANGE.0, RATE_RANGE.1)
    } else {
        1.0
    }
}

/// Streams a recording back in real time (or faster, or slower).
pub struct Player {
    recording: Recording,
    info: Info,
}

impl Player {
    pub fn open(path: &Path) -> Result<Self> {
        let recording = Recording::open(path)?;
        let info = recording.info()?;
        Ok(Self { recording, info })
    }

    pub fn info(&self) -> &Info {
        &self.info
    }

    pub fn path(&self) -> &Path {
        self.recording.path()
    }

    /// Delivers every message to `sink` at the time it was recorded, scaled by
    /// the playback rate. Returns at the end of the recording or when
    /// [`PlaybackControl::stop`] is called.
    pub fn run(&self, ctl: &PlaybackControl, mut sink: impl FnMut(&RecordedMessage)) -> Result<()> {
        ctl.update(|s| {
            let d = self.info.end_ns.saturating_sub(self.info.start_ns);
            s.duration_ns = Some(d);
            s.position_ns = s.position_ns.min(d);
            if let Some(to) = &mut s.seek_to_ns {
                *to = (*to).min(d);
            }
        });
        let result = self.run_inner(ctl, &mut sink);
        ctl.state.lock().finished = true;
        result
    }

    fn run_inner(
        &self,
        ctl: &PlaybackControl,
        sink: &mut dyn FnMut(&RecordedMessage),
    ) -> Result<()> {
        let start = self.info.start_ns;
        let mut messages: Box<dyn Iterator<Item = Result<RecordedMessage>>> =
            Box::new(self.recording.messages()?);
        // Wall time at which the recording offset `.1` was (or is) played.
        let mut anchor: Option<(Instant, u64)> = None;
        let mut pending: Option<RecordedMessage> = None;
        let mut skip_before: Option<u64> = None;

        loop {
            {
                let mut s = ctl.state.lock();
                if let Some(to) = s.seek_to_ns.take() {
                    messages = Box::new(self.recording.messages()?);
                    pending = None;
                    skip_before = Some(start.saturating_add(to));
                    anchor = None;
                }
                if s.paused {
                    while s.paused && !s.stop && s.seek_to_ns.is_none() {
                        ctl.cv.wait(&mut s);
                    }
                    anchor = None;
                }
                if s.stop {
                    return Ok(());
                }
                if s.seek_to_ns.is_some() {
                    continue;
                }
            }

            let msg = match pending.take() {
                Some(m) => m,
                None => match messages.next() {
                    Some(m) => m?,
                    None => return Ok(()),
                },
            };
            if let Some(t) = skip_before {
                if msg.log_time < t {
                    continue;
                }
                skip_before = None;
            }
            let offset = msg.log_time.saturating_sub(start);

            let mut s = ctl.state.lock();
            let (wall, base) = *anchor.get_or_insert((Instant::now(), offset));
            let delay = offset.saturating_sub(base) as f64 / s.rate;
            let due = wall + Duration::from_nanos(delay as u64);
            let epoch = s.epoch;
            while Instant::now() < due && s.epoch == epoch {
                ctl.cv.wait_until(&mut s, due);
            }
            if s.epoch != epoch {
                // A command arrived while waiting: re-anchor at the current
                // position so a rate change takes effect from here on.
                anchor = Some((Instant::now(), s.position_ns.min(offset)));
                pending = Some(msg);
                continue;
            }
            s.position_ns = offset;
            drop(s);
            sink(&msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::*;
    use crate::McapWriter;

    /// 50 messages, 10 ms apart, payload = index.
    fn recording(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ruro-player-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let mut w = McapWriter::create(&path, false).unwrap();
        let c = w
            .channel("src.value", "std/Scalar", crate::ENCODING_POSTCARD)
            .unwrap();
        for i in 0..50u64 {
            w.write(c, i + 1, 1_000_000_000 + i * 10_000_000, &[i as u8])
                .unwrap();
        }
        w.finish().unwrap();
        path
    }

    #[test]
    fn plays_in_real_time_and_in_order() {
        let player = Player::open(&recording("rt.mcap")).unwrap();
        assert!((player.info().duration_s() - 0.49).abs() < 1e-9);
        let ctl = PlaybackControl::new(1.0);
        let mut got = Vec::new();
        let t = Instant::now();
        player.run(&ctl, |m| got.push(m.data[0])).unwrap();
        let took = t.elapsed();
        assert_eq!(got, (0..50).collect::<Vec<u8>>());
        assert!(took >= Duration::from_millis(480), "{took:?}");
        assert!(took < Duration::from_millis(900), "{took:?}");
        assert!(ctl.is_finished());
    }

    #[test]
    fn rate_scales_time() {
        let player = Player::open(&recording("fast.mcap")).unwrap();
        let ctl = PlaybackControl::new(10.0);
        let t = Instant::now();
        let mut n = 0;
        player.run(&ctl, |_| n += 1).unwrap();
        assert_eq!(n, 50);
        assert!(
            t.elapsed() < Duration::from_millis(250),
            "{:?}",
            t.elapsed()
        );
    }

    #[test]
    fn seek_pause_and_stop() {
        let player = Arc::new(Player::open(&recording("seek.mcap")).unwrap());
        let ctl = Arc::new(PlaybackControl::new(1.0));
        ctl.seek(0.3);
        let got = Arc::new(Mutex::new(Vec::new()));
        let (p, c, g) = (player.clone(), ctl.clone(), got.clone());
        let t = std::thread::spawn(move || p.run(&c, |m| g.lock().push(m.data[0])).unwrap());
        std::thread::sleep(Duration::from_millis(60));
        ctl.pause();
        std::thread::sleep(Duration::from_millis(30));
        let paused_at = got.lock().len();
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            got.lock().len(),
            paused_at,
            "messages delivered while paused"
        );
        assert!(ctl.is_paused());
        ctl.resume();
        ctl.set_rate(5.0);
        t.join().unwrap();
        let got = got.lock();
        assert_eq!(got[0], 30, "seek must skip the first 0.3 s");
        assert_eq!(*got.last().unwrap(), 49);
        assert!(got.windows(2).all(|w| w[1] == w[0] + 1));

        let ctl = PlaybackControl::new(1.0);
        ctl.stop();
        let mut n = 0;
        player.run(&ctl, |_| n += 1).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn seeking_past_the_end_stops_at_the_end() {
        let player = Player::open(&recording("past-end.mcap")).unwrap();
        let ctl = PlaybackControl::new(1.0);
        // Before the player knows the length, and while it runs.
        ctl.seek(99.0);
        let mut got = Vec::new();
        player
            .run(&ctl, |m| {
                got.push(m.data[0]);
                if got.len() == 1 {
                    ctl.seek(f64::INFINITY);
                }
            })
            .unwrap();
        assert_eq!(got, [49, 49], "only the last message is at the end");
        assert!(
            (ctl.position_s() - 0.49).abs() < 1e-9,
            "{}",
            ctl.position_s()
        );
        ctl.seek(1e6);
        assert!(ctl.position_s() <= player.info().duration_s());
    }
}
