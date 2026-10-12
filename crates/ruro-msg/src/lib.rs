//! Message trait and the standard message vocabulary shared by every ruro node.
//!
//! A message has a stable type name (used to type-check wires), a compact binary
//! encoding (used when a wire crosses a process boundary) and a preview (used by
//! tooling to display live values). Inside a process messages are never encoded:
//! they travel as reference-counted values.

mod preview;
mod sensor_types;
mod std_types;

pub use preview::{Preview, flatten_json};
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
}

/// Implements [`Message`] for a `serde` type using the postcard wire format.
///
/// This is what `#[derive(Message)]` expands to; it can also be used directly.
#[macro_export]
macro_rules! impl_postcard_message {
    ($ty:ty, $name:expr) => {
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
                $crate::Preview::from_serialize(self)
            }
        }
    };
}

#[doc(hidden)]
pub mod __private {
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
