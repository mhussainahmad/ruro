//! Recording and replay sessions coordinated by the daemon.
//!
//! Recording: every host writes its own segment (so no message crosses a
//! process boundary just to be recorded); when the recording stops the
//! daemon merges the segments into the requested file. Hosts restarted
//! mid-recording start a new segment. Peers running groups of the graph
//! record theirs the same way; when the recording stops, each merges its
//! segments and the daemon fetches the result and merges it in. Replies
//! name the groups recorded on every machine, and the machines that could
//! not record. Log times are each machine's clock: when the recording
//! starts the daemon measures every peer's clock offset, and shifts the
//! peer's part onto its own clock if the offset is larger than the
//! measurement can resolve.
//!
//! Replay: the daemon streams a recording and publishes each message of a
//! replayed node on that node's injection key, under the daemon that runs
//! the node. The host owning the node mutes it (it stops stepping) and
//! publishes the injected messages on its outputs, so downstream nodes in
//! any group receive them exactly as if the node had sent them.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::{Context, Result, bail};
use ruro_proto::{Ctl, DataFrame, HostCommand, Keys, PlaybackCommand, PlaybackStatus};
use ruro_record::{PlaybackControl, Player};
use zenoh::Wait;

use crate::State;

/// A recording in progress.
pub struct RecordSession {
    /// Where the recording ends up.
    pub target: RecordTarget,
    /// Where hosts write their segments.
    pub dir: PathBuf,
    /// Peers recording their groups for this recording, with their clock
    /// offsets when they could be measured.
    pub peers: Vec<(String, Option<ClockOffset>)>,
}

/// How far a peer's clock is ahead of this daemon's, measured NTP-style:
/// the peer's reading is compared with the middle of the round trip that
/// fetched it, so the error is at most half the round trip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockOffset {
    pub offset_ns: i64,
    pub uncertainty_ns: u64,
}

impl ClockOffset {
    /// The best estimate from `(sent, peer, received)` readings: the one
    /// with the shortest round trip.
    pub fn estimate(samples: &[(u64, u64, u64)]) -> Option<Self> {
        samples
            .iter()
            .filter(|(t0, _, t2)| t2 >= t0)
            .min_by_key(|(t0, _, t2)| t2 - t0)
            .map(|&(t0, t1, t2)| Self {
                offset_ns: (t1 as i128 - (t0 as i128 + t2 as i128) / 2) as i64,
                uncertainty_ns: (t2 - t0).div_ceil(2),
            })
    }

    /// What to add to the peer's log times to put them on this daemon's
    /// clock. Zero when the clocks agree as far as can be measured (for
    /// example when both follow NTP or PTP more closely than the round trip
    /// shows).
    pub fn correction_ns(&self) -> i64 {
        if self.offset_ns.unsigned_abs() <= self.uncertainty_ns {
            0
        } else {
            -self.offset_ns
        }
    }
}

/// How long a peer gets to show it is still there before its part of a
/// recording is fetched.
const PEER_PROBE: std::time::Duration = std::time::Duration::from_secs(2);

/// Round trips used to measure a peer's clock.
const CLOCK_SAMPLES: usize = 8;

/// Measures how far `peer`'s clock is ahead of this daemon's.
fn measure_clock(st: &State, peer: &str) -> Result<ClockOffset> {
    let mut samples = Vec::with_capacity(CLOCK_SAMPLES);
    for _ in 0..CLOCK_SAMPLES {
        let t0 = ruro_msg::now_ns();
        let reply = st.peer_request(peer, Ctl::Clock, "")?;
        let t2 = ruro_msg::now_ns();
        let t1: u64 = reply.trim().parse().context("malformed clock reply")?;
        samples.push((t0, t1, t2));
    }
    ClockOffset::estimate(&samples).context("the clock went backwards")
}

/// `+1.2 ms`, for replies.
fn millis(ns: i64) -> String {
    format!("{:+.1} ms", ns as f64 / 1e6)
}

/// Where a recording ends up.
pub enum RecordTarget {
    /// The merged file a client asked for.
    File(PathBuf),
    /// Part of the recording of the daemon that forwarded groups here: the
    /// segments are merged and kept until that daemon fetches them.
    For(String),
}

