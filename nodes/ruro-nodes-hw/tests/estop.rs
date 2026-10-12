//! E-stop behaviour of the actuator drivers, checked byte for byte against
//! the simulated devices behind a mock transport.

use std::sync::Arc;

use parking_lot::Mutex;
use ruro::msg::{JointCommand, JointState, Joy};
use ruro::node::testing::Harness;
use ruro_nodes_hw::io::MockTransport;
use ruro_nodes_hw::{Dynamixel, ODrive, So100Arm, dynamixel, feetech, odrive};
use serde_json::json;

type Log = Arc<Mutex<Vec<Vec<u8>>>>;

/// A mock transport answering through `device`, logging every packet written.
fn logged(mut device: impl FnMut(&[u8]) -> Vec<u8> + Send + 'static) -> (MockTransport, Log) {
    let log = Log::default();
    let l = log.clone();
    let t = MockTransport::with_responder(move |p| {
        l.lock().push(p.to_vec());
        device(p)
    });
    (t, log)
}

fn command(position: &[f64]) -> JointCommand {
    JointCommand {
        position: position.to_vec(),
        ..Default::default()
    }
}

fn take(log: &Log) -> Vec<Vec<u8>> {
    std::mem::take(&mut *log.lock())
}

#[test]
fn so100_holds_the_position_latched_at_the_stop_with_torque_on() {
    use feetech::{rad_to_ticks, sts, sync_read, sync_write};
    let servos = Arc::new(Mutex::new(feetech::VirtualServos::new(&[1, 2])));
    let s = servos.clone();
    let (transport, log) = logged(move |p| s.lock().handle(p));
    let params = json!({ "ids": "1,2", "offsets": "2048,2048", "directions": "1,1" });
    let mut h =
        Harness::with_node::<So100Arm>(params, |arm| arm.attach(Box::new(transport))).unwrap();
    let set_present = |id: u8, ticks: u16| {
        let mut s = servos.lock();
        let t = &mut s.servos.iter_mut().find(|(i, _)| *i == id).unwrap().1;
        t[sts::PRESENT_POSITION as usize..][..2].copy_from_slice(&ticks.to_le_bytes());
    };
    let goals = |a: u16, b: u16| {
        sync_write(
            sts::GOAL_POSITION,
            2,
            &[(1, &a.to_le_bytes()), (2, &b.to_le_bytes())],
        )
    };
    let state_read = sync_read(sts::PRESENT_POSITION, 6, &[1, 2]);

    h.push("command", command(&[0.1, -0.1])).unwrap();
    h.step().unwrap();
    let sent = take(&log);
    let (a, b) = (rad_to_ticks(0.1, 2048, 1.0), rad_to_ticks(-0.1, 2048, 1.0));
    assert_eq!(sent.last().unwrap(), &goals(a, b), "follows commands");

    // Stopped mid-motion: the arm is short of its goal.
    set_present(1, 2090);
    set_present(2, 2000);
    h.set_estop(true);
    assert_eq!(
        take(&log),
        [
            sync_read(sts::PRESENT_POSITION, 2, &[1, 2]),
            goals(2090, 2000)
        ],
        "goal = present, written once on the stop"
    );

    // Sagging joint and new commands while stopped: the latched goal stays.
    set_present(1, 2070);
    h.push("command", command(&[0.3, 0.3])).unwrap();
    h.step().unwrap();
    assert_eq!(take(&log), [state_read.clone(), goals(2090, 2000)]);
    let s: Vec<JointState> = h.take("state").unwrap();
    assert_eq!(s.len(), 2, "state is still published while stopped");

    // A command queued during the stop is dropped on the release.
    h.push("command", command(&[0.2, 0.2])).unwrap();
    h.set_estop(false);
    h.step().unwrap();
    assert_eq!(
        take(&log),
        std::slice::from_ref(&state_read),
        "nothing is commanded"
    );
    h.step().unwrap();
    assert_eq!(take(&log), std::slice::from_ref(&state_read));

    h.push("command", command(&[0.05, 0.0])).unwrap();
    h.step().unwrap();
    assert_eq!(
        take(&log),
        [state_read, goals(rad_to_ticks(0.05, 2048, 1.0), 2048)],
        "fresh commands are followed"
    );
    // The exact packet sequences above also show that torque was never
    // disabled during the stop.
}

