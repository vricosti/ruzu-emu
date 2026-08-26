// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! GPU buffer cache for vertex, index, uniform, and storage data.
//!
//! Ref: Eden `video_core/renderer_vulkan/vk_buffer_cache.{h,cpp}` — caches
//! VkBuffer objects by GPU VA range to avoid redundant uploads of unchanged data.

use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;

use ash::vk;
use ash::vk::Handle;
use common::slot_vector::SlotVector;
use smallvec::SmallVec;

use super::compute_pass::{QuadIndexedPass, Uint8Pass};
use super::descriptor_pool::DescriptorPool;
use super::scheduler::Scheduler;
use super::staging_buffer_pool::StagingBufferPool;
use super::update_descriptor::{ComputePassDescriptorQueue, UpdateDescriptorQueue};
use crate::buffer_cache::buffer_base::{BufferBase, NullBufferParams};
use crate::buffer_cache::buffer_cache::BufferCache as CommonBufferCache;
use crate::buffer_cache::buffer_cache_base::{
    self as base, BufferCacheBuffer, BufferCopy, HostBindings,
};
use crate::buffer_cache::usage_tracker::UsageTracker;
use crate::engines::maxwell_3d::{IndexFormat, PrimitiveTopology};
use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
use crate::surface::PixelFormat;
use crate::vulkan_common::vulkan_device::{Device, DeviceReference, FormatType};
use crate::vulkan_common::vulkan_memory_allocator::{
    AllocatedBuffer, MemoryAllocator, MemoryUsage,
};
use crate::vulkan_common::vulkan_wrapper::{
    PIPELINE_STAGE_GRAPHICS_COMPUTE, PIPELINE_STAGE_GRAPHICS_COMPUTE_TRANSFER,
};

/// Cached Vulkan buffer view for texture/image buffer descriptors.
struct BufferView {
    pub offset: u32,
    pub size: u32,
    pub format: PixelFormat,
    pub view: vk::BufferView,
}

/// Vulkan buffer object selected by `BufferCacheParams::Buffer`.
///
/// Backend usage tracking and API identity live here, matching upstream
/// `Vulkan::Buffer` rather than the common `BufferBase`.
pub struct Buffer {
    base: BufferBase,
    device_owner: Option<DeviceReference>,
    allocation: Option<AllocatedBuffer>,
    views: Vec<BufferView>,
    device_address: vk::DeviceAddress,
    scheduler: NonNull<Scheduler>,
    tracker: UsageTracker,
    last_usage_tick: u64,
    is_null: bool,
}

impl Buffer {
    fn null(runtime: &mut BufferCacheRuntime) -> Self {
        let allocation = if runtime.has_null_descriptor {
            None
        } else {
            Some(runtime.create_null_buffer())
        };
        let buffer = allocation
            .as_ref()
            .map_or(vk::Buffer::null(), AllocatedBuffer::handle);
        let device_address = if buffer != vk::Buffer::null()
            && runtime.vulkan_device().is_buffer_device_address_supported()
        {
            unsafe {
                runtime
                    .vulkan_device()
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
        Self {
            base: BufferBase::null(NullBufferParams),
            device_owner: (!runtime.has_null_descriptor).then_some(runtime.device_owner),
            allocation,
            views: Vec::new(),
            device_address,
            scheduler: runtime.scheduler,
            tracker: UsageTracker::new(4096),
            last_usage_tick: 0,
            is_null: !runtime.has_null_descriptor,
        }
    }

    fn new(runtime: &mut BufferCacheRuntime, cpu_addr: u64, size_bytes: u64) -> Self {
        let allocation = runtime
            .create_gpu_buffer(
                size_bytes,
                common_buffer_usage_flags(runtime.vulkan_device()),
            )
            .expect("Vulkan buffer allocation failed");
        let buffer = allocation.handle();
        let device_address = runtime.buffer_device_address(buffer);
        if runtime.vulkan_device().has_debugging_tool_attached() {
            runtime
                .vulkan_device()
                .set_buffer_name(buffer, &format!("Buffer {cpu_addr:#x}"));
        }
        Self {
            base: BufferBase::new(cpu_addr, size_bytes),
            device_owner: Some(runtime.device_owner),
            allocation: Some(allocation),
            views: Vec::new(),
            device_address,
            scheduler: runtime.scheduler,
            tracker: UsageTracker::new(size_bytes as usize),
            last_usage_tick: 0,
            is_null: false,
        }
    }

    fn handle(&self) -> vk::Buffer {
        self.allocation
            .as_ref()
            .map_or(vk::Buffer::null(), AllocatedBuffer::handle)
    }

    fn device_address(&self) -> vk::DeviceAddress {
        self.device_address
    }

    fn view(&mut self, mut offset: u32, mut size: u32, format: PixelFormat) -> vk::BufferView {
        let requested_format = format;
        let Some(device_owner) = self.device_owner else {
            return vk::BufferView::null();
        };
        if self.is_null {
            offset = 0;
            size = 0;
        }
        if let Some(view) = self
            .views
            .iter()
            .find(|view| view.offset == offset && view.size == size && view.format == format)
        {
            return view.view;
        }
        let device = device_owner.get();
        let format = super::maxwell_to_vk::surface_format(
            device,
            FormatType::Buffer,
            false,
            requested_format,
        )
        .format;
        let info = vk::BufferViewCreateInfo::builder()
            .buffer(self.handle())
            .format(format)
            .offset(offset as vk::DeviceSize)
            .range(size as vk::DeviceSize)
            .build();
        let view = unsafe {
            device
                .get_logical()
                .create_buffer_view(&info, None)
                .expect("Vulkan buffer view creation failed")
        };
        self.views.push(BufferView {
            offset,
            size,
            format: requested_format,
            view,
        });
        view
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        let Some(device_owner) = self.device_owner else {
            return;
        };
        let device = device_owner.get();
        unsafe {
            for view in self.views.drain(..).rev() {
                device.get_logical().destroy_buffer_view(view.view, None);
            }
        }
    }
}

impl Deref for Buffer {
    type Target = BufferBase;

    fn deref(&self) -> &Self::Target {
        &self.base
    }
}

impl DerefMut for Buffer {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.base
    }
}

impl BufferCacheBuffer for Buffer {
    type Runtime = BufferCacheRuntime;

    fn null(runtime: &mut Self::Runtime, _params: NullBufferParams) -> Self {
        Buffer::null(runtime)
    }

    fn new(runtime: &mut Self::Runtime, cpu_addr: u64, size_bytes: u64) -> Self {
        Buffer::new(runtime, cpu_addr, size_bytes)
    }

    fn immediate_upload(&self, _offset: u64, _data: &[u8]) {
        unreachable!("Vulkan BufferCache uses mapped uploads")
    }

    fn immediate_download(&self, _offset: u64, _data: &mut [u8]) {
        unreachable!("Vulkan BufferCache uses mapped downloads")
    }

    fn raw_handle(&self) -> u64 {
        self.handle().as_raw()
    }

    fn mark_usage(&mut self, offset: u64, size: u64) {
        self.tracker.track(offset, size);
        self.last_usage_tick = unsafe { self.scheduler.as_ref() }.current_tick();
    }

