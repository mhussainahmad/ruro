//! Simulation nodes (`sim/*`).
//!
//! - `sim/robot-state`: forward kinematics of a URDF or MJCF model; turns
//!   joint states into the transforms of every link (always available).
//! - `sim/mujoco`: MuJoCo physics, loaded at run time (cargo feature `mujoco`;
//!   point `RURO_MUJOCO_LIB` at `libmujoco.so`).

#[cfg(feature = "mujoco")]
pub mod mujoco;
mod robot_state;

pub use robot_state::RobotState;

use ruro::prelude::*;

/// Named sensor readings, concatenated; sensor `i` has `dims[i]` values.
#[derive(Message, Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[message(name = "sim/SensorReadings")]
pub struct SensorReadings {
    pub names: Vec<String>,
    pub dims: Vec<u32>,
    pub values: Vec<f64>,
}

impl SensorReadings {
    /// The values of the sensor called `name`.
    pub fn get(&self, name: &str) -> Option<&[f64]> {
        let mut start = 0;
        for (n, &d) in self.names.iter().zip(&self.dims) {
            let end = start + d as usize;
            if n == name {
                return self.values.get(start..end);
            }
            start = end;
        }
        None
    }
}

/// Forces this crate to be linked. Hosts call it so the node registrations
/// survive dead-code elimination.
pub fn link() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensor_lookup() {
        let s = SensorReadings {
            names: vec!["a".into(), "accel".into(), "b".into()],
            dims: vec![1, 3, 1],
            values: vec![1.0, 2.0, 3.0, 4.0, 5.0],
        };
        assert_eq!(s.get("accel"), Some(&[2.0, 3.0, 4.0][..]));
        assert_eq!(s.get("b"), Some(&[5.0][..]));
        assert_eq!(s.get("c"), None);
    }
}
