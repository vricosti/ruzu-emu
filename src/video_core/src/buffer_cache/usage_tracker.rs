// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `video_core/buffer_cache/usage_tracker.h`, with Lichii's extensions
//! (Lichii `video_core/buffer_cache/usage_tracker.h`).
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

/// Lichii: the ranges tracked last, skipped when tracked again before a reset.
const RECENT_RANGES: usize = 4;

// ---------------------------------------------------------------------------
// UsageTracker
// ---------------------------------------------------------------------------

/// Bitset-based usage tracker for a buffer.
///
/// Each bit represents a 64-byte range. Pages of 64 bits are stored in a
/// `Vec<u64>`.
pub struct UsageTracker {
    pages: Vec<u64>,
    /// Ranges tracked last, (offset, size).
    recent: [(u64, u64); RECENT_RANGES],
    recent_cursor: usize,
    /// Pages known to be all ones (`fill_pages`).
    full_begin: usize,
    full_end: usize,
    /// The words set since the last reset lie in [set_begin, set_end) (none when set_end is 0).
    set_begin: usize,
    set_end: usize,
    tracked: bool,
}

impl UsageTracker {
    /// Create a new tracker for a buffer of `size` bytes.
    pub fn new(size: usize) -> Self {
        let num_pages = (size >> PAGE_SHIFT) + 1;
        Self {
            pages: vec![0u64; num_pages],
            recent: [(u64::MAX, 0); RECENT_RANGES],
            recent_cursor: 0,
            full_begin: 0,
            full_end: 0,
            set_begin: 0,
            set_end: 0,
            tracked: false,
        }
    }

    /// Lichii `UsageTracker::Reset`: only `track` sets words, and only in
    /// [set_begin, set_end): a buffer of hundreds of MiB bound in part cleared
    /// all its words (8 bytes a 4 KiB page) every frame.
    pub fn reset(&mut self) {
        if !self.tracked {
            return;
        }
        self.tracked = false;
        self.pages[self.set_begin..self.set_end].fill(0);
        self.set_begin = 0;
        self.set_end = 0;
        self.recent = [(u64::MAX, 0); RECENT_RANGES];
        self.full_begin = 0;
        self.full_end = 0;
    }

    /// Lichii `UsageTracker::Track`: tracking is idempotent until `reset`, so
    /// ranges tracked just before are skipped. Draws bind the same (often
    /// multi-megabyte) storage buffers again and again, and each track wrote a
    /// word per 4 KiB of the range.
    pub fn track(&mut self, offset: u64, size: u64) {
        if self.recent.contains(&(offset, size)) {
            return;
        }
        self.tracked = true;
        self.recent[self.recent_cursor % RECENT_RANGES] = (offset, size);
        self.recent_cursor = self.recent_cursor.wrapping_add(1);
        let page = (offset >> PAGE_SHIFT) as usize;
        let offset_end_u64 = offset.wrapping_add(size);
        let page_end = (offset_end_u64 >> PAGE_SHIFT) as usize;
        if page_end < page || page_end >= self.pages.len() {
            return;
        }
        // track_page and fill_pages below set words only in [page, page_end].
        self.set_begin = if self.set_end == 0 { page } else { self.set_begin.min(page) };
        self.set_end = self.set_end.max(page_end + 1);
        self.track_page(page, offset as usize, size as usize);
        if page == page_end {
            return;
        }
        self.fill_pages(page + 1, page_end);
        let offset_end = offset_end_u64 as usize;
        let offset_end_page_aligned = align_down(offset_end as u64, PAGE_BYTES as u64) as usize;
        self.track_page(page_end, offset_end_page_aligned, offset_end - offset_end_page_aligned);
    }

    /// Whether any byte of the range is tracked.
    pub fn is_used(&self, offset: u64, size: u64) -> bool {
        let page = (offset >> PAGE_SHIFT) as usize;
        let offset_end_u64 = offset.wrapping_add(size);
        let page_end = (offset_end_u64 >> PAGE_SHIFT) as usize;
        if page_end < page || page_end >= self.pages.len() {
            return false;
        }
        if self.is_page_used(page, offset as usize, size as usize) {
            return true;
        }
        if page == page_end {
            return false;
        }
        if self.pages[(page + 1)..page_end].iter().any(|&word| word != 0) {
            return true;
        }
        let offset_end = offset_end_u64 as usize;
        let offset_end_page_aligned = align_down(offset_end as u64, PAGE_BYTES as u64) as usize;
        self.is_page_used(page_end, offset_end_page_aligned, offset_end - offset_end_page_aligned)
    }

