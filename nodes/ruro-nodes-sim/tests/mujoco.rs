//! MuJoCo integration tests. They need the `mujoco` feature and the MuJoCo
//! shared library, and are skipped unless RURO_MUJOCO_LIB is set:
//!
//! RURO_MUJOCO_LIB=/path/to/libmujoco.so.3.15.0 cargo test -p ruro-nodes-sim --features mujoco

#![cfg(feature = "mujoco")]

use std::f64::consts::PI;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use ruro::node::testing::Harness;
use ruro::prelude::*;
use ruro_graph::Graph;
use ruro_host::{Executor, Remote};
use ruro_nodes_sim::mujoco::{Api, LIB_ENV, Sim};
use ruro_urdf::{Iso, Robot};

const G: f64 = 9.81;
const L: f64 = 0.8;
const M: f64 = 1.3;
const IYY: f64 = 1e-4;

fn pendulum_xml(damping: f64) -> String {
    format!(
        r#"<mujoco model="pendulum">
  <option timestep="0.0005" integrator="RK4" gravity="0 0 -{G}"/>
  <worldbody>
    <body name="pole" pos="0 0 2">
      <joint name="hinge" type="hinge" axis="0 1 0" damping="{damping}"/>
      <inertial pos="0 0 -{L}" mass="{M}" diaginertia="{IYY} {IYY} {IYY}"/>
      <geom type="capsule" fromto="0 0 0 0 0 -{L}" size="0.01" mass="0"/>
      <site name="tip" pos="0 0 -{L}"/>
    </body>
  </worldbody>
  <actuator><motor name="torque" joint="hinge" gear="1"/></actuator>
  <sensor><jointpos name="angle" joint="hinge"/><framepos name="tip_pos" objtype="site" objname="tip"/></sensor>
</mujoco>"#
    )
}

fn write_model(name: &str, xml: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ruro-mujoco-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, xml).unwrap();
    path
}

fn library() -> Option<String> {
    let lib = std::env::var(LIB_ENV).ok();
    if lib.is_none() {
        eprintln!("skipping: set {LIB_ENV} to run MuJoCo tests");
    }
    lib
}

fn load(name: &str, xml: &str) -> Sim {
    let path = write_model(name, xml);
    Sim::load(Api::load(None).unwrap(), &path).unwrap()
}

#[test]
fn pendulum_period_and_energy() {
    if library().is_none() {
        return;
    }
    let mut sim = load("pendulum.xml", &pendulum_xml(0.0));
    assert_eq!((sim.nq, sim.nv, sim.nu, sim.nbody), (1, 1, 1, 2));
    assert_eq!(sim.timestep(), 0.0005);

    let theta0 = 0.3;
    sim.qpos_mut()[0] = theta0;
    sim.forward();
    let inertia = M * L * L + IYY;
    let energy = |s: &Sim| 0.5 * inertia * s.qvel()[0].powi(2) - M * G * L * s.qpos()[0].cos();
    let e0 = energy(&sim);

    // Period from upward zero crossings over 20 s, interpolated.
    let mut crossings = Vec::new();
    let mut max_drift: f64 = 0.0;
    let mut prev = (sim.time(), sim.qpos()[0]);
    while sim.time() < 20.0 {
        sim.step();
        let now = (sim.time(), sim.qpos()[0]);
        if prev.1 < 0.0 && now.1 >= 0.0 {
            crossings.push(prev.0 + (now.0 - prev.0) * (-prev.1) / (now.1 - prev.1));
        }
        prev = now;
        max_drift = max_drift.max((energy(&sim) - e0).abs());
    }
    let measured = (crossings.last().unwrap() - crossings[0]) / (crossings.len() - 1) as f64;
    let small_angle = 2.0 * PI * (inertia / (M * G * L)).sqrt();
    // Large-amplitude correction: T = T0 (1 + θ²/16 + 11θ⁴/3072 + ...).
    let expected = small_angle * (1.0 + theta0.powi(2) / 16.0 + 11.0 * theta0.powi(4) / 3072.0);
    println!(
        "MuJoCo {}: period {measured:.6} s, expected {expected:.6} s (small-angle {small_angle:.6} s), \
         {} cycles, max energy drift {:.2e} J of {:.3} J",
        sim.version(),
        crossings.len() - 1,
        max_drift,
        e0.abs()
    );
    assert!(crossings.len() > 8);
    assert!(
        (measured - expected).abs() / expected < 1e-4,
        "period {measured} vs {expected}"
    );
    assert!(max_drift < 1e-6 * e0.abs(), "energy drifted by {max_drift}");

    // Sensors: joint angle and the tip position agree with the state
    // (rotating about +Y swings the tip at (0, 0, -L) towards -X).
    sim.forward();
    let sensors = sim.sensors();
    assert_eq!(sensors[0], ("angle".to_owned(), 0, 1));
    assert_eq!(sensors[1].2, 3);
    let data = sim.sensordata();
    let th = sim.qpos()[0];
    assert!((data[0] - th).abs() < 1e-12);
    let tip = &data[1..4];
    assert!((tip[0] + L * th.sin()).abs() < 1e-9 && (tip[2] - (2.0 - L * th.cos())).abs() < 1e-9);
}

