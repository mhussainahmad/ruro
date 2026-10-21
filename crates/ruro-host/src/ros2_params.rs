//! ROS 2 parameter services for the nodes of a group, for graphs with
//! `ros2: { parameters: true }`.
//!
//! Every node appears in the ROS graph as `/ruro_<node id>`, the name the
//! bridge nodes already use (a bridge node and its parameter services share
//! one ROS node), and offers the services rclcpp nodes do:
//! `get_parameters`, `set_parameters`, `set_parameters_atomically`,
//! `list_parameters`, `describe_parameters` and `get_parameter_types`. So
//! `ros2 param` and rqt read and tune ruro nodes. Values come from the
//! executor, which tracks changes from any source (`ruro param`, the
//! studio, ROS); sets go through the same checks and wait until the node
//! has applied them.
//!
//! As in rclcpp, every node also has a read-only `use_sim_time`, true when
//! the graph runs on `clock: sim`.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use ruro_graph::{ParamDesc, ParamKind, Ros2Settings};
use ruro_ros2::RosService;
use ruro_ros2::msgs::rcl_interfaces::{msg, srv};
use ruro_ros2::rmw::{Credentials, Endpoint, EndpointConfig, PendingRequest, Qos, RawService};
use serde_json::Value;

use crate::executor::Executor;

const NOT_SET: u8 = 0;
const BOOL: u8 = 1;
const INTEGER: u8 = 2;
const DOUBLE: u8 = 3;
const STRING: u8 = 4;

const USE_SIM_TIME: &str = "use_sim_time";

/// How long a set waits for its node, as for `ruro param`.
const APPLY_TIMEOUT: Duration = Duration::from_secs(2);

/// The services of every node of a group. Dropping it withdraws them.
pub struct ParamServers {
    _services: Vec<RawService>,
    _endpoints: Vec<Arc<Endpoint>>,
}

/// The ROS node name of a ruro node, as the bridge nodes name theirs.
pub fn ros_node_name(id: &str) -> String {
    let id: String = id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("ruro_{id}")
}

