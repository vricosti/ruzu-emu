// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native Metal graphics resource configuration.
//!
//! This is the Metal counterpart of Eden's
//! `renderer_vulkan/vk_graphics_pipeline.{h,cpp}`. The common cache is
//! configured in Eden's exact order; only the final descriptor-set write is
//! replaced by direct Metal buffer/texture/sampler bindings.

use std::num::NonZeroU32;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLSamplerState, MTLTexture};
use shader_recompiler::shader_info::{num_descriptors, Info as ShaderInfo};
use thiserror::Error;

use crate::buffer_cache::buffer_cache_base::BufferCacheRuntime as _;
use crate::engines::draw_manager::Maxwell3DDrawView;
use crate::renderer_vulkan::pipeline_helper::{
    pixel_format_from_image_format, RescalingPushConstant,
};
use crate::surface::{get_format_type, is_pixel_format_integer, PixelFormat, SurfaceType};
use crate::texture_cache::texture_cache_base::ImageViewInOut;
use crate::texture_cache::types::{
    ImageViewId, SamplerId, NULL_IMAGE_ID, NULL_IMAGE_VIEW_ID, NULL_SAMPLER_ID,
};
use crate::textures::texture::texture_pair;

use super::metal_buffer::MetalBuffer;
use super::metal_buffer_cache::{
    MetalBufferBinding as CachedBufferBinding, MetalCommonBufferCache, MetalIndexBinding,
    MetalVertexBinding,
};
use super::metal_device::MetalDevice;
use super::metal_pipeline_cache::MetalGraphicsShaderStages;
use super::metal_shader::{
    MetalResourceBinding, MetalResourceKind, MetalShaderBindingLayout, MetalShaderModule,
};
use super::metal_texture_cache::MetalTextureCache;

