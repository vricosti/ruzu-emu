//! Port of zuyu/src/core/hle/kernel/svc/svc_physical_memory.cpp
//! Status: Ported
//! Derniere synchro: 2026-03-20
//!
//! SVC handlers for physical memory operations (SetHeapSize, MapPhysicalMemory, etc.).

use crate::core::System;
use crate::hle::kernel::k_memory_layout::MAIN_MEMORY_SIZE_MAX;
use crate::hle::kernel::svc::svc_results::*;
use crate::hle::kernel::svc_common::HEAP_SIZE_ALIGNMENT;
use crate::hle::result::{ResultCode, RESULT_SUCCESS};

fn is_4kb_aligned(val: u64) -> bool {
    val % 4096 == 0
}

fn trace_physical_memory_svc(system: &System, name: &str, addr: u64, size: u64, result: u32) {
    if std::env::var_os("RUZU_TRACE_PHYS_MEMORY").is_none() {
        return;
    }
    let tid = system
        .current_thread()
        .and_then(|thread| thread.lock().ok().map(|guard| guard.get_thread_id()))
        .unwrap_or(0);
    eprintln!(
        "[PHYS_MEMORY] tid={} {} addr=0x{:X} size=0x{:X} result=0x{:08X}",
        tid, name, addr, size, result
    );
}

/// Set the process heap to a given size. Can both extend and shrink the heap.
pub fn set_heap_size(out_address: &mut u64, size: u64) -> ResultCode {
    log::trace!("svc::SetHeapSize called, heap_size=0x{:X}", size);

    let validation = validate_heap_size(size);
    if validation != RESULT_SUCCESS {
        return validation;
    }

    // Upstream note: This standalone function lacks system access.
    // Use set_heap_size_current_process instead.
    *out_address = 0;
    RESULT_NOT_IMPLEMENTED
}

fn validate_heap_size(size: u64) -> ResultCode {
    // Validate size.
    if size % (HEAP_SIZE_ALIGNMENT as u64) != 0 {
        return RESULT_INVALID_SIZE;
    }
    if size >= MAIN_MEMORY_SIZE_MAX as u64 {
        return RESULT_INVALID_SIZE;
    }
    RESULT_SUCCESS
}

/// Set the process heap to a given size using the current process page table.
///
/// Matches upstream ownership: current process -> page table -> SetHeapSize.
pub fn set_heap_size_current_process(
    system: &System,
    out_address: &mut u64,
    size: u64,
) -> ResultCode {
    let validation = validate_heap_size(size);
    if validation != RESULT_SUCCESS {
        return validation;
    }

    let (result, address, target_info) = {
        let process_arc = system.current_process_arc();
        let mut process = process_arc.lock().unwrap();
        let (result, address) = process.set_heap_size(size as usize);
        let target_info = super::svc_memory_history::target_address()
            .and_then(|target| process.page_table.query_info(target as usize));
        (result, address, target_info)
    };
    if result == RESULT_SUCCESS.get_inner_value() {
        *out_address = address.get();
    }
    super::svc_memory_history::record_heap(system, size, result, *out_address, target_info);
    if common::trace::is_enabled(common::trace::cat::HEAP) {
        common::trace::emit_raw(
            common::trace::cat::HEAP,
            &[size, result as u64, *out_address],
        );
    }
    ResultCode::new(result)
}

