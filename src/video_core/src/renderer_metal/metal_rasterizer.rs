// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native Metal rasterizer ownership.
//!
//! This file is the Metal counterpart of Eden's
//! `renderer_vulkan/vk_rasterizer.{h,cpp}`. It owns one scheduler, one staging
//! pool, and the common buffer/texture/shader caches used by every channel.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Arc;

use objc2_metal::{
    MTLComputeCommandEncoder, MTLComputePipelineState, MTLCullMode, MTLPrimitiveType,
    MTLRenderCommandEncoder, MTLScissorRect, MTLSize, MTLViewport, MTLWinding,
};
use thiserror::Error;

use crate::buffer_cache::buffer_cache_base::{
    DeviceMemoryAccess, DrawIndirectParams as CacheDrawIndirectParams, GpuMemoryAccess,
    ObtainBufferOperation, ObtainBufferSynchronize,
};
use crate::cache_types::CacheType;
use crate::control::channel_state::ChannelState;
use crate::control::channel_state_cache::{ChannelCacheAccessor, ChannelInfo, ChannelSetupCaches};
use crate::engines::draw_manager::{
    IndirectParams, Maxwell3DClearView, Maxwell3DDrawTextureView, Maxwell3DDrawView,
    Maxwell3DIndirectView,
};
use crate::engines::kepler_compute::DispatchCall;
use crate::engines::maxwell_3d::{CullFace, FrontFace, PrimitiveTopology, ViewportSwizzle, NUM_VIEWPORTS};
use crate::engines::maxwell_dma::{dma, AccelerateDMAInterface};
use crate::fence_manager::FenceBase;
use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
use crate::host1x::syncpoint_manager::SyncpointManager;
use crate::memory_manager::MemoryManager;
use crate::query_cache::types::QueryPropertiesFlags;
use crate::rasterizer_interface::{RasterizerDownloadArea, RasterizerInterface};
use crate::renderer_base::{
    GpuTickCallback, GpuTicksGetter, GuestMemoryWriter, InvalidateGpuCacheCallback,
};
use crate::shader_cache::ShaderCache;

