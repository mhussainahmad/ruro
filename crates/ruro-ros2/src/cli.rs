//! `ruro ros2`: inspect a ROS 2 system, call its services and generate
//! message types.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use crate::idl::TypeName;
use crate::rmw::{Credentials, DEFAULT_ROUTER, Endpoint, EndpointConfig, Qos};

/// Arguments of `ruro ros2`.
#[derive(Debug, Args)]
pub struct Ros2Args {
    #[command(subcommand)]
    cmd: Ros2Cmd,
}

#[derive(Debug, Subcommand)]
enum Ros2Cmd {
    /// List ROS 2 topics announced through rmw_zenoh liveliness tokens.
    Topics {
        #[command(flatten)]
        graph: GraphArgs,
        /// Also list the topics actions are made of (`*/_action/*`).
        #[arg(long)]
        all: bool,
    },
    /// List ROS 2 services (not counting the ones actions are made of).
    Services {
        #[command(flatten)]
        graph: GraphArgs,
    },
    /// List ROS 2 actions.
    Actions {
        #[command(flatten)]
        graph: GraphArgs,
    },
    /// Call a ROS 2 service of a bundled (or `--package`) type, e.g.
    /// `ruro ros2 call /add_two_ints example_interfaces/srv/AddTwoInts '{a: 2, b: 3}'`.
    Call {
        /// Service name.
        service: String,
        /// Service type, e.g. `std_srvs/srv/Trigger`.
        ty: String,
        /// The request as YAML (flow style works on one line); fields left
        /// out take their defaults.
        #[arg(default_value = "{}")]
        request: String,
        /// Zenoh router to use (rmw_zenohd or a ruro daemon).
        #[arg(long, default_value = DEFAULT_ROUTER)]
        router: String,
        /// ROS domain id (default: $ROS_DOMAIN_ID or 0).
        #[arg(long)]
        domain: Option<u32>,
        /// How long to wait for the server and for the reply, in milliseconds.
        #[arg(long, default_value_t = 10000)]
        timeout: u64,
        /// Extra package directories to load besides the bundled ones.
        #[arg(long = "package")]
        packages: Vec<PathBuf>,
    },
    /// Generate Rust types from .msg/.srv/.action definitions.
    Gen {
        /// Package directories (named after the package, containing msg/,
        /// srv/ and/or action/).
        #[arg(required = true)]
        packages: Vec<PathBuf>,
        /// Output file (default: stdout).
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Module path the output will live at.
        #[arg(long, default_value = "crate::ros")]
        root: String,
    },
    /// Convert a ROS 2 launch file (XML or YAML; Python with --eval) into a
    /// ruro graph of ros2/node nodes.
    Import(crate::launch::ImportArgs),
    /// Print the RIHS01 hash of a message, service or action type, e.g.
    /// `geometry_msgs/msg/Twist` or `example_interfaces/srv/AddTwoInts`.
    Hash {
        ty: String,
        /// Extra package directories to load besides the bundled ones.
        packages: Vec<PathBuf>,
        /// Also print the hashed type description.
        #[arg(long)]
        verbose: bool,
    },
}

/// Where and how to look at a ROS 2 graph.
#[derive(Debug, Args)]
struct GraphArgs {
    /// Zenoh router to query (rmw_zenohd or a ruro daemon).
    #[arg(long, default_value = DEFAULT_ROUTER)]
    router: String,
    /// ROS domain id (default: $ROS_DOMAIN_ID or 0).
    #[arg(long)]
    domain: Option<u32>,
    /// How long to wait for answers, in milliseconds.
    #[arg(long, default_value_t = 1000)]
    timeout: u64,
    /// Print JSON instead of a table.
    #[arg(long)]
    json: bool,
}

/// The credentials to try, in order: those in the environment (`RURO_TOKEN`
/// and friends, for a ruro daemon that requires them), then the same
/// without the token, because a router that does not authenticate refuses
/// sessions that present one. Unreadable credentials are skipped.
fn credential_attempts() -> Vec<Credentials> {
    match Credentials::from_env() {
        Ok(c) if c.token.is_some() => vec![c.clone(), Credentials { token: None, ..c }],
        Ok(c) => vec![c],
        Err(_) => vec![Credentials::default()],
    }
}

/// Runs `open` with each credential attempt until one succeeds; the first
/// error otherwise.
fn with_credentials<T>(mut open: impl FnMut(Credentials) -> Result<T>) -> Result<T> {
    let mut first = None;
    for c in credential_attempts() {
        match open(c) {
            Ok(v) => return Ok(v),
            Err(e) => {
                first.get_or_insert(e);
            }
        }
    }
    Err(first.expect("at least one attempt"))
}

