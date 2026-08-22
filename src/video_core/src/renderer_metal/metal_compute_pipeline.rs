// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native Metal compute resource configuration.
//!
//! This is the Metal counterpart of Eden's
//! `renderer_vulkan/vk_compute_pipeline.{h,cpp}`. The common caches are
//! configured in Eden's order; Vulkan descriptor writes are replaced by the
//! reflected direct Metal binding ABI owned by `MetalShaderModule`.

use std::ffi::c_void;
use std::num::NonZeroU32;
use std::ptr::NonNull;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLComputeCommandEncoder, MTLSamplerState, MTLTexture};
use shader_recompiler::shader_info::{
    ImageBufferDescriptor, ImageDescriptor, Info as ShaderInfo, TextureBufferDescriptor,
    TextureDescriptor,
};
use thiserror::Error;

use crate::engines::kepler_compute::{DispatchCall, LaunchParams};
use crate::renderer_vulkan::pipeline_helper::{
    pixel_format_from_image_format, RescalingPushConstant,
};
use crate::surface::{get_format_type, is_pixel_format_integer, PixelFormat, SurfaceType};
use crate::texture_cache::texture_cache_base::{ComputeDescriptorSyncRegs, ImageViewInOut};
use crate::texture_cache::types::{
    ImageViewId, SamplerId, NULL_IMAGE_ID, NULL_IMAGE_VIEW_ID, NULL_SAMPLER_ID,
};
use crate::textures::texture::texture_pair;

use super::metal_buffer::MetalBuffer;
use super::metal_buffer_cache::{
    MetalBufferBinding as CachedBufferBinding, MetalCommonBufferCache,
};
use super::metal_device::MetalDevice;
use super::metal_pipeline_cache::MetalComputePipeline;
use super::metal_shader::{MetalResourceBinding, MetalResourceKind, MetalShaderBindingLayout};
use super::metal_texture_cache::MetalTextureCache;

