//! Port of zuyu/src/core/hle/kernel/k_code_memory.h/.cpp
//!
//! KCodeMemory: kernel object for mapping executable code between processes.

use std::sync::Arc;

use super::k_memory_block::{KMemoryPermission, KMemoryState, PAGE_SIZE};
use super::k_page_group::KPageGroup;
use super::k_process::{KProcess, ProcessLock};
use super::k_typed_address::KProcessAddress;
use super::svc::svc_results::{RESULT_INVALID_SIZE, RESULT_INVALID_STATE};
use super::svc::svc_types::MemoryPermission;
use crate::device_memory::DeviceMemory;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};

/// Operations that can be performed on code memory.
/// Maps to upstream `CodeMemoryOperation`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeMemoryOperation {
    Map = 0,
    MapToOwner = 1,
    Unmap = 2,
    UnmapFromOwner = 3,
}

impl TryFrom<u32> for CodeMemoryOperation {
    type Error = ResultCode;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Map),
            1 => Ok(Self::MapToOwner),
            2 => Ok(Self::Unmap),
            3 => Ok(Self::UnmapFromOwner),
            _ => Err(super::svc::svc_results::RESULT_INVALID_ENUM_VALUE),
        }
    }
}

/// `KCodeMemory` shares executable pages between its owner and another process.
pub struct KCodeMemory {
    page_group: Option<KPageGroup>,
    owner: Option<Arc<ProcessLock>>,
    address: KProcessAddress,
    is_initialized: bool,
    is_owner_mapped: bool,
    is_mapped: bool,
}

impl KCodeMemory {
    pub fn new() -> Self {
        Self {
            page_group: None,
            owner: None,
            address: KProcessAddress::new(0),
            is_initialized: false,
            is_owner_mapped: false,
            is_mapped: false,
        }
    }

    /// Port of upstream `KCodeMemory::Initialize`.
    pub fn initialize(
        &mut self,
        device_memory: &DeviceMemory,
        owner: &Arc<ProcessLock>,
        address: KProcessAddress,
        size: usize,
    ) -> ResultCode {
        let mut owner_guard = owner.lock().unwrap();
        let mut page_group =
            KPageGroup::with_block_info_manager(owner_guard.page_table.get_block_info_manager());
        let result = owner_guard
            .page_table
            .lock_for_code_memory(&mut page_group, address, size);
        if result != 0 {
            return ResultCode::new(result);
        }
        drop(owner_guard);

        for block in page_group.iter() {
            unsafe {
                std::ptr::write_bytes(
                    device_memory.get_pointer(block.get_address()),
                    0xFF,
                    block.get_size(),
                );
            }
        }

        self.page_group = Some(page_group);
        self.owner = Some(Arc::clone(owner));
        self.address = address;
        self.is_initialized = true;
        self.is_owner_mapped = false;
        self.is_mapped = false;
        RESULT_SUCCESS
    }

    /// Finalize while the caller already owns the process lock.
    pub fn finalize_with_owner(&mut self, owner: &mut KProcess) {
        let Some(mut page_group) = self.page_group.take() else {
            self.owner = None;
            self.is_initialized = false;
            return;
        };

        if !self.is_mapped && !self.is_owner_mapped {
            let size = page_group.get_num_pages() * PAGE_SIZE;
            let _ = owner
                .page_table
                .unlock_for_code_memory(self.address, size, &page_group);
        }

        page_group.close();
        page_group.finalize();
        self.owner = None;
        self.is_initialized = false;
    }

    /// Port of upstream `KCodeMemory::Finalize`.
    pub fn finalize(&mut self) {
        let owner = self.owner.clone();
        if let Some(owner) = owner {
            self.finalize_with_owner(&mut owner.lock().unwrap());
        } else {
            self.page_group = None;
            self.owner = None;
            self.is_initialized = false;
        }
    }

