// SPDX-FileCopyrightText: Copyright 2024 ruzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `video_core/host1x/gpu_device_memory_manager.h` and
//! `gpu_device_memory_manager.cpp`.
//!
//! Defines the Maxwell device memory traits and the device memory manager type
//! alias. In C++ this instantiates `Core::DeviceMemoryManager<MaxwellDeviceTraits>`;
//! in Rust this file owns the current SMMU-backed subset while the full
//! physical-base-relative device-address table model is still being ported.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use common::address_space::FlatAllocator64;
use common::range_mutex::{RangeMutex, ScopedRangeLock};
use common::settings::MemoryLayout;
use ruzu_core::device_memory::DeviceMemory;
use ruzu_core::memory::memory::Memory;

use crate::buffer_cache::word_manager::DeviceTracker;

/// Number of virtual address bits for the Maxwell device address space.
///
/// Port of `MaxwellDeviceTraits::device_virtual_bits`.
pub const DEVICE_VIRTUAL_BITS: usize = 34;

/// Page size constants matching `core/memory.h:YUZU_PAGEBITS` /
/// `YUZU_PAGESIZE`. ruzu's `common::types::PAGE_SIZE` is 0x1000.
pub const PAGE_BITS: u32 = 12;
pub const PAGE_SIZE: u64 = 1 << PAGE_BITS;

/// Device address type (matches upstream `DAddr`).
pub type DAddr = u64;

/// Fallback callback signature for `MaxwellDeviceMethods::MarkRegionCaching`.
/// `(address, size, caching)`. The upstream inline method forwards through
/// `Core::Memory::Memory::RasterizerMarkRegionCached(address, size, caching)`;
/// Rust's owner-backed SMMU path calls the registered `Memory` directly, and
/// keeps this callback only for partial/test construction without CPU-backing
/// owner metadata.
#[cfg(test)]
pub type MarkRegionCachingFn = Box<dyn Fn(u64, usize, bool) + Send + Sync>;
pub type InvalidateRegionFn = Box<dyn Fn(DAddr, usize) + Send + Sync>;
pub type FlushRegionFn = Box<dyn Fn(DAddr, usize) + Send + Sync>;
type SharedInvalidateRegionFn = Arc<dyn Fn(DAddr, usize) + Send + Sync>;
type SharedFlushRegionFn = Arc<dyn Fn(DAddr, usize) + Send + Sync>;

/// Port of `Core::DeviceMemoryManager<MaxwellDeviceTraits>`.
///
/// This port implements the SMMU-backed read/write/cache slices used by the
/// current Host1x users. It uses dense device-page and physical-page tables for
/// the upstream `VirtualBuffer` counterparts.
pub struct MaxwellDeviceMemoryManager {
    /// Per-page reference count. Dense Rust counterpart of upstream
    /// `CachedPages`, whose entries contain `std::atomic_uint8_t` counters.
    cached_pages: Box<[AtomicU8]>,
    /// Port of upstream `Common::RangeMutex counter_guard`, used by
    /// `UpdatePagesCachedCount` and `UpdatePagesCachedBatch` through
    /// `ScopedRangeLock`.
    cached_pages_guard: RangeMutex,

    /// Test-only fallback invoked when reduced fixtures do not install a
    /// registered process `Memory` owner. Runtime pages without a registered
    /// owner do nothing, matching upstream's `memory_device_inter == nullptr`
    /// guard in `UpdatePagesCachedCount`.
    #[cfg(test)]
    mark_region_caching: Mutex<Option<MarkRegionCachingFn>>,

    /// Optional callback invoked after device writes. Mirrors upstream
    /// `DeviceMemoryManager<Traits>::WriteBlock`, which writes to backing
    /// memory and then calls `device_inter->InvalidateRegion(address, size)`.
    invalidate_region: Mutex<Option<SharedInvalidateRegionFn>>,

    /// Optional callback invoked before safe device reads. Mirrors upstream
    /// `DeviceMemoryManager<Traits>::ReadBlock`, which calls
    /// `device_inter->FlushRegion(address, size)` before copying from backing
    /// memory. Unsafe reads intentionally skip this callback.
    flush_region: Mutex<Option<SharedFlushRegionFn>>,

    /// SMMU address-space allocator. Mirrors upstream
    /// `DeviceMemoryManagerAllocator::main_allocator`.
    smmu_allocator: Mutex<FlatAllocator64>,

    /// Registered process memory interfaces indexed by ASID. Mirrors
    /// upstream's `registered_processes` plus `id_pool`, using Rust's
    /// per-process `Memory` owner in place of a raw `Core::Memory::Memory*`.
    smmu_registered_processes: Mutex<SmmuRegisteredProcesses>,

    /// Host backing base for `Core::DeviceMemory`, matching upstream's const
    /// `DeviceMemoryManager<Traits>::physical_base`. Runtime construction sets
    /// it before sharing the manager; zero is reserved for reduced fixtures
    /// that do not provide device memory.
    smmu_physical_base: AtomicUsize,

    /// Constructor-time physical reverse-table size. Upstream allocates
    /// `compressed_device_addr` once in `DeviceMemoryManager(...)` from the
    /// current memory layout setting and all later bounds follow that table.
    smmu_physical_page_count: usize,

    /// Device page -> compressed physical page (`physical_page + 1`).
    /// Atomic dense Rust counterpart for upstream `compressed_physical_ptr`.
    smmu_compressed_physical_ptr: AtomicDevicePageTable,

    /// Device page -> encoded upstream CPU backing metadata. Dense Rust
    /// counterpart for upstream `cpu_backing_address`, initialized to zero.
    smmu_cpu_backing: Mutex<DenseCpuBackingTable>,

    /// Device page -> number of physically-contiguous pages from this page.
    /// Dense Rust counterpart for upstream `continuity_tracker`, initialized
    /// to one entry per page.
    smmu_continuity_tracker: Mutex<DenseDevicePageTable>,

    /// Physical page -> encoded device-page reverse mapping. Dense Rust
    /// counterpart for upstream `compressed_device_addr` plus
    /// `MultiAddressContainer` reverse physical-to-device tracking.
    smmu_reverse_mappings: Mutex<SmmuReverseMappings>,

    /// Port of upstream `mapping_guard`. Serializes complete map, unmap,
    /// continuity-tracking, and reverse-gather operations.
    smmu_mapping_guard: Mutex<()>,

    /// Four-entry device-page translation cache (`t_slot` / `cache_cursor`).
    /// Upstream consults it for single-page reads and clears it after every
    /// map/unmap operation. Atomic fields provide Rust interior mutability for
    /// the manager's shared `Arc` ownership without serializing every cache
    /// hit; contents and replacement are otherwise exact.
    smmu_translation_cache: TranslationCache,
}

/// Page size used by the SMMU page table — matches CPU page size so each
/// host page maps to one SMMU page.
pub const SMMU_PAGE_BITS: u32 = 12;
pub const SMMU_PAGE_SIZE: u64 = 1 << SMMU_PAGE_BITS;
pub const SMMU_DEVICE_PAGE_COUNT: usize = 1usize << (DEVICE_VIRTUAL_BITS - SMMU_PAGE_BITS as usize);
const PHYSICAL_MIN_BITS: usize = 32;
const PHYSICAL_MAX_BITS: usize = 33;

/// Base and limit for upstream `DeviceMemoryManagerAllocator`.
///
/// Upstream uses `first_address = 1 << YUZU_PAGEBITS` and
/// `FlatAllocator<DAddr, 0, MaxwellDeviceTraits::device_virtual_bits>`.
pub const SMMU_BASE: u64 = 1 << SMMU_PAGE_BITS;
pub const SMMU_VA_LIMIT: u64 = (1 << DEVICE_VIRTUAL_BITS) - 1;
const MULTI_FLAG_BITS: u32 = 31;
const MULTI_FLAG: u32 = 1 << MULTI_FLAG_BITS;
const MULTI_MASK: u32 = !MULTI_FLAG;

fn smmu_num_pages_for_size(size: usize) -> u64 {
    ((size as u64).wrapping_add(SMMU_PAGE_SIZE - 1)) >> SMMU_PAGE_BITS
}

struct AtomicDevicePageTable {
    values: Box<[AtomicU32]>,
    default_value: u32,
}

impl AtomicDevicePageTable {
    fn new(default_value: u32) -> Self {
        Self {
            values: std::iter::repeat_with(|| AtomicU32::new(default_value))
                .take(SMMU_DEVICE_PAGE_COUNT)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            default_value,
        }
    }

    fn load(&self, page: u64) -> u32 {
        self.values
            .get(page as usize)
            .map_or(self.default_value, |entry| entry.load(Ordering::Acquire))
    }

    fn store(&self, page: u64, value: u32) {
        if let Some(entry) = self.values.get(page as usize) {
            entry.store(value, Ordering::Release);
        }
    }

    fn clear(&self, page: u64) -> Option<u32> {
        let entry = self.values.get(page as usize)?;
        let old = entry.swap(self.default_value, Ordering::AcqRel);
        (old != self.default_value).then_some(old)
    }
}

struct DenseDevicePageTable {
    values: Vec<u32>,
    default_value: u32,
}

impl DenseDevicePageTable {
    fn new(default_value: u32) -> Self {
        Self {
            values: vec![default_value; SMMU_DEVICE_PAGE_COUNT],
            default_value,
        }
    }

    fn get(&self, page: &u64) -> Option<&u32> {
        self.values.get(*page as usize)
    }

    fn insert(&mut self, page: u64, value: u32) -> Option<u32> {
        let entry = self.values.get_mut(page as usize)?;
        let old = std::mem::replace(entry, value);
        (old != self.default_value).then_some(old)
    }
}

const GUEST_MAX_AS_BITS: u32 = 39;
const GUEST_MASK: u64 = (1u64 << GUEST_MAX_AS_BITS) - 1;

struct DenseCpuBackingTable {
    values: Vec<u64>,
}

impl DenseCpuBackingTable {
    fn new() -> Self {
        Self {
            values: vec![0; SMMU_DEVICE_PAGE_COUNT],
        }
    }

    fn get(&self, page: &u64) -> Option<CpuBacking> {
        let content = *self.values.get(*page as usize)?;
        Some(CpuBacking {
            asid: (content >> GUEST_MAX_AS_BITS) as u32,
            virtual_address: content & GUEST_MASK,
        })
    }

    fn insert(&mut self, page: u64, backing: CpuBacking) {
        if let Some(entry) = self.values.get_mut(page as usize) {
            *entry = backing.virtual_address | ((backing.asid as u64) << GUEST_MAX_AS_BITS);
        }
    }

    fn remove(&mut self, page: &u64) {
        if let Some(entry) = self.values.get_mut(*page as usize) {
            *entry = 0;
        }
    }

    #[cfg(test)]
    fn contains_key(&self, page: &u64) -> bool {
        self.values
            .get(*page as usize)
            .is_some_and(|content| *content != 0)
    }
}

struct DensePhysicalPageTable {
    values: Vec<u32>,
}

impl DensePhysicalPageTable {
    fn new(page_count: usize) -> Self {
        Self {
            values: vec![0; page_count],
        }
    }

    fn get(&self, page: u32) -> u32 {
        self.values.get(page as usize).copied().unwrap_or(0)
    }

    fn set(&mut self, page: u32, value: u32) {
        if let Some(entry) = self.values.get_mut(page as usize) {
            *entry = value;
        }
    }
}

#[derive(Default)]
struct TranslationEntry {
    guest_page: AtomicU64,
    host_ptr: AtomicUsize,
}

struct TranslationCache {
    slots: [TranslationEntry; 4],
    cursor: AtomicU32,
}

impl Default for TranslationCache {
    fn default() -> Self {
        Self {
            slots: std::array::from_fn(|_| TranslationEntry::default()),
            cursor: AtomicU32::new(0),
        }
    }
}

impl TranslationCache {
    fn find(&self, guest_page: DAddr) -> Option<usize> {
        self.slots.iter().find_map(|entry| {
            let first_host_ptr = entry.host_ptr.load(Ordering::Acquire);
            if first_host_ptr == 0 || entry.guest_page.load(Ordering::Relaxed) != guest_page {
                return None;
            }
            let second_host_ptr = entry.host_ptr.load(Ordering::Acquire);
            (first_host_ptr == second_host_ptr).then_some(first_host_ptr)
        })
    }

    fn insert(&self, guest_page: DAddr, host_ptr: usize) {
        let cursor = self
            .cursor
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cursor| {
                Some(cursor.wrapping_add(1) & 3)
            })
            .unwrap();
        let slot = cursor as usize;
        let entry = &self.slots[slot];
        entry.host_ptr.store(0, Ordering::Release);
        entry.guest_page.store(guest_page, Ordering::Relaxed);
        entry.host_ptr.store(host_ptr, Ordering::Release);
    }

    fn clear(&self) {
        for entry in &self.slots {
            entry.host_ptr.store(0, Ordering::Release);
            entry.guest_page.store(0, Ordering::Relaxed);
        }
        self.cursor.store(0, Ordering::Relaxed);
    }
}

fn smmu_physical_bits() -> usize {
    if *common::settings::values().memory_layout_mode.get_value() == MemoryLayout::Memory4Gb {
        PHYSICAL_MIN_BITS
    } else {
        PHYSICAL_MAX_BITS
    }
}

fn smmu_physical_page_count_from_settings() -> usize {
    1usize << (smmu_physical_bits() - SMMU_PAGE_BITS as usize)
}

#[derive(Default)]
struct SmmuRegisteredProcesses {
    /// Each entry stores the process memory plus a raw pointer to the
    /// `Memory` value inside the `Arc<Mutex<..>>` allocation (as usize),
    /// captured once at registration. The pointee is heap-stable for the
    /// Arc's lifetime, which the registry itself keeps alive.
    ///
    /// The raw pointer exists so `update_pages_cached_count` can call
    /// `Memory::rasterizer_mark_region_cached` (atomic page-entry stores +
    /// mprotect only) WITHOUT taking the Memory mutex. Upstream
    /// `MarkRegionCaching` is lock-free; taking the mutex here deadlocks:
    /// the GPU thread holds the shader-cache lock (draw →
    /// `ShaderCache::register` → here) while a CPU core holds the Memory
    /// mutex (guest write → `handle_rasterizer_write` →
    /// `ShaderCache::invalidate_region`).
    processes: Vec<Option<(Arc<Mutex<Memory>>, usize)>>,
    id_pool: VecDeque<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuBacking {
    asid: u32,
    virtual_address: u64,
}

impl CpuBacking {
    pub fn asid(&self) -> u32 {
        self.asid
    }

