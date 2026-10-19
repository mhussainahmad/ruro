//! Action nodes: [`GoalDriver`], which drives goals of any action type from
//! a node's step, and `ros2/fibonacci-goal`, built on it for
//! `example_interfaces/action/Fibonacci`.
//!
//! Actions often command motion (navigation goals, trajectories), so the
//! driver honours the e-stop: when it engages, the active goal is canceled
//! (by id, so other clients' goals are untouched) and a goal still awaiting
//! acceptance is canceled as soon as it is accepted; while it is engaged,
//! no new goal is sent.

use std::sync::Arc;
use std::time::Duration;

use ruro::prelude::*;
use ruro_ros2::RosAction;
use ruro_ros2::action::{ActionClient, ClientGoal, GoalStatus, PendingGoal};
use ruro_ros2::msgs::example_interfaces::action::{Fibonacci, Fibonacci_Goal};
use ruro_ros2::rmw::Endpoint;

use super::Settings;

/// What happened to the driven goal since the last poll.
#[derive(Debug)]
pub enum GoalEvent<A: RosAction> {
    /// The server accepted the goal.
    Accepted,
    /// The server rejected the goal, or did not answer.
    Rejected(String),
    /// The goal's status changed.
    Status(GoalStatus),
    Feedback(A::Feedback),
    /// The goal ended with this status and result.
    Finished(GoalStatus, A::Result),
    /// The result could not be obtained.
    Failed(String),
}

/// Drives one goal at a time on an action server, without blocking: call
/// [`send`](Self::send) and [`cancel`](Self::cancel) from a node's step and
/// [`poll`](Self::poll) on every step. Sending a goal while one is active
/// cancels the active one.
pub struct GoalDriver<A: RosAction> {
    client: ActionClient<A>,
    pending: Option<PendingGoal<A>>,
    /// Goals replaced while awaiting acceptance, canceled once accepted.
    superseded: Vec<PendingGoal<A>>,
    active: Option<ClientGoal<A>>,
    status: GoalStatus,
    /// Cancel the pending goal once it is accepted.
    cancel_pending: bool,
    timeout: Duration,
}

impl<A: RosAction> GoalDriver<A> {
    /// A driver for the action `name`; `timeout` bounds service calls
    /// (sending a goal, canceling it), not the goal itself.
    pub fn new(endpoint: &Endpoint, name: &str, timeout: Duration) -> Result<Self> {
        Ok(Self {
            client: ActionClient::new(endpoint, name)?,
            pending: None,
            superseded: Vec::new(),
            active: None,
            status: GoalStatus::Unknown,
            cancel_pending: false,
            timeout,
        })
    }

    /// The action client.
    pub fn client(&self) -> &ActionClient<A> {
        &self.client
    }

    /// Whether a goal is awaiting acceptance or running.
    pub fn busy(&self) -> bool {
        self.pending.is_some() || self.active.is_some()
    }

    /// Sends a goal, canceling the current one (a goal still awaiting
    /// acceptance is canceled once accepted).
    pub fn send(&mut self, goal: A::Goal) -> Result {
        if let Some(active) = self.active.take()
            && !active.status().is_terminal()
            && let Err(e) = active.cancel_async(self.timeout)
        {
            warn!("canceling a goal of {}: {e:#}", self.client.name());
        }
        if let Some(pending) = self.pending.take() {
            self.superseded.push(pending);
        }
        self.cancel_pending = false;
        self.status = GoalStatus::Unknown;
        self.pending = Some(self.client.send_goal_async(goal, self.timeout)?);
        Ok(())
    }

    /// Asks the server to cancel the current goal (or the pending one, once
    /// accepted). Its result still arrives through [`poll`](Self::poll).
    pub fn cancel(&mut self) {
        if let Some(goal) = &self.active
            && !goal.status().is_terminal()
            && let Err(e) = goal.cancel_async(self.timeout)
        {
            warn!("canceling a goal of {}: {e:#}", self.client.name());
        }
        if self.pending.is_some() {
            self.cancel_pending = true;
        }
    }

    /// Events since the last poll.
    pub fn poll(&mut self) -> Vec<GoalEvent<A>> {
        let timeout = self.timeout;
        self.superseded.retain_mut(|p| match p.try_accepted() {
            None => true,
            Some(Ok(goal)) => {
                let _ = goal.cancel_async(timeout);
                false
            }
            Some(Err(_)) => false,
        });
        let mut events = Vec::new();
        if let Some(pending) = &mut self.pending
            && let Some(answer) = pending.try_accepted()
        {
            self.pending = None;
            match answer {
                Ok(goal) => {
                    events.push(GoalEvent::Accepted);
                    self.active = Some(goal);
                    if std::mem::take(&mut self.cancel_pending) {
                        self.cancel();
                    }
                }
                Err(e) => events.push(GoalEvent::Rejected(format!("{e:#}"))),
            }
        }
        if let Some(goal) = &self.active {
            for f in goal.feedback() {
                events.push(GoalEvent::Feedback(f));
            }
            let status = goal.status();
            if status != self.status && status != GoalStatus::Unknown {
                self.status = status;
                events.push(GoalEvent::Status(status));
            }
            if let Some(r) = goal.try_result() {
                match r {
                    Ok((status, result)) => events.push(GoalEvent::Finished(status, result)),
                    Err(e) => events.push(GoalEvent::Failed(format!("{e:#}"))),
                }
                self.active = None;
            }
        }
        events
    }
}