impl RecordSession {
    /// What the daemon's status shows.
    pub fn describe(&self) -> String {
        match &self.target {
            RecordTarget::File(p) => p.display().to_string(),
            RecordTarget::For(origin) => format!("part of the recording of `{origin}`"),
        }
    }
}

/// A replay in progress.
pub struct Playback {
    file: String,
    duration_s: f64,
    ctl: Arc<PlaybackControl>,
    thread: Option<JoinHandle<()>>,
    /// Machines whose replayed nodes are muted.
    machines: Vec<String>,
}

impl Playback {
    pub fn status(&self) -> PlaybackStatus {
        PlaybackStatus {
            file: self.file.clone(),
            position_s: self.ctl.position_s(),
            duration_s: self.duration_s,
            paused: self.ctl.is_paused(),
            rate: self.ctl.rate(),
        }
    }
}

/// Handles `Ctl::Record`.
pub fn record(st: &Arc<State>, text: &str) -> Result<String> {
    let text = text.trim();
    if text.is_empty() {
        return finish_recording(st, true)?.context("not recording");
    }
    if let Some(origin) = text.strip_prefix("from ") {
        let groups = start_segments(st, RecordTarget::For(origin.to_owned()), Vec::new())?;
        return Ok(format!(
            "recording {} group(s) for `{origin}`: {}",
            groups.len(),
            groups.join(", ")
        ));
    }
    if let Some(origin) = text.strip_prefix("stop-from ") {
        let forwarded = matches!(
            st.recording.lock().as_ref().map(|r| &r.target),
            Some(RecordTarget::For(o)) if o == origin
        );
        if forwarded {
            finish_recording(st, true)?;
        }
        let part = st
            .record_part
            .lock()
            .take_if(|(o, _)| o == origin)
            .map(|(_, part)| part)
            .with_context(|| format!("not recording for `{origin}`"))?;
        return Ok(serde_json::to_string(&part)?);
    }
    start_recording(st, Path::new(text))
}

fn start_recording(st: &Arc<State>, path: &Path) -> Result<String> {
    if !path.is_absolute() {
        bail!("the recording path must be absolute (the daemon writes it)");
    }
    if let Some(parent) = path.parent()
        && !parent.is_dir()
    {
        bail!("directory {} does not exist", parent.display());
    }
    if let Some(r) = &*st.recording.lock() {
        bail!("already recording ({})", r.describe());
    }
    // Groups on other machines are recorded there, and collected at the end.
    let mut recording_peers = Vec::new();
    let mut parts = Vec::new();
    let mut failed = Vec::new();
    for p in st.peers.lock().clone() {
        match st.peer_request(&p, Ctl::Record, &format!("from {}", st.name)) {
            Ok(reply) => {
                // Log times are each machine's clock: measured now, the
                // peer's part is corrected when it is merged in.
                let clock = measure_clock(st, &p)
                    .inspect_err(|e| tracing::warn!("cannot measure the clock of `{p}`: {e:#}"))
                    .ok();
                recording_peers.push((p.clone(), clock));
                parts.push(format!("on `{p}`: {}", after_colon(&reply)));
            }
            Err(e) => failed.push(format!("the groups on `{p}` ({e:#})")),
        }
    }
    let groups = match start_segments(
        st,
        RecordTarget::File(path.to_owned()),
        recording_peers.clone(),
    ) {
        Ok(g) => g,
        Err(e) => {
            for (p, _) in &recording_peers {
                let _ = st.peer_request(p, Ctl::Record, &format!("stop-from {}", st.name));
            }
            return Err(e);
        }
    };
    let total = groups.len()
        + parts
            .iter()
            .map(|p| after_colon(p).split(", ").filter(|g| !g.is_empty()).count())
            .sum::<usize>();
    tracing::info!("recording to {}", path.display());
    let mut reply = format!(
        "recording {total} group(s) to {}: {}",
        path.display(),
        groups.join(", ")
    );
    for p in parts {
        reply.push_str(&format!("; {p}"));
    }
    if !failed.is_empty() {
        reply.push_str(&format!("; not recorded: {}", failed.join(", ")));
    }
    Ok(reply)
}

/// The text after the first `: ` of a reply.
fn after_colon(reply: &str) -> &str {
    reply.split_once(": ").map_or("", |(_, rest)| rest)
}

