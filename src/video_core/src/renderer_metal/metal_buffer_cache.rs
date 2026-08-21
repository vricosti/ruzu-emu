// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native Metal counterpart of Eden's `renderer_vulkan/vk_buffer_cache.{h,cpp}`.

use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use common::slot_vector::SlotVector;
use objc2_metal::MTLIndexType;

use crate::buffer_cache::buffer_base::{BufferBase, NullBufferParams};
use crate::buffer_cache::buffer_cache::BufferCache as CommonBufferCache;
use crate::buffer_cache::buffer_cache_base::{
    self as base, BufferCacheAsyncBuffer, BufferCacheBuffer, BufferCopy, HostBindings,
};
use crate::buffer_cache::usage_tracker::UsageTracker;
use crate::engines::maxwell_3d::{IndexFormat, PrimitiveTopology};
use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
use crate::surface::PixelFormat;

use super::metal_buffer::MetalBuffer;
use super::metal_device::MetalDevice;
use super::metal_scheduler::MetalScheduler;
use super::metal_staging_buffer_pool::{MetalStagingBufferPool, StagingBufferRef};

#[derive(Clone)]
pub struct MetalBufferBinding {
    pub buffer: Arc<MetalBuffer>,
    pub offset: usize,
    pub size: usize,
    pub is_written: bool,
}

#[derive(Clone)]
pub struct MetalVertexBinding {
    pub buffer: Arc<MetalBuffer>,
    pub offset: usize,
    pub size: usize,
    pub stride: usize,
}

#[derive(Clone)]
pub struct MetalTexelBufferBinding {
    pub buffer: Arc<MetalBuffer>,
    pub offset: usize,
    pub size: usize,
    pub format: PixelFormat,
}

#[derive(Clone)]
pub struct MetalIndexBinding {
    pub buffer: Arc<MetalBuffer>,
    pub offset: usize,
    pub index_type: MTLIndexType,
}

#[derive(Default)]
pub struct MetalGraphicsBufferBindings {
    pub uniform_buffers: [Vec<MetalBufferBinding>; base::NUM_STAGES as usize],
    pub storage_buffers: [Vec<MetalBufferBinding>; base::NUM_STAGES as usize],
    pub texture_buffers: Vec<MetalTexelBufferBinding>,
    pub image_buffers: Vec<MetalTexelBufferBinding>,
}

#[derive(Default)]
pub struct MetalComputeBufferBindings {
    pub uniform_buffers: Vec<MetalBufferBinding>,
    pub storage_buffers: Vec<MetalBufferBinding>,
    pub texture_buffers: Vec<MetalTexelBufferBinding>,
    pub image_buffers: Vec<MetalTexelBufferBinding>,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum BindingTarget {
    #[default]
    Graphics,
    Compute,
}

/// Metal buffer object selected by the common cache specialization.
pub struct Buffer {
    base: BufferBase,
    allocation: Arc<MetalBuffer>,
    scheduler: NonNull<MetalScheduler>,
    tracker: UsageTracker,
    last_usage_tick: u64,
    allocation_bytes: u64,
    allocated_bytes: Arc<AtomicU64>,
}

impl Buffer {
    fn null(runtime: &mut BufferCacheRuntime) -> Self {
        Self::allocate(runtime, BufferBase::null(NullBufferParams), 4)
    }

    fn new(runtime: &mut BufferCacheRuntime, cpu_addr: u64, size_bytes: u64) -> Self {
        Self::allocate(
            runtime,
            BufferBase::new(cpu_addr, size_bytes),
            size_bytes.max(4),
        )
    }

    fn allocate(runtime: &mut BufferCacheRuntime, base: BufferBase, size: u64) -> Self {
        let allocation = Arc::new(
            MetalBuffer::new(&runtime.device, size as usize)
                .expect("Metal buffer-cache allocation failed"),
        );
        runtime.allocated_bytes.fetch_add(size, Ordering::Relaxed);
        Self {
            base,
            allocation,
            scheduler: runtime.scheduler,
            tracker: UsageTracker::new(size.max(4096) as usize),
            last_usage_tick: 0,
            allocation_bytes: size,
            allocated_bytes: Arc::clone(&runtime.allocated_bytes),
        }
    }

