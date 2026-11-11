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

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct IoctlNx2SyncpointShimInfo {
    pub address: u64,
    pub stride: u32,
    pub count: u32,
}
const _: () = assert!(std::mem::size_of::<IoctlNx2SyncpointShimInfo>() == 16);

struct VM {
    big_page_size: u32,
    big_page_size_bits: u32,
    va_range_start: u64,
    va_range_split: u64,
    va_range_end: u64,
    small_page_allocator: Option<FlatAllocator>,
    big_page_allocator: Option<FlatAllocator>,
    initialised: bool,
    absolute_big_page_limit: bool,
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
            absolute_big_page_limit: false,
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
#[derive(Clone, Copy)]
struct Nx2DeviceRange {
    address: u64,
    heap: bool,
}

// Callback lifetime adapter for this device's upstream-owned fields. Registry
// callbacks hold its Weak, not Weak<NvHostAsGpu>: dropping the last device Arc
// inside a callback would otherwise re-enter RemoveListener's registry lock.
// The device still owns registration/removal; both paths share the same mutex
// and GMMU, and all source methods remain in NvHostAsGpu in this file.
struct Nx2State {
    system: SystemRef,
    container: Container,
    mutex: Arc<Mutex<()>>,
    gmmu: Arc<Mutex<Option<Arc<dyn GpuMemoryManagerHandle>>>>,
    session: Mutex<SessionId>,
    device_ranges: Mutex<BTreeMap<(u64, u64), Nx2DeviceRange>>,
    /// GPU ranges whose unmap is queued on the GPU thread and has not run yet.
    pending_unmaps: Arc<Mutex<Vec<(u64, u64)>>>,
}

pub struct NvHostAsGpu {
    system: SystemRef,
    module: *const Module,
    nvmap: *const NvMap,
    mutex: Arc<Mutex<()>>,
    vm: Mutex<VM>,
    map_buffer_offsets: Mutex<HashSet<i64>>,
    mapping_map: Mutex<BTreeMap<u64, Arc<Mapping>>>,
    allocation_map: Mutex<BTreeMap<u64, Allocation>>,
    gmmu: Arc<Mutex<Option<Arc<dyn GpuMemoryManagerHandle>>>>,
    nx2_syncpoint_shim: Mutex<bool>,
    nx2_state: Arc<Nx2State>,
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
        let mutex = Arc::new(Mutex::new(()));
        let gmmu = Arc::new(Mutex::new(None));
        let nx2_state = Arc::new(Nx2State {
            system,
            container: container.clone(),
            mutex: Arc::clone(&mutex),
            gmmu: Arc::clone(&gmmu),
            session: Mutex::new(SessionId::default()),
            device_ranges: Mutex::new(BTreeMap::new()),
            pending_unmaps: Arc::new(Mutex::new(Vec::new())),
        });
        Self {
            system,
            module: module as *const _,
            nvmap: container.get_nv_map_file() as *const _,
            mutex,
            vm: Mutex::new(VM::default()),
            map_buffer_offsets: Mutex::new(HashSet::new()),
            mapping_map: Mutex::new(BTreeMap::new()),
            allocation_map: Mutex::new(BTreeMap::new()),
            gmmu,
            nx2_syncpoint_shim: Mutex::new(false),
            nx2_state,
        }
    }

    fn module(&self) -> &Module {
        unsafe { &*self.module }
    }

