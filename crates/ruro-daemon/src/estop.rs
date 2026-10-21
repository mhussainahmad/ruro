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
//! - Requests are applied one at a time, in the order they arrive, on a
//!   thread of their own: never behind a deploy or an update. Only passing
//!   them on to other machines happens alongside. Passed-on requests carry
//!   a sequence number per origin, so one that arrives after a later one
//!   from the same origin is ignored.
//! - A graph deployed from a daemon that is stopped (by any origin) comes up
//!   stopped on its peers, under that daemon's origin: releasing there lifts
//!   it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
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

/// The number of a request, as passed on: `<epoch>.<n>`. Each start of a
/// daemon picks a new epoch and counts from 1 in it, so numbers never
/// depend on the clock. A receiver orders requests of one origin within an
/// epoch, and takes the first request of a new epoch as the latest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Seq {
    epoch: u64,
    n: u64,
}

impl std::fmt::Display for Seq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:x}.{}", self.epoch, self.n)
    }
}

impl std::str::FromStr for Seq {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let (e, n) = s.split_once('.').context("expected `<epoch>.<n>`")?;
        Ok(Self {
            epoch: u64::from_str_radix(e, 16)?,
            n: n.parse()?,
        })
    }
}

/// The stops latched at one daemon.
#[derive(Debug)]
pub(crate) struct Latch {
    /// Daemons whose stop is latched here.
    origins: BTreeSet<String>,
    /// Peers this daemon forwarded its own stop to and that have not
    /// confirmed its release yet.
    sent: BTreeSet<String>,
    /// This daemon's epoch and the last number it gave a request.
    epoch: u64,
    counter: u64,
    /// The number of the last request applied, per origin.
    seen: BTreeMap<String, Seq>,
    /// The last request (engage or not) of each origin this daemon passes
    /// on to others: its own, and those relayed to it. Passed on again
    /// from time to time, for machines that missed it.
    last: BTreeMap<String, (bool, Seq)>,
}

impl Default for Latch {
    fn default() -> Self {
        // Different on every start; the clock and the process id are only
        // a source of variety here, not of order.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        let epoch = now ^ (u64::from(std::process::id()) << 32) | 1;
        Self {
            origins: BTreeSet::new(),
            sent: BTreeSet::new(),
            epoch,
            counter: 0,
            seen: BTreeMap::new(),
            last: BTreeMap::new(),
        }
    }
}

impl Latch {
    pub fn engaged(&self) -> bool {
        !self.origins.is_empty()
    }

    /// A number for a request of this daemon's own, above any before it.
    fn next_seq(&mut self, me: &str) -> Seq {
        self.counter += 1;
        let seq = Seq {
            epoch: self.epoch,
            n: self.counter,
        };
        self.seen.insert(me.to_owned(), seq);
        seq
    }

    /// Records `seq` for `origin`; false if it is not newer than the last
    /// request of that origin applied here (in the same epoch). Requests
    /// without a number (from older daemons) always apply.
    fn fresh(&mut self, origin: &str, seq: Option<Seq>) -> bool {
        let Some(seq) = seq else { return true };
        if self
            .seen
            .get(origin)
            .is_some_and(|last| last.epoch == seq.epoch && last.n >= seq.n)
        {
            return false;
        }
        self.seen.insert(origin.to_owned(), seq);
        true
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
        seq: Option<Seq>,
    },
    /// From a peer running part of this daemon's graph, where a client
    /// engaged or released its own stop.
    Relayed {
        engage: bool,
        origin: String,
        seq: Option<Seq>,
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
                let (origin, seq) = match origin.split_once(' ') {
                    Some((o, n)) => (o, Some(n.parse().context("sequence number")?)),
                    None => (origin, None),
                };
                if !ruro_graph::is_valid_ident(origin) {
                    bail!("invalid daemon name `{origin}`");
                }
                let origin = origin.to_owned();
                if relayed {
                    Self::Relayed {
                        engage,
                        origin,
                        seq,
                    }
                } else {
                    Self::Forwarded {
                        engage,
                        origin,
                        seq,
                    }
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
            ..
        }
        | Request::Relayed {
            engage: true,
            origin,
            ..
        } => {
            latch.origins.insert(origin.clone());
        }
        Request::Forwarded {
            engage: false,
            origin,
            ..
        }
        | Request::Relayed {
            engage: false,
            origin,
            ..
        } => {
            latch.origins.remove(origin);
        }
    }
    (latch.engaged(), latch.origins.iter().cloned().collect())
}

