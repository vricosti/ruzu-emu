// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Lichii's `video_core/buffer_cache/read_trap_tracker.h`.

use common::range_sets::RangeSet;

#[derive(Default)]
pub struct ReadTrapTracker {
    ranges: RangeSet,
}

impl ReadTrapTracker {
    /// Arms a page-aligned range, notifying the core only about newly armed parts.
    /// Rust separates the two upstream Arm overloads by name.
    pub fn arm_with(&mut self, address: u64, size: u64, mut arm: impl FnMut(u64, u64)) -> bool {
        let end = address.wrapping_add(size);
        let mut next = address;
        let mut any = false;
        self.ranges
            .for_each_in_range(address, size as usize, |lower, upper| {
                if lower > next {
                    arm(next, lower - next);
                    any = true;
                }
                next = next.max(upper);
            });
        if next < end {
            arm(next, end - next);
            any = true;
        }
        if any {
            self.ranges.add(address, size as usize);
        }
        any
    }

    pub fn arm(&mut self, address: u64, size: u64) -> bool {
        self.arm_with(address, size, |_, _| {})
    }

    /// A CPU read consumes the core's trap; unmapping also forgets the range.
    pub fn subtract(&mut self, address: u64, size: u64) {
        self.ranges.subtract(address, size as usize);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arms_only_gaps_in_address_order() {
        let mut tracker = ReadTrapTracker::default();
        assert!(tracker.arm(0x2000, 0x1000));
        assert!(tracker.arm(0x4000, 0x2000));
        let mut armed = Vec::new();
        assert!(tracker.arm_with(0x1000, 0x6000, |addr, size| armed.push((addr, size))));
        assert_eq!(
            armed,
            [(0x1000, 0x1000), (0x3000, 0x1000), (0x6000, 0x1000)]
        );
        assert!(!tracker.arm_with(0x1000, 0x6000, |_, _| panic!("already armed")));
        assert!(!tracker.arm(0x2000, 0x1000));
    }

    #[test]
    fn consumed_and_unmapped_pages_can_be_rearmed() {
        let mut tracker = ReadTrapTracker::default();
        assert!(tracker.arm(0x1000, 0x5000));
        tracker.subtract(0x2000, 0x2000);
        let mut armed = Vec::new();
        assert!(tracker.arm_with(0x1000, 0x5000, |addr, size| armed.push((addr, size))));
        assert_eq!(armed, [(0x2000, 0x2000)]);
        tracker.subtract(0, 0x10000);
        assert!(tracker.arm(0x1000, 0x5000));
    }

    #[test]
    fn empty_and_touching_ranges() {
        let mut tracker = ReadTrapTracker::default();
        assert!(!tracker.arm(0x1000, 0));
        assert!(tracker.arm(0x1000, 0x1000));
        assert!(!tracker.arm_with(0x1800, 0, |_, _| panic!("empty range")));
        assert!(tracker.arm(0x2000, 0x1000));
        assert!(!tracker.arm(0x1000, 0x2000));
        tracker.subtract(0x2000, 0);
        assert!(!tracker.arm(0x1000, 0x2000));
    }
}
