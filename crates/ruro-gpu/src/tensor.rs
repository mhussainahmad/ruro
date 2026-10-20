//! `gpu/Tensor`: an n-dimensional array in host or GPU memory.

use std::sync::Arc;

use anyhow::{Result, ensure};
use ruro_msg::schema::{Describe, Field, Schema, Variant, VariantPayload};
use ruro_msg::{DecodeError, Message, Preview};

use crate::image::{NO_BUFFER, decode_buffer, encode_buffer};
use crate::ipc::DESCRIPTOR_LEN;
use crate::memory::{DeviceBuffer, Stream, writable_buffer};

/// Element type of a [`Tensor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum DType {
    U8 = 0,
    I32 = 1,
    I64 = 2,
    F16 = 3,
    #[default]
    F32 = 4,
    F64 = 5,
}

impl DType {
    /// Bytes per element.
    pub fn size(self) -> usize {
        match self {
            DType::U8 => 1,
            DType::F16 => 2,
            DType::I32 | DType::F32 => 4,
            DType::I64 | DType::F64 => 8,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            DType::U8 => "u8",
            DType::I32 => "i32",
            DType::I64 => "i64",
            DType::F16 => "f16",
            DType::F32 => "f32",
            DType::F64 => "f64",
        }
    }

    fn from_u8(v: u8) -> Result<DType, DecodeError> {
        Ok(match v {
            0 => DType::U8,
            1 => DType::I32,
            2 => DType::I64,
            3 => DType::F16,
            4 => DType::F32,
            5 => DType::F64,
            other => return Err(DecodeError::Invalid(format!("unknown dtype {other}"))),
        })
    }
}

/// Where a tensor's elements live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Device {
    #[default]
    Cpu,
    /// CUDA device by ordinal.
    Cuda(u32),
}

impl std::fmt::Display for Device {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Device::Cpu => f.write_str("cpu"),
            Device::Cuda(n) => write!(f, "cuda:{n}"),
        }
    }
}

#[derive(Clone)]
enum Storage {
    Host(Vec<u8>),
    Device(Option<Arc<DeviceBuffer>>),
}

/// An n-dimensional array of one element type, in host or GPU memory.
///
/// Strides are in elements. Device tensors share their buffer the way
/// [`crate::GpuImage`] does: by reference within a process, by CUDA IPC
/// descriptor across process groups.
#[derive(Clone)]
pub struct Tensor {
    pub dtype: DType,
    pub shape: Vec<u64>,
    pub strides: Vec<i64>,
    storage: Storage,
}

impl Default for Tensor {
    fn default() -> Self {
        Tensor {
            dtype: DType::F32,
            shape: vec![0],
            strides: vec![1],
            storage: Storage::Host(Vec::new()),
        }
    }
}

impl std::fmt::Debug for Tensor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tensor")
            .field("dtype", &self.dtype)
            .field("shape", &self.shape)
            .field("strides", &self.strides)
            .field("device", &self.device())
            .finish()
    }
}

/// Row-major strides of `shape`, in elements.
pub fn contiguous_strides(shape: &[u64]) -> Vec<i64> {
    let mut strides = vec![1i64; shape.len()];
    for i in (0..shape.len().saturating_sub(1)).rev() {
        strides[i] = strides[i + 1] * shape[i + 1] as i64;
    }
    strides
}

fn numel_of(shape: &[u64]) -> Result<usize> {
    shape
        .iter()
        .try_fold(1usize, |n, &d| n.checked_mul(d as usize))
        .ok_or_else(|| anyhow::anyhow!("tensor shape {shape:?} overflows"))
}

impl Tensor {
    /// A contiguous host tensor from raw little-endian element bytes.
    pub fn from_host(dtype: DType, shape: Vec<u64>, bytes: Vec<u8>) -> Result<Tensor> {
        let n = numel_of(&shape)?;
        ensure!(
            bytes.len() == n * dtype.size(),
            "{} bytes do not hold {shape:?} {} elements",
            bytes.len(),
            dtype.name()
        );
        Ok(Tensor {
            dtype,
            strides: contiguous_strides(&shape),
            shape,
            storage: Storage::Host(bytes),
        })
    }

