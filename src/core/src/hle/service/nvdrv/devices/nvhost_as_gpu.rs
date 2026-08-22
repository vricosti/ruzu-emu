// SPDX-FileCopyrightText: 2021 yuzu Emulator Project
// SPDX-FileCopyrightText: 2021 Skyline Team and Contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of eden/src/core/hle/service/nvdrv/devices/nvhost_as_gpu.h
//! Port of eden/src/core/hle/service/nvdrv/devices/nvhost_as_gpu.cpp

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};

use common::address_space::FlatAllocator;

use crate::core::SystemRef;
use crate::gpu_core::GpuMemoryManagerHandle;
use crate::hle::service::nvdrv::core::container::{Container, SessionId};
use crate::hle::service::nvdrv::core::nvmap::NvMap;
use crate::hle::service::nvdrv::devices::nvdevice::NvDevice;
use crate::hle::service::nvdrv::devices::nvmap::{read_struct, write_struct};
use crate::hle::service::nvdrv::nvdata::{DeviceFD, Ioctl, NvResult};
use crate::hle::service::nvdrv::nvdrv::Module;

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct MappingFlags: u32 {
        const NONE = 0;
        const FIXED = 1 << 0;
        const SPARSE = 1 << 1;
        const REMAP = 1 << 8;
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VaRegion {
    pub offset: u64,
    pub page_size: u32,
    pub _pad0: u32,
    pub pages: u64,
}
const _: () = assert!(std::mem::size_of::<VaRegion>() == 0x18);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlAllocAsEx {
    pub flags: u32,
    pub as_fd: i32,
    pub big_page_size: u32,
    pub reserved: u32,
    pub va_range_start: u64,
    pub va_range_end: u64,
    pub va_range_split: u64,
}
const _: () = assert!(std::mem::size_of::<IoctlAllocAsEx>() == 40);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlAllocSpace {
    pub pages: u32,
    pub page_size: u32,
    pub flags: u32,
    pub _pad: u32,
    pub offset: u64,
}
const _: () = assert!(std::mem::size_of::<IoctlAllocSpace>() == 24);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlFreeSpace {
    pub offset: u64,
    pub pages: u32,
    pub page_size: u32,
}
const _: () = assert!(std::mem::size_of::<IoctlFreeSpace>() == 16);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlRemapEntry {
    pub flags: u16,
    pub kind: u16,
    pub handle: u32,
    pub handle_offset_big_pages: u32,
    pub as_offset_big_pages: u32,
    pub big_pages: u32,
}
const _: () = assert!(std::mem::size_of::<IoctlRemapEntry>() == 20);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlMapBufferEx {
    pub flags: u32,
    pub kind: u32,
    pub handle: u32,
    pub page_size: u32,
    pub buffer_offset: i64,
    pub mapping_size: u64,
    pub offset: i64,
}
const _: () = assert!(std::mem::size_of::<IoctlMapBufferEx>() == 40);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlUnmapBuffer {
    pub offset: i64,
}
const _: () = assert!(std::mem::size_of::<IoctlUnmapBuffer>() == 8);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlBindChannel {
    pub fd: i32,
}
const _: () = assert!(std::mem::size_of::<IoctlBindChannel>() == 0x4);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct IoctlGetVaRegions {
    pub buf_addr: u64,
    pub buf_size: u32,
    pub reserved: u32,
    pub regions: [VaRegion; 2],
}
const _: () =
    assert!(std::mem::size_of::<IoctlGetVaRegions>() == 16 + std::mem::size_of::<VaRegion>() * 2);

struct Mapping {
    handle: u32,
    ptr: u64,
    offset: u64,
    size: u64,
    fixed: bool,
    big_page: bool,
    sparse_alloc: bool,
}

struct Allocation {
    size: u64,
    mappings: Vec<Arc<Mapping>>,
    page_size: u32,
    sparse: bool,
    big_pages: bool,
}

struct VM {
    big_page_size: u32,
    big_page_size_bits: u32,
    va_range_start: u64,
    va_range_split: u64,
    va_range_end: u64,
    small_page_allocator: Option<FlatAllocator>,
    big_page_allocator: Option<FlatAllocator>,
    initialised: bool,
}

impl Default for VM {
    fn default() -> Self {
        const DEFAULT_BIG_PAGE_SIZE: u32 = 0x20000;
        Self {
            big_page_size: DEFAULT_BIG_PAGE_SIZE,
            big_page_size_bits: DEFAULT_BIG_PAGE_SIZE.trailing_zeros(),
            va_range_start: (DEFAULT_BIG_PAGE_SIZE as u64) << 10,
            va_range_split: 1u64 << 34,
            va_range_end: 1u64 << 37,
            small_page_allocator: None,
            big_page_allocator: None,
            initialised: false,
        }
    }
}

impl VM {
    const YUZU_PAGESIZE: u32 = 0x1000;
    const PAGE_SIZE_BITS: u32 = 12; // countr_zero(0x1000)
    const SUPPORTED_BIG_PAGE_SIZES: u32 = 0x30000;
    const VA_START_SHIFT: u32 = 10;
}

/// nvhost_as_gpu device.
pub struct NvHostAsGpu {
    system: SystemRef,
    module: *const Module,
    nvmap: *const NvMap,
    mutex: Mutex<()>,
    vm: Mutex<VM>,
    map_buffer_offsets: Mutex<HashSet<i64>>,
    mapping_map: Mutex<BTreeMap<u64, Arc<Mapping>>>,
    allocation_map: Mutex<BTreeMap<u64, Allocation>>,
    gmmu: Mutex<Option<Arc<dyn GpuMemoryManagerHandle>>>,
}

// Safety: the device is accessed through the nvdrv service thread, matching the
// ownership model used by other nvdrv device wrappers that retain raw pointers
// to the module and its NvMap state.
unsafe impl Send for NvHostAsGpu {}
unsafe impl Sync for NvHostAsGpu {}

impl NvHostAsGpu {
    fn should_trace_map_loop() -> bool {
        std::env::var_os("RUZU_TRACE_NVHOST_AS_MAP")
            .is_some_and(|value| value != std::ffi::OsStr::new("0"))
    }

    pub fn new(system: SystemRef, module: &Module, container: &Container) -> Self {
        Self {
            system,
            module: module as *const _,
            nvmap: container.get_nv_map_file() as *const _,
            mutex: Mutex::new(()),
            vm: Mutex::new(VM::default()),
            map_buffer_offsets: Mutex::new(HashSet::new()),
            mapping_map: Mutex::new(BTreeMap::new()),
            allocation_map: Mutex::new(BTreeMap::new()),
            gmmu: Mutex::new(None),
        }
    }

    fn module(&self) -> &Module {
        unsafe { &*self.module }
    }

    fn nvmap(&self) -> &NvMap {
        unsafe { &*self.nvmap }
    }

    fn free_mapping_locked(
        &self,
        vm: &mut VM,
        mapping_map: &mut BTreeMap<u64, Arc<Mapping>>,
        offset: u64,
    ) -> NvResult {
        let Some(mapping) = mapping_map.get(&offset).cloned() else {
            return NvResult::BadValue;
        };

        if !mapping.fixed {
            let allocator = if mapping.big_page {
                vm.big_page_allocator.as_mut()
            } else {
                vm.small_page_allocator.as_mut()
            };
            if let Some(allocator) = allocator {
                let page_size_bits = if mapping.big_page {
                    vm.big_page_size_bits
                } else {
                    VM::PAGE_SIZE_BITS
                };
                let page_size = if mapping.big_page {
                    vm.big_page_size
                } else {
                    VM::YUZU_PAGESIZE
                } as u64;
                let aligned_size = mapping.size.div_ceil(page_size) * page_size;
                allocator.free(
                    (mapping.offset >> page_size_bits) as u32,
                    (aligned_size >> page_size_bits) as u32,
                );
            }
        }

        self.nvmap().unpin_handle(mapping.handle);
        if let Some(gmmu) = self.gmmu.lock().unwrap().as_ref().cloned() {
            if mapping.sparse_alloc {
                gmmu.map_sparse(offset, mapping.size, mapping.big_page);
            } else {
                gmmu.unmap(offset, mapping.size);
            }
        }
        mapping_map.remove(&offset);
        NvResult::Success
    }

    pub fn alloc_as_ex(&self, params: &mut IoctlAllocAsEx) -> NvResult {
        log::debug!(
            "nvhost_as_gpu::AllocAsEx called, big_page_size=0x{:X}",
            params.big_page_size
        );
        log::debug!(
            "nvhost_as_gpu::AllocAsEx params flags=0x{:X} as_fd={} big_page_size=0x{:X} reserved=0x{:X} va_start=0x{:016X} va_end=0x{:016X} va_split=0x{:016X}",
            params.flags,
            params.as_fd,
            params.big_page_size,
            params.reserved,
            params.va_range_start,
            params.va_range_end,
            params.va_range_split
        );

        let _owner_guard = self.mutex.lock().unwrap();
        let mut vm = self.vm.lock().unwrap();

        if vm.initialised {
            log::error!("Cannot initialise an address space twice!");
            return NvResult::InvalidState;
        }

        if params.big_page_size != 0 {
            if !params.big_page_size.is_power_of_two() {
                log::error!(
                    "Non power-of-2 big page size: 0x{:X}!",
                    params.big_page_size
                );
                return NvResult::BadValue;
            }

            if (params.big_page_size & VM::SUPPORTED_BIG_PAGE_SIZES) == 0 {
                log::error!("Unsupported big page size: 0x{:X}!", params.big_page_size);
                return NvResult::BadValue;
            }

            vm.big_page_size = params.big_page_size;
            vm.big_page_size_bits = params.big_page_size.trailing_zeros();
            vm.va_range_start = (params.big_page_size as u64) << VM::VA_START_SHIFT;
        }

        // Upstream applies the custom VA range literally whenever start != 0.
        if params.va_range_start != 0 {
            vm.va_range_start = params.va_range_start;
            vm.va_range_split = params.va_range_split;
            vm.va_range_end = params.va_range_end;
        }

        let start_pages = (vm.va_range_start >> VM::PAGE_SIZE_BITS) as u32;
        let end_pages = (vm.va_range_split >> VM::PAGE_SIZE_BITS) as u32;
        vm.small_page_allocator = Some(FlatAllocator::new(start_pages, end_pages));

        let start_big_pages = (vm.va_range_split >> vm.big_page_size_bits) as u32;
        let end_big_pages =
            (vm.va_range_end.wrapping_sub(vm.va_range_split) >> vm.big_page_size_bits) as u32;
        vm.big_page_allocator = Some(FlatAllocator::new(start_big_pages, end_big_pages));

        let max_big_page_bits = 64 - (vm.va_range_end - 1).leading_zeros() as u64;
        let gmmu = self
            .system
            .get()
            .gpu_core()
            .expect("GPU core must be initialized before nvhost_as_gpu::AllocAsEx")
            .allocate_memory_manager_handle(
                max_big_page_bits,
                vm.va_range_split,
                vm.big_page_size_bits as u64,
                VM::PAGE_SIZE_BITS as u64,
            );
        self.system
            .get()
            .gpu_core()
            .expect("GPU core must remain initialized during nvhost_as_gpu::AllocAsEx")
            .init_address_space(gmmu.clone());
        *self.gmmu.lock().unwrap() = Some(gmmu);
        vm.initialised = true;

        NvResult::Success
    }

    pub fn allocate_space(&self, params: &mut IoctlAllocSpace) -> NvResult {
        log::debug!(
            "nvhost_as_gpu::AllocateSpace called, pages={:X}, page_size={:X}, flags={:X}",
            params.pages,
            params.page_size,
            params.flags
        );

        let _owner_guard = self.mutex.lock().unwrap();
        let mut vm = self.vm.lock().unwrap();
        if !vm.initialised {
            return NvResult::BadValue;
        }

        if params.page_size != VM::YUZU_PAGESIZE && params.page_size != vm.big_page_size {
            return NvResult::BadValue;
        }

        let flags = MappingFlags::from_bits_truncate(params.flags);

        if params.page_size != vm.big_page_size && flags.contains(MappingFlags::SPARSE) {
            log::error!("Sparse small pages are not implemented!");
            return NvResult::NotImplemented;
        }

        let page_size_bits = if params.page_size == VM::YUZU_PAGESIZE {
            VM::PAGE_SIZE_BITS
        } else {
            vm.big_page_size_bits
        };

        let allocator = if params.page_size == VM::YUZU_PAGESIZE {
            vm.small_page_allocator.as_mut()
        } else {
            vm.big_page_allocator.as_mut()
        };
        let Some(allocator) = allocator else {
            return NvResult::InvalidState;
        };

        if flags.contains(MappingFlags::FIXED) {
            allocator.allocate_fixed((params.offset >> page_size_bits) as u32, params.pages);
        } else {
            let Some(offset_pages) = allocator.allocate(params.pages) else {
                return NvResult::InsufficientMemory;
            };
            params.offset = (offset_pages as u64) << page_size_bits;
        }

        let size = (params.pages as u64) * (params.page_size as u64);
        if flags.contains(MappingFlags::SPARSE) {
            if let Some(gmmu) = self.gmmu.lock().unwrap().as_ref().cloned() {
                gmmu.map_sparse(params.offset, size, params.page_size != VM::YUZU_PAGESIZE);
            }
        }
        let mut alloc_map = self.allocation_map.lock().unwrap();
        alloc_map.insert(
            params.offset,
            Allocation {
                size,
                mappings: Vec::new(),
                page_size: params.page_size,
                sparse: flags.contains(MappingFlags::SPARSE),
                big_pages: params.page_size != VM::YUZU_PAGESIZE,
            },
        );
        log::debug!(
            "nvhost_as_gpu::AllocateSpace result offset=0x{:X} size=0x{:X} big_pages={}",
            params.offset,
            size,
            params.page_size != VM::YUZU_PAGESIZE
        );
        NvResult::Success
    }

    pub fn free_space(&self, params: &mut IoctlFreeSpace) -> NvResult {
        log::debug!(
            "nvhost_as_gpu::FreeSpace called, offset={:X}, pages={:X}, page_size={:X}",
            params.offset,
            params.pages,
            params.page_size
        );

        let _owner_guard = self.mutex.lock().unwrap();
        let mut vm = self.vm.lock().unwrap();
        if !vm.initialised {
            return NvResult::BadValue;
        }

        let mut alloc_map = self.allocation_map.lock().unwrap();
        let Some(allocation) = alloc_map.get(&params.offset) else {
            return NvResult::BadValue;
        };
        let allocation_size = allocation.size;
        let allocation_page_size = allocation.page_size;
        let allocation_sparse = allocation.sparse;
        let allocation_big_pages = allocation.big_pages;
        let allocation_mappings = allocation.mappings.clone();

        if allocation_page_size != params.page_size
            || allocation_size != (params.pages as u64) * (params.page_size as u64)
        {
            return NvResult::BadValue;
        }

        let mut mapping_map = self.mapping_map.lock().unwrap();
        for mapping in allocation_mappings {
            let result = self.free_mapping_locked(&mut vm, &mut mapping_map, mapping.offset);
            if !matches!(result, NvResult::Success) {
                return NvResult::BadValue;
            }
        }

        if allocation_sparse {
            if let Some(gmmu) = self.gmmu.lock().unwrap().as_ref().cloned() {
                gmmu.unmap(params.offset, allocation_size);
            }
        }
        let page_size_bits = if allocation_big_pages {
            vm.big_page_size_bits
        } else {
            VM::PAGE_SIZE_BITS
        };
        let allocator = if allocation_big_pages {
            vm.big_page_allocator.as_mut()
        } else {
            vm.small_page_allocator.as_mut()
        };
        if let Some(allocator) = allocator {
            allocator.free(
                (params.offset >> page_size_bits) as u32,
                (allocation_size >> page_size_bits) as u32,
            );
        }
        alloc_map.remove(&params.offset);

        NvResult::Success
    }

    pub fn remap(&self, entries: &[IoctlRemapEntry]) -> NvResult {
        log::debug!(
            "nvhost_as_gpu::Remap called, num_entries=0x{:X}",
            entries.len()
        );

        let _owner_guard = self.mutex.lock().unwrap();
        let vm = self.vm.lock().unwrap();
        if !vm.initialised {
            return NvResult::BadValue;
        }

        let alloc_map = self.allocation_map.lock().unwrap();
        for entry in entries {
            let virtual_address = (entry.as_offset_big_pages as u64) << vm.big_page_size_bits;
            let size = (entry.big_pages as u64) << vm.big_page_size_bits;

            let Some((alloc_start, alloc)) = alloc_map.range(..=virtual_address).next_back() else {
                log::warn!("Cannot remap into an unallocated region!");
                return NvResult::BadValue;
            };

            if virtual_address < *alloc_start
                || (virtual_address - *alloc_start) + size > alloc.size
            {
                log::warn!("Cannot remap into an unallocated region!");
                return NvResult::BadValue;
            }

            if !alloc.sparse {
                log::warn!("Cannot remap a non-sparse mapping!");
                return NvResult::BadValue;
            }

            let use_big_pages = alloc.big_pages;
            if entry.handle == 0 {
                if let Some(gmmu) = self.gmmu.lock().unwrap().as_ref().cloned() {
                    gmmu.map_sparse(virtual_address, size, use_big_pages);
                }
                log::debug!(
                    "nvhost_as_gpu::Remap sparse range gpu_va=0x{:X} size=0x{:X} big_pages={}",
                    virtual_address,
                    size,
                    use_big_pages
                );
                continue;
            }

            let Some(_handle) = self.nvmap().get_handle(entry.handle) else {
                return NvResult::BadValue;
            };

            let base = self.nvmap().pin_handle(entry.handle, false);
            if base == 0 {
                return NvResult::BadValue;
            }

            let device_address =
                base + ((entry.handle_offset_big_pages as u64) << vm.big_page_size_bits);
            if let Some(gmmu) = self.gmmu.lock().unwrap().as_ref().cloned() {
                gmmu.map(
                    virtual_address,
                    device_address,
                    size,
                    entry.kind as u32,
                    use_big_pages,
                );
            }
        }

        NvResult::Success
    }

    pub fn map_buffer_ex(&self, params: &mut IoctlMapBufferEx) -> NvResult {
        log::debug!(
            "nvhost_as_gpu::MapBufferEx called, flags={:X}, handle={:X}, offset={}",
            params.flags,
            params.handle,
            params.offset
        );
        if Self::should_trace_map_loop() {
            log::info!(
                "nvhost_as_gpu::MapBufferEx begin flags=0x{:X} handle=0x{:X} buffer_offset=0x{:X} mapping_size=0x{:X} offset=0x{:X} kind=0x{:X}",
                params.flags,
                params.handle,
                params.buffer_offset,
                params.mapping_size,
                params.offset,
                params.kind
            );
        }

        let _owner_guard = self.mutex.lock().unwrap();
        let mut vm = self.vm.lock().unwrap();
        if !vm.initialised {
            return NvResult::BadValue;
        }

        let flags = MappingFlags::from_bits_truncate(params.flags);

        if flags.contains(MappingFlags::REMAP) {
            let mapping_map = self.mapping_map.lock().unwrap();
            let Some(mapping) = mapping_map.get(&(params.offset as u64)) else {
                log::warn!(
                    "Cannot remap an unmapped GPU address space region: 0x{:X}",
                    params.offset
                );
                return NvResult::BadValue;
            };
            if mapping.size < params.mapping_size {
                log::warn!(
                    "Cannot remap a partially mapped GPU address space region: 0x{:X}",
                    params.offset
                );
                return NvResult::BadValue;
            }
            let gpu_address = (params.offset as u64).wrapping_add_signed(params.buffer_offset);
            let device_address = mapping.ptr.wrapping_add_signed(params.buffer_offset);
            if let Some(gmmu) = self.gmmu.lock().unwrap().as_ref().cloned() {
                gmmu.map(
                    gpu_address,
                    device_address,
                    params.mapping_size,
                    params.kind,
                    mapping.big_page,
                );
            }
            log::debug!(
                "nvhost_as_gpu::MapBufferEx remap gpu_va=0x{:X} device_address=0x{:X} size=0x{:X} big_page={}",
                gpu_address,
                device_address,
                params.mapping_size,
                mapping.big_page
            );
            if Self::should_trace_map_loop() {
                log::info!(
                    "nvhost_as_gpu::MapBufferEx remap gpu=0x{:X} dev=0x{:X} size=0x{:X} big_page={}",
                    gpu_address,
                    device_address,
                    params.mapping_size,
                    mapping.big_page
                );
            }
            Self::trace_gpu_va_map(
                params.handle,
                params.mapping_size,
                gpu_address,
                params.flags,
                params.kind,
            );
            return NvResult::Success;
        }

        let Some(handle) = self.nvmap().get_handle(params.handle) else {
            return NvResult::BadValue;
        };
        let (handle_align, handle_orig_size) = {
            let inner = handle.lock_inner();
            (inner.align, handle.orig_size())
        };
        let device_address = self
            .nvmap()
            .pin_handle(params.handle, false)
            .wrapping_add_signed(params.buffer_offset);
        if device_address == 0 {
            self.nvmap().unpin_handle(params.handle);
            return NvResult::BadValue;
        }

        let size = if params.mapping_size != 0 {
            params.mapping_size
        } else {
            handle_orig_size
        };

        let big_page = if handle_align % (vm.big_page_size as u64) == 0 {
            true
        } else if handle_align % (VM::YUZU_PAGESIZE as u64) == 0 {
            false
        } else {
            return NvResult::BadValue;
        };

        if flags.contains(MappingFlags::FIXED) {
            let mut alloc_map = self.allocation_map.lock().unwrap();
            let Some((alloc_start, alloc)) =
                alloc_map.range_mut(..=(params.offset as u64)).next_back()
            else {
                self.nvmap().unpin_handle(params.handle);
                return NvResult::BadValue;
            };
            if (params.offset as u64) < *alloc_start
                || ((params.offset as u64) - *alloc_start) + size > alloc.size
            {
                self.nvmap().unpin_handle(params.handle);
                return NvResult::BadValue;
            }
            let use_big_pages = alloc.big_pages && big_page;
            let offset = params.offset as u64;
            if let Some(gmmu) = self.gmmu.lock().unwrap().as_ref().cloned() {
                gmmu.map(offset, device_address, size, params.kind, use_big_pages);
            }
            let sparse_alloc = alloc.sparse;
            let mapping = Arc::new(Mapping {
                handle: params.handle,
                ptr: device_address,
                offset,
                size,
                fixed: true,
                big_page: use_big_pages,
                sparse_alloc,
            });
            alloc.mappings.push(Arc::clone(&mapping));
            let alloc_big_pages = alloc.big_pages;
            drop(alloc_map);
            self.mapping_map.lock().unwrap().insert(offset, mapping);
            log::debug!(
                "nvhost_as_gpu::MapBufferEx fixed result handle={} gpu_offset=0x{:X} device_address=0x{:X} size=0x{:X} big_page={}",
                params.handle,
                offset,
                device_address,
                size,
                use_big_pages
            );
            if Self::should_trace_map_loop() {
                log::info!(
                    "nvhost_as_gpu::MapBufferEx fixed gpu=0x{:X} dev=0x{:X} size=0x{:X} use_big_pages={} alloc_big_pages={}",
                    offset,
                    device_address,
                    size,
                    use_big_pages,
                    alloc_big_pages
                );
            }
            Self::trace_gpu_va_map(params.handle, size, offset, params.flags, params.kind);
            self.map_buffer_offsets
                .lock()
                .unwrap()
                .insert(params.offset);
            return NvResult::Success;
        }

        let page_size = if big_page {
            vm.big_page_size
        } else {
            VM::YUZU_PAGESIZE
        };
        let page_size_bits = if big_page {
            vm.big_page_size_bits
        } else {
            VM::PAGE_SIZE_BITS
        };
        let allocator = if big_page {
            vm.big_page_allocator.as_mut()
        } else {
            vm.small_page_allocator.as_mut()
        };
        let Some(allocator) = allocator else {
            self.nvmap().unpin_handle(params.handle);
            return NvResult::InvalidState;
        };
        let aligned_size = (size + page_size as u64 - 1) & !((page_size as u64) - 1);
        let Some(offset_pages) = allocator.allocate((aligned_size >> page_size_bits) as u32) else {
            self.nvmap().unpin_handle(params.handle);
            return NvResult::InsufficientMemory;
        };
        params.offset = ((offset_pages as u64) << page_size_bits) as i64;
        if let Some(gmmu) = self.gmmu.lock().unwrap().as_ref().cloned() {
            gmmu.map(
                params.offset as u64,
                device_address,
                aligned_size,
                params.kind,
                big_page,
            );
        }
        self.mapping_map.lock().unwrap().insert(
            params.offset as u64,
            Arc::new(Mapping {
                handle: params.handle,
                ptr: device_address,
                offset: params.offset as u64,
                size,
                fixed: false,
                big_page,
                sparse_alloc: false,
            }),
        );
        log::debug!(
            "nvhost_as_gpu::MapBufferEx dynamic result handle={} gpu_offset=0x{:X} device_address=0x{:X} size=0x{:X} big_page={}",
            params.handle,
            params.offset,
            device_address,
            size,
            big_page
        );
        if Self::should_trace_map_loop() {
            log::info!(
                "nvhost_as_gpu::MapBufferEx alloc gpu=0x{:X} dev=0x{:X} size=0x{:X} aligned=0x{:X} big_page={}",
                params.offset,
                device_address,
                size,
                aligned_size,
                big_page
            );
        }
        Self::trace_gpu_va_map(
            params.handle,
            size,
            params.offset as u64,
            params.flags,
            params.kind,
        );
        self.map_buffer_offsets
            .lock()
            .unwrap()
            .insert(params.offset);
        NvResult::Success
    }

    fn trace_gpu_va_map(handle: u32, size: u64, gpu_va: u64, flags: u32, kind: u32) {
        if !common::trace::is_enabled(common::trace::cat::GPU_VA_MAP) {
            return;
        }
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        common::trace::emit_raw(
            common::trace::cat::GPU_VA_MAP,
            &[
                seq as u64,
                handle as u64,
                size,
                gpu_va,
                flags as u64,
                kind as u64,
            ],
        );
    }

    fn trace_gpu_va_unmap(offset: u64) {
        if !common::trace::is_enabled(common::trace::cat::GPU_VA_UNMAP) {
            return;
        }
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        common::trace::emit_raw(common::trace::cat::GPU_VA_UNMAP, &[seq as u64, offset]);
    }

    pub fn unmap_buffer(&self, params: &mut IoctlUnmapBuffer) -> NvResult {
        log::debug!(
            "nvhost_as_gpu::UnmapBuffer called, offset=0x{:X}",
            params.offset
        );

        let _owner_guard = self.mutex.lock().unwrap();
        let mut map_buffer_offsets = self.map_buffer_offsets.lock().unwrap();
        if !map_buffer_offsets.contains(&params.offset) {
            return NvResult::Success;
        }
        let vm = self.vm.lock().unwrap();
        if !vm.initialised {
            return NvResult::BadValue;
        }
        drop(vm);

        let mut mapping_map = self.mapping_map.lock().unwrap();
        let Some(mapping) = mapping_map.get(&(params.offset as u64)).cloned() else {
            log::warn!(
                "Couldn't find region to unmap at 0x{:X}",
                params.offset as u64
            );
            return NvResult::Success;
        };

        if !mapping.fixed {
            let mut vm = self.vm.lock().unwrap();
            let page_size_bits = if mapping.big_page {
                vm.big_page_size_bits
            } else {
                VM::PAGE_SIZE_BITS
            };
            let allocator = if mapping.big_page {
                vm.big_page_allocator.as_mut()
            } else {
                vm.small_page_allocator.as_mut()
            };
            if let Some(allocator) = allocator {
                allocator.free(
                    (mapping.offset >> page_size_bits) as u32,
                    (mapping.size >> page_size_bits) as u32,
                );
            }
        }

        if let Some(gmmu) = self.gmmu.lock().unwrap().as_ref().cloned() {
            if mapping.sparse_alloc {
                gmmu.map_sparse(params.offset as u64, mapping.size, mapping.big_page);
            } else {
                gmmu.unmap(params.offset as u64, mapping.size);
            }
        }
        self.nvmap().unpin_handle(mapping.handle);
        mapping_map.remove(&(params.offset as u64));
        map_buffer_offsets.remove(&params.offset);
        Self::trace_gpu_va_unmap(params.offset as u64);
        NvResult::Success
    }

    pub fn bind_channel(&self, params: &mut IoctlBindChannel) -> NvResult {
        log::debug!("nvhost_as_gpu::BindChannel called, fd={:X}", params.fd);
        let Some(gpu_channel_device) = self.module().get_gpu_device(params.fd) else {
            return NvResult::BadParameter;
        };
        let Some(gmmu) = self.gmmu.lock().unwrap().as_ref().cloned() else {
            return NvResult::InvalidState;
        };

        gpu_channel_device.bind_address_space(gmmu);
        NvResult::Success
    }

    fn get_va_regions_impl(vm: &VM, params: &mut IoctlGetVaRegions) {
        let small_page_allocator = vm
            .small_page_allocator
            .as_ref()
            .expect("initialized VM must own its small-page allocator");
        let big_page_allocator = vm
            .big_page_allocator
            .as_ref()
            .expect("initialized VM must own its big-page allocator");

        params.buf_size = 2 * std::mem::size_of::<VaRegion>() as u32;
        // Match upstream's u32 overflow semantics: the shift happens on u32
        // (VaType) and silently truncates; the Switch runtime is compiled
        // assuming this overflow.
        params.regions[0] = VaRegion {
            offset: (small_page_allocator
                .get_va_start()
                .wrapping_shl(VM::PAGE_SIZE_BITS)) as u64,
            page_size: VM::YUZU_PAGESIZE,
            _pad0: 0,
            pages: (small_page_allocator.get_va_limit() - small_page_allocator.get_va_start())
                as u64,
        };
        params.regions[1] = VaRegion {
            offset: (big_page_allocator
                .get_va_start()
                .wrapping_shl(vm.big_page_size_bits)) as u64,
            page_size: vm.big_page_size,
            _pad0: 0,
            pages: (big_page_allocator.get_va_limit() - big_page_allocator.get_va_start()) as u64,
        };
    }

    pub fn get_va_regions1(&self, params: &mut IoctlGetVaRegions) -> NvResult {
        log::debug!(
            "nvhost_as_gpu::GetVARegions1 called, buf_addr={:X}, buf_size={:X}",
            params.buf_addr,
            params.buf_size
        );

        let _owner_guard = self.mutex.lock().unwrap();
        let vm = self.vm.lock().unwrap();
        if !vm.initialised {
            return NvResult::BadValue;
        }

        Self::get_va_regions_impl(&vm, params);
        log::debug!(
            "nvhost_as_gpu::GetVARegions result small=[0x{:X}, pages=0x{:X}] big=[0x{:X}, pages=0x{:X}, page_size=0x{:X}]",
            params.regions[0].offset,
            params.regions[0].pages,
            params.regions[1].offset,
            params.regions[1].pages,
            params.regions[1].page_size
        );

        NvResult::Success
    }

    pub fn get_va_regions3(
        &self,
        params: &mut IoctlGetVaRegions,
        regions: &mut [VaRegion],
    ) -> NvResult {
        log::debug!(
            "nvhost_as_gpu::GetVARegions3 called, buf_addr={:X}, buf_size={:X}",
            params.buf_addr,
            params.buf_size
        );

        let _owner_guard = self.mutex.lock().unwrap();
        let vm = self.vm.lock().unwrap();
        if !vm.initialised {
            return NvResult::BadValue;
        }

        Self::get_va_regions_impl(&vm, params);
        let num_regions = params.regions.len().min(regions.len());
        regions[..num_regions].copy_from_slice(&params.regions[..num_regions]);
        NvResult::Success
    }
}

impl NvDevice for NvHostAsGpu {
    fn ioctl1(&self, _fd: DeviceFD, command: Ioctl, input: &[u8], output: &mut [u8]) -> NvResult {
        match command.group() {
            b'A' => match command.cmd() {
                0x1 => {
                    let mut params: IoctlBindChannel = read_struct(input);
                    let r = self.bind_channel(&mut params);
                    write_struct(output, &params);
                    r
                }
                0x2 => {
                    let mut params: IoctlAllocSpace = read_struct(input);
                    let r = self.allocate_space(&mut params);
                    write_struct(output, &params);
                    r
                }
                0x3 => {
                    let mut params: IoctlFreeSpace = read_struct(input);
                    let r = self.free_space(&mut params);
                    write_struct(output, &params);
                    r
                }
                0x5 => {
                    let mut params: IoctlUnmapBuffer = read_struct(input);
                    let r = self.unmap_buffer(&mut params);
                    write_struct(output, &params);
                    r
                }
                0x6 => {
                    let mut params: IoctlMapBufferEx = read_struct(input);
                    let r = self.map_buffer_ex(&mut params);
                    write_struct(output, &params);
                    r
                }
                0x8 => {
                    let mut params: IoctlGetVaRegions = read_struct(input);
                    let r = self.get_va_regions1(&mut params);
                    write_struct(output, &params);
                    r
                }
                0x9 => {
                    let mut params: IoctlAllocAsEx = read_struct(input);
                    let r = self.alloc_as_ex(&mut params);
                    write_struct(output, &params);
                    r
                }
                0x14 => {
                    // Upstream WrapVariable<IoctlRemapEntry> echoes parsed entries back to output.
                    let entry_size = std::mem::size_of::<IoctlRemapEntry>();
                    let num_entries = input.len() / entry_size;
                    let mut entries = Vec::with_capacity(num_entries);
                    for i in 0..num_entries {
                        let start = i * entry_size;
                        let end = start + entry_size;
                        if end <= input.len() {
                            entries.push(read_struct::<IoctlRemapEntry>(&input[start..end]));
                        }
                    }
                    let r = self.remap(&entries);
                    for (i, entry) in entries.iter().enumerate() {
                        let start = i * entry_size;
                        let end = start + entry_size;
                        if end <= output.len() {
                            write_struct(&mut output[start..end], entry);
                        }
                    }
                    r
                }
                _ => {
                    log::error!("Unimplemented ioctl={:08X}", command.raw);
                    NvResult::NotImplemented
                }
            },
            _ => {
                log::error!("Unimplemented ioctl={:08X}", command.raw);
                NvResult::NotImplemented
            }
        }
    }

    fn ioctl2(
        &self,
        _fd: DeviceFD,
        command: Ioctl,
        _input: &[u8],
        _inline_input: &[u8],
        _output: &mut [u8],
    ) -> NvResult {
        log::error!("Unimplemented ioctl={:08X}", command.raw);
        NvResult::NotImplemented
    }

    fn ioctl3(
        &self,
        _fd: DeviceFD,
        command: Ioctl,
        input: &[u8],
        output: &mut [u8],
        inline_output: &mut [u8],
    ) -> NvResult {
        match command.group() {
            b'A' => match command.cmd() {
                0x8 => {
                    let mut params: IoctlGetVaRegions = read_struct(input);
                    let region_size = std::mem::size_of::<VaRegion>();
                    let mut regions = vec![VaRegion::default(); inline_output.len() / region_size];
                    let r = self.get_va_regions3(&mut params, &mut regions);
                    write_struct(output, &params);
                    for (i, region) in regions.iter().enumerate() {
                        let start = i * region_size;
                        if start + region_size <= inline_output.len() {
                            write_struct(&mut inline_output[start..], region);
                        }
                    }
                    r
                }
                _ => {
                    log::error!("Unimplemented ioctl={:08X}", command.raw);
                    NvResult::NotImplemented
                }
            },
            _ => {
                log::error!("Unimplemented ioctl={:08X}", command.raw);
                NvResult::NotImplemented
            }
        }
    }

    fn on_open(&self, _session_id: SessionId, _fd: DeviceFD) {}
    fn on_close(&self, _fd: DeviceFD) {}

    fn query_event(
        &self,
        event_id: u32,
    ) -> Option<
        std::sync::Arc<std::sync::Mutex<crate::hle::kernel::k_readable_event::KReadableEvent>>,
    > {
        log::error!("Unknown AS GPU Event {}", event_id);
        None
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{
        IoctlAllocAsEx, IoctlAllocSpace, IoctlFreeSpace, IoctlMapBufferEx, IoctlRemapEntry,
        IoctlUnmapBuffer, MappingFlags, NvHostAsGpu,
    };
    use crate::core::{System, SystemRef};
    use crate::gpu_core::{GpuChannelHandle, GpuCoreInterface, GpuMemoryManagerHandle};
    use crate::hle::service::nvdrv::core::container::{Container, SessionId};
    use crate::hle::service::nvdrv::core::nvmap::{Handle, HandleFlags};
    use crate::hle::service::nvdrv::nvdata::NvResult;
    use crate::hle::service::nvdrv::nvdrv::Module;

    #[derive(Default)]
    struct FakeGpuCore {
        ops: Arc<Mutex<Vec<String>>>,
    }

    struct FakeGpuMemoryManagerHandle {
        ops: Arc<Mutex<Vec<String>>>,
    }
    struct FakeGpuChannelHandle;

    impl GpuChannelHandle for FakeGpuChannelHandle {
        fn bind_memory_manager(&self, _memory_manager: Arc<dyn GpuMemoryManagerHandle>) {}

        fn init_channel(&self, _program_id: u64) {}

        fn bind_id(&self) -> i32 {
            1
        }
    }

    impl GpuMemoryManagerHandle for FakeGpuMemoryManagerHandle {
        fn as_any(&self) -> &(dyn std::any::Any + Send + Sync) {
            self
        }

        fn map(&self, gpu_addr: u64, device_addr: u64, size: u64, kind: u32, is_big_pages: bool) {
            self.ops.lock().unwrap().push(format!(
                "map:{gpu_addr:#x}:{device_addr:#x}:{size:#x}:{kind:#x}:{is_big_pages}"
            ));
        }

        fn map_sparse(&self, gpu_addr: u64, size: u64, is_big_pages: bool) {
            self.ops
                .lock()
                .unwrap()
                .push(format!("map_sparse:{gpu_addr:#x}:{size:#x}:{is_big_pages}"));
        }

        fn unmap(&self, gpu_addr: u64, size: u64) {
            self.ops
                .lock()
                .unwrap()
                .push(format!("unmap:{gpu_addr:#x}:{size:#x}"));
        }
    }

    impl GpuCoreInterface for FakeGpuCore {
        fn as_any(&self) -> &(dyn std::any::Any + Send) {
            self
        }

        fn allocate_channel_handle(&self) -> Arc<dyn GpuChannelHandle> {
            Arc::new(FakeGpuChannelHandle)
        }

        fn allocate_memory_manager_handle(
            &self,
            _address_space_bits: u64,
            _split_address: u64,
            _big_page_bits: u64,
            _page_bits: u64,
        ) -> Arc<dyn GpuMemoryManagerHandle> {
            Arc::new(FakeGpuMemoryManagerHandle {
                ops: Arc::clone(&self.ops),
            })
        }

        fn init_address_space(&self, _memory_manager: Arc<dyn GpuMemoryManagerHandle>) {
            self.ops
                .lock()
                .unwrap()
                .push("init_address_space".to_string());
        }

        fn push_gpu_entries(&self, _channel_id: i32, _entries: crate::gpu_core::GpuCommandList) {}

        fn request_composite(
            &self,
            _layers: Vec<crate::gpu_core::FramebufferConfig>,
            _fences: Vec<crate::hle::service::nvdrv::nvdata::NvFence>,
        ) {
        }

        fn wait_for_composite(&self) {}

        fn on_cpu_write(&self, _addr: u64, _size: u64) -> bool {
            false
        }

        fn on_cpu_read(&self, addr: u64, _size: u64) -> crate::gpu_core::RasterizerDownloadArea {
            crate::gpu_core::RasterizerDownloadArea {
                start_address: addr,
                end_address: addr,
                preemptive: true,
            }
        }

        fn flush_region(&self, _addr: u64, _size: u64) {}
    }

    fn system_with_fake_gpu() -> System {
        let mut system = System::new_for_test();
        system.set_gpu_core(Box::new(FakeGpuCore::default()));
        system
    }

    fn create_allocated_handle(container: &Container, size: u64, address: u64) -> Arc<Handle> {
        let handle = container.get_nv_map_file().create_handle(size).unwrap();
        assert_eq!(
            handle.alloc(
                HandleFlags { raw: 0 },
                size as u32,
                0,
                address,
                SessionId::default(),
            ),
            NvResult::Success
        );
        handle.lock_inner().d_address = address;
        handle
    }

    #[test]
    fn allocate_space_returns_non_zero_offset_for_dynamic_allocations() {
        let system = system_with_fake_gpu();
        let module = Module::new(SystemRef::from_ref(&system));
        let container = Container::new();
        let gpu_as = NvHostAsGpu::new(SystemRef::from_ref(&system), &module, &container);
        let mut alloc_as = IoctlAllocAsEx::default();
        assert_eq!(gpu_as.alloc_as_ex(&mut alloc_as), NvResult::Success);

        let mut params = IoctlAllocSpace {
            pages: 1,
            page_size: 0x1000,
            ..Default::default()
        };
        assert_eq!(gpu_as.allocate_space(&mut params), NvResult::Success);
        assert_ne!(params.offset, 0);
    }

    #[test]
    fn alloc_as_ex_initializes_gpu_address_space_after_allocating_memory_manager() {
        let system = system_with_fake_gpu();
        let module = Module::new(SystemRef::from_ref(&system));
        let container = Container::new();
        let gpu_as = NvHostAsGpu::new(SystemRef::from_ref(&system), &module, &container);

        let mut alloc_as = IoctlAllocAsEx::default();
        assert_eq!(gpu_as.alloc_as_ex(&mut alloc_as), NvResult::Success);

        let gpu_core = system.gpu_core().unwrap();
        let fake_gpu = gpu_core
            .as_any()
            .downcast_ref::<FakeGpuCore>()
            .expect("test system should expose FakeGpuCore");
        let ops = fake_gpu.ops.lock().unwrap();
        assert!(ops.iter().any(|op| op == "init_address_space"));
    }

    #[test]
    fn get_va_regions3_copies_only_the_available_inline_regions() {
        let system = system_with_fake_gpu();
        let module = Module::new(SystemRef::from_ref(&system));
        let container = Container::new();
        let gpu_as = NvHostAsGpu::new(SystemRef::from_ref(&system), &module, &container);
        let mut alloc_as = IoctlAllocAsEx::default();
        assert_eq!(gpu_as.alloc_as_ex(&mut alloc_as), NvResult::Success);

        let mut params = super::IoctlGetVaRegions::default();
        let mut inline_regions = [super::VaRegion::default(); 1];
        assert_eq!(
            gpu_as.get_va_regions3(&mut params, &mut inline_regions),
            NvResult::Success
        );

        assert_eq!(
            params.buf_size,
            (2 * std::mem::size_of::<super::VaRegion>()) as u32
        );
        assert_eq!(inline_regions[0].offset, params.regions[0].offset);
        assert_eq!(inline_regions[0].page_size, params.regions[0].page_size);
        assert_eq!(inline_regions[0].pages, params.regions[0].pages);
    }

    #[test]
    fn alloc_as_ex_applies_non_zero_va_ranges_literally() {
        let system = system_with_fake_gpu();
        let module = Module::new(SystemRef::from_ref(&system));
        let container = Container::new();
        let gpu_as = NvHostAsGpu::new(SystemRef::from_ref(&system), &module, &container);

        let mut alloc_as = IoctlAllocAsEx {
            va_range_start: 0xBFF0_0000_0000_0000,
            va_range_end: 0x3FF0_0000_0FF2_FB16,
            va_range_split: 0xBFF0_0000_0000_0000,
            ..Default::default()
        };
        assert_eq!(gpu_as.alloc_as_ex(&mut alloc_as), NvResult::Success);

        let vm = gpu_as.vm.lock().unwrap();
        assert_eq!(vm.va_range_start, alloc_as.va_range_start);
        assert_eq!(vm.va_range_end, alloc_as.va_range_end);
        assert_eq!(vm.va_range_split, alloc_as.va_range_split);
    }

    #[test]
    fn map_buffer_ex_returns_non_zero_offset_for_dynamic_mappings() {
        let system = system_with_fake_gpu();
        let module = Module::new(SystemRef::from_ref(&system));
        let container = Container::new();
        let gpu_as = NvHostAsGpu::new(SystemRef::from_ref(&system), &module, &container);
        let mut alloc_as = IoctlAllocAsEx::default();
        assert_eq!(gpu_as.alloc_as_ex(&mut alloc_as), NvResult::Success);

        let handle = create_allocated_handle(&container, 0x1000, 0x1000_0000);

        let mut params = IoctlMapBufferEx {
            mapping_size: 0x1000,
            page_size: 0x1000,
            handle: handle.id,
            ..Default::default()
        };
        assert_eq!(gpu_as.map_buffer_ex(&mut params), NvResult::Success);
        assert_ne!(params.offset, 0);
    }

    #[test]
    fn fixed_mapping_tracks_allocation_and_unpins_on_free_space() {
        let system = system_with_fake_gpu();
        let module = Module::new(SystemRef::from_ref(&system));
        let container = Container::new();
        let gpu_as = NvHostAsGpu::new(SystemRef::from_ref(&system), &module, &container);
        let mut alloc_as = IoctlAllocAsEx::default();
        assert_eq!(gpu_as.alloc_as_ex(&mut alloc_as), NvResult::Success);

        let mut space = IoctlAllocSpace {
            pages: 1,
            page_size: 0x1000,
            flags: MappingFlags::FIXED.bits(),
            offset: 0x2000_0000,
            ..Default::default()
        };
        assert_eq!(gpu_as.allocate_space(&mut space), NvResult::Success);

        let handle = create_allocated_handle(&container, 0x1000, 0x3000_0000);

        let mut map = IoctlMapBufferEx {
            flags: MappingFlags::FIXED.bits(),
            handle: handle.id,
            mapping_size: 0x1000,
            offset: space.offset as i64,
            ..Default::default()
        };
        assert_eq!(gpu_as.map_buffer_ex(&mut map), NvResult::Success);
        assert_eq!(
            container.get_nv_map_file().get_handle_address(handle.id),
            0x3000_0000
        );

        let mut free = super::IoctlFreeSpace {
            offset: space.offset,
            pages: 1,
            page_size: 0x1000,
        };
        assert_eq!(gpu_as.free_space(&mut free), NvResult::Success);

        let handle = container.get_nv_map_file().get_handle(handle.id).unwrap();
        assert_eq!(handle.lock_inner().pins, 0);
    }

    #[test]
    fn unmap_buffer_missing_mapping_returns_success_like_upstream() {
        let system = system_with_fake_gpu();
        let module = Module::new(SystemRef::from_ref(&system));
        let container = Container::new();
        let gpu_as = NvHostAsGpu::new(SystemRef::from_ref(&system), &module, &container);
        let mut alloc_as = IoctlAllocAsEx::default();
        assert_eq!(gpu_as.alloc_as_ex(&mut alloc_as), NvResult::Success);

        let mut unmap = IoctlUnmapBuffer {
            offset: 0xDEAD_0000,
        };
        assert_eq!(gpu_as.unmap_buffer(&mut unmap), NvResult::Success);
    }

    #[test]
    fn unmap_buffer_untracked_offset_succeeds_before_vm_initialization() {
        let system = system_with_fake_gpu();
        let module = Module::new(SystemRef::from_ref(&system));
        let container = Container::new();
        let gpu_as = NvHostAsGpu::new(SystemRef::from_ref(&system), &module, &container);
        let mut unmap = IoctlUnmapBuffer {
            offset: 0xDEAD_0000,
        };

        assert_eq!(gpu_as.unmap_buffer(&mut unmap), NvResult::Success);
    }

    #[test]
    fn unmap_buffer_sends_raw_mapping_size_to_gmmu_like_upstream() {
        let ops = Arc::new(Mutex::new(Vec::new()));
        let mut system = System::new_for_test();
        system.set_gpu_core(Box::new(FakeGpuCore {
            ops: Arc::clone(&ops),
        }));
        let module = Module::new(SystemRef::from_ref(&system));
        let container = Container::new();
        let gpu_as = NvHostAsGpu::new(SystemRef::from_ref(&system), &module, &container);
        let mut alloc_as = IoctlAllocAsEx::default();
        assert_eq!(gpu_as.alloc_as_ex(&mut alloc_as), NvResult::Success);

        let handle = container.get_nv_map_file().create_handle(0x1800).unwrap();
        assert_eq!(
            handle.alloc(
                HandleFlags { raw: 0 },
                0x1000,
                0,
                0x3100_0000,
                SessionId::default(),
            ),
            NvResult::Success
        );
        handle.lock_inner().d_address = 0x3100_0000;

        let mut map = IoctlMapBufferEx {
            mapping_size: 0x1800,
            handle: handle.id,
            ..Default::default()
        };
        assert_eq!(gpu_as.map_buffer_ex(&mut map), NvResult::Success);

        let mut unmap = IoctlUnmapBuffer { offset: map.offset };
        assert_eq!(gpu_as.unmap_buffer(&mut unmap), NvResult::Success);

        assert_eq!(handle.lock_inner().pins, 0);
        let ops = ops.lock().unwrap();
        assert!(ops
            .iter()
            .any(|op| op == &format!("unmap:{:#x}:0x1800", map.offset as u64)));
    }

    #[test]
    fn free_space_stops_on_stale_fixed_mapping_like_upstream() {
        let system = system_with_fake_gpu();
        let module = Module::new(SystemRef::from_ref(&system));
        let container = Container::new();
        let gpu_as = NvHostAsGpu::new(SystemRef::from_ref(&system), &module, &container);
        let mut alloc_as = IoctlAllocAsEx::default();
        assert_eq!(gpu_as.alloc_as_ex(&mut alloc_as), NvResult::Success);

        let mut space = IoctlAllocSpace {
            pages: 1,
            page_size: 0x1000,
            flags: MappingFlags::FIXED.bits(),
            offset: 0x2080_0000,
            ..Default::default()
        };
        assert_eq!(gpu_as.allocate_space(&mut space), NvResult::Success);

        let handle = create_allocated_handle(&container, 0x1000, 0x3080_0000);

        let mut map = IoctlMapBufferEx {
            flags: MappingFlags::FIXED.bits(),
            handle: handle.id,
            mapping_size: 0x1000,
            offset: space.offset as i64,
            ..Default::default()
        };
        assert_eq!(gpu_as.map_buffer_ex(&mut map), NvResult::Success);

        let mut unmap = IoctlUnmapBuffer {
            offset: space.offset as i64,
        };
        assert_eq!(gpu_as.unmap_buffer(&mut unmap), NvResult::Success);
        assert!(!gpu_as
            .mapping_map
            .lock()
            .unwrap()
            .contains_key(&(space.offset as u64)));

        let mut free = IoctlFreeSpace {
            offset: space.offset,
            pages: 1,
            page_size: 0x1000,
        };
        assert_eq!(gpu_as.free_space(&mut free), NvResult::BadValue);
        assert!(gpu_as
            .allocation_map
            .lock()
            .unwrap()
            .contains_key(&(space.offset as u64)));
    }

    #[test]
    fn fixed_mapping_uses_shared_mapping_owner_between_maps() {
        let system = system_with_fake_gpu();
        let module = Module::new(SystemRef::from_ref(&system));
        let container = Container::new();
        let gpu_as = NvHostAsGpu::new(SystemRef::from_ref(&system), &module, &container);
        let mut alloc_as = IoctlAllocAsEx::default();
        assert_eq!(gpu_as.alloc_as_ex(&mut alloc_as), NvResult::Success);

        let mut space = IoctlAllocSpace {
            pages: 1,
            page_size: 0x1000,
            flags: MappingFlags::FIXED.bits(),
            offset: 0x2100_0000,
            ..Default::default()
        };
        assert_eq!(gpu_as.allocate_space(&mut space), NvResult::Success);

        let handle = create_allocated_handle(&container, 0x1000, 0x3100_0000);

        let mut map = IoctlMapBufferEx {
            flags: MappingFlags::FIXED.bits(),
            handle: handle.id,
            mapping_size: 0x1000,
            offset: space.offset as i64,
            ..Default::default()
        };
        assert_eq!(gpu_as.map_buffer_ex(&mut map), NvResult::Success);

        let mapping_from_map = gpu_as
            .mapping_map
            .lock()
            .unwrap()
            .get(&(space.offset as u64))
            .cloned()
            .expect("fixed mapping should exist in mapping_map");
        let mapping_from_alloc = gpu_as
            .allocation_map
            .lock()
            .unwrap()
            .get(&(space.offset as u64))
            .and_then(|allocation| allocation.mappings.first().cloned())
            .expect("fixed allocation should reference its mapping");

        assert!(Arc::ptr_eq(&mapping_from_map, &mapping_from_alloc));
    }

    #[test]
    fn free_space_sparse_restores_mapping_then_unmaps_allocation_like_upstream() {
        let ops = Arc::new(Mutex::new(Vec::new()));
        let mut system = System::new_for_test();
        system.set_gpu_core(Box::new(FakeGpuCore {
            ops: Arc::clone(&ops),
        }));
        let module = Module::new(SystemRef::from_ref(&system));
        let container = Container::new();
        let gpu_as = NvHostAsGpu::new(SystemRef::from_ref(&system), &module, &container);
        let mut alloc_as = IoctlAllocAsEx::default();
        assert_eq!(gpu_as.alloc_as_ex(&mut alloc_as), NvResult::Success);

        let mut space = IoctlAllocSpace {
            pages: 1,
            page_size: 0x20_000,
            flags: (MappingFlags::FIXED | MappingFlags::SPARSE).bits(),
            offset: 0x4000_0000,
            ..Default::default()
        };
        assert_eq!(gpu_as.allocate_space(&mut space), NvResult::Success);

        let handle = create_allocated_handle(&container, 0x20_000, 0x5000_0000);
        let mut map = IoctlMapBufferEx {
            flags: MappingFlags::FIXED.bits(),
            handle: handle.id,
            mapping_size: 0x20_000,
            offset: space.offset as i64,
            ..Default::default()
        };
        assert_eq!(gpu_as.map_buffer_ex(&mut map), NvResult::Success);

        ops.lock().unwrap().clear();
        let mut free = IoctlFreeSpace {
            offset: space.offset,
            pages: 1,
            page_size: 0x20_000,
        };
        assert_eq!(gpu_as.free_space(&mut free), NvResult::Success);

        let ops = ops.lock().unwrap();
        let sparse_pos = ops
            .iter()
            .position(|op| op == "map_sparse:0x40000000:0x20000:true")
            .expect("freeing fixed sparse mapping should restore sparse GMMU range first");
        let unmap_pos = ops
            .iter()
            .position(|op| op == "unmap:0x40000000:0x20000")
            .expect("freeing sparse allocation should unmap the allocation");
        assert!(sparse_pos < unmap_pos);
    }

    #[test]
    fn remap_rejects_non_sparse_allocations() {
        let system = system_with_fake_gpu();
        let module = Module::new(SystemRef::from_ref(&system));
        let container = Container::new();
        let gpu_as = NvHostAsGpu::new(SystemRef::from_ref(&system), &module, &container);
        let mut alloc_as = IoctlAllocAsEx::default();
        assert_eq!(gpu_as.alloc_as_ex(&mut alloc_as), NvResult::Success);

        let mut space = IoctlAllocSpace {
            pages: 1,
            page_size: 0x20_000,
            offset: 0x4000_0000,
            flags: MappingFlags::FIXED.bits(),
            ..Default::default()
        };
        assert_eq!(gpu_as.allocate_space(&mut space), NvResult::Success);

        let entry = IoctlRemapEntry {
            handle: 0,
            as_offset_big_pages: (space.offset >> 17) as u32,
            big_pages: 1,
            ..Default::default()
        };
        assert_eq!(gpu_as.remap(&[entry]), NvResult::BadValue);
    }

    #[test]
    fn allocate_space_sparse_calls_gpu_memory_manager_map_sparse() {
        let ops = Arc::new(Mutex::new(Vec::new()));
        let mut system = System::new_for_test();
        system.set_gpu_core(Box::new(FakeGpuCore {
            ops: Arc::clone(&ops),
        }));
        let module = Module::new(SystemRef::from_ref(&system));
        let container = Container::new();
        let gpu_as = NvHostAsGpu::new(SystemRef::from_ref(&system), &module, &container);
        let mut alloc_as = IoctlAllocAsEx::default();
        assert_eq!(gpu_as.alloc_as_ex(&mut alloc_as), NvResult::Success);

        let mut space = IoctlAllocSpace {
            pages: 1,
            page_size: 0x20_000,
            flags: MappingFlags::SPARSE.bits(),
            ..Default::default()
        };
        assert_eq!(gpu_as.allocate_space(&mut space), NvResult::Success);

        let ops = ops.lock().unwrap();
        assert!(ops.iter().any(|op| op.starts_with("map_sparse:")));
    }

    #[test]
    fn remap_sparse_allocation_pins_handle() {
        let system = system_with_fake_gpu();
        let module = Module::new(SystemRef::from_ref(&system));
        let container = Container::new();
        let gpu_as = NvHostAsGpu::new(SystemRef::from_ref(&system), &module, &container);
        let mut alloc_as = IoctlAllocAsEx::default();
        assert_eq!(gpu_as.alloc_as_ex(&mut alloc_as), NvResult::Success);

        let mut space = IoctlAllocSpace {
            pages: 1,
            page_size: 0x20_000,
            offset: 0x4000_0000,
            flags: (MappingFlags::FIXED | MappingFlags::SPARSE).bits(),
            ..Default::default()
        };
        assert_eq!(gpu_as.allocate_space(&mut space), NvResult::Success);

        let handle = create_allocated_handle(&container, 0x20_000, 0x5000_0000);

        let entry = IoctlRemapEntry {
            handle: handle.id,
            as_offset_big_pages: (space.offset >> 17) as u32,
            big_pages: 1,
            ..Default::default()
        };
        assert_eq!(gpu_as.remap(&[entry]), NvResult::Success);
        assert_eq!(
            container.get_nv_map_file().get_handle_address(handle.id),
            0x5000_0000
        );
        assert_eq!(handle.lock_inner().pins, 1);
    }

    #[test]
    fn bind_channel_records_gpu_channel_binding() {
        let mut system = System::new_for_test();
        system.set_gpu_core(Box::new(FakeGpuCore::default()));
        let module = Module::new(SystemRef::from_ref(&system));
        let fd = module.open("/dev/nvhost-gpu", SessionId::default());
        let container = Container::new();
        let gpu_as = NvHostAsGpu::new(SystemRef::from_ref(&system), &module, &container);
        let mut alloc_as = IoctlAllocAsEx::default();
        assert_eq!(gpu_as.alloc_as_ex(&mut alloc_as), NvResult::Success);
        let mut params = super::IoctlBindChannel { fd };

        assert_eq!(gpu_as.bind_channel(&mut params), NvResult::Success);
        assert!(module.get_gpu_device(fd).unwrap().has_bound_address_space());

        std::mem::forget(module);
        std::mem::forget(system);
    }
}