/// Maps memory at a desired address.
pub fn map_physical_memory(system: &System, addr: u64, size: u64) -> ResultCode {
    log::debug!(
        "svc::MapPhysicalMemory called, addr=0x{:016X}, size=0x{:X}",
        addr,
        size
    );

    if !is_4kb_aligned(addr) {
        log::error!("Address is not aligned to 4KB, 0x{:016X}", addr);
        trace_physical_memory_svc(
            system,
            "MapPhysicalMemory",
            addr,
            size,
            RESULT_INVALID_ADDRESS.get_inner_value(),
        );
        super::svc_memory_history::record_physical(
            system,
            super::svc_memory_history::MemoryHistoryKind::MapPhysicalMemory,
            addr,
            size,
            RESULT_INVALID_ADDRESS.get_inner_value(),
        );
        return RESULT_INVALID_ADDRESS;
    }
    if !is_4kb_aligned(size) {
        log::error!("Size is not aligned to 4KB, 0x{:X}", size);
        trace_physical_memory_svc(
            system,
            "MapPhysicalMemory",
            addr,
            size,
            RESULT_INVALID_SIZE.get_inner_value(),
        );
        super::svc_memory_history::record_physical(
            system,
            super::svc_memory_history::MemoryHistoryKind::MapPhysicalMemory,
            addr,
            size,
            RESULT_INVALID_SIZE.get_inner_value(),
        );
        return RESULT_INVALID_SIZE;
    }
    if size == 0 {
        log::error!("Size is zero");
        trace_physical_memory_svc(
            system,
            "MapPhysicalMemory",
            addr,
            size,
            RESULT_INVALID_SIZE.get_inner_value(),
        );
        super::svc_memory_history::record_physical(
            system,
            super::svc_memory_history::MemoryHistoryKind::MapPhysicalMemory,
            addr,
            size,
            RESULT_INVALID_SIZE.get_inner_value(),
        );
        return RESULT_INVALID_SIZE;
    }
    if addr >= addr.wrapping_add(size) {
        log::error!("Size causes 64-bit overflow of address");
        trace_physical_memory_svc(
            system,
            "MapPhysicalMemory",
            addr,
            size,
            RESULT_INVALID_MEMORY_REGION.get_inner_value(),
        );
        super::svc_memory_history::record_physical(
            system,
            super::svc_memory_history::MemoryHistoryKind::MapPhysicalMemory,
            addr,
            size,
            RESULT_INVALID_MEMORY_REGION.get_inner_value(),
        );
        return RESULT_INVALID_MEMORY_REGION;
    }

    let process_arc = system.current_process_arc();
    let mut process = process_arc.lock().unwrap();

    if process.get_total_system_resource_size() == 0 {
        log::error!("System Resource Size is zero");
        trace_physical_memory_svc(
            system,
            "MapPhysicalMemory",
            addr,
            size,
            RESULT_INVALID_STATE.get_inner_value(),
        );
        super::svc_memory_history::record_physical(
            system,
            super::svc_memory_history::MemoryHistoryKind::MapPhysicalMemory,
            addr,
            size,
            RESULT_INVALID_STATE.get_inner_value(),
        );
        return RESULT_INVALID_STATE;
    }

    let addr_kpa = crate::hle::kernel::k_typed_address::KProcessAddress::new(addr);
    if !process.page_table.contains(addr_kpa, size as usize) {
        log::error!(
            "Address is not within the address space, addr=0x{:016X}, size=0x{:016X}",
            addr,
            size
        );
        trace_physical_memory_svc(
            system,
            "MapPhysicalMemory",
            addr,
            size,
            RESULT_INVALID_MEMORY_REGION.get_inner_value(),
        );
        super::svc_memory_history::record_physical(
            system,
            super::svc_memory_history::MemoryHistoryKind::MapPhysicalMemory,
            addr,
            size,
            RESULT_INVALID_MEMORY_REGION.get_inner_value(),
        );
        return RESULT_INVALID_MEMORY_REGION;
    }
    if !process
        .page_table
        .is_in_alias_region(addr_kpa, size as usize)
    {
        log::error!(
            "Address is not within the alias region, addr=0x{:016X}, size=0x{:016X}",
            addr,
            size
        );
        trace_physical_memory_svc(
            system,
            "MapPhysicalMemory",
            addr,
            size,
            RESULT_INVALID_MEMORY_REGION.get_inner_value(),
        );
        super::svc_memory_history::record_physical(
            system,
            super::svc_memory_history::MemoryHistoryKind::MapPhysicalMemory,
            addr,
            size,
            RESULT_INVALID_MEMORY_REGION.get_inner_value(),
        );
        return RESULT_INVALID_MEMORY_REGION;
    }

    let result = process
        .page_table
        .map_physical_memory(addr_kpa, size as usize);
    super::svc_memory_history::record_physical(
        system,
        super::svc_memory_history::MemoryHistoryKind::MapPhysicalMemory,
        addr,
        size,
        result,
    );
    trace_physical_memory_svc(system, "MapPhysicalMemory", addr, size, result);
    ResultCode::new(result)
}