    /// Port of upstream `KCodeMemory::Map`.
    pub fn map(
        &mut self,
        process: &mut KProcess,
        address: KProcessAddress,
        size: usize,
    ) -> ResultCode {
        let Some(page_group) = self.page_group.as_ref() else {
            return RESULT_INVALID_STATE;
        };
        if page_group.get_num_pages() != size.div_ceil(PAGE_SIZE) {
            return RESULT_INVALID_SIZE;
        }
        if self.is_mapped {
            return RESULT_INVALID_STATE;
        }

        let result = process.page_table.map_page_group(
            address,
            page_group,
            KMemoryState::CODE_OUT,
            KMemoryPermission::USER_READ_WRITE,
        );
        if result != 0 {
            return ResultCode::new(result);
        }
        self.is_mapped = true;
        RESULT_SUCCESS
    }

    /// Port of upstream `KCodeMemory::Unmap`.
    pub fn unmap(
        &mut self,
        process: &mut KProcess,
        address: KProcessAddress,
        size: usize,
    ) -> ResultCode {
        let Some(page_group) = self.page_group.as_ref() else {
            return RESULT_INVALID_STATE;
        };
        if page_group.get_num_pages() != size.div_ceil(PAGE_SIZE) {
            return RESULT_INVALID_SIZE;
        }

        let result =
            process
                .page_table
                .unmap_page_group(address, page_group, KMemoryState::CODE_OUT);
        if result != 0 {
            return ResultCode::new(result);
        }
        self.is_mapped = false;
        RESULT_SUCCESS
    }

    /// Port of upstream `KCodeMemory::MapToOwner`.
    pub fn map_to_owner(
        &mut self,
        owner: &mut KProcess,
        address: KProcessAddress,
        size: usize,
        permission: MemoryPermission,
    ) -> ResultCode {
        let Some(page_group) = self.page_group.as_ref() else {
            return RESULT_INVALID_STATE;
        };
        if page_group.get_num_pages() != size.div_ceil(PAGE_SIZE) {
            return RESULT_INVALID_SIZE;
        }
        if self.is_owner_mapped {
            return RESULT_INVALID_STATE;
        }

        let kernel_permission = match permission {
            MemoryPermission::Read => KMemoryPermission::USER_READ,
            MemoryPermission::ReadExecute => KMemoryPermission::USER_READ_EXECUTE,
            _ => unreachable!("ControlCodeMemory validates owner-map permissions"),
        };
        let result = owner.page_table.map_page_group(
            address,
            page_group,
            KMemoryState::GENERATED_CODE,
            kernel_permission,
        );
        if result != 0 {
            return ResultCode::new(result);
        }
        self.is_owner_mapped = true;
        RESULT_SUCCESS
    }

    /// Port of upstream `KCodeMemory::UnmapFromOwner`.
    pub fn unmap_from_owner(
        &mut self,
        owner: &mut KProcess,
        address: KProcessAddress,
        size: usize,
    ) -> ResultCode {
        let Some(page_group) = self.page_group.as_ref() else {
            return RESULT_INVALID_STATE;
        };
        if page_group.get_num_pages() != size.div_ceil(PAGE_SIZE) {
            return RESULT_INVALID_SIZE;
        }
        let result =
            owner
                .page_table
                .unmap_page_group(address, page_group, KMemoryState::GENERATED_CODE);
        if result != 0 {
            return ResultCode::new(result);
        }
        self.is_owner_mapped = false;
        RESULT_SUCCESS
    }

    pub fn is_initialized(&self) -> bool {
        self.is_initialized
    }

    pub fn get_owner(&self) -> Option<Arc<ProcessLock>> {
        self.owner.clone()
    }

    pub fn get_source_address(&self) -> KProcessAddress {
        self.address
    }

    pub fn get_size(&self) -> usize {
        if self.is_initialized {
            self.page_group
                .as_ref()
                .map_or(0, |group| group.get_num_pages() * PAGE_SIZE)
        } else {
            0
        }
    }
}

