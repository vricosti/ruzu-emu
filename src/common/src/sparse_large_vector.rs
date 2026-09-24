//! Port of eden/src/common/sparse_large_vector.h and
//! eden/src/common/sparse_large_vector.cpp (Eden 38df54edfe: decommit zeroed
//! pages; originally 5f142c7926, which replaced `virtual_buffer.h/.cpp`).
//! Status: COMPLET
//! Derniere synchro: 2026-09-24
//!
//! A large page-aligned buffer with optimized memory usage for zero-writes.
//! The whole region is only *reserved* (read-only on POSIX, `MEM_RESERVE` on
//! Windows) and individual host pages are committed on demand, tracked in an
//! atomic bitmap (`committed_pages`, 64 pages per word).
//!
//! Rust adaptations:
//! - Windows fault recovery uses the inaccessible data address and live byte
//!   ranges, and upgrades fault-committed zero pages before normal writes.
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
    use super::host_page_size;
    use std::sync::{Mutex, Once};
    use windows_sys::Win32::Foundation::{GetLastError, EXCEPTION_ACCESS_VIOLATION};
    use windows_sys::Win32::System::Diagnostics::Debug::{
        AddVectoredExceptionHandler, EXCEPTION_CONTINUE_EXECUTION, EXCEPTION_CONTINUE_SEARCH,
        EXCEPTION_POINTERS,
    };
    use windows_sys::Win32::System::Memory::{
        VirtualAlloc, VirtualProtect, VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT,
        MEM_RESERVE, PAGE_READONLY, PAGE_READWRITE,
    };

    /// Upstream `static std::vector<std::pair<u64, u64>> vector_regions`.
    pub(super) static VECTOR_REGIONS: Mutex<Vec<(u64, u64)>> = Mutex::new(Vec::new());
    /// Upstream `static std::once_flag flag` guarding
    /// `AddVectoredExceptionHandler(1, FakePageFaultHandler)`.
    pub(super) static INSTALL_HANDLER: Once = Once::new();
    // Serialize demand-read commits, normal write commits and decommits.
    // Ordinary JIT loads do not take this lock. Owners still serialize table
    // mutations (KPageTableBase::m_general_lock); this is not a data-access lock.
    pub(super) static COMMIT_LOCK: Mutex<()> = Mutex::new(());

    /// Upstream `FakePageFaultHandler`: workaround for handling non-committed
    /// memory accessed by Dynarmic; usually the result of an error.
    ///
    /// Intentional correction to upstream: ExceptionAddress is the instruction
    /// pointer, not the inaccessible data address. Registered ranges are byte
    /// addresses, not page numbers. Only recover reads inside a live vector.
    pub(super) unsafe extern "system" fn fake_page_fault_handler(
        info: *mut EXCEPTION_POINTERS,
    ) -> i32 {
        if info.is_null() || unsafe { (*info).ExceptionRecord.is_null() } {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        let record = unsafe { &*(*info).ExceptionRecord };
        if record.ExceptionCode != EXCEPTION_ACCESS_VIOLATION
            || record.NumberParameters < 2
            || record.ExceptionInformation[0] != 0
        {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        let address = record.ExceptionInformation[1] as u64;
        let Ok(regions) = VECTOR_REGIONS.lock() else {
            return EXCEPTION_CONTINUE_SEARCH;
        };
        if !regions
            .iter()
            .any(|&(start, end)| start <= address && address < end)
        {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        // Keep the region registered/allocated until commit finishes. If an
        // instruction spans pages, Windows reports the next missing page on
        // retry; never speculate outside the reservation with address + 0x40.
        let page = address & super::host_page_mask();
        if commit_vector_page(page as usize, false) {
            EXCEPTION_CONTINUE_EXECUTION
        } else {
            EXCEPTION_CONTINUE_SEARCH
        }
    }

    /// Upstream `Common::CommitVectorPage`.
    pub fn commit_vector_page(addr: usize, write: bool) -> bool {
        let Ok(_commit_guard) = COMMIT_LOCK.lock() else {
            return false;
        };
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
        } else if info.State == MEM_COMMIT {
            if info.Protect == PAGE_READWRITE || (!write && info.Protect == PAGE_READONLY) {
                return true;
            }
            if write && info.Protect == PAGE_READONLY {
                let mut old_protect = 0;
                return unsafe {
                    VirtualProtect(
                        addr as *const _,
                        host_page_size() as usize,
                        PAGE_READWRITE,
                        &mut old_protect,
                    )
                } != 0;
            }
            return false;
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
            assert!(
                !AddVectoredExceptionHandler(1, Some(fake_page_fault_handler)).is_null(),
                "Failed to install SparseLargeVector page-fault handler"
            );
        });
    }
}