    fn is_region_used(&self, offset: u64, size: u64) -> bool {
        self.tracker.is_used(offset, size)
    }

    fn reset_usage_tracking(&mut self) {
        self.tracker.reset();
    }

    fn last_usage_tick(&self) -> u64 {
        self.last_usage_tick
    }
}

/// Port of upstream's anonymous `QuadIndexBuffer` hierarchy state.
struct QuadIndexBuffer {
    buffer: Option<AllocatedBuffer>,
    index_type: vk::IndexType,
    num_indices: u32,
}

impl Default for QuadIndexBuffer {
    fn default() -> Self {
        Self {
            buffer: None,
            index_type: vk::IndexType::UINT16,
            num_indices: 0,
        }
    }
}

fn index_type_from_num_elements(
    num_elements: u32,
    index_type_uint8_supported: bool,
) -> vk::IndexType {
    if num_elements <= 0xff && index_type_uint8_supported {
        vk::IndexType::UINT8_EXT
    } else if num_elements <= 0xffff {
        vk::IndexType::UINT16
    } else {
        vk::IndexType::UINT32
    }
}

fn bytes_per_index(index_type: vk::IndexType) -> usize {
    match index_type {
        vk::IndexType::UINT8_EXT => 1,
        vk::IndexType::UINT16 => 2,
        vk::IndexType::UINT32 => 4,
        _ => unreachable!("invalid Vulkan index type"),
    }
}

fn quad_count_for_topology(topology: PrimitiveTopology, num_indices: u32) -> u32 {
    match topology {
        PrimitiveTopology::Quads => num_indices / 4,
        PrimitiveTopology::QuadStrip => {
            if num_indices >= 4 {
                (num_indices - 2) / 2
            } else {
                0
            }
        }
        _ => unreachable!("invalid quad topology"),
    }
}

fn write_quad_index(
    bytes: &mut [u8],
    byte_offset: &mut usize,
    index_type: vk::IndexType,
    index: u32,
) {
    let encoded = match index_type {
        vk::IndexType::UINT8_EXT => [index as u8, 0, 0, 0],
        vk::IndexType::UINT16 => {
            let value = (index as u16).to_ne_bytes();
            [value[0], value[1], 0, 0]
        }
        vk::IndexType::UINT32 => index.to_ne_bytes(),
        _ => unreachable!("invalid Vulkan index type"),
    };
    let size = bytes_per_index(index_type);
    bytes[*byte_offset..*byte_offset + size].copy_from_slice(&encoded[..size]);
    *byte_offset += size;
}

fn fill_quad_lut(
    bytes: &mut [u8],
    topology: PrimitiveTopology,
    num_indices: u32,
    index_type: vk::IndexType,
) {
    let num_quads = quad_count_for_topology(topology, num_indices);
    let mut byte_offset = 0;
    for first in 0u32..4 {
        for quad in 0..num_quads {
            let offsets = match topology {
                PrimitiveTopology::Quads => [0, 1, 2, 0, 2, 3]
                    .map(|index| first.wrapping_add(index).wrapping_add(quad.wrapping_mul(4))),
                PrimitiveTopology::QuadStrip => [0, 3, 1, 0, 2, 3]
                    .map(|index| first.wrapping_add(index).wrapping_add(quad.wrapping_mul(2))),
                _ => unreachable!("invalid quad topology"),
            };
            for index in offsets {
                write_quad_index(bytes, &mut byte_offset, index_type, index);
            }
        }
    }
}

#[cfg(test)]
fn make_quad_lut(
    topology: PrimitiveTopology,
    num_indices: u32,
    index_type: vk::IndexType,
) -> Vec<u8> {
    let size = quad_count_for_topology(topology, num_indices) as usize
        * 6
        * 4
        * bytes_per_index(index_type);
    let mut bytes = vec![0; size];
    fill_quad_lut(&mut bytes, topology, num_indices, index_type);
    bytes
}

/// Buffer cache parameters matching upstream `Vulkan::BufferCacheParams`.
pub struct BufferCacheParams;

impl BufferCacheParams {
    pub const IS_OPENGL: bool = false;
    pub const HAS_PERSISTENT_UNIFORM_BUFFER_BINDINGS: bool = false;
    pub const HAS_FULL_INDEX_AND_PRIMITIVE_SUPPORT: bool = false;
    pub const NEEDS_BIND_UNIFORM_INDEX: bool = false;
    pub const NEEDS_BIND_STORAGE_INDEX: bool = false;
    pub const USE_MEMORY_MAPS: bool = true;
    pub const SEPARATE_IMAGE_BUFFER_BINDINGS: bool = false;
    pub const USE_MEMORY_MAPS_FOR_UPLOADS: bool = true;
}

impl base::BufferCacheParams for BufferCacheParams {
    type Runtime = BufferCacheRuntime;
    type Buffer = Buffer;
    type AsyncBuffer = super::staging_buffer_pool::StagingBufferRef;