/// Has every running group of this daemon record its outputs into a
/// segment. Returns the groups.
fn start_segments(
    st: &Arc<State>,
    target: RecordTarget,
    peers: Vec<(String, Option<ClockOffset>)>,
) -> Result<Vec<String>> {
    let mut rec = st.recording.lock();
    if let Some(r) = &*rec {
        bail!("already recording ({})", r.describe());
    }
    let (dir, segments) = {
        let mut sup = st.sup.lock();
        if sup.graph().is_none() {
            bail!("no graph is running");
        }
        let dir = sup
            .dir()
            .join(format!("record-{}", ruro_msg::now_ns() / 1_000_000));
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        sup.host_state.record_dir = Some(dir.clone());
        let segments: Vec<(String, PathBuf)> = sup
            .running_groups()
            .into_iter()
            .filter_map(|g| sup.next_segment(&g).map(|s| (g, s)))
            .collect();
        (dir, segments)
    };
    for (group, segment) in &segments {
        let cmd = HostCommand::Record {
            path: Some(segment.display().to_string()),
        };
        if let Err(e) = st.host_request(group, &cmd) {
            st.sup.lock().host_state.record_dir = None;
            for (g, _) in &segments {
                let _ = st.host_request(g, &HostCommand::Record { path: None });
            }
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e.context("starting the recording"));
        }
    }
    *rec = Some(RecordSession { target, dir, peers });
    Ok(segments.into_iter().map(|(g, _)| g).collect())
}

/// Stops the recording, if any, and merges what was recorded: into the
/// requested file, including the parts of peers, or, for a recording a
/// peer asked for, into a file that peer fetches.
pub fn finish_recording(st: &Arc<State>, ask_hosts: bool) -> Result<Option<String>> {
    let Some(session) = st.recording.lock().take() else {
        return Ok(None);
    };
    let groups = {
        let mut sup = st.sup.lock();
        sup.host_state.record_dir = None;
        sup.running_groups()
    };
    if ask_hosts {
        for g in &groups {
            if let Err(e) = st.host_request(g, &HostCommand::Record { path: None }) {
                tracing::warn!("{e:#}");
            }
        }
    }
    let segments = readable_segments(&session.dir)?;
    let recorded: BTreeSet<String> = segments
        .iter()
        .filter_map(|p| {
            p.file_stem()?
                .to_str()?
                .rsplit_once('-')
                .map(|(g, _)| g.to_owned())
        })
        .collect();
    match session.target {
        RecordTarget::For(origin) => {
            let part = if segments.is_empty() {
                ruro_proto::RecordedPart {
                    file: None,
                    size: 0,
                    groups: Vec::new(),
                }
            } else {
                let file = st
                    .sup
                    .lock()
                    .dir()
                    .join(format!("record-for-{origin}.mcap"));
                ruro_record::merge(&segments, &file, true)?;
                ruro_proto::RecordedPart {
                    size: std::fs::metadata(&file)?.len(),
                    file: Some(file.display().to_string()),
                    groups: recorded.into_iter().collect(),
                }
            };
            let _ = std::fs::remove_dir_all(&session.dir);
            let msg = format!("recorded {} group(s) for `{origin}`", part.groups.len());
            *st.record_part.lock() = Some((origin, part));
            Ok(Some(msg))
        }
        RecordTarget::File(output) => {
            let mut notes = Vec::new();
            let mut inputs: Vec<(PathBuf, i64)> = segments.into_iter().map(|s| (s, 0)).collect();
            for (p, clock) in &session.peers {
                match fetch_part(st, p, &session.dir) {
                    Ok(Some((file, groups))) => {
                        let mut note = format!("from `{p}`: {}", groups.join(", "));
                        let correction = clock.map_or(0, |c| c.correction_ns());
                        match clock {
                            None => note.push_str(" (clock offset unknown, not corrected)"),
                            Some(_) if correction == 0 => {}
                            Some(c) => note.push_str(&format!(
                                " (clock {} ahead, corrected to within {})",
                                millis(c.offset_ns),
                                millis(c.uncertainty_ns as i64).trim_start_matches('+')
                            )),
                        }
                        inputs.push((file, correction));
                        notes.push(note);
                    }
                    Ok(None) => notes.push(format!("nothing recorded on `{p}`")),
                    Err(e) => notes.push(format!("not recorded: the groups on `{p}` ({e:#})")),
                }
            }
            if inputs.is_empty() {
                let _ = std::fs::remove_dir_all(&session.dir);
                bail!("nothing was recorded");
            }
            let info = ruro_record::merge_shifted(&inputs, &output, true)?;
            let _ = std::fs::remove_dir_all(&session.dir);
            let mut msg = format!(
                "recorded {} message(s) on {} channel(s) over {:.1} s to {}",
                info.messages,
                info.channels.len(),
                info.duration_s(),
                output.display()
            );
            for n in notes {
                msg.push_str(&format!("; {n}"));
            }
            tracing::info!("{msg}");
            Ok(Some(msg))
        }
    }
}

