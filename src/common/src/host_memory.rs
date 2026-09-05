//! Port of zuyu/src/common/host_memory.h and zuyu/src/common/host_memory.cpp
//! Status: COMPLET
//! Derniere synchro: 2026-03-05

use crate::alignment::align_up;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::free_region_manager::FreeRegionManager;
use crate::virtual_buffer::VirtualBuffer;
use log::error;
#[cfg(target_os = "macos")]
use std::ffi::CString;
use std::ptr;
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(target_os = "windows")]
use std::{
    collections::BTreeMap,
    sync::{Mutex, MutexGuard},
};

const PAGE_ALIGNMENT: usize = 0x1000;
const HUGE_PAGE_SIZE: usize = 0x200000;

bitflags::bitflags! {
    /// Memory permission flags, matching C++ Common::MemoryPermission.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MemoryPermission: u32 {
        const READ = 1 << 0;
        const WRITE = 1 << 1;
        const READ_WRITE = Self::READ.bits() | Self::WRITE.bits();
        const EXECUTE = 1 << 2;
    }
}

#[cfg(target_os = "windows")]
#[derive(Clone, Copy)]
struct WindowsMapping {
    end: usize,
    host_offset: usize,
}

/// Windows placeholder-backed alias mapping.
///
/// Upstream owner: `common/host_memory.cpp`, `HostMemory::Impl` under
/// `#ifdef _WIN32`.
#[cfg(target_os = "windows")]
struct HostMemoryImpl {
    // Kept for ownership parity with Eden's Windows `HostMemory::Impl`; the
    // constructor consumes the value while reserving the backing section.
    #[allow(dead_code)]
    backing_size: usize,
    virtual_size: usize,
    backing_base: *mut u8,
    virtual_base: *mut u8,
    process: windows_sys::Win32::Foundation::HANDLE,
    backing_handle: windows_sys::Win32::Foundation::HANDLE,
    mappings: Mutex<BTreeMap<usize, WindowsMapping>>,
}