#[derive(Debug, Error)]
pub enum MetalComputePipelineError {
    #[error("compute descriptor references disabled constant buffer {0}")]
    DisabledConstantBuffer(u32),
    #[error("compute image view {0} was not materialized")]
    MissingImageView(u32),
    #[error("compute sampler {0} was not materialized")]
    MissingSampler(u32),
    #[error("compute descriptor constant-buffer index {0} is out of range")]
    ConstantBufferOutOfRange(u32),
    #[error("compute descriptor view index {0} is out of range")]
    ViewOutOfRange(usize),
    #[error("compute descriptor sampler index {0} is out of range")]
    SamplerOutOfRange(usize),
    #[error("Metal compute binding {binding} has kind {actual:?}, expected {expected:?}")]
    BindingKind {
        binding: u32,
        actual: MetalResourceKind,
        expected: MetalResourceKind,
    },
    #[error("Metal compute binding {0} has no matching Maxwell descriptor")]
    MissingDescriptor(u32),
    #[error("Maxwell compute descriptor binding {0} is absent from the reflected Metal shader")]
    MissingReflectedBinding(u32),
    #[error("Metal compute descriptor array count mismatch at binding {binding}: reflected={reflected}, prepared={prepared}")]
    DescriptorArrayCount {
        binding: u32,
        reflected: u32,
        prepared: u32,
    },
    #[error("Metal compute texel-buffer view creation failed: {0}")]
    TexelBuffer(#[from] super::metal_buffer::MetalBufferError),
}

#[derive(Clone)]
pub struct MetalComputeBufferBinding {
    pub index: u32,
    pub buffer: Arc<MetalBuffer>,
    pub offset: usize,
}

#[derive(Clone)]
pub struct MetalComputeTextureBinding {
    pub index: u32,
    pub texture: Option<Retained<ProtocolObject<dyn MTLTexture>>>,
}

#[derive(Clone)]
pub struct MetalComputeSamplerBinding {
    pub index: u32,
    pub sampler: Retained<ProtocolObject<dyn MTLSamplerState>>,
}

#[derive(Clone, Default)]
pub struct MetalPreparedCompute {
    pub buffers: Vec<MetalComputeBufferBinding>,
    pub textures: Vec<MetalComputeTextureBinding>,
    pub samplers: Vec<MetalComputeSamplerBinding>,
    pub push_constants: Option<(u32, [u8; 32])>,
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

#[derive(Default)]
struct ComputeHandles {
    views: Vec<ImageViewInOut>,
    samplers: Vec<SamplerId>,
}

trait HandleDescriptor {
    fn count(&self) -> u32;
    fn has_secondary(&self) -> bool;
    fn cbuf_index(&self) -> u32;
    fn cbuf_offset(&self) -> u32;
    fn shift_left(&self) -> u32;
    fn secondary_cbuf_index(&self) -> u32;
    fn secondary_cbuf_offset(&self) -> u32;
    fn secondary_shift_left(&self) -> u32;
    fn size_shift(&self) -> u32;
}

macro_rules! impl_handle_descriptor {
    ($ty:ty) => {
        impl HandleDescriptor for $ty {
            fn count(&self) -> u32 {
                self.count
            }
            fn has_secondary(&self) -> bool {
                self.has_secondary
            }
            fn cbuf_index(&self) -> u32 {
                self.cbuf_index
            }
            fn cbuf_offset(&self) -> u32 {
                self.cbuf_offset
            }
            fn shift_left(&self) -> u32 {
                self.shift_left
            }
            fn secondary_cbuf_index(&self) -> u32 {
                self.secondary_cbuf_index
            }
            fn secondary_cbuf_offset(&self) -> u32 {
                self.secondary_cbuf_offset
            }
            fn secondary_shift_left(&self) -> u32 {
                self.secondary_shift_left
            }
            fn size_shift(&self) -> u32 {
                self.size_shift
            }
        }
    };
}

impl_handle_descriptor!(TextureBufferDescriptor);
impl_handle_descriptor!(TextureDescriptor);

macro_rules! impl_primary_handle_descriptor {
    ($ty:ty) => {
        impl HandleDescriptor for $ty {
            fn count(&self) -> u32 {
                self.count
            }
            fn has_secondary(&self) -> bool {
                false
            }
            fn cbuf_index(&self) -> u32 {
                self.cbuf_index
            }
            fn cbuf_offset(&self) -> u32 {
                self.cbuf_offset
            }
            fn shift_left(&self) -> u32 {
                0
            }
            fn secondary_cbuf_index(&self) -> u32 {
                0
            }
            fn secondary_cbuf_offset(&self) -> u32 {
                0
            }
            fn secondary_shift_left(&self) -> u32 {
                0
            }
            fn size_shift(&self) -> u32 {
                self.size_shift
            }
        }
    };
}

impl_primary_handle_descriptor!(ImageBufferDescriptor);
impl_primary_handle_descriptor!(ImageDescriptor);

/// Port of Eden `ComputePipeline::Configure`. The caller owns both cache
/// mutexes for this complete operation.
pub fn configure_compute_resources(
    device: &MetalDevice,
    pipeline: &MetalComputePipeline,
    dispatch: &DispatchCall,
    buffer_cache: &mut MetalCommonBufferCache,
    texture_cache: &mut MetalTextureCache,
    mut read_gpu: impl FnMut(u64, &mut [u8]),
) -> Result<MetalPreparedCompute, MetalComputePipelineError> {
    let info = pipeline.info();
    buffer_cache.runtime.begin_compute_bindings();
    unsafe {
        buffer_cache.set_compute_uniform_buffer_state(
            info.constant_buffer_mask,
            pipeline.uniform_buffer_sizes(),
        );
    }
    buffer_cache.unbind_compute_storage_buffers();
    for (index, descriptor) in info.storage_buffers_descriptors.iter().enumerate() {
        if descriptor.count != 1 {
            log::error!(
                "Metal compute SSBO descriptor {} has unsupported count {}",
                index,
                descriptor.count
            );
        }
        buffer_cache.bind_compute_storage_buffer(
            index,
            descriptor.cbuf_index,
            descriptor.cbuf_offset,
            descriptor.is_written,
        );
    }

    texture_cache
        .base
        .synchronize_compute_descriptors(ComputeDescriptorSyncRegs {
            linked_tsc: dispatch.launch_description.linked_tsc,
            tic_addr: dispatch.tic_address,
            tic_limit: dispatch.tic_limit,
            tsc_addr: dispatch.tsc_address,
            tsc_limit: dispatch.tsc_limit,
        });
    let mut handles = collect_handles(
        info,
        &dispatch.launch_description,
        texture_cache,
        &mut read_gpu,
    )?;
    texture_cache.fill_image_views(&mut handles.views, true, true);

    buffer_cache.unbind_compute_texture_buffers();
    let mut view_index = 0usize;
    for descriptor in &info.texture_buffer_descriptors {
        for _ in 0..descriptor.count {
            bind_texel_buffer(
                buffer_cache,
                texture_cache,
                view_index,
                handles
                    .views
                    .get(view_index)
                    .ok_or(MetalComputePipelineError::ViewOutOfRange(view_index))?
                    .id,
                false,
                false,
                None,
            )?;
            view_index += 1;
        }
    }
    for descriptor in &info.image_buffer_descriptors {
        for _ in 0..descriptor.count {
            bind_texel_buffer(
                buffer_cache,
                texture_cache,
                view_index,
                handles
                    .views
                    .get(view_index)
                    .ok_or(MetalComputePipelineError::ViewOutOfRange(view_index))?
                    .id,
                descriptor.is_written,
                true,
                pixel_format_from_image_format(descriptor.format),
            )?;
            view_index += 1;
        }
    }
    buffer_cache.update_compute_buffers();
    buffer_cache.bind_host_compute_buffers();

    let compute_buffers = buffer_cache.runtime.compute_bindings().clone();
    let null_buffer = buffer_cache.runtime.null_buffer();
    let mut declarations = Vec::new();
    let mut binding = 0u32;
    for (index, _) in info.constant_buffer_descriptors.iter().enumerate() {
        let value = compute_buffers
            .uniform_buffers
            .get(index)
            .cloned()
            .unwrap_or_else(|| null_binding(&null_buffer));
        declarations.push(DescriptorDeclaration {
            binding,
            expected_kind: MetalResourceKind::UniformBuffer,
            value: PreparedDescriptor::Buffer(value),
        });
        binding += 1;
    }
    for (index, _) in info.storage_buffers_descriptors.iter().enumerate() {
        let value = compute_buffers
            .storage_buffers
            .get(index)
            .cloned()
            .unwrap_or_else(|| null_binding(&null_buffer));
        declarations.push(DescriptorDeclaration {
            binding,
            expected_kind: MetalResourceKind::StorageBuffer,
            value: PreparedDescriptor::Buffer(value),
        });
        binding += 1;
    }

    let mut texture_buffer_index = 0usize;
    let mut image_buffer_index = 0usize;
    for descriptor in &info.texture_buffer_descriptors {
        let mut textures = Vec::with_capacity(descriptor.count as usize);
        for _ in 0..descriptor.count {
            let cached = compute_buffers
                .texture_buffers
                .get(texture_buffer_index)
                .cloned();
            texture_buffer_index += 1;
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
    }
    for descriptor in &info.image_buffer_descriptors {
        let mut textures = Vec::with_capacity(descriptor.count as usize);
        for _ in 0..descriptor.count {
            let cached = compute_buffers
                .image_buffers
                .get(image_buffer_index)
                .cloned();
            image_buffer_index += 1;
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
    }

    let mut view_cursor = view_index;
    let mut sampler_cursor = 0usize;
    let mut rescaling = RescalingPushConstant::new();
    for descriptor in &info.texture_descriptors {
        let mut textures = Vec::with_capacity(descriptor.count as usize);
        let mut samplers = Vec::with_capacity(descriptor.count as usize);
        let mut descriptor_rescaled = false;
        for _ in 0..descriptor.count {
            let view_id = handles
                .views
                .get(view_cursor)
                .ok_or(MetalComputePipelineError::ViewOutOfRange(view_cursor))?
                .id;
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
            let sampler_id = *handles
                .samplers
                .get(sampler_cursor)
                .ok_or(MetalComputePipelineError::SamplerOutOfRange(sampler_cursor))?;
            let sampler = texture_cache
                .sampler(sampler_id)
                .or_else(|| texture_cache.sampler(NULL_SAMPLER_ID))
                .ok_or(MetalComputePipelineError::MissingSampler(sampler_id.index))?;
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
            view_cursor += 1;
            sampler_cursor += 1;
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
            let view_id = handles
                .views
                .get(view_cursor)
                .ok_or(MetalComputePipelineError::ViewOutOfRange(view_cursor))?
                .id;
            if descriptor.is_written && view_id.is_valid() && view_id != NULL_IMAGE_VIEW_ID {
                let image_id = texture_cache.base.slot_image_views[view_id].image_id;
                if image_id.is_valid() && image_id != NULL_IMAGE_ID {
                    texture_cache.base.mark_modification_by_id(image_id);
                }
            }
            textures.push(
                texture_cache
                    .image_view(view_id)
                    .and_then(|view| view.retained_handle(descriptor.texture_type)),
            );
            descriptor_rescaled |= texture_cache.base.is_rescaling_image_view(view_id);
            view_cursor += 1;
        }
        rescaling.push_image(descriptor_rescaled);
        declarations.push(DescriptorDeclaration {
            binding,
            expected_kind: MetalResourceKind::StorageImage,
            value: PreparedDescriptor::Textures(textures),
        });
        binding += 1;
    }

    let mut prepared = bind_reflected_layout(pipeline.shader().bindings(), declarations)?;
    if let Some(index) = pipeline.shader().bindings().push_constant_buffer_index {
        prepared.push_constants = Some((index, make_push_constants(info, &rescaling)));
    }
    prepared.image_views = handles.views;
    Ok(prepared)
}

pub fn bind_compute_resources(
    encoder: &ProtocolObject<dyn MTLComputeCommandEncoder>,
    prepared: &MetalPreparedCompute,
) {
    unsafe {
        for binding in &prepared.buffers {
            encoder.setBuffer_offset_atIndex(
                Some(binding.buffer.handle()),
                binding.offset,
                binding.index as usize,
            );
        }
        for binding in &prepared.textures {
            encoder.setTexture_atIndex(binding.texture.as_deref(), binding.index as usize);
        }
        for binding in &prepared.samplers {
            encoder.setSamplerState_atIndex(Some(&binding.sampler), binding.index as usize);
        }
        if let Some((index, bytes)) = &prepared.push_constants {
            let pointer = NonNull::new(bytes.as_ptr() as *mut c_void)
                .expect("compute push-constant pointer is non-null");
            encoder.setBytes_length_atIndex(pointer, bytes.len(), *index as usize);
        }
    }
}

fn collect_handles(
    info: &ShaderInfo,
    qmd: &LaunchParams,
    texture_cache: &mut MetalTextureCache,
    read_gpu: &mut impl FnMut(u64, &mut [u8]),
) -> Result<ComputeHandles, MetalComputePipelineError> {
    let mut result = ComputeHandles::default();
    for descriptor in &info.texture_buffer_descriptors {
        add_views(&mut result.views, qmd, descriptor, false, read_gpu)?;
    }
    for descriptor in &info.image_buffer_descriptors {
        add_views(&mut result.views, qmd, descriptor, false, read_gpu)?;
    }
    for descriptor in &info.texture_descriptors {
        for element in 0..descriptor.count {
            let (image, sampler) = read_handle(qmd, descriptor, element, read_gpu)?;
            result.views.push(ImageViewInOut {
                index: image,
                blacklist: false,
                id: NULL_IMAGE_VIEW_ID,
            });
            result
                .samplers
                .push(texture_cache.get_sampler_id(sampler, true));
        }
    }
    for descriptor in &info.image_descriptors {
        add_views(
            &mut result.views,
            qmd,
            descriptor,
            descriptor.is_written,
            read_gpu,
        )?;
    }
    Ok(result)
}

fn add_views(
    views: &mut Vec<ImageViewInOut>,
    qmd: &LaunchParams,
    descriptor: &impl HandleDescriptor,
    blacklist: bool,
    read_gpu: &mut impl FnMut(u64, &mut [u8]),
) -> Result<(), MetalComputePipelineError> {
    for element in 0..descriptor.count() {
        let (image, _) = read_handle(qmd, descriptor, element, read_gpu)?;
        views.push(ImageViewInOut {
            index: image,
            blacklist,
            id: NULL_IMAGE_VIEW_ID,
        });
    }
    Ok(())
}

fn read_handle(
    qmd: &LaunchParams,
    descriptor: &impl HandleDescriptor,
    element: u32,
    read_gpu: &mut impl FnMut(u64, &mut [u8]),
) -> Result<(u32, u32), MetalComputePipelineError> {
    let cbuf_index = descriptor.cbuf_index();
    if ((qmd.const_buffer_enable_mask >> cbuf_index) & 1) == 0 {
        return Err(MetalComputePipelineError::DisabledConstantBuffer(
            cbuf_index,
        ));
    }
    let index_offset = element.wrapping_shl(descriptor.size_shift());
    let read_word = |address: u64, read_gpu: &mut dyn FnMut(u64, &mut [u8])| {
        let mut bytes = [0; 4];
        read_gpu(address, &mut bytes);
        u32::from_le_bytes(bytes)
    };
    let primary = qmd
        .const_buffers
        .get(cbuf_index as usize)
        .ok_or(MetalComputePipelineError::ConstantBufferOutOfRange(
            cbuf_index,
        ))?
        .address
        .wrapping_add(descriptor.cbuf_offset().wrapping_add(index_offset) as u64);
    let raw = if descriptor.has_secondary() {
        let secondary_index = descriptor.secondary_cbuf_index();
        if ((qmd.const_buffer_enable_mask >> secondary_index) & 1) == 0 {
            return Err(MetalComputePipelineError::DisabledConstantBuffer(
                secondary_index,
            ));
        }
        let secondary = qmd
            .const_buffers
            .get(secondary_index as usize)
            .ok_or(MetalComputePipelineError::ConstantBufferOutOfRange(
                secondary_index,
            ))?
            .address
            .wrapping_add(
                descriptor
                    .secondary_cbuf_offset()
                    .wrapping_add(index_offset) as u64,
            );
        (read_word(primary, read_gpu) << descriptor.shift_left())
            | (read_word(secondary, read_gpu) << descriptor.secondary_shift_left())
    } else {
        read_word(primary, read_gpu)
    };
    Ok(texture_pair(raw, qmd.linked_tsc))
}

fn bind_texel_buffer(
    buffer_cache: &mut MetalCommonBufferCache,
    texture_cache: &MetalTextureCache,
    binding_index: usize,
    view_id: ImageViewId,
    is_written: bool,
    is_image: bool,
    explicit_format: Option<PixelFormat>,
) -> Result<(), MetalComputePipelineError> {
    let (gpu_addr, size, mut format) = texture_cache
        .image_view_buffer_info(view_id)
        .ok_or(MetalComputePipelineError::MissingImageView(view_id.index))?;
    if let Some(explicit_format) = explicit_format {
        format = explicit_format;
    }
    buffer_cache.bind_compute_texture_buffer(
        binding_index,
        gpu_addr,
        size,
        format,
        is_written,
        is_image,
    );
    Ok(())
}

fn null_binding(buffer: &Arc<MetalBuffer>) -> CachedBufferBinding {
    CachedBufferBinding {
        buffer: Arc::clone(buffer),
        offset: 0,
        size: 4,
        is_written: false,
    }
}

fn bind_reflected_layout(
    layout: &MetalShaderBindingLayout,
    declarations: Vec<DescriptorDeclaration>,
) -> Result<MetalPreparedCompute, MetalComputePipelineError> {
    let mut prepared = MetalPreparedCompute::default();
    for declaration in declarations {
        let reflected = layout
            .resources
            .iter()
            .find(|resource| resource.binding == declaration.binding)
            .ok_or(MetalComputePipelineError::MissingReflectedBinding(
                declaration.binding,
            ))?;
        if reflected.kind != declaration.expected_kind {
            return Err(MetalComputePipelineError::BindingKind {
                binding: declaration.binding,
                actual: reflected.kind,
                expected: declaration.expected_kind,
            });
        }
        bind_declaration(&mut prepared, reflected, declaration.value)?;
    }
    for reflected in &layout.resources {
        if reflected.descriptor_set == 0 && !prepared_binding_exists(reflected, &prepared) {
            return Err(MetalComputePipelineError::MissingDescriptor(
                reflected.binding,
            ));
        }
    }
    Ok(prepared)
}

fn bind_declaration(
    prepared: &mut MetalPreparedCompute,
    reflected: &MetalResourceBinding,
    value: PreparedDescriptor,
) -> Result<(), MetalComputePipelineError> {
    match value {
        PreparedDescriptor::Buffer(binding) => {
            require_count(reflected, 1)?;
            prepared.buffers.push(MetalComputeBufferBinding {
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
                    MetalComputeTextureBinding {
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
                    MetalComputeTextureBinding {
                        index: reflected.texture_index + element as u32,
                        texture,
                    }
                }));
            prepared
                .samplers
                .extend(samplers.into_iter().enumerate().map(|(element, sampler)| {
                    MetalComputeSamplerBinding {
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
) -> Result<(), MetalComputePipelineError> {
    let reflected_count = reflected.count.map_or(1, NonZeroU32::get);
    if reflected_count != prepared {
        return Err(MetalComputePipelineError::DescriptorArrayCount {
            binding: reflected.binding,
            reflected: reflected_count,
            prepared,
        });
    }
    Ok(())
}

fn prepared_binding_exists(
    reflected: &MetalResourceBinding,
    prepared: &MetalPreparedCompute,
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
        data[..16].fill(0);
    }
    data
}