    const IS_OPENGL: bool = Self::IS_OPENGL;
    const HAS_PERSISTENT_UNIFORM_BUFFER_BINDINGS: bool =
        Self::HAS_PERSISTENT_UNIFORM_BUFFER_BINDINGS;
    const HAS_FULL_INDEX_AND_PRIMITIVE_SUPPORT: bool = Self::HAS_FULL_INDEX_AND_PRIMITIVE_SUPPORT;
    const NEEDS_BIND_UNIFORM_INDEX: bool = Self::NEEDS_BIND_UNIFORM_INDEX;
    const NEEDS_BIND_STORAGE_INDEX: bool = Self::NEEDS_BIND_STORAGE_INDEX;
    const USE_MEMORY_MAPS: bool = Self::USE_MEMORY_MAPS;
    const SEPARATE_IMAGE_BUFFER_BINDINGS: bool = Self::SEPARATE_IMAGE_BUFFER_BINDINGS;
    const USE_MEMORY_MAPS_FOR_UPLOADS: bool = Self::USE_MEMORY_MAPS_FOR_UPLOADS;
}

fn common_buffer_usage_flags(device: &Device) -> vk::BufferUsageFlags {
    let mut flags = common_buffer_base_usage_flags();
    if device.is_ext_transform_feedback_supported() {
        flags |= vk::BufferUsageFlags::TRANSFORM_FEEDBACK_BUFFER_EXT;
    }
    if device.is_ext_conditional_rendering() {
        flags |= vk::BufferUsageFlags::CONDITIONAL_RENDERING_EXT;
    }
    flags
}

fn common_buffer_base_usage_flags() -> vk::BufferUsageFlags {
    vk::BufferUsageFlags::TRANSFER_SRC
        | vk::BufferUsageFlags::TRANSFER_DST
        | vk::BufferUsageFlags::UNIFORM_TEXEL_BUFFER
        | vk::BufferUsageFlags::STORAGE_TEXEL_BUFFER
        | vk::BufferUsageFlags::UNIFORM_BUFFER
        | vk::BufferUsageFlags::STORAGE_BUFFER
        | vk::BufferUsageFlags::INDEX_BUFFER
        | vk::BufferUsageFlags::VERTEX_BUFFER
        | vk::BufferUsageFlags::INDIRECT_BUFFER
}

pub type VulkanCommonBufferCache = CommonBufferCache<BufferCacheParams, MaxwellDeviceMemoryManager>;

/// Vulkan implementation of upstream `BufferCacheRuntime`.
///
/// This is the runtime service owner used by the common `BufferCache<P>` port:
/// scheduler-recorded copies/clears, staging allocation, and backend buffer
/// materialization.
pub struct BufferCacheRuntime {
    device_owner: DeviceReference,
    memory_allocator: NonNull<MemoryAllocator>,
    scheduler: NonNull<Scheduler>,
    staging_pool: NonNull<StagingBufferPool>,
    guest_descriptor_queue: NonNull<UpdateDescriptorQueue>,
    uint8_pass: Option<Uint8Pass>,
    quad_index_pass: QuadIndexedPass,
    quad_array_index_buffer: QuadIndexBuffer,
    quad_strip_index_buffer: QuadIndexBuffer,
    index_type_uint8_supported: bool,
    null_buffer: Option<AllocatedBuffer>,
    has_null_descriptor: bool,
    extended_dynamic_state_supported: bool,
    transform_feedback: Option<vk::ExtTransformFeedbackFn>,
    max_vertex_input_bindings: u32,
    uniform_buffer_alignment: u32,
    limit_dynamic_storage_buffers: bool,
    max_dynamic_storage_buffers: u32,
}

impl BufferCacheRuntime {
    pub fn new(
        vulkan_device: &Device,
        instance: ash::Instance,
        physical_device: vk::PhysicalDevice,
        memory_allocator: &mut MemoryAllocator,
        scheduler: &mut Scheduler,
        staging_pool: &mut StagingBufferPool,
        guest_descriptor_queue: &mut UpdateDescriptorQueue,
        compute_pass_descriptor_queue: &mut ComputePassDescriptorQueue,
        descriptor_pool: &DescriptorPool,
        index_type_uint8_supported: bool,
        has_null_descriptor: bool,
        extended_dynamic_state_supported: bool,
        transform_feedback_supported: bool,
        max_vertex_input_bindings: u32,
    ) -> Result<Self, vk::Result> {
        let device = vulkan_device.get_logical();
        let quad_index_pass = QuadIndexedPass::new(
            vulkan_device,
            scheduler,
            descriptor_pool,
            staging_pool,
            compute_pass_descriptor_queue,
        )?;
        let uint8_pass = if vulkan_device.supports_uint8_indices() {
            Some(Uint8Pass::new(
                vulkan_device,
                scheduler,
                descriptor_pool,
                staging_pool,
                compute_pass_descriptor_queue,
            )?)
        } else {
            None
        };
        let transform_feedback = transform_feedback_supported.then(|| {
            vk::ExtTransformFeedbackFn::load(|name| unsafe {
                std::mem::transmute(instance.get_device_proc_addr(device.handle(), name.as_ptr()))
            })
        });
        let uniform_buffer_alignment = unsafe {
            instance
                .get_physical_device_properties(physical_device)
                .limits
                .min_uniform_buffer_offset_alignment as u32
        };
        let limit_dynamic_storage_buffers = matches!(
            vulkan_device.get_driver_id(),
            vk::DriverId::QUALCOMM_PROPRIETARY | vk::DriverId::ARM_PROPRIETARY
        );
        let max_dynamic_storage_buffers = if limit_dynamic_storage_buffers {
            vulkan_device.get_max_descriptor_set_storage_buffers_dynamic()
        } else {
            u32::MAX
        };
        Ok(Self {
            device_owner: DeviceReference::new(vulkan_device),
            memory_allocator: NonNull::from(memory_allocator),
            scheduler: NonNull::from(scheduler),
            staging_pool: NonNull::from(staging_pool),
            guest_descriptor_queue: NonNull::from(guest_descriptor_queue),
            uint8_pass,
            quad_index_pass,
            quad_array_index_buffer: QuadIndexBuffer::default(),
            quad_strip_index_buffer: QuadIndexBuffer::default(),
            index_type_uint8_supported,
            null_buffer: None,
            has_null_descriptor,
            extended_dynamic_state_supported,
            transform_feedback,
            max_vertex_input_bindings,
            uniform_buffer_alignment,
            limit_dynamic_storage_buffers,
            max_dynamic_storage_buffers,
        })
    }

    /// Port of `BufferCacheRuntime::BindVertexBuffer`.
    pub fn bind_vertex_buffer(
        &mut self,
        index: u32,
        mut buffer: vk::Buffer,
        mut offset: u32,
        size: u32,
        stride: u32,
    ) {
        if index >= self.max_vertex_input_bindings {
            return;
        }
        let device = self.device_owner;
        if self.extended_dynamic_state_supported {
            self.scheduler().record(move |cmdbuf| unsafe {
                let device = device.get().get_logical();
                let vk_offset = if buffer != vk::Buffer::null() {
                    u64::from(offset)
                } else {
                    0
                };
                let vk_size = if buffer != vk::Buffer::null() {
                    u64::from(size)
                } else {
                    vk::WHOLE_SIZE
                };
                let vk_stride = u64::from(stride);
                device.cmd_bind_vertex_buffers2(
                    cmdbuf,
                    index,
                    std::slice::from_ref(&buffer),
                    std::slice::from_ref(&vk_offset),
                    Some(std::slice::from_ref(&vk_size)),
                    Some(std::slice::from_ref(&vk_stride)),
                );
            });
            return;
        }

        if !self.has_null_descriptor && buffer == vk::Buffer::null() {
            self.reserve_null_buffer();
            buffer = self.null_buffer_handle();
            offset = 0;
        }
        self.scheduler().record(move |cmdbuf| unsafe {
            let device = device.get().get_logical();
            device.cmd_bind_vertex_buffers(cmdbuf, index, &[buffer], &[u64::from(offset)]);
        });
    }

    /// Port of `BufferCacheRuntime::BindTransformFeedbackBuffer`.
    pub fn bind_transform_feedback_buffer(
        &mut self,
        index: u32,
        mut buffer: vk::Buffer,
        mut offset: u32,
        mut size: u32,
    ) {
        let Some(transform_feedback) = self.transform_feedback.clone() else {
            return;
        };
        if buffer == vk::Buffer::null() {
            self.reserve_null_buffer();
            buffer = self.null_buffer_handle();
            offset = 0;
            size = 0;
        }
        self.scheduler().record(move |command_buffer| unsafe {
            let vk_offset = u64::from(offset);
            let vk_size = u64::from(size);
            (transform_feedback.cmd_bind_transform_feedback_buffers_ext)(
                command_buffer,
                index,
                1,
                &buffer,
                &vk_offset,
                &vk_size,
            );
        });
    }

    fn scheduler(&mut self) -> &mut Scheduler {
        // SAFETY: the runtime is constructed from boxed rasterizer services.
        // Their addresses remain stable and they outlive the runtime.
        unsafe { self.scheduler.as_mut() }
    }

