//! Port of zuyu/src/core/hle/kernel/k_memory_manager.h and k_memory_manager.cpp
//! Status: Ported — Impl managers with page heap allocation and ref counting.
//! Derniere synchro: 2026-03-19
//!
//! The kernel physical memory manager. Manages page heaps for each memory pool.
//!
//! Upstream owns an array of `Impl` managers (one per physical memory region),
//! linked into per-pool chains. Each Impl wraps a KPageHeap for block allocation
//! and maintains per-page reference counts.

use super::k_memory_block::PAGE_SIZE;
use super::k_page_heap::KPageHeap;
use super::svc::svc_results;
use std::sync::Mutex;

// ---------------------------------------------------------------------------
// Pool
// ---------------------------------------------------------------------------

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pool {
    Application = 0,
    Applet = 1,
    System = 2,
    SystemNonSecure = 3,
    Count = 4,
}

impl Pool {
    pub const SHIFT: u32 = 4;
    pub const MASK: u32 = 0xF << Self::SHIFT;

    /// Alias: Secure == System (upstream `Pool::Secure = Pool::System`).
    pub const SECURE: Pool = Pool::System;
}

// ---------------------------------------------------------------------------
// Direction
// ---------------------------------------------------------------------------

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    FromFront = 0,
    FromBack = 1,
}

impl Direction {
    pub const SHIFT: u32 = 0;
    pub const MASK: u32 = 0xF << Self::SHIFT;
}

// ---------------------------------------------------------------------------
// Impl — per-region page heap manager
// ---------------------------------------------------------------------------

/// Port of Kernel::KMemoryManager::Impl.
///
/// Each Impl manages a contiguous physical memory region with a KPageHeap
/// for block allocation and per-page reference counts.
struct Impl {
    m_heap: KPageHeap,
    m_page_reference_counts: Vec<u16>,
    m_pool: Pool,
    /// Index of the next Impl in the same pool's chain (None == tail).
    /// Upstream: `Impl* m_next` linked-list pointer.
    m_next_idx: Option<usize>,
    /// Index of the previous Impl in the same pool's chain (None == head).
    /// Upstream: `Impl* m_prev` linked-list pointer.
    m_prev_idx: Option<usize>,
}

impl Impl {
    fn new() -> Self {
        Self {
            m_heap: KPageHeap::new(),
            m_page_reference_counts: Vec::new(),
            m_pool: Pool::Application,
            m_next_idx: None,
            m_prev_idx: None,
        }
    }

    fn get_next(&self) -> Option<usize> {
        self.m_next_idx
    }
    fn get_prev(&self) -> Option<usize> {
        self.m_prev_idx
    }
    fn set_next(&mut self, idx: Option<usize>) {
        self.m_next_idx = idx;
    }
    fn set_prev(&mut self, idx: Option<usize>) {
        self.m_prev_idx = idx;
    }

    /// Initialize the manager for a physical memory region.
    ///
    /// Upstream `Impl::Initialize` takes management region pointers for metadata
    /// storage. We simplify by heap-allocating the ref count Vec and letting
    /// KPageHeap manage its own bitmap storage.
    fn initialize(&mut self, address: u64, size: usize, pool: Pool) {
        self.m_pool = pool;

        // Initialize the page heap for this region.
        self.m_heap.initialize(address, size);

        // Allocate reference counts (one u16 per page).
        let num_pages = size / PAGE_SIZE;
        self.m_page_reference_counts = vec![0u16; num_pages];

        // Free the entire region to the heap so it's available for allocation.
        self.m_heap.free(address, num_pages);

        log::debug!(
            "KMemoryManager::Impl::initialize: pool={:?}, addr={:#x}, size={:#x} ({} pages)",
            pool,
            address,
            size,
            num_pages
        );
    }

    fn get_pool(&self) -> Pool {
        self.m_pool
    }

    fn get_size(&self) -> usize {
        self.m_heap.get_size()
    }

    fn get_free_size(&self) -> usize {
        self.m_heap.get_free_size()
    }

    fn get_end_address(&self) -> u64 {
        self.m_heap.get_end_address()
    }

    fn get_page_offset(&self, address: u64) -> usize {
        self.m_heap.get_page_offset(address)
    }

    fn get_page_offset_to_end(&self, address: u64) -> usize {
        self.m_heap.get_page_offset_to_end(address)
    }

    /// Allocate aligned pages from the heap.
    /// Upstream: `Impl::AllocateAligned`.
    fn allocate_aligned(&mut self, heap_index: i32, num_pages: usize, align_pages: usize) -> u64 {
        self.m_heap
            .allocate_aligned(heap_index, num_pages, align_pages)
    }

