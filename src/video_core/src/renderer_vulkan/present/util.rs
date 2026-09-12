// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `present/util.h` / `present/util.cpp`.
//!
//! Utility functions for creating wrapped Vulkan objects used during
//! frame presentation: buffers, images, image views, render passes,
//! framebuffers, samplers, shaders, descriptor pools/sets/layouts,
//! pipeline layouts, and pipelines.

use ash::vk;

use crate::renderer_vulkan::scheduler::Scheduler;
use crate::vulkan_common::vulkan_device::Device;
use crate::vulkan_common::vulkan_memory_allocator::{
    AllocatedBuffer, AllocatedImage, MemoryAllocator, MemoryUsage,
};
use crate::vulkan_common::vulkan_wrapper::{
    PIPELINE_STAGE_GRAPHICS_COMPUTE, PIPELINE_STAGE_GRAPHICS_COMPUTE_TRANSFER,
};

fn assert_fail_soft(condition: bool, message: impl FnOnce() -> String) {
    if condition {
        return;
    }
    let message = message();
    log::error!("{message}");
    if *common::settings::values().use_debug_asserts.get_value() {
        panic!("{message}");
    }
}

// ---------------------------------------------------------------------------
// Buffer / Image creation
// ---------------------------------------------------------------------------

/// Port of `CreateWrappedBuffer`.
pub fn create_wrapped_buffer(
    allocator: &MemoryAllocator,
    size: vk::DeviceSize,
    usage: MemoryUsage,
) -> AllocatedBuffer {
    let buffer_ci = vk::BufferCreateInfo::default()
        .size(size)
        .usage(vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST)
        .sharing_mode(vk::SharingMode::EXCLUSIVE);
    allocator
        .create_buffer(&buffer_ci, usage)
        .expect("Failed to create wrapped buffer")
}

/// Port of `CreateWrappedImage`.
pub fn create_wrapped_image(
    allocator: &MemoryAllocator,
    dimensions: vk::Extent2D,
    format: vk::Format,
) -> AllocatedImage {
    allocator
        .create_image(&wrapped_image_create_info(dimensions, format))
        .expect("Failed to create wrapped image")
}

fn wrapped_image_create_info(dimensions: vk::Extent2D, format: vk::Format) -> vk::ImageCreateInfo<'static> {
    vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(format)
        .extent(vk::Extent3D {
            width: dimensions.width,
            height: dimensions.height,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(
            vk::ImageUsageFlags::TRANSFER_DST
                | vk::ImageUsageFlags::STORAGE
                | vk::ImageUsageFlags::SAMPLED
                | vk::ImageUsageFlags::COLOR_ATTACHMENT,
        )
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        
}

/// Port of `TransitionImageLayout`.
///
/// Inserts a pipeline barrier to transition `image` from `source_layout` to
/// `target_layout` using the graphics-and-compute stages used by Eden.
pub fn transition_image_layout(
    device: &ash::Device,
    cmdbuf: vk::CommandBuffer,
    image: vk::Image,
    target_layout: vk::ImageLayout,
    source_layout: vk::ImageLayout,
) {
    let flags = vk::AccessFlags::COLOR_ATTACHMENT_READ
        | vk::AccessFlags::COLOR_ATTACHMENT_WRITE
        | vk::AccessFlags::SHADER_READ;

    let barrier = vk::ImageMemoryBarrier::default()
        .src_access_mask(flags)
        .dst_access_mask(flags)
        .old_layout(source_layout)
        .new_layout(target_layout)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        });

    unsafe {
        device.cmd_pipeline_barrier(
            cmdbuf,
            vk::PipelineStageFlags::ALL_GRAPHICS | vk::PipelineStageFlags::COMPUTE_SHADER,
            vk::PipelineStageFlags::ALL_GRAPHICS | vk::PipelineStageFlags::COMPUTE_SHADER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[barrier],
        );
    }
}

