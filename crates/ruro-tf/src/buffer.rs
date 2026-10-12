//! A buffer of time-stamped transforms forming a tree of coordinate frames.

use std::collections::{BTreeMap, HashMap, VecDeque};

use ruro::msg::{Header, Transform, Transforms};

use crate::Isometry;

/// How long dynamic transforms are kept by default: 10 s.
pub const DEFAULT_CACHE_NS: u64 = 10_000_000_000;

/// When to look a transform up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Time {
    /// The latest time at which every transform on the path is known.
    Latest,
    /// A timestamp in nanoseconds; dynamic transforms are interpolated.
    At(u64),
}

/// Why a lookup failed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum TfError {
    #[error("frame `{0}` does not exist")]
    UnknownFrame(String),
    #[error("frames `{0}` and `{1}` are not connected")]
    NotConnected(String, String),
    #[error(
        "transform {parent} -> {child} unavailable at {time} ns (buffer holds {oldest}..={newest})"
    )]
    Extrapolation {
        parent: String,
        child: String,
        time: u64,
        oldest: u64,
        newest: u64,
    },
    #[error("making `{parent}` the parent of `{child}` would create a cycle")]
    Cycle { parent: String, child: String },
    #[error("a frame cannot be its own parent (`{0}`)")]
    SelfParent(String),
    #[error("empty frame name")]
    EmptyFrame,
}

#[derive(Debug, Clone)]
enum Samples {
    Static(Isometry),
    /// Sorted by time, oldest first.
    Dynamic(VecDeque<(u64, Isometry)>),
}

#[derive(Debug, Clone)]
struct Edge {
    parent: String,
    samples: Samples,
}

impl Edge {
    fn latest_time(&self) -> Option<u64> {
        match &self.samples {
            Samples::Static(_) => None,
            Samples::Dynamic(s) => s.back().map(|(t, _)| *t),
        }
    }

    fn at(&self, child: &str, time: Time) -> Result<Isometry, TfError> {
        let samples = match &self.samples {
            Samples::Static(iso) => return Ok(*iso),
            Samples::Dynamic(s) => s,
        };
        let (oldest, newest) = match (samples.front(), samples.back()) {
            (Some(a), Some(b)) => (a.0, b.0),
            _ => return Err(TfError::UnknownFrame(child.to_owned())),
        };
        let t = match time {
            Time::Latest => return Ok(samples.back().unwrap().1),
            Time::At(t) => t,
        };
        if t < oldest || t > newest {
            return Err(TfError::Extrapolation {
                parent: self.parent.clone(),
                child: child.to_owned(),
                time: t,
                oldest,
                newest,
            });
        }
        // First sample at or after t.
        let i = samples.partition_point(|(st, _)| *st < t);
        let (t1, b) = samples[i];
        if t1 == t || i == 0 {
            return Ok(b);
        }
        let (t0, a) = samples[i - 1];
        Ok(a.interpolate(&b, (t - t0) as f64 / (t1 - t0) as f64))
    }
}

/// Time-indexed transforms between named frames.
///
/// Every frame has at most one parent, so the frames form a forest. Each
/// parent→child edge is either static (valid at all times) or dynamic (a
/// history of time-stamped samples, interpolated on lookup and pruned after
/// [`cache_ns`](TfBuffer::with_cache_ns)).
///
/// Use it inside a node by feeding it every [`Transforms`] message received
/// and calling [`lookup`](TfBuffer::lookup) when a frame relation is needed.
#[derive(Debug, Clone)]
pub struct TfBuffer {
    /// Keyed by child frame.
    edges: HashMap<String, Edge>,
    cache_ns: u64,
}

impl Default for TfBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl TfBuffer {
    /// An empty buffer that keeps 10 s of history.
    pub fn new() -> Self {
        Self {
            edges: HashMap::new(),
            cache_ns: DEFAULT_CACHE_NS,
        }
    }

