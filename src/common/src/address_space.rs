//! Port of zuyu/src/common/address_space.h, address_space.inc, and address_space.cpp
//! Status: COMPLET
//! Derniere synchro: 2026-03-05
//!
//! The C++ version is heavily templated. This Rust port keeps the allocator
//! core generic for the bool-backed instantiations currently used by ruzu:
//!   - FlatAllocator<u32, 0, 32>
//!   - FlatAllocator64, used for upstream `FlatAllocator<DAddr, 0, 34>`

use std::fmt::{Debug, UpperHex};
use std::sync::Mutex;

pub trait FlatVa: Copy + Debug + UpperHex + Ord + PartialEq {
    const ZERO: Self;
    fn wrapping_add(self, rhs: Self) -> Self;
    fn wrapping_sub(self, rhs: Self) -> Self;
}

impl FlatVa for u32 {
    const ZERO: Self = 0;

    fn wrapping_add(self, rhs: Self) -> Self {
        u32::wrapping_add(self, rhs)
    }

    fn wrapping_sub(self, rhs: Self) -> Self {
        u32::wrapping_sub(self, rhs)
    }
}

impl FlatVa for u64 {
    const ZERO: Self = 0;

    fn wrapping_add(self, rhs: Self) -> Self {
        u64::wrapping_add(self, rhs)
    }

    fn wrapping_sub(self, rhs: Self) -> Self {
        u64::wrapping_sub(self, rhs)
    }
}

/// Represents a block of memory in the address space.
#[derive(Debug, Clone)]
struct Block<VaType: FlatVa> {
    /// VA of the block
    virt: VaType,
    /// PA (for allocator, this is just a bool mapped/unmapped)
    mapped: bool,
}

impl<VaType: FlatVa> Block<VaType> {
    fn new(virt: VaType, mapped: bool) -> Self {
        Self { virt, mapped }
    }
}

/// FlatAddressSpaceMap for the allocator case (PA = bool, no contig split).
struct FlatAddressSpaceMapBool<VaType: FlatVa> {
    blocks: Vec<Block<VaType>>,
    va_limit: VaType,
    unmap_callback: Option<Box<dyn Fn(VaType, VaType) + Send>>,
}

impl<VaType: FlatVa> FlatAddressSpaceMapBool<VaType> {
    fn new(va_limit: VaType, unmap_callback: Option<Box<dyn Fn(VaType, VaType) + Send>>) -> Self {
        Self {
            blocks: vec![Block::new(VaType::ZERO, false)],
            va_limit,
            unmap_callback,
        }
    }

    fn map_locked(&mut self, virt: VaType, size: VaType) {
        let virt_end = virt.wrapping_add(size);

        assert!(
            virt_end <= self.va_limit,
            "Trying to map a block past the VA limit: virt_end: {:#X}, va_limit: {:#X}",
            virt_end,
            self.va_limit
        );

        let block_end_succ_idx = self.blocks.partition_point(|b| b.virt < virt_end);
        assert!(
            block_end_succ_idx > 0,
            "Trying to map a block before the VA start"
        );

        let block_end_pred_idx = block_end_succ_idx - 1;

        if block_end_succ_idx < self.blocks.len() {
            if self.blocks[block_end_succ_idx].virt != virt_end {
                let tail_mapped = self.blocks[block_end_pred_idx].mapped;

                if block_end_pred_idx > 0 && self.blocks[block_end_pred_idx].virt >= virt {
                    self.blocks[block_end_pred_idx].virt = virt_end;
                    self.blocks[block_end_pred_idx].mapped = tail_mapped;

                    let new_succ_idx = block_end_pred_idx;
                    self.finish_map(virt, new_succ_idx, block_end_succ_idx, size);
                } else {
                    self.blocks
                        .insert(block_end_succ_idx, Block::new(virt_end, tail_mapped));
                    self.blocks
                        .insert(block_end_succ_idx, Block::new(virt, true));
                    if let Some(ref cb) = self.unmap_callback {
                        cb(virt, size);
                    }
                    return;
                }
            } else {
                self.finish_map(virt, block_end_succ_idx, block_end_succ_idx, size);
            }
        } else {
            if block_end_pred_idx > 0 && self.blocks[block_end_pred_idx].virt >= virt {
                self.blocks[block_end_pred_idx].virt = virt_end;
                let new_succ_idx = block_end_pred_idx;
                self.finish_map(virt, new_succ_idx, block_end_succ_idx, size);
            } else {
                self.blocks.push(Block::new(virt_end, false));
                let idx = self.blocks.len() - 1;
                self.blocks.insert(idx, Block::new(virt, true));
                if let Some(ref cb) = self.unmap_callback {
                    cb(virt, size);
                }
                return;
            }
        }
    }

