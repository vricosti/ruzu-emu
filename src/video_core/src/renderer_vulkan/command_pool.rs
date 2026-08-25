// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `vk_command_pool.h` / `vk_command_pool.cpp`.
//!
//! Command pool that extends `ResourcePool` to manage a growable set of
//! `VkCommandPool` / `VkCommandBuffer` pairs.

use std::sync::Arc;

use ash::vk;

use super::master_semaphore::MasterSemaphore;
use super::resource_pool::ResourcePool;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Number of command buffers per pool.
///
/// Port of `COMMAND_BUFFER_POOL_SIZE` from `vk_command_pool.cpp`.
const COMMAND_BUFFER_POOL_SIZE: usize = 4;

// ---------------------------------------------------------------------------
// CommandPool
// ---------------------------------------------------------------------------

/// Internal pool entry.
///
/// Port of `CommandPool::Pool` from `vk_command_pool.cpp`.
#[derive(Default)]
struct Pool {
    handle: vk::CommandPool,
    cmdbufs: Vec<vk::CommandBuffer>,
}

/// Port of `CommandPool` class.
///
/// Manages multiple `VkCommandPool` objects, each containing a batch of
/// pre-allocated command buffers. Extends `ResourcePool` for tick-based reuse.
pub struct CommandPool {
    device: ash::Device,
    graphics_family: u32,
    resource_pool: ResourcePool,
    pools: Vec<Pool>,
}

impl CommandPool {
    /// Port of `CommandPool::CommandPool`.
    pub fn new(
        master_semaphore: Arc<MasterSemaphore>,
        device: ash::Device,
        graphics_family: u32,
    ) -> Self {
        CommandPool {
            device,
            graphics_family,
            resource_pool: ResourcePool::new(master_semaphore, COMMAND_BUFFER_POOL_SIZE),
            pools: Vec::new(),
        }
    }

    /// Port of `CommandPool::Commit`.
    ///
    /// Returns the next available command buffer.
    pub fn commit(&mut self) -> Result<vk::CommandBuffer, vk::Result> {
        let device = self.device.clone();
        let graphics_family = self.graphics_family;
        let pools = &mut self.pools;

        let mut allocate = |_begin, _end| {
            // Command buffers are going to be committed, recorded, executed every
            // single usage cycle. They are also going to be reset when committed.
            pools.push(Pool::default());
            let pool = pools.last_mut().expect("new command pool entry is missing");
            let pool_ci = vk::CommandPoolCreateInfo::builder()
                .flags(
                    vk::CommandPoolCreateFlags::TRANSIENT
                        | vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER,
                )
                .queue_family_index(graphics_family)
                .build();

            pool.handle = unsafe { device.create_command_pool(&pool_ci, None)? };

            let alloc_info = vk::CommandBufferAllocateInfo::builder()
                .command_pool(pool.handle)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(COMMAND_BUFFER_POOL_SIZE as u32)
                .build();

            pool.cmdbufs = unsafe { device.allocate_command_buffers(&alloc_info)? };
            Ok(())
        };
        let index = self.resource_pool.try_commit_resource(&mut allocate)?;

        let pool_index = index / COMMAND_BUFFER_POOL_SIZE;
        let sub_index = index % COMMAND_BUFFER_POOL_SIZE;
        Ok(self.pools[pool_index].cmdbufs[sub_index])
    }
}

impl Drop for CommandPool {
    fn drop(&mut self) {
        for pool in &self.pools {
            if pool.handle != vk::CommandPool::null() {
                unsafe {
                    self.device.destroy_command_pool(pool.handle, None);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_buffer_pool_size() {
        assert_eq!(COMMAND_BUFFER_POOL_SIZE, 4);
    }

    #[test]
    fn pool_entry_is_empty_before_vulkan_allocation() {
        let pool = Pool::default();
        assert_eq!(pool.handle, vk::CommandPool::null());
        assert!(pool.cmdbufs.is_empty());
    }
}
