// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later
//! Port of Eden's `video_core/renderer_vulkan/vk_multi_range_buffer.h` and
//! `vk_multi_range_buffer.cpp` (Eden a538cd9aff).
//! Status: COMPLET
//! Derniere synchro: 2026-09-11
//!
//! Presents several host-contiguous buffer segments to a shader as one
//! storage buffer: either by aliasing their memory into a sparse `VkBuffer`
//! (`VK_BUFFER_CREATE_SPARSE_BINDING_BIT | SPARSE_ALIASED_BIT`, bound with
//! `vkQueueBindSparse`) or, when the sources cannot be aliased, by gathering
//! them with copies into a plain buffer.

use std::collections::HashMap;

use ash::vk;
use ash::vk::Handle;

use crate::vulkan_common::vulkan_device::{Device, DeviceReference};
use crate::vulkan_common::vulkan_memory_allocator::{AllocatedBuffer, MemoryAllocator, MemoryUsage};

use super::scheduler::Scheduler;

/// Owning `VkBuffer` handle created outside the allocator (sparse buffers
/// have no backing allocation of their own).
///
/// Upstream `using SparseBuffer = vk::Handle<VkBuffer, VkDevice, vk::DeviceDispatch>`.
pub struct SparseBuffer {
    handle: vk::Buffer,
    device: Option<DeviceReference>,
}

impl SparseBuffer {
    fn new(handle: vk::Buffer, device: &Device) -> Self {
        Self {
            handle,
            device: Some(DeviceReference::new(device)),
        }
    }

    pub fn handle(&self) -> vk::Buffer {
        self.handle
    }
}

impl Drop for SparseBuffer {
    fn drop(&mut self) {
        if let Some(device) = self.device.take() {
            if self.handle != vk::Buffer::null() {
                unsafe { device.get().get_logical().destroy_buffer(self.handle, None) };
            }
        }
    }
}

/// Upstream `Vulkan::MultiRangeSource`.
#[derive(Debug, Clone, Copy, Default)]
pub struct MultiRangeSource {
    pub handle: vk::Buffer,
    pub memory: vk::DeviceMemory,
    pub memory_offset: vk::DeviceSize,
    pub offset: vk::DeviceSize,
    pub size: vk::DeviceSize,
    pub write_tick: u64,
    pub memory_type: u32,
}

/// Upstream `Vulkan::MultiRangeRef`.
#[derive(Debug, Clone, Copy, Default)]
pub struct MultiRangeRef {
    pub handle: vk::Buffer,
    pub address: vk::DeviceAddress,
    pub size: vk::DeviceSize,
    pub sparse: bool,
    pub needs_gather: bool,
}

struct Retired {
    #[allow(dead_code)] // Kept alive until the GPU passed `tick` (upstream `Retired::handle`).
    handle: Option<SparseBuffer>,
    #[allow(dead_code)] // Kept alive until the GPU passed `tick` (upstream `Retired::gathered`).
    gathered: Option<AllocatedBuffer>,
    tick: u64,
}

struct Entry {
    gathered: Option<AllocatedBuffer>,
    sparse_handle: Option<SparseBuffer>,
    owners: Vec<vk::Buffer>,
    address: vk::DeviceAddress,
    size: vk::DeviceSize,
    geometry: u64,
    content: u64,
    dirty: bool,
}

impl Entry {
    fn handle(&self) -> vk::Buffer {
        match (&self.sparse_handle, &self.gathered) {
            (Some(sparse), _) => sparse.handle(),
            (None, Some(gathered)) => gathered.handle(),
            (None, None) => vk::Buffer::null(),
        }
    }
}

/// Upstream `Vulkan::MultiRangeBufferCache`.
pub struct MultiRangeBufferCache {
    /// Sparse block size (`VkMemoryRequirements::alignment` of a sparse
    /// probe buffer). Upstream `block_size`.
    pub block_size: vk::DeviceSize,
    /// Whether sparse aliasing is available. Upstream `use_sparse`.
    pub use_sparse: bool,

    entries: HashMap<u64, Entry>,
    /// Upstream `boost::container::static_vector<Retired, MAX_RETIRED>`.
    retired: Vec<Retired>,
    sparse_memory_type_bits: u32,
    sparse_usage: vk::BufferUsageFlags,
}

impl MultiRangeBufferCache {
    pub const DEFAULT_BLOCK_SIZE: vk::DeviceSize = 64 * 1024;
    pub const MAX_RETIRED: usize = 256;