impl Default for KCodeMemory {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for KCodeMemory {
    fn drop(&mut self) {
        if self.is_initialized {
            self.finalize();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::SystemRef;
    use crate::device_memory::DeviceMemory;
    use crate::hle::kernel::k_memory_block::KMemoryAttribute;
    use crate::hle::kernel::k_memory_manager::{Direction, KMemoryManager, Pool};
    use crate::hle::kernel::kernel::ScopedKernelForTest;
    use crate::memory::memory::Memory;

    fn initialized_code_memory(owner: &Arc<ProcessLock>, pages: usize) -> KCodeMemory {
        let mut page_group = KPageGroup::new();
        let option = KMemoryManager::encode_option(Pool::Application, Direction::FromFront);
        assert_eq!(
            crate::hle::kernel::kernel::get_kernel_mut()
                .unwrap()
                .memory_manager_mut()
                .allocate_and_open(&mut page_group, pages, option),
            0
        );
        KCodeMemory {
            page_group: Some(page_group),
            owner: Some(Arc::clone(owner)),
            address: KProcessAddress::new(0x20_0000),
            is_initialized: false,
            is_owner_mapped: false,
            is_mapped: false,
        }
    }

    struct ProcessFixture {
        process: Arc<ProcessLock>,
        _memory: Arc<std::sync::Mutex<Memory>>,
        _device_memory: Box<DeviceMemory>,
        _kernel: ScopedKernelForTest,
    }

    fn configured_process() -> ProcessFixture {
        let mut kernel = ScopedKernelForTest::new();
        kernel
            .kernel_mut()
            .initialize_memory_block_slab_manager(4096);
        kernel.kernel_mut().initialize_block_info_manager(4096);
        kernel.memory_manager_mut().initialize_pool(
            Pool::Application,
            crate::device_memory::dram_memory_map::BASE,
            0x20_0000,
        );
        let device_memory = Box::new(DeviceMemory::with_size(0x20_0000));
        let memory = Arc::new(std::sync::Mutex::new(unsafe {
            Memory::new(
                SystemRef::null(),
                device_memory.as_ref() as *const _,
                &device_memory.buffer as *const _,
            )
        }));
        let process = Arc::new(ProcessLock::new(KProcess::new()));
        {
            let mut process = process.lock().unwrap();
            process
                .page_table
                .configure_address_space(KProcessAddress::new(0), 0x1_0000_0000, 32);
            process
                .page_table
                .set_code_region(KProcessAddress::new(0x20_0000), 0x20_0000);
            process
                .page_table
                .set_heap_region(KProcessAddress::new(0x10_0000), 0x20_0000);
            process.page_table.set_memory(Arc::clone(&memory));
        }
        ProcessFixture {
            process,
            _memory: memory,
            _device_memory: device_memory,
            _kernel: kernel,
        }
    }

    #[test]
    fn owner_mapping_uses_generated_code_state_and_exact_permission() {
        let fixture = configured_process();
        let owner = &fixture.process;
        let mut code_memory = initialized_code_memory(&owner, 2);
        let mapped_address = KProcessAddress::new(0x40_0000);

        {
            let mut owner_guard = owner.lock().unwrap();
            assert_eq!(
                code_memory.map_to_owner(
                    &mut owner_guard,
                    mapped_address,
                    2 * PAGE_SIZE,
                    MemoryPermission::ReadExecute,
                ),
                RESULT_SUCCESS
            );
            let info = owner_guard
                .page_table
                .query_info(mapped_address.get() as usize)
                .unwrap();
            assert_eq!(info.m_state, KMemoryState::GENERATED_CODE);
            assert_eq!(info.m_permission, KMemoryPermission::USER_READ_EXECUTE);

            assert_eq!(
                code_memory.unmap_from_owner(&mut owner_guard, mapped_address, 2 * PAGE_SIZE,),
                RESULT_SUCCESS
            );
            let info = owner_guard
                .page_table
                .query_info(mapped_address.get() as usize)
                .unwrap();
            assert_eq!(info.m_state, KMemoryState::FREE);
            assert_eq!(info.m_permission, KMemoryPermission::NONE);
        }

        code_memory.page_group = None;
        code_memory.owner = None;
    }

    #[test]
    fn initialize_locks_and_clears_source_then_finalize_restores_it() {
        let fixture = configured_process();
        let source = KProcessAddress::new(0x10_0000);
        {
            let mut owner = fixture.process.lock().unwrap();
            assert_eq!(
                owner.page_table.map_pages_at_address(
                    source,
                    1,
                    KMemoryState::NORMAL,
                    KMemoryPermission::USER_READ_WRITE,
                ),
                0
            );
        }
        fixture
            ._memory
            .lock()
            .unwrap()
            .write_block(source.get(), &[0x23; PAGE_SIZE]);

        let mut code_memory = KCodeMemory::new();
        assert_eq!(
            code_memory.initialize(
                fixture._device_memory.as_ref(),
                &fixture.process,
                source,
                PAGE_SIZE,
            ),
            RESULT_SUCCESS
        );
        assert_eq!(code_memory.get_source_address(), source);
        assert_eq!(code_memory.get_size(), PAGE_SIZE);
        let physical_address = code_memory
            .page_group
            .as_ref()
            .unwrap()
            .iter()
            .next()
            .unwrap()
            .get_address();
        let bytes = unsafe {
            std::slice::from_raw_parts(
                fixture._device_memory.get_pointer_const(physical_address),
                PAGE_SIZE,
            )
        };
        assert!(
            bytes.iter().all(|&byte| byte == 0xFF),
            "first bytes after initialize: {:02X?}",
            &bytes[..16]
        );
        {
            let owner = fixture.process.lock().unwrap();
            let info = owner.page_table.query_info(source.get() as usize).unwrap();
            assert_eq!(info.m_state, KMemoryState::NORMAL);
            assert_eq!(
                info.m_permission,
                KMemoryPermission::KERNEL_READ_WRITE | KMemoryPermission::NOT_MAPPED
            );
            assert_eq!(info.m_attribute, KMemoryAttribute::LOCKED);
        }

        code_memory.finalize_with_owner(&mut fixture.process.lock().unwrap());
        let owner = fixture.process.lock().unwrap();
        let info = owner.page_table.query_info(source.get() as usize).unwrap();
        assert_eq!(info.m_state, KMemoryState::NORMAL);
        assert_eq!(info.m_permission, KMemoryPermission::USER_READ_WRITE);
        assert_eq!(info.m_attribute, KMemoryAttribute::NONE);
    }

    #[test]
    fn map_rejects_wrong_size_and_second_mapping() {
        let fixture = configured_process();
        let process = &fixture.process;
        let mut code_memory = initialized_code_memory(&process, 1);

        {
            let mut process_guard = process.lock().unwrap();
            assert_eq!(
                code_memory.map(
                    &mut process_guard,
                    KProcessAddress::new(0x40_0000),
                    2 * PAGE_SIZE,
                ),
                RESULT_INVALID_SIZE
            );
            assert_eq!(
                code_memory.map(
                    &mut process_guard,
                    KProcessAddress::new(0x40_0000),
                    PAGE_SIZE,
                ),
                RESULT_SUCCESS
            );
            assert_eq!(
                code_memory.map(
                    &mut process_guard,
                    KProcessAddress::new(0x41_0000),
                    PAGE_SIZE,
                ),
                RESULT_INVALID_STATE
            );
            assert_eq!(
                code_memory.unmap(
                    &mut process_guard,
                    KProcessAddress::new(0x40_0000),
                    PAGE_SIZE,
                ),
                RESULT_SUCCESS
            );
        }

        code_memory.page_group = None;
        code_memory.owner = None;
    }

    #[test]
    fn code_memory_operation_rejects_unknown_raw_values() {
        assert_eq!(
            CodeMemoryOperation::try_from(4),
            Err(super::super::svc::svc_results::RESULT_INVALID_ENUM_VALUE)
        );
    }
}
