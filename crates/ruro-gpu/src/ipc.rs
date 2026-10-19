//! Sharing device buffers with other processes on the same machine.
//!
//! A message that crosses a process boundary carries a fixed-size
//! [`Descriptor`] instead of the pixels: the allocation's CUDA IPC handle,
//! the producer's identity and a lease slot. The receiver maps the
//! allocation with `cuIpcOpenMemHandle` (once per allocation; mappings are
//! cached) and reads it in place.
//!
//! # Lifetime: leases with generation counters
//!
//! The producer must not reuse an allocation while a receiver reads it, and
//! a receiver must not read an allocation the producer has already reused.
//! Each producer process keeps a lease table, a small shared-memory file of
//! 64-bit words, one per exported allocation: the high half is a generation,
//! the low half the number of readers.
//!
//! - Sending stamps the descriptor with the slot's current generation.
//! - A receiver *pins* the slot by incrementing its readers, but only if the
//!   generation still matches the descriptor (one compare-and-swap). If it
//!   does not, the producer reused the buffer after sending it and the frame
//!   is dropped as stale. Dropping the received buffer unpins it.
//! - The producer *reclaims* an allocation before writing it again by
//!   swapping `(generation, 0 readers)` for `(generation + 1, 0)`. That fails
//!   while anyone reads it, and once it succeeds no old descriptor can pin.
//!
//! The producer never blocks on readers: its pool skips allocations that are
//! still pinned and allocates another. A receiver that crashes while holding
//! a pin leaks that allocation until the producer exits.

use std::collections::HashMap;
use std::fs::File;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result, anyhow, bail, ensure};
use parking_lot::Mutex;

use crate::driver::{CuDevicePtr, IpcMemHandle, cuda};
use crate::memory::DeviceBuffer;

/// Lease slots per producer process: allocations it can export over its life.
const SLOTS: u32 = 16384;

/// Directory holding lease tables. `/dev/shm` is shared by every process of
/// the machine, whatever their environment.
const TABLE_DIR: &str = "/dev/shm";
const TABLE_PREFIX: &str = "ruro-gpu-";

/// Encoded size of a [`Descriptor`].
pub const DESCRIPTOR_LEN: usize = 16 + 4 + 8 + 4 + 8 + 8 + 8 + 4 + 4 + 64;

/// Identifies this process among all that ever ran on this boot: pid reuse
/// does not make two producers look alike.
pub(crate) fn nonce() -> u64 {
    static NONCE: OnceLock<u64> = OnceLock::new();
    *NONCE.get_or_init(|| {
        use std::hash::{BuildHasher, Hasher};
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u32(std::process::id());
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        h.finish().max(1)
    })
}

/// This machine's boot id: descriptors from another machine (over Zenoh) or
/// an earlier boot (from a recording) are refused rather than misread.
pub(crate) fn boot_id() -> [u8; 16] {
    static BOOT: OnceLock<[u8; 16]> = OnceLock::new();
    *BOOT.get_or_init(|| {
        let text = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap_or_default();
        let hex: Vec<u8> = text
            .bytes()
            .filter(u8::is_ascii_hexdigit)
            .map(|b| (b as char).to_digit(16).unwrap() as u8)
            .collect();
        let mut id = [0u8; 16];
        for (i, pair) in hex.as_chunks::<2>().0.iter().take(16).enumerate() {
            id[i] = pair[0] << 4 | pair[1];
        }
        id
    })
}

/// A producer's lease table, mapped into this process.
pub(crate) struct LeaseTable {
    words: *const AtomicU64,
    /// Next unused slot (only meaningful in the producer's own table).
    next: AtomicU32,
}

// SAFETY: the mapping is shared memory accessed only through atomics.
unsafe impl Send for LeaseTable {}
unsafe impl Sync for LeaseTable {}

fn table_path(pid: u32, nonce: u64) -> PathBuf {
    PathBuf::from(format!("{TABLE_DIR}/{TABLE_PREFIX}{pid}-{nonce:016x}"))
}

