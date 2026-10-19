//! Service nodes: call and serve `std_srvs/Trigger` and `std_srvs/SetBool`
//! (`ros2/call-trigger`, `ros2/serve-trigger`, `ros2/call-set-bool`,
//! `ros2/serve-set-bool`).
//!
//! Calls and requests go through the node's step (20 Hz for callers, 50 Hz
//! for servers), so the e-stop is checked for each one: while it is
//! engaged, callers drop their input instead of calling, and servers refuse
//! requests (`success: false`) instead of passing them on, except the safe
//! `SetBool` value `false`. For other service types use
//! `ruro_ros2::rmw::Endpoint::client` and `service` from Rust.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use ruro::prelude::*;
use ruro_ros2::RosService;
use ruro_ros2::msgs::std_srvs::srv::{
    SetBool, SetBool_Request, SetBool_Response, Trigger, Trigger_Request, Trigger_Response,
};
use ruro_ros2::rmw::{Client, Endpoint, PendingRequest, PendingResponse, Qos, RawService};

use super::Settings;

/// The message servers answer with while the e-stop is engaged.
pub const ESTOP_REFUSAL: &str = "refused: ruro e-stop engaged";

/// A service client and the calls in flight.
pub struct CallState<S: RosService> {
    settings: Settings,
    _endpoint: Arc<Endpoint>,
    client: Client<S>,
    pending: Vec<PendingResponse<S>>,
}

fn connect<S: RosService>(settings: Settings) -> Result<CallState<S>> {
    let endpoint = settings.endpoint()?;
    let client = endpoint.client::<S>(&settings.topic, Qos::services())?;
    info!(
        "calling ROS 2 service {} ({}) in domain {}",
        settings.topic,
        S::TYPE_NAME,
        settings.domain
    );
    Ok(CallState {
        settings,
        _endpoint: endpoint,
        client,
        pending: Vec::new(),
    })
}

impl<S: RosService> CallState<S> {
    fn call(&mut self, request: &S::Request, timeout_ms: i64) -> Result {
        let timeout = Duration::from_millis(timeout_ms.max(1) as u64);
        self.pending.push(self.client.send(request, timeout)?);
        Ok(())
    }

    /// Responses that arrived, in call order of arrival; failed calls as
    /// errors.
    fn responses(&mut self) -> Vec<Result<S::Response>> {
        let mut out = Vec::new();
        self.pending.retain(|p| match p.try_recv() {
            Some(r) => {
                out.push(r);
                false
            }
            None => true,
        });
        out
    }
}

/// A service server whose requests wait for the node's step.
pub struct ServeState<S: RosService> {
    settings: Settings,
    _endpoint: Arc<Endpoint>,
    _service: RawService,
    requests: Receiver<PendingRequest<S>>,
}

fn serve<S: RosService>(settings: Settings) -> Result<ServeState<S>> {
    let endpoint = settings.endpoint()?;
    let (tx, requests) = channel();
    let service = endpoint.service_deferred::<S>(&settings.topic, Qos::services(), move |r| {
        let _ = tx.send(r);
    })?;
    info!(
        "serving ROS 2 service {} ({}) in domain {}",
        settings.topic,
        S::TYPE_NAME,
        settings.domain
    );
    Ok(ServeState {
        settings,
        _endpoint: endpoint,
        _service: service,
        requests,
    })
}

/// Settings of a service node.
fn settings(
    ctx: &Ctx,
    service: &str,
    domain_id: i64,
    router: &str,
    token: &str,
) -> Result<Settings> {
    if service.is_empty() {
        bail!("parameter `service` is empty");
    }
    Settings::new(ctx.id(), service, domain_id, "reliable", router, token)
}

