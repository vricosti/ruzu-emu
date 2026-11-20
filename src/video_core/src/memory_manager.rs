// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! GPU virtual address space manager.
//!
//! Port of zuyu/src/video_core/memory_manager.h and memory_manager.cpp.
//!
//! Implements dual page table architecture (big 64KB pages + small 4KB pages)
//! with bitpacked entry arrays, continuity tracking, and kind mapping.

use common::multi_level_page_table::MultiLevelPageTable;
use common::range_map::RangeMap;
use common::sparse_large_vector::SparseLargeVector;

use crate::cache_types::CacheType;
use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
use crate::invalidation_accumulator::InvalidationAccumulator;
use crate::pte_kind::PteKind;
use crate::rasterizer_interface::{RasterizerHandle, RasterizerInterface};

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

/// Non-owning memory-manager pointer matching upstream's stored
/// `Tegra::MemoryManager*` relationships.
///
/// The owning `Arc<Mutex<MemoryManager>>` remains responsible for lifetime.
/// This handle exists for callbacks entered from `MemoryManager::ReadBlock`,
/// where locking that same mutex again would recurse. Upstream directly uses
/// its already-borrowed pointer in that situation.
#[derive(Clone, Copy)]
pub struct MemoryManagerHandle {
    ptr: *const MemoryManager,
}

// Safety: this handle adds neither ownership nor synchronization. The channel
// and renderer lifetime graph must keep the pointed manager alive and serialize
// access, exactly like upstream's raw `Tegra::MemoryManager*`.
unsafe impl Send for MemoryManagerHandle {}
unsafe impl Sync for MemoryManagerHandle {}

impl MemoryManagerHandle {
    pub fn from_ref(memory_manager: &MemoryManager) -> Self {
        Self {
            ptr: memory_manager as *const MemoryManager,
        }
    }

    /// # Safety
    ///
    /// The caller must ensure the pointed manager is alive and that access is
    /// serialized by the renderer/channel owner graph.
    pub unsafe fn as_ref<'a>(self) -> &'a MemoryManager {
        unsafe { &*self.ptr }
    }
}

// ── Constants ───────────────────────────────────────────────────────────

/// CPU page bits — upstream `cpu_page_bits = 12`.
const CPU_PAGE_BITS: u64 = 12;

/// Number of entries packed per u64 (2 bits each).
const ENTRIES_PER_U64: usize = 32;

/// Number of continuity bits per u64.
const CONTINUOUS_BITS: usize = 64;

/// Device page size (4 KB) — matches upstream Core::DEVICE_PAGESIZE.
const DEVICE_PAGE_SIZE: u64 = 1 << 12;
const DEVICE_PAGE_MASK: u64 = DEVICE_PAGE_SIZE - 1;

/// Scalar types explicitly instantiated for Eden's `MemoryManager::Read/Write` templates.
pub(crate) trait MemoryValue: Copy + Default {}

impl MemoryValue for u8 {}
impl MemoryValue for u16 {}
impl MemoryValue for u32 {}
impl MemoryValue for u64 {}

// ── Entry type ──────────────────────────────────────────────────────────

/// Page entry state, packed as 2 bits in the entry arrays.
///
/// Upstream: `MemoryManager::EntryType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
enum EntryType {
    Free = 0,
    Reserved = 1,
    Mapped = 2,
}

// ── Static ID generator ────────────────────────────────────────────────

static UNIQUE_IDENTIFIER_GENERATOR: AtomicUsize = AtomicUsize::new(0);

/// Keys the per-thread `MappingSnapshot` views (Lichii keys them by `unique_identifier`,
/// memory_manager.cpp:809-811). ruzu's `MemoryManager` constructors overwrite
/// `unique_identifier` with a caller-chosen id that two live managers may share, so the views
/// use this never-reused per-instance token instead.
static SNAPSHOT_OWNER_GENERATOR: AtomicUsize = AtomicUsize::new(0);

// ── Continuous runs (Lichii extension) ─────────────────────────────────

/// Lichii: `MemoryManager::ContinuousRun` (memory_manager.h:301-304).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ContinuousRun {
    end: u64,
    device_addr: u64,
}

/// Lichii: `MemoryManager::MappingSnapshot::Run` (memory_manager.h:217-220).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MappingSnapshotRun {
    begin: u64,
    end: u64,
}

/// Lichii: `MemoryManager::MappingSnapshot` (memory_manager.h:214-222): an immutable copy of
/// the device-contiguous runs that readers query without a lock; each thread swaps its copy
/// when `map_serial` changes. Lichii also copies the kind map here for its
/// `GetMemoryLayoutSize`; that part is not ported (ruzu keeps Eden's kind-map lookup).
#[derive(Debug, Default)]
struct MappingSnapshot {
    runs: Vec<MappingSnapshotRun>,
}

/// State Lichii guards with `runs_guard` (memory_manager.h:305-310): `continuous_runs`,
/// `snapshot` and `snapshot_serial`.
#[derive(Debug, Default)]
struct ContinuousRunsState {
    /// Maximal GPU ranges, keyed by their start, that are mapped to one device range: what
    /// `max_continuous_range_uncached` walks to.
    continuous_runs: BTreeMap<u64, ContinuousRun>,
    snapshot: Option<Arc<MappingSnapshot>>,
    snapshot_serial: u64,
}

/// Lichii: `CurrentSnapshot`'s `View` (memory_manager.cpp:803-807).
#[derive(Default)]
struct SnapshotView {
    owner: usize,
    serial: u64,
    snapshot: Option<Arc<MappingSnapshot>>,
}

thread_local! {
    /// Lichii: `thread_local std::array<View, 4> views` (memory_manager.cpp:808).
    static SNAPSHOT_VIEWS: RefCell<[SnapshotView; 4]> = RefCell::new(Default::default());
}

/// Volume of guest traffic that sparse (`Reserved`) or unmapped (`Free`) GMMU
/// pages silently swallow: writes are dropped and reads come back as zeros,
/// which is what renders as black textures. The per-address warnings below are
/// capped at ten lines each, so these uncapped totals are what actually sizes
/// an on-demand commit of those pages.
static SPARSE_WRITE_DROPPED_BYTES: AtomicU64 = AtomicU64::new(0);
static SPARSE_READ_ZEROED_BYTES: AtomicU64 = AtomicU64::new(0);

/// Reports the first occurrence and then each crossing of a 1 MiB boundary, so
/// the log shows both onset and growth without one line per page.
fn note_sparse_traffic(counter: &AtomicU64, bytes: usize, what: &str) {
    const REPORT_STEP: u64 = 1 << 20;
    let before = counter.fetch_add(bytes as u64, Ordering::Relaxed);
    let after = before + bytes as u64;
    if before == 0 {
        log::warn!("gpu_mm: {what} started ({bytes} bytes)");
    } else if before / REPORT_STEP != after / REPORT_STEP {
        log::warn!("gpu_mm: {what} reached {} MiB", after >> 20);
    }
}

/// Diagnostic (`RUZU_GPU_XLATE_MISS=1`): counts GPU-VA translations that find
/// no mapping, per 64 MiB bucket, and reports each bucket at 1, 10, 100, ...
/// misses. Buffer bindings resolve through this path without ever reaching the
/// read counters above, so this is what shows a draw input living in an
/// unbacked region.
fn note_translation_miss(gpu_addr: u64) {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    if !*ENABLED.get_or_init(|| std::env::var_os("RUZU_GPU_XLATE_MISS").is_some()) {
        return;
    }
    static MISSES: OnceLock<std::sync::Mutex<std::collections::HashMap<u64, u64>>> =
        OnceLock::new();
    let mut misses = MISSES.get_or_init(Default::default).lock().unwrap();
    let count = misses.entry(gpu_addr >> 26).or_insert(0);
    *count += 1;
    let n = *count;
    let mut power = 1u64;
    while power < n {
        power *= 10;
    }
    if n == 1000 || (n == 1 && misses.len() % 16 == 1) {
        log::warn!(
            "gpu_mm: translation miss at {gpu_addr:#x} from\n{}",
            std::backtrace::Backtrace::force_capture()
        );
    }
    if power == n {
        log::warn!(
            "gpu_mm: translation miss #{n} in bucket {:#x} (address {gpu_addr:#x})",
            (gpu_addr >> 26) << 26
        );
    }
}

/// Total bytes dropped by writes to sparse or unmapped GMMU pages.
pub fn sparse_write_dropped_bytes() -> u64 {
    SPARSE_WRITE_DROPPED_BYTES.load(Ordering::Relaxed)
}

/// Total bytes zero-filled by reads of sparse or unmapped GMMU pages.
pub fn sparse_read_zeroed_bytes() -> u64 {
    SPARSE_READ_ZEROED_BYTES.load(Ordering::Relaxed)
}

fn gpu_va_trace_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("RUZU_GPU_VA_TRACE").is_some())
}

// ── GpuMemoryManager (inner implementation) ─────────────────────────────

/// GPU virtual memory manager with dual page tables.
///
/// This is the inner implementation corresponding to upstream `Tegra::MemoryManager`.
/// It is wrapped by the outer `MemoryManager` struct that adds the `Arc<Mutex<>>` layer.
///
/// Architecture:
/// - Big page table (`big_page_table_dev`): `SparseLargeVector<u32>` storing `dev_addr >> CPU_PAGE_BITS`
///   per big page (default 64KB).
/// - Small page table (`page_table`): `MultiLevelPageTable<u32>` storing `dev_addr >> CPU_PAGE_BITS`
///   per 4KB page.
/// - Bitpacked entry arrays: `Vec<u64>` with 2 bits per page (Free=0, Reserved=1, Mapped=2).
/// - Big page continuity bitmap: `Vec<u64>` with 1 bit per big page.
/// - Addresses below `split_address` (default `1 << 34`) use big pages; above use small pages.
pub struct GpuMemoryManager {
    // Geometry
    address_space_bits: u64,
    address_space_size: u64,
    split_address: u64,
    page_bits: u64,
    page_size: u64,
    page_mask: u64,
    page_table_mask: u64,
    big_page_bits: u64,
    big_page_size: u64,
    big_page_mask: u64,
    big_page_table_mask: u64,

    // Small page table (addresses >= split_address)
    page_table: MultiLevelPageTable<u32>,
    /// Bitpacked entry types for small pages, 2 bits each, 32 per u64.
    entries: Vec<u64>,

    // Big page table (addresses < split_address)
    big_page_table_dev: SparseLargeVector<u32>,
    /// Bitpacked entry types for big pages, 2 bits each, 32 per u64.
    big_entries: Vec<u64>,
    /// Continuity bitmap for big pages, 1 bit each, 64 per u64.
    big_page_continuous: Vec<u64>,

    // Kind tracking
    kind_map: RangeMap<PteKind>,

    // Unique identifier for rasterizer callbacks
    unique_identifier: usize,
    /// Upstream stores `MaxwellDeviceMemoryManager& memory` in `MemoryManager`.
    device_memory: Arc<MaxwellDeviceMemoryManager>,
    /// Upstream stores `VideoCore::RasterizerInterface* rasterizer`.
    rasterizer: Option<RasterizerHandle>,
    /// Upstream stores `std::unique_ptr<VideoCommon::InvalidationAccumulator>`.
    accumulator: InvalidationAccumulator,
    /// Upstream `page_stash2`: the device ranges `FlushCaching` hands to
    /// `RasterizerInterface::InnerInvalidation`, reused across calls.
    page_stash2: Vec<(u64, usize)>,
    /// Lichii `constant_buffer_writes` (memory_manager.h): the device ranges
    /// `write_constant_buffer_data` wrote since the last `flush_caching`,
    /// merged when they touch, handed to
    /// `RasterizerInterface::invalidate_constant_buffer_data`.
    constant_buffer_writes: Vec<(u64, usize)>,
    /// When `Some`, rasterizer notifications (`modify_gpu_memory`,
    /// `unmap_memory`) are recorded here instead of being invoked inline, so
    /// the caller can replay them AFTER releasing the memory-manager mutex.
    /// Upstream `MemoryManager` has no mutex, so its inline rasterizer calls
    /// never nest a cache lock under a memory-manager lock; in Rust the
    /// nvdrv (CPU) path holds `Arc<Mutex<MemoryManager>>` while the GPU
    /// thread holds the rasterizer lock during draws and then locks this
    /// memory manager — invoking the rasterizer inline here deadlocks (ABBA,
    /// GPU-thread-side callers.
    deferred_rasterizer_ops: Option<Vec<DeferredRasterizerOp>>,
    /// Lichii: `map_serial` (memory_manager.h:298-299). Bumped after each page table change;
    /// readers keep their `MappingSnapshot` until then.
    map_serial: AtomicU64,
    /// Lichii: `runs_guard` and the state it guards (memory_manager.h:305-310). The GPU thread
    /// reads through the unlocked `MemoryManagerHandle` while nvdrv maps under the outer
    /// mutex, so the runs are only touched under this lock and read from snapshots.
    runs_guard: parking_lot::Mutex<ContinuousRunsState>,
    /// Key of this instance's per-thread snapshot views; see `SNAPSHOT_OWNER_GENERATOR`.
    snapshot_owner: usize,
}

/// A rasterizer notification produced under the memory-manager lock and
/// replayed by the caller after releasing it. See
/// `GpuMemoryManager::deferred_rasterizer_ops`.
#[derive(Debug, Clone, Copy)]
pub enum DeferredRasterizerOp {
    ModifyGpuMemory { id: usize, gpu_addr: u64, size: u64 },
    UnmapMemory { device_addr: u64, size: u64 },
}

impl GpuMemoryManager {
    /// Upstream: `MemoryManager::MemoryManager(system, memory, address_space_bits, split_address,
    ///            big_page_bits, page_bits)`.
    #[cfg(test)]
    pub fn new() -> Self {
        Self::with_params(40, 1u64 << 34, 16, 12)
    }

    #[cfg(test)]
    pub fn with_params(
        address_space_bits: u64,
        split_address: u64,
        big_page_bits: u64,
        page_bits: u64,
    ) -> Self {
        Self::with_params_and_device_memory(
            Arc::new(MaxwellDeviceMemoryManager::default()),
            address_space_bits,
            split_address,
            big_page_bits,
            page_bits,
        )
    }

    pub fn with_params_and_device_memory(
        device_memory: Arc<MaxwellDeviceMemoryManager>,
        address_space_bits: u64,
        split_address: u64,
        big_page_bits: u64,
        page_bits: u64,
    ) -> Self {
        let address_space_size = 1u64 << address_space_bits;
        let page_size = 1u64 << page_bits;
        let page_mask = page_size - 1;
        let big_page_size = 1u64 << big_page_bits;
        let big_page_mask = big_page_size - 1;

        let page_table_bits = address_space_bits - page_bits;
        let big_page_table_bits = address_space_bits - big_page_bits;
        let page_table_size = 1u64 << page_table_bits;
        let big_page_table_size = 1u64 << big_page_table_bits;
        let page_table_mask = page_table_size - 1;
        let big_page_table_mask = big_page_table_size - 1;

        // Upstream: page_table{address_space_bits, address_space_bits + page_bits - 38,
        //                       page_bits != big_page_bits ? page_bits : 0}
        let first_level_bits = (address_space_bits + page_bits).saturating_sub(38);
        let effective_page_bits = if page_bits != big_page_bits {
            page_bits
        } else {
            0
        };
        let page_table = MultiLevelPageTable::<u32>::with_params(
            address_space_bits as usize,
            first_level_bits as usize,
            effective_page_bits as usize,
        );

        let mut big_page_table_dev = SparseLargeVector::<u32>::new();
        big_page_table_dev.resize_and_clear(big_page_table_size as usize);

        let big_entries = vec![0u64; (big_page_table_size as usize) / ENTRIES_PER_U64];
        let big_page_continuous = vec![0u64; (big_page_table_size as usize) / CONTINUOUS_BITS];
        let entries = vec![0u64; (page_table_size as usize) / ENTRIES_PER_U64];

        let unique_identifier = UNIQUE_IDENTIFIER_GENERATOR.fetch_add(1, Ordering::AcqRel);

        Self {
            address_space_bits,
            address_space_size,
            split_address,
            page_bits,
            page_size,
            page_mask,
            page_table_mask,
            big_page_bits,
            big_page_size,
            big_page_mask,
            big_page_table_mask,
            page_table,
            entries,
            big_page_table_dev,
            big_entries,
            big_page_continuous,
            kind_map: RangeMap::new(PteKind::INVALID),
            unique_identifier,
            device_memory,
            rasterizer: None,
            accumulator: InvalidationAccumulator::new(),
            page_stash2: Vec::new(),
            constant_buffer_writes: Vec::new(),
            deferred_rasterizer_ops: None,
            map_serial: AtomicU64::new(0),
            runs_guard: parking_lot::Mutex::new(ContinuousRunsState::default()),
            snapshot_owner: SNAPSHOT_OWNER_GENERATOR.fetch_add(1, Ordering::Relaxed),
        }
    }

    /// Upstream: `MemoryManager::GetID()`.
    pub fn get_id(&self) -> usize {
        self.unique_identifier
    }

    /// Lichii `MemoryManager::MapSerial` (memory_manager.h:176-178): changes with every page
    /// table update.
    pub fn map_serial(&self) -> u64 {
        self.map_serial.load(Ordering::Acquire)
    }

    fn with_rasterizer_mut<R>(
        &mut self,
        f: impl FnOnce(&mut dyn RasterizerInterface) -> R,
    ) -> Option<R> {
        let handle = self.rasterizer?;
        Some(unsafe { handle.with_mut(f) })
    }

    /// Upstream: `MemoryManager::BindRasterizer(rasterizer)`.
    pub fn bind_rasterizer(&mut self, rasterizer: &dyn RasterizerInterface) {
        self.rasterizer = Some(RasterizerHandle::from_ref(rasterizer));
    }

    /// Start recording rasterizer notifications instead of invoking them
    /// inline. See `deferred_rasterizer_ops` for the deadlock rationale.
    pub fn begin_deferring_rasterizer_ops(&mut self) {
        self.deferred_rasterizer_ops = Some(Vec::new());
    }

    /// Stop recording and return the pending notifications. The caller must
    /// replay them through the rasterizer AFTER releasing the
    /// memory-manager mutex.
    pub fn take_deferred_rasterizer_ops(&mut self) -> Vec<DeferredRasterizerOp> {
        self.deferred_rasterizer_ops.take().unwrap_or_default()
    }

    /// Copy of the bound rasterizer handle, for replaying deferred ops
    /// outside the memory-manager mutex.
    pub fn rasterizer_handle(&self) -> Option<RasterizerHandle> {
        self.rasterizer
    }

    /// `rasterizer.modify_gpu_memory(...)`, inline or deferred depending on
    /// `deferred_rasterizer_ops`.
    fn notify_modify_gpu_memory(&mut self, gpu_addr: u64, size: u64) {
        let id = self.unique_identifier;
        if let Some(buf) = self.deferred_rasterizer_ops.as_mut() {
            buf.push(DeferredRasterizerOp::ModifyGpuMemory { id, gpu_addr, size });
        } else {
            let _ = self
                .with_rasterizer_mut(|rasterizer| rasterizer.modify_gpu_memory(id, gpu_addr, size));
        }
    }

    // ── Entry access (bitpacked) ────────────────────────────────────────

    /// Upstream: `GetEntry<true>(position)`.
    fn get_entry_big(&self, position: u64) -> EntryType {
        let idx = (position >> self.big_page_bits) as usize;
        let entry_mask = self.big_entries[idx / ENTRIES_PER_U64];
        let sub_index = idx % ENTRIES_PER_U64;
        match (entry_mask >> (2 * sub_index)) & 0x03 {
            0 => EntryType::Free,
            1 => EntryType::Reserved,
            2 => EntryType::Mapped,
            _ => EntryType::Free,
        }
    }

    /// Upstream: `GetEntry<false>(position)`.
    fn get_entry_small(&self, position: u64) -> EntryType {
        let idx = (position >> self.page_bits) as usize;
        let entry_mask = self.entries[idx / ENTRIES_PER_U64];
        let sub_index = idx % ENTRIES_PER_U64;
        match (entry_mask >> (2 * sub_index)) & 0x03 {
            0 => EntryType::Free,
            1 => EntryType::Reserved,
            2 => EntryType::Mapped,
            _ => EntryType::Free,
        }
    }

    /// Upstream: `SetEntry<true>(position, entry)`.
    fn set_entry_big(&mut self, position: u64, entry: EntryType) {
        let idx = (position >> self.big_page_bits) as usize;
        let slot = idx / ENTRIES_PER_U64;
        let sub_index = idx % ENTRIES_PER_U64;
        let entry_mask = self.big_entries[slot];
        self.big_entries[slot] =
            (!((3u64) << (sub_index * 2)) & entry_mask) | ((entry as u64) << (sub_index * 2));
    }