    pub fn virtual_address(&self) -> u64 {
        self.virtual_address
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ExtractedCpuBacking {
    asid: u32,
    virtual_page: u64,
}

#[derive(Clone, Copy, Default)]
struct MultiAddressEntry {
    next_entry: u32,
    value: u32,
}

#[derive(Default)]
struct MultiAddressContainer {
    storage: Vec<MultiAddressEntry>,
    free_entries: VecDeque<u32>,
}

impl MultiAddressContainer {
    /// Port of upstream `MultiAddressContainer::GatherValues`.
    fn gather_values_into(
        &self,
        start_entry: u32,
        buffer: &mut common::scratch_buffer::ScratchBuffer<u32>,
    ) {
        buffer.resize(8);
        let mut index = 0usize;
        let mut iter_entry = start_entry;
        loop {
            let current = self.entry(iter_entry);
            if buffer.size() < index + 1 {
                buffer.resize(index + 8);
            }
            buffer[index] = current.value;
            index += 1;
            if current.next_entry == 0 {
                break;
            }
            iter_entry = current.next_entry;
        }
        buffer.resize(index);
    }

    #[cfg(test)]
    fn gather_values(&self, start_entry: u32) -> Vec<u32> {
        if start_entry == 0 {
            return Vec::new();
        }
        let mut values = Vec::new();
        let mut iter_entry = start_entry;
        loop {
            let current = self.entry(iter_entry);
            values.push(current.value);
            if current.next_entry == 0 {
                break;
            }
            iter_entry = current.next_entry;
        }
        values
    }

    fn register(&mut self, value: u32) -> u32 {
        self.register_implementation(value)
    }

    fn register_after(&mut self, value: u32, start_entry: u32) {
        let entry_id = self.register_implementation(value);
        let mut iter_entry = start_entry;
        loop {
            let next_entry = self.entry(iter_entry).next_entry;
            if next_entry == 0 {
                break;
            }
            iter_entry = next_entry;
        }
        self.entry_mut(iter_entry).next_entry = entry_id;
    }

    fn unregister(&mut self, value: u32, start_entry: u32) -> (bool, u32) {
        let mut iter_entry = start_entry;
        let mut previous = 0;
        let mut count = 0usize;
        while self.entry(iter_entry).value != value {
            count += 1;
            previous = iter_entry;
            iter_entry = self.entry(iter_entry).next_entry;
            debug_assert!(iter_entry != 0);
        }

        let next_entry = self.entry(iter_entry).next_entry;
        let mut more_than_one_remaining = false;
        let mut result_start = start_entry;
        if next_entry != 0 {
            let next = self.entry(next_entry);
            more_than_one_remaining = next.next_entry != 0 || previous != 0;
        }
        if previous != 0 {
            self.entry_mut(previous).next_entry = next_entry;
        } else {
            result_start = next_entry;
        }
        self.free_entries.push_back(iter_entry);
        (more_than_one_remaining || count > 1, result_start)
    }

    fn release_entry(&mut self, start_entry: u32) -> u32 {
        let value = self.entry(start_entry).value;
        self.free_entries.push_back(start_entry);
        value
    }

    fn register_implementation(&mut self, value: u32) -> u32 {
        let entry_id = self.new_entry();
        *self.entry_mut(entry_id) = MultiAddressEntry {
            next_entry: 0,
            value,
        };
        entry_id
    }

    fn new_entry(&mut self) -> u32 {
        if let Some(result) = self.free_entries.pop_front() {
            return result;
        }
        self.storage.push(MultiAddressEntry::default());
        self.storage.len() as u32
    }

    fn entry(&self, id: u32) -> &MultiAddressEntry {
        &self.storage[id as usize - 1]
    }

    fn entry_mut(&mut self, id: u32) -> &mut MultiAddressEntry {
        &mut self.storage[id as usize - 1]
    }
}

struct SmmuReverseMappings {
    compressed_device_addr: DensePhysicalPageTable,
    multi_dev_address: MultiAddressContainer,
}

impl SmmuReverseMappings {
    fn new(physical_page_count: usize) -> Self {
        Self {
            compressed_device_addr: DensePhysicalPageTable::new(physical_page_count),
            multi_dev_address: MultiAddressContainer::default(),
        }
    }

    #[cfg(test)]
    fn gather_device_pages(&self, physical_page: u32) -> Vec<u64> {
        let base = self.compressed_device_addr.get(physical_page);
        if base == 0 {
            return Vec::new();
        }
        if (base >> MULTI_FLAG_BITS) == 0 {
            return vec![u64::from(base)];
        }
        self.multi_dev_address
            .gather_values(base & MULTI_MASK)
            .into_iter()
            .map(u64::from)
            .collect()
    }

    /// Port of the single/multi-address split in upstream `ApplyOpOnPAddr`.
    /// A single alias is returned directly; only a multi-alias entry touches
    /// the caller-owned scratch buffer.
    fn gather_device_pages_for_apply(
        &self,
        physical_page: u32,
        scratch: &mut common::scratch_buffer::ScratchBuffer<u32>,
    ) -> Option<u32> {
        let base = self.compressed_device_addr.get(physical_page);
        if (base >> MULTI_FLAG_BITS) == 0 {
            return Some(base);
        }
        self.multi_dev_address
            .gather_values_into(base & MULTI_MASK, scratch);
        None
    }

    fn insert(&mut self, physical_page: u32, device_page: u64) {
        let device_page = device_page as u32;
        let base = self.compressed_device_addr.get(physical_page);
        if base == 0 {
            self.compressed_device_addr.set(physical_page, device_page);
            return;
        }

        if (base >> MULTI_FLAG_BITS) == 0 {
            let start_id = self.multi_dev_address.register(base);
            self.multi_dev_address.register_after(device_page, start_id);
            self.compressed_device_addr
                .set(physical_page, MULTI_FLAG | start_id);
            return;
        }

        let start_id = base & MULTI_MASK;
        self.multi_dev_address.register_after(device_page, start_id);
    }

    fn remove(&mut self, physical_page: u32, device_page: u64) {
        let base = self.compressed_device_addr.get(physical_page);
        if base == 0 {
            return;
        }
        let device_page = device_page as u32;
        if (base >> MULTI_FLAG_BITS) == 0 {
            if base == device_page {
                self.compressed_device_addr.set(physical_page, 0);
            }
            return;
        }

        let start_id = base & MULTI_MASK;
        let (more_entries, new_start) = self.multi_dev_address.unregister(device_page, start_id);
        if !more_entries {
            self.compressed_device_addr.set(
                physical_page,
                self.multi_dev_address.release_entry(new_start),
            );
            return;
        }
        self.compressed_device_addr
            .set(physical_page, MULTI_FLAG | new_start);
    }
}

impl SmmuRegisteredProcesses {
    fn register(&mut self, memory: Option<Arc<Mutex<Memory>>>) -> u32 {
        let entry = memory.map(|m| {
            let raw = {
                let guard = m.lock().unwrap();
                &*guard as *const Memory as usize
            };
            (m, raw)
        });
        if let Some(id) = self.id_pool.pop_front() {
            self.processes[id] = entry;
            id as u32
        } else {
            self.processes.push(entry);
            (self.processes.len() - 1) as u32
        }
    }

    fn unregister(&mut self, asid: u32) {
        let id = asid as usize;
        if let Some(slot) = self.processes.get_mut(id) {
            *slot = None;
            self.id_pool.push_front(id);
        }
    }

    fn get(&self, asid: u32) -> Option<Arc<Mutex<Memory>>> {
        self.processes
            .get(asid as usize)
            .and_then(|memory| memory.as_ref().map(|(m, _)| m.clone()))
    }

    /// Raw pointer (as usize) to the registered process `Memory`. See the
    /// `processes` field doc for the locking rationale and stability proof.
    fn get_raw(&self, asid: u32) -> Option<usize> {
        self.processes
            .get(asid as usize)
            .and_then(|memory| memory.as_ref().map(|(_, raw)| *raw))
    }
}

fn align_smmu_size(size: usize) -> u64 {
    ((size as u64) + SMMU_PAGE_SIZE - 1) & !(SMMU_PAGE_SIZE - 1)
}

/// Implement the texture-cache `GpuMemoryReader` adapter so descriptor
/// tables can read TIC/TSC entries directly via `smmu_read_block`. The
/// texture cache holds an `Arc<MaxwellDeviceMemoryManager>`, so passing
/// `&*device_memory` already yields a `&dyn GpuMemoryReader`.
impl crate::texture_cache::descriptor_table::GpuMemoryReader for MaxwellDeviceMemoryManager {
    fn read_block(&self, d_address: u64, output: &mut [u8]) -> bool {
        self.smmu_read_block_unsafe(d_address, output)
    }

    fn addr_valid(&self, d_address: u64) -> bool {
        self.smmu_get_host_ptr(d_address).is_some()
    }

    fn range_valid(&self, d_address: u64, size: u64) -> bool {
        self.smmu_range_has_mapping(d_address, size as usize)
    }
}

impl Default for MaxwellDeviceMemoryManager {
    fn default() -> Self {
        let physical_page_count = smmu_physical_page_count_from_settings();
        Self {
            cached_pages: (0..SMMU_DEVICE_PAGE_COUNT)
                .map(|_| AtomicU8::new(0))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            cached_pages_guard: RangeMutex::new(),
            #[cfg(test)]
            mark_region_caching: Mutex::new(None),
            invalidate_region: Mutex::new(None),
            flush_region: Mutex::new(None),
            smmu_allocator: Mutex::new(FlatAllocator64::new(SMMU_BASE, SMMU_VA_LIMIT)),
            smmu_registered_processes: Mutex::new(SmmuRegisteredProcesses::default()),
            smmu_physical_base: AtomicUsize::new(0),
            smmu_physical_page_count: physical_page_count,
            smmu_compressed_physical_ptr: AtomicDevicePageTable::new(0),
            smmu_cpu_backing: Mutex::new(DenseCpuBackingTable::new()),
            smmu_continuity_tracker: Mutex::new(DenseDevicePageTable::new(1)),
            smmu_reverse_mappings: Mutex::new(SmmuReverseMappings::new(physical_page_count)),
            smmu_mapping_guard: Mutex::new(()),
            smmu_translation_cache: TranslationCache::default(),
        }
    }
}

impl DeviceTracker for MaxwellDeviceMemoryManager {
    fn update_pages_cached_batch(&self, ranges: &[(DAddr, usize)], delta: i32) {
        MaxwellDeviceMemoryManager::update_pages_cached_batch(self, ranges, delta);
    }
}

impl MaxwellDeviceMemoryManager {
    /// Construct the manager from the process-wide device memory backing.
    ///
    /// Port of upstream `DeviceMemoryManager<Traits>::DeviceMemoryManager(const
    /// DeviceMemory&)`, which captures `physical_base` from
    /// `device_memory.buffer.BackingBasePointer()` during Host1x construction.
    pub fn new_with_device_memory(device_memory: &DeviceMemory) -> Self {
        let manager = Self::default();
        manager.smmu_physical_base.store(
            device_memory.buffer.backing_base_pointer() as usize,
            Ordering::Relaxed,
        );
        manager
    }

    /// Read a value from a device address.
    ///
    /// Port of upstream `DeviceMemoryManager<Traits>::Read<T>` for the current
    /// SMMU bridge: translate the first device page through the dense
    /// compressed-physical table to host backing, then copy the typed value.
    /// Unmapped reads return zero.
    pub fn read_u8(&self, addr: DAddr) -> u8 {
        let ptr = self.get_pointer(addr);
        if ptr.is_null() {
            return 0;
        }
        unsafe { std::ptr::read_unaligned(ptr) }
    }

    pub fn read_u16(&self, addr: DAddr) -> u16 {
        let ptr = self.get_pointer(addr);
        if ptr.is_null() {
            return 0;
        }
        unsafe { std::ptr::read_unaligned(ptr.cast::<u16>()) }
    }

    pub fn read_u32(&self, addr: DAddr) -> u32 {
        let ptr = self.get_pointer(addr);
        if ptr.is_null() {
            return 0;
        }
        unsafe { std::ptr::read_unaligned(ptr.cast::<u32>()) }
    }

    pub fn read_u64(&self, addr: DAddr) -> u64 {
        let ptr = self.get_pointer(addr);
        if ptr.is_null() {
            return 0;
        }
        unsafe { std::ptr::read_unaligned(ptr.cast::<u64>()) }
    }

    /// Write a value to a device address.
    ///
    /// Port of upstream `DeviceMemoryManager<Traits>::Write<T>`. Like upstream,
    /// this typed write does not call `InvalidateRegion`; block writes own that
    /// side effect.
    pub fn write_u8(&self, addr: DAddr, data: u8) {
        let ptr = self.get_pointer_mut(addr);
        if ptr.is_null() {
            return;
        }
        unsafe { std::ptr::write_unaligned(ptr, data) };
    }

    pub fn write_u16(&self, addr: DAddr, data: u16) {
        let ptr = self.get_pointer_mut(addr);
        if ptr.is_null() {
            return;
        }
        unsafe { std::ptr::write_unaligned(ptr.cast::<u16>(), data) };
    }

    pub fn write_u32(&self, addr: DAddr, data: u32) {
        let ptr = self.get_pointer_mut(addr);
        if ptr.is_null() {
            return;
        }
        unsafe { std::ptr::write_unaligned(ptr.cast::<u32>(), data) };
    }

    pub fn write_u64(&self, addr: DAddr, data: u64) {
        let ptr = self.get_pointer_mut(addr);
        if ptr.is_null() {
            return;
        }
        unsafe { std::ptr::write_unaligned(ptr.cast::<u64>(), data) };
    }

    /// Get a pointer to the given device address.
    ///
    /// Port of upstream `DeviceMemoryManager<Traits>::GetPointer<T>` for the
    /// current dense device-page SMMU bridge.
    pub fn get_pointer(&self, addr: DAddr) -> *const u8 {
        self.smmu_get_host_ptr(addr).unwrap_or(std::ptr::null())
    }

    /// Get a mutable pointer to the given device address.
    ///
    /// Port of upstream `DeviceMemoryManager<Traits>::GetPointer<T>` for write
    /// callers. Mutability follows the mapped guest backing, like upstream's raw
    /// pointer returned from device memory.
    pub fn get_pointer_mut(&self, addr: DAddr) -> *mut u8 {
        self.smmu_get_host_ptr(addr)
            .map_or(std::ptr::null_mut(), |ptr| ptr as *mut u8)
    }

    /// Get a mutable pointer to a contiguous device-address span.
    ///
    /// Port of upstream `DeviceMemoryManager<Traits>::GetSpan`. The range must
    /// be covered by the recorded continuity count from its first page; the
    /// final pointer still goes through `GetPointer` so unmapped pages return
    /// null.
    pub fn get_span(&self, src_addr: DAddr, size: usize) -> *mut u8 {
        let page = src_addr >> SMMU_PAGE_BITS;
        let subbits = (src_addr & (SMMU_PAGE_SIZE - 1)) as usize;
        let continuity_pages = self
            .smmu_continuity_tracker
            .lock()
            .unwrap()
            .get(&page)
            .copied()
            .unwrap_or(1)
            .max(1) as usize;

        if (continuity_pages << SMMU_PAGE_BITS) < size.saturating_add(subbits) {
            return std::ptr::null_mut();
        }
        self.get_pointer_mut(src_addr)
    }

    /// Const variant of `get_span`.
    pub fn get_span_const(&self, src_addr: DAddr, size: usize) -> *const u8 {
        self.get_span(src_addr, size) as *const u8
    }

    /// Install or replace the test fallback `MarkRegionCaching` callback.
    /// Normal ASID-backed pages route through their registered `Memory` owner;
    /// runtime pages without that owner are skipped like upstream.
    #[cfg(test)]
    pub fn set_mark_region_caching(&self, callback: MarkRegionCachingFn) {
        *self.mark_region_caching.lock().unwrap() = Some(callback);
    }

    pub fn set_invalidate_region(&self, callback: InvalidateRegionFn) {
        *self.invalidate_region.lock().unwrap() = Some(Arc::from(callback));
    }

    pub fn set_flush_region(&self, callback: FlushRegionFn) {
        *self.flush_region.lock().unwrap() = Some(Arc::from(callback));
    }

    /// Allocate `size` bytes of device-virtual address space. Returns a
    /// device address. Aligned up to `SMMU_PAGE_SIZE` and reserved through
    /// a reusable allocator.
    ///
    /// Port of `Core::DeviceMemoryManager<MaxwellDeviceTraits>::Allocate`.
    pub fn smmu_allocate(&self, size: usize) -> DAddr {
        let aligned_size = align_smmu_size(size);
        if aligned_size == 0 {
            return 0;
        }
        self.smmu_allocator
            .lock()
            .unwrap()
            .allocate(aligned_size)
            .unwrap_or(0)
    }

    /// Register an nvdrv process memory interface and return an ASID.
    ///
    /// Port of upstream `Core::DeviceMemoryManager<Traits>::RegisterProcess`.
    pub fn smmu_register_process(&self, memory: Option<Arc<Mutex<Memory>>>) -> u32 {
        if let Some(memory) = memory.as_ref() {
            if let Some(physical_base) = memory.lock().unwrap().device_memory_backing_base() {
                let stored_base = self.smmu_physical_base.load(Ordering::Relaxed);
                if stored_base == 0 {
                    let _ = self.smmu_physical_base.compare_exchange(
                        0,
                        physical_base,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    );
                }
                debug_assert_eq!(
                    self.smmu_physical_base.load(Ordering::Relaxed),
                    physical_base
                );
            }
        }
        self.smmu_registered_processes
            .lock()
            .unwrap()
            .register(memory)
    }

    /// Unregister an ASID previously returned by `smmu_register_process`.
    pub fn smmu_unregister_process(&self, asid: u32) {
        self.smmu_registered_processes
            .lock()
            .unwrap()
            .unregister(asid);
    }

    /// Free a previously allocated SMMU range.
    ///
    /// Port of `Core::DeviceMemoryManager<MaxwellDeviceTraits>::Free`.
    pub fn smmu_free(&self, d_address: DAddr, size: usize) {
        let aligned_size = align_smmu_size(size);
        if d_address == 0 {
            return;
        }
        self.smmu_allocator
            .lock()
            .unwrap()
            .free(d_address, aligned_size);
    }

    /// Install an SMMU mapping and record upstream CPU-backing metadata.
    ///
    /// Compatibility helper for reduced callers that already hold a host
    /// pointer. Runtime ASID-backed mappings should use `smmu_map`, which
    /// resolves `GetPointerSilent` per device page like upstream
    /// `Core::DeviceMemoryManager<Traits>::Map(address, vaddr, size, asid, track)`.
    pub fn smmu_map_with_cpu_backing(
        &self,
        d_address: DAddr,
        host_ptr: *const u8,
        virtual_address: u64,
        size: usize,
        asid: u32,
        track: bool,
    ) {
        self.smmu_map_internal(
            d_address,
            host_ptr,
            Some(CpuBacking {
                asid,
                virtual_address,
            }),
            size,
            track,
        );
    }

    /// Install an SMMU mapping using the registered process memory for `asid`.
    ///
    /// Port of `Core::DeviceMemoryManager<Traits>::Map(address, virtual_address,
    /// size, asid, track)`: resolve `GetPointerSilent` per device page, store
    /// compressed physical and CPU-backing metadata, then optionally track
    /// continuity for the whole range.
    pub fn smmu_map(
        &self,
        d_address: DAddr,
        virtual_address: u64,
        size: usize,
        asid: u32,
        track: bool,
    ) {
        let Some(memory) = self.smmu_registered_memory(asid) else {
            log::error!(
                "SMMU map requested with unregistered ASID {} at daddr=0x{:X} vaddr=0x{:X} size=0x{:X}",
                asid,
                d_address,
                virtual_address,
                size
            );
            return;
        };

        let _mapping_guard = self.smmu_mapping_guard.lock().unwrap();
        let start_page = d_address >> SMMU_PAGE_BITS;
        let num_pages = smmu_num_pages_for_size(size) as usize;
        for index in 0..num_pages {
            let page = start_page + index as u64;
            let current_vaddr = virtual_address.wrapping_add((index as u64) << SMMU_PAGE_BITS);
            let host_ptr = {
                let memory = memory.lock().unwrap();
                memory.get_pointer_silent(current_vaddr)
            };
            if host_ptr.is_null() {
                self.smmu_map_device_page(
                    page,
                    host_ptr,
                    Some(CpuBacking {
                        asid,
                        virtual_address: current_vaddr,
                    }),
                );
                continue;
            }
            self.smmu_map_device_page(
                page,
                host_ptr,
                Some(CpuBacking {
                    asid,
                    virtual_address: current_vaddr,
                }),
            );
        }
        self.smmu_translation_cache.clear();
        if track {
            self.smmu_track_continuity_registered_impl(d_address, virtual_address, size, &memory);
        }
    }

    fn smmu_compressed_physical_for_host_address(&self, host_address: usize) -> u32 {
        self.smmu_compressed_physical_from_device_memory(host_address)
            .unwrap_or(0)
    }

    fn smmu_compressed_physical_from_device_memory(&self, host_address: usize) -> Option<u32> {
        let physical_base = self.smmu_physical_base.load(Ordering::Relaxed);
        if physical_base == 0 {
            return None;
        }
        let raw_physical = host_address.checked_sub(physical_base)?;
        let physical_page = raw_physical >> SMMU_PAGE_BITS;
        if physical_page >= self.smmu_physical_page_count {
            return None;
        }
        Some(physical_page as u32 + 1)
    }

    fn smmu_registered_compressed_physical_for_host_address(
        &self,
        host_address: usize,
    ) -> Option<u32> {
        self.smmu_compressed_physical_from_device_memory(host_address)
    }

    fn smmu_host_ptr_from_compressed_physical(
        &self,
        compressed_physical: u32,
        page_offset: usize,
    ) -> Option<usize> {
        if compressed_physical == 0 {
            return None;
        }
        let physical_base = self.smmu_physical_base.load(Ordering::Relaxed);
        let physical_page = compressed_physical - 1;
        if physical_base != 0 && (physical_page as usize) < self.smmu_physical_page_count {
            return Some(
                physical_base + ((physical_page as usize) << SMMU_PAGE_BITS) + page_offset,
            );
        }
        None
    }

    #[cfg(test)]
    fn smmu_force_compressed_physical_for_test(&self, device_page: u64, compressed_physical: u32) {
        self.smmu_compressed_physical_ptr
            .store(device_page, compressed_physical);
    }

    #[cfg(test)]
    pub(crate) fn smmu_set_physical_base_for_test(&self, physical_base: usize) {
        self.smmu_physical_base
            .store(physical_base, Ordering::Relaxed);
    }

    fn smmu_map_internal(
        &self,
        d_address: DAddr,
        host_ptr: *const u8,
        cpu_backing: Option<CpuBacking>,
        size: usize,
        track: bool,
    ) {
        if size == 0 {
            return;
        }
        let _mapping_guard = self.smmu_mapping_guard.lock().unwrap();
        let start_page = d_address >> SMMU_PAGE_BITS;
        let end_page = start_page + smmu_num_pages_for_size(size);
        if host_ptr.is_null() {
            self.smmu_clear_compressed_physical_without_invalidation(start_page, end_page);
            self.smmu_translation_cache.clear();
            if track {
                self.smmu_track_continuity_impl(d_address, size);
            }
            return;
        }
        let host_base = host_ptr as usize;
        let d_base = start_page << SMMU_PAGE_BITS;
        {
            let mut reverse = self.smmu_reverse_mappings.lock().unwrap();
            for page in start_page..end_page {
                let page_offset = (page << SMMU_PAGE_BITS) - d_base;
                let host_address = host_base + page_offset as usize;
                let new_compressed = self.smmu_compressed_physical_for_host_address(host_address);
                self.smmu_compressed_physical_ptr
                    .store(page, new_compressed);
                if new_compressed == 0 {
                    continue;
                }
                reverse.insert(new_compressed - 1, page);
            }
        }
        if let Some(backing) = cpu_backing {
            let mut backing_table = self.smmu_cpu_backing.lock().unwrap();
            for page in start_page..end_page {
                let page_offset = (page << SMMU_PAGE_BITS) - d_base;
                let has_mapped_physical = self.smmu_compressed_physical_ptr.load(page) != 0;
                if !has_mapped_physical {
                    backing_table.remove(&page);
                    continue;
                }
                backing_table.insert(
                    page,
                    CpuBacking {
                        asid: backing.asid,
                        virtual_address: backing.virtual_address + page_offset,
                    },
                );
            }
        }
        self.smmu_translation_cache.clear();
        if track {
            self.smmu_track_continuity_impl(d_address, size);
        }
    }

    fn smmu_map_device_page(
        &self,
        page: u64,
        host_ptr: *const u8,
        cpu_backing: Option<CpuBacking>,
    ) {
        if host_ptr.is_null() {
            self.smmu_compressed_physical_ptr.store(page, 0);
            return;
        }

        let new_compressed = self.smmu_compressed_physical_for_host_address(host_ptr as usize);
        self.smmu_map_device_page_compressed(page, new_compressed, cpu_backing);
    }

    /// Apply the compressed physical-page and reverse-mapping updates shared
    /// by the per-page `Map` path and reduced test fixtures.
    fn smmu_map_device_page_compressed(
        &self,
        page: u64,
        new_compressed: u32,
        cpu_backing: Option<CpuBacking>,
    ) {
        {
            let mut reverse = self.smmu_reverse_mappings.lock().unwrap();
            self.smmu_compressed_physical_ptr
                .store(page, new_compressed);
            if new_compressed != 0 {
                reverse.insert(new_compressed - 1, page);
            }
        }

        if let Some(backing) = cpu_backing {
            let mut backing_table = self.smmu_cpu_backing.lock().unwrap();
            if new_compressed == 0 {
                backing_table.remove(&page);
            } else {
                backing_table.insert(page, backing);
            }
        }
    }

    fn smmu_clear_compressed_physical_without_invalidation(&self, start_page: u64, end_page: u64) {
        // Upstream `Map`'s null-backing branch only writes
        // `compressed_physical_ptr[page] = 0` and continues. It does not run
        // the `Unmap` reverse-table or CPU-backing cleanup path.
        for page in start_page..end_page {
            self.smmu_compressed_physical_ptr.store(page, 0);
        }
    }

    pub fn smmu_track_continuity(&self, d_address: DAddr, size: usize) {
        let _mapping_guard = self.smmu_mapping_guard.lock().unwrap();
        self.smmu_track_continuity_impl(d_address, size);
    }

    fn smmu_track_continuity_impl(&self, d_address: DAddr, size: usize) {
        let start_page = d_address >> SMMU_PAGE_BITS;
        let end_page = start_page + smmu_num_pages_for_size(size);
        let mut continuity = self.smmu_continuity_tracker.lock().unwrap();
        let mut last_ptr = 0usize;
        let mut page_count = 1u32;
        for page in (start_page..end_page).rev() {
            let new_ptr = self
                .smmu_host_ptr_from_compressed_physical(
                    self.smmu_compressed_physical_ptr.load(page),
                    0,
                )
                .unwrap_or(0);
            if new_ptr.wrapping_add(SMMU_PAGE_SIZE as usize) == last_ptr {
                page_count += 1;
            } else {
                page_count = 1;
            }
            last_ptr = new_ptr;
            continuity.insert(page, page_count);
        }
    }

    pub fn smmu_track_continuity_registered(
        &self,
        d_address: DAddr,
        virtual_address: u64,
        size: usize,
        asid: u32,
    ) {
        let Some(memory) = self.smmu_registered_memory(asid) else {
            log::error!(
                "SMMU continuity tracking requested with unregistered ASID {} at daddr=0x{:X} vaddr=0x{:X} size=0x{:X}",
                asid,
                d_address,
                virtual_address,
                size
            );
            return;
        };
        let _mapping_guard = self.smmu_mapping_guard.lock().unwrap();
        self.smmu_track_continuity_registered_impl(d_address, virtual_address, size, &memory);
    }

    fn smmu_track_continuity_registered_impl(
        &self,
        d_address: DAddr,
        virtual_address: u64,
        size: usize,
        memory: &Arc<Mutex<Memory>>,
    ) {
        let start_page = d_address >> SMMU_PAGE_BITS;
        let num_pages = smmu_num_pages_for_size(size) as usize;
        let memory = memory.lock().unwrap();
        let mut continuity = self.smmu_continuity_tracker.lock().unwrap();
        let mut last_ptr = 0usize;
        let mut page_count = 1u32;
        for index in (0..num_pages).rev() {
            let current_vaddr = virtual_address.wrapping_add((index as u64) << SMMU_PAGE_BITS);
            let new_ptr = memory.get_pointer_silent(current_vaddr) as usize;
            if new_ptr.wrapping_add(SMMU_PAGE_SIZE as usize) == last_ptr {
                page_count += 1;
            } else {
                page_count = 1;
            }
            last_ptr = new_ptr;
            continuity.insert(start_page + index as u64, page_count);
        }
    }

    fn smmu_reverse_remove(
        reverse: &mut SmmuReverseMappings,
        compressed_physical: u32,
        device_page: u64,
    ) {
        if compressed_physical != 0 {
            reverse.remove(compressed_physical - 1, device_page);
        }
    }

    #[cfg(test)]
    fn smmu_reverse_device_pages_for_test(&self, host_ptr: *const u8) -> Vec<u64> {
        self.smmu_reverse_mappings
            .lock()
            .unwrap()
            .gather_device_pages(
                self.smmu_registered_compressed_physical_for_host_address(host_ptr as usize)
                    .map_or(u32::MAX, |compressed| compressed - 1),
            )
    }

    /// Apply `operation` to every device address that aliases `host_ptr`.
    ///
    /// Rust bridge counterpart of upstream
    /// `DeviceMemoryManager<Traits>::ApplyOpOnPointer`: upstream converts the
    /// pointer to a physical address and fans out through `compressed_device_addr`;
    /// this port uses the sparse reverse host-page table populated by SMMU map.
    /// Returns the number of device aliases visited.
    pub fn smmu_apply_op_on_host_pointer<F>(
        &self,
        host_ptr: *const u8,
        scratch: &mut common::scratch_buffer::ScratchBuffer<u32>,
        mut operation: F,
    ) -> usize
    where
        F: FnMut(DAddr),
    {
        if host_ptr.is_null() {
            return 0;
        }

        let host_address = host_ptr as usize;
        let subbits = (host_address & (SMMU_PAGE_SIZE as usize - 1)) as DAddr;
        let Some(compressed_physical) =
            self.smmu_registered_compressed_physical_for_host_address(host_address)
        else {
            return 0;
        };
        // Upstream `ApplyOpOnPAddr` reads the single-alias entry directly and
        // only serializes the multi-alias gather.  The Rust reverse table has
        // its own mutex, so taking `mapping_guard` here is both redundant and
        // incorrect: CPU memory reads reach this function while holding the
        // process `Memory` mutex, whereas `Map` holds `mapping_guard` before
        // resolving process-memory pointers. Taking both in the opposite
        // order deadlocks (`Memory -> mapping_guard` versus
        // `mapping_guard -> Memory`). Snapshot only the multi-alias list under
        // the reverse table's lock; the common single-alias path does not
        // allocate, matching upstream `ApplyOpOnPAddr`.
        let single_page = self
            .smmu_reverse_mappings
            .lock()
            .unwrap()
            .gather_device_pages_for_apply(compressed_physical - 1, scratch);

        if let Some(device_page) = single_page {
            if std::env::var_os("RUZU_TRACE_TEXTURE_ALIAS").is_some()
                && (0xA6C01..0xA7C01).contains(&device_page)
            {
                eprintln!(
                    "[SMMU_CPU_WRITE_ALIAS] host=0x{host_address:X} physical_page=0x{:X} device=0x{:X}",
                    compressed_physical - 1,
                    (u64::from(device_page) << SMMU_PAGE_BITS) + subbits
                );
            }
            operation((u64::from(device_page) << SMMU_PAGE_BITS) + subbits);
            return 1;
        }

        for &device_page in scratch.iter() {
            operation((u64::from(device_page) << SMMU_PAGE_BITS) + subbits);
        }
        scratch.size()
    }

    fn smmu_extract_cpu_backing(&self, device_page: u64) -> ExtractedCpuBacking {
        self.smmu_cpu_backing
            .lock()
            .unwrap()
            .get(&device_page)
            .map(|backing| ExtractedCpuBacking {
                asid: backing.asid,
                virtual_page: backing.virtual_address >> PAGE_BITS,
            })
            .unwrap_or(ExtractedCpuBacking {
                asid: 0,
                virtual_page: 0,
            })
    }

    pub fn smmu_cpu_backing_for_address(&self, d_address: DAddr) -> Option<CpuBacking> {
        let device_page = d_address >> SMMU_PAGE_BITS;
        let page_offset = d_address & (SMMU_PAGE_SIZE - 1);
        self.smmu_cpu_backing
            .lock()
            .unwrap()
            .get(&device_page)
            .map(|backing| CpuBacking {
                asid: backing.asid,
                virtual_address: backing.virtual_address.wrapping_add(page_offset),
            })
    }

    fn smmu_registered_memory(&self, asid: u32) -> Option<Arc<Mutex<Memory>>> {
        self.smmu_registered_processes.lock().unwrap().get(asid)
    }

    /// Raw pointer (as usize) to the registered process `Memory`, for the
    /// mutex-free `rasterizer_mark_region_cached` call in
    /// `update_pages_cached_count`. See `SmmuRegisteredProcesses::processes`.
    fn smmu_registered_memory_raw(&self, asid: u32) -> Option<usize> {
        self.smmu_registered_processes.lock().unwrap().get_raw(asid)
    }

    /// Remove SMMU mappings for `[d_address, d_address + size)`.
    ///
    /// Port of upstream `Core::DeviceMemoryManager<Traits>::Unmap` for the
    /// current host-pointer backed SMMU bridge: invalidate the device range,
    /// then clear the mapped physical and CPU-backing entries. Upstream does
    /// not clear `continuity_tracker` here, so stale continuity counts are
    /// preserved until the next tracked map over the same pages.
    pub fn smmu_unmap(&self, d_address: DAddr, size: usize) {
        let invalidate_region = self.invalidate_region.lock().unwrap().clone();
        if let Some(callback) = invalidate_region {
            callback(d_address, size);
        }
        let _mapping_guard = self.smmu_mapping_guard.lock().unwrap();
        let start_page = d_address >> SMMU_PAGE_BITS;
        let end_page = start_page + smmu_num_pages_for_size(size);
        let mut reverse = self.smmu_reverse_mappings.lock().unwrap();
        for page in start_page..end_page {
            if let Some(compressed) = self.smmu_compressed_physical_ptr.clear(page) {
                Self::smmu_reverse_remove(&mut reverse, compressed, page);
            }
        }
        drop(reverse);
        let mut backing = self.smmu_cpu_backing.lock().unwrap();
        for page in start_page..end_page {
            backing.remove(&page);
        }
        drop(backing);
        self.smmu_translation_cache.clear();
    }

    /// Look up the host pointer for a device address via the SMMU page
    /// table. Returns `None` if no mapping exists.
    ///
    /// Port of the page-walk inside upstream
    /// `Core::DeviceMemoryManager<Traits>::ReadBlockUnsafe`.
    pub fn smmu_get_host_ptr(&self, d_address: DAddr) -> Option<*const u8> {
        let page = d_address >> SMMU_PAGE_BITS;
        let page_offset = (d_address & (SMMU_PAGE_SIZE - 1)) as usize;
        self.smmu_host_ptr_from_compressed_physical(
            self.smmu_compressed_physical_ptr.load(page),
            page_offset,
        )
        .map(|host_ptr| host_ptr as *const u8)
    }

    /// Return whether any page in a device-address range has backing memory.
    ///
    /// Port of upstream `MemoryManager::GpuToCpuAddress(addr, size)`, which
    /// scans pages and succeeds on the first translatable page.
    pub fn smmu_range_has_mapping(&self, d_address: DAddr, size: usize) -> bool {
        let mut page = d_address >> SMMU_PAGE_BITS;
        let page_last = d_address
            .wrapping_add(size as u64)
            .wrapping_add(SMMU_PAGE_SIZE - 1)
            >> SMMU_PAGE_BITS;
        while page < page_last {
            if self.smmu_get_host_ptr(page << SMMU_PAGE_BITS).is_some() {
                return true;
            }
            page += 1;
        }
        false
    }

    #[inline(never)]
    fn smmu_walk_block<F, U>(
        &self,
        d_address: DAddr,
        size: usize,
        mut on_unmapped: U,
        mut on_memory: F,
    ) -> bool
    where
        F: FnMut(usize, usize, usize),
        U: FnMut(DAddr, usize, usize),
    {
        let continuity = self.smmu_continuity_tracker.lock().unwrap();
        let mut remaining = size;
        let mut buffer_offset = 0usize;
        let mut page = d_address >> SMMU_PAGE_BITS;
        let mut page_offset = (d_address & (SMMU_PAGE_SIZE - 1)) as usize;
        let mut all_mapped = true;

        while remaining > 0 {
            let requested_pages = continuity.get(&page).copied().unwrap_or(1).max(1) as usize;
            let current_daddr = (page << SMMU_PAGE_BITS) + page_offset as u64;
            let base_compressed = self.smmu_compressed_physical_ptr.load(page);
            if base_compressed == 0 {
                let copy_amount =
                    ((requested_pages << SMMU_PAGE_BITS) - page_offset).min(remaining);
                all_mapped = false;
                on_unmapped(current_daddr, buffer_offset, copy_amount);
                remaining -= copy_amount;
                buffer_offset += copy_amount;
                page += requested_pages as u64;
                page_offset = 0;
                continue;
            }

            if let Some(base) = self.smmu_host_ptr_from_compressed_physical(base_compressed, 0) {
                let bytes_in_span =
                    ((requested_pages << SMMU_PAGE_BITS) - page_offset).min(remaining);
                on_memory(base + page_offset, buffer_offset, bytes_in_span);

                remaining -= bytes_in_span;
                buffer_offset += bytes_in_span;
                page += requested_pages as u64;
                page_offset = 0;
            } else {
                let copy_amount =
                    ((requested_pages << SMMU_PAGE_BITS) - page_offset).min(remaining);
                all_mapped = false;
                on_unmapped(current_daddr, buffer_offset, copy_amount);
                remaining -= copy_amount;
                buffer_offset += copy_amount;
                page += requested_pages as u64;
                page_offset = 0;
            }
        }

        all_mapped
    }

    /// Read `output.len()` bytes from a device address into `output`.
    ///
    /// Port of upstream `Core::DeviceMemoryManager<Traits>::ReadBlock`: flush
    /// the device range through the bound interface, then copy through
    /// upstream-style continuity segments. Returns `true` if every segment was
    /// mapped.
    pub fn smmu_read_block(&self, d_address: DAddr, output: &mut [u8]) -> bool {
        self.smmu_flush_region(d_address, output.len());
        self.smmu_read_block_unsafe(d_address, output)
    }

    /// Forward `DeviceMemoryManager::FlushRegion` to its bound device interface.
    pub fn smmu_flush_region(&self, d_address: DAddr, size: usize) {
        let flush_region = self.flush_region.lock().unwrap().clone();
        if let Some(callback) = flush_region {
            callback(d_address, size);
        }
    }

    /// Read `output.len()` bytes from a device address without flushing.
    ///
    /// Port of upstream `Core::DeviceMemoryManager<Traits>::ReadBlockUnsafe`.
    pub fn smmu_read_block_unsafe(&self, d_address: DAddr, output: &mut [u8]) -> bool {
        if output.is_empty() {
            return true;
        }

        let page_offset = (d_address & (SMMU_PAGE_SIZE - 1)) as usize;
        if output.len() <= SMMU_PAGE_SIZE as usize - page_offset {
            let guest_page = d_address & !(SMMU_PAGE_SIZE - 1);
            if let Some(host_ptr) = self.smmu_translation_cache.find(guest_page) {
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        (host_ptr + page_offset) as *const u8,
                        output.as_mut_ptr(),
                        output.len(),
                    );
                }
                return true;
            }

            let page = d_address >> SMMU_PAGE_BITS;
            let compressed = self.smmu_compressed_physical_ptr.load(page);
            if let Some(host_ptr) = self.smmu_host_ptr_from_compressed_physical(compressed, 0) {
                self.smmu_translation_cache.insert(guest_page, host_ptr);
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        (host_ptr + page_offset) as *const u8,
                        output.as_mut_ptr(),
                        output.len(),
                    );
                }
                return true;
            }
        }