    /// Free pages back to the heap.
    fn free(&mut self, addr: u64, num_pages: usize) {
        self.m_heap.free(addr, num_pages);
    }

    /// Set reference counts to 1 for first open.
    /// Upstream: `Impl::OpenFirst`.
    fn open_first(&mut self, address: u64, num_pages: usize) {
        let mut index = self.get_page_offset(address);
        let end = index + num_pages;
        while index < end {
            self.m_page_reference_counts[index] += 1;
            debug_assert_eq!(
                self.m_page_reference_counts[index], 1,
                "OpenFirst: ref count must be 1 at index {}",
                index
            );
            index += 1;
        }
    }

    /// Increment reference counts (must already be >= 1).
    /// Upstream: `Impl::Open`.
    fn open(&mut self, address: u64, num_pages: usize) {
        let mut index = self.get_page_offset(address);
        let end = index + num_pages;
        while index < end {
            self.m_page_reference_counts[index] += 1;
            debug_assert!(
                self.m_page_reference_counts[index] > 1,
                "Open: ref count must be > 1 at index {}",
                index
            );
            index += 1;
        }
    }

    /// Decrement reference counts; free pages when count reaches 0.
    /// Upstream: `Impl::Close`.
    fn close(&mut self, address: u64, num_pages: usize) {
        let mut index = self.get_page_offset(address);
        let end = index + num_pages;

        let mut free_start = 0usize;
        let mut free_count = 0usize;
        while index < end {
            debug_assert!(
                self.m_page_reference_counts[index] > 0,
                "Close: ref count must be > 0 at index {}",
                index
            );
            self.m_page_reference_counts[index] -= 1;
            let ref_count = self.m_page_reference_counts[index];

            if ref_count == 0 {
                if free_count > 0 {
                    free_count += 1;
                } else {
                    free_start = index;
                    free_count = 1;
                }
            } else {
                if free_count > 0 {
                    self.free(
                        self.m_heap.get_address() + (free_start as u64) * PAGE_SIZE as u64,
                        free_count,
                    );
                    free_count = 0;
                }
            }

            index += 1;
        }

        if free_count > 0 {
            self.free(
                self.m_heap.get_address() + (free_start as u64) * PAGE_SIZE as u64,
                free_count,
            );
        }
    }

    /// Check if this manager contains the given address.
    fn contains(&self, address: u64) -> bool {
        address >= self.m_heap.get_address() && address < self.get_end_address()
    }
}

// ---------------------------------------------------------------------------
// KMemoryManager
// ---------------------------------------------------------------------------

pub const MAX_MANAGER_COUNT: usize = 10;
const POOL_COUNT: usize = Pool::Count as usize;

/// Port of Kernel::KMemoryManager.
///
/// Upstream holds an array of `Impl` page-heap managers linked into per-pool
/// chains. We use a Vec of Impl managers with per-pool head indices.
pub struct KMemoryManager {
    m_managers: Vec<Impl>,
    m_num_managers: usize,
    /// Head index into m_managers for each pool. Upstream:
    /// `std::array<Impl*, MaxManagerCount> m_pool_managers_head`.
    m_pool_managers_head: [Option<usize>; POOL_COUNT],
    /// Tail index into m_managers for each pool. Upstream:
    /// `std::array<Impl*, MaxManagerCount> m_pool_managers_tail`.
    m_pool_managers_tail: [Option<usize>; POOL_COUNT],
    /// Per-pool optimized process tracking.
    m_optimized_process_ids: [u64; POOL_COUNT],
    m_has_optimized_process: [bool; POOL_COUNT],
    /// Per-pool lock. Upstream: `PoolArray<KLightLock>`.
    m_pool_locks: [Mutex<()>; POOL_COUNT],
}

impl KMemoryManager {
    pub fn new() -> Self {
        Self {
            m_managers: Vec::new(),
            m_num_managers: 0,
            m_pool_managers_head: [None; POOL_COUNT],
            m_pool_managers_tail: [None; POOL_COUNT],
            m_optimized_process_ids: [0; POOL_COUNT],
            m_has_optimized_process: [false; POOL_COUNT],
            m_pool_locks: [
                Mutex::new(()),
                Mutex::new(()),
                Mutex::new(()),
                Mutex::new(()),
            ],
        }
    }

