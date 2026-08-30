// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Pool of staging buffers for CPU↔GPU data transfer.
//!
//! Ref: zuyu `vk_staging_buffer_pool.h` — stream buffer + size-class caching
//! for efficient CPU→GPU and GPU→CPU transfers.

use std::ptr::NonNull;

use ash::vk;
use log::trace;

use super::scheduler::Scheduler;
use crate::vulkan_common::vulkan_device::{Device, DeviceReference};
use crate::vulkan_common::vulkan_memory_allocator::{
    AllocatedBuffer, MemoryAllocator, MemoryUsage,
};
use crate::vulkan_common::vulkan_wrapper::VulkanError;

// Port of the anonymous-namespace constants in `vk_staging_buffer_pool.cpp`.
const MAX_ALIGNMENT: vk::DeviceSize = 256;
#[cfg(any(target_os = "windows", target_os = "android"))]
const MAX_STREAM_BUFFER_SIZE: vk::DeviceSize = 256 * 1024 * 1024;
#[cfg(not(any(target_os = "windows", target_os = "android")))]
const MAX_STREAM_BUFFER_SIZE: vk::DeviceSize = 128 * 1024 * 1024;

/// Port of upstream `StagingBufferRef`.
#[derive(Clone, Copy)]
pub struct StagingBufferRef {
    pub buffer: vk::Buffer,
    pub device_address: vk::DeviceAddress,
    pub offset: vk::DeviceSize,
    pub mapped: *mut u8,
    pub size: vk::DeviceSize,
    pub usage: MemoryUsage,
    pub log2_level: u32,
    pub index: u64,
}

impl crate::buffer_cache::buffer_cache_base::BufferCacheAsyncBuffer for StagingBufferRef {
    fn offset(&self) -> u64 {
        self.offset
    }

    fn mapped_span(&self) -> &[u8] {
        if self.size == 0 {
            return &[];
        }
        assert!(!self.mapped.is_null());
        unsafe { std::slice::from_raw_parts(self.mapped, self.size as usize) }
    }

    fn mapped_span_mut(&mut self) -> &mut [u8] {
        if self.size == 0 {
            return &mut [];
        }
        assert!(!self.mapped.is_null());
        unsafe { std::slice::from_raw_parts_mut(self.mapped, self.size as usize) }
    }

    #[cfg(test)]
    fn empty_for_test() -> Self {
        Self {
            buffer: vk::Buffer::null(),
            device_address: 0,
            offset: 0,
            mapped: std::ptr::null_mut(),
            size: 0,
            usage: MemoryUsage::Upload,
            log2_level: 0,
            index: 0,
        }
    }
}

// Raw pointer is only used for mapped memory
unsafe impl Send for StagingBufferRef {}

struct OwnedStagingBuffer {
    reference: StagingBufferRef,
    _allocation: AllocatedBuffer,
    tick: u64,
    deferred: bool,
}

struct StagingBuffers {
    entries: Vec<OwnedStagingBuffer>,
    delete_index: usize,
    iterate_index: usize,
}

impl StagingBuffers {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            delete_index: 0,
            iterate_index: 0,
        }
    }
}

/// Pool of staging buffers for CPU↔GPU data transfer.
///
/// Ref: zuyu StagingBufferPool — uses a large stream buffer for small
/// allocations and a free list for larger ones.
pub struct StagingBufferPool {
    device_owner: DeviceReference,
    memory_allocator: NonNull<MemoryAllocator>,
    scheduler: NonNull<Scheduler>,