    fn nx2_map(state: &Nx2State, gpu_address: u64, cpu_address: u64, size: u64) {
        let _guard = state.mutex.lock().unwrap();
        let session_id = *state.session.lock().unwrap();
        let session = state.container.get_session_mapping(session_id);
        let gmmu = state.gmmu.lock().unwrap().clone();
        let (Some((asid, preallocated, mapper)), Some(gmmu)) = (session, gmmu) else {
            log::error!("Switch 2 GPU mapping {gpu_address:#x}+{size:#x}: no address space or session");
            return;
        };
        let mut ranges = state.device_ranges.lock().unwrap();
        let range = if let Some(range) = ranges.get(&(cpu_address, size)) {
            *range
        } else if let Some(mapper) = mapper.filter(|mapper| preallocated && mapper.is_in_bounds(cpu_address, size as usize)) {
            Nx2DeviceRange { address: mapper.map(cpu_address, size as usize), heap: true }
        } else {
            let smmu = state.system.get().host1x_core().expect("NX2 mapping requires Host1x");
            let address = smmu.smmu_allocate(size.wrapping_add(0x1FFFF) as usize & !0x1FFFF);
            if address == 0 {
                log::error!("Switch 2 GPU memory {cpu_address:#x}+{size:#x}: out of SMMU space");
                return;
            }
            smmu.smmu_map(address, cpu_address, size as usize, asid, true);
            Nx2DeviceRange { address, heap: false }
        };
        ranges.insert((cpu_address, size), range);
        super::nx2_stub_device::Nx2GpuMemoryRegistry::get()
            .record_device_range(cpu_address, size, range.address);
        // Unmaps are queued on the GPU thread (it may still be running command
        // lists that use the range) while maps apply at once. A title that
        // destroys a region and recreates one at the same GPU address - Mario
        // Kart World does, constantly, for its streamed texture regions - can
        // therefore have the stale unmap run *after* the new map and erase it:
        // images in the new region then find no backing and upload as zeros
        // (black scenery that later corrects itself). Order the new map behind
        // any unmap still pending on an overlapping range.
        let overlaps_pending_unmap = state.pending_unmaps.lock().unwrap().iter().any(
            |&(pending_address, pending_size)| {
                gpu_address < pending_address.wrapping_add(pending_size)
                    && pending_address < gpu_address.wrapping_add(size)
            },
        );
        if overlaps_pending_unmap {
            let device_address = range.address;
            state.system.get().gpu_core().expect("NX2 mapping requires GPU")
                .run_in_order(Box::new(move || {
                    gmmu.map(gpu_address, device_address, size, 0, false); // PTEKind::PITCH
                }));
        } else {
            gmmu.map(gpu_address, range.address, size, 0, false); // PTEKind::PITCH
        }
        log::debug!("Switch 2 GPU mapping {gpu_address:#x}+{size:#x} -> CPU {cpu_address:#x}");
    }

    fn nx2_unmap(state: &Nx2State, gpu_address: u64, size: u64) {
        let _guard = state.mutex.lock().unwrap();
        let Some(gmmu) = state.gmmu.lock().unwrap().clone() else { return };
        let pending_unmaps = Arc::clone(&state.pending_unmaps);
        pending_unmaps.lock().unwrap().push((gpu_address, size));
        // The GPU thread may still be running command lists that use the range.
        state.system.get().gpu_core().expect("NX2 unmapping requires GPU")
            .run_in_order(Box::new(move || {
                gmmu.unmap(gpu_address, size);
                let mut pending = pending_unmaps.lock().unwrap();
                if let Some(position) = pending.iter().position(|&entry| entry == (gpu_address, size)) {
                    pending.swap_remove(position);
                }
            }));
        log::debug!("Switch 2 GPU unmapping {gpu_address:#x}+{size:#x}");
    }

    fn nx2_release(state: &Nx2State, cpu_address: u64, size: u64) {
        let _guard = state.mutex.lock().unwrap();
        let Some(range) = state.device_ranges.lock().unwrap().remove(&(cpu_address, size)) else { return };
        let mapper = if range.heap {
            state.container.get_session_mapping(*state.session.lock().unwrap())
                .and_then(|(_, _, mapper)| mapper)
        } else { None };
        let system = state.system;
        system.get().gpu_core().expect("NX2 release requires GPU").run_in_order(Box::new(move || {
            if range.heap {
                if let Some(mapper) = mapper {
                    mapper.unmap(cpu_address, size as usize);
                }
                return;
            }
            let smmu = system.get().host1x_core().expect("NX2 release requires Host1x");
            smmu.smmu_unmap(range.address, size as usize);
            smmu.smmu_free(range.address, size.wrapping_add(0x1FFFF) as usize & !0x1FFFF);
        }));
        log::debug!("Switch 2 released backing {cpu_address:#x}+{size:#x}");
    }

