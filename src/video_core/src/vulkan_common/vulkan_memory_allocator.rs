// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden `src/video_core/vulkan_common/vulkan_memory_allocator.h` and
//! `src/video_core/vulkan_common/vulkan_memory_allocator.cpp`.
//!
//! Memory allocation subsystem for Vulkan.
//! Eden delegates image and buffer ownership to Vulkan Memory Allocator (VMA).

use ash::vk;
use std::sync::{Arc, Mutex};
use vk_mem::Alloc;

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

// ---------------------------------------------------------------------------
// MappedBuffer — port-facing wrapper for host-visible VMA buffers
// ---------------------------------------------------------------------------

/// Host-visible buffer allocation returned by `MemoryAllocator::CreateBuffer`.
///
/// Upstream uses a VMA allocation wrapper exposing `Mapped()`/`Flush()`. The
/// Rust port keeps the same ownership at the allocator boundary with a
/// dedicated allocation until the VMA backend is ported.
pub struct MappedBuffer {
    device: ash::Device,
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    mapped_ptr: *mut u8,
    size: vk::DeviceSize,
    coherent: bool,
}

unsafe impl Send for MappedBuffer {}
unsafe impl Sync for MappedBuffer {}

impl MappedBuffer {
    pub fn buffer(&self) -> vk::Buffer {
        self.buffer
    }

    pub fn mapped_slice_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.mapped_ptr, self.size as usize) }
    }

    pub fn mapped_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.mapped_ptr, self.size as usize) }
    }

    pub fn mapped_ptr(&self) -> *mut u8 {
        self.mapped_ptr
    }

    pub fn is_host_visible(&self) -> bool {
        !self.mapped_ptr.is_null()
    }

    pub fn is_host_coherent(&self) -> bool {
        self.coherent
    }

    pub fn flush(&self) {
        if self.coherent {
            return;
        }
        let range = vk::MappedMemoryRange::builder()
            .memory(self.memory)
            .offset(0)
            .size(self.size)
            .build();
        unsafe {
            self.device.flush_mapped_memory_ranges(&[range]).ok();
        }
    }

    pub fn invalidate(&self) {
        if self.coherent {
            return;
        }
        let range = vk::MappedMemoryRange::builder()
            .memory(self.memory)
            .offset(0)
            .size(self.size)
            .build();
        unsafe {
            self.device.invalidate_mapped_memory_ranges(&[range]).ok();
        }
    }
}

