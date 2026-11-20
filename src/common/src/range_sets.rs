// SPDX-FileCopyrightText: 2024 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/common/range_sets.h and zuyu/src/common/range_sets.inc
//!
//! Provides `RangeSet` and `OverlapRangeSet` for managing sets of address
//! ranges. Eden uses boost::icl interval sets; `RangeSet` follows Lichii's
//! sorted array (Lichii 47a1f19d54), `OverlapRangeSet` stays a sorted vector
//! of split segments.

// ---------------------------------------------------------------------------
// RangeSet
// ---------------------------------------------------------------------------

/// A set of non-overlapping address ranges that automatically merges adjacent
/// or overlapping intervals when ranges are added, and splits/trims when
/// ranges are subtracted.
///
/// Maps to the C++ `RangeSet<AddressType>`. Port of Lichii `RangeSetImpl`
/// (common/range_sets.inc:29-129, Lichii 47a1f19d54 "back RangeSet with a
/// sorted array"): sorted, disjoint, non-touching ranges in one array. The
/// buffer cache looks these up tens of thousands of times a frame and most
/// lookups miss; the sets stay small (a few hundred ranges), so a binary
/// search over contiguous memory and moving a few hundred bytes on an insert
/// cost less than tree nodes. Eden backs it with a boost::icl interval_set.
pub struct RangeSet {
    /// `[lower, upper)` pairs, sorted, disjoint and non-touching.
    ranges: Vec<(u64, u64)>,
}

impl RangeSet {
    pub fn new() -> Self {
        Self { ranges: Vec::new() }
    }

    /// Lichii `RangeSetImpl::FirstEndingAfter`: the first range ending after
    /// `address`, the first that could overlap what starts there.
    #[inline]
    fn first_ending_after(&self, address: u64) -> usize {
        self.ranges.partition_point(|&(_, upper)| upper <= address)
    }

    /// Add the range `[base_address, base_address + size)` to the set,
    /// merging with any overlapping or adjacent existing ranges.
    ///
    /// Port of Lichii `RangeSetImpl::Add` (range_sets.inc:48-73).
    pub fn add(&mut self, base_address: u64, size: usize) {
        let end_address = base_address.wrapping_add(size as u64);
        // Unsigned C++ addition wraps; reversed/empty intervals are empty.
        if end_address <= base_address {
            return;
        }
        // Ranges that touch the new one join it: the first ending at or after its start...
        let first = self.ranges.partition_point(|&(_, upper)| upper < base_address);
        if first == self.ranges.len() {
            self.ranges.push((base_address, end_address));
            return;
        }
        // ...up to the first starting after its end.
        let last = first + self.ranges[first..].partition_point(|&(lower, _)| lower <= end_address);
        if first == last {
            self.ranges.insert(first, (base_address, end_address));
            return;
        }
        let upper = self.ranges[last - 1].1.max(end_address);
        let range = &mut self.ranges[first];
        range.0 = range.0.min(base_address);
        range.1 = upper;
        self.ranges.drain(first + 1..last);
    }

    /// Subtract the range `[base_address, base_address + size)` from the set.
    ///
    /// Port of Lichii `RangeSetImpl::Subtract` (range_sets.inc:75-104).
    pub fn subtract(&mut self, base_address: u64, size: usize) {
        let end_address = base_address.wrapping_add(size as u64);
        if end_address <= base_address {
            return;
        }
        let first = self.first_ending_after(base_address);
        let last = first + self.ranges[first..].partition_point(|&(lower, _)| lower < end_address);
        if first == last {
            return;
        }
        let left_lower = self.ranges[first].0;
        let right_upper = self.ranges[last - 1].1;
        let keep_left = left_lower < base_address;
        let keep_right = end_address < right_upper;
        if keep_left && keep_right && last - first == 1 {
            // Cut out of the middle of one range: it becomes two.
            self.ranges[first].1 = base_address;
            self.ranges.insert(last, (end_address, right_upper));
            return;
        }
        let mut erase_from = first;
        let mut erase_to = last;
        if keep_left {
            self.ranges[first].1 = base_address;
            erase_from += 1;
        }
        if keep_right {
            erase_to -= 1;
            self.ranges[erase_to].0 = end_address;
        }
        self.ranges.drain(erase_from..erase_to);
    }