/// Port of `UploadImage`.
pub fn upload_image(
    device: &Device,
    allocator: &MemoryAllocator,
    scheduler: &mut Scheduler,
    image: vk::Image,
    dimensions: vk::Extent2D,
    _format: vk::Format,
    initial_contents: &[u8],
) {
    let logical = device.get_logical();
    let upload_ci = vk::BufferCreateInfo::default()
        .size(initial_contents.len() as vk::DeviceSize)
        .usage(vk::BufferUsageFlags::TRANSFER_SRC)
        .sharing_mode(vk::SharingMode::EXCLUSIVE);
    let mut upload_buffer = allocator
        .create_buffer(&upload_ci, MemoryUsage::Upload)
        .expect("Failed to create image upload buffer");
    upload_buffer.mapped_slice_mut()[..initial_contents.len()].copy_from_slice(initial_contents);
    upload_buffer.flush();

    let region = vk::BufferImageCopy::default()
        .buffer_offset(0)
        .buffer_row_length(dimensions.width)
        .buffer_image_height(dimensions.height)
        .image_subresource(vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            mip_level: 0,
            base_array_layer: 0,
            layer_count: 1,
        })
        .image_offset(vk::Offset3D::default())
        .image_extent(vk::Extent3D {
            width: dimensions.width,
            height: dimensions.height,
            depth: 1,
        });

    scheduler.request_outside_render_pass_operation_context();
    let device = logical.clone();
    let upload_buffer_handle = upload_buffer.handle();
    scheduler.record(move |cmdbuf| unsafe {
        transition_image_layout(
            &device,
            cmdbuf,
            image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::ImageLayout::UNDEFINED,
        );
        device.cmd_copy_buffer_to_image(
            cmdbuf,
            upload_buffer_handle,
            image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &[region],
        );
        transition_image_layout(
            &device,
            cmdbuf,
            image,
            vk::ImageLayout::GENERAL,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
        );
    });
    scheduler.finish();
}

/// Port of `DownloadColorImage`.
///
/// Transitions the image to TRANSFER_SRC_OPTIMAL, copies to buffer, then
/// transitions back to GENERAL.
pub fn download_color_image(
    device: &ash::Device,
    cmdbuf: vk::CommandBuffer,
    image: vk::Image,
    buffer: vk::Buffer,
    extent: vk::Extent3D,
) {
    let read_barrier = vk::ImageMemoryBarrier::default()
        .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
        .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
        .old_layout(vk::ImageLayout::GENERAL)
        .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: vk::REMAINING_MIP_LEVELS,
            base_array_layer: 0,
            layer_count: vk::REMAINING_ARRAY_LAYERS,
        });

    let image_write_barrier = vk::ImageMemoryBarrier::default()
        .src_access_mask(vk::AccessFlags::empty())
        .dst_access_mask(vk::AccessFlags::MEMORY_WRITE)
        .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
        .new_layout(vk::ImageLayout::GENERAL)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: vk::REMAINING_MIP_LEVELS,
            base_array_layer: 0,
            layer_count: vk::REMAINING_ARRAY_LAYERS,
        });

    let memory_write_barrier = vk::MemoryBarrier::default()
        .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
        .dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE);

    let copy = vk::BufferImageCopy::default()
        .buffer_offset(0)
        .buffer_row_length(0)
        .buffer_image_height(0)
        .image_subresource(vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            mip_level: 0,
            base_array_layer: 0,
            layer_count: 1,
        })
        .image_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
        .image_extent(extent);

    unsafe {
        device.cmd_pipeline_barrier(
            cmdbuf,
            PIPELINE_STAGE_GRAPHICS_COMPUTE_TRANSFER,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[read_barrier],
        );

        device.cmd_copy_image_to_buffer(
            cmdbuf,
            image,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            buffer,
            &[copy],
        );

        device.cmd_pipeline_barrier(
            cmdbuf,
            vk::PipelineStageFlags::TRANSFER,
            PIPELINE_STAGE_GRAPHICS_COMPUTE,
            vk::DependencyFlags::empty(),
            &[memory_write_barrier],
            &[],
            &[image_write_barrier],
        );
    }
}

/// Port of `ClearColorImage`.
///
/// Transitions image to GENERAL from UNDEFINED, then clears it.
pub fn clear_color_image(device: &ash::Device, cmdbuf: vk::CommandBuffer, image: vk::Image) {
    transition_image_layout(
        device,
        cmdbuf,
        image,
        vk::ImageLayout::GENERAL,
        vk::ImageLayout::UNDEFINED,
    );

    let subresource_range = vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    };

    let clear_value = vk::ClearColorValue {
        float32: [0.0, 0.0, 0.0, 0.0],
    };

    unsafe {
        device.cmd_clear_color_image(
            cmdbuf,
            image,
            vk::ImageLayout::GENERAL,
            &clear_value,
            &[subresource_range],
        );
    }
}

