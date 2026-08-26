// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden `src/video_core/vulkan_common/vulkan_memory_allocator.h` and
//! `src/video_core/vulkan_common/vulkan_memory_allocator.cpp`.
//!
//! Memory allocation subsystem for Vulkan.
//! Eden delegates image and buffer ownership to Vulkan Memory Allocator (VMA).

use ash::vk;
use ash::vk::Handle;
use std::sync::Arc;
use vk_mem::Alloc;

use crate::gpu_logging::{get_instance, is_active};

use super::vma::VmaAllocator;
use super::vulkan_device::Device;
use super::vulkan_wrapper::VulkanError;

// ---------------------------------------------------------------------------
// MemoryUsage — port of `Vulkan::MemoryUsage`
// ---------------------------------------------------------------------------

/// Hints and requirements for the backing memory type of a commit.
///
/// Port of `Vulkan::MemoryUsage` from `vulkan_memory_allocator.h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryUsage {
    /// Requests device local host visible buffer, falling back to device local memory.
    DeviceLocal,
    /// Requires a host visible memory type optimized for CPU to GPU uploads.
    Upload,
    /// Requires a host visible memory type optimized for GPU to CPU readbacks.
    Download,
    /// Requests device local host visible buffer, falling back to host memory.
    Stream,
}

// ---------------------------------------------------------------------------
// Helper: memory property flags for a given usage
// ---------------------------------------------------------------------------

/// Returns the `VkMemoryPropertyFlags` for a given `MemoryUsage`.
///
/// Port of `MemoryUsagePropertyFlags` from `vulkan_memory_allocator.cpp`.
#[allow(dead_code)] // Eden retains this helper as [[maybe_unused]].
fn memory_usage_property_flags(usage: MemoryUsage) -> vk::MemoryPropertyFlags {
    match usage {
        MemoryUsage::DeviceLocal => vk::MemoryPropertyFlags::DEVICE_LOCAL,
        MemoryUsage::Upload => {
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT
        }
        MemoryUsage::Download => {
            vk::MemoryPropertyFlags::HOST_VISIBLE
                | vk::MemoryPropertyFlags::HOST_COHERENT
                | vk::MemoryPropertyFlags::HOST_CACHED
        }
        MemoryUsage::Stream => {
            vk::MemoryPropertyFlags::DEVICE_LOCAL
                | vk::MemoryPropertyFlags::HOST_VISIBLE
                | vk::MemoryPropertyFlags::HOST_COHERENT
        }
    }
}

/// Port of `MemoryUsagePreferredVmaFlags`.
fn memory_usage_preferred_vma_flags(usage: MemoryUsage) -> vk::MemoryPropertyFlags {
    if usage == MemoryUsage::Download {
        return vk::MemoryPropertyFlags::HOST_CACHED | vk::MemoryPropertyFlags::HOST_COHERENT;
    }
    if usage != MemoryUsage::DeviceLocal {
        vk::MemoryPropertyFlags::HOST_COHERENT
    } else {
        vk::MemoryPropertyFlags::empty()
    }
}

/// Port of `MemoryUsageVmaFlags`.
fn memory_usage_vma_flags(usage: MemoryUsage) -> vk_mem::AllocationCreateFlags {
    match usage {
        MemoryUsage::Upload | MemoryUsage::Stream => {
            vk_mem::AllocationCreateFlags::MAPPED
                | vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
        }
        MemoryUsage::Download => {
            vk_mem::AllocationCreateFlags::MAPPED
                | vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
        }
        MemoryUsage::DeviceLocal => vk_mem::AllocationCreateFlags::empty(),
    }
}

/// Port of `MemoryUsageVma`.
fn memory_usage_vma(usage: MemoryUsage) -> vk_mem::MemoryUsage {
    match usage {
        MemoryUsage::DeviceLocal | MemoryUsage::Stream => vk_mem::MemoryUsage::AutoPreferDevice,
        MemoryUsage::Upload | MemoryUsage::Download => vk_mem::MemoryUsage::AutoPreferHost,
    }
}

