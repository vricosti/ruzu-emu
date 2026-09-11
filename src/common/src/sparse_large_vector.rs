//! Port of eden/src/common/sparse_large_vector.h and
//! eden/src/common/sparse_large_vector.cpp (Eden 5f142c7926, which replaced
//! `virtual_buffer.h/.cpp`).
//! Status: COMPLET
//! Derniere synchro: 2026-09-11
//!
//! A large page-aligned buffer with optimized memory usage for zero-writes.
//! The whole region is only *reserved* (read-only on POSIX, `MEM_RESERVE` on
//! Windows) and individual host pages are committed on demand, tracked in an
//! atomic bitmap (`committed_pages`, 64 pages per word).
//!
//! Rust adaptations (documented, no behavioural change):
//! - upstream `HostPageSize`/`HostPageBits`/`HostPageMask` are runtime globals
//!   on POSIX; here they are the functions [`host_page_size`],
//!   [`host_page_bits`] and [`host_page_mask`] on every platform.
//! - upstream methods that only touch the atomic bitmap and the OS commit
//!   state (`CommitRegion`, `GetUnchecked` for reads) take `&self`; methods
//!   that write elements (`GetAndFault`, `Set`, `ZeroRegion`,
//!   `get_unchecked_mut`) take `&mut self`.
//! - upstream requires `std::is_trivially_copyable_v<T>`; the element type
//!   must additionally be valid as all-zero bytes because uncommitted pages
//!   read as zero (this is also what upstream relies on).

use crate::alignment::{align_down, align_up};
use std::mem::size_of;
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(windows)]
pub const HOST_PAGE_SIZE: u64 = 0x1000;
#[cfg(windows)]
pub const HOST_PAGE_BITS: u64 = 12;
#[cfg(windows)]
pub const HOST_PAGE_MASK: u64 = !(HOST_PAGE_SIZE - 1);

/// Upstream `Common::HostPageSize`.
#[cfg(windows)]
pub fn host_page_size() -> u64 {
    HOST_PAGE_SIZE
}

/// Upstream `Common::HostPageSize` (`sysconf(_SC_PAGESIZE)`).
#[cfg(not(windows))]
pub fn host_page_size() -> u64 {
    static HOST_PAGE_SIZE: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *HOST_PAGE_SIZE.get_or_init(|| unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64)
}

/// Upstream `Common::HostPageBits` (`std::countr_zero(HostPageSize)`).
pub fn host_page_bits() -> u64 {
    host_page_size().trailing_zeros() as u64
}

/// Upstream `Common::HostPageMask` (`~(HostPageSize - 1)`).
pub fn host_page_mask() -> u64 {
    !(host_page_size() - 1)
}

#[cfg(any(target_os = "freebsd", target_os = "dragonfly"))]
const MAP_NOCORE: libc::c_int = libc::MAP_NOCORE;
/// Upstream: `#ifndef MAP_NOCORE #define MAP_NOCORE 0 #endif`.
#[cfg(not(any(windows, target_os = "freebsd", target_os = "dragonfly")))]
const MAP_NOCORE: libc::c_int = 0;

#[cfg(windows)]
mod win {
    use super::{host_page_bits, host_page_size};
    use std::sync::{Mutex, Once};
    use windows_sys::Win32::Foundation::{GetLastError, EXCEPTION_ACCESS_VIOLATION};
    use windows_sys::Win32::System::Diagnostics::Debug::{
        AddVectoredExceptionHandler, EXCEPTION_CONTINUE_EXECUTION, EXCEPTION_CONTINUE_SEARCH,
        EXCEPTION_POINTERS,
    };
    use windows_sys::Win32::System::Memory::{
        VirtualAlloc, VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT, MEM_RESERVE,
        PAGE_READONLY, PAGE_READWRITE,
    };

