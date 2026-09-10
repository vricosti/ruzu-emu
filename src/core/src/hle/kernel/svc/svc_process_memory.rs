//! Port of zuyu/src/core/hle/kernel/svc/svc_process_memory.cpp
//! Status: Ported
//! Derniere synchro: 2026-03-20
//!
//! SVC handlers for process memory operations.

use crate::core::System;
use crate::hle::kernel::svc::svc_results::*;
use crate::hle::kernel::svc::svc_types::*;
use crate::hle::kernel::svc_common::Handle;
use crate::hle::result::ResultCode;

fn is_valid_address_range(address: u64, size: u64) -> bool {
    address.checked_add(size).map_or(false, |end| end > address)
}

fn is_valid_process_memory_permission(perm: MemoryPermission) -> bool {
    matches!(
        perm,
        MemoryPermission::None
            | MemoryPermission::Read
            | MemoryPermission::ReadWrite
            | MemoryPermission::ReadExecute
    )
}

fn is_4kb_aligned(val: u64) -> bool {
    val % 4096 == 0
}

/// Sets process memory permission.
pub fn set_process_memory_permission(
    system: &System,
    process_handle: Handle,
    address: u64,
    size: u64,
    perm: MemoryPermission,
) -> ResultCode {
    log::trace!(
        "svc::SetProcessMemoryPermission called, handle=0x{:X}, addr=0x{:X}, size=0x{:X}, perm={:?}",
        process_handle, address, size, perm
    );

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
    if !is_valid_process_memory_permission(perm) {
        return RESULT_INVALID_NEW_MEMORY_PERMISSION;
    }

    let process_arc = resolve_process_handle(system, process_handle);
    let process_arc = match process_arc {
        Some(p) => p,
        None => return RESULT_INVALID_HANDLE,
    };

    let mut process = process_arc.lock().unwrap();
    let addr_kpa = crate::hle::kernel::k_typed_address::KProcessAddress::new(address);

    // Validate that the address is in range.
    if !process.page_table.contains(addr_kpa, size as usize) {
        return RESULT_INVALID_CURRENT_MEMORY;
    }

    // Set the memory permission.
    use crate::hle::kernel::k_memory_block::{
        convert_to_k_memory_permission, SvcMemoryPermission,
    };
    let k_perm = convert_to_k_memory_permission(SvcMemoryPermission::from_bits_truncate(perm as u8));
    let result = process
        .page_table
        .set_process_memory_permission(addr_kpa, size as usize, k_perm);
    ResultCode::new(result)
}