    /// Remove all ranges.
    pub fn clear(&mut self) {
        self.ranges.clear();
    }

    /// Returns true if the set contains no ranges.
    pub fn empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// Port of Lichii `RangeSet::Intersects` (range_sets.inc:271-279): whether
    /// any range overlaps `[base_address, base_address + size)`.
    pub fn intersects(&self, base_address: u64, size: usize) -> bool {
        if self.ranges.is_empty() || size == 0 {
            return false;
        }
        let end_address = base_address.wrapping_add(size as u64);
        let first = self.first_ending_after(base_address);
        first < self.ranges.len() && self.ranges[first].0 < end_address
    }

    /// Iterate over all ranges as `(start, end)` pairs where the range is
    /// `[start, end)`. Matches upstream `ForEach(func)` where func receives
    /// `(lower, upper)`.
    pub fn for_each<F: FnMut(u64, u64)>(&self, mut func: F) {
        for &(start, end) in &self.ranges {
            func(start, end);
        }
    }

    /// Iterate over ranges that intersect `[device_addr, device_addr + size)`.
    /// The callback receives `(clamped_start, clamped_end)` where the range is
    /// clamped to the query window.
    ///
    /// Port of Lichii `RangeSetImpl::ForEachInRange` (range_sets.inc:115-126):
    /// an empty or wrapping window reports nothing.
    pub fn for_each_in_range<F: FnMut(u64, u64)>(
        &self,
        device_addr: u64,
        size: usize,
        mut func: F,
    ) {
        let start_address = device_addr;
        let end_address = start_address.wrapping_add(size as u64);
        if end_address <= start_address {
            return;
        }
        for &(lower, upper) in &self.ranges[self.first_ending_after(start_address)..] {
            if lower >= end_address {
                break;
            }
            func(lower.max(start_address), upper.min(end_address));
        }
    }
}

impl Default for RangeSet {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// OverlapRangeSet
// ---------------------------------------------------------------------------

/// A set of address ranges that tracks overlap counts. Adding the same range
/// twice increments the count; subtracting decrements it. Ranges with a count
/// of zero or less are removed.
///
/// Maps to the C++ `OverlapRangeSet<AddressType>`.
///
/// Internally uses a sorted vector of `(start, end, count)` entries that are
/// split on boundaries as needed (similar to boost::icl::split_interval_map).
pub struct OverlapRangeSet {
    /// Sorted by start address. Ranges are non-overlapping.
    /// Each entry is (start_inclusive, end_exclusive, overlap_count).
    entries: Vec<(u64, u64, i32)>,
}

impl OverlapRangeSet {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Add the range `[base_address, base_address + size)`, incrementing
    /// overlap counts for any existing sub-ranges.
    pub fn add(&mut self, base_address: u64, size: usize) {
        if size == 0 {
            return;
        }
        let add_start = base_address;
        let add_end = base_address + size as u64;
        self.apply_delta(add_start, add_end, 1);
    }

    /// Subtract one overlap from `[base_address, base_address + size)`.
    /// Ranges whose count drops to zero are removed.
    pub fn subtract(&mut self, base_address: u64, size: usize) {
        self.subtract_with_callback(base_address, size, 1, |_, _| {});
    }

    /// Subtract one overlap and call `on_delete(start, end)` for each
    /// sub-range whose count drops to exactly zero before removal.
    pub fn subtract_with_on_delete<F: FnMut(u64, u64)>(
        &mut self,
        base_address: u64,
        size: usize,
        on_delete: F,
    ) {
        self.subtract_with_callback(base_address, size, 1, on_delete);
    }

    /// Delete all overlaps in `[base_address, base_address + size)`.
    pub fn delete_all(&mut self, base_address: u64, size: usize) {
        self.subtract_with_callback(base_address, size, i32::MAX, |_, _| {});
    }

    /// Remove all entries.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Returns true if no ranges are tracked.
    pub fn empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over all ranges as `(start, end, count)`.
    pub fn for_each<F: FnMut(u64, u64, i32)>(&self, mut func: F) {
        for &(s, e, c) in &self.entries {
            func(s, e, c);
        }
    }