    fn staging_pool(&mut self) -> &mut StagingBufferPool {
        // SAFETY: see `scheduler`.
        unsafe { self.staging_pool.as_mut() }
    }

    fn guest_descriptor_queue(&mut self) -> &mut UpdateDescriptorQueue {
        // SAFETY: see `scheduler`.
        unsafe { self.guest_descriptor_queue.as_mut() }
    }

    fn vulkan_device(&self) -> &Device {
        self.device_owner.get()
    }

    fn memory_allocator(&self) -> &MemoryAllocator {
        // SAFETY: RasterizerVulkan owns the allocator for longer than the
        // buffer-cache runtime and its concrete buffers.
        unsafe { self.memory_allocator.as_ref() }
    }

    fn buffer_device_address(&self, buffer: vk::Buffer) -> vk::DeviceAddress {
        if buffer == vk::Buffer::null()
            || !self.vulkan_device().is_buffer_device_address_supported()
        {
            return 0;
        }
        unsafe {
            self.vulkan_device()
                .get_logical()
                .get_buffer_device_address(
                    &vk::BufferDeviceAddressInfo::builder()
                        .buffer(buffer)
                        .build(),
                )
        }
    }

    fn update_quad_index_buffer(&mut self, topology: PrimitiveTopology, num_indices: u32) {
        let current_num_indices = match topology {
            PrimitiveTopology::Quads => self.quad_array_index_buffer.num_indices,
            PrimitiveTopology::QuadStrip => self.quad_strip_index_buffer.num_indices,
            _ => unreachable!("invalid quad topology"),
        };
        if num_indices <= current_num_indices {
            return;
        }

        self.scheduler().finish();
        let index_type = index_type_from_num_elements(num_indices, self.index_type_uint8_supported);
        {
            let state = match topology {
                PrimitiveTopology::Quads => &mut self.quad_array_index_buffer,
                PrimitiveTopology::QuadStrip => &mut self.quad_strip_index_buffer,
                _ => unreachable!("invalid quad topology"),
            };
            state.num_indices = num_indices;
            state.index_type = index_type;
        }
        let size = u64::from(quad_count_for_topology(topology, num_indices))
            * 6
            * 4
            * bytes_per_index(index_type) as u64;
        let allocation = self
            .create_gpu_buffer(
                size,
                vk::BufferUsageFlags::INDEX_BUFFER | vk::BufferUsageFlags::TRANSFER_DST,
            )
            .expect("quad index buffer allocation failed");
        {
            let state = match topology {
                PrimitiveTopology::Quads => &mut self.quad_array_index_buffer,
                PrimitiveTopology::QuadStrip => &mut self.quad_strip_index_buffer,
                _ => unreachable!("invalid quad topology"),
            };
            state.buffer = Some(allocation);
        }
        let buffer = match topology {
            PrimitiveTopology::Quads => self.quad_array_index_buffer.buffer.as_ref(),
            PrimitiveTopology::QuadStrip => self.quad_strip_index_buffer.buffer.as_ref(),
            _ => unreachable!("invalid quad topology"),
        }
        .expect("quad index buffer was just allocated")
        .handle();
        if self.vulkan_device().has_debugging_tool_attached() {
            self.vulkan_device().set_buffer_name(buffer, "Quad LUT");
        }
        let host_visible = match topology {
            PrimitiveTopology::Quads => self.quad_array_index_buffer.buffer.as_ref(),
            PrimitiveTopology::QuadStrip => self.quad_strip_index_buffer.buffer.as_ref(),
            _ => unreachable!("invalid quad topology"),
        }
        .expect("quad index buffer was just allocated")
        .is_host_visible();
        if host_visible {
            let allocation = match topology {
                PrimitiveTopology::Quads => self.quad_array_index_buffer.buffer.as_mut(),
                PrimitiveTopology::QuadStrip => self.quad_strip_index_buffer.buffer.as_mut(),
                _ => unreachable!("invalid quad topology"),
            }
            .expect("quad index buffer was just allocated");
            fill_quad_lut(allocation.mapped_slice_mut(), topology, num_indices, index_type);
            allocation.flush();
        } else {
            let staging = self
                .staging_pool()
                .request_upload_buffer(size)
                .expect("quad index upload staging allocation failed");
            unsafe {
                fill_quad_lut(
                    std::slice::from_raw_parts_mut(staging.mapped, size as usize),
                    topology,
                    num_indices,
                    index_type,
                );
            }

            let device = self.device_owner;
            let src_buffer = staging.buffer;
            let src_offset = staging.offset;
            let dst_buffer = buffer;
            self.scheduler().request_outside_render_pass_operation_context();
            self.scheduler().record(move |cmdbuf| unsafe {
                let device = device.get().get_logical();
                let copy = vk::BufferCopy {
                    src_offset,
                    dst_offset: 0,
                    size,
                };
                let barrier = vk::BufferMemoryBarrier::builder()
                    .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                    .dst_access_mask(vk::AccessFlags::INDEX_READ)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .buffer(dst_buffer)
                    .offset(0)
                    .size(size)
                    .build();
                device.cmd_copy_buffer(cmdbuf, src_buffer, dst_buffer, &[copy]);
                device.cmd_pipeline_barrier(
                    cmdbuf,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::VERTEX_INPUT,
                    vk::DependencyFlags::empty(),
                    &[],
                    std::slice::from_ref(&barrier),
                    &[],
                );
            });
        }
    }

    /// Port of upstream `BufferCacheRuntime::ReserveNullBuffer`.
    fn reserve_null_buffer(&mut self) {
        if self.null_buffer.is_some() {
            return;
        }
        self.null_buffer = Some(self.create_null_buffer());
    }

    /// Port of upstream `BufferCacheRuntime::CreateNullBuffer`.
    fn create_null_buffer(&mut self) -> AllocatedBuffer {
        let allocation = self
            .create_gpu_buffer(
                4,
                runtime_null_buffer_usage_flags(
                    self.vulkan_device().is_ext_transform_feedback_supported(),
                ),
            )
            .expect("Vulkan null buffer allocation failed");
        if self.vulkan_device().has_debugging_tool_attached() {
            self.vulkan_device()
                .set_buffer_name(allocation.handle(), "Null buffer");
        }
        let buffer = allocation.handle();

        let device = self.device_owner;
        self.scheduler().request_outside_render_pass_operation_context();
        self.scheduler().record(move |cmdbuf| unsafe {
            let device = device.get().get_logical();
            device.cmd_fill_buffer(cmdbuf, buffer, 0, vk::WHOLE_SIZE, 0);
        });
        allocation
    }

    fn null_buffer_handle(&self) -> vk::Buffer {
        self.null_buffer
            .as_ref()
            .map_or(vk::Buffer::null(), AllocatedBuffer::handle)
    }

    fn create_gpu_buffer(
        &self,
        size: vk::DeviceSize,
        usage: vk::BufferUsageFlags,
    ) -> Result<AllocatedBuffer, crate::vulkan_common::vulkan_wrapper::VulkanError> {
        let usage = if self.vulkan_device().is_buffer_device_address_supported() {
            usage | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS
        } else {
            usage
        };
        let buffer_info = vk::BufferCreateInfo::builder()
            .size(size)
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .build();
        self.memory_allocator()
            .create_buffer(&buffer_info, MemoryUsage::DeviceLocal)
    }