/// Unmaps memory previously mapped via MapPhysicalMemory.
pub fn unmap_physical_memory(system: &System, addr: u64, size: u64) -> ResultCode {
    log::debug!(
        "svc::UnmapPhysicalMemory called, addr=0x{:016X}, size=0x{:X}",
        addr,
        size
    );

    if !is_4kb_aligned(addr) {
        log::error!("Address is not aligned to 4KB, 0x{:016X}", addr);
        trace_physical_memory_svc(
            system,
            "UnmapPhysicalMemory",
            addr,
            size,
            RESULT_INVALID_ADDRESS.get_inner_value(),
        );
        super::svc_memory_history::record_physical(
            system,
            super::svc_memory_history::MemoryHistoryKind::UnmapPhysicalMemory,
            addr,
            size,
            RESULT_INVALID_ADDRESS.get_inner_value(),
        );
        return RESULT_INVALID_ADDRESS;
    }
    if !is_4kb_aligned(size) {
        log::error!("Size is not aligned to 4KB, 0x{:X}", size);
        trace_physical_memory_svc(
            system,
            "UnmapPhysicalMemory",
            addr,
            size,
            RESULT_INVALID_SIZE.get_inner_value(),
        );
        super::svc_memory_history::record_physical(
            system,
            super::svc_memory_history::MemoryHistoryKind::UnmapPhysicalMemory,
            addr,
            size,
            RESULT_INVALID_SIZE.get_inner_value(),
        );
        return RESULT_INVALID_SIZE;
    }
    if size == 0 {
        log::error!("Size is zero");
        trace_physical_memory_svc(
            system,
            "UnmapPhysicalMemory",
            addr,
            size,
            RESULT_INVALID_SIZE.get_inner_value(),
        );
        super::svc_memory_history::record_physical(
            system,
            super::svc_memory_history::MemoryHistoryKind::UnmapPhysicalMemory,
            addr,
            size,
            RESULT_INVALID_SIZE.get_inner_value(),
        );
        return RESULT_INVALID_SIZE;
    }
    if addr >= addr.wrapping_add(size) {
        log::error!("Size causes 64-bit overflow of address");
        trace_physical_memory_svc(
            system,
            "UnmapPhysicalMemory",
            addr,
            size,
            RESULT_INVALID_MEMORY_REGION.get_inner_value(),
        );
        super::svc_memory_history::record_physical(
            system,
            super::svc_memory_history::MemoryHistoryKind::UnmapPhysicalMemory,
            addr,
            size,
            RESULT_INVALID_MEMORY_REGION.get_inner_value(),
        );
        return RESULT_INVALID_MEMORY_REGION;
    }

    let process_arc = system.current_process_arc();
    let mut process = process_arc.lock().unwrap();

    if process.get_total_system_resource_size() == 0 {
        log::error!("System Resource Size is zero");
        trace_physical_memory_svc(
            system,
            "UnmapPhysicalMemory",
            addr,
            size,
            RESULT_INVALID_STATE.get_inner_value(),
        );
        super::svc_memory_history::record_physical(
            system,
            super::svc_memory_history::MemoryHistoryKind::UnmapPhysicalMemory,
            addr,
            size,
            RESULT_INVALID_STATE.get_inner_value(),
        );
        return RESULT_INVALID_STATE;
    }

    let addr_kpa = crate::hle::kernel::k_typed_address::KProcessAddress::new(addr);
    if !process.page_table.contains(addr_kpa, size as usize) {
        log::error!(
            "Address is not within the address space, addr=0x{:016X}, size=0x{:016X}",
            addr,
            size
        );
        trace_physical_memory_svc(
            system,
            "UnmapPhysicalMemory",
            addr,
            size,
            RESULT_INVALID_MEMORY_REGION.get_inner_value(),
        );
        super::svc_memory_history::record_physical(
            system,
            super::svc_memory_history::MemoryHistoryKind::UnmapPhysicalMemory,
            addr,
            size,
            RESULT_INVALID_MEMORY_REGION.get_inner_value(),
        );
        return RESULT_INVALID_MEMORY_REGION;
    }
    if !process
        .page_table
        .is_in_alias_region(addr_kpa, size as usize)
    {
        log::error!(
            "Address is not within the alias region, addr=0x{:016X}, size=0x{:016X}",
            addr,
            size
        );
        trace_physical_memory_svc(
            system,
            "UnmapPhysicalMemory",
            addr,
            size,
            RESULT_INVALID_MEMORY_REGION.get_inner_value(),
        );
        super::svc_memory_history::record_physical(
            system,
            super::svc_memory_history::MemoryHistoryKind::UnmapPhysicalMemory,
            addr,
            size,
            RESULT_INVALID_MEMORY_REGION.get_inner_value(),
        );
        return RESULT_INVALID_MEMORY_REGION;
    }

    let result = process
        .page_table
        .unmap_physical_memory(addr_kpa, size as usize);
    super::svc_memory_history::record_physical(
        system,
        super::svc_memory_history::MemoryHistoryKind::UnmapPhysicalMemory,
        addr,
        size,
        result,
    );
    trace_physical_memory_svc(system, "UnmapPhysicalMemory", addr, size, result);
    ResultCode::new(result)
}