    /// Upstream: `SetEntry<false>(position, entry)`.
    fn set_entry_small(&mut self, position: u64, entry: EntryType) {
        let idx = (position >> self.page_bits) as usize;
        let slot = idx / ENTRIES_PER_U64;
        let sub_index = idx % ENTRIES_PER_U64;
        let entry_mask = self.entries[slot];
        self.entries[slot] =
            (!((3u64) << (sub_index * 2)) & entry_mask) | ((entry as u64) << (sub_index * 2));
    }

    // ── Page entry index ────────────────────────────────────────────────

    /// Upstream: `PageEntryIndex<true>(gpu_addr)`.
    fn page_entry_index_big(&self, gpu_addr: u64) -> usize {
        ((gpu_addr >> self.big_page_bits) & self.big_page_table_mask) as usize
    }

    /// Upstream: `PageEntryIndex<false>(gpu_addr)`.
    fn page_entry_index_small(&self, gpu_addr: u64) -> usize {
        ((gpu_addr >> self.page_bits) & self.page_table_mask) as usize
    }

    // ── Big page continuity ─────────────────────────────────────────────

    /// Upstream: `IsBigPageContinuous(big_page_index)`.
    fn is_big_page_continuous(&self, big_page_index: usize) -> bool {
        let entry_mask = self.big_page_continuous[big_page_index / CONTINUOUS_BITS];
        let sub_index = big_page_index % CONTINUOUS_BITS;
        ((entry_mask >> sub_index) & 0x1) != 0
    }

    /// Upstream: `SetBigPageContinuous(big_page_index, value)`.
    fn set_big_page_continuous(&mut self, big_page_index: usize, value: bool) {
        let slot = big_page_index / CONTINUOUS_BITS;
        let sub_index = big_page_index % CONTINUOUS_BITS;
        let continuous_mask = self.big_page_continuous[slot];
        self.big_page_continuous[slot] =
            (!(1u64 << sub_index) & continuous_mask) | (if value { 1u64 << sub_index } else { 0 });
    }

    fn is_device_big_page_continuous(&self, current_dev_addr: u64) -> bool {
        let mut base_ptr = self.device_memory.get_pointer(current_dev_addr) as usize;
        if base_ptr == 0 {
            return false;
        }

        let mut start_dev_addr = current_dev_addr + self.page_size;
        while start_dev_addr < current_dev_addr + self.big_page_size {
            base_ptr += self.page_size as usize;
            let next_ptr = self.device_memory.get_pointer(start_dev_addr) as usize;
            if next_ptr == 0 || base_ptr != next_ptr {
                return false;
            }
            start_dev_addr += self.page_size;
        }
        true
    }

    // ── Page table operations ───────────────────────────────────────────

    /// Upstream: `PageTableOp<entry_type>(gpu_addr, dev_addr, size, kind)`.
    ///
    /// Operates on the small page table.
    fn page_table_op(
        &mut self,
        entry_type: EntryType,
        gpu_addr: u64,
        dev_addr: u64,
        size: u64,
        kind: PteKind,
    ) -> u64 {
        let page_size = self.page_size;
        if entry_type == EntryType::Mapped {
            self.page_table.reserve_range(gpu_addr, size as usize);
        }
        let mut offset = 0u64;
        while offset < size {
            let current_gpu_addr = gpu_addr + offset;
            let current_entry_type = self.get_entry_small(current_gpu_addr);
            self.set_entry_small(current_gpu_addr, entry_type);
            if current_entry_type != entry_type {
                self.notify_modify_gpu_memory(current_gpu_addr, page_size);
            }
            if entry_type == EntryType::Mapped {
                let current_dev_addr = dev_addr + offset;
                let index = self.page_entry_index_small(current_gpu_addr);
                let sub_value = (current_dev_addr >> CPU_PAGE_BITS) as u32;
                self.page_table[index] = sub_value;
            }
            offset += page_size;
        }
        self.kind_map.map(gpu_addr, gpu_addr + size, kind);
        // Lichii extension (memory_manager.cpp:167-168).
        self.update_continuous_runs(gpu_addr, size);
        self.map_serial.fetch_add(1, Ordering::Release);
        gpu_addr
    }

    /// Upstream: `BigPageTableOp<entry_type>(gpu_addr, dev_addr, size, kind)`.
    ///
    /// Operates on the big page table. The `check_contiguous` callback is used
    /// to determine if sub-pages within a big page are contiguous in host memory.
    /// When no host memory access is available, pass `None`.
    fn big_page_table_op(
        &mut self,
        entry_type: EntryType,
        gpu_addr: u64,
        dev_addr: u64,
        size: u64,
        kind: PteKind,
    ) -> u64 {
        let big_page_size = self.big_page_size;
        let mut offset = 0u64;
        while offset < size {
            let current_gpu_addr = gpu_addr + offset;
            let current_entry_type = self.get_entry_big(current_gpu_addr);
            self.set_entry_big(current_gpu_addr, entry_type);
            if current_entry_type != entry_type {
                self.notify_modify_gpu_memory(current_gpu_addr, big_page_size);
            }
            if entry_type == EntryType::Mapped {
                let current_dev_addr = dev_addr + offset;
                let index = self.page_entry_index_big(current_gpu_addr);
                let sub_value = (current_dev_addr >> CPU_PAGE_BITS) as u32;
                self.big_page_table_dev.set(index, sub_value);
                let is_continuous = self.is_device_big_page_continuous(current_dev_addr);
                self.set_big_page_continuous(index, is_continuous);
            }
            offset += big_page_size;
        }
        self.kind_map.map(gpu_addr, gpu_addr + size, kind);
        // Lichii extension (memory_manager.cpp:231-232).
        self.update_continuous_runs(gpu_addr, size);
        self.map_serial.fetch_add(1, Ordering::Release);
        gpu_addr
    }

    // ── Public map/unmap API ────────────────────────────────────────────

    /// Upstream: `MemoryManager::Map(gpu_addr, dev_addr, size, kind, is_big_pages)`.
    pub fn map_ex(
        &mut self,
        gpu_addr: u64,
        dev_addr: u64,
        size: u64,
        kind: PteKind,
        is_big_pages: bool,
    ) -> u64 {
        log::trace!(
            "gpu_mm: map GPU {:#x}..{:#x} -> DEV {:#x} kind={:?} big={}",
            gpu_addr,
            gpu_addr + size,
            dev_addr,
            kind,
            is_big_pages
        );
        if is_big_pages {
            self.big_page_table_op(EntryType::Mapped, gpu_addr, dev_addr, size, kind)
        } else {
            self.page_table_op(EntryType::Mapped, gpu_addr, dev_addr, size, kind)
        }
    }

    /// Simplified map for callers that pass kind as u32.
    pub fn map(&mut self, gpu_addr: u64, dev_addr: u64, size: u64, kind_raw: u32) {
        let kind = pte_kind_from_u32(kind_raw);
        // Decide big vs small pages based on whether address is below split_address.
        let is_big = gpu_addr < self.split_address;
        self.map_ex(gpu_addr, dev_addr, size, kind, is_big);
    }

    /// Upstream: `MemoryManager::MapSparse(gpu_addr, size, is_big_pages)`.
    pub fn map_sparse_ex(&mut self, gpu_addr: u64, size: u64, is_big_pages: bool) -> u64 {
        log::trace!(
            "gpu_mm: reserve GPU {:#x}..{:#x} big={}",
            gpu_addr,
            gpu_addr + size,
            is_big_pages
        );
        if is_big_pages {
            self.big_page_table_op(EntryType::Reserved, gpu_addr, 0, size, PteKind::INVALID)
        } else {
            self.page_table_op(EntryType::Reserved, gpu_addr, 0, size, PteKind::INVALID)
        }
    }

    /// Simplified map_sparse for legacy callers.
    pub fn map_sparse(&mut self, gpu_addr: u64, size: u64, _kind_raw: u32) {
        let is_big = gpu_addr < self.split_address;
        self.map_sparse_ex(gpu_addr, size, is_big);
    }

    /// Upstream: `MemoryManager::Unmap(gpu_addr, size)`.
    pub fn unmap(&mut self, gpu_addr: u64, size: u64) {
        if size == 0 {
            return;
        }
        log::trace!("gpu_mm: unmap GPU {:#x}..{:#x}", gpu_addr, gpu_addr + size);
        let ranges = self.get_submapped_device_ranges(gpu_addr, size);
        if let Some(buf) = self.deferred_rasterizer_ops.as_mut() {
            for (map_addr, map_size) in ranges {
                buf.push(DeferredRasterizerOp::UnmapMemory {
                    device_addr: map_addr,
                    size: map_size,
                });
            }
        } else {
            let _ = self.with_rasterizer_mut(|rasterizer| {
                for (map_addr, map_size) in ranges {
                    rasterizer.unmap_memory(map_addr, map_size);
                }
            });
        }
        self.big_page_table_op(EntryType::Free, gpu_addr, 0, size, PteKind::INVALID);
        self.page_table_op(EntryType::Free, gpu_addr, 0, size, PteKind::INVALID);
    }

    // ── Address translation ─────────────────────────────────────────────

    /// Upstream: `MemoryManager::GpuToCpuAddress(gpu_addr)`.
    ///
    /// Check big entries first (if below split_address), fall back to small entries.
    pub fn gpu_to_cpu_address(&self, gpu_addr: u64) -> Option<u64> {
        if gpu_addr >= self.address_space_size {
            return None;
        }
        if self.get_entry_big(gpu_addr) == EntryType::Mapped {
            let dev_addr_base = (self.big_page_table_dev[self.page_entry_index_big(gpu_addr)]
                as u64)
                << CPU_PAGE_BITS;
            return Some(dev_addr_base + (gpu_addr & self.big_page_mask));
        }
        if self.get_entry_small(gpu_addr) == EntryType::Mapped {
            let dev_addr_base =
                (self.page_table[self.page_entry_index_small(gpu_addr)] as u64) << CPU_PAGE_BITS;
            return Some(dev_addr_base + (gpu_addr & self.page_mask));
        }
        None
    }

    /// Upstream: `MemoryManager::GpuToCpuAddress(addr, size)`.
    ///
    /// Search pages in the range for the first mapped address.
    pub fn gpu_to_cpu_address_range(&self, addr: u64, size: u64) -> Option<u64> {
        let mut page_index = addr >> self.page_bits;
        let page_last = addr.wrapping_add(size).wrapping_add(self.page_size - 1) >> self.page_bits;
        while page_index < page_last {
            if let Some(page_addr) = self.gpu_to_cpu_address(page_index << self.page_bits) {
                return Some(page_addr);
            }
            page_index += 1;
        }
        None
    }

    /// Translate GPU VA — alias for gpu_to_cpu_address (backward compat).
    pub fn translate(&self, gpu_va: u64) -> Option<u64> {
        self.gpu_to_cpu_address(gpu_va)
    }

    // ── MemoryOperation (generic page walker) ───────────────────────────

    /// Walk big pages, calling the appropriate closure for each page's entry type.
    /// Returns true if a closure signaled early exit.
    ///
    /// Upstream: `MemoryOperation<true>(...)`.
    fn memory_operation_big<FM, FR, FU>(
        &self,
        gpu_src_addr: u64,
        size: u64,
        mut func_mapped: FM,
        mut func_reserved: FR,
        mut func_unmapped: FU,
    ) -> bool
    where
        FM: FnMut(usize, usize, usize) -> bool,
        FR: FnMut(usize, usize, usize) -> bool,
        FU: FnMut(usize, usize, usize) -> bool,
    {
        let mut remaining = size as usize;
        let mut page_index = (gpu_src_addr >> self.big_page_bits) as usize;
        let mut page_offset = (gpu_src_addr & self.big_page_mask) as usize;
        let mut current_address = gpu_src_addr;

        while remaining > 0 {
            let copy_amount = std::cmp::min(self.big_page_size as usize - page_offset, remaining);
            let entry = self.get_entry_big(current_address);
            let should_break = match entry {
                EntryType::Mapped => func_mapped(page_index, page_offset, copy_amount),
                EntryType::Reserved => func_reserved(page_index, page_offset, copy_amount),
                EntryType::Free => func_unmapped(page_index, page_offset, copy_amount),
            };
            if should_break {
                return true;
            }
            page_index += 1;
            page_offset = 0;
            remaining -= copy_amount;
            current_address += copy_amount as u64;
        }
        false
    }

    /// Walk small pages, calling the appropriate closure for each page's entry type.
    ///
    /// Upstream: `MemoryOperation<false>(...)`.
    fn memory_operation_small<FM, FR, FU>(
        &self,
        gpu_src_addr: u64,
        size: u64,
        mut func_mapped: FM,
        mut func_reserved: FR,
        mut func_unmapped: FU,
    ) -> bool
    where
        FM: FnMut(usize, usize, usize) -> bool,
        FR: FnMut(usize, usize, usize) -> bool,
        FU: FnMut(usize, usize, usize) -> bool,
    {
        let mut remaining = size as usize;
        let mut page_index = (gpu_src_addr >> self.page_bits) as usize;
        let mut page_offset = (gpu_src_addr & self.page_mask) as usize;
        let mut current_address = gpu_src_addr;

        while remaining > 0 {
            let copy_amount = std::cmp::min(self.page_size as usize - page_offset, remaining);
            let entry = self.get_entry_small(current_address);
            let should_break = match entry {
                EntryType::Mapped => func_mapped(page_index, page_offset, copy_amount),
                EntryType::Reserved => func_reserved(page_index, page_offset, copy_amount),
                EntryType::Free => func_unmapped(page_index, page_offset, copy_amount),
            };
            if should_break {
                return true;
            }
            page_index += 1;
            page_offset = 0;
            remaining -= copy_amount;
            current_address += copy_amount as u64;
        }
        false
    }

    /// Apply a callback to the mapped device-memory chunks produced by
    /// Eden's nested `MemoryOperation` walk, without coalescing adjacent
    /// pages. Returning true stops the walk like Eden's bool callbacks.
    fn for_each_mapped_device_segment(
        &self,
        gpu_addr: u64,
        size: u64,
        callback: impl FnMut(u64, u64) -> bool,
    ) -> bool {
        let callback = std::cell::RefCell::new(callback);

        self.memory_operation_big(
            gpu_addr,
            size,
            |page_index, offset, copy_amount| {
                let dev_addr =
                    ((self.big_page_table_dev[page_index] as u64) << CPU_PAGE_BITS) + offset as u64;
                callback.borrow_mut()(dev_addr, copy_amount as u64)
            },
            |_, _, _| false,
            |page_index, offset, copy_amount| {
                let base = ((page_index as u64) << self.big_page_bits) + offset as u64;
                self.memory_operation_small(
                    base,
                    copy_amount as u64,
                    |small_page_index, small_offset, small_copy_amount| {
                        let dev_addr = ((self.page_table[small_page_index] as u64)
                            << CPU_PAGE_BITS)
                            + small_offset as u64;
                        callback.borrow_mut()(dev_addr, small_copy_amount as u64)
                    },
                    |_, _, _| false,
                    |_, _, _| false,
                )
            },
        )
    }

    // ── Read/Write block operations ─────────────────────────────────────

    /// Reduced-fixture callback reader; production uses owner-backed `read_block*`.
    #[cfg(test)]
    pub fn read_with_callback(
        &self,
        gpu_src_addr: u64,
        dst: &mut [u8],
        read_cpu_mem: &dyn Fn(u64, &mut [u8]),
    ) {
        self.read_block_impl_unsafe(gpu_src_addr, dst, read_cpu_mem);
    }

    fn flush_device_segment(&self, dev_addr: u64, size: usize, which: CacheType) {
        let Some(handle) = self.rasterizer else {
            return;
        };
        // Upstream `ReadBlockImpl` is const but still flushes the rasterizer.
        // `RasterizerHandle` is the existing non-owning owner bridge for that
        // C++ pointer relationship.
        unsafe {
            handle.with_mut(|rasterizer| {
                rasterizer.flush_region(dev_addr, size as u64, which);
            });
        }
    }

    /// A download for a guest read not yet in guest memory would land over
    /// this write later: finish those it overlaps first.
    ///
    /// Lichii `MemoryManager::FinishReadsBefore` (memory_manager.cpp:550-562;
    /// async read-back, DIFF.md 2026-11-15). Its EDEN_NX2_READ_CHECK note is
    /// a diagnostic and is not ported.
    ///
    /// Lichii lands the downloads here (`FinishRead`), which takes the buffer
    /// cache's lock. ruzu's callers hold this memory manager's
    /// `ParkingLotMutex`, which buffer cache lock holders take too: the GPU
    /// thread, drawing with the buffer and texture caches locked, looks image
    /// views up through it (`TextureCacheBase::find_image_view_with_gpu_to_cpu`),
    /// while the fencing thread writes query results through it
    /// (`renderer_vulkan::query_cache::write_query_result`). Landing here
    /// deadlocked those two threads (stacks taken in a hung run,
    /// DIFF.md 2026-11-15). It calls `supersede_read_downloads` instead, which
    /// takes no buffer cache lock and leaves guest memory and the GPU-modified
    /// ranges as landing then writing would.
    pub(crate) fn finish_reads_before(&self, dev_addr: u64, size: usize) {
        if crate::rasterizer_download_area::G_READ_DOWNLOADS.load(Ordering::Relaxed) == 0 {
            return;
        }
        let Some(handle) = self.rasterizer else {
            return;
        };
        if !crate::rasterizer_download_area::read_downloads_may_cover(dev_addr, size as u64) {
            return;
        }
        unsafe {
            handle.with_mut(|rasterizer| {
                rasterizer.supersede_read_downloads(dev_addr, size as u64);
            });
        }
    }

    fn invalidate_device_segment(&self, dev_addr: u64, size: usize, which: CacheType) {
        let Some(handle) = self.rasterizer else {
            return;
        };
        unsafe {
            handle.with_mut(|rasterizer| {
                rasterizer.invalidate_region(dev_addr, size as u64, which);
            });
        }
    }

    /// Upstream: `MemoryManager::ReadBlockImpl<true>(...)`.
    fn read_block_impl_safe(
        &self,
        gpu_src_addr: u64,
        dst: &mut [u8],
        which: CacheType,
        read_cpu_mem: &dyn Fn(u64, &mut [u8]),
    ) {
        let size = dst.len();
        let mut dst_offset = 0usize;
        let mut remaining = size;
        let mut page_index = (gpu_src_addr >> self.big_page_bits) as usize;
        let mut page_offset = (gpu_src_addr & self.big_page_mask) as usize;
        let mut current_address = gpu_src_addr;

        while remaining > 0 {
            let copy_amount = std::cmp::min(self.big_page_size as usize - page_offset, remaining);
            let entry = self.get_entry_big(current_address);

            match entry {
                EntryType::Mapped => {
                    let dev_addr_base =
                        (self.big_page_table_dev[page_index] as u64) << CPU_PAGE_BITS;
                    let dev_addr = dev_addr_base + page_offset as u64;
                    self.flush_device_segment(dev_addr, copy_amount, which);
                    read_cpu_mem(dev_addr, &mut dst[dst_offset..dst_offset + copy_amount]);
                    dst_offset += copy_amount;
                }
                EntryType::Reserved => {
                    dst[dst_offset..dst_offset + copy_amount].fill(0);
                    dst_offset += copy_amount;
                }
                EntryType::Free => {
                    let base = (page_index as u64) << self.big_page_bits | page_offset as u64;
                    self.read_small_pages_safe(
                        base,
                        copy_amount,
                        &mut dst[dst_offset..dst_offset + copy_amount],
                        which,
                        read_cpu_mem,
                    );
                    dst_offset += copy_amount;
                }
            }
            page_index += 1;
            page_offset = 0;
            remaining -= copy_amount;
            current_address += copy_amount as u64;
        }
    }

    /// Internal unsafe read implementation with two-level page walk.
    fn read_block_impl_unsafe(
        &self,
        gpu_src_addr: u64,
        dst: &mut [u8],
        read_cpu_mem: &dyn Fn(u64, &mut [u8]),
    ) {
        let size = dst.len();
        let mut dst_offset = 0usize;
        let mut remaining = size;
        let mut page_index = (gpu_src_addr >> self.big_page_bits) as usize;
        let mut page_offset = (gpu_src_addr & self.big_page_mask) as usize;
        let mut current_address = gpu_src_addr;

        while remaining > 0 {
            let copy_amount = std::cmp::min(self.big_page_size as usize - page_offset, remaining);
            let entry = self.get_entry_big(current_address);

            match entry {
                EntryType::Mapped => {
                    let dev_addr_base =
                        (self.big_page_table_dev[page_index] as u64) << CPU_PAGE_BITS;
                    let dev_addr = dev_addr_base + page_offset as u64;
                    // For big pages that are not continuous, read sub-pages individually.
                    // For continuous big pages, read the whole chunk.
                    // Since we use closure-based CPU access, just read directly.
                    read_cpu_mem(dev_addr, &mut dst[dst_offset..dst_offset + copy_amount]);
                    dst_offset += copy_amount;
                }
                EntryType::Reserved => {
                    // Reserved (sparse) — fill with zeros.
                    dst[dst_offset..dst_offset + copy_amount].fill(0);
                    dst_offset += copy_amount;
                }
                EntryType::Free => {
                    // Unmapped in big table — fall back to small pages.
                    let base = (page_index as u64) << self.big_page_bits | page_offset as u64;
                    self.read_small_pages(
                        base,
                        copy_amount,
                        &mut dst[dst_offset..dst_offset + copy_amount],
                        read_cpu_mem,
                    );
                    dst_offset += copy_amount;
                }
            }
            page_index += 1;
            page_offset = 0;
            remaining -= copy_amount;
            current_address += copy_amount as u64;
        }
    }

