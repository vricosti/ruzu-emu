// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `pipeline_helper.h`.
//!
//! Helper types for building descriptor set layouts, pipeline layouts,
//! descriptor update templates, and push constant management for
//! rescaling and render area data.

use ash::vk;
use shader_recompiler::shader_info::{num_descriptors, Info as ShaderInfo};

use super::texture_cache::TextureCache;
use super::update_descriptor::{DescriptorUpdateEntry, UpdateDescriptorQueue};
use crate::texture_cache::texture_cache_base::ImageViewInOut;
use crate::texture_cache::types::{SamplerId, NULL_IMAGE_ID, NULL_IMAGE_VIEW_ID};
use crate::vulkan_common::vulkan_device::{Device, DeviceReference};

/// Port of `PixelFormatFromImageFormat` from upstream `pipeline_helper.h`.
pub fn pixel_format_from_image_format(
    format: shader_recompiler::shader_info::ImageFormat,
) -> Option<crate::surface::PixelFormat> {
    use crate::surface::PixelFormat;
    use shader_recompiler::shader_info::ImageFormat;
    match format {
        ImageFormat::Typeless => None,
        ImageFormat::R8Uint => Some(PixelFormat::R8Uint),
        ImageFormat::R8Sint => Some(PixelFormat::R8Sint),
        ImageFormat::R16Uint => Some(PixelFormat::R16Uint),
        ImageFormat::R16Sint => Some(PixelFormat::R16Sint),
        ImageFormat::R32Uint => Some(PixelFormat::R32Uint),
        ImageFormat::R32G32Uint => Some(PixelFormat::R32G32Uint),
        ImageFormat::R32G32B32A32Uint => Some(PixelFormat::R32G32B32A32Uint),
    }
}

/// Number of u32 words used for texture and image scaling bit flags.
/// Port of `NUM_TEXTURE_AND_IMAGE_SCALING_WORDS` from shader recompiler.
pub const NUM_TEXTURE_AND_IMAGE_SCALING_WORDS: usize = 6;

/// Number of u32 words for texture-only scaling.
/// Port of `NUM_TEXTURE_SCALING_WORDS`.
pub const NUM_TEXTURE_SCALING_WORDS: usize = 4;

pub const RESCALING_LAYOUT_WORDS_OFFSET: u32 = 0;
pub const RESCALING_LAYOUT_DOWN_FACTOR_OFFSET: u32 = 24;
pub const RESCALING_LAYOUT_SIZE: u32 = 32;
pub const RENDERAREA_LAYOUT_OFFSET: u32 = 0;
pub const RENDERAREA_LAYOUT_SIZE: u32 = 16;

/// Size of a single descriptor update entry (buffer info / image info).
/// Port of `sizeof(DescriptorUpdateEntry)` used as stride.
const DESCRIPTOR_UPDATE_ENTRY_SIZE: usize = std::mem::size_of::<DescriptorUpdateEntry>();

/// Port of upstream `NumDescriptorEntries` from `pipeline_helper.h`.
pub fn num_descriptor_entries(info: &ShaderInfo) -> u32 {
    num_descriptors(&info.constant_buffer_descriptors)
        + num_descriptors(&info.storage_buffers_descriptors)
        + num_descriptors(&info.texture_buffer_descriptors)
        + num_descriptors(&info.image_buffer_descriptors)
        + num_descriptors(&info.texture_descriptors)
        + num_descriptors(&info.image_descriptors)
}