    pub fn new(device: &Device) -> Self {
        let mut sparse_usage = vk::BufferUsageFlags::TRANSFER_SRC
            | vk::BufferUsageFlags::TRANSFER_DST
            | vk::BufferUsageFlags::STORAGE_BUFFER;
        if device.is_buffer_device_address_supported() {
            sparse_usage |= vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS;
        }
        let mut cache = Self {
            block_size: Self::DEFAULT_BLOCK_SIZE,
            use_sparse: false,
            entries: HashMap::new(),
            retired: Vec::with_capacity(Self::MAX_RETIRED),
            sparse_memory_type_bits: 0,
            sparse_usage,
        };
        if !device.is_sparse_binding_supported() {
            return cache;
        }
        let mut memory_type_bits = 0u32;
        let queried = cache.query_block_size(device, &mut memory_type_bits);
        if queried == 0 || memory_type_bits == 0 {
            return cache;
        }
        cache.block_size = queried;
        cache.sparse_memory_type_bits = memory_type_bits;
        cache.use_sparse = true;
        cache
    }

    /// Upstream `QueryBlockSize`: creates a probe sparse buffer to read the
    /// sparse block alignment and the compatible memory types.
    fn query_block_size(&self, device: &Device, memory_type_bits: &mut u32) -> vk::DeviceSize {
        let logical = device.get_logical();
        let probe_ci = vk::BufferCreateInfo::builder()
            .flags(vk::BufferCreateFlags::SPARSE_BINDING | vk::BufferCreateFlags::SPARSE_ALIASED)
            .size(Self::DEFAULT_BLOCK_SIZE)
            .usage(self.sparse_usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .build();
        let Ok(probe) = (unsafe { logical.create_buffer(&probe_ci, None) }) else {
            return 0;
        };
        let owned = SparseBuffer::new(probe, device);
        let reqs_info = vk::BufferMemoryRequirementsInfo2::builder()
            .buffer(owned.handle())
            .build();
        let mut reqs2 = vk::MemoryRequirements2::default();
        unsafe { logical.get_buffer_memory_requirements2(&reqs_info, &mut reqs2) };
        *memory_type_bits = reqs2.memory_requirements.memory_type_bits;
        reqs2.memory_requirements.alignment
    }

    /// Upstream `HashSources` (FNV-1a over handle/offset/size).
    fn hash_sources(sources: &[MultiRangeSource]) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        let mut mix = |value: u64| {
            hash ^= value;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        };
        for source in sources {
            mix(source.handle.as_raw());
            mix(source.offset);
            mix(source.size);
        }
        hash
    }

    /// Upstream `HashContent` (FNV-1a over the write ticks).
    fn hash_content(sources: &[MultiRangeSource]) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for source in sources {
            hash ^= source.write_tick;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }

    /// Upstream `CanBindSparse`: every source must live in a sparse-compatible
    /// memory type and be block aligned in offset and size.
    fn can_bind_sparse(&self, sources: &[MultiRangeSource]) -> bool {
        let block = self.block_size;
        let bits = self.sparse_memory_type_bits;
        self.use_sparse
            && !sources.iter().any(|e| {
                let memory_offset = e.memory_offset + e.offset;
                e.memory == vk::DeviceMemory::null()
                    || e.memory_type >= 32
                    || ((bits >> e.memory_type) & 1) == 0
                    || memory_offset % block != 0
                    || e.size % block != 0
            })
    }

