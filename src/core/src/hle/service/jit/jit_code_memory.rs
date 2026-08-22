// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of Eden `src/core/hle/service/jit/jit_code_memory.{h,cpp}`.

use std::sync::{Arc, Mutex};

use crate::hle::kernel::k_code_memory::KCodeMemory;
use crate::hle::kernel::k_memory_block::PAGE_SIZE;
use crate::hle::kernel::k_process::ProcessLock;
use crate::hle::kernel::k_typed_address::KProcessAddress;
use crate::hle::kernel::svc::svc_results::RESULT_INVALID_MEMORY_REGION;
use crate::hle::kernel::svc::svc_types::MemoryPermission;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};

/// Owns one additional reference to a kernel code-memory object while its pages
/// are mapped into the JIT process's alias-code region.
pub struct CodeMemory {
    code_memory: Option<Arc<Mutex<KCodeMemory>>>,
    size: usize,
    address: u64,
    permission: MemoryPermission,
}

impl CodeMemory {
    pub fn new() -> Self {
        Self {
            code_memory: None,
            size: 0,
            address: 0,
            permission: MemoryPermission::None,
        }
    }

    /// Port of upstream `CodeMemory::Initialize`.
    pub fn initialize(
        &mut self,
        process: &Arc<ProcessLock>,
        code_memory: &Arc<Mutex<KCodeMemory>>,
        size: usize,
        permission: MemoryPermission,
        generate_random: &mut impl FnMut() -> u64,
    ) -> ResultCode {
        let (alias_code_start, alias_code_size) = {
            let process = process.lock().unwrap();
            (
                process.page_table.get_alias_code_region_start().get() / PAGE_SIZE as u64,
                process.page_table.get_alias_code_region_size() as u64 / PAGE_SIZE as u64,
            )
        };

        // Upstream deliberately retries without a limit when this exact result
        // reports that the sampled alias-code address cannot be mapped.
        loop {
            let mapped_address =
                (alias_code_start + generate_random() % alias_code_size) * PAGE_SIZE as u64;

            let owner = code_memory
                .lock()
                .unwrap()
                .get_owner()
                .expect("initialized KCodeMemory must retain its owner");
            let mut owner = owner.lock().unwrap();
            let result = code_memory.lock().unwrap().map_to_owner(
                &mut owner,
                KProcessAddress::new(mapped_address),
                size,
                permission,
            );
            if result == RESULT_INVALID_MEMORY_REGION {
                continue;
            }
            if result != RESULT_SUCCESS {
                return result;
            }

            self.code_memory = Some(Arc::clone(code_memory));
            self.size = size;
            self.address = mapped_address;
            self.permission = permission;
            return RESULT_SUCCESS;
        }
    }

    /// Port of upstream `CodeMemory::Finalize`.
    pub fn finalize(&mut self) {
        if let Some(code_memory) = self.code_memory.take() {
            let owner = code_memory
                .lock()
                .unwrap()
                .get_owner()
                .expect("mapped KCodeMemory must retain its owner");
            let mut owner = owner.lock().unwrap();
            let result = code_memory.lock().unwrap().unmap_from_owner(
                &mut owner,
                KProcessAddress::new(self.address),
                self.size,
            );
            assert_eq!(result, RESULT_SUCCESS);
        }
    }

    pub fn get_size(&self) -> usize {
        self.size
    }

    pub fn get_address(&self) -> u64 {
        self.address
    }
}

impl Default for CodeMemory {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::SystemRef;
    use crate::device_memory::DeviceMemory;
    use crate::hle::kernel::k_memory_block::{KMemoryPermission, KMemoryState};
    use crate::hle::kernel::k_memory_manager::Pool;
    use crate::hle::kernel::k_process::KProcess;
    use crate::hle::kernel::kernel::ScopedKernelForTest;
    use crate::memory::memory::Memory;

    struct Fixture {
        process: Arc<ProcessLock>,
        code_memory: Arc<Mutex<KCodeMemory>>,
        mapped_address: u64,
        _memory: Arc<std::sync::Mutex<Memory>>,
        _device_memory: Box<DeviceMemory>,
        _kernel: ScopedKernelForTest,
    }

    fn fixture(random_page: u64) -> Fixture {
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
        let source = KProcessAddress::new(0x10_0000);
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
            assert_eq!(
                process.page_table.map_pages_at_address(
                    source,
                    1,
                    KMemoryState::NORMAL,
                    KMemoryPermission::USER_READ_WRITE,
                ),
                0
            );
        }

        let code_memory = Arc::new(Mutex::new(KCodeMemory::new()));
        assert_eq!(
            code_memory.lock().unwrap().initialize(
                device_memory.as_ref(),
                &process,
                source,
                PAGE_SIZE,
            ),
            RESULT_SUCCESS
        );

        let alias_start = process
            .lock()
            .unwrap()
            .page_table
            .get_alias_code_region_start()
            .get()
            / PAGE_SIZE as u64;
        let alias_size = process
            .lock()
            .unwrap()
            .page_table
            .get_alias_code_region_size() as u64
            / PAGE_SIZE as u64;

        Fixture {
            process,
            code_memory,
            mapped_address: (alias_start + random_page % alias_size) * PAGE_SIZE as u64,
            _memory: memory,
            _device_memory: device_memory,
            _kernel: kernel,
        }
    }

    #[test]
    fn initialize_maps_sampled_alias_page_and_finalize_unmaps_it() {
        let fixture = fixture(0x200);
        let mut jit_memory = CodeMemory::new();
        let mut random = || 0x200;

        assert_eq!(
            jit_memory.initialize(
                &fixture.process,
                &fixture.code_memory,
                PAGE_SIZE,
                MemoryPermission::ReadExecute,
                &mut random,
            ),
            RESULT_SUCCESS
        );
        assert_eq!(jit_memory.get_size(), PAGE_SIZE);
        assert_eq!(jit_memory.get_address(), fixture.mapped_address);

        let info = fixture
            .process
            .lock()
            .unwrap()
            .page_table
            .query_info(fixture.mapped_address as usize)
            .unwrap();
        assert_eq!(info.m_state, KMemoryState::GENERATED_CODE);
        assert_eq!(info.m_permission, KMemoryPermission::USER_READ_EXECUTE);

        jit_memory.finalize();
        let info = fixture
            .process
            .lock()
            .unwrap()
            .page_table
            .query_info(fixture.mapped_address as usize)
            .unwrap();
        assert_eq!(info.m_state, KMemoryState::FREE);
        assert_eq!(info.m_permission, KMemoryPermission::NONE);

        fixture.code_memory.lock().unwrap().finalize();
    }
}
