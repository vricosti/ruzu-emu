// SPDX-FileCopyrightText: Copyright 2023 yuzu Emulator Project
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of zuyu/src/core/hle/service/ro/ro_nro_utils.h
//! Port of zuyu/src/core/hle/service/ro/ro_nro_utils.cpp
//!
//! NRO memory mapping utility functions.

use super::ro_results;
use crate::hle::kernel::k_memory_block::PAGE_SIZE;
use crate::hle::kernel::k_process::KProcess;
use crate::hle::kernel::k_typed_address::KProcessAddress;
use crate::hle::kernel::svc::svc_types::MemoryPermission;
use crate::hle::result::ResultCode;

/// A region of process memory to be mapped.
///
/// Corresponds to `ProcessMemoryRegion` in upstream ro_nro_utils.cpp.
struct ProcessMemoryRegion {
    pub address: u64,
    pub size: u64,
}

/// Get total size of process memory regions.
///
/// Corresponds to `GetTotalProcessMemoryRegionSize` in upstream.
fn get_total_process_memory_region_size(regions: &[ProcessMemoryRegion]) -> u64 {
    regions.iter().map(|r| r.size).sum()
}

/// Set up NRO process memory regions (NRO + optional BSS).
///
/// Corresponds to `SetupNroProcessMemoryRegions` in upstream.
fn setup_nro_process_memory_regions(
    nro_heap_address: u64,
    nro_heap_size: u64,
    bss_heap_address: u64,
    bss_heap_size: u64,
) -> Vec<ProcessMemoryRegion> {
    let mut regions = vec![ProcessMemoryRegion {
        address: nro_heap_address,
        size: nro_heap_size,
    }];

    if bss_heap_size > 0 {
        regions.push(ProcessMemoryRegion {
            address: bss_heap_address,
            size: bss_heap_size,
        });
    }

    regions
}

/// Set memory permission on a process page table.
///
/// Corresponds to the local `SetProcessMemoryPermission` helper in upstream ro_nro_utils.cpp.
fn set_process_memory_permission(
    process: &mut KProcess,
    address: u64,
    size: u64,
    permission: MemoryPermission,
) -> Result<(), ResultCode> {
    use crate::hle::kernel::k_memory_block::{
        convert_to_k_memory_permission, SvcMemoryPermission,
    };
    // Rust's page-table entry point takes internal permissions, whereas the
    // C++ entry point converts SVC permissions inside SetProcessMemoryPermission.
    let k_perm = convert_to_k_memory_permission(SvcMemoryPermission::from_bits_truncate(
        permission as u8,
    ));
    let result = process.page_table.set_process_memory_permission(
        KProcessAddress::new(address),
        size as usize,
        k_perm,
    );
    if result != 0 {
        Err(ResultCode::new(result))
    } else {
        Ok(())
    }
}

/// Unmap process code memory for a set of regions in reverse order.
///
/// Corresponds to `UnmapProcessCodeMemory` in upstream ro_nro_utils.cpp.
fn unmap_process_code_memory(
    process: &mut KProcess,
    process_code_address: u64,
    regions: &[ProcessMemoryRegion],
) -> Result<(), ResultCode> {
    let total_size = get_total_process_memory_region_size(regions);

    // Unmap each region in reverse order.
    let mut cur_offset = total_size;
    for i in 0..regions.len() {
        let cur_region = &regions[regions.len() - 1 - i];

        cur_offset -= cur_region.size;

        let result = process.page_table.unmap_code_memory(
            KProcessAddress::new(process_code_address + cur_offset),
            KProcessAddress::new(cur_region.address),
            cur_region.size as usize,
        );
        if result != 0 {
            return Err(ResultCode::new(result));
        }
    }

    Ok(())
}

/// Ensure guard pages exist before and after a mapped region.
///
/// Corresponds to `EnsureGuardPages` in upstream ro_nro_utils.cpp.
fn ensure_guard_pages(
    process: &KProcess,
    map_address: u64,
    map_size: u64,
) -> Result<(), ResultCode> {
    use crate::hle::kernel::k_memory_block::SvcMemoryState;

    // Ensure page before mapping is unmapped.
    let info_before = process.page_table.query_info((map_address - 1) as usize);
    match info_before {
        Some(info) if info.get_svc_state() == SvcMemoryState::Free as u32 => {}
        _ => {
            return Err(crate::hle::kernel::svc::svc_results::RESULT_INVALID_STATE);
        }
    }

    // Ensure page after mapping is unmapped.
    let info_after = process
        .page_table
        .query_info((map_address + map_size) as usize);
    match info_after {
        Some(info) if info.get_svc_state() == SvcMemoryState::Free as u32 => {}
        _ => {
            return Err(crate::hle::kernel::svc::svc_results::RESULT_INVALID_STATE);
        }
    }

    Ok(())
}