    /// Read using small page table entries.
    fn read_small_pages_safe(
        &self,
        gpu_addr: u64,
        size: usize,
        dst: &mut [u8],
        which: CacheType,
        read_cpu_mem: &dyn Fn(u64, &mut [u8]),
    ) {
        let mut dst_offset = 0usize;
        let mut remaining = size;
        let mut page_index = (gpu_addr >> self.page_bits) as usize;
        let mut page_offset = (gpu_addr & self.page_mask) as usize;
        let mut current_address = gpu_addr;

        while remaining > 0 {
            let copy_amount = std::cmp::min(self.page_size as usize - page_offset, remaining);
            let entry = self.get_entry_small(current_address);

            match entry {
                EntryType::Mapped => {
                    let dev_addr_base = (self.page_table[page_index] as u64) << CPU_PAGE_BITS;
                    let dev_addr = dev_addr_base + page_offset as u64;
                    self.flush_device_segment(dev_addr, copy_amount, which);
                    read_cpu_mem(dev_addr, &mut dst[dst_offset..dst_offset + copy_amount]);
                }
                _ => {
                    if remaining > 0 {
                        static UNMAPPED_COUNT: AtomicUsize = AtomicUsize::new(0);
                        let c = UNMAPPED_COUNT.fetch_add(1, Ordering::Relaxed);
                        if c < 10 {
                            log::warn!(
                                "gpu_mm::read: unmapped GPU VA {:#x} (entry={:?})",
                                current_address,
                                entry
                            );
                        }
                    }
                    dst[dst_offset..dst_offset + copy_amount].fill(0);
                }
            }
            dst_offset += copy_amount;
            page_index += 1;
            page_offset = 0;
            remaining -= copy_amount;
            current_address += copy_amount as u64;
        }
    }

    /// Read using small page table entries.
    fn read_small_pages(
        &self,
        gpu_addr: u64,
        size: usize,
        dst: &mut [u8],
        read_cpu_mem: &dyn Fn(u64, &mut [u8]),
    ) {
        let mut dst_offset = 0usize;
        let mut remaining = size;
        let mut page_index = (gpu_addr >> self.page_bits) as usize;
        let mut page_offset = (gpu_addr & self.page_mask) as usize;
        let mut current_address = gpu_addr;

        while remaining > 0 {
            let copy_amount = std::cmp::min(self.page_size as usize - page_offset, remaining);
            let entry = self.get_entry_small(current_address);

            match entry {
                EntryType::Mapped => {
                    let dev_addr_base = (self.page_table[page_index] as u64) << CPU_PAGE_BITS;
                    let dev_addr = dev_addr_base + page_offset as u64;
                    read_cpu_mem(dev_addr, &mut dst[dst_offset..dst_offset + copy_amount]);
                }
                _ => {
                    // Reserved or Free — fill with zeros.
                    if remaining > 0 {
                        note_sparse_traffic(
                            &SPARSE_READ_ZEROED_BYTES,
                            copy_amount,
                            "reads of sparse/unmapped pages",
                        );
                        static UNMAPPED_COUNT: AtomicUsize = AtomicUsize::new(0);
                        let c = UNMAPPED_COUNT.fetch_add(1, Ordering::Relaxed);
                        if c < 10 {
                            log::warn!(
                                "gpu_mm::read: unmapped GPU VA {:#x} (entry={:?})",
                                current_address,
                                entry
                            );
                        }
                    }
                    dst[dst_offset..dst_offset + copy_amount].fill(0);
                }
            }
            dst_offset += copy_amount;
            page_index += 1;
            page_offset = 0;
            remaining -= copy_amount;
            current_address += copy_amount as u64;
        }
    }

    /// Reduced-fixture callback writer; production uses owner-backed `write_block*`.
    #[cfg(test)]
    pub fn write_with_callback(
        &self,
        gpu_dest_addr: u64,
        src: &[u8],
        write_cpu_mem: &mut dyn FnMut(u64, &[u8]),
    ) {
        self.write_block_impl(gpu_dest_addr, src, write_cpu_mem);
    }

    fn write_block_impl(
        &self,
        gpu_dest_addr: u64,
        src: &[u8],
        write_cpu_mem: &mut dyn FnMut(u64, &[u8]),
    ) {
        let size = src.len();
        let mut src_offset = 0usize;
        let mut remaining = size;
        let mut page_index = (gpu_dest_addr >> self.big_page_bits) as usize;
        let mut page_offset = (gpu_dest_addr & self.big_page_mask) as usize;
        let mut current_address = gpu_dest_addr;

        while remaining > 0 {
            let copy_amount = std::cmp::min(self.big_page_size as usize - page_offset, remaining);
            let entry = self.get_entry_big(current_address);

            match entry {
                EntryType::Mapped => {
                    let dev_addr_base =
                        (self.big_page_table_dev[page_index] as u64) << CPU_PAGE_BITS;
                    let dev_addr = dev_addr_base + page_offset as u64;
                    self.finish_reads_before(dev_addr, copy_amount);
                    write_cpu_mem(dev_addr, &src[src_offset..src_offset + copy_amount]);
                    src_offset += copy_amount;
                }
                EntryType::Reserved => {
                    // Reserved (sparse) — the write is dropped, so a later read
                    // of this page returns zeros instead of what was stored.
                    note_sparse_traffic(
                        &SPARSE_WRITE_DROPPED_BYTES,
                        copy_amount,
                        "writes dropped on sparse pages",
                    );
                    src_offset += copy_amount;
                }
                EntryType::Free => {
                    // Unmapped in big table — fall back to small pages.
                    let base = (page_index as u64) << self.big_page_bits | page_offset as u64;
                    self.write_small_pages(
                        base,
                        &src[src_offset..src_offset + copy_amount],
                        write_cpu_mem,
                    );
                    src_offset += copy_amount;
                }
            }
            page_index += 1;
            page_offset = 0;
            remaining -= copy_amount;
            current_address += copy_amount as u64;
        }
    }

    fn write_block_impl_safe(
        &self,
        gpu_dest_addr: u64,
        src: &[u8],
        which: CacheType,
        write_cpu_mem: &mut dyn FnMut(u64, &[u8]),
    ) {
        let size = src.len();
        let mut src_offset = 0usize;
        let mut remaining = size;
        let mut page_index = (gpu_dest_addr >> self.big_page_bits) as usize;
        let mut page_offset = (gpu_dest_addr & self.big_page_mask) as usize;
        let mut current_address = gpu_dest_addr;

        while remaining > 0 {
            let copy_amount = std::cmp::min(self.big_page_size as usize - page_offset, remaining);
            let entry = self.get_entry_big(current_address);

            match entry {
                EntryType::Mapped => {
                    let dev_addr_base =
                        (self.big_page_table_dev[page_index] as u64) << CPU_PAGE_BITS;
                    let dev_addr = dev_addr_base + page_offset as u64;
                    self.finish_reads_before(dev_addr, copy_amount);
                    self.invalidate_device_segment(dev_addr, copy_amount, which);
                    write_cpu_mem(dev_addr, &src[src_offset..src_offset + copy_amount]);
                    src_offset += copy_amount;
                }
                EntryType::Reserved => {
                    src_offset += copy_amount;
                }
                EntryType::Free => {
                    let base = (page_index as u64) << self.big_page_bits | page_offset as u64;
                    self.write_small_pages_safe(
                        base,
                        &src[src_offset..src_offset + copy_amount],
                        which,
                        write_cpu_mem,
                    );
                    src_offset += copy_amount;
                }
            }
            page_index += 1;
            page_offset = 0;
            remaining -= copy_amount;
            current_address += copy_amount as u64;
        }
    }

    fn write_small_pages_safe(
        &self,
        gpu_addr: u64,
        src: &[u8],
        which: CacheType,
        write_cpu_mem: &mut dyn FnMut(u64, &[u8]),
    ) {
        let mut src_offset = 0usize;
        let mut remaining = src.len();
        let mut page_index = (gpu_addr >> self.page_bits) as usize;
        let mut page_offset = (gpu_addr & self.page_mask) as usize;
        let mut current_address = gpu_addr;

        while remaining > 0 {
            let copy_amount = std::cmp::min(self.page_size as usize - page_offset, remaining);
            let entry = self.get_entry_small(current_address);

            if entry == EntryType::Mapped {
                let dev_addr_base = (self.page_table[page_index] as u64) << CPU_PAGE_BITS;
                let dev_addr = dev_addr_base + page_offset as u64;
                self.finish_reads_before(dev_addr, copy_amount);
                self.invalidate_device_segment(dev_addr, copy_amount, which);
                write_cpu_mem(dev_addr, &src[src_offset..src_offset + copy_amount]);
            }
            src_offset += copy_amount;
            page_index += 1;
            page_offset = 0;
            remaining -= copy_amount;
            current_address += copy_amount as u64;
        }
    }

    fn write_small_pages(
        &self,
        gpu_addr: u64,
        src: &[u8],
        write_cpu_mem: &mut dyn FnMut(u64, &[u8]),
    ) {
        let mut src_offset = 0usize;
        let mut remaining = src.len();
        let mut page_index = (gpu_addr >> self.page_bits) as usize;
        let mut page_offset = (gpu_addr & self.page_mask) as usize;
        let mut current_address = gpu_addr;

        while remaining > 0 {
            let copy_amount = std::cmp::min(self.page_size as usize - page_offset, remaining);
            let entry = self.get_entry_small(current_address);

            if entry == EntryType::Mapped {
                let dev_addr_base = (self.page_table[page_index] as u64) << CPU_PAGE_BITS;
                let dev_addr = dev_addr_base + page_offset as u64;
                self.finish_reads_before(dev_addr, copy_amount);
                write_cpu_mem(dev_addr, &src[src_offset..src_offset + copy_amount]);
            }
            // For Reserved/Free, just skip (advance src).
            src_offset += copy_amount;
            page_index += 1;
            page_offset = 0;
            remaining -= copy_amount;
            current_address += copy_amount as u64;
        }
    }

    // ── Block read/write public API ─────────────────────────────────────

    /// Reduced-fixture callback variant of `MemoryManager::ReadBlock`.
    #[cfg(test)]
    pub fn read_block_with_callback(
        &self,
        gpu_src: u64,
        output: &mut [u8],
        read_cpu: &dyn Fn(u64, &mut [u8]),
    ) {
        self.read_block_impl_safe(gpu_src, output, CacheType::ALL, read_cpu);
    }

    #[cfg(test)]
    pub fn read_block_with_cache_type_and_callback(
        &self,
        gpu_src: u64,
        output: &mut [u8],
        which: CacheType,
        read_cpu: &dyn Fn(u64, &mut [u8]),
    ) {
        self.read_block_impl_safe(gpu_src, output, which, read_cpu);
    }

    /// Reduced-fixture callback variant of `MemoryManager::ReadBlockUnsafe`.
    #[cfg(test)]
    pub fn read_block_unsafe_with_callback(
        &self,
        gpu_src: u64,
        output: &mut [u8],
        read_cpu: &dyn Fn(u64, &mut [u8]),
    ) {
        self.read_with_callback(gpu_src, output, read_cpu);
    }

    /// Reduced-fixture callback variant of `MemoryManager::WriteBlock`.
    #[cfg(test)]
    pub fn write_block_with_callback(
        &self,
        gpu_dest: u64,
        input: &[u8],
        write_cpu: &mut dyn FnMut(u64, &[u8]),
    ) {
        self.write_block_impl_safe(gpu_dest, input, CacheType::ALL, write_cpu);
    }

    /// Reduced-fixture callback variant of `MemoryManager::WriteBlockUnsafe`.
    #[cfg(test)]
    pub fn write_block_unsafe_with_callback(
        &self,
        gpu_dest: u64,
        input: &[u8],
        write_cpu: &mut dyn FnMut(u64, &[u8]),
    ) {
        self.write_with_callback(gpu_dest, input, write_cpu);
    }

    /// Reduced-fixture callback variant of `MemoryManager::WriteBlockCached`.
    #[cfg(test)]
    pub fn write_block_cached_with_callback(
        &mut self,
        gpu_dest: u64,
        input: &[u8],
        write_cpu: &mut dyn FnMut(u64, &[u8]),
    ) {
        self.write_with_callback(gpu_dest, input, write_cpu);
        self.accumulator.add(gpu_dest, input.len());
    }

    /// Upstream: `MemoryManager::ReadBlock(gpu_src, output, size)`.
    pub fn read_block(&self, gpu_src: u64, output: &mut [u8]) -> bool {
        self.read_block_with_cache_type(gpu_src, output, CacheType::ALL)
    }

    pub fn read_block_with_cache_type(
        &self,
        gpu_src: u64,
        output: &mut [u8],
        which: CacheType,
    ) -> bool {
        self.read_block_impl_safe(gpu_src, output, which, &|addr, output| {
            self.device_memory.smmu_read_block_unsafe(addr, output);
        });
        true
    }

    /// Upstream: `MemoryManager::ReadBlockUnsafe(gpu_src, output, size)`.
    pub fn read_block_unsafe(&self, gpu_src: u64, output: &mut [u8]) -> bool {
        self.read_block_impl_unsafe(gpu_src, output, &|addr, output| {
            self.device_memory.smmu_read_block_unsafe(addr, output);
        });
        true
    }

    /// Upstream: `MemoryManager::WriteBlock(gpu_dest, input, size)`.
    pub fn write_block(&self, gpu_dest: u64, input: &[u8]) -> bool {
        self.write_block_with_cache_type(gpu_dest, input, CacheType::ALL)
    }

    pub fn write_block_with_cache_type(
        &self,
        gpu_dest: u64,
        input: &[u8],
        which: CacheType,
    ) -> bool {
        self.write_block_impl_safe(gpu_dest, input, which, &mut |addr, data| {
            self.device_memory.smmu_write_block_unsafe(addr, data);
        });
        true
    }

    /// Upstream: `MemoryManager::WriteBlockUnsafe(gpu_dest, input, size)`.
    pub fn write_block_unsafe(&self, gpu_dest: u64, input: &[u8]) -> bool {
        let trace = gpu_va_trace_enabled();
        self.write_block_impl(gpu_dest, input, &mut |addr, data| {
            if trace {
                let head = data
                    .iter()
                    .take(4)
                    .enumerate()
                    .fold(0u32, |acc, (idx, byte)| acc | ((*byte as u32) << (idx * 8)));
                log::info!(
                    "[GPU_VA_WRITEBLOCK] gpu_va=0x{:X} cpu=0x{:X} size={} head_u32=0x{:X}",
                    gpu_dest,
                    addr,
                    data.len(),
                    head
                );
            }
            self.device_memory.smmu_write_block_unsafe(addr, data);
        });
        true
    }

    /// Upstream: `MemoryManager::WriteBlockCached(gpu_dest, input, size)`.
    pub fn write_block_cached(&mut self, gpu_dest: u64, input: &[u8]) -> bool {
        self.write_block_unsafe(gpu_dest, input);
        self.accumulator.add(gpu_dest, input.len());
        true
    }

    /// Leaves the caches' invalidation of a range already written for the
    /// next `flush_caching` (before every draw, dispatch, clear and copy),
    /// merged with neighbouring ranges.
    ///
    /// Lichii `MemoryManager::AccumulateInvalidation` (memory_manager.h:202-207),
    /// absent from Eden.
    pub fn accumulate_invalidation(&mut self, gpu_addr: u64, size: usize) {
        self.accumulator.add(gpu_addr, size);
    }

    /// Constant buffer data to device memory the caller translated.
    /// `flush_caching` hands the written ranges, merged, to the rasterizer's
    /// `invalidate_constant_buffer_data` instead of `inner_invalidation`.
    ///
    /// Lichii `MemoryManager::WriteConstantBufferData`
    /// (memory_manager.cpp:643-667), absent from Eden. Lichii's unmapped
    /// fallback `WriteKept` (sparse memory keeping) has no ruzu counterpart:
    /// an unmapped page goes through `smmu_write_block_unsafe`, which logs it,
    /// as ruzu's `write_block_unsafe` does.
    pub fn write_constant_buffer_data(&mut self, device_addr: u64, data: &[u8]) {
        let size = data.len();
        if size == 0 {
            return;
        }
        self.finish_reads_before(device_addr, size);
        let end = device_addr + size as u64;
        let mut address = device_addr;
        let mut src = 0usize;
        while address < end {
            let amount = (end - address).min(DEVICE_PAGE_SIZE - (address & DEVICE_PAGE_MASK))
                as usize;
            let physical = self.device_memory.get_pointer_mut(address);
            if !physical.is_null() {
                // SAFETY: `get_pointer_mut` returned the host page holding
                // `[address, address + amount)`, which stays within it.
                unsafe {
                    std::ptr::copy_nonoverlapping(data.as_ptr().add(src), physical, amount);
                }
            } else {
                self.device_memory
                    .smmu_write_block_unsafe(address, &data[src..src + amount]);
            }
            address += amount as u64;
            src += amount;
        }
        if let Some((last, last_size)) = self.constant_buffer_writes.last_mut() {
            let last_end = *last + *last_size as u64;
            if device_addr <= last_end && *last <= end {
                let merged_end = last_end.max(end);
                *last = (*last).min(device_addr);
                *last_size = (merged_end - *last) as usize;
                return;
            }
        }
        self.constant_buffer_writes.push((device_addr, size));
    }

    /// Upstream-owned copy path using the stored `MaxwellDeviceMemoryManager`.
    pub fn copy_block(&mut self, gpu_dest: u64, gpu_src: u64, size: u64) -> bool {
        self.copy_block_with_cache_type(gpu_dest, gpu_src, size, CacheType::ALL)
    }

    pub fn copy_block_with_cache_type(
        &mut self,
        gpu_dest: u64,
        gpu_src: u64,
        size: u64,
        which: CacheType,
    ) -> bool {
        let mut tmp = vec![0u8; size as usize];
        self.read_block(gpu_src, &mut tmp);
        self.flush_region_with_cache_type(gpu_dest, size, which);
        self.write_block(gpu_dest, &tmp);
        true
    }

    // ── Query methods ───────────────────────────────────────────────────

    /// Upstream: `MemoryManager::IsGranularRange(gpu_addr, size)`.
    ///
    /// Checks if a gpu region can be simply read with a pointer (fits in one page).
    pub fn is_granular_range(&self, gpu_addr: u64, size: u64) -> bool {
        if self.get_entry_big(gpu_addr) == EntryType::Mapped {
            let page_index = (gpu_addr >> self.big_page_bits) as usize;
            if self.is_big_page_continuous(page_index) {
                let page = ((page_index as u64) & self.big_page_mask) + size;
                return page <= self.big_page_size;
            }
            let page = (gpu_addr & DEVICE_PAGE_MASK) + size;
            return page <= DEVICE_PAGE_SIZE;
        }
        if self.get_entry_small(gpu_addr) != EntryType::Mapped {
            return false;
        }
        let page = (gpu_addr & DEVICE_PAGE_MASK) + size;
        page <= DEVICE_PAGE_SIZE
    }