    /// Initialize a pool with a physical memory region.
    ///
    /// Simplified version of upstream's `Initialize()` which traverses the
    /// memory layout tree. Here the caller provides the region directly.
    /// This creates one Impl manager for the pool and makes it available
    /// for allocation.
    pub fn initialize_pool(&mut self, pool: Pool, phys_start: u64, size: usize) {
        assert!(
            self.m_num_managers < MAX_MANAGER_COUNT,
            "Too many memory managers"
        );

        let mut manager = Impl::new();
        manager.initialize(phys_start, size, pool);
        let manager_index = self.m_num_managers;
        self.m_managers.push(manager);
        self.m_num_managers += 1;

        // Insert into the pool's chain. Upstream
        // (k_memory_manager.cpp:111):
        //
        //   if (m_pool_managers_tail[i] == nullptr) {
        //       m_pool_managers_head[i] = manager;
        //   } else {
        //       m_pool_managers_tail[i]->SetNext(manager);
        //       manager->SetPrev(m_pool_managers_tail[i]);
        //   }
        //   m_pool_managers_tail[i] = manager;
        let pool_index = pool as usize;
        match self.m_pool_managers_tail[pool_index] {
            None => {
                self.m_pool_managers_head[pool_index] = Some(manager_index);
            }
            Some(tail_idx) => {
                self.m_managers[tail_idx].set_next(Some(manager_index));
                self.m_managers[manager_index].set_prev(Some(tail_idx));
            }
        }
        self.m_pool_managers_tail[pool_index] = Some(manager_index);
        log::info!(
            "KMemoryManager: initialized pool {:?} at {:#x}..{:#x} ({:#x} bytes)",
            pool,
            phys_start,
            phys_start + size as u64,
            size
        );
    }

    /// Get the first Impl in the pool's chain for the given direction.
    /// Upstream `KMemoryManager::GetFirstManager`:
    ///
    /// ```cpp
    /// return dir == Direction::FromBack ? m_pool_managers_tail[pool]
    ///                                   : m_pool_managers_head[pool];
    /// ```
    fn get_first_manager(&self, pool: Pool, dir: Direction) -> Option<usize> {
        let pool_index = pool as usize;
        match dir {
            Direction::FromBack => self.m_pool_managers_tail[pool_index],
            Direction::FromFront => self.m_pool_managers_head[pool_index],
        }
    }

    /// Get the next Impl in the pool's chain for the given direction.
    /// Upstream `KMemoryManager::GetNextManager`:
    ///
    /// ```cpp
    /// return dir == Direction::FromBack ? cur->GetPrev() : cur->GetNext();
    /// ```
    fn get_next_manager(&self, cur: usize, dir: Direction) -> Option<usize> {
        match dir {
            Direction::FromBack => self.m_managers[cur].get_prev(),
            Direction::FromFront => self.m_managers[cur].get_next(),
        }
    }

    /// Initialize all pools by walking `layout`'s physical memory region
    /// tree for `DramUserPool`-derived entries, creating one Impl per
    /// region.
    ///
    /// Port of upstream `KMemoryManager::Initialize` (k_memory_manager.cpp:81+):
    /// ```cpp
    /// for (const auto& it : kernel.MemoryLayout().GetPhysicalMemoryRegionTree()) {
    ///     if (it.IsDerivedFrom(KMemoryRegionType_DramUserPool)) {
    ///         auto& manager = m_managers[it.GetAttributes()];
    ///         manager.Initialize(...);
    ///         // Insert into m_pool_managers_head/tail per region.GetType().
    ///     }
    /// }
    /// ```
    pub fn initialize_from_layout(&mut self, layout: &super::k_memory_layout::KMemoryLayout) {
        use super::k_memory_region_type::*;
        self.m_num_managers = 0;
        self.m_managers.clear();
        self.m_pool_managers_head.fill(None);
        self.m_pool_managers_tail.fill(None);
        while self.m_num_managers < MAX_MANAGER_COUNT {
            let mut combined: Option<(u64, usize, Pool)> = None;
            for region in layout.get_physical_memory_region_tree().iter() {
                if !region.is_derived_from(K_MEMORY_REGION_TYPE_DRAM_USER_POOL)
                    || region.get_attributes() as usize != self.m_num_managers {
                    continue;
                }
                assert_ne!(region.get_address(), 0);
                assert_ne!(region.get_end_address(), 0);
                assert!(region.get_size() > 0);
                let pool = if region.is_derived_from(K_MEMORY_REGION_TYPE_DRAM_APPLICATION_POOL) {
                    Pool::Application
                } else if region.is_derived_from(K_MEMORY_REGION_TYPE_DRAM_APPLET_POOL) {
                    Pool::Applet
                } else if region.is_derived_from(K_MEMORY_REGION_TYPE_DRAM_SYSTEM_NON_SECURE_POOL) {
                    Pool::SystemNonSecure
                } else if region.is_derived_from(K_MEMORY_REGION_TYPE_DRAM_SYSTEM_POOL) {
                    Pool::System
                } else {
                    continue;
                };
                match &mut combined {
                    Some((address, size, previous_pool)) => {
                        assert_eq!(region.get_address(), *address + *size as u64);
                        assert_eq!(pool, *previous_pool);
                        *size = (region.get_end_address() - *address) as usize;
                    }
                    None => combined = Some((region.get_address(), region.get_size(), pool)),
                }
            }
            let Some((address, size, pool)) = combined else { break; };
            self.initialize_pool(pool, address, size);
        }
    }