// ---------------------------------------------------------------------------
// Image view / Render pass / Framebuffer creation
// ---------------------------------------------------------------------------

/// Port of `CreateWrappedImageView`.
pub fn create_wrapped_image_view(
    device: &Device,
    image: vk::Image,
    format: vk::Format,
) -> vk::ImageView {
    let view_ci = vk::ImageViewCreateInfo::default()
        .image(image)
        .view_type(vk::ImageViewType::TYPE_2D)
        .format(format)
        .components(vk::ComponentMapping::default())
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        });

    unsafe {
        device
            .get_logical()
            .create_image_view(&view_ci, None)
            .expect("Failed to create wrapped image view")
    }
}

/// Port of `CreateWrappedRenderPass`.
pub fn create_wrapped_render_pass(
    device: &Device,
    format: vk::Format,
    initial_layout: vk::ImageLayout,
) -> vk::RenderPass {
    let load_op = if initial_layout == vk::ImageLayout::UNDEFINED {
        vk::AttachmentLoadOp::DONT_CARE
    } else {
        vk::AttachmentLoadOp::LOAD
    };

    let attachment = vk::AttachmentDescription {
        flags: vk::AttachmentDescriptionFlags::MAY_ALIAS,
        format,
        samples: vk::SampleCountFlags::TYPE_1,
        load_op,
        store_op: vk::AttachmentStoreOp::STORE,
        stencil_load_op: vk::AttachmentLoadOp::LOAD,
        stencil_store_op: vk::AttachmentStoreOp::STORE,
        initial_layout,
        final_layout: vk::ImageLayout::GENERAL,
    };

    let color_attachment_ref = vk::AttachmentReference {
        attachment: 0,
        layout: vk::ImageLayout::GENERAL,
    };

    let subpass = vk::SubpassDescription::default()
        .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
        .color_attachments(std::slice::from_ref(&color_attachment_ref));

    let dependency = vk::SubpassDependency {
        src_subpass: vk::SUBPASS_EXTERNAL,
        dst_subpass: 0,
        src_stage_mask: vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
        dst_stage_mask: vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
        src_access_mask: vk::AccessFlags::empty(),
        dst_access_mask: vk::AccessFlags::COLOR_ATTACHMENT_READ
            | vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
        dependency_flags: vk::DependencyFlags::empty(),
    };

    let render_pass_ci = vk::RenderPassCreateInfo::default()
        .attachments(std::slice::from_ref(&attachment))
        .subpasses(std::slice::from_ref(&subpass))
        .dependencies(std::slice::from_ref(&dependency));

    unsafe {
        device
            .get_logical()
            .create_render_pass(&render_pass_ci, None)
            .expect("Failed to create wrapped render pass")
    }
}

/// Port of `CreateWrappedFramebuffer`.
pub fn create_wrapped_framebuffer(
    device: &Device,
    render_pass: vk::RenderPass,
    dest_image_view: vk::ImageView,
    extent: vk::Extent2D,
) -> vk::Framebuffer {
    let attachments = [dest_image_view];
    let framebuffer_ci = vk::FramebufferCreateInfo::default()
        .render_pass(render_pass)
        .attachments(&attachments)
        .width(extent.width)
        .height(extent.height)
        .layers(1);

    unsafe {
        device
            .get_logical()
            .create_framebuffer(&framebuffer_ci, None)
            .expect("Failed to create wrapped framebuffer")
    }
}

// ---------------------------------------------------------------------------
// Sampler creation
// ---------------------------------------------------------------------------

/// Port of `CreateWrappedSampler`.
pub fn create_wrapped_sampler(device: &Device, filter: vk::Filter) -> vk::Sampler {
    let sampler_ci = vk::SamplerCreateInfo::default()
        .mag_filter(filter)
        .min_filter(filter)
        .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .mip_lod_bias(0.0)
        .anisotropy_enable(false)
        .max_anisotropy(0.0)
        .compare_enable(false)
        .compare_op(vk::CompareOp::NEVER)
        .min_lod(0.0)
        .max_lod(0.0)
        .border_color(vk::BorderColor::FLOAT_OPAQUE_BLACK)
        .unnormalized_coordinates(false);

    unsafe {
        device
            .get_logical()
            .create_sampler(&sampler_ci, None)
            .expect("Failed to create wrapped sampler")
    }
}

