// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `video_core/buffer_cache/usage_tracker.h`
//!
//! Tracks which byte ranges within a buffer have been used during
//! the current frame/pass, using a bitset at 64-byte granularity.

use common::types::align_down;

// ---------------------------------------------------------------------------
// Constants — match upstream exactly
// ---------------------------------------------------------------------------

/// Each bit covers 2^6 = 64 bytes.
const BYTES_PER_BIT_SHIFT: usize = 6;

/// Each page holds 64 bits (one `u64`), so each page covers
/// 64 * 64 = 4096 bytes.
const PAGE_SHIFT: usize = 6 + BYTES_PER_BIT_SHIFT;

/// Bytes per page (4096).
const PAGE_BYTES: usize = 1 << PAGE_SHIFT;

// ---------------------------------------------------------------------------
// UsageTracker
// ---------------------------------------------------------------------------

/// Bitset-based usage tracker for a buffer.
///
/// Each bit represents a 64-byte range. Pages of 64 bits are stored in a
/// `Vec<u64>`.
pub struct UsageTracker {
    pages: Vec<u64>,
}

impl UsageTracker {
    /// Create a new tracker for a buffer of `size` bytes.
    pub fn new(size: usize) -> Self {
        let num_pages = (size >> PAGE_SHIFT) + 1;
        Self {
            pages: vec![0u64; num_pages],
        }
    }

    /// Clear all usage bits.
    pub fn reset(&mut self) {
        self.pages.fill(0);
    }

    /// Mark the range `[offset, offset + size)` as used.
    pub fn track(&mut self, offset: u64, size: u64) {
        let page = (offset >> PAGE_SHIFT) as usize;
        let offset_end_u64 = offset.wrapping_add(size);
        let page_end = (offset_end_u64 >> PAGE_SHIFT) as usize;
        if page_end < page || page_end >= self.pages.len() {
            return;
        }
        Self::track_page(&mut self.pages, page, offset as usize, size as usize);
        if page == page_end {
            return;
        }
        for i in (page + 1)..page_end {
            self.pages[i] = !0u64;
        }
        let offset_end = offset_end_u64 as usize;
        let offset_end_page_aligned = align_down(offset_end as u64, PAGE_BYTES as u64) as usize;
        Self::track_page(
            &mut self.pages,
            page_end,
            offset_end_page_aligned,
            offset_end - offset_end_page_aligned,
        );
    }

    /// Returns true if any byte in `[offset, offset + size)` has been used.
    pub fn is_used(&self, offset: u64, size: u64) -> bool {
        let page = (offset >> PAGE_SHIFT) as usize;
        let offset_end_u64 = offset.wrapping_add(size);
        let page_end = (offset_end_u64 >> PAGE_SHIFT) as usize;
        if page_end < page || page_end >= self.pages.len() {
            return false;
        }
        if Self::is_page_used(&self.pages, page, offset as usize, size as usize) {
            return true;
        }
        if page == page_end {
            return false;
        }
        for i in (page + 1)..page_end {
            if self.pages[i] != 0 {
                return true;
            }
        }
        let offset_end = offset_end_u64 as usize;
        let offset_end_page_aligned = align_down(offset_end as u64, PAGE_BYTES as u64) as usize;
        Self::is_page_used(
            &self.pages,
            page_end,
            offset_end_page_aligned,
            offset_end - offset_end_page_aligned,
        )
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    fn track_page(pages: &mut [u64], page: usize, offset: usize, size: usize) {
        let offset_in_page = offset % PAGE_BYTES;
        let first_bit = offset_in_page >> BYTES_PER_BIT_SHIFT;
        let num_bits = size.min(PAGE_BYTES) >> BYTES_PER_BIT_SHIFT;
        // Eden shifts by 64 when `num_bits == 0`, which is undefined in C++.
        // Its supported x86-64 compilers lower the variable shift with the
        // hardware's modulo-64 count, producing an all-ones mask. Spell that
        // conservative over-marking explicitly instead of under-marking.
        let mask = if num_bits == 0 {
            !0u64
        } else {
            !0u64 >> (64 - num_bits)
        };
        pages[page] |= (!0u64 & mask) << first_bit;
    }

    fn is_page_used(pages: &[u64], page: usize, offset: usize, size: usize) -> bool {
        let offset_in_page = offset % PAGE_BYTES;
        let first_bit = offset_in_page >> BYTES_PER_BIT_SHIFT;
        let num_bits = size.min(PAGE_BYTES) >> BYTES_PER_BIT_SHIFT;
        let mask = if num_bits == 0 {
            !0u64
        } else {
            !0u64 >> (64 - num_bits)
        };
        let mask2 = (!0u64 & mask) << first_bit;
        (pages[page] & mask2) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_tracker() {
        let tracker = UsageTracker::new(4096);
        assert!(!tracker.is_used(0, 64));
    }

    #[test]
    fn test_track_and_query() {
        let mut tracker = UsageTracker::new(8192);
        tracker.track(0, 64);
        assert!(tracker.is_used(0, 64));
        assert!(!tracker.is_used(64, 64));
    }

    #[test]
    fn sub_granule_ranges_keep_edens_conservative_x64_mask() {
        let mut tracker = UsageTracker::new(4096);
        tracker.track(96, 1);

        assert!(!tracker.is_used(0, 64));
        assert!(tracker.is_used(64, 64));
        assert!(tracker.is_used(4032, 1));
    }

    #[test]
    fn zero_length_range_keeps_edens_effective_x64_mask() {
        let mut tracker = UsageTracker::new(4096);
        tracker.track(0, 0);

        assert!(tracker.is_used(0, 1));
        assert!(tracker.is_used(4032, 1));
    }

    #[test]
    fn test_track_cross_page() {
        let mut tracker = UsageTracker::new(16384);
        // Track a range spanning two pages
        tracker.track(4000, 200);
        assert!(tracker.is_used(4000, 200));
    }

    #[test]
    fn test_reset() {
        let mut tracker = UsageTracker::new(4096);
        tracker.track(0, 4096);
        assert!(tracker.is_used(0, 64));
        tracker.reset();
        assert!(!tracker.is_used(0, 64));
    }
}
