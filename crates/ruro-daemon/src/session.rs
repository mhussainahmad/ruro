//! Recording and replay sessions coordinated by the daemon.
//!
//! Recording: every host writes its own segment (so no message crosses a
//! process boundary just to be recorded); when the recording stops the
//! daemon merges the segments into the requested file. Hosts restarted
//! mid-recording start a new segment.
//!
//! Replay: the daemon streams a recording and publishes each message of a
//! replayed node on that node's injection key. The host owning the node
//! mutes it (it stops stepping) and publishes the injected messages on its
//! outputs, so downstream nodes in any group receive them exactly as if the
//! node had sent them.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::{Context, Result, bail};
use ruro_proto::{DataFrame, HostCommand, PlaybackCommand, PlaybackStatus};
use ruro_record::{PlaybackControl, Player};
use zenoh::Wait;

use crate::State;

/// A recording in progress.
pub struct RecordSession {
    /// The merged file the client asked for.
    pub output: PathBuf,
    /// Where hosts write their segments.
    pub dir: PathBuf,
}

/// A replay in progress.
pub struct Playback {
    file: String,
    duration_s: f64,
    ctl: Arc<PlaybackControl>,
    thread: Option<JoinHandle<()>>,
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

pub fn start_recording(st: &Arc<State>, path: &Path) -> Result<String> {
    let mut rec = st.recording.lock();
    if let Some(r) = &*rec {
        bail!("already recording to {}", r.output.display());
    }
    if !path.is_absolute() {
        bail!("the recording path must be absolute (the daemon writes it)");
    }
    if let Some(parent) = path.parent()
        && !parent.is_dir()
    {
        bail!("directory {} does not exist", parent.display());
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
            // Undo: stop the hosts that did start.
            st.sup.lock().host_state.record_dir = None;
            for (g, _) in &segments {
                let _ = st.host_request(g, &HostCommand::Record { path: None });
            }
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e.context("starting the recording"));
        }
    }
    *rec = Some(RecordSession {
        output: path.to_owned(),
        dir,
    });
    tracing::info!("recording to {}", path.display());
    Ok(format!(
        "recording {} group(s) to {}",
        segments.len(),
        path.display()
    ))
}

/// Ends the recording, if any, and merges its segments. With `ask_hosts`
/// the running hosts are told to finish their segments first; otherwise the
/// hosts have already exited (which finishes their segments).
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
        for g in groups {
            if let Err(e) = st.host_request(&g, &HostCommand::Record { path: None }) {
                tracing::warn!("{e:#}");
            }
        }
    }
    let mut segments: Vec<PathBuf> = std::fs::read_dir(&session.dir)?
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
    if segments.is_empty() {
        let _ = std::fs::remove_dir_all(&session.dir);
        bail!("nothing was recorded");
    }
    let info = ruro_record::merge(&segments, &session.output, true)?;
    let _ = std::fs::remove_dir_all(&session.dir);
    let msg = format!(
        "recorded {} message(s) on {} channel(s) over {:.1} s to {}",
        info.messages,
        info.channels.len(),
        info.duration_s(),
        session.output.display()
    );
    tracing::info!("{msg}");
    Ok(Some(msg))
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
    }
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
    let local = |node: &str| {
        graph.nodes.contains_key(node)
            && graph
                .group_config(graph.group_of(node).unwrap_or_default())
                .machine
                .is_none_or(|m| m == st.name)
    };
    let replayed: BTreeSet<String> = player
        .info()
        .channels
        .iter()
        .filter(|c| ruro_record::is_ruro_encoding(&c.encoding))
        .filter_map(|c| ruro_record::split_topic(&c.topic).map(|(n, _)| n.to_owned()))
        .filter(|n| local(n))
        .collect();
    if replayed.is_empty() {
        bail!("no channel of {file} belongs to a node of the running graph");
    }
    let nodes: Vec<String> = replayed.iter().cloned().collect();
    let groups = {
        let mut sup = st.sup.lock();
        sup.host_state.muted = nodes.clone();
        sup.running_groups()
    };
    for g in &groups {
        st.host_request(
            g,
            &HostCommand::Mute {
                nodes: nodes.clone(),
            },
        )?;
    }

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
                if !replayed.contains(node) {
                    return;
                }
                if !publishers.contains_key(m.topic()) {
                    match thread_st
                        .session
                        .declare_publisher(thread_st.keys.inject(node, port))
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
    });
    tracing::info!("playing {file}, replaying {}", nodes.join(", "));
    Ok(format!(
        "playing {file} ({duration_s:.1} s), replaying {}",
        nodes.join(", ")
    ))
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
    let groups = {
        let mut sup = st.sup.lock();
        sup.host_state.muted.clear();
        sup.running_groups()
    };
    for g in groups {
        if let Err(e) = st.host_request(&g, &HostCommand::Mute { nodes: Vec::new() }) {
            tracing::warn!("{e:#}");
        }
    }
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