    /// Upstream `static std::vector<std::pair<u64, u64>> vector_regions`.
    pub(super) static VECTOR_REGIONS: Mutex<Vec<(u64, u64)>> = Mutex::new(Vec::new());
    /// Upstream `static std::once_flag flag` guarding
    /// `AddVectoredExceptionHandler(1, FakePageFaultHandler)`.
    pub(super) static INSTALL_HANDLER: Once = Once::new();

    /// Upstream `FakePageFaultHandler`: workaround for handling non-committed
    /// memory accessed by Dynarmic; usually the result of an error.
    ///
    /// The region comparison against the page-shifted fault address is
    /// ported as written upstream.
    pub(super) unsafe extern "system" fn fake_page_fault_handler(
        info: *mut EXCEPTION_POINTERS,
    ) -> i32 {
        let record = unsafe { &*(*info).ExceptionRecord };
        let code = record.ExceptionCode;
        let exception_addr = record.ExceptionAddress as u64;

        if code != EXCEPTION_ACCESS_VIOLATION {
            // Not our problem
            return EXCEPTION_CONTINUE_SEARCH;
        }

        let mut addr = 0u64;
        let mut addr2 = 0u64;

        let regions = VECTOR_REGIONS.lock().unwrap();
        for region in regions.iter() {
            let addr_shifted = exception_addr >> host_page_bits();
            if region.0 <= addr_shifted && addr_shifted <= region.1 {
                addr = addr_shifted;
            }

            // Page-boundary accesses
            let addr_ = (exception_addr + 0x40) >> host_page_bits();
            if addr_ != addr_shifted && region.0 <= addr_ && addr_ <= region.1 {
                addr2 = addr_;
            }

            if addr != 0 || addr2 != 0 {
                break;
            }
        }
        drop(regions);

        if addr == 0 && addr2 == 0 {
            // Not our problem
            return EXCEPTION_CONTINUE_SEARCH;
        }

        log::error!(
            "Accessing an unallocated region of a SparseLargeVector at {:#x}; this shouldn't happen and is likely a Dynarmic error!",
            exception_addr
        );

        // Commit this region
        if addr != 0 && !commit_vector_page((addr << host_page_bits()) as usize, false) {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        // Commit next region if needed
        if addr2 != 0 && !commit_vector_page((addr2 << host_page_bits()) as usize, false) {
            return EXCEPTION_CONTINUE_SEARCH;
        }

        EXCEPTION_CONTINUE_EXECUTION
    }

    /// Upstream `Common::CommitVectorPage`.
    pub fn commit_vector_page(addr: usize, write: bool) -> bool {
        let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        let res = unsafe {
            VirtualQuery(
                addr as *const _,
                &mut info,
                std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        };
        if res == 0 {
            log::error!(
                "Failed to query large buffer region at {:#x} with error {}, will try committing anyway",
                addr,
                unsafe { GetLastError() }
            );
        } else if info.State != MEM_RESERVE {
            log::error!(
                "Tried to commit an unreserved large buffer region at {:#x} that is not mapped or is already committed (state {:#x})",
                addr,
                info.State
            );
            return false;
        }

        let perm = if write { PAGE_READWRITE } else { PAGE_READONLY };
        let res2 =
            unsafe { VirtualAlloc(addr as *mut _, host_page_size() as usize, MEM_COMMIT, perm) };
        if res2.is_null() {
            log::error!(
                "Failed to commit large buffer region at {:#x}, error {}",
                addr,
                unsafe { GetLastError() }
            );
            return false;
        }

        true
    }

    pub(super) fn install_fake_page_fault_handler() {
        INSTALL_HANDLER.call_once(|| unsafe {
            AddVectoredExceptionHandler(1, Some(fake_page_fault_handler));
        });
    }
}

#[cfg(windows)]
pub use win::commit_vector_page;

/// Upstream `Common::AllocateMemoryPages`: reserves `size` bytes (page
/// aligned). The pages are read-only on POSIX and reserved-only on Windows
/// until committed by the owning vector.
pub fn allocate_memory_pages(mut size: usize) -> *mut u8 {
    let page = host_page_size() as usize;
    if size % page != 0 {
        log::warn!(
            "Allocating unaligned large vector with size {:#x}; aligning to {} page size",
            size,
            page
        );
        size = align_up(size as u64, page as u64) as usize;
    }

    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::GetLastError;
        use windows_sys::Win32::System::Memory::{
            VirtualAlloc, MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE,
        };

        // We will never use this memory entirely so instead of committing it up front let's
        // just reserve it and commit each page individually
        let mut base =
            unsafe { VirtualAlloc(ptr::null_mut(), size, MEM_RESERVE, PAGE_READWRITE) } as *mut u8;

        if !base.is_null() {
            win::VECTOR_REGIONS
                .lock()
                .unwrap()
                .push((base as u64, base as u64 + size as u64));

            win::install_fake_page_fault_handler();
        } else {
            // Try committing everything instead??
            log::warn!(
                "Failed to reserve large vector region with error {}, trying to commit instead..",
                unsafe { GetLastError() }
            );
            base = unsafe { VirtualAlloc(ptr::null_mut(), size, MEM_COMMIT, PAGE_READWRITE) }
                as *mut u8;
        }
        assert!(
            !base.is_null(),
            "Failed to reserve {:#x} sized region with error {}",
            size,
            unsafe { GetLastError() }
        );
        base
    }
    #[cfg(not(windows))]
    {
        let base = unsafe {
            libc::mmap(
                ptr::null_mut(),
                size,
                libc::PROT_READ,
                libc::MAP_ANON | libc::MAP_PRIVATE | MAP_NOCORE,
                -1,
                0,
            )
        };
        let base = if base == libc::MAP_FAILED {
            ptr::null_mut()
        } else {
            base as *mut u8
        };
        assert!(
            !base.is_null(),
            "Failed to allocate {:#x} sized region with error {}",
            size,
            std::io::Error::last_os_error()
        );
        base
    }
}

/// Upstream `Common::FreeMemoryPages`.
pub fn free_memory_pages(base: *mut u8, mut size: usize) {
    let page = host_page_size() as usize;
    if size % page != 0 {
        size = align_up(size as u64, page as u64) as usize;
    }
    if base.is_null() {
        return;
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Memory::{VirtualFree, MEM_RELEASE};
        let _ = size;
        assert!(unsafe { VirtualFree(base as *mut _, 0, MEM_RELEASE) } != 0);
    }
    #[cfg(not(windows))]
    {
        assert!(unsafe { libc::munmap(base as *mut _, size) } == 0);
    }
}

/// A large page-aligned buffer that has optimized memory usage for zero-writes.
///
/// Upstream `Common::SparseLargeVector<T>`.
pub struct SparseLargeVector<T> {
    alloc_size: usize,
    base_ptr: *mut T,
    committed_pages: Vec<AtomicU64>,
    /// Upstream `const std::array<u8, sizeof(T)> default_val{}` (Windows only).
    #[cfg(windows)]
    default_val: std::mem::MaybeUninit<T>,
}

impl<T> SparseLargeVector<T> {
    /// Upstream `constexpr SparseLargeVector() = default`.
    pub const fn new() -> Self {
        Self {
            alloc_size: 0,
            base_ptr: ptr::null_mut(),
            committed_pages: Vec::new(),
            #[cfg(windows)]
            default_val: std::mem::MaybeUninit::zeroed(),
        }
    }