impl LeaseTable {
    fn map(file: &File) -> Result<LeaseTable> {
        use std::os::fd::AsRawFd;
        let len = SLOTS as usize * 8;
        // SAFETY: mapping a file we hold open, shared and read-write; the
        // file is at least `len` bytes long.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            bail!(
                "mmap of lease table failed: {}",
                std::io::Error::last_os_error()
            );
        }
        Ok(LeaseTable {
            words: ptr.cast(),
            next: AtomicU32::new(0),
        })
    }

    /// Creates this process's table.
    fn create() -> Result<LeaseTable> {
        remove_stale_tables();
        let path = table_path(std::process::id(), nonce());
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("creating GPU lease table {}", path.display()))?;
        file.set_len(SLOTS as u64 * 8)?;
        let table = Self::map(&file)?;
        let _ = TABLE_TO_REMOVE.set(path);
        // SAFETY: registering a plain `extern "C"` function.
        unsafe { libc::atexit(remove_own_table) };
        Ok(table)
    }

    /// Maps another process's table.
    fn open(pid: u32, nonce: u64) -> Result<LeaseTable> {
        let path = table_path(pid, nonce);
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| {
                format!(
                    "opening GPU lease table {} (has the producer exited?)",
                    path.display()
                )
            })?;
        ensure!(
            file.metadata()?.len() >= SLOTS as u64 * 8,
            "GPU lease table {} is truncated",
            path.display()
        );
        Self::map(&file)
    }

    fn word(&self, slot: u32) -> Result<&AtomicU64> {
        ensure!(slot < SLOTS, "lease slot {slot} out of range");
        // SAFETY: in bounds of the mapping, which lives as long as `self`
        // (tables are never unmapped); AtomicU64 has u64's layout.
        Ok(unsafe { &*self.words.add(slot as usize) })
    }

    fn claim_slot(&self) -> Result<u32> {
        let slot = self.next.fetch_add(1, Ordering::Relaxed);
        ensure!(
            slot < SLOTS,
            "GPU lease table full: this process exported {SLOTS} allocations"
        );
        Ok(slot)
    }
}

static TABLE_TO_REMOVE: OnceLock<PathBuf> = OnceLock::new();

extern "C" fn remove_own_table() {
    if let Some(path) = TABLE_TO_REMOVE.get() {
        let _ = std::fs::remove_file(path);
    }
}

