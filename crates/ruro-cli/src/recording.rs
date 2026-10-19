//! `ruro record`, `play`, `playback`, `info` and `bag`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use ruro_proto::{Ctl, PlaybackCommand};

use crate::Client;

#[derive(Subcommand)]
pub enum PlaybackCmd {
    Pause,
    Resume,
    /// Jump to a position, in seconds from the start of the recording.
    Seek {
        position_s: f64,
    },
    /// Change the speed; 1 is real time.
    Rate {
        rate: f64,
    },
    Stop,
}

#[derive(Subcommand)]
pub enum BagCmd {
    /// Convert a ROS 2 bag (rosbag2 directory or .mcap file, MCAP storage)
    /// into a ruro recording.
    Import {
        /// rosbag2 directory or .mcap file.
        src: PathBuf,
        /// Output file (default: `<src>.ruro.mcap`).
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Name a topic's channel, e.g. `/camera/image_raw=camera.image`
        /// (so replay feeds the graph's `camera` node). Repeatable.
        #[arg(long = "map", value_parser = parse_mapping)]
        map: Vec<(String, String)>,
        /// Keep every topic as ros2/Any (the exact CDR bytes), even topics
        /// whose type has a ruro equivalent.
        #[arg(long)]
        raw: bool,
    },
}

fn parse_mapping(s: &str) -> Result<(String, String), String> {
    let (topic, channel) = s
        .split_once('=')
        .ok_or_else(|| format!("expected TOPIC=node.port, got `{s}`"))?;
    if ruro_record::split_topic(channel).is_none() {
        return Err(format!("`{channel}` is not of the form node.port"));
    }
    Ok((topic.to_owned(), channel.to_owned()))
}

fn absolute(path: &Path) -> Result<PathBuf> {
    std::path::absolute(path).with_context(|| format!("resolving {}", path.display()))
}

pub fn record(c: &Client, file: Option<PathBuf>, stop: bool) -> Result<()> {
    match (file, stop) {
        (_, true) => println!("{}", c.request(Ctl::Record, "")?),
        (Some(file), false) => {
            let path = absolute(&file)?;
            println!("{}", c.request(Ctl::Record, &path.display().to_string())?);
            eprintln!("stop with `ruro record --stop`");
        }
        (None, false) => bail!("give a file to record to, or --stop"),
    }
    Ok(())
}

fn send(c: &Client, cmd: &PlaybackCommand) -> Result<String> {
    c.request(Ctl::Playback, &serde_json::to_string(cmd)?)
}

/// Starts a replay and follows it until it ends; Ctrl-C stops it.
pub fn play(c: &Client, file: &Path, rate: f64) -> Result<()> {
    let file = absolute(file)?.display().to_string();
    // Subscribed before starting, so every status that arrives after the
    // reply describes the replay we started (or its end).
    let sub = c.session.declare_subscriber(c.keys.status()).wait_sub()?;
    println!(
        "{}",
        send(
            c,
            &PlaybackCommand::Start {
                file: file.clone(),
                rate,
            }
        )?
    );
    let stop = crate::ctrl_c_flag()?;
    let started = std::time::Instant::now();
    while !stop.load(Ordering::Relaxed) {
        let Ok(Some(sample)) = sub.recv_timeout(Duration::from_millis(200)) else {
            continue;
        };
        let Ok(status) =
            ruro_proto::decode::<ruro_proto::DaemonStatus>(&sample.payload().to_bytes())
        else {
            continue;
        };
        if playback_ended(&file, status.playback.as_ref(), started.elapsed()) {
            eprintln!("\nplayback finished");
            return Ok(());
        }
        if let Some(p) = status.playback {
            eprint!(
                "\r{:>7.1} / {:.1} s  x{}{}   ",
                p.position_s,
                p.duration_s,
                p.rate,
                if p.paused { "  paused" } else { "" }
            );
        }
    }
    eprintln!();
    println!("{}", send(c, &PlaybackCommand::Stop)?);
    Ok(())
}

/// Whether a daemon status received `elapsed` after starting a replay of
/// `file` shows that replay over: nothing playing, or another file. A
/// status computed just before the start can still be in flight, so an
/// empty one is only trusted after a status period.
fn playback_ended(
    file: &str,
    playback: Option<&ruro_proto::PlaybackStatus>,
    elapsed: Duration,
) -> bool {
    match playback {
        Some(p) => p.file != file,
        None => elapsed >= STATUS_PERIOD,
    }
}

/// How often the daemon publishes its status.
const STATUS_PERIOD: Duration = Duration::from_millis(500);

trait WaitSub<T> {
    fn wait_sub(self) -> Result<T>;
}

impl<T, B: zenoh::Wait<To = zenoh::Result<T>>> WaitSub<T> for B {
    fn wait_sub(self) -> Result<T> {
        self.wait().map_err(|e| anyhow::anyhow!("{e}"))
    }
}

