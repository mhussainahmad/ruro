//! ROS 2 bridge nodes (`ros2/*`).
//!
//! Each supported message pair has a subscriber node (`ros2/sub-<type>`)
//! that turns a ROS 2 topic into a ruro output, and a publisher node
//! (`ros2/pub-<type>`) that publishes a ruro input on a ROS 2 topic. They
//! speak rmw_zenoh's protocol, so they work with ROS 2 nodes running
//! `RMW_IMPLEMENTATION=rmw_zenoh_cpp` that share a Zenoh router with them
//! (`rmw_zenohd`, or a ruro daemon, which is a Zenoh router too).
//!
//! | ruro | ROS 2 | kinds |
//! |---|---|---|
//! | `Twist` | `geometry_msgs/Twist` | `ros2/sub-twist`, `ros2/pub-twist` |
//! | `Pose` | `geometry_msgs/PoseStamped` | `ros2/sub-pose-stamped`, `ros2/pub-pose-stamped` |
//! | `Image` | `sensor_msgs/Image` | `ros2/sub-image`, `ros2/pub-image` |
//! | `CameraInfo` | `sensor_msgs/CameraInfo` | `ros2/sub-camera-info`, `ros2/pub-camera-info` |
//! | `LaserScan` | `sensor_msgs/LaserScan` | `ros2/sub-laser-scan`, `ros2/pub-laser-scan` |
//! | `Imu` | `sensor_msgs/Imu` | `ros2/sub-imu`, `ros2/pub-imu` |
//! | `JointState` | `sensor_msgs/JointState` | `ros2/sub-joint-state`, `ros2/pub-joint-state` |
//! | `Joy` | `sensor_msgs/Joy` | `ros2/sub-joy`, `ros2/pub-joy` |
//! | `PointCloud` | `sensor_msgs/PointCloud2` (xyz float32) | `ros2/sub-point-cloud2`, `ros2/pub-point-cloud2` |
//! | `Odometry` | `nav_msgs/Odometry` | `ros2/sub-odometry`, `ros2/pub-odometry` |
//! | `Transforms` | `tf2_msgs/TFMessage` | `ros2/sub-tf`, `ros2/pub-tf` |
//! | `Scalar` | `std_msgs/Float64` | `ros2/sub-float64`, `ros2/pub-float64` |
//! | `Text` | `std_msgs/String` | `ros2/sub-string`, `ros2/pub-string` |
//! | `Flag` | `std_msgs/Bool` | `ros2/sub-bool`, `ros2/pub-bool` |
//!
//! Limitations: durability is always volatile, so a `ros2/sub-tf` on
//! `/tf_static` only sees static transforms published after it starts, and a
//! `ros2/pub-tf` on `/tf_static` is not latched for late ROS subscribers.
//! Subscriber nodes send from the Zenoh callback; their own step runs at
//! 1 Hz only to apply parameter changes, so `ruro status` shows that rate
//! rather than the message rate.

pub mod convert;

use std::sync::Arc;

use ruro::prelude::*;
use ruro_ros2::RosMessage;
use ruro_ros2::rmw::{Endpoint, EndpointConfig, Publisher, Qos, Reliability, Subscriber};

/// Forces this crate to be linked so its nodes are registered.
pub fn link() {}

/// Connection settings a bridge node was started with.
#[derive(Debug, Clone, PartialEq)]
struct Settings {
    topic: String,
    domain: u32,
    qos: Qos,
    router: String,
    node: String,
}

impl Settings {
    fn new(node: &str, topic: &str, domain_id: i64, qos: &str, router: &str) -> Result<Self> {
        if topic.is_empty() {
            bail!("parameter `topic` is empty");
        }
        let domain = if domain_id < 0 {
            EndpointConfig::default().domain
        } else {
            u32::try_from(domain_id).context("domain_id")?
        };
        let reliability: Reliability = qos.parse()?;
        // ROS node names allow letters, digits and underscores.
        let node: String = node
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        Ok(Self {
            topic: topic.to_owned(),
            domain,
            qos: Qos {
                reliability,
                ..Qos::default()
            },
            router: router.to_owned(),
            node: format!("ruro_{node}"),
        })
    }

    fn endpoint(&self) -> Result<Arc<Endpoint>> {
        ruro_ros2::rmw::shared_endpoint(EndpointConfig {
            connect: vec![self.router.clone()],
            domain: self.domain,
            node_name: self.node.clone(),
            ..EndpointConfig::default()
        })
    }
}

/// An active subscription and the settings it was made with.
pub struct SubState {
    settings: Settings,
    _endpoint: Arc<Endpoint>,
    _subscriber: Subscriber,
}

/// An active publisher and the settings it was made with.
pub struct PubState<M> {
    settings: Settings,
    _endpoint: Arc<Endpoint>,
    publisher: Publisher<M>,
}