/// Sets the stop on this daemon's hosts.
fn set(st: &State, engaged: bool) {
    let was = st.estop.swap(engaged, Ordering::Relaxed);
    st.publish_estop();
    if was != engaged {
        tracing::warn!(
            "emergency stop {}",
            if engaged { "engaged" } else { "released" }
        );
    }
}

/// Handles `Ctl::Estop` from start to end: [`begin`], then [`Forwarding::finish`].
pub fn handle(st: &Arc<State>, text: &str) -> Result<String> {
    match begin(st, text)? {
        (done, Some(rest)) => rest.finish(st, done),
        (done, None) => Ok(done),
    }
}

/// What is left of a request once applied here: passing it on.
pub struct Forwarding {
    engage: bool,
    origin: String,
    seq: Seq,
    targets: BTreeSet<String>,
    /// The daemon whose graph this one runs part of, for a stop of this
    /// daemon's own.
    up: Option<String>,
}

/// Applies a request here, at once: the latch and the hosts' flag change
/// together, so requests applied one after another leave them agreeing.
/// Returns the reply so far and, unless the request came from another
/// daemon, the passing on still to do.
pub fn begin(st: &Arc<State>, text: &str) -> Result<(String, Option<Forwarding>)> {
    let req = Request::parse(text)?;
    let mut latch = st.estop_latch.lock();
    let (origin, seq) = match &req {
        Request::Engage | Request::Release { .. } => (st.name.clone(), latch.next_seq(&st.name)),
        Request::Forwarded { origin, seq, .. } | Request::Relayed { origin, seq, .. } => {
            if !latch.fresh(origin, *seq) {
                // A later request of this origin was applied already.
                let state = if latch.engaged() {
                    "engaged"
                } else {
                    "released"
                };
                return Ok((format!("e-stop stays {state}: request out of date"), None));
            }
            let seq = seq.unwrap_or_else(|| latch.next_seq(&st.name));
            (origin.clone(), seq)
        }
    };
    let (engaged, origins) = apply(&mut latch, &st.name, &req);
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
    if matches!(req, Request::Forwarded { .. }) {
        return Ok((done, None));
    }
    latch.last.insert(origin.clone(), (req.engage(), seq));

    // The machines running the rest of the graph follow, without passing
    // it further; a release also reaches machines that left the graph.
    let peers = st.peers.lock().clone();
    let mut targets = if !req.engage() {
        peers.union(&latch.sent).cloned().collect()
    } else if origin == st.name {
        latch.sent.extend(peers.iter().cloned());
        peers
    } else {
        peers
    };
    targets.remove(&origin);
    let up = st.origin.lock().clone().filter(|_| origin == st.name);
    let forwarding = Forwarding {
        engage: req.engage(),
        origin,
        seq,
        targets,
        up,
    };
    Ok((done, Some(forwarding)))
}