/// Rust counterpart of Eden's `reinterpret_cast<uintptr_t>(VkDeviceMemory)`
/// in the GPU-memory logging calls.
fn memory_handle_for_gpu_log(memory: vk::DeviceMemory) -> usize {
    memory.as_raw() as usize
}

// ---------------------------------------------------------------------------
// MemoryCommit — port of `Vulkan::MemoryCommit`
// ---------------------------------------------------------------------------

/// Ownership handle of a memory commitment.
/// Points to a subregion of a memory allocation.
///
/// Port of `Vulkan::MemoryCommit` from `vulkan_memory_allocator.h`.
pub struct MemoryCommit {
    allocator: Option<VmaAllocator>,
    allocation: Option<vk_mem::Allocation>,
    memory: vk::DeviceMemory,
    offset: vk::DeviceSize,
    size: vk::DeviceSize,
    mapped_ptr: *mut u8,
}

// SAFETY: The memory handle is owned by the Vulkan device and the commit
// is only accessed through the allocator which synchronizes access.
unsafe impl Send for MemoryCommit {}
unsafe impl Sync for MemoryCommit {}

impl MemoryCommit {
    /// Creates an empty commit.
    pub fn new() -> Self {
        Self {
            allocator: None,
            allocation: None,
            memory: vk::DeviceMemory::null(),
            offset: 0,
            size: 0,
            mapped_ptr: std::ptr::null_mut(),
        }
    }

    fn from_allocation(
        allocator: VmaAllocator,
        allocation: vk_mem::Allocation,
        info: &vk_mem::AllocationInfo,
    ) -> Self {
        if is_active()
            && *common::settings::values()
                .gpu_log_memory_tracking
                .get_value()
        {
            get_instance().log_memory_allocation(
                memory_handle_for_gpu_log(info.device_memory),
                info.size,
                0,
            );
        }
        Self {
            allocator: Some(allocator),
            allocation: Some(allocation),
            memory: info.device_memory,
            offset: info.offset,
            size: info.size,
            mapped_ptr: info.mapped_data.cast(),
        }
    }

    fn ensure_mapped(&mut self) -> bool {
        if self.allocation.is_none() {
            return false;
        }
        if self.mapped_ptr.is_null() {
            let allocator = Arc::clone(self.allocator.as_ref().unwrap());
            let allocator = allocator.lock().expect("VMA allocator mutex poisoned");
            let allocation = self.allocation.as_mut().unwrap();
            let Ok(mapped_ptr) = (unsafe { allocator.map_memory(allocation) }) else {
                return false;
            };
            self.mapped_ptr = mapped_ptr;
        }
        true
    }

    /// Maps the allocation and returns its complete byte span.
    pub fn map(&mut self) -> &mut [u8] {
        if !self.ensure_mapped() {
            return &mut [];
        }
        let size = usize::try_from(self.size).unwrap_or(usize::MAX);
        unsafe { std::slice::from_raw_parts_mut(self.mapped_ptr, size) }
    }

    /// Rust name for Eden's const `MemoryCommit::Map()` overload.
    pub fn map_read(&mut self) -> &[u8] {
        if !self.ensure_mapped() {
            return &[];
        }
        let size = usize::try_from(self.size).unwrap_or(usize::MAX);
        unsafe { std::slice::from_raw_parts(self.mapped_ptr, size) }
    }

    pub fn unmap(&mut self) {
        if self.allocation.is_some() && !self.mapped_ptr.is_null() {
            let allocator = Arc::clone(self.allocator.as_ref().unwrap());
            let allocator = allocator.lock().expect("VMA allocator mutex poisoned");
            unsafe {
                allocator.unmap_memory(self.allocation.as_mut().unwrap());
            }
            self.mapped_ptr = std::ptr::null_mut();
        }
    }