    /// Upstream `explicit SparseLargeVector(std::size_t count)`.
    pub fn with_count(count: usize) -> Self {
        let alloc_size = count * size_of::<T>();
        Self {
            alloc_size,
            base_ptr: allocate_memory_pages(alloc_size) as *mut T,
            // each item in vector holds information for 64 pages
            committed_pages: Self::make_committed_pages(alloc_size),
            #[cfg(windows)]
            default_val: std::mem::MaybeUninit::zeroed(),
        }
    }

    fn make_committed_pages(alloc_size: usize) -> Vec<AtomicU64> {
        // each item in vector holds information for 64 pages
        let denom = host_page_size() as usize * 64;
        (0..(alloc_size + denom - 1) / denom)
            .map(|_| AtomicU64::new(0))
            .collect()
    }

    /// Upstream `ResizeAndClear`. Previous contents are lost.
    ///
    /// A zero-element resize releases the region without reserving a new one
    /// (upstream would pass a zero size to `mmap`/`VirtualAlloc`).
    pub fn resize_and_clear(&mut self, count: usize) {
        let new_size = count * size_of::<T>();
        if new_size != self.alloc_size {
            free_memory_pages(self.base_ptr as *mut u8, self.alloc_size);
            self.alloc_size = new_size;
            self.base_ptr = if new_size == 0 {
                ptr::null_mut()
            } else {
                allocate_memory_pages(new_size) as *mut T
            };

            self.committed_pages = Self::make_committed_pages(new_size);
        }
    }