    pub fn handle(&self) -> Arc<MetalBuffer> {
        Arc::clone(&self.allocation)
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        self.allocated_bytes
            .fetch_sub(self.allocation_bytes, Ordering::Relaxed);
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
        Self::null(runtime)
    }

    fn new(runtime: &mut Self::Runtime, cpu_addr: u64, size_bytes: u64) -> Self {
        Self::new(runtime, cpu_addr, size_bytes)
    }

    fn immediate_upload(&self, offset: u64, data: &[u8]) {
        self.allocation
            .write(offset as usize, data)
            .expect("Metal immediate buffer upload exceeded its allocation");
    }

    fn immediate_download(&self, offset: u64, data: &mut [u8]) {
        self.allocation
            .read(offset as usize, data)
            .expect("Metal immediate buffer download exceeded its allocation");
    }

    fn raw_handle(&self) -> u64 {
        self.allocation.raw_handle()
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

pub struct BufferCacheParams;

impl base::BufferCacheParams for BufferCacheParams {
    type Runtime = BufferCacheRuntime;
    type Buffer = Buffer;
    type AsyncBuffer = StagingBufferRef;

    const IS_OPENGL: bool = false;
    const HAS_PERSISTENT_UNIFORM_BUFFER_BINDINGS: bool = false;
    const HAS_FULL_INDEX_AND_PRIMITIVE_SUPPORT: bool = false;
    const NEEDS_BIND_UNIFORM_INDEX: bool = false;
    const NEEDS_BIND_STORAGE_INDEX: bool = false;
    const USE_MEMORY_MAPS: bool = true;
    const SEPARATE_IMAGE_BUFFER_BINDINGS: bool = true;
    const USE_MEMORY_MAPS_FOR_UPLOADS: bool = true;
}

pub type MetalCommonBufferCache = CommonBufferCache<BufferCacheParams, MaxwellDeviceMemoryManager>;

/// Backend service owner corresponding to Eden's `Vulkan::BufferCacheRuntime`.
pub struct BufferCacheRuntime {
    device: MetalDevice,
    scheduler: NonNull<MetalScheduler>,
    staging_pool: NonNull<MetalStagingBufferPool>,
    allocated_bytes: Arc<AtomicU64>,
    null_buffer: Arc<MetalBuffer>,
    index_binding: Option<MetalIndexBinding>,
    vertex_bindings: Vec<Option<MetalVertexBinding>>,
    transform_feedback_bindings: Vec<MetalBufferBinding>,
    binding_target: BindingTarget,
    graphics: MetalGraphicsBufferBindings,
    compute: MetalComputeBufferBindings,
    quad_array_index_buffer: Option<(Arc<MetalBuffer>, u32)>,
    quad_strip_index_buffer: Option<(Arc<MetalBuffer>, u32)>,
}

impl BufferCacheRuntime {
    pub fn new(
        device: &MetalDevice,
        scheduler: &mut MetalScheduler,
        staging_pool: &mut MetalStagingBufferPool,
    ) -> Self {
        Self {
            device: device.clone(),
            scheduler: NonNull::from(scheduler),
            staging_pool: NonNull::from(staging_pool),
            allocated_bytes: Arc::new(AtomicU64::new(0)),
            null_buffer: Arc::new(MetalBuffer::new(device, 4).expect("Metal null buffer")),
            index_binding: None,
            vertex_bindings: vec![None; base::NUM_VERTEX_BUFFERS as usize],
            transform_feedback_bindings: Vec::new(),
            binding_target: BindingTarget::Graphics,
            graphics: MetalGraphicsBufferBindings::default(),
            compute: MetalComputeBufferBindings::default(),
            quad_array_index_buffer: None,
            quad_strip_index_buffer: None,
        }
    }

    fn scheduler(&mut self) -> &mut MetalScheduler {
        unsafe { self.scheduler.as_mut() }
    }

    fn staging_pool(&mut self) -> &mut MetalStagingBufferPool {
        unsafe { self.staging_pool.as_mut() }
    }

    pub fn begin_graphics_bindings(&mut self) {
        self.binding_target = BindingTarget::Graphics;
        self.graphics = MetalGraphicsBufferBindings::default();
        self.vertex_bindings.fill(None);
        self.transform_feedback_bindings.clear();
    }

    pub fn begin_compute_bindings(&mut self) {
        self.binding_target = BindingTarget::Compute;
        self.compute = MetalComputeBufferBindings::default();
    }

    pub fn index_binding(&self) -> Option<&MetalIndexBinding> {
        self.index_binding.as_ref()
    }

    pub fn vertex_bindings(&self) -> &[Option<MetalVertexBinding>] {
        &self.vertex_bindings
    }

    pub fn graphics_bindings(&self) -> &MetalGraphicsBufferBindings {
        &self.graphics
    }

    pub fn compute_bindings(&self) -> &MetalComputeBufferBindings {
        &self.compute
    }

    fn bind_buffer(
        buffer: &Buffer,
        offset: u32,
        size: u32,
        is_written: bool,
    ) -> MetalBufferBinding {
        MetalBufferBinding {
            buffer: buffer.handle(),
            offset: offset as usize,
            size: size as usize,
            is_written,
        }
    }

    fn bind_texel(
        buffer: &Buffer,
        offset: u32,
        size: u32,
        format: PixelFormat,
    ) -> MetalTexelBufferBinding {
        MetalTexelBufferBinding {
            buffer: buffer.handle(),
            offset: offset as usize,
            size: size as usize,
            format,
        }
    }

    fn encode_copies(&mut self, dst: &MetalBuffer, src: &MetalBuffer, copies: &[BufferCopy]) {
        self.scheduler()
            .request_outside_render_pass_operation_context();
        for copy in copies {
            src.encode_copy(
                self.scheduler(),
                dst,
                copy.src_offset as usize,
                copy.dst_offset as usize,
                copy.size as usize,
            )
            .expect("Metal buffer-cache copy failed");
        }
    }

    fn converted_index_buffer(
        &mut self,
        source: &Buffer,
        source_offset: u32,
        index_format: IndexFormat,
        topology: PrimitiveTopology,
        base_vertex: u32,
        num_indices: u32,
    ) -> Arc<MetalBuffer> {
        // The source is shared storage but may have prior GPU writers. Waiting
        // before the CPU conversion preserves Eden's compute-pass ordering.
        self.scheduler()
            .finish_all()
            .expect("Metal index source synchronization failed");
        let element_size = index_format_size(index_format);
        let mut input = vec![0; num_indices as usize * element_size];
        source
            .allocation
            .read(source_offset as usize, &mut input)
            .expect("Metal index source range");
        let swizzle = if topology == PrimitiveTopology::QuadStrip {
            [0usize, 3, 1, 0, 2, 3]
        } else {
            [0usize, 1, 2, 0, 2, 3]
        };
        let primitives = quad_count_for_topology(topology, num_indices);
        let mut output = Vec::with_capacity(primitives as usize * 6 * 4);
        for primitive in 0..primitives as usize {
            for vertex in swizzle {
                let source_index = if topology == PrimitiveTopology::QuadStrip {
                    primitive * 2 + vertex
                } else {
                    primitive * 4 + vertex
                };
                let value =
                    read_index(&input, index_format, source_index).wrapping_add(base_vertex);
                output.extend_from_slice(&value.to_ne_bytes());
            }
        }
        let result = Arc::new(
            MetalBuffer::new(&self.device, output.len().max(4))
                .expect("Metal converted index allocation"),
        );
        result.write(0, &output).expect("Metal converted indices");
        result
    }

    fn uint8_index_buffer(
        &mut self,
        source: &Buffer,
        source_offset: u32,
        num_indices: u32,
    ) -> Arc<MetalBuffer> {
        self.scheduler()
            .finish_all()
            .expect("Metal uint8 index source synchronization failed");
        let mut input = vec![0; num_indices as usize];
        source
            .allocation
            .read(source_offset as usize, &mut input)
            .expect("Metal uint8 index source range");
        let mut output = Vec::with_capacity(input.len() * 2);
        for index in input {
            let index = if index == u8::MAX {
                u16::MAX
            } else {
                index as u16
            };
            output.extend_from_slice(&index.to_ne_bytes());
        }
        let result = Arc::new(
            MetalBuffer::new(&self.device, output.len().max(4))
                .expect("Metal uint8 index allocation"),
        );
        result.write(0, &output).expect("Metal uint8 indices");
        result
    }

    fn update_quad_lut(&mut self, topology: PrimitiveTopology, num_indices: u32) {
        let current = match topology {
            PrimitiveTopology::Quads => &self.quad_array_index_buffer,
            PrimitiveTopology::QuadStrip => &self.quad_strip_index_buffer,
            _ => return,
        };
        if current
            .as_ref()
            .is_some_and(|(_, count)| *count >= num_indices)
        {
            return;
        }
        let data = make_quad_lut(topology, num_indices);
        let buffer = Arc::new(
            MetalBuffer::new(&self.device, data.len().max(4)).expect("Metal quad LUT allocation"),
        );
        buffer.write(0, &data).expect("Metal quad LUT upload");
        match topology {
            PrimitiveTopology::Quads => self.quad_array_index_buffer = Some((buffer, num_indices)),
            PrimitiveTopology::QuadStrip => {
                self.quad_strip_index_buffer = Some((buffer, num_indices))
            }
            _ => {}
        }
    }
}

impl base::BufferCacheRuntime for BufferCacheRuntime {
    type Buffer = Buffer;
    type AsyncBuffer = StagingBufferRef;

    fn tick_frame(&mut self, slot_buffers: &mut SlotVector<Buffer>) {
        let scheduler_ptr = self.scheduler;
        let staging_ptr = self.staging_pool;
        unsafe { staging_ptr.as_ptr().as_mut().unwrap() }
            .tick_frame(unsafe { scheduler_ptr.as_ptr().as_mut().unwrap() })
            .expect("Metal staging frame tick failed");
        let known = unsafe { scheduler_ptr.as_ref() }.completed_tick();
        for (_, buffer) in slot_buffers.iter_mut() {
            if buffer.last_usage_tick() <= known {
                buffer.reset_usage_tracking();
            }
        }
    }

    fn can_report_memory_usage(&self) -> bool {
        false
    }

    fn get_device_local_memory(&self) -> u64 {
        self.device.profile().recommended_resource_budget()
    }

    fn get_device_memory_usage(&self) -> u64 {
        self.allocated_bytes.load(Ordering::Relaxed)
    }

    fn get_storage_buffer_alignment(&self) -> u32 {
        16
    }

    fn finish(&mut self) {
        self.scheduler()
            .finish_all()
            .expect("Metal scheduler finish failed");
    }

    fn current_tick(&self) -> u64 {
        unsafe { self.scheduler.as_ref() }.current_tick()
    }

    fn known_gpu_tick(&self) -> u64 {
        unsafe { self.scheduler.as_ref() }.completed_tick()
    }

    fn wait(&mut self, tick: u64) {
        self.scheduler().wait(tick).expect("Metal tick wait failed");
    }

    fn upload_staging_buffer(&mut self, size: u64) -> StagingBufferRef {
        let scheduler_ptr = self.scheduler;
        self.staging_pool()
            .request_upload_buffer(
                unsafe { scheduler_ptr.as_ptr().as_mut().unwrap() },
                size as usize,
                false,
            )
            .expect("Metal upload staging allocation failed")
    }

    fn download_staging_buffer(&mut self, size: u64, deferred: bool) -> StagingBufferRef {
        let scheduler_ptr = self.scheduler;
        self.staging_pool()
            .request_download_buffer(
                unsafe { scheduler_ptr.as_ptr().as_mut().unwrap() },
                size as usize,
                deferred,
            )
            .expect("Metal download staging allocation failed")
    }

    fn free_deferred_staging_buffer(&mut self, buffer: &mut StagingBufferRef) {
        let scheduler_ptr = self.scheduler;
        self.staging_pool()
            .free_deferred(unsafe { scheduler_ptr.as_ref() }, buffer)
            .expect("Metal deferred staging release failed");
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
        // A single Metal command queue plus tracked resources establishes the
        // same write-to-blit dependency as Eden's Vulkan memory barrier.
        self.scheduler()
            .request_outside_render_pass_operation_context();
    }

    fn post_copy_barrier(&mut self) {
        self.scheduler()
            .request_outside_render_pass_operation_context();
    }

    fn copy_buffer(
        &mut self,
        dst: &Buffer,
        src: &Buffer,
        copies: &[BufferCopy],
        _barrier: bool,
        _can_reorder_upload: bool,
    ) {
        self.encode_copies(&dst.allocation, &src.allocation, copies);
    }

    fn copy_buffer_from_staging(
        &mut self,
        dst: &Buffer,
        src: &StagingBufferRef,
        copies: &[BufferCopy],
        _barrier: bool,
        _can_reorder_upload: bool,
    ) {
        self.encode_copies(&dst.allocation, &src.buffer, copies);
    }

    fn copy_buffer_to_staging(
        &mut self,
        dst: &StagingBufferRef,
        src: &Buffer,
        copies: &[BufferCopy],
        _barrier: bool,
    ) {
        self.encode_copies(&dst.buffer, &src.allocation, copies);
    }

    fn clear_buffer(&mut self, buffer: &Buffer, offset: u32, size: u64, value: u32) {
        let mut staging = self.upload_staging_buffer(size);
        for chunk in staging.mapped_span_mut().chunks_mut(4) {
            chunk.copy_from_slice(&value.to_ne_bytes()[..chunk.len()]);
        }
        let copies = [BufferCopy {
            src_offset: staging.offset(),
            dst_offset: offset as u64,
            size,
        }];
        self.copy_buffer_from_staging(buffer, &staging, &copies, true, false);
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
        let (buffer, index_type) = if matches!(
            topology,
            PrimitiveTopology::Quads | PrimitiveTopology::QuadStrip
        ) {
            (
                self.converted_index_buffer(
                    buffer,
                    offset,
                    index_format,
                    topology,
                    base_vertex,
                    num_indices,
                ),
                MTLIndexType::UInt32,
            )
        } else if index_format == IndexFormat::UnsignedByte {
            (
                self.uint8_index_buffer(buffer, offset, num_indices),
                MTLIndexType::UInt16,
            )
        } else {
            (buffer.handle(), metal_index_type(index_format))
        };
        self.index_binding = Some(MetalIndexBinding {
            buffer,
            offset: if matches!(
                topology,
                PrimitiveTopology::Quads | PrimitiveTopology::QuadStrip
            ) || index_format == IndexFormat::UnsignedByte
            {
                0
            } else {
                offset as usize
            },
            index_type,
        });
    }

    fn bind_quad_index_buffer(&mut self, topology: PrimitiveTopology, first: u32, count: u32) {
        if count == 0 {
            self.index_binding = Some(MetalIndexBinding {
                buffer: Arc::clone(&self.null_buffer),
                offset: 0,
                index_type: MTLIndexType::UInt32,
            });
            return;
        }
        self.update_quad_lut(topology, first.wrapping_add(count));
        let (buffer, num_indices) = match topology {
            PrimitiveTopology::Quads => self.quad_array_index_buffer.as_ref().unwrap(),
            PrimitiveTopology::QuadStrip => self.quad_strip_index_buffer.as_ref().unwrap(),
            _ => return,
        };
        let sub_first_offset =
            u64::from(first % 4) * u64::from(quad_count_for_topology(topology, *num_indices));
        let offset =
            (sub_first_offset + u64::from(quad_count_for_topology(topology, first))) * 6 * 4;
        self.index_binding = Some(MetalIndexBinding {
            buffer: Arc::clone(buffer),
            offset: offset as usize,
            index_type: MTLIndexType::UInt32,
        });
    }

    fn bind_vertex_buffers(&mut self, bindings: &HostBindings, buffers: &mut SlotVector<Buffer>) {
        for (slot, buffer_id) in bindings.buffer_ids.iter().enumerate() {
            let target = bindings.min_index as usize + slot;
            if target >= self.vertex_bindings.len() {
                break;
            }
            self.vertex_bindings[target] = if buffer_id.is_valid() {
                Some(MetalVertexBinding {
                    buffer: buffers[*buffer_id].handle(),
                    offset: bindings.offsets[slot] as usize,
                    size: bindings.sizes[slot] as usize,
                    stride: bindings.strides[slot] as usize,
                })
            } else {
                Some(MetalVertexBinding {
                    buffer: Arc::clone(&self.null_buffer),
                    offset: 0,
                    size: 4,
                    stride: 0,
                })
            };
        }
    }

    fn bind_uniform_buffer(
        &mut self,
        stage: usize,
        _binding_index: u32,
        buffer: &mut Buffer,
        offset: u32,
        size: u32,
    ) {
        self.graphics.uniform_buffers[stage].push(Self::bind_buffer(buffer, offset, size, false));
    }

    fn bind_storage_buffer(
        &mut self,
        stage: usize,
        _binding_index: u32,
        buffer: &mut Buffer,
        offset: u32,
        size: u32,
        is_written: bool,
    ) {
        self.graphics.storage_buffers[stage]
            .push(Self::bind_buffer(buffer, offset, size, is_written));
    }

    fn bind_texture_buffer(
        &mut self,
        buffer: &mut Buffer,
        offset: u32,
        size: u32,
        format: PixelFormat,
    ) {
        let binding = Self::bind_texel(buffer, offset, size, format);
        match self.binding_target {
            BindingTarget::Graphics => self.graphics.texture_buffers.push(binding),
            BindingTarget::Compute => self.compute.texture_buffers.push(binding),
        }
    }

    fn bind_image_buffer(
        &mut self,
        buffer: &mut Buffer,
        offset: u32,
        size: u32,
        format: PixelFormat,
    ) {
        let binding = Self::bind_texel(buffer, offset, size, format);
        match self.binding_target {
            BindingTarget::Graphics => self.graphics.image_buffers.push(binding),
            BindingTarget::Compute => self.compute.image_buffers.push(binding),
        }
    }

    fn bind_transform_feedback_buffers(
        &mut self,
        bindings: &HostBindings,
        buffers: &mut SlotVector<Buffer>,
    ) {
        self.transform_feedback_bindings = bindings
            .buffer_ids
            .iter()
            .enumerate()
            .map(|(index, id)| {
                if id.is_valid() {
                    MetalBufferBinding {
                        buffer: buffers[*id].handle(),
                        offset: bindings.offsets[index] as usize,
                        size: bindings.sizes[index] as usize,
                        is_written: true,
                    }
                } else {
                    MetalBufferBinding {
                        buffer: Arc::clone(&self.null_buffer),
                        offset: 0,
                        size: 4,
                        is_written: true,
                    }
                }
            })
            .collect();
    }

    fn bind_compute_uniform_buffer(
        &mut self,
        _binding_index: u32,
        buffer: &mut Buffer,
        offset: u32,
        size: u32,
    ) {
        self.compute
            .uniform_buffers
            .push(Self::bind_buffer(buffer, offset, size, false));
    }

    fn bind_compute_storage_buffer(
        &mut self,
        _binding_index: u32,
        buffer: &mut Buffer,
        offset: u32,
        size: u32,
        is_written: bool,
    ) {
        self.compute
            .storage_buffers
            .push(Self::bind_buffer(buffer, offset, size, is_written));
    }

    fn uniform_buffer_alignment(&self) -> u32 {
        16
    }

    fn with_mapped_uniform_buffer(
        &mut self,
        stage: usize,
        _binding_index: u32,
        size: u32,
        write: &mut dyn FnMut(&mut [u8]),
    ) -> bool {
        let mut staging = self.upload_staging_buffer(size as u64);
        write(staging.mapped_span_mut());
        self.graphics.uniform_buffers[stage].push(MetalBufferBinding {
            buffer: Arc::clone(&staging.buffer),
            offset: staging.offset,
            size: size as usize,
            is_written: false,
        });
        true
    }
}

fn metal_index_type(format: IndexFormat) -> MTLIndexType {
    match format {
        IndexFormat::UnsignedByte | IndexFormat::UnsignedShort => MTLIndexType::UInt16,
        IndexFormat::UnsignedInt => MTLIndexType::UInt32,
    }
}

fn index_format_size(format: IndexFormat) -> usize {
    match format {
        IndexFormat::UnsignedByte => 1,
        IndexFormat::UnsignedShort => 2,
        IndexFormat::UnsignedInt => 4,
    }
}

fn read_index(input: &[u8], format: IndexFormat, index: usize) -> u32 {
    let offset = index * index_format_size(format);
    match format {
        IndexFormat::UnsignedByte => input[offset] as u32,
        IndexFormat::UnsignedShort => {
            u16::from_ne_bytes(input[offset..offset + 2].try_into().unwrap()) as u32
        }
        IndexFormat::UnsignedInt => {
            u32::from_ne_bytes(input[offset..offset + 4].try_into().unwrap())
        }
    }
}

fn quad_count_for_topology(topology: PrimitiveTopology, num_indices: u32) -> u32 {
    match topology {
        PrimitiveTopology::Quads => num_indices / 4,
        PrimitiveTopology::QuadStrip => num_indices.saturating_sub(2) / 2,
        _ => 0,
    }
}

fn make_quad_lut(topology: PrimitiveTopology, num_indices: u32) -> Vec<u8> {
    let num_quads = quad_count_for_topology(topology, num_indices);
    let mut output = Vec::with_capacity(num_quads as usize * 6 * 4 * 4);
    for first in 0u32..4 {
        for quad in 0..num_quads {
            let indices =
                match topology {
                    PrimitiveTopology::Quads => [0, 1, 2, 0, 2, 3]
                        .map(|index| first.wrapping_add(index).wrapping_add(quad * 4)),
                    PrimitiveTopology::QuadStrip => [0, 3, 1, 0, 2, 3]
                        .map(|index| first.wrapping_add(index).wrapping_add(quad * 2)),
                    _ => unreachable!("invalid quad topology"),
                };
            for index in indices {
                output.extend_from_slice(&index.to_ne_bytes());
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime() -> (
        MetalDevice,
        Box<MetalScheduler>,
        Box<MetalStagingBufferPool>,
        BufferCacheRuntime,
    ) {
        let device = MetalDevice::new().expect("Metal device");
        let mut scheduler = Box::new(MetalScheduler::new(&device));
        let mut staging_pool =
            Box::new(MetalStagingBufferPool::new(&device).expect("Metal staging pool"));
        let runtime = BufferCacheRuntime::new(&device, &mut scheduler, &mut staging_pool);
        (device, scheduler, staging_pool, runtime)
    }

    #[test]
    fn metal_policy_matches_non_gl_upstream_cache_contract() {
        use crate::buffer_cache::buffer_cache_base::BufferCacheParams as _;
        assert!(!BufferCacheParams::IS_OPENGL);
        assert!(!BufferCacheParams::HAS_FULL_INDEX_AND_PRIMITIVE_SUPPORT);
        assert!(BufferCacheParams::USE_MEMORY_MAPS);
        assert!(BufferCacheParams::USE_MEMORY_MAPS_FOR_UPLOADS);
        assert!(BufferCacheParams::SEPARATE_IMAGE_BUFFER_BINDINGS);
    }

    #[test]
    fn uint8_restart_expands_to_uint16_restart() {
        let input = [0, 7, 0xff];
        let expanded = input.map(|index| {
            if index == 0xff {
                u16::MAX
            } else {
                index as u16
            }
        });
        assert_eq!(expanded, [0, 7, 0xffff]);
    }

    #[test]
    fn quad_lut_matches_eden_swizzles() {
        let quad = make_quad_lut(PrimitiveTopology::Quads, 4);
        let values = quad
            .chunks_exact(4)
            .map(|bytes| u32::from_ne_bytes(bytes.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(&values[..6], &[0, 1, 2, 0, 2, 3]);
        assert_eq!(&values[6..12], &[1, 2, 3, 1, 3, 4]);

        let strip = make_quad_lut(PrimitiveTopology::QuadStrip, 4);
        let values = strip
            .chunks_exact(4)
            .map(|bytes| u32::from_ne_bytes(bytes.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(&values[..6], &[0, 3, 1, 0, 2, 3]);
    }

    #[test]
    fn runtime_copies_and_clears_in_scheduler_order() {
        use crate::buffer_cache::buffer_cache_base::BufferCacheRuntime as _;

        let (_device, _scheduler, _staging_pool, mut runtime) = runtime();
        let source = Buffer::new(&mut runtime, 0x1000, 32);
        let destination = Buffer::new(&mut runtime, 0x2000, 32);
        source.immediate_upload(0, &[1, 2, 3, 4, 5, 6, 7, 8]);
        runtime.copy_buffer(
            &destination,
            &source,
            &[BufferCopy {
                src_offset: 2,
                dst_offset: 8,
                size: 4,
            }],
            true,
            false,
        );
        runtime.clear_buffer(&destination, 16, 8, 0x4433_2211);
        runtime.finish();

        let mut copied = [0; 4];
        destination.immediate_download(8, &mut copied);
        assert_eq!(copied, [3, 4, 5, 6]);
        let mut cleared = [0; 8];
        destination.immediate_download(16, &mut cleared);
        assert_eq!(cleared, [0x11, 0x22, 0x33, 0x44, 0x11, 0x22, 0x33, 0x44]);
    }

    #[test]
    fn runtime_expands_uint8_restart_for_metal() {
        use crate::buffer_cache::buffer_cache_base::BufferCacheRuntime as _;

        let (_device, _scheduler, _staging_pool, mut runtime) = runtime();
        let mut source = Buffer::new(&mut runtime, 0x1000, 4);
        source.immediate_upload(0, &[1, 0xff, 9, 3]);
        runtime.bind_index_buffer(
            PrimitiveTopology::Triangles,
            IndexFormat::UnsignedByte,
            0,
            4,
            &mut source,
            0,
            4,
        );
        let binding = runtime.index_binding().unwrap();
        assert_eq!(binding.index_type, MTLIndexType::UInt16);
        let mut bytes = [0; 8];
        binding.buffer.read(0, &mut bytes).unwrap();
        assert_eq!(
            bytes
                .chunks_exact(2)
                .map(|value| u16::from_ne_bytes(value.try_into().unwrap()))
                .collect::<Vec<_>>(),
            [1, 0xffff, 9, 3]
        );
    }
}