/// Map process code memory: try random addresses in the alias code region.
///
/// Corresponds to `MapProcessCodeMemory` in upstream ro_nro_utils.cpp.
fn map_process_code_memory(
    process: &mut KProcess,
    regions: &[ProcessMemoryRegion],
    generate_random: &mut impl FnMut() -> u64,
) -> Result<u64, ResultCode> {
    let alias_code_start =
        process.page_table.get_alias_code_region_start().get() / PAGE_SIZE as u64;
    let alias_code_size = process.page_table.get_alias_code_region_size() as u64 / PAGE_SIZE as u64;

    for _trial in 0..64 {
        // Generate a new trial address.
        let mapped_address =
            (alias_code_start + (generate_random() % alias_code_size)) * PAGE_SIZE as u64;

        // Try to map all regions at this address.
        let map_result = (|| -> Result<(), ResultCode> {
            let mut mapped_size: u64 = 0;
            for (i, region) in regions.iter().enumerate() {
                let result = process.page_table.map_code_memory(
                    KProcessAddress::new(mapped_address + mapped_size),
                    KProcessAddress::new(region.address),
                    region.size as usize,
                );
                if result != 0 {
                    // Unmap what we've mapped so far (regions 0..i).
                    if i > 0 {
                        let _ = unmap_process_code_memory(process, mapped_address, &regions[..i]);
                    }
                    return Err(ResultCode::new(result));
                }
                mapped_size += region.size;
            }

            // All regions mapped; check guard pages. If that fails, unmap everything.
            if let Err(e) = ensure_guard_pages(process, mapped_address, mapped_size) {
                let _ = unmap_process_code_memory(process, mapped_address, regions);
                return Err(e);
            }

            Ok(())
        })();

        if map_result.is_ok() {
            return Ok(mapped_address);
        }
    }

    // We failed to map anything.
    Err(ro_results::RESULT_OUT_OF_ADDRESS_SPACE)
}

/// Map an NRO into a process's address space.
///
/// Corresponds to `MapNro` in upstream ro_nro_utils.cpp.
pub fn map_nro(
    process: &mut KProcess,
    nro_heap_address: u64,
    nro_heap_size: u64,
    bss_heap_address: u64,
    bss_heap_size: u64,
    generate_random: &mut impl FnMut() -> u64,
) -> Result<u64, ResultCode> {
    // Set up the process memory regions.
    let regions = setup_nro_process_memory_regions(
        nro_heap_address,
        nro_heap_size,
        bss_heap_address,
        bss_heap_size,
    );

    // Re-map the nro/bss as code memory in the destination process.
    map_process_code_memory(process, &regions, generate_random)
}

/// Set NRO memory permissions (rx, ro, rw sections).
///
/// Corresponds to `SetNroPerms` in upstream ro_nro_utils.cpp.
pub fn set_nro_perms(
    process: &mut KProcess,
    base_address: u64,
    rx_size: u64,
    ro_size: u64,
    rw_size: u64,
) -> Result<(), ResultCode> {
    let rx_offset: u64 = 0;
    let ro_offset: u64 = rx_offset + rx_size;
    let rw_offset: u64 = ro_offset + ro_size;

    set_process_memory_permission(
        process,
        base_address + rx_offset,
        rx_size,
        MemoryPermission::ReadExecute,
    )?;
    set_process_memory_permission(
        process,
        base_address + ro_offset,
        ro_size,
        MemoryPermission::Read,
    )?;
    set_process_memory_permission(
        process,
        base_address + rw_offset,
        rw_size,
        MemoryPermission::ReadWrite,
    )?;

    Ok(())
}

/// Unmap an NRO from a process's address space.
///
/// Corresponds to `UnmapNro` in upstream ro_nro_utils.cpp.
pub fn unmap_nro(
    process: &mut KProcess,
    base_address: u64,
    nro_heap_address: u64,
    nro_heap_size: u64,
    bss_heap_address: u64,
    bss_heap_size: u64,
) -> Result<(), ResultCode> {
    // Set up the process memory regions.
    let regions = setup_nro_process_memory_regions(
        nro_heap_address,
        nro_heap_size,
        bss_heap_address,
        bss_heap_size,
    );

    // Unmap the nro/bss.
    unmap_process_code_memory(process, base_address, &regions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::kernel::k_memory_block::{
        KMemoryAttribute, KMemoryBlockDisableMergeAttribute, KMemoryPermission, KMemoryState,
    };
    use crate::hle::kernel::kernel::ScopedKernelForTest;

    #[test]
    fn writable_module_region_becomes_alias_code_data() {
        let mut kernel = ScopedKernelForTest::new();
        kernel.kernel_mut().initialize_memory_block_slab_manager(4096);
        kernel.kernel_mut().initialize_block_info_manager(4096);
        let mut process = KProcess::new();
        let address = 0x1000_0000;
        process.page_table.configure_address_space(KProcessAddress::new(address), 0x4000, 32);
        process.page_table.get_base_mut().get_memory_block_manager_mut().update(
            address as usize, 1, KMemoryState::ALIAS_CODE,
            KMemoryPermission::USER_READ, KMemoryAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
            KMemoryBlockDisableMergeAttribute::NONE,
        );
        set_process_memory_permission(&mut process, address, PAGE_SIZE as u64,
            MemoryPermission::ReadWrite).unwrap();
        let info = process.page_table.get_base_mut().get_memory_block_manager()
            .query_info(address as usize).unwrap();
        assert_eq!(info.m_state, KMemoryState::ALIAS_CODE_DATA);
        assert_eq!(info.m_permission, KMemoryPermission::USER_READ_WRITE);
    }
}