#[cfg(target_os = "windows")]
impl HostMemoryImpl {
    fn new(backing_size: usize, virtual_size: usize) -> Result<Self, String> {
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::System::Memory::{
            CreateFileMapping2, MapViewOfFile3, VirtualAlloc2, FILE_MAP_READ, FILE_MAP_WRITE,
            MEM_REPLACE_PLACEHOLDER, MEM_RESERVE, MEM_RESERVE_PLACEHOLDER, PAGE_NOACCESS,
            PAGE_READWRITE, SEC_COMMIT,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;

        let process = unsafe { GetCurrentProcess() };
        let mut result = Self {
            backing_size,
            virtual_size,
            backing_base: ptr::null_mut(),
            virtual_base: ptr::null_mut(),
            process,
            backing_handle: ptr::null_mut(),
            mappings: Mutex::new(BTreeMap::new()),
        };

        result.backing_handle = unsafe {
            CreateFileMapping2(
                INVALID_HANDLE_VALUE,
                ptr::null(),
                FILE_MAP_WRITE | FILE_MAP_READ,
                PAGE_READWRITE,
                SEC_COMMIT,
                backing_size as u64,
                ptr::null(),
                ptr::null_mut(),
                0,
            )
        };
        if result.backing_handle.is_null() {
            return Err(format!(
                "CreateFileMapping2 failed for {} MiB: {}",
                backing_size >> 20,
                std::io::Error::last_os_error()
            ));
        }

        result.backing_base = unsafe {
            VirtualAlloc2(
                process,
                ptr::null(),
                backing_size,
                MEM_RESERVE | MEM_RESERVE_PLACEHOLDER,
                PAGE_NOACCESS,
                ptr::null_mut(),
                0,
            )
            .cast()
        };
        if result.backing_base.is_null() {
            return Err(format!(
                "VirtualAlloc2 failed to reserve {} MiB of backing memory: {}",
                backing_size >> 20,
                std::io::Error::last_os_error()
            ));
        }

        let backing_view = unsafe {
            MapViewOfFile3(
                result.backing_handle,
                process,
                result.backing_base.cast(),
                0,
                backing_size,
                MEM_REPLACE_PLACEHOLDER,
                PAGE_READWRITE,
                ptr::null_mut(),
                0,
            )
        };
        if backing_view.Value != result.backing_base.cast() {
            return Err(format!(
                "MapViewOfFile3 failed to map {} MiB of backing memory: {}",
                backing_size >> 20,
                std::io::Error::last_os_error()
            ));
        }

        result.virtual_base = unsafe {
            VirtualAlloc2(
                process,
                ptr::null(),
                virtual_size,
                MEM_RESERVE | MEM_RESERVE_PLACEHOLDER,
                PAGE_NOACCESS,
                ptr::null_mut(),
                0,
            )
            .cast()
        };
        if result.virtual_base.is_null() {
            return Err(format!(
                "VirtualAlloc2 failed to reserve {} GiB of virtual memory: {}",
                virtual_size >> 30,
                std::io::Error::last_os_error()
            ));
        }

        Ok(result)
    }

    fn map(
        &self,
        virtual_offset: usize,
        host_offset: usize,
        length: usize,
        _perms: MemoryPermission,
    ) {
        let mut mappings = self.mappings();
        if !self.is_niche_placeholder(&mappings, virtual_offset, length) {
            self.split(virtual_offset, length);
        }
        assert!(self
            .find_overlapping(&mappings, virtual_offset, virtual_offset + length)
            .is_none());
        Self::track_mapping(&mut mappings, virtual_offset, host_offset, length);
        self.map_view(virtual_offset, host_offset, length);
    }

    fn unmap(&self, virtual_offset: usize, length: usize) {
        let mut mappings = self.mappings();
        while self.unmap_one_mapping(&mut mappings, virtual_offset, length) {}
    }

    fn protect(
        &self,
        virtual_offset: usize,
        length: usize,
        read: bool,
        write: bool,
        _execute: bool,
    ) {
        use windows_sys::Win32::System::Memory::{
            VirtualProtect, PAGE_NOACCESS, PAGE_READONLY, PAGE_READWRITE,
        };

        let new_flags = match (read, write) {
            (true, true) => PAGE_READWRITE,
            (true, false) => PAGE_READONLY,
            (false, false) => PAGE_NOACCESS,
            (false, true) => panic!(
                "unsupported Windows protection combination read={} write={}",
                read, write
            ),
        };
        let virtual_end = virtual_offset + length;
        let mappings = self.mappings();
        for (&start, mapping) in mappings.iter() {
            if mapping.end <= virtual_offset {
                continue;
            }
            if start >= virtual_end {
                break;
            }
            let offset = start.max(virtual_offset);
            let protect_length = mapping.end.min(virtual_end) - offset;
            let mut old_flags = 0;
            let success = unsafe {
                VirtualProtect(
                    self.virtual_base.add(offset).cast(),
                    protect_length,
                    new_flags,
                    &mut old_flags,
                )
            };
            if success == 0 {
                error!(
                    "Failed to change Windows virtual memory protection: {}",
                    std::io::Error::last_os_error()
                );
            }
        }
    }

    fn clear_backing_region(&self, _physical_offset: usize, _length: usize) -> bool {
        // Upstream: Windows cannot discard a range from the section mapping.
        false
    }

    fn enable_direct_mapped_address(&mut self) {
        panic!("EnableDirectMappedAddress is unreachable on Windows");
    }

    fn mappings(&self) -> MutexGuard<'_, BTreeMap<usize, WindowsMapping>> {
        self.mappings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn find_overlapping(
        &self,
        mappings: &BTreeMap<usize, WindowsMapping>,
        begin: usize,
        end: usize,
    ) -> Option<(usize, WindowsMapping)> {
        if let Some((&start, &mapping)) = mappings.range(..=begin).next_back() {
            if mapping.end > begin {
                return Some((start, mapping));
            }
        }
        mappings
            .range(begin..end)
            .next()
            .map(|(&start, &mapping)| (start, mapping))
    }

    fn unmap_one_mapping(
        &self,
        mappings: &mut BTreeMap<usize, WindowsMapping>,
        virtual_offset: usize,
        length: usize,
    ) -> bool {
        use windows_sys::Win32::System::Memory::{UnmapViewOfFile2, MEM_PRESERVE_PLACEHOLDER};

        let range_end = virtual_offset + length;
        let Some((mapping_begin, mapping)) =
            self.find_overlapping(mappings, virtual_offset, range_end)
        else {
            return false;
        };
        let mapping_end = mapping.end;
        let unmap_begin = virtual_offset.max(mapping_begin);
        let unmap_end = range_end.min(mapping_end);
        let split_left = unmap_begin > mapping_begin;
        let split_right = unmap_end < mapping_end;

        let previous_end = mappings
            .range(..mapping_begin)
            .next_back()
            .map(|(_, previous)| previous.end);
        let next_begin = mappings
            .range((mapping_begin + 1)..)
            .next()
            .map(|(&start, _)| start);

        let success = unsafe {
            UnmapViewOfFile2(
                self.process,
                windows_sys::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS {
                    Value: self.virtual_base.add(mapping_begin).cast(),
                },
                MEM_PRESERVE_PLACEHOLDER,
            )
        };
        if success == 0 {
            error!(
                "Failed to unmap Windows virtual memory placeholder: {}",
                std::io::Error::last_os_error()
            );
        }

        // Upstream's "panic region": partial unmaps must recreate both retained
        // aliases before any unrelated work can observe the temporary hole.
        if split_left || split_right {
            self.split(unmap_begin, unmap_end - unmap_begin);
        }
        if split_left {
            self.map_view(
                mapping_begin,
                mapping.host_offset,
                unmap_begin - mapping_begin,
            );
        }
        if split_right {
            self.map_view(
                unmap_end,
                mapping.host_offset + unmap_end - mapping_begin,
                mapping_end - unmap_end,
            );
        }

        let mut coalesce_begin = unmap_begin;
        if !split_left {
            coalesce_begin = previous_end.unwrap_or(0);
            if coalesce_begin != mapping_begin {
                self.coalesce(coalesce_begin, unmap_end - coalesce_begin);
            }
        }
        if !split_right {
            let next_begin = next_begin.unwrap_or(self.virtual_size);
            if mapping_end != next_begin {
                self.coalesce(coalesce_begin, next_begin - coalesce_begin);
            }
        }

        mappings.remove(&mapping_begin);
        if split_left {
            Self::track_mapping(
                mappings,
                mapping_begin,
                mapping.host_offset,
                unmap_begin - mapping_begin,
            );
        }
        if split_right {
            Self::track_mapping(
                mappings,
                unmap_end,
                mapping.host_offset + unmap_end - mapping_begin,
                mapping_end - unmap_end,
            );
        }
        true
    }

    fn map_view(&self, virtual_offset: usize, host_offset: usize, length: usize) {
        use windows_sys::Win32::System::Memory::{
            MapViewOfFile3, MEM_REPLACE_PLACEHOLDER, PAGE_READWRITE,
        };
        let expected = unsafe { self.virtual_base.add(virtual_offset) };
        let view = unsafe {
            MapViewOfFile3(
                self.backing_handle,
                self.process,
                expected.cast(),
                host_offset as u64,
                length,
                MEM_REPLACE_PLACEHOLDER,
                PAGE_READWRITE,
                ptr::null_mut(),
                0,
            )
        };
        if view.Value != expected.cast() {
            error!(
                "Failed to map Windows placeholder: {}",
                std::io::Error::last_os_error()
            );
        }
    }

    fn split(&self, virtual_offset: usize, length: usize) {
        use windows_sys::Win32::System::Memory::{
            VirtualFreeEx, MEM_PRESERVE_PLACEHOLDER, MEM_RELEASE,
        };
        let success = unsafe {
            VirtualFreeEx(
                self.process,
                self.virtual_base.add(virtual_offset).cast(),
                length,
                MEM_RELEASE | MEM_PRESERVE_PLACEHOLDER,
            )
        };
        if success == 0 {
            error!(
                "Failed to split Windows placeholder: {}",
                std::io::Error::last_os_error()
            );
        }
    }

    fn coalesce(&self, virtual_offset: usize, length: usize) {
        use windows_sys::Win32::System::Memory::{VirtualFreeEx, MEM_RELEASE};
        use windows_sys::Win32::System::SystemServices::MEM_COALESCE_PLACEHOLDERS;
        let success = unsafe {
            VirtualFreeEx(
                self.process,
                self.virtual_base.add(virtual_offset).cast(),
                length,
                MEM_RELEASE | MEM_COALESCE_PLACEHOLDERS,
            )
        };
        if success == 0 {
            error!(
                "Failed to coalesce Windows placeholders: {}",
                std::io::Error::last_os_error()
            );
        }
    }

    fn is_niche_placeholder(
        &self,
        mappings: &BTreeMap<usize, WindowsMapping>,
        virtual_offset: usize,
        length: usize,
    ) -> bool {
        let end = virtual_offset + length;
        let Some((&next_begin, _)) = mappings.range(end..).next() else {
            return false;
        };
        if next_begin != end {
            return false;
        }
        virtual_offset == 0
            || mappings
                .range(..next_begin)
                .next_back()
                .is_some_and(|(_, previous)| previous.end == virtual_offset)
    }

    fn track_mapping(
        mappings: &mut BTreeMap<usize, WindowsMapping>,
        virtual_offset: usize,
        host_offset: usize,
        length: usize,
    ) {
        let previous = mappings.insert(
            virtual_offset,
            WindowsMapping {
                end: virtual_offset + length,
                host_offset,
            },
        );
        assert!(previous.is_none());
    }
}

#[cfg(target_os = "windows")]
impl Drop for HostMemoryImpl {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Memory::{
            UnmapViewOfFile2, VirtualFree, VirtualFreeEx, MEMORY_MAPPED_VIEW_ADDRESS,
            MEM_PRESERVE_PLACEHOLDER, MEM_RELEASE,
        };