    // --- Allocation ---

    /// Allocate contiguous physical pages and open the first reference.
    ///
    /// Upstream: `KPhysicalAddress AllocateAndOpenContinuous(size_t num_pages,
    ///     size_t align_pages, u32 option)`.
    pub fn allocate_and_open_continuous(
        &mut self,
        num_pages: usize,
        align_pages: usize,
        option: u32,
    ) -> u64 {
        if num_pages == 0 {
            return 0;
        }

        let (pool, dir) = Self::decode_option(option);
        let pool_index = pool as usize;
        let _lk = self.m_pool_locks[pool_index].lock().unwrap();

        let heap_index = KPageHeap::get_aligned_block_index(num_pages, align_pages);

        // Walk the pool's Impl chain in `dir` order. Upstream
        // (k_memory_manager.cpp:222):
        //
        //   for (chosen_manager = GetFirstManager(pool, dir);
        //        chosen_manager != nullptr;
        //        chosen_manager = GetNextManager(chosen_manager, dir)) {
        //       allocated_block = chosen_manager->AllocateAligned(...);
        //       if (allocated_block != 0) break;
        //   }
        let mut allocated_block: u64 = 0;
        let mut chosen_idx: Option<usize> = None;
        let mut cur_idx = self.get_first_manager(pool, dir);
        while let Some(idx) = cur_idx {
            allocated_block =
                self.m_managers[idx].allocate_aligned(heap_index, num_pages, align_pages);
            if allocated_block != 0 {
                chosen_idx = Some(idx);
                break;
            }
            cur_idx = self.get_next_manager(idx, dir);
        }

        if allocated_block == 0 {
            log::error!(
                "AllocateAndOpenContinuous: allocation failed for {} pages in pool {:?}",
                num_pages,
                pool
            );
            return 0;
        }

        // Open first reference on the manager that actually allocated the
        // block (upstream uses the chosen_manager pointer captured during
        // the walk).
        if let Some(idx) = chosen_idx {
            self.m_managers[idx].open_first(allocated_block, num_pages);
        }

        log::debug!(
            "AllocateAndOpenContinuous: allocated {} pages at {:#x} from pool {:?}",
            num_pages,
            allocated_block,
            pool
        );

        allocated_block
    }

    /// Allocate `num_pages` from the pool as a sequence of buddy blocks,
    /// appending each to `out_pg` and opening-first the reference count of
    /// every page in the group.
    ///
    /// Port of upstream `KMemoryManager::AllocateAndOpen` (k_memory_manager.cpp:301).
    /// Calls the shared `AllocatePageGroupImpl` (largest-fit walk over the
    /// shift table), then matches upstream's per-block `OpenFirst` loop —
    /// each block is processed as a contiguous run within its owning Impl.
    pub fn allocate_and_open(
        &mut self,
        out_pg: &mut super::k_page_group::KPageGroup,
        num_pages: usize,
        option: u32,
    ) -> u32 {
        debug_assert!(out_pg.is_empty(), "out page group must be empty");
        if num_pages == 0 {
            return 0;
        }

        let (pool, dir) = Self::decode_option(option);
        let pool_index = pool as usize;
        let _lk = self.m_pool_locks[pool_index].lock().unwrap();

        // Allocate the page group. Use a free function to side-step the
        // borrow conflict between `_lk` (immutable borrow of m_pool_locks)
        // and `&mut self`-style method calls.
        let unoptimized = self.m_has_optimized_process[pool_index];
        let pool_first = self.get_first_manager(pool, dir);
        let rc = Self::allocate_page_group_impl(
            &mut self.m_managers,
            pool_first,
            dir,
            out_pg,
            num_pages,
            unoptimized,
            true,
        );
        if rc != 0 {
            return rc;
        }

        // Open-first each block, splitting per managing Impl per upstream.
        let blocks: Vec<(u64, usize)> = out_pg
            .iter()
            .map(|b| (b.get_address(), b.get_num_pages()))
            .collect();
        for (cur_address, mut remaining_pages) in blocks {
            let mut cur = cur_address;
            while remaining_pages > 0 {
                let idx = Self::find_manager_index(&self.m_managers, cur);
                let manager = &mut self.m_managers[idx];
                let to_end = manager.get_page_offset_to_end(cur);
                let cur_pages = remaining_pages.min(to_end);
                manager.open_first(cur, cur_pages);
                cur += (cur_pages * PAGE_SIZE) as u64;
                remaining_pages -= cur_pages;
            }
        }
        0
    }