    /// A contiguous host tensor of `f32`s.
    pub fn from_f32(shape: Vec<u64>, data: &[f32]) -> Result<Tensor> {
        let bytes = data.iter().flat_map(|v| v.to_le_bytes()).collect();
        Tensor::from_host(DType::F32, shape, bytes)
    }

    /// A contiguous tensor on `device` with unspecified contents.
    pub fn on_device(device: u32, dtype: DType, shape: Vec<u64>) -> Result<Tensor> {
        let mut t = Tensor::default();
        t.reshape_on(device, dtype, shape)?;
        Ok(t)
    }

    /// Makes this a contiguous device tensor of the given type and shape
    /// whose buffer may be written, keeping the current buffer if nobody
    /// else holds it and it is large enough (see `GpuImage::reshape`).
    pub fn reshape_on(&mut self, device: u32, dtype: DType, shape: Vec<u64>) -> Result<()> {
        let len = numel_of(&shape)?
            .checked_mul(dtype.size())
            .ok_or_else(|| anyhow::anyhow!("tensor too large"))?;
        let mut slot = match std::mem::replace(&mut self.storage, Storage::Device(None)) {
            Storage::Device(b) => b,
            Storage::Host(_) => None,
        };
        let result = writable_buffer(&mut slot, device, len).map(|_| ());
        self.storage = Storage::Device(slot);
        result?;
        self.dtype = dtype;
        self.strides = contiguous_strides(&shape);
        self.shape = shape;
        Ok(())
    }

    pub fn device(&self) -> Device {
        match &self.storage {
            Storage::Host(_) => Device::Cpu,
            Storage::Device(Some(b)) => Device::Cuda(b.device()),
            Storage::Device(None) => Device::Cpu,
        }
    }

    pub fn numel(&self) -> usize {
        numel_of(&self.shape).unwrap_or(0)
    }

    pub fn byte_len(&self) -> usize {
        self.numel() * self.dtype.size()
    }

    pub fn is_contiguous(&self) -> bool {
        self.strides == contiguous_strides(&self.shape)
    }

    /// Element bytes of a host tensor.
    pub fn host_bytes(&self) -> Option<&[u8]> {
        match &self.storage {
            Storage::Host(b) => Some(b),
            Storage::Device(_) => None,
        }
    }

    /// The buffer of a device tensor.
    pub fn buffer(&self) -> Option<&DeviceBuffer> {
        match &self.storage {
            Storage::Device(b) => b.as_deref(),
            Storage::Host(_) => None,
        }
    }

    /// The buffer of a device tensor for writing; `None` if shared.
    pub fn buffer_mut(&mut self) -> Option<&mut DeviceBuffer> {
        match &mut self.storage {
            Storage::Device(b) => b.as_mut().and_then(Arc::get_mut),
            Storage::Host(_) => None,
        }
    }

    /// A host copy (or a clone of a host tensor).
    pub fn to_host(&self) -> Result<Tensor> {
        let Some(buf) = self.buffer() else {
            return Ok(self.clone());
        };
        let mut bytes = vec![0u8; self.byte_len()];
        buf.download(&mut bytes, &Stream::default_for(buf.device()))?;
        Ok(Tensor {
            dtype: self.dtype,
            shape: self.shape.clone(),
            strides: self.strides.clone(),
            storage: Storage::Host(bytes),
        })
    }

    /// A copy on `device` (or a clone if it is already there).
    pub fn to_device(&self, device: u32) -> Result<Tensor> {
        if self.device() == Device::Cuda(device) {
            return Ok(self.clone());
        }
        let host = self.to_host()?;
        let mut t = Tensor::on_device(device, self.dtype, self.shape.clone())?;
        t.strides = self.strides.clone();
        let stream = Stream::default_for(device);
        t.buffer_mut()
            .expect("fresh buffer")
            .upload(host.host_bytes().expect("host tensor"), &stream)?;
        Ok(t)
    }

    /// The elements of an `f32` tensor, in memory order.
    pub fn to_f32_vec(&self) -> Result<Vec<f32>> {
        ensure!(
            self.dtype == DType::F32,
            "tensor is {}, not f32",
            self.dtype.name()
        );
        let host = self.to_host()?;
        let bytes = host.host_bytes().expect("host tensor");
        Ok(bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect())
    }