    /// Lichii `UsageTracker::Pages`.
    pub fn pages(&self) -> &[u64] {
        &self.pages
    }

    /// Lichii `UsageTracker::FillPages`: pages in [full_begin, full_end) are
    /// all ones already (only `reset` clears a page), so this fills around
    /// that run and grows it, keeping a multi-megabyte binding O(1) once filled.
    fn fill_pages(&mut self, begin: usize, end: usize) {
        if begin >= end {
            return;
        }
        if end < self.full_begin || begin > self.full_end {
            self.pages[begin..end].fill(!0u64);
            if end - begin > self.full_end - self.full_begin {
                self.full_begin = begin;
                self.full_end = end;
            }
            return;
        }
        if begin < self.full_begin {
            self.pages[begin..self.full_begin].fill(!0u64);
        }
        if end > self.full_end {
            self.pages[self.full_end..end].fill(!0u64);
        }
        self.full_begin = begin.min(self.full_begin);
        self.full_end = end.max(self.full_end);
    }

    /// Lichii `UsageTracker::TrackPage`: the bits of the 64-byte granules the
    /// range touches in the page; an empty range sets none.
    fn track_page(&mut self, page: usize, offset: usize, size: usize) {
        if size == 0 {
            return;
        }
        self.pages[page] |= Self::page_mask(offset, size);
    }

    /// Lichii `UsageTracker::IsPageUsed`.
    fn is_page_used(&self, page: usize, offset: usize, size: usize) -> bool {
        if size == 0 {
            return false;
        }
        self.pages[page] & Self::page_mask(offset, size) != 0
    }

    fn page_mask(offset: usize, size: usize) -> u64 {
        let offset_in_page = offset % PAGE_BYTES;
        let first_bit = offset_in_page >> BYTES_PER_BIT_SHIFT;
        let bytes = size.min(PAGE_BYTES - offset_in_page);
        let last_bit = (offset_in_page + bytes - 1) >> BYTES_PER_BIT_SHIFT;
        let num_bits = last_bit - first_bit + 1;
        let mask = !0u64 >> (64 - num_bits);
        mask << first_bit
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

    /// Lichii's TrackPage marks exactly the granules a range touches (Eden's
    /// mask was computed from the size alone and over-marked).
    #[test]
    fn sub_granule_ranges_mark_the_touched_granule() {
        let mut tracker = UsageTracker::new(4096);
        tracker.track(96, 1);

        assert!(!tracker.is_used(0, 64));
        assert!(tracker.is_used(64, 64));
        assert!(!tracker.is_used(4032, 1));
        // A range straddling two granules marks both.
        tracker.track(120, 16);
        assert!(tracker.is_used(128, 1));
    }

    /// Lichii: an empty range sets no bit (Eden set the whole page).
    #[test]
    fn zero_length_range_marks_nothing() {
        let mut tracker = UsageTracker::new(4096);
        tracker.track(0, 0);

        assert!(!tracker.is_used(0, 1));
        assert!(!tracker.is_used(4032, 1));
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

    /// Lichii: a range tracked again before a reset is skipped, the reset
    /// clears only the words that were set, and a full run grows in place.
    #[test]
    fn repeated_large_ranges_and_partial_reset() {
        let mut tracker = UsageTracker::new(64 * PAGE_BYTES);
        tracker.track(PAGE_BYTES as u64 * 2, PAGE_BYTES as u64 * 20);
        assert_eq!((tracker.set_begin, tracker.set_end), (2, 23));
        assert_eq!((tracker.full_begin, tracker.full_end), (3, 22));
        tracker.track(PAGE_BYTES as u64 * 2, PAGE_BYTES as u64 * 20);
        assert_eq!(tracker.recent_cursor, 1);
        tracker.track(PAGE_BYTES as u64 * 10, PAGE_BYTES as u64 * 30);
        assert_eq!((tracker.full_begin, tracker.full_end), (3, 40));
        assert!(tracker.pages()[3..40].iter().all(|&word| word == !0u64));
        assert!(tracker.is_used(PAGE_BYTES as u64 * 39, 1));
        assert!(!tracker.is_used(PAGE_BYTES as u64 * 41, 1));
        tracker.reset();
        assert!(tracker.pages().iter().all(|&word| word == 0));
        assert!(!tracker.is_used(PAGE_BYTES as u64 * 10, 64));
        // Tracked again after the reset.
        tracker.track(PAGE_BYTES as u64 * 2, PAGE_BYTES as u64 * 20);
        assert!(tracker.is_used(PAGE_BYTES as u64 * 5, 64));
    }
}
