//! Transform tree: where every coordinate frame of a robot is, over time.
//!
//! [`TfBuffer`] stores time-stamped transforms between named frames (a parent
//! and a child per transform), interpolates between samples and looks up the
//! transform between any two connected frames by walking the tree through
//! their common ancestor.
//!
//! ```
//! use ruro_tf::{Isometry, TfBuffer, Time};
//! use ruro::msg::{Header, Transform};
//!
//! let mut tf = TfBuffer::new();
//! let mount = Isometry::from_translation(0.2, 0.0, 0.5);
//! tf.set(&Transform {
//!     header: Header::new("base_link", 0),
//!     child_frame_id: "camera".into(),
//!     translation: mount.vec3(),
//!     rotation: mount.quat(),
//! }, true).unwrap();
//! let p = tf.transform_point("base_link", "camera", Time::Latest, [1.0, 0.0, 0.0]).unwrap();
//! assert!((p[0] - 1.2).abs() < 1e-9);
//! ```
//!
//! Nodes: `tf/publisher` publishes a fixed transform from parameters;
//! `tf/lookup` turns a transform stream into the transform between two
//! frames. Inside your own nodes, keep a [`TfBuffer`] as state and feed it
//! from an `Input<Transforms>`.

mod buffer;
mod math;
mod nodes;

pub use buffer::{DEFAULT_CACHE_NS, TfBuffer, TfError, Time};
pub use math::Isometry;
pub use nodes::{Lookup, StaticPublisher};

/// Forces this crate to be linked so its nodes are registered.
pub fn link() {}