        let mappings = self.mappings();
        if !mappings.is_empty() && !self.virtual_base.is_null() {
            for &start in mappings.keys() {
                unsafe {
                    UnmapViewOfFile2(
                        self.process,
                        MEMORY_MAPPED_VIEW_ADDRESS {
                            Value: self.virtual_base.add(start).cast(),
                        },
                        MEM_PRESERVE_PLACEHOLDER,
                    );
                }
            }
            self.coalesce(0, self.virtual_size);
        }
        drop(mappings);

        unsafe {
            if !self.virtual_base.is_null() {
                VirtualFree(self.virtual_base.cast(), 0, MEM_RELEASE);
                self.virtual_base = ptr::null_mut();
            }
            if !self.backing_base.is_null() {
                UnmapViewOfFile2(
                    self.process,
                    MEMORY_MAPPED_VIEW_ADDRESS {
                        Value: self.backing_base.cast(),
                    },
                    MEM_PRESERVE_PLACEHOLDER,
                );
                VirtualFreeEx(self.process, self.backing_base.cast(), 0, MEM_RELEASE);
                self.backing_base = ptr::null_mut();
            }
            if !self.backing_handle.is_null() {
                CloseHandle(self.backing_handle);
                self.backing_handle = ptr::null_mut();
            }
        }
    }
}

