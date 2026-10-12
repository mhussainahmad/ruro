use ruro::prelude::*;
use ruro_urdf::{Iso, Robot};

fn to_transform(parent: &str, child: &str, t: &Iso, prefix: &str, stamp: u64) -> Transform {
    let [x, y, z] = t.translation;
    let [qx, qy, qz, qw] = t.rotation;
    Transform {
        header: Header::new(format!("{prefix}{parent}"), stamp),
        child_frame_id: format!("{prefix}{child}"),
        translation: Vec3 { x, y, z },
        rotation: Quat {
            x: qx,
            y: qy,
            z: qz,
            w: qw,
        },
    }
}

/// Forward kinematics of a URDF or MJCF robot model: publishes the
/// parent-to-child transform of every joint for each incoming joint state,
/// like ROS's robot_state_publisher. Joint positions are matched to the
/// model's movable joints in document order (MJCF: MuJoCo qpos order).
#[derive(Node)]
#[node(kind = "sim/robot-state")]
pub struct RobotState {
    /// Joint positions, one per movable joint (mimic joints excluded).
    #[input]
    joints: Input<JointState>,
    /// One transform per joint, fixed joints included.
    #[output]
    transforms: Output<Transforms>,
    /// URDF (`.urdf`) or MJCF (`.xml`) file.
    #[param(default = "")]
    model: String,
    /// Prepended to every frame name (e.g. `left/`).
    #[param(default = "")]
    prefix: String,
    robot: Option<Robot>,
}

impl RobotState {
    /// Uses an already loaded model instead of the `model` file.
    pub fn with_robot(&mut self, robot: Robot) {
        self.robot = Some(robot);
    }

    fn transforms(&self, q: &[f64], stamp: u64) -> Result<Transforms> {
        let robot = self.robot.as_ref().context("no model loaded")?;
        let fk = robot.forward_kinematics(q)?;
        Ok(Transforms(
            fk.joints
                .iter()
                .map(|(p, c, t)| to_transform(p, c, t, &self.prefix, stamp))
                .collect(),
        ))
    }
}

impl Node for RobotState {
    fn start(&mut self, _ctx: &mut Ctx) -> Result {
        if self.robot.is_none() {
            ensure!(
                !self.model.is_empty(),
                "set the `model` parameter to a URDF or MJCF file"
            );
            let robot = Robot::from_file(&self.model)?;
            info!(
                "{}: `{}` with {} links, {} joint positions",
                self.model,
                robot.name,
                robot.links.len(),
                robot.nq()
            );
            self.robot = Some(robot);
        }
        Ok(())
    }

    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        let Some(js) = self.joints.latest() else {
            return Ok(());
        };
        let t = self.transforms(&js.position, js.stamp_ns)?;
        self.transforms.send(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transforms_follow_joint_positions() {
        let robot = Robot::from_urdf_str(
            r#"<robot name="r"><link name="base"/><link name="arm"/>
               <joint name="j" type="continuous"><parent link="base"/><child link="arm"/>
               <origin xyz="0 0 0.5"/><axis xyz="0 0 1"/></joint></robot>"#,
        )
        .unwrap();
        let params = Default::default();
        let mut b = ruro::node::Builder::new("rs", &params, Default::default());
        let mut node = <RobotState as ruro::NodeSpec>::build(&mut b).unwrap();
        node.prefix = "r1/".into();
        node.with_robot(robot);
        let t = node.transforms(&[std::f64::consts::PI], 7).unwrap();
        assert_eq!(t.0.len(), 1);
        let tf = &t.0[0];
        assert_eq!(
            (tf.header.frame_id.as_str(), tf.child_frame_id.as_str()),
            ("r1/base", "r1/arm")
        );
        assert_eq!(tf.header.stamp_ns, 7);
        assert_eq!(tf.translation.z, 0.5);
        assert!((tf.rotation.z.abs() - 1.0).abs() < 1e-12);
        assert!(node.transforms(&[1.0, 2.0], 0).is_err());
    }
}
