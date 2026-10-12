//! Message trait and the standard message vocabulary shared by every ruro node.
//!
//! A message has a stable type name (used to type-check wires), a compact binary
//! encoding (used when a wire crosses a process boundary) and a preview (used by
//! tooling to display live values). Inside a process messages are never encoded:
//! they travel as reference-counted values.

mod preview;
pub mod schema;
mod sensor_types;
mod std_schemas;
mod std_types;

pub use preview::{FramePreview, PREVIEW_MAX_POINTS, Preview, flatten_json, subsample_points};
pub use schema::{Describe, MessageSchema, Schema};
pub use sensor_types::*;
pub use std_types::*;

/// Error returned when a byte buffer cannot be decoded into a message.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("buffer too short: need {need} bytes, got {got}")]
    TooShort { need: usize, got: usize },
    #[error("invalid value: {0}")]
    Invalid(String),
    #[error(transparent)]
    Postcard(#[from] postcard::Error),
}

/// A value that can flow along a wire.
pub trait Message: Send + Sync + Sized + 'static {
    /// Stable, globally unique type name such as `std/Image`.
    const TYPE: &'static str;

    /// Exact number of bytes [`Message::encode_into`] will write.
    fn encoded_len(&self) -> usize;

    /// Writes the encoding into `buf`, which is exactly [`Message::encoded_len`] bytes long.
    fn encode_into(&self, buf: &mut [u8]);

    /// Reconstructs a message from bytes produced by [`Message::encode_into`].
    fn decode(buf: &[u8]) -> Result<Self, DecodeError>;

    /// A small, display-oriented summary of the message.
    fn preview(&self) -> Preview;

    /// Convenience helper that encodes into a fresh vector.
    fn to_bytes(&self) -> Vec<u8> {
        let mut buf = vec![0u8; self.encoded_len()];
        self.encode_into(&mut buf);
        buf
    }

    /// Layout of the encoding. Types that do not describe themselves report
    /// an opaque schema.
    fn schema() -> Schema {
        Schema::opaque(Self::TYPE)
    }

    /// The schema bundled with the type name and wire hash.
    fn message_schema() -> MessageSchema {
        MessageSchema::new(Self::TYPE, Self::schema())
    }
}

/// Implements [`Message`] for a `serde` type using the postcard wire format.
///
/// This is what `#[derive(Message)]` expands to; it can also be used directly.
/// Add a trailing `schema` argument when the type implements [`Describe`]: the
/// message then reports that schema and registers it for tooling.
///
/// ```ignore
/// impl_postcard_message!(Reading, "my/Reading", schema);
/// impl_postcard_message!(Reading, "my/Reading", preview: |r: &Reading| ..., schema);
/// ```
#[macro_export]
macro_rules! impl_postcard_message {
    ($ty:ty, $name:expr) => {
        $crate::impl_postcard_message!(@impl $ty, $name,
            |m: &$ty| $crate::Preview::from_serialize(m),
            || $crate::Schema::opaque($name));
    };
    ($ty:ty, $name:expr, schema) => {
        $crate::impl_postcard_message!(@impl $ty, $name,
            |m: &$ty| $crate::Preview::from_serialize(m),
            <$ty as $crate::Describe>::describe);
        $crate::register_schema!($ty);
    };
    ($ty:ty, $name:expr, preview: $preview:expr) => {
        $crate::impl_postcard_message!(@impl $ty, $name, $preview,
            || $crate::Schema::opaque($name));
    };
    ($ty:ty, $name:expr, preview: $preview:expr, schema) => {
        $crate::impl_postcard_message!(@impl $ty, $name, $preview,
            <$ty as $crate::Describe>::describe);
        $crate::register_schema!($ty);
    };
    (@impl $ty:ty, $name:expr, $preview:expr, $schema:expr) => {
        impl $crate::Message for $ty {
            const TYPE: &'static str = $name;

            fn encoded_len(&self) -> usize {
                $crate::__private::postcard::experimental::serialized_size(self)
                    .expect("message is serializable")
            }

            fn encode_into(&self, buf: &mut [u8]) {
                $crate::__private::postcard::to_slice(self, buf)
                    .expect("buffer sized by encoded_len");
            }

            fn decode(buf: &[u8]) -> ::core::result::Result<Self, $crate::DecodeError> {
                Ok($crate::__private::postcard::from_bytes(buf)?)
            }

            fn preview(&self) -> $crate::Preview {
                let f: fn(&$ty) -> $crate::Preview = $preview;
                f(self)
            }

            fn schema() -> $crate::Schema {
                let f: fn() -> $crate::Schema = $schema;
                f()
            }
        }
    };
}

/// Makes a message type's schema discoverable through
/// [`schema::registered`] in every binary that links the defining crate.
///
/// `impl_postcard_message!(T, name, schema)` and `#[derive(Message)]` do this
/// automatically; call it for hand-written [`Message`] impls.
#[macro_export]
macro_rules! register_schema {
    ($ty:ty) => {
        $crate::__private::inventory::submit! {
            $crate::schema::SchemaEntry {
                ty: <$ty as $crate::Message>::TYPE,
                schema: <$ty as $crate::Message>::schema,
            }
        }
    };
}

#[doc(hidden)]
pub mod __private {
    pub use inventory;
    pub use postcard;
    pub use serde_json;
}

/// Nanoseconds since the Unix epoch. Comparable across processes on one machine.
pub fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}