/// An action driver and the settings it was made with.
pub struct DriverState<A: RosAction> {
    settings: Settings,
    _endpoint: Arc<Endpoint>,
    driver: GoalDriver<A>,
}

fn sequence_text(s: &[i32]) -> String {
    s.iter().map(i32::to_string).collect::<Vec<_>>().join(" ")
}

/// Sends `example_interfaces/action/Fibonacci` goals: every `order` input
/// sends a goal (canceling the one before), feedback and the result come
/// out as space-separated sequences, `succeeded` tells how the goal ended
/// and `status` follows its status (`accepted`, `executing`, `succeeded`,
/// `canceled`, `aborted`, `rejected`, `failed`).
///
/// E-stop: the active goal is canceled when the stop engages, and input is
/// dropped while it is engaged.
#[derive(Node)]
#[node(
    kind = "ros2/fibonacci-goal",
    rate = 20,
    doc = "Sends ROS 2 example_interfaces/action/Fibonacci goals (one per order input, canceling the previous one) and outputs feedback, result and status. The active goal is canceled when the e-stop engages; input is dropped while it is engaged."
)]
pub struct FibonacciGoal {
    #[input]
    order: Input<Scalar>,
    #[output]
    feedback: Output<Text>,
    #[output]
    result: Output<Text>,
    #[output]
    succeeded: Output<Flag>,
    #[output]
    status: Output<Text>,
    /// ROS 2 action name.
    #[param(default = "/fibonacci")]
    action: String,
    /// How long to wait for the server to answer a goal or cancel request,
    /// in milliseconds.
    #[param(default = 5000, min = 1)]
    timeout_ms: i64,
    /// ROS_DOMAIN_ID; -1 uses the environment variable (default 0).
    #[param(default = -1, min = -1, max = 232)]
    domain_id: i64,
    /// Zenoh router shared with the ROS 2 nodes.
    #[param(default = "tcp/127.0.0.1:7447")]
    router: String,
    /// Token of a router that requires one (a ruro daemon started with a
    /// token). Empty uses RURO_TOKEN, which daemons give their hosts. TLS
    /// settings come from RURO_TLS_CA, RURO_TLS_CERT and RURO_TLS_KEY.
    #[param(default = "")]
    token: String,
    state: Option<DriverState<Fibonacci>>,
}

impl FibonacciGoal {
    fn ensure(&mut self, ctx: &Ctx) -> Result {
        if self.action.is_empty() {
            bail!("parameter `action` is empty");
        }
        let want = Settings::new(
            ctx.id(),
            &self.action,
            self.domain_id,
            "reliable",
            &self.router,
            &self.token,
        )?;
        if self.state.as_ref().is_none_or(|s| s.settings != want) {
            self.state = None;
            let endpoint = want.endpoint()?;
            let timeout = Duration::from_millis(self.timeout_ms.max(1) as u64);
            let driver = GoalDriver::new(&endpoint, &want.topic, timeout)?;
            info!(
                "sending ROS 2 {} goals to {} in domain {}",
                Fibonacci::TYPE_NAME,
                want.topic,
                want.domain
            );
            self.state = Some(DriverState {
                settings: want,
                _endpoint: endpoint,
                driver,
            });
        }
        Ok(())
    }
}

impl Node for FibonacciGoal {
    fn start(&mut self, ctx: &mut Ctx) -> Result {
        self.ensure(ctx)
    }

    fn step(&mut self, ctx: &mut Ctx) -> Result {
        self.ensure(ctx)?;
        let driver = &mut self.state.as_mut().expect("connected").driver;
        while let Some(s) = self.order.recv() {
            if ctx.estopped() {
                warn!("e-stop engaged: not sending a goal to {}", self.action);
                continue;
            }
            let order = s.msg.0;
            if !(0.0..=i32::MAX as f64).contains(&order) {
                warn!("{}: order {order} out of range", self.action);
                continue;
            }
            driver.send(Fibonacci_Goal {
                order: order as i32,
            })?;
        }
        for event in driver.poll() {
            match event {
                GoalEvent::Accepted => self.status.send(Text("accepted".into()))?,
                GoalEvent::Rejected(e) => {
                    warn!("{e}");
                    self.status.send(Text("rejected".into()))?;
                    self.succeeded.send(Flag(false))?;
                }
                GoalEvent::Status(s) => {
                    if !s.is_terminal() && s != GoalStatus::Accepted {
                        self.status.send(Text(s.to_string()))?;
                    }
                }
                GoalEvent::Feedback(f) => self.feedback.send(Text(sequence_text(&f.sequence)))?,
                GoalEvent::Finished(s, r) => {
                    self.result.send(Text(sequence_text(&r.sequence)))?;
                    self.status.send(Text(s.to_string()))?;
                    self.succeeded.send(Flag(s == GoalStatus::Succeeded))?;
                }
                GoalEvent::Failed(e) => {
                    warn!("{}: {e}", self.action);
                    self.status.send(Text("failed".into()))?;
                    self.succeeded.send(Flag(false))?;
                }
            }
        }
        Ok(())
    }

    fn on_estop(&mut self, _ctx: &mut Ctx) {
        if let Some(s) = &mut self.state {
            s.driver.cancel();
        }
    }

    fn stop(&mut self, _ctx: &mut Ctx) {
        if let Some(s) = &mut self.state {
            s.driver.cancel();
        }
        self.state = None;
    }
}