    pub fn is_valid(&self) -> bool {
        self.allocation.is_some()
    }

    /// Returns the Vulkan memory handle.
    ///
    /// Port of `MemoryCommit::Memory()`.
    pub fn memory(&self) -> vk::DeviceMemory {
        self.memory
    }

    /// Returns the start position of the commit relative to the allocation.
    ///
    /// Port of `MemoryCommit::Offset()`.
    pub fn offset(&self) -> vk::DeviceSize {
        self.offset
    }

    /// Returns the size of this commit.
    pub fn size(&self) -> vk::DeviceSize {
        self.size
    }

    pub fn allocation(&self) -> Option<&vk_mem::Allocation> {
        self.allocation.as_ref()
    }

    fn release(&mut self) {
        let Some(mut allocation) = self.allocation.take() else {
            return;
        };
        let allocator = self.allocator.take().unwrap();
        if is_active()
            && *common::settings::values()
                .gpu_log_memory_tracking
                .get_value()
            && self.memory != vk::DeviceMemory::null()
        {
            get_instance().log_memory_deallocation(memory_handle_for_gpu_log(self.memory));
        }
        let allocator = allocator.lock().expect("VMA allocator mutex poisoned");
        unsafe {
            if !self.mapped_ptr.is_null() {
                allocator.unmap_memory(&mut allocation);
                self.mapped_ptr = std::ptr::null_mut();
            }
            allocator.free_memory(&mut allocation);
        }
        self.memory = vk::DeviceMemory::null();
        self.offset = 0;
        self.size = 0;
    }
}

impl Drop for MemoryCommit {
    fn drop(&mut self) {
        self.release();
    }
}

impl Default for MemoryCommit {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// AllocatedImage — port-facing wrapper for MemoryAllocator::CreateImage
// ---------------------------------------------------------------------------

/// Device-local image allocation returned by `MemoryAllocator::CreateImage`.
///
/// Like Eden's `vk::Image`, this wrapper retains the VMA allocation and releases
/// the image and allocation together.
pub struct AllocatedImage {
    allocator: Option<VmaAllocator>,
    image: vk::Image,
    allocation: Option<vk_mem::Allocation>,
}

// ---------------------------------------------------------------------------
// AllocatedBuffer — owning Rust counterpart of upstream `vk::Buffer`
// ---------------------------------------------------------------------------

/// Buffer and VMA allocation returned together by `MemoryAllocator::CreateBuffer`.
pub struct AllocatedBuffer {
    allocator: VmaAllocator,
    buffer: vk::Buffer,
    allocation: Option<vk_mem::Allocation>,
    mapped_ptr: *mut u8,
    size: vk::DeviceSize,
    coherent: bool,
}

unsafe impl Send for AllocatedBuffer {}
unsafe impl Sync for AllocatedBuffer {}

impl AllocatedBuffer {
    pub fn buffer(&self) -> vk::Buffer {
        self.buffer
    }

    pub fn handle(&self) -> vk::Buffer {
        self.buffer
    }

    pub fn mapped_ptr(&self) -> *mut u8 {
        self.mapped_ptr
    }

    /// Port of `vk::Buffer::IsHostVisible` for the owning ash/VMA wrapper.
    pub fn is_host_visible(&self) -> bool {
        !self.mapped_ptr.is_null() && self.size != 0
    }

    /// Port of mutable `vk::Buffer::Mapped`.
    pub fn mapped_slice_mut(&mut self) -> &mut [u8] {
        if !self.is_host_visible() {
            return &mut [];
        }
        unsafe { std::slice::from_raw_parts_mut(self.mapped_ptr, self.size as usize) }
    }

    pub fn mapped_slice(&self) -> &[u8] {
        if !self.is_host_visible() {
            return &[];
        }
        unsafe { std::slice::from_raw_parts(self.mapped_ptr, self.size as usize) }
    }