/// The segments in `dir` that can be read.
fn readable_segments(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut segments: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "mcap"))
        .filter(
            |p| match ruro_record::Recording::open(p).and_then(|r| r.messages().map(|_| ())) {
                Ok(()) => true,
                Err(e) => {
                    tracing::warn!("skipping unreadable segment {}: {e:#}", p.display());
                    false
                }
            },
        )
        .collect();
    segments.sort();
    Ok(segments)
}

/// Has `peer` stop its part of the recording and copies it into `dir`.
/// Returns the copy and the groups it holds, or `None` if the peer
/// recorded nothing.
fn fetch_part(st: &State, peer: &str, dir: &Path) -> Result<Option<(PathBuf, Vec<String>)>> {
    use std::io::Write;
    // A machine that went away would hold the reply for the whole request
    // timeout, and with it the client, which waits no longer than that.
    ruro_proto::request(&st.session, &Keys::new(peer), Ctl::Ping, "", PEER_PROBE)
        .context("unreachable")?;
    let body = st.peer_request(peer, Ctl::Record, &format!("stop-from {}", st.name))?;
    let part: ruro_proto::RecordedPart = serde_json::from_str(&body).context("malformed reply")?;
    let Some(file) = part.file else {
        return Ok(None);
    };
    let copy = dir.join(format!("peer-{peer}.mcap"));
    let mut out = std::fs::File::create(&copy)?;
    let key = Keys::new(peer).ctl(Ctl::RecordPart);
    let mut offset = 0;
    loop {
        let req = ruro_proto::PartRequest {
            file: file.clone(),
            offset,
        };
        let chunk = ruro_proto::request_raw(
            &st.session,
            &key,
            serde_json::to_vec(&req)?,
            crate::REQUEST_TIMEOUT,
        )
        .with_context(|| format!("fetching the recording from `{peer}`"))?;
        if chunk.is_empty() {
            break;
        }
        out.write_all(&chunk)?;
        offset += chunk.len() as u64;
    }
    anyhow::ensure!(
        offset == part.size,
        "the recording from `{peer}` is {offset} bytes, expected {}",
        part.size
    );
    Ok(Some((copy, part.groups)))
}

/// Handles `Ctl::RecordPart`: a chunk of the part of a recording kept for
/// the daemon that asked for it. The file is removed once it has been read
/// to the end.
pub fn serve_part(st: &State, payload: &[u8]) -> Result<Vec<u8>> {
    use std::io::{Read, Seek};
    let req: ruro_proto::PartRequest = serde_json::from_slice(payload).context("bad request")?;
    // Only files this daemon merged for a peer are served.
    let dir = st.sup.lock().dir().to_owned();
    let path = PathBuf::from(&req.file);
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if path.parent() != Some(dir.as_path())
        || !name.starts_with("record-for-")
        || !name.ends_with(".mcap")
    {
        bail!("no such recording part");
    }
    let mut f = std::fs::File::open(&path).context("no such recording part")?;
    f.seek(std::io::SeekFrom::Start(req.offset))?;
    let mut chunk = Vec::with_capacity(ruro_proto::PART_CHUNK);
    f.take(ruro_proto::PART_CHUNK as u64)
        .read_to_end(&mut chunk)?;
    if chunk.is_empty() {
        let _ = std::fs::remove_file(&path);
    }
    Ok(chunk)
}