pub fn control(c: &Client, cmd: PlaybackCmd) -> Result<()> {
    let cmd = match cmd {
        PlaybackCmd::Pause => PlaybackCommand::Pause,
        PlaybackCmd::Resume => PlaybackCommand::Resume,
        PlaybackCmd::Seek { position_s } => PlaybackCommand::Seek { position_s },
        PlaybackCmd::Rate { rate } => PlaybackCommand::SetRate { rate },
        PlaybackCmd::Stop => PlaybackCommand::Stop,
    };
    println!("{}", send(c, &cmd)?);
    Ok(())
}

pub fn info(file: &Path) -> Result<()> {
    let rec = ruro_record::Recording::open(file)?;
    let info = rec.info()?;
    println!("file      {}", file.display());
    println!(
        "duration  {:.3} s  ({} to {})",
        info.duration_s(),
        crate::output::clock(info.start_ns),
        crate::output::clock(info.end_ns)
    );
    println!("messages  {}", info.messages);
    println!("channels  {}\n", info.channels.len());
    let width = info
        .channels
        .iter()
        .map(|c| c.topic.len())
        .max()
        .unwrap_or(5)
        .max(5);
    println!(
        "{:width$}  {:<28} {:<14} {:>9} {:>9}",
        "TOPIC", "TYPE", "ENCODING", "COUNT", "HZ"
    );
    let dur = info.duration_s();
    for c in &info.channels {
        let hz = if dur > 0.0 && c.count > 1 {
            format!("{:.1}", (c.count - 1) as f64 / dur)
        } else {
            "-".into()
        };
        println!(
            "{:width$}  {:<28} {:<14} {:>9} {:>9}",
            c.topic, c.ty, c.encoding, c.count, hz
        );
    }
    Ok(())
}

pub fn bag(cmd: BagCmd) -> Result<()> {
    match cmd {
        BagCmd::Import { src, out, map, raw } => {
            let out = out.unwrap_or_else(|| {
                let name = src
                    .file_name()
                    .map(|n| n.to_string_lossy().trim_end_matches(".mcap").to_owned())
                    .unwrap_or_else(|| "bag".into());
                src.with_file_name(format!("{name}.ruro.mcap"))
            });
            let opts = ruro_record::rosbag::ImportOptions {
                map: map.into_iter().collect::<HashMap<_, _>>(),
                compress: true,
                raw,
            };
            let report = ruro_record::rosbag::import(&src, &out, &opts)?;
            print_import_report(&report);
            let written = report.written();
            if written == 0 {
                bail!("the bag has no messages that could be imported");
            }
            println!("wrote {written} message(s) to {}", out.display());
            Ok(())
        }
    }
}

/// One line per ROS topic: where its messages went, and anything not
/// imported.
fn print_import_report(report: &ruro_record::rosbag::ImportReport) {
    let width = report
        .topics
        .iter()
        .map(|t| t.ros_topic.len())
        .max()
        .unwrap_or(0);
    let type_width = report
        .topics
        .iter()
        .map(|t| t.ros_type.len())
        .max()
        .unwrap_or(0);
    for t in &report.topics {
        let mut parts = Vec::new();
        if let Some(name) = &t.ruro_topic {
            parts.push(format!("{} converted -> {name}", t.converted));
        }
        if let Some(name) = &t.raw_topic {
            parts.push(format!("{} as ros2/Any -> {name}", t.raw));
        }
        if t.skipped > 0 {
            parts.push(format!("{} skipped", t.skipped));
        }
        let mut line = format!(
            "{:width$}  {:type_width$}  {}",
            t.ros_topic,
            t.ros_type,
            parts.join(", ")
        );
        if let Some(reason) = &t.reason {
            line.push_str(&format!(" ({reason})"));
        }
        println!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruro_proto::PlaybackStatus;

    fn playing(file: &str) -> PlaybackStatus {
        PlaybackStatus {
            file: file.into(),
            position_s: 0.5,
            duration_s: 2.0,
            paused: false,
            rate: 1.0,
        }
    }

    #[test]
    fn a_replay_that_ends_before_any_status_still_finishes() {
        // A 10 ms recording is over long before the first status shows it.
        let late = STATUS_PERIOD + Duration::from_millis(10);
        assert!(playback_ended("/a.mcap", None, late));
    }

    #[test]
    fn a_status_from_before_the_start_does_not_end_the_replay() {
        assert!(!playback_ended("/a.mcap", None, Duration::from_millis(20)));
    }

    #[test]
    fn follows_its_own_replay_until_another_one_replaces_it() {
        let elapsed = Duration::from_secs(3);
        assert!(!playback_ended(
            "/a.mcap",
            Some(&playing("/a.mcap")),
            elapsed
        ));
        assert!(playback_ended(
            "/a.mcap",
            Some(&playing("/b.mcap")),
            elapsed
        ));
    }
}