    fn make_buffer_copies(copies: &[BufferCopy]) -> SmallVec<[vk::BufferCopy; 8]> {
        copies
            .iter()
            .map(|copy| vk::BufferCopy {
                src_offset: copy.src_offset,
                dst_offset: copy.dst_offset,
                size: copy.size,
            })
            .collect()
    }

    fn texel_buffer_format(&self, format: PixelFormat) -> vk::Format {
        super::maxwell_to_vk::surface_format(
            self.vulkan_device(),
            FormatType::Buffer,
            false,
            format,
        )
        .format
    }

    fn bind_buffer_descriptor(&mut self, buffer: &Buffer, offset: u32, size: u32) {
        let resolved = buffer.handle();
        let device_address = buffer.device_address();
        if resolved == vk::Buffer::null() {
            self.guest_descriptor_queue()
                .add_buffer_with_address(resolved, 0, 0, vk::WHOLE_SIZE);
        } else {
            self.guest_descriptor_queue().add_buffer_with_address(
                resolved,
                device_address,
                offset as vk::DeviceSize,
                size as vk::DeviceSize,
            );
        }
    }

    fn copy_buffer_handles(
        &mut self,
        dst_buffer: vk::Buffer,
        src_buffer: vk::Buffer,
        copies: &[BufferCopy],
        barrier: bool,
        can_reorder_upload: bool,
    ) {
        if dst_buffer == vk::Buffer::null() || src_buffer == vk::Buffer::null() {
            return;
        }
        let vk_copies = Self::make_buffer_copies(copies);
        let device = self.device_owner;
        let can_use_upload_cmdbuf =
            can_reorder_upload && src_buffer == self.staging_pool().stream_buffer_handle();
        if can_use_upload_cmdbuf {
            self.scheduler()
                .record_with_upload_buffer(move |_cmdbuf, upload_cmdbuf| unsafe {
                    let device = device.get().get_logical();
                    device.cmd_copy_buffer(upload_cmdbuf, src_buffer, dst_buffer, &vk_copies);
                });
            return;
        }
        self.scheduler().request_outside_render_pass_operation_context();
        self.scheduler().record(move |cmdbuf| {
            let device = device.get().get_logical();
            let read_barrier = vk::MemoryBarrier::builder()
                .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ | vk::AccessFlags::TRANSFER_WRITE)
                .build();
            let write_barrier = vk::MemoryBarrier::builder()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE)
                .build();
            unsafe {
                if barrier {
                    device.cmd_pipeline_barrier(
                        cmdbuf,
                        PIPELINE_STAGE_GRAPHICS_COMPUTE_TRANSFER,
                        vk::PipelineStageFlags::TRANSFER,
                        vk::DependencyFlags::empty(),
                        std::slice::from_ref(&read_barrier),
                        &[],
                        &[],
                    );
                }
                device.cmd_copy_buffer(cmdbuf, src_buffer, dst_buffer, &vk_copies);
                if barrier {
                    device.cmd_pipeline_barrier(
                        cmdbuf,
                        vk::PipelineStageFlags::TRANSFER,
                        PIPELINE_STAGE_GRAPHICS_COMPUTE,
                        vk::DependencyFlags::empty(),
                        std::slice::from_ref(&write_barrier),
                        &[],
                        &[],
                    );
                }
            }
        });
    }
}

impl base::BufferCacheRuntime for BufferCacheRuntime {
    type Buffer = Buffer;
    type AsyncBuffer = super::staging_buffer_pool::StagingBufferRef;

    fn tick_frame(&mut self, slot_buffers: &mut SlotVector<Buffer>) {
        let scheduler = unsafe { self.scheduler.as_ref() };
        for (_, buffer) in slot_buffers.iter_mut() {
            if scheduler.is_free(buffer.last_usage_tick()) {
                buffer.reset_usage_tracking();
            }
        }
    }

    fn current_tick(&self) -> u64 {
        unsafe { self.scheduler.as_ref() }.current_tick()
    }

    fn known_gpu_tick(&self) -> u64 {
        unsafe { self.scheduler.as_ref() }.known_gpu_tick()
    }

    fn wait(&mut self, tick: u64) {
        self.scheduler().wait(tick);
    }

    fn can_report_memory_usage(&self) -> bool {
        self.vulkan_device().can_report_memory_usage()
    }

    fn get_device_local_memory(&self) -> u64 {
        self.vulkan_device().get_device_local_memory()
    }

    fn get_device_memory_usage(&self) -> u64 {
        self.vulkan_device().get_device_memory_usage()
    }

    fn get_storage_buffer_alignment(&self) -> u32 {
        self.vulkan_device().get_storage_buffer_alignment() as u32
    }

    fn should_limit_dynamic_storage_buffers(&self) -> bool {
        self.limit_dynamic_storage_buffers
    }

    fn max_dynamic_storage_buffers(&self) -> u32 {
        self.max_dynamic_storage_buffers
    }

    fn finish(&mut self) {
        self.scheduler().finish();
    }

    fn upload_staging_buffer(&mut self, size: u64) -> super::staging_buffer_pool::StagingBufferRef {
        self.staging_pool()
            .request_upload_buffer(size as vk::DeviceSize)
            .expect("Vulkan upload staging allocation failed")
    }

    fn download_staging_buffer(
        &mut self,
        size: u64,
        deferred: bool,
    ) -> super::staging_buffer_pool::StagingBufferRef {
        self.staging_pool()
            .request_download_buffer(size as vk::DeviceSize, deferred)
            .expect("Vulkan download staging allocation failed")
    }

    fn free_deferred_staging_buffer(
        &mut self,
        buffer: &mut super::staging_buffer_pool::StagingBufferRef,
    ) {
        self.staging_pool().free_deferred(buffer);
    }

    fn can_reorder_upload(&self, buffer: &Buffer, copies: &[BufferCopy]) -> bool {
        if *common::settings::values()
            .disable_buffer_reorder
            .get_value()
        {
            return false;
        }
        copies
            .iter()
            .all(|copy| !buffer.is_region_used(copy.dst_offset, copy.size))
    }

