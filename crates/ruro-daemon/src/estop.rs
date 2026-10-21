//! The emergency stop, latched per daemon and passed on to the machines of
//! a multi-machine graph.
//!
//! - A stop is latched per *origin*: the daemon a client engaged it at. A
//!   daemon is stopped while the stop of any origin is latched there.
//! - `engage` latches the daemon's own stop and forwards it to the peers
//!   running parts of its graph, which latch it under the daemon's name.
//! - `release` lifts only the daemon's own stop: here, and on every peer it
//!   forwarded that stop to, including peers a redeploy has since dropped
//!   from the graph (they are remembered until they confirm the release).
//!   Stops engaged at other daemons stay latched.
//! - `release-all` lifts every stop latched at this daemon, whatever its
//!   origin, and forwards the release of the daemon's own stop.
//! - A stop engaged or released directly on a peer goes back to the daemon
//!   that deployed the graph (`engage-up`/`release-up`), which latches it
//!   under the peer's name and forwards it to its other peers. A stop
//!   engaged on any machine of a graph therefore stops all of them, and
//!   only a release where it was engaged (or `release-all`) lifts it.
//! - Forwarded requests are not forwarded further, so stops do not loop.
//! - A graph deployed from a daemon that is stopped (by any origin) comes up
//!   stopped on its peers, under that daemon's origin: releasing there lifts
//!   it.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use ruro_proto::{Ctl, Keys};

use crate::State;

/// How long a peer gets to confirm a forwarded e-stop request.
const FORWARD_TIMEOUT: Duration = Duration::from_secs(2);

/// How long a peer waits for the daemon that deployed its graph, which
/// forwards the stop to its other peers first.
const RELAY_TIMEOUT: Duration = Duration::from_secs(5);

const ENGAGE_FROM: &str = "engage-from ";
const RELEASE_FROM: &str = "release-from ";
const ENGAGE_UP: &str = "engage-up ";
const RELEASE_UP: &str = "release-up ";

/// The stops latched at one daemon.
#[derive(Debug, Default)]
pub(crate) struct Latch {
    /// Daemons whose stop is latched here.
    origins: BTreeSet<String>,
    /// Peers this daemon forwarded its own stop to and that have not
    /// confirmed its release yet.
    sent: BTreeSet<String>,
}