use super::metal_blit_helper::{
    MetalBlitError, MetalBlitHelper, MetalBlitRegion, MetalClearColorType, MetalClearParameters,
};
use super::metal_buffer::MetalBufferError;
use super::metal_buffer_cache::{BufferCacheRuntime, MetalCommonBufferCache};
use super::metal_compute_pipeline::{
    bind_compute_resources, configure_compute_resources, MetalComputePipelineError,
};
use super::metal_compute_pass::{
    ConditionalArgumentLayout, ConditionalDirectArguments, ConditionalRenderingArgumentsPass, MetalComputePassError,
};
use super::metal_device::MetalDevice;
use super::metal_fence_manager::{MetalFence, MetalFenceManager};
use super::metal_framebuffer::{MetalFramebufferClear, MetalFramebufferError};
use super::metal_geometry_pipeline::{
    bind_vertex_resources, MetalGeometryPipelineError,
};
use super::metal_graphics_pipeline::{
    configure_graphics_resources, MetalGraphicsPipelineError, MetalPreparedGraphics,
    MetalPreparedStage,
};
use super::metal_pipeline_cache::MetalPipelineCache;
use super::metal_pipeline_cache::MetalPipelineError;
use super::metal_primitive_assembler::{
    MetalPrimitiveAssembler, MetalPrimitiveAssemblyError, MetalPrimitiveAssemblyParams,
};
use super::metal_query_cache::{MetalQueryCache, MetalQueryCacheError, MetalQueryReport, QueryCacheRuntime};
use super::metal_scheduler::{MetalScheduler, MetalSchedulerError};
use super::metal_staging_buffer_pool::{MetalStagingBufferError, MetalStagingBufferPool, StagingBufferRef};
use super::metal_state_tracker::MetalStateTracker;
use super::metal_texture_cache::{MetalTextureCache, MetalTextureCacheError};
use super::metal_tessellation_pipeline::MetalTessellationPipelineError;

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
    TextureCache(#[from] MetalTextureCacheError),
    #[error(transparent)]
    ComputePass(#[from] MetalComputePassError),
    #[error(transparent)]
    Geometry(#[from] MetalGeometryPipelineError),
    #[error(transparent)]
    Tessellation(#[from] MetalTessellationPipelineError),
    #[error(transparent)]
    Assembly(#[from] MetalPrimitiveAssemblyError),
    #[error("native geometry indirect input requires GPU draw-argument expansion")]
    GeometryIndirectInput,
    #[error("native tessellation indirect input requires GPU draw-argument expansion")]
    TessellationIndirectInput,
    #[error("geometry input references unbound vertex buffer {0}")]
    GeometryVertexBuffer(usize),
    #[error(transparent)]
    Buffer(#[from] MetalBufferError),
    #[error(transparent)]
    Staging(#[from] MetalStagingBufferError),
    #[error(transparent)]
    Scheduler(#[from] MetalSchedulerError),
    #[error(transparent)]
    Pipeline(#[from] MetalPipelineError),
    #[error(transparent)]
    GraphicsPipeline(#[from] MetalGraphicsPipelineError),
    #[error(transparent)]
    ComputePipeline(#[from] MetalComputePipelineError),
    #[error(transparent)]
    QueryCache(#[from] MetalQueryCacheError),
    #[error(transparent)]
    Framebuffer(#[from] MetalFramebufferError),
    #[error(transparent)]
    Blit(#[from] MetalBlitError),
    #[error("Metal does not support Maxwell primitive topology {0:?}")]
    UnsupportedTopology(PrimitiveTopology),
    #[error("Metal compute workgroup {requested:?} exceeds the native limit {maximum:?}")]
    UnsupportedComputeWorkgroup {
        requested: [u32; 3],
        maximum: (usize, usize, usize),
    },
}

#[derive(Clone, Copy)]
struct DrawParams {
    base_instance: u32,
    num_instances: u32,
    base_vertex: i32,
    num_vertices: u32,
    first_index: u32,
    is_indexed: bool,
}

struct MetalIndirectBinding {
    params: IndirectParams,
    buffer: Arc<super::metal_buffer::MetalBuffer>,
    offset: usize,
    draw_count: u32,
}

struct MetalConditionalDrawArguments {
    buffer: Arc<super::metal_buffer::MetalBuffer>,
    offset: usize,
    layout: ConditionalArgumentLayout,
    count: u32,
}

fn make_draw_params(draw: &Maxwell3DDrawView<'_>, instance_count: u32) -> DrawParams {
    let state = draw.draw_state();
    let is_indexed = draw.is_indexed();
    let mut params = DrawParams {
        base_instance: state.base_instance,
        num_instances: instance_count,
        base_vertex: if is_indexed {
            state.base_index as i32
        } else {
            state.vertex_buffer.first as i32
        },
        num_vertices: if is_indexed {
            state.index_buffer.count
        } else {
            state.vertex_buffer.count
        },
        first_index: if is_indexed {
            state.index_buffer.first
        } else {
            0
        },
        is_indexed,
    };
    match state.topology {
        PrimitiveTopology::Quads => {
            params.num_vertices = params.num_vertices / 4 * 6;
            params.base_vertex = 0;
            params.is_indexed = true;
        }
        PrimitiveTopology::QuadStrip => {
            params.num_vertices = params.num_vertices.wrapping_sub(2) / 2 * 6;
            params.base_vertex = 0;
            params.is_indexed = true;
        }
        _ => {}
    }
    params
}

fn metal_primitive_type(
    topology: PrimitiveTopology,
) -> Result<MTLPrimitiveType, MetalRasterizerError> {
    match topology {
        PrimitiveTopology::Points => Ok(MTLPrimitiveType::Point),
        PrimitiveTopology::Lines => Ok(MTLPrimitiveType::Line),
        PrimitiveTopology::LineStrip | PrimitiveTopology::LineLoop => {
            Ok(MTLPrimitiveType::LineStrip)
        }
        PrimitiveTopology::Triangles | PrimitiveTopology::TriangleFan
        | PrimitiveTopology::Quads | PrimitiveTopology::QuadStrip => {
            Ok(MTLPrimitiveType::Triangle)
        }
        PrimitiveTopology::TriangleStrip => Ok(MTLPrimitiveType::TriangleStrip),
        _ => Err(MetalRasterizerError::UnsupportedTopology(topology)),
    }
}

fn bind_stage(
    encoder: &objc2::runtime::ProtocolObject<dyn MTLRenderCommandEncoder>,
    stage: &MetalPreparedStage,
    vertex: bool,
) {
    unsafe {
        for binding in &stage.buffers {
            if vertex {
                encoder.setVertexBuffer_offset_atIndex(
                    Some(binding.buffer.handle()),
                    binding.offset,
                    binding.index as usize,
                );
            } else {
                encoder.setFragmentBuffer_offset_atIndex(
                    Some(binding.buffer.handle()),
                    binding.offset,
                    binding.index as usize,
                );
            }
        }
        for binding in &stage.textures {
            if vertex {
                encoder
                    .setVertexTexture_atIndex(binding.texture.as_deref(), binding.index as usize);
            } else {
                encoder
                    .setFragmentTexture_atIndex(binding.texture.as_deref(), binding.index as usize);
            }
        }
        for binding in stage.samplers.iter().filter(|_| !stage.samplers_in_argument_buffer) {
            if vertex {
                encoder
                    .setVertexSamplerState_atIndex(Some(&binding.sampler), binding.index as usize);
            } else {
                encoder.setFragmentSamplerState_atIndex(
                    Some(&binding.sampler),
                    binding.index as usize,
                );
            }
        }
        if let Some((index, bytes)) = &stage.push_constants {
            let pointer = NonNull::new(bytes.as_ptr() as *mut c_void).unwrap();
            if vertex {
                encoder.setVertexBytes_length_atIndex(pointer, bytes.len(), *index as usize);
            } else {
                encoder.setFragmentBytes_length_atIndex(pointer, bytes.len(), *index as usize);
            }
        }
    }
}

struct AccelerateDMA {
    buffer_cache: NonNull<MetalCommonBufferCache>,
}

impl AccelerateDMA {
    fn new(buffer_cache: &mut MetalCommonBufferCache) -> Self {
        Self {
            buffer_cache: NonNull::from(buffer_cache),
        }
    }
}

impl AccelerateDMAInterface for AccelerateDMA {
    fn buffer_copy(&mut self, src_address: u64, dest_address: u64, amount: u64) -> bool {
        unsafe {
            let cache = self.buffer_cache.as_mut();
            let mutex: *const _ = &cache.mutex;
            let _guard = (*mutex).lock();
            cache.dma_copy(src_address, dest_address, amount)
        }
    }

    fn buffer_clear(&mut self, dst_address: u64, amount: u64, value: u32) -> bool {
        unsafe {
            let cache = self.buffer_cache.as_mut();
            let mutex: *const _ = &cache.mutex;
            let _guard = (*mutex).lock();
            cache.dma_clear(dst_address, amount, value)
        }
    }

    fn image_to_buffer(
        &mut self,
        _copy_info: &dma::ImageCopy,
        _src: &dma::ImageOperand,
        _dst: &dma::BufferOperand,
    ) -> bool {
        false
    }

    fn buffer_to_image(
        &mut self,
        _copy_info: &dma::ImageCopy,
        _src: &dma::BufferOperand,
        _dst: &dma::ImageOperand,
    ) -> bool {
        false
    }
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

impl crate::query_cache::query_cache::GpuAddressTranslator for GpuMemoryAccessAdapter {
    fn gpu_to_cpu_address(&self, address: u64) -> Option<u64> {
        self.memory_manager.lock().gpu_to_cpu_address(address)
    }
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
    // Owns the storage referenced by cache runtimes through stable pointers.
    _staging_pool: Box<MetalStagingBufferPool>,
    pipeline_cache: MetalPipelineCache,
    primitive_assembler: Option<MetalPrimitiveAssembler>,
    shader_cache: ShaderCache,
    common_buffer_cache: Box<MetalCommonBufferCache>,
    texture_cache: Box<MetalTextureCache>,
    query_cache: MetalQueryCache,
    query_cache_runtime: QueryCacheRuntime,
    conditional_arguments_pass: ConditionalRenderingArgumentsPass,
    conditional_direct_arguments: ConditionalDirectArguments,
    state_tracker: MetalStateTracker,
    fence_manager: MetalFenceManager,
    blit_image: Box<MetalBlitHelper>,
    accelerate_dma: AccelerateDMA,
    syncpoints: Arc<SyncpointManager>,
    channel_caches: ChannelSetupCaches<ChannelInfo>,
    channel_memory_manager: Option<Arc<parking_lot::Mutex<MemoryManager>>>,
    guest_memory_writer: Option<GuestMemoryWriter>,
    gpu_ticks_getter: Option<GpuTicksGetter>,
    gpu_tick_callback: Option<GpuTickCallback>,
    invalidate_gpu_cache_callback: Option<InvalidateGpuCacheCallback>,
}

impl MetalRasterizer {
    pub fn new(
        device: MetalDevice,
        syncpoints: Arc<SyncpointManager>,
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

        let mut blit_image = Box::new(MetalBlitHelper::new(&device)?);
        let texture_cache = Box::new(MetalTextureCache::new(
            device.clone(),
            Arc::clone(&device_memory),
            scheduler.as_mut(),
            staging_pool.as_mut(),
            blit_image.as_mut(),
        ));
        let shader_cache = ShaderCache::new(device_memory);
        let pipeline_cache = MetalPipelineCache::new(device.clone());
        let query_cache = MetalQueryCache::new(&device)?;
        // Stable boxed services outlive all runtime calls; only the GPU thread
        // records through this owner, never the report/fence callbacks.
        let query_cache_runtime = unsafe {
            QueryCacheRuntime::new(&device, scheduler.as_mut(), staging_pool.as_mut())?
        };
        let conditional_arguments_pass = ConditionalRenderingArgumentsPass::new(&device)?;
        let state_tracker = MetalStateTracker::new();
        let fence_manager = MetalFenceManager::new(false);
        let accelerate_dma = AccelerateDMA::new(common_buffer_cache.as_mut());

        Ok(Self {
            device,
            scheduler,
            _staging_pool: staging_pool,
            pipeline_cache,
            primitive_assembler: None,
            shader_cache,
            common_buffer_cache,
            texture_cache,
            query_cache,
            query_cache_runtime,
            conditional_arguments_pass,
            conditional_direct_arguments: ConditionalDirectArguments::default(),
            state_tracker,
            fence_manager,
            blit_image,
            accelerate_dma,
            syncpoints,
            channel_caches: ChannelSetupCaches::new(),
            channel_memory_manager: None,
            guest_memory_writer: None,
            gpu_ticks_getter: None,
            gpu_tick_callback: None,
            invalidate_gpu_cache_callback: None,
        })
    }

    pub fn set_guest_memory_writer(&mut self, writer: GuestMemoryWriter) {
        self.texture_cache
            .base
            .set_guest_memory_writer(Arc::clone(&writer));
        self.guest_memory_writer = Some(writer);
    }

    pub fn set_gpu_ticks_getter(&mut self, getter: GpuTicksGetter) {
        self.gpu_ticks_getter = Some(getter);
    }

    pub fn set_gpu_tick_callback(&mut self, callback: GpuTickCallback) {
        self.gpu_tick_callback = Some(callback);
    }

    pub fn set_invalidate_gpu_cache_callback(&mut self, callback: InvalidateGpuCacheCallback) {
        self.invalidate_gpu_cache_callback = Some(callback);
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
        self.state_tracker.setup_tables(channel);
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
        self.state_tracker.change_channel(channel);
        self.state_tracker.invalidate_state(channel);
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
        if self.channel_caches.maxwell3d.is_none() {
            self.query_cache_runtime.end_host_conditional_rendering();
        }
        let buffer_mutex: *const _ = &self.common_buffer_cache.mutex;
        let texture_mutex: *const _ = &self.texture_cache.base.mutex;
        lock_two_reentrant_mutexes!(buffer_mutex, texture_mutex, _buffer_guard, _texture_guard);
        self.texture_cache.erase_channel(channel_id);
        self.common_buffer_cache.erase_channel(channel_id);
        self.shader_cache.erase_channel(channel_id);
        self.state_tracker.release_channel(channel_id);
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

    /// Port of Eden `RasterizerVulkan::Draw`/`PrepareDraw` to a native Metal
    /// render encoder. Cache preparation remains ordered exactly like the
    /// upstream path; only the final API bindings differ.
    pub fn draw(
        &mut self,
        draw: &mut Maxwell3DDrawView<'_>,
        instance_count: u32,
    ) -> Result<(), MetalRasterizerError> {
        self.draw_impl(draw, instance_count, None)
    }

    fn draw_impl(
        &mut self,
        draw: &mut Maxwell3DDrawView<'_>,
        instance_count: u32,
        indirect_params: Option<IndirectParams>,
    ) -> Result<(), MetalRasterizerError> {
        if let Some(memory_manager) = self.channel_memory_manager.as_ref() {
            memory_manager.lock().flush_caching();
        }

        let Some(stages) = self
            .pipeline_cache
            .current_graphics_shaders(draw, &mut self.shader_cache)?
        else {
            return Ok(());
        };
        if stages.geometry().is_some() && indirect_params.is_some() {
            return Err(MetalRasterizerError::GeometryIndirectInput);
        }
        if stages.tessellation().is_some() && indirect_params.is_some() {
            return Err(MetalRasterizerError::TessellationIndirectInput);
        }
        // Indirect fan inputs need GPU argument expansion before input assembly.
        // Do not feed their original fan indices to a native triangle-list draw.
        if draw.draw_state().topology == PrimitiveTopology::TriangleFan && indirect_params.is_some() {
            return Err(MetalRasterizerError::UnsupportedTopology(PrimitiveTopology::TriangleFan));
        }

        let buffer_mutex: *const _ = &self.common_buffer_cache.mutex;
        let texture_mutex: *const _ = &self.texture_cache.base.mutex;
        lock_two_reentrant_mutexes!(buffer_mutex, texture_mutex, _buffer_guard, _texture_guard);

        let memory_manager = self.channel_memory_manager.as_ref().cloned();
        let mut prepared = configure_graphics_resources(
            &self.device,
            &stages,
            draw,
            self.common_buffer_cache.as_mut(),
            self.texture_cache.as_mut(),
            |address, output| {
                if let Some(memory_manager) = memory_manager.as_ref() {
                    memory_manager.lock().read_block(address, output);
                } else {
                    output.fill(0);
                }
            },
        )?;

        let indirect_binding = if let Some(params) = indirect_params {
            let (buffer_id, offset) = self.common_buffer_cache.get_draw_indirect_buffer();
            let Some(buffer) = self
                .common_buffer_cache
                .backend_buffer(buffer_id)
                .map(|buffer| buffer.handle())
            else {
                log::warn!("Metal indirect draw skipped: missing indirect buffer");
                return Ok(());
            };
            let draw_count = if params.include_count {
                let (count_buffer_id, count_offset) =
                    self.common_buffer_cache.get_draw_indirect_count();
                let Some(count_buffer) = self
                    .common_buffer_cache
                    .backend_buffer(count_buffer_id)
                    .map(|buffer| buffer.handle())
                else {
                    log::warn!("Metal indirect draw skipped: missing count buffer");
                    return Ok(());
                };
                // Metal has no indirect-count render command. Synchronize the
                // count producer, then emit the exact number of native
                // indirect draws, preserving the guest command semantics.
                self.scheduler.finish_all()?;
                let mut bytes = [0; 4];
                count_buffer.read(count_offset as usize, &mut bytes)?;
                u32::from_ne_bytes(bytes).min(params.max_draw_counts as u32)
            } else {
                params.max_draw_counts as u32
            };
            Some(MetalIndirectBinding {
                params,
                buffer,
                offset: offset as usize,
                draw_count,
            })
        } else {
            None
        };

        let render_targets = draw.render_targets();
        let dirty_flags = *draw.dirty_flags();
        self.texture_cache
            .base
            .update_render_targets_from_snapshot_with_dirty_flags(
                &render_targets,
                &dirty_flags,
                |address, size| {
                    memory_manager
                        .as_ref()
                        .and_then(|manager| manager.lock().gpu_to_cpu_address_range(address, size))
                },
            );

        // Eden performs this after UpdateRenderTargets and before configuring
        // the draw. Defer the native boundary decision until effective write
        // state and independent sampled views are known.
        let mut feedback_requested = false;
        self.texture_cache
            .base
            .check_feedback_loop(&prepared.image_views, || {
                feedback_requested = true;
            });

        let visibility_query = self
            .query_cache
            .prepare_draw(self.scheduler.as_mut(), draw.zpass_pixel_count_enabled())?;
        let visibility_result_buffer = visibility_query
            .map(|_| self.query_cache.visibility_result_buffer_identity())
            .unwrap_or(0);

        let (render_pass, render_pass_key, render_area, pipeline_key) = {
            let framebuffer = self.texture_cache.base.get_framebuffer()?;
            let render_area = framebuffer.render_area();
            let pipeline_key = self
                .pipeline_cache
                .make_render_pipeline_key(&stages, framebuffer)?;
            (
                framebuffer.render_pass_descriptor(),
                framebuffer.render_pass_key(visibility_result_buffer),
                render_area,
                pipeline_key,
            )
        };
        if visibility_query.is_some() {
            self.query_cache.attach_render_pass(&render_pass);
        }

        patch_render_area(&mut prepared, &stages, render_area);
        let geometry_pipeline = stages
            .geometry()
            .map(|geometry| {
                self.pipeline_cache.get_or_create_geometry_pipeline(
                    pipeline_key,
                    geometry,
                    stages.fragment(),
                )
            })
            .transpose()?;
        let tessellation_pipeline = stages.tessellation().map(|tessellation| {
            self.pipeline_cache.get_or_create_tessellation_pipeline(
                pipeline_key, tessellation, stages.fragment(),
            )
        }).transpose()?;
        let pipeline_state = if let Some(geometry) = &geometry_pipeline {
            geometry.state.clone()
        } else if let Some(tessellation) = &tessellation_pipeline {
            tessellation.retained_state()
        } else {
            self.pipeline_cache
                .get_or_create_render_pipeline(
                    pipeline_key,
                    stages
                        .vertex()
                        .ok_or(MetalPipelineError::MissingVertexStage)?,
                    stages.fragment(),
                )?
                .retained_state()
        };
        let depth_key = self
            .pipeline_cache
            .make_depth_stencil_key(&stages, &draw.depth_stencil());
        if feedback_requested && !prepared.snapshot_read_only_depth_feedback(
            &mut self.texture_cache, depth_key.may_write_depth_stencil(),
        )? {
            self.scheduler.end_render_pass();
        }
        self.scheduler.profile_depth_feedback(
            &render_pass,
            feedback_requested,
            &depth_key,
            [&prepared.vertex, &prepared.control, &prepared.evaluation, &prepared.geometry, &prepared.fragment]
                .into_iter()
                .enumerate()
                .flat_map(|(index, stage)| stage.textures.iter().map(move |binding| (index + 1, binding)))
                .filter_map(|(stage, binding)| binding.texture.as_deref()
                    .map(|texture| (stage, binding.index, stages.key().unique_hashes[stage], texture))),
        );
        let depth_state = self
            .pipeline_cache
            .retained_depth_stencil_state(depth_key)?;
        let primitive_type = if geometry_pipeline.is_some() || tessellation_pipeline.is_some() {
            None
        } else {
            Some(metal_primitive_type(draw.draw_state().topology)?)
        };
        let mut draw_params = make_draw_params(draw, instance_count);
        if let Some(binding) = indirect_binding
            .as_ref()
            .filter(|binding| binding.params.is_byte_count)
        {
            // MTL has no draw-indirect-byte-count command. Eden's Vulkan path
            // derives vertexCount from a transform-feedback byte counter; the
            // synchronized fallback performs the same division explicitly.
            self.scheduler.finish_all()?;
            let mut bytes = [0; 4];
            binding.buffer.read(binding.offset, &mut bytes)?;
            draw_params.num_vertices =
                u32::from_ne_bytes(bytes) / binding.params.stride.max(1) as u32;
            draw_params.base_vertex = 0;
            draw_params.is_indexed = false;
        }
        let rasterizer = draw.rasterizer();
        let blend_color = draw.blend_color();
        let depth_stencil = draw.depth_stencil();
        let vertex_layouts = pipeline_key.vertex_input.layouts;

        for stage in [&prepared.vertex, &prepared.control, &prepared.evaluation,
            &prepared.geometry, &prepared.fragment] {
            if stage.samplers_in_argument_buffer {
                self.scheduler.retain_sampler_states(stage.samplers.iter().map(|s| s.sampler.clone()))?;
            }
        }

        self.query_cache_runtime.resume_host_conditional_rendering();
        let predicate = self.query_cache_runtime.active_conditional_rendering();

        let fan_draw = if geometry_pipeline.is_none()
            && tessellation_pipeline.is_none()
            && draw.draw_state().topology == PrimitiveTopology::TriangleFan
        {
            if self.primitive_assembler.is_none() {
                self.primitive_assembler = Some(MetalPrimitiveAssembler::new(&self.device)?);
            }
            let index = if draw_params.is_indexed {
                Some(prepared.index_buffer.as_ref().ok_or(MetalPrimitiveAssemblyError::IndexRange)?)
            } else {
                None
            };
            let index_bytes = index.map_or(0, |index| match index.index_type {
                objc2_metal::MTLIndexType::UInt16 => 2,
                _ => 4,
            });
            let restart = draw.primitive_restart();
            let restart_index = restart.enabled.then_some(
                if draw.draw_state().index_buffer.format
                    == crate::engines::maxwell_3d::IndexFormat::UnsignedByte && restart.index == 255
                {
                    65535
                } else {
                    restart.index
                },
            );
            let index = index.map(|index| {
                let offset = (draw_params.first_index as usize).checked_mul(index_bytes as usize)
                    .and_then(|first| index.offset.checked_add(first))
                    .ok_or(MetalPrimitiveAssemblyError::IndexRange)?;
                Ok::<_, MetalPrimitiveAssemblyError>((index.buffer.as_ref(), offset))
            }).transpose()?;
            Some(self.primitive_assembler.as_ref().unwrap().record_triangle_fan_draw(
                self.scheduler.as_mut(),
                MetalPrimitiveAssemblyParams {
                    topology: PrimitiveTopology::TriangleFan,
                    count: draw_params.num_vertices,
                    base_vertex: draw_params.base_vertex,
                    instances: draw_params.num_instances,
                    index_bytes,
                    restart_index,
                },
                index,
                draw_params.base_instance,
                draw.provoking_vertex_last(),
            )?)
        } else {
            None
        };

        let (geometry_inputs, tessellation_inputs) = if geometry_pipeline.is_some()
            || tessellation_pipeline.is_some()
        {
            let mut vertex_sizes = [0u64; 31];
            for (source, layout) in vertex_layouts.iter().enumerate().filter(|(_, l)| l.enabled) {
                let binding = prepared
                    .vertex_buffers
                    .get(source)
                    .and_then(Option::as_ref)
                    .ok_or(MetalRasterizerError::GeometryVertexBuffer(source))?;
                vertex_sizes[layout.buffer_index as usize] = binding
                    .size
                    .min(binding.buffer.length().saturating_sub(binding.offset))
                    as u64;
            }
            if self.primitive_assembler.is_none() {
                self.primitive_assembler = Some(MetalPrimitiveAssembler::new(&self.device)?);
            }
            let index = if draw_params.is_indexed {
                Some(
                    prepared
                        .index_buffer
                        .as_ref()
                        .ok_or(MetalPrimitiveAssemblyError::IndexRange)?,
                )
            } else {
                None
            };
            let index_bytes = index.map_or(0, |index| match index.index_type {
                objc2_metal::MTLIndexType::UInt16 => 2,
                _ => 4,
            });
            let converted_quads = tessellation_pipeline.is_none() && matches!(
                draw.draw_state().topology,
                PrimitiveTopology::Quads | PrimitiveTopology::QuadStrip
            );
            let restart = draw.primitive_restart();
            let restart_index = (restart.enabled && !converted_quads).then_some(
                if draw.draw_state().index_buffer.format
                    == crate::engines::maxwell_3d::IndexFormat::UnsignedByte
                    && restart.index == 255
                {
                    65535
                } else {
                    restart.index
                },
            );
            let assembly_params = MetalPrimitiveAssemblyParams {
                    // Eden forces patch-list input when TES is enabled.
                    topology: if tessellation_pipeline.is_some() {
                        PrimitiveTopology::Patches
                    } else if converted_quads {
                        PrimitiveTopology::Triangles
                    } else {
                        draw.draw_state().topology
                    },
                    count: draw_params.num_vertices,
                    base_vertex: draw_params.base_vertex,
                    instances: draw_params.num_instances,
                    index_bytes,
                    restart_index,
                };
            let index = index.map(|index| {
                    let offset = (draw_params.first_index as usize)
                        .checked_mul(index_bytes as usize)
                        .and_then(|first| index.offset.checked_add(first))
                        .ok_or(MetalPrimitiveAssemblyError::IndexRange)?;
                    Ok::<_, MetalPrimitiveAssemblyError>((
                        index.buffer.as_ref(),
                        offset,
                    ))
                }).transpose()?;
            let bind_resources = |encoder: &objc2::runtime::ProtocolObject<dyn MTLComputeCommandEncoder>| {
                    bind_vertex_resources(encoder, &prepared.vertex);
                    for (source, layout) in
                        vertex_layouts.iter().enumerate().filter(|(_, l)| l.enabled)
                    {
                        if let Some(binding) =
                            prepared.vertex_buffers.get(source).and_then(Option::as_ref)
                        {
                            unsafe {
                                encoder.setBuffer_offset_atIndex(
                                    Some(binding.buffer.handle()),
                                    binding.offset,
                                    layout.buffer_index as usize,
                                );
                            }
                        }
                    }
                };
            if let Some(tessellation) = &tessellation_pipeline {
                let assembly = self.primitive_assembler.as_ref().unwrap().record_patches(
                    self.scheduler.as_mut(), assembly_params,
                    stages.key().fixed_state.patch_control_points(), index,
                    draw_params.base_instance,
                )?;
                let inputs = tessellation.record_inputs(
                    self.scheduler.as_mut(), self._staging_pool.as_mut(),
                    &self.conditional_arguments_pass,
                    predicate.as_ref().map(|p| (p.buffer.as_ref(), p.offset, p.inverted)),
                    &assembly, &vertex_sizes, bind_resources,
                    |encoder| bind_vertex_resources(encoder, &prepared.control),
                )?;
                (None, Some(inputs))
            } else {
                let geometry = geometry_pipeline.as_ref().unwrap();
                let assembly = self.primitive_assembler.as_ref().unwrap().record(
                    self.scheduler.as_mut(), assembly_params, index,
                )?;
                let (assembly, vertices) = if let Some(predicate) = &predicate {
                    geometry.record_conditional_inputs(
                        self.scheduler.as_mut(), self._staging_pool.as_mut(),
                        &self.conditional_arguments_pass,
                        &predicate.buffer, predicate.offset, predicate.inverted,
                        &assembly, draw_params.base_instance, &vertex_sizes, bind_resources,
                    )?
                } else {
                    let vertices = geometry.vertex.record(
                        self.scheduler.as_mut(), &assembly, draw_params.base_instance,
                        &vertex_sizes, bind_resources,
                    )?;
                    (assembly, vertices)
                };
                let captured = geometry.capture_output(self.scheduler.as_mut(), &assembly, &vertices, &prepared.geometry)?;
                (Some((assembly, vertices, captured)), None)
            }
        } else {
            (None, None)
        };

        let conditional_draw = if geometry_inputs.is_none() && tessellation_inputs.is_none() {
            if let Some(predicate) = &predicate {
                let direct_words = if fan_draw.is_none()
                    && !indirect_binding.as_ref().is_some_and(|b| !b.params.is_byte_count)
                {
                    Some(if draw_params.is_indexed {
                        [draw_params.num_vertices, draw_params.num_instances, draw_params.first_index,
                            draw_params.base_vertex as u32, draw_params.base_instance]
                    } else {
                        [draw_params.num_vertices, draw_params.num_instances,
                            draw_params.base_vertex.max(0) as u32, draw_params.base_instance, 0]
                    })
                } else { None };
                let batched = if let Some(words) = direct_words {
                    self.conditional_direct_arguments.append(
                        &self.conditional_arguments_pass, self.scheduler.as_mut(),
                        self._staging_pool.as_mut(), predicate, words,
                    )?
                } else { None };
                if let Some((buffer, offset)) = batched {
                    Some(MetalConditionalDrawArguments {
                        buffer, offset, count: 1,
                        layout: if draw_params.is_indexed { ConditionalArgumentLayout::DrawIndexed }
                            else { ConditionalArgumentLayout::Draw },
                    })
                } else {
                    let (source, source_offset, stride, count, layout) = if let Some(fan) = &fan_draw {
                        (Arc::clone(&fan.arguments), 0, 20, 1, ConditionalArgumentLayout::DrawIndexed)
                    } else if let Some(binding) = indirect_binding.as_ref().filter(|b| !b.params.is_byte_count) {
                        (Arc::clone(&binding.buffer), binding.offset, binding.params.stride as u32,
                            binding.draw_count, if binding.params.is_indexed {
                                ConditionalArgumentLayout::DrawIndexed
                            } else { ConditionalArgumentLayout::Draw })
                    } else {
                        let words = direct_words.expect("direct conditional arguments");
                        let layout = if draw_params.is_indexed { ConditionalArgumentLayout::DrawIndexed }
                            else { ConditionalArgumentLayout::Draw };
                        let source = self._staging_pool.request_upload_buffer(
                            self.scheduler.as_mut(), layout.byte_size(), false,
                        )?;
                        source.buffer.write(source.offset, &bytemuck::cast_slice(&words)[..layout.byte_size()])?;
                        (source.buffer, source.offset, layout.byte_size() as u32, 1, layout)
                    };
                    let arguments = self.conditional_arguments_pass.resolve(
                        self.scheduler.as_mut(), self._staging_pool.as_mut(),
                        &predicate.buffer, predicate.offset, predicate.inverted,
                        &source, source_offset, stride, count, layout,
                    )?;
                    Some(MetalConditionalDrawArguments {
                        buffer: arguments.buffer, offset: arguments.offset, layout, count,
                    })
                }
            } else { None }
        } else { None };

        self.scheduler
            .begin_or_reuse_render_pass(&render_pass, render_pass_key)?;
        self.update_viewports_state(draw)?;
        self.update_scissors_state(draw, render_area)?;
        self.scheduler.profile_graphics_draw(stages.key().unique_hashes);
        self.texture_cache.mark_render_target_contents_modified(u32::MAX, depth_key.may_write_depth_stencil());
        self.scheduler.with_render_encoder(|encoder| {
            MetalQueryCache::configure_draw(encoder, visibility_query);
            encoder.setRenderPipelineState(&pipeline_state);
            encoder.setDepthStencilState(Some(&depth_state));
            encoder.setCullMode(if rasterizer.cull_enable {
                match rasterizer.cull_face {
                    CullFace::Front => MTLCullMode::Front,
                    CullFace::Back => MTLCullMode::Back,
                    // Triangle rasterization is disabled in the PSO. Point/line
                    // output is unaffected; shader execution must not be skipped.
                    CullFace::FrontAndBack => MTLCullMode::None,
                }
            } else {
                MTLCullMode::None
            });
            encoder.setFrontFacingWinding(match rasterizer.front_face {
                FrontFace::CW => MTLWinding::Clockwise,
                FrontFace::CCW => MTLWinding::CounterClockwise,
            });
            encoder.setDepthBias_slopeScale_clamp(
                rasterizer.depth_bias,
                rasterizer.slope_scale_depth_bias,
                rasterizer.depth_bias_clamp,
            );
            encoder.setBlendColorRed_green_blue_alpha(
                blend_color.r,
                blend_color.g,
                blend_color.b,
                blend_color.a,
            );
            if depth_stencil.stencil_two_side {
                encoder.setStencilFrontReferenceValue_backReferenceValue(
                    depth_stencil.front.ref_value,
                    depth_stencil.back.ref_value,
                );
            } else {
                encoder.setStencilReferenceValue(depth_stencil.front.ref_value);
            }

            if let Some((assembly, vertices, captured)) = &geometry_inputs {
                bind_stage(encoder, &prepared.fragment, false);
                return geometry_pipeline.as_ref().unwrap().record_draw(encoder, assembly, vertices,
                    &prepared.geometry, captured.as_ref()).map_err(MetalRasterizerError::from);
            }
            if let Some(inputs) = &tessellation_inputs {
                bind_stage(encoder, &prepared.evaluation, true);
                bind_stage(encoder, &prepared.fragment, false);
                return tessellation_pipeline.as_ref().unwrap().record_draw(encoder, inputs)
                    .map_err(MetalRasterizerError::from);
            }
            bind_stage(encoder, &prepared.vertex, true);
            bind_stage(encoder, &prepared.fragment, false);
            let primitive_type = primitive_type.expect("native primitive topology was validated");
            unsafe {
                for (source, layout) in vertex_layouts.iter().enumerate() {
                    if !layout.enabled {
                        continue;
                    }
                    let Some(binding) = prepared.vertex_buffers.get(source).and_then(Option::as_ref)
                    else {
                        continue;
                    };
                    encoder.setVertexBuffer_offset_atIndex(
                        Some(binding.buffer.handle()),
                        binding.offset,
                        layout.buffer_index as usize,
                    );
                }

                if let Some(conditional) = &conditional_draw {
                    for command in 0..conditional.count as usize {
                        let offset = conditional.offset + command * conditional.layout.byte_size();
                        match conditional.layout {
                            ConditionalArgumentLayout::DrawIndexed => {
                                let (buffer, index_offset, index_type, primitive) = if let Some(fan) = &fan_draw {
                                    (&fan.indices, 0, objc2_metal::MTLIndexType::UInt32, MTLPrimitiveType::Triangle)
                                } else {
                                    let index = prepared.index_buffer.as_ref().expect("conditional indexed binding");
                                    (&index.buffer, index.offset, index.index_type, primitive_type)
                                };
                                encoder.drawIndexedPrimitives_indexType_indexBuffer_indexBufferOffset_indirectBuffer_indirectBufferOffset(
                                    primitive, index_type, buffer.handle(), index_offset,
                                    conditional.buffer.handle(), offset,
                                );
                            }
                            ConditionalArgumentLayout::Draw => {
                                encoder.drawPrimitives_indirectBuffer_indirectBufferOffset(
                                    primitive_type, conditional.buffer.handle(), offset,
                                );
                            }
                            ConditionalArgumentLayout::Dispatch => unreachable!("raster arguments only"),
                        }
                    }
                } else if let Some(fan) = &fan_draw {
                    encoder.drawIndexedPrimitives_indexType_indexBuffer_indexBufferOffset_indirectBuffer_indirectBufferOffset(
                        MTLPrimitiveType::Triangle,
                        objc2_metal::MTLIndexType::UInt32,
                        fan.indices.handle(),
                        0,
                        fan.arguments.handle(),
                        0,
                    );
                } else if let Some(binding) = indirect_binding
                    .as_ref()
                    .filter(|binding| !binding.params.is_byte_count)
                {
                    let stride = binding.params.stride as usize;
                    if binding.params.is_indexed {
                        let index = prepared
                            .index_buffer
                            .as_ref()
                            .expect("indexed Metal indirect draw requires an index buffer binding");
                        for draw_index in 0..binding.draw_count as usize {
                            encoder.drawIndexedPrimitives_indexType_indexBuffer_indexBufferOffset_indirectBuffer_indirectBufferOffset(
                                primitive_type,
                                index.index_type,
                                index.buffer.handle(),
                                index.offset,
                                binding.buffer.handle(),
                                binding.offset + draw_index * stride,
                            );
                        }
                    } else {
                        for draw_index in 0..binding.draw_count as usize {
                            encoder.drawPrimitives_indirectBuffer_indirectBufferOffset(
                                primitive_type,
                                binding.buffer.handle(),
                                binding.offset + draw_index * stride,
                            );
                        }
                    }
                } else if draw_params.is_indexed {
                    let binding = prepared
                        .index_buffer
                        .as_ref()
                        .expect("indexed Metal draw requires an index buffer binding");
                    let index_size = match binding.index_type {
                        objc2_metal::MTLIndexType::UInt16 => 2,
                        objc2_metal::MTLIndexType::UInt32 => 4,
                        _ => 4,
                    };
                    encoder.drawIndexedPrimitives_indexCount_indexType_indexBuffer_indexBufferOffset_instanceCount_baseVertex_baseInstance(
                        primitive_type,
                        draw_params.num_vertices as usize,
                        binding.index_type,
                        binding.buffer.handle(),
                        binding.offset + draw_params.first_index as usize * index_size,
                        draw_params.num_instances as usize,
                        draw_params.base_vertex as isize,
                        draw_params.base_instance as usize,
                    );
                } else {
                    encoder.drawPrimitives_vertexStart_vertexCount_instanceCount_baseInstance(
                        primitive_type,
                        draw_params.base_vertex.max(0) as usize,
                        draw_params.num_vertices as usize,
                        draw_params.num_instances as usize,
                        draw_params.base_instance as usize,
                    );
                }
            }
            Ok(())
        })??;
        Ok(())
    }

    pub fn draw_indirect(
        &mut self,
        indirect_view: &mut Maxwell3DIndirectView<'_>,
    ) -> Result<(), MetalRasterizerError> {
        let params = *indirect_view.params();
        self.common_buffer_cache
            .set_draw_indirect(Some(CacheDrawIndirectParams {
                indirect_start_address: params.indirect_start_address,
                count_start_address: params.count_start_address,
                buffer_size: params.buffer_size as u64,
                max_draw_counts: params.max_draw_counts as u32,
                stride: params.stride as u32,
                include_count: params.include_count,
            }));
        let instance_count = indirect_view.draw_view_mut().draw_state().instance_count;
        let result = self.draw_impl(indirect_view.draw_view_mut(), instance_count, Some(params));
        self.common_buffer_cache.set_draw_indirect(None);
        result
    }

    /// Port of Eden `RasterizerVulkan::DrawTexture` using a native Metal
    /// textured quad rather than a Vulkan render-pass helper.
    pub fn draw_texture(
        &mut self,
        mut draw_texture_view: Maxwell3DDrawTextureView<'_>,
    ) -> Result<(), MetalRasterizerError> {
        if let Some(memory_manager) = self.channel_memory_manager.as_ref() {
            memory_manager.lock().flush_caching();
        }
        let state = draw_texture_view.draw_texture_state();
        let render_targets = draw_texture_view.render_targets();
        let original_dirty_flags = *draw_texture_view.dirty_flags();
        let mut dirty_flags = original_dirty_flags;
        let memory_manager = self.channel_memory_manager.as_ref().cloned();

        let texture_mutex: *const _ = &self.texture_cache.base.mutex;
        let _texture_guard = unsafe { (*texture_mutex).lock() };
        self.texture_cache
            .synchronize_graphics_descriptors(draw_texture_view.descriptor_sync_regs());
        self.texture_cache.base.update_render_targets_with_snapshot(
            &render_targets,
            &mut dirty_flags,
            |address, size| {
                memory_manager
                    .as_ref()
                    .and_then(|manager| manager.lock().gpu_to_cpu_address_range(address, size))
            },
            false,
            None,
        );
        for (index, (&was_dirty, &is_dirty)) in original_dirty_flags
            .iter()
            .zip(dirty_flags.iter())
            .enumerate()
        {
            if was_dirty && !is_dirty {
                draw_texture_view.clear_dirty_flag(index as u8);
            }
        }

        let sampler_id = self.texture_cache.get_sampler_id(state.src_sampler, false);
        let Some(sampler) = self
            .texture_cache
            .sampler(sampler_id)
            .map(|sampler| sampler.retained_handle())
        else {
            log::warn!(
                "Metal DrawTexture skipped: invalid sampler {}",
                state.src_sampler
            );
            return Ok(());
        };
        let Some((source, source_width, source_height, source_rescaled)) =
            self.texture_cache.draw_texture_source(state.src_texture)
        else {
            log::warn!(
                "Metal DrawTexture skipped: invalid texture {}",
                state.src_texture
            );
            return Ok(());
        };
        let (render_pass, signature, render_area) = {
            let framebuffer = self.texture_cache.base.get_framebuffer()?;
            (
                framebuffer.render_pass_descriptor(),
                framebuffer.signature(),
                framebuffer.render_area(),
            )
        };
        let visibility_query = self.query_cache.prepare_draw(
            self.scheduler.as_mut(),
            draw_texture_view.zpass_pixel_count_enabled(),
        )?;
        if visibility_query.is_some() {
            self.query_cache.attach_render_pass(&render_pass);
        }

        let destination_rescaled = self.texture_cache.base.is_rescaling;
        let resolution = common::settings::values().resolution_info.clone();
        let scale = |value: f32, rescaled: bool| {
            let value = value as i32;
            if rescaled {
                resolution.scale_up_i32(value)
            } else {
                value
            }
        };
        let dst = MetalBlitRegion {
            start: (
                scale(state.dst_x0, destination_rescaled),
                scale(state.dst_y0, destination_rescaled),
            ),
            end: (
                scale(state.dst_x1, destination_rescaled),
                scale(state.dst_y1, destination_rescaled),
            ),
        };
        let src = MetalBlitRegion {
            start: (
                scale(state.src_x0, source_rescaled),
                scale(state.src_y0, source_rescaled),
            ),
            end: (
                scale(state.src_x1, source_rescaled),
                scale(state.src_y1, source_rescaled),
            ),
        };
        let source_size = if source_rescaled {
            (
                resolution.scale_up_u32(source_width),
                resolution.scale_up_u32(source_height),
            )
        } else {
            (source_width, source_height)
        };
        let conditional_arguments = self.conditional_quad_arguments()?;
        self.texture_cache.mark_render_target_contents_modified(u32::MAX, false);
        self.blit_image.blit_color_with_sampler(
            self.scheduler.as_mut(),
            &render_pass,
            signature,
            render_area,
            &source,
            &sampler,
            dst,
            src,
            source_size,
            visibility_query,
            conditional_arguments.as_ref().map(|a| (a.buffer.as_ref(), a.offset)),
        )?;
        Ok(())
    }

    /// Port of Eden `RasterizerVulkan::Clear` for full attachment clears.
    /// Scissored and channel-masked clears are kept out of this path because
    /// Metal load actions cannot express them; `MetalBlitHelper` owns that
    /// shader-based prerequisite.
    pub fn clear(
        &mut self,
        mut clear_view: Maxwell3DClearView<'_>,
        layer_count: u32,
    ) -> Result<(), MetalRasterizerError> {
        if let Some(memory_manager) = self.channel_memory_manager.as_ref() {
            memory_manager.lock().flush_caching();
        }
        let state = clear_view.clear_state();
        let use_depth = state.flags & (1 << 0) != 0;
        let use_stencil = state.flags & (1 << 1) != 0;
        let use_r = state.flags & (1 << 2) != 0;
        let use_g = state.flags & (1 << 3) != 0;
        let use_b = state.flags & (1 << 4) != 0;
        let use_a = state.flags & (1 << 5) != 0;
        let use_color = use_r || use_g || use_b || use_a;
        if !use_color && !use_depth && !use_stencil {
            return Ok(());
        }

        let render_targets = clear_view.render_targets();
        let clear_scissor = clear_view.use_scissor().then(|| {
            let scissor = clear_view.scissor(0);
            (scissor.min_x, scissor.min_y, scissor.max_x, scissor.max_y)
        });
        let original_dirty_flags = *clear_view.dirty_flags();
        let mut dirty_flags = original_dirty_flags;
        let memory_manager = self.channel_memory_manager.as_ref().cloned();
        let texture_mutex: *const _ = &self.texture_cache.base.mutex;
        let _texture_guard = unsafe { (*texture_mutex).lock() };
        self.texture_cache.base.update_render_targets_with_snapshot(
            &render_targets,
            &mut dirty_flags,
            |address, size| {
                memory_manager
                    .as_ref()
                    .and_then(|manager| manager.lock().gpu_to_cpu_address_range(address, size))
            },
            true,
            clear_scissor,
        );
        for (index, (&was_dirty, &is_dirty)) in original_dirty_flags
            .iter()
            .zip(dirty_flags.iter())
            .enumerate()
        {
            if was_dirty && !is_dirty {
                clear_view.clear_dirty_flag(index as u8);
            }
        }

        let depth_stencil = clear_view.depth_stencil();
        let color_attachment = ((state.flags >> 6) & 0xf) as usize;
        let clear_layer = (state.flags >> 10) & 0xffff;
        let color_mask = u8::from(use_r)
            | (u8::from(use_g) << 1)
            | (u8::from(use_b) << 2)
            | (u8::from(use_a) << 3);
        let stencil_mask = depth_stencil.front.write_mask;
        let stencil_partial = use_stencil && stencil_mask != 0 && stencil_mask != 0xff;
        let (signature, render_area, color_present, depth_present, stencil_present) = {
            let framebuffer = self.texture_cache.base.get_framebuffer()?;
            let signature = framebuffer.signature();
            let color_present = use_color
                && signature
                    .color_formats
                    .get(color_attachment)
                    .is_some_and(|format| *format != objc2_metal::MTLPixelFormat::Invalid);
            (
                signature,
                framebuffer.render_area(),
                color_present,
                use_depth && framebuffer.has_depth(),
                use_stencil && framebuffer.has_stencil(),
            )
        };
        if !color_present && !depth_present && !stencil_present {
            return Ok(());
        }
        self.texture_cache.mark_render_target_contents_modified(
            if color_present { 1 << color_attachment } else { 0 }, depth_present || stencil_present);
        let color_format = color_present.then(|| {
            crate::surface::pixel_format_from_render_target_format(
                render_targets.render_targets[color_attachment].format,
            )
        });
        // VkClearValue carries integer attachments as typed integer payloads.
        // MTLClearColor is floating-point, so keep integer clears on the typed
        // shader path even when every channel and the full extent are selected.
        let color_is_integer = color_format.is_some_and(crate::surface::is_pixel_format_integer);
        let conditional_arguments = self.conditional_quad_arguments()?;
        let full_clear = conditional_arguments.is_none() && !clear_view.use_scissor()
            && (!use_color || color_mask == 0xf)
            && !color_is_integer
            && !stencil_partial;

        if full_clear {
            for layer in 0..layer_count.max(1) {
                let descriptor = {
                    let framebuffer = self.texture_cache.base.get_framebuffer()?;
                    framebuffer.clear_render_pass_descriptor(MetalFramebufferClear {
                        color: color_present.then_some((color_attachment, state.color)),
                        depth: depth_present.then_some(state.depth),
                        stencil: stencil_present.then_some(state.stencil as u32),
                        base_layer: clear_layer + layer,
                        layer_count: 1,
                    })
                };
                self.scheduler.begin_render_pass(&descriptor)?;
                self.scheduler.end_render_pass();
            }
            return Ok(());
        }

        let resolution = common::settings::values().resolution_info.clone();
        let (up_scale, down_shift) = if self.texture_cache.base.is_rescaling {
            (resolution.up_scale, resolution.down_shift)
        } else {
            (1, 0)
        };
        let mut region = if clear_view.use_scissor() {
            let scissor = clear_view.scissor(0);
            let (min_y, max_y) = if clear_view.window_origin_lower_left() {
                (
                    render_targets
                        .surface_clip
                        .height
                        .saturating_sub(scissor.max_y),
                    render_targets
                        .surface_clip
                        .height
                        .saturating_sub(scissor.min_y),
                )
            } else {
                (scissor.min_y, scissor.max_y)
            };
            MetalBlitRegion {
                start: (
                    (scissor.min_x.wrapping_mul(up_scale) >> down_shift) as i32,
                    (min_y.wrapping_mul(up_scale) >> down_shift) as i32,
                ),
                end: (
                    (scissor.max_x.wrapping_mul(up_scale) >> down_shift) as i32,
                    (max_y.wrapping_mul(up_scale) >> down_shift) as i32,
                ),
            }
        } else {
            MetalBlitRegion {
                start: (0, 0),
                end: (render_area.0 as i32, render_area.1 as i32),
            }
        };
        region.start.0 = region.start.0.clamp(0, render_area.0 as i32);
        region.start.1 = region.start.1.clamp(0, render_area.1 as i32);
        region.end.0 = region.end.0.clamp(region.start.0, render_area.0 as i32);
        region.end.1 = region.end.1.clamp(region.start.1, render_area.1 as i32);
        if region.start == region.end {
            return Ok(());
        }

        let color_type = match color_format {
            Some(format) if crate::surface::is_pixel_format_signed_integer(format) => {
                MetalClearColorType::Sint
            }
            Some(format) if crate::surface::is_pixel_format_integer(format) => {
                MetalClearColorType::Uint
            }
            _ => MetalClearColorType::Float,
        };
        let mut signed_color = [0; 4];
        let mut unsigned_color = [0; 4];
        if let Some(format) =
            color_format.filter(|format| crate::surface::is_pixel_format_integer(*format))
        {
            let bits = crate::surface::pixel_component_size_bits_integer(format);
            if crate::surface::is_pixel_format_signed_integer(format) {
                let scale = (((bits - 1) as i64) << 1) as f32;
                signed_color = state
                    .color
                    .map(|component| (scale * (component - 0.5)) as i32);
            } else {
                let scale = ((bits as u64) << 1) as f32;
                unsigned_color = state.color.map(|component| (scale * component) as u32);
            }
        }
        for layer in 0..layer_count.max(1) {
            let (render_pass, render_pass_key) = {
                let framebuffer = self.texture_cache.base.get_framebuffer()?;
                (
                    framebuffer.render_pass_descriptor_for_layer(clear_layer + layer),
                    framebuffer.render_pass_key_for_layer(clear_layer + layer),
                )
            };
            self.blit_image.clear_attachments(
                self.scheduler.as_mut(),
                &render_pass,
                render_pass_key,
                signature,
                color_present.then_some(color_attachment as u8),
                color_type,
                color_mask,
                depth_present,
                stencil_present,
                stencil_mask,
                MetalClearParameters {
                    region,
                    render_area,
                    color: state.color,
                    signed_color,
                    unsigned_color,
                    depth: state.depth,
                    stencil: state.stencil as u32,
                },
                conditional_arguments.as_ref().map(|a| (a.buffer.as_ref(), a.offset)),
            )?;
        }
        Ok(())
    }

    /// Port of Eden `RasterizerVulkan::DispatchCompute` using one native
    /// Metal compute encoder in the scheduler's guest-order command buffer.
    pub fn dispatch_compute(
        &mut self,
        dispatch: &DispatchCall,
    ) -> Result<(), MetalRasterizerError> {
        if let Some(memory_manager) = self.channel_memory_manager.as_ref() {
            memory_manager.lock().flush_caching();
        }
        let Some(pipeline) = self
            .pipeline_cache
            .current_compute_pipeline(&mut self.shader_cache)?
        else {
            return Ok(());
        };
        let Some(memory_manager) = self.channel_memory_manager.as_ref().cloned() else {
            return Ok(());
        };
        let read_gpu = |address: u64, output: &mut [u8]| {
            memory_manager.lock().read_block_unsafe(address, output);
        };
        let buffer_cache_mutex: *const _ = Arc::as_ptr(&self.common_buffer_cache.mutex);
        let texture_cache_mutex: *const _ = &self.texture_cache.base.mutex;
        lock_two_reentrant_mutexes!(
            buffer_cache_mutex,
            texture_cache_mutex,
            _buffer_cache_guard,
            _texture_cache_guard
        );
        let prepared = configure_compute_resources(
            &self.device,
            &pipeline,
            dispatch,
            self.common_buffer_cache.as_mut(),
            self.texture_cache.as_mut(),
            read_gpu,
        )?;

        let workgroup = pipeline.key().workgroup_size;
        let maximum = self.device.profile().max_threads_per_threadgroup;
        let total_threads = workgroup
            .iter()
            .fold(1u64, |total, value| total.saturating_mul(*value as u64));
        if workgroup[0] as usize > maximum.0
            || workgroup[1] as usize > maximum.1
            || workgroup[2] as usize > maximum.2
            || total_threads > pipeline.state().maxTotalThreadsPerThreadgroup() as u64
        {
            return Err(MetalRasterizerError::UnsupportedComputeWorkgroup {
                requested: workgroup,
                maximum,
            });
        }
        let threads_per_threadgroup = MTLSize {
            width: workgroup[0].max(1) as usize,
            height: workgroup[1].max(1) as usize,
            depth: workgroup[2].max(1) as usize,
        };
        let pipeline_state = pipeline.retained_state();

        if let Some(indirect_address) = dispatch.indirect_compute_address {
            let (buffer_id, offset) = self.common_buffer_cache.obtain_buffer(
                indirect_address,
                12,
                ObtainBufferSynchronize::FullSynchronize,
                ObtainBufferOperation::DiscardWrite,
            );
            let Some(indirect_buffer) = self
                .common_buffer_cache
                .backend_buffer(buffer_id)
                .map(|buffer| buffer.handle())
            else {
                return Ok(());
            };
            if prepared.samplers_in_argument_buffer {
                self.scheduler.retain_sampler_states(prepared.samplers.iter().map(|s| s.sampler.clone()))?;
            }
            self.scheduler.with_compute_encoder_for(super::metal_gpu_profiler::ComputeWork::Guest, |encoder| unsafe {
                encoder.setComputePipelineState(&pipeline_state);
                bind_compute_resources(encoder, &prepared);
                encoder.dispatchThreadgroupsWithIndirectBuffer_indirectBufferOffset_threadsPerThreadgroup(
                    indirect_buffer.handle(),
                    offset as usize,
                    threads_per_threadgroup,
                );
            })?;
            return Ok(());
        }

        let grid = &dispatch.launch_description;
        let threadgroups_per_grid = MTLSize {
            width: grid.grid_dim_x as usize,
            height: grid.grid_dim_y as usize,
            depth: grid.grid_dim_z as usize,
        };
        if prepared.samplers_in_argument_buffer {
            self.scheduler.retain_sampler_states(prepared.samplers.iter().map(|s| s.sampler.clone()))?;
        }
        self.scheduler.with_compute_encoder_for(super::metal_gpu_profiler::ComputeWork::Guest, |encoder| {
            encoder.setComputePipelineState(&pipeline_state);
            bind_compute_resources(encoder, &prepared);
            encoder.dispatchThreadgroups_threadsPerThreadgroup(
                threadgroups_per_grid,
                threads_per_threadgroup,
            );
        })?;
        Ok(())
    }

    fn update_viewports_state(
        &mut self,
        draw: &Maxwell3DDrawView<'_>,
    ) -> Result<(), MetalSchedulerError> {
        let count = self.device.profile().max_viewports().min(NUM_VIEWPORTS);
        let mut viewports: [MTLViewport; NUM_VIEWPORTS] = std::array::from_fn(|index| {
            if draw.viewport_scale_offset_enabled() {
                get_viewport_state(draw, index)
            } else {
                surface_clip_viewport(draw)
            }
        });
        self.scheduler.with_render_encoder(|encoder| {
            if count == 1 {
                encoder.setViewport(viewports[0]);
            } else {
                // Metal copies the array during this call; it does not retain the pointer.
                unsafe { encoder.setViewports_count(NonNull::from(&mut viewports[0]), count) };
            }
        })
    }

    fn update_scissors_state(
        &mut self,
        draw: &Maxwell3DDrawView<'_>,
        render_area: (u32, u32),
    ) -> Result<(), MetalSchedulerError> {
        let count = self.device.profile().max_viewports().min(NUM_VIEWPORTS);
        let mut scissors: [MTLScissorRect; NUM_VIEWPORTS] = std::array::from_fn(|index| {
            if draw.viewport_scale_offset_enabled() {
                get_scissor_state(draw, index, render_area)
            } else {
                surface_clip_scissor(draw, render_area)
            }
        });
        self.scheduler.with_render_encoder(|encoder| {
            if count == 1 {
                encoder.setScissorRect(scissors[0]);
            } else {
                unsafe { encoder.setScissorRects_count(NonNull::from(&mut scissors[0]), count) };
            }
        })
    }

    pub fn tick_frame(&mut self) {
        // Match RasterizerVulkan::TickFrame's separate cache critical sections;
        // CPU invalidation can retire resources while the GPU advances frames.
        {
            let mutex: *const _ = &self.texture_cache.base.mutex;
            // SAFETY: the cache stays in place and the guard only protects its
            // interior state; no cache mutation moves or replaces the mutex.
            let _guard = unsafe { (*mutex).lock() };
            self.texture_cache.tick_frame();
        }
        {
            let mutex = Arc::clone(&self.common_buffer_cache.mutex);
            let _guard = mutex.lock();
            self.common_buffer_cache.tick_frame();
        }
    }

    pub fn finish(&mut self) -> Result<(), MetalRasterizerError> {
        self.scheduler.finish_all()?;
        Ok(())
    }

    fn create_fence(&mut self, is_stubbed: bool) -> MetalFence {
        if is_stubbed || !self.scheduler.has_active_work() {
            return MetalFence::stubbed();
        }
        MetalFence::from_command_buffer(
            self.scheduler
                .active_command_buffer()
                .expect("Metal fence command buffer allocation failed"),
        )
    }

    fn flush_commands_for_fence(&mut self) {
        if let Err(error) = self.scheduler.flush() {
            log::error!("Metal fence submission failed: {error}");
        }
    }

    /// Native replacement for a conditional region around the helper's quad.
    fn conditional_quad_arguments(&mut self) -> Result<Option<StagingBufferRef>, MetalRasterizerError> {
        self.query_cache_runtime.resume_host_conditional_rendering();
        let Some(predicate) = self.query_cache_runtime.active_conditional_rendering() else {
            return Ok(None);
        };
        let source = self._staging_pool.request_upload_buffer(self.scheduler.as_mut(), 16, false)?;
        source.buffer.write(source.offset, bytemuck::cast_slice(&[4u32, 1, 0, 0]))?;
        Ok(Some(self.conditional_arguments_pass.resolve(
            self.scheduler.as_mut(), self._staging_pool.as_mut(),
            &predicate.buffer, predicate.offset, predicate.inverted,
            &source.buffer, source.offset, 16, 1, ConditionalArgumentLayout::Draw,
        )?))
    }

    fn notify_query_wfi(&mut self) {
        if let Err(error) = self.query_cache.notify_wfi(
            &mut self.query_cache_runtime,
            &mut self.common_buffer_cache,
        ) {
            log::error!("Metal query GPU synchronization failed: {error}");
        }
    }

    fn invalidate_gpu_cache_callback(&self) {
        if let Some(callback) = &self.invalidate_gpu_cache_callback {
            callback();
        }
    }
}

impl RasterizerInterface for MetalRasterizer {
    fn accelerate_conditional_rendering_with_state(
        &mut self,
        state: crate::query_cache::query_cache::RenderConditionState,
    ) -> bool {
        let Some(memory_manager) = self.channel_memory_manager.clone() else {
            self.query_cache_runtime.end_host_conditional_rendering();
            return false;
        };
        memory_manager.lock().flush_caching();
        match self.query_cache.accelerate_host_conditional_rendering(
            &mut self.query_cache_runtime, self.common_buffer_cache.as_mut(),
            &GpuMemoryAccessAdapter { memory_manager }, state,
        ) {
            Ok(accelerated) => accelerated,
            Err(error) => {
                self.query_cache_runtime.end_host_conditional_rendering();
                log::error!("Metal conditional rendering failed: {error}");
                false
            }
        }
    }

    fn accelerate_surface_copy(
        &mut self,
        src: &crate::engines::fermi_2d::Surface,
        dst: &crate::engines::fermi_2d::Surface,
        copy: &crate::engines::fermi_2d::Config,
    ) -> bool {
        let mutex: *const _ = &self.texture_cache.base.mutex;
        let _guard = unsafe { &*mutex }.lock();
        self.texture_cache.blit_image(dst, src, copy)
    }

    fn load_disk_resources(
        &mut self,
        title_id: u64,
        stop_loading: crate::rasterizer_interface::DiskResourceLoadStop,
        callback: crate::rasterizer_interface::DiskResourceLoadCallback,
    ) {
        let shader_dir =
            common::fs::path_util::get_ruzu_path(common::fs::path_util::RuzuPath::ShaderDir);
        self.pipeline_cache
            .load_disk_resources(title_id, &shader_dir, stop_loading, callback);
    }

    fn draw(&mut self, mut draw_view: Maxwell3DDrawView<'_>, instance_count: u32) {
        if let Err(error) = MetalRasterizer::draw(self, &mut draw_view, instance_count) {
            log::error!("Metal draw failed: {error}");
        }
    }

    fn draw_indirect(&mut self, mut indirect_view: Maxwell3DIndirectView<'_>) {
        if let Err(error) = MetalRasterizer::draw_indirect(self, &mut indirect_view) {
            log::error!("Metal indirect draw failed: {error}");
        }
    }

    fn draw_texture(&mut self, draw_texture_view: Maxwell3DDrawTextureView<'_>) {
        if let Err(error) = MetalRasterizer::draw_texture(self, draw_texture_view) {
            log::error!("Metal draw-texture failed: {error}");
        }
    }

    fn clear(&mut self, clear_view: Maxwell3DClearView<'_>, layer_count: u32) {
        if let Err(error) = MetalRasterizer::clear(self, clear_view, layer_count) {
            log::error!("Metal clear failed: {error}");
        }
    }

    fn dispatch_compute(&mut self, dispatch: &DispatchCall) {
        if let Err(error) = MetalRasterizer::dispatch_compute(self, dispatch) {
            log::error!("Metal compute dispatch failed: {error}");
        }
    }

    fn reset_counter(&mut self, query_type: u32) {
        self.query_cache.reset_counter(query_type);
    }

    fn query(
        &mut self,
        gpu_addr: u64,
        query_type: u32,
        flags: QueryPropertiesFlags,
        payload: u32,
        _subreport: u32,
    ) {
        let report = self.query_cache.report(
            self.scheduler.as_mut(),
            self.channel_memory_manager.clone(),
            self.gpu_ticks_getter.clone(),
            gpu_addr,
            query_type,
            flags,
            payload,
        );
        match report {
            Ok(MetalQueryReport::Complete) => {}
            Ok(MetalQueryReport::SignalFence(operation)) => self.signal_fence(operation),
            Ok(MetalQueryReport::SignalFenceAfterCompletion(operation)) => {
                self.sync_operation(operation);
                self.signal_fence(Box::new(|| {}));
            }
            Ok(MetalQueryReport::SyncOperation(operation)) => self.sync_operation(operation),
            Err(error) => log::error!("Metal query report failed: {error}"),
        }
    }

    fn bind_graphics_uniform_buffer(&mut self, stage: usize, index: u32, gpu_addr: u64, size: u32) {
        self.common_buffer_cache
            .bind_graphics_uniform_buffer(stage, index, gpu_addr, size);
    }

    fn disable_graphics_uniform_buffer(&mut self, stage: usize, index: u32) {
        self.common_buffer_cache
            .disable_graphics_uniform_buffer(stage, index);
    }

    fn signal_fence(&mut self, func: Box<dyn FnOnce() + Send>) {
        self.notify_query_wfi();
        self.fence_manager.sync_operation(self.query_cache.commit_async_flushes());
        let (should_wait_queries, pop_queries) = self.query_cache.async_flush_callbacks();
        let this = self as *mut Self;
        self.fence_manager.signal_fence(
            func,
            move |is_stubbed| unsafe { (*this).create_fence(is_stubbed) },
            |_fence| {},
            should_wait_queries,
            |fence| fence.is_signaled(),
            pop_queries,
            move || unsafe { (*this).scheduler.has_active_work() },
            || {},
            move || unsafe { (*this).flush_commands_for_fence() },
            move || unsafe { (*this).invalidate_gpu_cache_callback() },
        );
    }

    fn sync_operation(&mut self, func: Box<dyn FnOnce() + Send>) {
        self.fence_manager.sync_operation(func);
    }

    fn signal_sync_point(&mut self, value: u32) {
        self.notify_query_wfi();
        self.fence_manager.sync_operation(self.query_cache.commit_async_flushes());
        let (should_wait_queries, pop_queries) = self.query_cache.async_flush_callbacks();
        let this = self as *mut Self;
        let syncpoints = Arc::clone(&self.syncpoints);
        self.fence_manager.signal_sync_point(
            value,
            {
                let syncpoints = Arc::clone(&syncpoints);
                move |id| syncpoints.increment_guest(id)
            },
            move |id| syncpoints.increment_host(id),
            move |is_stubbed| unsafe { (*this).create_fence(is_stubbed) },
            |_fence| {},
            should_wait_queries,
            |fence| fence.is_signaled(),
            pop_queries,
            move || unsafe { (*this).scheduler.has_active_work() },
            || {},
            move || unsafe { (*this).flush_commands_for_fence() },
            move || unsafe { (*this).invalidate_gpu_cache_callback() },
        );
    }

    fn signal_reference(&mut self) {
        self.notify_query_wfi();
        self.fence_manager.sync_operation(self.query_cache.commit_async_flushes());
        let (should_wait_queries, pop_queries) = self.query_cache.async_flush_callbacks();
        let this = self as *mut Self;
        self.fence_manager.signal_reference(
            move |is_stubbed| unsafe { (*this).create_fence(is_stubbed) },
            |_fence| {},
            should_wait_queries,
            |fence| fence.is_signaled(),
            pop_queries,
            move || unsafe { (*this).scheduler.has_active_work() },
            || {},
            move || unsafe { (*this).flush_commands_for_fence() },
            move || unsafe { (*this).invalidate_gpu_cache_callback() },
        );
    }

    fn release_fences(&mut self, force: bool) {
        let (should_wait_queries, pop_queries) = self.query_cache.async_flush_callbacks();
        let this = self as *mut Self;
        self.fence_manager.wait_pending_fences(
            force,
            move |is_stubbed| unsafe { (*this).create_fence(is_stubbed) },
            |_fence| {},
            should_wait_queries,
            |fence| fence.is_signaled(),
            |fence| fence.wait_for_fence(),
            pop_queries,
            move || unsafe { (*this).scheduler.has_active_work() },
            || {},
            move || unsafe { (*this).flush_commands_for_fence() },
            move || unsafe { (*this).invalidate_gpu_cache_callback() },
        );
    }

    fn flush_all(&mut self) {}

    fn flush_region(&mut self, addr: u64, size: u64, which: CacheType) {
        if addr == 0 || size == 0 {
            return;
        }
        if which.contains(CacheType::TEXTURE_CACHE) {
            let mutex: *const _ = &self.texture_cache.base.mutex;
            let _guard = unsafe { (*mutex).lock() };
            self.texture_cache.base.download_memory(addr, size as usize);
        }
        if which.contains(CacheType::BUFFER_CACHE) {
            let mutex: *const _ = &self.common_buffer_cache.mutex;
            let _guard = unsafe { (*mutex).lock() };
            self.common_buffer_cache.download_memory(addr, size);
        }
        if which.contains(CacheType::QUERY_CACHE)
            && self.query_cache.flush_region(
                addr, size as usize, self.shader_cache.device_memory(),
            )
        {
            // Query callbacks lock the report owner; flush_region has released
            // that lock before RequestGuestHostSync drains the pending fences.
            self.release_fences(true);
        }
    }

    fn must_flush_region(&self, addr: u64, size: u64, which: CacheType) -> bool {
        if which.contains(CacheType::BUFFER_CACHE) {
            let _guard = self.common_buffer_cache.mutex.lock();
            if self
                .common_buffer_cache
                .is_region_gpu_modified(addr, size as usize)
            {
                return true;
            }
        }
        if !common::settings::is_gpu_level_high(&common::settings::values()) {
            return false;
        }
        if which.contains(CacheType::TEXTURE_CACHE) {
            let _guard = self.texture_cache.base.mutex.lock();
            return self
                .texture_cache
                .base
                .is_region_gpu_modified(addr, size as usize);
        }
        false
    }

    fn get_flush_area(&self, addr: u64, size: u64) -> RasterizerDownloadArea {
        let mutex: *const _ = &self.texture_cache.base.mutex;
        let _guard = unsafe { (*mutex).lock() };
        let cache = &*self.texture_cache as *const MetalTextureCache as *mut MetalTextureCache;
        if let Some(area) = unsafe { (*cache).base.get_flush_area(addr, size as usize) } {
            return area;
        }
        const PAGE: u64 = 4096;
        RasterizerDownloadArea {
            start_address: addr & !(PAGE - 1),
            end_address: (addr + size + PAGE - 1) & !(PAGE - 1),
            preemtive: true,
        }
    }

    fn invalidate_region(&mut self, addr: u64, size: u64, which: CacheType) {
        if addr == 0 || size == 0 {
            return;
        }
        if which.contains(CacheType::TEXTURE_CACHE) {
            let mutex: *const _ = &self.texture_cache.base.mutex;
            let _guard = unsafe { (*mutex).lock() };
            self.texture_cache.base.write_memory(addr, size as usize);
        }
        if which.contains(CacheType::BUFFER_CACHE) {
            let mutex: *const _ = &self.common_buffer_cache.mutex;
            let _guard = unsafe { (*mutex).lock() };
            self.common_buffer_cache.write_memory(addr, size);
        }
        if which.contains(CacheType::QUERY_CACHE) {
            self.query_cache.invalidate_region(addr, size as usize);
        }
        if which.contains(CacheType::SHADER_CACHE) {
            self.shader_cache.invalidate_region(addr, size as usize);
        }
    }

    fn inner_invalidation(&mut self, sequences: &[(u64, usize)]) {
        // Match RasterizerInterface's setting gate despite overriding its body.
        if *common::settings::values().skip_cpu_inner_invalidation.get_value() {
            return;
        }
        let texture_mutex: *const _ = &self.texture_cache.base.mutex;
        let _texture_guard = unsafe { (*texture_mutex).lock() };
        for &(addr, size) in sequences {
            self.texture_cache.base.write_memory(addr, size);
        }
        drop(_texture_guard);
        let buffer_mutex: *const _ = &self.common_buffer_cache.mutex;
        let _buffer_guard = unsafe { (*buffer_mutex).lock() };
        for &(addr, size) in sequences {
            self.common_buffer_cache.write_memory(addr, size as u64);
        }
        drop(_buffer_guard);
        for &(addr, size) in sequences {
            self.query_cache.invalidate_region(addr, size);
            self.shader_cache.invalidate_region(addr, size);
        }
    }

    fn on_cache_invalidation(&mut self, addr: u64, size: u64) {
        if addr == 0 || size == 0 {
            return;
        }
        let texture_mutex: *const _ = &self.texture_cache.base.mutex;
        let _texture_guard = unsafe { (*texture_mutex).lock() };
        self.texture_cache.base.write_memory(addr, size as usize);
        drop(_texture_guard);
        let buffer_mutex: *const _ = &self.common_buffer_cache.mutex;
        let _buffer_guard = unsafe { (*buffer_mutex).lock() };
        self.common_buffer_cache.write_memory(addr, size);
        drop(_buffer_guard);
        self.shader_cache.on_cache_invalidation(addr, size as usize);
    }

    fn on_cpu_write(&mut self, addr: u64, size: u64) -> bool {
        debug_assert!(addr != 0 || size != 0);
        let buffer_mutex: *const _ = &self.common_buffer_cache.mutex;
        let _buffer_guard = unsafe { (*buffer_mutex).lock() };
        let handled = self.common_buffer_cache.on_cpu_write(addr, size);
        drop(_buffer_guard);
        if handled {
            return true;
        }
        let texture_mutex: *const _ = &self.texture_cache.base.mutex;
        let _texture_guard = unsafe { (*texture_mutex).lock() };
        self.texture_cache.base.write_memory(addr, size as usize);
        drop(_texture_guard);
        self.shader_cache.invalidate_region(addr, size as usize);
        false
    }

    fn invalidate_gpu_cache(&mut self) {
        self.invalidate_gpu_cache_callback();
    }

    fn unmap_memory(&mut self, addr: u64, size: u64) {
        let texture_mutex: *const _ = &self.texture_cache.base.mutex;
        let _texture_guard = unsafe { (*texture_mutex).lock() };
        self.texture_cache.base.unmap_memory(addr, size as usize);
        drop(_texture_guard);
        let buffer_mutex: *const _ = &self.common_buffer_cache.mutex;
        let _buffer_guard = unsafe { (*buffer_mutex).lock() };
        self.common_buffer_cache.write_memory(addr, size);
        drop(_buffer_guard);
        self.shader_cache.on_cache_invalidation(addr, size as usize);
    }

    fn modify_gpu_memory(&mut self, as_id: usize, addr: u64, size: u64) {
        let mutex: *const _ = &self.texture_cache.base.mutex;
        let _guard = unsafe { (*mutex).lock() };
        self.texture_cache
            .base
            .unmap_gpu_memory(as_id, addr, size as usize);
    }

    fn flush_and_invalidate_region(&mut self, addr: u64, size: u64, which: CacheType) {
        if common::settings::is_gpu_level_high(&common::settings::values()) {
            self.flush_region(addr, size, which);
        }
        self.invalidate_region(addr, size, which);
    }

    fn wait_for_idle(&mut self) {
        self.notify_query_wfi();
        self.scheduler
            .request_outside_render_pass_operation_context();
        let (should_wait_queries, pop_queries) = self.query_cache.async_flush_callbacks();
        let this = self as *mut Self;
        self.fence_manager.signal_ordering(
            should_wait_queries,
            |fence| fence.is_signaled(),
            pop_queries,
            move || unsafe { (*this).common_buffer_cache.flush_cached_writes() },
        );
    }

    fn fragment_barrier(&mut self) {
        self.scheduler
            .request_outside_render_pass_operation_context();
    }

    fn tiled_cache_barrier(&mut self) {}

    fn flush_commands(&mut self) {
        self.flush_commands_for_fence();
    }

    fn tick_frame(&mut self) {
        self.fence_manager.tick_frame();
        MetalRasterizer::tick_frame(self);
    }

    fn access_accelerate_dma(&mut self) -> &mut dyn AccelerateDMAInterface {
        &mut self.accelerate_dma
    }

    fn accelerate_inline_to_memory(&mut self, address: u64, copy_size: usize, memory: &[u8]) {
        debug_assert!(copy_size <= memory.len());
        if copy_size == 0 {
            return;
        }
        let Some(memory_manager) = self.channel_memory_manager.as_ref().cloned() else {
            return;
        };
        let memory = unsafe { std::slice::from_raw_parts(memory.as_ptr(), copy_size) };
        let manager = memory_manager.lock();
        let cpu_addr = manager.gpu_to_cpu_address(address);
        if cpu_addr.is_none() {
            manager.write_block(address, memory);
            return;
        }
        manager.write_block_unsafe(address, memory);
        drop(manager);
        let cpu_addr = cpu_addr.unwrap();
        let buffer_mutex: *const _ = &self.common_buffer_cache.mutex;
        let _buffer_guard = unsafe { (*buffer_mutex).lock() };
        if !self
            .common_buffer_cache
            .inline_memory(cpu_addr, copy_size, memory)
        {
            self.common_buffer_cache
                .write_memory(cpu_addr, copy_size as u64);
        }
        drop(_buffer_guard);
        let texture_mutex: *const _ = &self.texture_cache.base.mutex;
        let _texture_guard = unsafe { (*texture_mutex).lock() };
        self.texture_cache.base.write_memory(cpu_addr, copy_size);
        drop(_texture_guard);
        self.shader_cache.invalidate_region(cpu_addr, copy_size);
        self.query_cache.invalidate_region(cpu_addr, copy_size);
    }

    fn initialize_channel(&mut self, channel: &mut ChannelState) {
        MetalRasterizer::initialize_channel(self, channel);
    }

    fn bind_channel(&mut self, channel: &mut ChannelState) {
        MetalRasterizer::bind_channel(self, channel);
    }

    fn release_channel(&mut self, channel_id: i32) {
        MetalRasterizer::release_channel(self, channel_id);
    }
}

fn patch_render_area(
    prepared: &mut MetalPreparedGraphics,
    stages: &super::metal_pipeline_cache::MetalGraphicsShaderStages,
    render_area: (u32, u32),
) {
    let words = [render_area.0 as f32, render_area.1 as f32, 0.0, 0.0];
    let bytes = bytemuck::cast_slice::<f32, u8>(&words);
    for (info, stage) in stages.stage_infos().iter().zip([
        &mut prepared.vertex, &mut prepared.control, &mut prepared.evaluation,
        &mut prepared.geometry, &mut prepared.fragment,
    ]) {
        if info.uses_render_area {
            if let Some((_, data)) = stage.push_constants.as_mut() {
                data[..16].copy_from_slice(bytes);
            }
        }
    }
}

// Mechanical extraction of Eden UpdateViewportsState's scale/offset-disabled branch.
fn surface_clip_viewport(draw: &Maxwell3DDrawView<'_>) -> MTLViewport {
    let surface = draw.surface_clip();
    let mut y = surface.y as f64;
    let mut height = surface.height.max(1) as f64;
    if draw.window_origin_lower_left() {
        y += height;
        height = -height;
    }
    MTLViewport {
        originX: surface.x as f64,
        originY: y + height,
        width: surface.width.max(1) as f64,
        height: -height,
        znear: 0.0,
        zfar: 1.0,
    }
}

// Eden GetViewportState, followed by conversion from Vulkan's downward NDC Y
// to Metal's upward NDC Y. Signed extents and off-attachment origins are valid.
fn get_viewport_state(draw: &Maxwell3DDrawView<'_>, index: usize) -> MTLViewport {
    let source = draw.viewport_transform(index);
    let x = source.translate_x - source.scale_x;
    let mut y = source.translate_y - source.scale_y;
    let width = source.scale_x * 2.0;
    let mut height = source.scale_y * 2.0;
    if draw.window_origin_lower_left() {
        y += draw.surface_clip().height as f32;
        height = -height;
    }
    // Native Metal has no NV viewport-swizzle state. Eden handles NegativeY
    // here when that extension is unavailable.
    if (source.swizzle >> 4) & 7 == ViewportSwizzle::NegativeY as u32 {
        y += height;
        height = -height;
    }
    let reduce_z = if draw.depth_mode() == crate::engines::maxwell_3d::DepthMode::MinusOneToOne {
        1.0
    } else {
        0.0
    };
    MTLViewport {
        originX: x as f64,
        originY: y as f64 + if height == 0.0 { 1.0 } else { height as f64 },
        width: if width == 0.0 { 1.0 } else { width as f64 },
        height: if height == 0.0 { -1.0 } else { -(height as f64) },
        znear: (source.translate_z - source.scale_z * reduce_z).clamp(0.0, 1.0) as f64,
        zfar: (source.translate_z + source.scale_z).clamp(0.0, 1.0) as f64,
    }
}

// The scale/offset-disabled branch of Eden UpdateScissorsState, intersected
// with the Metal attachment. Kept pure so native tests exercise this conversion.
fn surface_clip_scissor(
    draw: &Maxwell3DDrawView<'_>,
    render_area: (u32, u32),
) -> MTLScissorRect {
    let surface = draw.surface_clip();
    let x = i64::from(surface.x);
    let y = if draw.window_origin_lower_left() {
        i64::from(surface.height) - i64::from(surface.y) - i64::from(surface.height.max(1))
    } else {
        i64::from(surface.y)
    };
    let bound_x = |value: i64| value.clamp(0, i64::from(render_area.0)) as usize;
    let bound_y = |value: i64| value.clamp(0, i64::from(render_area.1)) as usize;
    let left = bound_x(x);
    let top = bound_y(y);
    MTLScissorRect {
        x: left,
        y: top,
        width: bound_x(x + i64::from(surface.width.max(1))) - left,
        height: bound_y(y + i64::from(surface.height.max(1))) - top,
    }
}

// Eden GetScissorState: flip by surface clip height before bounding Y.
// Metal additionally requires intersection with the actual attachment extent.
fn get_scissor_state(
    draw: &Maxwell3DDrawView<'_>,
    index: usize,
    render_area: (u32, u32),
) -> MTLScissorRect {
    let source = draw.scissor(index);
    if !source.enabled {
        return MTLScissorRect {
            x: 0,
            y: 0,
            width: render_area.0 as usize,
            height: render_area.1 as usize,
        };
    }
    let (min_y, max_y) = if draw.window_origin_lower_left() {
        let height = i64::from(draw.surface_clip().height);
        (
            (height - i64::from(source.max_y)).max(0) as u32,
            (height - i64::from(source.min_y)).max(0) as u32,
        )
    } else {
        (source.min_y, source.max_y)
    };
    let min_x = source.min_x.min(render_area.0);
    let min_y = min_y.min(render_area.1);
    let max_x = source.max_x.min(render_area.0).max(min_x);
    let max_y = max_y.min(render_area.1).max(min_y);
    MTLScissorRect {
        x: min_x as usize,
        y: min_y as usize,
        width: (max_x - min_x) as usize,
        height: (max_y - min_y) as usize,
    }
}

impl Drop for MetalRasterizer {
    fn drop(&mut self) {
        <Self as RasterizerInterface>::release_fences(self, true);
        if let Err(error) = self.scheduler.finish_all() {
            log::error!("Metal rasterizer shutdown failed: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::draw_manager::{DrawState, Maxwell3DDrawRegisters};
    use crate::engines::maxwell_3d::{DepthMode, ScissorInfo, SurfaceClipInfo, ViewportTransformInfo};

    #[test]
    fn viewport_transform_preserves_signed_extents_and_origin() {
        let state = DrawState::default();
        for (sx, sy, lower_left, swizzle, expected) in [
            (2.0, 2.0, false, 0x6420, [1.0, 7.0, 4.0, -4.0]),
            (-2.0, -2.0, false, 0x6420, [5.0, 3.0, -4.0, 4.0]),
            (2.0, 2.0, true, 0x6420, [1.0, 9.0, 4.0, 4.0]),
            (2.0, 2.0, false, 0x6430, [1.0, 3.0, 4.0, 4.0]),
            (2.0, 2.0, true, 0x6430, [1.0, 13.0, 4.0, -4.0]),
            (0.0, 0.0, false, 0x6420, [3.0, 6.0, 1.0, -1.0]),
            (0.25, 0.25, false, 0x6420, [2.75, 5.25, 0.5, -0.5]),
        ] {
            let mut registers = Maxwell3DDrawRegisters::default();
            registers.surface_clip.height = 10;
            registers.window_origin_lower_left = lower_left;
            registers.depth_mode = DepthMode::MinusOneToOne;
            registers.viewport_transforms[7] = ViewportTransformInfo {
                scale_x: sx, scale_y: sy, scale_z: 0.5,
                translate_x: 3.0, translate_y: 5.0, translate_z: 0.25,
                swizzle, ..Default::default()
            };
            let draw = Maxwell3DDrawView::with_register_snapshot(&state, false, registers);
            let viewport = get_viewport_state(&draw, 7);
            assert_eq!([viewport.originX, viewport.originY, viewport.width, viewport.height], expected);
            assert_eq!([viewport.znear, viewport.zfar], [0.0, 0.75]);
        }
        let mut registers = Maxwell3DDrawRegisters::default();
        registers.viewport_transforms[0] = ViewportTransformInfo {
            scale_x: 10.0, scale_y: 20.0, translate_x: 2.0, translate_y: 3.0,
            ..Default::default()
        };
        let draw = Maxwell3DDrawView::with_register_snapshot(&state, false, registers);
        let viewport = get_viewport_state(&draw, 0);
        assert_eq!([viewport.originX, viewport.originY, viewport.width, viewport.height], [-8.0, 23.0, 20.0, -40.0]);
    }

    #[test]
    fn viewport_disabled_transform_preserves_surface_extent_and_origin() {
        let state = DrawState::default();
        for (lower_left, expected_y, expected_height) in [(false, 8.0, -5.0), (true, 3.0, 5.0)] {
            let registers = Maxwell3DDrawRegisters {
                window_origin_lower_left: lower_left,
                surface_clip: SurfaceClipInfo { x: 2, y: 3, width: 0, height: 5 },
                ..Default::default()
            };
            let draw = Maxwell3DDrawView::with_register_snapshot(&state, false, registers);
            let viewport = surface_clip_viewport(&draw);
            assert_eq!([viewport.originX, viewport.originY, viewport.width, viewport.height], [2.0, expected_y, 1.0, expected_height]);
        }
    }

    #[test]
    fn native_viewport_arrays_route_signed_transforms_and_matching_scissors() {
        use objc2_foundation::NSString;
        use objc2_metal::{
            MTLBlitCommandEncoder, MTLClearColor, MTLDevice, MTLLibrary, MTLLoadAction,
            MTLOrigin, MTLPixelFormat, MTLRenderPassDescriptor, MTLRenderPipelineDescriptor,
            MTLStoreAction, MTLTextureDescriptor, MTLTextureUsage,
        };
        use super::super::metal_buffer::MetalBuffer;

        let device = MetalDevice::new().unwrap();
        let count = device.profile().max_viewports().min(NUM_VIEWPORTS);
        let member = if count > 1 { "uint viewport [[viewport_array_index]];" } else { "" };
        let store = if count > 1 { "output.viewport = id;" } else { "" };
        let shader = NSString::from_str(&format!(r#"
#include <metal_stdlib>
using namespace metal;
struct Out {{ float4 position [[position]]; float size [[point_size]]; {member} }};
vertex Out vs(uint id [[vertex_id]]) {{
    Out output; output.position = float4(-0.25,-0.25,0,1); output.size = 1;
    {store} return output;
}}
fragment float4 fs() {{ return float4(1,0,0,1); }}
"#));
        let library = device.device().newLibraryWithSource_options_error(&shader, None).unwrap();
        let descriptor = MTLRenderPipelineDescriptor::new();
        let vertex = library.newFunctionWithName(&NSString::from_str("vs")).unwrap();
        let fragment = library.newFunctionWithName(&NSString::from_str("fs")).unwrap();
        descriptor.setVertexFunction(Some(&vertex));
        descriptor.setFragmentFunction(Some(&fragment));
        unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(0) }
            .setPixelFormat(MTLPixelFormat::RGBA8Unorm);
        let pipeline = device.device().newRenderPipelineStateWithDescriptor_error(&descriptor).unwrap();
        let td = MTLTextureDescriptor::new();
        td.setPixelFormat(MTLPixelFormat::RGBA8Unorm);
        td.setUsage(MTLTextureUsage::RenderTarget);
        unsafe { td.setWidth(16); td.setHeight(16); }
        let texture = device.device().newTextureWithDescriptor(&td).unwrap();
        let pass = MTLRenderPassDescriptor::renderPassDescriptor();
        let attachment = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
        attachment.setTexture(Some(&texture));
        attachment.setLoadAction(MTLLoadAction::Clear);
        attachment.setStoreAction(MTLStoreAction::Store);
        attachment.setClearColor(MTLClearColor { red: 0.0, green: 0.0, blue: 0.0, alpha: 0.0 });
        let mut rasterizer = MetalRasterizer::new(
            device.clone(), Arc::new(SyncpointManager::new()),
            Arc::new(MaxwellDeviceMemoryManager::default()),
        ).unwrap();
        let download = MetalBuffer::new(&device, 4096).unwrap();
        let state = DrawState::default();
        let mut registers = Maxwell3DDrawRegisters::default();
        registers.viewport_scale_offset_enabled = true;
        for index in 0..NUM_VIEWPORTS {
            let x = (index % 4 * 4) as u32;
            let y = (index / 4 * 4) as u32;
            registers.viewport_transforms[index] = ViewportTransformInfo {
                scale_x: if index & 1 == 0 { 2.0 } else { -2.0 },
                scale_y: if index & 2 == 0 { 2.0 } else { -2.0 },
                translate_x: (x + 2) as f32, translate_y: (y + 2) as f32,
                scale_z: 1.0, swizzle: 0x6420, ..Default::default()
            };
            registers.scissors[index] = ScissorInfo {
                enabled: true, min_x: x, max_x: if index == 5 { x } else { x + 4 },
                min_y: y, max_y: y + 4,
            };
        }
        let draw = Maxwell3DDrawView::with_register_snapshot(&state, false, registers);
        rasterizer.scheduler.begin_render_pass(&pass).unwrap();
        rasterizer.update_viewports_state(&draw).unwrap();
        rasterizer.update_scissors_state(&draw, (16, 16)).unwrap();
        rasterizer.scheduler.with_render_encoder(|encoder| {
            encoder.setRenderPipelineState(&pipeline);
            unsafe { encoder.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::Point, 0, count) };
        }).unwrap();
        rasterizer.scheduler.with_blit_encoder(|encoder| unsafe {
            encoder.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toBuffer_destinationOffset_destinationBytesPerRow_destinationBytesPerImage(
                &texture, 0, 0, MTLOrigin { x: 0, y: 0, z: 0 }, MTLSize { width: 16, height: 16, depth: 1 },
                download.handle(), 0, 256, 4096,
            );
        }).unwrap();
        rasterizer.finish().unwrap();
        let mut pixels = [0; 4096];
        download.read(0, &mut pixels).unwrap();
        for y in 0..16 {
            for x in 0..16 {
                let index = y / 4 * 4 + x / 4;
                let expected = index < count && index != 5
                    && x % 4 == if index & 1 == 0 { 1 } else { 2 }
                    && y % 4 == if index & 2 == 0 { 1 } else { 2 };
                let offset = y * 256 + x * 4;
                assert_eq!(&pixels[offset..offset + 4], if expected { &[255, 0, 0, 255] } else { &[0, 0, 0, 0] },
                    "viewport={index} pixel=({x},{y})");
            }
        }
    }

    fn scissor_cases() -> Vec<(ScissorInfo, bool, [usize; 4])> {
        [
            (false, false, [9, 12, 9, 12], [0, 0, 4, 4]),
            (true, false, [1, 3, 0, 2], [1, 0, 2, 2]),
            (true, true, [1, 3, 3, 5], [1, 1, 2, 2]),
            (true, true, [0, 4, 5, 8], [0, 0, 4, 1]),
            (true, false, [2, 2, 0, 4], [2, 0, 0, 4]),
            (true, false, [0, 4, 2, 2], [0, 2, 4, 0]),
            (true, false, [9, 12, 9, 12], [4, 4, 0, 0]),
            (true, true, [0, 4, 7, 8], [0, 0, 4, 0]),
        ]
        .into_iter()
        .map(|(enabled, lower_left, bounds, expected)| {
            (
                ScissorInfo {
                    enabled,
                    min_x: bounds[0],
                    max_x: bounds[1],
                    min_y: bounds[2],
                    max_y: bounds[3],
                },
                lower_left,
                expected,
            )
        })
        .collect()
    }

    fn surface_clip_cases() -> Vec<(SurfaceClipInfo, bool, [usize; 4])> {
        [
            ([1, 1, 2, 2], false, [1, 1, 2, 2]),
            ([1, 1, 2, 2], true, [1, 0, 2, 1]),
            ([3, 3, 8, 8], false, [3, 3, 1, 1]),
            ([9, 9, 2, 2], false, [4, 4, 0, 0]),
            ([0, 0, 0, 0], false, [0, 0, 1, 1]),
            ([0, 0, 0, 0], true, [0, 0, 1, 0]),
        ].into_iter().map(|([x, y, width, height], lower_left, expected)| {
            (SurfaceClipInfo { x, y, width, height }, lower_left, expected)
        }).collect()
    }

    #[test]
    fn disabled_viewport_transform_uses_surface_clip_intersection() {
        let state = DrawState::default();
        for (surface_clip, lower_left, expected) in surface_clip_cases() {
            let registers = Maxwell3DDrawRegisters {
                surface_clip,
                window_origin_lower_left: lower_left,
                ..Default::default()
            };
            let draw = Maxwell3DDrawView::with_register_snapshot(&state, false, registers);
            let rect = surface_clip_scissor(&draw, (4, 4));
            assert_eq!([rect.x, rect.y, rect.width, rect.height], expected);
        }
    }

    #[test]
    fn scissor_preserves_empty_regions_and_flips_before_intersection() {
        let state = DrawState::default();
        for (source, lower_left, expected) in scissor_cases() {
            let mut registers = Maxwell3DDrawRegisters::default();
            registers.scissors[5] = source;
            registers.window_origin_lower_left = lower_left;
            registers.surface_clip.height = 6;
            let draw = Maxwell3DDrawView::with_register_snapshot(&state, false, registers);
            let rect = get_scissor_state(&draw, 5, (4, 4));
            assert_eq!([rect.x, rect.y, rect.width, rect.height], expected);
        }
    }

    #[test]
    fn native_scissor_clips_pixels_without_dropping_vertex_side_effects() {
        use objc2_foundation::NSString;
        use objc2_metal::{
            MTLBlitCommandEncoder, MTLClearColor, MTLDevice, MTLLibrary, MTLLoadAction,
            MTLOrigin, MTLPixelFormat, MTLRenderPassDescriptor, MTLRenderPipelineDescriptor,
            MTLStoreAction, MTLTextureDescriptor, MTLTextureUsage,
        };
        use super::super::metal_buffer::MetalBuffer;

        let device = MetalDevice::new().unwrap();
        let source = NSString::from_str(r#"
#include <metal_stdlib>
using namespace metal;
vertex float4 vs(uint i [[vertex_id]], device atomic_uint* count [[buffer(0)]]) {
    atomic_fetch_add_explicit(count, 1u, memory_order_relaxed);
    const float2 p[3] = {float2(-1,-1), float2(3,-1), float2(-1,3)};
    return float4(p[i],0,1);
}
fragment float4 fs() { return float4(1,0,0,1); }
"#);
        let library = device.device().newLibraryWithSource_options_error(&source, None).unwrap();
        let descriptor = MTLRenderPipelineDescriptor::new();
        let vertex = library.newFunctionWithName(&NSString::from_str("vs")).unwrap();
        let fragment = library.newFunctionWithName(&NSString::from_str("fs")).unwrap();
        descriptor.setVertexFunction(Some(&vertex));
        descriptor.setFragmentFunction(Some(&fragment));
        unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(0) }
            .setPixelFormat(MTLPixelFormat::RGBA8Unorm);
        let pipeline = device.device().newRenderPipelineStateWithDescriptor_error(&descriptor).unwrap();
        let td = MTLTextureDescriptor::new();
        td.setPixelFormat(MTLPixelFormat::RGBA8Unorm);
        td.setUsage(MTLTextureUsage::RenderTarget);
        unsafe { td.setWidth(4); td.setHeight(4); }
        let texture = device.device().newTextureWithDescriptor(&td).unwrap();
        let pass = MTLRenderPassDescriptor::renderPassDescriptor();
        let attachment = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
        attachment.setTexture(Some(&texture));
        attachment.setLoadAction(MTLLoadAction::Clear);
        attachment.setStoreAction(MTLStoreAction::Store);
        attachment.setClearColor(MTLClearColor { red: 0.0, green: 0.0, blue: 0.0, alpha: 0.0 });
        let counter = MetalBuffer::new(&device, 4).unwrap();
        let download = MetalBuffer::new(&device, 1024).unwrap();
        let mut scheduler = MetalScheduler::new(&device);
        let state = DrawState::default();
        let cases = scissor_cases().into_iter().map(|(source, lower_left, expected)| {
            let mut registers = Maxwell3DDrawRegisters::default();
            registers.scissors[0] = source;
            registers.window_origin_lower_left = lower_left;
            registers.surface_clip.height = 6;
            registers.viewport_scale_offset_enabled = true;
            (registers, expected)
        }).chain(surface_clip_cases().into_iter().map(|(surface_clip, lower_left, expected)| {
            (Maxwell3DDrawRegisters {
                surface_clip,
                window_origin_lower_left: lower_left,
                ..Default::default()
            }, expected)
        }));
        for (registers, [x, y, width, height]) in cases {
            let draw = Maxwell3DDrawView::with_register_snapshot(&state, false, registers);
            let rect = if draw.viewport_scale_offset_enabled() {
                get_scissor_state(&draw, 0, (4, 4))
            } else {
                surface_clip_scissor(&draw, (4, 4))
            };
            counter.write(0, &[0; 4]).unwrap();
            scheduler.begin_render_pass(&pass).unwrap();
            scheduler.with_render_encoder(|encoder| {
                encoder.setRenderPipelineState(&pipeline);
                encoder.setScissorRect(rect);
                unsafe {
                    encoder.setVertexBuffer_offset_atIndex(Some(counter.handle()), 0, 0);
                    encoder.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::Triangle, 0, 3);
                }
            }).unwrap();
            scheduler.with_blit_encoder(|encoder| unsafe {
                encoder.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toBuffer_destinationOffset_destinationBytesPerRow_destinationBytesPerImage(
                    &texture, 0, 0, MTLOrigin { x: 0, y: 0, z: 0 },
                    MTLSize { width: 4, height: 4, depth: 1 }, download.handle(), 0, 256, 1024,
                );
            }).unwrap();
            scheduler.finish_all().unwrap();
            let mut pixels = [0; 1024];
            download.read(0, &mut pixels).unwrap();
            for py in 0..4 {
                for px in 0..4 {
                    let offset = py * 256 + px * 4;
                    let inside = (x..x + width).contains(&px) && (y..y + height).contains(&py);
                    assert_eq!(
                        &pixels[offset..offset + 4],
                        if inside { &[255, 0, 0, 255] } else { &[0, 0, 0, 0] },
                        "rect={rect:?}, pixel=({px},{py})",
                    );
                }
            }
            let mut count = [0; 4];
            counter.read(0, &mut count).unwrap();
            assert_eq!(u32::from_ne_bytes(count), 3, "empty scissors must not skip draws");
        }
    }

    #[test]
    fn owns_one_scheduler_and_shared_cache_runtime() {
        let device = MetalDevice::new().expect("Metal device must exist on macOS test hosts");
        let device_memory = Arc::new(MaxwellDeviceMemoryManager::default());
        let syncpoints = Arc::new(SyncpointManager::new());
        let mut rasterizer = MetalRasterizer::new(device, syncpoints, device_memory).unwrap();

        let initial_tick = rasterizer.scheduler().current_tick();
        rasterizer.tick_frame();
        rasterizer.finish().unwrap();
        assert_eq!(rasterizer.scheduler().current_tick(), initial_tick);
    }

    #[test]
    fn live_conditional_hook_masks_quad_without_submitting_or_reading_back() {
        use crate::query_cache::query_cache::{RenderConditionState, SyncValuesStruct};
        use crate::query_cache::types::ComparisonMode;
        use super::super::metal_buffer::MetalBuffer;

        let _accuracy = crate::test_support::GpuAccuracyGuard::set(
            common::settings_enums::GpuAccuracy::High,
        );
        let device = MetalDevice::new().unwrap();
        let memory = Arc::new(MaxwellDeviceMemoryManager::default());
        let mut backing = vec![0u8; 0x1000];
        memory.smmu_set_physical_base_for_test(backing.as_mut_ptr() as usize);
        memory.smmu_map_with_cpu_backing(
            0x8000, backing.as_mut_ptr(), 0x4000, backing.len(), 1, true,
        );
        let mut channel_memory = MemoryManager::new_with_geometry_and_device_memory(
            1, memory.clone(), 32, 0x1_0000_0000, 16, 12,
        );
        channel_memory.map(0x10000, 0x8000, 0x1000, 0, false);
        let readback = MetalBuffer::new(&device, 16).unwrap();
        let mut rasterizer = MetalRasterizer::new(
            device, Arc::new(SyncpointManager::new()), memory.clone(),
        ).unwrap();
        rasterizer.channel_memory_manager = Some(Arc::new(parking_lot::Mutex::new(channel_memory)));
        let channel = ChannelState::new(1);
        rasterizer.common_buffer_cache.create_channel(&channel);
        rasterizer.common_buffer_cache.bind_to_channel(1);
        let mut state = RenderConditionState {
            override_mode: 0,
            comparison_mode: ComparisonMode::IfEqual,
            address: 0x10100,
        };
        for (value, instances) in [(0x100000004, 0), (0x100000003, 1)] {
            rasterizer.query_cache_runtime.sync_values(
                rasterizer.common_buffer_cache.as_mut(),
                &[
                    SyncValuesStruct { address: 0x8100, value: 0x100000003, size: 8 },
                    SyncValuesStruct { address: 0x8110, value, size: 8 },
                ], None,
            ).unwrap();
            rasterizer.common_buffer_cache.obtain_cpu_buffer(
                0x8100, 24, ObtainBufferSynchronize::NoSynchronize,
                ObtainBufferOperation::MarkAsWritten,
            );
            let tick = rasterizer.scheduler.current_tick();
            assert!(rasterizer.accelerate_conditional_rendering_with_state(state));
            let arguments = rasterizer.conditional_quad_arguments().unwrap().unwrap();
            assert_eq!(rasterizer.scheduler.current_tick(), tick);
            assert_eq!(memory.read_u32(0x8100), 0, "predicate stays GPU-owned");
            arguments.buffer.encode_copy(
                rasterizer.scheduler.as_mut(), &readback, arguments.offset, 0, 16,
            ).unwrap();
            rasterizer.finish().unwrap();
            let mut bytes = [0; 16];
            readback.read(0, &mut bytes).unwrap();
            let words: Vec<u32> = bytes.chunks_exact(4)
                .map(|word| u32::from_ne_bytes(word.try_into().unwrap())).collect();
            assert_eq!(words, [4, instances, 0, 0]);
        }
        state.comparison_mode = ComparisonMode::True;
        assert!(!rasterizer.accelerate_conditional_rendering_with_state(state));
        assert!(rasterizer.conditional_quad_arguments().unwrap().is_none());
    }

    #[test]
    fn scoped_flush_region_live_rasterizer_routes_query_cache_flag() {
        let _accuracy = crate::test_support::GpuAccuracyGuard::set(
            common::settings_enums::GpuAccuracy::High,
        );
        let device = MetalDevice::new().unwrap();
        let memory = Arc::new(MaxwellDeviceMemoryManager::default());
        let mut backing = vec![0xa5u8; 0x1000];
        memory.smmu_set_physical_base_for_test(backing.as_mut_ptr() as usize);
        memory.smmu_map_with_cpu_backing(0x8000, backing.as_mut_ptr(), 0x4000,
            backing.len(), 1, true);
        let mut channel_memory = MemoryManager::new_with_geometry_and_device_memory(
            1, memory.clone(), 32, 0x1_0000_0000, 16, 12,
        );
        channel_memory.map(0x10000, 0x8000, 0x1000, 0, false);
        let mut rasterizer = MetalRasterizer::new(
            device, Arc::new(SyncpointManager::new()), memory.clone(),
        ).unwrap();
        rasterizer.channel_memory_manager = Some(Arc::new(parking_lot::Mutex::new(channel_memory)));
        rasterizer.query(0x10020, crate::query_cache::types::QueryType::Payload as u32,
            QueryPropertiesFlags::empty(), 0x12345678, 0);
        assert_eq!(memory.read_u32(0x8020), 0xa5a5a5a5);
        rasterizer.flush_region(0x8020, 4, CacheType::empty());
        assert_eq!(memory.read_u32(0x8020), 0xa5a5a5a5);
        let tick = rasterizer.scheduler.current_tick();
        rasterizer.flush_region(0x8020, 4, CacheType::QUERY_CACHE);
        assert_eq!(memory.read_u32(0x8020), 0x12345678);
        assert_eq!(memory.read_u32(0x8024), 0xa5a5a5a5);
        assert_eq!(rasterizer.scheduler.current_tick(), tick);

        // Exercise RequestGuestHostSync through the live ReleaseFences path.
        // An allocated visibility slot with no fragments has a real GPU zero;
        // the CPU destination must remain unchanged until the query is flushed.
        let channel = ChannelState::new(1);
        rasterizer.common_buffer_cache.create_channel(&channel);
        rasterizer.common_buffer_cache.bind_to_channel(1);
        rasterizer.query_cache.prepare_draw(rasterizer.scheduler.as_mut(), true).unwrap();
        rasterizer.query(0x10040,
            crate::query_cache::types::QueryType::ZPassPixelCount64 as u32,
            QueryPropertiesFlags::IS_A_FENCE, 0, 0);
        assert_eq!(memory.read_u32(0x8040), 0xa5a5a5a5);
        rasterizer.flush_region(0x8040, 4, CacheType::QUERY_CACHE);
        assert_eq!(memory.read_u32(0x8040), 0);
        assert_eq!(memory.read_u32(0x8044), 0xa5a5a5a5);
    }
}
