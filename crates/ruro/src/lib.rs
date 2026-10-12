//! # ruro
//!
//! Fast, lightweight robotics dataflow in Rust.
//!
//! Write a node as a struct of typed ports and parameters, implement `step`,
//! and it becomes available to every ruro graph, the CLI and the studio.
//!
//! ```
//! use ruro::prelude::*;
//!
//! /// Multiplies a signal by a constant.
//! #[derive(Node)]
//! #[node(kind = "example/gain")]
//! struct Gain {
//!     #[input]
//!     input: Input<Scalar>,
//!     #[output]
//!     output: Output<Scalar>,
//!     /// Multiplier applied to every sample.
//!     #[param(default = 2.0, min = -10, max = 10)]
//!     k: f64,
//! }
//!
//! impl Node for Gain {
//!     fn step(&mut self, _ctx: &mut Ctx) -> Result {
//!         while let Some(s) = self.input.recv() {
//!             self.output.send(Scalar(s.0 * self.k))?;
//!         }
//!         Ok(())
//!     }
//! }
//! ```

pub use ruro_graph as graph;
pub use ruro_macros::{Message, Node};
pub use ruro_msg as msg;
pub use ruro_node as node;
pub use ruro_node::__private::serde_json;

pub use ruro_node::{Ctx, Input, Node, NodeSpec, Output, Result, Sample};

#[doc(hidden)]
pub use anyhow;
#[doc(hidden)]
pub use ruro_node::NodeDescriptor;
pub use tracing;

/// Everything a node author usually needs.
pub mod prelude {
    pub use crate::msg::{
        Bytes, CameraInfo, Encoding, Flag, Header, Image, Imu, JointCommand, JointState, Joy,
        LaserScan, Message as _, Odometry, PointCloud, Pose, Quat, Scalar, Text, Transform,
        Transforms, Twist, Vec3,
    };
    pub use crate::{Ctx, Input, Message, Node, Output, Result, Sample};
    pub use anyhow::{Context as _, anyhow, bail, ensure};
    pub use serde::{Deserialize, Serialize};
    pub use tracing::{debug, error, info, trace, warn};
}