    /// Stream buffer for small per-draw allocations (uniforms, index data).
    ///
    /// Port of upstream's 128MiB stream buffer split into `NUM_SYNCS`
    /// regions, each stamped with the tick of the submission that may read
    /// it (vk_staging_buffer_pool.cpp GetStreamBuffer). A region is only
    /// reused once `Scheduler::known_gpu_tick` passes its stamp — the old
    /// per-frame `stream_offset = 0` reset recycled mappings while earlier
    /// (possibly not even submitted) work still read them.
    stream_buffer: OwnedStagingBuffer,
    stream_capacity: vk::DeviceSize,
    stream_iterator: vk::DeviceSize,
    stream_used_iterator: vk::DeviceSize,
    stream_free_iterator: vk::DeviceSize,
    stream_sync_ticks: [u64; Self::NUM_SYNCS],

    device_local_cache: [StagingBuffers; Self::NUM_LEVELS],
    upload_cache: [StagingBuffers; Self::NUM_LEVELS],
    download_cache: [StagingBuffers; Self::NUM_LEVELS],
    current_delete_level: usize,
    buffer_index: u64,
    unique_ids: u64,
}

/// Port of upstream `GetStreamBufferSize`.
fn get_stream_buffer_size(device: &Device) -> vk::DeviceSize {
    if !device.has_debugging_tool_attached() {
        return MAX_STREAM_BUFFER_SIZE;
    }

    let memory_properties = unsafe {
        device
            .get_instance()
            .get_physical_device_memory_properties(device.get_physical())
    };
    let mut size = 0;
    let mut has_device_local_host_visible_heap = false;
    for index in 0..memory_properties.memory_type_count as usize {
        let memory_type = memory_properties.memory_types[index];
        if memory_type
            .property_flags
            .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
            && memory_type
                .property_flags
                .contains(vk::MemoryPropertyFlags::HOST_VISIBLE)
        {
            has_device_local_host_visible_heap = true;
            size = size.max(memory_properties.memory_heaps[memory_type.heap_index as usize].size);
        }
    }
    if has_device_local_host_visible_heap {
        if size <= MAX_STREAM_BUFFER_SIZE {
            size = size * 40 / 100;
        }
    } else {
        size = MAX_STREAM_BUFFER_SIZE;
    }
    let aligned = (size + MAX_ALIGNMENT - 1) & !(MAX_ALIGNMENT - 1);
    aligned.min(MAX_STREAM_BUFFER_SIZE)
}

impl StagingBufferPool {
    /// Port of upstream `StagingBufferPool::NUM_SYNCS`.
    const NUM_SYNCS: usize = 16;
    const NUM_LEVELS: usize = usize::BITS as usize;

    /// Port of upstream `StagingBufferPool::StreamBuf`.
    pub fn stream_buffer_handle(&self) -> vk::Buffer {
        self.stream_buffer.reference.buffer
    }