    fn pre_copy_barrier(&mut self) {
        let device = self.device_owner;
        self.scheduler().request_outside_render_pass_operation_context();
        self.scheduler().record(move |cmdbuf| {
            let device = device.get().get_logical();
            let read_barrier = vk::MemoryBarrier::builder()
                .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ | vk::AccessFlags::TRANSFER_WRITE)
                .build();
            unsafe {
                device.cmd_pipeline_barrier(
                    cmdbuf,
                    PIPELINE_STAGE_GRAPHICS_COMPUTE_TRANSFER,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    std::slice::from_ref(&read_barrier),
                    &[],
                    &[],
                );
            }
        });
    }

    fn post_copy_barrier(&mut self) {
        let device = self.device_owner;
        self.scheduler().request_outside_render_pass_operation_context();
        self.scheduler().record(move |cmdbuf| {
            let device = device.get().get_logical();
            let write_barrier = vk::MemoryBarrier::builder()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE)
                .build();
            unsafe {
                device.cmd_pipeline_barrier(
                    cmdbuf,
                    vk::PipelineStageFlags::TRANSFER,
                    PIPELINE_STAGE_GRAPHICS_COMPUTE,
                    vk::DependencyFlags::empty(),
                    std::slice::from_ref(&write_barrier),
                    &[],
                    &[],
                );
            }
        });
    }

    fn copy_buffer(
        &mut self,
        dst: &Buffer,
        src: &Buffer,
        copies: &[BufferCopy],
        barrier: bool,
        can_reorder_upload: bool,
    ) {
        self.copy_buffer_handles(
            dst.handle(),
            src.handle(),
            copies,
            barrier,
            can_reorder_upload,
        );
    }

    fn copy_buffer_from_staging(
        &mut self,
        dst: &Buffer,
        src: &super::staging_buffer_pool::StagingBufferRef,
        copies: &[BufferCopy],
        barrier: bool,
        can_reorder_upload: bool,
    ) {
        self.copy_buffer_handles(
            dst.handle(),
            src.buffer,
            copies,
            barrier,
            can_reorder_upload,
        );
    }

    fn copy_buffer_to_staging(
        &mut self,
        dst: &super::staging_buffer_pool::StagingBufferRef,
        src: &Buffer,
        copies: &[BufferCopy],
        barrier: bool,
    ) {
        self.copy_buffer_handles(dst.buffer, src.handle(), copies, barrier, false);
    }

    fn clear_buffer(&mut self, buffer: &Buffer, offset: u32, size: u64, value: u32) {
        let dest_buffer = buffer.handle();
        if dest_buffer == vk::Buffer::null() {
            return;
        }
        let device = self.device_owner;
        self.scheduler().request_outside_render_pass_operation_context();
        self.scheduler().record(move |cmdbuf| {
            let device = device.get().get_logical();
            let read_barrier = vk::MemoryBarrier::builder()
                .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ | vk::AccessFlags::TRANSFER_WRITE)
                .build();
            let write_barrier = vk::MemoryBarrier::builder()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE)
                .build();
            unsafe {
                device.cmd_pipeline_barrier(
                    cmdbuf,
                    PIPELINE_STAGE_GRAPHICS_COMPUTE_TRANSFER,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    std::slice::from_ref(&read_barrier),
                    &[],
                    &[],
                );
                device.cmd_fill_buffer(cmdbuf, dest_buffer, offset as u64, size, value);
                device.cmd_pipeline_barrier(
                    cmdbuf,
                    vk::PipelineStageFlags::TRANSFER,
                    PIPELINE_STAGE_GRAPHICS_COMPUTE,
                    vk::DependencyFlags::empty(),
                    std::slice::from_ref(&write_barrier),
                    &[],
                    &[],
                );
            }
        });
    }

    fn bind_index_buffer(
        &mut self,
        topology: PrimitiveTopology,
        index_format: IndexFormat,
        base_vertex: u32,
        num_indices: u32,
        buffer: &mut Buffer,
        offset: u32,
        _size: u32,
    ) {
        let mut buffer = buffer.handle();
        let mut vk_offset = u64::from(offset);
        let mut index_type = match index_format {
            IndexFormat::UnsignedByte => vk::IndexType::UINT8_EXT,
            IndexFormat::UnsignedShort => vk::IndexType::UINT16,
            IndexFormat::UnsignedInt => vk::IndexType::UINT32,
        };
        if matches!(
            topology,
            PrimitiveTopology::Quads | PrimitiveTopology::QuadStrip
        ) {
            index_type = vk::IndexType::UINT32;
            (buffer, vk_offset) = self.quad_index_pass.assemble(
                index_format,
                num_indices,
                base_vertex,
                buffer,
                offset,
                topology == PrimitiveTopology::QuadStrip,
            );
        } else if index_type == vk::IndexType::UINT8_EXT && !self.index_type_uint8_supported {
            index_type = vk::IndexType::UINT16;
            if let Some(uint8_pass) = &mut self.uint8_pass {
                (buffer, vk_offset) = uint8_pass.assemble(num_indices, buffer, offset);
            } else if self.vulkan_device().get_driver_id() == vk::DriverId::QUALCOMM_PROPRIETARY {
                self.reserve_null_buffer();
                buffer = self.null_buffer_handle();
                vk_offset = 0;
            }
        }
        if buffer == vk::Buffer::null() {
            self.reserve_null_buffer();
            buffer = self.null_buffer_handle();
        }
        let device = self.device_owner;
        self.scheduler().record(move |cmdbuf| unsafe {
            let device = device.get().get_logical();
            device.cmd_bind_index_buffer(cmdbuf, buffer, vk_offset, index_type);
        });
    }

    fn bind_quad_index_buffer(&mut self, topology: PrimitiveTopology, first: u32, count: u32) {
        if count == 0 {
            self.reserve_null_buffer();
            let buffer = self.null_buffer_handle();
            let device = self.device_owner;
            self.scheduler().record(move |cmdbuf| unsafe {
                let device = device.get().get_logical();
                device.cmd_bind_index_buffer(cmdbuf, buffer, 0, vk::IndexType::UINT32);
            });
            return;
        }

        if !matches!(
            topology,
            PrimitiveTopology::Quads | PrimitiveTopology::QuadStrip
        ) {
            return;
        }

        self.update_quad_index_buffer(topology, first.wrapping_add(count));
        let state = match topology {
            PrimitiveTopology::Quads => &self.quad_array_index_buffer,
            PrimitiveTopology::QuadStrip => &self.quad_strip_index_buffer,
            _ => return,
        };
        let sub_first_offset =
            u64::from(first % 4) * u64::from(quad_count_for_topology(topology, state.num_indices));
        let offset = (sub_first_offset + u64::from(quad_count_for_topology(topology, first)))
            * 6
            * bytes_per_index(state.index_type) as u64;
        let buffer = state
            .buffer
            .as_ref()
            .map_or(vk::Buffer::null(), AllocatedBuffer::handle);
        let index_type = state.index_type;
        let device = self.device_owner;
        self.scheduler().record(move |cmdbuf| unsafe {
            let device = device.get().get_logical();
            device.cmd_bind_index_buffer(cmdbuf, buffer, offset, index_type);
        });
    }

    fn bind_vertex_buffer(
        &mut self,
        index: u32,
        buffer: &mut Buffer,
        offset: u32,
        size: u32,
        stride: u32,
    ) {
        BufferCacheRuntime::bind_vertex_buffer(self, index, buffer.handle(), offset, size, stride);
    }

    fn bind_vertex_buffers(
        &mut self,
        bindings: &HostBindings,
        buffers: &mut common::slot_vector::SlotVector<Buffer>,
    ) {
        let binding_count = vertex_binding_count(
            bindings.min_index,
            bindings.max_index,
            self.max_vertex_input_bindings,
        ) as usize;
        let mut vk_buffers = SmallVec::<[vk::Buffer; 32]>::new();
        let mut offsets = SmallVec::<[u64; 32]>::new();
        let mut sizes = SmallVec::<[u64; 32]>::new();
        let mut strides = SmallVec::<[u64; 32]>::new();
        for index in 0..bindings.buffer_ids.len() {
            let buffer_id = bindings.buffer_ids[index];
            let buffer = if buffer_id.is_valid() {
                buffers[buffer_id].handle()
            } else {
                vk::Buffer::null()
            };
            if buffer == vk::Buffer::null() && !self.has_null_descriptor {
                self.reserve_null_buffer();
            }
            let null_buffer = self.null_buffer_handle();
            let (buffer, offset, size) = prepare_vertex_binding(
                buffer,
                bindings.offsets[index],
                bindings.sizes[index],
                self.has_null_descriptor,
                null_buffer,
            );
            vk_buffers.push(buffer);
            offsets.push(offset);
            sizes.push(size);
            strides.push(bindings.strides[index]);
        }
        if binding_count == 0 {
            return;
        }
        vk_buffers.truncate(binding_count);
        offsets.truncate(binding_count);
        sizes.truncate(binding_count);
        strides.truncate(binding_count);
        let first_binding = bindings.min_index;
        let dynamic_stride = self.extended_dynamic_state_supported;
        let device = self.device_owner;
        self.scheduler().record(move |cmdbuf| unsafe {
            let device = device.get().get_logical();
            if dynamic_stride {
                device.cmd_bind_vertex_buffers2(
                    cmdbuf,
                    first_binding,
                    &vk_buffers,
                    &offsets,
                    Some(&sizes),
                    Some(&strides),
                );
            } else {
                device.cmd_bind_vertex_buffers(cmdbuf, first_binding, &vk_buffers, &offsets);
            }
        });
    }

    fn bind_uniform_buffer(
        &mut self,
        _stage: usize,
        _binding_index: u32,
        buffer: &mut Buffer,
        offset: u32,
        size: u32,
    ) {
        self.bind_buffer_descriptor(buffer, offset, size);
    }

    fn bind_storage_buffer(
        &mut self,
        _stage: usize,
        _binding_index: u32,
        buffer: &mut Buffer,
        offset: u32,
        size: u32,
        _is_written: bool,
    ) {
        self.bind_buffer_descriptor(buffer, offset, size);
    }

    fn bind_texture_buffer(
        &mut self,
        buffer: &mut Buffer,
        offset: u32,
        size: u32,
        format: PixelFormat,
    ) {
        let view = buffer.view(offset, size, format);
        let device_address = buffer.device_address();
        let vk_format = self.texel_buffer_format(format);
        self.guest_descriptor_queue().add_texel_buffer_with_address(
            view,
            device_address,
            offset as u64,
            size as u64,
            vk_format,
        );
    }

    fn bind_image_buffer(
        &mut self,
        buffer: &mut Buffer,
        offset: u32,
        size: u32,
        format: PixelFormat,
    ) {
        let view = buffer.view(offset, size, format);
        let device_address = buffer.device_address();
        let vk_format = self.texel_buffer_format(format);
        self.guest_descriptor_queue().add_texel_buffer_with_address(
            view,
            device_address,
            offset as u64,
            size as u64,
            vk_format,
        );
    }

    fn bind_transform_feedback_buffers(
        &mut self,
        bindings: &HostBindings,
        buffers: &mut SlotVector<Buffer>,
    ) {
        let Some(transform_feedback) = self.transform_feedback.clone() else {
            return;
        };
        if bindings
            .buffer_ids
            .iter()
            .any(|&buffer_id| buffers[buffer_id].handle() == vk::Buffer::null())
        {
            self.reserve_null_buffer();
        }
        let null_buffer = self.null_buffer_handle();
        let mut buffer_handles = Vec::with_capacity(bindings.buffer_ids.len());
        let mut offsets: Vec<vk::DeviceSize> = bindings.offsets.iter().copied().collect();
        let mut sizes: Vec<vk::DeviceSize> = bindings.sizes.iter().copied().collect();
        for (index, &buffer_id) in bindings.buffer_ids.iter().enumerate() {
            let (buffer, offset, size) = prepare_transform_feedback_binding(
                buffers[buffer_id].handle(),
                offsets[index],
                sizes[index],
                null_buffer,
            );
            buffer_handles.push(buffer);
            offsets[index] = offset;
            sizes[index] = size;
        }
        self.scheduler().record(move |command_buffer| unsafe {
            (transform_feedback.cmd_bind_transform_feedback_buffers_ext)(
                command_buffer,
                0,
                buffer_handles.len() as u32,
                buffer_handles.as_ptr(),
                offsets.as_ptr(),
                sizes.as_ptr(),
            );
        });
    }

    fn bind_compute_uniform_buffer(
        &mut self,
        _binding_index: u32,
        buffer: &mut Buffer,
        offset: u32,
        size: u32,
    ) {
        self.bind_buffer_descriptor(buffer, offset, size);
    }

    fn bind_compute_storage_buffer(
        &mut self,
        _binding_index: u32,
        buffer: &mut Buffer,
        offset: u32,
        size: u32,
        _is_written: bool,
    ) {
        // Vulkan shares the same descriptor-buffer path as graphics storage
        // buffers (only NEEDS_BIND_STORAGE_INDEX backends route here at all).
        self.bind_buffer_descriptor(buffer, offset, size);
    }

    fn uniform_buffer_alignment(&self) -> u32 {
        self.uniform_buffer_alignment
    }

    fn with_mapped_uniform_buffer(
        &mut self,
        _stage: usize,
        _binding_index: u32,
        size: u32,
        write: &mut dyn FnMut(&mut [u8]),
    ) -> bool {
        let staging = self
            .staging_pool()
            .request_upload_buffer(size as vk::DeviceSize);
        let Some(staging) = staging else {
            return false;
        };
        unsafe {
            let span = std::slice::from_raw_parts_mut(staging.mapped, size as usize);
            write(span);
        }
        self.guest_descriptor_queue().add_buffer_with_address(
            staging.buffer,
            staging.device_address,
            staging.offset,
            size as vk::DeviceSize,
        );
        true
    }
}

