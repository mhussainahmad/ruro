use ruro::prelude::*;

/// PID controller. Runs when a new measurement arrives.
#[derive(Node)]
#[node(kind = "std/pid")]
pub struct Pid {
    #[input]
    setpoint: Input<Scalar>,
    #[input]
    measured: Input<Scalar>,
    #[output]
    command: Output<Scalar>,
    #[param(default = 2.0, min = 0, max = 100)]
    kp: f64,
    #[param(default = 0.0, min = 0, max = 100)]
    ki: f64,
    #[param(default = 0.1, min = 0, max = 100)]
    kd: f64,
    /// Output saturation (absolute value).
    #[param(default = 10.0, min = 0, max = 1000)]
    limit: f64,
    target: f64,
    integral: f64,
    prev_error: Option<f64>,
}

impl Node for Pid {
    fn step(&mut self, ctx: &mut Ctx) -> Result {
        if let Some(sp) = self.setpoint.latest() {
            self.target = sp.0;
        }
        let Some(m) = self.measured.latest() else {
            return Ok(());
        };
        let dt = ctx.dt().as_secs_f64().max(1e-6);
        let error = self.target - m.0;
        let derivative = self.prev_error.map_or(0.0, |p| (error - p) / dt);
        self.prev_error = Some(error);

        let unclamped =
            self.kp * error + self.ki * (self.integral + error * dt) + self.kd * derivative;
        let out = unclamped.clamp(-self.limit, self.limit);
        // Anti-windup: only integrate while the output is not saturated.
        if out == unclamped {
            self.integral += error * dt;
        }
        self.command.send(Scalar(out))
    }
}

/// Simulated single joint driven by a velocity command, with inertia and friction.
///
/// E-stop: the commanded velocity drops to zero and the joint slows down with
/// its usual lag; velocity commands are ignored while stopped. After the
/// release the joint stays at rest until a new command arrives (commands sent
/// during the stop are discarded).
#[derive(Node)]
#[node(kind = "std/fake-arm", rate = 200)]
pub struct FakeArm {
    #[input]
    velocity: Input<Scalar>,
    #[output]
    position: Output<Scalar>,
    #[output]
    state: Output<JointState>,
    /// Time constant of the velocity response in seconds.
    #[param(default = 0.15, min = 0.001, max = 10)]
    lag: f64,
    /// Maximum joint speed in rad/s.
    #[param(default = 3.0, min = 0, max = 100)]
    max_speed: f64,
    command: f64,
    pos: f64,
    vel: f64,
    /// Stopped by the e-stop; commands are discarded until one arrives after the release.
    held: bool,
}

impl Node for FakeArm {
    fn step(&mut self, ctx: &mut Ctx) -> Result {
        if ctx.estopped() || self.held {
            // Commands queued while stopped are stale.
            while self.velocity.recv().is_some() {}
            self.command = 0.0;
            self.held = ctx.estopped();
        } else if let Some(c) = self.velocity.latest() {
            self.command = c.0.clamp(-self.max_speed, self.max_speed);
        }
        let dt = ctx.dt().as_secs_f64();
        let alpha = (dt / self.lag).min(1.0);
        self.vel += (self.command - self.vel) * alpha;
        self.pos += self.vel * dt;
        self.position.send(Scalar(self.pos))?;
        self.state.send(JointState {
            position: vec![self.pos],
            velocity: vec![self.vel],
            effort: vec![self.command - self.vel],
        })
    }

    fn on_estop(&mut self, _ctx: &mut Ctx) {
        self.command = 0.0;
        self.held = true;
    }
}

/// Reactive obstacle avoidance for a differential-drive base. Drives forward
/// while nothing is within `stop_distance` in the sector ahead, slowing down
/// as obstacles get nearer, and otherwise turns on the spot towards the side
/// with more room. Runs when a scan arrives; angles follow ROS (x forward,
/// counter-clockwise positive, so positive angles are on the left).
///
/// E-stop: sends a zero twist for every scan while stopped.
#[derive(Node)]
#[node(kind = "std/scan-avoid")]
pub struct ScanAvoid {
    #[input]
    scan: Input<LaserScan>,
    /// Velocity command: `linear.x` in m/s, `angular.z` in rad/s.
    #[output]
    cmd: Output<Twist>,
    /// Forward speed in m/s when the way ahead is clear.
    #[param(default = 0.3, min = 0, max = 10)]
    speed: f64,
    /// Turn rate in rad/s while avoiding.
    #[param(default = 0.8, min = 0, max = 10)]
    turn: f64,
    /// Obstacles nearer than this, in metres, stop forward motion.
    #[param(default = 0.7, min = 0.05, max = 50)]
    stop_distance: f64,
    /// Half-width of the sector ahead, in degrees.
    #[param(default = 30.0, min = 1, max = 180)]
    sector_deg: f64,
    /// Keeps turning the same way until the way is clear again.
    turning: Option<f64>,
}

/// Nearest valid return in each region: ahead (within `sector` radians of
/// straight ahead), to the left and to the right. Infinite where none.
/// `-inf`, which REP 117 gives for an object too close to measure, is at 0.
fn clearance(scan: &LaserScan, sector: f64) -> (f64, f64, f64) {
    let (mut ahead, mut left, mut right) = (f64::INFINITY, f64::INFINITY, f64::INFINITY);
    for (i, &r) in scan.ranges.iter().enumerate() {
        let r = if r == f32::NEG_INFINITY {
            0.0
        } else if r.is_finite() && r >= scan.range_min && r <= scan.range_max {
            r
        } else {
            continue;
        };
        let (r, a) = (
            r as f64,
            (scan.angle_min + i as f32 * scan.angle_increment) as f64,
        );
        // Wrap to [-pi, pi) so scans given as 0..2pi work too.
        let a = (a + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU) - std::f64::consts::PI;
        if a.abs() <= sector {
            ahead = ahead.min(r);
        }
        if a > 0.0 && a <= std::f64::consts::FRAC_PI_2 {
            left = left.min(r);
        } else if (-std::f64::consts::FRAC_PI_2..0.0).contains(&a) {
            right = right.min(r);
        }
    }
    (ahead, left, right)
}

impl Node for ScanAvoid {
    fn step(&mut self, ctx: &mut Ctx) -> Result {
        let Some(scan) = self.scan.latest() else {
            return Ok(());
        };
        if ctx.estopped() {
            self.turning = None;
            return self.cmd.send(Twist::default());
        }
        let (ahead, left, right) = clearance(&scan, self.sector_deg.to_radians());
        let mut cmd = Twist::default();
        if ahead > self.stop_distance {
            self.turning = None;
            // Full speed from twice the stop distance, slower before it.
            let room = ((ahead - self.stop_distance) / self.stop_distance).min(1.0);
            cmd.linear.x = self.speed * room.max(0.2);
        } else {
            let dir = *self
                .turning
                .get_or_insert(if left >= right { 1.0 } else { -1.0 });
            cmd.angular.z = dir * self.turn;
        }
        self.cmd.send(cmd)
    }
}