/// Removes the tables of producers that are gone (killed processes do not
/// run their exit handlers).
fn remove_stale_tables() {
    let Ok(entries) = std::fs::read_dir(TABLE_DIR) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(rest) = name.to_str().and_then(|n| n.strip_prefix(TABLE_PREFIX)) else {
            continue;
        };
        let Some(pid) = rest.split('-').next().and_then(|p| p.parse::<i32>().ok()) else {
            continue;
        };
        // SAFETY: signal 0 only checks that the process exists.
        let gone = unsafe { libc::kill(pid, 0) } != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        if gone {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn own_table() -> Result<&'static LeaseTable> {
    static OWN: OnceLock<Result<LeaseTable, String>> = OnceLock::new();
    OWN.get_or_init(|| LeaseTable::create().map_err(|e| format!("{e:#}")))
        .as_ref()
        .map_err(|e| anyhow!("{e}"))
}

const READERS: u64 = 0xffff_ffff;

fn generation(word: u64) -> u32 {
    (word >> 32) as u32
}

/// Takes a slot back for writing: succeeds only with no readers, and bumps
/// the generation so descriptors sent earlier can no longer pin it.
fn reclaim_word(word: &AtomicU64) -> bool {
    let mut v = word.load(Ordering::Acquire);
    loop {
        if v & READERS != 0 {
            return false;
        }
        let next = u64::from(generation(v).wrapping_add(1)) << 32;
        match word.compare_exchange_weak(v, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return true,
            Err(now) => v = now,
        }
    }
}

/// Adds a reader if the slot is still at generation `expected`.
fn pin_word(word: &AtomicU64, expected: u32) -> bool {
    let mut v = word.load(Ordering::Acquire);
    loop {
        if generation(v) != expected || v & READERS == READERS - 1 {
            return false;
        }
        match word.compare_exchange_weak(v, v + 1, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return true,
            Err(now) => v = now,
        }
    }
}

fn unpin_word(word: &AtomicU64) {
    word.fetch_sub(1, Ordering::AcqRel);
}

/// How an allocation of this process is shared.
pub(crate) struct Export {
    handle: IpcMemHandle,
    slot: u32,
}

/// Takes an exported allocation back from readers (see the module docs).
pub(crate) fn reclaim(export: &Export) -> bool {
    match own_table().and_then(|t| t.word(export.slot)) {
        Ok(word) => reclaim_word(word),
        Err(_) => false,
    }
}

/// Allocations of this process, so that a descriptor coming back to it (two
/// groups of one graph run in one process, as tests do) resolves without
/// CUDA IPC, which refuses handles of the calling process.
/// Device, address and size of each allocation, by id.
type LocalAllocations = HashMap<u64, (u32, CuDevicePtr, usize)>;
static LOCAL: OnceLock<Mutex<LocalAllocations>> = OnceLock::new();

pub(crate) fn register_local(id: u64, device: u32, ptr: CuDevicePtr, size: usize) {
    LOCAL
        .get_or_init(Default::default)
        .lock()
        .insert(id, (device, ptr, size));
}

pub(crate) fn unregister_local(id: u64) {
    if let Some(map) = LOCAL.get() {
        map.lock().remove(&id);
    }
}

/// A device buffer as described to another process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Descriptor {
    pub boot_id: [u8; 16],
    pub pid: u32,
    pub nonce: u64,
    pub device: u32,
    pub allocation: u64,
    pub allocation_size: u64,
    /// Bytes of the buffer in use.
    pub len: u64,
    pub slot: u32,
    pub generation: u32,
    pub handle: IpcMemHandle,
}

impl Descriptor {
    /// Describes `buf` for another process, exporting its allocation the
    /// first time. A received buffer is described as its producer did, so
    /// it can be passed on to a third process.
    pub fn of(buf: &DeviceBuffer) -> Result<Descriptor> {
        if let Some(import) = buf.backing_import() {
            return Ok(import.desc);
        }
        let alloc = buf
            .backing_allocation()
            .context("buffer has no allocation")?;
        let export = alloc
            .export
            .get_or_init(|| {
                let export = || -> Result<Export> {
                    let table = own_table()?;
                    let handle = cuda()?.ipc_get_mem_handle(alloc.device, alloc.ptr)?;
                    Ok(Export {
                        handle,
                        slot: table.claim_slot()?,
                    })
                };
                export().map_err(|e| format!("{e:#}"))
            })
            .as_ref()
            .map_err(|e| anyhow!("cannot share GPU buffer: {e}"))?;
        let word = own_table()?.word(export.slot)?.load(Ordering::Acquire);
        Ok(Descriptor {
            boot_id: boot_id(),
            pid: std::process::id(),
            nonce: nonce(),
            device: alloc.device,
            allocation: alloc.id,
            allocation_size: alloc.size as u64,
            len: buf.len() as u64,
            slot: export.slot,
            generation: generation(word),
            handle: export.handle,
        })
    }

    pub fn write(&self, buf: &mut [u8]) {
        let mut w = Writer(buf);
        w.put(&self.boot_id);
        w.put(&self.pid.to_le_bytes());
        w.put(&self.nonce.to_le_bytes());
        w.put(&self.device.to_le_bytes());
        w.put(&self.allocation.to_le_bytes());
        w.put(&self.allocation_size.to_le_bytes());
        w.put(&self.len.to_le_bytes());
        w.put(&self.slot.to_le_bytes());
        w.put(&self.generation.to_le_bytes());
        w.put(&self.handle.0);
    }