fn subscribe<M: RosMessage, R: ruro::msg::Message>(
    settings: Settings,
    out: Output<R>,
    convert: fn(M) -> Result<R>,
) -> Result<SubState> {
    let endpoint = settings.endpoint()?;
    let topic = settings.topic.clone();
    let subscriber = endpoint.subscriber::<M>(&settings.topic, settings.qos, move |msg, _| {
        let converted = msg.map_err(anyhow::Error::from).and_then(convert);
        match converted {
            Ok(v) => {
                let _ = out.send(v);
            }
            Err(e) => warn!("{topic}: dropped message: {e:#}"),
        }
    })?;
    info!(
        "subscribed to ROS 2 {} ({}) in domain {}",
        settings.topic,
        M::TYPE_NAME,
        settings.domain
    );
    Ok(SubState {
        settings,
        _endpoint: endpoint,
        _subscriber: subscriber,
    })
}

fn advertise<M: RosMessage>(settings: Settings) -> Result<PubState<M>> {
    let endpoint = settings.endpoint()?;
    let publisher = endpoint.publisher::<M>(&settings.topic, settings.qos)?;
    info!(
        "publishing ROS 2 {} ({}) in domain {}",
        settings.topic,
        M::TYPE_NAME,
        settings.domain
    );
    Ok(PubState {
        settings,
        _endpoint: endpoint,
        publisher,
    })
}

macro_rules! bridge {
    (
        $sub_kind:literal, $Sub:ident, $pub_kind:literal, $Pub:ident,
        $ruro:ty, $ros:ty, $from:path, $to:path, $topic:literal, $ros_name:literal
    ) => {
        #[doc = concat!("Subscribes to a ROS 2 `", $ros_name, "` topic and outputs each message.")]
        #[derive(Node)]
        #[node(kind = $sub_kind, rate = 1, doc = concat!("Subscribes to a ROS 2 `", $ros_name, "` topic and outputs each message."))]
        pub struct $Sub {
            #[output]
            out: Output<$ruro>,
            /// ROS 2 topic name.
            #[param(default = $topic)]
            topic: String,
            /// ROS_DOMAIN_ID; -1 uses the environment variable (default 0).
            #[param(default = -1, min = -1, max = 232)]
            domain_id: i64,
            /// `reliable` or `best_effort`.
            #[param(default = "reliable")]
            qos: String,
            /// Zenoh router shared with the ROS 2 nodes.
            #[param(default = "tcp/127.0.0.1:7447")]
            router: String,
            state: Option<SubState>,
        }

        impl $Sub {
            fn ensure(&mut self, ctx: &Ctx) -> Result {
                let want = Settings::new(ctx.id(), &self.topic, self.domain_id, &self.qos, &self.router)?;
                if self.state.as_ref().is_none_or(|s| s.settings != want) {
                    self.state = None;
                    self.state = Some(subscribe::<$ros, $ruro>(want, self.out.clone(), $from)?);
                }
                Ok(())
            }
        }

        impl Node for $Sub {
            fn start(&mut self, ctx: &mut Ctx) -> Result {
                self.ensure(ctx)
            }

            fn step(&mut self, ctx: &mut Ctx) -> Result {
                // Messages are sent from the subscription callback; steps
                // only pick up parameter changes.
                self.ensure(ctx)
            }

            fn stop(&mut self, _ctx: &mut Ctx) {
                self.state = None;
            }
        }

        #[doc = concat!("Publishes every input message on a ROS 2 `", $ros_name, "` topic.")]
        #[derive(Node)]
        #[node(kind = $pub_kind, doc = concat!("Publishes every input message on a ROS 2 `", $ros_name, "` topic."))]
        pub struct $Pub {
            #[input]
            input: Input<$ruro>,
            /// ROS 2 topic name.
            #[param(default = $topic)]
            topic: String,
            /// ROS_DOMAIN_ID; -1 uses the environment variable (default 0).
            #[param(default = -1, min = -1, max = 232)]
            domain_id: i64,
            /// `reliable` or `best_effort`.
            #[param(default = "reliable")]
            qos: String,
            /// Zenoh router shared with the ROS 2 nodes.
            #[param(default = "tcp/127.0.0.1:7447")]
            router: String,
            /// Frame for ROS messages whose ruro form carries none.
            #[param(default = "base_link")]
            frame_id: String,
            state: Option<PubState<$ros>>,
        }

        impl $Pub {
            fn ensure(&mut self, ctx: &Ctx) -> Result {
                let want = Settings::new(ctx.id(), &self.topic, self.domain_id, &self.qos, &self.router)?;
                if self.state.as_ref().is_none_or(|s| s.settings != want) {
                    self.state = None;
                    self.state = Some(advertise::<$ros>(want)?);
                }
                Ok(())
            }
        }

        impl Node for $Pub {
            fn start(&mut self, ctx: &mut Ctx) -> Result {
                self.ensure(ctx)
            }

            fn step(&mut self, ctx: &mut Ctx) -> Result {
                self.ensure(ctx)?;
                let state = self.state.as_ref().expect("publisher created");
                while let Some(sample) = self.input.recv() {
                    let pub_ctx = convert::PubContext {
                        frame_id: self.frame_id.clone(),
                        stamp_ns: sample.stamp_ns,
                    };
                    let msg = $to(&sample.msg, &pub_ctx)?;
                    state.publisher.publish(&msg)?;
                }
                Ok(())
            }

            fn stop(&mut self, _ctx: &mut Ctx) {
                self.state = None;
            }
        }
    };
}