/// Platform-specific implementation of host memory management.
#[cfg(any(target_os = "linux", target_os = "macos"))]
struct HostMemoryImpl {
    backing_size: usize,
    virtual_size: usize,
    backing_base: *mut u8,
    virtual_base: *mut u8,
    virtual_map_base: *mut u8,
    fd: i32,
    free_manager: FreeRegionManager,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl HostMemoryImpl {
    fn new(backing_size: usize, virtual_size: usize) -> Result<Self, String> {
        unsafe {
            // Verify page size
            let page_size = libc::sysconf(libc::_SC_PAGESIZE);
            if page_size != 0x1000 {
                return Err(format!(
                    "page size {:#x} is incompatible with 4K paging",
                    page_size
                ));
            }

            let fd = create_backing_fd()?;
            if fd < 0 {
                return Err(format!(
                    "create backing fd failed: {}",
                    std::io::Error::last_os_error()
                ));
            }

            // Extend the file to backing_size
            let ret = libc::ftruncate(fd, backing_size as libc::off_t);
            if ret != 0 {
                libc::close(fd);
                return Err(format!(
                    "ftruncate failed: {}",
                    std::io::Error::last_os_error()
                ));
            }

            // Map backing memory
            let backing_base = libc::mmap(
                ptr::null_mut(),
                backing_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            );
            if backing_base == libc::MAP_FAILED {
                libc::close(fd);
                return Err(format!(
                    "mmap backing failed: {}",
                    std::io::Error::last_os_error()
                ));
            }

            // Map virtual address space
            let virtual_map_base = libc::mmap(
                ptr::null_mut(),
                virtual_size,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | map_noreserve(),
                -1,
                0,
            );
            if virtual_map_base == libc::MAP_FAILED {
                libc::munmap(backing_base, backing_size);
                libc::close(fd);
                return Err(format!(
                    "mmap virtual failed: {}",
                    std::io::Error::last_os_error()
                ));
            }

            // Enable huge pages (skip when RUZU_NO_HUGEPAGE=1; diagnostic for
            // multi-thread mmap THP interactions).
            #[cfg(target_os = "linux")]
            if std::env::var_os("RUZU_NO_HUGEPAGE").is_none() {
                libc::madvise(virtual_map_base, virtual_size, libc::MADV_HUGEPAGE);
            }

            let free_manager = FreeRegionManager::new();
            free_manager.set_address_space(virtual_map_base as *mut u8, virtual_size);

            Ok(Self {
                backing_size,
                virtual_size,
                backing_base: backing_base as *mut u8,
                virtual_base: virtual_map_base as *mut u8,
                virtual_map_base: virtual_map_base as *mut u8,
                fd,
                free_manager,
            })
        }
    }

    fn map(
        &self,
        virtual_offset: usize,
        host_offset: usize,
        length: usize,
        perms: MemoryPermission,
    ) {
        let (mut vo, mut len) = (virtual_offset, length);
        self.adjust_map(&mut vo, &mut len);
        if len == 0 {
            return;
        }

        // Remove from free regions
        unsafe {
            self.free_manager
                .allocate_block(self.virtual_base.add(vo), len);
        }

        // Deduce protection flags
        let mut flags = libc::PROT_NONE;
        if perms.contains(MemoryPermission::READ) {
            flags |= libc::PROT_READ;
        }
        if perms.contains(MemoryPermission::WRITE) {
            flags |= libc::PROT_WRITE;
        }

        unsafe {
            let ret = libc::mmap(
                self.virtual_base.add(vo) as *mut libc::c_void,
                len,
                flags,
                libc::MAP_SHARED | libc::MAP_FIXED,
                self.fd,
                host_offset as libc::off_t,
            );
            assert!(ret != libc::MAP_FAILED, "mmap failed during Map");
            // RUZU_TRACE_HOST_MMAP=0xVADDR — log HostMemoryImpl::map calls
            // whose `vo` (= passed virtual_offset) covers the GUEST vaddr.
            // Used to verify fastmem arena mapping actually happens for
            // the mstate region.
            if let Ok(spec) = std::env::var("RUZU_TRACE_HOST_MMAP") {
                if let Ok(target_vaddr) =
                    u64::from_str_radix(spec.trim().trim_start_matches("0x"), 16)
                {
                    // `vo` is the offset within virtual_base; for our setup,
                    // it equals the guest vaddr (the JIT uses R13 + vaddr).
                    if (vo as u64) <= target_vaddr && target_vaddr < (vo as u64) + len as u64 {
                        let mmap_target_va = self.virtual_base.add(vo) as u64;
                        eprintln!(
                            "[HOST_MMAP] virtual_base={:p} vo=0x{:X} mmap_target_host_va=0x{:X} len=0x{:X} fd={} host_offset=0x{:X} flags=0x{:X} perms={:?} ret={:p}",
                            self.virtual_base, vo, mmap_target_va, len, self.fd, host_offset, flags, perms, ret
                        );
                    }
                }
            }
        }
    }

    fn unmap(&self, virtual_offset: usize, length: usize) {
        let (mut vo, mut len) = (virtual_offset, length);
        self.adjust_map(&mut vo, &mut len);
        if len == 0 {
            return;
        }

        // Merge with adjacent placeholder mappings
        let (merged_pointer, merged_size) =
            unsafe { self.free_manager.free_block(self.virtual_base.add(vo), len) };

        unsafe {
            let ret = libc::mmap(
                merged_pointer as *mut libc::c_void,
                merged_size,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED,
                -1,
                0,
            );
            assert!(ret != libc::MAP_FAILED, "mmap failed during Unmap");
        }
    }

    fn protect(
        &self,
        virtual_offset: usize,
        length: usize,
        read: bool,
        write: bool,
        _execute: bool,
    ) {
        // RUZU_TRACE_HOST_MMAP=0xVADDR — also log protect calls covering it.
        if let Ok(spec) = std::env::var("RUZU_TRACE_HOST_MMAP") {
            if let Ok(target_vaddr) = u64::from_str_radix(spec.trim().trim_start_matches("0x"), 16)
            {
                if (virtual_offset as u64) <= target_vaddr
                    && target_vaddr < (virtual_offset as u64) + length as u64
                {
                    eprintln!(
                        "[HOST_PROTECT] vo=0x{:X} len=0x{:X} read={} write={}",
                        virtual_offset, length, read, write
                    );
                }
            }
        }
        let (mut vo, mut len) = (virtual_offset, length);
        self.adjust_map(&mut vo, &mut len);
        if len == 0 {
            return;
        }

        let mut flags = libc::PROT_NONE;
        if read {
            flags |= libc::PROT_READ;
        }
        if write {
            flags |= libc::PROT_WRITE;
        }

        unsafe {
            let ret = libc::mprotect(self.virtual_base.add(vo) as *mut libc::c_void, len, flags);
            assert!(ret == 0, "mprotect failed");
        }
    }

    fn clear_backing_region(&self, physical_offset: usize, length: usize) -> bool {
        #[cfg(target_os = "linux")]
        unsafe {
            let ret = libc::madvise(
                self.backing_base.add(physical_offset) as *mut libc::c_void,
                length,
                libc::MADV_REMOVE,
            );
            assert!(ret == 0, "madvise MADV_REMOVE failed");
        }
        #[cfg(target_os = "linux")]
        {
            true
        }
        #[cfg(target_os = "macos")]
        {
            let _ = physical_offset;
            let _ = length;
            false
        }
    }

    #[cfg(target_os = "linux")]
    fn enable_direct_mapped_address(&mut self) {
        self.virtual_base = ptr::null_mut();
    }

    fn adjust_map(&self, virtual_offset: &mut usize, length: &mut usize) {
        if !self.virtual_base.is_null() {
            return;
        }

        let intended_start = *virtual_offset;
        let intended_end = intended_start + *length;
        let address_space_start = self.virtual_map_base as usize;
        let address_space_end = address_space_start + self.virtual_size;

        if address_space_start > intended_end || intended_start > address_space_end {
            *virtual_offset = 0;
            *length = 0;
        } else {
            *virtual_offset = std::cmp::max(intended_start, address_space_start);
            *length = std::cmp::min(intended_end, address_space_end) - *virtual_offset;
        }
    }
}

#[cfg(target_os = "linux")]
fn create_backing_fd() -> Result<i32, String> {
    unsafe {
        let name = b"HostMemory\0";
        let fd = libc::syscall(libc::SYS_memfd_create, name.as_ptr(), 0) as i32;
        if fd < 0 {
            Err(format!(
                "memfd_create failed: {}",
                std::io::Error::last_os_error()
            ))
        } else {
            Ok(fd)
        }
    }
}

#[cfg(target_os = "macos")]
fn create_backing_fd() -> Result<i32, String> {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let name = CString::new(format!("/ruzu-host-memory-{}-{}", std::process::id(), id))
        .map_err(|e| e.to_string())?;
    unsafe {
        let fd = libc::shm_open(
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
            0o600,
        );
        if fd < 0 {
            return Err(format!(
                "shm_open failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        libc::shm_unlink(name.as_ptr());
        Ok(fd)
    }
}

#[cfg(target_os = "linux")]
fn map_noreserve() -> i32 {
    libc::MAP_NORESERVE
}

#[cfg(target_os = "macos")]
fn map_noreserve() -> i32 {
    0
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Drop for HostMemoryImpl {
    fn drop(&mut self) {
        unsafe {
            if self.virtual_map_base != libc::MAP_FAILED as *mut u8
                && !self.virtual_map_base.is_null()
            {
                libc::munmap(
                    self.virtual_map_base as *mut libc::c_void,
                    self.virtual_size,
                );
            }
            if self.backing_base != libc::MAP_FAILED as *mut u8 && !self.backing_base.is_null() {
                libc::munmap(self.backing_base as *mut libc::c_void, self.backing_size);
            }
            if self.fd != -1 {
                libc::close(self.fd);
            }
        }
    }
}

/// A low level linear memory buffer, which supports multiple mappings.
/// Its purpose is to rebuild a given sparse memory layout, including mirrors.
pub struct HostMemory {
    backing_size: usize,
    virtual_size: usize,
    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    imp: Option<HostMemoryImpl>,
    backing_base: *mut u8,
    virtual_base: *mut u8,
    virtual_base_offset: usize,
    /// Fallback if fastmem is not supported.
    /// Kept alive to ensure the backing memory is not freed.
    #[allow(dead_code)]
    fallback_buffer: Option<VirtualBuffer<u8>>,
}

impl HostMemory {
    pub fn new(backing_size: usize, virtual_size: usize) -> Self {
        let aligned_backing = align_up(backing_size as u64, PAGE_ALIGNMENT as u64) as usize;
        let aligned_virtual =
            align_up(virtual_size as u64, PAGE_ALIGNMENT as u64) as usize + HUGE_PAGE_SIZE;

        #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
        {
            match HostMemoryImpl::new(aligned_backing, aligned_virtual) {
                Ok(imp) => {
                    let backing_base = imp.backing_base;
                    let mut virtual_base = imp.virtual_base;
                    let mut virtual_base_offset = 0;

                    if !virtual_base.is_null() {
                        // Ensure virtual base is aligned to HUGE_PAGE_SIZE
                        let aligned =
                            align_up(virtual_base as u64, HUGE_PAGE_SIZE as u64) as *mut u8;
                        virtual_base_offset = aligned as usize - virtual_base as usize;
                        virtual_base = aligned;
                    }

                    return Self {
                        backing_size,
                        virtual_size,
                        imp: Some(imp),
                        backing_base,
                        virtual_base,
                        virtual_base_offset,
                        fallback_buffer: None,
                    };
                }
                Err(e) => {
                    error!("Fastmem unavailable ({}), falling back to VirtualBuffer", e);
                }
            }
        }

        // Fallback path
        let mut fallback = VirtualBuffer::<u8>::with_count(backing_size);
        let backing_base = fallback.data_mut();
        Self {
            backing_size,
            virtual_size,
            #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
            imp: None,
            backing_base,
            virtual_base: ptr::null_mut(),
            virtual_base_offset: 0,
            fallback_buffer: Some(fallback),
        }
    }

    pub fn map(
        &self,
        virtual_offset: usize,
        host_offset: usize,
        length: usize,
        perms: MemoryPermission,
        _separate_heap: bool,
    ) {
        assert!(virtual_offset % PAGE_ALIGNMENT == 0);
        assert!(host_offset % PAGE_ALIGNMENT == 0);
        assert!(length % PAGE_ALIGNMENT == 0);
        assert!(virtual_offset + length <= self.virtual_size);
        assert!(
            host_offset + length <= self.backing_size,
            "host_memory::map: host_offset=0x{:X} + length=0x{:X} > backing_size=0x{:X} (virtual_offset=0x{:X}, virtual_size=0x{:X})",
            host_offset, length, self.backing_size, virtual_offset, self.virtual_size
        );

        if length == 0 || self.virtual_base.is_null() {
            return;
        }

        #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
        if let Some(ref imp) = self.imp {
            imp.map(
                virtual_offset + self.virtual_base_offset,
                host_offset,
                length,
                perms,
            );
        }
    }

    pub fn unmap(&self, virtual_offset: usize, length: usize, _separate_heap: bool) {
        assert!(virtual_offset % PAGE_ALIGNMENT == 0);
        assert!(length % PAGE_ALIGNMENT == 0);
        assert!(virtual_offset + length <= self.virtual_size);

        if length == 0 || self.virtual_base.is_null() {
            return;
        }

        #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
        if let Some(ref imp) = self.imp {
            imp.unmap(virtual_offset + self.virtual_base_offset, length);
        }
    }

    pub fn protect(&self, virtual_offset: usize, length: usize, perm: MemoryPermission) {
        assert!(virtual_offset % PAGE_ALIGNMENT == 0);
        assert!(length % PAGE_ALIGNMENT == 0);
        assert!(virtual_offset + length <= self.virtual_size);

        if length == 0 || self.virtual_base.is_null() {
            return;
        }

        let read = perm.contains(MemoryPermission::READ);
        let write = perm.contains(MemoryPermission::WRITE);
        let execute = perm.contains(MemoryPermission::EXECUTE);

        #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
        if let Some(ref imp) = self.imp {
            imp.protect(
                virtual_offset + self.virtual_base_offset,
                length,
                read,
                write,
                execute,
            );
        }
    }

    pub fn clear_backing_region(&self, physical_offset: usize, length: usize, fill_value: u32) {
        #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
        {
            if fill_value == 0 {
                if let Some(ref imp) = self.imp {
                    if imp.clear_backing_region(physical_offset, length) {
                        return;
                    }
                }
            }
        }

        // Fallback: memset
        unsafe {
            ptr::write_bytes(
                self.backing_base.add(physical_offset),
                fill_value as u8,
                length,
            );
        }
    }

    pub fn enable_direct_mapped_address(&mut self) {
        #[cfg(target_os = "linux")]
        if let Some(ref mut imp) = self.imp {
            imp.enable_direct_mapped_address();
            self.virtual_size += self.virtual_base as usize;
        }
        #[cfg(target_os = "windows")]
        if let Some(ref mut imp) = self.imp {
            imp.enable_direct_mapped_address();
        }
    }

    pub fn backing_base_pointer(&self) -> *mut u8 {
        self.backing_base
    }

    pub fn backing_size(&self) -> usize {
        self.backing_size
    }

    pub fn virtual_base_pointer(&self) -> *mut u8 {
        self.virtual_base
    }

    pub fn is_in_virtual_range(&self, address: *const u8) -> bool {
        let addr = address as usize;
        let base = self.virtual_base as usize;
        addr >= base && addr < base + self.virtual_size
    }
}

// Safety: HostMemory manages raw pointers to mmap'd memory regions.
// The memory is allocated and freed in a controlled manner.
unsafe impl Send for HostMemory {}
unsafe impl Sync for HostMemory {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_host_memory() {
        // Create a small host memory (backing + virtual)
        let hm = HostMemory::new(0x100000, 0x200000);
        assert!(!hm.backing_base_pointer().is_null());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_placeholder_mapping_aliases_backing_memory() {
        let hm = HostMemory::new(0x20_000, 0x40_000);
        assert!(!hm.virtual_base_pointer().is_null());

        hm.map(
            0x10_000,
            0x4_000,
            0x2_000,
            MemoryPermission::READ_WRITE,
            false,
        );

        unsafe {
            hm.backing_base_pointer().add(0x4_123).write(0x5a);
            assert_eq!(hm.virtual_base_pointer().add(0x10_123).read(), 0x5a);

            hm.virtual_base_pointer().add(0x11_abc).write(0xc3);
            assert_eq!(hm.backing_base_pointer().add(0x5_abc).read(), 0xc3);
        }

        hm.unmap(0x10_000, 0x2_000, false);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_partial_unmap_preserves_both_alias_fragments() {
        let hm = HostMemory::new(0x40_000, 0x80_000);
        assert!(!hm.virtual_base_pointer().is_null());

        hm.map(
            0x20_000,
            0x8_000,
            0x3_000,
            MemoryPermission::READ_WRITE,
            false,
        );
        hm.unmap(0x21_000, 0x1_000, false);

        unsafe {
            hm.backing_base_pointer().add(0x8_123).write(0x19);
            hm.backing_base_pointer().add(0xa_456).write(0x73);
            assert_eq!(hm.virtual_base_pointer().add(0x20_123).read(), 0x19);
            assert_eq!(hm.virtual_base_pointer().add(0x22_456).read(), 0x73);
        }

        hm.map(
            0x21_000,
            0x10_000,
            0x1_000,
            MemoryPermission::READ_WRITE,
            false,
        );
        unsafe {
            hm.virtual_base_pointer().add(0x21_abc).write(0xd4);
            assert_eq!(hm.backing_base_pointer().add(0x10_abc).read(), 0xd4);
        }

        hm.unmap(0x20_000, 0x3_000, false);
    }
}