        let output_len = output.len();
        let output_ptr = output.as_mut_ptr();
        let all_mapped = self.smmu_walk_block(
            d_address,
            output_len,
            |current_daddr, out_off, copy_amount| {
                log::error!(
                    "Unmapped Device ReadBlock @ 0x{:016X} (start address = 0x{:016X}, size = {})",
                    current_daddr,
                    d_address,
                    output_len
                );
                unsafe {
                    std::ptr::write_bytes(output_ptr.add(out_off), 0, copy_amount);
                }
            },
            |src, out_off, copy_amount| unsafe {
                std::ptr::copy_nonoverlapping(
                    src as *const u8,
                    output_ptr.add(out_off),
                    copy_amount,
                );
            },
        );

        all_mapped
    }

    /// Write `data.len()` bytes from `data` to a device address and invalidate
    /// the written range through the installed GPU callback.
    ///
    /// Port of upstream `Core::DeviceMemoryManager<Traits>::WriteBlock`.
    pub fn smmu_write_block(&self, d_address: DAddr, data: &[u8]) -> bool {
        let all_mapped = self.smmu_write_block_unsafe(d_address, data);
        self.smmu_invalidate_region(d_address, data.len());
        all_mapped
    }

    /// Forward `DeviceMemoryManager::InvalidateRegion` to its bound device interface.
    pub fn smmu_invalidate_region(&self, d_address: DAddr, size: usize) {
        if std::env::var_os("RUZU_TRACE_TEXTURE_ALIAS").is_some()
            && d_address < 0xA7C0_1000
            && d_address.saturating_add(size as u64) > 0xA6C0_1000
        {
            eprintln!("[SMMU_DEVICE_WRITE_INVALIDATE] device=0x{d_address:X} size=0x{size:X}");
        }
        let invalidate_region = self.invalidate_region.lock().unwrap().clone();
        if let Some(callback) = invalidate_region {
            callback(d_address, size);
        }
    }

    /// Write `data.len()` bytes from `data` to a device address without
    /// invalidating GPU caches.
    ///
    /// Port of upstream `Core::DeviceMemoryManager<Traits>::WriteBlockUnsafe`.
    pub fn smmu_write_block_unsafe(&self, d_address: DAddr, data: &[u8]) -> bool {
        if data.is_empty() {
            return true;
        }
        self.smmu_walk_block(
            d_address,
            data.len(),
            |current_daddr, _in_off, _copy_amount| {
                log::error!(
                    "Unmapped Device WriteBlock @ 0x{:016X} (start address = 0x{:016X}, size = {})",
                    current_daddr,
                    d_address,
                    data.len()
                );
            },
            |dst, in_off, copy_amount| unsafe {
                std::ptr::copy_nonoverlapping(
                    data.as_ptr().add(in_off),
                    dst as *mut u8,
                    copy_amount,
                );
            },
        )
    }

    #[cfg(test)]
    fn smmu_walk_block_segments_for_test(
        &self,
        d_address: DAddr,
        size: usize,
    ) -> Option<Vec<usize>> {
        let segments = std::cell::RefCell::new(Vec::new());
        if self.smmu_walk_block(
            d_address,
            size,
            |_current_daddr, _buffer_offset, copy_amount| {
                segments.borrow_mut().push(copy_amount);
            },
            |_host, _buffer_offset, copy_amount| {
                segments.borrow_mut().push(copy_amount);
            },
        ) {
            Some(segments.into_inner())
        } else {
            None
        }
    }

    /// Port of upstream
    /// `Core::DeviceMemoryManager<Traits>::UpdatePagesCachedCount`
    /// (`core/device_memory_manager.inc:510-587`).
    ///
    /// Per-page reference counting: each page's `count.fetch_add(delta)`
    /// runs once, with batched `MarkRegionCaching(begin, bytes, caching)`
    /// calls when contiguous runs of pages transition from `0`↔non-zero.
    ///
    /// **Port adaptations vs upstream**:
    /// - `ExtractCPUBacking` is backed by the dense SMMU CPU-backing table
    ///   recorded by `smmu_map(...)` and by the reduced-fixture
    ///   `smmu_map_with_cpu_backing(...)` bridge. Pages whose encoded backing is
    ///   zero are skipped like upstream; only reduced fixtures without a
    ///   registered `Memory` owner fall back to the legacy callback path.
    /// - 8-bit wrapping counter: matches upstream's `CounterType = u8`
    ///   atomic fetch-add semantics. Upstream comment: "Assume delta is
    ///   either -1 or 1" — same here.
    pub fn update_pages_cached_count(&self, addr: DAddr, size: usize, delta: i32) {
        if size == 0 {
            return;
        }

        let _counter_guard = ScopedRangeLock::new(&self.cached_pages_guard, addr, size as u64);
        self.update_pages_cached_count_no_lock(addr, size, delta);
    }

    /// Port of upstream
    /// `Core::DeviceMemoryManager<Traits>::UpdatePagesCachedBatch`.
    pub fn update_pages_cached_batch(&self, ranges: &[(DAddr, usize)], delta: i32) {
        if ranges.is_empty() {
            return;
        }

        let mut sorted = ranges.to_vec();
        sorted.sort_unstable_by_key(|&(addr, _)| addr);

        let mut coalesced = Vec::with_capacity(sorted.len());
        let (mut current_addr, mut current_size) = sorted[0];
        for &(next_addr, next_size) in &sorted[1..] {
            if current_addr + current_size as u64 >= next_addr {
                let end = (current_addr + current_size as u64).max(next_addr + next_size as u64);
                current_size = (end - current_addr) as usize;
            } else {
                coalesced.push((current_addr, current_size));
                current_addr = next_addr;
                current_size = next_size;
            }
        }
        coalesced.push((current_addr, current_size));

        let lock_begin = coalesced[0].0;
        let &(last_addr, last_size) = coalesced.last().expect("coalesced ranges are not empty");
        let lock_end = last_addr + last_size as u64;
        let _counter_guard =
            ScopedRangeLock::new(&self.cached_pages_guard, lock_begin, lock_end - lock_begin);

        for &(addr, size) in &coalesced {
            self.update_pages_cached_count_no_lock(addr, size, delta);
        }
    }

    /// Port of upstream
    /// `Core::DeviceMemoryManager<Traits>::UpdatePagesCachedCountNoLock`.
    fn update_pages_cached_count_no_lock(&self, addr: DAddr, size: usize, delta: i32) {
        if size == 0 {
            return;
        }

        let page_begin = addr >> PAGE_BITS;
        let page_end = (addr + size as u64 + PAGE_SIZE - 1) >> PAGE_BITS;
        if std::env::var_os("RUZU_TRACE_TEXTURE_ALIAS").is_some()
            && addr < 0xA7C0_1000
            && addr.saturating_add(size as u64) > 0xA6C0_1000
        {
            let first = self.smmu_extract_cpu_backing(page_begin);
            let target = self.smmu_extract_cpu_backing(0xA77D_7000 >> PAGE_BITS);
            eprintln!(
                "[SMMU_TRACK_ALIAS] device=0x{addr:X} size=0x{size:X} delta={delta} first_asid={} first_vpage=0x{:X} target_asid={} target_vpage=0x{:X}",
                first.asid,
                first.virtual_page,
                target.asid,
                target.virtual_page
            );
        }

        // Pending-batch tracking for grouped MarkRegionCaching calls.
        // `uncache_*` accumulates pages that just transitioned to count==0.
        // `cache_*` accumulates pages that just transitioned to count==1 (and delta>0).
        let mut uncache_begin: u64 = 0;
        let mut uncache_bytes: u64 = 0;
        let mut cache_begin: u64 = 0;
        let mut cache_bytes: u64 = 0;

        fn release_pending(
            manager: &MaxwellDeviceMemoryManager,
            memory: Option<usize>,
            uncache_begin: &mut u64,
            uncache_bytes: &mut u64,
            cache_begin: &mut u64,
            cache_bytes: &mut u64,
        ) {
            if *uncache_bytes > 0 {
                manager.mark_region_caching(
                    memory,
                    *uncache_begin << PAGE_BITS,
                    *uncache_bytes as usize,
                    false,
                );
                *uncache_bytes = 0;
            }
            if *cache_bytes > 0 {
                manager.mark_region_caching(
                    memory,
                    *cache_begin << PAGE_BITS,
                    *cache_bytes as usize,
                    true,
                );
                *cache_bytes = 0;
            }
        }

        let first_backing = self.smmu_extract_cpu_backing(page_begin);
        let mut old_vpage: u64 = first_backing.virtual_page.wrapping_sub(1);
        let old_asid = first_backing.asid;
        let mut current_memory = self.smmu_registered_memory_raw(old_asid);

        for page in page_begin..page_end {
            let backing = self.smmu_extract_cpu_backing(page);
            if backing.virtual_page == 0 {
                release_pending(
                    self,
                    current_memory,
                    &mut uncache_begin,
                    &mut uncache_bytes,
                    &mut cache_begin,
                    &mut cache_bytes,
                );
                continue;
            }
            if backing.asid != old_asid || backing.virtual_page != old_vpage.wrapping_add(1) {
                release_pending(
                    self,
                    current_memory,
                    &mut uncache_begin,
                    &mut uncache_bytes,
                    &mut cache_begin,
                    &mut cache_bytes,
                );
                if backing.asid != old_asid {
                    current_memory = self.smmu_registered_memory_raw(backing.asid);
                }
            }
            old_vpage = backing.virtual_page;

            // delta is typically ±1; `wrapping_add` matches upstream's
            // `count.fetch_add(static_cast<CounterType>(delta))` on `u8`.
            let count = self
                .cached_pages
                .get(page as usize)
                .expect("device cached-page index must be in range");
            count.fetch_add(delta as u8, Ordering::Release);
            let new_count = count.load(Ordering::Relaxed);

            // Transition to 0 → schedule uncache.
            if new_count == 0 {
                if uncache_bytes == 0 {
                    uncache_begin = backing.virtual_page;
                }
                uncache_bytes += PAGE_SIZE;
            } else if uncache_bytes > 0 {
                // Non-zero count interrupts an in-progress uncache batch.
                self.mark_region_caching(
                    current_memory,
                    uncache_begin << PAGE_BITS,
                    uncache_bytes as usize,
                    false,
                );
                uncache_bytes = 0;
            }

            // First-time cache (0→1 with delta>0) → schedule cache.
            if new_count == 1 && delta > 0 {
                if cache_bytes == 0 {
                    cache_begin = backing.virtual_page;
                }
                cache_bytes += PAGE_SIZE;
            } else if cache_bytes > 0 {
                // Anything else interrupts the cache batch.
                self.mark_region_caching(
                    current_memory,
                    cache_begin << PAGE_BITS,
                    cache_bytes as usize,
                    true,
                );
                cache_bytes = 0;
            }
        }

        release_pending(
            self,
            current_memory,
            &mut uncache_begin,
            &mut uncache_bytes,
            &mut cache_begin,
            &mut cache_bytes,
        );
    }

    /// Rust bridge for upstream `DeviceMethods::MarkRegionCaching`.
    fn mark_region_caching(&self, memory: Option<usize>, address: u64, size: usize, caching: bool) {
        if let Some(memory_raw) = memory {
            // SAFETY: `memory_raw` points at the `Memory` value inside the
            // `Arc<Mutex<Memory>>` held by the SMMU process registry; the
            // allocation remains stable while that registration lives.
            // Eden invokes MarkRegionCaching while holding `counter_guard` and
            // does not acquire the process Memory mutex.
            unsafe {
                (*(memory_raw as *const Memory)).rasterizer_mark_region_cached(
                    address,
                    size as u64,
                    caching,
                );
            }
        } else {
            #[cfg(test)]
            {
                let callback_guard = self.mark_region_caching.lock().unwrap();
                if let Some(callback) = callback_guard.as_ref() {
                    callback(address, size, caching);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer_cache::memory_tracker_base::MemoryTrackerBase;
    use common::page_table::{PageTable, PageType};
    use ruzu_core::core::SystemRef;
    use ruzu_core::device_memory::dram_memory_map;
    use std::sync::Arc;

    /// Helper: a `MarkRegionCaching` callback that records every call so
    /// tests can assert the upstream batching semantics.
    fn recorder() -> (MarkRegionCachingFn, Arc<Mutex<Vec<(u64, usize, bool)>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let log_clone = Arc::clone(&log);
        let cb: MarkRegionCachingFn = Box::new(move |addr, size, caching| {
            log_clone.lock().unwrap().push((addr, size, caching))
        });
        (cb, log)
    }

    fn install_test_physical_base(mgr: &MaxwellDeviceMemoryManager, ptr: *const u8) {
        mgr.smmu_set_physical_base_for_test(ptr as usize);
    }

    fn smmu_map_test_backing(
        mgr: &MaxwellDeviceMemoryManager,
        d_address: DAddr,
        host_ptr: *const u8,
        virtual_address: u64,
        size: usize,
    ) {
        mgr.smmu_map_with_cpu_backing(d_address, host_ptr, virtual_address, size, 5, true);
    }

    #[test]
    fn default_memory_layout_uses_upstream_4gb_physical_reverse_table_size() {
        assert_eq!(
            *common::settings::values().memory_layout_mode.get_value(),
            MemoryLayout::Memory4Gb
        );
        assert_eq!(smmu_physical_bits(), PHYSICAL_MIN_BITS);
        let mgr = MaxwellDeviceMemoryManager::default();
        let expected = 1usize << (PHYSICAL_MIN_BITS - SMMU_PAGE_BITS as usize);
        assert_eq!(mgr.smmu_physical_page_count, expected);
        assert_eq!(
            mgr.smmu_reverse_mappings
                .lock()
                .unwrap()
                .compressed_device_addr
                .values
                .len(),
            expected
        );
    }

    #[test]
    fn new_with_device_memory_installs_physical_base() {
        let device_memory = DeviceMemory::with_size(0x4000);
        let mgr = MaxwellDeviceMemoryManager::new_with_device_memory(&device_memory);
        let backing = unsafe {
            device_memory
                .buffer
                .backing_base_pointer()
                .add(0x2000)
                .cast_const()
        };

        mgr.smmu_map_with_cpu_backing(0x8000, backing, 0x4000_2000, 0x1000, 5, true);

        assert_eq!(mgr.get_pointer(0x8000), backing.cast_mut());
    }

    #[test]
    fn single_page_cache_then_uncache() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let (cb, log) = recorder();
        mgr.set_mark_region_caching(cb);
        let backing = vec![0u8; 0x1000];
        install_test_physical_base(&mgr, backing.as_ptr());
        mgr.smmu_map_with_cpu_backing(0x1000, backing.as_ptr(), 0x1000, 0x1000, 3, true);

        // 0→1 within one page → one cache call.
        mgr.update_pages_cached_count(0x1000, 0x100, 1);
        // 1→0 same page → one uncache call.
        mgr.update_pages_cached_count(0x1000, 0x100, -1);

        let calls = log.lock().unwrap();
        assert_eq!(
            *calls,
            vec![(0x1000, 0x1000, true), (0x1000, 0x1000, false)]
        );
    }

    #[test]
    fn memory_tracker_forwards_cached_page_updates_to_device_memory() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let (cb, log) = recorder();
        mgr.set_mark_region_caching(cb);
        let backing = vec![0u8; 0x1000];
        install_test_physical_base(&mgr, backing.as_ptr());
        mgr.smmu_map_with_cpu_backing(0x2000, backing.as_ptr(), 0x2000, 0x1000, 3, true);

        let mut memory_tracker = MemoryTrackerBase::new(&mgr);
        memory_tracker.unmark_region_as_cpu_modified(0x2000, 0x80);
        memory_tracker.mark_region_as_cpu_modified(0x2000, 0x80);

        assert_eq!(
            *log.lock().unwrap(),
            vec![(0x2000, 0x1000, true), (0x2000, 0x1000, false)]
        );
    }

    #[test]
    fn contiguous_pages_batched_into_one_call() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let (cb, log) = recorder();
        mgr.set_mark_region_caching(cb);
        let backing = vec![0u8; 4 * 0x1000];
        install_test_physical_base(&mgr, backing.as_ptr());
        mgr.smmu_map_with_cpu_backing(0x4000, backing.as_ptr(), 0x4000, 4 * 0x1000, 3, true);

        // 4 pages, 0→1 each → single batched cache call covering all 4.
        mgr.update_pages_cached_count(0x4000, 4 * 0x1000, 1);

        let calls = log.lock().unwrap();
        assert_eq!(*calls, vec![(0x4000, 4 * 0x1000, true)]);
    }

    #[test]
    fn cached_batch_sorts_and_coalesces_overlapping_ranges() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let (cb, log) = recorder();
        mgr.set_mark_region_caching(cb);
        let backing = vec![0u8; 4 * 0x1000];
        install_test_physical_base(&mgr, backing.as_ptr());
        mgr.smmu_map_with_cpu_backing(0x1000, backing.as_ptr(), 0x1000, 4 * 0x1000, 3, true);

        let ranges = [(0x3000, 0x1000), (0x1000, 0x1800), (0x2000, 0x1000)];
        mgr.update_pages_cached_batch(&ranges, 1);

        assert_eq!(mgr.cached_pages[1].load(Ordering::Relaxed), 1);
        assert_eq!(mgr.cached_pages[2].load(Ordering::Relaxed), 1);
        assert_eq!(mgr.cached_pages[3].load(Ordering::Relaxed), 1);
        assert_eq!(*log.lock().unwrap(), vec![(0x1000, 0x3000, true)]);

        mgr.update_pages_cached_batch(&ranges, -1);
        assert_eq!(
            *log.lock().unwrap(),
            vec![(0x1000, 0x3000, true), (0x1000, 0x3000, false)]
        );
    }

    #[test]
    fn empty_cached_batch_is_noop() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let (cb, log) = recorder();
        mgr.set_mark_region_caching(cb);

        mgr.update_pages_cached_batch(&[], 1);

        assert!(log.lock().unwrap().is_empty());
        assert!(mgr
            .cached_pages
            .iter()
            .all(|count| count.load(Ordering::Relaxed) == 0));
    }

    #[test]
    fn refcount_stays_cached_until_zero() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let (cb, log) = recorder();
        mgr.set_mark_region_caching(cb);
        let backing = vec![0u8; 0x1000];
        install_test_physical_base(&mgr, backing.as_ptr());
        mgr.smmu_map_with_cpu_backing(0x2000, backing.as_ptr(), 0x2000, 0x1000, 3, true);

        mgr.update_pages_cached_count(0x2000, 0x1000, 1); // 0→1: cache
        mgr.update_pages_cached_count(0x2000, 0x1000, 1); // 1→2: no callback
        mgr.update_pages_cached_count(0x2000, 0x1000, -1); // 2→1: no callback
        mgr.update_pages_cached_count(0x2000, 0x1000, -1); // 1→0: uncache

        let calls = log.lock().unwrap();
        assert_eq!(
            *calls,
            vec![(0x2000, 0x1000, true), (0x2000, 0x1000, false)]
        );
    }

    #[test]
    fn no_callback_still_tracks_counts_for_cpu_backing() {
        // Counts are maintained even when no `MarkRegionCaching` callback is
        // installed. Upstream only guards the side-effect call on
        // `memory_device_inter != nullptr`.
        let mgr = MaxwellDeviceMemoryManager::default();
        let backing = vec![0u8; 0x2000];
        install_test_physical_base(&mgr, backing.as_ptr());
        mgr.smmu_map_with_cpu_backing(0x3000, backing.as_ptr(), 0x3000, 0x2000, 3, true);
        mgr.update_pages_cached_count(0x3000, 0x2000, 1);
        assert_eq!(mgr.cached_pages[3].load(Ordering::Relaxed), 1);
        assert_eq!(mgr.cached_pages[4].load(Ordering::Relaxed), 1);
    }

    #[test]
    fn missing_cpu_backing_skips_cached_count() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let (cb, log) = recorder();
        mgr.set_mark_region_caching(cb);

        mgr.update_pages_cached_count(0x3000, 0x2000, 1);

        assert!(log.lock().unwrap().is_empty());
        assert!(mgr
            .cached_pages
            .iter()
            .all(|count| count.load(Ordering::Relaxed) == 0));
    }

    #[test]
    fn cached_count_uses_cpu_backing_address_when_present() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let (cb, log) = recorder();
        mgr.set_mark_region_caching(cb);
        let backing = vec![0u8; 0x2000];

        install_test_physical_base(&mgr, backing.as_ptr());
        mgr.smmu_map_with_cpu_backing(0x8000, backing.as_ptr(), 0x4000_0000, 0x2000, 3, true);
        mgr.update_pages_cached_count(0x8000, 0x2000, 1);

        let calls = log.lock().unwrap();
        assert_eq!(*calls, vec![(0x4000_0000, 0x2000, true)]);
    }

    #[test]
    fn cached_count_splits_on_cpu_backing_discontinuity() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let (cb, log) = recorder();
        mgr.set_mark_region_caching(cb);
        {
            let mut backing = mgr.smmu_cpu_backing.lock().unwrap();
            backing.insert(
                0x8,
                CpuBacking {
                    asid: 4,
                    virtual_address: 0x4000_0000,
                },
            );
            backing.insert(
                0x9,
                CpuBacking {
                    asid: 4,
                    virtual_address: 0x5000_0000,
                },
            );
        }

        mgr.update_pages_cached_count(0x8000, 0x2000, 1);

        let calls = log.lock().unwrap();
        assert_eq!(
            *calls,
            vec![(0x4000_0000, 0x1000, true), (0x5000_0000, 0x1000, true)]
        );
    }

    #[test]
    fn cached_count_keeps_upstream_initial_asid_batch_boundary() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let (cb, log) = recorder();
        mgr.set_mark_region_caching(cb);
        {
            let mut backing = mgr.smmu_cpu_backing.lock().unwrap();
            backing.insert(
                0x8,
                CpuBacking {
                    asid: 4,
                    virtual_address: 0x4000_0000,
                },
            );
            backing.insert(
                0x9,
                CpuBacking {
                    asid: 5,
                    virtual_address: 0x5000_0000,
                },
            );
            backing.insert(
                0xa,
                CpuBacking {
                    asid: 5,
                    virtual_address: 0x5000_1000,
                },
            );
        }

        mgr.update_pages_cached_count(0x8000, 0x3000, 1);

        let calls = log.lock().unwrap();
        assert_eq!(
            *calls,
            vec![
                (0x4000_0000, 0x1000, true),
                (0x5000_0000, 0x1000, true),
                (0x5000_1000, 0x1000, true),
            ]
        );
    }

    #[test]
    fn zero_size_is_noop() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let (cb, log) = recorder();
        mgr.set_mark_region_caching(cb);
        mgr.update_pages_cached_count(0x1000, 0, 1);
        assert!(log.lock().unwrap().is_empty());
    }

    #[test]
    fn smmu_free_uses_flat_allocator_linear_pass_before_reuse() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let first = mgr.smmu_allocate(0x1000);
        let second = mgr.smmu_allocate(0x1000);

        mgr.smmu_free(first, 0x1000);
        let third = mgr.smmu_allocate(0x800);

        assert_eq!(first, SMMU_BASE);
        assert_eq!(second, SMMU_BASE + 0x1000);
        assert_eq!(third, SMMU_BASE + 0x2000);
    }

    #[test]
    fn smmu_register_process_reuses_unregistered_asids() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let first = mgr.smmu_register_process(None);
        let second = mgr.smmu_register_process(None);

        mgr.smmu_unregister_process(first);
        let reused = mgr.smmu_register_process(None);

        assert_eq!(first, 0);
        assert_eq!(second, 1);
        assert_eq!(reused, first);
    }

    #[test]
    fn smmu_map_with_cpu_backing_tracks_backing_and_continuity() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let backing = vec![0u8; 0x3000];
        install_test_physical_base(&mgr, backing.as_ptr());
        mgr.smmu_map_with_cpu_backing(0x8000, backing.as_ptr(), 0x4000_0000, 0x3000, 5, true);

        {
            let cpu_backing = mgr.smmu_cpu_backing.lock().unwrap();
            assert_eq!(
                cpu_backing.get(&(0x8000 >> SMMU_PAGE_BITS)),
                Some(CpuBacking {
                    asid: 5,
                    virtual_address: 0x4000_0000,
                })
            );
            assert_eq!(
                cpu_backing.get(&(0x9000 >> SMMU_PAGE_BITS)),
                Some(CpuBacking {
                    asid: 5,
                    virtual_address: 0x4000_1000,
                })
            );
        }

        {
            let continuity = mgr.smmu_continuity_tracker.lock().unwrap();
            assert_eq!(continuity.get(&(0x8000 >> SMMU_PAGE_BITS)), Some(&3));
            assert_eq!(continuity.get(&(0x9000 >> SMMU_PAGE_BITS)), Some(&2));
            assert_eq!(continuity.get(&(0xA000 >> SMMU_PAGE_BITS)), Some(&1));
        }

        mgr.smmu_unmap(0x8000, 0x3000);
        assert_eq!(
            mgr.smmu_cpu_backing
                .lock()
                .unwrap()
                .get(&(0x8000 >> SMMU_PAGE_BITS)),
            Some(CpuBacking {
                asid: 0,
                virtual_address: 0,
            })
        );
        {
            let continuity = mgr.smmu_continuity_tracker.lock().unwrap();
            assert_eq!(continuity.get(&(0x8000 >> SMMU_PAGE_BITS)), Some(&3));
            assert_eq!(continuity.get(&(0x9000 >> SMMU_PAGE_BITS)), Some(&2));
            assert_eq!(continuity.get(&(0xA000 >> SMMU_PAGE_BITS)), Some(&1));
        }
    }

    #[test]
    fn smmu_map_resolves_backing_from_registered_asid_memory() {
        let device_memory = DeviceMemory::with_size(0x10000);
        let mgr = MaxwellDeviceMemoryManager::new_with_device_memory(&device_memory);
        let target = dram_memory_map::BASE + 0x2000;
        let host_ptr = unsafe { device_memory.get_pointer(target) };

        let mut memory = unsafe {
            Memory::new(
                SystemRef::null(),
                &device_memory as *const DeviceMemory,
                &device_memory.buffer as *const common::host_memory::HostMemory,
            )
        };
        let mut page_table = Box::new(PageTable::new());
        page_table.resize(36, PAGE_BITS as usize);
        page_table.map_pages(
            0x4000_0000 >> PAGE_BITS,
            1,
            target,
            PageType::Memory,
            host_ptr as usize,
        );
        page_table.map_pages(
            0x4000_1000 >> PAGE_BITS,
            1,
            target + PAGE_SIZE,
            PageType::Memory,
            unsafe { host_ptr.add(PAGE_SIZE as usize) } as usize,
        );
        memory.set_current_page_table(&mut *page_table, true);

        let asid = mgr.smmu_register_process(Some(Arc::new(Mutex::new(memory))));
        mgr.smmu_map(0x8000, 0x4000_0000, 0x2000, asid, true);

        assert_eq!(mgr.get_pointer(0x8000), host_ptr);
        assert_eq!(
            mgr.smmu_cpu_backing.lock().unwrap().get(&0x8),
            Some(CpuBacking {
                asid,
                virtual_address: 0x4000_0000,
            })
        );
        assert_eq!(
            mgr.smmu_continuity_tracker.lock().unwrap().get(&0x8),
            Some(&2)
        );
    }

    #[test]
    fn smmu_map_does_not_merge_discontiguous_physical_pages() {
        let device_memory = DeviceMemory::with_size(0x10000);
        let mgr = MaxwellDeviceMemoryManager::new_with_device_memory(&device_memory);
        let first_target = dram_memory_map::BASE + 0x2000;
        let second_target = dram_memory_map::BASE + 0x5000;
        let first_host_ptr = unsafe { device_memory.get_pointer(first_target) };
        let second_host_ptr = unsafe { device_memory.get_pointer(second_target) };

        unsafe {
            std::ptr::write_bytes(first_host_ptr, 0x11, 0x1000);
            std::ptr::write_bytes(second_host_ptr, 0x22, 0x1000);
        }

        let mut memory = unsafe {
            Memory::new(
                SystemRef::null(),
                &device_memory as *const DeviceMemory,
                &device_memory.buffer as *const common::host_memory::HostMemory,
            )
        };
        let mut page_table = Box::new(PageTable::new());
        page_table.resize(36, PAGE_BITS as usize);
        page_table.map_pages(
            0x4000_0000 >> PAGE_BITS,
            1,
            first_target,
            PageType::Memory,
            first_host_ptr as usize,
        );
        page_table.map_pages(
            0x4000_1000 >> PAGE_BITS,
            1,
            second_target,
            PageType::Memory,
            second_host_ptr as usize,
        );
        memory.set_current_page_table(&mut *page_table, true);

        let asid = mgr.smmu_register_process(Some(Arc::new(Mutex::new(memory))));
        mgr.smmu_map(0x8000, 0x4000_0000, 0x2000, asid, true);

        let mut output = vec![0u8; 0x2000];
        assert!(mgr.smmu_read_block_unsafe(0x8000, &mut output));
        assert!(output[..0x1000].iter().all(|&value| value == 0x11));
        assert!(output[0x1000..].iter().all(|&value| value == 0x22));
    }

    #[test]
    fn smmu_map_uses_device_page_progression_for_unaligned_virtual_address_like_upstream() {
        let device_memory = DeviceMemory::with_size(0x10000);
        let mgr = MaxwellDeviceMemoryManager::new_with_device_memory(&device_memory);
        let target = dram_memory_map::BASE + 0x2000;
        let host_ptr = unsafe { device_memory.get_pointer(target) };

        let mut memory = unsafe {
            Memory::new(
                SystemRef::null(),
                &device_memory as *const DeviceMemory,
                &device_memory.buffer as *const common::host_memory::HostMemory,
            )
        };
        let mut page_table = Box::new(PageTable::new());
        page_table.resize(36, PAGE_BITS as usize);
        page_table.map_pages(
            0x4000_0000 >> PAGE_BITS,
            1,
            target,
            PageType::Memory,
            host_ptr as usize,
        );
        page_table.map_pages(
            0x4000_1000 >> PAGE_BITS,
            1,
            target + PAGE_SIZE,
            PageType::Memory,
            unsafe { host_ptr.add(PAGE_SIZE as usize) } as usize,
        );
        memory.set_current_page_table(&mut *page_table, true);

        let asid = mgr.smmu_register_process(Some(Arc::new(Mutex::new(memory))));
        mgr.smmu_map(0x8000, 0x4000_0080, 0x2000, asid, true);

        assert_eq!(
            mgr.smmu_cpu_backing.lock().unwrap().get(&0x8),
            Some(CpuBacking {
                asid,
                virtual_address: 0x4000_0080,
            })
        );
        assert_eq!(
            mgr.smmu_cpu_backing.lock().unwrap().get(&0x9),
            Some(CpuBacking {
                asid,
                virtual_address: 0x4000_1080,
            })
        );
        assert_eq!(
            mgr.smmu_walk_block_segments_for_test(0x8080, 0x1800),
            Some(vec![0x1800])
        );
    }

    #[test]
    fn smmu_unmap_uses_upstream_size_only_page_count_for_unaligned_device_address() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let backing = vec![0u8; 0x2000];
        install_test_physical_base(&mgr, backing.as_ptr());
        mgr.smmu_map_with_cpu_backing(0x8000, backing.as_ptr(), 0x4000_0000, 0x2000, 5, true);

        mgr.smmu_unmap(0x8001, 0x1000);

        assert!(mgr.get_pointer(0x8000).is_null());
        assert_eq!(mgr.get_pointer(0x9000), unsafe {
            backing.as_ptr().add(0x1000) as *mut u8
        });
        assert!(!mgr.smmu_cpu_backing.lock().unwrap().contains_key(&0x8));
        assert!(mgr.smmu_cpu_backing.lock().unwrap().contains_key(&0x9));
    }

    #[test]
    fn smmu_track_continuity_after_page_split_mapping_matches_full_range_map() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let backing = vec![0u8; 0x3000];
        install_test_physical_base(&mgr, backing.as_ptr());

        for page in 0..3 {
            let offset = page * SMMU_PAGE_SIZE as usize;
            mgr.smmu_map_with_cpu_backing(
                0x8000 + offset as u64,
                unsafe { backing.as_ptr().add(offset) },
                0x4000_0000 + offset as u64,
                SMMU_PAGE_SIZE as usize,
                5,
                false,
            );
        }
        mgr.smmu_track_continuity(0x8000, 0x3000);

        let continuity = mgr.smmu_continuity_tracker.lock().unwrap();
        assert_eq!(continuity.get(&(0x8000 >> SMMU_PAGE_BITS)), Some(&3));
        assert_eq!(continuity.get(&(0x9000 >> SMMU_PAGE_BITS)), Some(&2));
        assert_eq!(continuity.get(&(0xA000 >> SMMU_PAGE_BITS)), Some(&1));
    }

    #[test]
    fn smmu_walk_block_uses_continuity_for_contiguous_backing() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let backing = vec![0u8; 0x3000];
        install_test_physical_base(&mgr, backing.as_ptr());
        mgr.smmu_map_with_cpu_backing(0x8000, backing.as_ptr(), 0x4000_0000, 0x3000, 5, true);

        assert_eq!(
            mgr.smmu_walk_block_segments_for_test(0x8800, 0x1800),
            Some(vec![0x1800])
        );
    }

    #[test]
    fn smmu_walk_block_trusts_recorded_continuity_like_upstream() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let backing = vec![0u8; 0x6000];
        install_test_physical_base(&mgr, backing.as_ptr());
        mgr.smmu_map_with_cpu_backing(0x8000, backing.as_ptr(), 0x4000_0000, 0x3000, 5, true);
        let other_backing = unsafe { backing.as_ptr().add(0x4000) };
        let other_compressed =
            mgr.smmu_compressed_physical_for_host_address(other_backing as usize);
        mgr.smmu_force_compressed_physical_for_test(0x9, other_compressed);

        assert_eq!(
            mgr.smmu_walk_block_segments_for_test(0x8800, 0x1800),
            Some(vec![0x1800])
        );
    }

    #[test]
    fn smmu_range_has_mapping_matches_upstream_size_aware_gpu_to_cpu_address() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let backing = vec![0u8; 0x1000];
        install_test_physical_base(&mgr, backing.as_ptr());
        mgr.smmu_map_with_cpu_backing(0x9000, backing.as_ptr(), 0x4000_0000, 0x1000, 5, true);

        assert!(mgr.smmu_get_host_ptr(0x8000).is_none());
        assert!(mgr.smmu_range_has_mapping(0x8004, 0x2000));
        assert!(!mgr.smmu_range_has_mapping(0xA000, 0x1000));
    }

    #[test]
    fn get_span_requires_recorded_continuity() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let backing = vec![0u8; 0x3000];
        install_test_physical_base(&mgr, backing.as_ptr());
        mgr.smmu_map_with_cpu_backing(0x8000, backing.as_ptr(), 0x4000_0000, 0x3000, 5, true);

        assert_eq!(mgr.get_span(0x8800, 0x1800), unsafe {
            backing.as_ptr().add(0x800) as *mut u8
        });
        assert!(mgr.get_span(0x8800, 0x2801).is_null());
        assert!(mgr.get_span(0xC000, 0x100).is_null());
    }

    #[test]
    fn smmu_reverse_mapping_tracks_aliases_and_unmap() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let backing = vec![0u8; 0x1000];
        install_test_physical_base(&mgr, backing.as_ptr());
        smmu_map_test_backing(&mgr, 0x8000, backing.as_ptr(), 0x4000_0000, 0x1000);
        smmu_map_test_backing(&mgr, 0xA000, backing.as_ptr(), 0x4000_0000, 0x1000);

        let mut pages = mgr.smmu_reverse_device_pages_for_test(backing.as_ptr());
        pages.sort_unstable();
        assert_eq!(pages, vec![0x8, 0xA]);

        mgr.smmu_unmap(0x8000, 0x1000);
        assert_eq!(
            mgr.smmu_reverse_device_pages_for_test(backing.as_ptr()),
            vec![0xA]
        );

        mgr.smmu_unmap(0xA000, 0x1000);
        assert!(mgr
            .smmu_reverse_device_pages_for_test(backing.as_ptr())
            .is_empty());
    }

    #[test]
    fn smmu_reverse_mapping_handles_three_aliases_and_collapses() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let backing = vec![0u8; 0x1000];
        install_test_physical_base(&mgr, backing.as_ptr());

        smmu_map_test_backing(&mgr, 0x8000, backing.as_ptr(), 0x4000_0000, 0x1000);
        smmu_map_test_backing(&mgr, 0xA000, backing.as_ptr(), 0x4000_0000, 0x1000);
        smmu_map_test_backing(&mgr, 0xC000, backing.as_ptr(), 0x4000_0000, 0x1000);

        let mut pages = mgr.smmu_reverse_device_pages_for_test(backing.as_ptr());
        pages.sort_unstable();
        assert_eq!(pages, vec![0x8, 0xA, 0xC]);

        mgr.smmu_unmap(0xA000, 0x1000);
        let mut pages = mgr.smmu_reverse_device_pages_for_test(backing.as_ptr());
        pages.sort_unstable();
        assert_eq!(pages, vec![0x8, 0xC]);

        mgr.smmu_unmap(0x8000, 0x1000);
        assert_eq!(
            mgr.smmu_reverse_device_pages_for_test(backing.as_ptr()),
            vec![0xC]
        );

        mgr.smmu_unmap(0xC000, 0x1000);
        assert!(mgr
            .smmu_reverse_device_pages_for_test(backing.as_ptr())
            .is_empty());
    }

    #[test]
    fn smmu_reverse_mapping_remap_preserves_old_host_page_like_upstream() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let backing = vec![0u8; 0x5000];
        let old_backing = backing.as_ptr();
        let new_backing = unsafe { backing.as_ptr().add(0x3000) };
        install_test_physical_base(&mgr, backing.as_ptr());

        smmu_map_test_backing(&mgr, 0x8000, old_backing, 0x4000_0000, 0x1000);
        smmu_map_test_backing(&mgr, 0x8000, new_backing, 0x4000_3000, 0x1000);

        assert_eq!(
            mgr.smmu_reverse_device_pages_for_test(old_backing),
            vec![0x8]
        );
        assert_eq!(
            mgr.smmu_reverse_device_pages_for_test(new_backing),
            vec![0x8]
        );
    }

    #[test]
    fn smmu_reverse_mapping_repeated_map_keeps_duplicate_like_upstream() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let backing = vec![0u8; 0x1000];
        install_test_physical_base(&mgr, backing.as_ptr());

        smmu_map_test_backing(&mgr, 0x8000, backing.as_ptr(), 0x4000_0000, 0x1000);
        smmu_map_test_backing(&mgr, 0x8000, backing.as_ptr(), 0x4000_0000, 0x1000);
        assert_eq!(
            mgr.smmu_reverse_device_pages_for_test(backing.as_ptr()),
            vec![0x8, 0x8]
        );

        mgr.smmu_unmap(0x8000, 0x1000);
        assert_eq!(
            mgr.smmu_reverse_device_pages_for_test(backing.as_ptr()),
            vec![0x8]
        );
    }

    #[test]
    fn smmu_unmap_zero_size_still_invalidates_like_upstream() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let invalidations = Arc::new(Mutex::new(Vec::new()));
        let invalidations_clone = Arc::clone(&invalidations);
        mgr.set_invalidate_region(Box::new(move |addr, size| {
            invalidations_clone.lock().unwrap().push((addr, size));
        }));

        mgr.smmu_unmap(0x8000, 0);

        assert_eq!(*invalidations.lock().unwrap(), vec![(0x8000, 0)]);
    }

    #[test]
    fn smmu_map_null_backing_clears_only_compressed_physical() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let backing = vec![0xA5u8; 0x1000];
        let invalidations = Arc::new(Mutex::new(Vec::new()));
        let invalidations_clone = Arc::clone(&invalidations);
        mgr.set_invalidate_region(Box::new(move |addr, size| {
            invalidations_clone.lock().unwrap().push((addr, size));
        }));

        install_test_physical_base(&mgr, backing.as_ptr());
        mgr.smmu_map_with_cpu_backing(0x8000, backing.as_ptr(), 0x4000_0000, 0x1000, 5, true);
        assert!(!mgr.get_pointer(0x8000).is_null());
        assert_eq!(
            mgr.smmu_reverse_device_pages_for_test(backing.as_ptr()),
            vec![0x8]
        );
        assert!(mgr.smmu_cpu_backing.lock().unwrap().contains_key(&0x8));

        mgr.smmu_map_with_cpu_backing(0x8000, std::ptr::null(), 0x4000_0000, 0x1000, 5, true);

        assert!(mgr.get_pointer(0x8000).is_null());
        assert_eq!(
            mgr.smmu_reverse_device_pages_for_test(backing.as_ptr()),
            vec![0x8]
        );
        assert!(mgr.smmu_cpu_backing.lock().unwrap().contains_key(&0x8));
        assert_eq!(
            mgr.smmu_continuity_tracker.lock().unwrap().get(&0x8),
            Some(&1)
        );
        assert!(invalidations.lock().unwrap().is_empty());
    }

    #[test]
    fn smmu_map_with_cpu_backing_requires_physical_base() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let backing = vec![0xA5u8; 0x1000];

        mgr.smmu_map_with_cpu_backing(0x8000, backing.as_ptr(), 0x4000_0000, 0x1000, 5, true);

        assert!(mgr.get_pointer(0x8000).is_null());
        assert!(mgr
            .smmu_reverse_device_pages_for_test(backing.as_ptr())
            .is_empty());
        assert!(!mgr.smmu_cpu_backing.lock().unwrap().contains_key(&0x8));
    }

    #[test]
    fn smmu_apply_op_on_host_pointer_fans_out_aliases_with_offset() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let backing = vec![0u8; 0x4000];
        let aligned_backing = ((backing.as_ptr() as usize + SMMU_PAGE_SIZE as usize - 1)
            & !(SMMU_PAGE_SIZE as usize - 1)) as *const u8;
        install_test_physical_base(&mgr, aligned_backing);
        smmu_map_test_backing(&mgr, 0x8000, aligned_backing, 0x4000_0000, 0x2000);
        smmu_map_test_backing(&mgr, 0xA000, aligned_backing, 0x4000_0000, 0x2000);

        let mut addresses = Vec::new();
        let mut scratch = common::scratch_buffer::ScratchBuffer::new();
        let count = mgr.smmu_apply_op_on_host_pointer(
            unsafe { aligned_backing.add(0x123) },
            &mut scratch,
            |addr| {
                addresses.push(addr);
            },
        );

        addresses.sort_unstable();
        assert_eq!(count, 2);
        assert_eq!(addresses, vec![0x8123, 0xA123]);
        assert_eq!(scratch.size(), 2);
    }

    #[test]
    fn smmu_apply_op_on_host_pointer_leaves_scratch_untouched_for_single_alias() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let backing = vec![0u8; 0x4000];
        let aligned_backing = ((backing.as_ptr() as usize + SMMU_PAGE_SIZE as usize - 1)
            & !(SMMU_PAGE_SIZE as usize - 1)) as *const u8;
        install_test_physical_base(&mgr, aligned_backing);
        smmu_map_test_backing(&mgr, 0x8000, aligned_backing, 0x4000_0000, 0x1000);

        let mut scratch = common::scratch_buffer::ScratchBuffer::with_capacity(3);
        scratch[0] = 0xCAFE_BABE;
        let mut addresses = Vec::new();
        let count = mgr.smmu_apply_op_on_host_pointer(
            unsafe { aligned_backing.add(0x123) },
            &mut scratch,
            |addr| addresses.push(addr),
        );

        assert_eq!(count, 1);
        assert_eq!(addresses, vec![0x8123]);
        assert_eq!(scratch.size(), 3);
        assert_eq!(scratch[0], 0xCAFE_BABE);
    }

    #[test]
    fn smmu_apply_op_on_host_pointer_does_not_wait_for_mapping_guard() {
        let mgr = Arc::new(MaxwellDeviceMemoryManager::default());
        let backing = vec![0u8; 0x4000];
        let aligned_backing = ((backing.as_ptr() as usize + SMMU_PAGE_SIZE as usize - 1)
            & !(SMMU_PAGE_SIZE as usize - 1)) as *const u8;
        install_test_physical_base(&mgr, aligned_backing);
        smmu_map_test_backing(&mgr, 0x8000, aligned_backing, 0x4000_0000, 0x1000);

        // Eden's `ApplyOpOnPAddr` does not acquire `mapping_guard` for a
        // single alias.  Holding it here also reproduces the ordering that
        // deadlocked Bowser's Fury when a CPU memory read entered the apply
        // path while another host thread was mapping memory.
        let mapping_guard = mgr.smmu_mapping_guard.lock().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker_mgr = Arc::clone(&mgr);
        let host_address = aligned_backing as usize;
        let worker = std::thread::spawn(move || {
            let mut addresses = Vec::new();
            let mut scratch = common::scratch_buffer::ScratchBuffer::new();
            let count = worker_mgr.smmu_apply_op_on_host_pointer(
                (host_address + 0x80) as *const u8,
                &mut scratch,
                |addr| addresses.push(addr),
            );
            let _ = tx.send((count, addresses));
        });

        let result = rx.recv_timeout(std::time::Duration::from_secs(1));
        drop(mapping_guard);
        worker.join().unwrap();

        assert_eq!(result.unwrap(), (1, vec![0x8080]));
    }

    #[test]
    fn smmu_single_page_read_does_not_wait_for_mapping_guard() {
        let mgr = Arc::new(MaxwellDeviceMemoryManager::default());
        let backing = vec![0xA5u8; 0x4000];
        let aligned_backing = ((backing.as_ptr() as usize + SMMU_PAGE_SIZE as usize - 1)
            & !(SMMU_PAGE_SIZE as usize - 1)) as *const u8;
        install_test_physical_base(&mgr, aligned_backing);
        smmu_map_test_backing(&mgr, 0x8000, aligned_backing, 0x4000_0000, 0x1000);
        mgr.smmu_translation_cache.clear();

        // Eden serializes Map/Unmap with `mapping_guard`, but its
        // `tracked_entries[page].compressed_physical_ptr` read is lock-free.
        // Exercise a translation-cache miss while that writer guard is held.
        let mapping_guard = mgr.smmu_mapping_guard.lock().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker_mgr = Arc::clone(&mgr);
        let worker = std::thread::spawn(move || {
            let mut output = [0u8; 4];
            let mapped = worker_mgr.smmu_read_block_unsafe(0x8000, &mut output);
            let _ = tx.send((mapped, output));
        });

        let result = rx.recv_timeout(std::time::Duration::from_secs(1));
        drop(mapping_guard);
        worker.join().unwrap();

        assert_eq!(result.unwrap(), (true, [0xA5; 4]));
    }

    #[test]
    fn smmu_physical_base_path_uses_raw_physical_page_indices() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let backing = vec![0u8; 0x8000];
        let physical_base = (backing.as_ptr() as usize + SMMU_PAGE_SIZE as usize - 1)
            & !(SMMU_PAGE_SIZE as usize - 1);
        let mapped = (physical_base + 0x2000) as *const u8;
        mgr.smmu_set_physical_base_for_test(physical_base);

        smmu_map_test_backing(&mgr, 0x8000, mapped, 0x4000_2000, 0x1000);

        assert_eq!(
            mgr.smmu_compressed_physical_ptr
                .load(0x8000 >> SMMU_PAGE_BITS),
            3
        );
        assert_eq!(mgr.get_pointer(0x8000), mapped as *mut u8);
        assert_eq!(mgr.smmu_reverse_device_pages_for_test(mapped), vec![0x8]);

        let mut addresses = Vec::new();
        let mut scratch = common::scratch_buffer::ScratchBuffer::new();
        let count =
            mgr.smmu_apply_op_on_host_pointer(unsafe { mapped.add(0x80) }, &mut scratch, |addr| {
                addresses.push(addr)
            });
        assert_eq!(count, 1);
        assert_eq!(addresses, vec![0x8080]);
    }

    #[test]
    fn smmu_apply_op_on_host_pointer_returns_zero_for_unregistered_pointer() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let backing = vec![0u8; 0x1000];

        let mut called = false;
        let mut scratch = common::scratch_buffer::ScratchBuffer::new();
        let count = mgr
            .smmu_apply_op_on_host_pointer(backing.as_ptr(), &mut scratch, |_addr| called = true);

        assert_eq!(count, 0);
        assert!(!called);
    }

    #[test]
    fn smmu_apply_op_on_host_pointer_visits_zero_device_page_for_unmapped_physical_page() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let backing = vec![0u8; 0x3000];
        let physical_base = (backing.as_ptr() as usize + SMMU_PAGE_SIZE as usize - 1)
            & !(SMMU_PAGE_SIZE as usize - 1);
        mgr.smmu_set_physical_base_for_test(physical_base);

        let mut addresses = Vec::new();
        let mut scratch = common::scratch_buffer::ScratchBuffer::new();
        let count = mgr.smmu_apply_op_on_host_pointer(
            (physical_base + 0x180) as *const u8,
            &mut scratch,
            |addr| {
                addresses.push(addr);
            },
        );

        assert_eq!(count, 1);
        assert_eq!(addresses, vec![0x180]);
    }

    #[test]
    fn read_block_zero_fills_unmapped_segments_but_keeps_mapped_data() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let backing = vec![0xA5u8; 0x1000];
        install_test_physical_base(&mgr, backing.as_ptr());
        smmu_map_test_backing(&mgr, 0x8000, backing.as_ptr(), 0x4000_0000, backing.len());

        let mut output = [0xFFu8; 0x2000];
        let all_mapped = mgr.smmu_read_block_unsafe(0x8000, &mut output);

        assert!(!all_mapped);
        assert_eq!(&output[..0x1000], vec![0xA5u8; 0x1000].as_slice());
        assert_eq!(&output[0x1000..], vec![0u8; 0x1000].as_slice());
    }

    #[test]
    fn write_block_skips_unmapped_segments_but_still_invalidates_range() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let mut backing = vec![0u8; 0x1000];
        let invalidations = Arc::new(Mutex::new(Vec::new()));
        let invalidations_clone = invalidations.clone();
        mgr.set_invalidate_region(Box::new(move |addr, size| {
            invalidations_clone.lock().unwrap().push((addr, size));
        }));
        install_test_physical_base(&mgr, backing.as_ptr());
        smmu_map_test_backing(
            &mgr,
            0x8000,
            backing.as_mut_ptr(),
            0x4000_0000,
            backing.len(),
        );

        let input = vec![0x5Au8; 0x2000];
        let all_mapped = mgr.smmu_write_block(0x8000, &input);

        assert!(!all_mapped);
        assert_eq!(backing, vec![0x5Au8; 0x1000]);
        assert_eq!(*invalidations.lock().unwrap(), vec![(0x8000, 0x2000)]);
    }

    #[test]
    fn write_block_unsafe_does_not_invalidate_range() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let mut backing = vec![0u8; 0x1000];
        let invalidations = Arc::new(Mutex::new(Vec::new()));
        let invalidations_clone = invalidations.clone();
        mgr.set_invalidate_region(Box::new(move |addr, size| {
            invalidations_clone.lock().unwrap().push((addr, size));
        }));
        install_test_physical_base(&mgr, backing.as_ptr());
        smmu_map_test_backing(
            &mgr,
            0x8000,
            backing.as_mut_ptr(),
            0x4000_0000,
            backing.len(),
        );

        let input = vec![0xA5u8; 0x1000];
        assert!(mgr.smmu_write_block_unsafe(0x8000, &input));

        assert_eq!(backing, input);
        assert!(invalidations.lock().unwrap().is_empty());
    }

    #[test]
    fn typed_read_write_uses_smmu_backing() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let mut backing = vec![0u8; 0x1000];
        install_test_physical_base(&mgr, backing.as_ptr());
        smmu_map_test_backing(
            &mgr,
            0x8000,
            backing.as_mut_ptr(),
            0x4000_0000,
            backing.len(),
        );

        mgr.write_u8(0x8000, 0x12);
        mgr.write_u16(0x8001, 0x3456);
        mgr.write_u32(0x8004, 0x789A_BCDE);
        mgr.write_u64(0x8008, 0x0123_4567_89AB_CDEF);

        assert_eq!(mgr.read_u8(0x8000), 0x12);
        assert_eq!(mgr.read_u16(0x8001), 0x3456);
        assert_eq!(mgr.read_u32(0x8004), 0x789A_BCDE);
        assert_eq!(mgr.read_u64(0x8008), 0x0123_4567_89AB_CDEF);
        assert_eq!(backing[0], 0x12);
    }

    #[test]
    fn typed_unmapped_access_matches_upstream_zero_and_noop() {
        let mgr = MaxwellDeviceMemoryManager::default();

        assert_eq!(mgr.read_u8(0x8000), 0);
        assert_eq!(mgr.read_u16(0x8000), 0);
        assert_eq!(mgr.read_u32(0x8000), 0);
        assert_eq!(mgr.read_u64(0x8000), 0);

        mgr.write_u8(0x8000, 0x12);
        mgr.write_u16(0x8000, 0x3456);
        mgr.write_u32(0x8000, 0x789A_BCDE);
        mgr.write_u64(0x8000, 0x0123_4567_89AB_CDEF);
        assert!(mgr.get_pointer(0x8000).is_null());
        assert!(mgr.get_pointer_mut(0x8000).is_null());
    }

    #[test]
    fn read_block_flushes_but_unsafe_read_does_not() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let backing = vec![0xA5u8; 0x1000];
        let flushes = Arc::new(Mutex::new(Vec::new()));
        let flushes_clone = flushes.clone();
        mgr.set_flush_region(Box::new(move |addr, size| {
            flushes_clone.lock().unwrap().push((addr, size));
        }));
        install_test_physical_base(&mgr, backing.as_ptr());
        smmu_map_test_backing(&mgr, 0x8000, backing.as_ptr(), 0x4000_0000, backing.len());

        let mut output = [0u8; 4];
        assert!(mgr.smmu_read_block_unsafe(0x8000, &mut output));
        assert!(flushes.lock().unwrap().is_empty());

        assert!(mgr.smmu_read_block(0x8004, &mut output));
        assert_eq!(*flushes.lock().unwrap(), vec![(0x8004, 4)]);
        assert_eq!(output, [0xA5; 4]);
    }

    #[test]
    fn single_page_read_cache_is_cleared_by_remap_and_unmap() {
        let mgr = MaxwellDeviceMemoryManager::default();
        let mut backing = vec![0u8; 0x2000];
        backing[0] = 0x11;
        backing[0x1000] = 0x22;
        install_test_physical_base(&mgr, backing.as_ptr());
        smmu_map_test_backing(&mgr, 0x8000, backing.as_ptr(), 0x4000_0000, 0x1000);

        let mut output = [0u8; 1];
        assert!(mgr.smmu_read_block_unsafe(0x8000, &mut output));
        assert_eq!(output, [0x11]);
        assert_eq!(mgr.smmu_translation_cache.cursor.load(Ordering::Relaxed), 1);

        smmu_map_test_backing(
            &mgr,
            0x8000,
            unsafe { backing.as_ptr().add(0x1000) },
            0x5000_0000,
            0x1000,
        );
        assert_eq!(mgr.smmu_translation_cache.cursor.load(Ordering::Relaxed), 0);
        assert!(mgr.smmu_read_block_unsafe(0x8000, &mut output));
        assert_eq!(output, [0x22]);

        mgr.smmu_unmap(0x8000, 0x1000);
        assert_eq!(mgr.smmu_translation_cache.cursor.load(Ordering::Relaxed), 0);
        assert!(!mgr.smmu_read_block_unsafe(0x8000, &mut output));
        assert_eq!(output, [0]);
    }

    #[test]
    fn cache_callbacks_run_without_holding_callback_mutexes() {
        let mgr = Arc::new(MaxwellDeviceMemoryManager::default());
        let backing = vec![0xA5u8; 0x1000];
        install_test_physical_base(&mgr, backing.as_ptr());
        smmu_map_test_backing(&mgr, 0x8000, backing.as_ptr(), 0x4000_0000, backing.len());

        let weak = Arc::downgrade(&mgr);
        mgr.set_flush_region(Box::new(move |_, _| {
            weak.upgrade()
                .unwrap()
                .set_flush_region(Box::new(|_, _| {}));
        }));
        let mut output = [0u8; 4];
        assert!(mgr.smmu_read_block(0x8000, &mut output));

        let weak = Arc::downgrade(&mgr);
        mgr.set_invalidate_region(Box::new(move |_, _| {
            weak.upgrade()
                .unwrap()
                .set_invalidate_region(Box::new(|_, _| {}));
        }));
        assert!(mgr.smmu_write_block(0x8000, &[1, 2, 3, 4]));
    }
}
