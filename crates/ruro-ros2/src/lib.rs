//! ROS 2 interoperability over Zenoh, without a ROS installation.
//!
//! - [`idl`] parses `.msg`/`.srv`/`.action` definitions; [`codegen`] turns
//!   them into Rust types with [`cdr`] encoding (XCDR1 little endian, as
//!   every ROS 2 middleware including rmw_zenoh puts on the wire).
//! - [`hash`] computes RIHS01 type hashes (REP-2011).
//! - [`msgs`] holds generated types for `builtin_interfaces`, `std_msgs`,
//!   `std_srvs`, `geometry_msgs`, `sensor_msgs`, `nav_msgs`, `tf2_msgs`,
//!   `action_msgs`, `unique_identifier_msgs`, `service_msgs` and
//!   `example_interfaces`, whose definitions are bundled in `interfaces/`
//!   (see `interfaces/NOTICE.md`).
//! - [`rmw`] publishes and subscribes the way rmw_zenoh does: the same key
//!   expressions, attachments and liveliness tokens, so ROS 2 nodes using
//!   `RMW_IMPLEMENTATION=rmw_zenoh_cpp` see ruro as just another node.
//! - [`discovery`] lists the topics of a ROS 2 system from its liveliness
//!   tokens.

pub mod action;
pub mod cdr;
pub mod cli;
pub mod codegen;
pub mod discovery;
pub mod hash;
pub mod idl;
#[rustfmt::skip]
pub mod msgs;
pub mod rmw;
pub mod time;

pub use discovery::{TopicInfo, bridge_kinds, list_topics};
pub use rmw::{Endpoint, Publisher, Qos, Reliability, Subscriber};

mod bundled {
    include!(concat!(env!("OUT_DIR"), "/bundled.rs"));
}

/// A generated ROS 2 message type.
pub trait RosMessage:
    cdr::CdrEncode + cdr::CdrDecode + Default + Clone + Send + Sync + 'static
{
    /// e.g. `std_msgs/msg/String`.
    const TYPE_NAME: &'static str;
    /// e.g. `std_msgs::msg::dds_::String_`.
    const DDS_TYPE_NAME: &'static str;
    /// RIHS01 hash of the type description.
    const TYPE_HASH: &'static str;
}

/// A generated ROS 2 service type: a marker naming its request and
/// response messages.
pub trait RosService: Send + Sync + 'static {
    /// e.g. `example_interfaces/srv/AddTwoInts`.
    const TYPE_NAME: &'static str;
    /// e.g. `example_interfaces::srv::dds_::AddTwoInts_`.
    const DDS_TYPE_NAME: &'static str;
    /// RIHS01 hash of the service type description.
    const TYPE_HASH: &'static str;
    type Request: RosMessage;
    type Response: RosMessage;
}

/// The 16 bytes of a `unique_identifier_msgs/UUID` naming an action goal.
pub type GoalId = [u8; 16];

/// A generated ROS 2 action type: a marker naming its messages and
/// services, with the glue to build and split the messages the action
/// protocol wraps goals, results and feedback in. See [`action`].
pub trait RosAction: Send + Sync + 'static {
    /// e.g. `example_interfaces/action/Fibonacci`.
    const TYPE_NAME: &'static str;
    /// RIHS01 hash of the action type description.
    const TYPE_HASH: &'static str;
    type Goal: RosMessage;
    type Result: RosMessage;
    type Feedback: RosMessage;
    /// `<Action>_SendGoal`.
    type SendGoal: RosService;
    /// `<Action>_GetResult`.
    type GetResult: RosService;
    /// `<Action>_FeedbackMessage`.
    type FeedbackMessage: RosMessage;

    fn send_goal_request(
        goal_id: GoalId,
        goal: Self::Goal,
    ) -> <Self::SendGoal as RosService>::Request;
    fn split_send_goal_request(
        request: <Self::SendGoal as RosService>::Request,
    ) -> (GoalId, Self::Goal);
    fn send_goal_response(
        accepted: bool,
        stamp: msgs::builtin_interfaces::msg::Time,
    ) -> <Self::SendGoal as RosService>::Response;
    fn split_send_goal_response(
        response: <Self::SendGoal as RosService>::Response,
    ) -> (bool, msgs::builtin_interfaces::msg::Time);
    fn get_result_request(goal_id: GoalId) -> <Self::GetResult as RosService>::Request;
    fn split_get_result_request(request: <Self::GetResult as RosService>::Request) -> GoalId;
    fn get_result_response(
        status: i8,
        result: Self::Result,
    ) -> <Self::GetResult as RosService>::Response;
    fn split_get_result_response(
        response: <Self::GetResult as RosService>::Response,
    ) -> (i8, Self::Result);
    fn feedback_message(goal_id: GoalId, feedback: Self::Feedback) -> Self::FeedbackMessage;
    fn split_feedback_message(message: Self::FeedbackMessage) -> (GoalId, Self::Feedback);
}

/// The bundled interface definitions, parsed.
pub fn bundled_interfaces() -> idl::Interfaces {
    let mut i = idl::Interfaces::default();
    for (pkg, kind, name, text) in bundled::BUNDLED {
        i.add_text(pkg, kind, name, text, name)
            .expect("bundled definitions parse");
    }
    i
}

/// Package names of the bundled definitions.
pub fn bundled_packages() -> Vec<String> {
    let mut p: Vec<String> = bundled::BUNDLED
        .iter()
        .map(|(pkg, _, _, _)| (*pkg).to_owned())
        .collect();
    p.dedup();
    p
}

/// Code generation options for the bundled packages, as used for [`msgs`].
pub fn bundled_codegen_options() -> codegen::Options {
    codegen::Options {
        packages: bundled_packages(),
        root: "crate::msgs".into(),
        runtime: "crate".into(),
        externs: Default::default(),
    }
}