#[derive(Debug, Error)]
pub enum MetalGraphicsPipelineError {
    #[error("graphics descriptor references disabled constant buffer stage={stage} index={index}")]
    DisabledConstantBuffer { stage: usize, index: u32 },
    #[error("graphics image view {0} was not materialized")]
    MissingImageView(u32),
    #[error("graphics sampler {0} was not materialized")]
    MissingSampler(u32),
    #[error("Metal shader binding {binding} has kind {actual:?}, expected {expected:?}")]
    BindingKind {
        binding: u32,
        actual: MetalResourceKind,
        expected: MetalResourceKind,
    },
    #[error("Metal shader binding {0} has no matching Maxwell descriptor")]
    MissingDescriptor(u32),
    #[error("Maxwell descriptor binding {0} is absent from the reflected Metal shader")]
    MissingReflectedBinding(u32),
    #[error("Metal descriptor array count mismatch at binding {binding}: reflected={reflected}, prepared={prepared}")]
    DescriptorArrayCount {
        binding: u32,
        reflected: u32,
        prepared: u32,
    },
    #[error("Metal texel-buffer view creation failed: {0}")]
    TexelBuffer(#[from] super::metal_buffer::MetalBufferError),
}

#[derive(Clone)]
pub struct MetalStageBufferBinding {
    pub index: u32,
    pub buffer: Arc<MetalBuffer>,
    pub offset: usize,
}

#[derive(Clone)]
pub struct MetalStageTextureBinding {
    pub index: u32,
    pub texture: Option<Retained<ProtocolObject<dyn MTLTexture>>>,
}

#[derive(Clone)]
pub struct MetalStageSamplerBinding {
    pub index: u32,
    pub sampler: Retained<ProtocolObject<dyn MTLSamplerState>>,
}

#[derive(Clone, Default)]
pub struct MetalPreparedStage {
    pub buffers: Vec<MetalStageBufferBinding>,
    pub textures: Vec<MetalStageTextureBinding>,
    pub samplers: Vec<MetalStageSamplerBinding>,
    pub push_constants: Option<(u32, [u8; 32])>,
}

pub struct MetalPreparedGraphics {
    pub vertex: MetalPreparedStage,
    pub fragment: MetalPreparedStage,
    pub vertex_buffers: Vec<Option<MetalVertexBinding>>,
    pub index_buffer: Option<MetalIndexBinding>,
    pub image_views: Vec<ImageViewInOut>,
}

enum PreparedDescriptor {
    Buffer(CachedBufferBinding),
    Textures(Vec<Option<Retained<ProtocolObject<dyn MTLTexture>>>>),
    Sampled {
        textures: Vec<Option<Retained<ProtocolObject<dyn MTLTexture>>>>,
        samplers: Vec<Retained<ProtocolObject<dyn MTLSamplerState>>>,
    },
}

struct DescriptorDeclaration {
    binding: u32,
    expected_kind: MetalResourceKind,
    value: PreparedDescriptor,
}

struct StageDescriptorCursors {
    texture_buffer: usize,
    image_buffer: usize,
    view: usize,
    sampler: usize,
}

/// Port of Eden `GraphicsPipeline::ConfigureImpl` through descriptor
/// preparation. The caller owns both cache mutexes for the complete call.
pub fn configure_graphics_resources(
    device: &MetalDevice,
    stages: &MetalGraphicsShaderStages,
    draw: &Maxwell3DDrawView<'_>,
    buffer_cache: &mut MetalCommonBufferCache,
    texture_cache: &mut MetalTextureCache,
    mut read_gpu: impl FnMut(u64, &mut [u8]),
) -> Result<MetalPreparedGraphics, MetalGraphicsPipelineError> {
    let descriptor_regs = draw.descriptor_sync_regs();
    texture_cache.synchronize_graphics_descriptors(descriptor_regs);
    unsafe {
        buffer_cache.set_uniform_buffers_state(
            stages.enabled_uniform_buffer_masks(),
            stages.uniform_buffer_sizes(),
        );
    }
    buffer_cache.runtime.begin_graphics_bindings();

    let mut views = Vec::new();
    let mut sampler_ids = Vec::new();
    views.reserve(
        stages
            .stage_infos()
            .iter()
            .map(|info| {
                num_descriptors(&info.texture_buffer_descriptors)
                    + num_descriptors(&info.image_buffer_descriptors)
                    + num_descriptors(&info.texture_descriptors)
                    + num_descriptors(&info.image_descriptors)
            })
            .sum::<u32>() as usize,
    );

    let via_header = descriptor_regs.sampler_binding_via_header;
    for (stage, info) in stages.stage_infos().iter().enumerate() {
        buffer_cache.unbind_graphics_storage_buffers(stage);
        for (index, descriptor) in info.storage_buffers_descriptors.iter().enumerate() {
            buffer_cache.bind_graphics_storage_buffer(
                stage,
                index,
                descriptor.cbuf_index,
                descriptor.cbuf_offset,
                descriptor.is_written,
            );
        }

        for descriptor in &info.texture_buffer_descriptors {
            for element in 0..descriptor.count {
                let (tic, _) = read_texture_handle(
                    draw,
                    stage,
                    descriptor.cbuf_index,
                    descriptor.cbuf_offset,
                    descriptor.size_shift,
                    element,
                    descriptor.has_secondary,
                    descriptor.shift_left,
                    descriptor.secondary_cbuf_index,
                    descriptor.secondary_cbuf_offset,
                    descriptor.secondary_shift_left,
                    via_header,
                    &mut read_gpu,
                )?;
                views.push(ImageViewInOut {
                    index: tic,
                    blacklist: false,
                    id: NULL_IMAGE_VIEW_ID,
                });
            }
        }
        for descriptor in &info.image_buffer_descriptors {
            for element in 0..descriptor.count {
                let (tic, _) = read_texture_handle(
                    draw,
                    stage,
                    descriptor.cbuf_index,
                    descriptor.cbuf_offset,
                    descriptor.size_shift,
                    element,
                    false,
                    0,
                    0,
                    0,
                    0,
                    via_header,
                    &mut read_gpu,
                )?;
                views.push(ImageViewInOut {
                    index: tic,
                    blacklist: false,
                    id: NULL_IMAGE_VIEW_ID,
                });
            }
        }
        for descriptor in &info.texture_descriptors {
            for element in 0..descriptor.count {
                let (tic, tsc) = read_texture_handle(
                    draw,
                    stage,
                    descriptor.cbuf_index,
                    descriptor.cbuf_offset,
                    descriptor.size_shift,
                    element,
                    descriptor.has_secondary,
                    descriptor.shift_left,
                    descriptor.secondary_cbuf_index,
                    descriptor.secondary_cbuf_offset,
                    descriptor.secondary_shift_left,
                    via_header,
                    &mut read_gpu,
                )?;
                views.push(ImageViewInOut {
                    index: tic,
                    blacklist: false,
                    id: NULL_IMAGE_VIEW_ID,
                });
                sampler_ids.push(texture_cache.get_sampler_id(tsc, false));
            }
        }
        for descriptor in &info.image_descriptors {
            for element in 0..descriptor.count {
                let (tic, _) = read_texture_handle(
                    draw,
                    stage,
                    descriptor.cbuf_index,
                    descriptor.cbuf_offset,
                    descriptor.size_shift,
                    element,
                    false,
                    0,
                    0,
                    0,
                    0,
                    via_header,
                    &mut read_gpu,
                )?;
                views.push(ImageViewInOut {
                    index: tic,
                    blacklist: descriptor.is_written,
                    id: NULL_IMAGE_VIEW_ID,
                });
            }
        }
    }
    texture_cache.fill_image_views(&mut views, false, true);

    let mut view_cursor = 0usize;
    for (stage, info) in stages.stage_infos().iter().enumerate() {
        buffer_cache.unbind_graphics_texture_buffers(stage);
        let mut binding_index = 0usize;
        for descriptor in &info.texture_buffer_descriptors {
            for _ in 0..descriptor.count {
                bind_texel_buffer(
                    buffer_cache,
                    texture_cache,
                    stage,
                    binding_index,
                    views[view_cursor].id,
                    false,
                    false,
                    None,
                )?;
                binding_index += 1;
                view_cursor += 1;
            }
        }
        for descriptor in &info.image_buffer_descriptors {
            for _ in 0..descriptor.count {
                bind_texel_buffer(
                    buffer_cache,
                    texture_cache,
                    stage,
                    binding_index,
                    views[view_cursor].id,
                    descriptor.is_written,
                    true,
                    pixel_format_from_image_format(descriptor.format),
                )?;
                binding_index += 1;
                view_cursor += 1;
            }
        }
        view_cursor += num_descriptors(&info.texture_descriptors) as usize;
        view_cursor += num_descriptors(&info.image_descriptors) as usize;
    }

    buffer_cache.update_graphics_buffers(draw.is_indexed());
    buffer_cache.bind_host_geometry_buffers(draw.is_indexed());
    for stage in 0..stages.stage_infos().len() {
        buffer_cache.bind_host_stage_buffers(stage);
    }
    if buffer_cache.any_buffer_uploaded {
        buffer_cache.runtime.post_copy_barrier();
        buffer_cache.any_buffer_uploaded = false;
    }

    let graphics_buffers = buffer_cache.runtime.graphics_bindings().clone();
    let vertex_buffers = buffer_cache.runtime.vertex_bindings().to_vec();
    let index_buffer = buffer_cache.runtime.index_binding().cloned();
    let null_buffer = buffer_cache.runtime.null_buffer();
    let mut rescaling = RescalingPushConstant::new();
    let mut cursors = StageDescriptorCursors {
        texture_buffer: 0,
        image_buffer: 0,
        view: 0,
        sampler: 0,
    };
    // Eden's DescriptorLayoutBuilder and SPIR-V Bindings both assign one
    // shared binding sequence across every graphics stage.
    let mut descriptor_binding = 0u32;

    let vertex = prepare_stage(
        device,
        0,
        stages.stage_infos(),
        stages.vertex(),
        texture_cache,
        &graphics_buffers,
        &null_buffer,
        &views,
        &sampler_ids,
        &mut cursors,
        &mut rescaling,
        &mut descriptor_binding,
    )?;
    for stage in 1..4 {
        advance_empty_native_stage(
            stage,
            &stages.stage_infos()[stage],
            &mut cursors,
            &mut rescaling,
            &mut descriptor_binding,
        );
    }
    let fragment = if let Some(module) = stages.fragment() {
        prepare_stage(
            device,
            4,
            stages.stage_infos(),
            module,
            texture_cache,
            &graphics_buffers,
            &null_buffer,
            &views,
            &sampler_ids,
            &mut cursors,
            &mut rescaling,
            &mut descriptor_binding,
        )?
    } else {
        MetalPreparedStage::default()
    };

    Ok(MetalPreparedGraphics {
        vertex,
        fragment,
        vertex_buffers,
        index_buffer,
        image_views: views,
    })
}

#[allow(clippy::too_many_arguments)]
fn read_texture_handle(
    draw: &Maxwell3DDrawView<'_>,
    stage: usize,
    cbuf_index: u32,
    cbuf_offset: u32,
    size_shift: u32,
    element: u32,
    has_secondary: bool,
    shift_left: u32,
    secondary_cbuf_index: u32,
    secondary_cbuf_offset: u32,
    secondary_shift_left: u32,
    via_header: bool,
    read_gpu: &mut impl FnMut(u64, &mut [u8]),
) -> Result<(u32, u32), MetalGraphicsPipelineError> {
    let index_offset = element.wrapping_shl(size_shift);
    let primary = draw.const_buffer_binding(stage, cbuf_index as usize);
    if !primary.enabled {
        return Err(MetalGraphicsPipelineError::DisabledConstantBuffer {
            stage,
            index: cbuf_index,
        });
    }
    let read_word = |address: u64, read_gpu: &mut dyn FnMut(u64, &mut [u8])| {
        let mut bytes = [0; 4];
        read_gpu(address, &mut bytes);
        u32::from_le_bytes(bytes)
    };
    let primary_address = primary
        .address
        .wrapping_add(cbuf_offset.wrapping_add(index_offset) as u64);
    if !has_secondary {
        return Ok(texture_pair(
            read_word(primary_address, read_gpu),
            via_header,
        ));
    }
    let secondary = draw.const_buffer_binding(stage, secondary_cbuf_index as usize);
    if !secondary.enabled {
        return Err(MetalGraphicsPipelineError::DisabledConstantBuffer {
            stage,
            index: secondary_cbuf_index,
        });
    }
    let secondary_address = secondary
        .address
        .wrapping_add(secondary_cbuf_offset.wrapping_add(index_offset) as u64);
    Ok(texture_pair(
        (read_word(primary_address, read_gpu) << shift_left)
            | (read_word(secondary_address, read_gpu) << secondary_shift_left),
        via_header,
    ))
}

#[allow(clippy::too_many_arguments)]
fn bind_texel_buffer(
    buffer_cache: &mut MetalCommonBufferCache,
    texture_cache: &MetalTextureCache,
    stage: usize,
    binding_index: usize,
    view_id: ImageViewId,
    is_written: bool,
    is_image: bool,
    explicit_format: Option<PixelFormat>,
) -> Result<(), MetalGraphicsPipelineError> {
    let (gpu_addr, size, mut format) = texture_cache
        .image_view_buffer_info(view_id)
        .ok_or(MetalGraphicsPipelineError::MissingImageView(view_id.index))?;
    if let Some(explicit_format) = explicit_format {
        format = explicit_format;
    }
    buffer_cache.bind_graphics_texture_buffer(
        stage,
        binding_index,
        gpu_addr,
        size,
        format,
        is_written,
        is_image,
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn prepare_stage(
    device: &MetalDevice,
    stage: usize,
    stage_infos: &[ShaderInfo; 5],
    module: &MetalShaderModule,
    texture_cache: &mut MetalTextureCache,
    graphics_buffers: &super::metal_buffer_cache::MetalGraphicsBufferBindings,
    null_buffer: &Arc<MetalBuffer>,
    views: &[ImageViewInOut],
    sampler_ids: &[SamplerId],
    cursors: &mut StageDescriptorCursors,
    rescaling: &mut RescalingPushConstant,
    descriptor_binding: &mut u32,
) -> Result<MetalPreparedStage, MetalGraphicsPipelineError> {
    let info = &stage_infos[stage];
    let mut declarations = Vec::new();
    let mut binding = *descriptor_binding;
    let mut uniform_cursor = 0usize;
    for _ in &info.constant_buffer_descriptors {
        let value = graphics_buffers.uniform_buffers[stage]
            .get(uniform_cursor)
            .cloned()
            .unwrap_or_else(|| CachedBufferBinding {
                buffer: Arc::clone(null_buffer),
                offset: 0,
                size: 4,
                is_written: false,
            });
        declarations.push(DescriptorDeclaration {
            binding,
            expected_kind: MetalResourceKind::UniformBuffer,
            value: PreparedDescriptor::Buffer(value),
        });
        uniform_cursor += 1;
        binding += 1;
    }
    for (storage_cursor, _) in info.storage_buffers_descriptors.iter().enumerate() {
        let value = graphics_buffers.storage_buffers[stage]
            .get(storage_cursor)
            .cloned()
            .unwrap_or_else(|| CachedBufferBinding {
                buffer: Arc::clone(null_buffer),
                offset: 0,
                size: 4,
                is_written: false,
            });
        declarations.push(DescriptorDeclaration {
            binding,
            expected_kind: MetalResourceKind::StorageBuffer,
            value: PreparedDescriptor::Buffer(value),
        });
        binding += 1;
    }
    for descriptor in &info.texture_buffer_descriptors {
        let mut textures = Vec::with_capacity(descriptor.count as usize);
        for _ in 0..descriptor.count {
            let cached = graphics_buffers
                .texture_buffers
                .get(cursors.texture_buffer)
                .cloned();
            cursors.texture_buffer += 1;
            textures.push(
                cached
                    .map(|cached| {
                        cached.buffer.new_texture_view(
                            device,
                            cached.format,
                            cached.offset,
                            cached.size,
                            false,
                        )
                    })
                    .transpose()?,
            );
        }
        declarations.push(DescriptorDeclaration {
            binding,
            expected_kind: MetalResourceKind::SeparateImage,
            value: PreparedDescriptor::Textures(textures),
        });
        binding += 1;
        cursors.view += descriptor.count as usize;
    }
    for descriptor in &info.image_buffer_descriptors {
        let mut textures = Vec::with_capacity(descriptor.count as usize);
        for _ in 0..descriptor.count {
            let cached = graphics_buffers
                .image_buffers
                .get(cursors.image_buffer)
                .cloned();
            cursors.image_buffer += 1;
            textures.push(
                cached
                    .map(|cached| {
                        cached.buffer.new_texture_view(
                            device,
                            cached.format,
                            cached.offset,
                            cached.size,
                            descriptor.is_written,
                        )
                    })
                    .transpose()?,
            );
        }
        declarations.push(DescriptorDeclaration {
            binding,
            expected_kind: MetalResourceKind::StorageImage,
            value: PreparedDescriptor::Textures(textures),
        });
        binding += 1;
        cursors.view += descriptor.count as usize;
    }
    for descriptor in &info.texture_descriptors {
        let mut textures = Vec::with_capacity(descriptor.count as usize);
        let mut samplers = Vec::with_capacity(descriptor.count as usize);
        let mut descriptor_rescaled = false;
        for _ in 0..descriptor.count {
            let view_id = views[cursors.view].id;
            let image_view = texture_cache.image_view(view_id);
            let texture = image_view.and_then(|view| view.retained_handle(descriptor.texture_type));
            let format = image_view.map_or(PixelFormat::Invalid, |view| view.base().format);
            let supports_anisotropy =
                image_view.is_some_and(|view| view.base().supports_anisotropy());
            let supports_depth_comparison = image_view.is_some_and(|view| {
                matches!(
                    get_format_type(view.base().format),
                    SurfaceType::Depth | SurfaceType::DepthStencil
                )
            });
            let sampler_id = sampler_ids[cursors.sampler];
            let sampler = texture_cache
                .sampler(sampler_id)
                .or_else(|| texture_cache.sampler(NULL_SAMPLER_ID))
                .ok_or(MetalGraphicsPipelineError::MissingSampler(sampler_id.index))?;
            let sampler = if sampler.has_added_anisotropy() && !supports_anisotropy {
                sampler.retained_handle_with_default_anisotropy()
            } else if sampler.has_linear_filtering() && is_pixel_format_integer(format) {
                sampler.retained_handle_with_nearest_filter()
            } else if descriptor.is_depth
                && sampler.has_depth_comparison()
                && !supports_depth_comparison
            {
                sampler.retained_handle_without_depth_comparison()
            } else {
                sampler.retained_handle()
            };
            descriptor_rescaled |= texture_cache.base.is_rescaling_image_view(view_id);
            textures.push(texture);
            samplers.push(sampler);
            cursors.view += 1;
            cursors.sampler += 1;
        }
        rescaling.push_texture(descriptor_rescaled);
        declarations.push(DescriptorDeclaration {
            binding,
            expected_kind: MetalResourceKind::SampledImage,
            value: PreparedDescriptor::Sampled { textures, samplers },
        });
        binding += 1;
    }
    for descriptor in &info.image_descriptors {
        let mut textures = Vec::with_capacity(descriptor.count as usize);
        let mut descriptor_rescaled = false;
        for _ in 0..descriptor.count {
            let view_id = views[cursors.view].id;
            if descriptor.is_written && view_id.is_valid() && view_id != NULL_IMAGE_VIEW_ID {
                let image_id = texture_cache.base.slot_image_views[view_id].image_id;
                if image_id.is_valid() && image_id != NULL_IMAGE_ID {
                    texture_cache.base.mark_modification_by_id(image_id);
                }
            }
            let texture = texture_cache
                .image_view(view_id)
                .and_then(|view| view.retained_handle(descriptor.texture_type));
            descriptor_rescaled |= texture_cache.base.is_rescaling_image_view(view_id);
            textures.push(texture);
            cursors.view += 1;
        }
        rescaling.push_image(descriptor_rescaled);
        declarations.push(DescriptorDeclaration {
            binding,
            expected_kind: MetalResourceKind::StorageImage,
            value: PreparedDescriptor::Textures(textures),
        });
        binding += 1;
    }

    *descriptor_binding = binding;

    let mut prepared = bind_reflected_layout(module.bindings(), declarations)?;
    if let Some(index) = module.bindings().push_constant_buffer_index {
        prepared.push_constants = Some((index, make_push_constants(info, rescaling)));
    }
    Ok(prepared)
}

fn bind_reflected_layout(
    layout: &MetalShaderBindingLayout,
    declarations: Vec<DescriptorDeclaration>,
) -> Result<MetalPreparedStage, MetalGraphicsPipelineError> {
    let mut prepared = MetalPreparedStage::default();
    for declaration in declarations {
        let reflected = layout
            .resources
            .iter()
            .find(|resource| resource.binding == declaration.binding)
            .ok_or(MetalGraphicsPipelineError::MissingReflectedBinding(
                declaration.binding,
            ))?;
        if reflected.kind != declaration.expected_kind {
            return Err(MetalGraphicsPipelineError::BindingKind {
                binding: declaration.binding,
                actual: reflected.kind,
                expected: declaration.expected_kind,
            });
        }
        bind_declaration(&mut prepared, reflected, declaration.value)?;
    }
    for reflected in &layout.resources {
        if reflected.descriptor_set == 0 && !prepared_binding_exists(reflected, &prepared) {
            return Err(MetalGraphicsPipelineError::MissingDescriptor(
                reflected.binding,
            ));
        }
    }
    Ok(prepared)
}

fn bind_declaration(
    prepared: &mut MetalPreparedStage,
    reflected: &MetalResourceBinding,
    value: PreparedDescriptor,
) -> Result<(), MetalGraphicsPipelineError> {
    match value {
        PreparedDescriptor::Buffer(binding) => {
            require_count(reflected, 1)?;
            prepared.buffers.push(MetalStageBufferBinding {
                index: reflected.buffer_index,
                buffer: binding.buffer,
                offset: binding.offset,
            });
        }
        PreparedDescriptor::Textures(textures) => {
            require_count(reflected, textures.len() as u32)?;
            prepared
                .textures
                .extend(textures.into_iter().enumerate().map(|(element, texture)| {
                    MetalStageTextureBinding {
                        index: reflected.texture_index + element as u32,
                        texture,
                    }
                }));
        }
        PreparedDescriptor::Sampled { textures, samplers } => {
            require_count(reflected, textures.len() as u32)?;
            prepared
                .textures
                .extend(textures.into_iter().enumerate().map(|(element, texture)| {
                    MetalStageTextureBinding {
                        index: reflected.texture_index + element as u32,
                        texture,
                    }
                }));
            prepared
                .samplers
                .extend(samplers.into_iter().enumerate().map(|(element, sampler)| {
                    MetalStageSamplerBinding {
                        index: reflected.sampler_index + element as u32,
                        sampler,
                    }
                }));
        }
    }
    Ok(())
}

fn require_count(
    reflected: &MetalResourceBinding,
    prepared: u32,
) -> Result<(), MetalGraphicsPipelineError> {
    let reflected_count = reflected.count.map_or(1, NonZeroU32::get);
    if reflected_count != prepared {
        return Err(MetalGraphicsPipelineError::DescriptorArrayCount {
            binding: reflected.binding,
            reflected: reflected_count,
            prepared,
        });
    }
    Ok(())
}

fn prepared_binding_exists(
    reflected: &MetalResourceBinding,
    prepared: &MetalPreparedStage,
) -> bool {
    match reflected.kind {
        MetalResourceKind::UniformBuffer | MetalResourceKind::StorageBuffer => prepared
            .buffers
            .iter()
            .any(|binding| binding.index == reflected.buffer_index),
        MetalResourceKind::StorageImage | MetalResourceKind::SeparateImage => prepared
            .textures
            .iter()
            .any(|binding| binding.index == reflected.texture_index),
        MetalResourceKind::SampledImage => {
            prepared
                .textures
                .iter()
                .any(|binding| binding.index == reflected.texture_index)
                && prepared
                    .samplers
                    .iter()
                    .any(|binding| binding.index == reflected.sampler_index)
        }
        MetalResourceKind::SeparateSampler => prepared
            .samplers
            .iter()
            .any(|binding| binding.index == reflected.sampler_index),
    }
}

fn make_push_constants(info: &ShaderInfo, rescaling: &RescalingPushConstant) -> [u8; 32] {
    let mut data = [0u8; 32];
    for (index, word) in rescaling.data().iter().enumerate() {
        data[index * 4..index * 4 + 4].copy_from_slice(&word.to_ne_bytes());
    }
    let down_factor = if common::settings::values().resolution_info.active {
        common::settings::values().resolution_info.down_factor
    } else {
        1.0
    };
    data[24..28].copy_from_slice(&down_factor.to_ne_bytes());
    if info.uses_render_area {
        // The concrete surface dimensions are patched by the rasterizer after
        // framebuffer selection, matching Eden's overlapping push-constant
        // layout and command order.
        data[..16].fill(0);
    }
    data
}

fn advance_empty_native_stage(
    _stage: usize,
    info: &ShaderInfo,
    cursors: &mut StageDescriptorCursors,
    rescaling: &mut RescalingPushConstant,
    descriptor_binding: &mut u32,
) {
    *descriptor_binding += descriptor_binding_count(info);
    cursors.texture_buffer += num_descriptors(&info.texture_buffer_descriptors) as usize;
    cursors.image_buffer += num_descriptors(&info.image_buffer_descriptors) as usize;
    cursors.view += num_descriptors(&info.texture_buffer_descriptors) as usize;
    cursors.view += num_descriptors(&info.image_buffer_descriptors) as usize;
    for descriptor in &info.texture_descriptors {
        cursors.view += descriptor.count as usize;
        cursors.sampler += descriptor.count as usize;
        rescaling.push_texture(false);
    }
    for descriptor in &info.image_descriptors {
        cursors.view += descriptor.count as usize;
        rescaling.push_image(false);
    }
}

/// Eden `DescriptorLayoutBuilder::Add` allocates one binding per descriptor
/// declaration. Array elements affect `descriptorCount`, not binding numbers.
fn descriptor_binding_count(info: &ShaderInfo) -> u32 {
    (info.constant_buffer_descriptors.len()
        + info.storage_buffers_descriptors.len()
        + info.texture_buffer_descriptors.len()
        + info.image_buffer_descriptors.len()
        + info.texture_descriptors.len()
        + info.image_descriptors.len()) as u32
}

#[cfg(test)]
mod tests {
    use shader_recompiler::shader_info::{
        ConstantBufferDescriptor, Info as ShaderInfo, StorageBufferDescriptor,
    };

    use super::descriptor_binding_count;

    #[test]
    fn descriptor_binding_count_counts_declarations_not_array_elements() {
        let mut info = ShaderInfo::default();
        info.constant_buffer_descriptors
            .push(ConstantBufferDescriptor { index: 0, count: 7 });
        info.storage_buffers_descriptors
            .push(StorageBufferDescriptor {
                cbuf_index: 1,
                cbuf_offset: 0,
                count: 3,
                is_written: false,
            });

        assert_eq!(descriptor_binding_count(&info), 2);
    }
}