    /// Upstream: `MemoryManager::IsContinuousRange(gpu_addr, size)`.
    pub fn is_continuous_range(&self, gpu_addr: u64, size: u64) -> bool {
        let mut old_page_addr: Option<u64> = None;
        let mut result = true;

        let big_page_bits = self.big_page_bits;
        let page_bits = self.page_bits;

        // We implement the two-level walk inline to avoid borrow issues.
        let mut remaining = size as usize;
        let mut big_page_index = (gpu_addr >> big_page_bits) as usize;
        let mut big_page_offset = (gpu_addr & self.big_page_mask) as usize;
        let mut current_address = gpu_addr;

        while remaining > 0 && result {
            let copy_amount =
                std::cmp::min(self.big_page_size as usize - big_page_offset, remaining);
            let entry = self.get_entry_big(current_address);

            match entry {
                EntryType::Mapped => {
                    let dev_addr_base =
                        (self.big_page_table_dev[big_page_index] as u64) << CPU_PAGE_BITS;
                    let dev_addr = dev_addr_base + big_page_offset as u64;
                    if let Some(expected) = old_page_addr {
                        if expected != dev_addr {
                            result = false;
                            break;
                        }
                    }
                    old_page_addr = Some(dev_addr + copy_amount as u64);
                }
                EntryType::Reserved => {
                    result = false;
                    break;
                }
                EntryType::Free => {
                    // Fall back to small pages.
                    let base = (big_page_index as u64) << big_page_bits | big_page_offset as u64;
                    let mut sm_remaining = copy_amount;
                    let mut sm_page_index = (base >> page_bits) as usize;
                    let mut sm_page_offset = (base & self.page_mask) as usize;
                    let mut sm_current = base;

                    while sm_remaining > 0 && result {
                        let sm_copy =
                            std::cmp::min(self.page_size as usize - sm_page_offset, sm_remaining);
                        let sm_entry = self.get_entry_small(sm_current);
                        match sm_entry {
                            EntryType::Mapped => {
                                let dev_addr_base =
                                    (self.page_table[sm_page_index] as u64) << CPU_PAGE_BITS;
                                let dev_addr = dev_addr_base + sm_page_offset as u64;
                                if let Some(expected) = old_page_addr {
                                    if expected != dev_addr {
                                        result = false;
                                        break;
                                    }
                                }
                                old_page_addr = Some(dev_addr + sm_copy as u64);
                            }
                            _ => {
                                result = false;
                                break;
                            }
                        }
                        sm_page_index += 1;
                        sm_page_offset = 0;
                        sm_remaining -= sm_copy;
                        sm_current += sm_copy as u64;
                    }
                }
            }
            big_page_index += 1;
            big_page_offset = 0;
            remaining -= copy_amount;
            current_address += copy_amount as u64;
        }
        result
    }

    /// Upstream: `MemoryManager::IsFullyMappedRange(gpu_addr, size)`.
    pub fn is_fully_mapped_range(&self, gpu_addr: u64, size: u64) -> bool {
        let mut result = true;
        let big_page_bits = self.big_page_bits;

        let mut remaining = size as usize;
        let mut big_page_index = (gpu_addr >> big_page_bits) as usize;
        let mut big_page_offset = (gpu_addr & self.big_page_mask) as usize;
        let mut current_address = gpu_addr;

        while remaining > 0 && result {
            let copy_amount =
                std::cmp::min(self.big_page_size as usize - big_page_offset, remaining);
            let entry = self.get_entry_big(current_address);

            match entry {
                EntryType::Mapped => { /* pass */ }
                EntryType::Reserved => {
                    result = false;
                    break;
                }
                EntryType::Free => {
                    // Check small pages.
                    let base = (big_page_index as u64) << big_page_bits | big_page_offset as u64;
                    let mut sm_remaining = copy_amount;
                    let mut sm_page_offset = (base & self.page_mask) as usize;
                    let mut sm_current = base;

                    while sm_remaining > 0 && result {
                        let sm_copy =
                            std::cmp::min(self.page_size as usize - sm_page_offset, sm_remaining);
                        let sm_entry = self.get_entry_small(sm_current);
                        match sm_entry {
                            EntryType::Mapped | EntryType::Reserved => { /* pass */ }
                            EntryType::Free => {
                                result = false;
                                break;
                            }
                        }
                        sm_page_offset = 0;
                        sm_remaining -= sm_copy;
                        sm_current += sm_copy as u64;
                    }
                }
            }
            big_page_index += 1;
            big_page_offset = 0;
            remaining -= copy_amount;
            current_address += copy_amount as u64;
        }
        result
    }

    /// Lichii: `MemoryManager::MaxContinuousRange(gpu_addr, size)` (memory_manager.cpp:719-743).
    ///
    /// Eden walks the page tables here; storage buffer binds ask for up to 8 MiB on each bind,
    /// so this answers from the runs kept at map time (`update_continuous_runs`) instead.
    /// Lichii also returns 0 when `size > address_space_size - gpu_addr`; ruzu's walk
    /// (`max_continuous_range_uncached`) answers such ranges that end their run inside the
    /// address space, so that guard is not ported and both stay equal there. Lichii's
    /// `Nx2Perf::Verify` cross-check is not ported.
    pub fn max_continuous_range(&self, gpu_addr: u64, size: u64) -> u64 {
        if !self.is_within_gpu_address_range(gpu_addr) {
            return 0;
        }
        self.current_snapshot(|snapshot| {
            let runs = &snapshot.runs;
            let index = runs.partition_point(|run| run.begin <= gpu_addr);
            if index != 0 && gpu_addr < runs[index - 1].end {
                std::cmp::min(size, runs[index - 1].end - gpu_addr)
            } else {
                0
            }
        })
    }

    /// Lichii: `MemoryManager::UpdateContinuousRuns(gpu_addr, size)` (memory_manager.cpp:745-800).
    ///
    /// Rebuilds the runs through the big pages a page table operation touched.
    fn update_continuous_runs(&mut self, gpu_addr: u64, size: u64) {
        // Every entry the operation set lies in these big pages; the walk reads nothing else there.
        let begin = gpu_addr & !self.big_page_mask;
        let end = std::cmp::min(
            (gpu_addr + size + self.big_page_mask) & !self.big_page_mask,
            self.address_space_size,
        );
        if size == 0 || begin >= end {
            return;
        }
        let mut rebuilt: Vec<(u64, ContinuousRun)> = Vec::new();
        let add = |rebuilt: &mut Vec<(u64, ContinuousRun)>,
                   address: u64,
                   length: u64,
                   device_addr: u64| {
            if let Some((run_begin, run)) = rebuilt.last_mut() {
                if run.end == address && run.device_addr + (run.end - *run_begin) == device_addr {
                    run.end += length;
                    return;
                }
            }
            rebuilt.push((
                address,
                ContinuousRun {
                    end: address + length,
                    device_addr,
                },
            ));
        };
        let mut state = self.runs_guard.lock();
        let continuous_runs = &mut state.continuous_runs;
        // Runs that overlap or touch [begin, end) are cut at its edges and merged again.
        let first = match continuous_runs.range(..begin).next_back() {
            Some((&run_begin, run)) if run.end >= begin => run_begin,
            _ => begin,
        };
        let touched: Vec<(u64, ContinuousRun)> = continuous_runs
            .range(first..=end)
            .map(|(&run_begin, &run)| (run_begin, run))
            .collect();
        if let Some(&(run_begin, run)) = touched.first() {
            if run_begin < begin {
                add(&mut rebuilt, run_begin, begin - run_begin, run.device_addr);
            }
        }
        let mut tail: Option<(u64, ContinuousRun)> = None;
        for &(run_begin, run) in &touched {
            if run.end > end {
                tail = Some((
                    end,
                    ContinuousRun {
                        end: run.end,
                        device_addr: run.device_addr + (end - run_begin),
                    },
                ));
            }
            continuous_runs.remove(&run_begin);
        }
        let mut page = begin;
        while page < end {
            match self.get_entry_big(page) {
                EntryType::Mapped => {
                    let device_addr = (self.big_page_table_dev[self.page_entry_index_big(page)]
                        as u64)
                        << CPU_PAGE_BITS;
                    add(&mut rebuilt, page, self.big_page_size, device_addr);
                }
                EntryType::Free => {
                    let mut small = page;
                    while small < page + self.big_page_size {
                        if self.get_entry_small(small) == EntryType::Mapped {
                            let device_addr = (self.page_table
                                [self.page_entry_index_small(small)]
                                as u64)
                                << CPU_PAGE_BITS;
                            add(&mut rebuilt, small, self.page_size, device_addr);
                        }
                        small += self.page_size;
                    }
                }
                EntryType::Reserved => {}
            }
            page += self.big_page_size;
        }
        if let Some((tail_begin, tail_run)) = tail {
            add(
                &mut rebuilt,
                tail_begin,
                tail_run.end - tail_begin,
                tail_run.device_addr,
            );
        }
        for (run_begin, run) in rebuilt {
            continuous_runs.insert(run_begin, run);
        }
    }

    /// Lichii: `MemoryManager::CurrentSnapshot()` (memory_manager.cpp:802-831).
    ///
    /// Lichii returns a reference into the calling thread's view; Rust cannot hand out a
    /// reference into thread-local storage, so the snapshot is lent to `f` instead.
    fn current_snapshot<R>(&self, f: impl FnOnce(&MappingSnapshot) -> R) -> R {
        SNAPSHOT_VIEWS.with(|views| {
            let mut views = views.borrow_mut();
            let view = &mut views[self.snapshot_owner % 4];
            let serial = self.map_serial.load(Ordering::Acquire);
            if view.owner == self.snapshot_owner + 1 && view.serial == serial {
                if let Some(snapshot) = view.snapshot.as_deref() {
                    return f(snapshot);
                }
            }
            let snapshot = {
                let mut state = self.runs_guard.lock();
                if state.snapshot.is_none() || state.snapshot_serial != serial {
                    let mut next = MappingSnapshot {
                        runs: Vec::with_capacity(state.continuous_runs.len()),
                    };
                    for (&begin, run) in &state.continuous_runs {
                        next.runs.push(MappingSnapshotRun {
                            begin,
                            end: run.end,
                        });
                    }
                    state.snapshot = Some(Arc::new(next));
                    state.snapshot_serial = serial;
                }
                state.snapshot.clone()
            };
            *view = SnapshotView {
                owner: self.snapshot_owner + 1,
                serial,
                snapshot,
            };
            f(view.snapshot.as_deref().expect("snapshot was just built"))
        })
    }

    /// Lichii: `MemoryManager::MaxContinuousRangeUncached(gpu_addr, size)`
    /// (memory_manager.cpp:833-871): Eden's `MaxContinuousRange` page-table walk.
    pub fn max_continuous_range_uncached(&self, gpu_addr: u64, size: u64) -> u64 {
        let mut old_page_addr: Option<u64> = None;
        let mut range_so_far = 0u64;
        let mut done = false;

        let big_page_bits = self.big_page_bits;
        let page_bits = self.page_bits;

        let mut remaining = size as usize;
        let mut big_page_index = (gpu_addr >> big_page_bits) as usize;
        let mut big_page_offset = (gpu_addr & self.big_page_mask) as usize;
        let mut current_address = gpu_addr;

        while remaining > 0 && !done {
            let copy_amount =
                std::cmp::min(self.big_page_size as usize - big_page_offset, remaining);
            let entry = self.get_entry_big(current_address);

            match entry {
                EntryType::Mapped => {
                    let dev_addr_base =
                        (self.big_page_table_dev[big_page_index] as u64) << CPU_PAGE_BITS;
                    let dev_addr = dev_addr_base + big_page_offset as u64;
                    if let Some(expected) = old_page_addr {
                        if expected != dev_addr {
                            break;
                        }
                    }
                    range_so_far += copy_amount as u64;
                    old_page_addr = Some(dev_addr + copy_amount as u64);
                }
                EntryType::Reserved => {
                    break;
                }
                EntryType::Free => {
                    // Fall back to small pages.
                    let base = (big_page_index as u64) << big_page_bits | big_page_offset as u64;
                    let mut sm_remaining = copy_amount;
                    let mut sm_page_index = (base >> page_bits) as usize;
                    let mut sm_page_offset = (base & self.page_mask) as usize;
                    let mut sm_current = base;

                    while sm_remaining > 0 && !done {
                        let sm_copy =
                            std::cmp::min(self.page_size as usize - sm_page_offset, sm_remaining);
                        let sm_entry = self.get_entry_small(sm_current);
                        match sm_entry {
                            EntryType::Mapped => {
                                let dev_addr_base =
                                    (self.page_table[sm_page_index] as u64) << CPU_PAGE_BITS;
                                let dev_addr = dev_addr_base + sm_page_offset as u64;
                                if let Some(expected) = old_page_addr {
                                    if expected != dev_addr {
                                        done = true;
                                        break;
                                    }
                                }
                                range_so_far += sm_copy as u64;
                                old_page_addr = Some(dev_addr + sm_copy as u64);
                            }
                            _ => {
                                done = true;
                                break;
                            }
                        }
                        sm_page_index += 1;
                        sm_page_offset = 0;
                        sm_remaining -= sm_copy;
                        sm_current += sm_copy as u64;
                    }
                }
            }
            big_page_index += 1;
            big_page_offset = 0;
            remaining -= copy_amount;
            current_address += copy_amount as u64;
        }
        range_so_far
    }

    /// Upstream: `MemoryManager::GetSubmappedRange(gpu_addr, size)`.
    ///
    /// Returns GPU address ranges (not device addresses).
    pub fn get_submapped_range(&self, gpu_addr: u64, size: u64) -> Vec<(u64, u64)> {
        let mut result = Vec::new();
        let mut last_segment: Option<(u64, u64)> = None;
        let mut old_page_addr: Option<u64> = None;

        let big_page_bits = self.big_page_bits;
        let page_bits = self.page_bits;

        let split = |last_segment: &mut Option<(u64, u64)>, result: &mut Vec<(u64, u64)>| {
            if let Some(seg) = last_segment.take() {
                result.push(seg);
            }
        };

        let mut remaining = size as usize;
        let mut big_page_index = (gpu_addr >> big_page_bits) as usize;
        let mut big_page_offset = (gpu_addr & self.big_page_mask) as usize;
        let mut current_address = gpu_addr;

        while remaining > 0 {
            let copy_amount =
                std::cmp::min(self.big_page_size as usize - big_page_offset, remaining);
            let entry = self.get_entry_big(current_address);

            match entry {
                EntryType::Mapped => {
                    let dev_addr_base =
                        (self.big_page_table_dev[big_page_index] as u64) << CPU_PAGE_BITS;
                    let dev_addr = dev_addr_base + big_page_offset as u64;
                    if let Some(expected) = old_page_addr {
                        if expected != dev_addr {
                            split(&mut last_segment, &mut result);
                        }
                    }
                    old_page_addr = Some(dev_addr + copy_amount as u64);
                    let new_base_addr =
                        ((big_page_index as u64) << big_page_bits) + big_page_offset as u64;
                    if let Some(seg) = &mut last_segment {
                        seg.1 += copy_amount as u64;
                    } else {
                        last_segment = Some((new_base_addr, copy_amount as u64));
                    }
                }
                EntryType::Reserved => {
                    split(&mut last_segment, &mut result);
                    old_page_addr = None;
                }
                EntryType::Free => {
                    // Walk small pages.
                    let base = (big_page_index as u64) << big_page_bits | big_page_offset as u64;
                    let mut sm_remaining = copy_amount;
                    let mut sm_page_index = (base >> page_bits) as usize;
                    let mut sm_page_offset = (base & self.page_mask) as usize;
                    let mut sm_current = base;

                    while sm_remaining > 0 {
                        let sm_copy =
                            std::cmp::min(self.page_size as usize - sm_page_offset, sm_remaining);
                        let sm_entry = self.get_entry_small(sm_current);
                        match sm_entry {
                            EntryType::Mapped => {
                                let dev_addr_base =
                                    (self.page_table[sm_page_index] as u64) << CPU_PAGE_BITS;
                                let dev_addr = dev_addr_base + sm_page_offset as u64;
                                if let Some(expected) = old_page_addr {
                                    if expected != dev_addr {
                                        split(&mut last_segment, &mut result);
                                    }
                                }
                                old_page_addr = Some(dev_addr + sm_copy as u64);
                                let new_base_addr =
                                    ((sm_page_index as u64) << page_bits) + sm_page_offset as u64;
                                if let Some(seg) = &mut last_segment {
                                    seg.1 += sm_copy as u64;
                                } else {
                                    last_segment = Some((new_base_addr, sm_copy as u64));
                                }
                            }
                            _ => {
                                split(&mut last_segment, &mut result);
                                old_page_addr = None;
                            }
                        }
                        sm_page_index += 1;
                        sm_page_offset = 0;
                        sm_remaining -= sm_copy;
                        sm_current += sm_copy as u64;
                    }
                }
            }
            big_page_index += 1;
            big_page_offset = 0;
            remaining -= copy_amount;
            current_address += copy_amount as u64;
        }
        split(&mut last_segment, &mut result);
        result
    }

    /// Reduced-fixture callback variant of `MemoryManager::CopyBlock`.
    #[cfg(test)]
    pub fn copy_block_with_callback(
        &mut self,
        gpu_dest: u64,
        gpu_src: u64,
        size: u64,
        read_cpu: &dyn Fn(u64, &mut [u8]),
        write_cpu: &mut dyn FnMut(u64, &[u8]),
    ) {
        let mut tmp = vec![0u8; size as usize];
        self.read_with_callback(gpu_src, &mut tmp, read_cpu);
        self.flush_region(gpu_dest, size);
        self.write_with_callback(gpu_dest, &tmp, write_cpu);
    }

    /// Upstream: `MemoryManager::GetPageKind(gpu_addr)`.
    pub fn get_page_kind(&self, gpu_addr: u64) -> PteKind {
        self.kind_map.get_value_at(gpu_addr)
    }

    /// Upstream: `MemoryManager::GetMemoryLayoutSize(gpu_addr, max_size)`.
    pub fn get_memory_layout_size(&self, gpu_addr: u64, _max_size: u64) -> u64 {
        self.kind_map.get_continuous_size_from(gpu_addr) as u64
    }

    fn get_submapped_device_ranges(&self, gpu_addr: u64, size: u64) -> Vec<(u64, u64)> {
        let mut result = Vec::new();
        self.get_submapped_device_ranges_into(gpu_addr, size, &mut result);
        result
            .into_iter()
            .map(|(addr, size)| (addr, size as u64))
            .collect()
    }

    /// Upstream: `MemoryManager::GetSubmappedRangeImpl<false>(gpu_addr, size, result)`;
    /// appends the device ranges to the caller's `result` (Eden passes its
    /// reusable `page_stash`/`page_stash2` members).
    fn get_submapped_device_ranges_into(
        &self,
        gpu_addr: u64,
        size: u64,
        result: &mut Vec<(u64, usize)>,
    ) {
        let mut last_segment: Option<(u64, usize)> = None;
        let mut old_page_addr: Option<u64> = None;

        let split = |last_segment: &mut Option<(u64, usize)>, result: &mut Vec<(u64, usize)>| {
            if let Some(seg) = last_segment.take() {
                result.push(seg);
            }
        };

        let mut remaining = size as usize;
        let mut big_page_index = (gpu_addr >> self.big_page_bits) as usize;
        let mut big_page_offset = (gpu_addr & self.big_page_mask) as usize;
        let mut current_address = gpu_addr;

        while remaining > 0 {
            let copy_amount =
                std::cmp::min(self.big_page_size as usize - big_page_offset, remaining);
            match self.get_entry_big(current_address) {
                EntryType::Mapped => {
                    let dev_addr_base =
                        (self.big_page_table_dev[big_page_index] as u64) << CPU_PAGE_BITS;
                    let dev_addr = dev_addr_base + big_page_offset as u64;
                    if let Some(expected) = old_page_addr {
                        if expected != dev_addr {
                            split(&mut last_segment, result);
                        }
                    }
                    old_page_addr = Some(dev_addr + copy_amount as u64);
                    if let Some(seg) = &mut last_segment {
                        seg.1 += copy_amount;
                    } else {
                        last_segment = Some((dev_addr, copy_amount));
                    }
                }
                EntryType::Reserved => {
                    split(&mut last_segment, result);
                    old_page_addr = None;
                }
                EntryType::Free => {
                    let base =
                        (big_page_index as u64) << self.big_page_bits | big_page_offset as u64;
                    let mut sm_remaining = copy_amount;
                    let mut sm_page_index = (base >> self.page_bits) as usize;
                    let mut sm_page_offset = (base & self.page_mask) as usize;
                    let mut sm_current = base;
                    while sm_remaining > 0 {
                        let sm_copy =
                            std::cmp::min(self.page_size as usize - sm_page_offset, sm_remaining);
                        match self.get_entry_small(sm_current) {
                            EntryType::Mapped => {
                                let dev_addr_base =
                                    (self.page_table[sm_page_index] as u64) << CPU_PAGE_BITS;
                                let dev_addr = dev_addr_base + sm_page_offset as u64;
                                if let Some(expected) = old_page_addr {
                                    if expected != dev_addr {
                                        split(&mut last_segment, result);
                                    }
                                }
                                old_page_addr = Some(dev_addr + sm_copy as u64);
                                if let Some(seg) = &mut last_segment {
                                    seg.1 += sm_copy;
                                } else {
                                    last_segment = Some((dev_addr, sm_copy));
                                }
                            }
                            _ => {
                                split(&mut last_segment, result);
                                old_page_addr = None;
                            }
                        }
                        sm_page_index += 1;
                        sm_page_offset = 0;
                        sm_remaining -= sm_copy;
                        sm_current += sm_copy as u64;
                    }
                }
            }
            big_page_index += 1;
            big_page_offset = 0;
            remaining -= copy_amount;
            current_address += copy_amount as u64;
        }
        split(&mut last_segment, result);
    }

