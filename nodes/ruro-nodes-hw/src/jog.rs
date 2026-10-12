//! The `hw/joint-jog` node: gamepad teleoperation in joint space.

use ruro::prelude::*;

use crate::io::parse_list;

/// Jogs joints with a gamepad: each mapped stick axis moves one joint's
/// position target at up to `speed` rad/s. The targets start from the first
/// joint state received, so the arm does not jump when teleoperation begins.
///
/// E-stop: no commands are sent while stopped (the drivers hold the arm), and
/// stick input is ignored. The targets are dropped, so after the release they
/// start again from the next joint state received, where the arm actually is,
/// and only stick input sent after the release moves it.
#[derive(Node)]
#[node(kind = "hw/joint-jog", rate = 50)]
pub struct JointJog {
    #[input]
    joy: Input<Joy>,
    /// Current joint positions (used to initialise the targets).
    #[input]
    state: Input<JointState>,
    #[output]
    command: Output<JointCommand>,
    /// Joy axis index driving each joint, comma separated; -1 leaves a joint still.
    #[param(default = "0,1,4,3,6,7")]
    axes: String,
    /// Per-joint direction (+1/-1), comma separated; empty for all +1.
    #[param(default = "")]
    directions: String,
    /// Joint speed at full stick deflection, rad/s.
    #[param(default = 0.8, min = 0, max = 10)]
    speed: f64,
    /// Stick deflection below which a joint does not move.
    #[param(default = 0.1, min = 0, max = 0.9)]
    deadzone: f64,
    /// Comma separated lower joint limits (rad); empty for none.
    #[param(default = "")]
    lower: String,
    /// Comma separated upper joint limits (rad); empty for none.
    #[param(default = "")]
    upper: String,
    target: Option<Vec<f64>>,
    stick: Vec<f32>,
    /// Stopped by the e-stop; stick input is discarded until the release.
    held: bool,
}

impl JointJog {
    /// Advances the targets by `dt` seconds for the given stick values.
    fn advance(&mut self, dt: f64) -> Result<Option<Vec<f64>>> {
        let Some(target) = self.target.as_mut() else {
            return Ok(None);
        };
        let map: Vec<i32> = parse_list(&self.axes).map_err(|e| anyhow!("axes: {e}"))?;
        let dirs: Vec<f64> =
            parse_list(&self.directions).map_err(|e| anyhow!("directions: {e}"))?;
        let lower: Vec<f64> = parse_list(&self.lower).map_err(|e| anyhow!("lower: {e}"))?;
        let upper: Vec<f64> = parse_list(&self.upper).map_err(|e| anyhow!("upper: {e}"))?;
        for (j, t) in target.iter_mut().enumerate() {
            let Some(&axis) = map.get(j).filter(|a| **a >= 0) else {
                continue;
            };
            let v = self.stick.get(axis as usize).copied().unwrap_or(0.0) as f64;
            if v.abs() < self.deadzone {
                continue;
            }
            let dir = dirs.get(j).copied().unwrap_or(1.0);
            *t += dir * v * self.speed * dt;
            if let Some(&lo) = lower.get(j) {
                *t = t.max(lo);
            }
            if let Some(&hi) = upper.get(j) {
                *t = t.min(hi);
            }
        }
        Ok(Some(target.clone()))
    }
}

impl Node for JointJog {
    fn step(&mut self, ctx: &mut Ctx) -> Result {
        if ctx.estopped() || self.held {
            // Stick input queued while stopped is stale.
            while self.joy.recv().is_some() {}
            self.stick.clear();
            self.target = None;
            self.held = ctx.estopped();
            if self.held {
                return Ok(());
            }
        } else if let Some(j) = self.joy.latest() {
            self.stick.clone_from(&j.axes);
        }
        if let Some(s) = self.state.latest()
            && self.target.is_none()
        {
            self.target = Some(s.position.clone());
        }
        let dt = ctx.dt().as_secs_f64().min(0.1);
        if let Some(position) = self.advance(dt)? {
            self.command.send(JointCommand {
                header: Header::new("", ruro::msg::now_ns()),
                position,
                ..Default::default()
            })?;
        }
        Ok(())
    }

    fn on_estop(&mut self, _ctx: &mut Ctx) {
        self.held = true;
        self.target = None;
        self.stick.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node() -> JointJog {
        let params = Default::default();
        let mut b = ruro::node::Builder::new("jog", &params, Default::default());
        <JointJog as ruro::NodeSpec>::build(&mut b).unwrap()
    }

    #[test]
    fn integrates_sticks_into_targets() {
        let mut n = node();
        n.axes = "1,-1,0".into();
        n.directions = "-1,1,1".into();
        n.speed = 2.0;
        n.upper = "10,10,0.05".into();
        assert_eq!(
            n.advance(0.1).unwrap(),
            None,
            "no targets before a joint state"
        );
        n.target = Some(vec![0.0, 1.0, 0.0]);
        n.stick = vec![0.5, -1.0, 0.0];
        let t = n.advance(0.1).unwrap().unwrap();
        // Joint 0 follows axis 1 inverted, joint 1 is unmapped, joint 2 clamps at 0.05.
        assert!((t[0] - 0.2).abs() < 1e-12);
        assert_eq!(t[1], 1.0);
        assert_eq!(t[2], 0.05);
        // Inside the dead zone nothing moves.
        n.stick = vec![0.05, 0.05];
        assert_eq!(n.advance(0.1).unwrap().unwrap(), t);
    }
}