impl Drop for MappedBuffer {
    fn drop(&mut self) {
        unsafe {
            if !self.mapped_ptr.is_null() {
                self.device.unmap_memory(self.memory);
                self.mapped_ptr = std::ptr::null_mut();
            }
            if self.buffer != vk::Buffer::null() {
                self.device.destroy_buffer(self.buffer, None);
                self.buffer = vk::Buffer::null();
            }
            if self.memory != vk::DeviceMemory::null() {
                self.device.free_memory(self.memory, None);
                self.memory = vk::DeviceMemory::null();
            }
        }
    }
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

enum DedicatedResource {
    Buffer {
        buffer: vk::Buffer,
        memory: vk::DeviceMemory,
    },
}

/// Memory allocator container.
///
/// Port of `Vulkan::MemoryAllocator` from `vulkan_memory_allocator.h`.
///
/// This allocator owns VMA commits and the image/buffer allocation paths used
/// by the Rust renderer.
pub struct MemoryAllocator {
    /// VMA allocator owned by `Device`, matching upstream `device.GetAllocator()`.
    allocator: VmaAllocator,
    /// The Vulkan device.
    device: ash::Device,
    /// Physical device memory properties.
    properties: vk::PhysicalDeviceMemoryProperties,
    /// Buffer-image granularity from device limits.
    #[allow(dead_code)] // Retained because Eden's allocator owns the same device limit.
    buffer_image_granularity: vk::DeviceSize,
    dedicated_resources: Mutex<Vec<DedicatedResource>>,
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
            dedicated_resources: Mutex::new(Vec::new()),
            valid_memory_types,
            driver_id: vulkan_device.get_driver_id(),
        }
    }

    /// Creates an owning VMA buffer with Eden's exact allocation policy.
    pub fn create_owned_buffer(
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
    pub fn create_owned_image(
        &self,
        ci: &vk::ImageCreateInfo,
    ) -> Result<AllocatedImage, VulkanError> {
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
        Ok(AllocatedImage::new(
            Arc::clone(&self.allocator),
            image,
            allocation,
        ))
    }

    /// Creates a VMA-allocated buffer.
    ///
    /// Port of `MemoryAllocator::CreateBuffer`.
    ///
    /// NOTE: Upstream delegates to VMA. The Rust port currently uses a
    /// dedicated Vulkan allocation per buffer and keeps it alive in the
    /// allocator until drop.
    pub fn create_buffer(
        &self,
        ci: &vk::BufferCreateInfo,
        usage: MemoryUsage,
    ) -> Result<vk::Buffer, VulkanError> {
        let buffer = unsafe {
            self.device
                .create_buffer(ci, None)
                .map_err(VulkanError::new)?
        };
        let requirements = unsafe { self.device.get_buffer_memory_requirements(buffer) };
        let flags = self.memory_property_flags(
            requirements.memory_type_bits,
            memory_usage_property_flags(usage),
        );
        let type_index = match self.find_type(flags, requirements.memory_type_bits) {
            Some(index) => index,
            None => {
                unsafe {
                    self.device.destroy_buffer(buffer, None);
                }
                return Err(VulkanError::new(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY));
            }
        };
        let mut address_flags =
            vk::MemoryAllocateFlagsInfo::builder().flags(vk::MemoryAllocateFlags::DEVICE_ADDRESS);
        let mut alloc_info_builder = vk::MemoryAllocateInfo::builder()
            .allocation_size(requirements.size)
            .memory_type_index(type_index);
        if ci
            .usage
            .contains(vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS)
        {
            alloc_info_builder = alloc_info_builder.push_next(&mut address_flags);
        }
        let alloc_info = alloc_info_builder.build();
        let memory = match unsafe { self.device.allocate_memory(&alloc_info, None) } {
            Ok(memory) => memory,
            Err(err) => {
                unsafe {
                    self.device.destroy_buffer(buffer, None);
                }
                return Err(VulkanError::new(err));
            }
        };
        if let Err(err) = unsafe { self.device.bind_buffer_memory(buffer, memory, 0) } {
            unsafe {
                self.device.free_memory(memory, None);
                self.device.destroy_buffer(buffer, None);
            }
            return Err(VulkanError::new(err));
        }

        self.dedicated_resources
            .lock()
            .expect("dedicated resource mutex poisoned")
            .push(DedicatedResource::Buffer { buffer, memory });
        Ok(buffer)
    }

    /// Releases a dedicated buffer previously returned by `create_buffer`.
    /// This is the explicit Rust counterpart of dropping upstream's owning
    /// `vk::Buffer` wrapper.
    pub fn destroy_buffer(&self, buffer: vk::Buffer) {
        if buffer == vk::Buffer::null() {
            return;
        }
        let Ok(mut resources) = self.dedicated_resources.lock() else {
            return;
        };
        let Some(index) = resources.iter().position(|resource| {
            matches!(resource, DedicatedResource::Buffer { buffer: owned, .. } if *owned == buffer)
        }) else {
            return;
        };
        let DedicatedResource::Buffer { buffer, memory } = resources.swap_remove(index) else {
            unreachable!();
        };
        unsafe {
            self.device.destroy_buffer(buffer, None);
            self.device.free_memory(memory, None);
        }
    }

    /// Creates a host-visible mapped buffer.
    ///
    /// Port-facing equivalent of upstream `MemoryAllocator::CreateBuffer`
    /// when callers immediately use the returned allocation's `Mapped()`.
    pub fn create_mapped_buffer(
        &self,
        ci: &vk::BufferCreateInfo,
        usage: MemoryUsage,
    ) -> Result<MappedBuffer, VulkanError> {
        let buffer = unsafe {
            self.device
                .create_buffer(ci, None)
                .map_err(VulkanError::new)?
        };
        let requirements = unsafe { self.device.get_buffer_memory_requirements(buffer) };
        let flags = self.memory_property_flags(
            requirements.memory_type_bits,
            memory_usage_property_flags(usage),
        );
        if !flags.contains(vk::MemoryPropertyFlags::HOST_VISIBLE) {
            unsafe {
                self.device.destroy_buffer(buffer, None);
            }
            return Err(VulkanError::new(vk::Result::ERROR_MEMORY_MAP_FAILED));
        }
        let type_index = match self.find_type(flags, requirements.memory_type_bits) {
            Some(index) => index,
            None => {
                unsafe {
                    self.device.destroy_buffer(buffer, None);
                }
                return Err(VulkanError::new(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY));
            }
        };
        let alloc_info = vk::MemoryAllocateInfo::builder()
            .allocation_size(requirements.size)
            .memory_type_index(type_index)
            .build();
        let memory = match unsafe { self.device.allocate_memory(&alloc_info, None) } {
            Ok(memory) => memory,
            Err(err) => {
                unsafe {
                    self.device.destroy_buffer(buffer, None);
                }
                return Err(VulkanError::new(err));
            }
        };
        if let Err(err) = unsafe { self.device.bind_buffer_memory(buffer, memory, 0) } {
            unsafe {
                self.device.free_memory(memory, None);
                self.device.destroy_buffer(buffer, None);
            }
            return Err(VulkanError::new(err));
        }
        let mapped_ptr = match unsafe {
            self.device
                .map_memory(memory, 0, requirements.size, vk::MemoryMapFlags::empty())
        } {
            Ok(ptr) => ptr.cast::<u8>(),
            Err(err) => {
                unsafe {
                    self.device.free_memory(memory, None);
                    self.device.destroy_buffer(buffer, None);
                }
                return Err(VulkanError::new(err));
            }
        };
        let coherent = self.properties.memory_types[type_index as usize]
            .property_flags
            .contains(vk::MemoryPropertyFlags::HOST_COHERENT);
        Ok(MappedBuffer {
            device: self.device.clone(),
            buffer,
            memory,
            mapped_ptr,
            size: requirements.size,
            coherent,
        })
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

    /// Returns the best compatible memory property flags.
    ///
    /// Port of `MemoryAllocator::MemoryPropertyFlags`.
    fn memory_property_flags(
        &self,
        type_mask: u32,
        flags: vk::MemoryPropertyFlags,
    ) -> vk::MemoryPropertyFlags {
        if self.find_type(flags, type_mask).is_some() {
            return flags;
        }
        if flags.contains(vk::MemoryPropertyFlags::HOST_CACHED) {
            return self
                .memory_property_flags(type_mask, flags & !vk::MemoryPropertyFlags::HOST_CACHED);
        }
        if flags.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL) {
            return self
                .memory_property_flags(type_mask, flags & !vk::MemoryPropertyFlags::DEVICE_LOCAL);
        }
        log::error!("No compatible memory types found");
        vk::MemoryPropertyFlags::empty()
    }

    /// Finds a memory type index matching the given flags and type mask.
    ///
    /// Port of `MemoryAllocator::FindType`.
    fn find_type(&self, flags: vk::MemoryPropertyFlags, type_mask: u32) -> Option<u32> {
        for type_index in 0..self.properties.memory_type_count {
            let type_flags = self.properties.memory_types[type_index as usize].property_flags;
            let shifted_type = 1u32 << type_index;
            if (self.valid_memory_types & shifted_type) != 0
                && (type_mask & shifted_type) != 0
                && (type_flags & flags) == flags
            {
                return Some(type_index);
            }
        }
        None
    }
}

impl Drop for MemoryAllocator {
    fn drop(&mut self) {
        if let Ok(mut resources) = self.dedicated_resources.lock() {
            for resource in resources.drain(..).rev() {
                unsafe {
                    match resource {
                        DedicatedResource::Buffer { buffer, memory } => {
                            self.device.destroy_buffer(buffer, None);
                            self.device.free_memory(memory, None);
                        }
                    }
                }
            }
        }
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
}