    /// Upstream `CreateSparse`: creates the sparse buffer and binds every
    /// source's memory into it with a synchronous `vkQueueBindSparse`.
    fn create_sparse(
        &self,
        device: &Device,
        scheduler: &mut Scheduler,
        sources: &[MultiRangeSource],
        total: vk::DeviceSize,
    ) -> Option<SparseBuffer> {
        let logical = device.get_logical();
        let buffer_ci = vk::BufferCreateInfo::builder()
            .flags(vk::BufferCreateFlags::SPARSE_BINDING | vk::BufferCreateFlags::SPARSE_ALIASED)
            .size(total)
            .usage(self.sparse_usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .build();
        let raw = unsafe { logical.create_buffer(&buffer_ci, None) }.ok()?;
        let handle = SparseBuffer::new(raw, device);
        let mut binds: Vec<vk::SparseMemoryBind> = Vec::with_capacity(sources.len());
        let mut resource_offset: vk::DeviceSize = 0;
        for source in sources {
            binds.push(vk::SparseMemoryBind {
                resource_offset,
                size: source.size,
                memory: source.memory,
                memory_offset: source.memory_offset + source.offset,
                flags: vk::SparseMemoryBindFlags::empty(),
            });
            resource_offset += source.size;
        }
        let buffer_bind = vk::SparseBufferMemoryBindInfo::builder()
            .buffer(raw)
            .binds(&binds)
            .build();
        let bind_info = vk::BindSparseInfo::builder()
            .buffer_binds(std::slice::from_ref(&buffer_bind))
            .build();
        let fence = unsafe { logical.create_fence(&vk::FenceCreateInfo::default(), None) }.ok()?;
        let bind_result = {
            let submit_mutex = scheduler.submit_mutex();
            let _lock = submit_mutex.lock().unwrap();
            unsafe {
                logical.queue_bind_sparse(
                    device.get_graphics_queue(),
                    std::slice::from_ref(&bind_info),
                    fence,
                )
            }
        };
        let result = match bind_result {
            Ok(()) => {
                let waited = unsafe { logical.wait_for_fences(&[fence], true, u64::MAX) };
                waited.ok().map(|_| handle)
            }
            Err(_) => None,
        };
        unsafe { logical.destroy_fence(fence, None) };
        result
    }

    /// Upstream `RetireEntry`: keeps the entry's buffers alive until the
    /// current tick has been processed by the GPU.
    fn retire_entry(&mut self, scheduler: &mut Scheduler, entry: &mut Entry) {
        if entry.sparse_handle.is_none() && entry.gathered.is_none() {
            return;
        }
        if self.retired.len() == Self::MAX_RETIRED {
            self.drain_retired(scheduler);
        }
        if self.retired.len() == Self::MAX_RETIRED {
            let oldest = self
                .retired
                .iter()
                .map(|item| item.tick)
                .min()
                .unwrap_or(0);
            scheduler.wait(oldest);
            self.drain_retired(scheduler);
        }
        self.retired.push(Retired {
            handle: entry.sparse_handle.take(),
            gathered: entry.gathered.take(),
            tick: scheduler.current_tick(),
        });
    }

    /// Upstream `DrainRetired` (swap-remove of every retired item the GPU is done with).
    fn drain_retired(&mut self, scheduler: &mut Scheduler) {
        let mut index = 0;
        while index < self.retired.len() {
            if scheduler.is_free(self.retired[index].tick) {
                self.retired.swap_remove(index);
            } else {
                index += 1;
            }
        }
    }

    /// Upstream `Get`: returns (creating if needed) the combined buffer for
    /// `key`, reusing the cached entry when the sources' geometry is unchanged.
    pub fn get(
        &mut self,
        device: &Device,
        scheduler: &mut Scheduler,
        memory_allocator: &MemoryAllocator,
        key: u64,
        sources: &[MultiRangeSource],
        total: vk::DeviceSize,
    ) -> MultiRangeRef {
        if sources.is_empty() || total == 0 {
            return MultiRangeRef::default();
        }
        if !self.retired.is_empty() {
            self.drain_retired(scheduler);
        }
        let geometry = Self::hash_sources(sources);
        let content = Self::hash_content(sources);
        if let Some(entry) = self.entries.get_mut(&key) {
            if entry.geometry == geometry && entry.size == total {
                if entry.content != content {
                    entry.content = content;
                    entry.dirty = true;
                }
                let mut r = MultiRangeRef {
                    handle: entry.handle(),
                    address: entry.address,
                    size: entry.size,
                    sparse: true,
                    needs_gather: false,
                };
                if entry.sparse_handle.is_none() {
                    r.sparse = false;
                    r.needs_gather = entry.dirty;
                }
                return r;
            }
        }
        if let Some(mut stale) = self.entries.remove(&key) {
            self.retire_entry(scheduler, &mut stale);
        }

        let mut entry = Entry {
            gathered: None,
            sparse_handle: None,
            owners: Vec::new(),
            address: 0,
            size: total,
            geometry,
            content,
            dirty: true,
        };
        if self.can_bind_sparse(sources) {
            entry.sparse_handle = self.create_sparse(device, scheduler, sources, total);
            if entry.sparse_handle.is_some() {
                entry.owners.reserve(sources.len());
                for source in sources {
                    entry.owners.push(source.handle);
                }
            }
        }
        if entry.sparse_handle.is_none() {
            let mut flags = vk::BufferUsageFlags::TRANSFER_SRC
                | vk::BufferUsageFlags::TRANSFER_DST
                | vk::BufferUsageFlags::STORAGE_BUFFER;
            if device.is_buffer_device_address_supported() {
                flags |= vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS;
            }
            let gather_ci = vk::BufferCreateInfo::builder()
                .size(total)
                .usage(flags)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .build();
            // Upstream goes through `vk::Check`, which throws before the entry
            // is cached; a failed allocation must not be memoized as a null
            // buffer or the key would never retry multi-range binding.
            match memory_allocator.create_buffer(&gather_ci, MemoryUsage::DeviceLocal) {
                Ok(gathered) => entry.gathered = Some(gathered),
                Err(error) => {
                    log::error!(
                        "Failed to allocate multi-range gather buffer of {:#x} bytes: {:?}",
                        total,
                        error
                    );
                    return MultiRangeRef::default();
                }
            }
            entry.dirty = true;
        }
        if device.is_buffer_device_address_supported() {
            let address_handle = entry.handle();
            if address_handle != vk::Buffer::null() {
                entry.address = unsafe {
                    device.get_logical().get_buffer_device_address(
                        &vk::BufferDeviceAddressInfo::builder()
                            .buffer(address_handle)
                            .build(),
                    )
                };
            }
        }

        let mut r = MultiRangeRef {
            handle: entry.handle(),
            address: entry.address,
            size: entry.size,
            sparse: true,
            needs_gather: false,
        };
        if entry.sparse_handle.is_none() {
            r.sparse = false;
            r.needs_gather = true;
        }
        self.entries.insert(key, entry);
        r
    }

    /// Upstream `MarkGathered`.
    pub fn mark_gathered(&mut self, key: u64) {
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.dirty = false;
        }
    }