    /// Sets how much dynamic history to keep.
    pub fn with_cache_ns(mut self, cache_ns: u64) -> Self {
        self.cache_ns = cache_ns;
        self
    }

    /// Adds a transform from `t.header.frame_id` (parent) to
    /// `t.child_frame_id`. Static transforms replace any previous edge to the
    /// child and never expire. A transform with a zero timestamp is treated as
    /// static too, which is how [`StaticPublisher`](crate::StaticPublisher)
    /// marks them.
    pub fn set(&mut self, t: &Transform, is_static: bool) -> Result<(), TfError> {
        let parent = t.header.frame_id.trim_start_matches('/');
        let child = t.child_frame_id.trim_start_matches('/');
        if parent.is_empty() || child.is_empty() {
            return Err(TfError::EmptyFrame);
        }
        if parent == child {
            return Err(TfError::SelfParent(child.to_owned()));
        }
        // Reject cycles: parent must not be a descendant of child.
        let mut cur = parent;
        while let Some(e) = self.edges.get(cur) {
            if e.parent == child {
                return Err(TfError::Cycle {
                    parent: parent.to_owned(),
                    child: child.to_owned(),
                });
            }
            cur = &e.parent;
        }
        let iso = Isometry::from_msg(t);
        let is_static = is_static || t.header.stamp_ns == 0;
        let stamp = t.header.stamp_ns;
        let cache_ns = self.cache_ns;
        let edge = self.edges.entry(child.to_owned()).or_insert_with(|| Edge {
            parent: parent.to_owned(),
            samples: Samples::Dynamic(VecDeque::new()),
        });
        if edge.parent != parent || is_static || matches!(edge.samples, Samples::Static(_)) {
            // Re-parenting or switching between static and dynamic starts a
            // fresh history.
            edge.parent = parent.to_owned();
            edge.samples = Samples::Dynamic(VecDeque::new());
        }
        if is_static {
            edge.samples = Samples::Static(iso);
            return Ok(());
        }
        let Samples::Dynamic(samples) = &mut edge.samples else {
            unreachable!()
        };
        // Insert in time order; replace a sample with the same stamp.
        let i = samples.partition_point(|(t, _)| *t < stamp);
        if samples.get(i).is_some_and(|(t, _)| *t == stamp) {
            samples[i].1 = iso;
        } else {
            samples.insert(i, (stamp, iso));
        }
        let newest = samples.back().map_or(stamp, |(t, _)| *t);
        while samples
            .front()
            .is_some_and(|(t, _)| newest.saturating_sub(*t) > cache_ns)
        {
            samples.pop_front();
        }
        Ok(())
    }

    /// Adds every transform of a message. Returns the first error, after
    /// applying all valid transforms.
    pub fn ingest(&mut self, msg: &Transforms, is_static: bool) -> Result<(), TfError> {
        let mut first = Ok(());
        for t in &msg.0 {
            if let Err(e) = self.set(t, is_static)
                && first.is_ok()
            {
                first = Err(e);
            }
        }
        first
    }

    /// Every known frame, sorted.
    pub fn frames(&self) -> Vec<String> {
        let mut all: BTreeMap<&str, ()> = BTreeMap::new();
        for (child, e) in &self.edges {
            all.insert(child, ());
            all.insert(&e.parent, ());
        }
        all.into_keys().map(str::to_owned).collect()
    }

    /// The parent of a frame, if it has one.
    pub fn parent(&self, frame: &str) -> Option<&str> {
        self.edges.get(frame).map(|e| e.parent.as_str())
    }

    fn exists(&self, frame: &str) -> bool {
        self.edges.contains_key(frame) || self.edges.values().any(|e| e.parent == frame)
    }

    /// Frames from `frame` up to its root, starting with `frame` itself.
    fn chain(&self, frame: &str) -> Vec<String> {
        let mut out = vec![frame.to_owned()];
        let mut cur = frame;
        while let Some(e) = self.edges.get(cur) {
            out.push(e.parent.clone());
            cur = &e.parent;
        }
        out
    }