#[test]
fn so100_stop_engaged_before_start_holds_from_the_first_step() {
    let servos = Arc::new(Mutex::new(feetech::VirtualServos::new(&[1])));
    let s = servos.clone();
    let (transport, log) = logged(move |p| s.lock().handle(p));
    let params = json!({ "ids": "1", "offsets": "2048", "directions": "1" });
    let mut h =
        Harness::with_node::<So100Arm>(params, |arm| arm.attach(Box::new(transport))).unwrap();
    h.set_estop(true);
    h.push("command", command(&[0.4])).unwrap();
    h.step().unwrap();
    let hold = feetech::sync_write(
        feetech::sts::GOAL_POSITION,
        2,
        &[(1, &2048u16.to_le_bytes())],
    );
    let sent = take(&log);
    assert!(sent.contains(&hold));
    let goal_writes = sent
        .iter()
        .filter(|p| p[4] == feetech::inst::SYNC_WRITE && p[5] == feetech::sts::GOAL_POSITION);
    assert!(goal_writes.into_iter().all(|p| *p == hold), "{sent:?}");
}

#[test]
fn dynamixel_holds_the_position_latched_at_the_stop() {
    use dynamixel::{rad_to_ticks, sync_read, sync_write, x_series};
    let servos = Arc::new(Mutex::new(dynamixel::VirtualServos::new(&[1, 2])));
    let s = servos.clone();
    let (transport, log) = logged(move |p| s.lock().handle(p));
    let params = json!({ "ids": "1,2" });
    let mut h = Harness::with_node::<Dynamixel>(params, |d| d.attach(Box::new(transport))).unwrap();
    let set_present = |id: u8, ticks: i32| {
        let mut s = servos.lock();
        let t = &mut s.servos.iter_mut().find(|(i, _)| *i == id).unwrap().1;
        t[x_series::PRESENT_POSITION as usize..][..4].copy_from_slice(&ticks.to_le_bytes());
    };
    let goals = |a: i32, b: i32| {
        sync_write(
            x_series::GOAL_POSITION,
            4,
            &[(1, &a.to_le_bytes()), (2, &b.to_le_bytes())],
        )
    };
    let state_read = sync_read(x_series::PRESENT_CURRENT, 10, &[1, 2]);

    h.push("command", command(&[1.0, -0.5])).unwrap();
    h.step().unwrap();
    let sent = take(&log);
    let n = sent.len();
    assert_eq!(
        sent[n - 2..],
        [
            goals(rad_to_ticks(1.0), rad_to_ticks(-0.5)),
            state_read.clone()
        ]
    );

    set_present(1, 2500);
    set_present(2, 1900);
    h.set_estop(true);
    assert_eq!(
        take(&log),
        [
            sync_read(x_series::PRESENT_POSITION, 4, &[1, 2]),
            goals(2500, 1900)
        ]
    );

    set_present(1, 2450);
    h.push("command", command(&[0.0, 0.0])).unwrap();
    h.step().unwrap();
    assert_eq!(take(&log), [goals(2500, 1900), state_read.clone()]);

    h.push("command", command(&[0.1, 0.1])).unwrap();
    h.set_estop(false);
    h.step().unwrap();
    assert_eq!(
        take(&log),
        std::slice::from_ref(&state_read),
        "stale command dropped"
    );

    h.push("command", command(&[0.2, 0.0])).unwrap();
    h.step().unwrap();
    assert_eq!(
        take(&log),
        [goals(rad_to_ticks(0.2), 2048), state_read],
        "fresh commands are followed"
    );
}

/// Lines written to a virtual ODrive.
fn odrive(mode: &str) -> (Harness, Log, Arc<Mutex<odrive::VirtualODrive>>) {
    let drive = Arc::new(Mutex::new(odrive::VirtualODrive::default()));
    let d = drive.clone();
    let (transport, log) = logged(move |p| d.lock().handle(p));
    let params = json!({ "axes": "0,1", "mode": mode });
    let h = Harness::with_node::<ODrive>(params, |o| o.attach(Box::new(transport))).unwrap();
    (h, log, drive)
}

fn lines(log: &Log) -> Vec<String> {
    take(log)
        .into_iter()
        .map(|l| String::from_utf8(l).unwrap().trim_end().to_owned())
        .collect()
}

const FEEDBACK: [&str; 2] = ["f 0", "f 1"];

