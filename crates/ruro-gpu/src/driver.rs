//! Run-time binding to the CUDA driver API (`libcuda.so`).
//!
//! Functions are resolved with `libloading` when first needed, so building
//! ruro never needs the CUDA toolkit and a machine without a GPU only fails
//! when a GPU node starts. Only the driver library is used: no runtime
//! (`cudart`), no `nvcc`. Kernels are PTX text compiled by the driver.
//!
//! Every wrapper makes the device's primary context current on the calling
//! thread first, because the driver's current context is per thread and
//! buffers are used (and dropped) from node threads, receive threads and tap
//! callbacks alike.

use std::ffi::{CStr, c_char, c_int, c_uint, c_void};
use std::sync::OnceLock;

use anyhow::{Result, anyhow, bail};
use libloading::Library;

pub type CuResult = c_int;
pub type CuDevice = c_int;
pub type CuContext = *mut c_void;
pub type CuDevicePtr = u64;
pub type CuStream = *mut c_void;
pub type CuEvent = *mut c_void;
pub type CuModule = *mut c_void;
pub type CuFunction = *mut c_void;

/// `CUipcMemHandle`: an opaque 64-byte handle to device memory that another
/// process can map.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct IpcMemHandle(pub [u8; 64]);

impl std::fmt::Debug for IpcMemHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("IpcMemHandle(..)")
    }
}

/// `CUmemorytype`.
const MEMORY_HOST: c_uint = 1;
const MEMORY_DEVICE: c_uint = 2;

/// `CUDA_MEMCPY2D`.
#[repr(C)]
struct Memcpy2d {
    src_x_in_bytes: usize,
    src_y: usize,
    src_memory_type: c_uint,
    src_host: *const c_void,
    src_device: CuDevicePtr,
    src_array: *mut c_void,
    src_pitch: usize,
    dst_x_in_bytes: usize,
    dst_y: usize,
    dst_memory_type: c_uint,
    dst_host: *mut c_void,
    dst_device: CuDevicePtr,
    dst_array: *mut c_void,
    dst_pitch: usize,
    width_in_bytes: usize,
    height: usize,
}

const SUCCESS: CuResult = 0;
const STREAM_NON_BLOCKING: c_uint = 1;
const EVENT_DEFAULT: c_uint = 0;
const IPC_MEM_LAZY_ENABLE_PEER_ACCESS: c_uint = 1;
const JIT_ERROR_LOG_BUFFER: c_int = 5;
const JIT_ERROR_LOG_BUFFER_SIZE_BYTES: c_int = 6;
const ATTR_COMPUTE_CAPABILITY_MAJOR: c_int = 75;
const ATTR_COMPUTE_CAPABILITY_MINOR: c_int = 76;

/// Environment variable naming the CUDA driver library to load.
pub const LIB_ENV: &str = "RURO_CUDA_LIB";