    /// Allocate pages for a process without opening page references.
    ///
    /// Port of upstream `KMemoryManager::AllocateForProcess`. The caller is
    /// responsible for opening the first references when pages become owned by
    /// a mapping, and for releasing unowned allocated pages on failure.
    pub fn allocate_for_process(
        &mut self,
        out_pg: &mut super::k_page_group::KPageGroup,
        num_pages: usize,
        option: u32,
        process_id: u64,
    ) -> u32 {
        debug_assert!(out_pg.is_empty(), "out page group must be empty");
        if num_pages == 0 {
            return 0;
        }

        let (pool, dir) = Self::decode_option(option);
        let pool_index = pool as usize;
        let _lk = self.m_pool_locks[pool_index].lock().unwrap();

        let has_optimized = self.m_has_optimized_process[pool_index];
        let is_optimized = self.m_optimized_process_ids[pool_index] == process_id;
        let pool_first = self.get_first_manager(pool, dir);
        Self::allocate_page_group_impl(
            &mut self.m_managers,
            pool_first,
            dir,
            out_pg,
            num_pages,
            has_optimized && !is_optimized,
            false,
        )
    }

    /// Allocate a `KPageGroup` for `num_pages` from the pool whose head Impl
    /// is `pool_head`, using the buddy heap's largest-block-first strategy.
    ///
    /// Port of upstream `KMemoryManager::AllocatePageGroupImpl`
    /// (k_memory_manager.cpp:246). Walks block sizes from largest-fit down
    /// to 4 KB, allocating multiples of each from each Impl in the pool's
    /// chain. On failure, frees any blocks already added back to the pool
    /// and finalizes the group.
    ///
    /// Free function so that callers can hold the pool lock as an immutable
    /// borrow on `self.m_pool_locks` while passing `&mut self.m_managers`
    /// here — Rust's borrow checker cannot prove disjointness across method
    /// calls on `&mut self`.
    fn allocate_page_group_impl(
        managers: &mut [Impl],
        pool_first: Option<usize>,
        dir: Direction,
        out_pg: &mut super::k_page_group::KPageGroup,
        num_pages: usize,
        _unoptimized: bool,
        random: bool,
    ) -> u32 {
        let heap_index = KPageHeap::get_block_index(num_pages);
        if heap_index < 0 {
            return svc_results::RESULT_OUT_OF_MEMORY.get_inner_value();
        }

        let mut remaining = num_pages;

        for index in (0..=heap_index).rev() {
            if remaining == 0 {
                break;
            }
            let pages_per_alloc = KPageHeap::get_block_num_pages(index as usize);

            // Walk the pool's Impl chain in `dir` order. Upstream
            // (k_memory_manager.cpp:266):
            //   for (Impl* cur_manager = GetFirstManager(pool, dir);
            //        cur_manager != nullptr;
            //        cur_manager = GetNextManager(cur_manager, dir))
            let mut cur_idx = pool_first;
            while let Some(idx) = cur_idx {
                while remaining >= pages_per_alloc {
                    let allocated_block = managers[idx].m_heap.allocate_block(index, random);
                    if allocated_block == 0 {
                        break;
                    }
                    if out_pg.add_block(allocated_block, pages_per_alloc).is_err() {
                        managers[idx].free(allocated_block, pages_per_alloc);
                        Self::free_page_group_blocks(managers, out_pg);
                        out_pg.finalize();
                        return svc_results::RESULT_OUT_OF_MEMORY.get_inner_value();
                    }
                    remaining -= pages_per_alloc;
                }
                cur_idx = match dir {
                    Direction::FromBack => managers[idx].get_prev(),
                    Direction::FromFront => managers[idx].get_next(),
                };
            }
        }

        if remaining != 0 {
            Self::free_page_group_blocks(managers, out_pg);
            out_pg.finalize();
            return svc_results::RESULT_OUT_OF_MEMORY.get_inner_value();
        }
        0
    }

    /// Free every block currently in `pg` back to its owning Impl. Upstream
    /// uses an `ON_RESULT_FAILURE` lambda that does the same walk.
    fn free_page_group_blocks(managers: &mut [Impl], pg: &super::k_page_group::KPageGroup) {
        let blocks: Vec<(u64, usize)> = pg
            .iter()
            .map(|b| (b.get_address(), b.get_num_pages()))
            .collect();
        for (addr, pages) in blocks {
            let idx = Self::find_manager_index(managers, addr);
            let manager = &mut managers[idx];
            let to_end = manager.get_page_offset_to_end(addr);
            manager.free(addr, pages.min(to_end));
        }
    }

