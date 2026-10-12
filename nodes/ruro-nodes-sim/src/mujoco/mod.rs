//! The `sim/mujoco` node (feature `mujoco`).

pub mod ffi;

use std::path::Path;
use std::time::Instant;

use ruro::msg::Clock;
use ruro::prelude::*;

pub use ffi::{Api, LIB_ENV, Sim};

use crate::SensorReadings;

/// MuJoCo physics. Loads an MJCF model, writes joint commands into the
/// actuator controls, steps in (scaled) real time and publishes joint states,
/// world poses of all bodies, sensor readings and the simulation time.
///
/// The simulation time goes out as a `std/Clock`, so a graph with
/// `clock: sim` runs its timers on simulated time.
///
/// E-stop: the physics keeps running and commands are ignored while stopped.
/// With `command_field: velocity` or `effort` the controls are set to zero.
/// With `command_field: position` (position actuators, where zero would be a
/// move to the zero pose) each actuator holds the present position of its
/// joint, taking actuator i to drive the i-th hinge or slide joint; models
/// with a different number of actuators and joints keep their last controls
/// instead. After the release the controls stay until a command arrives
/// after the release (commands sent during the stop are discarded).
///
/// The MuJoCo shared library is loaded at run time from `library`, the
/// `RURO_MUJOCO_LIB` environment variable or the system search path.
#[derive(Node)]
#[node(kind = "sim/mujoco", rate = 100)]
pub struct MuJoCo {
    /// Actuator controls, one value per actuator in model order (see `command_field`).
    #[input]
    command: Input<JointCommand>,
    /// Position and velocity of every hinge and slide joint, in model order.
    #[output]
    state: Output<JointState>,
    /// World pose of every body (parent frame `world`).
    #[output]
    bodies: Output<Transforms>,
    /// All sensor readings.
    #[output]
    sensors: Output<SensorReadings>,
    /// Simulation time (graph time for `clock: sim`), sent after the other outputs.
    #[output]
    clock: Output<Clock>,
    /// MJCF model file.
    #[param(default = "")]
    model: String,
    /// Path to libmujoco.so; empty uses RURO_MUJOCO_LIB.
    #[param(default = "")]
    library: String,
    /// Simulated seconds per wall-clock second; 0 runs `max_steps` physics steps per tick.
    #[param(default = 1.0, min = 0, max = 100)]
    real_time_factor: f64,
    /// Most physics steps per tick (bounds the catch-up after stalls).
    #[param(default = 1000, min = 1, max = 1000000)]
    max_steps: u32,
    /// JointCommand field copied into the controls: `position`, `velocity` or `effort`.
    #[param(default = "position")]
    command_field: String,
    sim: Option<Sim>,
    /// Wall-clock instant and simulation time the real-time pacing starts from.
    anchor: Option<(Instant, f64)>,
    hinge_joints: Vec<(usize, usize)>,
    sensor_layout: Vec<(String, usize, usize)>,
    body_names: Vec<String>,
    /// Stopped by the e-stop; commands are discarded until one arrives after the release.
    held: bool,
}

impl MuJoCo {
    /// Puts the controls in their e-stop state (see the node documentation).
    fn engage(&mut self) {
        self.held = true;
        let Some(sim) = self.sim.as_mut() else {
            return;
        };
        if self.command_field != "position" {
            sim.ctrl_mut().fill(0.0);
        } else if sim.nu == self.hinge_joints.len() {
            let present: Vec<f64> = self
                .hinge_joints
                .iter()
                .map(|&(q, _)| sim.qpos()[q])
                .collect();
            sim.ctrl_mut().copy_from_slice(&present);
        } else {
            warn!(
                "e-stop: {} actuators for {} joints; keeping the last controls",
                sim.nu,
                self.hinge_joints.len()
            );
        }
    }

    fn apply_command(&mut self, cmd: &JointCommand) -> Result {
        let values = match self.command_field.as_str() {
            "position" => &cmd.position,
            "velocity" => &cmd.velocity,
            "effort" => &cmd.effort,
            other => bail!("command_field must be position, velocity or effort, not `{other}`"),
        };
        let sim = self.sim.as_mut().context("not started")?;
        let ctrl = sim.ctrl_mut();
        ensure!(
            values.len() <= ctrl.len(),
            "command has {} values for {} actuators",
            values.len(),
            ctrl.len()
        );
        ctrl[..values.len()].copy_from_slice(values);
        Ok(())
    }