fn runtime_null_buffer_usage_flags(transform_feedback_supported: bool) -> vk::BufferUsageFlags {
    let mut flags = vk::BufferUsageFlags::VERTEX_BUFFER
        | vk::BufferUsageFlags::INDEX_BUFFER
        | vk::BufferUsageFlags::TRANSFER_DST
        | vk::BufferUsageFlags::INDIRECT_BUFFER;
    if transform_feedback_supported {
        flags |= vk::BufferUsageFlags::TRANSFORM_FEEDBACK_BUFFER_EXT;
    }
    flags
}

fn vertex_binding_count(min_index: u32, max_index: u32, device_max: u32) -> u32 {
    let min_binding = min_index.min(device_max);
    let max_binding = max_index.min(device_max);
    max_binding.wrapping_sub(min_binding)
}

/// Port of the null-handle branch in upstream
/// `BufferCacheRuntime::BindVertexBuffers`.
fn prepare_vertex_binding(
    buffer: vk::Buffer,
    offset: vk::DeviceSize,
    size: vk::DeviceSize,
    has_null_descriptor: bool,
    null_buffer: vk::Buffer,
) -> (vk::Buffer, vk::DeviceSize, vk::DeviceSize) {
    if buffer != vk::Buffer::null() {
        return (buffer, offset, size);
    }
    if has_null_descriptor {
        return (vk::Buffer::null(), 0, vk::WHOLE_SIZE);
    }
    (null_buffer, 0, vk::WHOLE_SIZE)
}