    /// Iterate over ranges intersecting `[device_addr, device_addr + size)`,
    /// clamping to the query window.
    pub fn for_each_in_range<F: FnMut(u64, u64, i32)>(
        &self,
        device_addr: u64,
        size: usize,
        mut func: F,
    ) {
        if self.entries.is_empty() {
            return;
        }
        let start_address = device_addr;
        let end_address = device_addr + size as u64;

        for &(s, e, c) in &self.entries {
            if e <= start_address {
                continue;
            }
            if s >= end_address {
                break;
            }
            let clamped_start = s.max(start_address);
            let clamped_end = e.min(end_address);
            func(clamped_start, clamped_end, c);
        }
    }

    // -- internal helpers --

    /// Apply a delta to all sub-ranges within `[add_start, add_end)`.
    /// Splits existing entries at the boundaries as needed and inserts new
    /// entries for uncovered portions.
    fn apply_delta(&mut self, add_start: u64, add_end: u64, delta: i32) {
        if add_start >= add_end {
            return;
        }

        let mut new_entries: Vec<(u64, u64, i32)> = Vec::new();
        let mut pos = add_start;
        let mut i = 0;

        // Copy entries before the affected range.
        while i < self.entries.len() && self.entries[i].1 <= add_start {
            new_entries.push(self.entries[i]);
            i += 1;
        }

        // Process entries that overlap [add_start, add_end).
        while i < self.entries.len() && self.entries[i].0 < add_end {
            let (es, ee, ec) = self.entries[i];

            // Fill gap before this entry with the delta.
            if pos < es.min(add_end) {
                let gap_end = es.min(add_end);
                new_entries.push((pos, gap_end, delta));
                pos = gap_end;
            }

            if es < add_start {
                // Entry starts before our range -- split off the prefix.
                new_entries.push((es, add_start, ec));
                // The overlapping part:
                let overlap_end = ee.min(add_end);
                new_entries.push((add_start, overlap_end, ec + delta));
                pos = overlap_end;
                if ee > add_end {
                    // Entry extends past our range -- split off the suffix.
                    new_entries.push((add_end, ee, ec));
                    pos = add_end;
                }
            } else {
                // Entry starts within our range.
                let overlap_start = es.max(pos);
                // Fill any gap.
                if pos < overlap_start {
                    new_entries.push((pos, overlap_start, delta));
                }
                let overlap_end = ee.min(add_end);
                new_entries.push((overlap_start, overlap_end, ec + delta));
                pos = overlap_end;
                if ee > add_end {
                    new_entries.push((add_end, ee, ec));
                    pos = add_end;
                }
            }
            i += 1;
        }

        // Fill remaining gap.
        if pos < add_end {
            new_entries.push((pos, add_end, delta));
        }

        // Copy entries after the affected range.
        while i < self.entries.len() {
            new_entries.push(self.entries[i]);
            i += 1;
        }

        self.entries = new_entries;
    }

    /// Subtract `amount` from overlap counts in `[base_address, base_address + size)`.
    /// Calls `on_delete` for sub-ranges whose count drops to exactly zero.
    /// Removes sub-ranges whose count drops to zero or below.
    fn subtract_with_callback<F: FnMut(u64, u64)>(
        &mut self,
        base_address: u64,
        size: usize,
        amount: i32,
        mut on_delete: F,
    ) {
        if size == 0 || self.entries.is_empty() {
            return;
        }
        let sub_start = base_address;
        let sub_end = base_address + size as u64;

        // First, apply the negative delta.
        self.apply_delta(sub_start, sub_end, -amount);

        // Now remove entries with count <= 0, calling on_delete for count == 0.
        let mut cleaned = Vec::with_capacity(self.entries.len());
        for &(s, e, c) in &self.entries {
            if c <= 0 {
                if c == 0 {
                    on_delete(s, e);
                }
                // Drop this entry (count <= 0).
            } else {
                cleaned.push((s, e, c));
            }
        }
        self.entries = cleaned;
    }
}

impl Default for OverlapRangeSet {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_range_set_add_merge() {
        let mut rs = RangeSet::new();
        rs.add(10, 10); // [10, 20)
        rs.add(20, 10); // [20, 30) -- should merge into [10, 30)

        let mut ranges = Vec::new();
        rs.for_each(|s, e| ranges.push((s, e)));
        assert_eq!(ranges, vec![(10, 30)]);
    }