    /// Find the manager index containing the given physical address.
    fn find_manager_index(managers: &[Impl], address: u64) -> usize {
        for (i, m) in managers.iter().enumerate() {
            if m.contains(address) {
                return i;
            }
        }
        panic!("KMemoryManager: no manager for address {:#x}", address);
    }

    /// Open first references for pages at the given address.
    /// Upstream: `void OpenFirst(KPhysicalAddress, size_t)`.
    pub fn open_first(&mut self, mut address: u64, mut num_pages: usize) {
        while num_pages > 0 {
            let manager_idx = Self::find_manager_index(&self.m_managers, address);
            let pool_index = self.m_managers[manager_idx].m_pool as usize;
            let cur_pages =
                num_pages.min(self.m_managers[manager_idx].get_page_offset_to_end(address));

            {
                let _lk = self.m_pool_locks[pool_index].lock().unwrap();
                self.m_managers[manager_idx].open_first(address, cur_pages);
            }

            num_pages -= cur_pages;
            address += (cur_pages * PAGE_SIZE) as u64;
        }
    }

    /// Open (increment) reference counts for pages at the given address.
    /// Upstream: `void Open(KPhysicalAddress, size_t)`.
    pub fn open(&mut self, mut address: u64, mut num_pages: usize) {
        while num_pages > 0 {
            let manager_idx = Self::find_manager_index(&self.m_managers, address);
            let pool_index = self.m_managers[manager_idx].m_pool as usize;
            let cur_pages =
                num_pages.min(self.m_managers[manager_idx].get_page_offset_to_end(address));

            {
                let _lk = self.m_pool_locks[pool_index].lock().unwrap();
                self.m_managers[manager_idx].open(address, cur_pages);
            }

            num_pages -= cur_pages;
            address += (cur_pages * PAGE_SIZE) as u64;
        }
    }

    /// Return whether a physical address is owned by one of the kernel page
    /// heaps. Rust counterpart of upstream `IsHeapPhysicalAddress(...)` checks
    /// before opening/closing page references.
    pub fn contains_physical_address(&self, address: u64) -> bool {
        self.m_managers
            .iter()
            .any(|manager| manager.contains(address))
    }

    /// Close (decrement) reference counts; free pages when count reaches 0.
    /// Upstream: `void Close(KPhysicalAddress, size_t)`.
    pub fn close(&mut self, mut address: u64, mut num_pages: usize) {
        while num_pages > 0 {
            let manager_idx = Self::find_manager_index(&self.m_managers, address);
            let pool_index = self.m_managers[manager_idx].m_pool as usize;
            let cur_pages =
                num_pages.min(self.m_managers[manager_idx].get_page_offset_to_end(address));

            {
                let _lk = self.m_pool_locks[pool_index].lock().unwrap();
                self.m_managers[manager_idx].close(address, cur_pages);
            }

            num_pages -= cur_pages;
            address += (cur_pages * PAGE_SIZE) as u64;
        }
    }

    // --- Query ---

    /// Upstream: `Pool GetPool(KPhysicalAddress address) const`.
    ///
    /// Rust names the address overload explicitly because it cannot overload
    /// the static option decoder `get_pool(u32)`.
    pub fn get_pool_for_address(&self, address: u64) -> Pool {
        self.m_managers[Self::find_manager_index(&self.m_managers, address)].get_pool()
    }

    /// Upstream: `size_t GetSize(Pool pool)`.
    pub fn get_size(&self, pool: Pool) -> usize {
        let mut total = 0;
        let mut manager = self.get_first_manager(pool, Direction::FromFront);
        while let Some(index) = manager {
            total += self.m_managers[index].get_size();
            manager = self.get_next_manager(index, Direction::FromFront);
        }
        total
    }

    /// Upstream: `size_t GetSize()`.
    pub fn get_total_size(&self) -> usize {
        self.m_managers[..self.m_num_managers]
            .iter()
            .map(Impl::get_size)
            .sum()
    }

    /// Upstream: `size_t GetFreeSize(Pool pool)`.
    pub fn get_free_size(&self, pool: Pool) -> usize {
        let _lk = self.m_pool_locks[pool as usize].lock().unwrap();
        let mut total = 0;
        let mut manager = self.get_first_manager(pool, Direction::FromFront);
        while let Some(index) = manager {
            total += self.m_managers[index].get_free_size();
            manager = self.get_next_manager(index, Direction::FromFront);
        }
        total
    }

    /// Upstream: `size_t GetFreeSize()`.
    pub fn get_total_free_size(&self) -> usize {
        let mut total = 0;
        for manager in &self.m_managers[..self.m_num_managers] {
            let _lk = self.m_pool_locks[manager.get_pool() as usize]
                .lock()
                .unwrap();
            total += manager.get_free_size();
        }
        total
    }