    pub fn new(
        vulkan_device: &Device,
        memory_allocator: &mut MemoryAllocator,
        scheduler: &mut Scheduler,
    ) -> Result<Self, VulkanError> {
        let stream_capacity = get_stream_buffer_size(vulkan_device);
        let mut usage = vk::BufferUsageFlags::TRANSFER_SRC
            | vk::BufferUsageFlags::UNIFORM_BUFFER
            | vk::BufferUsageFlags::INDEX_BUFFER
            | vk::BufferUsageFlags::STORAGE_BUFFER;
        if vulkan_device.is_ext_transform_feedback_supported() {
            usage |= vk::BufferUsageFlags::TRANSFORM_FEEDBACK_BUFFER_EXT;
        }
        if vulkan_device.is_buffer_device_address_supported() {
            usage |= vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS;
        }
        let stream_ci = vk::BufferCreateInfo::builder()
            .size(stream_capacity)
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .build();
        let allocation = memory_allocator.create_buffer(&stream_ci, MemoryUsage::Stream)?;
        assert!(
            !allocation.mapped_ptr().is_null(),
            "Stream buffer must be host visible"
        );
        let stream_buffer_address = if vulkan_device.is_buffer_device_address_supported() {
            unsafe {
                vulkan_device.get_logical().get_buffer_device_address(
                    &vk::BufferDeviceAddressInfo::builder()
                        .buffer(allocation.handle())
                        .build(),
                )
            }
        } else {
            0
        };
        if vulkan_device.has_debugging_tool_attached() {
            vulkan_device.set_buffer_name(allocation.handle(), "Stream Buffer");
        }
        let stream_buffer = StagingBufferRef {
            buffer: allocation.handle(),
            device_address: stream_buffer_address,
            mapped: allocation.mapped_ptr(),
            offset: 0,
            size: stream_capacity,
            usage: MemoryUsage::DeviceLocal,
            index: 0,
            log2_level: 0,
        };
        Ok(Self {
            device_owner: DeviceReference::new(vulkan_device),
            memory_allocator: NonNull::from(memory_allocator),
            scheduler: NonNull::from(scheduler),
            stream_buffer: OwnedStagingBuffer {
                reference: stream_buffer,
                _allocation: allocation,
                tick: 0,
                deferred: false,
            },
            stream_capacity,
            stream_iterator: 0,
            stream_used_iterator: 0,
            stream_free_iterator: 0,
            stream_sync_ticks: [0; Self::NUM_SYNCS],
            device_local_cache: std::array::from_fn(|_| StagingBuffers::new()),
            upload_cache: std::array::from_fn(|_| StagingBuffers::new()),
            download_cache: std::array::from_fn(|_| StagingBuffers::new()),
            current_delete_level: 0,
            buffer_index: 0,
            unique_ids: 0,
        })
    }

    /// Request a staging buffer for CPU→GPU upload.
    pub fn request_upload_buffer(&mut self, size: vk::DeviceSize) -> Option<StagingBufferRef> {
        self.request_buffer(size, MemoryUsage::Upload, false)
    }

    /// Persistent upload allocation released explicitly by the async texture
    /// unswizzle lifecycle.
    pub fn request_deferred_upload_buffer(
        &mut self,
        size: vk::DeviceSize,
    ) -> Option<StagingBufferRef> {
        self.request_buffer(size, MemoryUsage::Upload, true)
    }

    /// Request device-local scratch storage for GPU-side conversion passes.
    pub fn request_device_local_buffer(
        &mut self,
        size: vk::DeviceSize,
    ) -> Option<StagingBufferRef> {
        self.request_buffer(size, MemoryUsage::DeviceLocal, false)
    }

    /// Request a staging buffer for GPU→CPU readback.
    pub fn request_download_buffer(
        &mut self,
        size: vk::DeviceSize,
        deferred: bool,
    ) -> Option<StagingBufferRef> {
        self.request_buffer(size, MemoryUsage::Download, deferred)
    }

    /// Port of upstream `StagingBufferPool::FreeDeferred`.
    pub fn free_deferred(&mut self, buffer: &mut StagingBufferRef) {
        let scheduler = self.scheduler;
        let entry = self.get_cache_mut(buffer.usage)[buffer.log2_level as usize]
            .entries
            .iter_mut()
            .find(|entry| entry.reference.index == buffer.index)
            .expect("deferred staging buffer missing from Vulkan staging pool");
        assert!(entry.deferred);
        // SAFETY: the scheduler is boxed by the rasterizer and outlives the
        // staging pool, matching upstream's `Scheduler&` member.
        entry.tick = unsafe { scheduler.as_ref() }.current_tick();
        entry.deferred = false;
    }

    /// Per-frame housekeeping (upstream `TickFrame`): rotate the deletion
    /// level. The stream buffer is NOT reset here — its regions retire
    /// individually against the GPU timeline (see `try_stream_allocate`).
    pub fn new_frame(&mut self) {
        self.current_delete_level = (self.current_delete_level + 1) % Self::NUM_LEVELS;
        self.release_cache(MemoryUsage::DeviceLocal);
        self.release_cache(MemoryUsage::Upload);
        self.release_cache(MemoryUsage::Download);
    }

