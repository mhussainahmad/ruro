//! Robot descriptions from URDF and MJCF, and forward kinematics.
//!
//! Both formats load into the same [`Robot`] model: a tree of links connected
//! by joints. [`Robot::forward_kinematics`] turns joint positions into the
//! transform of every joint (parent link to child link) and the pose of every
//! link in the root frame.
//!
//! ```
//! let robot = ruro_urdf::Robot::from_urdf_str(r#"
//!   <robot name="pendulum">
//!     <link name="base"/>
//!     <link name="arm"/>
//!     <joint name="hinge" type="revolute">
//!       <parent link="base"/><child link="arm"/>
//!       <origin xyz="0 0 1"/><axis xyz="0 1 0"/>
//!       <limit lower="-3" upper="3" effort="1" velocity="1"/>
//!     </joint>
//!   </robot>"#).unwrap();
//! let fk = robot.forward_kinematics(&[0.5]).unwrap();
//! assert_eq!(fk.link_pose("arm").unwrap().translation, [0.0, 0.0, 1.0]);
//! ```

mod kinematics;
pub mod math;
mod mjcf;
mod urdf;

pub use kinematics::Kinematics;
pub use math::Iso;

use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("XML: {0}")]
    Xml(#[from] roxmltree::Error),
    #[error("reading {path}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("{0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, Error>;

fn invalid<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Invalid(msg.into()))
}

/// A robot: links connected by joints into a tree.
#[derive(Debug, Clone, Default)]
pub struct Robot {
    pub name: String,
    pub links: Vec<Link>,
    /// In document order; each joint's child link appears exactly once.
    pub joints: Vec<Joint>,
}

