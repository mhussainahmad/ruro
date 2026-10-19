//! GPU buffers for ruro: images and tensors in CUDA device memory, passed
//! between nodes without copying.
//!
//! - [`GpuImage`] (`gpu/Image`) and [`Tensor`] (`gpu/Tensor`) hold a
//!   [`DeviceBuffer`]. In a process, receivers share it by reference count,
//!   like every ruro message. Between process groups on one machine, only a
//!   CUDA IPC descriptor travels through shared memory and the receiver maps
//!   the same device memory ([`ipc`] explains how reuse is kept safe).
//! - Buffers come from a per-device pool, so producing a frame per step does
//!   not allocate once warm.
//! - The CUDA driver (`libcuda.so.1`) is loaded at run time: building needs
//!   no CUDA toolkit, and without a GPU only using these types fails, with a
//!   clear error. Kernels are PTX compiled by the driver ([`normalize`]).
//!
//! Previews (the studio's watch panel, `ruro echo`) download a downscaled
//! copy only while someone watches.

mod driver;
mod image;
pub mod ipc;
mod kernel;
mod memory;
mod tensor;

pub use driver::{Cuda, LIB_ENV, cuda, unavailable};
pub use image::GpuImage;
pub use kernel::{Normalize, normalize};
pub use memory::{DeviceBuffer, Event, PinnedBuffer, PoolStats, Stream, pool_stats};
pub use tensor::{DType, Device, Tensor, contiguous_strides};