    /// Returns a reference to the value of the requested index and allocates memory if needed.
    ///
    /// Upstream `GetAndFault`.
    pub fn get_and_fault(&mut self, index: usize) -> &mut T {
        if index > self.alloc_size / size_of::<T>() {
            unreachable!("Out of bounds RW access on SparseLargeVector @ {}", index);
        }

        if !self.is_committed_page(index) {
            self.commit_page(index);
        }
        unsafe { &mut *self.base_ptr.add(index) }
    }

    /// Returns a reference to the value of the requested index if initialized, or will
    /// otherwise return a zero-initialized object.
    ///
    /// Upstream `GetOrDefault`. Upstream performs no bounds check here; the
    /// port asserts instead of reading past the reservation.
    pub fn get_or_default(&self, index: usize) -> &T {
        assert!(
            index < self.size(),
            "Out of bounds read on SparseLargeVector @ {}",
            index
        );
        #[cfg(windows)]
        if !self.is_committed_page(index) {
            return unsafe { &*self.default_val.as_ptr() };
        }
        // On non-Windows, OS page table should optimize this by pointing to a zero page if
        // unallocated.
        unsafe { &*self.base_ptr.add(index) }
    }

    /// Upstream `Set`.
    pub fn set(&mut self, index: usize, value: T) {
        if index > self.alloc_size / size_of::<T>() {
            log::error!("Out of bounds write on SparseLargeVector @ {}", index);
            return;
        }
        if !self.is_committed_page(index) {
            self.commit_page(index);
        }
        unsafe { ptr::write(self.base_ptr.add(index), value) };
    }

    /// Zeroes the elements in `[start, end_)`, skipping host pages that were
    /// never committed (they already read as zero).
    ///
    /// Upstream `ZeroRegion`. Upstream checks `IsCommittedPage(start / sizeof(T))`
    /// for the first partial page; `start` is already an element index (every
    /// other `IsCommittedPage` caller passes one), so the port checks
    /// `is_committed_page(start)`.
    pub fn zero_region(&mut self, start: usize, end_: usize) {
        let host_page_size = host_page_size();
        let mut base = unsafe { self.base_ptr.add(start) } as u64;
        let end = unsafe { self.base_ptr.add(end_) } as u64;

        let end_page = align_up(base, host_page_size);
        let first_size = end_page.min(end) - base;

        if self.is_committed_page(start) {
            unsafe { ptr::write_bytes(base as *mut u8, 0, first_size as usize) };
        }

        if end <= end_page {
            return;
        }

        base = end_page;

        let mut page = base;
        while page < end {
            if !self.is_committed_page((page - self.base_ptr as u64) as usize / size_of::<T>()) {
                page += host_page_size;
                continue;
            }

            unsafe {
                ptr::write_bytes(
                    page as *mut u8,
                    0,
                    host_page_size.min(end - page) as usize,
                )
            };
            page += host_page_size;
        }
    }