    pub fn size(&self) -> vk::DeviceSize {
        self.size
    }

    pub fn is_host_coherent(&self) -> bool {
        self.coherent
    }

    /// Port of `vk::Buffer::Flush`.
    pub fn flush(&self) {
        if self.coherent {
            return;
        }
        let Some(allocation) = self.allocation.as_ref() else {
            return;
        };
        let _ = self
            .allocator
            .lock()
            .expect("VMA allocator mutex poisoned")
            .flush_allocation(allocation, 0, vk::WHOLE_SIZE as usize);
    }

    pub fn invalidate(&self) {
        if self.coherent {
            return;
        }
        let Some(allocation) = self.allocation.as_ref() else {
            return;
        };
        let _ = self
            .allocator
            .lock()
            .expect("VMA allocator mutex poisoned")
            .invalidate_allocation(allocation, 0, vk::WHOLE_SIZE as usize);
    }
}

impl Drop for AllocatedBuffer {
    fn drop(&mut self) {
        let Some(mut allocation) = self.allocation.take() else {
            return;
        };
        let allocator = self.allocator.lock().expect("VMA allocator mutex poisoned");
        unsafe {
            allocator.destroy_buffer(self.buffer, &mut allocation);
        }
        self.buffer = vk::Buffer::null();
        self.mapped_ptr = std::ptr::null_mut();
    }
}

unsafe impl Send for AllocatedImage {}
unsafe impl Sync for AllocatedImage {}

impl AllocatedImage {
    /// Default-constructed counterpart of Eden's empty `vk::Image` wrapper.
    pub(crate) fn null() -> Self {
        Self {
            allocator: None,
            image: vk::Image::null(),
            allocation: None,
        }
    }

    fn new(allocator: VmaAllocator, image: vk::Image, allocation: vk_mem::Allocation) -> Self {
        Self {
            allocator: Some(allocator),
            image,
            allocation: Some(allocation),
        }
    }

    pub fn handle(&self) -> vk::Image {
        self.image
    }
}

impl Drop for AllocatedImage {
    fn drop(&mut self) {
        let Some(mut allocation) = self.allocation.take() else {
            return;
        };
        let allocator = self
            .allocator
            .as_ref()
            .expect("allocated image must retain its VMA allocator")
            .lock()
            .expect("VMA allocator mutex poisoned");
        unsafe {
            allocator.destroy_image(self.image, &mut allocation);
        }
        self.image = vk::Image::null();
    }
}

// ---------------------------------------------------------------------------
// MemoryAllocator — port of `Vulkan::MemoryAllocator`
// ---------------------------------------------------------------------------

/// Memory allocator container.
///
/// Port of `Vulkan::MemoryAllocator` from `vulkan_memory_allocator.h`.
///
/// This allocator owns VMA commits and the image/buffer allocation paths used
/// by the Rust renderer.
pub struct MemoryAllocator {
    /// VMA allocator owned by `Device`, matching upstream `device.GetAllocator()`.
    allocator: VmaAllocator,
    /// The Vulkan device retained by the upstream allocator owner. Ash/VMA
    /// wrappers carry the handles used by Ruzu's allocation paths directly.
    #[allow(dead_code)]
    device: ash::Device,
    /// Physical device memory properties.
    properties: vk::PhysicalDeviceMemoryProperties,
    /// Buffer-image granularity from device limits.
    #[allow(dead_code)] // Retained because Eden's allocator owns the same device limit.
    buffer_image_granularity: vk::DeviceSize,
    /// Valid memory types bitmask (may exclude small device-local heaps for debugging).
    valid_memory_types: u32,
    driver_id: vk::DriverId,
}

impl MemoryAllocator {
    /// Constructs a memory allocator.
    ///
    /// Port of `MemoryAllocator::MemoryAllocator`.
    ///
    pub fn new(vulkan_device: &Device) -> Self {
        let memory_properties = unsafe {
            vulkan_device
                .get_instance()
                .get_physical_device_memory_properties(vulkan_device.get_physical())
        };
        let mut valid_memory_types = !0u32;

        // Port of the RenderDoc heap size check from the C++ constructor.
        // GPUs not supporting rebar may only have a small host visible/device local region.
        // With RenderDoc attached and only a small region, restrict which types are valid.
        if vulkan_device.has_debugging_tool_attached() {
            const SMALL_HEAP_THRESHOLD: u64 = 256 * 1024 * 1024; // 256 MiB
            for i in 0..memory_properties.memory_type_count as usize {
                let mem_type = memory_properties.memory_types[i];
                let flags = mem_type.property_flags;
                if flags.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
                    && flags.contains(vk::MemoryPropertyFlags::HOST_VISIBLE)
                {
                    let heap = memory_properties.memory_heaps[mem_type.heap_index as usize];
                    if heap.size <= SMALL_HEAP_THRESHOLD {
                        valid_memory_types &= !(1u32 << i);
                    }
                }
            }
        }

        Self {
            allocator: Arc::clone(vulkan_device.get_allocator()),
            device: vulkan_device.get_logical().clone(),
            properties: memory_properties,
            buffer_image_granularity: vulkan_device
                .device_properties
                .limits
                .buffer_image_granularity,
            valid_memory_types,
            driver_id: vulkan_device.get_driver_id(),
        }
    }

