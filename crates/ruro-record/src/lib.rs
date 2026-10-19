//! Recording and replay of ruro graphs in [MCAP](https://mcap.dev).
//!
//! A recording has one channel per output port. The channel topic is
//! `node.port`, its schema is named after the message type (`std/Image`) and
//! each message holds exactly the bytes that would cross shared memory, so
//! replay feeds them back through the same decoder. Log time is the send
//! stamp in nanoseconds since the Unix epoch.
//!
//! Hosts write one segment per process; [`merge`] interleaves segments into a
//! single time-ordered file. [`Player`] streams a recording back with pause,
//! seek and rate control, and [`rosbag`] converts ROS 2 bags.

pub mod cdr;
mod player;
mod reader;
pub mod rosbag;
mod writer;

pub use player::{PlaybackControl, Player};
pub use reader::{ChannelInfo, Info, RecordedMessage, Recording, merge, merge_shifted};
pub use writer::McapWriter;

/// Message encoding of channels whose payload is postcard (`#[derive(Message)]` types).
pub const ENCODING_POSTCARD: &str = "ruro-postcard";
/// Message encoding of channels whose payload is a type's own binary layout
/// (for example `std/Image`).
pub const ENCODING_RAW: &str = "ruro-raw";
/// Schema encoding used for ruro message types. The schema data is the type name.
pub const SCHEMA_ENCODING: &str = "ruro";

/// Channel topic of an output port.
pub fn topic(node: &str, port: &str) -> String {
    format!("{node}.{port}")
}

/// Splits a channel topic into `(node, port)`.
pub fn split_topic(topic: &str) -> Option<(&str, &str)> {
    topic
        .split_once('.')
        .filter(|(n, p)| !n.is_empty() && !p.is_empty())
}

/// Channel message encoding for a ruro message type. The standard types with
/// a hand-written binary layout are `ruro-raw`; everything else (including
/// `#[derive(Message)]` types) is postcard. Either way the payload is decoded
/// with the type's `Message::decode`; the label is informational.
pub fn encoding_of(ty: &str) -> &'static str {
    match ty {
        "std/Image" | "std/PointCloud" => ENCODING_RAW,
        _ => ENCODING_POSTCARD,
    }
}

/// True if a channel was written by ruro (as opposed to, say, a ROS 2 bag).
pub fn is_ruro_encoding(encoding: &str) -> bool {
    encoding == ENCODING_POSTCARD || encoding == ENCODING_RAW
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topics() {
        assert_eq!(topic("cam", "image"), "cam.image");
        assert_eq!(split_topic("cam.image"), Some(("cam", "image")));
        assert_eq!(split_topic("/camera/image_raw"), None);
        assert_eq!(split_topic(".x"), None);
    }
}
