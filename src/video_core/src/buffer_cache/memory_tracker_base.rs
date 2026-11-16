// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `video_core/buffer_cache/memory_tracker_base.h`
//!
//! Two-level memory tracker that dispatches page-level queries to
//! per-region `WordManager` instances. Each higher page covers 4 MiB
//! and is lazily allocated from a pooled free-list.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};

use common::types::VAddr;

use super::word_manager::{DeviceTracker, Type, WordManager, BYTES_PER_WORD};

// ---------------------------------------------------------------------------
// Constants — match upstream exactly
// ---------------------------------------------------------------------------

/// Maximum device address bits tracked (36 bits = 64 GiB), as in Eden NX2.
const MAX_CPU_PAGE_BITS: usize = 36;

/// Bits per higher page (22 bits = 4 MiB per higher page).
const HIGHER_PAGE_BITS: usize = 22;

/// Size in bytes of one higher page.
const HIGHER_PAGE_SIZE: u64 = 1u64 << HIGHER_PAGE_BITS;

/// Mask for offset within a higher page.
const HIGHER_PAGE_MASK: u64 = HIGHER_PAGE_SIZE - 1;

/// Number of higher-page slots.
const NUM_HIGH_PAGES: usize = 1 << (MAX_CPU_PAGE_BITS - HIGHER_PAGE_BITS);

/// Pool allocation batch size.
const MANAGER_POOL_SIZE: usize = 32;

/// Number of words each manager needs on the stack.
const WORDS_STACK_NEEDED: usize = (HIGHER_PAGE_SIZE as usize) / (BYTES_PER_WORD as usize);

/// Matching upstream `using Manager = WordManager<...>` alias.
type Manager<DT> = WordManager<DT, WORDS_STACK_NEEDED, HIGHER_PAGE_SIZE>;

/// Diagnostic: how many out-of-range tracker queries have been logged so far.
/// Upstream indexes a `std::array<Manager*, NUM_HIGH_PAGES>` with the raw page
/// index (silent UB if the device address exceeds the 64 GiB device address
/// space). ruzu's bounds-checked fixed array would panic instead, so we detect
/// and log the offending (address, size) to surface the real bug — a device
/// address above `SMMU_VA_LIMIT` (2^36 - 1) must never reach the buffer cache.
static OOR_LOG_COUNT: AtomicU64 = AtomicU64::new(0);

