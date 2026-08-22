//! Port of zuyu/src/core/hle/kernel/svc/svc_code_memory.cpp
//! Status: Ported
//! Derniere synchro: 2026-03-20
//!
//! SVC handlers for code memory operations.

use std::sync::{Arc, Mutex};

use crate::core::System;
use crate::hle::kernel::k_code_memory::{CodeMemoryOperation as KCodeMemoryOperation, KCodeMemory};
use crate::hle::kernel::svc::svc_results::*;
use crate::hle::kernel::svc::svc_types::*;
use crate::hle::kernel::svc_common::Handle;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};

fn is_valid_map_code_memory_permission(perm: MemoryPermission) -> bool {
    matches!(perm, MemoryPermission::ReadWrite)
}

fn is_valid_map_to_owner_code_memory_permission(perm: MemoryPermission) -> bool {
    matches!(perm, MemoryPermission::Read | MemoryPermission::ReadExecute)
}

fn is_valid_unmap_code_memory_permission(perm: MemoryPermission) -> bool {
    matches!(perm, MemoryPermission::None)
}

fn is_valid_unmap_from_owner_code_memory_permission(perm: MemoryPermission) -> bool {
    matches!(perm, MemoryPermission::None)
}

/// Creates a code memory object.
///
/// Upstream: Creates KCodeMemory, verifies region is in range, initializes,
/// registers, and adds to handle table.
pub fn create_code_memory(
    system: &System,
    out: &mut Handle,
    address: u64,
    size: u64,
) -> ResultCode {
    log::trace!(
        "svc::CreateCodeMemory called, address=0x{:X}, size=0x{:X}",
        address,
        size
    );

    // Validate address / size.
    if address % PAGE_SIZE != 0 {
        return RESULT_INVALID_ADDRESS;
    }
    if size % PAGE_SIZE != 0 {
        return RESULT_INVALID_SIZE;
    }
    if size == 0 {
        return RESULT_INVALID_SIZE;
    }
    if address >= address.wrapping_add(size) {
        return RESULT_INVALID_CURRENT_MEMORY;
    }

    let process_arc = system.current_process_arc();
    let addr_kpa = crate::hle::kernel::k_typed_address::KProcessAddress::new(address);
    if !process_arc
        .lock()
        .unwrap()
        .page_table
        .contains(addr_kpa, size as usize)
    {
        return RESULT_INVALID_CURRENT_MEMORY;
    }

    let code_mem_id = system
        .kernel()
        .expect("CreateCodeMemory requires an initialized kernel")
        .create_new_object_id() as u64;
    let code_memory = Arc::new(Mutex::new(KCodeMemory::new()));

    let result = code_memory.lock().unwrap().initialize(
        system.device_memory(),
        &process_arc,
        addr_kpa,
        size as usize,
    );
    if result != RESULT_SUCCESS {
        return result;
    }

    let mut process = process_arc.lock().unwrap();
    match process.handle_table.add(code_mem_id) {
        Ok(h) => {
            *out = h;
            process.register_code_memory_object(code_mem_id, code_memory);
        }
        Err(_) => {
            *out = 0;
            code_memory
                .lock()
                .unwrap()
                .finalize_with_owner(&mut process);
            return RESULT_OUT_OF_HANDLES;
        }
    }

    RESULT_SUCCESS
}