    fn try_preview(&self) -> Result<Preview> {
        let what = format!(
            "tensor {} {:?} on {}",
            self.dtype.name(),
            self.shape,
            self.device()
        );
        // Statistics only for float32 tensors of reasonable size.
        if self.dtype != DType::F32 || self.byte_len() > 64 << 20 {
            return Ok(Preview::Text(what));
        }
        let data = self.to_f32_vec()?;
        if data.is_empty() {
            return Ok(Preview::Text(what));
        }
        let (min, max, sum) = data
            .iter()
            .fold((f32::INFINITY, f32::NEG_INFINITY, 0f64), |(a, b, s), &v| {
                (a.min(v), b.max(v), s + v as f64)
            });
        let mean = sum / data.len() as f64;
        Ok(Preview::Text(format!(
            "{what}, values in [{min}, {max}], mean {mean:.4}"
        )))
    }
}

const VERSION: u8 = 1;
const HOST: u8 = 3;

impl Message for Tensor {
    const TYPE: &'static str = "gpu/Tensor";

    fn schema() -> Schema {
        <Self as Describe>::describe()
    }

    fn encoded_len(&self) -> usize {
        let dims = 3 + 16 * self.shape.len();
        dims + 1
            + match &self.storage {
                Storage::Host(b) => 8 + b.len(),
                Storage::Device(Some(_)) => DESCRIPTOR_LEN,
                Storage::Device(None) => 0,
            }
    }

    fn encode_into(&self, buf: &mut [u8]) {
        assert!(self.shape.len() <= u8::MAX as usize && self.strides.len() == self.shape.len());
        buf[0] = VERSION;
        buf[1] = self.dtype as u8;
        buf[2] = self.shape.len() as u8;
        let mut i = 3;
        for (&d, &s) in self.shape.iter().zip(&self.strides) {
            buf[i..i + 8].copy_from_slice(&d.to_le_bytes());
            buf[i + 8..i + 16].copy_from_slice(&s.to_le_bytes());
            i += 16;
        }
        match &self.storage {
            Storage::Host(b) => {
                buf[i] = HOST;
                buf[i + 1..i + 9].copy_from_slice(&(b.len() as u64).to_le_bytes());
                buf[i + 9..].copy_from_slice(b);
            }
            Storage::Device(Some(b)) => encode_buffer(b, &mut buf[i..]),
            Storage::Device(None) => buf[i] = NO_BUFFER,
        }
    }

    fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        let short = |need: usize| DecodeError::TooShort {
            need,
            got: buf.len(),
        };
        if buf.len() < 4 {
            return Err(short(4));
        }
        if buf[0] != VERSION {
            return Err(DecodeError::Invalid(format!(
                "unknown gpu/Tensor version {}",
                buf[0]
            )));
        }
        let dtype = DType::from_u8(buf[1])?;
        let ndim = buf[2] as usize;
        let mut i = 3;
        if buf.len() < i + 16 * ndim + 1 {
            return Err(short(i + 16 * ndim + 1));
        }
        let (mut shape, mut strides) = (Vec::with_capacity(ndim), Vec::with_capacity(ndim));
        for _ in 0..ndim {
            shape.push(u64::from_le_bytes(buf[i..i + 8].try_into().unwrap()));
            strides.push(i64::from_le_bytes(buf[i + 8..i + 16].try_into().unwrap()));
            i += 16;
        }
        let invalid = |e: anyhow::Error| DecodeError::Invalid(format!("{e:#}"));
        let need = numel_of(&shape)
            .ok()
            .and_then(|n| n.checked_mul(dtype.size()))
            .ok_or_else(|| DecodeError::Invalid(format!("tensor shape {shape:?} overflows")))?;
        let flag = buf[i];
        let storage = match flag {
            HOST => {
                if buf.len() < i + 9 {
                    return Err(short(i + 9));
                }
                let len = u64::from_le_bytes(buf[i + 1..i + 9].try_into().unwrap()) as usize;
                let end = (i + 9)
                    .checked_add(len)
                    .ok_or_else(|| DecodeError::Invalid(format!("tensor of {len} bytes")))?;
                if buf.len() != end {
                    return Err(short(end));
                }
                if len != need {
                    return Err(invalid(anyhow::anyhow!(
                        "{len} bytes do not match shape {shape:?}"
                    )));
                }
                Storage::Host(buf[i + 9..].to_vec())
            }
            NO_BUFFER => Storage::Device(None),
            _ => {
                if buf.len() < i + 1 + DESCRIPTOR_LEN {
                    return Err(short(i + 1 + DESCRIPTOR_LEN));
                }
                let b = decode_buffer(flag, &buf[i + 1..])?;
                if b.len() < need {
                    return Err(DecodeError::Invalid(
                        "tensor is larger than its buffer".into(),
                    ));
                }
                Storage::Device(Some(Arc::new(b)))
            }
        };
        Ok(Tensor {
            dtype,
            shape,
            strides,
            storage,
        })
    }

    fn preview(&self) -> Preview {
        self.try_preview()
            .unwrap_or_else(|e| Preview::Text(format!("tensor {:?}: {e:#}", self.shape)))
    }
}