    /// Creates an owning VMA buffer with Eden's exact allocation policy.
    pub fn create_buffer(
        &self,
        ci: &vk::BufferCreateInfo,
        usage: MemoryUsage,
    ) -> Result<AllocatedBuffer, VulkanError> {
        let anv_flags = if usage == MemoryUsage::Stream
            && self.driver_id == vk::DriverId::INTEL_OPEN_SOURCE_MESA
        {
            vk::MemoryPropertyFlags::HOST_CACHED
        } else {
            vk::MemoryPropertyFlags::empty()
        };
        let allocation_ci = vk_mem::AllocationCreateInfo {
            flags: vk_mem::AllocationCreateFlags::WITHIN_BUDGET | memory_usage_vma_flags(usage),
            usage: memory_usage_vma(usage),
            required_flags: vk::MemoryPropertyFlags::empty(),
            preferred_flags: memory_usage_preferred_vma_flags(usage) | anv_flags,
            memory_type_bits: if usage == MemoryUsage::Stream {
                0
            } else {
                self.valid_memory_types
            },
            ..Default::default()
        };
        let allocator = self.allocator.lock().expect("VMA allocator mutex poisoned");
        let (buffer, allocation) = unsafe {
            allocator
                .create_buffer(ci, &allocation_ci)
                .map_err(VulkanError::new)?
        };
        let allocation_info = allocator.get_allocation_info(&allocation);
        let property_flags =
            self.properties.memory_types[allocation_info.memory_type as usize].property_flags;
        drop(allocator);
        if is_active()
            && *common::settings::values()
                .gpu_log_memory_tracking
                .get_value()
        {
            get_instance().log_memory_allocation(
                memory_handle_for_gpu_log(allocation_info.device_memory),
                allocation_info.size,
                property_flags.as_raw(),
            );
        }
        Ok(AllocatedBuffer {
            allocator: Arc::clone(&self.allocator),
            buffer,
            allocation: Some(allocation),
            mapped_ptr: allocation_info.mapped_data.cast(),
            size: ci.size,
            coherent: property_flags.contains(vk::MemoryPropertyFlags::HOST_COHERENT),
        })
    }

