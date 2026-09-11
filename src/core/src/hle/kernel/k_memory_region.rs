//! Port of zuyu/src/core/hle/kernel/k_memory_region.h
//! Status: Ported
//! Derniere synchro: 2026-03-11
//!
//! KMemoryRegion, KMemoryRegionTree, and KMemoryRegionAllocator.

use super::k_memory_region_type::KMemoryRegionTypeValue;
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// KMemoryRegion
// ---------------------------------------------------------------------------

/// Port of Kernel::KMemoryRegion.
#[derive(Debug, Clone)]
pub struct KMemoryRegion {
    m_address: u64,
    m_last_address: u64,
    m_pair_address: u64,
    m_attributes: u32,
    m_type_id: u32,
}

impl KMemoryRegion {
    pub fn new() -> Self {
        Self {
            m_address: 0,
            m_last_address: 0,
            m_pair_address: 0,
            m_attributes: 0,
            m_type_id: 0,
        }
    }

    pub fn new_range(address: u64, last_address: u64) -> Self {
        Self {
            m_address: address,
            m_last_address: last_address,
            m_pair_address: u64::MAX,
            m_attributes: 0,
            m_type_id: 0,
        }
    }

    pub fn new_with_attr(address: u64, last_address: u64, attributes: u32, type_id: u32) -> Self {
        Self {
            m_address: address,
            m_last_address: last_address,
            m_pair_address: u64::MAX,
            m_attributes: attributes,
            m_type_id: type_id,
        }
    }

    pub fn new_full(
        address: u64,
        last_address: u64,
        pair_address: u64,
        attributes: u32,
        type_id: u32,
    ) -> Self {
        Self {
            m_address: address,
            m_last_address: last_address,
            m_pair_address: pair_address,
            m_attributes: attributes,
            m_type_id: type_id,
        }
    }

    pub fn get_address(&self) -> u64 {
        self.m_address
    }
    pub fn get_pair_address(&self) -> u64 {
        self.m_pair_address
    }
    pub fn get_last_address(&self) -> u64 {
        self.m_last_address
    }
    pub fn get_end_address(&self) -> u64 {
        self.m_last_address + 1
    }
    pub fn get_size(&self) -> usize {
        (self.get_end_address() - self.m_address) as usize
    }
    pub fn get_attributes(&self) -> u32 {
        self.m_attributes
    }
    pub fn get_type(&self) -> u32 {
        self.m_type_id
    }

    pub fn set_type(&mut self, type_id: u32) {
        debug_assert!(self.can_derive(type_id));
        self.m_type_id = type_id;
    }

    pub fn contains(&self, addr: u64) -> bool {
        debug_assert!(self.get_end_address() != 0);
        self.m_address <= addr && addr <= self.m_last_address
    }

    pub fn is_derived_from(&self, type_id: KMemoryRegionTypeValue) -> bool {
        (self.m_type_id | type_id.get_value()) == self.m_type_id
    }

    pub fn is_derived_from_raw(&self, type_id: u32) -> bool {
        (self.m_type_id | type_id) == self.m_type_id
    }

    pub fn has_type_attribute(&self, attr: u32) -> bool {
        (self.m_type_id | attr) == self.m_type_id
    }

    pub fn can_derive(&self, type_id: u32) -> bool {
        (self.m_type_id | type_id) == type_id
    }

    pub fn set_pair_address(&mut self, a: u64) {
        self.m_pair_address = a;
    }

    pub fn set_type_attribute(&mut self, attr: u32) {
        self.m_type_id |= attr;
    }

    pub fn reset(&mut self, a: u64, la: u64, p: u64, r: u32, t: u32) {
        self.m_address = a;
        self.m_last_address = la;
        self.m_pair_address = p;
        self.m_attributes = r;
        self.m_type_id = t;
    }
}

impl Default for KMemoryRegion {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// DerivedRegionExtents
// ---------------------------------------------------------------------------

/// Stores first and last region pointers for derived region extent queries.
#[derive(Debug, Clone)]
pub struct DerivedRegionExtents {
    pub first_region: Option<KMemoryRegion>,
    pub last_region: Option<KMemoryRegion>,
}

impl DerivedRegionExtents {
    pub fn new() -> Self {
        Self {
            first_region: None,
            last_region: None,
        }
    }