    fn finish_map(
        &mut self,
        virt: VaType,
        block_end_succ_idx: usize,
        _original_end_succ: usize,
        size: VaType,
    ) {
        let virt_end = virt.wrapping_add(size);

        let mut start_succ_idx = block_end_succ_idx;
        while start_succ_idx > 0 && self.blocks[start_succ_idx - 1].virt >= virt {
            start_succ_idx -= 1;
        }

        if self.blocks[start_succ_idx].virt > virt_end {
            panic!(
                "Unsorted block in AS map: virt: {:#X}",
                self.blocks[start_succ_idx].virt
            );
        } else if self.blocks[start_succ_idx].virt == virt_end {
            self.blocks.insert(start_succ_idx, Block::new(virt, true));
        } else {
            if start_succ_idx + 1 < block_end_succ_idx {
                self.blocks.drain(start_succ_idx + 1..block_end_succ_idx);
            }
            self.blocks[start_succ_idx].virt = virt;
            self.blocks[start_succ_idx].mapped = true;
        }

        if let Some(ref cb) = self.unmap_callback {
            cb(virt, size);
        }
    }

    fn unmap_locked(&mut self, virt: VaType, size: VaType) {
        let virt_end = virt.wrapping_add(size);

        assert!(
            virt_end <= self.va_limit,
            "Trying to unmap a block past the VA limit"
        );

        let block_end_succ_idx = self.blocks.partition_point(|b| b.virt < virt_end);
        assert!(block_end_succ_idx > 0);

        let block_end_pred_idx = block_end_succ_idx - 1;

        // If predecessor is unmapped, handle simply
        if !self.blocks[block_end_pred_idx].mapped {
            if self.blocks[block_end_pred_idx].virt > virt {
                self.erase_blocks_with_end_unmapped(virt, block_end_pred_idx);
            }
            if let Some(ref cb) = self.unmap_callback {
                cb(virt, size);
            }
            return;
        }

        if block_end_succ_idx < self.blocks.len()
            && self.blocks[block_end_succ_idx].virt == virt_end
            && !self.blocks[block_end_succ_idx].mapped
        {
            self.erase_blocks_with_end_unmapped(virt, block_end_succ_idx);
            if let Some(ref cb) = self.unmap_callback {
                cb(virt, size);
            }
            return;
        }

        assert!(
            block_end_succ_idx < self.blocks.len(),
            "Unexpected Memory Manager state!"
        );

        if self.blocks[block_end_succ_idx].virt != virt_end {
            if self.blocks[block_end_pred_idx].virt >= virt {
                self.blocks[block_end_pred_idx].virt = virt_end;
                self.blocks[block_end_pred_idx].mapped = true;

                let new_succ = block_end_pred_idx;
                self.finish_unmap(virt, size, new_succ);
            } else {
                self.blocks
                    .insert(block_end_succ_idx, Block::new(virt_end, true));
                self.blocks
                    .insert(block_end_succ_idx, Block::new(virt, false));
                if let Some(ref cb) = self.unmap_callback {
                    cb(virt, size);
                }
                return;
            }
        } else {
            self.finish_unmap(virt, size, block_end_succ_idx);
        }
    }