#[test]
fn damped_pendulum_loses_energy_and_motor_holds_it() {
    if library().is_none() {
        return;
    }
    let mut sim = load("damped.xml", &pendulum_xml(0.5));
    sim.qpos_mut()[0] = 1.0;
    while sim.time() < 10.0 {
        sim.step();
    }
    assert!(
        sim.qpos()[0].abs() < 0.1,
        "damping should settle the pendulum"
    );

    // Gravity exerts -m g l sin θ about +Y, so a constant motor torque of
    // m g l sin θ* holds the pendulum at θ*.
    sim.reset();
    let target: f64 = -0.4;
    sim.ctrl_mut()[0] = M * G * L * target.sin();
    while sim.time() < 20.0 {
        sim.step();
    }
    assert!((sim.qpos()[0] - target).abs() < 1e-3, "{}", sim.qpos()[0]);
}

/// MuJoCo's own body poses agree with ruro-urdf's MJCF forward kinematics.
#[test]
fn body_poses_match_ruro_urdf_kinematics() {
    if library().is_none() {
        return;
    }
    let xml = r#"<mujoco model="mix">
  <compiler angle="degree" eulerseq="zyx"/>
  <default><joint axis="1 0 0"/><default class="side"><joint axis="0 0 1"/></default></default>
  <worldbody>
    <body name="base" pos="0.1 0.2 0.3" euler="10 20 30"><freejoint/>
      <geom size="0.1"/>
      <body name="link1" pos="0 0 0.5" axisangle="1 1 0 30">
        <joint name="j1" pos="0 0 -0.1"/>
        <joint name="j2" class="side" range="-90 90"/>
        <geom size="0.05"/>
        <frame pos="0.2 0 0" quat="0.9238795 0 0.3826834 0">
          <body name="link2" xyaxes="0 1 0 -1 0 0">
            <joint name="slide" type="slide" axis="0 1 0" range="-1 1"/>
            <geom size="0.05"/>
            <body name="ball_end" pos="0 0 -0.3" zaxis="1 1 1">
              <joint name="ball" type="ball"/>
              <geom size="0.05"/>
            </body>
          </body>
        </frame>
      </body>
    </body>
  </worldbody>
</mujoco>"#;
    let mut sim = load("mix.xml", xml);
    let robot = Robot::from_mjcf_str(xml).unwrap();
    assert_eq!(robot.nq(), sim.nq);

    let n = (0.5f64).sqrt();
    let q = [
        0.3, -0.2, 1.1, n, 0.0, n, 0.0,  // free: pos, wxyz
        0.7,  // j1
        -0.4, // j2
        0.25, // slide
        0.5, 0.5, 0.5, 0.5, // ball (wxyz)
    ];
    sim.qpos_mut().copy_from_slice(&q);
    sim.forward();
    let fk = robot.forward_kinematics(&q).unwrap();
    let (xpos, xquat) = (sim.xpos().to_vec(), sim.xquat().to_vec());
    for b in 1..sim.nbody {
        let name = sim.body_name(b);
        let mj = Iso::new(
            [xpos[3 * b], xpos[3 * b + 1], xpos[3 * b + 2]],
            [
                xquat[4 * b + 1],
                xquat[4 * b + 2],
                xquat[4 * b + 3],
                xquat[4 * b],
            ],
        );
        let ours = fk.link_pose(&name).unwrap();
        assert!(
            mj.approx_eq(ours, 1e-6),
            "{name}: MuJoCo {mj:?} vs ruro {ours:?}"
        );
    }
}

/// The example SO-100 model loads in MuJoCo and its position actuators reach a target.
#[test]
fn example_arm_tracks_position_targets() {
    if library().is_none() {
        return;
    }
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/models/so100.xml");
    let mut sim = Sim::load(Api::load(None).unwrap(), &path).unwrap();
    assert_eq!((sim.nq, sim.nu), (6, 6));
    let target = [0.5, -0.4, 0.8, 0.3, -1.0, 0.6];
    sim.ctrl_mut().copy_from_slice(&target);
    while sim.time() < 3.0 {
        sim.step();
    }
    for (q, t) in sim.qpos().iter().zip(target) {
        assert!((q - t).abs() < 0.05, "{:?} vs {target:?}", sim.qpos());
    }
    // MuJoCo and ruro-urdf agree on where the hand is.
    sim.forward();
    let robot = Robot::from_file(&path).unwrap();
    let fk = robot.forward_kinematics(sim.qpos()).unwrap();
    let hand = sim.id(1, "hand").unwrap();
    let ours = fk.link_pose("hand").unwrap().translation;
    for (mj, o) in sim.xpos()[3 * hand..3 * hand + 3].iter().zip(ours) {
        assert!((mj - o).abs() < 1e-9);
    }
}