impl GraphArgs {
    /// The graph's entities, and the domain asked.
    fn entities(&self) -> Result<(Vec<crate::discovery::Entity>, u32)> {
        let domain = self.domain.unwrap_or(EndpointConfig::default().domain);
        let session = with_credentials(|credentials| {
            let cfg = EndpointConfig {
                connect: vec![self.router.clone()],
                credentials,
                ..Default::default()
            };
            zenoh::Wait::wait(zenoh::open(cfg.zenoh_config()?))
                .map_err(|e| anyhow::anyhow!("cannot reach Zenoh at {}: {e}", self.router))
        })?;
        let entities =
            crate::discovery::entities(&session, domain, Duration::from_millis(self.timeout))?;
        Ok((entities, domain))
    }
}

/// Prints rows under a header, the first column as wide as needed.
fn table(header: [&str; 4], rows: Vec<[String; 4]>) {
    let w = rows
        .iter()
        .map(|r| r[0].len())
        .chain([header[0].len()])
        .max()
        .unwrap_or(5);
    let t = rows
        .iter()
        .map(|r| r[1].len())
        .chain([header[1].len(), 36])
        .max()
        .unwrap_or(36);
    println!(
        "{:<w$}  {:<t$}  {:>3}  {:>3}",
        header[0], header[1], header[2], header[3]
    );
    for r in rows {
        println!("{:<w$}  {:<t$}  {:>3}  {:>3}", r[0], r[1], r[2], r[3]);
    }
}

/// Calls a service whose type is described by bundled or loaded
/// definitions, with the request given as YAML.
fn call(
    service: &str,
    ty: &str,
    request: &str,
    endpoint: &Endpoint,
    packages: &[PathBuf],
    timeout: Duration,
) -> Result<serde_json::Value> {
    let mut interfaces = crate::bundled_interfaces();
    for dir in packages {
        interfaces.load_package(&dir.canonicalize()?)?;
    }
    let name = match TypeName::parse(ty) {
        Some(t) if t.namespace == "msg" && !ty.contains("/msg/") => TypeName {
            namespace: "srv".into(),
            ..t
        },
        Some(t) => t,
        None => bail!("expected a type like `pkg/srv/Type`, got `{ty}`"),
    };
    let Some(srv) = interfaces.services.get(&name) else {
        bail!(
            "unknown service type {name} (bundled packages: {}; add others with --package)",
            crate::bundled_packages().join(", ")
        );
    };
    let value: serde_json::Value = serde_saphyr::from_str(request)
        .map_err(|e| anyhow::anyhow!("request is not valid YAML: {e}"))?;
    let payload = crate::json_cdr::encode(&interfaces, &srv.request.name, &value)?;
    let hash = crate::hash::type_hash(&interfaces, &name)
        .map_err(|missing| anyhow::anyhow!("unknown message type {missing}"))?;
    let client = endpoint.raw_client(service, &name.dds_name(), &hash, Qos::services())?;
    if !client.wait_for_server(timeout) {
        bail!("no server for {service} ({name}) appeared within {timeout:?}");
    }
    let (reply, _) = client.call(payload, timeout)?;
    crate::json_cdr::decode(&interfaces, &srv.response.name, &reply)
}