    #[test]
    fn test_range_set_add_overlap() {
        let mut rs = RangeSet::new();
        rs.add(10, 20); // [10, 30)
        rs.add(15, 10); // [15, 25) -- already covered

        let mut ranges = Vec::new();
        rs.for_each(|s, e| ranges.push((s, e)));
        assert_eq!(ranges, vec![(10, 30)]);
    }

    #[test]
    fn test_range_set_subtract_split() {
        let mut rs = RangeSet::new();
        rs.add(10, 30); // [10, 40)
        rs.subtract(20, 5); // remove [20, 25) -> [10,20) and [25,40)

        let mut ranges = Vec::new();
        rs.for_each(|s, e| ranges.push((s, e)));
        assert_eq!(ranges, vec![(10, 20), (25, 40)]);
    }

    #[test]
    fn test_range_set_subtract_trim_start() {
        let mut rs = RangeSet::new();
        rs.add(10, 20); // [10, 30)
        rs.subtract(10, 5); // [10, 15) removed -> [15, 30)

        let mut ranges = Vec::new();
        rs.for_each(|s, e| ranges.push((s, e)));
        assert_eq!(ranges, vec![(15, 30)]);
    }

    #[test]
    fn test_range_set_for_each_in_range() {
        let mut rs = RangeSet::new();
        rs.add(0, 100);
        rs.add(200, 100);

        let mut ranges = Vec::new();
        rs.for_each_in_range(50, 200, |s, e| ranges.push((s, e)));
        assert_eq!(ranges, vec![(50, 100), (200, 250)]);
    }

    #[test]
    fn test_range_set_empty_and_clear() {
        let mut rs = RangeSet::new();
        assert!(rs.empty());
        rs.add(0, 10);
        assert!(!rs.empty());
        rs.clear();
        assert!(rs.empty());
    }

    fn bitmap_ranges(bits: &[bool], base: u64) -> Vec<(u64, u64)> {
        let mut ranges = Vec::new();
        let mut i = 0;
        while i < bits.len() {
            if !bits[i] {
                i += 1;
                continue;
            }
            let start = i;
            while i < bits.len() && bits[i] {
                i += 1;
            }
            ranges.push((base + start as u64, base + i as u64));
        }
        ranges
    }

    fn assert_range_set(rs: &RangeSet, expected: &[(u64, u64)]) {
        let mut actual = Vec::new();
        rs.for_each(|s, e| actual.push((s, e)));
        assert_eq!(actual, expected);
        assert_eq!(rs.empty(), expected.is_empty());
        assert!(actual.iter().all(|&(s, e)| s < e));
        assert!(actual.windows(2).all(|pair| pair[0].1 < pair[1].0));
    }

    #[test]
    fn test_range_set_exhaustive_small_domain() {
        const SPAN: usize = 8;
        // Every possible initial set, insertion/subtraction and query window.
        // The independent byte bitmap checks joining, touching, gaps, splits,
        // contained intervals, full erasure, empty requests and callback order.
        for mask in 0..1u16 << SPAN {
            let bits: Vec<bool> = (0..SPAN).map(|i| mask & (1 << i) != 0).collect();
            let mut rs = RangeSet::new();
            for (i, &present) in bits.iter().enumerate().rev() {
                if present {
                    rs.add(i as u64, 1);
                }
            }
            assert_range_set(&rs, &bitmap_ranges(&bits, 0));
            for start in 0..=SPAN {
                for end in start..=SPAN {
                    let mut queried = Vec::new();
                    rs.for_each_in_range(start as u64, end - start, |s, e| queried.push((s, e)));
                    let expected: Vec<_> = bitmap_ranges(&bits, 0)
                        .into_iter()
                        .filter(|&(s, e)| start < end && s < end as u64 && e > start as u64)
                        .map(|(s, e)| (s.max(start as u64), e.min(end as u64)))
                        .collect();
                    assert_eq!(queried, expected);
                    for insert in [false, true] {
                        let mut changed = RangeSet {
                            ranges: rs.ranges.clone(),
                        };
                        let mut expected = bits.clone();
                        expected[start..end].fill(insert);
                        if insert {
                            changed.add(start as u64, end - start);
                        } else {
                            changed.subtract(start as u64, end - start);
                        }
                        assert_range_set(&changed, &bitmap_ranges(&expected, 0));
                    }
                }
            }
        }
    }

