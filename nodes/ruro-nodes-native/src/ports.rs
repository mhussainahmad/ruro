//! Ports of any standard message type, as seen from C.

use std::ffi::c_void;
use std::sync::Arc;

use anyhow::{Result, bail, ensure};
use ruro_msg::Message;
use ruro_node::{Builder, Input, Output, Sample};

use crate::abi::ruro_sample;
use crate::convert::{CMessage, Scratch};

macro_rules! ports {
    ($($t:ident),* $(,)?) => {
        enum Typed {
            $($t(Input<ruro_msg::$t>, Option<Arc<Sample<ruro_msg::$t>>>),)*
        }

        /// An input handed to C. The message last taken stays here, with the
        /// strings copied for it, until the next one is taken.
        pub struct CInput {
            typed: Typed,
            scratch: Scratch,
        }

        impl CInput {
            pub fn declare(b: &mut Builder, ty: &str, name: &str) -> Result<Self> {
                let typed = match ty {
                    $(<ruro_msg::$t as Message>::TYPE => Typed::$t(b.input(name)?, None),)*
                    other => bail!("C nodes do not support message type {other}"),
                };
                Ok(Self { typed, scratch: Scratch::default() })
            }

            pub fn queued(&self) -> usize {
                match &self.typed {
                    $(Typed::$t(i, _) => i.len(),)*
                }
            }

            /// Takes the oldest (or newest) message into `view`; returns
            /// whether there was one.
            ///
            /// # Safety
            /// `view` must point to the C struct of the input's type, and
            /// `sample` be NULL or valid.
            pub unsafe fn take(&mut self, latest: bool, view: *mut c_void, sample: *mut ruro_sample) -> bool {
                let scratch = &mut self.scratch;
                match &mut self.typed {
                    $(Typed::$t(input, current) => {
                        let next = if latest { input.latest() } else { input.recv() };
                        let Some(s) = next else { return false };
                        scratch.clear();
                        let v = s.msg.view(scratch);
                        if !sample.is_null() {
                            // SAFETY: the caller's contract.
                            unsafe { *sample = ruro_sample { seq: s.seq, stamp_ns: s.stamp_ns } };
                        }
                        // SAFETY: the caller's contract.
                        unsafe { *view.cast::<<ruro_msg::$t as CMessage>::C>() = v };
                        // Keeps what `v` points into alive.
                        *current = Some(s);
                        true
                    })*
                }
            }
        }

        /// An output handed to C.
        pub enum COutput {
            $($t(Output<ruro_msg::$t>),)*
        }

        impl COutput {
            pub fn declare(b: &mut Builder, ty: &str, name: &str) -> Result<Self> {
                Ok(match ty {
                    $(<ruro_msg::$t as Message>::TYPE => Self::$t(b.output(name)?),)*
                    other => bail!("C nodes do not support message type {other}"),
                })
            }

            /// # Safety
            /// `msg` must point to the C struct of the output's type, whose
            /// pointers are valid for their lengths.
            pub unsafe fn send(&self, msg: *const c_void) -> Result<()> {
                ensure!(!msg.is_null(), "send: the message is NULL");
                match self {
                    $(Self::$t(o) => {
                        // SAFETY: the caller's contract.
                        let c = unsafe { &*msg.cast::<<ruro_msg::$t as CMessage>::C>() };
                        // SAFETY: the caller's contract.
                        let m = unsafe { <ruro_msg::$t as CMessage>::from_c(c)? };
                        o.send(m)
                    })*
                }
            }
        }

        /// Message types C nodes can use.
        pub const SUPPORTED: &[&str] = &[$(<ruro_msg::$t as Message>::TYPE),*];
    };
}
ruro_msg::for_each_std_message! { ports }