    pub fn read(buf: &[u8]) -> Result<Descriptor> {
        ensure!(
            buf.len() >= DESCRIPTOR_LEN,
            "GPU buffer descriptor truncated"
        );
        let mut r = Reader(buf);
        Ok(Descriptor {
            boot_id: r.array(),
            pid: u32::from_le_bytes(r.array()),
            nonce: u64::from_le_bytes(r.array()),
            device: u32::from_le_bytes(r.array()),
            allocation: u64::from_le_bytes(r.array()),
            allocation_size: u64::from_le_bytes(r.array()),
            len: u64::from_le_bytes(r.array()),
            slot: u32::from_le_bytes(r.array()),
            generation: u32::from_le_bytes(r.array()),
            handle: IpcMemHandle(r.array()),
        })
    }

    /// Maps the described buffer into this process and pins it.
    pub fn import(&self) -> Result<DeviceBuffer> {
        ensure!(
            self.boot_id == boot_id(),
            "GPU buffer comes from another machine or an earlier boot; \
             put gpu/download before wires that leave the machine or are recorded"
        );
        ensure!(
            self.len <= self.allocation_size,
            "GPU buffer descriptor is inconsistent"
        );
        if self.nonce == nonce() {
            return self.import_local();
        }
        let table = remote_table(self.pid, self.nonce)?;
        let word = table.word(self.slot)?;
        if !pin_word(word, self.generation) {
            bail!("stale GPU buffer: the producer reused it before it was received");
        }
        let lease = Lease::Remote {
            table: table.clone(),
            slot: self.slot,
        };
        let mapping = match mapping(self) {
            Ok(m) => m,
            Err(e) => {
                drop(lease);
                return Err(e);
            }
        };
        let ptr = mapping.ptr;
        Ok(DeviceBuffer::imported(
            self.device,
            ptr,
            self.len as usize,
            Import {
                desc: *self,
                _lease: lease,
                _mapping: Some(mapping),
            },
        ))
    }

    fn import_local(&self) -> Result<DeviceBuffer> {
        let (device, ptr, size) = LOCAL
            .get()
            .and_then(|m| m.lock().get(&self.allocation).copied())
            .context("stale GPU buffer: its allocation was freed")?;
        ensure!(
            device == self.device && size as u64 == self.allocation_size,
            "GPU buffer descriptor does not match its allocation"
        );
        let word = own_table()?.word(self.slot)?;
        if !pin_word(word, self.generation) {
            bail!("stale GPU buffer: the producer reused it before it was received");
        }
        Ok(DeviceBuffer::imported(
            device,
            ptr,
            self.len as usize,
            Import {
                desc: *self,
                _lease: Lease::Local { slot: self.slot },
                _mapping: None,
            },
        ))
    }
}

/// Lease tables of other producers, mapped once each.
fn remote_table(pid: u32, nonce: u64) -> Result<Arc<LeaseTable>> {
    static TABLES: OnceLock<Mutex<HashMap<u64, Arc<LeaseTable>>>> = OnceLock::new();
    let mut tables = TABLES.get_or_init(Default::default).lock();
    if let Some(t) = tables.get(&nonce) {
        return Ok(t.clone());
    }
    let t = Arc::new(LeaseTable::open(pid, nonce)?);
    tables.insert(nonce, t.clone());
    Ok(t)
}

/// An allocation of another process mapped into this one. Mappings are
/// cached for the life of the process: CUDA refuses to map one handle twice
/// in a context, and mapping costs far more than a frame's worth of work.
struct Mapping {
    device: u32,
    ptr: CuDevicePtr,
}

impl Drop for Mapping {
    fn drop(&mut self) {
        if let Ok(c) = cuda() {
            // SAFETY: opened by `ipc_open_mem_handle`; the last reference is
            // gone, so no buffer uses it.
            let _ = unsafe { c.ipc_close_mem_handle(self.device, self.ptr) };
        }
    }
}