pub fn playback(st: &Arc<State>, cmd: PlaybackCommand) -> Result<String> {
    let with = |f: &dyn Fn(&PlaybackControl)| -> Result<String> {
        let p = st.playback.lock();
        let p = p.as_ref().context("nothing is playing")?;
        f(&p.ctl);
        Ok(format!("{:.1}/{:.1} s", p.ctl.position_s(), p.duration_s))
    };
    match cmd {
        PlaybackCommand::Start { file, rate } => start_playback(st, &file, rate),
        PlaybackCommand::Pause => with(&|c| c.pause()),
        PlaybackCommand::Resume => with(&|c| c.resume()),
        PlaybackCommand::Seek { position_s } => with(&|c| c.seek(position_s)),
        PlaybackCommand::SetRate { rate } => with(&|c| c.set_rate(rate)),
        PlaybackCommand::Stop => {
            if stop_playback(st) {
                Ok("playback stopped".into())
            } else {
                bail!("nothing is playing")
            }
        }
        PlaybackCommand::Mute { nodes } => {
            mute_here(st, &nodes)?;
            Ok(format!("{} node(s) replayed on `{}`", nodes.len(), st.name))
        }
    }
}

/// Mutes `nodes` (those of them that run here) on this daemon's hosts,
/// also when they restart; empty resumes every node.
fn mute_here(st: &State, nodes: &[String]) -> Result<()> {
    let groups = {
        let mut sup = st.sup.lock();
        sup.host_state.muted = nodes.to_vec();
        sup.running_groups()
    };
    for g in &groups {
        st.host_request(
            g,
            &HostCommand::Mute {
                nodes: nodes.to_vec(),
            },
        )?;
    }
    Ok(())
}

fn start_playback(st: &Arc<State>, file: &str, rate: f64) -> Result<String> {
    stop_playback(st);
    let player = Player::open(Path::new(file))?;
    let graph = st
        .sup
        .lock()
        .graph()
        .cloned()
        .context("no graph is running; deploy one first")?;
    // Each replayed node, with the machine that runs it.
    let machine_of = |node: &str| -> Option<String> {
        let group = graph.group_of(node)?;
        Some(
            graph
                .group_config(group)
                .machine
                .unwrap_or_else(|| st.name.clone()),
        )
    };
    let replayed: BTreeMap<String, String> = player
        .info()
        .channels
        .iter()
        .filter(|c| ruro_record::is_ruro_encoding(&c.encoding))
        .filter_map(|c| ruro_record::split_topic(&c.topic).map(|(n, _)| n.to_owned()))
        .filter_map(|n| machine_of(&n).map(|m| (n, m)))
        .collect();
    if replayed.is_empty() {
        bail!("no channel of {file} belongs to a node of the running graph");
    }
    let nodes: Vec<String> = replayed.keys().cloned().collect();
    let machines: BTreeSet<String> = replayed.values().cloned().collect();
    // Every machine mutes its replayed nodes; a machine that cannot is
    // left out of the replay rather than fighting it.
    let mut skipped = Vec::new();
    let mut muted = Vec::new();
    for m in &machines {
        let r = if *m == st.name {
            mute_here(st, &nodes)
        } else {
            let cmd = PlaybackCommand::Mute {
                nodes: nodes.clone(),
            };
            st.peer_request(m, Ctl::Playback, &serde_json::to_string(&cmd)?)
                .map(|_| ())
        };
        match r {
            Ok(()) => muted.push(m.clone()),
            Err(e) if *m == st.name => {
                unmute(st, &muted);
                return Err(e);
            }
            Err(e) => skipped.push(format!("the nodes on `{m}` ({e:#})")),
        }
    }
    let replayed: BTreeMap<String, Keys> = replayed
        .into_iter()
        .filter(|(_, m)| muted.contains(m))
        .map(|(n, m)| (n, Keys::new(&m)))
        .collect();
    if replayed.is_empty() {
        bail!("cannot replay {file}: {}", skipped.join(", "));
    }
    let nodes: Vec<String> = replayed.keys().cloned().collect();

    let ctl = Arc::new(PlaybackControl::new(rate));
    let duration_s = player.info().duration_s();
    let (thread_ctl, thread_st) = (ctl.clone(), st.clone());
    let thread = std::thread::Builder::new()
        .name("ruro-playback".into())
        .spawn(move || {
            let mut publishers: HashMap<String, zenoh::pubsub::Publisher<'static>> = HashMap::new();
            let result = player.run(&thread_ctl, |m| {
                let Some((node, port)) = ruro_record::split_topic(m.topic()) else {
                    return;
                };
                // Published to the daemon that runs the node.
                let Some(keys) = replayed.get(node) else {
                    return;
                };
                if !publishers.contains_key(m.topic()) {
                    match thread_st
                        .session
                        .declare_publisher(keys.inject(node, port))
                        .congestion_control(zenoh::qos::CongestionControl::Block)
                        .wait()
                    {
                        Ok(p) => {
                            publishers.insert(m.topic().to_owned(), p);
                        }
                        Err(e) => return tracing::warn!("cannot replay {}: {e}", m.topic()),
                    }
                }
                let frame = DataFrame::encode(m.seq as u64, ruro_msg::now_ns(), &m.data, false);
                let _ = publishers[m.topic()].put(frame).wait();
            });
            if let Err(e) = result {
                tracing::warn!("playback failed: {e:#}");
            }
        })?;
    *st.playback.lock() = Some(Playback {
        file: file.to_owned(),
        duration_s,
        ctl,
        thread: Some(thread),
        machines: muted,
    });
    tracing::info!("playing {file}, replaying {}", nodes.join(", "));
    let mut reply = format!(
        "playing {file} ({duration_s:.1} s), replaying {}",
        nodes.join(", ")
    );
    if !skipped.is_empty() {
        reply.push_str(&format!("; not replayed: {}", skipped.join(", ")));
    }
    Ok(reply)
}