    // --- Optimized memory ---

    /// Upstream: `Result InitializeOptimizedMemory(u64 process_id, Pool pool)`.
    pub fn initialize_optimized_memory(&mut self, process_id: u64, pool: Pool) -> u32 {
        let pool_index = pool as usize;
        let _lk = self.m_pool_locks[pool_index].lock().unwrap();
        if self.m_has_optimized_process[pool_index] {
            return svc_results::RESULT_BUSY.get_inner_value();
        }

        self.m_optimized_process_ids[pool_index] = process_id;
        self.m_has_optimized_process[pool_index] = true;
        0
    }

    /// Upstream: `void FinalizeOptimizedMemory(u64 process_id, Pool pool)`.
    pub fn finalize_optimized_memory(&mut self, process_id: u64, pool: Pool) {
        let pool_index = pool as usize;
        let _lk = self.m_pool_locks[pool_index].lock().unwrap();
        if self.m_has_optimized_process[pool_index]
            && self.m_optimized_process_ids[pool_index] == process_id
        {
            self.m_has_optimized_process[pool_index] = false;
        }
    }

    // --- Static helpers ---

    pub fn encode_option(pool: Pool, dir: Direction) -> u32 {
        ((pool as u32) << Pool::SHIFT) | ((dir as u32) << Direction::SHIFT)
    }

    pub fn get_pool(option: u32) -> Pool {
        let raw = (option & Pool::MASK) >> Pool::SHIFT;
        match raw {
            0 => Pool::Application,
            1 => Pool::Applet,
            2 => Pool::System,
            3 => Pool::SystemNonSecure,
            _ => panic!("Invalid pool value: {}", raw),
        }
    }

    pub fn get_direction(option: u32) -> Direction {
        let raw = (option & Direction::MASK) >> Direction::SHIFT;
        match raw {
            0 => Direction::FromFront,
            1 => Direction::FromBack,
            _ => panic!("Invalid direction value: {}", raw),
        }
    }

    pub fn decode_option(option: u32) -> (Pool, Direction) {
        (Self::get_pool(option), Self::get_direction(option))
    }

    pub fn calculate_management_overhead_size(region_size: usize) -> usize {
        let ref_count_size = (region_size / PAGE_SIZE) * std::mem::size_of::<u16>();
        let optimize_map_size =
            (common::alignment::align_up((region_size / PAGE_SIZE) as u64, 64) as usize / 64)
                * std::mem::size_of::<u64>();
        let manager_meta_size = common::alignment::align_up(
            (optimize_map_size + ref_count_size) as u64,
            PAGE_SIZE as u64,
        ) as usize;
        let page_heap_size = KPageHeap::calculate_management_overhead_size(region_size);
        manager_meta_size + page_heap_size
    }
}