    // Mechanical binding of the three source lambdas; called after AllocAsEx
    // releases its mutex because registration immediately replays mappings.
    fn nx2_register_listener(&self) {
        let map_state = Arc::downgrade(&self.nx2_state);
        let unmap_state = map_state.clone();
        let release_state = map_state.clone();
        super::nx2_stub_device::Nx2GpuMemoryRegistry::get().add_listener(
            Arc::as_ptr(&self.nx2_state) as usize,
            Box::new(move |gpu, cpu, size| {
                if let Some(state) = map_state.upgrade() { Self::nx2_map(&state, gpu, cpu, size); }
            }),
            Box::new(move |gpu, size| {
                if let Some(state) = unmap_state.upgrade() { Self::nx2_unmap(&state, gpu, size); }
            }),
            Some(Box::new(move |cpu, size| {
                if let Some(state) = release_state.upgrade() { Self::nx2_release(&state, cpu, size); }
            })),
        );
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

    pub fn nx2_get_syncpoint_shim_info(&self, params: &mut IoctlNx2SyncpointShimInfo) -> NvResult {
        let _owner_guard = self.mutex.lock().unwrap();
        let vm = self.vm.lock().unwrap();
        let gmmu = self.gmmu.lock().unwrap();
        let Some(gmmu) = gmmu.as_ref().filter(|_| vm.initialised) else {
            return NvResult::NotSupported;
        };
        let address = vm.va_range_end.wrapping_sub(u64::from(VM::YUZU_PAGESIZE));
        let mut enabled = self.nx2_syncpoint_shim.lock().unwrap();
        if !*enabled {
            *enabled = true;
            gmmu.set_native_syncpoint_shim(address);
        }
        params.address = address;
        params.stride = 4;
        params.count = gmmu.num_shim_syncpoints();
        log::info!("Switch 2 syncpoint shim at {address:#x} ({} syncpoints)", params.count);
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
            if vm.va_range_split == 0 {
                vm.va_range_split = vm.va_range_end;
                vm.va_range_end = vm.va_range_end.wrapping_add(4u64 << 30);
                vm.absolute_big_page_limit = true;
                log::warn!("No VA split given; small pages up to {:#x}", vm.va_range_split);
            }
        }

        let start_pages = (vm.va_range_start >> VM::PAGE_SIZE_BITS) as u32;
        let end_pages = (vm.va_range_split >> VM::PAGE_SIZE_BITS) as u32;
        vm.small_page_allocator = Some(FlatAllocator::new(start_pages, end_pages));

        let start_big_pages = (vm.va_range_split >> vm.big_page_size_bits) as u32;
        let end_big_pages = (if vm.absolute_big_page_limit { vm.va_range_end }
            else { vm.va_range_end.wrapping_sub(vm.va_range_split) }
            >> vm.big_page_size_bits) as u32;
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

impl Drop for NvHostAsGpu {
    fn drop(&mut self) {
        super::nx2_stub_device::Nx2GpuMemoryRegistry::get()
            .remove_listener(Arc::as_ptr(&self.nx2_state) as usize);
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
                0xB if super::nx2_stub_device::G_NX2_DRIVER.load(std::sync::atomic::Ordering::SeqCst) => {
                    let mut params: IoctlNx2SyncpointShimInfo = read_struct(input);
                    let result = self.nx2_get_syncpoint_shim_info(&mut params);
                    write_struct(output, &params);
                    result
                }
                0x9 => {
                    let mut params: IoctlAllocAsEx = read_struct(input);
                    let r = self.alloc_as_ex(&mut params);
                    write_struct(output, &params);
                    if r == NvResult::Success && super::nx2_stub_device::G_NX2_DRIVER.load(std::sync::atomic::Ordering::SeqCst) {
                        self.nx2_register_listener();
                    }
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

    fn on_open(&self, session_id: SessionId, _fd: DeviceFD) {
        *self.nx2_state.session.lock().unwrap() = session_id;
    }
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
        pending: Mutex<std::collections::VecDeque<Box<dyn FnOnce() + Send>>>,
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
        fn set_native_syncpoint_shim(&self, address: u64) {
            self.ops.lock().unwrap().push(format!("syncpoint_shim:{address:#x}"));
        }
        fn num_shim_syncpoints(&self) -> u32 { 192 }

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
        fn run_in_order(&self, func: Box<dyn FnOnce() + Send>) {
            self.pending.lock().unwrap().push_back(func);
        }

        fn set_nx2_ampere(&self, _enabled: bool) {}

        fn nx2_add_semaphore_region(&self, _start: u64) {
            panic!("NX2 semaphore registration is outside this test fixture");
        }

        fn nx2_remove_semaphore_region(&self, _start: u64) {
            panic!("NX2 semaphore removal is outside this test fixture");
        }

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

        fn run_on_gpu_thread(&self, _action: Box<dyn FnOnce() + Send>) -> u64 {
            panic!("GPU sync operation is outside this test fixture");
        }

        fn renderer_flush_region(&self, _addr: u64, _size: u64) {
            panic!("renderer flush is outside this test fixture");
        }

        fn request_screenshot(
            &self,
            _pixels: Vec<u8>,
            _callback: Box<dyn FnOnce(Vec<u8>, bool) + Send>,
            _layout: crate::frontend::framebuffer_layout::FramebufferLayout,
            _layer_stack: crate::hle::service::nvnflinger::hwc_layer::LayerStackId,
        ) {
            panic!("screenshot is outside this test fixture");
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

    struct FakeHost1xCore {
        ops: Arc<Mutex<Vec<String>>>,
        next: std::sync::atomic::AtomicU64,
    }

    impl crate::host1x_core::Host1xCoreInterface for FakeHost1xCore {
        fn create_nx2_video_decoder(&self, _mime: &str) -> Box<dyn crate::host1x_core::Nx2VideoDecoderHandle> {
            panic!("video decoding is outside this memory test fixture");
        }

        fn increment_guest(&self, _id: u32) {
            panic!("guest increment is outside this fixture");
        }

        fn increment_host(&self, _id: u32) {
            panic!("host increment is outside this fixture");
        }

        fn is_ready_guest(&self, _id: u32, _expected_value: u32) -> bool {
            panic!("memory-test Host1x fixture does not model guest syncpoints")
        }

        fn get_guest_syncpoint_value(&self, _id: u32) -> u32 {
            panic!("memory-test Host1x fixture does not model guest syncpoints")
        }

        fn deregister_guest_action(&self, _id: u32, _handle: u64) {
            panic!("memory-test Host1x fixture does not model guest actions")
        }

        fn as_any(&self) -> &(dyn std::any::Any + Send + Sync) {
            self
        }

        fn get_host_syncpoint_value(&self, _id: u32) -> u32 {
            0
        }

        fn wait_host(&self, _id: u32, _expected_value: u32) {}

        fn register_guest_action(
            &self,
            _id: u32,
            _expected_value: u32,
            _action: Box<dyn FnOnce() + Send>,
        ) -> Option<u64> {
            None
        }

        fn register_host_action(
            &self,
            _id: u32,
            _expected_value: u32,
            _action: Box<dyn FnOnce() + Send>,
        ) -> Option<u64> {
            None
        }

        fn deregister_host_action(&self, _id: u32, _handle: u64) {}

        fn smmu_allocate(&self, _size: usize) -> u64 {
            self.ops.lock().unwrap().push(format!("allocate:{_size:#x}"));
            self.next.fetch_add(0x20000, std::sync::atomic::Ordering::Relaxed)
        }

        fn smmu_register_process(&self, _memory: Option<Arc<crate::memory::memory::SharedMemory>>) -> u32 {
            7
        }

        fn smmu_unregister_process(&self, _asid: u32) {}

        fn smmu_free(&self, _d_address: u64, _size: usize) {
            self.ops.lock().unwrap().push(format!("free:{_d_address:#x}:{_size:#x}"));
        }

        fn smmu_map(
            &self,
            _d_address: u64,
            _virtual_address: u64,
            _size: usize,
            _asid: u32,
            _track: bool,
        ) {
            self.ops.lock().unwrap().push(format!("smmu_map:{_d_address:#x}:{_virtual_address:#x}:{_size:#x}:{_asid}:{_track}"));
        }

        fn smmu_track_continuity(&self, _d_address: u64, _size: usize) {}

        fn smmu_track_continuity_registered(
            &self,
            _d_address: u64,
            _virtual_address: u64,
            _size: usize,
            _asid: u32,
        ) {
        }

        fn smmu_unmap(&self, _d_address: u64, _size: usize) {
            self.ops.lock().unwrap().push(format!("smmu_unmap:{_d_address:#x}:{_size:#x}"));
        }

        fn smmu_read_block(&self, _address: u64, _output: &mut [u8]) -> bool {
            panic!("device reads are outside this fixture");
        }

        fn smmu_lookup(&self, _d_address: u64) -> usize {
            0
        }

        fn smmu_apply_op_on_host_pointer(
            &self,
            _host_ptr: usize,
            _scratch: &mut common::scratch_buffer::ScratchBuffer<u32>,
            _operation: &mut dyn FnMut(u64),
        ) -> usize {
            panic!("alias invalidation is outside this fixture")
        }

        fn bind_device_memory_invalidator(&self, _callback: Box<dyn Fn(u64, usize) + Send + Sync>) {
        }

        fn bind_device_memory_flusher(&self, _callback: Box<dyn Fn(u64, usize) + Send + Sync>) {}

        fn host1x_gmmu_map_low(&self, _d_address: u64, _size: usize) -> u32 {
            0
        }

        fn host1x_gmmu_unmap_low(&self, _gpu_address: u32, _size: usize) {}

        fn start_device(&self, _fd: i32, _channel_type: crate::host1x_core::Host1xChannelType, _syncpt: u32) {}

        fn stop_device(&self, _fd: i32, _channel_type: crate::host1x_core::Host1xChannelType) {}

        fn push_entries(&self, _fd: i32, _entries: Vec<u32>) {}
    }

    fn system_with_fake_gpu() -> Box<System> {
        let mut system = System::new_boxed_for_test();
        system.set_gpu_core(Box::new(FakeGpuCore::default()));
        system
    }

    #[test]
    fn nx2_registry_replays_reuses_and_releases_after_queued_gpu_unmap() {
        use crate::hle::service::nvdrv::devices::nvdevice::NvDevice;
        use crate::hle::service::nvdrv::devices::nx2_stub_device::{G_NX2_DRIVER, Nx2GpuMemoryRegistry};
        use crate::hle::kernel::k_process::{KProcess, ProcessLock};
        use std::sync::atomic::{AtomicU64, Ordering};
        G_NX2_DRIVER.store(true, Ordering::SeqCst);
        let ops = Arc::new(Mutex::new(Vec::new()));
        let mut system = system_with_fake_gpu();
        system.set_gpu_core(Box::new(FakeGpuCore { ops: Arc::clone(&ops), ..Default::default() }));
        system.set_host1x_core(Box::new(FakeHost1xCore {
            ops: Arc::clone(&ops), next: AtomicU64::new(0x40000),
        }));
        let module = Module::new(SystemRef::from_ref(&system));
        let container = Container::new_with_system(SystemRef::from_ref(&system));
        let process = Arc::new(ProcessLock::from_value(KProcess::new()));
        let session = container.open_session(&process);
        let gpu_as = NvHostAsGpu::new(SystemRef::from_ref(&system), &module, &container);
        gpu_as.on_open(session, 9);
        let registry = Nx2GpuMemoryRegistry::get();
        let cpu = 0x550000000;
        let gpu = 0x440000000;
        let size = 0x3000;
        registry.map_at(gpu, cpu, size);
        let input = vec![0; std::mem::size_of::<IoctlAllocAsEx>()];
        let mut output = input.clone();
        assert_eq!(gpu_as.ioctl1(9, super::Ioctl { raw: 0x4109 }, &input, &mut output), NvResult::Success);
        assert_eq!(&*ops.lock().unwrap(), &[
            "init_address_space", "allocate:0x20000",
            "smmu_map:0x40000:0x550000000:0x3000:7:true",
            "map:0x440000000:0x40000:0x3000:0x0:false",
        ]);
        registry.map_at(gpu + 0x10000, cpu, size);
        assert_eq!(ops.lock().unwrap().iter().filter(|s| s.starts_with("allocate:")).count(), 1);
        assert_eq!(registry.device_address_of(cpu + 0x100), Some(0x40100));
        registry.unmap(gpu, size);
        registry.unmap(gpu + 0x10000, size);
        registry.release_backing(cpu, size);
        assert!(gpu_as.nx2_state.device_ranges.lock().unwrap().is_empty());
        assert_eq!(registry.device_address_of(cpu), None);
        assert!(!ops.lock().unwrap().iter().any(|s| s.starts_with("unmap:") || s.starts_with("free:")));
        let core = system.gpu_core().unwrap().as_any().downcast_ref::<FakeGpuCore>().unwrap();
        assert_eq!(core.pending.lock().unwrap().len(), 3);
        loop {
            let next = core.pending.lock().unwrap().pop_front();
            let Some(next) = next else { break };
            next();
        }
        assert_eq!(&ops.lock().unwrap()[5..], &[
            "unmap:0x440000000:0x3000", "unmap:0x440010000:0x3000",
            "smmu_unmap:0x40000:0x3000", "free:0x40000:0x20000",
        ]);
        // Exhaustion must not publish either a device range or a GPU mapping.
        let host = system.host1x_core().unwrap().as_any().downcast_ref::<FakeHost1xCore>().unwrap();
        host.next.store(0, Ordering::Relaxed);
        let before = ops.lock().unwrap().len();
        NvHostAsGpu::nx2_map(&gpu_as.nx2_state, gpu, cpu, size);
        assert_eq!(ops.lock().unwrap().len(), before + 1);
        assert!(gpu_as.nx2_state.device_ranges.lock().unwrap().is_empty());
        drop(gpu_as);
        let before = ops.lock().unwrap().len();
        registry.map_at(gpu, cpu, size);
        assert_eq!(ops.lock().unwrap().len(), before);
        registry.unmap(gpu, size);
        container.close_session(session);
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
    fn nx2_zero_split_geometry_and_syncpoint_shim_once() {
        let system = system_with_fake_gpu();
        let module = Module::new(SystemRef::from_ref(&system));
        let container = Container::new();
        let gpu_as = NvHostAsGpu::new(SystemRef::from_ref(&system), &module, &container);
        let mut info = super::IoctlNx2SyncpointShimInfo { address: 7, stride: 8, count: 9 };
        assert_eq!(gpu_as.nx2_get_syncpoint_shim_info(&mut info), NvResult::NotSupported);
        assert_eq!((info.address, info.stride, info.count), (7, 8, 9));
        let mut params = IoctlAllocAsEx {
            flags: 5,
            va_range_start: 0x1000,
            va_range_end: 1u64 << 39,
            va_range_split: 0,
            ..Default::default()
        };
        assert_eq!(gpu_as.alloc_as_ex(&mut params), NvResult::Success);
        {
            let vm = gpu_as.vm.lock().unwrap();
            assert_eq!(vm.va_range_split, 1u64 << 39);
            assert_eq!(vm.va_range_end, (1u64 << 39) + (4u64 << 30));
            assert!(vm.absolute_big_page_limit);
        }
        let mut allocation = IoctlAllocSpace { pages: 1, page_size: 0x20000, ..Default::default() };
        assert_eq!(gpu_as.allocate_space(&mut allocation), NvResult::Success);
        assert_eq!(allocation.offset, 1u64 << 39);
        for _ in 0..2 {
            assert_eq!(gpu_as.nx2_get_syncpoint_shim_info(&mut info), NvResult::Success);
            assert_eq!(info.address, (1u64 << 39) + (4u64 << 30) - 0x1000);
            assert_eq!((info.stride, info.count), (4, 192));
        }
        let gpu = system.gpu_core().unwrap().as_any().downcast_ref::<FakeGpuCore>().unwrap();
        assert_eq!(gpu.ops.lock().unwrap().iter().filter(|op| op.starts_with("syncpoint_shim:")).count(), 1);
        let mut bytes = [0u8; 16];
        super::write_struct(&mut bytes, &info);
        assert_eq!(&bytes[..8], &info.address.to_le_bytes());
        assert_eq!(&bytes[8..12], &4u32.to_le_bytes());
        assert_eq!(&bytes[12..], &192u32.to_le_bytes());
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