#[cfg(windows)]
pub use win::commit_vector_page;

/// Upstream `Common::DecommitVectorPage` (38df54edfe).
/// Keeps the virtual reservation but discards backing for one whole host page.
/// Unlike upstream, returns OS failure so the caller cannot silently publish
/// a cleared bitmap while stale data remains readable by the JIT.
///
/// # Safety
/// `base` must be host-page aligned in a live vector reservation. The caller
/// must serialize mutations and ensure no outstanding references need its data.
/// On non-Linux Unix, the page must be writable for the explicit zero-fill.
pub unsafe fn decommit_vector_page(base: usize) -> bool {
    debug_assert_eq!(base & (host_page_size() as usize - 1), 0);
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Memory::{VirtualFree, MEM_DECOMMIT};
        let Ok(_guard) = win::COMMIT_LOCK.lock() else {
            return false;
        };
        unsafe { VirtualFree(base as *mut _, host_page_size() as usize, MEM_DECOMMIT) != 0 }
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        // Anonymous private mappings read as zero after MADV_DONTNEED.
        unsafe {
            libc::madvise(
                base as *mut _,
                host_page_size() as usize,
                libc::MADV_DONTNEED,
            ) == 0
        }
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "android")))]
    {
        // Upstream: MADV_FREE, with MADV_DONTNEED fallback where unavailable.
        #[cfg(any(
            target_vendor = "apple",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly",
            target_os = "solaris",
            target_os = "haiku",
            target_os = "emscripten",
            target_os = "illumos"
        ))]
        let advice = libc::MADV_FREE;
        #[cfg(not(any(
            target_vendor = "apple",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly",
            target_os = "solaris",
            target_os = "haiku",
            target_os = "emscripten",
            target_os = "illumos"
        )))]
        let advice = libc::MADV_DONTNEED;
        if unsafe { libc::madvise(base as *mut _, host_page_size() as usize, advice) } != 0 {
            return false;
        }
        // MADV_FREE alone does not guarantee zeroes on the next read.
        unsafe { ptr::write_bytes(base as *mut u8, 0, host_page_size() as usize) };
        true
    }
}

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
        let mut regions = win::VECTOR_REGIONS.lock().unwrap();
        assert!(unsafe { VirtualFree(base as *mut _, 0, MEM_RELEASE) } != 0);
        regions.retain(|&(start, _)| start != base as u64);
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
    /// Upstream `ZeroRegion` at 38df54edfe: the first partial page uses an
    /// element index, whole pages are decommitted, partial edges are zeroed.
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
            let index = (page - self.base_ptr as u64) as usize / size_of::<T>();
            if !self.is_committed_page(index) {
                page += host_page_size;
                continue;
            }

            if end - page >= host_page_size {
                self.decommit_page(index);
            } else {
                unsafe { ptr::write_bytes(page as *mut u8, 0, (end - page) as usize) };
            }
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

    /// Upstream `DecommitPage`. As upstream, mutations require exclusive
    /// ownership / the caller's page-table lock, not a lock on every JIT read.
    fn decommit_page(&mut self, index: usize) {
        let page_index = (index * size_of::<T>()) >> host_page_bits();
        let word = &self.committed_pages[page_index >> 6];
        let mask = 1u64 << (page_index & 63);
        let page = (unsafe { self.base_ptr.add(index) } as usize) & host_page_mask() as usize;
        word.fetch_and(!mask, Ordering::Release);
        if !unsafe { decommit_vector_page(page) } {
            // Restore bookkeeping before failing; never continue with stale
            // backing and a bitmap claiming the page contains only zeros.
            word.fetch_or(mask, Ordering::Release);
            panic!(
                "Failed to decommit SparseLargeVector page: {}",
                std::io::Error::last_os_error()
            );
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
            assert!(
                win::commit_vector_page(page, true),
                "Failed to commit SparseLargeVector page"
            );
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
    fn zero_region_decommits_full_pages_preserving_partial_edges() {
        let per_page = host_page_size() as usize / size_of::<u64>();
        let mut v = SparseLargeVector::<u64>::with_count(per_page * 4);
        for i in 0..v.size() {
            v.set(i, 123);
        }
        let base = v.data();
        v.zero_region(per_page - 1, per_page * 3 + 1);
        assert_eq!(v.data(), base, "reservation address must stay stable");
        assert!(v.is_committed_page(0));
        assert!(!v.is_committed_page(per_page));
        assert!(!v.is_committed_page(per_page * 2));
        assert!(v.is_committed_page(per_page * 3));
        for i in 0..v.size() {
            assert_eq!(
                v[i],
                if (per_page - 1..per_page * 3 + 1).contains(&i) {
                    0
                } else {
                    123
                }
            );
        }
        v.commit_region(per_page, per_page * 3);
        *v.get_unchecked_mut(per_page + 7) = 456;
        assert_eq!(v[per_page + 7], 456);
        assert_eq!(v[per_page * 2], 0);
    }

    #[test]
    fn zero_region_first_partial_page_uses_element_index() {
        let per_page = host_page_size() as usize / size_of::<u64>();
        let mut v = SparseLargeVector::<u64>::with_count(per_page * 4);
        // Page zero stays uncommitted. Dividing this index by sizeof(T)
        // again (the upstream bug) incorrectly queries page zero.
        v.set(per_page * 2 + 2, 99);
        v.set(per_page * 2 + 3, 99);
        v.zero_region(per_page * 2 + 3, per_page * 2 + 4);
        assert_eq!(v[per_page * 2 + 2], 99);
        assert_eq!(v[per_page * 2 + 3], 0);
        assert!(v.is_committed_page(per_page * 2));
    }

    #[test]
    fn aligned_zero_region_clears_bitmap_across_word_boundary_and_reuses_pages() {
        let per_page = host_page_size() as usize / size_of::<u64>();
        let mut v = SparseLargeVector::<u64>::with_count(per_page * 66);
        for _ in 0..32 {
            for page in [0, 62, 63, 64, 65] {
                v.set(per_page * page, 77);
            }
            v.zero_region(0, per_page);
            v.zero_region(per_page * 63, per_page * 65);
            for page in [0, 63, 64] {
                assert!(!v.is_committed_page(per_page * page));
                assert_eq!(v[per_page * page], 0);
            }
            for page in [62, 65] {
                assert!(v.is_committed_page(per_page * page));
                assert_eq!(v[per_page * page], 77);
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_commit_and_decommit_operations_are_serialized() {
        let page = host_page_size() as usize;
        let base = allocate_memory_pages(page) as usize;
        let barrier = std::sync::Barrier::new(5);
        // Exercise the shared OS-operation lock without racing Rust data
        // accesses or pretending the vector supports concurrent mutations.
        std::thread::scope(|scope| {
            for write in [false, true, false, true] {
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    for _ in 0..256 {
                        assert!(commit_vector_page(base, write));
                    }
                });
            }
            barrier.wait();
            for _ in 0..256 {
                assert!(unsafe { decommit_vector_page(base) });
            }
        });
        assert!(commit_vector_page(base, true));
        assert_eq!(unsafe { (base as *const u64).read_volatile() }, 0);
        free_memory_pages(base as *mut u8, page);
    }

    #[cfg(windows)]
    #[test]
    fn windows_decommit_releases_backing_and_concurrent_raw_reads_recover() {
        use windows_sys::Win32::System::Memory::{
            VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT, MEM_RESERVE, PAGE_READONLY,
            PAGE_READWRITE,
        };
        const CHILD: &str = "RUZU_SPARSE_DECOMMIT_TEST";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "sparse_large_vector::tests::windows_decommit_releases_backing_and_concurrent_raw_reads_recover"])
                .env(CHILD, "1").status().unwrap();
            assert!(
                status.success(),
                "decommit/raw-read subprocess failed: {status}"
            );
            return;
        }
        fn query(address: usize) -> MEMORY_BASIC_INFORMATION {
            let mut info = unsafe { std::mem::zeroed() };
            assert_ne!(
                unsafe {
                    VirtualQuery(
                        address as *const _,
                        &mut info,
                        size_of::<MEMORY_BASIC_INFORMATION>(),
                    )
                },
                0
            );
            info
        }
        let per_page = host_page_size() as usize / size_of::<u64>();
        let mut v = SparseLargeVector::<u64>::with_count(per_page * 2);
        let address = v.data() as usize;
        for _ in 0..16 {
            v.set(0, 42);
            assert_eq!(query(address).Protect, PAGE_READWRITE);
            v.zero_region(0, per_page);
            let info = query(address);
            assert_eq!(
                info.State, MEM_RESERVE,
                "must release backing, not just clear bitmap"
            );
            assert_eq!(info.AllocationBase as usize, address);
            assert_eq!(v[0], 0);
            assert_eq!(
                query(address).State,
                MEM_RESERVE,
                "ordinary zero read must not commit"
            );
            let barrier = std::sync::Barrier::new(8);
            std::thread::scope(|scope| {
                for _ in 0..8 {
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        assert_eq!(unsafe { (address as *const u64).read_volatile() }, 0);
                    });
                }
            });
            assert_eq!(query(address).State, MEM_COMMIT);
            assert_eq!(query(address).Protect, PAGE_READONLY);
            assert!(!v.is_committed_page(0));
            v.set(0, 17);
            assert_eq!(query(address).Protect, PAGE_READWRITE);
            assert_eq!(v[0], 17);
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_raw_read_recovers_and_can_be_upgraded() {
        // Isolate the real access violation so a broken handler fails this
        // test without terminating the rest of the test harness.
        const CHILD: &str = "RUZU_SPARSE_VECTOR_FAULT_TEST";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "sparse_large_vector::tests::windows_raw_read_recovers_and_can_be_upgraded",
                ])
                .env(CHILD, "1")
                .status()
                .unwrap();
            assert!(status.success(), "raw page-table read failed: {status}");
            return;
        }
        let mut v = SparseLargeVector::<u64>::with_count(1024);
        assert_eq!(unsafe { v.data().read_volatile() }, 0);
        assert!(!v.is_committed_page(0));
        v.set(0, 42);
        assert_eq!(unsafe { v.data().read_volatile() }, 42);
        assert!(commit_vector_page(v.data() as usize, false));
        v.set(0, 43);
        assert_eq!(v[0], 43);
    }

    #[cfg(windows)]
    #[test]
    fn windows_fault_handler_rejects_unrelated_accesses() {
        use windows_sys::Win32::Foundation::EXCEPTION_ACCESS_VIOLATION;
        use windows_sys::Win32::System::Diagnostics::Debug::{
            EXCEPTION_CONTINUE_SEARCH, EXCEPTION_POINTERS, EXCEPTION_RECORD,
        };
        let v = SparseLargeVector::<u64>::with_count(1024);
        let mut record: EXCEPTION_RECORD = unsafe { std::mem::zeroed() };
        record.ExceptionCode = EXCEPTION_ACCESS_VIOLATION;
        record.NumberParameters = 2;
        record.ExceptionInformation[1] = v.data() as usize;
        for access in [1, 8] {
            record.ExceptionInformation[0] = access;
            let mut pointers = EXCEPTION_POINTERS {
                ExceptionRecord: &mut record,
                ContextRecord: ptr::null_mut(),
            };
            assert_eq!(
                unsafe { win::fake_page_fault_handler(&mut pointers) },
                EXCEPTION_CONTINUE_SEARCH
            );
        }
        record.ExceptionInformation[0] = 0;
        record.ExceptionInformation[1] = 0;
        let mut pointers = EXCEPTION_POINTERS {
            ExceptionRecord: &mut record,
            ContextRecord: ptr::null_mut(),
        };
        assert_eq!(
            unsafe { win::fake_page_fault_handler(&mut pointers) },
            EXCEPTION_CONTINUE_SEARCH
        );
        assert_eq!(
            unsafe { win::fake_page_fault_handler(ptr::null_mut()) },
            EXCEPTION_CONTINUE_SEARCH
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_jit_read_fault_uses_data_address_not_instruction_address() {
        use windows_sys::Win32::Foundation::EXCEPTION_ACCESS_VIOLATION;
        use windows_sys::Win32::System::Diagnostics::Debug::{
            EXCEPTION_CONTINUE_EXECUTION, EXCEPTION_POINTERS, EXCEPTION_RECORD,
        };
        let mut v = SparseLargeVector::<u64>::with_count(1024);
        let mut record: EXCEPTION_RECORD = unsafe { std::mem::zeroed() };
        record.ExceptionCode = EXCEPTION_ACCESS_VIOLATION;
        record.ExceptionAddress =
            windows_jit_read_fault_uses_data_address_not_instruction_address as *const () as *mut _;
        record.NumberParameters = 2;
        record.ExceptionInformation[0] = 0;
        record.ExceptionInformation[1] = v.data() as usize;
        let mut pointers = EXCEPTION_POINTERS {
            ExceptionRecord: &mut record,
            ContextRecord: ptr::null_mut(),
        };
        assert_eq!(
            unsafe { win::fake_page_fault_handler(&mut pointers) },
            EXCEPTION_CONTINUE_EXECUTION
        );
        assert_eq!(unsafe { v.data().read_volatile() }, 0);
        // A fault-committed read-only page must later become writable normally.
        v.set(0, 42);
        assert_eq!(v[0], 42);
    }

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