/// Port of the null-handle branch in upstream
/// `BufferCacheRuntime::BindTransformFeedbackBuffers`.
fn prepare_transform_feedback_binding(
    buffer: vk::Buffer,
    offset: vk::DeviceSize,
    size: vk::DeviceSize,
    null_buffer: vk::Buffer,
) -> (vk::Buffer, vk::DeviceSize, vk::DeviceSize) {
    if buffer == vk::Buffer::null() {
        (null_buffer, 0, 0)
    } else {
        (buffer, offset, size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_keeps_upstream_device_reference() {
        fn device_reference(runtime: &BufferCacheRuntime) -> DeviceReference {
            runtime.device_owner
        }
        fn require_signature(_: fn(&BufferCacheRuntime) -> DeviceReference) {}

        require_signature(device_reference);
    }

    #[test]
    fn runtime_exposes_upstream_single_buffer_binding_methods() {
        fn require_vertex_signature(
            _: fn(&mut BufferCacheRuntime, u32, vk::Buffer, u32, u32, u32),
        ) {
        }
        fn require_transform_feedback_signature(
            _: fn(&mut BufferCacheRuntime, u32, vk::Buffer, u32, u32),
        ) {
        }

        require_vertex_signature(BufferCacheRuntime::bind_vertex_buffer);
        require_transform_feedback_signature(BufferCacheRuntime::bind_transform_feedback_buffer);
    }

    #[test]
    fn copy_regions_keep_upstream_inline_capacity() {
        let copies = [BufferCopy::default(); 8];
        let vk_copies = BufferCacheRuntime::make_buffer_copies(&copies);
        assert_eq!(vk_copies.len(), 8);
        assert!(!vk_copies.spilled());
    }

    #[test]
    fn vertex_binding_count_is_capped_to_the_device_limit() {
        assert_eq!(vertex_binding_count(0, 32, 16), 16);
        assert_eq!(vertex_binding_count(12, 20, 16), 4);
        assert_eq!(vertex_binding_count(16, 32, 16), 0);
    }

    #[test]
    fn runtime_null_buffer_base_usage_matches_upstream() {
        assert_eq!(
            runtime_null_buffer_usage_flags(false),
            vk::BufferUsageFlags::VERTEX_BUFFER
                | vk::BufferUsageFlags::INDEX_BUFFER
                | vk::BufferUsageFlags::TRANSFER_DST
                | vk::BufferUsageFlags::INDIRECT_BUFFER
        );
        assert!(runtime_null_buffer_usage_flags(true)
            .contains(vk::BufferUsageFlags::TRANSFORM_FEEDBACK_BUFFER_EXT));
    }

    #[test]
    fn null_vertex_binding_preserves_upstream_null_descriptor_path() {
        let fallback = vk::Buffer::from_raw(0x1234);
        assert_eq!(
            prepare_vertex_binding(vk::Buffer::null(), 91, 73, true, fallback),
            (vk::Buffer::null(), 0, vk::WHOLE_SIZE)
        );
    }

    #[test]
    fn null_vertex_binding_fallback_preserves_upstream_whole_size() {
        let fallback = vk::Buffer::from_raw(0x1234);
        assert_eq!(
            prepare_vertex_binding(vk::Buffer::null(), 91, 73, false, fallback),
            (fallback, 0, vk::WHOLE_SIZE)
        );
    }

    #[test]
    fn null_transform_feedback_binding_uses_zero_sized_fallback() {
        let fallback = vk::Buffer::from_raw(0x1234);
        assert_eq!(
            prepare_transform_feedback_binding(vk::Buffer::null(), 91, 73, fallback),
            (fallback, 0, 0)
        );
    }

    #[test]
    fn transform_feedback_binding_preserves_non_null_range() {
        let buffer = vk::Buffer::from_raw(0x5678);
        assert_eq!(
            prepare_transform_feedback_binding(buffer, 91, 73, vk::Buffer::null()),
            (buffer, 91, 73)
        );
    }

    #[test]
    fn buffer_cache_params_match_upstream_vulkan() {
        assert!(!BufferCacheParams::IS_OPENGL);
        assert!(!BufferCacheParams::HAS_PERSISTENT_UNIFORM_BUFFER_BINDINGS);
        assert!(!BufferCacheParams::HAS_FULL_INDEX_AND_PRIMITIVE_SUPPORT);
        assert!(!BufferCacheParams::NEEDS_BIND_UNIFORM_INDEX);
        assert!(!BufferCacheParams::NEEDS_BIND_STORAGE_INDEX);
        assert!(BufferCacheParams::USE_MEMORY_MAPS);
        assert!(!BufferCacheParams::SEPARATE_IMAGE_BUFFER_BINDINGS);
        assert!(BufferCacheParams::USE_MEMORY_MAPS_FOR_UPLOADS);
    }

    #[test]
    fn common_buffers_support_texel_buffer_views() {
        let usage = common_buffer_base_usage_flags();
        assert!(usage.contains(vk::BufferUsageFlags::UNIFORM_TEXEL_BUFFER));
        assert!(usage.contains(vk::BufferUsageFlags::STORAGE_TEXEL_BUFFER));
    }

    #[test]
    fn quad_lut_matches_upstream_swizzles() {
        let quads = make_quad_lut(PrimitiveTopology::Quads, 4, vk::IndexType::UINT8_EXT);
        assert_eq!(&quads[..6], &[0, 1, 2, 0, 2, 3]);
        assert_eq!(&quads[6..12], &[1, 2, 3, 1, 3, 4]);

        let strip = make_quad_lut(PrimitiveTopology::QuadStrip, 4, vk::IndexType::UINT8_EXT);
        assert_eq!(&strip[..6], &[0, 3, 1, 0, 2, 3]);
        assert_eq!(&strip[6..12], &[1, 4, 2, 1, 3, 4]);
    }

    #[test]
    fn quad_lut_index_type_uses_upstream_boundaries() {
        assert_eq!(
            index_type_from_num_elements(0xff, true),
            vk::IndexType::UINT8_EXT
        );
        assert_eq!(
            index_type_from_num_elements(0x100, true),
            vk::IndexType::UINT16
        );
        assert_eq!(
            index_type_from_num_elements(0xff, false),
            vk::IndexType::UINT16
        );
        assert_eq!(
            index_type_from_num_elements(0xffff, true),
            vk::IndexType::UINT16
        );
        assert_eq!(
            index_type_from_num_elements(0x1_0000, true),
            vk::IndexType::UINT32
        );
    }
}