static TIMES: Mutex<Vec<f64>> = Mutex::new(Vec::new());
static STATES: AtomicU64 = AtomicU64::new(0);

#[derive(Node)]
#[node(kind = "test/sim-probe")]
struct SimProbe {
    #[input]
    time: Input<ruro::msg::Clock>,
    #[input]
    state: Input<JointState>,
}

impl Node for SimProbe {
    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        while let Some(t) = self.time.recv() {
            TIMES.lock().push(t.time_ns as f64 * 1e-9);
        }
        while let Some(s) = self.state.recv() {
            assert_eq!(s.position.len(), 1);
            STATES.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }
}

#[test]
fn node_runs_in_real_time() {
    if library().is_none() {
        return;
    }
    ruro_nodes_sim::link();
    let path = write_model("node.xml", &pendulum_xml(0.0));
    let graph = Graph::from_yaml(&format!(
        r#"
name: sim
nodes:
  sim: {{ type: sim/mujoco, rate: 100, params: {{ model: "{}", real_time_factor: 2.0 }} }}
  probe: {{ type: test/sim-probe }}
wires: [sim.clock -> probe.time, sim.state -> probe.state]
"#,
        path.display()
    ))
    .unwrap();
    let exec = Executor::start(&graph, "main", Remote::Disabled).unwrap();
    let start = Instant::now();
    std::thread::sleep(Duration::from_millis(1000));
    let wall = start.elapsed().as_secs_f64();
    let m = exec.metrics();
    exec.stop();
    let sim = m.nodes.iter().find(|n| n.id == "sim").unwrap();
    assert!(sim.last_error.is_none(), "{:?}", sim.last_error);
    let last = *TIMES.lock().last().unwrap();
    println!("simulated {last:.3} s in {wall:.3} s of wall time at real_time_factor 2");
    assert!(
        (last - 2.0 * wall).abs() < 0.15,
        "sim time {last} after {wall} s"
    );
    assert!(STATES.load(Ordering::Relaxed) > 50);
}

/// Records the graph time and step of each tick.
#[derive(Node)]
#[node(kind = "test/sim-ticker")]
struct SimTicker {
    #[output]
    out: Output<Scalar>,
}

static TICKS: Mutex<Vec<(u64, Duration)>> = Mutex::new(Vec::new());

impl Node for SimTicker {
    fn step(&mut self, ctx: &mut Ctx) -> Result {
        TICKS.lock().push((ctx.now_ns(), ctx.dt()));
        self.out.send(Scalar(0.0))
    }
}

#[test]
fn timers_follow_simulation_time() {
    if library().is_none() {
        return;
    }
    ruro_nodes_sim::link();
    // Simulated time runs at twice wall speed, so a 50 Hz timer (on graph
    // time) ticks about 100 times per wall second, 20 ms of graph time apart.
    let path = write_model("clock.xml", &pendulum_xml(0.0));
    let graph = Graph::from_yaml(&format!(
        r#"
name: sim-clock
clock: sim
nodes:
  sim: {{ type: sim/mujoco, rate: 100, params: {{ model: "{}", real_time_factor: 2.0 }} }}
  tick: {{ type: test/sim-ticker, rate: 50 }}
"#,
        path.display()
    ))
    .unwrap();
    let exec = Executor::start(&graph, "main", Remote::Disabled).unwrap();
    let start = Instant::now();
    std::thread::sleep(Duration::from_millis(1000));
    let wall = start.elapsed().as_secs_f64();
    let sim_now = exec.clock().now_ns();
    let m = exec.metrics();
    exec.stop();
    let sim = m.nodes.iter().find(|n| n.id == "sim").unwrap();
    assert!(sim.last_error.is_none(), "{:?}", sim.last_error);
    let ticks = TICKS.lock().clone();
    println!(
        "{} ticks in {wall:.3} s wall, graph time {:.3} s",
        ticks.len(),
        sim_now as f64 * 1e-9
    );
    assert!(
        (sim_now as f64 * 1e-9 - 2.0 * wall).abs() < 0.15,
        "graph time is simulation time"
    );
    let expected = 100.0 * wall;
    assert!(
        (ticks.len() as f64 - expected).abs() < 15.0,
        "{} ticks, expected about {expected:.0}",
        ticks.len()
    );
    for w in ticks.windows(2).skip(1) {
        assert_eq!(w[1].1, Duration::from_millis(20), "dt follows graph time");
        assert_eq!(w[1].0 - w[0].0, 20_000_000, "now_ns is graph time");
    }
    assert!(ticks.last().unwrap().0 <= sim_now);
}

