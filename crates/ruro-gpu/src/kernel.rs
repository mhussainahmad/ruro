//! Device kernels, written in PTX and compiled by the driver at run time
//! (there is no `nvcc` in the build).

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::OnceLock;

use anyhow::{Result, ensure};
use parking_lot::Mutex;
use ruro_msg::Encoding;

use crate::driver::{CuFunction, cuda};
use crate::image::GpuImage;
use crate::memory::Stream;
use crate::tensor::{DType, Tensor};

/// `ruro_normalize`: 8-bit interleaved image to float32 planar (NCHW), with
/// nearest-neighbour resizing, optional red/blue swap and per-channel
/// `value * scale + bias`. One thread per output pixel.
///
/// Parameters: src, dst, src width, height, step, channels, dst width,
/// height, channels (1 or 3), swap, scale[3], bias[3]. A mono source feeds
/// every output channel.
const NORMALIZE_PTX: &str = r#"
.version 7.0
.target sm_52
.address_size 64

.visible .entry ruro_normalize(
    .param .u64 p_src,
    .param .u64 p_dst,
    .param .u32 p_sw,
    .param .u32 p_sh,
    .param .u32 p_step,
    .param .u32 p_sch,
    .param .u32 p_dw,
    .param .u32 p_dh,
    .param .u32 p_dch,
    .param .u32 p_swap,
    .param .f32 p_scale0,
    .param .f32 p_scale1,
    .param .f32 p_scale2,
    .param .f32 p_bias0,
    .param .f32 p_bias1,
    .param .f32 p_bias2
)
{
    .reg .pred %p<8>;
    .reg .b32 %r<24>;
    .reg .b64 %rd<16>;
    .reg .f32 %f<8>;

    ld.param.u64 %rd1, [p_src];
    ld.param.u64 %rd2, [p_dst];
    cvta.to.global.u64 %rd1, %rd1;
    cvta.to.global.u64 %rd2, %rd2;
    ld.param.u32 %r1, [p_sw];
    ld.param.u32 %r2, [p_sh];
    ld.param.u32 %r3, [p_step];
    ld.param.u32 %r4, [p_sch];
    ld.param.u32 %r5, [p_dw];
    ld.param.u32 %r6, [p_dh];
    ld.param.u32 %r7, [p_dch];
    ld.param.u32 %r8, [p_swap];

    // x and y of the output pixel.
    mov.u32 %r9, %ctaid.x;
    mov.u32 %r10, %ntid.x;
    mov.u32 %r11, %tid.x;
    mad.lo.u32 %r12, %r9, %r10, %r11;
    mov.u32 %r9, %ctaid.y;
    mov.u32 %r10, %ntid.y;
    mov.u32 %r11, %tid.y;
    mad.lo.u32 %r13, %r9, %r10, %r11;
    setp.ge.u32 %p1, %r12, %r5;
    setp.ge.u32 %p2, %r13, %r6;
    or.pred %p3, %p1, %p2;
    @%p3 bra DONE;

    // Source pixel: sx = x * sw / dw, sy = y * sh / dh (64-bit products).
    mul.wide.u32 %rd3, %r12, %r1;
    cvt.u64.u32 %rd4, %r5;
    div.u64 %rd3, %rd3, %rd4;
    mul.wide.u32 %rd5, %r13, %r2;
    cvt.u64.u32 %rd4, %r6;
    div.u64 %rd5, %rd5, %rd4;
    cvt.u64.u32 %rd6, %r3;
    mul.lo.u64 %rd5, %rd5, %rd6;
    cvt.u64.u32 %rd6, %r4;
    mul.lo.u64 %rd3, %rd3, %rd6;
    add.u64 %rd3, %rd3, %rd5;
    add.u64 %rd3, %rd1, %rd3;

    // Output address of channel 0 and the plane size in bytes.
    mul.wide.u32 %rd7, %r5, %r6;
    shl.b64 %rd7, %rd7, 2;
    mul.wide.u32 %rd8, %r13, %r5;
    cvt.u64.u32 %rd9, %r12;
    add.u64 %rd8, %rd8, %rd9;
    shl.b64 %rd8, %rd8, 2;
    add.u64 %rd8, %rd2, %rd8;

    // Byte offsets of the source channels feeding output channels 0, 1, 2.
    setp.ne.u32 %p4, %r8, 0;
    selp.u64 %rd10, 2, 0, %p4;
    selp.u64 %rd11, 0, 2, %p4;
    mov.u64 %rd12, 1;
    setp.eq.u32 %p5, %r4, 1;
    @%p5 mov.u64 %rd10, 0;
    @%p5 mov.u64 %rd11, 0;
    @%p5 mov.u64 %rd12, 0;

    add.u64 %rd13, %rd3, %rd10;
    ld.global.u8 %r14, [%rd13];
    cvt.rn.f32.u32 %f1, %r14;
    ld.param.f32 %f2, [p_scale0];
    ld.param.f32 %f3, [p_bias0];
    fma.rn.f32 %f4, %f1, %f2, %f3;
    st.global.f32 [%rd8], %f4;

    setp.lt.u32 %p6, %r7, 3;
    @%p6 bra DONE;

    add.u64 %rd13, %rd3, %rd12;
    ld.global.u8 %r14, [%rd13];
    cvt.rn.f32.u32 %f1, %r14;
    ld.param.f32 %f2, [p_scale1];
    ld.param.f32 %f3, [p_bias1];
    fma.rn.f32 %f4, %f1, %f2, %f3;
    add.u64 %rd14, %rd8, %rd7;
    st.global.f32 [%rd14], %f4;

    add.u64 %rd13, %rd3, %rd11;
    ld.global.u8 %r14, [%rd13];
    cvt.rn.f32.u32 %f1, %r14;
    ld.param.f32 %f2, [p_scale2];
    ld.param.f32 %f3, [p_bias2];
    fma.rn.f32 %f4, %f1, %f2, %f3;
    add.u64 %rd14, %rd14, %rd7;
    st.global.f32 [%rd14], %f4;

DONE:
    ret;
}
"#;