/// Port of `CreateBilinearSampler`.
pub fn create_bilinear_sampler(device: &Device) -> vk::Sampler {
    let sampler_ci = vk::SamplerCreateInfo::default()
        .mag_filter(vk::Filter::LINEAR)
        .min_filter(vk::Filter::LINEAR)
        .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_BORDER)
        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_BORDER)
        .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_BORDER)
        .mip_lod_bias(0.0)
        .anisotropy_enable(false)
        .max_anisotropy(0.0)
        .compare_enable(false)
        .compare_op(vk::CompareOp::NEVER)
        .min_lod(0.0)
        .max_lod(0.0)
        .border_color(vk::BorderColor::FLOAT_OPAQUE_BLACK)
        .unnormalized_coordinates(false);

    unsafe {
        device
            .get_logical()
            .create_sampler(&sampler_ci, None)
            .expect("Failed to create bilinear sampler")
    }
}

/// Port of `CreateNearestNeighborSampler`.
pub fn create_nearest_neighbor_sampler(device: &Device) -> vk::Sampler {
    let sampler_ci = vk::SamplerCreateInfo::default()
        .mag_filter(vk::Filter::NEAREST)
        .min_filter(vk::Filter::NEAREST)
        .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_BORDER)
        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_BORDER)
        .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_BORDER)
        .mip_lod_bias(0.0)
        .anisotropy_enable(false)
        .max_anisotropy(0.0)
        .compare_enable(false)
        .compare_op(vk::CompareOp::NEVER)
        .min_lod(0.0)
        .max_lod(0.0)
        .border_color(vk::BorderColor::FLOAT_OPAQUE_BLACK)
        .unnormalized_coordinates(false);

    unsafe {
        device
            .get_logical()
            .create_sampler(&sampler_ci, None)
            .expect("Failed to create nearest neighbor sampler")
    }
}

/// Upstream passes `VkCubicFilterWeightsQCOM` values directly.
pub type CubicFilterWeights = vk::CubicFilterWeightsQCOM;

/// Port of `CreateCubicSampler`.
pub fn create_cubic_sampler(device: &Device, qcom_weights: CubicFilterWeights) -> vk::Sampler {
    let filter = if device.is_ext_filter_cubic_supported() {
        vk::Filter::CUBIC_EXT
    } else {
        vk::Filter::LINEAR
    };
    let mut sampler_ci = vk::SamplerCreateInfo::default()
        .mag_filter(filter)
        .min_filter(filter)
        .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_BORDER)
        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_BORDER)
        .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_BORDER)
        .mip_lod_bias(0.0)
        .anisotropy_enable(false)
        .max_anisotropy(0.0)
        .compare_enable(false)
        .compare_op(vk::CompareOp::NEVER)
        .min_lod(0.0)
        .max_lod(0.0)
        .border_color(vk::BorderColor::FLOAT_OPAQUE_BLACK)
        .unnormalized_coordinates(false);
    let mut qcom_ci = vk::SamplerCubicWeightsCreateInfoQCOM::default().cubic_weights(qcom_weights);
    if qcom_weights != CubicFilterWeights::CATMULL_ROM {
        sampler_ci = sampler_ci.push_next(&mut qcom_ci);
    }

    unsafe {
        device
            .get_logical()
            .create_sampler(&sampler_ci, None)
            .expect("Failed to create cubic sampler")
    }
}

// ---------------------------------------------------------------------------
// Shader module creation
// ---------------------------------------------------------------------------

/// Port of `CreateWrappedShaderModule`.
pub fn create_wrapped_shader_module(device: &Device, code: &[u32]) -> vk::ShaderModule {
    let shader_ci = vk::ShaderModuleCreateInfo::default().code(code);

    unsafe {
        device
            .get_logical()
            .create_shader_module(&shader_ci, None)
            .expect("Failed to create wrapped shader module")
    }
}

// ---------------------------------------------------------------------------
// Descriptor pool / set / layout creation
// ---------------------------------------------------------------------------

/// Port of `CreateWrappedDescriptorPool`.
pub fn create_wrapped_descriptor_pool(
    device: &Device,
    max_descriptors: usize,
    max_sets: usize,
    types: &[vk::DescriptorType],
) -> vk::DescriptorPool {
    let pool_sizes = wrapped_descriptor_pool_sizes(max_descriptors, types);

    let pool_ci = vk::DescriptorPoolCreateInfo::default()
        .max_sets(max_sets as u32)
        .pool_sizes(&pool_sizes);

    unsafe {
        device
            .get_logical()
            .create_descriptor_pool(&pool_ci, None)
            .expect("Failed to create wrapped descriptor pool")
    }
}