    fn scheduler(&self) -> &Scheduler {
        // SAFETY: `StagingBufferPool` is constructed after the boxed
        // rasterizer scheduler and is dropped before it. The box keeps a stable
        // address for the scheduler.
        unsafe { self.scheduler.as_ref() }
    }

    fn request_buffer(
        &mut self,
        size: vk::DeviceSize,
        usage: MemoryUsage,
        deferred: bool,
    ) -> Option<StagingBufferRef> {
        // Upstream only uses the stream buffer for non-deferred uploads that
        // fit in one region (Request: `size <= region_size`).
        if !deferred
            && usage == MemoryUsage::Upload
            && size <= self.stream_capacity / Self::NUM_SYNCS as vk::DeviceSize
        {
            if let Some(buf) = self.try_stream_allocate(size) {
                return Some(buf);
            }
        }

        if let Some(buffer) = self.try_get_reserved_buffer(size, usage, deferred) {
            return Some(buffer);
        }
        self.create_staging_buffer(size, usage, deferred)
    }

    /// Port of upstream `StagingBufferPool::GetStreamBuffer`
    /// (vk_staging_buffer_pool.cpp:105-139). Returns `None` (upstream falls
    /// back to `GetStagingBuffer`) instead of waiting when the required
    /// regions are still referenced by in-flight GPU work.
    fn try_stream_allocate(&mut self, size: vk::DeviceSize) -> Option<StagingBufferRef> {
        let region_size = self.stream_capacity / Self::NUM_SYNCS as vk::DeviceSize;
        let region = |offset: vk::DeviceSize| (offset / region_size) as usize;

        if self.are_stream_regions_active(
            region(self.stream_free_iterator) + 1,
            (region(self.stream_iterator + size) + 1).min(Self::NUM_SYNCS),
        ) {
            // Avoid waiting for the previous usages to be free.
            return None;
        }

        let current_tick = self.scheduler().current_tick();
        for tick in &mut self.stream_sync_ticks
            [region(self.stream_used_iterator)..region(self.stream_iterator)]
        {
            *tick = current_tick;
        }
        self.stream_used_iterator = self.stream_iterator;
        self.stream_free_iterator = self.stream_free_iterator.max(self.stream_iterator + size);

        if self.stream_iterator + size >= self.stream_capacity {
            for tick in
                &mut self.stream_sync_ticks[region(self.stream_used_iterator)..Self::NUM_SYNCS]
            {
                *tick = current_tick;
            }
            self.stream_used_iterator = 0;
            self.stream_iterator = 0;
            self.stream_free_iterator = size;

            if self.are_stream_regions_active(0, region(size) + 1) {
                // Avoid waiting for the previous usages to be free.
                return None;
            }
        }

        let offset = self.stream_iterator;
        self.stream_iterator =
            (self.stream_iterator + size + MAX_ALIGNMENT - 1) & !(MAX_ALIGNMENT - 1);

        let stream = &self.stream_buffer.reference;
        Some(StagingBufferRef {
            buffer: stream.buffer,
            device_address: stream.device_address,
            mapped: unsafe { stream.mapped.add(offset as usize) },
            offset,
            size,
            usage: MemoryUsage::DeviceLocal,
            index: 0,
            log2_level: 0,
        })
    }

    /// Port of upstream `StagingBufferPool::AreRegionsActive`.
    fn are_stream_regions_active(&self, region_begin: usize, region_end: usize) -> bool {
        let gpu_tick = self.scheduler().known_gpu_tick();
        self.stream_sync_ticks[region_begin..region_end]
            .iter()
            .any(|&sync_tick| gpu_tick < sync_tick)
    }