/// The compiled `ruro_normalize` of each device. Modules stay loaded for
/// the life of the process.
fn normalize_kernel(device: u32) -> Result<CuFunction> {
    struct Kernel(CuFunction);
    // SAFETY: a CUfunction is a handle usable from any thread.
    unsafe impl Send for Kernel {}
    static KERNELS: OnceLock<Mutex<HashMap<u32, Kernel>>> = OnceLock::new();
    let mut kernels = KERNELS.get_or_init(Default::default).lock();
    if let Some(k) = kernels.get(&device) {
        return Ok(k.0);
    }
    let c = cuda()?;
    let module = c.module_load_ptx(device, NORMALIZE_PTX)?;
    let f = match c.module_get_function(device, module, c"ruro_normalize") {
        Ok(f) => f,
        Err(e) => {
            // SAFETY: just loaded, not used elsewhere.
            let _ = unsafe { c.module_unload(device, module) };
            return Err(e);
        }
    };
    kernels.insert(device, Kernel(f));
    Ok(f)
}

/// How [`normalize`] turns pixels into model input.
#[derive(Debug, Clone, PartialEq)]
pub struct Normalize {
    /// Output size; 0 keeps the image's.
    pub width: u32,
    pub height: u32,
    /// Per-channel mean and standard deviation, applied to pixels scaled
    /// to 0..1: `(v / 255 - mean) / std`.
    pub mean: [f32; 3],
    pub std: [f32; 3],
    /// Output channels in BGR order.
    pub bgr: bool,
}

impl Default for Normalize {
    fn default() -> Self {
        Normalize {
            width: 0,
            height: 0,
            mean: [0.0; 3],
            std: [1.0; 3],
            bgr: false,
        }
    }
}

/// Converts `src` into a `[1, C, H, W]` float32 tensor on the same device:
/// C is 3 for colour images (alpha is dropped), 1 for mono. Waits for the
/// kernel before returning. `dst` keeps its buffer when it can.
pub fn normalize(src: &GpuImage, dst: &mut Tensor, p: &Normalize, stream: &Stream) -> Result<()> {
    let buf = src
        .buffer()
        .ok_or_else(|| anyhow::anyhow!("GPU image has no pixels"))?;
    let device = buf.device();
    ensure!(src.width > 0 && src.height > 0, "empty image");
    ensure!(
        src.encoding != Encoding::Mono16,
        "normalize takes 8-bit images, not {:?}",
        src.encoding
    );
    // The fields are public: the kernel must not read past the buffer.
    ensure!(
        src.step as u64 >= src.width as u64 * src.encoding.channels() as u64
            && src.step as u64 * src.height as u64 <= buf.len() as u64,
        "image {}x{} with step {} does not fit its buffer of {} bytes",
        src.width,
        src.height,
        src.step,
        buf.len()
    );
    ensure!(p.std.iter().all(|s| *s != 0.0), "std must not be zero");
    let (dw, dh) = match (p.width, p.height) {
        (0, 0) => (src.width, src.height),
        (w, h) if w > 0 && h > 0 => (w, h),
        _ => anyhow::bail!("set both width and height, or neither"),
    };
    let channels = if src.encoding == Encoding::Mono8 {
        1
    } else {
        3
    };
    dst.reshape_on(
        device,
        DType::F32,
        vec![1, channels as u64, dh as u64, dw as u64],
    )?;
    let out = dst
        .buffer_mut()
        .expect("reshape made it writable")
        .device_ptr_mut()?;
    let f = normalize_kernel(device)?;
    let mut src_ptr = buf.device_ptr();
    let mut dst_ptr = out;
    let (mut sw, mut sh, mut step, mut sch) = (
        src.width,
        src.height,
        src.step,
        src.encoding.channels() as u32,
    );
    let (mut dw_, mut dh_, mut dch, mut swap) = (dw, dh, channels, u32::from(p.bgr));
    let mut scale: [f32; 3] = std::array::from_fn(|c| 1.0 / (255.0 * p.std[c]));
    let mut bias: [f32; 3] = std::array::from_fn(|c| -p.mean[c] / p.std[c]);
    fn ptr<T>(v: &mut T) -> *mut c_void {
        (v as *mut T).cast()
    }
    let [s0, s1, s2] = &mut scale;
    let [b0, b1, b2] = &mut bias;
    let mut params = [
        ptr(&mut src_ptr),
        ptr(&mut dst_ptr),
        ptr(&mut sw),
        ptr(&mut sh),
        ptr(&mut step),
        ptr(&mut sch),
        ptr(&mut dw_),
        ptr(&mut dh_),
        ptr(&mut dch),
        ptr(&mut swap),
        ptr(s0),
        ptr(s1),
        ptr(s2),
        ptr(b0),
        ptr(b1),
        ptr(b2),
    ];
    let block = (16, 16);
    let grid = (dw.div_ceil(block.0), dh.div_ceil(block.1));
    ensure!(stream.device() == device, "stream is on another device");
    // SAFETY: the parameters match the kernel's list in order and type; the
    // kernel reads `sh` rows of `step` bytes from the source buffer (checked
    // when the image was made or received) and writes `dch * dw * dh` floats,
    // the size `reshape_on` gave the output buffer.
    unsafe { cuda()?.launch(device, f, grid, block, stream.raw(), &mut params)? };
    stream.synchronize()
}