/// Controls a code memory object (map, unmap, map to owner, unmap from owner).
///
/// Upstream: Gets KCodeMemory from handle, validates region, performs the
/// requested operation.
pub fn control_code_memory(
    system: &System,
    code_memory_handle: Handle,
    operation: u32,
    address: u64,
    size: u64,
    perm: MemoryPermission,
) -> ResultCode {
    log::trace!(
        "svc::ControlCodeMemory called, handle=0x{:X}, op={:?}, addr=0x{:X}, size=0x{:X}, perm={:?}",
        code_memory_handle, operation, address, size, perm
    );

    // Validate the address / size.
    if address % PAGE_SIZE != 0 {
        return RESULT_INVALID_ADDRESS;
    }
    if size % PAGE_SIZE != 0 {
        return RESULT_INVALID_SIZE;
    }
    if size == 0 {
        return RESULT_INVALID_SIZE;
    }
    if address >= address.wrapping_add(size) {
        return RESULT_INVALID_CURRENT_MEMORY;
    }

    let process_arc = system.current_process_arc();
    let code_memory = {
        let process = process_arc.lock().unwrap();
        let object_id = match process.handle_table.get_object(code_memory_handle) {
            Some(id) => id,
            None => return RESULT_INVALID_HANDLE,
        };
        match process.get_code_memory_by_object_id(object_id) {
            Some(code_memory) => code_memory,
            None => return RESULT_INVALID_HANDLE,
        }
    };

    let addr_kpa = crate::hle::kernel::k_typed_address::KProcessAddress::new(address);

    let operation = match KCodeMemoryOperation::try_from(operation) {
        Ok(operation) => operation,
        Err(result) => return result,
    };

    match operation {
        KCodeMemoryOperation::Map => {
            let mut process = process_arc.lock().unwrap();
            // Check that the region is in range.
            if !process.page_table.can_contain(
                addr_kpa,
                size as usize,
                crate::hle::kernel::k_memory_block::KMemoryState::CODE_OUT,
            ) {
                return RESULT_INVALID_MEMORY_REGION;
            }

            // Check the memory permission.
            if !is_valid_map_code_memory_permission(perm) {
                return RESULT_INVALID_NEW_MEMORY_PERMISSION;
            }

            return code_memory
                .lock()
                .unwrap()
                .map(&mut process, addr_kpa, size as usize);
        }
        KCodeMemoryOperation::Unmap => {
            let mut process = process_arc.lock().unwrap();
            // Check that the region is in range.
            if !process.page_table.can_contain(
                addr_kpa,
                size as usize,
                crate::hle::kernel::k_memory_block::KMemoryState::CODE_OUT,
            ) {
                return RESULT_INVALID_MEMORY_REGION;
            }

            // Check the memory permission.
            if !is_valid_unmap_code_memory_permission(perm) {
                return RESULT_INVALID_NEW_MEMORY_PERMISSION;
            }

            return code_memory
                .lock()
                .unwrap()
                .unmap(&mut process, addr_kpa, size as usize);
        }
        KCodeMemoryOperation::MapToOwner => {
            let owner = match code_memory.lock().unwrap().get_owner() {
                Some(owner) => owner,
                None => return RESULT_INVALID_STATE,
            };
            let mut owner = owner.lock().unwrap();
            // Check that the region is in range.
            if !owner.page_table.can_contain(
                addr_kpa,
                size as usize,
                crate::hle::kernel::k_memory_block::KMemoryState::GENERATED_CODE,
            ) {
                return RESULT_INVALID_MEMORY_REGION;
            }

            // Check the memory permission.
            if !is_valid_map_to_owner_code_memory_permission(perm) {
                return RESULT_INVALID_NEW_MEMORY_PERMISSION;
            }

            return code_memory.lock().unwrap().map_to_owner(
                &mut owner,
                addr_kpa,
                size as usize,
                perm,
            );
        }
        KCodeMemoryOperation::UnmapFromOwner => {
            let owner = match code_memory.lock().unwrap().get_owner() {
                Some(owner) => owner,
                None => return RESULT_INVALID_STATE,
            };
            let mut owner = owner.lock().unwrap();
            // Check that the region is in range.
            if !owner.page_table.can_contain(
                addr_kpa,
                size as usize,
                crate::hle::kernel::k_memory_block::KMemoryState::GENERATED_CODE,
            ) {
                return RESULT_INVALID_MEMORY_REGION;
            }

            // Check the memory permission.
            if !is_valid_unmap_from_owner_code_memory_permission(perm) {
                return RESULT_INVALID_NEW_MEMORY_PERMISSION;
            }

            return code_memory.lock().unwrap().unmap_from_owner(
                &mut owner,
                addr_kpa,
                size as usize,
            );
        }
    }
}
