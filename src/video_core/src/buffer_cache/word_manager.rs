// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `video_core/buffer_cache/word_manager.h`
//!
//! Word-level dirty tracking for buffer cache pages.
//! Tracks CPU, GPU, cached CPU, untracked, and preflushable states
//! using bitmask words where each bit represents one device page.

use common::types::VAddr;

// ---------------------------------------------------------------------------
// Constants (from word_manager.h top-level)
// ---------------------------------------------------------------------------

/// Number of device pages tracked per 64-bit word.
pub const PAGES_PER_WORD: u64 = 64;

/// Bytes per device page (matches `Core::DEVICE_PAGESIZE`).
pub const BYTES_PER_PAGE: u64 = 4096;

/// Bytes covered by a single tracking word.
pub const BYTES_PER_WORD: u64 = PAGES_PER_WORD * BYTES_PER_PAGE;

// ---------------------------------------------------------------------------
// Type — tracking channel enum
// ---------------------------------------------------------------------------

/// Which tracking channel a query / mutation targets.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    Cpu = 0,
    Gpu = 1,
    CachedCpu = 2,
    Untracked = 3,
    Preflushable = 4,
    Max = 5,
}

const TYPE_COUNT: usize = Type::Max as usize;

// ---------------------------------------------------------------------------
// DeviceTracker trait — abstraction for the rasterizer notification callback
// ---------------------------------------------------------------------------

/// Trait that the rasterizer must implement so that the word manager can
/// notify it about page-tracking changes.
///
/// Corresponds to the `DeviceTracker` template parameter in C++.
pub trait DeviceTracker {
    /// Adjust cached-page reference counts for multiple ranges under one
    /// tracker-side lock acquisition.
    fn update_pages_cached_batch(&self, ranges: &[(VAddr, usize)], delta: i32);
}

// ---------------------------------------------------------------------------
// WordManager<DT, STACK_WORDS, SIZE_BYTES>
// ---------------------------------------------------------------------------

/// Per-region word-level dirty tracker.
///
/// Corresponds to the C++
/// `WordManager<DeviceTracker, stack_words, size_bytes>` template.
#[repr(C)]
pub struct WordManager<DT: DeviceTracker, const STACK_WORDS: usize, const SIZE_BYTES: u64> {
    // Stable Rust cannot use `Type::Max * num_words` as a generic array
    // expression, so preserve the same fixed inline storage as one array per
    // tracking type. The type-major, word-minor ordering is unchanged.
    heap: [[u64; STACK_WORDS]; TYPE_COUNT],
    tracker: *const DT,
    cpu_addr: VAddr,
}

unsafe impl<DT: DeviceTracker, const N: usize, const S: u64> Send for WordManager<DT, N, S> {}
unsafe impl<DT: DeviceTracker, const N: usize, const S: u64> Sync for WordManager<DT, N, S> {}

