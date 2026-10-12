//! Transform tree nodes.

use ruro::prelude::*;

use crate::{Isometry, TfBuffer, Time};

/// Publishes one fixed transform, e.g. where a sensor is mounted on the robot.
/// Transforms are stamped zero, which marks them static for every tf buffer.
#[derive(Node)]
#[node(kind = "tf/publisher", rate = 1)]
pub struct StaticPublisher {
    #[output]
    tf_static: Output<Transforms>,
    /// Parent frame.
    #[param(default = "base_link")]
    parent: String,
    /// Child frame.
    #[param(default = "sensor")]
    child: String,
    /// Translation in metres.
    #[param(default = 0.0)]
    x: f64,
    #[param(default = 0.0)]
    y: f64,
    #[param(default = 0.0)]
    z: f64,
    /// Rotation about X in radians.
    #[param(default = 0.0, min = -6.3, max = 6.3)]
    roll: f64,
    /// Rotation about Y in radians.
    #[param(default = 0.0, min = -6.3, max = 6.3)]
    pitch: f64,
    /// Rotation about Z in radians.
    #[param(default = 0.0, min = -6.3, max = 6.3)]
    yaw: f64,
}

impl StaticPublisher {
    /// The transform this node publishes with its current parameters.
    pub fn transform(&self) -> Transform {
        let iso = Isometry::from_rpy(self.roll, self.pitch, self.yaw)
            .with_translation(self.x, self.y, self.z);
        Transform {
            header: Header::new(self.parent.clone(), 0),
            child_frame_id: self.child.clone(),
            translation: iso.vec3(),
            rotation: iso.quat(),
        }
    }
}

impl Node for StaticPublisher {
    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        self.tf_static.send(Transforms(vec![self.transform()]))
    }
}

/// Looks up the transform between two frames from a stream of transforms.
#[derive(Node)]
#[node(kind = "tf/lookup", rate = 10)]
pub struct Lookup {
    /// Dynamic transforms.
    #[input]
    tf: Input<Transforms>,
    /// Static transforms.
    #[input]
    tf_static: Input<Transforms>,
    /// `source` expressed in `target`, at the latest common time.
    #[output]
    transform: Output<Transform>,
    /// Frame to express the result in.
    #[param(default = "world")]
    target: String,
    /// Frame to look up.
    #[param(default = "base_link")]
    source: String,
    buffer: TfBuffer,
    last_error: Option<String>,
}

impl Node for Lookup {
    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        while let Some(m) = self.tf_static.recv() {
            if let Err(e) = self.buffer.ingest(&m, true) {
                warn!("ignored static transform: {e}");
            }
        }
        while let Some(m) = self.tf.recv() {
            if let Err(e) = self.buffer.ingest(&m, false) {
                warn!("ignored transform: {e}");
            }
        }
        match self
            .buffer
            .lookup_msg(&self.target, &self.source, Time::Latest)
        {
            Ok(t) => {
                self.last_error = None;
                self.transform.send(t)
            }
            Err(e) => {
                // Report each distinct failure once instead of on every step.
                let msg = e.to_string();
                if self.last_error.as_deref() != Some(&msg) {
                    debug!("{msg}");
                    self.last_error = Some(msg);
                }
                Ok(())
            }
        }
    }
}
