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

/// Maximum CPU address bits tracked (34 bits = 16 GiB).
const MAX_CPU_PAGE_BITS: usize = 34;

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
/// index (silent UB if the device address exceeds the 16 GiB device address
/// space). ruzu's bounds-checked fixed array would panic instead, so we detect
/// and log the offending (address, size) to surface the real bug — a device
/// address above `SMMU_VA_LIMIT` (2^34) must never reach the buffer cache.
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

    /// Unmark region as CPU-modified, notifying the device tracker.
    pub fn unmark_region_as_cpu_modified(&mut self, dirty_cpu_addr: VAddr, query_size: u64) {
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
        self.iterate_pages::<true, _>(query_cpu_range, query_size, |mgr, offset, size| {
            mgr.for_each_modified_range(
                Type::Cpu,
                true,
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
    /// must not panic the GPU thread. Upstream indexes a fixed `std::array`
    /// (silent UB); ruzu's bounds-checked `Vec` would panic, so the iteration
    /// device address ~24 GiB reached `for_each_download_range_and_clear`.
    #[test]
    fn test_out_of_range_address_does_not_panic() {
        let tracker = DummyTracker;
        let mut mem_tracker = MemoryTrackerBase::new(&tracker);

        // page_index = oor_addr >> HIGHER_PAGE_BITS = NUM_HIGH_PAGES + 1689,
        // which previously indexed past the 4096-entry top tier and panicked.
        let oor_addr: VAddr = (5785u64) << HIGHER_PAGE_BITS;

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
}