    /// Commits every host page backing the elements in `[index, end_)`.
    ///
    /// Upstream `CommitRegion`.
    pub fn commit_region(&self, index: usize, end_: usize) {
        let host_page_size = host_page_size();
        let base = index as u64 * size_of::<T>() as u64;
        let end = end_ as u64 * size_of::<T>() as u64;

        let mut page = align_down(base, host_page_size);
        while page < end {
            let element = page as usize / size_of::<T>();
            if !self.is_committed_page(element) {
                self.commit_page(element);
            }
            page += host_page_size;
        }
    }

    /// Upstream `GetUnchecked` (read access). The page must already be
    /// committed if the reference is used to write through interior
    /// mutability (atomics).
    pub fn get_unchecked(&self, index: usize) -> &T {
        unsafe { &*self.base_ptr.add(index) }
    }

    /// Upstream `GetUnchecked` (write access). The page must already be
    /// committed (see [`SparseLargeVector::commit_region`]).
    pub fn get_unchecked_mut(&mut self, index: usize) -> &mut T {
        unsafe { &mut *self.base_ptr.add(index) }
    }

    /// Upstream `data()`.
    pub fn data(&self) -> *const T {
        self.base_ptr
    }

    /// Upstream `size()` (element count).
    pub fn size(&self) -> usize {
        self.alloc_size / size_of::<T>()
    }

    /// Upstream `IsCommittedPage`.
    ///
    /// Upstream only rejects `index > size` and would then index past its
    /// bitmap for `index == size`; the port treats any page outside the
    /// bitmap as not committed.
    fn is_committed_page(&self, index: usize) -> bool {
        if index > self.alloc_size / size_of::<T>() {
            log::error!("Out of bounds access on large vector @ {}", index);
            return false;
        }

        let page = (index * size_of::<T>()) >> host_page_bits();
        match self.committed_pages.get(page >> 6) {
            Some(word) => (word.load(Ordering::Acquire) >> (page & 63)) & 1 != 0,
            None => false,
        }
    }

    /// Upstream `CommitPage`.
    fn commit_page(&self, index: usize) {
        let page_index = (index * size_of::<T>()) >> host_page_bits();
        let Some(word) = self.committed_pages.get(page_index >> 6) else {
            return;
        };
        let page = (unsafe { self.base_ptr.add(index) } as usize) & host_page_mask() as usize;
        #[cfg(windows)]
        {
            win::commit_vector_page(page, true);
        }
        #[cfg(not(windows))]
        unsafe {
            libc::mprotect(
                page as *mut _,
                host_page_size() as usize,
                libc::PROT_READ | libc::PROT_WRITE,
            );
        }

        word.fetch_or(1u64 << (page_index & 63), Ordering::Release);
    }
}

impl<T> std::ops::Index<usize> for SparseLargeVector<T> {
    type Output = T;

    /// Upstream `operator[]` → `GetOrDefault`.
    fn index(&self, index: usize) -> &T {
        self.get_or_default(index)
    }
}

impl<T> Default for SparseLargeVector<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Drop for SparseLargeVector<T> {
    fn drop(&mut self) {
        free_memory_pages(self.base_ptr as *mut u8, self.alloc_size);
    }
}

// The reservation is owned by the vector; element writes are either
// `&mut self` or go through atomics inside `T`.
unsafe impl<T: Send> Send for SparseLargeVector<T> {}
unsafe impl<T: Sync> Sync for SparseLargeVector<T> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_page_constants_are_consistent() {
        let size = host_page_size();
        assert!(size.is_power_of_two());
        assert_eq!(1u64 << host_page_bits(), size);
        assert_eq!(host_page_mask(), !(size - 1));
    }

    #[test]
    fn uncommitted_pages_read_as_zero_and_are_not_committed() {
        let v = SparseLargeVector::<u64>::with_count(1 << 16);
        assert_eq!(v.size(), 1 << 16);
        assert_eq!(v[0], 0);
        assert_eq!(v[(1 << 16) - 1], 0);
        assert!(!v.is_committed_page(0));
        assert!(!v.is_committed_page((1 << 16) - 1));
    }

