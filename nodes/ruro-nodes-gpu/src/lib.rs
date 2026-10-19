//! GPU nodes (`gpu/*`): move images into CUDA memory, process them there
//! and bring results back.
//!
//! The CUDA driver is loaded when a node starts; without it (or without a
//! GPU) the node fails to start with a message saying so. See the `ruro-gpu`
//! crate for the `gpu/Image` and `gpu/Tensor` messages.

use ruro::prelude::*;
use ruro_gpu::{GpuImage, Normalize as NormalizeParams, Stream, Tensor};

/// A stream on `device`, or an error explaining why CUDA cannot be used.
fn open_stream(device: u32) -> Result<Stream> {
    let cuda = ruro_gpu::cuda()?;
    let stream = Stream::new(device)?;
    info!(
        "using CUDA device {device}: {}",
        cuda.device_name(device).unwrap_or("?")
    );
    Ok(stream)
}

fn stream(s: &Option<Stream>) -> Result<&Stream> {
    s.as_ref().context("node not started")
}

/// Copies images into GPU memory.
#[derive(Node)]
#[node(kind = "gpu/upload")]
pub struct Upload {
    #[input]
    image: Input<Image>,
    #[output]
    out: Output<GpuImage>,
    /// CUDA device ordinal.
    #[param(default = 0, min = 0, max = 15)]
    device: u32,
    stream: Option<Stream>,
}

impl Node for Upload {
    fn start(&mut self, _ctx: &mut Ctx) -> Result {
        self.stream = Some(open_stream(self.device)?);
        Ok(())
    }

    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        let stream = stream(&self.stream)?;
        while let Some(img) = self.image.recv() {
            // A pooled frame: its device buffer is reused once every
            // receiver, in this process or another, has let go of it.
            let mut out = self.out.loan();
            out.upload(self.device, &img, stream)?;
            out.send()?;
        }
        Ok(())
    }
}

/// Copies GPU images back into host memory.
#[derive(Node)]
#[node(kind = "gpu/download")]
pub struct Download {
    #[input]
    image: Input<GpuImage>,
    #[output]
    out: Output<Image>,
    stream: Option<Stream>,
}

impl Node for Download {
    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        while let Some(g) = self.image.recv() {
            let Some(device) = g.device() else {
                continue;
            };
            if self.stream.as_ref().is_none_or(|s| s.device() != device) {
                self.stream = Some(open_stream(device)?);
            }
            let mut img = self.out.loan();
            g.download_into(&mut img, stream(&self.stream)?)?;
            drop(g);
            img.send()?;
        }
        Ok(())
    }
}

fn triple(s: &str, what: &str) -> Result<[f32; 3]> {
    let v: Vec<f32> = s
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| {
            p.parse::<f32>()
                .map_err(|_| anyhow!("{what}: not a number: `{p}`"))
        })
        .collect::<Result<_>>()?;
    v.try_into()
        .map_err(|_| anyhow!("{what} needs three comma separated values"))
}

/// Turns a GPU image into a float32 `[1, C, H, W]` tensor on the same GPU,
/// ready for inference: resized (nearest neighbour), scaled to 0..1, then
/// `(v - mean) / std` per channel. C is 3 for colour images (alpha is
/// dropped) and 1 for mono. Runs a PTX kernel; the pixels never leave the GPU.
#[derive(Node)]
#[node(kind = "gpu/normalize")]
pub struct Normalize {
    #[input]
    image: Input<GpuImage>,
    #[output]
    tensor: Output<Tensor>,
    /// Output width; 0 keeps the image's.
    #[param(default = 0, min = 0, max = 8192)]
    width: u32,
    /// Output height; 0 keeps the image's.
    #[param(default = 0, min = 0, max = 8192)]
    height: u32,
    /// Per-channel mean, applied after scaling pixels to 0..1.
    #[param(default = "0,0,0")]
    mean: String,
    /// Per-channel standard deviation, applied after the mean.
    #[param(default = "1,1,1")]
    std: String,
    /// Output channels in BGR order.
    #[param(default = false)]
    bgr: bool,
    stream: Option<Stream>,
}

impl Normalize {
    fn params(&self) -> Result<NormalizeParams> {
        ensure!(
            (self.width == 0) == (self.height == 0),
            "set both width and height, or neither"
        );
        Ok(NormalizeParams {
            width: self.width,
            height: self.height,
            mean: triple(&self.mean, "mean")?,
            std: triple(&self.std, "std")?,
            bgr: self.bgr,
        })
    }
}

impl Node for Normalize {
    fn start(&mut self, _ctx: &mut Ctx) -> Result {
        self.params()?;
        Ok(())
    }

    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        let Some(g) = self.image.latest() else {
            return Ok(());
        };
        let Some(device) = g.device() else {
            return Ok(());
        };
        if self.stream.as_ref().is_none_or(|s| s.device() != device) {
            self.stream = Some(open_stream(device)?);
        }
        let p = self.params()?;
        let mut t = self.tensor.loan();
        ruro_gpu::normalize(&g, &mut t, &p, stream(&self.stream)?)?;
        drop(g);
        t.send()
    }
}

/// Copies a float32 tensor (from the GPU or the host) into an `ml/Tensor`,
/// for nodes that run on the CPU.
#[derive(Node)]
#[node(kind = "gpu/tensor-to-cpu")]
pub struct TensorToCpu {
    #[input]
    tensor: Input<Tensor>,
    #[output]
    out: Output<ruro_nodes_ml::Tensor>,
}

impl Node for TensorToCpu {
    fn step(&mut self, _ctx: &mut Ctx) -> Result {
        while let Some(t) = self.tensor.recv() {
            ensure!(t.is_contiguous(), "tensor is not contiguous");
            let shape = t
                .shape
                .iter()
                .map(|&d| u32::try_from(d).context("dimension too large"))
                .collect::<Result<Vec<u32>>>()?;
            let data = t.to_f32_vec()?;
            drop(t);
            self.out.send(ruro_nodes_ml::Tensor::new(shape, data))?;
        }
        Ok(())
    }
}

/// Forces this crate to be linked. Hosts call it so the node registrations
/// survive dead-code elimination.
pub fn link() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triples_parse() {
        assert_eq!(triple("0.5, 1,2", "mean").unwrap(), [0.5, 1.0, 2.0]);
        assert!(triple("1,2", "mean").is_err());
        assert!(triple("a,b,c", "mean").is_err());
    }
}
