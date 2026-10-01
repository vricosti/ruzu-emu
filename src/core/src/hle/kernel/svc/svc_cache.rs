//! Port of zuyu/src/core/hle/kernel/svc/svc_cache.cpp
//! Status: COMPLET
//! Derniere synchro: 2026-03-20
//!
//! SVC handlers for data cache operations.

use crate::core::System;
use crate::hle::kernel::k_typed_address::KProcessAddress;
use crate::hle::kernel::svc::svc_results::*;
use crate::hle::kernel::svc_common::{Handle, PseudoHandle};
use crate::hle::result::{ResultCode, RESULT_SUCCESS};

/// Flushes the entire data cache.
/// Upstream: UNIMPLEMENTED() — intentionally unimplemented.
pub fn flush_entire_data_cache() {
    log::warn!("svc::FlushEntireDataCache: Upstream UNIMPLEMENTED");
}

/// Flushes a range of the data cache.
/// Upstream: UNIMPLEMENTED() — intentionally unimplemented.
pub fn flush_data_cache(_address: u64, _size: u64) -> ResultCode {
    log::warn!("svc::FlushDataCache: Upstream UNIMPLEMENTED");
    RESULT_NOT_IMPLEMENTED
}

/// Invalidates process data cache.
/// Upstream: UNIMPLEMENTED() — intentionally unimplemented.
pub fn invalidate_process_data_cache(
    _process_handle: Handle,
    _address: u64,
    _size: u64,
) -> ResultCode {
    log::warn!("svc::InvalidateProcessDataCache: Upstream UNIMPLEMENTED");
    RESULT_NOT_IMPLEMENTED
}

/// Stores process data cache.
/// Upstream: UNIMPLEMENTED() — intentionally unimplemented.
pub fn store_process_data_cache(_process_handle: Handle, _address: u64, _size: u64) -> ResultCode {
    log::warn!("svc::StoreProcessDataCache: Upstream UNIMPLEMENTED");
    RESULT_NOT_IMPLEMENTED
}

/// Flushes process data cache.
///
/// Validates address/size, gets the process from its handle, verifies the region
/// is within range, then performs the flush.
pub fn flush_process_data_cache(
    system: &System,
    process_handle: Handle,
    address: u64,
    size: u64,
) -> ResultCode {
    // Validate address/size.
    if size == 0 {
        return RESULT_INVALID_SIZE;
    }
    // Upstream checks address == static_cast<uint64_t>(address) and same for size,
    // which is always true for u64 inputs. Included for parity:
    // R_UNLESS(address == static_cast<uint64_t>(address), ResultInvalidCurrentMemory);
    // R_UNLESS(size == static_cast<uint64_t>(size), ResultInvalidCurrentMemory);

    // Get the process from its handle.
    // Upstream: GetCurrentProcess(kernel).GetHandleTable().GetObject<KProcess>(process_handle).
    // `GetObject<KProcess>` accepts CurrentProcess as a typed pseudo-handle.
    let process_arc = system.current_process_arc();
    let process = process_arc.lock().unwrap();
    if process_handle != PseudoHandle::CurrentProcess as Handle
        && process.handle_table.get_object(process_handle).is_none()
    {
        return RESULT_INVALID_HANDLE;
    }

    // Verify the region is within range.
    // Upstream: page_table.Contains(address, size)
    if !process
        .page_table
        .contains(KProcessAddress::new(address), size as usize)
    {
        return RESULT_INVALID_CURRENT_MEMORY;
    }

    // Perform the operation.
    // Upstream: GetCurrentMemory(kernel).FlushDataCache(address, size)
    let Some(memory) = process.get_memory() else {
        return RESULT_INVALID_CURRENT_MEMORY;
    };
    drop(process);

    // Upstream `PerformCacheOperation` holds no memory-wide lock while it
    // demo) issue thousands of small flushes per frame; holding the `Memory`
    // mutex across `on_cpu_write` serializes every guest-memory access in the
    // emulator behind rasterizer-lock contention. Collect the rasterizer
    // ranges under the lock, then notify with the lock released.
    let batch = memory
        .access()
        .unwrap()
        .collect_rasterizer_write_ranges(address, size as usize);
    batch.apply();
    RESULT_SUCCESS
}