    fn publish(&mut self) -> Result {
        let sim = self.sim.as_ref().context("not started")?;
        let (qpos, qvel) = (sim.qpos(), sim.qvel());
        self.state.send(JointState {
            position: self.hinge_joints.iter().map(|&(q, _)| qpos[q]).collect(),
            velocity: self.hinge_joints.iter().map(|&(_, d)| qvel[d]).collect(),
            effort: Vec::new(),
        })?;
        let stamp = ruro::msg::now_ns();
        let (xpos, xquat) = (sim.xpos(), sim.xquat());
        let transforms = (1..sim.nbody)
            .map(|b| Transform {
                header: Header::new("world", stamp),
                child_frame_id: self.body_names[b].clone(),
                translation: Vec3 {
                    x: xpos[3 * b],
                    y: xpos[3 * b + 1],
                    z: xpos[3 * b + 2],
                },
                rotation: Quat {
                    w: xquat[4 * b],
                    x: xquat[4 * b + 1],
                    y: xquat[4 * b + 2],
                    z: xquat[4 * b + 3],
                },
            })
            .collect();
        self.bodies.send(Transforms(transforms))?;
        if !self.sensor_layout.is_empty() {
            let data = sim.sensordata();
            self.sensors.send(SensorReadings {
                names: self.sensor_layout.iter().map(|s| s.0.clone()).collect(),
                dims: self.sensor_layout.iter().map(|s| s.2 as u32).collect(),
                values: self
                    .sensor_layout
                    .iter()
                    .flat_map(|&(_, a, d)| data[a..a + d].iter().copied())
                    .collect(),
            })?;
        }
        self.clock.send(Clock {
            time_ns: (sim.time() * 1e9).round() as u64,
        })
    }
}

impl Node for MuJoCo {
    fn start(&mut self, _ctx: &mut Ctx) -> Result {
        ensure!(
            !self.model.is_empty(),
            "set the `model` parameter to an MJCF file"
        );
        let api = Api::load(Some(&self.library)).map_err(|e| anyhow!(e))?;
        let sim = Sim::load(api, Path::new(&self.model)).map_err(|e| anyhow!(e))?;
        info!(
            "MuJoCo {}: {} (nq {}, nu {}, {} bodies, dt {} s)",
            sim.version(),
            self.model,
            sim.nq,
            sim.nu,
            sim.nbody,
            sim.timestep()
        );
        self.hinge_joints = sim
            .joints()
            .into_iter()
            .filter(|(t, _, _)| matches!(*t, ffi::joint_type::HINGE | ffi::joint_type::SLIDE))
            .map(|(_, q, d)| (q, d))
            .collect();
        self.sensor_layout = sim.sensors();
        self.body_names = (0..sim.nbody).map(|b| sim.body_name(b)).collect();
        self.sim = Some(sim);
        self.anchor = None;
        Ok(())
    }

    fn step(&mut self, ctx: &mut Ctx) -> Result {
        if ctx.estopped() && !self.held {
            self.engage();
        }
        if self.held {
            // Commands queued while stopped are stale.
            while self.command.recv().is_some() {}
            self.held = ctx.estopped();
        } else if let Some(cmd) = self.command.latest() {
            self.apply_command(&cmd)?;
        }
        let rtf = self.real_time_factor;
        let max_steps = self.max_steps;
        let sim = self.sim.as_mut().context("not started")?;
        let now = Instant::now();
        let (wall0, sim0) = *self.anchor.get_or_insert((now, sim.time()));
        let steps = if rtf > 0.0 {
            let target = sim0 + now.duration_since(wall0).as_secs_f64() * rtf;
            let behind = ((target - sim.time()) / sim.timestep()).floor().max(0.0) as u64;
            if behind > max_steps as u64 {
                // Too slow to keep up: run what we can and re-anchor the clock.
                self.anchor = None;
                max_steps as u64
            } else {
                behind
            }
        } else {
            max_steps as u64
        };
        for _ in 0..steps {
            sim.step();
        }
        // mj_step leaves positions-dependent quantities one step behind.
        sim.forward();
        self.publish()
    }

    fn on_estop(&mut self, _ctx: &mut Ctx) {
        self.engage();
    }
}