    fn finish_unmap(&mut self, virt: VaType, size: VaType, block_end_succ_idx: usize) {
        let mut idx = block_end_succ_idx;
        while idx > 0 && self.blocks[idx - 1].virt >= virt {
            idx -= 1;
        }
        let start_pred_idx = if idx > 0 { idx - 1 } else { 0 };
        let start_succ_idx = start_pred_idx + 1;

        let virt_end = virt.wrapping_add(size);
        if start_succ_idx < self.blocks.len() && self.blocks[start_succ_idx].virt > virt_end {
            panic!("Unsorted block in AS map");
        }

        if start_succ_idx < self.blocks.len() && self.blocks[start_succ_idx].virt == virt_end {
            if self.blocks[start_pred_idx].mapped {
                self.blocks.insert(start_succ_idx, Block::new(virt, false));
            }
        } else if !self.blocks[start_pred_idx].mapped {
            let drain_end = block_end_succ_idx.min(self.blocks.len());
            if start_succ_idx < drain_end {
                self.blocks.drain(start_succ_idx..drain_end);
            }
        } else {
            let drain_end = block_end_succ_idx.min(self.blocks.len());
            if start_succ_idx + 1 < drain_end {
                self.blocks.drain(start_succ_idx + 1..drain_end);
            }
            if start_succ_idx < self.blocks.len() {
                self.blocks[start_succ_idx].virt = virt;
                self.blocks[start_succ_idx].mapped = false;
            }
        }

        if let Some(ref cb) = self.unmap_callback {
            cb(virt, size);
        }
    }

    fn erase_blocks_with_end_unmapped(&mut self, virt: VaType, unmapped_end_idx: usize) {
        let mut idx = unmapped_end_idx;
        while idx > 0 && self.blocks[idx - 1].virt >= virt {
            idx -= 1;
        }
        let start_pred_idx = if idx > 0 { idx - 1 } else { 0 };
        let start_succ_idx = start_pred_idx + 1;

        let erase_end = if !self.blocks[start_pred_idx].mapped {
            unmapped_end_idx + 1
        } else {
            self.blocks[unmapped_end_idx].virt = virt;
            unmapped_end_idx
        };

        let erase_end = erase_end.min(self.blocks.len());
        if start_succ_idx < erase_end {
            self.blocks.drain(start_succ_idx..erase_end);
        }
    }
}

/// FlatAllocatorBool - specialises FlatAddressSpaceMap to work as an allocator,
/// with an initial fast linear pass and a subsequent slower pass that iterates
/// until it finds a free block.
pub struct FlatAllocatorBool<VaType: FlatVa> {
    inner: FlatAddressSpaceMapBool<VaType>,
    block_mutex: Mutex<()>,
    virt_start: VaType,
    current_linear_alloc_end: VaType,
    va_limit: VaType,
}

impl<VaType: FlatVa> FlatAllocatorBool<VaType> {
    pub fn new(virt_start: VaType, va_limit: VaType) -> Self {
        Self {
            inner: FlatAddressSpaceMapBool::new(va_limit, None),
            block_mutex: Mutex::new(()),
            virt_start,
            current_linear_alloc_end: virt_start,
            va_limit,
        }
    }

    /// Allocates a region in the AS of the given size and returns its address.
    pub fn allocate(&mut self, size: VaType) -> Option<VaType> {
        let _lock = self.block_mutex.lock().unwrap();

        let mut alloc_start: Option<VaType> = None;
        let alloc_end = self.current_linear_alloc_end.wrapping_add(size);

        // Avoid searching backwards in the address space if possible
        if alloc_end >= self.current_linear_alloc_end && alloc_end <= self.va_limit {
            let succ_idx = self.inner.blocks.partition_point(|b| b.virt < alloc_end);
            if succ_idx == 0 {
                panic!("First block in AS map is invalid!");
            }

            let pred_idx = succ_idx - 1;
            if self.inner.blocks[pred_idx].virt <= self.current_linear_alloc_end {
                alloc_start = Some(self.current_linear_alloc_end);
            } else {
                // Skip over fixed mappings
                let mut pred = pred_idx;
                let mut succ = succ_idx;
                while succ < self.inner.blocks.len() {
                    let gap = self.inner.blocks[succ]
                        .virt
                        .wrapping_sub(self.inner.blocks[pred].virt);
                    if gap < size || self.inner.blocks[pred].mapped {
                        alloc_start = Some(self.inner.blocks[pred].virt);
                        break;
                    }
                    pred = succ;
                    succ += 1;

                    if succ == self.inner.blocks.len() {
                        let end = self.inner.blocks[pred].virt.wrapping_add(size);
                        if end >= self.inner.blocks[pred].virt && end <= self.va_limit {
                            alloc_start = Some(self.inner.blocks[pred].virt);
                        }
                    }
                }
            }
        }

        if let Some(start) = alloc_start {
            self.current_linear_alloc_end = start.wrapping_add(size);
            self.inner.map_locked(start, size);
            Some(start)
        } else {
            // Slower fallback: find a gap
            if self.inner.blocks.len() <= 2 {
                panic!("Unexpected allocator state!");
            }

            let mut pred = 1;
            let mut succ = 2;

            while succ < self.inner.blocks.len() {
                let gap = self.inner.blocks[succ]
                    .virt
                    .wrapping_sub(self.inner.blocks[pred].virt);
                if gap >= size && !self.inner.blocks[pred].mapped {
                    break;
                }
                pred = succ;
                succ += 1;
            }

            if succ < self.inner.blocks.len() {
                let start = self.inner.blocks[pred].virt;
                self.inner.map_locked(start, size);
                Some(start)
            } else {
                None // AS is full
            }
        }
    }