/// Maps process memory from src_process to dst (current) process.
pub fn map_process_memory(
    system: &System,
    dst_address: u64,
    process_handle: Handle,
    src_address: u64,
    size: u64,
) -> ResultCode {
    log::trace!(
        "svc::MapProcessMemory called, dst=0x{:X}, handle=0x{:X}, src=0x{:X}, size=0x{:X}",
        dst_address,
        process_handle,
        src_address,
        size
    );

    if dst_address % PAGE_SIZE != 0 {
        return RESULT_INVALID_ADDRESS;
    }
    if src_address % PAGE_SIZE != 0 {
        return RESULT_INVALID_ADDRESS;
    }
    if size % PAGE_SIZE != 0 {
        return RESULT_INVALID_SIZE;
    }
    if size == 0 {
        return RESULT_INVALID_SIZE;
    }
    if dst_address >= dst_address.wrapping_add(size) {
        return RESULT_INVALID_CURRENT_MEMORY;
    }
    if src_address >= src_address.wrapping_add(size) {
        return RESULT_INVALID_CURRENT_MEMORY;
    }

    let dst_process_arc = system.current_process_arc().clone();
    let src_process_arc = match resolve_process_handle(system, process_handle) {
        Some(p) => p,
        None => return RESULT_INVALID_HANDLE,
    };

    let src_kpa = crate::hle::kernel::k_typed_address::KProcessAddress::new(src_address);
    let dst_kpa = crate::hle::kernel::k_typed_address::KProcessAddress::new(dst_address);

    let mut page_group = crate::hle::kernel::k_page_group::KPageGroup::new();
    if std::sync::Arc::ptr_eq(&dst_process_arc, &src_process_arc) {
        let mut process = dst_process_arc.lock().unwrap();
        if !process.page_table.contains(src_kpa, size as usize) {
            return RESULT_INVALID_CURRENT_MEMORY;
        }
        if !process.page_table.can_contain(
            dst_kpa,
            size as usize,
            crate::hle::kernel::k_memory_block::KMemoryState::SHARED_CODE,
        ) {
            return RESULT_INVALID_MEMORY_REGION;
        }
        let result = process.page_table.make_and_open_page_group(
            &mut page_group,
            src_kpa,
            (size / PAGE_SIZE) as usize,
            crate::hle::kernel::k_memory_block::KMemoryState::FLAG_CAN_MAP_PROCESS,
            crate::hle::kernel::k_memory_block::KMemoryState::FLAG_CAN_MAP_PROCESS,
            crate::hle::kernel::k_memory_block::KMemoryPermission::NONE,
            crate::hle::kernel::k_memory_block::KMemoryPermission::NONE,
            crate::hle::kernel::k_memory_block::KMemoryAttribute::all(),
            crate::hle::kernel::k_memory_block::KMemoryAttribute::NONE,
        );
        if result != 0 {
            return ResultCode::new(result);
        }
        let result = process.page_table.map_page_group(
            dst_kpa,
            &page_group,
            crate::hle::kernel::k_memory_block::KMemoryState::SHARED_CODE,
            crate::hle::kernel::k_memory_block::KMemoryPermission::USER_READ_WRITE,
        );
        return ResultCode::new(result);
    }

    {
        let src_process = src_process_arc.lock().unwrap();
        if !src_process.page_table.contains(src_kpa, size as usize) {
            return RESULT_INVALID_CURRENT_MEMORY;
        }
        let result = src_process.page_table.make_and_open_page_group(
            &mut page_group,
            src_kpa,
            (size / PAGE_SIZE) as usize,
            crate::hle::kernel::k_memory_block::KMemoryState::FLAG_CAN_MAP_PROCESS,
            crate::hle::kernel::k_memory_block::KMemoryState::FLAG_CAN_MAP_PROCESS,
            crate::hle::kernel::k_memory_block::KMemoryPermission::NONE,
            crate::hle::kernel::k_memory_block::KMemoryPermission::NONE,
            crate::hle::kernel::k_memory_block::KMemoryAttribute::all(),
            crate::hle::kernel::k_memory_block::KMemoryAttribute::NONE,
        );
        if result != 0 {
            return ResultCode::new(result);
        }
    }

    let mut dst_process = dst_process_arc.lock().unwrap();
    if !dst_process.page_table.can_contain(
        dst_kpa,
        size as usize,
        crate::hle::kernel::k_memory_block::KMemoryState::SHARED_CODE,
    ) {
        return RESULT_INVALID_MEMORY_REGION;
    }
    let result = dst_process.page_table.map_page_group(
        dst_kpa,
        &page_group,
        crate::hle::kernel::k_memory_block::KMemoryState::SHARED_CODE,
        crate::hle::kernel::k_memory_block::KMemoryPermission::USER_READ_WRITE,
    );
    ResultCode::new(result)
}

/// Unmaps process memory.
pub fn unmap_process_memory(
    system: &System,
    dst_address: u64,
    process_handle: Handle,
    src_address: u64,
    size: u64,
) -> ResultCode {
    log::trace!(
        "svc::UnmapProcessMemory called, dst=0x{:X}, handle=0x{:X}, src=0x{:X}, size=0x{:X}",
        dst_address,
        process_handle,
        src_address,
        size
    );

    if dst_address % PAGE_SIZE != 0 {
        return RESULT_INVALID_ADDRESS;
    }
    if src_address % PAGE_SIZE != 0 {
        return RESULT_INVALID_ADDRESS;
    }
    if size % PAGE_SIZE != 0 {
        return RESULT_INVALID_SIZE;
    }
    if size == 0 {
        return RESULT_INVALID_SIZE;
    }
    if dst_address >= dst_address.wrapping_add(size) {
        return RESULT_INVALID_CURRENT_MEMORY;
    }
    if src_address >= src_address.wrapping_add(size) {
        return RESULT_INVALID_CURRENT_MEMORY;
    }

    let dst_process_arc = system.current_process_arc().clone();
    let src_process_arc = match resolve_process_handle(system, process_handle) {
        Some(p) => p,
        None => return RESULT_INVALID_HANDLE,
    };

    let src_kpa = crate::hle::kernel::k_typed_address::KProcessAddress::new(src_address);
    let dst_kpa = crate::hle::kernel::k_typed_address::KProcessAddress::new(dst_address);

    if std::sync::Arc::ptr_eq(&dst_process_arc, &src_process_arc) {
        let mut process = dst_process_arc.lock().unwrap();
        if !process.page_table.contains(src_kpa, size as usize) {
            return RESULT_INVALID_CURRENT_MEMORY;
        }
        if !process.page_table.can_contain(
            dst_kpa,
            size as usize,
            crate::hle::kernel::k_memory_block::KMemoryState::SHARED_CODE,
        ) {
            return RESULT_INVALID_MEMORY_REGION;
        }
        let result =
            process
                .page_table
                .unmap_process_memory_same_table(dst_kpa, size as usize, src_kpa);
        return ResultCode::new(result);
    }

    let src_process = src_process_arc.lock().unwrap();
    if !src_process.page_table.contains(src_kpa, size as usize) {
        return RESULT_INVALID_CURRENT_MEMORY;
    }
    let mut dst_process = dst_process_arc.lock().unwrap();
    if !dst_process.page_table.can_contain(
        dst_kpa,
        size as usize,
        crate::hle::kernel::k_memory_block::KMemoryState::SHARED_CODE,
    ) {
        return RESULT_INVALID_MEMORY_REGION;
    }
    let result = dst_process.page_table.unmap_process_memory(
        dst_kpa,
        size as usize,
        &src_process.page_table,
        src_kpa,
    );
    ResultCode::new(result)
}