    #[test]
    fn test_range_set_randomized_bitmap_at_address_edges() {
        const SPAN: usize = 256;
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for origin in [0, (1u64 << 32) - 128, u64::MAX - SPAN as u64] {
            for _ in 0..50 {
                let mut rs = RangeSet::new();
                let mut bits = [false; SPAN];
                for _ in 0..100 {
                    let start = (next() % (SPAN + 1) as u64) as usize;
                    let size = (next() % 48) as usize;
                    let end = (start + size).min(SPAN);
                    let insert = next() & 1 != 0;
                    if insert {
                        rs.add(origin + start as u64, end - start);
                    } else {
                        rs.subtract(origin + start as u64, end - start);
                    }
                    bits[start..end].fill(insert);
                    assert_range_set(&rs, &bitmap_ranges(&bits, origin));
                    let start = (next() % (SPAN + 1) as u64) as usize;
                    let end = (start + (next() % 80) as usize).min(SPAN);
                    let mut queried = Vec::new();
                    rs.for_each_in_range(origin + start as u64, end - start, |s, e| {
                        queried.push((s, e));
                    });
                    let expected: Vec<_> = bitmap_ranges(&bits, origin)
                        .into_iter()
                        .filter(|&(s, e)| {
                            start < end && s < origin + end as u64 && e > origin + start as u64
                        })
                        .map(|(s, e)| (s.max(origin + start as u64), e.min(origin + end as u64)))
                        .collect();
                    assert_eq!(queried, expected);
                }
            }
        }
    }

    #[test]
    fn test_range_set_subtract_edges_like_lichii() {
        // Cases after Lichii tests/common/range_set_subtract.cpp: misses on both sides and between
        // ranges, touching edges, a cut inside one range, a span over several ranges keeping
        // both ends, and exact removals.
        let mut rs = RangeSet::new();
        rs.add(10, 10); // [10, 20)
        rs.add(30, 10); // [30, 40)
        rs.add(50, 10); // [50, 60)
        let all = [(10, 20), (30, 40), (50, 60)];
        for (start, size) in [(0, 10), (20, 10), (40, 10), (60, 5), (100, 1)] {
            rs.subtract(start, size);
            assert_range_set(&rs, &all);
        }
        rs.subtract(12, 2);
        assert_range_set(&rs, &[(10, 12), (14, 20), (30, 40), (50, 60)]);
        rs.subtract(15, 40);
        assert_range_set(&rs, &[(10, 12), (14, 15), (55, 60)]);
        rs.subtract(14, 1);
        assert_range_set(&rs, &[(10, 12), (55, 60)]);
        rs.subtract(55, 5);
        assert_range_set(&rs, &[(10, 12)]);
        rs.subtract(0, 100);
        assert_range_set(&rs, &[]);
        assert!(rs.empty());
    }

    #[test]
    fn test_range_set_add_joins_touching_ranges_and_intersects() {
        let mut rs = RangeSet::new();
        rs.add(10, 10);
        rs.add(30, 10);
        rs.add(20, 10); // touches both: one range
        assert_range_set(&rs, &[(10, 40)]);
        rs.add(50, 5);
        rs.add(5, 3);
        assert_range_set(&rs, &[(5, 8), (10, 40), (50, 55)]);
        assert!(rs.intersects(39, 1));
        assert!(!rs.intersects(40, 10));
        assert!(!rs.intersects(8, 2));
        assert!(rs.intersects(0, 6));
        assert!(!rs.intersects(20, 0));
    }

    #[test]
    fn test_range_set_wrapping_and_empty_mutations_are_noops() {
        let mut rs = RangeSet::new();
        rs.add(10, 10);
        rs.add(u64::MAX - 20, 20);
        let expected = [(10, 20), (u64::MAX - 20, u64::MAX)];
        for (start, size) in [
            (0, 0),
            (10, 0),
            (15, 0),
            (20, 0),
            (u64::MAX, 0),
            (u64::MAX, 1),
            (u64::MAX - 10, 20),
        ] {
            rs.add(start, size);
            assert_range_set(&rs, &expected);
            rs.subtract(start, size);
            assert_range_set(&rs, &expected);
        }
    }

