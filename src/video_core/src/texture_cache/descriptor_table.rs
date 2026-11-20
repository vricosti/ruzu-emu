// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/texture_cache/descriptor_table.h
//!
//! Generic descriptor table that reads GPU-memory-resident descriptor arrays
//! on demand, tracking which entries have been read and whether they changed.

use super::image_base::GPUVAddr;
use common::div_ceil::div_ceil_usize;
use std::mem::MaybeUninit;

/// Read N bytes from a GPU device address into `output`.
///
/// Port of upstream `Tegra::MemoryManager::ReadBlockUnsafe`: the descriptor
/// table calls this with the table base + `index * sizeof(Descriptor)` so
/// `Read(index)` lands on a real TICEntry / TSCEntry from GPU memory.
/// Returns `true` if every byte was successfully read (all underlying
/// pages mapped). Like upstream `ReadBlockUnsafe`, implementations zero any
/// unmapped portions; `DescriptorTable::read` consumes those bytes regardless
/// of the mapping-status return value.
pub trait GpuMemoryReader {
    /// Read `output.len()` bytes from a GPU device address into `output`.
    /// Returns `true` if every byte was successfully read.
    fn read_block(&self, d_address: u64, output: &mut [u8]) -> bool;

    /// Returns `true` if the GPU device address has a backing host mapping
    /// (i.e. the page-table walk succeeds). Used by `is_valid_entry` to
    /// reject TIC descriptors whose `Address()` points outside any mapped
    /// SMMU page. Equivalent to upstream
    /// `Tegra::MemoryManager::GpuToCpuAddress(addr).has_value()`.
    fn addr_valid(&self, d_address: u64) -> bool;

    /// Returns `true` if the GPU device range contains a backing host mapping.
    ///
    /// Equivalent to upstream
    /// `Tegra::MemoryManager::GpuToCpuAddress(addr, size).has_value()` for
    /// TIC validation.
    fn range_valid(&self, d_address: u64, size: u64) -> bool {
        let _ = size;
        self.addr_valid(d_address)
    }
}

// ── DescriptorTable<T, Id> ────────────────────────────────────────────

/// The id type a table keeps per descriptor (`DescriptorTable::bound_ids`). Lichii writes
/// `Id{0}` into entries it clears (descriptor_table.h:88, 107): `ZERO` is that value.
pub trait BoundId: Copy {
    const ZERO: Self;
}

impl BoundId for u32 {
    const ZERO: Self = 0;
}

impl BoundId for common::slot_vector::SlotId {
    const ZERO: Self = common::slot_vector::SlotId { index: 0 };
}

/// A lazily-synchronised descriptor array backed by GPU memory.
///
/// Port of `VideoCommon::DescriptorTable<Descriptor>`.
///
/// Eden accepts a concrete `Tegra::MemoryManager&` in `Read`; ruzu accepts a
/// `&dyn GpuMemoryReader` so the caller can supply either the channel memory
/// manager or the backend-independent SMMU reader.
///
/// `T` must be `Copy + PartialEq + Default`. Reads overwrite stack-backed
/// `MaybeUninit<T>` storage directly, so `T` must be a plain `#[repr(C)]` POD
/// type (TICEntry / TSCEntry both qualify — fixed-size `[u64; 4]` wrappers).
pub struct DescriptorTable<T: Copy + PartialEq + Default, Id: BoundId = u32> {
    /// Bitset: one bit per descriptor; 1 = previously read.
    pub read_descriptors: Vec<u64>,
    /// Cached descriptor values.
    pub descriptors: Vec<T>,
    /// Lichii `bound_ids` (descriptor_table.h:93, Lichii 863730d82e): the image view or sampler
    /// id the texture cache resolved each read descriptor to, indexed like `descriptors`.
    pub bound_ids: Vec<Id>,
    pub current_gpu_addr: GPUVAddr,
    pub current_limit: u32,
    /// Lichii `invalidated` (descriptor_table.h:96): `read_descriptors` is to be cleared before
    /// the next read.
    pub invalidated: bool,
    /// Lichii `changes` (descriptor_table.h:97-99): bumped by every `invalidate` and
    /// `refresh`, so the descriptors read before are only good while it stays the same.
    pub changes: u64,
}