/// Maps physical memory unsafely. (Upstream: UNIMPLEMENTED)
pub fn map_physical_memory_unsafe(_address: u64, _size: u64) -> ResultCode {
    log::warn!("svc::MapPhysicalMemoryUnsafe: Upstream UNIMPLEMENTED");
    RESULT_NOT_IMPLEMENTED
}

/// Unmaps physical memory unsafely. (Upstream: UNIMPLEMENTED)
pub fn unmap_physical_memory_unsafe(_address: u64, _size: u64) -> ResultCode {
    log::warn!("svc::UnmapPhysicalMemoryUnsafe: Upstream UNIMPLEMENTED");
    RESULT_NOT_IMPLEMENTED
}

/// Sets the unsafe memory limit. (Upstream: UNIMPLEMENTED)
pub fn set_unsafe_limit(_limit: u64) -> ResultCode {
    log::warn!("svc::SetUnsafeLimit: Upstream UNIMPLEMENTED");
    RESULT_NOT_IMPLEMENTED
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejected_heap_size_preserves_output_address() {
        let system = System::new_for_test();
        for size in [1, 8 << 30] {
            let mut address = 0x12345678;
            assert_eq!(
                set_heap_size_current_process(&system, &mut address, size),
                RESULT_INVALID_SIZE
            );
            assert_eq!(address, 0x12345678);
        }
    }

    #[test]
    fn heap_size_validation_matches_nx2_eight_gib_exclusive_bound() {
        for size in [0, 2 << 20, 4 << 30, 0x1FE200000, (8 << 30) - (2 << 20)] {
            assert_eq!(validate_heap_size(size), RESULT_SUCCESS, "size={size:#x}");
        }
        for size in [1, (4 << 30) + 1, 8 << 30, 12 << 30, u64::MAX] {
            assert_eq!(validate_heap_size(size), RESULT_INVALID_SIZE, "size={size:#x}");
        }
    }
}