/// Common parameters, documented once.
macro_rules! service_node {
    (
        $(#[$meta:meta])*
        pub struct $Name:ident {
            $($body:tt)*
        }
    ) => {
        $(#[$meta])*
        pub struct $Name {
            $($body)*
            /// ROS_DOMAIN_ID; -1 uses the environment variable (default 0).
            #[param(default = -1, min = -1, max = 232)]
            domain_id: i64,
            /// Zenoh router shared with the ROS 2 nodes.
            #[param(default = "tcp/127.0.0.1:7447")]
            router: String,
            /// Token of a router that requires one (a ruro daemon started
            /// with a token). Empty uses RURO_TOKEN, which daemons give
            /// their hosts. TLS settings come from RURO_TLS_CA,
            /// RURO_TLS_CERT and RURO_TLS_KEY.
            #[param(default = "")]
            token: String,
        }
    };
}

service_node! {
    /// Calls a `std_srvs/Trigger` service for every true `trigger` input and
    /// outputs the response's `success` and `message`. A failed call (no
    /// server, timeout) outputs `success: false` and the error. While the
    /// e-stop is engaged, input is dropped and nothing is called.
    #[derive(Node)]
    #[node(kind = "ros2/call-trigger", rate = 20, doc = "Calls a ROS 2 std_srvs/Trigger service for every true input and outputs the response. While the e-stop is engaged, input is dropped and nothing is called.")]
    pub struct CallTrigger {
        #[input]
        trigger: Input<Flag>,
        #[output]
        success: Output<Flag>,
        #[output]
        message: Output<Text>,
        /// ROS 2 service name.
        #[param(default = "/trigger")]
        service: String,
        /// How long to wait for each response, in milliseconds.
        #[param(default = 5000, min = 1)]
        timeout_ms: i64,
        state: Option<CallState<Trigger>>,
    }
}

impl CallTrigger {
    fn ensure(&mut self, ctx: &Ctx) -> Result {
        let want = settings(
            ctx,
            &self.service,
            self.domain_id,
            &self.router,
            &self.token,
        )?;
        if self.state.as_ref().is_none_or(|s| s.settings != want) {
            self.state = None;
            self.state = Some(connect(want)?);
        }
        Ok(())
    }
}

impl Node for CallTrigger {
    fn start(&mut self, ctx: &mut Ctx) -> Result {
        self.ensure(ctx)
    }

    fn step(&mut self, ctx: &mut Ctx) -> Result {
        self.ensure(ctx)?;
        let state = self.state.as_mut().expect("connected");
        while let Some(s) = self.trigger.recv() {
            if !s.msg.0 {
                continue;
            }
            if ctx.estopped() {
                warn!("e-stop engaged: not calling {}", self.service);
                continue;
            }
            state.call(&Trigger_Request {}, self.timeout_ms)?;
        }
        for r in state.responses() {
            match r {
                Ok(resp) => {
                    self.success.send(Flag(resp.success))?;
                    self.message.send(Text(resp.message))?;
                }
                Err(e) => {
                    warn!("{}: {e:#}", self.service);
                    self.success.send(Flag(false))?;
                    self.message.send(Text(format!("{e:#}")))?;
                }
            }
        }
        Ok(())
    }

    fn stop(&mut self, _ctx: &mut Ctx) {
        self.state = None;
    }
}

service_node! {
    /// Calls a `std_srvs/SetBool` service with every `data` input and
    /// outputs the response's `success` and `message`. While the e-stop is
    /// engaged, `true` inputs are dropped; `false` (the safe value, e.g.
    /// motors off) is still sent.
    #[derive(Node)]
    #[node(kind = "ros2/call-set-bool", rate = 20, doc = "Calls a ROS 2 std_srvs/SetBool service with every input and outputs the response. While the e-stop is engaged, true inputs are dropped; false is still sent.")]
    pub struct CallSetBool {
        #[input]
        data: Input<Flag>,
        #[output]
        success: Output<Flag>,
        #[output]
        message: Output<Text>,
        /// ROS 2 service name.
        #[param(default = "/set_bool")]
        service: String,
        /// How long to wait for each response, in milliseconds.
        #[param(default = 5000, min = 1)]
        timeout_ms: i64,
        state: Option<CallState<SetBool>>,
    }
}

impl CallSetBool {
    fn ensure(&mut self, ctx: &Ctx) -> Result {
        let want = settings(
            ctx,
            &self.service,
            self.domain_id,
            &self.router,
            &self.token,
        )?;
        if self.state.as_ref().is_none_or(|s| s.settings != want) {
            self.state = None;
            self.state = Some(connect(want)?);
        }
        Ok(())
    }
}

impl Node for CallSetBool {
    fn start(&mut self, ctx: &mut Ctx) -> Result {
        self.ensure(ctx)
    }

    fn step(&mut self, ctx: &mut Ctx) -> Result {
        self.ensure(ctx)?;
        let state = self.state.as_mut().expect("connected");
        while let Some(s) = self.data.recv() {
            if s.msg.0 && ctx.estopped() {
                warn!("e-stop engaged: not sending true to {}", self.service);
                continue;
            }
            state.call(&SetBool_Request { data: s.msg.0 }, self.timeout_ms)?;
        }
        for r in state.responses() {
            match r {
                Ok(resp) => {
                    self.success.send(Flag(resp.success))?;
                    self.message.send(Text(resp.message))?;
                }
                Err(e) => {
                    warn!("{}: {e:#}", self.service);
                    self.success.send(Flag(false))?;
                    self.message.send(Text(format!("{e:#}")))?;
                }
            }
        }
        Ok(())
    }

    fn stop(&mut self, _ctx: &mut Ctx) {
        self.state = None;
    }
}

service_node! {
    /// Serves `std_srvs/Trigger`: each request outputs `triggered: true` and
    /// is answered `success: true` with `message`. While the e-stop is
    /// engaged, requests are refused (`success: false`) and nothing is
    /// output.
    #[derive(Node)]
    #[node(kind = "ros2/serve-trigger", rate = 50, doc = "Serves a ROS 2 std_srvs/Trigger service: each request outputs true and is answered success. While the e-stop is engaged, requests are refused and nothing is output.")]
    pub struct ServeTrigger {
        #[output]
        triggered: Output<Flag>,
        /// ROS 2 service name.
        #[param(default = "/trigger")]
        service: String,
        /// The response message.
        #[param(default = "triggered")]
        message: String,
        state: Option<ServeState<Trigger>>,
    }
}

impl ServeTrigger {
    fn ensure(&mut self, ctx: &Ctx) -> Result {
        let want = settings(
            ctx,
            &self.service,
            self.domain_id,
            &self.router,
            &self.token,
        )?;
        if self.state.as_ref().is_none_or(|s| s.settings != want) {
            self.state = None;
            self.state = Some(serve(want)?);
        }
        Ok(())
    }
}

impl Node for ServeTrigger {
    fn start(&mut self, ctx: &mut Ctx) -> Result {
        self.ensure(ctx)
    }

    fn step(&mut self, ctx: &mut Ctx) -> Result {
        self.ensure(ctx)?;
        let state = self.state.as_ref().expect("serving");
        while let Ok(req) = state.requests.try_recv() {
            let resp = if ctx.estopped() {
                Trigger_Response {
                    success: false,
                    message: ESTOP_REFUSAL.into(),
                }
            } else {
                self.triggered.send(Flag(true))?;
                Trigger_Response {
                    success: true,
                    message: self.message.clone(),
                }
            };
            req.respond(&resp)?;
        }
        Ok(())
    }

    fn stop(&mut self, _ctx: &mut Ctx) {
        self.state = None;
    }
}

service_node! {
    /// Serves `std_srvs/SetBool`: each request outputs its `data` and is
    /// answered `success: true`. While the e-stop is engaged, requests for
    /// `true` are refused (`success: false`) and not output; `false` (the
    /// safe value) still goes through.
    #[derive(Node)]
    #[node(kind = "ros2/serve-set-bool", rate = 50, doc = "Serves a ROS 2 std_srvs/SetBool service: each request outputs its data and is answered success. While the e-stop is engaged, requests for true are refused.")]
    pub struct ServeSetBool {
        #[output]
        data: Output<Flag>,
        /// ROS 2 service name.
        #[param(default = "/set_bool")]
        service: String,
        state: Option<ServeState<SetBool>>,
    }
}

impl ServeSetBool {
    fn ensure(&mut self, ctx: &Ctx) -> Result {
        let want = settings(
            ctx,
            &self.service,
            self.domain_id,
            &self.router,
            &self.token,
        )?;
        if self.state.as_ref().is_none_or(|s| s.settings != want) {
            self.state = None;
            self.state = Some(serve(want)?);
        }
        Ok(())
    }
}

impl Node for ServeSetBool {
    fn start(&mut self, ctx: &mut Ctx) -> Result {
        self.ensure(ctx)
    }

    fn step(&mut self, ctx: &mut Ctx) -> Result {
        self.ensure(ctx)?;
        let state = self.state.as_ref().expect("serving");
        while let Ok(req) = state.requests.try_recv() {
            let value = req.request.data;
            let resp = if value && ctx.estopped() {
                SetBool_Response {
                    success: false,
                    message: ESTOP_REFUSAL.into(),
                }
            } else {
                self.data.send(Flag(value))?;
                SetBool_Response {
                    success: true,
                    message: String::new(),
                }
            };
            req.respond(&resp)?;
        }
        Ok(())
    }

    fn stop(&mut self, _ctx: &mut Ctx) {
        self.state = None;
    }
}