fn positions(h: &mut Harness) -> Vec<f64> {
    let s: Vec<JointState> = h.take("state").unwrap();
    s.last().unwrap().position.clone()
}

#[test]
fn estop_zeroes_motor_controls_while_physics_runs() {
    if library().is_none() {
        return;
    }
    ruro_nodes_sim::link();
    // Damped pendulum with a torque motor; each step simulates 1 s.
    let path = write_model("estop-motor.xml", &pendulum_xml(0.5));
    let mut h = Harness::new(
        "sim/mujoco",
        ruro::serde_json::json!({
            "model": path.display().to_string(),
            "real_time_factor": 0.0,
            "max_steps": 2000,
            "command_field": "effort",
        }),
    )
    .unwrap();
    let torque = |t: f64| JointCommand {
        effort: vec![t],
        ..Default::default()
    };
    // m g l sin θ = 5 Nm holds the pendulum about 0.5 rad out.
    h.push("command", torque(5.0)).unwrap();
    for _ in 0..20 {
        h.step().unwrap();
    }
    let pushed = positions(&mut h)[0];
    assert!(
        (pushed.abs() - (5.0 / (M * G * L)).asin()).abs() < 0.02,
        "{pushed}"
    );

    h.set_estop(true);
    for _ in 0..20 {
        h.push("command", torque(5.0)).unwrap();
        h.step().unwrap();
    }
    let clocks: Vec<ruro::msg::Clock> = h.take("clock").unwrap();
    assert!(
        clocks.windows(2).all(|w| w[1].time_ns > w[0].time_ns),
        "physics keeps running"
    );
    assert!(
        positions(&mut h)[0].abs() < 0.02,
        "zero torque: the pendulum hangs down"
    );

    h.push("command", torque(5.0)).unwrap();
    h.set_estop(false);
    for _ in 0..3 {
        h.step().unwrap();
    }
    assert!(positions(&mut h)[0].abs() < 0.02, "stale command dropped");
    h.push("command", torque(5.0)).unwrap();
    for _ in 0..3 {
        h.step().unwrap();
    }
    assert!(
        positions(&mut h)[0].abs() > 0.3,
        "fresh commands are followed"
    );
}

#[test]
fn estop_holds_position_actuators_where_the_arm_is() {
    if library().is_none() {
        return;
    }
    ruro_nodes_sim::link();
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/models/so100.xml");
    // Each step simulates 0.1 s.
    let mut h = Harness::new(
        "sim/mujoco",
        ruro::serde_json::json!({
            "model": path.display().to_string(),
            "real_time_factor": 0.0,
            "max_steps": 50,
        }),
    )
    .unwrap();
    let target = [0.8, -0.6, 1.0, 0.5, -1.5, 1.0];
    let goto = |p: &[f64]| JointCommand {
        position: p.to_vec(),
        ..Default::default()
    };
    h.push("command", goto(&target)).unwrap();
    h.step().unwrap();
    let at_stop = positions(&mut h);
    let dist = |a: &[f64], b: &[f64]| {
        a.iter()
            .zip(b)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0, f64::max)
    };
    assert!(
        dist(&at_stop, &target) > 0.2,
        "still on the way: {at_stop:?}"
    );

    h.set_estop(true);
    for _ in 0..30 {
        h.push("command", goto(&target)).unwrap();
        h.step().unwrap();
    }
    let held = positions(&mut h);
    println!("stopped at {at_stop:?}, settled at {held:?}");
    assert!(
        dist(&held, &at_stop) < 0.15,
        "holds near where it was: {at_stop:?} -> {held:?}"
    );
    assert!(dist(&held, &target) > 0.2, "did not go on to the target");
    assert!(dist(&held, &[0.0; 6]) > 0.1, "did not go to the zero pose");

    h.push("command", goto(&target)).unwrap();
    h.set_estop(false);
    for _ in 0..10 {
        h.step().unwrap();
    }
    assert!(
        dist(&positions(&mut h), &held) < 0.05,
        "stale command dropped"
    );
    h.push("command", goto(&target)).unwrap();
    for _ in 0..30 {
        h.step().unwrap();
    }
    assert!(
        dist(&positions(&mut h), &target) < 0.05,
        "fresh commands are followed"
    );
}

/// The simulated example graph follows the MuJoCo clock.
#[test]
fn example_graph_runs_on_the_simulation_clock() {
    ruro_nodes_sim::link();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/arm-sim.yaml");
    let graph = Graph::load(path).unwrap();
    assert_eq!(
        ruro_host::clock_port(&graph).unwrap(),
        Some(ruro_graph::PortRef::new("arm", "clock"))
    );
}