    pub fn get_address(&self) -> u64 {
        self.first_region.as_ref().unwrap().get_address()
    }

    pub fn get_last_address(&self) -> u64 {
        self.last_region.as_ref().unwrap().get_last_address()
    }

    pub fn get_end_address(&self) -> u64 {
        self.get_last_address() + 1
    }

    pub fn get_size(&self) -> usize {
        (self.get_end_address() - self.get_address()) as usize
    }
}

impl Default for DerivedRegionExtents {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// KMemoryRegionTree
// ---------------------------------------------------------------------------

/// Port of Kernel::KMemoryRegionTree.
///
/// Uses a BTreeMap keyed by address for ordering (matching the intrusive RB-tree upstream).
pub struct KMemoryRegionTree {
    regions: BTreeMap<u64, KMemoryRegion>,
}

impl KMemoryRegionTree {
    pub fn new() -> Self {
        Self {
            regions: BTreeMap::new(),
        }
    }

    pub fn find(&self, address: u64) -> Option<&KMemoryRegion> {
        // Find the region whose range contains address.
        // Walk backward from the first key > address.
        for (_, region) in self.regions.range(..=address).rev() {
            if region.contains(address) {
                return Some(region);
            }
            break;
        }
        None
    }

    pub fn find_modifiable(&mut self, address: u64) -> Option<&mut KMemoryRegion> {
        // We need to find the key first, then get mutable ref.
        let key = {
            let mut found_key = None;
            for (&k, region) in self.regions.range(..=address).rev() {
                if region.contains(address) {
                    found_key = Some(k);
                }
                break;
            }
            found_key
        };
        key.and_then(move |k| self.regions.get_mut(&k))
    }

    pub fn find_by_type(&self, type_id: KMemoryRegionTypeValue) -> Option<&KMemoryRegion> {
        for region in self.regions.values() {
            if region.get_type() == type_id.get_value() {
                return Some(region);
            }
        }
        None
    }

    pub fn find_by_type_and_attribute(&self, type_id: u32, attr: u32) -> Option<&KMemoryRegion> {
        for region in self.regions.values() {
            if region.get_type() == type_id && region.get_attributes() == attr {
                return Some(region);
            }
        }
        None
    }

    pub fn find_first_derived(&self, type_id: KMemoryRegionTypeValue) -> Option<&KMemoryRegion> {
        for region in self.regions.values() {
            if region.is_derived_from(type_id) {
                return Some(region);
            }
        }
        None
    }

    pub fn find_last_derived(&self, type_id: KMemoryRegionTypeValue) -> Option<&KMemoryRegion> {
        let mut last = None;
        for region in self.regions.values() {
            if region.is_derived_from(type_id) {
                last = Some(region);
            }
        }
        last
    }

    pub fn get_derived_region_extents(
        &self,
        type_id: KMemoryRegionTypeValue,
    ) -> DerivedRegionExtents {
        let mut extents = DerivedRegionExtents::new();
        for region in self.regions.values() {
            if region.is_derived_from(type_id) {
                if extents.first_region.is_none() {
                    extents.first_region = Some(region.clone());
                }
                extents.last_region = Some(region.clone());
            }
        }
        debug_assert!(extents.first_region.is_some());
        debug_assert!(extents.last_region.is_some());
        extents
    }

    pub fn get_derived_region_extents_raw(&self, type_id: u32) -> DerivedRegionExtents {
        let mut extents = DerivedRegionExtents::new();
        for region in self.regions.values() {
            if region.is_derived_from_raw(type_id) {
                if extents.first_region.is_none() {
                    extents.first_region = Some(region.clone());
                }
                extents.last_region = Some(region.clone());
            }
        }
        debug_assert!(extents.first_region.is_some());
        debug_assert!(extents.last_region.is_some());
        extents
    }