    // ── Cache/rasterizer operations ─────────────────────────────────────

    /// Upstream: `MemoryManager::FlushRegion(gpu_addr, size)`.
    pub fn flush_region(&mut self, gpu_addr: u64, size: u64) {
        self.flush_region_with_cache_type(gpu_addr, size, CacheType::ALL);
    }

    pub fn flush_region_with_cache_type(&mut self, gpu_addr: u64, size: u64, which: CacheType) {
        let Some(handle) = self.rasterizer else {
            return;
        };
        unsafe {
            handle.with_mut(|rasterizer| {
                self.for_each_mapped_device_segment(gpu_addr, size, |addr, map_size| {
                    rasterizer.flush_region(addr, map_size, which);
                    false
                });
            });
        }
    }

    /// Upstream: `MemoryManager::InvalidateRegion(gpu_addr, size)`.
    pub fn invalidate_region(&mut self, gpu_addr: u64, size: u64) {
        self.invalidate_region_with_cache_type(gpu_addr, size, CacheType::ALL);
    }

    pub fn invalidate_region_with_cache_type(
        &mut self,
        gpu_addr: u64,
        size: u64,
        which: CacheType,
    ) {
        let Some(handle) = self.rasterizer else {
            return;
        };
        unsafe {
            handle.with_mut(|rasterizer| {
                self.for_each_mapped_device_segment(gpu_addr, size, |addr, map_size| {
                    rasterizer.invalidate_region(addr, map_size, which);
                    false
                });
            });
        }
    }

    /// Upstream: `MemoryManager::FlushCaching()`.
    pub fn flush_caching(&mut self) {
        // `page_stash2` and the accumulator are moved out only so that the
        // range walk can borrow `self`; both keep their capacity across calls
        // like Eden's members.
        let mut page_stash2 = std::mem::take(&mut self.page_stash2);
        let mut accumulator = std::mem::take(&mut self.accumulator);
        let invalidated = accumulator.invalidate_all(|addr, size| {
            self.get_submapped_device_ranges_into(addr, size as u64, &mut page_stash2);
        });
        self.accumulator = accumulator;
        if invalidated {
            let _ = self.with_rasterizer_mut(|rasterizer| {
                rasterizer.inner_invalidation(&page_stash2);
            });
            page_stash2.clear();
        }
        self.page_stash2 = page_stash2;
        // Lichii MemoryManager::FlushCaching (memory_manager.cpp:1097-1109):
        // after the accumulator, the constant buffer data ranges.
        if !self.constant_buffer_writes.is_empty() {
            let constant_buffer_writes = std::mem::take(&mut self.constant_buffer_writes);
            let _ = self.with_rasterizer_mut(|rasterizer| {
                rasterizer.invalidate_constant_buffer_data(&constant_buffer_writes);
            });
            self.constant_buffer_writes = constant_buffer_writes;
            self.constant_buffer_writes.clear();
        }
    }

    /// Check if a GPU address is within the valid address range.
    ///
    /// Upstream: `MemoryManager::IsWithinGPUAddressRange(gpu_addr)`.
    pub fn is_within_gpu_address_range(&self, gpu_addr: u64) -> bool {
        gpu_addr < self.address_space_size
    }

    /// Upstream: `MemoryManager::IsMemoryDirty(gpu_addr, size)`.
    pub fn is_memory_dirty(&mut self, gpu_addr: u64, size: u64) -> bool {
        self.is_memory_dirty_with_cache_type(gpu_addr, size, CacheType::ALL)
    }

    pub fn is_memory_dirty_with_cache_type(
        &mut self,
        gpu_addr: u64,
        size: u64,
        which: CacheType,
    ) -> bool {
        let Some(handle) = self.rasterizer else {
            return false;
        };
        unsafe {
            handle.with_mut(|rasterizer| {
                self.for_each_mapped_device_segment(gpu_addr, size, |addr, map_size| {
                    rasterizer.must_flush_region(addr, map_size, which)
                })
            })
        }
    }

    /// Read a value of type `T` from GPU virtual address space.
    ///
    /// Reduced-fixture typed read; production callers should use owner-backed block reads.
    #[cfg(test)]
    pub fn read_value<T: Copy>(
        &self,
        gpu_addr: u64,
        read_cpu_mem: &dyn Fn(u64, &mut [u8]),
    ) -> Option<T> {
        let size = std::mem::size_of::<T>();
        if self.gpu_to_cpu_address(gpu_addr).is_none() {
            return None;
        }
        let mut bytes = vec![0u8; size];
        self.read_with_callback(gpu_addr, &mut bytes, read_cpu_mem);
        // Safety: T is Copy and we have exactly size_of::<T>() bytes.
        let value = unsafe { std::ptr::read(bytes.as_ptr() as *const T) };
        Some(value)
    }

    /// Reduced-fixture typed write; production callers should use owner-backed block writes.
    #[cfg(test)]
    pub fn write_value<T: Copy>(
        &self,
        gpu_addr: u64,
        data: T,
        write_cpu_mem: &mut dyn FnMut(u64, &[u8]),
    ) {
        let size = std::mem::size_of::<T>();
        let bytes = unsafe { std::slice::from_raw_parts(&data as *const T as *const u8, size) };
        self.write_with_callback(gpu_addr, bytes, write_cpu_mem);
    }

    // ── Legacy helpers (backward compat) ────────────────────────────────

    /// Map at a specific GPU VA (alias for `map`).
    pub fn alloc_fixed(&mut self, gpu_va: u64, cpu_addr: u64, size: u64) {
        self.map(gpu_va, cpu_addr, size, 0xFF);
    }

    /// Allocate GPU VA from a simple bump allocator and map to a CPU address.
    /// NOTE: This is not an upstream method. Kept for backward compatibility with
    /// code that used the old allocator. Upstream manages allocation at a higher level.
    pub fn alloc_any(&mut self, cpu_addr: u64, size: u64) -> u64 {
        // Start allocations at 64 MB to avoid the zero page region.
        static NEXT_ALLOC: AtomicUsize = AtomicUsize::new(0x0400_0000);
        let page_size = self.page_size;
        let aligned_size = (size + page_size - 1) & !(page_size - 1);
        let gpu_va = NEXT_ALLOC.fetch_add(aligned_size as usize, Ordering::Relaxed) as u64;
        self.map(gpu_va, cpu_addr, aligned_size, 0xFF);
        gpu_va
    }
}

#[cfg(test)]
impl Default for GpuMemoryManager {
    fn default() -> Self {
        Self::new()
    }
}

// ── Helper ──────────────────────────────────────────────────────────────

fn pte_kind_from_u32(raw: u32) -> PteKind {
    // Upstream callsites use `static_cast<Tegra::PTEKind>(params.kind)`.
    // `PteKind` is a transparent raw-byte newtype so invalid/unknown values are
    // preserved in `kind_map` instead of being collapsed to INVALID.
    PteKind::from_raw(raw as u8)
}

// ── MemoryManager (outer wrapper) ───────────────────────────────────────

/// Port owner for `Tegra::MemoryManager`.
///
/// Wraps `GpuMemoryManager` with the outer API expected by channel_state, gpu.rs,
/// nvdrv, etc.
pub struct MemoryManager {
    inner: GpuMemoryManager,
    native_syncpoint_shim: AtomicU64,
    /// Upstream Core::System&; runtime constructors keep the original owner.
    system: ruzu_core::core::SystemRef,
    /// Upstream MemoryManager::native_tiling; enabled per address space.
    native_tiling: bool,
    /// Upstream stores `MaxwellDeviceMemoryManager& memory` directly.
    /// Rust keeps the Host1x owner as an `Arc` while the broader owner graph is
    /// still shared through handles.
    device_memory: Arc<MaxwellDeviceMemoryManager>,
}

impl MemoryManager {
    pub const NUM_SHIM_SYNCPOINTS: u32 = 192;
    /// Upstream `MemoryManager::HAS_FLUSH_INVALIDATION`.
    pub const HAS_FLUSH_INVALIDATION: bool = true;

    /// Reduced test constructor. Upstream runtime constructors always receive
    /// `Core::System&` and resolve `MaxwellDeviceMemoryManager&` from it or
    /// receive the device-memory owner explicitly.
    #[cfg(test)]
    pub fn new(id: usize) -> Self {
        Self::new_with_geometry(id, 40, 1u64 << 34, 16, 12)
    }

    #[cfg(test)]
    pub fn new_with_geometry(
        id: usize,
        address_space_bits: u64,
        split_address: u64,
        big_page_bits: u64,
        page_bits: u64,
    ) -> Self {
        Self::new_with_geometry_and_device_memory(
            id,
            Arc::new(MaxwellDeviceMemoryManager::default()),
            address_space_bits,
            split_address,
            big_page_bits,
            page_bits,
        )
    }

    #[cfg(test)]
    pub fn new_with_geometry_and_device_memory(
        id: usize,
        device_memory: Arc<MaxwellDeviceMemoryManager>,
        address_space_bits: u64,
        split_address: u64,
        big_page_bits: u64,
        page_bits: u64,
    ) -> Self {
        Self::new_with_system_and_device_memory(
            ruzu_core::core::SystemRef::null(), id, device_memory,
            address_space_bits, split_address, big_page_bits, page_bits,
        )
    }

    pub fn new_with_system_and_device_memory(
        system: ruzu_core::core::SystemRef,
        id: usize,
        device_memory: Arc<MaxwellDeviceMemoryManager>,
        address_space_bits: u64,
        split_address: u64,
        big_page_bits: u64,
        page_bits: u64,
    ) -> Self {
        let inner_device_memory = Arc::clone(&device_memory);
        let mut inner = GpuMemoryManager::with_params_and_device_memory(
            inner_device_memory,
            address_space_bits,
            split_address,
            big_page_bits,
            page_bits,
        );
        // The Rust GPU owner allocates the address-space identifier before it
        // constructs this object. Keep the identifier on the actual upstream
        // MemoryManager counterpart so GetID and ModifyGPUMemory cannot diverge.
        inner.unique_identifier = id;
        Self {
            inner,
            native_syncpoint_shim: AtomicU64::new(0),
            system,
            native_tiling: false,
            device_memory,
        }
    }

    /// Upstream MemoryManager::SetNativeSyncpointShim.
    pub fn set_native_syncpoint_shim(&self, address: u64) {
        self.native_syncpoint_shim.store(address, Ordering::Release);
    }

    /// Upstream inline MemoryManager::EnableNativeTiling.
    pub fn enable_native_tiling(&mut self) { self.native_tiling = true; }

    /// Upstream inline MemoryManager::UsesNativeTiling.
    pub fn uses_native_tiling(&self) -> bool { self.native_tiling }

    /// Upstream: `MemoryManager::GetID()`.
    pub fn get_id(&self) -> usize {
        self.inner.get_id()
    }

    /// Lichii `MemoryManager::MapSerial`.
    pub fn map_serial(&self) -> u64 {
        self.inner.map_serial()
    }

    /// `map_serial()` read without taking the owner's mutex, as Lichii's unlocked readers do.
    /// Rust adaptation: the split front checks its cached translations against it
    /// (GpuSplitFrontend::note_write) instead of locking the manager for every noted write.
    pub(crate) fn map_serial_unlocked(memory: &parking_lot::Mutex<Self>) -> u64 {
        // SAFETY: the Mutex owns a live manager for the borrow of `memory`. Only the atomic
        // field is reached, through a raw place projection (no reference to the manager is
        // formed), and every access to that field, the lock holder's included, is atomic: the
        // same unlocked-reader contract as `MemoryManagerHandle`. The load pairs with the
        // Release bump after every page table change.
        unsafe {
            (*std::ptr::addr_of!((*memory.data_ptr()).inner.map_serial)).load(Ordering::Acquire)
        }
    }

    /// Lichii `MemoryManager::HostPointerSerial` (memory_manager.h:180-184): changes whenever
    /// this address space or the device memory behind it is mapped or unmapped, so a host
    /// pointer `get_pointer` returned is only good while it stays the same. Both terms only
    /// grow, and managers' generations are 2^32 apart, so the sum changes with either; a
    /// `set_device_memory_manager` swap changes it too.
    pub fn host_pointer_serial(&self) -> u64 {
        self.map_serial()
            .wrapping_add(self.device_memory.translation_generation())
    }

    pub fn device_memory(&self) -> &Arc<MaxwellDeviceMemoryManager> {
        &self.device_memory
    }

    pub fn gpu_to_cpu_address(&self, gpu_addr: u64) -> Option<u64> {
        let result = self.inner.gpu_to_cpu_address(gpu_addr);
        if result.is_none() && gpu_addr != 0 {
            note_translation_miss(gpu_addr);
        }
        result
    }

    /// Upstream MemoryManager::IsShuttingDown reads the owning Core::System.
    pub fn is_shutting_down(&self) -> bool {
        self.system.get().is_shutting_down()
    }

    pub fn gpu_to_cpu_address_range(&self, gpu_addr: u64, size: u64) -> Option<u64> {
        self.inner.gpu_to_cpu_address_range(gpu_addr, size)
    }

    /// Upstream: `MemoryManager::GetPointer(gpu_addr)`.
    pub fn get_pointer(&self, gpu_addr: u64) -> *mut u8 {
        let Some(device_addr) = self.inner.gpu_to_cpu_address(gpu_addr) else {
            return std::ptr::null_mut();
        };
        self.device_memory.get_pointer_mut(device_addr)
    }

    /// Whether `read::<u32>(gpu_addr)` returns a guest syncpoint value from the native
    /// syncpoint shim (Lichii memory_manager.cpp:303-310) instead of the memory there.
    /// Same condition as `read`; the texture memo compares memory bytes and must not keep
    /// such a word.
    pub(crate) fn reads_native_syncpoint(&self, gpu_addr: u64) -> bool {
        let shim = self.native_syncpoint_shim.load(Ordering::Acquire);
        shim != 0
            && gpu_addr >= shim
            && gpu_addr - shim < u64::from(Self::NUM_SHIM_SYNCPOINTS) * 4
            && (gpu_addr - shim) % 4 == 0
    }

    /// Port of `MemoryManager::Read<T>` for Eden's explicitly instantiated
    /// `u8`, `u16`, `u32` and `u64` scalar types.
    #[inline]
    pub(crate) fn read<T: MemoryValue>(&self, gpu_addr: u64) -> T {
        if std::mem::size_of::<T>() == std::mem::size_of::<u32>() {
            let shim = self.native_syncpoint_shim.load(Ordering::Acquire);
            if shim != 0 && gpu_addr >= shim
                && gpu_addr - shim < u64::from(Self::NUM_SHIM_SYNCPOINTS) * 4
                && (gpu_addr - shim) % 4 == 0
            {
                let value = self.system.get().host1x_core()
                    .expect("native syncpoint shim requires Host1x")
                    .get_guest_syncpoint_value(((gpu_addr - shim) / 4) as u32);
                // MemoryValue admits only u8/u16/u32/u64; the size guard selects u32.
                return unsafe { std::ptr::read_unaligned((&value as *const u32).cast::<T>()) };
            }
        }
        let page_pointer = self.get_pointer(gpu_addr);
        if !page_pointer.is_null() {
            // Match Eden's memcpy fast path, including unaligned addresses.
            return unsafe { std::ptr::read_unaligned(page_pointer.cast::<T>()) };
        }

        // C++ has one static counter per explicitly instantiated Read<T>.
        // Rust generic-function statics are shared, so keep a slot per width.
        static REPORTED: [std::sync::atomic::AtomicU32; 4] =
            [const { std::sync::atomic::AtomicU32::new(0) }; 4];
        let width = std::mem::size_of::<T>();
        if REPORTED[width.trailing_zeros() as usize].fetch_add(1, Ordering::SeqCst) < 16 {
            log::error!("Read{} of unmapped GPU address {gpu_addr:#x}", width * 8);
        }
        T::default()
    }

    /// Port of `MemoryManager::Write<T>` for Eden's explicitly instantiated
    /// scalar types.
    #[inline]
    pub(crate) fn write<T: MemoryValue>(&mut self, gpu_addr: u64, data: T) {
        // Lichii MemoryManager::Write<T> (memory_manager.cpp:338-350): a
        // download for a guest read not yet in guest memory would land over
        // this write later.
        if crate::rasterizer_download_area::G_READ_DOWNLOADS.load(Ordering::Relaxed) != 0 {
            if let Some(address) = self.inner.gpu_to_cpu_address(gpu_addr) {
                self.inner.finish_reads_before(address, std::mem::size_of::<T>());
            }
        }
        let page_pointer = self.get_pointer(gpu_addr);
        if !page_pointer.is_null() {
            // Match Eden's memcpy fast path, including unaligned addresses.
            unsafe { std::ptr::write_unaligned(page_pointer.cast::<T>(), data) };
            return;
        }

        log::error!("MemoryManager::Write to unmapped GPU address {gpu_addr:#x}");
    }

    pub fn is_continuous_range(&self, gpu_addr: u64, size: u64) -> bool {
        self.inner.is_continuous_range(gpu_addr, size)
    }

    pub fn is_fully_mapped_range(&self, gpu_addr: u64, size: u64) -> bool {
        self.inner.is_fully_mapped_range(gpu_addr, size)
    }

    pub fn is_granular_range(&self, gpu_addr: u64, size: u64) -> bool {
        self.inner.is_granular_range(gpu_addr, size)
    }

    #[cfg(test)]
    pub fn read_block_with_callback(
        &self,
        gpu_src: u64,
        output: &mut [u8],
        read_cpu: &dyn Fn(u64, &mut [u8]),
    ) {
        self.inner
            .read_block_with_callback(gpu_src, output, read_cpu);
    }

    #[cfg(test)]
    pub fn read_block_unsafe_with_callback(
        &self,
        gpu_src: u64,
        output: &mut [u8],
        read_cpu: &dyn Fn(u64, &mut [u8]),
    ) {
        self.inner
            .read_block_unsafe_with_callback(gpu_src, output, read_cpu);
    }

    #[cfg(test)]
    pub fn write_block_with_callback(
        &self,
        gpu_dest: u64,
        input: &[u8],
        write_cpu: &mut dyn FnMut(u64, &[u8]),
    ) {
        self.inner
            .write_block_with_callback(gpu_dest, input, write_cpu);
    }

    #[cfg(test)]
    pub fn write_block_unsafe_with_callback(
        &self,
        gpu_dest: u64,
        input: &[u8],
        write_cpu: &mut dyn FnMut(u64, &[u8]),
    ) {
        self.inner
            .write_block_unsafe_with_callback(gpu_dest, input, write_cpu);
    }

    #[cfg(test)]
    pub fn write_block_cached_with_callback(
        &mut self,
        gpu_dest: u64,
        input: &[u8],
        write_cpu: &mut dyn FnMut(u64, &[u8]),
    ) {
        self.inner
            .write_block_cached_with_callback(gpu_dest, input, write_cpu);
    }

    pub fn flush_caching(&mut self) {
        self.inner.flush_caching();
    }

    /// Reduced-fixture callback variant of `MemoryManager::CopyBlock`.
    #[cfg(test)]
    pub fn copy_block_with_callback(
        &mut self,
        gpu_dest: u64,
        gpu_src: u64,
        size: u64,
        read_cpu: &dyn Fn(u64, &mut [u8]),
        write_cpu: &mut dyn FnMut(u64, &[u8]),
    ) {
        self.inner
            .copy_block_with_callback(gpu_dest, gpu_src, size, read_cpu, write_cpu);
    }

    /// Upstream: `MemoryManager::GetSpan(gpu_addr, size)`.
    pub fn get_span(&self, gpu_addr: u64, size: usize) -> *mut u8 {
        if !self.inner.is_continuous_range(gpu_addr, size as u64) {
            return std::ptr::null_mut();
        }
        let Some(device_addr) = self.inner.gpu_to_cpu_address(gpu_addr) else {
            return std::ptr::null_mut();
        };
        self.device_memory.get_span(device_addr, size)
    }

    /// Const variant of `get_span`.
    pub fn get_span_const(&self, gpu_addr: u64, size: usize) -> *const u8 {
        self.get_span(gpu_addr, size) as *const u8
    }

    pub fn set_device_memory_manager(&mut self, device_memory: Arc<MaxwellDeviceMemoryManager>) {
        self.inner.device_memory = Arc::clone(&device_memory);
        self.device_memory = device_memory;
    }

    pub fn read_block(&self, gpu_src: u64, output: &mut [u8]) -> bool {
        self.inner.read_block(gpu_src, output)
    }

    pub fn read_block_with_cache_type(
        &self,
        gpu_src: u64,
        output: &mut [u8],
        which: CacheType,
    ) -> bool {
        self.inner
            .read_block_with_cache_type(gpu_src, output, which)
    }

    pub fn read_block_unsafe(&self, gpu_src: u64, output: &mut [u8]) -> bool {
        self.inner.read_block_unsafe(gpu_src, output)
    }

