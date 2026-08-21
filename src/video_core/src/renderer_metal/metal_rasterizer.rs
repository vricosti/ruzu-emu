// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native Metal rasterizer ownership.
//!
//! This file is the Metal counterpart of Eden's
//! `renderer_vulkan/vk_rasterizer.{h,cpp}`. It owns one scheduler, one staging
//! pool, and the common buffer/texture/shader caches used by every channel.

use std::sync::Arc;

use thiserror::Error;

use crate::buffer_cache::buffer_cache_base::{DeviceMemoryAccess, GpuMemoryAccess};
use crate::control::channel_state::ChannelState;
use crate::control::channel_state_cache::{ChannelCacheAccessor, ChannelInfo, ChannelSetupCaches};
use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
use crate::memory_manager::MemoryManager;
use crate::shader_cache::ShaderCache;

use super::metal_buffer_cache::{BufferCacheRuntime, MetalCommonBufferCache};
use super::metal_device::MetalDevice;
use super::metal_pipeline_cache::MetalPipelineCache;
use super::metal_scheduler::{MetalScheduler, MetalSchedulerError};
use super::metal_staging_buffer_pool::{MetalStagingBufferError, MetalStagingBufferPool};
use super::metal_texture_cache::MetalTextureCache;

macro_rules! lock_two_reentrant_mutexes {
    ($first:expr, $second:expr, $first_guard:ident, $second_guard:ident) => {
        let first_address = $first as usize;
        let second_address = $second as usize;
        let ($first_guard, $second_guard) = if first_address <= second_address {
            (unsafe { (*$first).lock() }, unsafe { (*$second).lock() })
        } else {
            let second_guard = unsafe { (*$second).lock() };
            let first_guard = unsafe { (*$first).lock() };
            (first_guard, second_guard)
        };
    };
}