impl Default for KMemoryManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn layout_manager_attributes_order_and_coalesce_regions() {
        use super::*;
        use crate::hle::kernel::{k_memory_layout::KMemoryLayout, k_memory_region_type::*};
        let mut layout = KMemoryLayout::new();
        let tree = layout.get_physical_memory_region_tree_mut();
        tree.insert_directly(0x10000, 0x11fff, 1, K_MEMORY_REGION_TYPE_DRAM_APPLET_POOL.get_value());
        tree.insert_directly(0x20000, 0x21fff, 0, K_MEMORY_REGION_TYPE_DRAM_APPLICATION_POOL.get_value());
        tree.insert_directly(0x22000, 0x23fff, 0, K_MEMORY_REGION_TYPE_DRAM_APPLICATION_POOL.get_value());
        let mut manager = KMemoryManager::new();
        manager.initialize_from_layout(&layout);
        assert_eq!(manager.m_num_managers, 2);
        assert_eq!(manager.m_managers[0].m_pool, Pool::Application);
        assert_eq!(manager.m_managers[1].m_pool, Pool::Applet);
        assert_eq!(manager.get_size(Pool::Application), 0x4000);
        assert_eq!(manager.get_free_size(Pool::Application), 0x4000);
    }

    use super::*;

    #[test]
    fn allocate_and_open_continuous_returns_address_in_pool() {
        let mut mgr = KMemoryManager::new();
        // Initialize a small pool: 16 pages at physical address 0x1_0000_0000.
        let pool_base = 0x1_0000_0000u64;
        let pool_size = 16 * PAGE_SIZE;
        mgr.initialize_pool(Pool::SECURE, pool_base, pool_size);

        let option = KMemoryManager::encode_option(Pool::SECURE, Direction::FromBack);
        let addr = mgr.allocate_and_open_continuous(1, 1, option);
        assert_ne!(addr, 0);
        assert!(addr >= pool_base);
        assert!(addr < pool_base + pool_size as u64);
    }

    #[test]
    fn size_free_size_and_address_pool_follow_manager_chain() {
        let mut mgr = KMemoryManager::new();
        let application_base = 0x1_0000_0000u64;
        let second_application_base = application_base + (16 * PAGE_SIZE) as u64;
        let system_base = 0x2_0000_0000u64;
        mgr.initialize_pool(Pool::Application, application_base, 16 * PAGE_SIZE);
        mgr.initialize_pool(Pool::Application, second_application_base, 16 * PAGE_SIZE);
        mgr.initialize_pool(Pool::System, system_base, 8 * PAGE_SIZE);

        assert_eq!(mgr.get_size(Pool::Application), 32 * PAGE_SIZE);
        assert_eq!(mgr.get_size(Pool::System), 8 * PAGE_SIZE);
        assert_eq!(mgr.get_total_size(), 40 * PAGE_SIZE);
        assert_eq!(mgr.get_free_size(Pool::Application), 32 * PAGE_SIZE);
        assert_eq!(mgr.get_total_free_size(), 40 * PAGE_SIZE);
        assert_eq!(
            mgr.get_pool_for_address(second_application_base + PAGE_SIZE as u64),
            Pool::Application
        );
        assert_eq!(mgr.get_pool_for_address(system_base), Pool::System);

        let option = KMemoryManager::encode_option(Pool::Application, Direction::FromFront);
        assert_ne!(mgr.allocate_and_open_continuous(2, 1, option), 0);
        assert_eq!(mgr.get_free_size(Pool::Application), 30 * PAGE_SIZE);
        assert_eq!(mgr.get_total_free_size(), 38 * PAGE_SIZE);
    }

    #[test]
    fn open_and_close_ref_counting() {
        let mut mgr = KMemoryManager::new();
        let pool_base = 0x1_0000_0000u64;
        let pool_size = 16 * PAGE_SIZE;
        mgr.initialize_pool(Pool::SECURE, pool_base, pool_size);

        let option = KMemoryManager::encode_option(Pool::SECURE, Direction::FromBack);

        // Allocate 1 page (OpenFirst sets refcount to 1).
        let addr = mgr.allocate_and_open_continuous(1, 1, option);
        assert_ne!(addr, 0);

        // Open again (refcount becomes 2).
        mgr.open(addr, 1);

        // First close (refcount becomes 1, page NOT freed).
        mgr.close(addr, 1);

        // Second close (refcount becomes 0, page freed).
        mgr.close(addr, 1);
    }

    #[test]
    fn open_first_open_close_split_across_impl_managers() {
        let mut mgr = KMemoryManager::new();
        let first_base = 0x1_0000_0000u64;
        let second_base = first_base + (2 * PAGE_SIZE) as u64;
        mgr.initialize_pool(Pool::SECURE, first_base, 2 * PAGE_SIZE);
        mgr.initialize_pool(Pool::SECURE, second_base, 2 * PAGE_SIZE);

        let cross_boundary = first_base + PAGE_SIZE as u64;
        mgr.open_first(cross_boundary, 2);
        assert_eq!(mgr.m_managers[0].m_page_reference_counts[1], 1);
        assert_eq!(mgr.m_managers[1].m_page_reference_counts[0], 1);

        mgr.open(cross_boundary, 2);
        assert_eq!(mgr.m_managers[0].m_page_reference_counts[1], 2);
        assert_eq!(mgr.m_managers[1].m_page_reference_counts[0], 2);

        mgr.close(cross_boundary, 2);
        assert_eq!(mgr.m_managers[0].m_page_reference_counts[1], 1);
        assert_eq!(mgr.m_managers[1].m_page_reference_counts[0], 1);
    }

    #[test]
    fn optimized_memory_tracks_single_process_per_pool() {
        let mut mgr = KMemoryManager::new();

        assert_eq!(mgr.initialize_optimized_memory(0x1234, Pool::SECURE), 0);
        assert!(mgr.m_has_optimized_process[Pool::SECURE as usize]);
        assert_eq!(mgr.m_optimized_process_ids[Pool::SECURE as usize], 0x1234);

        assert_eq!(
            mgr.initialize_optimized_memory(0x5678, Pool::SECURE),
            svc_results::RESULT_BUSY.get_inner_value()
        );

        mgr.finalize_optimized_memory(0x5678, Pool::SECURE);
        assert!(mgr.m_has_optimized_process[Pool::SECURE as usize]);

        mgr.finalize_optimized_memory(0x1234, Pool::SECURE);
        assert!(!mgr.m_has_optimized_process[Pool::SECURE as usize]);
    }
}
