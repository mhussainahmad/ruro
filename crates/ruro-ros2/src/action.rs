//! ROS 2 actions, built from services and topics as `rcl_action` builds
//! them (there is no action concept in rmw_zenoh itself).
//!
//! An action `/fibonacci` of type `pkg/action/Fibonacci` consists of:
//!
//! - `/fibonacci/_action/send_goal`: service `pkg/action/Fibonacci_SendGoal`
//!   (`{goal_id, goal}` → `{accepted, stamp}`);
//! - `/fibonacci/_action/cancel_goal`: service `action_msgs/srv/CancelGoal`;
//! - `/fibonacci/_action/get_result`: service `pkg/action/Fibonacci_GetResult`
//!   (`{goal_id}` → `{status, result}`), answered once the goal ends;
//! - `/fibonacci/_action/feedback`: topic `pkg/action/Fibonacci_FeedbackMessage`
//!   (`{goal_id, feedback}`);
//! - `/fibonacci/_action/status`: topic `action_msgs/msg/GoalStatusArray`,
//!   reliable and transient-local with depth 1.
//!
//! Goals are named by a random UUID chosen by the client. The services use
//! the services QoS profile and feedback the default profile, as in
//! `rcl_action`. Servers keep a finished goal's result for
//! [`RESULT_TIMEOUT`] so a late `get_result` still gets it.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};

use crate::msgs::action_msgs::msg::{GoalInfo, GoalStatus as GoalStatusMsg, GoalStatusArray};
use crate::msgs::action_msgs::srv::{CancelGoal, CancelGoal_Request, CancelGoal_Response};
use crate::msgs::builtin_interfaces::msg::Time;
use crate::msgs::unique_identifier_msgs::msg::UUID;
use crate::rmw::{
    Client, Endpoint, FOREVER, PendingRequest, PendingResponse, Publisher, Qos, RawService,
    Subscriber,
};
use crate::time::{time_from_ns, time_to_ns};
use crate::{GoalId, RosAction, RosService};

/// How long a server keeps a finished goal's result (`rcl_action`'s
/// default `result_timeout`).
pub const RESULT_TIMEOUT: Duration = Duration::from_secs(10);

/// The state of a goal (`action_msgs/msg/GoalStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GoalStatus {
    Unknown,
    Accepted,
    Executing,
    Canceling,
    Succeeded,
    Canceled,
    Aborted,
}

impl GoalStatus {
    /// From the `STATUS_*` value.
    pub fn from_i8(v: i8) -> Self {
        match v {
            GoalStatusMsg::STATUS_ACCEPTED => Self::Accepted,
            GoalStatusMsg::STATUS_EXECUTING => Self::Executing,
            GoalStatusMsg::STATUS_CANCELING => Self::Canceling,
            GoalStatusMsg::STATUS_SUCCEEDED => Self::Succeeded,
            GoalStatusMsg::STATUS_CANCELED => Self::Canceled,
            GoalStatusMsg::STATUS_ABORTED => Self::Aborted,
            _ => Self::Unknown,
        }
    }

    /// The `STATUS_*` value.
    pub fn as_i8(self) -> i8 {
        match self {
            Self::Unknown => GoalStatusMsg::STATUS_UNKNOWN,
            Self::Accepted => GoalStatusMsg::STATUS_ACCEPTED,
            Self::Executing => GoalStatusMsg::STATUS_EXECUTING,
            Self::Canceling => GoalStatusMsg::STATUS_CANCELING,
            Self::Succeeded => GoalStatusMsg::STATUS_SUCCEEDED,
            Self::Canceled => GoalStatusMsg::STATUS_CANCELED,
            Self::Aborted => GoalStatusMsg::STATUS_ABORTED,
        }
    }

    /// Whether the goal has ended.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Canceled | Self::Aborted)
    }
}

impl std::fmt::Display for GoalStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Unknown => "unknown",
            Self::Accepted => "accepted",
            Self::Executing => "executing",
            Self::Canceling => "canceling",
            Self::Succeeded => "succeeded",
            Self::Canceled => "canceled",
            Self::Aborted => "aborted",
        };
        f.write_str(s)
    }
}

