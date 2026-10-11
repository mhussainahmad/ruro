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
}

impl Node for FakeArm {
    fn step(&mut self, ctx: &mut Ctx) -> Result {
        if let Some(c) = self.velocity.latest() {
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
}