fn wrapped_descriptor_pool_sizes(
    max_descriptors: usize,
    types: &[vk::DescriptorType],
) -> Vec<vk::DescriptorPoolSize> {
    types
        .iter()
        .map(|&ty| vk::DescriptorPoolSize {
            ty,
            descriptor_count: max_descriptors as u32,
        })
        .collect()
}

/// Port of `CreateWrappedDescriptorSetLayout`.
pub fn create_wrapped_descriptor_set_layout(
    device: &Device,
    types: &[vk::DescriptorType],
    stages: vk::ShaderStageFlags,
) -> vk::DescriptorSetLayout {
    let bindings = wrapped_descriptor_set_layout_bindings(types, stages);

    let layout_ci = vk::DescriptorSetLayoutCreateInfo::default()
        .bindings(&bindings);

    unsafe {
        device
            .get_logical()
            .create_descriptor_set_layout(&layout_ci, None)
            .expect("Failed to create wrapped descriptor set layout")
    }
}

fn wrapped_descriptor_set_layout_bindings(
    types: &[vk::DescriptorType],
    stages: vk::ShaderStageFlags,
) -> Vec<vk::DescriptorSetLayoutBinding<'_>> {
    types
        .iter()
        .enumerate()
        .map(|(i, &ty)| vk::DescriptorSetLayoutBinding {
            binding: i as u32,
            descriptor_type: ty,
            descriptor_count: 1,
            stage_flags: stages,
            p_immutable_samplers: std::ptr::null(),
            ..Default::default()
        })
        .collect()
}

/// Port of `CreateWrappedDescriptorSets`.
pub fn create_wrapped_descriptor_sets(
    device: &ash::Device,
    pool: vk::DescriptorPool,
    layouts: &[vk::DescriptorSetLayout],
) -> Vec<vk::DescriptorSet> {
    let alloc_info = vk::DescriptorSetAllocateInfo::default()
        .descriptor_pool(pool)
        .set_layouts(layouts);

    unsafe {
        device
            .allocate_descriptor_sets(&alloc_info)
            .expect("Failed to create wrapped descriptor sets")
    }
}

// ---------------------------------------------------------------------------
// Pipeline layout creation
// ---------------------------------------------------------------------------

/// Port of `CreateWrappedPipelineLayout`.
pub fn create_wrapped_pipeline_layout(
    device: &Device,
    layout: vk::DescriptorSetLayout,
) -> vk::PipelineLayout {
    let layouts = [layout];
    let pipeline_layout_ci = vk::PipelineLayoutCreateInfo::default()
        .set_layouts(&layouts);

    unsafe {
        device
            .get_logical()
            .create_pipeline_layout(&pipeline_layout_ci, None)
            .expect("Failed to create wrapped pipeline layout")
    }
}

/// Port of `CreateWrappedComputePipeline`.
pub fn create_wrapped_compute_pipeline(
    device: &Device,
    layout: vk::PipelineLayout,
    shader: vk::ShaderModule,
) -> vk::Pipeline {
    let main_name = c"main";
    let stage = vk::PipelineShaderStageCreateInfo::default()
        .stage(vk::ShaderStageFlags::COMPUTE)
        .module(shader)
        .name(main_name);
    let pipeline_ci = vk::ComputePipelineCreateInfo::default()
        .stage(stage)
        .layout(layout)
        .base_pipeline_handle(vk::Pipeline::null())
        .base_pipeline_index(0);

    let pipelines = unsafe {
        device
            .get_logical()
            .create_compute_pipelines(device.static_pipeline_cache(), &[pipeline_ci], None)
            .expect("Failed to create wrapped compute pipeline")
    };
    pipelines[0]
}

// ---------------------------------------------------------------------------
// Pipeline creation helpers (internal)
// ---------------------------------------------------------------------------

