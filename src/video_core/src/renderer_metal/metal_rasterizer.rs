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
    MTLCullMode, MTLPrimitiveType, MTLRenderCommandEncoder, MTLScissorRect, MTLViewport,
    MTLWinding,
};
use thiserror::Error;

use crate::buffer_cache::buffer_cache_base::{DeviceMemoryAccess, GpuMemoryAccess};
use crate::cache_types::CacheType;
use crate::control::channel_state::ChannelState;
use crate::control::channel_state_cache::{ChannelCacheAccessor, ChannelInfo, ChannelSetupCaches};
use crate::engines::draw_manager::{Maxwell3DDrawTextureView, Maxwell3DDrawView};
use crate::engines::maxwell_3d::{CullFace, FrontFace, PrimitiveTopology};
use crate::engines::maxwell_dma::{dma, AccelerateDMAInterface};
use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
use crate::host1x::syncpoint_manager::SyncpointManager;
use crate::memory_manager::MemoryManager;
use crate::query_cache::types::QueryPropertiesFlags;
use crate::rasterizer_interface::{RasterizerDownloadArea, RasterizerInterface};
use crate::renderer_base::{
    GpuTickCallback, GpuTicksGetter, GuestMemoryWriter, InvalidateGpuCacheCallback,
};
use crate::shader_cache::ShaderCache;