/// Rust adaptation: each table's `changes` starts past every earlier table's, so a table built
/// where a destroyed one was (a channel's) cannot repeat a count a texture memo recorded for
/// that one. Lichii starts every table at 0.
static NEXT_CHANGES_BASE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl<T: Copy + PartialEq + Default, Id: BoundId> DescriptorTable<T, Id> {
    /// Create a new, empty descriptor table.
    pub fn new() -> Self {
        Self {
            read_descriptors: Vec::new(),
            descriptors: Vec::new(),
            bound_ids: Vec::new(),
            current_gpu_addr: 0,
            current_limit: 0,
            invalidated: false,
            changes: NEXT_CHANGES_BASE
                .fetch_add(1u64 << 32, std::sync::atomic::Ordering::Relaxed),
        }
    }

    /// Synchronise the table pointer and limit.
    ///
    /// Returns `true` if the table was refreshed (address or limit changed).
    ///
    /// Port of `DescriptorTable::Synchronize`.
    pub fn synchronize(&mut self, gpu_addr: GPUVAddr, limit: u32) -> bool {
        if self.current_gpu_addr == gpu_addr && self.current_limit == limit {
            return false;
        }
        self.refresh(gpu_addr, limit);
        true
    }

    /// Mark all descriptors as unread.
    ///
    /// Port of Lichii `DescriptorTable::Invalidate` (descriptor_table.h:34-39): cleared on the
    /// next read, since the texture cache invalidates every channel's tables for each image it
    /// deletes, dozens of times a frame while its collector runs. Eden clears at once.
    pub fn invalidate(&mut self) {
        self.invalidated = true;
        self.changes = self.changes.wrapping_add(1);
    }

    /// Lichii `DescriptorTable::LiveBoundIds` (descriptor_table.h:41-44): the bound ids the
    /// current limit addresses.
    pub fn live_bound_ids(&mut self) -> &mut [Id] {
        let ids = self.bound_ids.len().min(self.current_limit as usize + 1);
        &mut self.bound_ids[..ids]
    }

    /// Read a descriptor at `index`.
    ///
    /// Returns `(descriptor, changed)`.  `changed` is `true` if this is the
    /// first read or the value differs from the cached copy.
    ///
    /// Port of upstream `DescriptorTable::Read`: reads `sizeof(Descriptor)`
    /// bytes from `current_gpu_addr + index * sizeof(Descriptor)` via the
    /// supplied `gpu_memory` reader. The caller (texture-cache
    /// `visit_image_view`) wires this up to the channel's
    /// `MaxwellDeviceMemoryManager::smmu_read_block_unsafe`.
    ///
    pub fn read(&mut self, gpu_memory: &dyn GpuMemoryReader, index: u32) -> (T, bool) {
        self.read_with(index, |descriptor_addr, output| {
            gpu_memory.read_block(descriptor_addr, output)
        })
    }

    /// Read a descriptor through a caller-provided GPU-VA reader.
    ///
    /// Same ownership as upstream `DescriptorTable::Read`; this overload exists
    /// because some ruzu call sites have direct access to the channel
    /// `MemoryManager` rather than the backend-independent SMMU reader.
    pub fn read_with(
        &mut self,
        index: u32,
        mut read_block: impl FnMut(GPUVAddr, &mut [u8]) -> bool,
    ) -> (T, bool) {
        debug_assert!(index <= self.current_limit);
        // Lichii descriptor_table.h:49-52.
        if self.invalidated {
            self.clear_read_descriptors();
            self.invalidated = false;
        }
        let item_size = std::mem::size_of::<T>();
        let descriptor_addr = self
            .current_gpu_addr
            .wrapping_add((index as u64).wrapping_mul(item_size as u64));

        // `std::pair<T, bool> result` default-constructs `result.first` in
        // upstream before `ReadBlockUnsafe` overwrites its bytes.
        let mut descriptor = MaybeUninit::new(T::default());
        let descriptor_bytes = unsafe {
            std::slice::from_raw_parts_mut(descriptor.as_mut_ptr().cast::<u8>(), item_size)
        };
        let _all_mapped = read_block(descriptor_addr, descriptor_bytes);
        // SAFETY: the storage started as an initialized `T::default()` and
        // descriptor readers overwrite bytes in place. Descriptor tables are
        // instantiated with POD types whose bit patterns are all valid.
        let descriptor = unsafe { descriptor.assume_init() };

        let changed = if self.is_descriptor_read(index) {
            descriptor != self.descriptors[index as usize]
        } else {
            self.mark_descriptor_as_read(index);
            true
        };

        if changed {
            self.descriptors[index as usize] = descriptor;
        }
        (descriptor, changed)
    }

    /// Read a descriptor from the channel's GPU address space.
    ///
    /// Port of Lichii `DescriptorTable::Read(const MemoryManager&, u32)`
    /// (descriptor_table.h:46-73): this runs for every texture and sampler of
    /// every draw, so an entry within one page is copied from its host
    /// pointer instead of going through the generic block read. Eden always
    /// uses `ReadBlockUnsafe`.
    pub fn read_from_memory(
        &mut self,
        gpu_memory: &crate::memory_manager::MemoryManager,
        index: u32,
    ) -> (T, bool) {
        use ruzu_core::device_memory_manager::{DEVICE_PAGEMASK, DEVICE_PAGESIZE};
        self.read_with(index, |gpu_addr, output| {
            let one_page = (gpu_addr & DEVICE_PAGEMASK as u64) + output.len() as u64
                <= DEVICE_PAGESIZE as u64;
            let host = if one_page {
                gpu_memory.get_pointer(gpu_addr).cast_const()
            } else {
                std::ptr::null()
            };
            if host.is_null() {
                return gpu_memory.read_block_unsafe(gpu_addr, output);
            }
            // SAFETY: `get_pointer` returned the host page holding the whole
            // entry, which stays mapped while the channel is bound.
            unsafe { std::ptr::copy_nonoverlapping(host, output.as_mut_ptr(), output.len()) };
            true
        })
    }

    // ── Private helpers ────────────────────────────────────────────────

    fn refresh(&mut self, gpu_addr: GPUVAddr, limit: u32) {
        self.current_gpu_addr = gpu_addr;
        self.current_limit = limit;
        // Lichii descriptor_table.h:79.
        self.changes = self.changes.wrapping_add(1);

        // Some games repeatedly grow these tables. Match upstream's aggressive
        // 0x80000-entry allocation buckets rather than reallocating at every
        // observed limit.
        let num_descriptors = (((limit.wrapping_add(0x80000)) & !0x7ffff).wrapping_add(1)) as usize;
        self.read_descriptors
            .resize(div_ceil_usize(num_descriptors, 64), 0);
        self.clear_read_descriptors();
        self.invalidated = false;
        self.descriptors.resize(num_descriptors, T::default());
        self.bound_ids.resize(num_descriptors, Id::ZERO);
    }

    /// Port of Lichii `DescriptorTable::ClearReadDescriptors` (descriptor_table.h:102-110):
    /// only the words and bound ids the current limit addresses; a later limit increase
    /// clears its new range in `refresh`.
    fn clear_read_descriptors(&mut self) {
        let words = self
            .read_descriptors
            .len()
            .min(self.current_limit as usize / 64 + 1);
        self.read_descriptors[..words].fill(0);
        let ids = self.bound_ids.len().min(self.current_limit as usize + 1);
        self.bound_ids[..ids].fill(Id::ZERO);
    }

    fn mark_descriptor_as_read(&mut self, index: u32) {
        self.read_descriptors[(index / 64) as usize] |= 1u64 << (index % 64);
    }

    fn is_descriptor_read(&self, index: u32) -> bool {
        (self.read_descriptors[(index / 64) as usize] & (1u64 << (index % 64))) != 0
    }
}