/// Log an out-of-range page query (rate-limited) so the bad address source can
/// be traced. Out-of-range pages are skipped rather than crashing the GPU thread.
#[cold]
#[inline(never)]
fn report_out_of_range(cpu_address: VAddr, size: u64, page_index: usize) {
    if OOR_LOG_COUNT.fetch_add(1, Ordering::Relaxed) < 16 {
        log::warn!(
            "[MEMTRACK_OOR] device address out of tracked range: addr=0x{:X} size=0x{:X} \
             page_index={} (NUM_HIGH_PAGES={}); address exceeds SMMU_VA_LIMIT (2^{}). \
             Skipping page to avoid crash — the producer of this address is the real bug.",
            cpu_address,
            size,
            page_index,
            NUM_HIGH_PAGES,
            MAX_CPU_PAGE_BITS
        );
        if common::env_flag!("RUZU_TRACE_MEMTRACK_OOR_BT") {
            eprintln!(
                "[MEMTRACK_OOR_BT] addr=0x{cpu_address:X} size=0x{size:X} page_index={page_index}\n{}",
                std::backtrace::Backtrace::force_capture()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// MemoryTrackerBase<DT>
// ---------------------------------------------------------------------------

/// Two-level paged memory tracker.
///
/// The top tier is a fixed-size array of `NUM_HIGH_PAGES` optional pointers
/// to `WordManager` instances. Managers are allocated in pools and recycled
/// through a free-list.
///
/// Corresponds to the C++ `MemoryTrackerBase<DeviceTracker>` template.
pub struct MemoryTrackerBase<DT: DeviceTracker> {
    /// Pool storage for word managers.
    manager_pool: Vec<Box<[Manager<DT>; MANAGER_POOL_SIZE]>>,
    /// Free-list indices: `(pool_index, slot_index)`.
    free_managers: VecDeque<(usize, usize)>,
    /// Top-tier mapping from higher-page index to pool location.
    /// `None` means no manager allocated yet.
    top_tier: Box<[Option<(usize, usize)>; NUM_HIGH_PAGES]>,
    /// Set of higher-page indices that have pending cached writes.
    cached_pages: HashSet<u32>,
    /// Pointer to the device tracker (rasterizer).
    device_tracker: *const DT,
    /// Lichii `ever_synchronized_pages` (memory_tracker_base.h:361): pages
    /// that ever left the CPU-modified state managers are created in.
    /// Shared with `for_each_upload_range`'s callback, which marks it while
    /// the managers are borrowed.
    ever_synchronized_pages: std::sync::Arc<crate::page_filter::PageFilter>,
}

unsafe impl<DT: DeviceTracker> Send for MemoryTrackerBase<DT> {}
unsafe impl<DT: DeviceTracker> Sync for MemoryTrackerBase<DT> {}

impl<DT: DeviceTracker> MemoryTrackerBase<DT> {
    /// Create a new memory tracker.
    pub fn new(device_tracker: &DT) -> Self {
        Self {
            manager_pool: Vec::new(),
            free_managers: VecDeque::new(),
            top_tier: vec![None; NUM_HIGH_PAGES]
                .into_boxed_slice()
                .try_into()
                .unwrap_or_else(|_| unreachable!("memory tracker top tier has its fixed size")),
            cached_pages: HashSet::new(),
            device_tracker: device_tracker as *const DT,
            ever_synchronized_pages: std::sync::Arc::new(crate::page_filter::PageFilter::new()),
        }
    }

    // -----------------------------------------------------------------------
    // Region queries
    // -----------------------------------------------------------------------

    /// Returns the inclusive CPU-modified range as a `(begin, end)` pair.
    pub fn modified_cpu_region(&mut self, query_cpu_addr: VAddr, query_size: u64) -> (u64, u64) {
        self.iterate_pairs::<true, _>(query_cpu_addr, query_size, |mgr, offset, size| {
            mgr.modified_region(Type::Cpu, offset, size)
        })
    }

    /// Returns the inclusive GPU-modified range as a `(begin, end)` pair.
    pub fn modified_gpu_region(&mut self, query_cpu_addr: VAddr, query_size: u64) -> (u64, u64) {
        self.iterate_pairs::<false, _>(query_cpu_addr, query_size, |mgr, offset, size| {
            mgr.modified_region(Type::Gpu, offset, size)
        })
    }

    /// Returns true if a region has been modified from the CPU.
    pub fn is_region_cpu_modified(&mut self, query_cpu_addr: VAddr, query_size: u64) -> bool {
        self.iterate_pages::<true, _>(query_cpu_addr, query_size, |mgr, offset, size| {
            IterateResult::Bool(mgr.is_region_modified(Type::Cpu, offset, size))
        })
    }

    /// Returns true if a region has been modified from the GPU.
    pub fn is_region_gpu_modified(&mut self, query_cpu_addr: VAddr, query_size: u64) -> bool {
        self.iterate_pages::<false, _>(query_cpu_addr, query_size, |mgr, offset, size| {
            IterateResult::Bool(mgr.is_region_modified(Type::Gpu, offset, size))
        })
    }

    /// Returns true if a region has been marked as preflushable.
    pub fn is_region_preflushable(&mut self, query_cpu_addr: VAddr, query_size: u64) -> bool {
        self.iterate_pages::<false, _>(query_cpu_addr, query_size, |mgr, offset, size| {
            IterateResult::Bool(mgr.is_region_modified(Type::Preflushable, offset, size))
        })
    }

    // -----------------------------------------------------------------------
    // Region mutations
    // -----------------------------------------------------------------------

    /// Mark region as CPU-modified, notifying the device tracker about this change.
    pub fn mark_region_as_cpu_modified(&mut self, dirty_cpu_addr: VAddr, query_size: u64) {
        self.iterate_pages::<true, _>(dirty_cpu_addr, query_size, |mgr, offset, size| {
            mgr.change_region_state(
                Type::Cpu,
                true,
                mgr.get_cpu_addr().wrapping_add(offset),
                size,
            );
            IterateResult::Void
        });
    }

    /// As `mark_region_as_cpu_modified`, for data the GPU's own engines wrote
    /// to guest memory (inline constant buffer data): the pages stay tracked,
    /// as no guest CPU write needs to be let through, and the next upload has
    /// nothing to track again.
    ///
    /// Lichii `MemoryTrackerBase::MarkRegionAsWrittenTracked`
    /// (memory_tracker_base.h:83-90), absent from Eden. Its
    /// `ForgetSynchronized` call belongs to the synchronized-range skips
    /// (`WatchSynchronized`), which ruzu does not have.
    pub fn mark_region_as_written_tracked(&mut self, dirty_cpu_addr: VAddr, query_size: u64) {
        self.iterate_pages::<true, _>(dirty_cpu_addr, query_size, |mgr, offset, size| {
            mgr.mark_written_tracked(mgr.get_cpu_addr().wrapping_add(offset), size);
            IterateResult::Void
        });
    }

    /// Unmark region as CPU-modified, notifying the device tracker.
    pub fn unmark_region_as_cpu_modified(&mut self, dirty_cpu_addr: VAddr, query_size: u64) {
        // Lichii UnmarkRegionAsCpuModified (memory_tracker_base.h:94).
        self.ever_synchronized_pages.mark(dirty_cpu_addr, query_size);
        self.iterate_pages::<true, _>(dirty_cpu_addr, query_size, |mgr, offset, size| {
            mgr.change_region_state(
                Type::Cpu,
                false,
                mgr.get_cpu_addr().wrapping_add(offset),
                size,
            );
            IterateResult::Void
        });
    }

    /// Mark region as modified from the host GPU.
    pub fn mark_region_as_gpu_modified(&mut self, dirty_cpu_addr: VAddr, query_size: u64) {
        // Lichii MarkRegionAsGpuModified (memory_tracker_base.h:102).
        self.ever_synchronized_pages.mark(dirty_cpu_addr, query_size);
        self.iterate_pages::<true, _>(dirty_cpu_addr, query_size, |mgr, offset, size| {
            mgr.change_region_state(
                Type::Gpu,
                true,
                mgr.get_cpu_addr().wrapping_add(offset),
                size,
            );
            IterateResult::Void
        });
    }

    /// Mark region as preflushable.
    pub fn mark_region_as_preflushable(&mut self, dirty_cpu_addr: VAddr, query_size: u64) {
        // Lichii MarkRegionAsPreflushable (memory_tracker_base.h:110).
        self.ever_synchronized_pages.mark(dirty_cpu_addr, query_size);
        self.iterate_pages::<true, _>(dirty_cpu_addr, query_size, |mgr, offset, size| {
            mgr.change_region_state(
                Type::Preflushable,
                true,
                mgr.get_cpu_addr().wrapping_add(offset),
                size,
            );
            IterateResult::Void
        });
    }

    /// Unmark region as GPU-modified.
    pub fn unmark_region_as_gpu_modified(&mut self, dirty_cpu_addr: VAddr, query_size: u64) {
        self.iterate_pages::<true, _>(dirty_cpu_addr, query_size, |mgr, offset, size| {
            mgr.change_region_state(
                Type::Gpu,
                false,
                mgr.get_cpu_addr().wrapping_add(offset),
                size,
            );
            IterateResult::Void
        });
    }

    /// Unmark region as preflushable.
    pub fn unmark_region_as_preflushable(&mut self, dirty_cpu_addr: VAddr, query_size: u64) {
        self.iterate_pages::<true, _>(dirty_cpu_addr, query_size, |mgr, offset, size| {
            mgr.change_region_state(
                Type::Preflushable,
                false,
                mgr.get_cpu_addr().wrapping_add(offset),
                size,
            );
            IterateResult::Void
        });
    }

    /// Mark region as modified from the CPU, but defer until `flush_cached_writes`.
    pub fn cached_cpu_write(&mut self, dirty_cpu_addr: VAddr, query_size: u64) {
        // We need to collect page indices first because iterate_pages borrows self mutably
        let mut page_indices = Vec::new();
        self.iterate_pages::<true, _>(dirty_cpu_addr, query_size, |mgr, offset, size| {
            let cpu_address = mgr.get_cpu_addr().wrapping_add(offset);
            mgr.change_region_state(Type::CachedCpu, true, cpu_address, size);
            page_indices.push((cpu_address >> HIGHER_PAGE_BITS) as u32);
            IterateResult::Void
        });
        for idx in page_indices {
            self.cached_pages.insert(idx);
        }
    }

    /// Flush cached CPU writes for a specific range.
    pub fn flush_cached_writes_range(&mut self, query_cpu_addr: VAddr, query_size: u64) {
        self.iterate_pages::<false, _>(query_cpu_addr, query_size, |mgr, _offset, _size| {
            mgr.flush_cached_writes();
            IterateResult::Void
        });
    }

    /// Flush all cached CPU writes.
    pub fn flush_cached_writes(&mut self) {
        let pages: Vec<u32> = self.cached_pages.drain().collect();
        for id in pages {
            if let Some(loc) = self.top_tier[id as usize] {
                self.manager_pool[loc.0][loc.1].flush_cached_writes();
            }
        }
    }

    /// Call `func` for each CPU-modified range and unmark those pages.
    pub fn for_each_upload_range<F>(
        &mut self,
        query_cpu_range: VAddr,
        query_size: u64,
        func: &mut F,
    ) where
        F: FnMut(VAddr, u64),
    {
        // Lichii ForEachUploadRange (memory_tracker_base.h:172-180): each
        // uploaded range is marked ever synchronized before `func` sees it.
        let ever_synchronized_pages = std::sync::Arc::clone(&self.ever_synchronized_pages);
        let mut upload = |address: VAddr, size: u64| {
            ever_synchronized_pages.mark(address, size);
            func(address, size);
        };
        self.iterate_pages::<true, _>(query_cpu_range, query_size, |mgr, offset, size| {
            mgr.for_each_modified_range(
                Type::Cpu,
                true,
                mgr.get_cpu_addr().wrapping_add(offset),
                size,
                &mut upload,
            );
            IterateResult::Void
        });
    }

    /// Whether a page of the range ever left the CPU-modified state (an
    /// upload or an unmark) or was marked GPU-modified or preflushable. Pages
    /// without any are CPU-modified as their managers were created. Any
    /// thread, without a lock.
    ///
    /// Lichii `MemoryTrackerBase::IsRegionEverSynchronized`
    /// (memory_tracker_base.h:228-233), absent from Eden.
    pub fn is_region_ever_synchronized(&self, query_cpu_addr: VAddr, query_size: u64) -> bool {
        self.ever_synchronized_pages.any(query_cpu_addr, query_size)
    }

    /// Lichii ForEachPreflushableRange: enumerate marked pages without clearing
    /// them or allocating managers for untouched address ranges.
    pub fn for_each_preflushable_range<F>(
        &mut self,
        query_cpu_range: VAddr,
        query_size: u64,
        func: &mut F,
    ) where
        F: FnMut(VAddr, u64),
    {
        self.iterate_pages::<false, _>(query_cpu_range, query_size, |mgr, offset, size| {
            mgr.for_each_modified_range(
                Type::Preflushable,
                false,
                mgr.get_cpu_addr().wrapping_add(offset),
                size,
                func,
            );
            IterateResult::Void
        });
    }

    /// Call `func` for each GPU-modified range, optionally clearing the bits.
    pub fn for_each_download_range<F>(
        &mut self,
        query_cpu_range: VAddr,
        query_size: u64,
        clear: bool,
        func: &mut F,
    ) where
        F: FnMut(VAddr, u64),
    {
        self.iterate_pages::<false, _>(query_cpu_range, query_size, |mgr, offset, size| {
            mgr.for_each_modified_range(
                Type::Gpu,
                clear,
                mgr.get_cpu_addr().wrapping_add(offset),
                size,
                func,
            );
            IterateResult::Void
        });
    }

    /// Call `func` for each GPU-modified range and clear the bits.
    pub fn for_each_download_range_and_clear<F>(
        &mut self,
        query_cpu_range: VAddr,
        query_size: u64,
        func: &mut F,
    ) where
        F: FnMut(VAddr, u64),
    {
        self.iterate_pages::<false, _>(query_cpu_range, query_size, |mgr, offset, size| {
            mgr.for_each_modified_range(
                Type::Gpu,
                true,
                mgr.get_cpu_addr().wrapping_add(offset),
                size,
                func,
            );
            IterateResult::Void
        });
    }

    // -----------------------------------------------------------------------
    // Private: page iteration
    // -----------------------------------------------------------------------

    /// Get a mutable reference to the manager at a given pool location.
    fn get_manager_mut(
        pool: &mut [Box<[Manager<DT>; MANAGER_POOL_SIZE]>],
        loc: (usize, usize),
    ) -> &mut Manager<DT> {
        &mut pool[loc.0][loc.1]
    }

    /// Iterate over higher pages in the given range, calling `func` for each
    /// existing (or newly created) manager.
    fn iterate_pages<const CREATE_ON_FAIL: bool, F>(
        &mut self,
        cpu_address: VAddr,
        size: u64,
        mut func: F,
    ) -> bool
    where
        F: FnMut(&mut Manager<DT>, u64, u64) -> IterateResult,
    {
        let mut remaining_size = size as usize;
        let mut page_index = (cpu_address >> HIGHER_PAGE_BITS) as usize;
        let mut page_offset = cpu_address & HIGHER_PAGE_MASK;

        while remaining_size > 0 {
            let copy_amount = ((HIGHER_PAGE_SIZE - page_offset) as usize).min(remaining_size);

            if page_index >= NUM_HIGH_PAGES {
                report_out_of_range(cpu_address, size, page_index);
                break;
            }

            if let Some(loc) = self.top_tier[page_index] {
                let mgr = Self::get_manager_mut(&mut self.manager_pool, loc);
                match func(mgr, page_offset, copy_amount as u64) {
                    IterateResult::Bool(true) => return true,
                    _ => {}
                }
            } else if CREATE_ON_FAIL {
                self.create_region(page_index);
                let loc = self.top_tier[page_index].unwrap();
                let mgr = Self::get_manager_mut(&mut self.manager_pool, loc);
                match func(mgr, page_offset, copy_amount as u64) {
                    IterateResult::Bool(true) => return true,
                    _ => {}
                }
            }

            page_index += 1;
            page_offset = 0;
            remaining_size -= copy_amount;
        }
        false
    }

    /// Iterate over higher pages, collecting `(begin, end)` pairs.
    fn iterate_pairs<const CREATE_ON_FAIL: bool, F>(
        &mut self,
        cpu_address: VAddr,
        size: u64,
        mut func: F,
    ) -> (u64, u64)
    where
        F: FnMut(&Manager<DT>, u64, u64) -> (u64, u64),
    {
        let mut remaining_size = size as usize;
        let mut page_index = (cpu_address >> HIGHER_PAGE_BITS) as usize;
        let mut page_offset = cpu_address & HIGHER_PAGE_MASK;
        let mut begin: u64 = u64::MAX;
        let mut end: u64 = 0;

        while remaining_size > 0 {
            let copy_amount = ((HIGHER_PAGE_SIZE - page_offset) as usize).min(remaining_size);

            if page_index >= NUM_HIGH_PAGES {
                report_out_of_range(cpu_address, size, page_index);
                break;
            }

            let mut execute = |mgr: &Manager<DT>, begin: &mut u64, end: &mut u64| {
                let (new_begin, new_end) = func(mgr, page_offset, copy_amount as u64);
                if new_begin != 0 || new_end != 0 {
                    let base_address = (page_index as u64) << HIGHER_PAGE_BITS;
                    *begin = (*begin).min(new_begin.wrapping_add(base_address));
                    *end = (*end).max(new_end.wrapping_add(base_address));
                }
            };

            if let Some(loc) = self.top_tier[page_index] {
                let mgr = Self::get_manager_mut(&mut self.manager_pool, loc);
                execute(mgr, &mut begin, &mut end);
            } else if CREATE_ON_FAIL {
                self.create_region(page_index);
                let loc = self.top_tier[page_index].unwrap();
                let mgr = Self::get_manager_mut(&mut self.manager_pool, loc);
                execute(mgr, &mut begin, &mut end);
            }

            page_index += 1;
            page_offset = 0;
            remaining_size -= copy_amount;
        }

        if begin < end {
            (begin, end)
        } else {
            (0, 0)
        }
    }

    /// Allocate a manager for the given higher-page index.
    fn create_region(&mut self, page_index: usize) {
        let base_cpu_addr = (page_index as u64) << HIGHER_PAGE_BITS;
        let loc = self.get_new_manager(base_cpu_addr);
        self.top_tier[page_index] = Some(loc);
    }

    /// Get a manager from the free-list or allocate a new pool batch.
    fn get_new_manager(&mut self, base_cpu_address: VAddr) -> (usize, usize) {
        if let Some(loc) = self.free_managers.pop_front() {
            let mgr = &mut self.manager_pool[loc.0][loc.1];
            mgr.set_cpu_address(base_cpu_address);
            return loc;
        }

        // Allocate a new pool batch
        let pool_index = self.manager_pool.len();
        let tracker = unsafe { &*self.device_tracker };
        let mut batch = Vec::with_capacity(MANAGER_POOL_SIZE);
        for _ in 0..MANAGER_POOL_SIZE {
            batch.push(Manager::new(0, tracker));
        }
        let batch = batch
            .into_boxed_slice()
            .try_into()
            .unwrap_or_else(|_| unreachable!("manager pool batch has its fixed size"));
        self.manager_pool.push(batch);

        // Eden appends every manager in slot order and then consumes the FIFO front.
        for i in 0..MANAGER_POOL_SIZE {
            self.free_managers.push_back((pool_index, i));
        }

        let loc = self
            .free_managers
            .pop_front()
            .expect("new manager pool must populate the free list");
        let mgr = &mut self.manager_pool[loc.0][loc.1];
        mgr.set_cpu_address(base_cpu_address);
        loc
    }
}

// ---------------------------------------------------------------------------
// IterateResult — helper for the bool-break pattern
// ---------------------------------------------------------------------------

/// Return type from iterate callbacks, matching the C++ bool-break / void pattern.
enum IterateResult {
    Void,
    Bool(bool),
}

#[cfg(test)]
mod tests {
    use super::super::word_manager::DeviceTracker;
    use super::*;

    struct DummyTracker;
    impl DeviceTracker for DummyTracker {
        fn update_pages_cached_batch(&self, _ranges: &[(VAddr, usize)], _delta: i32) {}
    }

    #[test]
    fn written_tracked_pages_upload_again_without_changing_their_tracking() {
        struct CountingTracker(std::sync::Mutex<Vec<(VAddr, usize, i32)>>);
        impl DeviceTracker for CountingTracker {
            fn update_pages_cached_batch(&self, ranges: &[(VAddr, usize)], delta: i32) {
                self.0
                    .lock()
                    .unwrap()
                    .extend(ranges.iter().map(|&(addr, size)| (addr, size, delta)));
            }
        }
        let tracker = CountingTracker(std::sync::Mutex::new(Vec::new()));
        let mut memory = MemoryTrackerBase::new(&tracker);
        // The first upload tracks the pages.
        memory.for_each_upload_range(0x1_0000, 0x2000, &mut |_, _| {});
        assert!(!tracker.0.lock().unwrap().is_empty());
        assert!(!memory.is_region_cpu_modified(0x1_0000, 0x2000));
        tracker.0.lock().unwrap().clear();

        // An engine write: CPU-modified for the next upload, still tracked.
        memory.mark_region_as_written_tracked(0x1_0100, 0x10);
        assert!(memory.is_region_cpu_modified(0x1_0000, 0x1000));
        assert!(!memory.is_region_cpu_modified(0x1_1000, 0x1000));
        let mut uploads = Vec::new();
        memory.for_each_upload_range(0x1_0000, 0x2000, &mut |addr, size| uploads.push((addr, size)));
        assert_eq!(uploads, [(0x1_0000, 0x1000)]);
        let changes = tracker.0.lock().unwrap().clone();
        assert!(changes.is_empty(), "{changes:?}");

        // A CPU mark untracks the page and the upload tracks it again.
        memory.mark_region_as_cpu_modified(0x1_0100, 0x10);
        memory.for_each_upload_range(0x1_0000, 0x2000, &mut |_, _| {});
        let changes = tracker.0.lock().unwrap().clone();
        assert_eq!(changes, [(0x1_0000, 0x1000, -1), (0x1_0000, 0x1000, 1)]);
    }

    #[test]
    fn ever_synchronized_pages_follow_every_way_out_of_the_cpu_modified_state() {
        let tracker = DummyTracker;
        let mut memory = MemoryTrackerBase::new(&tracker);
        assert!(!memory.is_region_ever_synchronized(0, HIGHER_PAGE_SIZE * 2));
        // A CPU mark keeps pages as created: CPU-modified, nothing to change.
        memory.mark_region_as_cpu_modified(0x1_0000, 0x1000);
        assert!(!memory.is_region_ever_synchronized(0x1_0000, 0x1000));
        // An upload marks only the ranges it uploaded.
        memory.for_each_upload_range(0x2_0000, 0x2000, &mut |_, _| {});
        assert!(memory.is_region_ever_synchronized(0x2_0000, 4));
        assert!(memory.is_region_ever_synchronized(0x2_1ffc, 4));
        assert!(!memory.is_region_ever_synchronized(0x2_2000, 0x1000));
        // A second upload over clean pages finds nothing and changes nothing.
        let mut uploads = 0;
        memory.for_each_upload_range(0x2_0000, 0x2000, &mut |_, _| uploads += 1);
        assert_eq!(uploads, 0);
        // An unmark, a GPU mark and a preflushable mark each count.
        memory.unmark_region_as_cpu_modified(0x3_0000, 0x10);
        memory.mark_region_as_gpu_modified(0x4_0000, 0x10);
        memory.mark_region_as_preflushable(0x5_0000, 0x10);
        for page in [0x3_0000u64, 0x4_0000, 0x5_0000] {
            assert!(memory.is_region_ever_synchronized(page + 0xff0, 0x10), "{page:#x}");
            assert!(!memory.is_region_ever_synchronized(page + 0x1000, 0x10), "{page:#x}");
        }
        // Marking CPU-modified again does not forget a page.
        memory.mark_region_as_cpu_modified(0x2_0000, 0x2000);
        assert!(memory.is_region_ever_synchronized(0x2_0000, 0x2000));
    }

    #[test]
    fn preflushable_iteration_preserves_marks_and_skips_absent_managers() {
        let tracker = DummyTracker;
        let mut memory = MemoryTrackerBase::new(&tracker);
        let pools = memory.manager_pool.len();
        let free = memory.free_managers.len();
        memory.for_each_preflushable_range(0, HIGHER_PAGE_SIZE * 3, &mut |_, _| {
            panic!("untouched memory has no preflushable pages")
        });
        assert_eq!(memory.manager_pool.len(), pools);
        assert_eq!(memory.free_managers.len(), free);
        let boundary = HIGHER_PAGE_SIZE;
        memory.mark_region_as_preflushable(boundary - 0x1000, 0x2000);
        memory.unmark_region_as_cpu_modified(boundary + 0x3000, 0x1000);
        memory.mark_region_as_gpu_modified(boundary + 0x3000, 0x1000);
        for _ in 0..2 {
            let mut ranges = Vec::new();
            memory.for_each_preflushable_range(boundary - 0x2000, 0x6000,
                &mut |addr, size| ranges.push((addr, size)));
            assert_eq!(ranges, [(boundary - 0x1000, 0x1000), (boundary, 0x1000)]);
        }
        memory.unmark_region_as_preflushable(boundary, 0x1000);
        let mut ranges = Vec::new();
        memory.for_each_preflushable_range(boundary - 0x1000, 0x2000,
            &mut |addr, size| ranges.push((addr, size)));
        assert_eq!(ranges, [(boundary - 0x1000, 0x1000)]);
        assert!(memory.is_region_gpu_modified(boundary + 0x3000, 0x1000));
    }

    #[test]
    fn test_create_and_query() {
        let tracker = DummyTracker;
        let mut mem_tracker = MemoryTrackerBase::new(&tracker);

        // Initially, CPU-modified should be true for any region
        // (because cpu words are initialized to all-ones)
        assert!(mem_tracker.is_region_cpu_modified(0x1000, 0x1000));
        assert_eq!(mem_tracker.top_tier.len(), NUM_HIGH_PAGES);
        assert_eq!(mem_tracker.manager_pool.len(), 1);
        assert_eq!(mem_tracker.manager_pool[0].len(), MANAGER_POOL_SIZE);
        assert_eq!(mem_tracker.free_managers.front(), Some(&(0, 1)));

        // GPU-modified should be false initially
        assert!(!mem_tracker.is_region_gpu_modified(0x1000, 0x1000));
    }

    /// Regression: a device address beyond the tracked address space
    /// (`>= NUM_HIGH_PAGES * HIGHER_PAGE_SIZE`, i.e. above `SMMU_VA_LIMIT`)
    /// retains the Rust bounds guard against undefined upstream indexing.
    #[test]
    fn test_out_of_range_address_does_not_panic() {
        let tracker = DummyTracker;
        let mut mem_tracker = MemoryTrackerBase::new(&tracker);

        let oor_addr: VAddr = (NUM_HIGH_PAGES as u64 + 1689) << HIGHER_PAGE_BITS;

        // iterate_pairs path (read-only query): must return "not modified",
        // not panic.
        assert!(!mem_tracker.is_region_gpu_modified(oor_addr, 0x1000));
        assert!(!mem_tracker.is_region_cpu_modified(oor_addr, 0x1000));

        // iterate_pages path (download): func must simply never be called for
        // the out-of-range range, and the call must not panic.
        let mut hit = 0u32;
        mem_tracker.for_each_download_range_and_clear(oor_addr, 0x4000, &mut |_, _| {
            hit += 1;
        });
        assert_eq!(hit, 0);

        // A range that STRADDLES the boundary must still process its in-range
        // portion without panicking on the overflow pages.
        let near_limit: VAddr = ((NUM_HIGH_PAGES as u64) - 1) << HIGHER_PAGE_BITS;
        assert!(!mem_tracker.is_region_gpu_modified(near_limit, HIGHER_PAGE_SIZE * 4));
    }

    #[test]
    fn nx2_device_addresses_above_16_gib_preserve_upload_and_download_tracking() {
        assert_eq!(MAX_CPU_PAGE_BITS, 36);
        assert_eq!(NUM_HIGH_PAGES, 16384);
        let tracker = DummyTracker;
        let mut mem_tracker = MemoryTrackerBase::new(&tracker);
        for (address, size) in [
            // The real game's 0x431262b00 lies within this region.
            (0x431260000, 0x10000),
            ((1u64 << 34) - 0x1000, 0x2000),
            ((1u64 << 36) - 0x1000, 0x1000),
        ] {
            assert!(mem_tracker.is_region_cpu_modified(address, size));
            mem_tracker.unmark_region_as_cpu_modified(address, size);
            assert!(!mem_tracker.is_region_cpu_modified(address, size));
            mem_tracker.mark_region_as_gpu_modified(address, size);
            assert!(mem_tracker.is_region_gpu_modified(address, size));
            let mut downloaded = 0;
            mem_tracker.for_each_download_range_and_clear(address, size, &mut |start, length| {
                assert!(start >= address && start + length <= address + size);
                downloaded += length;
            });
            assert_eq!(downloaded, size);
            assert!(!mem_tracker.is_region_gpu_modified(address, size));
            mem_tracker.mark_region_as_cpu_modified(address, size);
            assert!(mem_tracker.is_region_cpu_modified(address, size));
        }
    }
}