use ruro_ros2::msgs::{geometry_msgs as gm, nav_msgs, sensor_msgs as sm, std_msgs, tf2_msgs};

bridge!(
    "ros2/sub-twist",
    SubTwist,
    "ros2/pub-twist",
    PubTwist,
    Twist,
    gm::msg::Twist,
    convert::twist_from_ros,
    convert::twist_to_ros,
    "/cmd_vel",
    "geometry_msgs/Twist"
);
bridge!(
    "ros2/sub-pose-stamped",
    SubPoseStamped,
    "ros2/pub-pose-stamped",
    PubPoseStamped,
    Pose,
    gm::msg::PoseStamped,
    convert::pose_stamped_from_ros,
    convert::pose_stamped_to_ros,
    "/pose",
    "geometry_msgs/PoseStamped"
);
bridge!(
    "ros2/sub-image",
    SubImage,
    "ros2/pub-image",
    PubImage,
    Image,
    sm::msg::Image,
    convert::image_from_ros,
    convert::image_to_ros,
    "/image_raw",
    "sensor_msgs/Image"
);
bridge!(
    "ros2/sub-camera-info",
    SubCameraInfo,
    "ros2/pub-camera-info",
    PubCameraInfo,
    CameraInfo,
    sm::msg::CameraInfo,
    convert::camera_info_from_ros,
    convert::camera_info_to_ros,
    "/camera_info",
    "sensor_msgs/CameraInfo"
);
bridge!(
    "ros2/sub-laser-scan",
    SubLaserScan,
    "ros2/pub-laser-scan",
    PubLaserScan,
    LaserScan,
    sm::msg::LaserScan,
    convert::laser_scan_from_ros,
    convert::laser_scan_to_ros,
    "/scan",
    "sensor_msgs/LaserScan"
);
bridge!(
    "ros2/sub-imu",
    SubImu,
    "ros2/pub-imu",
    PubImu,
    Imu,
    sm::msg::Imu,
    convert::imu_from_ros,
    convert::imu_to_ros,
    "/imu",
    "sensor_msgs/Imu"
);
bridge!(
    "ros2/sub-joint-state",
    SubJointState,
    "ros2/pub-joint-state",
    PubJointState,
    JointState,
    sm::msg::JointState,
    convert::joint_state_from_ros,
    convert::joint_state_to_ros,
    "/joint_states",
    "sensor_msgs/JointState"
);
bridge!(
    "ros2/sub-joy",
    SubJoy,
    "ros2/pub-joy",
    PubJoy,
    Joy,
    sm::msg::Joy,
    convert::joy_from_ros,
    convert::joy_to_ros,
    "/joy",
    "sensor_msgs/Joy"
);
bridge!(
    "ros2/sub-point-cloud2",
    SubPointCloud2,
    "ros2/pub-point-cloud2",
    PubPointCloud2,
    PointCloud,
    sm::msg::PointCloud2,
    convert::point_cloud2_from_ros,
    convert::point_cloud2_to_ros,
    "/points",
    "sensor_msgs/PointCloud2"
);
bridge!(
    "ros2/sub-odometry",
    SubOdometry,
    "ros2/pub-odometry",
    PubOdometry,
    Odometry,
    nav_msgs::msg::Odometry,
    convert::odometry_from_ros,
    convert::odometry_to_ros,
    "/odom",
    "nav_msgs/Odometry"
);
bridge!(
    "ros2/sub-tf",
    SubTf,
    "ros2/pub-tf",
    PubTf,
    Transforms,
    tf2_msgs::msg::TFMessage,
    convert::tf_from_ros,
    convert::tf_to_ros,
    "/tf",
    "tf2_msgs/TFMessage"
);
bridge!(
    "ros2/sub-float64",
    SubFloat64,
    "ros2/pub-float64",
    PubFloat64,
    Scalar,
    std_msgs::msg::Float64,
    convert::float64_from_ros,
    convert::float64_to_ros,
    "/value",
    "std_msgs/Float64"
);
bridge!(
    "ros2/sub-string",
    SubString,
    "ros2/pub-string",
    PubString,
    Text,
    std_msgs::msg::String,
    convert::string_from_ros,
    convert::string_to_ros,
    "/chatter",
    "std_msgs/String"
);
bridge!(
    "ros2/sub-bool",
    SubBool,
    "ros2/pub-bool",
    PubBool,
    Flag,
    std_msgs::msg::Bool,
    convert::bool_from_ros,
    convert::bool_to_ros,
    "/flag",
    "std_msgs/Bool"
);