#[test]
fn odrive_velocity_mode_zeroes_the_setpoint() {
    let (mut h, log, drive) = odrive("velocity");
    let cmd = JointCommand {
        velocity: vec![std::f64::consts::TAU, -std::f64::consts::TAU],
        ..Default::default()
    };
    h.push("command", cmd.clone()).unwrap();
    h.step().unwrap();
    assert_eq!(drive.lock().axes[0].1, 1.0);
    lines(&log);

    let zero = ["v 0 0.000000 0.000000", "v 1 0.000000 0.000000"];
    h.set_estop(true);
    assert_eq!(lines(&log), zero, "zero velocity on the stop");
    assert_eq!(drive.lock().axes[0].1, 0.0);
    assert!(drive.lock().axes[0].2, "axes stay in closed loop control");

    h.push("command", cmd.clone()).unwrap();
    h.step().unwrap();
    assert_eq!(lines(&log), [&zero[..], &FEEDBACK].concat(), "kept at zero");

    h.push("command", cmd).unwrap();
    h.set_estop(false);
    h.step().unwrap();
    assert_eq!(lines(&log), FEEDBACK, "stale command dropped");
    assert_eq!(drive.lock().axes[0].1, 0.0);

    h.push(
        "command",
        JointCommand {
            velocity: vec![std::f64::consts::PI, 0.0],
            ..Default::default()
        },
    )
    .unwrap();
    h.step().unwrap();
    assert_eq!(drive.lock().axes[0].1, 0.5, "fresh commands are followed");
}

#[test]
fn odrive_position_mode_holds_the_present_position() {
    let (mut h, log, drive) = odrive("position");
    h.push("command", command(&[std::f64::consts::PI, 0.0]))
        .unwrap();
    h.step().unwrap();
    lines(&log);
    // Short of the goal when the stop engages.
    drive.lock().axes[0].0 = 0.25;
    drive.lock().axes[1].0 = -0.125;
    h.set_estop(true);
    let hold = [
        "p 0 0.250000 0.000000 0.000000",
        "p 1 -0.125000 0.000000 0.000000",
    ];
    assert_eq!(lines(&log), [&FEEDBACK[..], &hold].concat());

    drive.lock().axes[0].0 = 0.2;
    h.push("command", command(&[1.0, 1.0])).unwrap();
    h.step().unwrap();
    assert_eq!(lines(&log), [&hold[..], &FEEDBACK].concat(), "latched hold");

    h.push("command", command(&[1.0, 1.0])).unwrap();
    h.set_estop(false);
    h.step().unwrap();
    assert_eq!(lines(&log), FEEDBACK, "stale command dropped");
    h.push("command", command(&[0.0, 0.0])).unwrap();
    h.step().unwrap();
    assert_eq!(lines(&log)[0], "p 0 0.000000 0.000000 0.000000");
}

#[test]
fn odrive_torque_mode_zeroes_the_torque() {
    let (mut h, log, _) = odrive("torque");
    h.step().unwrap();
    lines(&log);
    h.set_estop(true);
    assert_eq!(lines(&log), ["c 0 0.000000", "c 1 0.000000"]);
}

fn joy(x: f32) -> Joy {
    Joy {
        axes: vec![x, 0.0],
        ..Default::default()
    }
}

fn jog_commands(h: &mut Harness) -> Vec<Vec<f64>> {
    let c: Vec<JointCommand> = h.take("command").unwrap();
    c.into_iter().map(|c| c.position).collect()
}

#[test]
fn joint_jog_holds_the_measured_position_while_stopped() {
    ruro_nodes_hw::link();
    let mut h = Harness::new("hw/joint-jog", json!({ "axes": "0,1", "speed": 1.0 })).unwrap();
    let state = |p: [f64; 2]| JointState {
        position: p.to_vec(),
        ..Default::default()
    };
    h.push("state", state([0.0, 0.0])).unwrap();
    h.push("joy", joy(1.0)).unwrap();
    h.step().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    h.step().unwrap();
    let c = jog_commands(&mut h);
    assert!(c.last().unwrap()[0] > 0.0, "jogging: {c:?}");

    h.set_estop(true);
    h.push("joy", joy(1.0)).unwrap();
    h.step().unwrap();
    assert!(
        jog_commands(&mut h).is_empty(),
        "the pre-stop target is not sent; nothing until a joint state arrives"
    );
    for _ in 0..3 {
        h.push("joy", joy(1.0)).unwrap();
        h.push("state", state([0.3, 0.0])).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        h.step().unwrap();
    }
    assert_eq!(
        jog_commands(&mut h),
        [[0.3, 0.0]; 3],
        "holds the measured position while stopped"
    );

    // The stick was still deflected when the stop was released.
    h.push("joy", joy(1.0)).unwrap();
    h.push("state", state([0.3, 0.0])).unwrap();
    h.set_estop(false);
    h.step().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    h.step().unwrap();
    assert_eq!(
        jog_commands(&mut h),
        [vec![0.3, 0.0], vec![0.3, 0.0]],
        "holds where the arm is until new stick input"
    );
    h.push("joy", joy(1.0)).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    h.step().unwrap();
    assert!(
        jog_commands(&mut h)[0][0] > 0.3,
        "fresh stick input moves it"
    );
}