impl<T: Copy + PartialEq + Default, Id: BoundId> Default for DescriptorTable<T, Id> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::DescriptorTable;

    #[test]
    fn refresh_uses_upstream_aggressive_allocation_buckets() {
        let mut table = DescriptorTable::<u32>::new();

        assert!(table.synchronize(0x1000, 0));
        assert_eq!(table.descriptors.len(), 0x80001);
        assert_eq!(table.read_descriptors.len(), (0x80001 + 63) / 64);

        assert!(table.synchronize(0x1000, 0x80000));
        assert_eq!(table.descriptors.len(), 0x100001);
        assert_eq!(table.read_descriptors.len(), (0x100001 + 63) / 64);
    }

    #[test]
    fn synchronize_invalidates_previously_read_descriptors() {
        let mut table = DescriptorTable::<u32>::new();
        table.synchronize(0x1000, 0);

        assert_eq!(
            table.read_with(0, |_, out| {
                out.copy_from_slice(&7u32.to_ne_bytes());
                true
            }),
            (7, true)
        );
        assert_eq!(
            table.read_with(0, |_, out| {
                out.copy_from_slice(&7u32.to_ne_bytes());
                true
            }),
            (7, false)
        );

        assert!(table.synchronize(0x2000, 0));
        assert_eq!(
            table.read_with(0, |_, out| {
                out.copy_from_slice(&7u32.to_ne_bytes());
                true
            }),
            (7, true)
        );
    }

    #[test]
    fn invalidate_clears_read_descriptors_lazily_and_counts_changes() {
        // Lichii descriptor_table.h: Invalidate only flags the table and bumps `changes`; the
        // next Read clears the read bits, so an unchanged descriptor reads as new once.
        let mut table = DescriptorTable::<u32>::new();
        let created = table.changes;
        assert!(table.synchronize(0x1000, 3));
        let refreshed = table.changes;
        assert_eq!(refreshed, created + 1);
        assert!(!table.invalidated);
        let seven = |_, out: &mut [u8]| {
            out.copy_from_slice(&7u32.to_ne_bytes());
            true
        };
        assert_eq!(table.read_with(2, seven), (7, true));
        assert_eq!(table.read_with(2, seven), (7, false));

        table.invalidate();
        table.invalidate();
        assert!(table.invalidated);
        assert_eq!(table.changes, refreshed + 2);
        // The bits stay until the next read.
        assert_ne!(table.read_descriptors[0], 0);
        assert_eq!(table.read_with(2, seven), (7, true));
        assert!(!table.invalidated);
        assert_eq!(table.read_with(2, seven), (7, false));
        assert_eq!(table.changes, refreshed + 2);

        // A refresh clears a pending invalidation too.
        table.invalidate();
        assert!(!table.synchronize(0x1000, 3));
        assert!(table.synchronize(0x2000, 3));
        assert!(!table.invalidated);
        assert_eq!(table.changes, refreshed + 4);
        assert_eq!(table.read_with(2, seven), (7, true));
    }

    #[test]
    fn tables_never_share_a_changes_count() {
        let first = DescriptorTable::<u32>::new();
        let second = DescriptorTable::<u32>::new();
        assert_ne!(first.changes, second.changes);
    }

    #[test]
    fn unmapped_read_consumes_upstream_zero_fill() {
        let mut table = DescriptorTable::<u32>::new();
        table.synchronize(0x1000, 0);

        assert_eq!(
            table.read_with(0, |_, out| {
                out.copy_from_slice(&7u32.to_ne_bytes());
                true
            }),
            (7, true)
        );
        assert_eq!(
            table.read_with(0, |_, out| {
                out.fill(0);
                false
            }),
            (0, true)
        );
    }

    #[test]
    fn bound_ids_follow_the_table_and_are_cleared_with_its_reads() {
        // Lichii descriptor_table.h (863730d82e): the table keeps one id per descriptor; a
        // refresh sizes them like `descriptors`, and clearing the read bits (refresh, or the
        // read after an invalidation) clears the ids the current limit addresses.
        use common::slot_vector::SlotId;
        let mut table = DescriptorTable::<u32, SlotId>::new();
        assert!(table.synchronize(0x1000, 3));
        assert_eq!(table.bound_ids.len(), table.descriptors.len());
        assert!(table.bound_ids.iter().all(|id| id.index == 0));
        let seven = |_, out: &mut [u8]| {
            out.copy_from_slice(&7u32.to_ne_bytes());
            true
        };
        assert_eq!(table.read_with(2, seven), (7, true));
        table.bound_ids[2] = SlotId { index: 5 };
        assert_eq!(table.read_with(2, seven), (7, false));
        assert_eq!(table.bound_ids[2].index, 5);
        assert_eq!(table.live_bound_ids().len(), 4);

        table.invalidate();
        // The ids stay until the next read, like the read bits.
        assert_eq!(table.bound_ids[2].index, 5);
        assert_eq!(table.read_with(1, seven), (7, true));
        assert_eq!(table.bound_ids[2].index, 0);
        assert_eq!(table.read_with(2, seven), (7, true));

        table.bound_ids[2] = SlotId { index: 6 };
        assert!(table.synchronize(0x2000, 3));
        assert_eq!(table.bound_ids[2].index, 0);
    }
}