/// The resolved driver functions. Names are those the CUDA 12 headers map
/// the API to (the `_v2` entry points use 64-bit sizes).
#[allow(clippy::type_complexity)]
struct Api {
    init: unsafe extern "C" fn(c_uint) -> CuResult,
    driver_get_version: unsafe extern "C" fn(*mut c_int) -> CuResult,
    get_error_name: unsafe extern "C" fn(CuResult, *mut *const c_char) -> CuResult,
    get_error_string: unsafe extern "C" fn(CuResult, *mut *const c_char) -> CuResult,
    device_get_count: unsafe extern "C" fn(*mut c_int) -> CuResult,
    device_get: unsafe extern "C" fn(*mut CuDevice, c_int) -> CuResult,
    device_get_name: unsafe extern "C" fn(*mut c_char, c_int, CuDevice) -> CuResult,
    device_get_attribute: unsafe extern "C" fn(*mut c_int, c_int, CuDevice) -> CuResult,
    primary_ctx_retain: unsafe extern "C" fn(*mut CuContext, CuDevice) -> CuResult,
    ctx_set_current: unsafe extern "C" fn(CuContext) -> CuResult,
    mem_alloc: unsafe extern "C" fn(*mut CuDevicePtr, usize) -> CuResult,
    mem_free: unsafe extern "C" fn(CuDevicePtr) -> CuResult,
    mem_get_info: unsafe extern "C" fn(*mut usize, *mut usize) -> CuResult,
    mem_alloc_host: unsafe extern "C" fn(*mut *mut c_void, usize) -> CuResult,
    mem_free_host: unsafe extern "C" fn(*mut c_void) -> CuResult,
    memcpy_htod_async:
        unsafe extern "C" fn(CuDevicePtr, *const c_void, usize, CuStream) -> CuResult,
    memcpy_dtoh_async: unsafe extern "C" fn(*mut c_void, CuDevicePtr, usize, CuStream) -> CuResult,
    memcpy_dtod_async: unsafe extern "C" fn(CuDevicePtr, CuDevicePtr, usize, CuStream) -> CuResult,
    memcpy_2d_async: unsafe extern "C" fn(*const Memcpy2d, CuStream) -> CuResult,
    memset_d8_async: unsafe extern "C" fn(CuDevicePtr, u8, usize, CuStream) -> CuResult,
    stream_create: unsafe extern "C" fn(*mut CuStream, c_uint) -> CuResult,
    stream_synchronize: unsafe extern "C" fn(CuStream) -> CuResult,
    stream_destroy: unsafe extern "C" fn(CuStream) -> CuResult,
    event_create: unsafe extern "C" fn(*mut CuEvent, c_uint) -> CuResult,
    event_record: unsafe extern "C" fn(CuEvent, CuStream) -> CuResult,
    event_synchronize: unsafe extern "C" fn(CuEvent) -> CuResult,
    event_elapsed_time: unsafe extern "C" fn(*mut f32, CuEvent, CuEvent) -> CuResult,
    event_destroy: unsafe extern "C" fn(CuEvent) -> CuResult,
    ipc_get_mem_handle: unsafe extern "C" fn(*mut IpcMemHandle, CuDevicePtr) -> CuResult,
    ipc_open_mem_handle: unsafe extern "C" fn(*mut CuDevicePtr, IpcMemHandle, c_uint) -> CuResult,
    ipc_close_mem_handle: unsafe extern "C" fn(CuDevicePtr) -> CuResult,
    module_load_data_ex: unsafe extern "C" fn(
        *mut CuModule,
        *const c_void,
        c_uint,
        *mut c_int,
        *mut *mut c_void,
    ) -> CuResult,
    module_get_function: unsafe extern "C" fn(*mut CuFunction, CuModule, *const c_char) -> CuResult,
    module_unload: unsafe extern "C" fn(CuModule) -> CuResult,
    launch_kernel: unsafe extern "C" fn(
        CuFunction,
        c_uint,
        c_uint,
        c_uint,
        c_uint,
        c_uint,
        c_uint,
        c_uint,
        CuStream,
        *mut *mut c_void,
        *mut *mut c_void,
    ) -> CuResult,
    pointer_get_attribute: unsafe extern "C" fn(*mut c_void, c_int, CuDevicePtr) -> CuResult,
    ctx_synchronize: unsafe extern "C" fn() -> CuResult,
    _lib: Library,
}

/// One GPU and its primary context.
struct DeviceInfo {
    name: String,
    compute_capability: (i32, i32),
    context: CuContext,
}

/// The loaded driver and its devices. Get it with [`cuda`].
pub struct Cuda {
    api: Api,
    driver_version: i32,
    devices: Vec<DeviceInfo>,
}

// SAFETY: the driver API is thread safe; contexts are plain handles that any
// thread may make current.
unsafe impl Send for Cuda {}
unsafe impl Sync for Cuda {}

static CUDA: OnceLock<Result<Cuda, String>> = OnceLock::new();

/// The CUDA driver, loaded and initialised on first use. Fails (every time,
/// with the same message) when the library or a device is missing.
pub fn cuda() -> Result<&'static Cuda> {
    CUDA.get_or_init(|| Cuda::load().map_err(|e| format!("{e:#}")))
        .as_ref()
        .map_err(|e| anyhow!("CUDA unavailable: {e}"))
}

/// Why CUDA cannot be used here, or `None` if it can. Tests use this to skip.
pub fn unavailable() -> Option<String> {
    cuda().err().map(|e| e.to_string())
}

