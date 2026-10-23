//! The node that stands in for a program in the graph.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use ruro_node::{Builder, Ctx, DynNode};
use serde_json::{Map, Value, json};

use crate::child::Child;
use crate::manifest::Manifest;
use crate::message::{AnyInput, AnyOutput};

/// How long `stop` waits for the program to acknowledge.
const STOP_TIMEOUT: Duration = Duration::from_secs(2);
const BACKOFF_MIN: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(10);

pub struct ProcessNode {
    manifest: Arc<Manifest>,
    id: String,
    inputs: Vec<(String, AnyInput)>,
    outputs: Vec<(String, AnyOutput)>,
    params: Map<String, Value>,
    child: Option<Child>,
    /// When the program may be started again after it died.
    retry_at: Option<Instant>,
    backoff: Duration,
    restarts: u32,
}

impl ProcessNode {
    pub fn build(manifest: Arc<Manifest>, b: &mut Builder) -> Result<Box<dyn DynNode>> {
        let d = &manifest.descriptor;
        let inputs = d
            .inputs
            .iter()
            .map(|p| Ok((p.name.clone(), AnyInput::declare(b, &p.ty, &p.name)?)))
            .collect::<Result<_>>()?;
        let outputs = d
            .outputs
            .iter()
            .map(|p| Ok((p.name.clone(), AnyOutput::declare(b, &p.ty, &p.name)?)))
            .collect::<Result<_>>()?;
        let params = d
            .params
            .iter()
            .map(|p| {
                let v = b.raw_param(&p.name).cloned().unwrap_or(p.default.clone());
                (p.name.clone(), v)
            })
            .collect();
        Ok(Box::new(Self {
            id: b.id().to_owned(),
            manifest,
            inputs,
            outputs,
            params,
            child: None,
            retry_at: None,
            backoff: BACKOFF_MIN,
            restarts: 0,
        }))
    }

    /// Starts the program and sends it `start`.
    fn launch(&mut self, ctx: &Ctx) -> Result<()> {
        let mut child = Child::spawn(&self.manifest, &self.id)?;
        let start = json!({
            "op": "start",
            "node": self.id,
            "kind": self.manifest.descriptor.kind,
            "params": self.params,
            "estopped": ctx.estopped(),
        });
        let reply = child.request(&start, self.manifest.start_timeout)?;
        refused(&reply).context("the program refused to start")?;
        tracing::debug!(
            "{}: started `{}` (pid {})",
            self.id,
            self.manifest.command[0],
            child.pid()
        );
        self.child = Some(child);
        Ok(())
    }

    /// Kills the program after a failure; it is started again later.
    fn lost(&mut self, e: anyhow::Error) -> anyhow::Error {
        self.child = None;
        self.retry_at = Some(Instant::now() + self.backoff);
        let e = e.context(format!(
            "restarting it in {:.1} s",
            self.backoff.as_secs_f64()
        ));
        self.backoff = (self.backoff * 2).min(BACKOFF_MAX);
        e
    }

    fn step_inner(&mut self, ctx: &Ctx) -> Result<()> {
        if self.child.is_none() {
            if self.retry_at.is_some_and(|t| Instant::now() < t) {
                bail!("the program is down; waiting to restart it");
            }
            self.restarts += 1;
            if let Err(e) = self.launch(ctx) {
                return Err(self.lost(e));
            }
            tracing::warn!("{}: program restarted ({} so far)", self.id, self.restarts);
        }
        let inputs: Map<String, Value> = self
            .inputs
            .iter()
            .map(|(name, i)| (name.clone(), Value::Array(i.drain())))
            .collect();
        let request = json!({
            "op": "step",
            "steps": ctx.steps(),
            "dt": ctx.dt().as_secs_f64(),
            "time": ctx.time().as_secs_f64(),
            "now_ns": ctx.now_ns(),
            "estopped": ctx.estopped(),
            "inputs": inputs,
        });
        let timeout = self.manifest.step_timeout;
        let reply = match self
            .child
            .as_mut()
            .expect("running")
            .request(&request, timeout)
        {
            Ok(r) => r,
            Err(e) => return Err(self.lost(e)),
        };
        // A healthy answer resets the backoff.
        self.backoff = BACKOFF_MIN;
        self.send_outputs(&reply)?;
        refused(&reply)
    }

    fn send_outputs(&self, reply: &Value) -> Result<()> {
        let Some(outputs) = reply.get("outputs") else {
            return Ok(());
        };
        let outputs = outputs
            .as_object()
            .context("`outputs` in the reply is not an object")?;
        for (name, msgs) in outputs {
            let (_, port) = self
                .outputs
                .iter()
                .find(|(n, _)| n == name)
                .ok_or_else(|| {
                    let names: Vec<&str> = self.outputs.iter().map(|(n, _)| n.as_str()).collect();
                    anyhow!("the program sent to output `{name}`, which the manifest does not declare (outputs: {})", names.join(", "))
                })?;
            let msgs = msgs
                .as_array()
                .with_context(|| format!("output `{name}`: expected a list of messages"))?;
            for m in msgs {
                port.send(m.clone())
                    .with_context(|| format!("output `{name}`"))?;
            }
        }
        Ok(())
    }
}

/// The program's `error`, if its reply has one.
fn refused(reply: &Value) -> Result<()> {
    match reply.get("error") {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(e)) => bail!("{e}"),
        Some(e) => bail!("{e}"),
    }
}

impl DynNode for ProcessNode {
    fn start(&mut self, ctx: &mut Ctx) -> ruro_node::Result {
        self.launch(ctx)
            .with_context(|| format!("`{}`", self.manifest.command.join(" ")))
    }

    fn step(&mut self, ctx: &mut Ctx) -> ruro_node::Result {
        self.step_inner(ctx)
    }

    fn stop(&mut self, _ctx: &mut Ctx) {
        // Dropping the child terminates whatever is left of it.
        if let Some(mut child) = self.child.take()
            && child.request(&json!({"op": "stop"}), STOP_TIMEOUT).is_ok()
        {
            child.wait_exit(STOP_TIMEOUT);
        }
    }

    fn set_param(&mut self, name: &str, value: &Value) -> ruro_node::Result {
        if let Some(child) = self.child.as_mut() {
            let request = json!({"op": "param", "name": name, "value": value});
            let reply = child.request(&request, self.manifest.step_timeout)?;
            refused(&reply)?;
        }
        self.params.insert(name.to_owned(), value.clone());
        Ok(())
    }

    fn on_estop(&mut self, _ctx: &mut Ctx) {
        if let Some(child) = self.child.as_mut()
            && let Err(e) = child.request(&json!({"op": "estop"}), self.manifest.step_timeout)
        {
            tracing::warn!("{}: telling the program about the e-stop: {e:#}", self.id);
        }
    }
}