impl<DT: DeviceTracker, const STACK_WORDS: usize, const SIZE_BYTES: u64>
    WordManager<DT, STACK_WORDS, SIZE_BYTES>
{
    /// Matching upstream `static constexpr size_t num_words`.
    pub const NUM_WORDS: usize = SIZE_BYTES.div_ceil(BYTES_PER_WORD) as usize;

    /// Create a new word manager for a region starting at `cpu_addr`.
    pub fn new(cpu_addr: VAddr, tracker: &DT) -> Self {
        Self::assert_template_parameters();

        let mut heap = [[0; STACK_WORDS]; TYPE_COUNT];
        heap[Type::Cpu as usize].fill(!0);
        heap[Type::Untracked as usize].fill(!0);

        // Clean up trailing bits exactly like the upstream constructor.
        let last_word_size = SIZE_BYTES % BYTES_PER_WORD;
        let last_local_page = last_word_size.div_ceil(BYTES_PER_PAGE);
        let shift = (PAGES_PER_WORD - last_local_page) % PAGES_PER_WORD;
        let last_word = (!0u64 << shift) >> shift;
        heap[Type::Cpu as usize][STACK_WORDS - 1] = last_word;
        heap[Type::Untracked as usize][STACK_WORDS - 1] = last_word;

        Self {
            heap,
            tracker: tracker as *const DT,
            cpu_addr,
        }
    }

    /// Create a default (empty) word manager.
    pub fn empty() -> Self {
        Self::assert_template_parameters();
        Self {
            heap: [[0; STACK_WORDS]; TYPE_COUNT],
            tracker: std::ptr::null(),
            cpu_addr: 0,
        }
    }

    #[inline]
    fn span(&self, ty: Type) -> &[u64] {
        &self.heap[ty as usize]
    }

    #[inline]
    fn span_mut(&mut self, ty: Type) -> &mut [u64] {
        &mut self.heap[ty as usize]
    }

    #[inline]
    fn assert_template_parameters() {
        assert_eq!(
            STACK_WORDS,
            Self::NUM_WORDS,
            "stack_words must equal DivCeil(size_bytes, BYTES_PER_WORD)"
        );
    }

    pub fn set_cpu_address(&mut self, new_cpu_addr: VAddr) {
        self.cpu_addr = new_cpu_addr;
    }

    pub fn get_cpu_addr(&self) -> VAddr {
        self.cpu_addr
    }

    /// Extract bits from a word between `page_start` and `page_end`.
    #[inline]
    pub fn extract_bits(word: u64, page_start: usize, page_end: usize) -> u64 {
        let number_bits: usize = 64;
        let limit_page_end = number_bits - page_end.min(number_bits);
        let bits = (word >> page_start) << page_start;
        (bits << limit_page_end) >> limit_page_end
    }

    /// Get the word index and page-within-word for an address.
    #[inline]
    pub fn get_word_page(address: VAddr) -> (usize, usize) {
        let addr = address as usize;
        let word_number = addr / BYTES_PER_WORD as usize;
        let amount_pages = addr % BYTES_PER_WORD as usize;
        (word_number, amount_pages / BYTES_PER_PAGE as usize)
    }

    /// Iterate over words that overlap `[offset, offset+size)`, calling `func`
    /// with `(word_index, mask)`.
    ///
    /// If `func` returns `Some(true)`, iteration stops early (bool-break pattern).
    pub fn iterate_words<F>(&self, offset: u64, size: u64, mut func: F)
    where
        F: FnMut(usize, u64) -> Option<bool>,
    {
        let start = (offset as i64).max(0) as usize;
        let end = (offset.wrapping_add(size) as i64).max(0) as usize;
        if start >= SIZE_BYTES as usize || end <= start {
            return;
        }
        let (mut start_word, start_page) = Self::get_word_page(start as u64);
        let (mut end_word, mut end_page) =
            Self::get_word_page((end as u64).wrapping_add(BYTES_PER_PAGE).wrapping_sub(1));
        let num_words = Self::NUM_WORDS;
        start_word = start_word.min(num_words);
        end_word = end_word.min(num_words);
        let diff = end_word - start_word;
        end_word += (end_page + PAGES_PER_WORD as usize - 1) / PAGES_PER_WORD as usize;
        end_word = end_word.min(num_words);
        end_page += diff * PAGES_PER_WORD as usize;
        let mut current_start_page = start_page;
        let base_mask: u64 = !0u64;

        for word_index in start_word..end_word {
            let mask = Self::extract_bits(base_mask, current_start_page, end_page);
            current_start_page = 0;
            end_page = end_page.wrapping_sub(PAGES_PER_WORD as usize);
            if let Some(true) = func(word_index, mask) {
                return;
            }
        }
    }

    /// Iterate over contiguous runs of set pages within a word mask.
    #[inline]
    pub fn iterate_pages<F>(mask: u64, mut func: F)
    where
        F: FnMut(usize, usize),
    {
        let mut m = mask;
        let mut offset: usize = 0;
        while m != 0 {
            let empty_bits = m.trailing_zeros() as usize;
            offset += empty_bits;
            m >>= empty_bits;

            let continuous_bits = m.trailing_ones() as usize;
            func(offset, continuous_bits);
            m = if continuous_bits < PAGES_PER_WORD as usize {
                m >> continuous_bits
            } else {
                0
            };
            offset += continuous_bits;
        }
    }

    /// Change the state of a range of pages.
    ///
    /// `enable` = true sets the bits, false clears them.
    pub fn change_region_state(&mut self, ty: Type, enable: bool, dirty_addr: u64, size: u64) {
        // Use raw pointers to split simultaneous mutable access to distinct
        // tracking channels, matching the independent upstream spans.
        let state_ptr = self.heap[ty as usize].as_mut_ptr();
        let untracked_ptr = self.heap[Type::Untracked as usize].as_mut_ptr();
        let cached_ptr = self.heap[Type::CachedCpu as usize].as_mut_ptr();

        let cpu_addr = self.cpu_addr;
        let tracker = self.tracker;
        let mut ranges = Vec::new();

        self.iterate_words(dirty_addr.wrapping_sub(cpu_addr), size, |index, mask| {
            unsafe {
                let state_word = state_ptr.add(index);
                let untracked_word = untracked_ptr.add(index);
                let cached_word = cached_ptr.add(index);

                match ty {
                    Type::Cpu | Type::CachedCpu => {
                        Self::collect_changed_ranges(
                            cpu_addr,
                            !enable,
                            index,
                            *untracked_word,
                            mask,
                            &mut ranges,
                        );
                    }
                    _ => {}
                }

                if enable {
                    *state_word |= mask;
                    if matches!(ty, Type::Cpu | Type::CachedCpu) {
                        *untracked_word |= mask;
                    }
                    if matches!(ty, Type::Cpu) {
                        *cached_word &= !mask;
                    }
                } else {
                    if matches!(ty, Type::Cpu) {
                        let word = *state_word & mask;
                        *cached_word &= !word;
                    }
                    *state_word &= !mask;
                    if matches!(ty, Type::Cpu | Type::CachedCpu) {
                        *untracked_word &= !mask;
                    }
                }
            }
            None
        });
        Self::apply_collected_ranges(tracker, &mut ranges, if enable { -1 } else { 1 });
    }

    /// Call `func` for each modified range and optionally clear the modified bits.
    pub fn for_each_modified_range<F>(
        &mut self,
        ty: Type,
        clear: bool,
        query_cpu_range: VAddr,
        size: u64,
        func: &mut F,
    ) where
        F: FnMut(VAddr, u64),
    {
        let state_ptr = self.heap[ty as usize].as_mut_ptr();
        let untracked_ptr = self.heap[Type::Untracked as usize].as_mut_ptr();
        let cached_ptr = self.heap[Type::CachedCpu as usize].as_mut_ptr();

        let offset = query_cpu_range.wrapping_sub(self.cpu_addr);
        let cpu_addr = self.cpu_addr;
        let tracker = self.tracker;

        let mut pending = false;
        let mut pending_offset: usize = 0;
        let mut pending_pointer: usize = 0;
        let mut ranges = Vec::new();

        self.iterate_words(offset, size, |index, mut mask| {
            unsafe {
                if matches!(ty, Type::Gpu) {
                    mask &= !(*untracked_ptr.add(index));
                }
                let word = (*state_ptr.add(index)) & mask;

                if clear {
                    match ty {
                        Type::Cpu | Type::CachedCpu => {
                            Self::collect_changed_ranges(
                                cpu_addr,
                                true,
                                index,
                                *untracked_ptr.add(index),
                                mask,
                                &mut ranges,
                            );
                        }
                        _ => {}
                    }
                    *state_ptr.add(index) &= !mask;
                    if matches!(ty, Type::Cpu | Type::CachedCpu) {
                        *untracked_ptr.add(index) &= !mask;
                    }
                    if matches!(ty, Type::Cpu) {
                        *cached_ptr.add(index) &= !word;
                    }
                }

                let base_offset = index * PAGES_PER_WORD as usize;
                Self::iterate_pages(word, |pages_offset, pages_size| {
                    if !pending {
                        pending_offset = base_offset + pages_offset;
                        pending_pointer = base_offset + pages_offset + pages_size;
                        pending = true;
                        return;
                    }
                    if pending_pointer == base_offset + pages_offset {
                        pending_pointer += pages_size;
                        return;
                    }
                    // Release the pending range
                    func(
                        cpu_addr.wrapping_add((pending_offset as u64).wrapping_mul(BYTES_PER_PAGE)),
                        ((pending_pointer - pending_offset) as u64).wrapping_mul(BYTES_PER_PAGE),
                    );
                    pending_offset = base_offset + pages_offset;
                    pending_pointer = base_offset + pages_offset + pages_size;
                });
            }
            None
        });
        if pending {
            func(
                cpu_addr.wrapping_add((pending_offset as u64).wrapping_mul(BYTES_PER_PAGE)),
                ((pending_pointer - pending_offset) as u64).wrapping_mul(BYTES_PER_PAGE),
            );
        }
        Self::apply_collected_ranges(tracker, &mut ranges, 1);
    }

    /// Returns true when a region has been modified for the given type.
    pub fn is_region_modified(&self, ty: Type, offset: u64, size: u64) -> bool {
        let state_words = self.span(ty);
        let untracked_words = self.span(Type::Untracked);
        let mut result = false;

        self.iterate_words(offset, size, |index, mut mask| {
            if matches!(ty, Type::Gpu) {
                mask &= !untracked_words[index];
            }
            let word = state_words[index] & mask;
            if word != 0 {
                result = true;
                return Some(true);
            }
            Some(false)
        });
        result
    }

    /// Returns the inclusive modified region as a `(begin, end)` pair in bytes.
    pub fn modified_region(&self, ty: Type, offset: u64, size: u64) -> (u64, u64) {
        let state_words = self.span(ty);
        let untracked_words = self.span(Type::Untracked);
        let mut begin: u64 = u64::MAX;
        let mut end: u64 = 0;

        self.iterate_words(offset, size, |index, mut mask| {
            if matches!(ty, Type::Gpu) {
                mask &= !untracked_words[index];
            }
            let word = state_words[index] & mask;
            if word == 0 {
                return None;
            }
            let local_page_begin = word.trailing_zeros() as u64;
            let local_page_end = PAGES_PER_WORD - word.leading_zeros() as u64;
            let page_index = (index as u64).wrapping_mul(PAGES_PER_WORD);
            begin = begin.min(page_index.wrapping_add(local_page_begin));
            end = page_index.wrapping_add(local_page_end);
            None
        });

        if begin < end {
            (
                begin.wrapping_mul(BYTES_PER_PAGE),
                end.wrapping_mul(BYTES_PER_PAGE),
            )
        } else {
            (0, 0)
        }
    }

    /// Flush cached CPU writes: move cached bits into the CPU channel.
    pub fn flush_cached_writes(&mut self) {
        let num_words = Self::NUM_WORDS;
        let cached_ptr = self.heap[Type::CachedCpu as usize].as_mut_ptr();
        let untracked_ptr = self.heap[Type::Untracked as usize].as_mut_ptr();
        let cpu_ptr = self.heap[Type::Cpu as usize].as_mut_ptr();
        let tracker = self.tracker;
        let cpu_addr = self.cpu_addr;
        let mut ranges = Vec::new();

        for word_index in 0..num_words {
            unsafe {
                let cached_bits = *cached_ptr.add(word_index);
                Self::collect_changed_ranges(
                    cpu_addr,
                    false,
                    word_index,
                    *untracked_ptr.add(word_index),
                    cached_bits,
                    &mut ranges,
                );
                *untracked_ptr.add(word_index) |= cached_bits;
                *cpu_ptr.add(word_index) |= cached_bits;
                *cached_ptr.add(word_index) = 0;
            }
        }
        Self::apply_collected_ranges(tracker, &mut ranges, -1);
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Port of `WordManager::CollectChangedRanges`.
    fn collect_changed_ranges(
        cpu_addr: VAddr,
        add_to_tracker: bool,
        word_index: usize,
        current_bits: u64,
        new_bits: u64,
        ranges: &mut Vec<(VAddr, usize)>,
    ) {
        let changed_bits = if add_to_tracker {
            current_bits & new_bits
        } else {
            !current_bits & new_bits
        };
        let addr = cpu_addr.wrapping_add((word_index as u64).wrapping_mul(BYTES_PER_WORD));
        Self::iterate_pages(changed_bits, |page_offset, page_size| {
            ranges.push((
                addr.wrapping_add((page_offset as u64).wrapping_mul(BYTES_PER_PAGE)),
                page_size.wrapping_mul(BYTES_PER_PAGE as usize),
            ));
        });
    }

    /// Port of `WordManager::ApplyCollectedRanges`.
    fn apply_collected_ranges(tracker: *const DT, ranges: &mut Vec<(VAddr, usize)>, delta: i32) {
        if ranges.is_empty() {
            return;
        }

        ranges.sort_unstable_by_key(|&(addr, _)| addr);
        let mut coalesced = Vec::with_capacity(ranges.len());
        let (mut current_addr, mut current_size) = ranges[0];
        for &(next_addr, next_size) in &ranges[1..] {
            if current_addr.wrapping_add(current_size as u64) == next_addr {
                current_size = current_size.wrapping_add(next_size);
            } else {
                coalesced.push((current_addr, current_size));
                current_addr = next_addr;
                current_size = next_size;
            }
        }
        coalesced.push((current_addr, current_size));

        let tracker = unsafe {
            tracker
                .as_ref()
                .expect("a WordManager that applies ranges must have a device tracker")
        };
        tracker.update_pages_cached_batch(&coalesced, delta);
        ranges.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    type DummyManager = WordManager<DummyTracker, 1, { BYTES_PER_WORD }>;
    type RecordingManager = WordManager<RecordingTracker, 2, { BYTES_PER_WORD * 2 }>;
    type TailManager = WordManager<RecordingTracker, 1, { 3 * BYTES_PER_PAGE }>;

    #[test]
    fn test_extract_bits() {
        // Full word
        assert_eq!(DummyManager::extract_bits(!0u64, 0, 64), !0u64);
        // First bit only
        assert_eq!(DummyManager::extract_bits(!0u64, 0, 1), 1);
        // Bits 2..5
        assert_eq!(DummyManager::extract_bits(!0u64, 2, 5), 0b11100);
    }

    #[test]
    fn test_get_word_page() {
        let (w, p) = DummyManager::get_word_page(0);
        assert_eq!(w, 0);
        assert_eq!(p, 0);

        let (w, p) = DummyManager::get_word_page(BYTES_PER_WORD);
        assert_eq!(w, 1);
        assert_eq!(p, 0);
    }

    #[test]
    fn test_iterate_pages() {
        let mut ranges = Vec::new();
        DummyManager::iterate_pages(0b1110_0011, |off, sz| {
            ranges.push((off, sz));
        });
        assert_eq!(ranges, vec![(0, 2), (5, 3)]);
    }

    #[test]
    fn change_region_state_batches_across_word_boundaries() {
        let tracker = RecordingTracker::default();
        let base = 0x4000_0000;
        let mut manager = RecordingManager::new(base, &tracker);
        let addr = base + 63 * BYTES_PER_PAGE;
        let size = 3 * BYTES_PER_PAGE;

        manager.change_region_state(Type::Cpu, false, addr, size);
        manager.change_region_state(Type::Cpu, true, addr, size);

        assert_eq!(
            *tracker.calls.lock().unwrap(),
            vec![
                (vec![(addr, size as usize)], 1),
                (vec![(addr, size as usize)], -1)
            ]
        );
    }

    #[test]
    fn flush_cached_writes_coalesces_word_ranges() {
        let tracker = RecordingTracker::default();
        let base = 0x5000_0000;
        let mut manager = RecordingManager::new(base, &tracker);

        manager.span_mut(Type::CachedCpu)[0] = 1 << 63;
        manager.span_mut(Type::CachedCpu)[1] = 0b11;
        manager.span_mut(Type::Untracked)[0] &= !(1 << 63);
        manager.span_mut(Type::Untracked)[1] &= !0b11;

        manager.flush_cached_writes();

        let addr = base + 63 * BYTES_PER_PAGE;
        assert_eq!(
            *tracker.calls.lock().unwrap(),
            vec![(vec![(addr, (3 * BYTES_PER_PAGE) as usize)], -1)]
        );
    }

    #[test]
    fn type_order_and_inline_storage_match_upstream() {
        assert_eq!(
            [
                Type::Cpu as usize,
                Type::Gpu as usize,
                Type::CachedCpu as usize,
                Type::Untracked as usize,
                Type::Preflushable as usize,
                Type::Max as usize,
            ],
            [0, 1, 2, 3, 4, 5]
        );
        assert_eq!(std::mem::size_of::<Type>(), std::mem::size_of::<i32>());

        let tracker = RecordingTracker::default();
        let manager = RecordingManager::new(0, &tracker);
        assert_eq!(manager.heap.len(), Type::Max as usize);
        assert!(manager.heap.iter().all(|words| words.len() == 2));
        assert_eq!(RecordingManager::NUM_WORDS, 2);
        assert_eq!(
            std::mem::size_of::<RecordingManager>(),
            Type::Max as usize * 2 * std::mem::size_of::<u64>()
                + std::mem::size_of::<*const RecordingTracker>()
                + std::mem::size_of::<VAddr>()
        );
    }

    #[test]
    fn constructor_cleans_trailing_pages_like_upstream() {
        let tracker = RecordingTracker::default();
        let manager = TailManager::new(0, &tracker);
        assert_eq!(manager.span(Type::Cpu), &[0b111]);
        assert_eq!(manager.span(Type::Untracked), &[0b111]);
        assert_eq!(manager.span(Type::Gpu), &[0]);
        assert_eq!(manager.span(Type::CachedCpu), &[0]);
        assert_eq!(manager.span(Type::Preflushable), &[0]);
    }

    #[derive(Default)]
    struct RecordingTracker {
        calls: Mutex<Vec<(Vec<(VAddr, usize)>, i32)>>,
    }

    impl DeviceTracker for RecordingTracker {
        fn update_pages_cached_batch(&self, ranges: &[(VAddr, usize)], delta: i32) {
            self.calls.lock().unwrap().push((ranges.to_vec(), delta));
        }
    }

    struct DummyTracker;
    impl DeviceTracker for DummyTracker {
        fn update_pages_cached_batch(&self, _ranges: &[(VAddr, usize)], _delta: i32) {}
    }
}
