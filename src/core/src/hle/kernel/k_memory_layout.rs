//! Port of zuyu/src/core/hle/kernel/k_memory_layout.h and k_memory_layout.cpp
//! Status: Ported (structures and constants)
//! Derniere synchro: 2026-03-11

use super::k_memory_block::PAGE_SIZE;
use super::k_memory_region::{DerivedRegionExtents, KMemoryRegion, KMemoryRegionTree};
use super::k_memory_region_type::*;

// Upstream defines this KMemoryRegionTree method in k_memory_layout.cpp.
impl KMemoryRegionTree {
    pub fn get_random_aligned_region(&self, size: usize, alignment: usize, type_id: u32) -> u64 {
        let extents = self.get_derived_region_extents_raw(type_id);
        assert_eq!(extents.get_address() % alignment as u64, 0);
        let first_index = extents.get_address() / alignment as u64;
        let last_index = extents.get_last_address() / alignment as u64;
        loop {
            let candidate = super::board::k_system_control::generate_random_range(
                first_index,
                last_index,
            ) * alignment as u64;
            let end = candidate.wrapping_add(size as u64);
            if candidate >= end {
                continue;
            }
            let last = end - 1;
            if last > extents.get_last_address() {
                continue;
            }
            let region = self.find(candidate).expect("candidate must belong to the region tree");
            if last <= region.get_last_address() && region.get_type() == type_id {
                return candidate;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Constants from k_memory_layout.h
// ---------------------------------------------------------------------------

pub const L1_BLOCK_SIZE: usize = 1 << 30; // 1 GiB
pub const L2_BLOCK_SIZE: usize = 2 << 20; // 2 MiB

pub const fn get_maximum_overhead_size(size: usize) -> usize {
    (common::alignment::divide_up(size as u64, L1_BLOCK_SIZE as u64)
        + common::alignment::divide_up(size as u64, L2_BLOCK_SIZE as u64)) as usize
        * PAGE_SIZE
}

pub const MAIN_MEMORY_SIZE: usize = 4 * (1 << 30); // 4 GiB
// The exposed layouts reach 12 GiB. Eden's older 8 GiB bound undersizes
// GetMaximumOverheadSize for the two larger layouts.
pub const MAIN_MEMORY_SIZE_MAX: usize = 12 * (1 << 30);

pub const RESERVED_EARLY_DRAM_SIZE: usize = 384 * 1024;
pub const DRAM_PHYSICAL_ADDRESS: usize = 0x80000000;

pub const KERNEL_ASLR_ALIGNMENT: usize = 2 * (1 << 20); // 2 MiB
pub const KERNEL_VIRTUAL_ADDRESS_SPACE_WIDTH: usize = 1 << 39;
pub const KERNEL_PHYSICAL_ADDRESS_SPACE_WIDTH: usize = 1 << 48;

pub const KERNEL_VIRTUAL_ADDRESS_SPACE_BASE: usize =
    0usize.wrapping_sub(KERNEL_VIRTUAL_ADDRESS_SPACE_WIDTH);
pub const KERNEL_VIRTUAL_ADDRESS_SPACE_END: usize = KERNEL_VIRTUAL_ADDRESS_SPACE_BASE
    + (KERNEL_VIRTUAL_ADDRESS_SPACE_WIDTH - KERNEL_ASLR_ALIGNMENT);
pub const KERNEL_VIRTUAL_ADDRESS_SPACE_LAST: usize = KERNEL_VIRTUAL_ADDRESS_SPACE_END - 1;
pub const KERNEL_VIRTUAL_ADDRESS_SPACE_SIZE: usize =
    KERNEL_VIRTUAL_ADDRESS_SPACE_END - KERNEL_VIRTUAL_ADDRESS_SPACE_BASE;
pub const KERNEL_VIRTUAL_ADDRESS_CODE_BASE: usize = KERNEL_VIRTUAL_ADDRESS_SPACE_BASE;
pub const KERNEL_VIRTUAL_ADDRESS_CODE_SIZE: usize = 392 * 1024;
pub const KERNEL_VIRTUAL_ADDRESS_CODE_END: usize =
    KERNEL_VIRTUAL_ADDRESS_CODE_BASE + KERNEL_VIRTUAL_ADDRESS_CODE_SIZE;

pub const KERNEL_PHYSICAL_ADDRESS_SPACE_BASE: usize = 0;
pub const KERNEL_PHYSICAL_ADDRESS_SPACE_END: usize =
    KERNEL_PHYSICAL_ADDRESS_SPACE_BASE + KERNEL_PHYSICAL_ADDRESS_SPACE_WIDTH;
pub const KERNEL_PHYSICAL_ADDRESS_SPACE_LAST: usize = KERNEL_PHYSICAL_ADDRESS_SPACE_END - 1;
pub const KERNEL_PHYSICAL_ADDRESS_SPACE_SIZE: usize =
    KERNEL_PHYSICAL_ADDRESS_SPACE_END - KERNEL_PHYSICAL_ADDRESS_SPACE_BASE;
pub const KERNEL_PHYSICAL_ADDRESS_CODE_BASE: usize =
    DRAM_PHYSICAL_ADDRESS + RESERVED_EARLY_DRAM_SIZE;

pub const KERNEL_PAGE_TABLE_HEAP_SIZE: usize = get_maximum_overhead_size(MAIN_MEMORY_SIZE_MAX);
pub const KERNEL_INITIAL_PAGE_HEAP_SIZE: usize = 128 * 1024;

pub const KERNEL_SLAB_HEAP_DATA_SIZE: usize = 5 * (1 << 20);
pub const KERNEL_SLAB_HEAP_GAPS_SIZE_MAX: usize = 2 * (1 << 20) - 64 * 1024;
pub const KERNEL_SLAB_HEAP_SIZE: usize =
    KERNEL_SLAB_HEAP_DATA_SIZE + KERNEL_SLAB_HEAP_GAPS_SIZE_MAX;

pub const KERNEL_PAGE_BUFFER_HEAP_SIZE: usize = 0x3E0000;
pub const KERNEL_SLAB_HEAP_ADDITIONAL_SIZE: usize = 0x148000;
pub const KERNEL_PAGE_BUFFER_ADDITIONAL_SIZE: usize = 0x33C000;

pub const KERNEL_RESOURCE_SIZE: usize = KERNEL_PAGE_TABLE_HEAP_SIZE
    + KERNEL_INITIAL_PAGE_HEAP_SIZE
    + KERNEL_SLAB_HEAP_SIZE
    + KERNEL_PAGE_BUFFER_HEAP_SIZE;

pub fn is_kernel_address(address: usize) -> bool {
    KERNEL_VIRTUAL_ADDRESS_SPACE_BASE <= address && address < KERNEL_VIRTUAL_ADDRESS_SPACE_END
}

// ---------------------------------------------------------------------------
// KMemoryLayout
// ---------------------------------------------------------------------------

/// Port of Kernel::KMemoryLayout.
pub struct KMemoryLayout {
    m_linear_phys_to_virt_diff: u64,
    m_linear_virt_to_phys_diff: u64,
    m_virtual_tree: KMemoryRegionTree,
    m_physical_tree: KMemoryRegionTree,
    m_virtual_linear_tree: KMemoryRegionTree,
    m_physical_linear_tree: KMemoryRegionTree,
}

impl KMemoryLayout {
    pub fn new() -> Self {
        Self {
            m_linear_phys_to_virt_diff: 0,
            m_linear_virt_to_phys_diff: 0,
            m_virtual_tree: KMemoryRegionTree::new(),
            m_physical_tree: KMemoryRegionTree::new(),
            m_virtual_linear_tree: KMemoryRegionTree::new(),
            m_physical_linear_tree: KMemoryRegionTree::new(),
        }
    }

    pub fn get_virtual_memory_region_tree(&self) -> &KMemoryRegionTree {
        &self.m_virtual_tree
    }
    pub fn get_virtual_memory_region_tree_mut(&mut self) -> &mut KMemoryRegionTree {
        &mut self.m_virtual_tree
    }
    pub fn get_physical_memory_region_tree(&self) -> &KMemoryRegionTree {
        &self.m_physical_tree
    }
    pub fn get_physical_memory_region_tree_mut(&mut self) -> &mut KMemoryRegionTree {
        &mut self.m_physical_tree
    }
    pub fn get_virtual_linear_memory_region_tree(&self) -> &KMemoryRegionTree {
        &self.m_virtual_linear_tree
    }
    pub fn get_virtual_linear_memory_region_tree_mut(&mut self) -> &mut KMemoryRegionTree {
        &mut self.m_virtual_linear_tree
    }
    pub fn get_physical_linear_memory_region_tree(&self) -> &KMemoryRegionTree {
        &self.m_physical_linear_tree
    }
    pub fn get_physical_linear_memory_region_tree_mut(&mut self) -> &mut KMemoryRegionTree {
        &mut self.m_physical_linear_tree
    }

    pub fn get_linear_virtual_address(&self, address: u64) -> u64 {
        address.wrapping_add(self.m_linear_phys_to_virt_diff)
    }

    pub fn get_linear_physical_address(&self, address: u64) -> u64 {
        address.wrapping_add(self.m_linear_virt_to_phys_diff)
    }

    pub fn find_virtual(&self, address: u64) -> Option<&KMemoryRegion> {
        self.m_virtual_tree.find(address)
    }

    pub fn find_physical(&self, address: u64) -> Option<&KMemoryRegion> {
        self.m_physical_tree.find(address)
    }

    pub fn find_virtual_linear(&self, address: u64) -> Option<&KMemoryRegion> {
        self.m_virtual_linear_tree.find(address)
    }

    pub fn find_physical_linear(&self, address: u64) -> Option<&KMemoryRegion> {
        self.m_physical_linear_tree.find(address)
    }

    pub fn initialize_linear_memory_region_trees(
        &mut self,
        aligned_linear_phys_start: u64,
        linear_virtual_start: u64,
    ) {
        self.m_linear_phys_to_virt_diff =
            linear_virtual_start.wrapping_sub(aligned_linear_phys_start);
        self.m_linear_virt_to_phys_diff =
            aligned_linear_phys_start.wrapping_sub(linear_virtual_start);

        // Copy linear-mapped physical regions.
        let phys_regions: Vec<KMemoryRegion> = self
            .m_physical_tree
            .iter()
            .filter(|r| r.has_type_attribute(K_MEMORY_REGION_ATTR_LINEAR_MAPPED))
            .cloned()
            .collect();
        for region in phys_regions {
            self.m_physical_linear_tree.insert_directly(
                region.get_address(),
                region.get_last_address(),
                region.get_attributes(),
                region.get_type(),
            );
        }

        // Copy DRAM-derived virtual regions.
        let virt_regions: Vec<KMemoryRegion> = self
            .m_virtual_tree
            .iter()
            .filter(|r| r.is_derived_from(K_MEMORY_REGION_TYPE_DRAM))
            .cloned()
            .collect();
        for region in virt_regions {
            self.m_virtual_linear_tree.insert_directly(
                region.get_address(),
                region.get_last_address(),
                region.get_attributes(),
                region.get_type(),
            );
        }
    }

    /// Port of KMemoryLayout::GetResourceRegionSizeForInit.
    pub fn get_resource_region_size_for_init(use_extra_resource: bool) -> usize {
        KERNEL_RESOURCE_SIZE
            + super::board::k_system_control::SECURE_APPLET_MEMORY_SIZE
            + if use_extra_resource {
                KERNEL_SLAB_HEAP_ADDITIONAL_SIZE + KERNEL_PAGE_BUFFER_ADDITIONAL_SIZE
            } else {
                0
            }
    }

    /// Get the physical extents of main memory (DRAM).
    /// Port of upstream `KMemoryLayout::GetMainMemoryPhysicalExtents()`.
    pub fn get_main_memory_physical_extents(&self) -> DerivedRegionExtents {
        self.m_physical_tree
            .get_derived_region_extents(K_MEMORY_REGION_TYPE_DRAM)
    }

    pub fn get_total_and_kernel_memory_sizes(&self) -> (usize, usize) {
        let mut total_size: usize = 0;
        let mut kernel_size: usize = 0;
        for region in self.m_physical_tree.iter() {
            if region.is_derived_from(K_MEMORY_REGION_TYPE_DRAM) {
                total_size += region.get_size();
                if !region.is_derived_from(K_MEMORY_REGION_TYPE_DRAM_USER_POOL) {
                    kernel_size += region.get_size();
                }
            }
        }
        (total_size, kernel_size)
    }

}

impl Default for KMemoryLayout {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_table_reservation_covers_every_supported_memory_layout() {
        for gib in [4usize, 6, 8, 10, 12] {
            let memory_size = gib << 30;
            assert!(memory_size <= MAIN_MEMORY_SIZE_MAX);
            assert!(get_maximum_overhead_size(memory_size) <= KERNEL_PAGE_TABLE_HEAP_SIZE);
        }
        assert_eq!(KERNEL_PAGE_TABLE_HEAP_SIZE, (12 + 6144) * PAGE_SIZE);
    }

    #[test]
    fn aligned_region_requires_exact_type_and_single_region_fit() {
        let mut tree = KMemoryRegionTree::new();
        let parent = K_MEMORY_REGION_TYPE_KERNEL.get_value();
        let child = K_MEMORY_REGION_TYPE_KERNEL_CODE.get_value();
        tree.insert_directly(0x1000, 0x2fff, 0, child);
        tree.insert_directly(0x3000, 0x3fff, 0, parent);
        tree.insert_directly(0x4000, 0x5fff, 0, parent);
        for _ in 0..32 {
            assert_eq!(tree.get_random_aligned_region(0x2000, 0x1000, parent), 0x4000);
        }
    }

    #[test]
    fn aligned_region_with_guard_reserves_both_margins() {
        let mut tree = KMemoryRegionTree::new();
        let kind = K_MEMORY_REGION_TYPE_KERNEL.get_value();
        tree.insert_directly(0x8000, 0xbfff, 0, kind);
        assert_eq!(
            tree.get_random_aligned_region_with_guard(0x2000, 0x1000, kind, 0x1000),
            0x9000
        );
    }

    #[test]
    fn aligned_region_rejects_wrapping_end_addresses() {
        let mut tree = KMemoryRegionTree::new();
        let kind = K_MEMORY_REGION_TYPE_KERNEL.get_value();
        // Keep the tree's nonzero-end invariant; the last aligned candidate
        // still wraps when the requested page size is added.
        tree.insert_directly(u64::MAX - 0x1fff, u64::MAX - 1, 0, kind);
        assert_eq!(tree.get_random_aligned_region(0x1000, 0x1000, kind), u64::MAX - 0x1fff);
    }

    #[test]
    fn resource_region_includes_secure_applet_memory_in_both_modes() {
        // KMemoryLayout::GetResourceRegionSizeForInit adds the NX board's
        // SecureAppletMemorySize independently of the extra-resource flag.
        let base = KERNEL_RESOURCE_SIZE + 4 * 1024 * 1024;
        assert_eq!(KMemoryLayout::get_resource_region_size_for_init(false), base);
        assert_eq!(
            KMemoryLayout::get_resource_region_size_for_init(true),
            base + KERNEL_SLAB_HEAP_ADDITIONAL_SIZE + KERNEL_PAGE_BUFFER_ADDITIONAL_SIZE
        );
    }
}