fn mapping(desc: &Descriptor) -> Result<Arc<Mapping>> {
    type Mappings = HashMap<(u64, u64), Arc<Mapping>>;
    static MAPPINGS: OnceLock<Mutex<Mappings>> = OnceLock::new();
    let key = (desc.nonce, desc.allocation);
    let mut maps = MAPPINGS.get_or_init(Default::default).lock();
    if let Some(m) = maps.get(&key) {
        return Ok(m.clone());
    }
    let ptr = cuda()?.ipc_open_mem_handle(desc.device, desc.handle)?;
    let m = Arc::new(Mapping {
        device: desc.device,
        ptr,
    });
    maps.insert(key, m.clone());
    Ok(m)
}

/// A pin on a producer's lease slot, released on drop.
enum Lease {
    Local { slot: u32 },
    Remote { table: Arc<LeaseTable>, slot: u32 },
}

impl Drop for Lease {
    fn drop(&mut self) {
        let word = match self {
            Lease::Local { slot } => own_table().and_then(|t| t.word(*slot)),
            Lease::Remote { table, slot } => table.word(*slot),
        };
        if let Ok(word) = word {
            unpin_word(word);
        }
    }
}

/// What keeps a received buffer valid: the pin, and the mapping.
pub(crate) struct Import {
    desc: Descriptor,
    _lease: Lease,
    _mapping: Option<Arc<Mapping>>,
}

struct Writer<'a>(&'a mut [u8]);

impl Writer<'_> {
    fn put(&mut self, bytes: &[u8]) {
        let (head, tail) = std::mem::take(&mut self.0).split_at_mut(bytes.len());
        head.copy_from_slice(bytes);
        self.0 = tail;
    }
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn array<const N: usize>(&mut self) -> [u8; N] {
        let (head, tail) = self.0.split_at(N);
        self.0 = tail;
        head.try_into().expect("length checked by the caller")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_and_reclaims_follow_the_generation() {
        let w = AtomicU64::new(0);
        assert!(pin_word(&w, 0));
        assert!(pin_word(&w, 0));
        assert!(!reclaim_word(&w), "pinned slots cannot be reclaimed");
        unpin_word(&w);
        unpin_word(&w);
        assert!(reclaim_word(&w));
        assert_eq!(generation(w.load(Ordering::Relaxed)), 1);
        assert!(!pin_word(&w, 0), "an old descriptor is stale");
        assert!(pin_word(&w, 1));
        unpin_word(&w);
        // The generation wraps around.
        let w = AtomicU64::new(u64::from(u32::MAX) << 32);
        assert!(reclaim_word(&w));
        assert_eq!(generation(w.load(Ordering::Relaxed)), 0);
    }

    #[test]
    fn descriptor_roundtrip() {
        let d = Descriptor {
            boot_id: [7; 16],
            pid: 42,
            nonce: 0x1234_5678_9abc_def0,
            device: 1,
            allocation: 99,
            allocation_size: 6 << 20,
            len: 1920 * 1080 * 3,
            slot: 3,
            generation: 17,
            handle: IpcMemHandle([5; 64]),
        };
        let mut buf = vec![0u8; DESCRIPTOR_LEN];
        d.write(&mut buf);
        assert_eq!(Descriptor::read(&buf).unwrap(), d);
        assert!(Descriptor::read(&buf[..DESCRIPTOR_LEN - 1]).is_err());
    }

    #[test]
    fn foreign_descriptors_are_refused() {
        let d = Descriptor {
            boot_id: [0xee; 16],
            pid: 1,
            nonce: 2,
            device: 0,
            allocation: 1,
            allocation_size: 1,
            len: 1,
            slot: 0,
            generation: 0,
            handle: IpcMemHandle([0; 64]),
        };
        let err = d.import().unwrap_err().to_string();
        assert!(err.contains("another machine"), "{err}");
    }

    #[test]
    fn boot_id_is_read() {
        assert_ne!(boot_id(), [0; 16]);
    }
}