/// Internal helper: creates a graphics pipeline with the given blending state.
///
/// Port of the file-static `CreateWrappedPipelineImpl`.
fn create_wrapped_pipeline_impl(
    device: &Device,
    renderpass: vk::RenderPass,
    layout: vk::PipelineLayout,
    vert_shader: vk::ShaderModule,
    frag_shader: vk::ShaderModule,
    blending: vk::PipelineColorBlendAttachmentState,
) -> vk::Pipeline {
    let main_name = c"main";

    let shader_stages = [
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::VERTEX)
            .module(vert_shader)
            .name(main_name),
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::FRAGMENT)
            .module(frag_shader)
            .name(main_name),
    ];

    let vertex_input_ci = vk::PipelineVertexInputStateCreateInfo::default();

    let input_assembly_ci = wrapped_pipeline_input_assembly_state(device.is_molten_vk());

    let viewport_state_ci = vk::PipelineViewportStateCreateInfo::default()
        .viewport_count(1)
        .scissor_count(1);

    let rasterization_ci = vk::PipelineRasterizationStateCreateInfo::default()
        .depth_clamp_enable(false)
        .rasterizer_discard_enable(false)
        .polygon_mode(vk::PolygonMode::FILL)
        .cull_mode(vk::CullModeFlags::NONE)
        .front_face(vk::FrontFace::CLOCKWISE)
        .depth_bias_enable(false)
        .line_width(1.0);

    let multisampling_ci = vk::PipelineMultisampleStateCreateInfo::default()
        .rasterization_samples(vk::SampleCountFlags::TYPE_1)
        .sample_shading_enable(false)
        .min_sample_shading(0.0);

    let blend_attachments = [blending];
    let color_blend_ci = vk::PipelineColorBlendStateCreateInfo::default()
        .logic_op_enable(false)
        .logic_op(vk::LogicOp::COPY)
        .attachments(&blend_attachments)
        .blend_constants([0.0, 0.0, 0.0, 0.0]);

    let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
    let dynamic_state_ci = vk::PipelineDynamicStateCreateInfo::default()
        .dynamic_states(&dynamic_states);

    let pipeline_ci = vk::GraphicsPipelineCreateInfo::default()
        .stages(&shader_stages)
        .vertex_input_state(&vertex_input_ci)
        .input_assembly_state(&input_assembly_ci)
        .viewport_state(&viewport_state_ci)
        .rasterization_state(&rasterization_ci)
        .multisample_state(&multisampling_ci)
        .color_blend_state(&color_blend_ci)
        .dynamic_state(&dynamic_state_ci)
        .layout(layout)
        .render_pass(renderpass)
        .subpass(0);

    let pipelines = unsafe {
        device
            .get_logical()
            .create_graphics_pipelines(device.static_pipeline_cache(), &[pipeline_ci], None)
            .expect("Failed to create wrapped pipeline")
    };
    pipelines[0]
}

fn wrapped_pipeline_input_assembly_state(
    is_molten_vk: bool,
) -> vk::PipelineInputAssemblyStateCreateInfo<'static> {
    vk::PipelineInputAssemblyStateCreateInfo::default()
        .topology(vk::PrimitiveTopology::TRIANGLE_STRIP)
        .primitive_restart_enable(is_molten_vk)
        
}

/// Port of `CreateWrappedPipeline` — no blending.
pub fn create_wrapped_pipeline(
    device: &Device,
    renderpass: vk::RenderPass,
    layout: vk::PipelineLayout,
    vert_shader: vk::ShaderModule,
    frag_shader: vk::ShaderModule,
) -> vk::Pipeline {
    let blending = vk::PipelineColorBlendAttachmentState {
        blend_enable: vk::FALSE,
        src_color_blend_factor: vk::BlendFactor::ZERO,
        dst_color_blend_factor: vk::BlendFactor::ZERO,
        color_blend_op: vk::BlendOp::ADD,
        src_alpha_blend_factor: vk::BlendFactor::ZERO,
        dst_alpha_blend_factor: vk::BlendFactor::ZERO,
        alpha_blend_op: vk::BlendOp::ADD,
        color_write_mask: vk::ColorComponentFlags::R
            | vk::ColorComponentFlags::G
            | vk::ColorComponentFlags::B
            | vk::ColorComponentFlags::A,
    };
    create_wrapped_pipeline_impl(
        device,
        renderpass,
        layout,
        vert_shader,
        frag_shader,
        blending,
    )
}

