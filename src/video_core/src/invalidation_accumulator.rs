// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/invalidation_accumulator.h
//!
//! Accumulates cache invalidation ranges, merging adjacent regions.

/// GPU virtual address type.
pub type GPUVAddr = u64;

/// Virtual address type.
pub type VAddr = u64;

const ATOMICITY_BITS: usize = 5;
const ATOMICITY_SIZE: usize = 1 << ATOMICITY_BITS;
const ATOMICITY_SIZE_MASK: usize = ATOMICITY_SIZE - 1;
const ATOMICITY_MASK: u64 = !(ATOMICITY_SIZE_MASK as u64);

/// Accumulates invalidation ranges, merging adjacent entries aligned to 32-byte boundaries.
pub struct InvalidationAccumulator {
    start_address: GPUVAddr,
    accumulated_size: usize,
    buffer: Vec<(VAddr, usize)>,
}

impl InvalidationAccumulator {
    pub fn new() -> Self {
        Self {
            start_address: 0,
            accumulated_size: 0,
            buffer: Vec::new(),
        }
    }

    /// Add an invalidation range. Merges with the current range if adjacent.
    pub fn add(&mut self, mut address: GPUVAddr, mut size: usize) {
        let end_address = self
            .start_address
            .wrapping_add(self.accumulated_size as u64);
        if address >= self.start_address && address.wrapping_add(size as u64) <= end_address {
            return;
        }

        size = (address
            .wrapping_add(size as u64)
            .wrapping_add(ATOMICITY_SIZE_MASK as u64)
            & ATOMICITY_MASK)
            .wrapping_sub(address) as usize;
        address &= ATOMICITY_MASK;

        if self.start_address == 0 {
            self.start_address = address;
            self.accumulated_size = size;
        } else if address != end_address {
            self.buffer
                .push((self.start_address, self.accumulated_size));
            self.start_address = address;
            self.accumulated_size = size;
        } else {
            self.accumulated_size = self.accumulated_size.wrapping_add(size);
        }
    }

    /// Invoke a callback for every accumulated range, then reset the accumulator.
    /// Returns whether any range was invalidated.
    pub fn invalidate_all<F: FnMut(VAddr, usize)>(&mut self, mut func: F) -> bool {
        if self.start_address > 0 {
            for &(address, size) in &self.buffer {
                func(address, size);
            }
            func(self.start_address, self.accumulated_size);
            self.buffer.clear();
            self.start_address = 0;
            self.accumulated_size = 0;
            return true;
        }
        false
    }
}

impl Default for InvalidationAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::InvalidationAccumulator;

    #[test]
    fn adjacent_ranges_merge_and_invalidate_all_resets() {
        let mut accumulator = InvalidationAccumulator::new();
        accumulator.add(0x1000, 0x20);
        accumulator.add(0x1020, 0x20);

        let mut ranges = Vec::new();
        assert!(accumulator.invalidate_all(|address, size| ranges.push((address, size))));
        assert_eq!(ranges, vec![(0x1000, 0x40)]);
        assert!(!accumulator.invalidate_all(|_, _| unreachable!()));
    }

    #[test]
    fn disjoint_ranges_preserve_insertion_order() {
        let mut accumulator = InvalidationAccumulator::new();
        accumulator.add(0x1010, 1);
        accumulator.add(0x2010, 1);

        let mut ranges = Vec::new();
        assert!(accumulator.invalidate_all(|address, size| ranges.push((address, size))));
        assert_eq!(ranges, vec![(0x1000, 0x10), (0x2000, 0x10)]);
    }

    #[test]
    fn zero_start_address_matches_upstream_empty_sentinel() {
        let mut accumulator = InvalidationAccumulator::new();
        accumulator.add(0, 1);

        assert!(!accumulator.invalidate_all(|_, _| unreachable!()));
    }

    #[test]
    fn range_arithmetic_wraps_like_unsigned_cpp() {
        let mut accumulator = InvalidationAccumulator::new();
        accumulator.add(u64::MAX - 15, 32);

        let mut ranges = Vec::new();
        assert!(accumulator.invalidate_all(|address, size| ranges.push((address, size))));
        assert_eq!(ranges, vec![(u64::MAX - 31, 48)]);
    }
}