    /// Creates a device-local image with allocation ownership returned to the caller.
    ///
    /// This is the Rust equivalent of the upstream `vk::Image` wrapper returned
    /// by `MemoryAllocator::CreateImage`.
    pub fn create_image(&self, ci: &vk::ImageCreateInfo) -> Result<AllocatedImage, VulkanError> {
        let allocation_ci = vk_mem::AllocationCreateInfo {
            flags: vk_mem::AllocationCreateFlags::WITHIN_BUDGET,
            usage: vk_mem::MemoryUsage::AutoPreferDevice,
            required_flags: vk::MemoryPropertyFlags::empty(),
            preferred_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            memory_type_bits: 0,
            ..Default::default()
        };
        let allocator = self.allocator.lock().expect("VMA allocator mutex poisoned");
        let (image, allocation) = unsafe {
            allocator
                .create_image(ci, &allocation_ci)
                .map_err(VulkanError::new)?
        };
        let allocation_info = allocator.get_allocation_info(&allocation);
        drop(allocator);
        if is_active()
            && *common::settings::values()
                .gpu_log_memory_tracking
                .get_value()
        {
            get_instance().log_memory_allocation(
                memory_handle_for_gpu_log(allocation_info.device_memory),
                allocation_info.size,
                vk::MemoryPropertyFlags::DEVICE_LOCAL.as_raw(),
            );
        }
        Ok(AllocatedImage::new(
            Arc::clone(&self.allocator),
            image,
            allocation,
        ))
    }

    /// Commits a memory region with the specified requirements.
    ///
    /// Port of `MemoryAllocator::Commit(VkMemoryRequirements, MemoryUsage)`.
    pub fn commit(
        &self,
        requirements: &vk::MemoryRequirements,
        usage: MemoryUsage,
    ) -> Result<MemoryCommit, VulkanError> {
        let mut allocation_ci = vk_mem::AllocationCreateInfo {
            flags: vk_mem::AllocationCreateFlags::WITHIN_BUDGET | memory_usage_vma_flags(usage),
            usage: memory_usage_vma(usage),
            memory_type_bits: requirements.memory_type_bits & self.valid_memory_types,
            required_flags: vk::MemoryPropertyFlags::empty(),
            preferred_flags: memory_usage_preferred_vma_flags(usage),
            ..Default::default()
        };
        let allocator = self.allocator.lock().expect("VMA allocator mutex poisoned");
        let mut result = unsafe { allocator.allocate_memory(requirements, &allocation_ci) };
        if result.is_err() {
            allocation_ci
                .flags
                .remove(vk_mem::AllocationCreateFlags::WITHIN_BUDGET);
            result = unsafe { allocator.allocate_memory(requirements, &allocation_ci) };
            if result.is_err()
                && allocation_ci
                    .preferred_flags
                    .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
            {
                allocation_ci.preferred_flags &= !vk::MemoryPropertyFlags::DEVICE_LOCAL;
                result = unsafe { allocator.allocate_memory(requirements, &allocation_ci) };
            }
        }
        let allocation = result.map_err(VulkanError::new)?;
        let info = allocator.get_allocation_info(&allocation);
        drop(allocator);
        Ok(MemoryCommit::from_allocation(
            Arc::clone(&self.allocator),
            allocation,
            &info,
        ))
    }