    pub fn write_block(&self, gpu_dest: u64, input: &[u8]) -> bool {
        self.inner.write_block(gpu_dest, input)
    }

    pub fn write_block_with_cache_type(
        &self,
        gpu_dest: u64,
        input: &[u8],
        which: CacheType,
    ) -> bool {
        self.inner
            .write_block_with_cache_type(gpu_dest, input, which)
    }

    pub fn write_block_unsafe(&self, gpu_dest: u64, input: &[u8]) -> bool {
        let written = self.inner.write_block_unsafe(gpu_dest, input);
        if !written && gpu_va_trace_enabled() {
            log::info!(
                "[GPU_VA_WRITEBLOCK] gpu_va=0x{:X} size={} unmapped_reason=no_writer",
                gpu_dest,
                input.len()
            );
        }
        written
    }

    pub fn write_block_cached(&mut self, gpu_dest: u64, input: &[u8]) -> bool {
        self.inner.write_block_cached(gpu_dest, input)
    }

    /// Lichii `MemoryManager::WriteConstantBufferData`.
    pub fn write_constant_buffer_data(&mut self, device_addr: u64, data: &[u8]) {
        self.inner.write_constant_buffer_data(device_addr, data);
    }

    /// Lichii `MemoryManager::AccumulateInvalidation`.
    pub fn accumulate_invalidation(&mut self, gpu_addr: u64, size: usize) {
        self.inner.accumulate_invalidation(gpu_addr, size);
    }

    pub fn copy_block(&mut self, gpu_dest: u64, gpu_src: u64, size: u64) -> bool {
        self.inner.copy_block(gpu_dest, gpu_src, size)
    }

    pub fn copy_block_with_cache_type(
        &mut self,
        gpu_dest: u64,
        gpu_src: u64,
        size: u64,
        which: CacheType,
    ) -> bool {
        self.inner
            .copy_block_with_cache_type(gpu_dest, gpu_src, size, which)
    }

    pub fn flush_region(&mut self, gpu_addr: u64, size: u64) {
        self.inner.flush_region(gpu_addr, size);
    }

    pub fn flush_region_with_cache_type(&mut self, gpu_addr: u64, size: u64, which: CacheType) {
        self.inner
            .flush_region_with_cache_type(gpu_addr, size, which);
    }

    pub fn invalidate_region(&mut self, gpu_addr: u64, size: u64) {
        self.inner.invalidate_region(gpu_addr, size);
    }

    pub fn invalidate_region_with_cache_type(
        &mut self,
        gpu_addr: u64,
        size: u64,
        which: CacheType,
    ) {
        self.inner
            .invalidate_region_with_cache_type(gpu_addr, size, which);
    }

    pub fn is_memory_dirty(&mut self, gpu_addr: u64, size: u64) -> bool {
        self.inner.is_memory_dirty(gpu_addr, size)
    }

    pub fn is_memory_dirty_with_cache_type(
        &mut self,
        gpu_addr: u64,
        size: u64,
        which: CacheType,
    ) -> bool {
        self.inner
            .is_memory_dirty_with_cache_type(gpu_addr, size, which)
    }

    pub fn is_within_gpu_address_range(&self, gpu_addr: u64) -> bool {
        self.inner.is_within_gpu_address_range(gpu_addr)
    }

    pub fn max_continuous_range(&self, gpu_addr: u64, size: u64) -> u64 {
        self.inner.max_continuous_range(gpu_addr, size)
    }

    pub fn get_memory_layout_size(&self, gpu_addr: u64) -> u64 {
        self.inner.get_memory_layout_size(gpu_addr, u64::MAX)
    }

    pub fn get_memory_layout_size_bounded(&self, gpu_addr: u64, max_size: u64) -> u64 {
        self.inner.get_memory_layout_size(gpu_addr, max_size)
    }

    pub fn get_page_kind_raw(&self, gpu_addr: u64) -> u32 {
        self.inner.get_page_kind(gpu_addr).raw() as u32
    }

    pub fn get_submapped_range(&self, gpu_addr: u64, size: u64) -> Vec<(u64, u64)> {
        self.inner.get_submapped_range(gpu_addr, size)
    }

    /// Upstream: `MemoryManager::Map(gpu_addr, dev_addr, size, kind, is_big_pages)`.
    pub fn map(
        &mut self,
        gpu_addr: u64,
        device_addr: u64,
        size: u64,
        kind: u32,
        is_big_pages: bool,
    ) -> u64 {
        let pte_kind = pte_kind_from_u32(kind);
        self.inner
            .map_ex(gpu_addr, device_addr, size, pte_kind, is_big_pages)
    }

    /// Upstream: `MemoryManager::MapSparse(gpu_addr, size, is_big_pages)`.
    pub fn map_sparse(&mut self, gpu_addr: u64, size: u64, is_big_pages: bool) -> u64 {
        self.inner.map_sparse_ex(gpu_addr, size, is_big_pages)
    }

    /// Upstream: `MemoryManager::Unmap(gpu_addr, size)`.
    pub fn unmap(&mut self, gpu_addr: u64, size: u64) {
        self.inner.unmap(gpu_addr, size);
    }

    /// See `GpuMemoryManager::begin_deferring_rasterizer_ops`.
    pub fn begin_deferring_rasterizer_ops(&mut self) {
        self.inner.begin_deferring_rasterizer_ops();
    }

    /// See `GpuMemoryManager::take_deferred_rasterizer_ops`.
    pub fn take_deferred_rasterizer_ops(&mut self) -> Vec<DeferredRasterizerOp> {
        self.inner.take_deferred_rasterizer_ops()
    }

    /// See `GpuMemoryManager::rasterizer_handle`.
    pub fn rasterizer_handle(&self) -> Option<crate::rasterizer_interface::RasterizerHandle> {
        self.inner.rasterizer_handle()
    }

    pub fn address_space_bits(&self) -> u64 {
        self.inner.address_space_bits
    }

    pub fn split_address(&self) -> u64 {
        self.inner.split_address
    }

    pub fn big_page_bits(&self) -> u64 {
        self.inner.big_page_bits
    }

    pub fn page_bits(&self) -> u64 {
        self.inner.page_bits
    }

    /// Upstream: `MemoryManager::BindRasterizer(rasterizer)`.
    pub fn bind_rasterizer(&mut self, rasterizer: &dyn RasterizerInterface) {
        self.inner.bind_rasterizer(rasterizer);
    }

    pub fn has_bound_rasterizer(&self) -> bool {
        self.inner.rasterizer.is_some()
    }
}