    fn try_get_reserved_buffer(
        &mut self,
        size: vk::DeviceSize,
        usage: MemoryUsage,
        deferred: bool,
    ) -> Option<StagingBufferRef> {
        let log2_level = log2_ceil(size) as usize;
        let cache_level = &self.get_cache(usage)[log2_level];
        let hint = cache_level.iterate_index;
        let is_free =
            |entry: &OwnedStagingBuffer| !entry.deferred && self.scheduler().is_free(entry.tick);
        let free_index = cache_level.entries[hint..]
            .iter()
            .position(|entry| is_free(entry))
            .map(|index| hint + index)
            .or_else(|| {
                cache_level.entries[..hint]
                    .iter()
                    .position(|entry| is_free(entry))
            })?;
        self.get_cache_mut(usage)[log2_level].iterate_index = free_index + 1;
        let tick = if deferred {
            u64::MAX
        } else {
            self.scheduler().current_tick()
        };
        let cache_level = &mut self.get_cache_mut(usage)[log2_level];
        let entry = &mut cache_level.entries[free_index];
        entry.tick = tick;
        assert!(!entry.deferred);
        entry.deferred = deferred;
        Some(entry.reference)
    }

    fn create_staging_buffer(
        &mut self,
        size: vk::DeviceSize,
        usage: MemoryUsage,
        deferred: bool,
    ) -> Option<StagingBufferRef> {
        let log2_level = log2_ceil((size as u32) as vk::DeviceSize);
        let allocation_size = 1u64 << log2_level;
        let mut buffer = self.allocate_buffer(allocation_size, usage)?;
        buffer.reference.index = self.unique_ids;
        buffer.reference.log2_level = log2_level;
        buffer.tick = if deferred {
            u64::MAX
        } else {
            self.scheduler().current_tick()
        };
        buffer.deferred = deferred;
        self.unique_ids = self.unique_ids.wrapping_add(1);
        let reference = buffer.reference;
        self.get_cache_mut(usage)[log2_level as usize]
            .entries
            .push(buffer);
        Some(reference)
    }

    fn get_cache(&self, usage: MemoryUsage) -> &[StagingBuffers; Self::NUM_LEVELS] {
        match usage {
            MemoryUsage::DeviceLocal => &self.device_local_cache,
            MemoryUsage::Upload => &self.upload_cache,
            MemoryUsage::Download => &self.download_cache,
            MemoryUsage::Stream => panic!("invalid staging-cache memory usage: Stream"),
        }
    }

    fn get_cache_mut(&mut self, usage: MemoryUsage) -> &mut [StagingBuffers; Self::NUM_LEVELS] {
        match usage {
            MemoryUsage::DeviceLocal => &mut self.device_local_cache,
            MemoryUsage::Upload => &mut self.upload_cache,
            MemoryUsage::Download => &mut self.download_cache,
            MemoryUsage::Stream => panic!("invalid staging-cache memory usage: Stream"),
        }
    }

    fn release_cache(&mut self, usage: MemoryUsage) {
        self.release_level(usage, self.current_delete_level);
    }

    fn release_level(&mut self, usage: MemoryUsage, log2_level: usize) {
        const DELETIONS_PER_TICK: usize = 16;
        let (begin, end) = {
            let level = &self.get_cache(usage)[log2_level];
            let begin = level.delete_index;
            (begin, (begin + DELETIONS_PER_TICK).min(level.entries.len()))
        };
        let scheduler = self.scheduler;
        let level = &mut self.get_cache_mut(usage)[log2_level];
        erase_if_in_range(&mut level.entries, begin..end, |entry| {
            // SAFETY: the scheduler is boxed by the rasterizer and outlives
            // the staging pool, matching upstream's `Scheduler&` member.
            unsafe { scheduler.as_ref() }.is_free(entry.tick)
        });
        level.delete_index += DELETIONS_PER_TICK;
        if level.delete_index >= level.entries.len() {
            level.delete_index = 0;
        }
        if level.iterate_index > level.entries.len() {
            level.iterate_index = 0;
        }
    }

