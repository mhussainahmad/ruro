//! Device memory: streams, events, pinned host memory, and pooled device
//! buffers.
//!
//! Every device buffer comes from a per-device pool of whole `cuMemAlloc`
//! allocations. Dropping a buffer returns its allocation to the pool, so a
//! loop that produces a frame per step allocates only while it warms up.
//! Whole allocations (not sub-allocations of a big block) are what CUDA IPC
//! can export, so an allocation is also the unit shared with other
//! processes; see [`crate::ipc`] for how its reuse waits for their readers.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use anyhow::{Result, bail, ensure};
use parking_lot::Mutex;

use crate::driver::{CuDevicePtr, CuEvent, CuStream, cuda};
use crate::ipc;

/// An ordered queue of device work. Copies and kernels on one stream run in
/// order; different streams may overlap.
pub struct Stream {
    device: u32,
    raw: CuStream,
    owned: bool,
}

// SAFETY: CUDA streams may be used from any thread.
unsafe impl Send for Stream {}
unsafe impl Sync for Stream {}

impl Stream {
    /// A new non-blocking stream on `device`.
    pub fn new(device: u32) -> Result<Stream> {
        let raw = cuda()?.stream_create(device)?;
        Ok(Stream {
            device,
            raw,
            owned: true,
        })
    }

    /// The device's default (null) stream, for one-off work such as
    /// previews. It does not order against non-blocking streams.
    pub fn default_for(device: u32) -> Stream {
        Stream {
            device,
            raw: std::ptr::null_mut(),
            owned: false,
        }
    }

    pub fn device(&self) -> u32 {
        self.device
    }

    pub(crate) fn raw(&self) -> CuStream {
        self.raw
    }

    /// Waits until all work queued on the stream has finished.
    pub fn synchronize(&self) -> Result<()> {
        cuda()?.stream_synchronize(self.device, self.raw)
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        if self.owned
            && let Ok(c) = cuda()
        {
            // SAFETY: created by `stream_create` and not used after this.
            let _ = unsafe { c.stream_destroy(self.device, self.raw) };
        }
    }
}

/// A marker in a stream, used to time device work.
pub struct Event {
    device: u32,
    raw: CuEvent,
}

// SAFETY: CUDA events may be used from any thread.
unsafe impl Send for Event {}
unsafe impl Sync for Event {}

impl Event {
    pub fn new(device: u32) -> Result<Event> {
        Ok(Event {
            device,
            raw: cuda()?.event_create(device)?,
        })
    }

    /// Marks the current end of `stream`'s queue.
    pub fn record(&self, stream: &Stream) -> Result<()> {
        cuda()?.event_record(self.device, self.raw, stream.raw())
    }

    /// Waits until the stream has reached this event.
    pub fn synchronize(&self) -> Result<()> {
        cuda()?.event_synchronize(self.device, self.raw)
    }

    /// Milliseconds of device time between `earlier` and this event.
    pub fn elapsed_ms(&self, earlier: &Event) -> Result<f32> {
        cuda()?.event_elapsed_ms(self.device, earlier.raw, self.raw)
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        if let Ok(c) = cuda() {
            // SAFETY: created by `event_create` and not used after this.
            let _ = unsafe { c.event_destroy(self.device, self.raw) };
        }
    }
}

/// Page-locked host memory. Copies between it and the device run at full
/// bus speed and truly asynchronously.
pub struct PinnedBuffer {
    ptr: *mut u8,
    len: usize,
}

// SAFETY: plain host memory owned by this value.
unsafe impl Send for PinnedBuffer {}
unsafe impl Sync for PinnedBuffer {}

impl PinnedBuffer {
    /// Allocates `len` zeroed bytes.
    pub fn new(len: usize) -> Result<PinnedBuffer> {
        let ptr = cuda()?.mem_alloc_host(len)?;
        // SAFETY: freshly allocated, at least `len` bytes.
        unsafe { ptr.write_bytes(0, len) };
        Ok(PinnedBuffer { ptr, len })
    }
}