    /// Marks the given region as allocated.
    pub fn allocate_fixed(&mut self, virt: VaType, size: VaType) {
        let _lock = self.block_mutex.lock().unwrap();
        self.inner.map_locked(virt, size);
    }

    /// Frees an AS region so it can be used again.
    pub fn free(&mut self, virt: VaType, size: VaType) {
        let _lock = self.block_mutex.lock().unwrap();
        self.inner.unmap_locked(virt, size);
    }

    pub fn get_va_start(&self) -> VaType {
        self.virt_start
    }

    pub fn get_va_limit(&self) -> VaType {
        self.va_limit
    }
}

/// Concrete `FlatAllocator<u32, 0, 32>` equivalent.
pub type FlatAllocator = FlatAllocatorBool<u32>;
/// `FlatAllocator<DAddr, 0, 34>` equivalent for Maxwell device memory.
pub type FlatAllocator64 = FlatAllocatorBool<u64>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flat_allocator_basic() {
        let mut alloc = FlatAllocator::new(0x1000, 0x100000);
        let addr = alloc.allocate(0x1000);
        assert!(addr.is_some());
        assert_eq!(addr.unwrap(), 0x1000);
    }

    #[test]
    fn test_flat_allocator_multiple() {
        let mut alloc = FlatAllocator::new(0x1000, 0x100000);
        let a1 = alloc.allocate(0x1000).unwrap();
        let a2 = alloc.allocate(0x1000).unwrap();
        assert_eq!(a1, 0x1000);
        assert_eq!(a2, 0x2000);
    }

    #[test]
    fn test_flat_allocator_free_and_realloc() {
        let mut alloc = FlatAllocator::new(0x1000, 0x100000);
        let a1 = alloc.allocate(0x1000).unwrap();
        alloc.free(a1, 0x1000);
        // After freeing, next linear alloc continues from where it left off
        let a2 = alloc.allocate(0x1000).unwrap();
        assert_eq!(a2, 0x2000);
    }

    #[test]
    fn test_flat_allocator_fixed_mapping_uses_upstream_linear_selection() {
        let mut alloc = FlatAllocator64::new(0x1000, (1u64 << 34) - 1);
        alloc.allocate_fixed(0x2000, 0x1000);

        // This is the literal result of Eden's linear selection condition:
        // a request straddling a fixed block selects that block's start.
        assert_eq!(alloc.allocate(0x2000), Some(0x2000));
    }

    #[test]
    fn test_flat_allocator64_can_allocate_above_32bit() {
        let mut alloc = FlatAllocator64::new(0x1_0000_0000, (1u64 << 34) - 1);
        let a1 = alloc.allocate(0x1000).unwrap();
        let a2 = alloc.allocate(0x1000).unwrap();
        assert_eq!(a1, 0x1_0000_0000);
        assert_eq!(a2, 0x1_0000_1000);
    }

    #[test]
    fn test_flat_allocator64_free_keeps_linear_pass() {
        let mut alloc = FlatAllocator64::new(0x1000, (1u64 << 34) - 1);
        let a1 = alloc.allocate(0x1000).unwrap();
        alloc.free(a1, 0x1000);
        let a2 = alloc.allocate(0x1000).unwrap();
        assert_eq!(a2, 0x2000);
    }
}