#[cfg(test)]
impl Default for MemoryManager {
    fn default() -> Self {
        Self::new(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rasterizer_interface::RasterizerDownloadArea;
    use std::sync::{Arc, Mutex};

    #[test]
    fn shutdown_state_tracks_the_original_system_owner() {
        use ruzu_core::core::{System, SystemRef};
        let system = System::new_boxed_for_test();
        let mm = MemoryManager::new_with_system_and_device_memory(
            SystemRef::from_ref(&system), 13, Arc::new(MaxwellDeviceMemoryManager::default()),
            32, 0x1_0000_0000, 16, 12,
        );
        assert!(!mm.is_shutting_down());
        system.set_shutting_down(true);
        assert!(mm.is_shutting_down());
        system.set_shutting_down(false);
        assert!(!mm.is_shutting_down());
    }

    #[test]
    fn native_syncpoint_shim_reads_live_host1x_only_for_aligned_u32() {
        use ruzu_core::core::{System, SystemRef};
        use crate::host1x::host1x::Host1x;
        let mut system = System::new_boxed_for_test();
        system.set_host1x_core(Box::new(Host1x::new()));
        let host = system.host1x_core().unwrap().as_any().downcast_ref::<Host1x>().unwrap();
        let device_memory = Arc::new(MaxwellDeviceMemoryManager::default());
        let mut backing = vec![0x5Au8; 0x1000];
        device_memory.smmu_set_physical_base_for_test(backing.as_ptr() as usize);
        device_memory.smmu_map_with_cpu_backing(0x9000_0000, backing.as_mut_ptr(), 0x4000_0000, 0x1000, 1, true);
        let mut mm = MemoryManager::new_with_system_and_device_memory(
            SystemRef::from_ref(&system), 13, device_memory, 32, 0x1_0000_0000, 16, 12,
        );
        mm.map(0x20000, 0x9000_0000, 0x1000, 0, false);
        assert_eq!(mm.read::<u32>(0x20000), 0x5A5A5A5A);
        mm.set_native_syncpoint_shim(0x20000);
        assert_eq!(mm.read::<u32>(0x20000), 0);
        host.syncpoint_manager().increment_guest(0);
        host.syncpoint_manager().increment_guest(191);
        assert_eq!(mm.read::<u32>(0x20000), 1);
        assert_eq!(mm.read::<u32>(0x20000 + 191 * 4), 1);
        host.syncpoint_manager().increment_guest(191);
        assert_eq!(mm.read::<u32>(0x20000 + 191 * 4), 2);
        assert_eq!(mm.read::<u32>(0x20000 + 192 * 4), 0x5A5A5A5A);
        assert_eq!(mm.read::<u32>(0x20001), 0x5A5A5A5A);
        assert_eq!(mm.read::<u8>(0x20000), 0x5A);
        assert_eq!(mm.read::<u16>(0x20000), 0x5A5A);
        assert_eq!(mm.read::<u64>(0x20000), 0x5A5A5A5A5A5A5A5A);
        mm.write::<u32>(0x20000, 99);
        assert_eq!(mm.read::<u32>(0x20000), 1);
        assert_eq!(&backing[..4], &99u32.to_ne_bytes());
        mm.set_native_syncpoint_shim(0);
        assert_eq!(mm.read::<u32>(0x20000), 99);
        mm.set_native_syncpoint_shim(0x30000);
        // No normal page mapping is needed for the read-only counter shim.
        assert_eq!(mm.read::<u32>(0x30000), 1);
        assert_eq!(mm.read::<u32>(0x2FFFC), 0);
    }

    struct TestRasterizer {
        accelerate_dma: crate::rasterizer_interface::TestAccelerateDMA,
        modify_calls: Arc<Mutex<Vec<(usize, u64, u64)>>>,
        unmap_calls: Arc<Mutex<Vec<(u64, u64)>>>,
        flush_calls: Arc<Mutex<Vec<(u64, u64)>>>,
        flush_cache_types: Arc<Mutex<Vec<CacheType>>>,
        invalidate_calls: Arc<Mutex<Vec<(u64, u64)>>>,
        invalidate_cache_types: Arc<Mutex<Vec<CacheType>>>,
        inner_invalidation_calls: Arc<Mutex<Vec<Vec<(u64, usize)>>>>,
        constant_buffer_data_calls: Arc<Mutex<Vec<Vec<(u64, usize)>>>>,
        dirty_regions: Arc<Mutex<Vec<(u64, u64)>>>,
        must_flush_calls: Arc<Mutex<Vec<(u64, u64)>>>,
        must_flush_cache_types: Arc<Mutex<Vec<CacheType>>>,
    }

    impl TestRasterizer {
        fn new() -> Self {
            Self {
                accelerate_dma: Default::default(),
                modify_calls: Arc::new(Mutex::new(Vec::new())),
                unmap_calls: Arc::new(Mutex::new(Vec::new())),
                flush_calls: Arc::new(Mutex::new(Vec::new())),
                flush_cache_types: Arc::new(Mutex::new(Vec::new())),
                invalidate_calls: Arc::new(Mutex::new(Vec::new())),
                invalidate_cache_types: Arc::new(Mutex::new(Vec::new())),
                inner_invalidation_calls: Arc::new(Mutex::new(Vec::new())),
                constant_buffer_data_calls: Arc::new(Mutex::new(Vec::new())),
                dirty_regions: Arc::new(Mutex::new(Vec::new())),
                must_flush_calls: Arc::new(Mutex::new(Vec::new())),
                must_flush_cache_types: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl RasterizerInterface for TestRasterizer {
        fn access_accelerate_dma(
            &mut self,
        ) -> &mut dyn crate::engines::maxwell_dma::AccelerateDMAInterface {
            &mut self.accelerate_dma
        }

        fn draw(
            &mut self,
            _draw_view: crate::engines::draw_manager::Maxwell3DDrawView<'_>,
            _instance_count: u32,
        ) {
        }
        fn draw_texture(
            &mut self,
            _draw_texture_view: crate::engines::draw_manager::Maxwell3DDrawTextureView<'_>,
        ) {
        }
        fn clear(
            &mut self,
            _clear_view: crate::engines::draw_manager::Maxwell3DClearView<'_>,
            _layer_count: u32,
        ) {
        }
        fn dispatch_compute(&mut self, _dispatch: &crate::engines::kepler_compute::DispatchCall) {}
        fn reset_counter(&mut self, _query_type: u32) {}
        fn query(
            &mut self,
            _gpu_addr: u64,
            _query_type: u32,
            _flags: crate::query_cache::types::QueryPropertiesFlags,
            _payload: u32,
            _subreport: u32,
        ) {
        }
        fn bind_graphics_uniform_buffer(
            &mut self,
            _stage: usize,
            _index: u32,
            _gpu_addr: u64,
            _size: u32,
        ) {
        }
        fn disable_graphics_uniform_buffer(&mut self, _stage: usize, _index: u32) {}
        fn signal_fence(&mut self, _func: Box<dyn FnOnce() + Send>) {}
        fn sync_operation(&mut self, _func: Box<dyn FnOnce() + Send>) {}
        fn signal_sync_point(&mut self, _value: u32) {}
        fn signal_reference(&mut self) {}
        fn release_fences(&mut self, _force: bool) {}
        fn flush_all(&mut self) {}
        fn flush_region(&mut self, addr: u64, size: u64, which: CacheType) {
            self.flush_calls.lock().unwrap().push((addr, size));
            self.flush_cache_types.lock().unwrap().push(which);
        }
        fn must_flush_region(&self, addr: u64, size: u64, which: CacheType) -> bool {
            self.must_flush_calls.lock().unwrap().push((addr, size));
            self.must_flush_cache_types.lock().unwrap().push(which);
            self.dirty_regions.lock().unwrap().contains(&(addr, size))
        }
        fn get_flush_area(&self, addr: u64, size: u64) -> RasterizerDownloadArea {
            RasterizerDownloadArea {
                start_address: addr,
                end_address: addr + size,
                preemtive: false,
                in_flight: false,
            }
        }
        fn invalidate_region(&mut self, addr: u64, size: u64, which: CacheType) {
            self.invalidate_calls.lock().unwrap().push((addr, size));
            self.invalidate_cache_types.lock().unwrap().push(which);
        }
        fn inner_invalidation(&mut self, sequences: &[(u64, usize)]) {
            self.inner_invalidation_calls
                .lock()
                .unwrap()
                .push(sequences.to_vec());
        }
        fn invalidate_constant_buffer_data(&mut self, sequences: &[(u64, usize)]) {
            self.constant_buffer_data_calls
                .lock()
                .unwrap()
                .push(sequences.to_vec());
        }
        fn on_cache_invalidation(&mut self, _addr: u64, _size: u64) {}
        fn on_cpu_write(&mut self, _addr: u64, _size: u64) -> bool {
            false
        }
        fn invalidate_gpu_cache(&mut self) {}
        fn unmap_memory(&mut self, addr: u64, size: u64) {
            self.unmap_calls.lock().unwrap().push((addr, size));
        }
        fn modify_gpu_memory(&mut self, as_id: usize, addr: u64, size: u64) {
            self.modify_calls.lock().unwrap().push((as_id, addr, size));
        }
        fn flush_and_invalidate_region(&mut self, _addr: u64, _size: u64, _which: CacheType) {}
        fn wait_for_idle(&mut self) {}
        fn fragment_barrier(&mut self) {}
        fn tiled_cache_barrier(&mut self) {}
        fn flush_commands(&mut self) {}
        fn tick_frame(&mut self) {}
        fn accelerate_inline_to_memory(
            &mut self,
            _address: u64,
            _copy_size: usize,
            _memory: &[u8],
        ) {
        }
    }

    #[test]
    fn test_map_and_translate_big_pages() {
        let mut mm = GpuMemoryManager::new();
        // Map using big pages (address < split_address = 1<<34)
        mm.map_ex(0x10000, 0xDEAD_0000, 0x10000, PteKind::INVALID, true);

        assert_eq!(mm.translate(0x10000), Some(0xDEAD_0000));
        assert_eq!(mm.translate(0x10500), Some(0xDEAD_0500));
        assert_eq!(mm.translate(0x1FFFF), Some(0xDEAD_FFFF));
    }

    #[test]
    fn test_map_and_translate_small_pages() {
        let mut mm = GpuMemoryManager::new();
        // Map using small pages
        mm.map_ex(0x1000, 0xBEEF_0000, 0x2000, PteKind::INVALID, false);

        assert_eq!(mm.translate(0x1000), Some(0xBEEF_0000));
        assert_eq!(mm.translate(0x1500), Some(0xBEEF_0500));
        assert_eq!(mm.translate(0x2000), Some(0xBEEF_1000));
        assert_eq!(mm.translate(0x2FFF), Some(0xBEEF_1FFF));
    }

    #[test]
    fn test_unmapped_returns_none() {
        let mm = GpuMemoryManager::new();
        assert_eq!(mm.translate(0x1000), None);
        assert_eq!(mm.translate(0), None);
    }

    #[test]
    fn test_unmap() {
        let mut mm = GpuMemoryManager::new();
        mm.map_ex(0x10000, 0xBEEF_0000, 0x10000, PteKind::INVALID, true);
        assert_eq!(mm.translate(0x10000), Some(0xBEEF_0000));

        mm.unmap(0x10000, 0x10000);
        assert_eq!(mm.translate(0x10000), None);
    }

    #[test]
    fn page_kind_preserves_unknown_raw_values() {
        let mut mm = MemoryManager::new_with_geometry(42, 32, 0x1_0000_0000, 16, 12);

        mm.map(0x10000, 0x9000_0000, 0x1000, 0x93, true);
        assert_eq!(mm.get_page_kind_raw(0x10000), 0x93);

        mm.map(0x20000, 0x9000_1000, 0x1000, 0x1ff, true);
        assert_eq!(mm.get_page_kind_raw(0x20000), 0xff);
    }

    #[test]
    fn test_dual_table_fallback() {
        let mut mm = GpuMemoryManager::new();
        // Map small pages at an address below split (big table will be Free).
        mm.map_ex(0x1000, 0xAAAA_0000, 0x1000, PteKind::INVALID, false);

        // Big entry is Free, so gpu_to_cpu_address falls back to small table.
        assert_eq!(mm.translate(0x1000), Some(0xAAAA_0000));
    }

    #[test]
    fn test_read_via_callback() {
        let mut mm = GpuMemoryManager::new();
        mm.map_ex(0x10000, 0x8000_0000, 0x10000, PteKind::INVALID, true);

        let mut buf = [0u8; 4];
        mm.read_with_callback(0x10000, &mut buf, &|addr, dst| {
            let bytes = (addr as u32).to_le_bytes();
            let len = dst.len().min(bytes.len());
            dst[..len].copy_from_slice(&bytes[..len]);
        });

        let val = u32::from_le_bytes(buf);
        assert_eq!(val, 0x8000_0000);
    }

    #[test]
    fn test_write_via_callback() {
        let mut mm = GpuMemoryManager::new();
        mm.map_ex(0x10000, 0x8000_0000, 0x10000, PteKind::INVALID, true);

        let mut written: Vec<(u64, Vec<u8>)> = Vec::new();
        let data = [0xDE, 0xAD, 0xBE, 0xEF];
        mm.write_with_callback(0x10000, &data, &mut |addr, src| {
            written.push((addr, src.to_vec()));
        });

        assert_eq!(written.len(), 1);
        assert_eq!(written[0].0, 0x8000_0000);
        assert_eq!(written[0].1, vec![0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn test_is_continuous_range() {
        let mut mm = GpuMemoryManager::new();
        // Two contiguous big pages.
        mm.map_ex(0x10000, 0x8000_0000, 0x20000, PteKind::INVALID, true);
        assert!(mm.is_continuous_range(0x10000, 0x20000));

        // Non-contiguous: two separate mappings.
        let mut mm2 = GpuMemoryManager::new();
        mm2.map_ex(0x10000, 0x8000_0000, 0x10000, PteKind::INVALID, true);
        mm2.map_ex(0x20000, 0x9000_0000, 0x10000, PteKind::INVALID, true);
        assert!(!mm2.is_continuous_range(0x10000, 0x20000));
    }

    #[test]
    fn big_page_continuity_uses_device_memory_pointers() {
        let device_memory = Arc::new(MaxwellDeviceMemoryManager::default());
        let backing = vec![0u8; 0x1_0000];
        device_memory.smmu_set_physical_base_for_test(backing.as_ptr() as usize);
        device_memory.smmu_map_with_cpu_backing(
            0x9000_0000,
            backing.as_ptr(),
            0x5000_0000,
            backing.len(),
            3,
            true,
        );

        let mut mm = GpuMemoryManager::with_params_and_device_memory(
            Arc::clone(&device_memory),
            32,
            0x1_0000_0000,
            16,
            12,
        );
        mm.map_ex(0x10000, 0x9000_0000, 0x1_0000, PteKind::INVALID, true);
        assert!(mm.is_granular_range(0x10800, 0x8000));

        device_memory.smmu_map_with_cpu_backing(
            0x9010_0000,
            backing.as_ptr(),
            0x5000_0000,
            0x1000,
            3,
            true,
        );
        let mut mm = GpuMemoryManager::with_params_and_device_memory(
            Arc::clone(&device_memory),
            32,
            0x1_0000_0000,
            16,
            12,
        );
        mm.map_ex(0x20000, 0x9010_0000, 0x1_0000, PteKind::INVALID, true);
        assert!(!mm.is_granular_range(0x20800, 0x8000));
    }

    #[test]
    fn test_max_continuous_range() {
        let mut mm = GpuMemoryManager::new();
        mm.map_ex(0x10000, 0x8000_0000, 0x10000, PteKind::INVALID, true);
        mm.map_ex(0x20000, 0x9000_0000, 0x10000, PteKind::INVALID, true);

        assert_eq!(mm.max_continuous_range(0x10000, 0x20000), 0x10000);
    }

    // ── Continuous runs (Lichii UpdateContinuousRuns / MaxContinuousRange) ──

    /// 32-bit address space, 64 KiB big pages, 4 KiB small pages.
    fn small_space_manager() -> GpuMemoryManager {
        GpuMemoryManager::with_params(32, 1u64 << 31, 16, 12)
    }

    /// The precomputed answer must equal Eden's page-table walk.
    fn assert_matches_walk(mm: &GpuMemoryManager, gpu_addr: u64, size: u64) {
        assert_eq!(
            mm.max_continuous_range(gpu_addr, size),
            mm.max_continuous_range_uncached(gpu_addr, size),
            "max_continuous_range({gpu_addr:#x}, {size:#x})"
        );
    }

    /// Every small page start (and a mid-page offset) of `[window_begin, window_end)`, asked
    /// up to the window end and for one page.
    fn assert_window_matches_walk(mm: &GpuMemoryManager, window_begin: u64, window_end: u64) {
        let mut address = window_begin;
        while address < window_end {
            assert_matches_walk(mm, address, window_end - address);
            assert_matches_walk(mm, address + 0x123, window_end - address - 0x123);
            assert_matches_walk(mm, address, 0x1000);
            // Eden's walk indexes past the entry tables beyond the address space; stay inside.
            assert_matches_walk(mm, address + 0x800, 0x1000.min(window_end - address - 0x800));
            address += 0x1000;
        }
    }

    fn runs_of(mm: &GpuMemoryManager) -> Vec<(u64, u64, u64)> {
        mm.runs_guard
            .lock()
            .continuous_runs
            .iter()
            .map(|(&begin, run)| (begin, run.end, run.device_addr))
            .collect()
    }

    #[test]
    fn continuous_runs_merge_adjacent_separate_maps() {
        let mut mm = small_space_manager();
        // Two big-page maps, device-contiguous, mapped separately: one run.
        mm.map_ex(0x10000, 0x8000_0000, 0x10000, PteKind::INVALID, true);
        mm.map_ex(0x20000, 0x8001_0000, 0x10000, PteKind::INVALID, true);
        assert_eq!(runs_of(&mm), vec![(0x10000, 0x30000, 0x8000_0000)]);
        assert_eq!(mm.max_continuous_range(0x10800, 0x100000), 0x1f800);
        // Small pages continuing the same device range right after it: still one run.
        mm.map_ex(0x30000, 0x8002_0000, 0x3000, PteKind::INVALID, false);
        assert_eq!(runs_of(&mm), vec![(0x10000, 0x33000, 0x8000_0000)]);
        // A small page that does not continue the device range starts a new run.
        mm.map_ex(0x33000, 0x9000_0000, 0x1000, PteKind::INVALID, false);
        assert_eq!(
            runs_of(&mm),
            vec![(0x10000, 0x33000, 0x8000_0000), (0x33000, 0x34000, 0x9000_0000)]
        );
        assert_window_matches_walk(&mm, 0, 0x50000);
    }

    #[test]
    fn continuous_runs_cut_by_sparse_unmap_and_remap() {
        let mut mm = small_space_manager();
        mm.map_ex(0x100000, 0x4000_0000, 0x80000, PteKind::INVALID, true);
        assert_eq!(runs_of(&mm), vec![(0x100000, 0x180000, 0x4000_0000)]);
        // A sparse big page in the middle: head and tail of the run are kept.
        mm.map_sparse_ex(0x130000, 0x10000, true);
        assert_eq!(
            runs_of(&mm),
            vec![(0x100000, 0x130000, 0x4000_0000), (0x140000, 0x180000, 0x4004_0000)]
        );
        assert_eq!(mm.max_continuous_range(0x130000, 0x1000), 0);
        assert_window_matches_walk(&mm, 0xf0000, 0x190000);
        // Mapping it back as before joins the three pieces again.
        mm.map_ex(0x130000, 0x4003_0000, 0x10000, PteKind::INVALID, true);
        assert_eq!(runs_of(&mm), vec![(0x100000, 0x180000, 0x4000_0000)]);
        // Unmapping one big page inside the run leaves a hole.
        mm.unmap(0x150000, 0x10000);
        assert_eq!(
            runs_of(&mm),
            vec![(0x100000, 0x150000, 0x4000_0000), (0x160000, 0x180000, 0x4006_0000)]
        );
        // Remap the hole with small pages to another device range.
        mm.map_ex(0x150000, 0x7000_0000, 0x10000, PteKind::INVALID, false);
        assert_window_matches_walk(&mm, 0xf0000, 0x190000);
        // Sparse small pages inside a free big page split the small-page run.
        mm.unmap(0x150000, 0x10000);
        mm.map_ex(0x150000, 0x7000_0000, 0x10000, PteKind::INVALID, false);
        mm.map_sparse_ex(0x154000, 0x2000, false);
        assert_window_matches_walk(&mm, 0xf0000, 0x190000);
        assert_eq!(mm.max_continuous_range(0x150000, 0x10000), 0x4000);
        assert_eq!(mm.max_continuous_range(0x156000, 0x10000), 0xa000);
    }

    #[test]
    fn continuous_runs_at_the_end_of_the_address_space() {
        let mut mm = small_space_manager();
        let top = 1u64 << 32;
        mm.map_ex(top - 0x30000, 0x1000_0000, 0x30000, PteKind::INVALID, true);
        assert_eq!(runs_of(&mm), vec![(top - 0x30000, top, 0x1000_0000)]);
        assert_window_matches_walk(&mm, top - 0x40000, top);
        // (Small maps stay below the last first-level chunk: `MultiLevelPageTable::reserve_range`
        // indexes one entry past its table for a range ending at the top.)
        mm.map_ex(top - 0x1d000, 0x2000_0000, 0x3000, PteKind::INVALID, false);
        mm.unmap(top - 0x20000, 0x10000);
        mm.map_ex(top - 0x1d000, 0x2000_0000, 0x3000, PteKind::INVALID, false);
        assert_eq!(
            runs_of(&mm),
            vec![
                (top - 0x30000, top - 0x20000, 0x1000_0000),
                (top - 0x1d000, top - 0x1a000, 0x2000_0000),
                (top - 0x10000, top, 0x1002_0000),
            ]
        );
        assert_window_matches_walk(&mm, top - 0x40000, top);
        // A walk that ends its run inside the address space answers a range reaching past
        // the end (Lichii returns 0 there; ruzu keeps the walk's answer).
        assert_eq!(mm.max_continuous_range(top - 0x30000, 0x100000), 0x10000);
        assert_eq!(mm.max_continuous_range_uncached(top - 0x30000, 0x100000), 0x10000);
        assert_eq!(mm.max_continuous_range(top, 0x1000), 0);
        assert_eq!(mm.max_continuous_range(u64::MAX - 0xfff, 0x1000), 0);
    }

    #[test]
    fn continuous_runs_zero_size_and_unaligned_operations() {
        let mut mm = small_space_manager();
        mm.map_ex(0x40000, 0x5000_0000, 0, PteKind::INVALID, true);
        assert!(runs_of(&mm).is_empty());
        // Big-page map from a non-big-aligned address and a size that is not a page multiple.
        mm.map_ex(0x48000, 0x5000_0000, 0x10800, PteKind::INVALID, true);
        mm.map_ex(0x71000, 0x6000_0000, 0x1800, PteKind::INVALID, false);
        assert_window_matches_walk(&mm, 0x30000, 0x90000);
        assert_eq!(mm.max_continuous_range(0x48000, 0), 0);
        assert_eq!(mm.max_continuous_range_uncached(0x48000, 0), 0);
    }

    #[test]
    fn continuous_runs_snapshot_per_manager_and_serial() {
        // More managers than per-thread views, two of them with the same caller-chosen id.
        let mut managers: Vec<MemoryManager> = (0..6)
            .map(|i| MemoryManager::new_with_geometry(if i < 2 { 7 } else { i }, 32, 1u64 << 31, 16, 12))
            .collect();
        for round in 0..4u64 {
            for (i, mm) in managers.iter_mut().enumerate() {
                let i = i as u64;
                let gpu = 0x10000 * (1 + i + round);
                mm.inner
                    .map_ex(gpu, 0x1000_0000 * (1 + i), 0x10000, PteKind::INVALID, true);
            }
            for (i, mm) in managers.iter().enumerate() {
                for gpu in (0..0x100000u64).step_by(0x8000) {
                    assert_matches_walk(&mm.inner, gpu, 0x80000);
                }
                let gpu = 0x10000 * (1 + i as u64 + round);
                assert!(mm.max_continuous_range(gpu, 0x10000) == 0x10000);
            }
        }
    }

    #[test]
    fn continuous_runs_random_operations_match_walk() {
        // xorshift64*, fixed seeds: deterministic sequences of maps, sparse maps and unmaps.
        for seed in [0x9e37_79b9_7f4a_7c15u64, 0x1234_5678, 0xdead_beef_cafe] {
            let mut state = seed;
            let mut next = move || {
                state ^= state >> 12;
                state ^= state << 25;
                state ^= state >> 27;
                state.wrapping_mul(0x2545_f491_4f6c_dd1d)
            };
            let mut mm = small_space_manager();
            let window_begin = 0x200000u64;
            let window_big_pages = 24u64;
            let window_end = window_begin + window_big_pages * 0x10000;
            let mut last_dev_end = 0x4000_0000u64;
            for step in 0..300 {
                let kind = next() % 8;
                let big = next() % 2 == 0;
                let (gpu, size) = if big {
                    let first = next() % window_big_pages;
                    let count = 1 + next() % 4;
                    let mut gpu = window_begin + first * 0x10000;
                    let mut size = count * 0x10000;
                    if next() % 8 == 0 {
                        gpu += (next() % 16) * 0x1000;
                        size -= (next() % 16) * 0x100;
                    }
                    (gpu, size)
                } else {
                    let first = next() % (window_big_pages * 16);
                    let count = 1 + next() % 24;
                    (window_begin + first * 0x1000, count * 0x1000)
                };
                let size = size.min(window_end - gpu);
                match kind {
                    0..=4 => {
                        // Continue the previous device range, reuse a nearby one, or jump.
                        let dev = match next() % 3 {
                            0 => last_dev_end,
                            1 => 0x4000_0000 + ((gpu - window_begin) & !0xfff),
                            _ => 0x4000_0000 + (next() % 0x1000) * 0x1000,
                        };
                        mm.map_ex(gpu, dev, size, PteKind::INVALID, big);
                        last_dev_end = dev + size;
                    }
                    5 => {
                        mm.map_sparse_ex(gpu, size, big);
                    }
                    _ => {
                        mm.unmap(gpu, size);
                    }
                }
                if step % 3 == 0 || step == 299 {
                    assert_window_matches_walk(&mm, window_begin - 0x10000, window_end + 0x10000);
                }
                for _ in 0..64 {
                    let gpu = window_begin - 0x10000 + next() % (window_end - window_begin + 0x20000);
                    let size = next() % 0x200000;
                    assert_matches_walk(&mm, gpu, size);
                }
            }
            // Runs stay sorted, disjoint, non-empty and maximal (no two that touch and continue
            // one device range).
            let runs = runs_of(&mm);
            for pair in runs.windows(2) {
                let (b0, e0, d0) = pair[0];
                let (b1, _, d1) = pair[1];
                assert!(b0 < e0 && e0 <= b1);
                assert!(!(e0 == b1 && d0 + (e0 - b0) == d1), "runs not merged: {pair:?}");
            }
        }
    }

    #[test]
    fn test_memory_manager_wrapper() {
        let mut mm = MemoryManager::new_with_geometry(5, 40, 1u64 << 34, 16, 12);

        assert_eq!(mm.get_id(), 5);
        assert_eq!(mm.address_space_bits(), 40);
        assert_eq!(mm.split_address(), 1u64 << 34);
        assert_eq!(mm.big_page_bits(), 16);
        assert_eq!(mm.page_bits(), 12);

        // Map with big pages (is_big_pages=true).
        assert_eq!(mm.map(0x10000, 0x9000_0000, 0x10000, 0xFF, true), 0x10000);
        assert_eq!(mm.gpu_to_cpu_address(0x10000), Some(0x9000_0000));
        assert_eq!(mm.gpu_to_cpu_address(0x10ABC), Some(0x9000_0ABC));
        assert_eq!(
            mm.gpu_to_cpu_address_range(0x0F000, 0x3000),
            Some(0x9000_0000)
        );

        mm.unmap(0x10000, 0x10000);
        assert_eq!(mm.gpu_to_cpu_address(0x10000), None);
    }

    #[test]
    fn test_bind_rasterizer_enables_modify_and_unmap_callbacks() {
        let mut mm = MemoryManager::new_with_geometry(7, 32, 0x1_0000_0000, 16, 12);
        let rasterizer = TestRasterizer::new();
        let modify_calls = Arc::clone(&rasterizer.modify_calls);
        let unmap_calls = Arc::clone(&rasterizer.unmap_calls);

        mm.bind_rasterizer(&rasterizer);
        mm.map(0x10000, 0x9000_0000, 0x1000, 0, false);
        mm.unmap(0x10000, 0x1000);

        assert!(mm.has_bound_rasterizer());
        assert_eq!(
            *modify_calls.lock().unwrap(),
            vec![
                (mm.get_id(), 0x10000, 0x1000),
                (mm.get_id(), 0x10000, 0x1000)
            ]
        );
        assert_eq!(*unmap_calls.lock().unwrap(), vec![(0x9000_0000, 0x1000)]);
    }

    #[test]
    fn test_flush_invalidate_and_dirty_follow_device_ranges() {
        let mut mm = MemoryManager::new_with_geometry(9, 32, 0x1_0000_0000, 16, 12);
        let rasterizer = TestRasterizer::new();
        let flush_calls = Arc::clone(&rasterizer.flush_calls);
        let invalidate_calls = Arc::clone(&rasterizer.invalidate_calls);
        let dirty_regions = Arc::clone(&rasterizer.dirty_regions);
        let must_flush_calls = Arc::clone(&rasterizer.must_flush_calls);

        mm.bind_rasterizer(&rasterizer);
        mm.map(0x20000, 0x9100_0000, 0x2000, 0, false);
        dirty_regions.lock().unwrap().push((0x9100_1000, 0x1000));

        assert!(mm.inner.is_memory_dirty(0x20000, 0x2000));
        mm.flush_region(0x20000, 0x2000);
        mm.invalidate_region(0x20000, 0x2000);

        assert_eq!(
            *must_flush_calls.lock().unwrap(),
            vec![(0x9100_0000, 0x1000), (0x9100_1000, 0x1000)]
        );
        assert_eq!(
            *flush_calls.lock().unwrap(),
            vec![(0x9100_0000, 0x1000), (0x9100_1000, 0x1000)]
        );
        assert_eq!(
            *invalidate_calls.lock().unwrap(),
            vec![(0x9100_0000, 0x1000), (0x9100_1000, 0x1000)]
        );
    }

    #[test]
    fn granular_big_page_range_uses_upstream_page_index_mask() {
        let mut mm = GpuMemoryManager::new();
        let gpu_addr = 0x10_0000;
        mm.big_page_table_op(
            EntryType::Mapped,
            gpu_addr,
            0x9000_0000,
            mm.big_page_size,
            PteKind::INVALID,
        );
        let page_index = mm.page_entry_index_big(gpu_addr);
        mm.set_big_page_continuous(page_index, true);

        assert!(mm.is_granular_range(gpu_addr, mm.big_page_size - page_index as u64));
        assert!(!mm.is_granular_range(gpu_addr, mm.big_page_size));
    }

    #[test]
    fn cache_type_is_forwarded_through_safe_memory_operations() {
        let mut mm = MemoryManager::new_with_geometry(19, 32, 0x1_0000_0000, 16, 12);
        let rasterizer = TestRasterizer::new();
        let flush_cache_types = Arc::clone(&rasterizer.flush_cache_types);
        let invalidate_cache_types = Arc::clone(&rasterizer.invalidate_cache_types);
        let must_flush_cache_types = Arc::clone(&rasterizer.must_flush_cache_types);

        mm.bind_rasterizer(&rasterizer);
        mm.map(0x20000, 0x9100_0000, 0x1000, 0, false);

        let mut output = [0u8; 0x20];
        mm.inner.read_block_with_cache_type_and_callback(
            0x20040,
            &mut output,
            CacheType::NO_TEXTURE_CACHE,
            &|_, out| out.fill(0x5a),
        );
        mm.invalidate_region_with_cache_type(0x20080, 0x20, CacheType::NO_QUERY_CACHE);
        assert!(!mm.is_memory_dirty_with_cache_type(0x200c0, 0x20, CacheType::BUFFER_CACHE,));

        assert_eq!(output, [0x5a; 0x20]);
        assert_eq!(
            *flush_cache_types.lock().unwrap(),
            vec![CacheType::NO_TEXTURE_CACHE]
        );
        assert_eq!(
            *invalidate_cache_types.lock().unwrap(),
            vec![CacheType::NO_QUERY_CACHE]
        );
        assert_eq!(
            *must_flush_cache_types.lock().unwrap(),
            vec![CacheType::BUFFER_CACHE]
        );
    }

    #[test]
    fn write_block_cached_accumulates_and_flushes_device_ranges() {
        let mut mm = MemoryManager::new_with_geometry(10, 32, 0x1_0000_0000, 16, 12);
        let rasterizer = TestRasterizer::new();
        let inner_calls = Arc::clone(&rasterizer.inner_invalidation_calls);

        mm.bind_rasterizer(&rasterizer);
        mm.map(0x20000, 0x9100_0000, 0x2000, 0, false);

        let mut writes = Vec::new();
        mm.write_block_cached_with_callback(0x20000, &[0x11; 0x20], &mut |addr, data| {
            writes.push((addr, data.len()));
        });
        mm.write_block_cached_with_callback(0x20020, &[0x22; 0x20], &mut |addr, data| {
            writes.push((addr, data.len()));
        });

        assert_eq!(writes, vec![(0x9100_0000, 0x20), (0x9100_0020, 0x20)]);
        assert!(inner_calls.lock().unwrap().is_empty());

        mm.flush_caching();

        assert_eq!(
            *inner_calls.lock().unwrap(),
            vec![vec![(0x9100_0000, 0x40)]]
        );

        mm.flush_caching();
        assert_eq!(inner_calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn flush_caching_reuses_page_stash2_and_hands_over_only_new_ranges() {
        let mut mm = MemoryManager::new_with_geometry(14, 32, 0x1_0000_0000, 16, 12);
        let rasterizer = TestRasterizer::new();
        let inner_calls = Arc::clone(&rasterizer.inner_invalidation_calls);
        mm.bind_rasterizer(&rasterizer);
        // Two GPU-adjacent small pages backed by non-adjacent device pages.
        mm.map(0x20000, 0x9100_0000, 0x1000, 0, false);
        mm.map(0x21000, 0x9300_0000, 0x1000, 0, false);

        mm.write_block_cached_with_callback(0x20FE0, &[0x11; 0x40], &mut |_, _| {});
        mm.flush_caching();
        // Eden GetSubmappedRangeImpl<false>: one device range per backing run.
        assert_eq!(
            *inner_calls.lock().unwrap(),
            vec![vec![(0x9100_0FE0, 0x20), (0x9300_0000, 0x20)]]
        );
        assert!(mm.inner.page_stash2.is_empty());
        assert!(mm.inner.page_stash2.capacity() >= 2);

        mm.write_block_cached_with_callback(0x21100, &[0x22; 0x20], &mut |_, _| {});
        mm.flush_caching();
        assert_eq!(
            inner_calls.lock().unwrap().last().unwrap(),
            &vec![(0x9300_0100, 0x20)]
        );
        assert!(mm.inner.page_stash2.is_empty());
    }

    #[test]
    fn accumulated_invalidations_reach_inner_invalidation_at_the_next_flush() {
        // Lichii AccumulateInvalidation (EDEN_NX2_INLINE_DEFER): an inline
        // write's invalidation waits for FlushCaching, merged with its
        // neighbours and translated to device ranges there.
        let mut mm = MemoryManager::new_with_geometry(16, 32, 0x1_0000_0000, 16, 12);
        let rasterizer = TestRasterizer::new();
        let inner_calls = Arc::clone(&rasterizer.inner_invalidation_calls);
        mm.bind_rasterizer(&rasterizer);
        mm.map(0x20000, 0x9100_0000, 0x1000, 0, false);
        mm.accumulate_invalidation(0x20100, 0x10);
        mm.accumulate_invalidation(0x20110, 0x10);
        assert!(inner_calls.lock().unwrap().is_empty());
        mm.flush_caching();
        assert_eq!(*inner_calls.lock().unwrap(), vec![vec![(0x9100_0100, 0x20)]]);
        mm.flush_caching();
        assert_eq!(inner_calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn constant_buffer_data_is_written_merged_and_handed_over_after_cached_writes() {
        let device_memory = Arc::new(MaxwellDeviceMemoryManager::default());
        let mut backing = vec![0u8; 0x2000];
        device_memory.smmu_set_physical_base_for_test(backing.as_mut_ptr() as usize);
        device_memory.smmu_map_with_cpu_backing(
            0x8000,
            backing.as_mut_ptr(),
            0x4000,
            backing.len(),
            1,
            true,
        );
        let mut mm = MemoryManager::new_with_geometry_and_device_memory(
            15,
            Arc::clone(&device_memory),
            32,
            0x1_0000_0000,
            16,
            12,
        );
        let rasterizer = TestRasterizer::new();
        let inner_calls = Arc::clone(&rasterizer.inner_invalidation_calls);
        let cb_calls = Arc::clone(&rasterizer.constant_buffer_data_calls);
        mm.bind_rasterizer(&rasterizer);

        // Words written one after another (and one rewritten) merge into one
        // range; a write that neither touches nor overlaps starts another.
        mm.write_constant_buffer_data(0x8100, &0x1111_1111u32.to_le_bytes());
        mm.write_constant_buffer_data(0x8104, &0x2222_2222u32.to_le_bytes());
        mm.write_constant_buffer_data(0x80FC, &0x3333_3333u32.to_le_bytes());
        mm.write_constant_buffer_data(0x8100, &0x4444_4444u32.to_le_bytes());
        mm.write_constant_buffer_data(0x8200, &[0x55; 8]);
        // Across a device page boundary: both pages are written.
        mm.write_constant_buffer_data(0x8FFC, &[0x66; 8]);
        assert_eq!(&backing[0xFC..0x100], &0x3333_3333u32.to_le_bytes());
        assert_eq!(&backing[0x100..0x104], &0x4444_4444u32.to_le_bytes());
        assert_eq!(&backing[0x104..0x108], &0x2222_2222u32.to_le_bytes());
        assert_eq!(&backing[0x200..0x208], &[0x55; 8]);
        assert_eq!(&backing[0xFFC..0x1004], &[0x66; 8]);
        assert!(cb_calls.lock().unwrap().is_empty());

        mm.flush_caching();
        // Nothing went through the accumulator; Lichii FlushCaching hands the
        // constant buffer ranges over on their own.
        assert!(inner_calls.lock().unwrap().is_empty());
        assert_eq!(
            *cb_calls.lock().unwrap(),
            vec![vec![(0x80FC, 0xC), (0x8200, 8), (0x8FFC, 8)]]
        );
        mm.flush_caching();
        assert_eq!(cb_calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn scalar_read_does_not_flush_like_read_block() {
        // Eden `MemoryManager::Read<T>` is GetPointer + memcpy; only
        // `ReadBlock` flushes the rasterizer caches first. The graphics
        // pipeline's texture handle reads rely on that difference.
        let mut mm = MemoryManager::new_with_geometry(15, 32, 0x1_0000_0000, 16, 12);
        let rasterizer = TestRasterizer::new();
        let flush_calls = Arc::clone(&rasterizer.flush_calls);
        mm.bind_rasterizer(&rasterizer);
        mm.map(0x20000, 0x9100_0000, 0x1000, 0, false);

        let _ = mm.read::<u32>(0x20010);
        assert!(flush_calls.lock().unwrap().is_empty());
        let mut bytes = [0u8; 4];
        mm.read_block(0x20010, &mut bytes);
        assert!(!flush_calls.lock().unwrap().is_empty());
    }

    #[test]
    fn copy_block_flushes_destination_before_writing() {
        let mut mm = MemoryManager::new_with_geometry(12, 32, 0x1_0000_0000, 16, 12);
        let rasterizer = TestRasterizer::new();
        let flush_calls = Arc::clone(&rasterizer.flush_calls);

        mm.bind_rasterizer(&rasterizer);
        mm.map(0x10000, 0x8000_0000, 0x1000, 0, false);
        mm.map(0x20000, 0x9000_0000, 0x1000, 0, false);

        let mut writes = Vec::new();
        mm.copy_block_with_callback(
            0x20040,
            0x10020,
            0x20,
            &|addr, output| {
                for (index, byte) in output.iter_mut().enumerate() {
                    *byte = (addr as u8).wrapping_add(index as u8);
                }
            },
            &mut |addr, data| {
                assert_eq!(*flush_calls.lock().unwrap(), vec![(0x9000_0040, 0x20)]);
                writes.push((addr, data.to_vec()));
            },
        );

        let expected: Vec<u8> = (0..0x20)
            .map(|index| (0x8000_0020u64 as u8).wrapping_add(index as u8))
            .collect();
        assert_eq!(writes, vec![(0x9000_0040, expected)]);
        assert_eq!(*flush_calls.lock().unwrap(), vec![(0x9000_0040, 0x20)]);
    }

    #[test]
    fn read_block_flushes_but_read_block_unsafe_does_not() {
        let mut mm = MemoryManager::new_with_geometry(14, 32, 0x1_0000_0000, 16, 12);
        let rasterizer = TestRasterizer::new();
        let flush_calls = Arc::clone(&rasterizer.flush_calls);

        mm.bind_rasterizer(&rasterizer);
        mm.map(0x20000, 0x9000_0000, 0x1000, 0, false);

        let reader = |addr: u64, output: &mut [u8]| {
            for (index, byte) in output.iter_mut().enumerate() {
                *byte = (addr as u8).wrapping_add(index as u8);
            }
        };

        let mut output = [0u8; 0x20];
        mm.read_block_with_callback(0x20040, &mut output, &reader);
        assert_eq!(*flush_calls.lock().unwrap(), vec![(0x9000_0040, 0x20)]);

        mm.read_block_unsafe_with_callback(0x20080, &mut output, &reader);
        assert_eq!(flush_calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn write_block_invalidates_but_write_block_unsafe_does_not() {
        let mut mm = MemoryManager::new_with_geometry(15, 32, 0x1_0000_0000, 16, 12);
        let rasterizer = TestRasterizer::new();
        let invalidate_calls = Arc::clone(&rasterizer.invalidate_calls);

        mm.bind_rasterizer(&rasterizer);
        mm.map(0x20000, 0x9000_0000, 0x1000, 0, false);

        let mut writes = Vec::new();
        mm.write_block_with_callback(0x20040, &[0xA5; 0x20], &mut |addr, data| {
            writes.push((addr, data.len()));
        });
        assert_eq!(*invalidate_calls.lock().unwrap(), vec![(0x9000_0040, 0x20)]);
        assert_eq!(writes, vec![(0x9000_0040, 0x20)]);

        mm.write_block_unsafe_with_callback(0x20080, &[0x5A; 0x20], &mut |addr, data| {
            writes.push((addr, data.len()));
        });
        assert_eq!(invalidate_calls.lock().unwrap().len(), 1);
        assert_eq!(writes, vec![(0x9000_0040, 0x20), (0x9000_0080, 0x20)]);
    }

    #[test]
    fn get_span_translates_gpu_range_through_device_memory() {
        let device_memory = Arc::new(MaxwellDeviceMemoryManager::default());
        let backing = vec![0u8; 0x3000];
        device_memory.smmu_set_physical_base_for_test(backing.as_ptr() as usize);
        device_memory.smmu_map_with_cpu_backing(
            0x9000_0000,
            backing.as_ptr(),
            0x4000_0000,
            0x3000,
            3,
            true,
        );
        let mut mm = MemoryManager::new_with_geometry_and_device_memory(
            11,
            Arc::clone(&device_memory),
            32,
            0x1_0000_0000,
            16,
            12,
        );
        mm.map(0x20000, 0x9000_0000, 0x3000, 0, false);

        assert_eq!(mm.get_span(0x20800, 0x1800), unsafe {
            backing.as_ptr().add(0x800) as *mut u8
        });
        assert!(mm.get_span(0x20800, 0x2801).is_null());

        mm.unmap(0x20000, 0x3000);
        assert!(mm.get_span(0x20800, 0x100).is_null());
    }

    #[test]
    fn get_pointer_translates_one_gpu_address_like_upstream() {
        let device_memory = Arc::new(MaxwellDeviceMemoryManager::default());
        let backing = vec![0u8; 0x1000];
        device_memory.smmu_set_physical_base_for_test(backing.as_ptr() as usize);
        device_memory.smmu_map_with_cpu_backing(
            0x9000_0000,
            backing.as_ptr(),
            0x4000_0000,
            0x1000,
            1,
            true,
        );
        let mut mm = MemoryManager::new_with_geometry_and_device_memory(
            12,
            Arc::clone(&device_memory),
            32,
            0x1_0000_0000,
            16,
            12,
        );
        mm.map(0x20000, 0x9000_0000, 0x1000, 0, false);

        assert_eq!(mm.get_pointer(0x20800), unsafe {
            backing.as_ptr().add(0x800) as *mut u8
        });
        assert!(mm.get_pointer(0x30000).is_null());
    }

    #[test]
    fn host_pointer_serial_follows_gpu_and_device_mappings() {
        // Lichii MemoryManager::HostPointerSerial: MapSerial + the device memory's
        // TranslationGeneration.
        let device_memory = Arc::new(MaxwellDeviceMemoryManager::default());
        let backing = vec![0u8; 0x2000];
        device_memory.smmu_set_physical_base_for_test(backing.as_ptr() as usize);
        device_memory.smmu_map_with_cpu_backing(
            0x9000_0000, backing.as_ptr(), 0x4000_0000, 0x2000, 1, true,
        );
        let mut mm = MemoryManager::new_with_geometry_and_device_memory(
            14, Arc::clone(&device_memory), 32, 0x1_0000_0000, 16, 12,
        );
        let start = mm.host_pointer_serial();
        assert_eq!(start, mm.map_serial() + device_memory.translation_generation());

        mm.map(0x20000, 0x9000_0000, 0x1000, 0, false);
        let gpu_mapped = mm.host_pointer_serial();
        assert_ne!(gpu_mapped, start);
        let _ = mm.get_pointer(0x20000);
        assert_eq!(mm.host_pointer_serial(), gpu_mapped);

        device_memory.smmu_unmap(0x9000_1000, 0x1000);
        let device_unmapped = mm.host_pointer_serial();
        assert_ne!(device_unmapped, gpu_mapped);

        mm.unmap(0x20000, 0x1000);
        assert_ne!(mm.host_pointer_serial(), device_unmapped);

        let before_swap = mm.host_pointer_serial();
        mm.set_device_memory_manager(Arc::new(MaxwellDeviceMemoryManager::default()));
        assert_ne!(mm.host_pointer_serial(), before_swap);
    }

    #[test]
    fn scalar_read_write_use_direct_unaligned_pointer_and_unmapped_defaults() {
        let device_memory = Arc::new(MaxwellDeviceMemoryManager::default());
        let mut backing = vec![0u8; 0x1000];
        device_memory.smmu_set_physical_base_for_test(backing.as_ptr() as usize);
        device_memory.smmu_map_with_cpu_backing(
            0x9000_0000,
            backing.as_mut_ptr(),
            0x4000_0000,
            0x1000,
            1,
            true,
        );
        let mut mm = MemoryManager::new_with_geometry_and_device_memory(
            13,
            Arc::clone(&device_memory),
            32,
            0x1_0000_0000,
            16,
            12,
        );
        mm.map(0x20000, 0x9000_0000, 0x1000, 0, false);

        backing[0x801..0x805].copy_from_slice(&0x1122_3344u32.to_ne_bytes());
        assert_eq!(mm.read::<u32>(0x20801), 0x1122_3344);

        mm.write::<u64>(0x20803, 0x0123_4567_89ab_cdef);
        assert_eq!(
            &backing[0x803..0x80b],
            &0x0123_4567_89ab_cdefu64.to_ne_bytes()
        );

        assert_eq!(mm.read::<u16>(0x30000), 0);
        let before = backing.clone();
        mm.write::<u32>(0x30000, u32::MAX);
        assert_eq!(backing, before);
    }

    #[test]
    fn write_block_unsafe_uses_device_memory_without_invalidation() {
        let device_memory = Arc::new(MaxwellDeviceMemoryManager::default());
        let mut backing = vec![0u8; 0x1000];
        let invalidations = Arc::new(Mutex::new(Vec::new()));
        let invalidations_clone = Arc::clone(&invalidations);
        device_memory.set_invalidate_region(Box::new(move |addr, size| {
            invalidations_clone.lock().unwrap().push((addr, size));
        }));
        device_memory.smmu_set_physical_base_for_test(backing.as_ptr() as usize);
        device_memory.smmu_map_with_cpu_backing(
            0x9000_0000,
            backing.as_mut_ptr(),
            0x5000_0000,
            backing.len(),
            3,
            true,
        );

        let mut mm = MemoryManager::new_with_geometry_and_device_memory(
            13,
            Arc::clone(&device_memory),
            32,
            0x1_0000_0000,
            16,
            12,
        );
        mm.map(0x20000, 0x9000_0000, 0x1000, 0, false);

        mm.write_block_unsafe(0x20040, &[0x7B; 0x20]);

        assert_eq!(&backing[0x40..0x60], &[0x7B; 0x20]);
        assert!(invalidations.lock().unwrap().is_empty());
    }

    #[test]
    fn read_block_unsafe_uses_device_memory_without_flush() {
        let device_memory = Arc::new(MaxwellDeviceMemoryManager::default());
        let backing: Vec<u8> = (0..0x1000).map(|value| value as u8).collect();
        let flushes = Arc::new(Mutex::new(Vec::new()));
        let flushes_clone = Arc::clone(&flushes);
        device_memory.set_flush_region(Box::new(move |addr, size| {
            flushes_clone.lock().unwrap().push((addr, size));
        }));
        device_memory.smmu_set_physical_base_for_test(backing.as_ptr() as usize);
        device_memory.smmu_map_with_cpu_backing(
            0x9000_0000,
            backing.as_ptr(),
            0x5000_0000,
            backing.len(),
            3,
            true,
        );

        let mut mm = MemoryManager::new_with_geometry_and_device_memory(
            14,
            Arc::clone(&device_memory),
            32,
            0x1_0000_0000,
            16,
            12,
        );
        mm.map(0x20000, 0x9000_0000, 0x1000, 0, false);

        let mut output = [0u8; 0x20];
        assert!(mm.read_block_unsafe(0x20040, &mut output));

        assert_eq!(&output, &backing[0x40..0x60]);
        assert!(flushes.lock().unwrap().is_empty());
    }

    #[test]
    fn read_block_flushes_once_before_unsafe_device_memory_copy() {
        let device_memory = Arc::new(MaxwellDeviceMemoryManager::default());
        let backing: Vec<u8> = (0..0x1000).map(|value| value as u8).collect();
        let device_flushes = Arc::new(Mutex::new(Vec::new()));
        let device_flushes_clone = Arc::clone(&device_flushes);
        device_memory.set_flush_region(Box::new(move |addr, size| {
            device_flushes_clone
                .lock()
                .unwrap()
                .push((addr, size as u64));
        }));
        device_memory.smmu_set_physical_base_for_test(backing.as_ptr() as usize);
        device_memory.smmu_map_with_cpu_backing(
            0x9000_0000,
            backing.as_ptr(),
            0x5000_0000,
            backing.len(),
            3,
            true,
        );

        let mut mm = MemoryManager::new_with_geometry_and_device_memory(
            15,
            Arc::clone(&device_memory),
            32,
            0x1_0000_0000,
            16,
            12,
        );
        let rasterizer = TestRasterizer::new();
        let rasterizer_flushes = Arc::clone(&rasterizer.flush_calls);
        mm.bind_rasterizer(&rasterizer);
        mm.map(0x20000, 0x9000_0000, 0x1000, 0, false);

        let mut output = [0u8; 0x20];
        assert!(mm.read_block(0x20040, &mut output));

        assert_eq!(&output, &backing[0x40..0x60]);
        assert_eq!(
            *rasterizer_flushes.lock().unwrap(),
            vec![(0x9000_0040, 0x20)]
        );
        assert!(device_flushes.lock().unwrap().is_empty());
    }

    #[test]
    fn read_block_uses_device_memory_owner_when_present() {
        let device_memory = Arc::new(MaxwellDeviceMemoryManager::default());
        let mut mm = MemoryManager::new_with_geometry_and_device_memory(
            11,
            Arc::clone(&device_memory),
            32,
            0x1_0000_0000,
            16,
            12,
        );
        mm.map(0x10000, 0x8000, 0x1000, 0, false);

        let mut output = [0xFFu8; 0x1000];
        assert!(mm.read_block(0x10000, &mut output));

        assert_eq!(output, [0u8; 0x1000]);
    }

    #[test]
    fn write_block_uses_device_memory_owner_when_present() {
        let device_memory = Arc::new(MaxwellDeviceMemoryManager::default());
        let mut mm = MemoryManager::new_with_geometry_and_device_memory(
            12,
            Arc::clone(&device_memory),
            32,
            0x1_0000_0000,
            16,
            12,
        );
        mm.map(0x10000, 0x8000, 0x1000, 0, false);

        mm.write_block(0x10000, &[0x5A; 0x1000]);
    }

    #[test]
    fn write_block_cached_uses_device_memory_owner_and_accumulates() {
        let device_memory = Arc::new(MaxwellDeviceMemoryManager::default());
        let mut backing = vec![0u8; 0x1000];
        device_memory.smmu_set_physical_base_for_test(backing.as_mut_ptr() as usize);
        device_memory.smmu_map_with_cpu_backing(
            0x8000,
            backing.as_mut_ptr(),
            0x4000,
            backing.len(),
            1,
            true,
        );
        let mut mm = MemoryManager::new_with_geometry_and_device_memory(
            13,
            Arc::clone(&device_memory),
            32,
            0x1_0000_0000,
            16,
            12,
        );
        mm.map(0x10000, 0x8000, 0x1000, 0, false);

        assert!(mm.write_block_cached(0x10020, &[0xA5; 0x20]));
        mm.flush_caching();

        assert_eq!(&backing[0x20..0x40], &[0xA5; 0x20]);
    }

    #[test]
    fn test_get_submapped_range() {
        let mut mm = GpuMemoryManager::new();
        // Map two contiguous big pages, then a gap, then one more.
        mm.map_ex(0x10000, 0x8000_0000, 0x20000, PteKind::INVALID, true);
        // Gap at 0x30000
        mm.map_ex(0x40000, 0x9000_0000, 0x10000, PteKind::INVALID, true);

        let ranges = mm.get_submapped_range(0x10000, 0x40000);
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0], (0x10000, 0x20000));
        assert_eq!(ranges[1], (0x40000, 0x10000));
    }

    #[test]
    fn get_submapped_range_falls_back_to_small_pages_like_upstream() {
        let mut mm = GpuMemoryManager::new();
        mm.map_ex(0x1000, 0x8000_0000, 0x1000, PteKind::INVALID, false);
        mm.map_ex(0x2000, 0x8000_1000, 0x1000, PteKind::INVALID, false);
        mm.map_ex(0x3000, 0x9000_0000, 0x1000, PteKind::INVALID, false);

        let ranges = mm.get_submapped_range(0x1000, 0x4000);

        assert_eq!(ranges, vec![(0x1000, 0x2000), (0x3000, 0x1000)]);
    }

    #[test]
    fn gpu_to_cpu_address_range_uses_wrapping_page_last_like_upstream() {
        let mm = GpuMemoryManager::new();

        assert_eq!(mm.gpu_to_cpu_address_range(u64::MAX - 0x800, 0x2000), None);
    }
}