impl Latch {
    pub fn engaged(&self) -> bool {
        !self.origins.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Request {
    /// From a client at this daemon.
    Engage,
    Release {
        all: bool,
    },
    /// From the daemon whose graph this one runs part of.
    Forwarded {
        engage: bool,
        origin: String,
    },
    /// From a peer running part of this daemon's graph, where a client
    /// engaged or released its own stop.
    Relayed {
        engage: bool,
        origin: String,
    },
}

impl Request {
    fn parse(text: &str) -> Result<Self> {
        let text = text.trim();
        Ok(match text {
            "engage" | "" => Self::Engage,
            "release" => Self::Release { all: false },
            "release-all" => Self::Release { all: true },
            _ => {
                let (engage, relayed, origin) = if let Some(o) = text.strip_prefix(ENGAGE_FROM) {
                    (true, false, o)
                } else if let Some(o) = text.strip_prefix(RELEASE_FROM) {
                    (false, false, o)
                } else if let Some(o) = text.strip_prefix(ENGAGE_UP) {
                    (true, true, o)
                } else if let Some(o) = text.strip_prefix(RELEASE_UP) {
                    (false, true, o)
                } else {
                    bail!("expected `engage`, `release` or `release-all`, got `{text}`");
                };
                if !ruro_graph::is_valid_ident(origin) {
                    bail!("invalid daemon name `{origin}`");
                }
                let origin = origin.to_owned();
                if relayed {
                    Self::Relayed { engage, origin }
                } else {
                    Self::Forwarded { engage, origin }
                }
            }
        })
    }

    fn engage(&self) -> bool {
        match self {
            Self::Engage => true,
            Self::Release { .. } => false,
            Self::Forwarded { engage, .. } | Self::Relayed { engage, .. } => *engage,
        }
    }
}

/// Applies a request to a latch held by daemon `me`. Returns whether the
/// daemon is now stopped and which origins hold it.
fn apply(latch: &mut Latch, me: &str, req: &Request) -> (bool, Vec<String>) {
    match req {
        Request::Engage => {
            latch.origins.insert(me.to_owned());
        }
        Request::Release { all: false } => {
            latch.origins.remove(me);
        }
        Request::Release { all: true } => latch.origins.clear(),
        Request::Forwarded {
            engage: true,
            origin,
        }
        | Request::Relayed {
            engage: true,
            origin,
        } => {
            latch.origins.insert(origin.clone());
        }
        Request::Forwarded {
            engage: false,
            origin,
        }
        | Request::Relayed {
            engage: false,
            origin,
        } => {
            latch.origins.remove(origin);
        }
    }
    (latch.engaged(), latch.origins.iter().cloned().collect())
}

/// Sets the stop on this daemon's hosts.
fn set(st: &State, engaged: bool) {
    let was = st.estop.swap(engaged, Ordering::Relaxed);
    st.sup.lock().host_state.estopped = engaged;
    st.publish_estop();
    if was != engaged {
        tracing::warn!(
            "emergency stop {}",
            if engaged { "engaged" } else { "released" }
        );
    }
}

/// Handles `Ctl::Estop`.
pub fn handle(st: &Arc<State>, text: &str) -> Result<String> {
    let req = Request::parse(text)?;
    let (engaged, origins) = apply(&mut st.estop_latch.lock(), &st.name, &req);
    set(st, engaged);
    let done = if req.engage() {
        "e-stop engaged".to_owned()
    } else if engaged {
        let by: Vec<String> = origins.iter().map(|o| format!("`{o}`")).collect();
        format!(
            "e-stop stays engaged: engaged by {} (release it there, or here with --all)",
            by.join(", ")
        )
    } else {
        "e-stop released".to_owned()
    };
    // Whose stop this is, as the other machines latch it.
    let origin = match &req {
        Request::Forwarded { .. } => return Ok(done),
        Request::Relayed { origin, .. } => origin.clone(),
        Request::Engage | Request::Release { .. } => st.name.clone(),
    };

    // The machines running the rest of the graph follow, without passing
    // it further; a release also reaches machines that left the graph.
    let targets: BTreeSet<String> = {
        let peers = st.peers.lock().clone();
        let mut latch = st.estop_latch.lock();
        let mut targets = if !req.engage() {
            peers.union(&latch.sent).cloned().collect()
        } else if origin == st.name {
            latch.sent.extend(peers.iter().cloned());
            peers
        } else {
            peers
        };
        targets.remove(&origin);
        targets
    };
    let op = if req.engage() {
        format!("{ENGAGE_FROM}{origin}")
    } else {
        format!("{RELEASE_FROM}{origin}")
    };
    let mut results = forward(st, &targets, &op, FORWARD_TIMEOUT);
    // A stop of this daemon's own, while it runs part of another daemon's
    // graph, goes back to that daemon, which passes it on.
    let up = st.origin.lock().clone();
    if let Some(up) = up.filter(|_| origin == st.name) {
        let op = if req.engage() {
            format!("{ENGAGE_UP}{origin}")
        } else {
            format!("{RELEASE_UP}{origin}")
        };
        results.extend(forward(st, &BTreeSet::from([up]), &op, RELAY_TIMEOUT));
    }
    if origin == st.name && !req.engage() {
        let mut latch = st.estop_latch.lock();
        for (p, r) in &results {
            if r.is_ok() {
                latch.sent.remove(p);
            }
        }
    }
    // Machines of the graph must follow; one that has left it and cannot
    // be reached is only noted (and tried again on the next release).
    let mut current = st.peers.lock().clone();
    current.extend(st.origin.lock().clone());
    let (mut failed, mut gone) = (Vec::new(), Vec::new());
    for (p, r) in &results {
        if let Err(e) = r {
            let note = format!("`{p}` ({e:#})");
            if current.contains(p) {
                failed.push(note);
            } else {
                gone.push(note);
            }
        }
    }
    if !failed.is_empty() {
        bail!("{done} here, but not on {}", failed.join(", "));
    }
    let mut reply: Vec<String> = std::iter::once(done)
        .chain(
            results
                .into_iter()
                .filter_map(|(p, r)| Some(format!("{p}: {}", r.ok()?))),
        )
        .collect();
    if !gone.is_empty() {
        reply.push(format!(
            "not reached, no longer in the graph: {}",
            gone.join(", ")
        ));
    }
    Ok(reply.join("; "))
}

/// Before a deploy forwards groups to `machines`: if this daemon is
/// stopped, they come up stopped, under this daemon's origin.
pub fn forward_on_deploy(st: &State, machines: &BTreeSet<String>) -> Result<()> {
    if !st.estop.load(Ordering::Relaxed) || machines.is_empty() {
        return Ok(());
    }
    st.estop_latch.lock().sent.extend(machines.iter().cloned());
    let op = format!("{ENGAGE_FROM}{}", st.name);
    for (p, r) in forward(st, machines, &op, FORWARD_TIMEOUT) {
        r.map_err(|e| anyhow!("engaging the e-stop on `{p}`: {e:#}"))?;
    }
    Ok(())
}

/// Sends `op` to every peer at once, and briefly: a peer that does not
/// answer must not hold the stop back from the others, nor outlast the
/// client.
fn forward(
    st: &State,
    peers: &BTreeSet<String>,
    op: &str,
    timeout: Duration,
) -> Vec<(String, Result<String>)> {
    std::thread::scope(|s| {
        let pending: Vec<_> = peers
            .iter()
            .map(|p| {
                let ask =
                    || ruro_proto::request(&st.session, &Keys::new(p), Ctl::Estop, op, timeout);
                (p.clone(), s.spawn(ask))
            })
            .collect();
        pending
            .into_iter()
            .map(|(p, h)| {
                let r = h.join().unwrap_or_else(|_| Err(anyhow!("request failed")));
                (p, r)
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(latch: &mut Latch, me: &str, text: &str) -> bool {
        apply(latch, me, &Request::parse(text).unwrap()).0
    }

    #[test]
    fn stops_are_latched_per_origin() {
        let mut robot = Latch::default();
        run(&mut robot, "robot", "engage");
        run(&mut robot, "robot", "engage-from laptop");
        assert!(run(&mut robot, "robot", "release-from laptop"));
        assert!(
            run(&mut robot, "robot", "engage-from laptop"),
            "the robot's own stop is still there"
        );
        assert!(
            run(&mut robot, "robot", "release"),
            "a release here leaves the laptop's stop"
        );
        assert!(!run(&mut robot, "robot", "release-from laptop"));

        run(&mut robot, "robot", "engage-from laptop");
        run(&mut robot, "robot", "engage-from console");
        assert!(
            !run(&mut robot, "robot", "release-all"),
            "--all lifts every stop"
        );
    }

    #[test]
    fn requests_parse() {
        assert_eq!(Request::parse("").unwrap(), Request::Engage);
        assert_eq!(
            Request::parse("release-from robot-1").unwrap(),
            Request::Forwarded {
                engage: false,
                origin: "robot-1".into()
            }
        );
        assert_eq!(
            Request::parse("engage-up arm").unwrap(),
            Request::Relayed {
                engage: true,
                origin: "arm".into()
            }
        );
        assert!(Request::parse("maybe").is_err());
        assert!(Request::parse("engage-from a/b").is_err());
    }
}