/// Port of `CreateWrappedPremultipliedBlendingPipeline`.
pub fn create_wrapped_premultiplied_blending_pipeline(
    device: &Device,
    renderpass: vk::RenderPass,
    layout: vk::PipelineLayout,
    vert_shader: vk::ShaderModule,
    frag_shader: vk::ShaderModule,
) -> vk::Pipeline {
    let blending = vk::PipelineColorBlendAttachmentState {
        blend_enable: vk::TRUE,
        src_color_blend_factor: vk::BlendFactor::ONE,
        dst_color_blend_factor: vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
        color_blend_op: vk::BlendOp::ADD,
        src_alpha_blend_factor: vk::BlendFactor::ONE,
        dst_alpha_blend_factor: vk::BlendFactor::ZERO,
        alpha_blend_op: vk::BlendOp::ADD,
        color_write_mask: vk::ColorComponentFlags::R
            | vk::ColorComponentFlags::G
            | vk::ColorComponentFlags::B
            | vk::ColorComponentFlags::A,
    };
    create_wrapped_pipeline_impl(
        device,
        renderpass,
        layout,
        vert_shader,
        frag_shader,
        blending,
    )
}

/// Port of `CreateWrappedCoverageBlendingPipeline`.
pub fn create_wrapped_coverage_blending_pipeline(
    device: &Device,
    renderpass: vk::RenderPass,
    layout: vk::PipelineLayout,
    vert_shader: vk::ShaderModule,
    frag_shader: vk::ShaderModule,
) -> vk::Pipeline {
    let blending = vk::PipelineColorBlendAttachmentState {
        blend_enable: vk::TRUE,
        src_color_blend_factor: vk::BlendFactor::SRC_ALPHA,
        dst_color_blend_factor: vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
        color_blend_op: vk::BlendOp::ADD,
        src_alpha_blend_factor: vk::BlendFactor::ONE,
        dst_alpha_blend_factor: vk::BlendFactor::ZERO,
        alpha_blend_op: vk::BlendOp::ADD,
        color_write_mask: vk::ColorComponentFlags::R
            | vk::ColorComponentFlags::G
            | vk::ColorComponentFlags::B
            | vk::ColorComponentFlags::A,
    };
    create_wrapped_pipeline_impl(
        device,
        renderpass,
        layout,
        vert_shader,
        frag_shader,
        blending,
    )
}

// ---------------------------------------------------------------------------
// Descriptor set write helper
// ---------------------------------------------------------------------------