    /// Allocates and binds memory for a buffer created outside VMA.
    pub fn commit_buffer(
        &self,
        buffer: vk::Buffer,
        usage: MemoryUsage,
    ) -> Result<MemoryCommit, VulkanError> {
        let mut allocation_ci = vk_mem::AllocationCreateInfo {
            flags: vk_mem::AllocationCreateFlags::WITHIN_BUDGET | memory_usage_vma_flags(usage),
            usage: memory_usage_vma(usage),
            required_flags: vk::MemoryPropertyFlags::empty(),
            preferred_flags: memory_usage_preferred_vma_flags(usage),
            ..Default::default()
        };
        let allocator = self.allocator.lock().expect("VMA allocator mutex poisoned");
        let mut result = unsafe { allocator.allocate_memory_for_buffer(buffer, &allocation_ci) };
        if result.is_err() {
            allocation_ci
                .flags
                .remove(vk_mem::AllocationCreateFlags::WITHIN_BUDGET);
            result = unsafe { allocator.allocate_memory_for_buffer(buffer, &allocation_ci) };
            if result.is_err()
                && allocation_ci
                    .preferred_flags
                    .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
            {
                allocation_ci.preferred_flags &= !vk::MemoryPropertyFlags::DEVICE_LOCAL;
                result = unsafe { allocator.allocate_memory_for_buffer(buffer, &allocation_ci) };
            }
        }
        let allocation = result.map_err(VulkanError::new)?;
        unsafe {
            allocator
                .bind_buffer_memory2(&allocation, 0, buffer, std::ptr::null())
                .map_err(VulkanError::new)?;
        }
        let info = allocator.get_allocation_info(&allocation);
        drop(allocator);
        Ok(MemoryCommit::from_allocation(
            Arc::clone(&self.allocator),
            allocation,
            &info,
        ))
    }
}

// ---------------------------------------------------------------------------
// ForEachDeviceLocalHostVisibleHeap helper
// ---------------------------------------------------------------------------

/// Iterates over device-local, host-visible memory heaps.
///
/// Port of `ForEachDeviceLocalHostVisibleHeap` from `vulkan_memory_allocator.h`.
pub fn for_each_device_local_host_visible_heap<F>(
    memory_props: &vk::PhysicalDeviceMemoryProperties,
    mut f: F,
) where
    F: FnMut(usize, &vk::MemoryHeap),
{
    for i in 0..memory_props.memory_type_count as usize {
        let memory_type = &memory_props.memory_types[i];
        if memory_type
            .property_flags
            .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
            && memory_type
                .property_flags
                .contains(vk::MemoryPropertyFlags::HOST_VISIBLE)
        {
            let heap = &memory_props.memory_heaps[memory_type.heap_index as usize];
            f(memory_type.heap_index as usize, heap);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_memory_commit_matches_upstream_null_state() {
        let commit = MemoryCommit::new();
        assert!(!commit.is_valid());
        assert_eq!(commit.memory(), vk::DeviceMemory::null());
        assert_eq!(commit.offset(), 0);
        assert_eq!(commit.size(), 0);
        assert!(commit.allocation().is_none());
    }

    #[test]
    fn test_memory_usage_property_flags() {
        let flags = memory_usage_property_flags(MemoryUsage::DeviceLocal);
        assert!(flags.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL));

        let flags = memory_usage_property_flags(MemoryUsage::Upload);
        assert!(flags.contains(vk::MemoryPropertyFlags::HOST_VISIBLE));
        assert!(flags.contains(vk::MemoryPropertyFlags::HOST_COHERENT));

        let flags = memory_usage_property_flags(MemoryUsage::Download);
        assert!(flags.contains(vk::MemoryPropertyFlags::HOST_CACHED));

        let flags = memory_usage_property_flags(MemoryUsage::Stream);
        assert!(flags.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL));
        assert!(flags.contains(vk::MemoryPropertyFlags::HOST_VISIBLE));
    }

    #[test]
    fn gpu_log_memory_handle_preserves_the_vulkan_handle_bits() {
        let raw = 0x7654_3210u64;
        let memory = vk::DeviceMemory::from_raw(raw);
        assert_eq!(memory_handle_for_gpu_log(memory) as u64, raw);
    }

    #[test]
    fn create_methods_return_the_owning_raii_wrappers() {
        let _: fn(
            &MemoryAllocator,
            &vk::BufferCreateInfo,
            MemoryUsage,
        ) -> Result<AllocatedBuffer, VulkanError> = MemoryAllocator::create_buffer;
        let _: fn(&MemoryAllocator, &vk::ImageCreateInfo) -> Result<AllocatedImage, VulkanError> =
            MemoryAllocator::create_image;
        assert!(std::mem::needs_drop::<AllocatedBuffer>());
        assert!(std::mem::needs_drop::<AllocatedImage>());
    }
}