impl std::ops::Deref for PinnedBuffer {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        // SAFETY: `len` initialised bytes owned by this value.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl std::ops::DerefMut for PinnedBuffer {
    fn deref_mut(&mut self) -> &mut [u8] {
        // SAFETY: as above, borrowed mutably.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for PinnedBuffer {
    fn drop(&mut self) {
        if let Ok(c) = cuda() {
            // SAFETY: allocated by `mem_alloc_host`, not used after this.
            let _ = unsafe { c.mem_free_host(self.ptr) };
        }
    }
}

static NEXT_ALLOCATION: AtomicU64 = AtomicU64::new(1);

/// One `cuMemAlloc` allocation, owned by a pool or a buffer.
pub(crate) struct Allocation {
    pub device: u32,
    pub ptr: CuDevicePtr,
    pub size: usize,
    /// Unique within this process; names the allocation to other processes.
    pub id: u64,
    /// IPC handle and lease slot, set when first sent to another process.
    pub export: OnceLock<Result<ipc::Export, String>>,
}

impl Allocation {
    fn new(device: u32, size: usize) -> Result<Allocation> {
        let ptr = cuda()?.mem_alloc(device, size)?;
        let id = NEXT_ALLOCATION.fetch_add(1, Ordering::Relaxed);
        ipc::register_local(id, device, ptr, size);
        Ok(Allocation {
            device,
            ptr,
            size,
            id,
            export: OnceLock::new(),
        })
    }

    /// True once the allocation has been sent to another process.
    pub fn is_exported(&self) -> bool {
        matches!(self.export.get(), Some(Ok(_)))
    }

    /// Takes the allocation back from other processes' readers so it can be
    /// written. False while any of them still holds it.
    pub fn try_reclaim(&self) -> bool {
        match self.export.get() {
            Some(Ok(export)) => ipc::reclaim(export),
            _ => true,
        }
    }
}

impl Drop for Allocation {
    fn drop(&mut self) {
        ipc::unregister_local(self.id);
        if let Some(Ok(export)) = self.export.get() {
            ipc::retire(export);
        }
        if let Ok(c) = cuda() {
            // SAFETY: allocated by `mem_alloc`; callers drop an exported
            // allocation only after reclaiming it from every reader.
            let _ = unsafe { c.mem_free(self.device, self.ptr) };
        }
    }
}

/// Allocation sizes are rounded up so that buffers of similar size can be
/// reused for one another.
fn size_class(len: usize) -> usize {
    const MIB: usize = 1 << 20;
    if len <= MIB {
        len.max(256).next_power_of_two()
    } else {
        len.div_ceil(MIB) * MIB
    }
}

/// Most free allocations a pool keeps; beyond that, returned ones are freed.
const MAX_FREE: usize = 64;

/// An allocation sent to another process is reused only after this many
/// newer ones of its size have been returned, so its readers have a few
/// frame periods to pin it (and its frame is not dropped as stale) even
/// when the producer recycles every frame at once. Producers that send to
/// other processes therefore cycle through at least `RING + 1` buffers.
const RING: usize = 3;

/// Counters of a device's buffer pool.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PoolStats {
    /// `cuMemAlloc` calls so far.
    pub allocations: u64,
    /// Allocations freed again (pool trimming).
    pub frees: u64,
    /// Allocations currently idle in the pool.
    pub idle: usize,
    /// Bytes currently allocated by the pool, idle or in use.
    pub bytes: u64,
}

struct Pool {
    device: u32,
    /// Idle allocations, least recently returned first.
    free: Mutex<VecDeque<Allocation>>,
    allocations: AtomicU64,
    frees: AtomicU64,
    bytes: AtomicU64,
}

