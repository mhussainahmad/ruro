//! `ruro fleet` and host binary uploads.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use ruro_proto::{Ctl, DaemonStatus, GroupMetrics, Keys, UploadChunk};
use sha2::{Digest, Sha256};
use zenoh::Wait;

use crate::Client;

/// Bytes per upload request.
const CHUNK: usize = 1 << 20;

/// How long to listen for daemons announcing themselves.
const LISTEN: Duration = Duration::from_millis(1500);

/// Lists every daemon whose status reaches this session, with the CPU its
/// groups use (summed from their metrics).
pub fn fleet(session: &zenoh::Session) -> Result<()> {
    let err = |e: zenoh::Error| anyhow!("{e}");
    let status_sub = session
        .declare_subscriber(Keys::all_status())
        .wait()
        .map_err(err)?;
    let metrics_sub = session
        .declare_subscriber("ruro/*/metrics/*")
        .wait()
        .map_err(err)?;
    let mut daemons: BTreeMap<String, DaemonStatus> = BTreeMap::new();
    // Latest metrics per (daemon, group).
    let mut metrics: BTreeMap<(String, String), GroupMetrics> = BTreeMap::new();
    // Daemons answer status and metrics requests at once; the request ends
    // when every reachable one has answered.
    for key in [Keys::all_status().to_string(), "ruro/*/metrics/*".to_owned()] {
        let Ok(replies) = session.get(key.as_str()).timeout(LISTEN).wait() else {
            continue;
        };
        while let Ok(reply) = replies.recv() {
            let Ok(s) = reply.result() else { continue };
            let bytes = s.payload().to_bytes();
            if key.ends_with("status") {
                if let Ok(st) = ruro_proto::decode_status(&bytes) {
                    daemons.insert(st.name.clone(), st);
                }
            } else if let Ok(m) = ruro_proto::decode::<GroupMetrics>(&bytes) {
                let daemon = s.key_expr().as_str().split('/').nth(1).unwrap_or_default();
                metrics.insert((daemon.to_owned(), m.group.clone()), m);
            }
        }
    }
    // 0.1.0 daemons only publish; listen for them unless every daemon seen
    // answered already.
    let answered = !daemons.is_empty();
    let deadline = Instant::now() + if answered { Duration::ZERO } else { LISTEN };
    loop {
        while let Ok(Some(s)) = status_sub.try_recv() {
            if let Ok(st) = ruro_proto::decode_status(&s.payload().to_bytes()) {
                daemons.insert(st.name.clone(), st);
            }
        }
        while let Ok(Some(s)) = metrics_sub.try_recv() {
            let daemon = s.key_expr().as_str().split('/').nth(1).unwrap_or_default();
            if let Ok(m) = ruro_proto::decode::<GroupMetrics>(&s.payload().to_bytes()) {
                metrics.insert((daemon.to_owned(), m.group.clone()), m);
            }
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if daemons.is_empty() {
        bail!("no daemon answered");
    }
    println!(
        "{:<16} {:<8} {:<20} {:>6} {:>8} {:>7}  STATE",
        "NAME", "VERSION", "GRAPH", "GROUPS", "UPTIME", "CPU"
    );
    for (name, st) in &daemons {
        let cpu = cpu_cell(
            metrics
                .iter()
                .filter(|((d, _), _)| d == name)
                .map(|(_, m)| m),
        );
        let running = st.groups.iter().filter(|g| g.pid.is_some()).count();
        let mut state = Vec::new();
        if st.estopped {
            state.push("E-STOP".to_owned());
        }
        if st.recording.is_some() {
            state.push("recording".to_owned());
        }
        if st.playback.is_some() {
            state.push("playing".to_owned());
        }
        println!(
            "{:<16} {:<8} {:<20} {:>6} {:>8} {:>7}  {}",
            name,
            st.version,
            st.graph.as_deref().unwrap_or("-"),
            format!("{running}/{}", st.groups.len()),
            crate::output::duration(st.uptime_s),
            cpu,
            state.join(", ")
        );
    }
    Ok(())
}

/// CPU use summed over a daemon's groups, or `-` when none reported.
fn cpu_cell<'a>(groups: impl Iterator<Item = &'a GroupMetrics>) -> String {
    let mut groups = groups.peekable();
    if groups.peek().is_none() {
        return "-".into();
    }
    // Not `sum`, which starts from -0.0 and prints an idle daemon as "-0.0%".
    let cpu = groups
        .flat_map(|m| &m.nodes)
        .fold(0.0, |cpu, n| cpu + n.cpu_pct);
    format!("{cpu:.1}%")
}

/// SHA-256 of `data`, lowercase hex.
pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Uploads a host binary to the daemon, which verifies its checksum and
/// stores it under `name` (default: the file name).
pub fn upload_host(c: &Client, path: &Path, name: Option<&str>) -> Result<String> {
    let name = match name {
        Some(n) => n.to_owned(),
        None => path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .context("host binary path has no file name")?,
    };
    let mut data = Vec::new();
    std::fs::File::open(path)
        .with_context(|| format!("opening {}", path.display()))?
        .read_to_end(&mut data)?;
    if data.is_empty() {
        bail!("{} is empty", path.display());
    }
    let sha256 = sha256_hex(&data);
    let total = data.len() as u64;
    let mut reply = String::new();
    for (i, part) in data.chunks(CHUNK).enumerate() {
        let chunk = UploadChunk {
            name: name.clone(),
            offset: (i * CHUNK) as u64,
            total,
            sha256: sha256.clone(),
            data: part.to_vec(),
        };
        reply = ruro_proto::request_bytes(
            &c.session,
            &c.keys,
            Ctl::Upload,
            ruro_proto::encode(&chunk),
            Duration::from_secs(30),
        )
        .with_context(|| format!("uploading {name}"))?;
        eprint!("\ruploaded {}/{total} bytes", (i * CHUNK + part.len()));
    }
    eprintln!();
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_daemons_show_no_cpu_rather_than_minus_zero() {
        assert_eq!(cpu_cell(std::iter::empty()), "-");
        let idle = GroupMetrics {
            group: "g".into(),
            pid: 1,
            nodes: Vec::new(),
        };
        assert_eq!(cpu_cell([&idle].into_iter()), "0.0%");
    }

    #[test]
    fn sha256_matches_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