    pub fn insert_directly(&mut self, address: u64, last_address: u64, attr: u32, type_id: u32) {
        let region = KMemoryRegion::new_with_attr(address, last_address, attr, type_id);
        self.regions.insert(address, region);
    }

    pub fn insert(
        &mut self,
        address: u64,
        size: usize,
        type_id: u32,
        new_attr: u32,
        old_attr: u32,
    ) -> bool {
        // Find region containing address.
        let found_key = {
            let mut key = None;
            for (&k, region) in self.regions.range(..=address).rev() {
                if region.contains(address) {
                    key = Some(k);
                }
                break;
            }
            key
        };

        let found_key = match found_key {
            Some(k) => k,
            None => return false,
        };

        let found = self.regions.get(&found_key).unwrap().clone();
        if found.get_attributes() != old_attr {
            return false;
        }

        let inserted_end = address + size as u64;
        let inserted_last = inserted_end - 1;
        if found.get_last_address() < inserted_last {
            return false;
        }
        if !found.can_derive(type_id) {
            return false;
        }

        let old_address = found.get_address();
        let old_last = found.get_last_address();
        let old_pair = found.get_pair_address();
        let old_type = found.get_type();

        // Remove old region.
        self.regions.remove(&found_key);

        // Insert new/adjusted regions.
        if old_address == address {
            let region =
                KMemoryRegion::new_full(address, inserted_last, old_pair, new_attr, type_id);
            self.regions.insert(address, region);
        } else {
            // Keep the old region adjusted.
            let before =
                KMemoryRegion::new_full(old_address, address - 1, old_pair, old_attr, old_type);
            self.regions.insert(old_address, before);

            let new_pair = if old_pair != u64::MAX {
                old_pair + (address - old_address)
            } else {
                old_pair
            };
            let new_region =
                KMemoryRegion::new_full(address, inserted_last, new_pair, new_attr, type_id);
            self.regions.insert(address, new_region);
        }

        if old_last != inserted_last {
            let after_pair = if old_pair != u64::MAX {
                old_pair + (inserted_end - old_address)
            } else {
                old_pair
            };
            let after =
                KMemoryRegion::new_full(inserted_end, old_last, after_pair, old_attr, old_type);
            self.regions.insert(inserted_end, after);
        }

        true
    }

    /// Inline wrapper from upstream k_memory_region.h.
    pub fn get_random_aligned_region_with_guard(
        &self,
        size: usize,
        alignment: usize,
        type_id: u32,
        guard_size: usize,
    ) -> u64 {
        self.get_random_aligned_region(
            size.wrapping_add(guard_size.wrapping_mul(2)),
            alignment,
            type_id,
        )
        .wrapping_add(guard_size as u64)
    }

    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &KMemoryRegion> {
        self.regions.values()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut KMemoryRegion> {
        self.regions.values_mut()
    }
}

impl Default for KMemoryRegionTree {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// KMemoryRegionAllocator
// ---------------------------------------------------------------------------

/// Port of Kernel::KMemoryRegionAllocator.
/// Upstream uses a fixed-size array of 200 regions. We use a Vec for simplicity.
pub struct KMemoryRegionAllocator {
    regions: Vec<KMemoryRegion>,
}

impl KMemoryRegionAllocator {
    pub const MAX_MEMORY_REGIONS: usize = 200;

    pub fn new() -> Self {
        Self {
            regions: Vec::with_capacity(Self::MAX_MEMORY_REGIONS),
        }
    }

    pub fn allocate(&mut self, region: KMemoryRegion) -> usize {
        debug_assert!(self.regions.len() < Self::MAX_MEMORY_REGIONS);
        let index = self.regions.len();
        self.regions.push(region);
        index
    }

    pub fn get(&self, index: usize) -> &KMemoryRegion {
        &self.regions[index]
    }

    pub fn get_mut(&mut self, index: usize) -> &mut KMemoryRegion {
        &mut self.regions[index]
    }
}

impl Default for KMemoryRegionAllocator {
    fn default() -> Self {
        Self::new()
    }
}
