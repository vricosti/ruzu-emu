use std::collections::HashSet;
use std::ops::RangeInclusive;

use crate::ir::location::LocationDescriptor;

/// Maps emitted guest-address ranges back to their location descriptors.
///
/// Upstream owner: `backend/block_range_information.{h,cpp}`.
pub struct BlockRangeInformation<P> {
    block_ranges: Vec<(RangeInclusive<P>, LocationDescriptor)>,
}

impl<P> Default for BlockRangeInformation<P> {
    fn default() -> Self {
        Self {
            block_ranges: Vec::new(),
        }
    }
}

impl<P> BlockRangeInformation<P>
where
    P: Copy + Ord,
{
    pub fn add_range(&mut self, range: RangeInclusive<P>, location: LocationDescriptor) {
        self.block_ranges.push((range, location));
    }

    pub fn clear_cache(&mut self) {
        self.block_ranges.clear();
    }

    pub fn invalidate_ranges(&self, ranges: &[RangeInclusive<P>]) -> HashSet<LocationDescriptor> {
        let mut erase_locations = HashSet::new();
        for invalidate_interval in ranges {
            for (block_interval, descriptor) in &self.block_ranges {
                if ranges_overlap(block_interval, invalidate_interval) {
                    erase_locations.insert(*descriptor);
                }
            }
        }
        // Upstream intentionally leaves stale ranges in the map. Its source
        // carries the same efficiency TODO rather than erasing them here.
        erase_locations
    }

    #[cfg(test)]
    pub fn ranges(&self) -> &[(RangeInclusive<P>, LocationDescriptor)] {
        &self.block_ranges
    }
}

fn ranges_overlap<P: Ord>(lhs: &RangeInclusive<P>, rhs: &RangeInclusive<P>) -> bool {
    lhs.start() <= rhs.end() && rhs.start() <= lhs.end()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalidation_uses_the_complete_block_interval() {
        let mut information = BlockRangeInformation::<u32>::default();
        let descriptor = LocationDescriptor::new(0x1000);
        information.add_range(0x1000..=0x100f, descriptor);

        assert_eq!(
            information.invalidate_ranges(&[0x1008..=0x1008]),
            HashSet::from([descriptor])
        );
    }

    #[test]
    fn invalidation_collects_each_overlapping_descriptor_once() {
        let mut information = BlockRangeInformation::<u64>::default();
        let first = LocationDescriptor::new(0x1000);
        let second = LocationDescriptor::new(0x2000);
        information.add_range(0x1000..=0x101f, first);
        information.add_range(0x2000..=0x201f, second);

        assert_eq!(
            information.invalidate_ranges(&[0x1008..=0x2008, 0x1000..=0x1000]),
            HashSet::from([first, second])
        );
        assert_eq!(information.ranges().len(), 2);
    }

    #[test]
    fn clear_cache_removes_all_registered_ranges() {
        let mut information = BlockRangeInformation::<u32>::default();
        information.add_range(1..=4, LocationDescriptor::new(1));
        information.clear_cache();
        assert!(information.ranges().is_empty());
    }
}
