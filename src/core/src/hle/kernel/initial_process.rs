//! Port of zuyu/src/core/hle/kernel/initial_process.h
//! Status: COMPLET
//! Derniere synchro: 2026-03-11
//!
//! Constants and helpers for the initial process binary loaded at boot.
//! Note: initial_process.cpp does not exist upstream; this is header-only.

use super::board::k_memory_layout::MAIN_MEMORY_ADDRESS;
use super::board::k_system_control;

/// Maximum size of the initial process binary (12 MiB).
pub const INITIAL_PROCESS_BINARY_SIZE_MAX: usize = 12 * 1024 * 1024;

/// Get the physical address where the initial process binary is loaded.
pub fn get_initial_process_binary_physical_address(mode: common::settings::MemoryLayout) -> u64 {
    k_system_control::init::get_kernel_physical_base_address(MAIN_MEMORY_ADDRESS, mode)
}

/// Get the size of the initial process binary region.
pub fn get_initial_process_binary_size() -> usize {
    INITIAL_PROCESS_BINARY_SIZE_MAX
}
