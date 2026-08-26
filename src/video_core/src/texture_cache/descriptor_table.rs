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

// ── DescriptorTable<T> ────────────────────────────────────────────────

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
pub struct DescriptorTable<T: Copy + PartialEq + Default> {
    /// Bitset: one bit per descriptor; 1 = previously read.
    pub read_descriptors: Vec<u64>,
    /// Cached descriptor values.
    pub descriptors: Vec<T>,
    pub current_gpu_addr: GPUVAddr,
    pub current_limit: u32,
}

impl<T: Copy + PartialEq + Default> DescriptorTable<T> {
    /// Create a new, empty descriptor table.
    pub fn new() -> Self {
        Self {
            read_descriptors: Vec::new(),
            descriptors: Vec::new(),
            current_gpu_addr: 0,
            current_limit: 0,
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
    /// Port of `DescriptorTable::Invalidate`.
    pub fn invalidate(&mut self) {
        self.read_descriptors.fill(0);
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

    // ── Private helpers ────────────────────────────────────────────────

    fn refresh(&mut self, gpu_addr: GPUVAddr, limit: u32) {
        self.current_gpu_addr = gpu_addr;
        self.current_limit = limit;

        // Some games repeatedly grow these tables. Match upstream's aggressive
        // 0x80000-entry allocation buckets rather than reallocating at every
        // observed limit.
        let num_descriptors = (((limit.wrapping_add(0x80000)) & !0x7ffff).wrapping_add(1)) as usize;
        let old_read_size = self.read_descriptors.len();
        self.read_descriptors
            .resize(div_ceil_usize(num_descriptors, 64), 0);
        let retained_read_size = old_read_size.min(self.read_descriptors.len());
        self.read_descriptors[..retained_read_size].fill(0);
        self.descriptors.resize(num_descriptors, T::default());
    }

    fn mark_descriptor_as_read(&mut self, index: u32) {
        self.read_descriptors[(index / 64) as usize] |= 1u64 << (index % 64);
    }

    fn is_descriptor_read(&self, index: u32) -> bool {
        (self.read_descriptors[(index / 64) as usize] & (1u64 << (index % 64))) != 0
    }
}

impl<T: Copy + PartialEq + Default> Default for DescriptorTable<T> {
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
}
