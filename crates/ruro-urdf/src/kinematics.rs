use std::collections::HashMap;

use crate::{Iso, Result, Robot, invalid};

/// Result of forward kinematics.
#[derive(Debug, Clone)]
pub struct Kinematics {
    /// `(parent link, child link, parent-to-child transform)` for every joint,
    /// in the robot's joint order.
    pub joints: Vec<(String, String, Iso)>,
    /// Pose of every link in the root link's frame (root first).
    pub links: Vec<(String, Iso)>,
}

impl Kinematics {
    pub fn link_pose(&self, name: &str) -> Option<&Iso> {
        self.links.iter().find(|(n, _)| n == name).map(|(_, p)| p)
    }
}

pub(crate) fn compute(robot: &Robot, q: &[f64]) -> Result<Kinematics> {
    let nq = robot.nq();
    if q.len() != nq {
        return invalid(format!(
            "{} joint positions given, the model needs {nq}",
            q.len()
        ));
    }
    // Positions of active joints by name.
    let mut values: HashMap<&str, &[f64]> = HashMap::new();
    let mut i = 0;
    for j in robot.active_joints() {
        values.insert(&j.name, &q[i..i + j.nq()]);
        i += j.nq();
    }

    let mut joints = Vec::with_capacity(robot.joints.len());
    for j in &robot.joints {
        let t = match (&j.mimic, j.kind.nq()) {
            (_, 0) => j.origin,
            (Some(m), _) => {
                let lead = values.get(m.joint.as_str()).map_or(0.0, |v| v[0]);
                j.transform(&[lead * m.multiplier + m.offset])
            }
            (None, _) => j.transform(values[j.name.as_str()]),
        };
        joints.push((j.parent.clone(), j.child.clone(), t));
    }

    // Walk the tree from the root.
    let root = robot
        .root()
        .ok_or_else(|| crate::Error::Invalid("model has no root link".into()))?;
    let mut by_parent: HashMap<&str, Vec<usize>> = HashMap::new();
    for (k, (p, _, _)) in joints.iter().enumerate() {
        by_parent.entry(p.as_str()).or_default().push(k);
    }
    let mut links = vec![(root.to_owned(), Iso::IDENTITY)];
    let mut stack = vec![(root.to_owned(), Iso::IDENTITY)];
    while let Some((link, pose)) = stack.pop() {
        for &k in by_parent.get(link.as_str()).map_or(&[][..], |v| v) {
            let (_, child, t) = &joints[k];
            let p = pose.mul(t);
            links.push((child.clone(), p));
            stack.push((child.clone(), p));
        }
    }
    Ok(Kinematics { joints, links })
}