    /// The two halves of the path between `target` and `source`: frames
    /// below the common ancestor on each side.
    fn path(&self, target: &str, source: &str) -> Result<(Vec<String>, Vec<String>), TfError> {
        for f in [target, source] {
            if !self.exists(f) {
                return Err(TfError::UnknownFrame(f.to_owned()));
            }
        }
        let up_t = self.chain(target);
        let up_s = self.chain(source);
        let common = up_s
            .iter()
            .find(|f| up_t.contains(f))
            .ok_or_else(|| TfError::NotConnected(target.to_owned(), source.to_owned()))?;
        let below = |chain: &[String]| -> Vec<String> {
            chain.iter().take_while(|f| *f != common).cloned().collect()
        };
        Ok((below(&up_t), below(&up_s)))
    }

    /// Composes the edges from `frames[0]` up to (excluding) the common
    /// ancestor: the pose of `frames[0]` in the ancestor frame.
    fn to_ancestor(&self, frames: &[String], time: Time) -> Result<Isometry, TfError> {
        let mut iso = Isometry::IDENTITY;
        for child in frames {
            let edge = &self.edges[child];
            iso = edge.at(child, time)?.compose(&iso);
        }
        Ok(iso)
    }

    /// The latest time at which every dynamic edge on the path has data, or
    /// `None` if the path is entirely static.
    fn latest_common(&self, frames: impl Iterator<Item = String>) -> Option<u64> {
        frames
            .filter_map(|f| self.edges.get(&f).and_then(Edge::latest_time))
            .min()
    }

    /// The transform that maps coordinates in `source` into `target`; as a
    /// pose, `source` expressed in `target`.
    ///
    /// With [`Time::Latest`] the lookup uses the newest time all transforms
    /// on the path share, so a fast and a slow edge are not mixed from
    /// different moments.
    pub fn lookup(&self, target: &str, source: &str, time: Time) -> Result<Isometry, TfError> {
        let target = target.trim_start_matches('/');
        let source = source.trim_start_matches('/');
        let (down, up) = self.path(target, source)?;
        let time = match time {
            Time::Latest => match self.latest_common(down.iter().chain(&up).cloned()) {
                Some(t) => Time::At(t),
                None => Time::Latest,
            },
            at => at,
        };
        let target_in_root = self.to_ancestor(&down, time)?;
        let source_in_root = self.to_ancestor(&up, time)?;
        Ok(target_in_root.inverse().compose(&source_in_root))
    }

    /// Like [`lookup`](Self::lookup), returned as a transform message with
    /// `target` as parent and `source` as child, stamped with the lookup time.
    pub fn lookup_msg(&self, target: &str, source: &str, time: Time) -> Result<Transform, TfError> {
        let iso = self.lookup(target, source, time)?;
        let stamp = match time {
            Time::At(t) => t,
            Time::Latest => {
                let (down, up) = self.path(target, source)?;
                self.latest_common(down.into_iter().chain(up)).unwrap_or(0)
            }
        };
        Ok(Transform {
            header: Header::new(target, stamp),
            child_frame_id: source.to_owned(),
            translation: iso.vec3(),
            rotation: iso.quat(),
        })
    }

    /// Whether [`lookup`](Self::lookup) would succeed.
    pub fn can_transform(&self, target: &str, source: &str, time: Time) -> bool {
        self.lookup(target, source, time).is_ok()
    }

    /// Maps a point from `source` coordinates into `target` coordinates.
    pub fn transform_point(
        &self,
        target: &str,
        source: &str,
        time: Time,
        p: [f64; 3],
    ) -> Result<[f64; 3], TfError> {
        Ok(self.lookup(target, source, time)?.transform_point(p))
    }

    /// Removes every transform.
    pub fn clear(&mut self) {
        self.edges.clear();
    }
}