impl Describe for Tensor {
    fn describe() -> Schema {
        let unit = |name: &str| Variant {
            name: name.into(),
            payload: VariantPayload::Unit,
        };
        Schema::Custom {
            name: "Tensor".into(),
            encoding: "ruro/gpu-tensor-v1: u8 version (1), u8 dtype (0 u8, 1 i32, 2 i64, \
                       3 f16, 4 f32, 5 f64), u8 ndim, ndim pairs of u64 LE size and i64 LE \
                       stride (elements), u8 storage (3 host: u64 LE length then the \
                       elements, little endian; 0 no buffer; 1 shared and 2 export failed: \
                       a 128-byte CUDA IPC descriptor valid only on the sending machine)"
                .into(),
            fields: vec![
                Field::new(
                    "dtype",
                    Schema::Enum {
                        name: "DType".into(),
                        variants: ["U8", "I32", "I64", "F16", "F32", "F64"]
                            .into_iter()
                            .map(unit)
                            .collect(),
                    },
                ),
                Field::new("shape", Schema::seq(Schema::U64)),
                Field::new("strides", Schema::seq(Schema::I64)).doc("In elements."),
                Field::new("data", Schema::seq(Schema::U8))
                    .doc("Host elements, or device memory described by its CUDA IPC handle."),
            ],
        }
    }
}

ruro_msg::register_schema!(Tensor);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_tensors_roundtrip() {
        let t = Tensor::from_f32(vec![2, 3], &[0.0, 1.0, 2.0, 3.0, 4.0, -5.5]).unwrap();
        assert_eq!(t.strides, vec![3, 1]);
        assert!(t.is_contiguous());
        let back = Tensor::decode(&t.to_bytes()).unwrap();
        assert_eq!(back.shape, t.shape);
        assert_eq!(back.device(), Device::Cpu);
        assert_eq!(back.to_f32_vec().unwrap(), t.to_f32_vec().unwrap());
        let Preview::Text(p) = t.preview() else {
            panic!()
        };
        assert!(p.contains("[-5.5, 4]") && p.contains("cpu"), "{p}");
    }

    #[test]
    fn bad_encodings_are_errors() {
        let t = Tensor::from_f32(vec![4], &[1.0; 4]).unwrap();
        let bytes = t.to_bytes();
        assert!(Tensor::decode(&bytes[..bytes.len() - 1]).is_err());
        let mut wrong = bytes.clone();
        wrong[1] = 99;
        assert!(Tensor::decode(&wrong).is_err());
        assert!(Tensor::from_host(DType::F32, vec![3], vec![0; 8]).is_err());
        // A host length that would overflow: the 8 bytes before the data.
        let mut huge = bytes.clone();
        let at = bytes.len() - 16 - 8;
        huge[at..at + 8].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(Tensor::decode(&huge).is_err());
    }

    #[test]
    fn strides_are_row_major() {
        assert_eq!(contiguous_strides(&[1, 3, 4, 5]), vec![60, 20, 5, 1]);
        assert!(contiguous_strides(&[]).is_empty());
    }
}