    #[test]
    fn set_commits_only_the_touched_host_page() {
        let mut v = SparseLargeVector::<u64>::with_count(1 << 16);
        let per_page = host_page_size() as usize / size_of::<u64>();
        v.set(per_page * 3 + 5, 0xDEAD_BEEF);
        assert_eq!(v[per_page * 3 + 5], 0xDEAD_BEEF);
        assert!(v.is_committed_page(per_page * 3));
        assert!(v.is_committed_page(per_page * 4 - 1));
        assert!(!v.is_committed_page(per_page * 2));
        assert!(!v.is_committed_page(per_page * 4));
    }

    #[test]
    fn get_and_fault_commits_and_returns_writable_reference() {
        let mut v = SparseLargeVector::<u32>::with_count(1 << 12);
        *v.get_and_fault(17) = 42;
        assert_eq!(v[17], 42);
        assert_eq!(v[16], 0);
    }

    #[test]
    fn commit_region_covers_every_page_of_the_range() {
        let v = SparseLargeVector::<u64>::with_count(1 << 16);
        let per_page = host_page_size() as usize / size_of::<u64>();
        v.commit_region(per_page + 1, per_page * 3 + 1);
        assert!(!v.is_committed_page(0));
        assert!(v.is_committed_page(per_page + 1));
        assert!(v.is_committed_page(per_page * 2));
        assert!(v.is_committed_page(per_page * 3));
        assert!(!v.is_committed_page(per_page * 4));
        // Committed pages are writable through get_unchecked_mut.
        let mut v = v;
        *v.get_unchecked_mut(per_page * 3) = 7;
        assert_eq!(v[per_page * 3], 7);
    }

    #[test]
    fn zero_region_clears_committed_pages_and_skips_uncommitted_ones() {
        let mut v = SparseLargeVector::<u64>::with_count(1 << 16);
        let per_page = host_page_size() as usize / size_of::<u64>();
        for i in [per_page - 1, per_page, per_page * 3, per_page * 3 + 2] {
            v.set(i, 1);
        }
        // Page 2 stays uncommitted and must be skipped without faulting.
        v.zero_region(per_page - 1, per_page * 3 + 1);
        assert_eq!(v[per_page - 1], 0);
        assert_eq!(v[per_page], 0);
        assert_eq!(v[per_page * 3], 0);
        assert_eq!(v[per_page * 3 + 2], 1);
        assert!(!v.is_committed_page(per_page * 2));
    }

    #[test]
    fn zero_region_within_a_single_page() {
        let mut v = SparseLargeVector::<u64>::with_count(1 << 12);
        v.set(4, 9);
        v.set(5, 9);
        v.set(6, 9);
        v.zero_region(4, 6);
        assert_eq!(v[4], 0);
        assert_eq!(v[5], 0);
        assert_eq!(v[6], 9);
    }

    #[test]
    fn resize_and_clear_drops_previous_contents() {
        let mut v = SparseLargeVector::<u32>::with_count(1 << 10);
        v.set(3, 5);
        v.resize_and_clear(1 << 11);
        assert_eq!(v.size(), 1 << 11);
        assert_eq!(v[3], 0);
        assert!(!v.is_committed_page(3));
        v.resize_and_clear(0);
        assert_eq!(v.size(), 0);
        assert!(v.data().is_null());
    }

    #[test]
    fn default_vector_is_empty() {
        let v = SparseLargeVector::<u64>::new();
        assert_eq!(v.size(), 0);
        assert!(v.data().is_null());
    }

    #[test]
    fn atomic_elements_can_be_stored_through_shared_reference() {
        let v = SparseLargeVector::<AtomicU64>::with_count(1 << 10);
        v.commit_region(0, 8);
        v.get_unchecked(2).store(0x1234, Ordering::Relaxed);
        assert_eq!(v[2].load(Ordering::Relaxed), 0x1234);
    }
}