/// Lets the replayed nodes on `machines` run again.
fn unmute(st: &State, machines: &[String]) {
    for m in machines {
        let r = if *m == st.name {
            mute_here(st, &[])
        } else {
            serde_json::to_string(&PlaybackCommand::Mute { nodes: Vec::new() })
                .map_err(Into::into)
                .and_then(|cmd| st.peer_request(m, Ctl::Playback, &cmd).map(|_| ()))
        };
        if let Err(e) = r {
            tracing::warn!("{e:#}");
        }
    }
}

/// Stops the replay, if any, and lets the replayed nodes run again.
/// Returns whether something was playing.
pub fn stop_playback(st: &Arc<State>) -> bool {
    let Some(mut p) = st.playback.lock().take() else {
        return false;
    };
    p.ctl.stop();
    if let Some(t) = p.thread.take() {
        let _ = t.join();
    }
    unmute(st, &p.machines);
    tracing::info!("playback of {} ended", p.file);
    true
}

/// Cleans up a replay that reached the end of its recording.
pub fn reap_playback(st: &Arc<State>) {
    let finished = st
        .playback
        .lock()
        .as_ref()
        .is_some_and(|p| p.ctl.is_finished());
    if finished {
        stop_playback(st);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_offsets_come_from_the_shortest_round_trip() {
        // The peer runs 5 ms ahead; one sample was delayed on the way back.
        let ms: u64 = 1_000_000;
        let samples = [
            (100 * ms, 106 * ms, 102 * ms),
            (200 * ms, 205 * ms + ms / 10, 200 * ms + ms / 5),
            (300 * ms, 305 * ms, 340 * ms),
        ];
        let c = ClockOffset::estimate(&samples).unwrap();
        assert_eq!(c.offset_ns, 5_000_000);
        assert_eq!(c.uncertainty_ns, ms / 10);
        assert_eq!(c.correction_ns(), -5_000_000);
        assert_eq!(millis(c.offset_ns), "+5.0 ms");

        // Clocks that agree within the round trip are left alone.
        let close = ClockOffset::estimate(&[(1000, 1700, 2000)]).unwrap();
        assert_eq!((close.offset_ns, close.uncertainty_ns), (200, 500));
        assert_eq!(close.correction_ns(), 0);
        // A peer behind this daemon.
        let behind = ClockOffset::estimate(&[(10 * ms, 7 * ms, 10 * ms + 2)]).unwrap();
        assert_eq!(behind.correction_ns(), 3_000_001);
        assert!(ClockOffset::estimate(&[(5, 1, 4)]).is_none());
    }
}