/// Offers the parameter services of every node of `exec`'s group.
pub fn attach(exec: &Arc<Executor>, settings: &Ros2Settings) -> Result<ParamServers> {
    let mut services = Vec::new();
    let mut endpoints = Vec::new();
    for id in exec.node_ids().map(str::to_owned).collect::<Vec<_>>() {
        let endpoint = endpoint(settings, &ros_node_name(&id))?;
        let base = format!("/{}", ros_node_name(&id));
        let node = Node {
            exec: exec.clone(),
            id: id.clone(),
        };
        let qos = Qos::services();
        let n = node.clone();
        services.push(endpoint.service::<srv::GetParameters>(
            &format!("{base}/get_parameters"),
            qos,
            move |req| n.get(&req.names),
        )?);
        let n = node.clone();
        services.push(endpoint.service::<srv::GetParameterTypes>(
            &format!("{base}/get_parameter_types"),
            qos,
            move |req| srv::GetParameterTypes_Response {
                types: n.get(&req.names).values.iter().map(|v| v.r#type).collect(),
            },
        )?);
        let n = node.clone();
        services.push(endpoint.service::<srv::ListParameters>(
            &format!("{base}/list_parameters"),
            qos,
            move |req| n.list(&req),
        )?);
        let n = node.clone();
        services.push(endpoint.service::<srv::DescribeParameters>(
            &format!("{base}/describe_parameters"),
            qos,
            move |req| n.describe(&req.names),
        )?);
        // Sets wait for the node: answered from a thread of their own, not
        // from Zenoh's callback.
        let n = node.clone();
        services.push(endpoint.service_deferred::<srv::SetParameters>(
            &format!("{base}/set_parameters"),
            qos,
            move |pending| {
                let n = n.clone();
                spawn(pending, move |req| srv::SetParameters_Response {
                    results: req.parameters.iter().map(|p| n.set(p)).collect(),
                });
            },
        )?);
        let n = node;
        services.push(endpoint.service_deferred::<srv::SetParametersAtomically>(
            &format!("{base}/set_parameters_atomically"),
            qos,
            move |pending| {
                let n = n.clone();
                spawn(pending, move |req| srv::SetParametersAtomically_Response {
                    result: n.set_all(&req.parameters),
                });
            },
        )?);
        endpoints.push(endpoint);
    }
    Ok(ParamServers {
        _services: services,
        _endpoints: endpoints,
    })
}

/// Answers `pending` from a new thread.
fn spawn<S: RosService>(
    pending: PendingRequest<S>,
    answer: impl FnOnce(&S::Request) -> S::Response + Send + 'static,
) where
    S::Request: Send + 'static,
{
    std::thread::spawn(move || {
        let response = answer(&pending.request);
        let _ = pending.respond(&response);
    });
}

/// The ROS node for `name`, shared with bridge nodes of the same settings.
/// A token from the environment is dropped if the router refuses it, as
/// the bridge nodes do.
fn endpoint(settings: &Ros2Settings, name: &str) -> Result<Arc<Endpoint>> {
    let credentials = Credentials::from_env()?;
    let mut attempts = vec![credentials.clone()];
    if credentials.token.is_some() {
        attempts.push(Credentials {
            token: None,
            ..credentials
        });
    }
    let mut first_error = None;
    for credentials in attempts {
        let mut cfg = EndpointConfig {
            connect: vec![settings.router.clone()],
            node_name: name.to_owned(),
            credentials,
            ..EndpointConfig::default()
        };
        if let Some(d) = settings.domain_id {
            cfg.domain = d;
        }
        match ruro_ros2::rmw::shared_endpoint(cfg) {
            Ok(e) => return Ok(e),
            Err(e) => {
                first_error.get_or_insert(e);
            }
        }
    }
    Err(first_error.expect("at least one attempt"))
}

#[derive(Clone)]
struct Node {
    exec: Arc<Executor>,
    id: String,
}

impl Node {
    fn params(&self) -> Vec<(ParamDesc, Value)> {
        self.exec.params(&self.id).unwrap_or_default()
    }

    fn sim_time(&self) -> bool {
        self.exec.clock().is_sim()
    }

    fn get(&self, names: &[String]) -> srv::GetParameters_Response {
        let params = self.params();
        let values = names
            .iter()
            .map(|name| {
                if name == USE_SIM_TIME {
                    return to_ros(ParamKind::Bool, &Value::Bool(self.sim_time()));
                }
                params
                    .iter()
                    .find(|(p, _)| &p.name == name)
                    .map(|(p, v)| to_ros(p.kind, v))
                    .unwrap_or_default()
            })
            .collect();
        srv::GetParameters_Response { values }
    }

    fn list(&self, req: &srv::ListParameters_Request) -> srv::ListParameters_Response {
        let mut names: Vec<String> = self.params().into_iter().map(|(p, _)| p.name).collect();
        names.push(USE_SIM_TIME.into());
        // As rclcpp: a name is listed if it is under one of the prefixes (or
        // there are none), and has fewer than `depth` dots below it (0:
        // any number).
        let shown = |name: &str| {
            let below = req
                .prefixes
                .iter()
                .find_map(|p| {
                    if name == p {
                        Some("")
                    } else {
                        name.strip_prefix(&format!("{p}."))
                    }
                })
                .or(req.prefixes.is_empty().then_some(name));
            below.is_some_and(|rest| {
                req.depth == 0 || (rest.matches('.').count() as u64) < req.depth
            })
        };
        names.retain(|n| shown(n));
        let mut prefixes: Vec<String> = names
            .iter()
            .filter_map(|n| n.rsplit_once('.').map(|(p, _)| p.to_owned()))
            .collect();
        prefixes.sort();
        prefixes.dedup();
        srv::ListParameters_Response {
            result: msg::ListParametersResult { names, prefixes },
        }
    }

    fn describe(&self, names: &[String]) -> srv::DescribeParameters_Response {
        let params = self.params();
        let descriptors = names
            .iter()
            .map(|name| {
                if name == USE_SIM_TIME {
                    return msg::ParameterDescriptor {
                        name: name.clone(),
                        r#type: BOOL,
                        description: "Whether the graph runs on simulated time (clock: sim)."
                            .into(),
                        read_only: true,
                        ..Default::default()
                    };
                }
                match params.iter().find(|(p, _)| &p.name == name) {
                    Some((p, _)) => descriptor(p),
                    None => msg::ParameterDescriptor {
                        name: name.clone(),
                        r#type: NOT_SET,
                        ..Default::default()
                    },
                }
            })
            .collect();
        srv::DescribeParameters_Response { descriptors }
    }

    /// The value `p` asks for, checked against its declaration.
    fn checked(&self, p: &msg::Parameter) -> Result<(String, Value), String> {
        if p.name == USE_SIM_TIME {
            return Err(format!(
                "`{USE_SIM_TIME}` is read-only: it follows the graph's `clock`"
            ));
        }
        let params = self.params();
        let (desc, _) = params
            .iter()
            .find(|(d, _)| d.name == p.name)
            .ok_or_else(|| format!("parameter `{}` is not declared", p.name))?;
        let value = from_ros(desc.kind, &p.value)?;
        desc.check(&value)?;
        Ok((p.name.clone(), value))
    }

    fn apply(&self, name: &str, value: Value) -> Result<(), String> {
        self.exec
            .apply_param(&self.id, name, value, APPLY_TIMEOUT)
            .map_err(|e| format!("{e:#}"))
    }

    fn set(&self, p: &msg::Parameter) -> msg::SetParametersResult {
        result(
            self.checked(p)
                .and_then(|(name, value)| self.apply(&name, value)),
        )
    }

    /// Checks every value before applying any. A node that refuses one
    /// keeps those applied before it.
    fn set_all(&self, ps: &[msg::Parameter]) -> msg::SetParametersResult {
        let checked: Result<Vec<_>, String> = ps.iter().map(|p| self.checked(p)).collect();
        result(checked.and_then(|values| {
            values
                .into_iter()
                .try_for_each(|(name, value)| self.apply(&name, value))
        }))
    }
}

fn result(r: Result<(), String>) -> msg::SetParametersResult {
    match r {
        Ok(()) => msg::SetParametersResult {
            successful: true,
            reason: String::new(),
        },
        Err(reason) => msg::SetParametersResult {
            successful: false,
            reason,
        },
    }
}

fn type_of(kind: ParamKind) -> u8 {
    match kind {
        ParamKind::Bool => BOOL,
        ParamKind::Int => INTEGER,
        ParamKind::Float => DOUBLE,
        ParamKind::String => STRING,
    }
}

/// A parameter value as ROS sees it: floats are always DOUBLE, also when
/// the graph wrote `3`.
fn to_ros(kind: ParamKind, v: &Value) -> msg::ParameterValue {
    let mut out = msg::ParameterValue {
        r#type: type_of(kind),
        ..Default::default()
    };
    match kind {
        ParamKind::Bool => out.bool_value = v.as_bool().unwrap_or_default(),
        ParamKind::Int => {
            out.integer_value = v
                .as_i64()
                .or_else(|| v.as_u64().map(|u| i64::try_from(u).unwrap_or(i64::MAX)))
                .or_else(|| v.as_f64().map(|f| f as i64))
                .unwrap_or_default();
        }
        ParamKind::Float => out.double_value = v.as_f64().unwrap_or_default(),
        ParamKind::String => out.string_value = v.as_str().unwrap_or_default().to_owned(),
    }
    out
}

/// A value from ROS for a parameter of `kind`. Integers are taken for
/// float parameters (`ros2 param set /n k 2` sends one).
fn from_ros(kind: ParamKind, v: &msg::ParameterValue) -> Result<Value, String> {
    let names = ["no value", "a bool", "an integer", "a double", "a string"];
    let got = names.get(v.r#type as usize).copied().unwrap_or("an array");
    Ok(match (kind, v.r#type) {
        (ParamKind::Bool, BOOL) => Value::Bool(v.bool_value),
        (ParamKind::Int, INTEGER) => Value::from(v.integer_value),
        (ParamKind::Float, DOUBLE) => Value::from(v.double_value),
        (ParamKind::Float, INTEGER) => Value::from(v.integer_value as f64),
        (ParamKind::String, STRING) => Value::String(v.string_value.clone()),
        _ => {
            let want = names[type_of(kind) as usize];
            return Err(format!("parameter expects {want}, got {got}"));
        }
    })
}

fn descriptor(p: &ParamDesc) -> msg::ParameterDescriptor {
    let mut d = msg::ParameterDescriptor {
        name: p.name.clone(),
        r#type: type_of(p.kind),
        description: p.doc.clone(),
        ..Default::default()
    };
    if p.min.is_none() && p.max.is_none() {
        return d;
    }
    // ROS ranges have both ends; an open end is the type's limit.
    match p.kind {
        ParamKind::Int => d.integer_range.push(msg::IntegerRange {
            from_value: p.min.map_or(i64::MIN, |m| m.ceil() as i64),
            to_value: p.max.map_or(i64::MAX, |m| m.floor() as i64),
            step: 0,
        }),
        ParamKind::Float => d.floating_point_range.push(msg::FloatingPointRange {
            from_value: p.min.unwrap_or(f64::MIN),
            to_value: p.max.unwrap_or(f64::MAX),
            step: 0.0,
        }),
        ParamKind::Bool | ParamKind::String => {}
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_convert_like_rclcpp() {
        assert_eq!(to_ros(ParamKind::Float, &Value::from(3)).r#type, DOUBLE);
        assert_eq!(to_ros(ParamKind::Float, &Value::from(3)).double_value, 3.0);
        let int = msg::ParameterValue {
            r#type: INTEGER,
            integer_value: 2,
            ..Default::default()
        };
        assert_eq!(from_ros(ParamKind::Float, &int).unwrap(), Value::from(2.0));
        assert_eq!(from_ros(ParamKind::Int, &int).unwrap(), Value::from(2));
        let e = from_ros(ParamKind::Bool, &int).unwrap_err();
        assert_eq!(e, "parameter expects a bool, got an integer");
    }

    #[test]
    fn open_ranges_end_at_the_type_limits() {
        let p = ParamDesc {
            name: "n".into(),
            kind: ParamKind::Int,
            default: Value::from(1),
            min: Some(1.0),
            max: None,
            doc: String::new(),
        };
        let r = &descriptor(&p).integer_range[0];
        assert_eq!((r.from_value, r.to_value, r.step), (1, i64::MAX, 0));
    }
}