/// Maps process code memory.
pub fn map_process_code_memory(
    system: &System,
    process_handle: Handle,
    dst_address: u64,
    src_address: u64,
    size: u64,
) -> ResultCode {
    log::debug!(
        "svc::MapProcessCodeMemory called. handle=0x{:08X}, dst=0x{:016X}, src=0x{:016X}, size=0x{:016X}",
        process_handle, dst_address, src_address, size
    );

    if !is_4kb_aligned(src_address) {
        log::error!("src_address is not page-aligned (0x{:016X})", src_address);
        return RESULT_INVALID_ADDRESS;
    }
    if !is_4kb_aligned(dst_address) {
        log::error!("dst_address is not page-aligned (0x{:016X})", dst_address);
        return RESULT_INVALID_ADDRESS;
    }
    if size == 0 || !is_4kb_aligned(size) {
        log::error!("Size is zero or not page-aligned (0x{:016X})", size);
        return RESULT_INVALID_SIZE;
    }
    if !is_valid_address_range(dst_address, size) {
        log::error!(
            "Destination address range overflows (0x{:016X}, 0x{:016X})",
            dst_address,
            size
        );
        return RESULT_INVALID_CURRENT_MEMORY;
    }
    if !is_valid_address_range(src_address, size) {
        log::error!(
            "Source address range overflows (0x{:016X}, 0x{:016X})",
            src_address,
            size
        );
        return RESULT_INVALID_CURRENT_MEMORY;
    }

    let process_arc = resolve_process_handle(system, process_handle);
    let process_arc = match process_arc {
        Some(p) => p,
        None => {
            log::error!(
                "Invalid process handle specified (handle=0x{:08X})",
                process_handle
            );
            return RESULT_INVALID_HANDLE;
        }
    };

    let mut process = process_arc.lock().unwrap();
    let src_kpa = crate::hle::kernel::k_typed_address::KProcessAddress::new(src_address);

    if !process.page_table.contains(src_kpa, size as usize) {
        log::error!(
            "Source address range is not within the address space (0x{:016X}, 0x{:016X})",
            src_address,
            size
        );
        return RESULT_INVALID_CURRENT_MEMORY;
    }

    let dst_kpa = crate::hle::kernel::k_typed_address::KProcessAddress::new(dst_address);
    let result = process
        .page_table
        .map_code_memory(dst_kpa, src_kpa, size as usize);
    if result == 0 {
        invalidate_process_instruction_cache(&mut process, dst_address, size);
    }
    ResultCode::new(result)
}