    /// Upstream `DropOwner`: retires every entry aliasing memory of `owner`.
    pub fn drop_owner(&mut self, scheduler: &mut Scheduler, owner: vk::Buffer) {
        if owner == vk::Buffer::null() {
            return;
        }
        let keys: Vec<u64> = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.owners.contains(&owner))
            .map(|(key, _)| *key)
            .collect();
        for key in keys {
            if let Some(mut entry) = self.entries.remove(&key) {
                self.retire_entry(scheduler, &mut entry);
            }
        }
    }

    /// Upstream `Invalidate`.
    pub fn invalidate(&mut self, key: u64) {
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.dirty = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ash::vk::Handle;

    fn source(handle: u64, offset: u64, size: u64, tick: u64, memory_type: u32) -> MultiRangeSource {
        MultiRangeSource {
            handle: vk::Buffer::from_raw(handle),
            memory: vk::DeviceMemory::from_raw(0x10),
            memory_offset: 0,
            offset,
            size,
            write_tick: tick,
            memory_type,
        }
    }

    fn sparse_cache(use_sparse: bool) -> MultiRangeBufferCache {
        MultiRangeBufferCache {
            block_size: 0x1_0000,
            use_sparse,
            entries: HashMap::new(),
            retired: Vec::new(),
            sparse_memory_type_bits: 0b10,
            sparse_usage: vk::BufferUsageFlags::STORAGE_BUFFER,
        }
    }

    #[test]
    fn geometry_hash_ignores_ticks_and_content_hash_ignores_geometry() {
        let a = [source(1, 0, 0x1000, 5, 1), source(2, 0x1000, 0x2000, 6, 1)];
        let b = [source(1, 0, 0x1000, 9, 1), source(2, 0x1000, 0x2000, 9, 1)];
        assert_eq!(
            MultiRangeBufferCache::hash_sources(&a),
            MultiRangeBufferCache::hash_sources(&b)
        );
        assert_ne!(
            MultiRangeBufferCache::hash_content(&a),
            MultiRangeBufferCache::hash_content(&b)
        );
        let c = [source(1, 0, 0x1000, 5, 1), source(3, 0x1000, 0x2000, 6, 1)];
        assert_ne!(
            MultiRangeBufferCache::hash_sources(&a),
            MultiRangeBufferCache::hash_sources(&c)
        );
        assert_eq!(
            MultiRangeBufferCache::hash_content(&a),
            MultiRangeBufferCache::hash_content(&c)
        );
    }

    #[test]
    fn can_bind_sparse_requires_block_alignment_and_memory_type() {
        let cache = sparse_cache(true);
        let aligned = [source(1, 0, 0x1_0000, 0, 1), source(2, 0x2_0000, 0x1_0000, 0, 1)];
        assert!(cache.can_bind_sparse(&aligned));
        let misaligned_offset = [source(1, 0x1000, 0x1_0000, 0, 1)];
        assert!(!cache.can_bind_sparse(&misaligned_offset));
        let misaligned_size = [source(1, 0, 0x1800, 0, 1)];
        assert!(!cache.can_bind_sparse(&misaligned_size));
        let wrong_type = [source(1, 0, 0x1_0000, 0, 0)];
        assert!(!cache.can_bind_sparse(&wrong_type));
        let out_of_range_type = [source(1, 0, 0x1_0000, 0, 32)];
        assert!(!cache.can_bind_sparse(&out_of_range_type));
        let mut null_memory = source(1, 0, 0x1_0000, 0, 1);
        null_memory.memory = vk::DeviceMemory::null();
        assert!(!cache.can_bind_sparse(&[null_memory]));
        assert!(!sparse_cache(false).can_bind_sparse(&aligned));
    }
}
