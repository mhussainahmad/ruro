//! Connects a running executor to the control plane: parameter updates,
//! e-stop and daemon commands (recording, replay) in; output previews (taps)
//! out.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Result, anyhow};
use crossbeam_channel::bounded;
use ruro_msg::Message;
use ruro_proto::{CtlReply, HostCommand, Keys, SetParam, TapFrame};
use zenoh::Wait;

use crate::Executor;

/// Keeps publishers and subscribers alive for as long as the host runs.
pub struct Control {
    _taps: Arc<Vec<zenoh::pubsub::Publisher<'static>>>,
    _subscribers: Vec<zenoh::pubsub::Subscriber<()>>,
    _queryable: zenoh::query::Queryable<()>,
}

pub fn attach(session: &zenoh::Session, keys: &Keys, exec: &Arc<Executor>) -> Result<Control> {
    let err = |e: zenoh::Error| anyhow!("{e}");

    // Taps: each output gets a publisher. Previews are only generated while
    // something subscribes, which zenoh reports through matching status.
    let (tx, rx) = bounded::<(usize, ruro_msg::Preview)>(16);
    let mut publishers = Vec::new();
    for (port, out) in exec.outputs() {
        let index = publishers.len();
        let publisher = session
            .declare_publisher(keys.tap(&port.node, &port.port))
            .wait()
            .map_err(err)?;
        let tap_out = out.clone();
        publisher
            .matching_listener()
            .callback(move |status| tap_out.tap().set_active(status.matching()))
            .background()
            .wait()
            .map_err(err)?;
        let tx = tx.clone();
        out.tap().set_sink(Arc::new(move |preview| {
            // Never block a node thread: drop previews if the publisher lags.
            let _ = tx.try_send((index, preview));
        }));
        publishers.push(publisher);
    }
    let publishers = Arc::new(publishers);
    let pubs = publishers.clone();
    std::thread::Builder::new()
        .name("ruro-taps".into())
        .spawn(move || {
            let seq = AtomicU64::new(0);
            while let Ok((index, preview)) = rx.recv() {
                let frame = TapFrame {
                    seq: seq.fetch_add(1, Ordering::Relaxed),
                    preview,
                };
                let _ = pubs[index].put(ruro_proto::encode(&frame)).wait();
            }
        })?;

    // Parameters: one subscriber per node.
    let mut subscribers = Vec::new();
    for node in exec.node_ids() {
        let node_id = node.to_owned();
        let sender = exec.param_sender(node).expect("node exists");
        let sub = session
            .declare_subscriber(keys.param(node))
            .callback(move |sample| {
                let set: SetParam = match ruro_proto::decode(&sample.payload().to_bytes()) {
                    Ok(s) => s,
                    Err(e) => return tracing::warn!("{node_id}: bad parameter message: {e:#}"),
                };
                match serde_json::from_str(&set.value) {
                    Ok(value) => sender(set.name, value),
                    Err(e) => tracing::warn!("{node_id}.{}: invalid value: {e}", set.name),
                }
            })
            .wait()
            .map_err(err)?;
        subscribers.push(sub);
    }

    // Emergency stop, latched and republished by the daemon.
    let e = exec.clone();
    let sub = session
        .declare_subscriber(keys.estop())
        .callback(
            move |sample| match ruro_msg::Flag::decode(&sample.payload().to_bytes()) {
                Ok(flag) => e.set_estop(flag.0),
                Err(err) => tracing::warn!("bad e-stop message: {err}"),
            },
        )
        .wait()
        .map_err(err)?;
    subscribers.push(sub);

    // Daemon commands for this group.
    let e = exec.clone();
    let queryable = session
        .declare_queryable(keys.host_ctl(exec.group()))
        .callback(move |query| {
            let payload = query
                .payload()
                .map(|p| p.to_bytes().into_owned())
                .unwrap_or_default();
            let reply: CtlReply = serde_json::from_slice::<HostCommand>(&payload)
                .map_err(|e| anyhow!("bad host command: {e}"))
                .and_then(|cmd| command(&e, cmd))
                .map_err(|e| format!("{e:#}"));
            if let Err(msg) = &reply {
                tracing::warn!("{msg}");
            }
            let _ = query
                .reply(query.key_expr().clone(), ruro_proto::reply_bytes(&reply))
                .wait();
        })
        .wait()
        .map_err(err)?;

    Ok(Control {
        _taps: publishers,
        _subscribers: subscribers,
        _queryable: queryable,
    })
}

fn command(exec: &Executor, cmd: HostCommand) -> Result<String> {
    match cmd {
        HostCommand::Record { path: Some(path) } => {
            exec.start_recording(path.as_ref())?;
            Ok(format!("recording to {path}"))
        }
        HostCommand::Record { path: None } => Ok(match exec.stop_recording()? {
            Some((written, dropped)) => format!("recorded {written} message(s), dropped {dropped}"),
            None => "not recording".into(),
        }),
        HostCommand::Mute { nodes } => {
            exec.set_muted(&nodes);
            Ok(format!("{} node(s) replayed", nodes.len()))
        }
    }
}