/// Port of `CreateWriteDescriptorSet`.
///
/// Pushes a new `VkDescriptorImageInfo` into `images` and returns a
/// `VkWriteDescriptorSet` pointing at it. The caller must keep `images` alive
/// until after `vkUpdateDescriptorSets`.
pub fn create_write_descriptor_set(
    images: &mut Vec<vk::DescriptorImageInfo>,
    sampler: vk::Sampler,
    view: vk::ImageView,
    set: vk::DescriptorSet,
    binding: u32,
) -> vk::WriteDescriptorSet<'static> {
    assert_fail_soft(images.capacity() > images.len(), || {
        "CreateWriteDescriptorSet requires pre-reserved image storage".to_owned()
    });
    images.push(vk::DescriptorImageInfo {
        sampler,
        image_view: view,
        image_layout: vk::ImageLayout::GENERAL,
    });
    let last = images.last().unwrap();
    vk::WriteDescriptorSet {
        s_type: vk::StructureType::WRITE_DESCRIPTOR_SET,
        p_next: std::ptr::null(),
        dst_set: set,
        dst_binding: binding,
        dst_array_element: 0,
        descriptor_count: 1,
        descriptor_type: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
        p_image_info: last as *const _,
        p_buffer_info: std::ptr::null(),
        p_texel_buffer_view: std::ptr::null(),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Render pass begin helper
// ---------------------------------------------------------------------------

/// Port of `BeginRenderPass`.
///
/// Begins a render pass and sets the viewport and scissor to cover the full
/// extent.
pub fn begin_render_pass(
    device: &ash::Device,
    cmdbuf: vk::CommandBuffer,
    render_pass: vk::RenderPass,
    framebuffer: vk::Framebuffer,
    extent: vk::Extent2D,
) {
    let renderpass_bi = vk::RenderPassBeginInfo::default()
        .render_pass(render_pass)
        .framebuffer(framebuffer)
        .render_area(vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent,
        });

    let viewport = vk::Viewport {
        x: 0.0,
        y: 0.0,
        width: extent.width as f32,
        height: extent.height as f32,
        min_depth: 0.0,
        max_depth: 1.0,
    };

    let scissor = vk::Rect2D {
        offset: vk::Offset2D { x: 0, y: 0 },
        extent,
    };

    unsafe {
        device.cmd_begin_render_pass(cmdbuf, &renderpass_bi, vk::SubpassContents::INLINE);
        device.cmd_set_viewport(cmdbuf, 0, &[viewport]);
        device.cmd_set_scissor(cmdbuf, 0, &[scissor]);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        create_write_descriptor_set, wrapped_descriptor_pool_sizes,
        wrapped_descriptor_set_layout_bindings, wrapped_pipeline_input_assembly_state,
        CubicFilterWeights,
    };
    use ash::vk;

    #[test]
    fn qcom_cubic_weight_values_match_vulkan() {
        assert_eq!(CubicFilterWeights::CATMULL_ROM.as_raw(), 0);
        assert_eq!(CubicFilterWeights::ZERO_TANGENT_CARDINAL.as_raw(), 1);
        assert_eq!(CubicFilterWeights::B_SPLINE.as_raw(), 2);
        assert_eq!(CubicFilterWeights::MITCHELL_NETRAVALI.as_raw(), 3);
        assert_eq!(
            vk::StructureType::SAMPLER_CUBIC_WEIGHTS_CREATE_INFO_QCOM.as_raw(),
            1_000_519_000
        );
    }

    #[test]
    fn descriptor_pool_preserves_an_explicit_empty_type_list() {
        assert!(wrapped_descriptor_pool_sizes(7, &[]).is_empty());

        let sizes = wrapped_descriptor_pool_sizes(
            7,
            &[
                vk::DescriptorType::STORAGE_IMAGE,
                vk::DescriptorType::UNIFORM_BUFFER,
            ],
        );
        assert_eq!(sizes.len(), 2);
        assert_eq!(sizes[0].ty, vk::DescriptorType::STORAGE_IMAGE);
        assert_eq!(sizes[0].descriptor_count, 7);
        assert_eq!(sizes[1].ty, vk::DescriptorType::UNIFORM_BUFFER);
        assert_eq!(sizes[1].descriptor_count, 7);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn descriptor_count_preserves_upstream_size_t_to_u32_cast() {
        let sizes = wrapped_descriptor_pool_sizes(
            u32::MAX as usize + 2,
            &[vk::DescriptorType::STORAGE_BUFFER],
        );
        assert_eq!(sizes[0].descriptor_count, 1);
    }

    #[test]
    fn descriptor_layout_preserves_the_requested_shader_stages() {
        let bindings = wrapped_descriptor_set_layout_bindings(
            &[
                vk::DescriptorType::STORAGE_IMAGE,
                vk::DescriptorType::STORAGE_BUFFER,
            ],
            vk::ShaderStageFlags::COMPUTE,
        );
        assert_eq!(bindings.len(), 2);
        assert_eq!(bindings[0].binding, 0);
        assert_eq!(bindings[1].binding, 1);
        assert_eq!(bindings[0].stage_flags, vk::ShaderStageFlags::COMPUTE);
        assert_eq!(bindings[1].stage_flags, vk::ShaderStageFlags::COMPUTE);
    }

    #[test]
    fn presentation_pipeline_enables_restart_only_for_molten_vk() {
        let native = wrapped_pipeline_input_assembly_state(false);
        let molten_vk = wrapped_pipeline_input_assembly_state(true);
        assert_eq!(native.topology, vk::PrimitiveTopology::TRIANGLE_STRIP);
        assert_eq!(native.primitive_restart_enable, vk::FALSE);
        assert_eq!(molten_vk.topology, vk::PrimitiveTopology::TRIANGLE_STRIP);
        assert_eq!(molten_vk.primitive_restart_enable, vk::TRUE);
    }

    #[test]
    fn descriptor_write_points_at_preallocated_image_storage() {
        let mut images = Vec::with_capacity(2);
        let write = create_write_descriptor_set(
            &mut images,
            vk::Sampler::null(),
            vk::ImageView::null(),
            vk::DescriptorSet::null(),
            3,
        );
        assert_eq!(write.dst_binding, 3);
        assert_eq!(write.descriptor_count, 1);
        assert_eq!(write.p_image_info, images.as_ptr());
        assert_eq!(images.capacity(), 2);
    }
}