    #[test]
    fn test_range_set_zero_length_query_reports_nothing() {
        // Lichii range_sets.inc:118-121: an empty window reports no range (boost's interval
        // query reported the point inside a range).
        let mut rs = RangeSet::new();
        rs.add(10, 10);
        for (start, expected) in [
            (0, Vec::<(u64, u64)>::new()),
            (10, vec![]),
            (15, vec![]),
            (20, vec![]),
        ] {
            let mut queried = Vec::new();
            rs.for_each_in_range(start, 0, |s, e| queried.push((s, e)));
            assert_eq!(queried, expected);
        }
    }

    #[test]
    fn test_range_set_sparse_full_and_tail_walks() {
        let mut rs = RangeSet::new();
        let expected: Vec<_> = (0..4096u64).map(|i| (i * 4, i * 4 + 2)).collect();
        for &(s, e) in &expected {
            rs.add(s, (e - s) as usize);
        }
        assert_range_set(&rs, &expected);
        rs.subtract(16382, 1); // Touching the final interval must not erase it.
        assert_range_set(&rs, &expected);
        let mut tail = Vec::new();
        rs.for_each_in_range(16377, 6, |s, e| tail.push((s, e)));
        assert_eq!(tail, [(16377, 16378), (16380, 16382)]);
        rs.subtract(1, 16380); // Many removals, preserving both edge fragments.
        assert_range_set(&rs, &[(0, 1), (16381, 16382)]);
        rs.add(1, 16380); // Touch both ends and bridge all intervening gaps.
        assert_range_set(&rs, &[(0, 16382)]);
        rs.subtract(0, 16382);
        assert_range_set(&rs, &[]);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_range_set_full_representable_address_span() {
        let mut rs = RangeSet::new();
        rs.add(0, usize::MAX);
        assert_range_set(&rs, &[(0, u64::MAX)]);
        let mut queried = Vec::new();
        rs.for_each_in_range(0, usize::MAX, |s, e| queried.push((s, e)));
        assert_eq!(queried, [(0, u64::MAX)]);
        rs.subtract(1, usize::MAX - 2);
        assert_range_set(&rs, &[(0, 1), (u64::MAX - 1, u64::MAX)]);
        rs.add(1, usize::MAX - 2);
        assert_range_set(&rs, &[(0, u64::MAX)]);
        rs.subtract(0, usize::MAX);
        assert!(rs.empty());
    }

    #[test]
    fn test_overlap_range_set_basic() {
        let mut ors = OverlapRangeSet::new();
        ors.add(10, 10); // [10, 20) count=1
        ors.add(10, 10); // [10, 20) count=2

        let mut ranges = Vec::new();
        ors.for_each(|s, e, c| ranges.push((s, e, c)));
        assert_eq!(ranges, vec![(10, 20, 2)]);

        ors.subtract(10, 10);
        let mut ranges = Vec::new();
        ors.for_each(|s, e, c| ranges.push((s, e, c)));
        assert_eq!(ranges, vec![(10, 20, 1)]);

        ors.subtract(10, 10);
        assert!(ors.empty());
    }

    #[test]
    fn test_overlap_range_set_partial_overlap() {
        let mut ors = OverlapRangeSet::new();
        ors.add(10, 20); // [10, 30) count=1
        ors.add(20, 20); // [20, 40) count=1 on [20,30) becomes 2, [30,40) becomes 1

        let mut ranges = Vec::new();
        ors.for_each(|s, e, c| ranges.push((s, e, c)));
        assert_eq!(ranges, vec![(10, 20, 1), (20, 30, 2), (30, 40, 1)]);
    }

    #[test]
    fn test_overlap_range_set_delete_all() {
        let mut ors = OverlapRangeSet::new();
        ors.add(10, 10);
        ors.add(10, 10);
        ors.add(10, 10);
        ors.delete_all(10, 10);
        assert!(ors.empty());
    }

    #[test]
    fn test_overlap_range_set_on_delete_callback() {
        let mut ors = OverlapRangeSet::new();
        ors.add(10, 10);
        let mut deleted = Vec::new();
        ors.subtract_with_on_delete(10, 10, |s, e| deleted.push((s, e)));
        assert_eq!(deleted, vec![(10, 20)]);
        assert!(ors.empty());
    }
}
