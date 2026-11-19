// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Lichii `video_core/page_filter.h` (GPL-3.0-or-later; an extension
//! absent from Eden, DIFF.md 2026-11-15).

use std::sync::atomic::{AtomicU64, Ordering};

const PAGE_BITS: u64 = 12;
/// 2 MiB of bits over the 36-bit device address space.
const NUM_WORDS: u64 = (1u64 << (36 - PAGE_BITS)) / 64;

/// The 4 KiB device pages a cache has ever registered objects on. Bits are
/// never cleared, so a clear bit proves the cache holds nothing there and an
/// invalidation of those pages does nothing. Ranges past the 36-bit space
/// aren't tracked: `any` reports them as possibly holding objects (such as
/// stand-in addresses of images without CPU backing).
///
/// Lichii `VideoCommon::PageFilter` (page_filter.h:14-66).
pub struct PageFilter {
    words: Box<[AtomicU64]>,
}

impl Default for PageFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl PageFilter {
    pub fn new() -> Self {
        Self { words: (0..NUM_WORDS).map(|_| AtomicU64::new(0)).collect() }
    }

    /// The bits of `[page, page + count)` within its word.
    fn word_mask(page: u64, last: u64) -> (usize, u64, u64) {
        let bit = page % 64;
        let count = (64 - bit).min(last - page + 1);
        let mask = if count == 64 { !0u64 } else { (1u64 << count) - 1 } << bit;
        ((page / 64) as usize, mask, count)
    }

    /// Records that the cache holds objects on `[address, address + size)`.
    pub fn mark(&self, address: u64, size: u64) {
        if size == 0 {
            return;
        }
        let first = address >> PAGE_BITS;
        let last = (address + size - 1) >> PAGE_BITS;
        if last >= NUM_WORDS * 64 {
            return;
        }
        let mut page = first;
        while page <= last {
            let (index, mask, count) = Self::word_mask(page, last);
            let word = &self.words[index];
            if (word.load(Ordering::Relaxed) & mask) != mask {
                word.fetch_or(mask, Ordering::Relaxed);
            }
            page += count;
        }
    }

    /// Whether the cache may hold objects on `[address, address + size)`.
    pub fn any(&self, address: u64, size: u64) -> bool {
        if size == 0 {
            return false;
        }
        let first = address >> PAGE_BITS;
        let last = (address + size - 1) >> PAGE_BITS;
        if last >= NUM_WORDS * 64 {
            return true;
        }
        let mut page = first;
        while page <= last {
            let (index, mask, count) = Self::word_mask(page, last);
            if self.words[index].load(Ordering::Relaxed) & mask != 0 {
                return true;
            }
            page += count;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_marked_range_is_always_seen_on_every_page_it_touches() {
        let filter = PageFilter::new();
        // Crosses a word boundary (page 63 -> 64) and covers whole words.
        let address = 63 * 0x1000 + 0x800;
        let size = 70 * 0x1000;
        filter.mark(address, size);
        let first = address >> 12;
        let last = (address + size - 1) >> 12;
        for page in first..=last {
            assert!(filter.any(page << 12, 1), "page {page} lost");
            assert!(filter.any((page << 12) + 0xfff, 1));
        }
        // Any overlap of the range is seen, from either side.
        assert!(filter.any(address - 0x10, 0x11));
        assert!(filter.any(address + size - 1, 0x10_0000));
        // The pages around it are not.
        assert!(!filter.any((first - 1) << 12, 0x1000));
        assert!(!filter.any((last + 1) << 12, 0x1000));
    }

    #[test]
    fn marks_accumulate_and_are_never_lost() {
        let filter = PageFilter::new();
        assert!(!filter.any(0x1234_5000, 0x1000));
        filter.mark(0x1234_5000, 1);
        filter.mark(0x1234_7000, 0x2000);
        // A later mark elsewhere in the same word keeps the earlier bits.
        filter.mark(0x1234_6000, 1);
        for page in [0x1234_5000u64, 0x1234_6000, 0x1234_7000, 0x1234_8000] {
            assert!(filter.any(page, 4));
        }
        assert!(!filter.any(0x1234_9000, 0x1000));
        assert!(!filter.any(0x1234_4000, 0x1000));
    }

    #[test]
    fn empty_ranges_and_ranges_past_the_space() {
        let filter = PageFilter::new();
        assert!(!filter.any(0x1000, 0));
        filter.mark(0x1000, 0);
        assert!(!filter.any(0x1000, 1));
        // Untracked addresses may hold objects; marking them records nothing.
        assert!(filter.any(1u64 << 36, 1));
        assert!(filter.any((1u64 << 36) - 1, 2));
        filter.mark(1u64 << 36, 0x1000);
        assert!(!filter.any(0, 0x1000));
    }
}
