// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `vk_descriptor_buffer.{h,cpp}`.

use ash::vk;
use common::alignment::{align_down, align_up};

use super::scheduler::Scheduler;
use crate::vulkan_common::vulkan_device::{Device, DeviceReference};
use crate::vulkan_common::vulkan_memory_allocator::{
    AllocatedBuffer, MemoryAllocator, MemoryUsage,
};
use crate::vulkan_common::vulkan_wrapper::VulkanError;

const FRAMES_IN_FLIGHT: usize = 8;
const TILER_FRAME_SIZE: vk::DeviceSize = 2 * 1024 * 1024;
const DESKTOP_FRAME_SIZE: vk::DeviceSize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default)]
pub struct Allocation {
    pub host: *mut u8,
    pub offset: vk::DeviceSize,
    pub chunk: u32,
    pub generation: u64,
}

pub struct DescriptorBufferRing {
    // Stored to mirror Eden's `const Device&` member and parent-owned lifetime.
    #[allow(dead_code)]
    device: DeviceReference,
    chunks: Vec<AllocatedBuffer>,
    chunk_addresses: Vec<vk::DeviceAddress>,
    chunk_hosts: Vec<*mut u8>,
    alignment: vk::DeviceSize,
    chunk_capacity: vk::DeviceSize,
    chunks_per_frame: usize,
    frame_index: usize,
    chunk_cursor: usize,
    cursor: vk::DeviceSize,
    generation: u64,
    frame_ticks: [u64; FRAMES_IN_FLIGHT],
    frame_reused: bool,
}

// The ring is owned and mutated by the GPU thread. Its mapped allocation
// pointers remain valid for the lifetime of their owning `AllocatedBuffer`s.
unsafe impl Send for DescriptorBufferRing {}