use super::metal_blit_helper::{MetalBlitError, MetalBlitHelper, MetalBlitRegion};
use super::metal_buffer_cache::{BufferCacheRuntime, MetalCommonBufferCache};
use super::metal_device::MetalDevice;
use super::metal_framebuffer::MetalFramebufferError;
use super::metal_graphics_pipeline::{
    configure_graphics_resources, MetalGraphicsPipelineError, MetalPreparedGraphics,
    MetalPreparedStage,
};
use super::metal_pipeline_cache::MetalPipelineCache;
use super::metal_pipeline_cache::MetalPipelineError;
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
    #[error(transparent)]
    Pipeline(#[from] MetalPipelineError),
    #[error(transparent)]
    GraphicsPipeline(#[from] MetalGraphicsPipelineError),
    #[error(transparent)]
    Framebuffer(#[from] MetalFramebufferError),
    #[error(transparent)]
    Blit(#[from] MetalBlitError),
    #[error("Metal does not support Maxwell primitive topology {0:?}")]
    UnsupportedTopology(PrimitiveTopology),
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

fn metal_primitive_type(topology: PrimitiveTopology) -> Result<MTLPrimitiveType, MetalRasterizerError> {
    match topology {
        PrimitiveTopology::Points => Ok(MTLPrimitiveType::Point),
        PrimitiveTopology::Lines => Ok(MTLPrimitiveType::Line),
        PrimitiveTopology::LineStrip | PrimitiveTopology::LineLoop => Ok(MTLPrimitiveType::LineStrip),
        PrimitiveTopology::Triangles | PrimitiveTopology::Quads | PrimitiveTopology::QuadStrip => {
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
                encoder.setVertexTexture_atIndex(
                    binding.texture.as_deref(),
                    binding.index as usize,
                );
            } else {
                encoder.setFragmentTexture_atIndex(
                    binding.texture.as_deref(),
                    binding.index as usize,
                );
            }
        }
        for binding in &stage.samplers {
            if vertex {
                encoder.setVertexSamplerState_atIndex(
                    Some(&binding.sampler),
                    binding.index as usize,
                );
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
    blit_image: MetalBlitHelper,
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

        let texture_cache = Box::new(MetalTextureCache::new(
            device.clone(),
            Arc::clone(&device_memory),
            scheduler.as_mut(),
            staging_pool.as_mut(),
        ));
        let shader_cache = ShaderCache::new(device_memory);
        let pipeline_cache = MetalPipelineCache::new(device.clone());
        let blit_image = MetalBlitHelper::new(&device)?;
        let accelerate_dma = AccelerateDMA::new(common_buffer_cache.as_mut());

        Ok(Self {
            device,
            scheduler,
            staging_pool,
            pipeline_cache,
            shader_cache,
            common_buffer_cache,
            texture_cache,
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

    /// Port of Eden `RasterizerVulkan::Draw`/`PrepareDraw` to a native Metal
    /// render encoder. Cache preparation remains ordered exactly like the
    /// upstream path; only the final API bindings differ.
    pub fn draw(
        &mut self,
        draw: &mut Maxwell3DDrawView<'_>,
        instance_count: u32,
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

        let (render_pass, render_area, pipeline_key) = {
            let framebuffer = self.texture_cache.base.get_framebuffer()?;
            let render_area = framebuffer.render_area();
            let pipeline_key = self
                .pipeline_cache
                .make_render_pipeline_key(&stages, framebuffer)?;
            (framebuffer.render_pass_descriptor(), render_area, pipeline_key)
        };

        patch_render_area(&mut prepared, &stages, render_area);
        let pipeline_state = self
            .pipeline_cache
            .get_or_create_render_pipeline(
                pipeline_key,
                stages.vertex(),
                stages.fragment(),
            )?
            .retained_state();
        let depth_key = self
            .pipeline_cache
            .make_depth_stencil_key(&stages, &draw.depth_stencil());
        let depth_state = self
            .pipeline_cache
            .retained_depth_stencil_state(depth_key)?;
        let primitive_type = metal_primitive_type(draw.draw_state().topology)?;
        let draw_params = make_draw_params(draw, instance_count);
        let rasterizer = draw.rasterizer();
        if rasterizer.cull_enable && rasterizer.cull_face == CullFace::FrontAndBack {
            return Ok(());
        }
        let blend_color = draw.blend_color();
        let depth_stencil = draw.depth_stencil();
        let viewport = metal_viewport(draw, render_area);
        let scissor = metal_scissor(draw, render_area);
        let vertex_layouts = pipeline_key.vertex_input.layouts;

        self.scheduler.begin_render_pass(&render_pass)?;
        self.scheduler.with_render_encoder(|encoder| {
            encoder.setRenderPipelineState(&pipeline_state);
            encoder.setDepthStencilState(Some(&depth_state));
            encoder.setViewport(viewport);
            encoder.setScissorRect(scissor);
            encoder.setCullMode(if rasterizer.cull_enable {
                match rasterizer.cull_face {
                    CullFace::Front => MTLCullMode::Front,
                    CullFace::Back => MTLCullMode::Back,
                    CullFace::FrontAndBack => unreachable!("front-and-back culling returned above"),
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

            bind_stage(encoder, &prepared.vertex, true);
            bind_stage(encoder, &prepared.fragment, false);
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

                if draw_params.is_indexed {
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
        })?;
        Ok(())
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
        )?;
        Ok(())
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

fn patch_render_area(
    prepared: &mut MetalPreparedGraphics,
    stages: &super::metal_pipeline_cache::MetalGraphicsShaderStages,
    render_area: (u32, u32),
) {
    let words = [render_area.0 as f32, render_area.1 as f32, 0.0, 0.0];
    let bytes = bytemuck::cast_slice::<f32, u8>(&words);
    if stages.stage_infos()[0].uses_render_area {
        if let Some((_, data)) = prepared.vertex.push_constants.as_mut() {
            data[..16].copy_from_slice(bytes);
        }
    }
    if stages.stage_infos()[4].uses_render_area {
        if let Some((_, data)) = prepared.fragment.push_constants.as_mut() {
            data[..16].copy_from_slice(bytes);
        }
    }
}

fn metal_viewport(draw: &Maxwell3DDrawView<'_>, render_area: (u32, u32)) -> MTLViewport {
    let surface = draw.surface_clip();
    if !draw.viewport_scale_offset_enabled() {
        return MTLViewport {
            originX: surface.x as f64,
            originY: surface.y as f64,
            width: surface.width.max(1).min(render_area.0.max(1)) as f64,
            height: surface.height.max(1).min(render_area.1.max(1)) as f64,
            znear: 0.0,
            zfar: 1.0,
        };
    }
    let source = draw.viewport_transform(0);
    let mut x = source.translate_x - source.scale_x;
    let mut y = source.translate_y - source.scale_y;
    let mut width = source.scale_x * 2.0;
    let mut height = source.scale_y * 2.0;
    if width < 0.0 {
        x += width;
        width = -width;
    }
    if height < 0.0 {
        y += height;
        height = -height;
    }
    let reduce_z = if draw.depth_mode() == crate::engines::maxwell_3d::DepthMode::MinusOneToOne {
        1.0
    } else {
        0.0
    };
    MTLViewport {
        originX: x.max(0.0) as f64,
        originY: y.max(0.0) as f64,
        width: width.max(1.0).min(render_area.0.max(1) as f32) as f64,
        height: height.max(1.0).min(render_area.1.max(1) as f32) as f64,
        znear: (source.translate_z - source.scale_z * reduce_z).clamp(0.0, 1.0) as f64,
        zfar: (source.translate_z + source.scale_z).clamp(0.0, 1.0) as f64,
    }
}

fn metal_scissor(draw: &Maxwell3DDrawView<'_>, render_area: (u32, u32)) -> MTLScissorRect {
    let source = draw.scissor(0);
    if !source.enabled {
        return MTLScissorRect {
            x: 0,
            y: 0,
            width: render_area.0.max(1) as usize,
            height: render_area.1.max(1) as usize,
        };
    }
    let min_x = source.min_x.min(render_area.0);
    let min_y = source.min_y.min(render_area.1);
    let max_x = source.max_x.min(render_area.0).max(min_x);
    let max_y = source.max_y.min(render_area.1).max(min_y);
    MTLScissorRect {
        x: min_x as usize,
        y: min_y as usize,
        width: max_x.saturating_sub(min_x).max(1) as usize,
        height: max_y.saturating_sub(min_y).max(1) as usize,
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
        let syncpoints = Arc::new(SyncpointManager::new());
        let mut rasterizer = MetalRasterizer::new(device, syncpoints, device_memory).unwrap();

        let initial_tick = rasterizer.scheduler().current_tick();
        rasterizer.tick_frame();
        rasterizer.finish().unwrap();
        assert_eq!(rasterizer.scheduler().current_tick(), initial_tick);
    }
}