    fn allocate_buffer(
        &mut self,
        size: vk::DeviceSize,
        usage: MemoryUsage,
    ) -> Option<OwnedStagingBuffer> {
        let supports_device_address = self.device_owner.get().is_buffer_device_address_supported();
        let buf_info = vk::BufferCreateInfo::builder()
            .size(size)
            .usage({
                let mut flags = vk::BufferUsageFlags::TRANSFER_SRC
                    | vk::BufferUsageFlags::TRANSFER_DST
                    | vk::BufferUsageFlags::UNIFORM_BUFFER
                    | vk::BufferUsageFlags::STORAGE_BUFFER
                    | vk::BufferUsageFlags::INDEX_BUFFER
                    | vk::BufferUsageFlags::VERTEX_BUFFER;
                if self
                    .device_owner
                    .get()
                    .is_ext_transform_feedback_supported()
                {
                    flags |= vk::BufferUsageFlags::TRANSFORM_FEEDBACK_BUFFER_EXT;
                }
                if supports_device_address {
                    flags |= vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS;
                }
                flags
            })
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .build();

        // SAFETY: the allocator is boxed by RendererVulkan and outlives the
        // rasterizer and its staging pool, matching upstream's reference member.
        let memory_allocator = unsafe { self.memory_allocator.as_ref() };
        let allocation = memory_allocator.create_buffer(&buf_info, usage).ok()?;
        let buffer = allocation.handle();
        let mapped = allocation.mapped_ptr();
        assert!(usage == MemoryUsage::DeviceLocal || !mapped.is_null());

        if self.device_owner.get().has_debugging_tool_attached() {
            self.buffer_index = self.buffer_index.wrapping_add(1);
            self.device_owner
                .get()
                .set_buffer_name(buffer, &format!("Staging Buffer {}", self.buffer_index));
        }

        trace!("StagingBufferPool: allocated {} bytes", size);

        let device_address = if supports_device_address {
            unsafe {
                self.device_owner
                    .get()
                    .get_logical()
                    .get_buffer_device_address(
                        &vk::BufferDeviceAddressInfo::builder()
                            .buffer(buffer)
                            .build(),
                    )
            }
        } else {
            0
        };
        Some(OwnedStagingBuffer {
            reference: StagingBufferRef {
                buffer,
                device_address,
                mapped,
                offset: 0,
                size,
                usage,
                index: 0,
                log2_level: log2_ceil(size),
            },
            _allocation: allocation,
            tick: 0,
            deferred: false,
        })
    }
}

/// Mechanical Rust equivalent of upstream's `erase(remove_if(begin, end))`.
fn erase_if_in_range<T>(
    entries: &mut Vec<T>,
    range: std::ops::Range<usize>,
    predicate: impl FnMut(&mut T) -> bool,
) {
    entries.extract_if(range, predicate).for_each(drop);
}

fn log2_ceil(value: vk::DeviceSize) -> u32 {
    if value <= 1 {
        0
    } else {
        vk::DeviceSize::BITS - (value - 1).leading_zeros()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_staging_buffer_send() {
        // Verify StagingBufferRef is Send
        fn assert_send<T: Send>() {}
        assert_send::<StagingBufferRef>();
    }

    #[test]
    fn log2_ceil_matches_staging_size_classes() {
        assert_eq!(log2_ceil(1), 0);
        assert_eq!(log2_ceil(2), 1);
        assert_eq!(log2_ceil(3), 2);
        assert_eq!(log2_ceil(4), 2);
        assert_eq!(log2_ceil(5), 3);
        assert_eq!(log2_ceil(1024), 10);
        assert_eq!(log2_ceil(1025), 11);
    }

    #[test]
    fn release_level_compacts_only_the_upstream_deletion_window() {
        let mut entries = vec![0, 1, 2, 3, 4, 5, 6];
        erase_if_in_range(&mut entries, 1..5, |entry| *entry % 2 == 0);
        assert_eq!(entries, [0, 1, 3, 5, 6]);
    }
}
