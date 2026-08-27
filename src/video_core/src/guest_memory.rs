// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/guest_memory.h
//!
//! Type aliases for guest memory access patterns used by the GPU.

use std::sync::Arc;

use parking_lot::Mutex;
use ruzu_core::guest_memory::{GuestMemory, GuestMemoryInterface, GuestMemoryScoped};

use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
use crate::memory_manager::MemoryManager;

pub use ruzu_core::guest_memory::GuestMemoryFlags;

pub struct DeviceMemoryManagerHandle {
    memory_manager: Arc<MaxwellDeviceMemoryManager>,
}

impl DeviceMemoryManagerHandle {
    pub fn new(memory_manager: Arc<MaxwellDeviceMemoryManager>) -> Self {
        Self { memory_manager }
    }
}

impl GuestMemoryInterface for DeviceMemoryManagerHandle {
    const HAS_FLUSH_INVALIDATION: bool = true;

    fn get_span(&self, addr: u64, size: usize) -> Option<*mut u8> {
        let ptr = self.memory_manager.get_span(addr, size);
        (!ptr.is_null()).then_some(ptr)
    }

    fn read_block(&self, addr: u64, dest: *mut u8, size: usize) {
        let output = unsafe { std::slice::from_raw_parts_mut(dest, size) };
        self.memory_manager.smmu_read_block(addr, output);
    }

    fn read_block_unsafe(&self, addr: u64, dest: *mut u8, size: usize) {
        let output = unsafe { std::slice::from_raw_parts_mut(dest, size) };
        self.memory_manager.smmu_read_block_unsafe(addr, output);
    }

    fn write_block(&self, addr: u64, src: *const u8, size: usize) {
        let input = unsafe { std::slice::from_raw_parts(src, size) };
        self.memory_manager.smmu_write_block(addr, input);
    }

    fn write_block_unsafe(&self, addr: u64, src: *const u8, size: usize) {
        let input = unsafe { std::slice::from_raw_parts(src, size) };
        self.memory_manager.smmu_write_block_unsafe(addr, input);
    }

    fn write_block_cached(&self, addr: u64, src: *const u8, size: usize) {
        let input = unsafe { std::slice::from_raw_parts(src, size) };
        self.memory_manager.smmu_write_block_unsafe(addr, input);
    }

    fn flush_region(&self, addr: u64, size: usize) {
        self.memory_manager.smmu_flush_region(addr, size);
    }

    fn invalidate_region(&self, addr: u64, size: usize) {
        self.memory_manager.smmu_invalidate_region(addr, size);
    }
}

/// Rust synchronization adapter for upstream's direct `Tegra::MemoryManager&`.
/// Calls take the existing channel memory-manager mutex only for the duration
/// of the corresponding upstream memory operation; a direct span therefore
/// does not keep that mutex locked while the caller consumes it.
pub struct GpuMemoryManagerHandle {
    memory_manager: Arc<Mutex<MemoryManager>>,
}

impl GpuMemoryManagerHandle {
    pub fn new(memory_manager: Arc<Mutex<MemoryManager>>) -> Self {
        Self { memory_manager }
    }
}

impl GuestMemoryInterface for GpuMemoryManagerHandle {
    const HAS_FLUSH_INVALIDATION: bool = MemoryManager::HAS_FLUSH_INVALIDATION;

    fn get_span(&self, addr: u64, size: usize) -> Option<*mut u8> {
        let ptr = self.memory_manager.lock().get_span(addr, size);
        (!ptr.is_null()).then_some(ptr)
    }

    fn read_block(&self, addr: u64, dest: *mut u8, size: usize) {
        let output = unsafe { std::slice::from_raw_parts_mut(dest, size) };
        self.memory_manager.lock().read_block(addr, output);
    }

    fn read_block_unsafe(&self, addr: u64, dest: *mut u8, size: usize) {
        let output = unsafe { std::slice::from_raw_parts_mut(dest, size) };
        self.memory_manager.lock().read_block_unsafe(addr, output);
    }

    fn write_block(&self, addr: u64, src: *const u8, size: usize) {
        let input = unsafe { std::slice::from_raw_parts(src, size) };
        self.memory_manager.lock().write_block(addr, input);
    }

    fn write_block_unsafe(&self, addr: u64, src: *const u8, size: usize) {
        let input = unsafe { std::slice::from_raw_parts(src, size) };
        self.memory_manager.lock().write_block_unsafe(addr, input);
    }

    fn write_block_cached(&self, addr: u64, src: *const u8, size: usize) {
        let input = unsafe { std::slice::from_raw_parts(src, size) };
        self.memory_manager.lock().write_block_cached(addr, input);
    }

    fn flush_region(&self, addr: u64, size: usize) {
        self.memory_manager.lock().flush_region(addr, size as u64);
    }

    fn invalidate_region(&self, addr: u64, size: usize) {
        self.memory_manager
            .lock()
            .invalidate_region(addr, size as u64);
    }
}

pub type DeviceGuestMemory<'memory, 'backup, T> =
    GuestMemory<'memory, 'backup, DeviceMemoryManagerHandle, T>;
pub type DeviceGuestMemoryScoped<'memory, 'backup, T> =
    GuestMemoryScoped<'memory, 'backup, DeviceMemoryManagerHandle, T>;
pub type GpuGuestMemory<'memory, 'backup, T> =
    GuestMemory<'memory, 'backup, GpuMemoryManagerHandle, T>;
pub type GpuGuestMemoryScoped<'memory, 'backup, T> =
    GuestMemoryScoped<'memory, 'backup, GpuMemoryManagerHandle, T>;