impl Cuda {
    fn load() -> Result<Cuda> {
        let path = std::env::var(LIB_ENV)
            .ok()
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| "libcuda.so.1".into());
        // SAFETY: loading the driver runs its initialisers, which have no
        // requirements on the caller.
        let lib = unsafe { Library::new(&path) }.map_err(|e| {
            anyhow!("cannot load `{path}` ({e}); install the NVIDIA driver or set {LIB_ENV}")
        })?;
        // SAFETY: the symbol types match the declarations in cuda.h (CUDA 12).
        let api = unsafe {
            macro_rules! sym {
                ($name:literal) => {
                    *lib.get(concat!($name, "\0").as_bytes())
                        .map_err(|e| anyhow!("{}: {e}", $name))?
                };
            }
            Api {
                init: sym!("cuInit"),
                driver_get_version: sym!("cuDriverGetVersion"),
                get_error_name: sym!("cuGetErrorName"),
                get_error_string: sym!("cuGetErrorString"),
                device_get_count: sym!("cuDeviceGetCount"),
                device_get: sym!("cuDeviceGet"),
                device_get_name: sym!("cuDeviceGetName"),
                device_get_attribute: sym!("cuDeviceGetAttribute"),
                primary_ctx_retain: sym!("cuDevicePrimaryCtxRetain"),
                ctx_set_current: sym!("cuCtxSetCurrent"),
                mem_alloc: sym!("cuMemAlloc_v2"),
                mem_free: sym!("cuMemFree_v2"),
                mem_get_info: sym!("cuMemGetInfo_v2"),
                mem_alloc_host: sym!("cuMemAllocHost_v2"),
                mem_free_host: sym!("cuMemFreeHost"),
                memcpy_htod_async: sym!("cuMemcpyHtoDAsync_v2"),
                memcpy_dtoh_async: sym!("cuMemcpyDtoHAsync_v2"),
                memcpy_dtod_async: sym!("cuMemcpyDtoDAsync_v2"),
                memcpy_2d_async: sym!("cuMemcpy2DAsync_v2"),
                memset_d8_async: sym!("cuMemsetD8Async"),
                stream_create: sym!("cuStreamCreate"),
                stream_synchronize: sym!("cuStreamSynchronize"),
                stream_destroy: sym!("cuStreamDestroy_v2"),
                event_create: sym!("cuEventCreate"),
                event_record: sym!("cuEventRecord"),
                event_synchronize: sym!("cuEventSynchronize"),
                event_elapsed_time: sym!("cuEventElapsedTime"),
                event_destroy: sym!("cuEventDestroy_v2"),
                ipc_get_mem_handle: sym!("cuIpcGetMemHandle"),
                ipc_open_mem_handle: sym!("cuIpcOpenMemHandle_v2"),
                ipc_close_mem_handle: sym!("cuIpcCloseMemHandle"),
                module_load_data_ex: sym!("cuModuleLoadDataEx"),
                module_get_function: sym!("cuModuleGetFunction"),
                module_unload: sym!("cuModuleUnload"),
                launch_kernel: sym!("cuLaunchKernel"),
                pointer_get_attribute: sym!("cuPointerGetAttribute"),
                ctx_synchronize: sym!("cuCtxSynchronize"),
                _lib: lib,
            }
        };
        let mut cuda = Cuda {
            api,
            driver_version: 0,
            devices: Vec::new(),
        };
        // SAFETY: plain driver calls with valid out-pointers.
        unsafe {
            cuda.check((cuda.api.init)(0), "cuInit")?;
            let mut v = 0;
            cuda.check((cuda.api.driver_get_version)(&mut v), "cuDriverGetVersion")?;
            cuda.driver_version = v;
            let mut n = 0;
            cuda.check((cuda.api.device_get_count)(&mut n), "cuDeviceGetCount")?;
            if n == 0 {
                bail!("no CUDA device found");
            }
            for ordinal in 0..n {
                let mut dev = 0;
                cuda.check((cuda.api.device_get)(&mut dev, ordinal), "cuDeviceGet")?;
                let mut name = [0 as c_char; 256];
                cuda.check(
                    (cuda.api.device_get_name)(name.as_mut_ptr(), name.len() as c_int, dev),
                    "cuDeviceGetName",
                )?;
                let (mut major, mut minor) = (0, 0);
                cuda.check(
                    (cuda.api.device_get_attribute)(&mut major, ATTR_COMPUTE_CAPABILITY_MAJOR, dev),
                    "cuDeviceGetAttribute",
                )?;
                cuda.check(
                    (cuda.api.device_get_attribute)(&mut minor, ATTR_COMPUTE_CAPABILITY_MINOR, dev),
                    "cuDeviceGetAttribute",
                )?;
                let mut context = std::ptr::null_mut();
                cuda.check(
                    (cuda.api.primary_ctx_retain)(&mut context, dev),
                    "cuDevicePrimaryCtxRetain",
                )?;
                cuda.devices.push(DeviceInfo {
                    name: CStr::from_ptr(name.as_ptr()).to_string_lossy().into_owned(),
                    compute_capability: (major, minor),
                    context,
                });
            }
        }
        Ok(cuda)
    }

    /// Turns a driver status into an error naming the call.
    fn check(&self, r: CuResult, what: &str) -> Result<()> {
        if r == SUCCESS {
            return Ok(());
        }
        let (mut name, mut text) = (std::ptr::null(), std::ptr::null());
        // SAFETY: the driver returns static strings (or leaves the pointer null).
        unsafe {
            (self.api.get_error_name)(r, &mut name);
            (self.api.get_error_string)(r, &mut text);
        }
        let s = |p: *const c_char| {
            if p.is_null() {
                String::new()
            } else {
                // SAFETY: non-null strings from the driver are NUL-terminated.
                unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
            }
        };
        Err(anyhow!("{what} failed: {} ({}, {r})", s(text), s(name)))
    }

    /// Driver version as `major * 1000 + minor * 10`, e.g. 13020.
    pub fn driver_version(&self) -> i32 {
        self.driver_version
    }

    pub fn device_count(&self) -> u32 {
        self.devices.len() as u32
    }

    /// Name of device `ordinal`, e.g. "NVIDIA GeForce RTX 4090".
    pub fn device_name(&self, device: u32) -> Result<&str> {
        Ok(&self.device(device)?.name)
    }

    pub fn compute_capability(&self, device: u32) -> Result<(i32, i32)> {
        Ok(self.device(device)?.compute_capability)
    }

    fn device(&self, device: u32) -> Result<&DeviceInfo> {
        self.devices.get(device as usize).ok_or_else(|| {
            anyhow!(
                "no CUDA device {device} (this machine has {})",
                self.devices.len()
            )
        })
    }

    /// Makes `device`'s primary context current on this thread.
    pub(crate) fn bind(&self, device: u32) -> Result<()> {
        let ctx = self.device(device)?.context;
        // SAFETY: a retained primary context stays valid for the process.
        self.check(
            unsafe { (self.api.ctx_set_current)(ctx) },
            "cuCtxSetCurrent",
        )
    }

    /// Free and total memory of `device`, in bytes.
    pub fn memory_info(&self, device: u32) -> Result<(usize, usize)> {
        self.bind(device)?;
        let (mut free, mut total) = (0, 0);
        // SAFETY: valid out-pointers.
        self.check(
            unsafe { (self.api.mem_get_info)(&mut free, &mut total) },
            "cuMemGetInfo",
        )?;
        Ok((free, total))
    }

    pub(crate) fn mem_alloc(&self, device: u32, len: usize) -> Result<CuDevicePtr> {
        self.bind(device)?;
        let mut ptr = 0;
        // SAFETY: valid out-pointer.
        self.check(
            unsafe { (self.api.mem_alloc)(&mut ptr, len.max(1)) },
            "cuMemAlloc",
        )?;
        Ok(ptr)
    }

    /// # Safety
    /// `ptr` must come from [`Cuda::mem_alloc`] on `device` and not be in use.
    pub(crate) unsafe fn mem_free(&self, device: u32, ptr: CuDevicePtr) -> Result<()> {
        self.bind(device)?;
        // SAFETY: guaranteed by the caller.
        self.check(unsafe { (self.api.mem_free)(ptr) }, "cuMemFree")
    }

    pub(crate) fn mem_alloc_host(&self, len: usize) -> Result<*mut u8> {
        self.bind(0)?;
        let mut ptr = std::ptr::null_mut();
        // SAFETY: valid out-pointer.
        self.check(
            unsafe { (self.api.mem_alloc_host)(&mut ptr, len.max(1)) },
            "cuMemAllocHost",
        )?;
        Ok(ptr.cast())
    }

    /// # Safety
    /// `ptr` must come from [`Cuda::mem_alloc_host`] and not be in use.
    pub(crate) unsafe fn mem_free_host(&self, ptr: *mut u8) -> Result<()> {
        self.bind(0)?;
        // SAFETY: guaranteed by the caller.
        self.check(
            unsafe { (self.api.mem_free_host)(ptr.cast()) },
            "cuMemFreeHost",
        )
    }

    /// # Safety
    /// `dst` must have `src.len()` writable bytes on `device`. `src` must stay
    /// valid until the stream reaches this copy.
    pub(crate) unsafe fn memcpy_htod(
        &self,
        device: u32,
        dst: CuDevicePtr,
        src: &[u8],
        stream: CuStream,
    ) -> Result<()> {
        self.bind(device)?;
        // SAFETY: guaranteed by the caller.
        self.check(
            unsafe { (self.api.memcpy_htod_async)(dst, src.as_ptr().cast(), src.len(), stream) },
            "cuMemcpyHtoDAsync",
        )
    }

    /// # Safety
    /// `src` must have `dst.len()` readable bytes on `device`. `dst` must stay
    /// valid and untouched until the stream reaches this copy.
    pub(crate) unsafe fn memcpy_dtoh(
        &self,
        device: u32,
        dst: &mut [u8],
        src: CuDevicePtr,
        stream: CuStream,
    ) -> Result<()> {
        self.bind(device)?;
        // SAFETY: guaranteed by the caller.
        self.check(
            unsafe {
                (self.api.memcpy_dtoh_async)(dst.as_mut_ptr().cast(), src, dst.len(), stream)
            },
            "cuMemcpyDtoHAsync",
        )
    }

    /// # Safety
    /// Both ranges must be valid on `device` for `len` bytes.
    pub(crate) unsafe fn memcpy_dtod(
        &self,
        device: u32,
        dst: CuDevicePtr,
        src: CuDevicePtr,
        len: usize,
        stream: CuStream,
    ) -> Result<()> {
        self.bind(device)?;
        // SAFETY: guaranteed by the caller.
        self.check(
            unsafe { (self.api.memcpy_dtod_async)(dst, src, len, stream) },
            "cuMemcpyDtoDAsync",
        )
    }

    /// Copies `rows` rows of `row_bytes` from device memory with pitch
    /// `src_pitch` into `dst` (tightly packed).
    ///
    /// # Safety
    /// The source rectangle must be readable on `device`; `dst` must hold
    /// `rows * row_bytes` bytes and stay valid until the stream reaches the copy.
    #[allow(clippy::too_many_arguments)]
    pub(crate) unsafe fn memcpy_2d_dtoh(
        &self,
        device: u32,
        dst: &mut [u8],
        src: CuDevicePtr,
        src_pitch: usize,
        row_bytes: usize,
        rows: usize,
        stream: CuStream,
    ) -> Result<()> {
        assert!(dst.len() >= row_bytes * rows);
        self.bind(device)?;
        let p = Memcpy2d {
            src_x_in_bytes: 0,
            src_y: 0,
            src_memory_type: MEMORY_DEVICE,
            src_host: std::ptr::null(),
            src_device: src,
            src_array: std::ptr::null_mut(),
            src_pitch,
            dst_x_in_bytes: 0,
            dst_y: 0,
            dst_memory_type: MEMORY_HOST,
            dst_host: dst.as_mut_ptr().cast(),
            dst_device: 0,
            dst_array: std::ptr::null_mut(),
            dst_pitch: row_bytes,
            width_in_bytes: row_bytes,
            height: rows,
        };
        // SAFETY: guaranteed by the caller; `p` lives across the call, which
        // reads the descriptor before returning.
        self.check(
            unsafe { (self.api.memcpy_2d_async)(&p, stream) },
            "cuMemcpy2DAsync",
        )
    }

    /// # Safety
    /// `dst` must have `len` writable bytes on `device`.
    pub(crate) unsafe fn memset_d8(
        &self,
        device: u32,
        dst: CuDevicePtr,
        value: u8,
        len: usize,
        stream: CuStream,
    ) -> Result<()> {
        self.bind(device)?;
        // SAFETY: guaranteed by the caller.
        self.check(
            unsafe { (self.api.memset_d8_async)(dst, value, len, stream) },
            "cuMemsetD8Async",
        )
    }

    pub(crate) fn stream_create(&self, device: u32) -> Result<CuStream> {
        self.bind(device)?;
        let mut s = std::ptr::null_mut();
        // SAFETY: valid out-pointer.
        self.check(
            unsafe { (self.api.stream_create)(&mut s, STREAM_NON_BLOCKING) },
            "cuStreamCreate",
        )?;
        Ok(s)
    }

    /// The CUDA device that device address `ptr` belongs to (memory any
    /// library of this process allocated).
    pub fn device_of_pointer(&self, ptr: CuDevicePtr) -> Result<u32> {
        /// `CU_POINTER_ATTRIBUTE_DEVICE_ORDINAL`.
        const DEVICE_ORDINAL: c_int = 9;
        let mut ordinal: c_int = -1;
        // SAFETY: a valid out-pointer for this attribute's int.
        self.check(
            unsafe {
                (self.api.pointer_get_attribute)(
                    (&mut ordinal as *mut c_int).cast(),
                    DEVICE_ORDINAL,
                    ptr,
                )
            },
            "cuPointerGetAttribute",
        )?;
        u32::try_from(ordinal).map_err(|_| anyhow!("{ptr:#x} is not device memory"))
    }

    /// Waits for work queued on a stream another library created, given by
    /// its handle (`__cuda_array_interface__`'s `stream`: 1 and 2 are the
    /// legacy and per-thread default streams).
    ///
    /// # Safety
    /// `handle` must be a live stream of `device`, or 1 or 2.
    pub unsafe fn synchronize_raw_stream(&self, device: u32, handle: usize) -> Result<()> {
        self.stream_synchronize(device, handle as CuStream)
    }

    /// Waits for all work queued on `device` by this process, on any stream.
    pub fn synchronize_device(&self, device: u32) -> Result<()> {
        self.bind(device)?;
        // SAFETY: no arguments; the device's context is current.
        self.check(unsafe { (self.api.ctx_synchronize)() }, "cuCtxSynchronize")
    }

    pub(crate) fn stream_synchronize(&self, device: u32, s: CuStream) -> Result<()> {
        self.bind(device)?;
        // SAFETY: `s` is a live stream (or null, the legacy default stream).
        self.check(
            unsafe { (self.api.stream_synchronize)(s) },
            "cuStreamSynchronize",
        )
    }

    /// # Safety
    /// `s` must be a stream created by [`Cuda::stream_create`], not used after.
    pub(crate) unsafe fn stream_destroy(&self, device: u32, s: CuStream) -> Result<()> {
        self.bind(device)?;
        // SAFETY: guaranteed by the caller.
        self.check(unsafe { (self.api.stream_destroy)(s) }, "cuStreamDestroy")
    }

    pub(crate) fn event_create(&self, device: u32) -> Result<CuEvent> {
        self.bind(device)?;
        let mut e = std::ptr::null_mut();
        // SAFETY: valid out-pointer.
        self.check(
            unsafe { (self.api.event_create)(&mut e, EVENT_DEFAULT) },
            "cuEventCreate",
        )?;
        Ok(e)
    }

    pub(crate) fn event_record(&self, device: u32, e: CuEvent, s: CuStream) -> Result<()> {
        self.bind(device)?;
        // SAFETY: live event and stream.
        self.check(unsafe { (self.api.event_record)(e, s) }, "cuEventRecord")
    }

    pub(crate) fn event_synchronize(&self, device: u32, e: CuEvent) -> Result<()> {
        self.bind(device)?;
        // SAFETY: live event.
        self.check(
            unsafe { (self.api.event_synchronize)(e) },
            "cuEventSynchronize",
        )
    }

    pub(crate) fn event_elapsed_ms(
        &self,
        device: u32,
        start: CuEvent,
        end: CuEvent,
    ) -> Result<f32> {
        self.bind(device)?;
        let mut ms = 0.0;
        // SAFETY: live, recorded events.
        self.check(
            unsafe { (self.api.event_elapsed_time)(&mut ms, start, end) },
            "cuEventElapsedTime",
        )?;
        Ok(ms)
    }

    /// # Safety
    /// `e` must be an event created by [`Cuda::event_create`], not used after.
    pub(crate) unsafe fn event_destroy(&self, device: u32, e: CuEvent) -> Result<()> {
        self.bind(device)?;
        // SAFETY: guaranteed by the caller.
        self.check(unsafe { (self.api.event_destroy)(e) }, "cuEventDestroy")
    }

    /// Exports an allocation (the base pointer of a `cuMemAlloc`) to other processes.
    pub(crate) fn ipc_get_mem_handle(&self, device: u32, ptr: CuDevicePtr) -> Result<IpcMemHandle> {
        self.bind(device)?;
        let mut h = IpcMemHandle([0; 64]);
        // SAFETY: valid out-pointer; the driver validates `ptr`.
        self.check(
            unsafe { (self.api.ipc_get_mem_handle)(&mut h, ptr) },
            "cuIpcGetMemHandle",
        )?;
        Ok(h)
    }

    /// Maps another process's allocation into this one.
    pub(crate) fn ipc_open_mem_handle(&self, device: u32, h: IpcMemHandle) -> Result<CuDevicePtr> {
        self.bind(device)?;
        let mut ptr = 0;
        // SAFETY: valid out-pointer; the driver validates the handle.
        self.check(
            unsafe { (self.api.ipc_open_mem_handle)(&mut ptr, h, IPC_MEM_LAZY_ENABLE_PEER_ACCESS) },
            "cuIpcOpenMemHandle",
        )?;
        Ok(ptr)
    }

    /// # Safety
    /// `ptr` must come from [`Cuda::ipc_open_mem_handle`] and not be in use.
    pub(crate) unsafe fn ipc_close_mem_handle(&self, device: u32, ptr: CuDevicePtr) -> Result<()> {
        self.bind(device)?;
        // SAFETY: guaranteed by the caller.
        self.check(
            unsafe { (self.api.ipc_close_mem_handle)(ptr) },
            "cuIpcCloseMemHandle",
        )
    }

    /// JIT-compiles PTX text for `device`. The error carries the compiler log.
    pub(crate) fn module_load_ptx(&self, device: u32, ptx: &str) -> Result<CuModule> {
        self.bind(device)?;
        let src = std::ffi::CString::new(ptx).map_err(|_| anyhow!("PTX contains a NUL byte"))?;
        let mut log = vec![0u8; 8192];
        let mut options = [JIT_ERROR_LOG_BUFFER, JIT_ERROR_LOG_BUFFER_SIZE_BYTES];
        let mut values = [log.as_mut_ptr().cast::<c_void>(), log.len() as *mut c_void];
        let mut module = std::ptr::null_mut();
        // SAFETY: NUL-terminated image; option values are a buffer and its
        // size passed as a pointer-sized integer, as the API specifies.
        let r = unsafe {
            (self.api.module_load_data_ex)(
                &mut module,
                src.as_ptr().cast(),
                options.len() as c_uint,
                options.as_mut_ptr(),
                values.as_mut_ptr(),
            )
        };
        self.check(r, "cuModuleLoadDataEx").map_err(|e| {
            let end = log.iter().position(|&b| b == 0).unwrap_or(log.len());
            anyhow!("{e}: {}", String::from_utf8_lossy(&log[..end]).trim())
        })?;
        Ok(module)
    }

    pub(crate) fn module_get_function(
        &self,
        device: u32,
        module: CuModule,
        name: &CStr,
    ) -> Result<CuFunction> {
        self.bind(device)?;
        let mut f = std::ptr::null_mut();
        // SAFETY: live module, NUL-terminated name.
        self.check(
            unsafe { (self.api.module_get_function)(&mut f, module, name.as_ptr()) },
            "cuModuleGetFunction",
        )?;
        Ok(f)
    }

    /// # Safety
    /// `module` must come from [`Cuda::module_load_ptx`] and not be used after.
    pub(crate) unsafe fn module_unload(&self, device: u32, module: CuModule) -> Result<()> {
        self.bind(device)?;
        // SAFETY: guaranteed by the caller.
        self.check(
            unsafe { (self.api.module_unload)(module) },
            "cuModuleUnload",
        )
    }

    /// # Safety
    /// `params` must point at values matching the kernel's parameter list,
    /// and every pointer the kernel dereferences must be valid.
    pub(crate) unsafe fn launch(
        &self,
        device: u32,
        f: CuFunction,
        grid: (u32, u32),
        block: (u32, u32),
        stream: CuStream,
        params: &mut [*mut c_void],
    ) -> Result<()> {
        self.bind(device)?;
        // SAFETY: guaranteed by the caller.
        self.check(
            unsafe {
                (self.api.launch_kernel)(
                    f,
                    grid.0,
                    grid.1,
                    1,
                    block.0,
                    block.1,
                    1,
                    0,
                    stream,
                    params.as_mut_ptr(),
                    std::ptr::null_mut(),
                )
            },
            "cuLaunchKernel",
        )
    }
}