impl Forwarding {
    /// Passes the request on and adds what the other machines said to
    /// `done`. Fails if a machine of the graph did not follow.
    pub fn finish(self, st: &State, done: String) -> Result<String> {
        let (from, upward) = if self.engage {
            (ENGAGE_FROM, ENGAGE_UP)
        } else {
            (RELEASE_FROM, RELEASE_UP)
        };
        let origin = &self.origin;
        let op = format!("{from}{origin} {}", self.seq);
        let mut results = forward(st, &self.targets, &op, FORWARD_TIMEOUT);
        // A stop of this daemon's own, while it runs part of another
        // daemon's graph, goes back to that daemon, which passes it on.
        if let Some(up) = self.up {
            let op = format!("{upward}{origin} {}", self.seq);
            results.extend(forward(st, &BTreeSet::from([up]), &op, RELAY_TIMEOUT));
        }
        if *origin == st.name && !self.engage {
            let mut latch = st.estop_latch.lock();
            for (p, r) in &results {
                if r.is_ok() {
                    latch.sent.remove(p);
                }
            }
        }
        // Machines of the graph must follow; one that has left it and
        // cannot be reached is only noted (and tried again on the next
        // release).
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
}

/// Before a deploy forwards groups to `machines`: if this daemon is
/// stopped, they come up stopped, under this daemon's origin.
pub fn forward_on_deploy(st: &State, machines: &BTreeSet<String>) -> Result<()> {
    if !st.estop.load(Ordering::Relaxed) || machines.is_empty() {
        return Ok(());
    }
    let seq = {
        let mut latch = st.estop_latch.lock();
        latch.sent.extend(machines.iter().cloned());
        let seq = latch.next_seq(&st.name);
        latch.last.insert(st.name.clone(), (true, seq));
        seq
    };
    let op = format!("{ENGAGE_FROM}{} {seq}", st.name);
    for (p, r) in forward(st, machines, &op, FORWARD_TIMEOUT) {
        r.map_err(|e| anyhow!("engaging the e-stop on `{p}`: {e:#}"))?;
    }
    Ok(())
}

/// Passes on again the last request of every origin this daemon passes
/// on, to the machines of its graph: one that missed it (it was cut off,
/// or restarted) catches up. Ones that have it already ignore it, by its
/// number. Called every [`RESEND_INTERVAL`].
pub fn resend(st: &State) {
    let (pending, peers, sent, up) = {
        let latch = st.estop_latch.lock();
        (
            latch.last.clone(),
            st.peers.lock().clone(),
            latch.sent.clone(),
            st.origin.lock().clone(),
        )
    };
    for (origin, (engage, seq)) in pending {
        let (from, upward) = if engage {
            (ENGAGE_FROM, ENGAGE_UP)
        } else {
            (RELEASE_FROM, RELEASE_UP)
        };
        let mut targets: BTreeSet<String> = if engage {
            peers.clone()
        } else {
            peers.union(&sent).cloned().collect()
        };
        targets.remove(&origin);
        let _ = forward(
            st,
            &targets,
            &format!("{from}{origin} {seq}"),
            FORWARD_TIMEOUT,
        );
        if let Some(up) = up.clone().filter(|_| origin == st.name) {
            let op = format!("{upward}{origin} {seq}");
            let _ = forward(st, &BTreeSet::from([up]), &op, RELAY_TIMEOUT);
        }
    }
}

/// How often [`resend`] runs.
pub const RESEND_INTERVAL: Duration = Duration::from_secs(2);

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
    fn out_of_date_requests_of_an_origin_are_ignored() {
        let mut l = Latch::default();
        let seq = |epoch, n| Some(Seq { epoch, n });
        assert!(l.fresh("laptop", seq(7, 5)));
        assert!(!l.fresh("laptop", seq(7, 4)), "sent earlier, arrived later");
        assert!(!l.fresh("laptop", seq(7, 5)), "passed on again");
        assert!(l.fresh("console", seq(9, 1)), "origins are numbered apart");
        // The laptop restarted, whatever its clock says now.
        assert!(l.fresh("laptop", seq(3, 1)));
        assert!(l.fresh("laptop", None), "older daemons send no number");
        let (a, b) = (l.next_seq("me"), l.next_seq("me"));
        assert_eq!((a.epoch, a.n + 1), (b.epoch, b.n));
        let other = Latch::default().next_seq("me");
        assert_ne!(a.epoch, other.epoch, "each start has its own epoch");
        assert_eq!("2a.5".parse::<Seq>().unwrap(), Seq { epoch: 42, n: 5 });
    }

    #[test]
    fn requests_parse() {
        assert_eq!(Request::parse("").unwrap(), Request::Engage);
        assert_eq!(
            Request::parse("release-from robot-1").unwrap(),
            Request::Forwarded {
                engage: false,
                origin: "robot-1".into(),
                seq: None,
            }
        );
        assert_eq!(
            Request::parse("engage-up arm a.7").unwrap(),
            Request::Relayed {
                engage: true,
                origin: "arm".into(),
                seq: Some(Seq { epoch: 10, n: 7 }),
            }
        );
        assert!(Request::parse("maybe").is_err());
        assert!(Request::parse("engage-from a/b").is_err());
    }
}