/// A new random goal id (a version 4 UUID).
pub fn new_goal_id() -> GoalId {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut seed = Vec::with_capacity(48);
    seed.extend_from_slice(&COUNTER.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    seed.extend_from_slice(&now_ns().to_le_bytes());
    seed.extend_from_slice(&std::process::id().to_le_bytes());
    let local = 0u8;
    seed.extend_from_slice(&(&local as *const u8 as usize).to_le_bytes());
    seed.extend_from_slice(format!("{:?}", std::thread::current().id()).as_bytes());
    let mut id = xxhash_rust::xxh3::xxh3_128(&seed).to_le_bytes();
    id[6] = (id[6] & 0x0f) | 0x40;
    id[8] = (id[8] & 0x3f) | 0x80;
    id
}

/// A goal id in the usual UUID text form.
pub fn goal_id_string(id: &GoalId) -> String {
    let h: String = id.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// The names of an action's services and topics:
/// `[send_goal, cancel_goal, get_result, feedback, status]`.
pub fn action_names(action: &str) -> [String; 5] {
    let a = action.trim_end_matches('/');
    [
        "send_goal",
        "cancel_goal",
        "get_result",
        "feedback",
        "status",
    ]
    .map(|s| format!("{a}/_action/{s}"))
}

/// The QoS of the status topic (`rcl_action_qos_profile_status_default`).
pub fn status_qos() -> Qos {
    Qos::transient_local(1)
}

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

type GetResultRequest<A> = PendingRequest<<A as RosAction>::GetResult>;

struct ServerGoal<A: RosAction> {
    status: GoalStatus,
    stamp: Time,
    accepted_ns: u64,
    result: Option<A::Result>,
    /// `get_result` requests waiting for the goal to end.
    waiting: Vec<GetResultRequest<A>>,
    ended: Option<Instant>,
}

struct ServerState<A: RosAction> {
    goals: HashMap<GoalId, ServerGoal<A>>,
    /// Accepted goals not yet taken with `next_goal`, in order.
    new: VecDeque<(GoalId, A::Goal)>,
}

type AcceptFn<A> = dyn Fn(&GoalId, &<A as RosAction>::Goal) -> bool + Send + Sync;

struct ServerShared<A: RosAction> {
    state: Mutex<ServerState<A>>,
    feedback: Publisher<A::FeedbackMessage>,
    status: Publisher<GoalStatusArray>,
    /// Held while taking and publishing a status snapshot, so that one
    /// taken earlier is never published after a later one.
    publishing: Mutex<()>,
    accept: Box<AcceptFn<A>>,
    result_timeout: Duration,
}

impl<A: RosAction> ServerShared<A> {
    fn status_array(state: &ServerState<A>) -> GoalStatusArray {
        let mut list: Vec<_> = state.goals.iter().collect();
        list.sort_by_key(|(_, g)| g.accepted_ns);
        GoalStatusArray {
            status_list: list
                .into_iter()
                .map(|(id, g)| GoalStatusMsg {
                    goal_info: GoalInfo {
                        goal_id: UUID { uuid: *id },
                        stamp: g.stamp.clone(),
                    },
                    status: g.status.as_i8(),
                })
                .collect(),
        }
    }

    /// Drops results kept longer than the result timeout; returns whether
    /// any went.
    fn expire(&self, state: &mut ServerState<A>) -> bool {
        let before = state.goals.len();
        let timeout = self.result_timeout;
        state
            .goals
            .retain(|_, g| g.ended.is_none_or(|t| t.elapsed() < timeout));
        state.goals.len() != before
    }

    /// Runs `f` on the state, then publishes the status if `f` says it
    /// changed. Publishing happens without the state lock held: a
    /// subscriber in this process may be called synchronously.
    fn update<R>(&self, f: impl FnOnce(&mut ServerState<A>) -> (R, bool)) -> R {
        let (r, changed) = {
            let mut state = self.state.lock().unwrap();
            let expired = self.expire(&mut state);
            let (r, changed) = f(&mut state);
            (r, changed || expired)
        };
        if changed {
            // The snapshot is taken after the publishing lock: whoever
            // publishes last sends the newest state.
            let _publishing = self.publishing.lock().unwrap();
            let s = Self::status_array(&self.state.lock().unwrap());
            let _ = self.status.publish(&s);
        }
        r
    }

    fn on_send_goal(&self, req: PendingRequest<A::SendGoal>) {
        let (id, goal) = A::split_send_goal_request(req.request.clone());
        let now = now_ns();
        let stamp = time_from_ns(now);
        let accepted = self.update(|state| {
            let ok = !state.goals.contains_key(&id) && (self.accept)(&id, &goal);
            if ok {
                state.goals.insert(
                    id,
                    ServerGoal {
                        status: GoalStatus::Accepted,
                        stamp: stamp.clone(),
                        accepted_ns: now,
                        result: None,
                        waiting: Vec::new(),
                        ended: None,
                    },
                );
                state.new.push_back((id, goal));
            }
            (ok, ok)
        });
        let _ = req.respond(&A::send_goal_response(accepted, stamp));
    }

    fn on_get_result(&self, req: GetResultRequest<A>) {
        let id = A::split_get_result_request(req.request.clone());
        let ready = self.update(|state| {
            let ready = match state.goals.get_mut(&id) {
                None => Some((GoalStatus::Unknown, A::Result::default(), req)),
                Some(g) if g.status.is_terminal() => {
                    let result = g.result.clone().unwrap_or_default();
                    Some((g.status, result, req))
                }
                Some(g) => {
                    g.waiting.push(req);
                    None
                }
            };
            (ready, false)
        });
        if let Some((status, result, req)) = ready {
            let _ = req.respond(&A::get_result_response(status.as_i8(), result));
        }
    }

    /// Cancel semantics of `action_msgs/srv/CancelGoal`: a zero id and
    /// zero stamp cancel every goal, a zero id with a stamp those accepted
    /// at or before it, an id that goal (and, with a stamp, those before).
    fn on_cancel(&self, req: CancelGoal_Request) -> CancelGoal_Response {
        let id = req.goal_info.goal_id.uuid;
        let before = time_to_ns(&req.goal_info.stamp);
        let zero_id = id == [0; 16];
        self.update(|state| {
            let mut canceling = Vec::new();
            for (gid, g) in state.goals.iter_mut() {
                let wanted = (zero_id && before == 0)
                    || (!zero_id && *gid == id)
                    || (before != 0 && g.accepted_ns <= before);
                if wanted && matches!(g.status, GoalStatus::Accepted | GoalStatus::Executing) {
                    g.status = GoalStatus::Canceling;
                    canceling.push(GoalInfo {
                        goal_id: UUID { uuid: *gid },
                        stamp: g.stamp.clone(),
                    });
                }
            }
            let return_code = if !canceling.is_empty() {
                CancelGoal_Response::ERROR_NONE
            } else if !zero_id {
                match state.goals.get(&id) {
                    None => CancelGoal_Response::ERROR_UNKNOWN_GOAL_ID,
                    Some(g) if g.status.is_terminal() => CancelGoal_Response::ERROR_GOAL_TERMINATED,
                    Some(_) => CancelGoal_Response::ERROR_REJECTED,
                }
            } else {
                // Cancel all, or all before a time: none to cancel is no
                // error, as in rcl_action.
                CancelGoal_Response::ERROR_NONE
            };
            let changed = !canceling.is_empty();
            (
                CancelGoal_Response {
                    return_code,
                    goals_canceling: canceling,
                },
                changed,
            )
        })
    }

    /// Marks an accepted goal executing. A goal already canceling stays
    /// canceling, as in rcl_action's state machine.
    fn set_executing(&self, id: &GoalId) {
        self.update(|state| {
            let changed = match state.goals.get_mut(id) {
                Some(g) if g.status == GoalStatus::Accepted => {
                    g.status = GoalStatus::Executing;
                    true
                }
                _ => false,
            };
            ((), changed)
        });
    }

    fn status_of(&self, id: &GoalId) -> GoalStatus {
        self.state
            .lock()
            .unwrap()
            .goals
            .get(id)
            .map_or(GoalStatus::Unknown, |g| g.status)
    }

    /// Ends a goal by a transition rcl_action allows: one only accepted is
    /// marked executing first, and canceled only follows a cancel request
    /// (otherwise the goal is aborted).
    fn finish(&self, id: &GoalId, status: GoalStatus, result: A::Result) {
        self.set_executing(id);
        let (waiting, status) = self.update(|state| match state.goals.get_mut(id) {
            Some(g) if !g.status.is_terminal() => {
                let status = match status {
                    GoalStatus::Canceled if g.status != GoalStatus::Canceling => {
                        GoalStatus::Aborted
                    }
                    s => s,
                };
                g.status = status;
                g.result = Some(result.clone());
                g.ended = Some(Instant::now());
                ((std::mem::take(&mut g.waiting), status), true)
            }
            _ => ((Vec::new(), status), false),
        });
        for req in waiting {
            let _ = req.respond(&A::get_result_response(status.as_i8(), result.clone()));
        }
    }
}

/// Serves an action. Accepted goals are taken with
/// [`next_goal`](Self::next_goal) and driven through their
/// [`GoalHandle`]; cancel requests for active goals are always accepted
/// and show as [`GoalHandle::is_cancel_requested`].
pub struct ActionServer<A: RosAction> {
    shared: Arc<ServerShared<A>>,
    name: String,
    _services: [RawService; 3],
}

impl<A: RosAction> ActionServer<A> {
    /// Serves `name`, accepting every goal.
    pub fn new(endpoint: &Endpoint, name: &str) -> Result<Self> {
        Self::with_acceptor(endpoint, name, |_, _| true)
    }

    /// Serves `name`, accepting the goals `accept` returns true for. It is
    /// called on a Zenoh thread: keep it short.
    pub fn with_acceptor(
        endpoint: &Endpoint,
        name: &str,
        accept: impl Fn(&GoalId, &A::Goal) -> bool + Send + Sync + 'static,
    ) -> Result<Self> {
        let name = endpoint.qualify(name);
        let [send_goal, cancel_goal, get_result, feedback, status] = action_names(&name);
        let shared = Arc::new(ServerShared::<A> {
            state: Mutex::new(ServerState {
                goals: HashMap::new(),
                new: VecDeque::new(),
            }),
            feedback: endpoint.publisher(&feedback, Qos::default())?,
            status: endpoint.publisher(&status, status_qos())?,
            publishing: Mutex::new(()),
            accept: Box::new(accept),
            result_timeout: RESULT_TIMEOUT,
        });
        let s = Arc::downgrade(&shared);
        let send =
            endpoint.service_deferred::<A::SendGoal>(&send_goal, Qos::services(), move |req| {
                if let Some(s) = s.upgrade() {
                    s.on_send_goal(req);
                }
            })?;
        let s = Arc::downgrade(&shared);
        let cancel =
            endpoint.service_deferred::<CancelGoal>(&cancel_goal, Qos::services(), move |req| {
                if let Some(s) = s.upgrade() {
                    let resp = s.on_cancel(req.request.clone());
                    let _ = req.respond(&resp);
                }
            })?;
        let s = Arc::downgrade(&shared);
        let result = endpoint.service_deferred::<A::GetResult>(
            &get_result,
            Qos::services(),
            move |req| {
                if let Some(s) = s.upgrade() {
                    s.on_get_result(req);
                }
            },
        )?;
        // An empty status array, so late clients see the server's state.
        let _ = shared.status.publish(&GoalStatusArray::default());
        Ok(Self {
            shared,
            name,
            _services: [send, cancel, result],
        })
    }

    /// The next accepted goal not yet taken.
    pub fn next_goal(&self) -> Option<GoalHandle<A>> {
        let (id, goal) = self.shared.state.lock().unwrap().new.pop_front()?;
        Some(GoalHandle {
            id,
            goal,
            shared: self.shared.clone(),
            done: false,
        })
    }

    /// Every goal the server knows, with its status.
    pub fn goals(&self) -> Vec<(GoalId, GoalStatus)> {
        let state = self.shared.state.lock().unwrap();
        let mut g: Vec<_> = state
            .goals
            .iter()
            .map(|(id, g)| (g.accepted_ns, *id, g.status))
            .collect();
        g.sort_by_key(|(t, id, _)| (*t, *id));
        g.into_iter().map(|(_, id, s)| (id, s)).collect()
    }

    /// Drops results kept longer than [`RESULT_TIMEOUT`] (also done on
    /// every request).
    pub fn expire_results(&self) {
        self.shared.update(|_| ((), false));
    }

    /// The fully qualified action name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// An accepted goal on the server side. Dropping it before it ends
/// aborts the goal with a default result, as rclcpp does.
pub struct GoalHandle<A: RosAction> {
    id: GoalId,
    goal: A::Goal,
    shared: Arc<ServerShared<A>>,
    done: bool,
}

impl<A: RosAction> GoalHandle<A> {
    /// The goal id.
    pub fn id(&self) -> GoalId {
        self.id
    }

    /// The goal.
    pub fn goal(&self) -> &A::Goal {
        &self.goal
    }

    /// Marks the goal executing (unless a cancel request already came).
    pub fn execute(&self) {
        self.shared.set_executing(&self.id);
    }

    /// The goal's status.
    pub fn status(&self) -> GoalStatus {
        self.shared.status_of(&self.id)
    }

    /// Whether a client asked to cancel the goal; finish it with
    /// [`canceled`](Self::canceled) (or another ending) soon.
    pub fn is_cancel_requested(&self) -> bool {
        self.status() == GoalStatus::Canceling
    }

    /// Publishes feedback.
    pub fn publish_feedback(&self, feedback: A::Feedback) -> Result<()> {
        self.shared
            .feedback
            .publish(&A::feedback_message(self.id, feedback))
    }

    fn end(mut self, status: GoalStatus, result: A::Result) {
        self.done = true;
        self.shared.finish(&self.id, status, result);
    }

    /// Ends the goal successfully.
    pub fn succeed(self, result: A::Result) {
        self.end(GoalStatus::Succeeded, result);
    }

    /// Ends the goal as aborted.
    pub fn abort(self, result: A::Result) {
        self.end(GoalStatus::Aborted, result);
    }

    /// Ends the goal as canceled.
    pub fn canceled(self, result: A::Result) {
        self.end(GoalStatus::Canceled, result);
    }
}

impl<A: RosAction> Drop for GoalHandle<A> {
    fn drop(&mut self) {
        if !self.done {
            self.shared
                .finish(&self.id, GoalStatus::Aborted, A::Result::default());
        }
    }
}

struct ClientShared<A: RosAction> {
    /// Feedback of the goals this client sent, oldest first.
    feedback: Mutex<HashMap<GoalId, VecDeque<A::Feedback>>>,
    /// Latest status of every goal on the status topic.
    status: Mutex<HashMap<GoalId, GoalStatus>>,
}

struct ClientInner<A: RosAction> {
    send_goal: Client<A::SendGoal>,
    cancel_goal: Client<CancelGoal>,
    get_result: Client<A::GetResult>,
    shared: Arc<ClientShared<A>>,
    name: String,
    _feedback: Subscriber,
    _status: Subscriber,
}

/// Feedback kept per goal before it is taken; older feedback is dropped.
const FEEDBACK_BACKLOG: usize = 100;

/// Sends goals to an action server and follows them.
pub struct ActionClient<A: RosAction> {
    inner: Arc<ClientInner<A>>,
}

impl<A: RosAction> ActionClient<A> {
    /// A client of the action `name`.
    pub fn new(endpoint: &Endpoint, name: &str) -> Result<Self> {
        let name = endpoint.qualify(name);
        let [send_goal, cancel_goal, get_result, feedback, status] = action_names(&name);
        let shared = Arc::new(ClientShared::<A> {
            feedback: Mutex::new(HashMap::new()),
            status: Mutex::new(HashMap::new()),
        });
        let s = shared.clone();
        let feedback_sub =
            endpoint.subscriber::<A::FeedbackMessage>(&feedback, Qos::default(), move |m, _| {
                let Ok(m) = m else { return };
                let (id, fb) = A::split_feedback_message(m);
                if let Some(q) = s.feedback.lock().unwrap().get_mut(&id) {
                    if q.len() >= FEEDBACK_BACKLOG {
                        q.pop_front();
                    }
                    q.push_back(fb);
                }
            })?;
        let s = shared.clone();
        let status_sub =
            endpoint.subscriber::<GoalStatusArray>(&status, status_qos(), move |m, _| {
                let Ok(m) = m else { return };
                let mut status = s.status.lock().unwrap();
                status.clear();
                for g in m.status_list {
                    status.insert(g.goal_info.goal_id.uuid, GoalStatus::from_i8(g.status));
                }
            })?;
        Ok(Self {
            inner: Arc::new(ClientInner {
                send_goal: endpoint.client(&send_goal, Qos::services())?,
                cancel_goal: endpoint.client(&cancel_goal, Qos::services())?,
                get_result: endpoint.client(&get_result, Qos::services())?,
                shared,
                name,
                _feedback: feedback_sub,
                _status: status_sub,
            }),
        })
    }

    /// Whether the action's three services have a server, waiting up to
    /// `timeout` for liveliness answers.
    pub fn server_available(&self, timeout: Duration) -> bool {
        let i = &self.inner;
        i.send_goal.raw().server_available(timeout)
            && i.cancel_goal.raw().server_available(timeout)
            && i.get_result.raw().server_available(timeout)
    }

    /// Polls [`server_available`](Self::server_available) until it holds or
    /// `timeout` passes.
    pub fn wait_for_server(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.server_available(Duration::from_millis(200)) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Sends a goal without waiting for the server's answer.
    pub fn send_goal_async(&self, goal: A::Goal, timeout: Duration) -> Result<PendingGoal<A>> {
        let id = new_goal_id();
        self.inner
            .shared
            .feedback
            .lock()
            .unwrap()
            .insert(id, VecDeque::new());
        let response = self
            .inner
            .send_goal
            .send(&A::send_goal_request(id, goal), timeout);
        match response {
            Ok(response) => Ok(PendingGoal {
                id,
                response: Some(response),
                inner: self.inner.clone(),
            }),
            Err(e) => {
                self.inner.shared.feedback.lock().unwrap().remove(&id);
                Err(e)
            }
        }
    }

    /// Sends a goal and waits for the server to accept it; fails if it
    /// rejects it or does not answer within `timeout`.
    pub fn send_goal(&self, goal: A::Goal, timeout: Duration) -> Result<ClientGoal<A>> {
        let mut pending = self.send_goal_async(goal, timeout)?;
        let response = pending.response.take().expect("not yet answered");
        let (accepted, stamp) = A::split_send_goal_response(response.wait()?);
        pending.accept(accepted, stamp)
    }

    /// Asks the server to cancel every goal (of any client).
    pub fn cancel_all(&self, timeout: Duration) -> Result<CancelGoal_Response> {
        self.inner
            .cancel_goal
            .call(&CancelGoal_Request::default(), timeout)
    }

    /// The fully qualified action name.
    pub fn name(&self) -> &str {
        &self.inner.name
    }
}

/// A goal sent but not yet answered.
pub struct PendingGoal<A: RosAction> {
    id: GoalId,
    response: Option<PendingResponse<A::SendGoal>>,
    inner: Arc<ClientInner<A>>,
}

impl<A: RosAction> PendingGoal<A> {
    /// The goal id.
    pub fn id(&self) -> GoalId {
        self.id
    }

    fn accept(&mut self, accepted: bool, stamp: Time) -> Result<ClientGoal<A>> {
        if !accepted {
            bail!(
                "{} rejected goal {}",
                self.inner.name,
                goal_id_string(&self.id)
            );
        }
        // As rclcpp: ask for the result right away; the server answers
        // when the goal ends.
        let result = self
            .inner
            .get_result
            .send(&A::get_result_request(self.id), FOREVER)?;
        let goal = ClientGoal {
            id: self.id,
            stamp,
            result: Mutex::new(ResultState::Pending(result)),
            inner: self.inner.clone(),
        };
        // The goal's feedback queue now belongs to the ClientGoal.
        self.id = [0; 16];
        Ok(goal)
    }

    /// The accepted goal once the server has answered (`None` while
    /// waiting, and after the answer was taken); an error if it rejected
    /// the goal or did not answer in time.
    pub fn try_accepted(&mut self) -> Option<Result<ClientGoal<A>>> {
        let answer = self.response.as_ref()?.try_recv()?;
        self.response = None;
        Some(answer.and_then(|r| {
            let (accepted, stamp) = A::split_send_goal_response(r);
            self.accept(accepted, stamp)
        }))
    }
}

impl<A: RosAction> Drop for PendingGoal<A> {
    fn drop(&mut self) {
        if self.id != [0; 16] {
            self.inner.shared.feedback.lock().unwrap().remove(&self.id);
        }
    }
}

enum ResultState<A: RosAction> {
    Pending(PendingResponse<A::GetResult>),
    Done(Result<(GoalStatus, A::Result), String>),
}

/// A goal the server accepted.
pub struct ClientGoal<A: RosAction> {
    id: GoalId,
    stamp: Time,
    result: Mutex<ResultState<A>>,
    inner: Arc<ClientInner<A>>,
}

impl<A: RosAction> ClientGoal<A> {
    /// The goal id.
    pub fn id(&self) -> GoalId {
        self.id
    }

    /// When the server accepted the goal.
    pub fn stamp(&self) -> &Time {
        &self.stamp
    }

    /// The latest status from the status topic (`Unknown` until one
    /// arrives). Once the result is in, its status.
    pub fn status(&self) -> GoalStatus {
        if let ResultState::Done(Ok((s, _))) = &*self.result.lock().unwrap() {
            return *s;
        }
        self.inner
            .shared
            .status
            .lock()
            .unwrap()
            .get(&self.id)
            .copied()
            .unwrap_or(GoalStatus::Unknown)
    }

    /// Feedback received since the last call, oldest first.
    pub fn feedback(&self) -> Vec<A::Feedback> {
        self.inner
            .shared
            .feedback
            .lock()
            .unwrap()
            .get_mut(&self.id)
            .map(|q| q.drain(..).collect())
            .unwrap_or_default()
    }

    /// The final status and result once the goal has ended (`None` while
    /// it runs).
    pub fn try_result(&self) -> Option<Result<(GoalStatus, A::Result)>> {
        let mut state = self.result.lock().unwrap();
        if let ResultState::Pending(p) = &*state {
            let r = p.try_recv()?;
            *state = ResultState::Done(
                r.map(|r| {
                    let (s, result) = A::split_get_result_response(r);
                    (GoalStatus::from_i8(s), result)
                })
                .map_err(|e| format!("{e:#}")),
            );
        }
        match &*state {
            ResultState::Done(r) => Some(r.clone().map_err(|e| anyhow!(e))),
            ResultState::Pending(_) => None,
        }
    }

    /// Waits up to `timeout` for the goal to end.
    pub fn result(&self, timeout: Duration) -> Result<(GoalStatus, A::Result)> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(r) = self.try_result() {
                return r;
            }
            if Instant::now() >= deadline {
                bail!(
                    "goal {} of {} did not end within {timeout:?}",
                    goal_id_string(&self.id),
                    self.inner.name
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn cancel_request(&self) -> CancelGoal_Request {
        CancelGoal_Request {
            goal_info: GoalInfo {
                goal_id: UUID { uuid: self.id },
                stamp: Time::default(),
            },
        }
    }

    /// Asks the server to cancel this goal and waits for its answer:
    /// whether the goal is now canceling.
    pub fn cancel(&self, timeout: Duration) -> Result<bool> {
        let resp = self
            .inner
            .cancel_goal
            .call(&self.cancel_request(), timeout)?;
        Ok(resp
            .goals_canceling
            .iter()
            .any(|g| g.goal_id.uuid == self.id))
    }

    /// Asks the server to cancel this goal without waiting.
    pub fn cancel_async(&self, timeout: Duration) -> Result<PendingResponse<CancelGoal>> {
        self.inner.cancel_goal.send(&self.cancel_request(), timeout)
    }
}

impl<A: RosAction> Drop for ClientGoal<A> {
    fn drop(&mut self) {
        self.inner.shared.feedback.lock().unwrap().remove(&self.id);
    }
}

/// The type name of an action's goal status array, for discovery.
pub const STATUS_TYPE: &str = <GoalStatusArray as crate::RosMessage>::TYPE_NAME;

/// The type name of the cancel service, for discovery.
pub const CANCEL_TYPE: &str = <CancelGoal as RosService>::TYPE_NAME;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_rcl_action() {
        assert_eq!(
            action_names("/fibonacci"),
            [
                "/fibonacci/_action/send_goal",
                "/fibonacci/_action/cancel_goal",
                "/fibonacci/_action/get_result",
                "/fibonacci/_action/feedback",
                "/fibonacci/_action/status",
            ]
        );
        assert_eq!(status_qos().to_keyexpr(), ":1:,1:,:,:,,");
    }

    #[test]
    fn goal_ids_are_v4_uuids_and_differ() {
        let a = new_goal_id();
        let b = new_goal_id();
        assert_ne!(a, b);
        assert_eq!(a[6] >> 4, 4);
        assert_eq!(a[8] >> 6, 2);
        let s = goal_id_string(&[0x12; 16]);
        assert_eq!(s, "12121212-1212-1212-1212-121212121212");
    }

    #[test]
    fn statuses_roundtrip() {
        for v in 0..=6 {
            assert_eq!(GoalStatus::from_i8(v).as_i8(), v);
        }
        assert!(GoalStatus::Canceled.is_terminal());
        assert!(!GoalStatus::Canceling.is_terminal());
    }
}