fn descriptor_size_for_type(device: &Device, descriptor_type: vk::DescriptorType) -> usize {
    let props = device.descriptor_buffer_properties();
    let robust = device.is_robust_buffer_access_enabled();
    match descriptor_type {
        vk::DescriptorType::UNIFORM_BUFFER => {
            if robust {
                props.robust_uniform_buffer_descriptor_size
            } else {
                props.uniform_buffer_descriptor_size
            }
        }
        vk::DescriptorType::STORAGE_BUFFER => {
            if robust {
                props.robust_storage_buffer_descriptor_size
            } else {
                props.storage_buffer_descriptor_size
            }
        }
        vk::DescriptorType::UNIFORM_TEXEL_BUFFER => {
            if robust {
                props.robust_uniform_texel_buffer_descriptor_size
            } else {
                props.uniform_texel_buffer_descriptor_size
            }
        }
        vk::DescriptorType::STORAGE_TEXEL_BUFFER => {
            if robust {
                props.robust_storage_texel_buffer_descriptor_size
            } else {
                props.storage_texel_buffer_descriptor_size
            }
        }
        vk::DescriptorType::COMBINED_IMAGE_SAMPLER => props.combined_image_sampler_descriptor_size,
        vk::DescriptorType::STORAGE_IMAGE => props.storage_image_descriptor_size,
        _ => 0,
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DescriptorBufferBinding {
    pub descriptor_type: vk::DescriptorType,
    pub count: u32,
    pub offset: vk::DeviceSize,
    pub stride: vk::DeviceSize,
}

#[derive(Clone, Debug, Default)]
pub struct DescriptorBufferLayout {
    pub size: vk::DeviceSize,
    pub bindings: Vec<DescriptorBufferBinding>,
}

impl DescriptorBufferLayout {
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }
}

pub unsafe fn write_descriptor_buffer(
    device: &Device,
    layout: &DescriptorBufferLayout,
    mut payload: *const DescriptorUpdateEntry,
    host: *mut u8,
) {
    let extension = device
        .descriptor_buffer_extension()
        .expect("descriptor-buffer layout requires VK_EXT_descriptor_buffer");
    for binding in &layout.bindings {
        for index in 0..binding.count {
            let entry = unsafe { &*payload };
            payload = unsafe { payload.add(1) };
            let address = unsafe { entry.address };
            let address_info = vk::DescriptorAddressInfoEXT::builder()
                .address(address.address)
                .range(address.range)
                .format(address.format)
                .build();
            let data = match binding.descriptor_type {
                vk::DescriptorType::UNIFORM_BUFFER => vk::DescriptorDataEXT {
                    p_uniform_buffer: &address_info,
                },
                vk::DescriptorType::STORAGE_BUFFER => vk::DescriptorDataEXT {
                    p_storage_buffer: &address_info,
                },
                vk::DescriptorType::UNIFORM_TEXEL_BUFFER => vk::DescriptorDataEXT {
                    p_uniform_texel_buffer: &address_info,
                },
                vk::DescriptorType::STORAGE_TEXEL_BUFFER => vk::DescriptorDataEXT {
                    p_storage_texel_buffer: &address_info,
                },
                vk::DescriptorType::COMBINED_IMAGE_SAMPLER => vk::DescriptorDataEXT {
                    p_combined_image_sampler: unsafe { &entry.image },
                },
                vk::DescriptorType::STORAGE_IMAGE => vk::DescriptorDataEXT {
                    p_storage_image: unsafe { &entry.image },
                },
                _ => continue,
            };
            let get_info = vk::DescriptorGetInfoEXT {
                ty: binding.descriptor_type,
                data,
                ..Default::default()
            };
            let destination = unsafe {
                std::slice::from_raw_parts_mut(
                    host.add((binding.offset + u64::from(index) * binding.stride) as usize),
                    binding.stride as usize,
                )
            };
            unsafe { extension.get_descriptor(&get_info, destination) };
        }
    }
}

// ---------------------------------------------------------------------------
// DescriptorLayoutBuilder
// ---------------------------------------------------------------------------

/// Descriptor binding info for a single descriptor type.
#[derive(Debug, Clone, Copy)]
pub struct DescriptorInfo {
    pub count: u32,
}

/// Port of `DescriptorLayoutBuilder` class.
///
/// Incrementally builds descriptor set layout bindings, update template
/// entries, and pipeline layouts from shader info.
pub struct DescriptorLayoutBuilder {
    device: DeviceReference,
    is_compute: bool,
    bindings: Vec<vk::DescriptorSetLayoutBinding>,
    entries: Vec<vk::DescriptorUpdateTemplateEntry>,
    binding: u32,
    num_descriptors: u32,
    offset: usize,
}

impl DescriptorLayoutBuilder {
    /// Port of `DescriptorLayoutBuilder::DescriptorLayoutBuilder`.
    pub fn new(device: &Device) -> Self {
        DescriptorLayoutBuilder {
            device: DeviceReference::new(device),
            is_compute: false,
            bindings: Vec::new(),
            entries: Vec::new(),
            binding: 0,
            num_descriptors: 0,
            offset: 0,
        }
    }

    #[cfg(test)]
    pub(crate) fn new_for_test() -> Self {
        Self {
            device: DeviceReference::dangling_for_test(),
            is_compute: false,
            bindings: Vec::new(),
            entries: Vec::new(),
            binding: 0,
            num_descriptors: 0,
            offset: 0,
        }
    }

    /// Port of `DescriptorLayoutBuilder::CanUsePushDescriptor`.
    pub fn can_use_push_descriptor(&self) -> bool {
        let device = self.device.get();
        if !device.is_khr_push_descriptor_supported()
            || self.num_descriptors > device.max_push_descriptors()
        {
            return false;
        }
        !device.is_ext_descriptor_buffer_supported()
            || device
                .descriptor_buffer_properties()
                .bufferless_push_descriptors
                != 0
    }

    pub fn can_use_descriptor_buffer(&self) -> bool {
        let device = self.device.get();
        let props = device.descriptor_buffer_properties();
        if !device.is_ext_descriptor_buffer_supported()
            || self.bindings.is_empty()
            || props.combined_image_sampler_descriptor_single_array == 0
        {
            return false;
        }
        props.bufferless_push_descriptors == 0 || !self.can_use_push_descriptor()
    }

    pub fn make_descriptor_buffer_layout(
        &self,
        layout: vk::DescriptorSetLayout,
    ) -> DescriptorBufferLayout {
        if layout == vk::DescriptorSetLayout::null() {
            return DescriptorBufferLayout::default();
        }
        let device = self.device.get();
        let extension = device
            .descriptor_buffer_extension()
            .expect("descriptor-buffer layout requires VK_EXT_descriptor_buffer");
        DescriptorBufferLayout {
            size: unsafe { extension.get_descriptor_set_layout_size(layout) },
            bindings: self
                .bindings
                .iter()
                .map(|binding| DescriptorBufferBinding {
                    descriptor_type: binding.descriptor_type,
                    count: binding.descriptor_count,
                    offset: unsafe {
                        extension.get_descriptor_set_layout_binding_offset(layout, binding.binding)
                    },
                    stride: descriptor_size_for_type(device, binding.descriptor_type) as u64,
                })
                .collect(),
        }
    }

    pub fn num_descriptors(&self) -> u32 {
        self.num_descriptors
    }

    /// Port of `DescriptorLayoutBuilder::CreateDescriptorSetLayout`.
    pub fn create_descriptor_set_layout(
        &self,
        use_push_descriptor: bool,
        use_descriptor_buffer: bool,
    ) -> Result<vk::DescriptorSetLayout, vk::Result> {
        if self.bindings.is_empty() {
            return Ok(vk::DescriptorSetLayout::null());
        }
        let device = self.device.get();
        let mut flags = vk::DescriptorSetLayoutCreateFlags::empty();
        if use_push_descriptor {
            flags |= vk::DescriptorSetLayoutCreateFlags::PUSH_DESCRIPTOR_KHR;
        }
        if use_descriptor_buffer {
            flags |= vk::DescriptorSetLayoutCreateFlags::DESCRIPTOR_BUFFER_EXT;
        }
        let binding_flags = vec![vk::DescriptorBindingFlags::PARTIALLY_BOUND; self.bindings.len()];
        let mut binding_flags_info =
            vk::DescriptorSetLayoutBindingFlagsCreateInfo::builder().binding_flags(&binding_flags);
        let mut ci = vk::DescriptorSetLayoutCreateInfo::builder()
            .flags(flags)
            .bindings(&self.bindings);
        if !use_push_descriptor && device.is_descriptor_binding_partially_bound_supported() {
            ci = ci.push_next(&mut binding_flags_info);
        }
        unsafe { device.get_logical().create_descriptor_set_layout(&ci, None) }
    }

    /// Port of `DescriptorLayoutBuilder::CreateTemplate`.
    pub fn create_template(
        &self,
        descriptor_set_layout: vk::DescriptorSetLayout,
        pipeline_layout: vk::PipelineLayout,
        use_push_descriptor: bool,
    ) -> Result<vk::DescriptorUpdateTemplate, vk::Result> {
        if self.entries.is_empty() {
            return Ok(vk::DescriptorUpdateTemplate::null());
        }
        let template_type = if use_push_descriptor {
            vk::DescriptorUpdateTemplateType::PUSH_DESCRIPTORS_KHR
        } else {
            vk::DescriptorUpdateTemplateType::DESCRIPTOR_SET
        };
        let ci = vk::DescriptorUpdateTemplateCreateInfo {
            s_type: vk::StructureType::DESCRIPTOR_UPDATE_TEMPLATE_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: vk::DescriptorUpdateTemplateCreateFlags::empty(),
            descriptor_update_entry_count: self.entries.len() as u32,
            p_descriptor_update_entries: self.entries.as_ptr(),
            template_type,
            descriptor_set_layout,
            pipeline_bind_point: if self.is_compute {
                vk::PipelineBindPoint::COMPUTE
            } else {
                vk::PipelineBindPoint::GRAPHICS
            },
            pipeline_layout,
            set: 0,
        };
        unsafe {
            self.device
                .get()
                .get_logical()
                .create_descriptor_update_template(&ci, None)
        }
    }

    /// Port of `DescriptorLayoutBuilder::CreatePipelineLayout`.
    ///
    /// Creates a pipeline layout with push constant ranges for rescaling
    /// and render area data.
    pub fn create_pipeline_layout(
        &self,
        descriptor_set_layout: vk::DescriptorSetLayout,
    ) -> Result<vk::PipelineLayout, vk::Result> {
        // Push constant range covers rescaling layout + render area layout
        // Rescaling layout: NUM_TEXTURE_AND_IMAGE_SCALING_WORDS * 4 bytes + optional down_factor (4 bytes for compute)
        let size_offset: u32 = if self.is_compute { 4 } else { 0 };
        let rescaling_size = RESCALING_LAYOUT_SIZE;
        let render_area_size = RENDERAREA_LAYOUT_SIZE;
        let range = vk::PushConstantRange {
            stage_flags: if self.is_compute {
                vk::ShaderStageFlags::COMPUTE
            } else {
                vk::ShaderStageFlags::ALL_GRAPHICS
            },
            offset: 0,
            size: rescaling_size - size_offset + render_area_size,
        };

        let set_layout_count = if descriptor_set_layout == vk::DescriptorSetLayout::null() {
            0u32
        } else {
            1u32
        };
        let layouts = [descriptor_set_layout];
        let ci = vk::PipelineLayoutCreateInfo {
            s_type: vk::StructureType::PIPELINE_LAYOUT_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: vk::PipelineLayoutCreateFlags::empty(),
            set_layout_count,
            p_set_layouts: if self.bindings.is_empty() {
                std::ptr::null()
            } else {
                layouts.as_ptr()
            },
            push_constant_range_count: 1,
            p_push_constant_ranges: &range,
        };
        unsafe {
            self.device
                .get()
                .get_logical()
                .create_pipeline_layout(&ci, None)
        }
    }

    /// Port of `DescriptorLayoutBuilder::Add`.
    ///
    /// Adds descriptor bindings for a list of descriptors of a given type and stage.
    pub fn add_descriptors(
        &mut self,
        descriptor_type: vk::DescriptorType,
        stage: vk::ShaderStageFlags,
        descriptors: &[DescriptorInfo],
    ) {
        for desc in descriptors {
            self.bindings.push(vk::DescriptorSetLayoutBinding {
                binding: self.binding,
                descriptor_type,
                descriptor_count: desc.count,
                stage_flags: stage,
                p_immutable_samplers: std::ptr::null(),
            });
            self.entries.push(vk::DescriptorUpdateTemplateEntry {
                dst_binding: self.binding,
                dst_array_element: 0,
                descriptor_count: desc.count,
                descriptor_type,
                offset: self.offset,
                stride: DESCRIPTOR_UPDATE_ENTRY_SIZE,
            });
            self.binding += 1;
            self.num_descriptors += desc.count;
            self.offset += DESCRIPTOR_UPDATE_ENTRY_SIZE * desc.count as usize;
        }
    }

    /// Port of `DescriptorLayoutBuilder::Add(const Shader::Info&, ...)`.
    pub fn add(&mut self, info: &ShaderInfo, stage: vk::ShaderStageFlags) {
        self.is_compute |= stage.contains(vk::ShaderStageFlags::COMPUTE);
        let descriptors = |counts: Vec<u32>| {
            counts
                .into_iter()
                .map(|count| DescriptorInfo { count })
                .collect::<Vec<_>>()
        };
        self.add_descriptors(
            vk::DescriptorType::UNIFORM_BUFFER,
            stage,
            &descriptors(
                info.constant_buffer_descriptors
                    .iter()
                    .map(|desc| desc.count)
                    .collect(),
            ),
        );
        self.add_descriptors(
            vk::DescriptorType::STORAGE_BUFFER,
            stage,
            &descriptors(
                info.storage_buffers_descriptors
                    .iter()
                    .map(|desc| desc.count)
                    .collect(),
            ),
        );
        self.add_descriptors(
            vk::DescriptorType::UNIFORM_TEXEL_BUFFER,
            stage,
            &descriptors(
                info.texture_buffer_descriptors
                    .iter()
                    .map(|desc| desc.count)
                    .collect(),
            ),
        );
        self.add_descriptors(
            vk::DescriptorType::STORAGE_TEXEL_BUFFER,
            stage,
            &descriptors(
                info.image_buffer_descriptors
                    .iter()
                    .map(|desc| desc.count)
                    .collect(),
            ),
        );
        self.add_descriptors(
            vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
            stage,
            &descriptors(
                info.texture_descriptors
                    .iter()
                    .map(|desc| desc.count)
                    .collect(),
            ),
        );
        self.add_descriptors(
            vk::DescriptorType::STORAGE_IMAGE,
            stage,
            &descriptors(
                info.image_descriptors
                    .iter()
                    .map(|desc| desc.count)
                    .collect(),
            ),
        );
    }
}

/// Port of upstream `PushImageDescriptors` from `pipeline_helper.h`.
///
/// Rescaling stores one bit per descriptor declaration. Array elements are
/// ORed together before advancing that descriptor's bit.
#[allow(clippy::too_many_arguments)]
pub fn push_image_descriptors(
    texture_cache: &mut TextureCache,
    descriptor_queue: &mut UpdateDescriptorQueue,
    info: &ShaderInfo,
    rescaling: &mut RescalingPushConstant,
    samplers: &[SamplerId],
    sampler_cursor: &mut usize,
    views: &[ImageViewInOut],
    view_cursor: &mut usize,
    fallback_sampler: vk::Sampler,
) {
    *view_cursor += num_descriptors(&info.texture_buffer_descriptors) as usize;
    *view_cursor += num_descriptors(&info.image_buffer_descriptors) as usize;

    for desc in &info.texture_descriptors {
        let mut is_rescaled = false;
        for _ in 0..desc.count {
            let view_id = views[*view_cursor].id;
            let image_view = texture_cache.get_image_view(view_id);
            let mut vk_image_view = image_view
                .map(|view| view.handle(desc.texture_type))
                .unwrap_or(vk::ImageView::null());
            if vk_image_view == vk::ImageView::null() {
                let null_image_view = texture_cache.null_image_view_handle(desc.texture_type);
                if null_image_view != vk::ImageView::null() {
                    vk_image_view = null_image_view;
                }
            }
            let supports_anisotropy =
                image_view.is_some_and(|view| view.base().supports_anisotropy());
            let format = image_view.map_or(crate::surface::PixelFormat::Invalid, |view| {
                view.base().format
            });
            let supports_depth_comparison =
                image_view.is_some_and(|view| view.supports_depth_comparison);
            let sampler = texture_cache
                .sampler(samplers[*sampler_cursor])
                .map(|sampler| {
                    let mut handle = if sampler.has_added_anisotropy() && !supports_anisotropy {
                        sampler.handle_with_default_anisotropy()
                    } else {
                        sampler.handle()
                    };
                    if sampler.has_linear_filtering()
                        && crate::surface::is_pixel_format_integer(format)
                    {
                        handle = sampler.handle_with_nearest_filter();
                    }
                    if desc.is_depth && sampler.has_depth_comparison() && !supports_depth_comparison
                    {
                        handle = sampler.handle_without_depth_comparison();
                    }
                    handle
                })
                .unwrap_or(fallback_sampler);
            if std::env::var_os("RUZU_TRACE_TEXTURE_DESCRIPTORS").is_some()
                && (!view_id.is_valid()
                    || view_id == NULL_IMAGE_VIEW_ID
                    || image_view.is_none()
                    || vk_image_view == vk::ImageView::null()
                    || format == crate::surface::PixelFormat::Invalid)
            {
                eprintln!(
                    "[TEXTURE_DESCRIPTOR] view={view_id:?} present={} handle={:?} format={format:?} sampler={:?} type={:?}",
                    image_view.is_some(),
                    vk_image_view,
                    sampler,
                    desc.texture_type,
                );
            }
            descriptor_queue.add_sampled_image(vk_image_view, sampler);
            is_rescaled |= texture_cache.base.is_rescaling_image_view(view_id);
            *view_cursor += 1;
            *sampler_cursor += 1;
        }
        rescaling.push_texture(is_rescaled);
    }

    for desc in &info.image_descriptors {
        let mut is_rescaled = false;
        for _ in 0..desc.count {
            let view_id = views[*view_cursor].id;
            let image_view = if view_id.is_valid() && view_id != NULL_IMAGE_VIEW_ID {
                if desc.is_written {
                    let image_id = texture_cache.base.slot_image_views[view_id].image_id;
                    if image_id.is_valid() && image_id != NULL_IMAGE_ID {
                        texture_cache.base.mark_modification_by_id(image_id);
                    }
                }
                texture_cache
                    .image_view_storage_view(view_id, desc.texture_type, desc.format)
                    .or_else(|| {
                        texture_cache.null_storage_image_view(desc.texture_type, desc.format)
                    })
                    .unwrap_or(vk::ImageView::null())
            } else {
                texture_cache
                    .null_storage_image_view(desc.texture_type, desc.format)
                    .unwrap_or(vk::ImageView::null())
            };
            descriptor_queue.add_image(image_view);
            is_rescaled |= texture_cache.base.is_rescaling_image_view(view_id);
            *view_cursor += 1;
        }
        rescaling.push_image(is_rescaled);
    }
}

// ---------------------------------------------------------------------------
// RescalingPushConstant
// ---------------------------------------------------------------------------

/// Port of `RescalingPushConstant` class.
///
/// Tracks per-texture and per-image rescaling flags as bit-packed words
/// for push constant upload.
pub struct RescalingPushConstant {
    words: [u32; NUM_TEXTURE_AND_IMAGE_SCALING_WORDS],
    texture_index: usize,
    texture_bit: u32,
    image_index: usize,
    image_bit: u32,
}

impl RescalingPushConstant {
    /// Port of `RescalingPushConstant::RescalingPushConstant`.
    pub fn new() -> Self {
        RescalingPushConstant {
            words: [0u32; NUM_TEXTURE_AND_IMAGE_SCALING_WORDS],
            texture_index: 0,
            texture_bit: 1,
            image_index: NUM_TEXTURE_SCALING_WORDS,
            image_bit: 1,
        }
    }

    /// Port of `RescalingPushConstant::PushTexture`.
    pub fn push_texture(&mut self, is_rescaled: bool) {
        if is_rescaled {
            self.words[self.texture_index] |= self.texture_bit;
        }
        self.texture_bit <<= 1;
        if self.texture_bit == 0 {
            self.texture_bit = 1;
            self.texture_index += 1;
        }
    }

    /// Port of `RescalingPushConstant::PushImage`.
    pub fn push_image(&mut self, is_rescaled: bool) {
        if is_rescaled {
            self.words[self.image_index] |= self.image_bit;
        }
        self.image_bit <<= 1;
        if self.image_bit == 0 {
            self.image_bit = 1;
            self.image_index += 1;
        }
    }

    /// Port of `RescalingPushConstant::Data`.
    pub fn data(&self) -> &[u32; NUM_TEXTURE_AND_IMAGE_SCALING_WORDS] {
        &self.words
    }
}

// ---------------------------------------------------------------------------
// RenderAreaPushConstant
// ---------------------------------------------------------------------------

/// Port of `RenderAreaPushConstant` class.
pub struct RenderAreaPushConstant {
    pub uses_render_area: bool,
    pub words: [f32; 4],
}

impl RenderAreaPushConstant {
    pub fn new() -> Self {
        RenderAreaPushConstant {
            uses_render_area: false,
            words: [0.0; 4],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rescaling_push_constant_default() {
        let rpc = RescalingPushConstant::new();
        assert_eq!(rpc.words, [0; NUM_TEXTURE_AND_IMAGE_SCALING_WORDS]);
    }

    #[test]
    fn rescaling_push_texture() {
        let mut rpc = RescalingPushConstant::new();
        rpc.push_texture(true);
        assert_eq!(rpc.words[0], 1);
        rpc.push_texture(false);
        assert_eq!(rpc.words[0], 1);
        rpc.push_texture(true);
        assert_eq!(rpc.words[0], 0b101);
    }

    #[test]
    fn rescaling_push_image() {
        let mut rpc = RescalingPushConstant::new();
        rpc.push_image(true);
        assert_eq!(rpc.words[NUM_TEXTURE_SCALING_WORDS], 1);
    }

    #[test]
    fn descriptor_layout_builder_matches_upstream_template_stride() {
        let mut builder = DescriptorLayoutBuilder::new_for_test();
        builder.add_descriptors(
            vk::DescriptorType::UNIFORM_BUFFER,
            vk::ShaderStageFlags::VERTEX,
            &[DescriptorInfo { count: 2 }, DescriptorInfo { count: 1 }],
        );

        assert_eq!(builder.bindings.len(), 2);
        assert_eq!(builder.entries.len(), 2);
        assert_eq!(builder.entries[0].offset, 0);
        assert_eq!(builder.entries[0].stride, DESCRIPTOR_UPDATE_ENTRY_SIZE);
        assert_eq!(builder.entries[1].offset, DESCRIPTOR_UPDATE_ENTRY_SIZE * 2);
        assert_eq!(builder.num_descriptors, 3);
    }

    #[test]
    fn compute_layout_selects_compute_template_bind_point() {
        let mut builder = DescriptorLayoutBuilder::new_for_test();
        let mut info = ShaderInfo::default();
        info.storage_buffers_descriptors.push(
            shader_recompiler::shader_info::StorageBufferDescriptor {
                cbuf_index: 0,
                cbuf_offset: 0,
                count: 1,
                is_written: false,
            },
        );
        builder.add(&info, vk::ShaderStageFlags::COMPUTE);
        assert!(builder.is_compute);
    }

    #[test]
    fn descriptorless_compute_shader_still_selects_compute_layout() {
        let mut builder = DescriptorLayoutBuilder::new_for_test();
        builder.add(&ShaderInfo::default(), vk::ShaderStageFlags::COMPUTE);
        assert!(builder.is_compute);
    }
}