impl Pool {
    fn take(&self, len: usize) -> Result<Allocation> {
        let class = size_class(len);
        {
            let mut free = self.free.lock();
            // Oldest first: a buffer returned long ago is the least likely to
            // be still queued for a reader in another process. Reclaiming
            // comes last, as it invalidates descriptors sent earlier.
            let rested = |i: usize| {
                let newer = free.iter().skip(i + 1).filter(|b| b.size == class);
                newer.count() >= RING
            };
            if let Some(i) = (0..free.len()).find(|&i| {
                let a = &free[i];
                a.size == class && (!a.is_exported() || rested(i)) && a.try_reclaim()
            }) {
                return Ok(free.remove(i).expect("index in range"));
            }
        }
        let alloc = match Allocation::new(self.device, class) {
            Ok(a) => a,
            Err(e) => {
                // Out of memory, perhaps: give idle buffers back and retry.
                if self.trim(0) == 0 {
                    return Err(e);
                }
                Allocation::new(self.device, class)?
            }
        };
        self.allocations.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(class as u64, Ordering::Relaxed);
        Ok(alloc)
    }

    fn give_back(&self, alloc: Allocation) {
        let mut free = self.free.lock();
        free.push_back(alloc);
        if free.len() > MAX_FREE {
            drop(free);
            self.trim(MAX_FREE);
        }
    }

    /// Frees idle allocations nobody reads until at most `keep` remain.
    /// Returns how many were freed.
    fn trim(&self, keep: usize) -> usize {
        let mut freed = Vec::new();
        {
            let mut free = self.free.lock();
            let mut i = 0;
            while free.len() > keep && i < free.len() {
                if free[i].try_reclaim() {
                    freed.push(free.remove(i).expect("index in range"));
                } else {
                    i += 1;
                }
            }
        }
        for a in &freed {
            self.frees.fetch_add(1, Ordering::Relaxed);
            self.bytes.fetch_sub(a.size as u64, Ordering::Relaxed);
        }
        freed.len()
    }
}

static POOLS: OnceLock<Mutex<HashMap<u32, &'static Pool>>> = OnceLock::new();

fn pool(device: u32) -> Result<&'static Pool> {
    let c = cuda()?;
    ensure!(
        device < c.device_count(),
        "no CUDA device {device} (this machine has {})",
        c.device_count()
    );
    let mut pools = POOLS.get_or_init(Default::default).lock();
    Ok(*pools.entry(device).or_insert_with(|| {
        // One pool per device for the life of the process.
        Box::leak(Box::new(Pool {
            device,
            free: Mutex::new(VecDeque::new()),
            allocations: AtomicU64::new(0),
            frees: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
        }))
    }))
}

/// Counters of `device`'s buffer pool.
pub fn pool_stats(device: u32) -> Result<PoolStats> {
    let p = pool(device)?;
    Ok(PoolStats {
        allocations: p.allocations.load(Ordering::Relaxed),
        frees: p.frees.load(Ordering::Relaxed),
        idle: p.free.lock().len(),
        bytes: p.bytes.load(Ordering::Relaxed),
    })
}

enum Backing {
    /// An allocation of this process, returned to its pool on drop.
    Owned(Option<Allocation>),
    /// Memory of another buffer (possibly in another process), held by a lease.
    Imported(ipc::Import),
}

/// A buffer in GPU memory.
///
/// Messages hold buffers in an `Arc`, so in-process receivers share one
/// allocation. Writing needs `&mut`, which a shared buffer cannot give.
pub struct DeviceBuffer {
    device: u32,
    ptr: CuDevicePtr,
    len: usize,
    backing: Backing,
}

impl std::fmt::Debug for DeviceBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceBuffer")
            .field("device", &self.device)
            .field("len", &self.len)
            .field("imported", &self.is_imported())
            .finish()
    }
}

impl DeviceBuffer {
    /// A buffer of `len` bytes on `device`, from the device's pool. Its
    /// contents are unspecified.
    pub fn alloc(device: u32, len: usize) -> Result<DeviceBuffer> {
        let alloc = pool(device)?.take(len)?;
        Ok(DeviceBuffer {
            device,
            ptr: alloc.ptr,
            len,
            backing: Backing::Owned(Some(alloc)),
        })
    }