#[derive(Debug, Error)]
pub enum MetalRasterizerError {
    #[error(transparent)]
    Staging(#[from] MetalStagingBufferError),
    #[error(transparent)]
    Scheduler(#[from] MetalSchedulerError),
}

struct GpuMemoryAccessAdapter {
    memory_manager: Arc<parking_lot::Mutex<MemoryManager>>,
}

impl GpuMemoryAccess for GpuMemoryAccessAdapter {
    fn gpu_to_cpu_address(&self, gpu_addr: u64) -> Option<u64> {
        self.memory_manager.lock().gpu_to_cpu_address(gpu_addr)
    }

    fn read_u64(&self, gpu_addr: u64) -> Option<u64> {
        let mut bytes = [0; 8];
        self.memory_manager.lock().read_block(gpu_addr, &mut bytes);
        Some(u64::from_le_bytes(bytes))
    }

    fn read_u32(&self, gpu_addr: u64) -> Option<u32> {
        let mut bytes = [0; 4];
        self.memory_manager.lock().read_block(gpu_addr, &mut bytes);
        Some(u32::from_le_bytes(bytes))
    }

    fn is_within_gpu_address_range(&self, gpu_addr: u64) -> bool {
        self.memory_manager
            .lock()
            .is_within_gpu_address_range(gpu_addr)
    }

    fn max_continuous_range(&self, gpu_addr: u64, size: u64) -> u64 {
        self.memory_manager
            .lock()
            .max_continuous_range(gpu_addr, size)
    }

    fn get_memory_layout_size(&self, gpu_addr: u64) -> u64 {
        self.memory_manager.lock().get_memory_layout_size(gpu_addr)
    }
}

struct DeviceMemoryAccessAdapter {
    device_memory: Arc<MaxwellDeviceMemoryManager>,
}

impl DeviceMemoryAccess for DeviceMemoryAccessAdapter {
    fn get_pointer(&self, device_addr: u64) -> Option<*const u8> {
        let pointer = self.device_memory.get_pointer(device_addr);
        (!pointer.is_null()).then_some(pointer)
    }

    fn read_block_unsafe(&self, device_addr: u64, dst: &mut [u8]) {
        self.device_memory.smmu_read_block_unsafe(device_addr, dst);
    }

    fn write_block_unsafe(&self, device_addr: u64, src: &[u8]) {
        self.device_memory.smmu_write_block_unsafe(device_addr, src);
    }
}

/// Backend owner corresponding to Eden's `RasterizerVulkan` construction and
/// channel-cache lifecycle.
pub struct MetalRasterizer {
    device: MetalDevice,
    scheduler: Box<MetalScheduler>,
    staging_pool: Box<MetalStagingBufferPool>,
    pipeline_cache: MetalPipelineCache,
    shader_cache: ShaderCache,
    common_buffer_cache: Box<MetalCommonBufferCache>,
    texture_cache: Box<MetalTextureCache>,
    channel_caches: ChannelSetupCaches<ChannelInfo>,
    channel_memory_manager: Option<Arc<parking_lot::Mutex<MemoryManager>>>,
}

impl MetalRasterizer {
    pub fn new(
        device: MetalDevice,
        device_memory: Arc<MaxwellDeviceMemoryManager>,
    ) -> Result<Self, MetalRasterizerError> {
        let mut scheduler = Box::new(MetalScheduler::new(&device));
        let mut staging_pool = Box::new(MetalStagingBufferPool::new(&device)?);

        let buffer_runtime =
            BufferCacheRuntime::new(&device, scheduler.as_mut(), staging_pool.as_mut());
        let mut common_buffer_cache = Box::new(MetalCommonBufferCache::new(
            device_memory.as_ref(),
            buffer_runtime,
        ));
        common_buffer_cache.set_device_memory(Box::new(DeviceMemoryAccessAdapter {
            device_memory: Arc::clone(&device_memory),
        }));

        let texture_cache = Box::new(MetalTextureCache::new(
            device.clone(),
            Arc::clone(&device_memory),
            scheduler.as_mut(),
            staging_pool.as_mut(),
        ));
        let shader_cache = ShaderCache::new(device_memory);
        let pipeline_cache = MetalPipelineCache::new(device.clone());

        Ok(Self {
            device,
            scheduler,
            staging_pool,
            pipeline_cache,
            shader_cache,
            common_buffer_cache,
            texture_cache,
            channel_caches: ChannelSetupCaches::new(),
            channel_memory_manager: None,
        })
    }

    pub fn device(&self) -> &MetalDevice {
        &self.device
    }

    pub fn scheduler(&mut self) -> &mut MetalScheduler {
        self.scheduler.as_mut()
    }

    pub fn pipeline_cache(&mut self) -> &mut MetalPipelineCache {
        &mut self.pipeline_cache
    }

    pub fn shader_cache(&mut self) -> &mut ShaderCache {
        &mut self.shader_cache
    }

    pub fn common_buffer_cache(&mut self) -> &mut MetalCommonBufferCache {
        self.common_buffer_cache.as_mut()
    }

    pub fn texture_cache(&mut self) -> &mut MetalTextureCache {
        self.texture_cache.as_mut()
    }

    /// Port of Eden `RasterizerVulkan::InitializeChannel` for the caches
    /// currently owned by the Metal backend.
    pub fn initialize_channel(&mut self, channel: &mut ChannelState) {
        self.channel_caches.create_channel(channel);
        let buffer_mutex: *const _ = &self.common_buffer_cache.mutex;
        let texture_mutex: *const _ = &self.texture_cache.base.mutex;
        lock_two_reentrant_mutexes!(buffer_mutex, texture_mutex, _buffer_guard, _texture_guard);
        self.texture_cache.create_channel(channel);
        self.common_buffer_cache.create_channel(channel);
        self.shader_cache.create_channel(channel);
    }

    /// Port of Eden `RasterizerVulkan::BindChannel` for the caches currently
    /// owned by the Metal backend.
    pub fn bind_channel(&mut self, channel: &mut ChannelState) {
        self.channel_caches.bind_to_channel(channel.bind_id);
        let buffer_mutex: *const _ = &self.common_buffer_cache.mutex;
        let texture_mutex: *const _ = &self.texture_cache.base.mutex;
        lock_two_reentrant_mutexes!(buffer_mutex, texture_mutex, _buffer_guard, _texture_guard);
        self.texture_cache.bind_to_channel(channel.bind_id);
        self.common_buffer_cache.bind_to_channel(channel.bind_id);
        self.shader_cache.bind_to_channel(channel.bind_id);
        self.channel_memory_manager = self
            .channel_caches
            .current_channel_state()
            .and_then(ChannelCacheAccessor::gpu_memory_arc);
        if let Some(memory_manager) = self.channel_memory_manager.as_ref() {
            self.common_buffer_cache
                .set_gpu_memory(Box::new(GpuMemoryAccessAdapter {
                    memory_manager: Arc::clone(memory_manager),
                }));
        } else {
            self.common_buffer_cache.clear_gpu_memory();
        }
    }

    /// Port of Eden `RasterizerVulkan::ReleaseChannel` for the caches
    /// currently owned by the Metal backend.
    pub fn release_channel(&mut self, channel_id: i32) {
        self.channel_caches.erase_channel(channel_id);
        let buffer_mutex: *const _ = &self.common_buffer_cache.mutex;
        let texture_mutex: *const _ = &self.texture_cache.base.mutex;
        lock_two_reentrant_mutexes!(buffer_mutex, texture_mutex, _buffer_guard, _texture_guard);
        self.texture_cache.erase_channel(channel_id);
        self.common_buffer_cache.erase_channel(channel_id);
        self.shader_cache.erase_channel(channel_id);
        self.channel_memory_manager = self
            .channel_caches
            .current_channel_state()
            .and_then(ChannelCacheAccessor::gpu_memory_arc);
        if let Some(memory_manager) = self.channel_memory_manager.as_ref() {
            self.common_buffer_cache
                .set_gpu_memory(Box::new(GpuMemoryAccessAdapter {
                    memory_manager: Arc::clone(memory_manager),
                }));
        } else {
            self.common_buffer_cache.clear_gpu_memory();
        }
    }

    pub fn tick_frame(&mut self) {
        self.texture_cache.tick_frame();
        self.common_buffer_cache.tick_frame();
    }

    pub fn finish(&mut self) -> Result<(), MetalRasterizerError> {
        self.scheduler.finish_all()?;
        Ok(())
    }
}

impl Drop for MetalRasterizer {
    fn drop(&mut self) {
        if let Err(error) = self.scheduler.finish_all() {
            log::error!("Metal rasterizer shutdown failed: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owns_one_scheduler_and_shared_cache_runtime() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let device_memory = Arc::new(MaxwellDeviceMemoryManager::default());
        let mut rasterizer = MetalRasterizer::new(device, device_memory).unwrap();

        let initial_tick = rasterizer.scheduler().current_tick();
        rasterizer.tick_frame();
        rasterizer.finish().unwrap();
        assert_eq!(rasterizer.scheduler().current_tick(), initial_tick);
    }
}