#[derive(Debug, Clone, Default)]
pub struct Link {
    pub name: String,
    pub visuals: Vec<Shape>,
    pub collisions: Vec<Shape>,
    pub inertial: Option<Inertial>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Inertial {
    pub origin: Iso,
    pub mass: f64,
}

/// A geometric primitive or mesh placed in its link's frame.
#[derive(Debug, Clone)]
pub struct Shape {
    pub name: Option<String>,
    pub origin: Iso,
    pub geometry: Geometry,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Geometry {
    /// Full edge lengths.
    Box {
        size: [f64; 3],
    },
    /// Along the local Z axis; `length` is the full length.
    Cylinder {
        radius: f64,
        length: f64,
    },
    /// Along Z; `length` is the full length of the cylindrical part.
    Capsule {
        radius: f64,
        length: f64,
    },
    Sphere {
        radius: f64,
    },
    Ellipsoid {
        radii: [f64; 3],
    },
    /// Infinite plane through the origin with normal +Z.
    Plane,
    /// Mesh file, as written in the description (often `package://` or relative).
    Mesh {
        filename: String,
        scale: [f64; 3],
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JointType {
    Revolute,
    /// Revolute without limits.
    Continuous,
    Prismatic,
    Fixed,
    /// Six degrees of freedom; position `[x, y, z, qw, qx, qy, qz]` (MuJoCo order).
    Floating,
    /// Three rotational degrees of freedom; position `[qw, qx, qy, qz]`.
    Ball,
    /// Not supported for motion; treated as fixed.
    Planar,
}

impl JointType {
    /// Number of position coordinates this joint consumes.
    pub fn nq(self) -> usize {
        match self {
            JointType::Revolute | JointType::Continuous | JointType::Prismatic => 1,
            JointType::Floating => 7,
            JointType::Ball => 4,
            JointType::Fixed | JointType::Planar => 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    pub lower: f64,
    pub upper: f64,
    pub effort: Option<f64>,
    pub velocity: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Mimic {
    pub joint: String,
    pub multiplier: f64,
    pub offset: f64,
}

#[derive(Debug, Clone)]
pub struct Joint {
    pub name: String,
    pub kind: JointType,
    pub parent: String,
    pub child: String,
    /// Child frame relative to the parent frame at zero position.
    pub origin: Iso,
    /// Unit axis in the joint frame.
    pub axis: [f64; 3],
    /// Point (in the joint frame) the motion happens about. Zero for URDF;
    /// MJCF joints may be anchored away from their body's origin.
    pub anchor: [f64; 3],
    pub limits: Option<Limits>,
    /// Joint position that corresponds to the zero pose (MJCF `ref`).
    pub reference: f64,
    /// Follows another joint instead of taking a position of its own.
    pub mimic: Option<Mimic>,
}

impl Joint {
    /// Positional degrees of freedom driven by joint state (0 for fixed and mimic joints).
    pub fn nq(&self) -> usize {
        if self.mimic.is_some() {
            0
        } else {
            self.kind.nq()
        }
    }

    /// Transform from parent to child at position `q` (`q.len() == self.kind.nq()`).
    pub fn transform(&self, q: &[f64]) -> Iso {
        let motion = match self.kind {
            JointType::Revolute | JointType::Continuous => {
                Iso::rotation(math::quat_from_axis_angle(self.axis, q[0] - self.reference))
            }
            JointType::Prismatic => {
                let d = q[0] - self.reference;
                Iso::translation([self.axis[0] * d, self.axis[1] * d, self.axis[2] * d])
            }
            JointType::Floating => Iso::new(
                [q[0], q[1], q[2]],
                math::quat_normalize([q[4], q[5], q[6], q[3]]),
            ),
            JointType::Ball => Iso::rotation(math::quat_normalize([q[1], q[2], q[3], q[0]])),
            JointType::Fixed | JointType::Planar => return self.origin,
        };
        if self.anchor == [0.0; 3] {
            self.origin.mul(&motion)
        } else {
            let a = self.anchor;
            self.origin
                .mul(&Iso::translation(a))
                .mul(&motion)
                .mul(&Iso::translation([-a[0], -a[1], -a[2]]))
        }
    }
}

impl Robot {
    /// Parses a URDF document.
    pub fn from_urdf_str(xml: &str) -> Result<Self> {
        urdf::parse(xml)
    }

    /// Parses an MJCF (MuJoCo XML) document. Bodies become links; a body with
    /// several joints is split into intermediate links named `body/joint`.
    pub fn from_mjcf_str(xml: &str) -> Result<Self> {
        mjcf::parse(xml)
    }

    /// Loads a `.urdf` or MJCF `.xml` file, picking the format from the root element.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|source| Error::Io {
            path: path.display().to_string(),
            source,
        })?;
        Self::from_str_any(&text)
    }

    /// Parses either format, picking it from the root element.
    pub fn from_str_any(xml: &str) -> Result<Self> {
        let doc = roxmltree::Document::parse(xml)?;
        match doc.root_element().tag_name().name() {
            "robot" => urdf::parse(xml),
            "mujoco" => mjcf::parse(xml),
            other => invalid(format!(
                "unknown root element <{other}> (expected <robot> or <mujoco>)"
            )),
        }
    }

    pub fn link(&self, name: &str) -> Option<&Link> {
        self.links.iter().find(|l| l.name == name)
    }

    pub fn joint(&self, name: &str) -> Option<&Joint> {
        self.joints.iter().find(|j| j.name == name)
    }

    /// The link that is no joint's child.
    pub fn root(&self) -> Option<&str> {
        self.links
            .iter()
            .map(|l| l.name.as_str())
            .find(|n| !self.joints.iter().any(|j| j.child == *n))
    }

    /// Joints that take a position, in the order `forward_kinematics` expects them.
    pub fn active_joints(&self) -> impl Iterator<Item = &Joint> {
        self.joints.iter().filter(|j| j.nq() > 0)
    }

    /// Total number of position coordinates.
    pub fn nq(&self) -> usize {
        self.active_joints().map(Joint::nq).sum()
    }

    /// Checks the tree structure: unique names, known links, one parent per link.
    pub fn validate(&self) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        for l in &self.links {
            if !seen.insert(l.name.as_str()) {
                return invalid(format!("duplicate link `{}`", l.name));
            }
        }
        let mut children = std::collections::HashSet::new();
        for j in &self.joints {
            for l in [&j.parent, &j.child] {
                if !seen.contains(l.as_str()) {
                    return invalid(format!("joint `{}` references unknown link `{l}`", j.name));
                }
            }
            if !children.insert(j.child.as_str()) {
                return invalid(format!("link `{}` has more than one parent", j.child));
            }
            if let Some(m) = &j.mimic
                && self.joint(&m.joint).is_none_or(|t| t.kind.nq() != 1)
            {
                return invalid(format!(
                    "joint `{}` mimics `{}`, which is not a single-axis joint",
                    j.name, m.joint
                ));
            }
        }
        let roots = self.links.len() - children.len();
        if roots != 1 {
            return invalid(format!("expected one root link, found {roots}"));
        }
        Ok(())
    }

    /// Computes every joint transform and link pose for joint positions `q`
    /// (concatenated in [`Robot::active_joints`] order).
    pub fn forward_kinematics(&self, q: &[f64]) -> Result<Kinematics> {
        kinematics::compute(self, q)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_errors_name_the_file_and_chain_the_cause_once() {
        let err = Robot::from_file("/nonexistent/robot.urdf").unwrap_err();
        assert_eq!(err.to_string(), "reading /nonexistent/robot.urdf");
        let cause = std::error::Error::source(&err).expect("io error as the source");
        assert!(cause.to_string().contains("No such file"));
    }
}