/// Unmaps process code memory.
pub fn unmap_process_code_memory(
    system: &System,
    process_handle: Handle,
    dst_address: u64,
    src_address: u64,
    size: u64,
) -> ResultCode {
    log::debug!(
        "svc::UnmapProcessCodeMemory called. handle=0x{:08X}, dst=0x{:016X}, src=0x{:016X}, size=0x{:016X}",
        process_handle, dst_address, src_address, size
    );

    if !is_4kb_aligned(dst_address) {
        log::error!("dst_address is not page-aligned (0x{:016X})", dst_address);
        return RESULT_INVALID_ADDRESS;
    }
    if !is_4kb_aligned(src_address) {
        log::error!("src_address is not page-aligned (0x{:016X})", src_address);
        return RESULT_INVALID_ADDRESS;
    }
    if size == 0 || !is_4kb_aligned(size) {
        log::error!("Size is zero or not page-aligned (0x{:016X})", size);
        return RESULT_INVALID_SIZE;
    }
    if !is_valid_address_range(dst_address, size) {
        log::error!(
            "Destination address range overflows (0x{:016X}, 0x{:016X})",
            dst_address,
            size
        );
        return RESULT_INVALID_CURRENT_MEMORY;
    }
    if !is_valid_address_range(src_address, size) {
        log::error!(
            "Source address range overflows (0x{:016X}, 0x{:016X})",
            src_address,
            size
        );
        return RESULT_INVALID_CURRENT_MEMORY;
    }

    let process_arc = resolve_process_handle(system, process_handle);
    let process_arc = match process_arc {
        Some(p) => p,
        None => {
            log::error!(
                "Invalid process handle specified (handle=0x{:08X})",
                process_handle
            );
            return RESULT_INVALID_HANDLE;
        }
    };

    let mut process = process_arc.lock().unwrap();
    let src_kpa = crate::hle::kernel::k_typed_address::KProcessAddress::new(src_address);

    if !process.page_table.contains(src_kpa, size as usize) {
        log::error!(
            "Source address range is not within the address space (0x{:016X}, 0x{:016X})",
            src_address,
            size
        );
        return RESULT_INVALID_CURRENT_MEMORY;
    }

    let dst_kpa = crate::hle::kernel::k_typed_address::KProcessAddress::new(dst_address);
    let result = process
        .page_table
        .unmap_code_memory(dst_kpa, src_kpa, size as usize);
    ResultCode::new(result)
}

fn invalidate_process_instruction_cache(
    process: &mut crate::hle::kernel::k_process::KProcess,
    address: u64,
    size: u64,
) {
    for core in 0..crate::hardware_properties::NUM_CPU_CORES as usize {
        if let Some(cpu) = process.get_arm_interface_mut(core) {
            cpu.invalidate_cache_range(address, size as usize);
        }
    }
}

fn resolve_process_handle(
    system: &System,
    handle: Handle,
) -> Option<std::sync::Arc<crate::hle::kernel::k_process::ProcessLock>> {
    let current = system.current_process_arc();
    let guard = current.lock().unwrap();
    let object_id = guard.handle_table.get_object(handle)?;
    let current_process_id = guard.get_process_id();
    drop(guard);

    if object_id == current_process_id {
        return Some(current.clone());
    }

    let kernel = system.kernel()?;
    kernel.get_process_by_id(object_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::kernel::k_process::{KProcess, ProcessLock};
    use std::sync::Arc;

    #[test]
    fn explicit_process_handle_resolves_current_process_id() {
        let mut system = System::new_for_test();
        let mut process = KProcess::new();
        process.process_id = 0x1234;
        process.initialize_handle_table();
        let process = Arc::new(ProcessLock::from_value(process));

        let process_handle = {
            let mut guard = process.lock().unwrap();
            let process_id = guard.get_process_id();
            guard.handle_table.add(process_id).unwrap()
        };
        system.set_current_process_arc(process.clone());

        let resolved = resolve_process_handle(&system, process_handle).unwrap();
        assert!(Arc::ptr_eq(&resolved, &process));
    }

    #[test]
    fn non_process_object_handle_is_rejected() {
        let mut system = System::new_for_test();
        let mut process = KProcess::new();
        process.process_id = 0x1234;
        process.initialize_handle_table();
        let process = Arc::new(ProcessLock::from_value(process));

        let event_like_handle = {
            let mut guard = process.lock().unwrap();
            guard.handle_table.add(0xE000_0000).unwrap()
        };
        system.set_current_process_arc(process);

        assert!(resolve_process_handle(&system, event_like_handle).is_none());
    }

    #[test]
    fn process_memory_resolver_rejects_pseudo_and_zero_handles() {
        let mut system = System::new_for_test();
        let mut process = KProcess::new();
        process.process_id = 0x1234;
        process.initialize_handle_table();
        let process = Arc::new(ProcessLock::from_value(process));
        system.set_current_process_arc(process);

        assert!(resolve_process_handle(&system, 0).is_none());
        assert!(resolve_process_handle(
            &system,
            crate::hle::kernel::svc_common::PseudoHandle::CurrentProcess as Handle,
        )
        .is_none());
    }
}
