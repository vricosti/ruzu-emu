use crate::common::common::CpuAddr;
use crate::renderer::behavior::behavior_info::ErrorInfo;
use crate::renderer::memory::{
    AddressInfo, MemoryPoolInParameter, MemoryPoolInfo, MemoryPoolOutStatus, MemoryPoolResultState,
    MemoryPoolState, PoolLocation,
};
use common::alignment::is_aligned;
use common::{ResultCode, PAGE_SIZE_U64};

pub struct PoolMapper<'a> {
    process_handle: Option<usize>,
    pool_infos: Option<&'a mut [MemoryPoolInfo]>,
    pool_count: u64,
    force_map: bool,
}

impl<'a> PoolMapper<'a> {
    pub fn new(process_handle: Option<usize>, force_map: bool) -> Self {
        Self {
            process_handle,
            pool_infos: None,
            pool_count: 0,
            force_map,
        }
    }

    pub fn with_pools(
        process_handle: Option<usize>,
        pool_infos: &'a mut [MemoryPoolInfo],
        pool_count: u32,
        force_map: bool,
    ) -> Self {
        Self {
            process_handle,
            pool_infos: Some(pool_infos),
            pool_count: pool_count as u64,
            force_map,
        }
    }

    pub fn clear_use_state(pools: &mut [MemoryPoolInfo], count: u32) {
        for pool in pools.iter_mut().take(count as usize) {
            pool.set_used(false);
        }
    }

    pub fn find_memory_pool(
        &self,
        pools: &mut [MemoryPoolInfo],
        count: u64,
        address: CpuAddr,
        size: u64,
    ) -> *mut MemoryPoolInfo {
        for (index, pool) in pools.iter_mut().take(count as usize).enumerate() {
            if pool.contains(address, size) {
                return unsafe { pools.as_mut_ptr().add(index) };
            }
        }
        std::ptr::null_mut()
    }

    pub fn find_memory_pool_in_self(&self, address: CpuAddr, size: u64) -> *mut MemoryPoolInfo {
        let Some(pools) = &self.pool_infos else {
            return std::ptr::null_mut();
        };
        for index in 0..self.pool_count as usize {
            if pools[index].contains(address, size) {
                return unsafe { pools.as_ptr().add(index) as *mut MemoryPoolInfo };
            }
        }
        std::ptr::null_mut()
    }

    pub fn fill_dsp_addr_in_slice(
        &self,
        address_info: &mut AddressInfo,
        pools: &mut [MemoryPoolInfo],
        count: u32,
    ) -> bool {
        if address_info.get_cpu_addr() == 0 {
            address_info.set_pool(std::ptr::null_mut());
            return false;
        }
        let found_pool = self.find_memory_pool(
            pools,
            count as u64,
            address_info.get_cpu_addr(),
            address_info.get_size(),
        );
        if !found_pool.is_null() {
            address_info.set_pool(found_pool);
            return true;
        }
        if self.force_map {
            address_info.set_force_mapped_dsp_addr(address_info.get_cpu_addr());
        } else {
            address_info.set_pool(std::ptr::null_mut());
        }
        false
    }

    pub fn fill_dsp_addr(&self, address_info: &mut AddressInfo) -> bool {
        if address_info.get_cpu_addr() == 0 {
            address_info.set_pool(std::ptr::null_mut());
            return false;
        }
        let found_pool =
            self.find_memory_pool_in_self(address_info.get_cpu_addr(), address_info.get_size());
        if !found_pool.is_null() {
            address_info.set_pool(found_pool);
            return true;
        }
        if self.force_map {
            address_info.set_force_mapped_dsp_addr(address_info.get_cpu_addr());
        } else {
            address_info.set_pool(std::ptr::null_mut());
        }
        false
    }

    pub fn try_attach_buffer(
        &self,
        error_info: &mut ErrorInfo,
        address_info: &mut AddressInfo,
        address: CpuAddr,
        size: u64,
    ) -> bool {
        address_info.setup(address, size);
        if !self.fill_dsp_addr(address_info) {
            error_info.error_code = crate::errors::RESULT_INVALID_ADDRESS_INFO;
            error_info.address = address;
            return self.force_map;
        }
        error_info.error_code = ResultCode::SUCCESS;
        error_info.address = 0;
        true
    }

    pub fn is_force_map_enabled(&self) -> bool {
        self.force_map
    }

    pub fn get_process_handle(&self, pool: &MemoryPoolInfo) -> Option<usize> {
        match pool.get_location() {
            PoolLocation::Cpu => self.process_handle,
            PoolLocation::Dsp => None,
        }
    }

    pub fn map_region(&self, _handle: u32, _cpu_addr: CpuAddr, _size: u64) -> bool {
        true
    }

    pub fn map_pool(&self, pool: &mut MemoryPoolInfo) -> bool {
        match pool.get_location() {
            PoolLocation::Cpu | PoolLocation::Dsp => {
                pool.set_dsp_address(pool.get_cpu_address());
                true
            }
        }
    }

    pub fn unmap_region(&self, _handle: u32, _cpu_addr: CpuAddr, _size: u64) -> bool {
        true
    }

    pub fn unmap_pool(&self, pool: &mut MemoryPoolInfo) -> bool {
        pool.set_cpu_address(0, 0);
        pool.set_dsp_address(0);
        true
    }

    pub fn force_unmap_pointer(&self, _address_info: &AddressInfo) {}

    pub fn update(
        &self,
        pool: &mut MemoryPoolInfo,
        in_params: &MemoryPoolInParameter,
        out_params: &mut MemoryPoolOutStatus,
    ) -> MemoryPoolResultState {
        if in_params.state != MemoryPoolState::RequestAttach
            && in_params.state != MemoryPoolState::RequestDetach
        {
            return MemoryPoolResultState::Success;
        }

        if in_params.address == 0
            || in_params.size == 0
            || !is_aligned(in_params.address, PAGE_SIZE_U64)
            || !is_aligned(in_params.size, PAGE_SIZE_U64)
        {
            return MemoryPoolResultState::BadParam;
        }

        match in_params.state {
            MemoryPoolState::RequestAttach => {
                pool.set_cpu_address(in_params.address as CpuAddr, in_params.size);
                self.map_pool(pool);
                if pool.is_mapped() {
                    out_params.state = MemoryPoolState::Attached;
                    return MemoryPoolResultState::Success;
                }
                pool.set_cpu_address(0, 0);
                MemoryPoolResultState::MapFailed
            }
            MemoryPoolState::RequestDetach => {
                if pool.get_cpu_address() != in_params.address as CpuAddr
                    || pool.get_size() != in_params.size
                {
                    return MemoryPoolResultState::BadParam;
                }
                if pool.is_used() {
                    return MemoryPoolResultState::InUse;
                }
                self.unmap_pool(pool);
                out_params.state = MemoryPoolState::Detached;
                MemoryPoolResultState::Success
            }
            _ => MemoryPoolResultState::Success,
        }
    }

    pub fn initialize_system_pool(
        &self,
        pool: &mut MemoryPoolInfo,
        memory: &[u8],
        size: u64,
    ) -> bool {
        match pool.get_location() {
            PoolLocation::Cpu => false,
            PoolLocation::Dsp => {
                pool.set_cpu_address(memory.as_ptr() as CpuAddr, size);
                pool.set_dsp_address(pool.get_cpu_address());
                true
            }
        }
    }
}