impl DescriptorBufferRing {
    pub fn new(device: &Device, memory_allocator: &MemoryAllocator) -> Result<Self, VulkanError> {
        let mut ring = Self {
            device: DeviceReference::new(device),
            chunks: Vec::new(),
            chunk_addresses: Vec::new(),
            chunk_hosts: Vec::new(),
            alignment: 1,
            chunk_capacity: 0,
            chunks_per_frame: 0,
            frame_index: 0,
            chunk_cursor: 0,
            cursor: 0,
            generation: 1,
            frame_ticks: [0; FRAMES_IN_FLIGHT],
            frame_reused: false,
        };
        if !device.is_ext_descriptor_buffer_supported()
            || !device.is_buffer_device_address_supported()
        {
            return Ok(ring);
        }

        let props = device.descriptor_buffer_properties();
        ring.alignment = props.descriptor_buffer_offset_alignment.max(1);
        let max_bound = [
            props.max_sampler_descriptor_buffer_range,
            props.max_resource_descriptor_buffer_range,
            props.sampler_descriptor_buffer_address_space_size,
            props.resource_descriptor_buffer_address_space_size,
            props.descriptor_buffer_address_space_size,
        ]
        .into_iter()
        .min()
        .unwrap();
        let frame_size = if device.is_tiler() {
            TILER_FRAME_SIZE
        } else {
            DESKTOP_FRAME_SIZE
        };
        let chunk_size = align_down(frame_size.min(max_bound), ring.alignment);
        if chunk_size <= ring.alignment {
            log::debug!(
                "Descriptor buffer binding limit of {} is unusable, disabling",
                max_bound
            );
            return Ok(ring);
        }
        ring.chunk_capacity = chunk_size - ring.alignment;
        ring.chunks_per_frame = (frame_size / chunk_size) as usize;

        let buffer_info = vk::BufferCreateInfo::builder()
            .size(chunk_size)
            .usage(
                vk::BufferUsageFlags::RESOURCE_DESCRIPTOR_BUFFER_EXT
                    | vk::BufferUsageFlags::SAMPLER_DESCRIPTOR_BUFFER_EXT
                    | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .build();
        let total_chunks = ring.chunks_per_frame * FRAMES_IN_FLIGHT;
        ring.chunks.reserve(total_chunks);
        ring.chunk_addresses.reserve(total_chunks);
        ring.chunk_hosts.reserve(total_chunks);
        for _ in 0..total_chunks {
            let buffer = memory_allocator.create_buffer(&buffer_info, MemoryUsage::Upload)?;
            if !buffer.is_host_visible() {
                log::debug!("Descriptor buffer is not host visible, disabling");
                ring.chunks.clear();
                return Ok(ring);
            }
            if !buffer.is_host_coherent() {
                log::debug!("Descriptor buffer is not host coherent, disabling");
                ring.chunks.clear();
                return Ok(ring);
            }
            device.set_buffer_name(buffer.buffer(), "Descriptor buffer");
            let raw_address = unsafe {
                device.get_logical().get_buffer_device_address(
                    &vk::BufferDeviceAddressInfo::builder()
                        .buffer(buffer.buffer())
                        .build(),
                )
            };
            let address = align_up(raw_address, ring.alignment);
            ring.chunk_addresses.push(address);
            ring.chunk_hosts.push(unsafe {
                buffer
                    .mapped_ptr()
                    .add(address.wrapping_sub(raw_address) as usize)
            });
            ring.chunks.push(buffer);
        }
        Ok(ring)
    }

    pub fn current_generation(&self) -> u64 {
        self.generation
    }

    pub fn touch_frame(&mut self, scheduler: &Scheduler) {
        self.frame_ticks[self.frame_index] = scheduler.current_tick();
    }

    pub fn can_allocate(&self, size: vk::DeviceSize) -> bool {
        align_up(size, self.alignment) <= self.chunk_capacity
    }

    pub fn tick_frame(&mut self) {
        self.frame_index = self.frame_index.wrapping_add(1);
        if self.frame_index >= FRAMES_IN_FLIGHT {
            self.frame_index = 0;
        }
        self.chunk_cursor = 0;
        self.cursor = 0;
        self.generation = self.generation.wrapping_add(1);
        self.frame_reused = true;
    }

    pub fn allocate(&mut self, scheduler: &mut Scheduler, size: vk::DeviceSize) -> Allocation {
        assert!(!self.chunks.is_empty());
        if !self.can_allocate(size) {
            log::debug!(
                "Descriptor set of {} bytes exceeds chunk capacity {}",
                size,
                self.chunk_capacity
            );
            return Allocation::default();
        }
        let needed = align_up(size, self.alignment);
        if self.frame_reused {
            self.frame_reused = false;
            scheduler.wait(self.frame_ticks[self.frame_index]);
        }
        if self.cursor.wrapping_add(needed) > self.chunk_capacity {
            if self.chunk_cursor.wrapping_add(1) < self.chunks_per_frame {
                self.chunk_cursor = self.chunk_cursor.wrapping_add(1);
            } else {
                log::debug!("Descriptor buffer frame exhausted, stalling on the GPU");
                scheduler.finish();
                self.chunk_cursor = 0;
                self.generation = self.generation.wrapping_add(1);
            }
            self.cursor = 0;
        }
        let chunk = self
            .frame_index
            .wrapping_mul(self.chunks_per_frame)
            .wrapping_add(self.chunk_cursor);
        let offset = self.cursor;
        self.cursor = self.cursor.wrapping_add(needed);
        self.frame_ticks[self.frame_index] = scheduler.current_tick();
        Allocation {
            host: unsafe { self.chunk_hosts[chunk].add(offset as usize) },
            offset,
            chunk: chunk as u32,
            generation: self.generation,
        }
    }

    pub fn binding_info(&self, chunk: u32) -> vk::DescriptorBufferBindingInfoEXT {
        vk::DescriptorBufferBindingInfoEXT::builder()
            .address(self.chunk_addresses[chunk as usize])
            .usage(
                vk::BufferUsageFlags::RESOURCE_DESCRIPTOR_BUFFER_EXT
                    | vk::BufferUsageFlags::SAMPLER_DESCRIPTOR_BUFFER_EXT,
            )
            .build()
    }

    pub fn is_valid(&self) -> bool {
        !self.chunks.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_ring_alignment_matches_upstream() {
        assert_eq!(align_up(1, 32), 32);
        assert_eq!(align_up(32, 32), 32);
        assert_eq!(align_down(63, 32), 32);
        assert_eq!(align_up(u64::MAX, 2), 0);
    }
}