    pub(crate) fn imported(device: u32, ptr: CuDevicePtr, len: usize, import: ipc::Import) -> Self {
        DeviceBuffer {
            device,
            ptr,
            len,
            backing: Backing::Imported(import),
        }
    }

    pub(crate) fn backing_allocation(&self) -> Option<&Allocation> {
        match &self.backing {
            Backing::Owned(a) => a.as_ref(),
            Backing::Imported(_) => None,
        }
    }

    pub(crate) fn backing_import(&self) -> Option<&ipc::Import> {
        match &self.backing {
            Backing::Imported(i) => Some(i),
            Backing::Owned(_) => None,
        }
    }

    pub fn device(&self) -> u32 {
        self.device
    }

    /// Device address, for passing to kernels.
    pub fn device_ptr(&self) -> u64 {
        self.ptr
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Bytes the buffer can hold without reallocating.
    pub fn capacity(&self) -> usize {
        match &self.backing {
            Backing::Owned(a) => a.as_ref().map_or(0, |a| a.size),
            Backing::Imported(_) => self.len,
        }
    }

    /// True if the memory belongs to another buffer (received from another
    /// process group). Such buffers are read-only.
    pub fn is_imported(&self) -> bool {
        matches!(self.backing, Backing::Imported(_))
    }

    /// Makes sure the buffer may be written: it is this process's own and
    /// no reader in another process still holds it.
    fn writable(&self) -> Result<()> {
        match &self.backing {
            Backing::Imported(_) => bail!("a received GPU buffer is read-only"),
            Backing::Owned(a) => {
                let a = a.as_ref().expect("owned until dropped");
                ensure!(
                    a.try_reclaim(),
                    "GPU buffer is still being read by another process"
                );
                Ok(())
            }
        }
    }

    /// Changes the length within the capacity.
    pub fn set_len(&mut self, len: usize) -> Result<()> {
        ensure!(
            len <= self.capacity(),
            "length {len} exceeds capacity {}",
            self.capacity()
        );
        self.len = len;
        Ok(())
    }

    /// Device address for kernels that write the buffer.
    pub fn device_ptr_mut(&mut self) -> Result<u64> {
        self.writable()?;
        Ok(self.ptr)
    }

    /// Copies `src` to the start of the buffer and waits for the copy.
    pub fn upload(&mut self, src: &[u8], stream: &Stream) -> Result<()> {
        self.writable()?;
        ensure!(
            src.len() <= self.len,
            "{} bytes do not fit a {} byte buffer",
            src.len(),
            self.len
        );
        self.check_device(stream)?;
        // SAFETY: the range is inside the allocation; `src` outlives the
        // copy because the stream is synchronized before returning.
        unsafe { cuda()?.memcpy_htod(self.device, self.ptr, src, stream.raw())? };
        stream.synchronize()
    }

    /// Copies the start of the buffer into `dst` and waits for the copy.
    pub fn download(&self, dst: &mut [u8], stream: &Stream) -> Result<()> {
        ensure!(
            dst.len() <= self.len,
            "cannot read {} bytes from a {} byte buffer",
            dst.len(),
            self.len
        );
        self.check_device(stream)?;
        // SAFETY: as in `upload`.
        unsafe { cuda()?.memcpy_dtoh(self.device, dst, self.ptr, stream.raw())? };
        stream.synchronize()
    }

    /// Copies every `row_stride`-th row (rows of `row_bytes`, `pitch` apart)
    /// into `dst`, packed, and waits for the copy.
    pub fn download_rows(
        &self,
        dst: &mut [u8],
        pitch: usize,
        row_bytes: usize,
        row_stride: usize,
        rows: usize,
        stream: &Stream,
    ) -> Result<()> {
        let src_pitch = pitch * row_stride.max(1);
        ensure!(
            rows == 0 || (rows - 1) * src_pitch + row_bytes <= self.len,
            "rows outside the buffer"
        );
        ensure!(dst.len() >= rows * row_bytes, "destination too small");
        if rows == 0 {
            return Ok(());
        }
        self.check_device(stream)?;
        // SAFETY: rectangle checked above; synchronized before returning.
        unsafe {
            cuda()?.memcpy_2d_dtoh(
                self.device,
                dst,
                self.ptr,
                src_pitch,
                row_bytes,
                rows,
                stream.raw(),
            )?
        };
        stream.synchronize()
    }

    /// Sets every byte to `value` and waits.
    pub fn fill(&mut self, value: u8, stream: &Stream) -> Result<()> {
        self.writable()?;
        self.check_device(stream)?;
        // SAFETY: the whole buffer is inside the allocation.
        unsafe { cuda()?.memset_d8(self.device, self.ptr, value, self.len, stream.raw())? };
        stream.synchronize()
    }

    /// Copies `src` (on the same device) into the start of this buffer and waits.
    pub fn copy_from(&mut self, src: &DeviceBuffer, stream: &Stream) -> Result<()> {
        self.writable()?;
        ensure!(
            src.device == self.device,
            "buffers are on different devices"
        );
        ensure!(src.len <= self.len, "source larger than destination");
        self.check_device(stream)?;
        // SAFETY: both ranges are inside their allocations.
        unsafe { cuda()?.memcpy_dtod(self.device, self.ptr, src.ptr, src.len, stream.raw())? };
        stream.synchronize()
    }

    /// Copies `len` bytes from device address `src`, memory another library
    /// owns (a PyTorch or CuPy array, say), into the start of this buffer,
    /// and waits.
    ///
    /// # Safety
    /// `src..src + len` must be device memory of this buffer's device that
    /// nobody writes while the copy runs.
    pub unsafe fn copy_from_raw(&mut self, src: u64, len: usize, stream: &Stream) -> Result<()> {
        self.writable()?;
        ensure!(
            len <= self.len,
            "{len} bytes do not fit a {} byte buffer",
            self.len
        );
        self.check_device(stream)?;
        // SAFETY: the destination range is inside the allocation; the
        // source range is the caller's contract.
        unsafe { cuda()?.memcpy_dtod(self.device, self.ptr, src, len, stream.raw())? };
        stream.synchronize()
    }

    fn check_device(&self, stream: &Stream) -> Result<()> {
        ensure!(
            stream.device() == self.device,
            "stream is on device {}, buffer on device {}",
            stream.device(),
            self.device
        );
        Ok(())
    }
}

impl Drop for DeviceBuffer {
    fn drop(&mut self) {
        if let Backing::Owned(a) = &mut self.backing
            && let Some(a) = a.take()
            && let Ok(p) = pool(a.device)
        {
            p.give_back(a);
        }
    }
}

/// Makes `slot` hold a buffer of `len` bytes on `device` that this process
/// may write: the current one if nobody else holds it, it is large enough
/// and it has never been sent to another process; otherwise one from the
/// pool (which makes exported buffers rest before reuse).
pub(crate) fn writable_buffer(
    slot: &mut Option<Arc<DeviceBuffer>>,
    device: u32,
    len: usize,
) -> Result<&mut DeviceBuffer> {
    let reuse = slot.as_mut().and_then(Arc::get_mut).is_some_and(|b| {
        b.device == device
            && b.capacity() >= len
            && b.backing_allocation().is_some_and(|a| !a.is_exported())
    });
    if reuse {
        let b = Arc::get_mut(slot.as_mut().expect("checked")).expect("checked");
        b.set_len(len)?;
    } else {
        // Drop the old buffer first so a single-buffer loop can reuse it.
        *slot = None;
        *slot = Some(Arc::new(DeviceBuffer::alloc(device, len)?));
    }
    Ok(Arc::get_mut(slot.as_mut().expect("set above")).expect("unique"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_classes() {
        assert_eq!(size_class(0), 256);
        assert_eq!(size_class(300), 512);
        assert_eq!(size_class(1 << 20), 1 << 20);
        assert_eq!(size_class(1920 * 1080 * 3), 6 << 20);
    }
}
