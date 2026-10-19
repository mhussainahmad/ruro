//! ROS 2 interoperability over Zenoh, without a ROS installation.
//!
//! - [`idl`] parses `.msg`/`.srv` definitions; [`codegen`] turns them into
//!   Rust types with [`cdr`] encoding (XCDR1 little endian, as every ROS 2
//!   middleware including rmw_zenoh puts on the wire).
//! - [`hash`] computes RIHS01 type hashes (REP-2011).
//! - [`msgs`] holds generated types for `builtin_interfaces`, `std_msgs`,
//!   `geometry_msgs`, `sensor_msgs`, `nav_msgs` and `tf2_msgs`, whose
//!   definitions are bundled in `interfaces/` (see `interfaces/NOTICE.md`).
//! - [`rmw`] publishes and subscribes the way rmw_zenoh does: the same key
//!   expressions, attachments and liveliness tokens, so ROS 2 nodes using
//!   `RMW_IMPLEMENTATION=rmw_zenoh_cpp` see ruro as just another node.
//! - [`discovery`] lists the topics of a ROS 2 system from its liveliness
//!   tokens.
//! - [`dynamic`] encodes and decodes messages of any type at run time from
//!   their definitions, which [`search`] finds in `AMENT_PREFIX_PATH`, in
//!   user directories or among the bundled ones.

pub mod cdr;
pub mod cli;
pub mod codegen;
pub mod discovery;
pub mod dynamic;
pub mod hash;
pub mod idl;
pub mod idl_file;
#[rustfmt::skip]
pub mod msgs;
pub mod rmw;
pub mod search;
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

/// The bundled interface definitions, parsed.
pub fn bundled_interfaces() -> idl::Interfaces {
    let mut i = idl::Interfaces::default();
    for (pkg, msg, text) in bundled::BUNDLED {
        let spec = idl::parse_message(idl::TypeName::msg(*pkg, *msg), text, msg)
            .expect("bundled definitions parse");
        i.add(spec);
    }
    i
}

/// Package names of the bundled definitions.
pub fn bundled_packages() -> Vec<String> {
    let mut p: Vec<String> = bundled::BUNDLED
        .iter()
        .map(|(pkg, _, _)| (*pkg).to_owned())
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