/// Runs `ruro ros2`.
pub fn run(args: Ros2Args) -> Result<()> {
    match args.cmd {
        Ros2Cmd::Topics { graph, all } => {
            let (entities, domain) = graph.entities()?;
            let mut topics = crate::discovery::topics(&entities);
            if !all {
                topics.retain(|t| !crate::discovery::is_action_part(&t.name));
            }
            if graph.json {
                println!("{}", serde_json::to_string_pretty(&topics)?);
            } else if topics.is_empty() {
                eprintln!("no ROS 2 topics in domain {domain} via {}", graph.router);
            } else {
                let rows = topics
                    .iter()
                    .map(|t| {
                        [
                            t.name.clone(),
                            t.ros_type.clone(),
                            t.publishers.to_string(),
                            t.subscribers.to_string(),
                        ]
                    })
                    .collect();
                table(["TOPIC", "TYPE", "PUB", "SUB"], rows);
            }
            Ok(())
        }
        Ros2Cmd::Services { graph } => {
            let (entities, domain) = graph.entities()?;
            let services = crate::discovery::services(&entities);
            if graph.json {
                println!("{}", serde_json::to_string_pretty(&services)?);
            } else if services.is_empty() {
                eprintln!("no ROS 2 services in domain {domain} via {}", graph.router);
            } else {
                let rows = services
                    .iter()
                    .map(|s| {
                        [
                            s.name.clone(),
                            s.ros_type.clone(),
                            s.servers.to_string(),
                            s.clients.to_string(),
                        ]
                    })
                    .collect();
                table(["SERVICE", "TYPE", "SRV", "CLI"], rows);
            }
            Ok(())
        }
        Ros2Cmd::Actions { graph } => {
            let (entities, domain) = graph.entities()?;
            let actions = crate::discovery::actions(&entities);
            if graph.json {
                println!("{}", serde_json::to_string_pretty(&actions)?);
            } else if actions.is_empty() {
                eprintln!("no ROS 2 actions in domain {domain} via {}", graph.router);
            } else {
                let rows = actions
                    .iter()
                    .map(|a| {
                        [
                            a.name.clone(),
                            a.ros_type.clone(),
                            a.servers.to_string(),
                            a.clients.to_string(),
                        ]
                    })
                    .collect();
                table(["ACTION", "TYPE", "SRV", "CLI"], rows);
            }
            Ok(())
        }
        Ros2Cmd::Call {
            service,
            ty,
            request,
            router,
            domain,
            timeout,
            packages,
        } => {
            let endpoint = with_credentials(|credentials| {
                let mut cfg = EndpointConfig {
                    connect: vec![router.clone()],
                    node_name: "ruro_ros2_call".into(),
                    credentials,
                    ..Default::default()
                };
                if let Some(d) = domain {
                    cfg.domain = d;
                }
                Endpoint::open(cfg)
            })?;
            let response = call(
                &service,
                &ty,
                &request,
                &endpoint,
                &packages,
                Duration::from_millis(timeout),
            )?;
            println!("{}", serde_json::to_string_pretty(&response)?);
            Ok(())
        }
        Ros2Cmd::Gen {
            packages,
            output,
            root,
        } => {
            let mut interfaces = crate::bundled_interfaces();
            let mut names = Vec::new();
            for dir in &packages {
                // Drop bundled definitions of a package being regenerated.
                let name = dir
                    .canonicalize()
                    .with_context(|| format!("{}", dir.display()))?
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                interfaces.remove_package(&name);
                interfaces.load_package(&dir.canonicalize()?)?;
                names.push(name);
            }
            let externs: BTreeMap<String, String> = crate::bundled_packages()
                .into_iter()
                .filter(|p| !names.contains(p))
                .map(|p| (p.clone(), format!("ruro_ros2::msgs::{p}")))
                .collect();
            let code = crate::codegen::generate(
                &interfaces,
                &crate::codegen::Options {
                    packages: names,
                    root,
                    runtime: "ruro_ros2".into(),
                    externs,
                },
            )?;
            match output {
                Some(path) => {
                    std::fs::write(&path, code)?;
                    eprintln!("wrote {}", path.display());
                }
                None => print!("{code}"),
            }
            Ok(())
        }
        Ros2Cmd::Import(args) => crate::launch::run(args),
        Ros2Cmd::Hash {
            ty,
            packages,
            verbose,
        } => {
            let mut interfaces = crate::bundled_interfaces();
            for dir in &packages {
                interfaces.load_package(&dir.canonicalize()?)?;
            }
            let Some(name) = TypeName::parse(&ty) else {
                bail!("expected a type like `pkg/msg/Type`, got `{ty}`");
            };
            let hash = crate::hash::type_hash(&interfaces, &name)
                .map_err(|missing| anyhow::anyhow!("unknown message type {missing}"))?;
            println!("{hash}");
            if verbose {
                println!(
                    "{}",
                    crate::hash::hashable_description(&interfaces, &name).unwrap_or_default()
                );
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msgs::example_interfaces::srv::{AddTwoInts, AddTwoInts_Response};
    use zenoh::Wait;

    #[test]
    fn calls_a_service_from_yaml() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let ep = format!("tcp/127.0.0.1:{port}");
        let mut c = zenoh::Config::default();
        c.insert_json5("mode", "\"router\"").unwrap();
        c.insert_json5("listen/endpoints", &format!("[\"{ep}\"]"))
            .unwrap();
        c.insert_json5("scouting/multicast/enabled", "false")
            .unwrap();
        let _router = zenoh::open(c).wait().unwrap();
        let node = Endpoint::open(EndpointConfig {
            connect: vec![ep],
            domain: 0,
            ..Default::default()
        })
        .unwrap();
        let _srv = node
            .service::<AddTwoInts>("/add", Qos::services(), |r| AddTwoInts_Response {
                sum: r.a + r.b,
            })
            .unwrap();
        let t = Duration::from_secs(10);
        let v = call(
            "/add",
            "example_interfaces/AddTwoInts",
            "{a: 2, b: 3}",
            &node,
            &[],
            t,
        )
        .unwrap();
        assert_eq!(v, serde_json::json!({"sum": 5}));
        let e = call("/add", "nope_msgs/srv/X", "{}", &node, &[], t).unwrap_err();
        assert!(e.to_string().contains("unknown service type"), "{e}");
        let e = call(
            "/add",
            "example_interfaces/srv/AddTwoInts",
            "{a: [",
            &node,
            &[],
            t,
        )
        .unwrap_err();
        assert!(e.to_string().contains("YAML"), "{e}");
    }
}
