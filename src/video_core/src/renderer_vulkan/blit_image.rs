// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `blit_image.h` / `blit_image.cpp`.
//!
//! Helper that blits, converts, and clears images using fullscreen-triangle
//! fragment shaders. Manages pipelines for color blits, depth/stencil blits,
//! format conversions, and color/stencil clears.

use ash::vk;
use std::collections::VecDeque;
use std::ffi::CString;
use std::ptr::NonNull;

use crate::engines::fermi_2d::{Filter, Operation};
use crate::host_shaders::spirv_shaders::{
    BLIT_COLOR_FLOAT_FRAG_SPV, BLIT_COLOR_MSAA_FRAG_SPV, BLIT_DEPTH_MSAA_FRAG_SPV,
    BLIT_DEPTH_STENCIL_MSAA_FRAG_SPV, CONVERT_ABGR8_TO_D24S8_FRAG_SPV,
    CONVERT_ABGR8_TO_D32F_FRAG_SPV, CONVERT_D24S8_TO_ABGR8_FRAG_SPV,
    CONVERT_D32F_TO_ABGR8_FRAG_SPV, CONVERT_DEPTH_TO_FLOAT_FRAG_SPV,
    CONVERT_FLOAT_TO_DEPTH_FRAG_SPV, CONVERT_MSAA_TO_NON_MSAA_FRAG_SPV,
    CONVERT_NON_MSAA_TO_MSAA_FRAG_SPV, CONVERT_S8D24_TO_ABGR8_FRAG_SPV,
    FULL_SCREEN_TRIANGLE_VERT_SPV, VULKAN_BLIT_DEPTH_STENCIL_FRAG_SPV, VULKAN_COLOR_CLEAR_FRAG_SPV,
    VULKAN_COLOR_CLEAR_VERT_SPV, VULKAN_DEPTHSTENCIL_CLEAR_FRAG_SPV,
};
use crate::renderer_vulkan::descriptor_pool::{
    DescriptorAllocator, DescriptorBankInfo, DescriptorPool,
};
use crate::renderer_vulkan::render_pass_cache::{RenderPassCache, RenderPassKey};
use crate::renderer_vulkan::scheduler::Scheduler;
use crate::renderer_vulkan::shader_util::build_shader;
use crate::renderer_vulkan::state_tracker::StateTracker;
use crate::surface::{PixelFormat, SurfaceType};
use crate::texture_cache::samples_helper::samples_log2;
use crate::texture_cache::types::{ImageCopy, SubresourceRange, NUM_RT};
use crate::vulkan_common::vulkan_device::{Device, FormatType};

// ---------------------------------------------------------------------------
// Push constants (file-local, matching upstream anonymous namespace)
// ---------------------------------------------------------------------------

/// Port of anonymous `PushConstants` struct for blit operations.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
struct PushConstants {
    tex_scale: [f32; 2],
    tex_offset: [f32; 2],
}

/// Port of anonymous `MSAACopyPushConstants`.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
struct MsaaCopyPushConstants {
    dst_offset: [i32; 2],
    src_offset: [i32; 2],
    scale: [i32; 2],
}

// ---------------------------------------------------------------------------
// Pipeline key types
// ---------------------------------------------------------------------------

/// Port of `BlitImagePipelineKey`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlitImagePipelineKey {
    pub renderpass: vk::RenderPass,
    pub operation: Operation,
}

/// Port of `BlitDepthStencilPipelineKey`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlitDepthStencilPipelineKey {
    pub renderpass: vk::RenderPass,
    pub depth_clear: bool,
    pub stencil_mask: u8,
    pub stencil_compare_mask: u32,
    pub stencil_ref: u32,
}

/// Port of `MSAACopyPipelineKey`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct MsaaCopyPipelineKey {
    renderpass: vk::RenderPass,
    samples: vk::SampleCountFlags,
    msaa_to_non_msaa: bool,
}

/// Port of `BlitMSAAPipelineKey`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct BlitMsaaPipelineKey {
    renderpass: vk::RenderPass,
    samples: vk::SampleCountFlags,
}

/// Resources referenced by an asynchronously recorded MSAA copy.
///
/// Port of `BlitImageHelper::MSAACopyResources`.
struct MsaaCopyResources {
    tick: u64,
    src_view: vk::ImageView,
    dst_view: vk::ImageView,
    framebuffer: vk::Framebuffer,
}

/// Minimal framebuffer view consumed by `BlitImageHelper`, matching the
/// upstream `Framebuffer` methods used by `blit_image.cpp`.
#[derive(Debug, Clone, Copy)]
pub struct BlitFramebufferInfo {
    pub framebuffer: vk::Framebuffer,
    pub render_pass: vk::RenderPass,
    pub render_area: vk::Extent2D,
    pub images: [vk::Image; NUM_RT + 1],
    pub image_ranges: [vk::ImageSubresourceRange; NUM_RT + 1],
    pub num_images: usize,
    pub samples: vk::SampleCountFlags,
    pub has_stencil: bool,
}

/// Snapshot of the upstream `ImageView` data consumed by `BlitImageHelper`.
#[derive(Debug, Clone, Copy)]
pub struct BlitImageView {
    pub image: vk::Image,
    pub subresource_range: vk::ImageSubresourceRange,
    pub color_view: vk::ImageView,
    pub depth_view: vk::ImageView,
    pub stencil_view: vk::ImageView,
    pub size: Extent3D,
    pub is_rescaled: bool,
}

// ---------------------------------------------------------------------------
// Region / Extent helpers (matching upstream using statements)
// ---------------------------------------------------------------------------

/// 2D offset used for blit regions.
#[derive(Debug, Clone, Copy, Default)]
pub struct Offset2D {
    pub x: i32,
    pub y: i32,
}

/// 2D region defined by two corners.
#[derive(Debug, Clone, Copy, Default)]
pub struct Region2D {
    pub start: Offset2D,
    pub end: Offset2D,
}

/// 3D extent.
#[derive(Debug, Clone, Copy, Default)]
pub struct Extent3D {
    pub width: u32,
    pub height: u32,
    pub depth: u32,
}

/// Port of anonymous `SubresourceRangeFromView` and its
/// `AspectMaskFromFormat` helper.
pub(crate) fn subresource_range_from_view(
    format: PixelFormat,
    mut range: SubresourceRange,
    is_slice: bool,
) -> vk::ImageSubresourceRange {
    if is_slice {
        range.base.layer = 0;
        range.extent.layers = 1;
    }
    let aspect_mask = match crate::surface::get_format_type(format) {
        SurfaceType::ColorTexture => vk::ImageAspectFlags::COLOR,
        SurfaceType::Depth => vk::ImageAspectFlags::DEPTH,
        SurfaceType::Stencil => vk::ImageAspectFlags::STENCIL,
        SurfaceType::DepthStencil => vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL,
        SurfaceType::Invalid => vk::ImageAspectFlags::COLOR,
    };
    vk::ImageSubresourceRange {
        aspect_mask,
        base_mip_level: range.base.level as u32,
        level_count: range.extent.levels as u32,
        base_array_layer: range.base.layer as u32,
        layer_count: range.extent.layers as u32,
    }
}

fn assert_fail_soft(condition: bool, message: &str) {
    if condition {
        return;
    }
    log::error!("BlitImageHelper: {message}");
    if *common::settings::values().use_debug_asserts.get_value() {
        panic!("BlitImageHelper: {message}");
    }
}

fn update_one_texture_descriptor_set(
    device: &ash::Device,
    descriptor_set: vk::DescriptorSet,
    sampler: vk::Sampler,
    image_view: vk::ImageView,
) {
    let image_info = vk::DescriptorImageInfo {
        sampler,
        image_view,
        image_layout: vk::ImageLayout::GENERAL,
    };
    let write = vk::WriteDescriptorSet::builder()
        .dst_set(descriptor_set)
        .dst_binding(0)
        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
        .image_info(std::slice::from_ref(&image_info))
        .build();
    unsafe {
        device.update_descriptor_sets(&[write], &[]);
    }
}

fn update_two_textures_descriptor_set(
    device: &ash::Device,
    descriptor_set: vk::DescriptorSet,
    sampler: vk::Sampler,
    image_view_0: vk::ImageView,
    image_view_1: vk::ImageView,
) {
    let image_infos = [
        vk::DescriptorImageInfo {
            sampler,
            image_view: image_view_0,
            image_layout: vk::ImageLayout::GENERAL,
        },
        vk::DescriptorImageInfo {
            sampler,
            image_view: image_view_1,
            image_layout: vk::ImageLayout::GENERAL,
        },
    ];
    let writes = [
        vk::WriteDescriptorSet::builder()
            .dst_set(descriptor_set)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(std::slice::from_ref(&image_infos[0]))
            .build(),
        vk::WriteDescriptorSet::builder()
            .dst_set(descriptor_set)
            .dst_binding(1)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(std::slice::from_ref(&image_infos[1]))
            .build(),
    ];
    unsafe {
        device.update_descriptor_sets(&writes, &[]);
    }
}

fn bind_blit_state(
    device: &ash::Device,
    cmdbuf: vk::CommandBuffer,
    layout: vk::PipelineLayout,
    dst_region: Region2D,
    src_region: Region2D,
    src_size: Option<Extent3D>,
) {
    let offset = vk::Offset2D {
        x: dst_region.start.x.min(dst_region.end.x),
        y: dst_region.start.y.min(dst_region.end.y),
    };
    let extent = vk::Extent2D {
        width: dst_region.end.x.abs_diff(dst_region.start.x),
        height: dst_region.end.y.abs_diff(dst_region.start.y),
    };
    let viewport = vk::Viewport {
        x: offset.x as f32,
        y: offset.y as f32,
        width: extent.width as f32,
        height: extent.height as f32,
        min_depth: 0.0,
        max_depth: 1.0,
    };
    let scissor = vk::Rect2D { offset, extent };
    let src_size = src_size.unwrap_or(Extent3D {
        width: 1,
        height: 1,
        depth: 1,
    });
    let push_constants = PushConstants {
        tex_scale: [
            (src_region.end.x - src_region.start.x) as f32 / src_size.width as f32,
            (src_region.end.y - src_region.start.y) as f32 / src_size.height as f32,
        ],
        tex_offset: [
            src_region.start.x as f32 / src_size.width as f32,
            src_region.start.y as f32 / src_size.height as f32,
        ],
    };
    let push_bytes = unsafe {
        std::slice::from_raw_parts(
            (&push_constants as *const PushConstants).cast::<u8>(),
            std::mem::size_of::<PushConstants>(),
        )
    };
    unsafe {
        device.cmd_set_viewport(cmdbuf, 0, &[viewport]);
        device.cmd_set_scissor(cmdbuf, 0, &[scissor]);
        device.cmd_push_constants(cmdbuf, layout, vk::ShaderStageFlags::VERTEX, 0, push_bytes);
    }
}

fn bind_clear_state(device: &ash::Device, cmdbuf: vk::CommandBuffer, dst_region: Region2D) {
    let offset = vk::Offset2D {
        x: dst_region.start.x.min(dst_region.end.x),
        y: dst_region.start.y.min(dst_region.end.y),
    };
    let extent = vk::Extent2D {
        width: dst_region.end.x.abs_diff(dst_region.start.x),
        height: dst_region.end.y.abs_diff(dst_region.start.y),
    };
    let viewport = vk::Viewport {
        x: offset.x as f32,
        y: offset.y as f32,
        width: extent.width as f32,
        height: extent.height as f32,
        min_depth: 0.0,
        max_depth: 1.0,
    };
    let scissor = vk::Rect2D { offset, extent };
    unsafe {
        device.cmd_set_viewport(cmdbuf, 0, &[viewport]);
        device.cmd_set_scissor(cmdbuf, 0, &[scissor]);
    }
}

fn conversion_extent(src: BlitImageView) -> vk::Extent2D {
    let resolution = common::settings::values().resolution_info.clone();
    vk::Extent2D {
        width: if src.is_rescaled {
            resolution.scale_up_u32(src.size.width)
        } else {
            src.size.width
        },
        height: if src.is_rescaled {
            resolution.scale_up_u32(src.size.height)
        } else {
            src.size.height
        },
    }
}

/// Port of anonymous `RecordShaderReadBarrier`.
fn record_shader_read_barrier(
    device: &ash::Device,
    scheduler: &mut Scheduler,
    src_image_view: BlitImageView,
) {
    let device = device.clone();
    scheduler.request_outside_render_pass_operation_context();
    scheduler.record(move |cmdbuf| unsafe {
        let barrier = vk::ImageMemoryBarrier::builder()
            .src_access_mask(
                vk::AccessFlags::COLOR_ATTACHMENT_WRITE
                    | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE
                    | vk::AccessFlags::SHADER_WRITE
                    | vk::AccessFlags::TRANSFER_WRITE,
            )
            .dst_access_mask(vk::AccessFlags::SHADER_READ)
            .old_layout(vk::ImageLayout::GENERAL)
            .new_layout(vk::ImageLayout::GENERAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(src_image_view.image)
            .subresource_range(src_image_view.subresource_range)
            .build();
        device.cmd_pipeline_barrier(
            cmdbuf,
            vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
                | vk::PipelineStageFlags::COMPUTE_SHADER
                | vk::PipelineStageFlags::FRAGMENT_SHADER
                | vk::PipelineStageFlags::TRANSFER
                | vk::PipelineStageFlags::EARLY_FRAGMENT_TESTS
                | vk::PipelineStageFlags::LATE_FRAGMENT_TESTS,
            vk::PipelineStageFlags::FRAGMENT_SHADER | vk::PipelineStageFlags::COMPUTE_SHADER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[barrier],
        );
    });
}

/// Port of anonymous `GetPipelineInputAssemblyStateCreateInfo`.
fn pipeline_input_assembly_state(device: &Device) -> vk::PipelineInputAssemblyStateCreateInfo {
    vk::PipelineInputAssemblyStateCreateInfo::builder()
        .topology(vk::PrimitiveTopology::TRIANGLE_LIST)
        .primitive_restart_enable(device.is_molten_vk())
        .build()
}

fn pipeline_depth_stencil_state() -> vk::PipelineDepthStencilStateCreateInfo {
    let stencil = vk::StencilOpState {
        fail_op: vk::StencilOp::REPLACE,
        pass_op: vk::StencilOp::REPLACE,
        depth_fail_op: vk::StencilOp::KEEP,
        compare_op: vk::CompareOp::ALWAYS,
        compare_mask: 0,
        write_mask: u32::MAX,
        reference: 0,
    };
    vk::PipelineDepthStencilStateCreateInfo::builder()
        .depth_test_enable(true)
        .depth_write_enable(true)
        .depth_compare_op(vk::CompareOp::ALWAYS)
        .depth_bounds_test_enable(false)
        .stencil_test_enable(true)
        .front(stencil)
        .back(stencil)
        .build()
}

fn pipeline_depth_only_state() -> vk::PipelineDepthStencilStateCreateInfo {
    vk::PipelineDepthStencilStateCreateInfo::builder()
        .depth_test_enable(true)
        .depth_write_enable(true)
        .depth_compare_op(vk::CompareOp::ALWAYS)
        .depth_bounds_test_enable(false)
        .stencil_test_enable(false)
        .build()
}

fn sample_count_flag(num_samples: u32) -> vk::SampleCountFlags {
    match num_samples {
        2 => vk::SampleCountFlags::TYPE_2,
        4 => vk::SampleCountFlags::TYPE_4,
        8 => vk::SampleCountFlags::TYPE_8,
        16 => vk::SampleCountFlags::TYPE_16,
        _ => vk::SampleCountFlags::TYPE_1,
    }
}

fn make_msaa_copy_view(
    device: &ash::Device,
    image: vk::Image,
    format: vk::Format,
    base_level: u32,
) -> Result<vk::ImageView, vk::Result> {
    let create_info = vk::ImageViewCreateInfo::builder()
        .image(image)
        .view_type(vk::ImageViewType::TYPE_2D)
        .format(format)
        .components(vk::ComponentMapping {
            r: vk::ComponentSwizzle::IDENTITY,
            g: vk::ComponentSwizzle::IDENTITY,
            b: vk::ComponentSwizzle::IDENTITY,
            a: vk::ComponentSwizzle::IDENTITY,
        })
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: base_level,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        })
        .build();
    unsafe { device.create_image_view(&create_info, None) }
}

// ---------------------------------------------------------------------------
// BlitImageHelper
// ---------------------------------------------------------------------------

/// Port of `BlitImageHelper` class.
///
/// Provides GPU-accelerated blit, conversion, and clear operations via
/// fullscreen-triangle shaders and cached pipelines.
pub struct BlitImageHelper {
    device: ash::Device,
    device_owner: NonNull<Device>,
    scheduler: NonNull<Scheduler>,
    _state_tracker: NonNull<StateTracker>,
    shader_stencil_export_supported: bool,

    // Descriptor layouts
    one_texture_set_layout: vk::DescriptorSetLayout,
    two_textures_set_layout: vk::DescriptorSetLayout,
    one_texture_descriptor_allocator: DescriptorAllocator,
    two_textures_descriptor_allocator: DescriptorAllocator,

    // Pipeline layouts
    one_texture_pipeline_layout: vk::PipelineLayout,
    two_textures_pipeline_layout: vk::PipelineLayout,
    clear_color_pipeline_layout: vk::PipelineLayout,
    msaa_copy_pipeline_layout: vk::PipelineLayout,

    // Shader modules
    full_screen_vert: vk::ShaderModule,
    blit_color_to_color_frag: vk::ShaderModule,
    blit_color_msaa_frag: vk::ShaderModule,
    blit_depth_stencil_frag: vk::ShaderModule,
    blit_depth_msaa_frag: vk::ShaderModule,
    blit_depth_stencil_msaa_frag: vk::ShaderModule,
    clear_color_vert: vk::ShaderModule,
    clear_color_frag: vk::ShaderModule,
    clear_stencil_frag: vk::ShaderModule,
    convert_depth_to_float_frag: vk::ShaderModule,
    convert_float_to_depth_frag: vk::ShaderModule,
    convert_abgr8_to_d24s8_frag: vk::ShaderModule,
    convert_abgr8_to_d32f_frag: vk::ShaderModule,
    convert_d32f_to_abgr8_frag: vk::ShaderModule,
    convert_d24s8_to_abgr8_frag: vk::ShaderModule,
    convert_s8d24_to_abgr8_frag: vk::ShaderModule,
    convert_msaa_to_non_msaa_frag: vk::ShaderModule,
    convert_non_msaa_to_msaa_frag: vk::ShaderModule,

    // Samplers
    linear_sampler: vk::Sampler,
    nearest_sampler: vk::Sampler,

    // Cached pipeline vectors (key + pipeline in parallel)
    blit_color_keys: Vec<BlitImagePipelineKey>,
    blit_color_pipelines: Vec<vk::Pipeline>,
    blit_depth_stencil_keys: Vec<BlitImagePipelineKey>,
    blit_depth_stencil_pipelines: Vec<vk::Pipeline>,
    clear_color_keys: Vec<BlitImagePipelineKey>,
    clear_color_pipelines: Vec<vk::Pipeline>,
    clear_stencil_keys: Vec<BlitDepthStencilPipelineKey>,
    clear_stencil_pipelines: Vec<vk::Pipeline>,
    msaa_copy_keys: Vec<MsaaCopyPipelineKey>,
    msaa_copy_pipelines: Vec<vk::Pipeline>,
    blit_msaa_color_keys: Vec<BlitMsaaPipelineKey>,
    blit_msaa_color_pipelines: Vec<vk::Pipeline>,
    resolve_depth_keys: Vec<vk::RenderPass>,
    resolve_depth_pipelines: Vec<vk::Pipeline>,
    resolve_depth_stencil_keys: Vec<vk::RenderPass>,
    resolve_depth_stencil_pipelines: Vec<vk::Pipeline>,
    msaa_copy_resources: VecDeque<MsaaCopyResources>,

    // Conversion pipelines (lazily created)
    convert_d32_to_r32_pipeline: vk::Pipeline,
    convert_r32_to_d32_pipeline: vk::Pipeline,
    convert_d16_to_r16_pipeline: vk::Pipeline,
    convert_r16_to_d16_pipeline: vk::Pipeline,
    convert_abgr8_to_d24s8_pipeline: vk::Pipeline,
    convert_abgr8_to_d32f_pipeline: vk::Pipeline,
    convert_d32f_to_abgr8_pipeline: vk::Pipeline,
    convert_d24s8_to_abgr8_pipeline: vk::Pipeline,
    convert_s8d24_to_abgr8_pipeline: vk::Pipeline,
}

impl BlitImageHelper {
    const ONE_TEXTURE_BANK_INFO: DescriptorBankInfo = DescriptorBankInfo {
        uniform_buffers: 0,
        storage_buffers: 0,
        texture_buffers: 0,
        image_buffers: 0,
        textures: 1,
        images: 0,
        score: 2,
    };

    const TWO_TEXTURES_BANK_INFO: DescriptorBankInfo = DescriptorBankInfo {
        uniform_buffers: 0,
        storage_buffers: 0,
        texture_buffers: 0,
        image_buffers: 0,
        textures: 2,
        images: 0,
        score: 2,
    };

    /// Port of `BlitImageHelper::BlitImageHelper`.
    pub fn new(
        vulkan_device: &Device,
        scheduler: &mut Scheduler,
        state_tracker: &mut StateTracker,
        descriptor_pool: &mut DescriptorPool,
    ) -> Self {
        let device = vulkan_device.get_logical().clone();
        let shader_stencil_export_supported =
            vulkan_device.is_ext_shader_stencil_export_supported();
        // Create one-texture descriptor set layout (1 combined image sampler)
        let one_tex_binding = vk::DescriptorSetLayoutBinding {
            binding: 0,
            descriptor_type: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
            descriptor_count: 1,
            stage_flags: vk::ShaderStageFlags::FRAGMENT,
            p_immutable_samplers: std::ptr::null(),
        };
        let one_tex_layout_ci = vk::DescriptorSetLayoutCreateInfo::builder()
            .bindings(std::slice::from_ref(&one_tex_binding))
            .build();
        let one_texture_set_layout = unsafe {
            device
                .create_descriptor_set_layout(&one_tex_layout_ci, None)
                .expect("Failed to create one-texture set layout")
        };

        // Create two-texture descriptor set layout (2 combined image samplers)
        let two_tex_bindings = [
            vk::DescriptorSetLayoutBinding {
                binding: 0,
                descriptor_type: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                descriptor_count: 1,
                stage_flags: vk::ShaderStageFlags::FRAGMENT,
                p_immutable_samplers: std::ptr::null(),
            },
            vk::DescriptorSetLayoutBinding {
                binding: 1,
                descriptor_type: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                descriptor_count: 1,
                stage_flags: vk::ShaderStageFlags::FRAGMENT,
                p_immutable_samplers: std::ptr::null(),
            },
        ];
        let two_tex_layout_ci = vk::DescriptorSetLayoutCreateInfo::builder()
            .bindings(&two_tex_bindings)
            .build();
        let two_textures_set_layout = unsafe {
            device
                .create_descriptor_set_layout(&two_tex_layout_ci, None)
                .expect("Failed to create two-textures set layout")
        };
        let one_texture_descriptor_allocator = descriptor_pool
            .allocator(one_texture_set_layout, &Self::ONE_TEXTURE_BANK_INFO)
            .expect("Failed to create one-texture descriptor allocator");
        let two_textures_descriptor_allocator = descriptor_pool
            .allocator(two_textures_set_layout, &Self::TWO_TEXTURES_BANK_INFO)
            .expect("Failed to create two-texture descriptor allocator");

        // Create one-texture pipeline layout with push constants
        let push_range = vk::PushConstantRange {
            stage_flags: vk::ShaderStageFlags::VERTEX,
            offset: 0,
            size: std::mem::size_of::<PushConstants>() as u32,
        };
        let one_tex_layouts = [one_texture_set_layout];
        let one_tex_pl_ci = vk::PipelineLayoutCreateInfo::builder()
            .set_layouts(&one_tex_layouts)
            .push_constant_ranges(std::slice::from_ref(&push_range))
            .build();
        let one_texture_pipeline_layout = unsafe {
            device
                .create_pipeline_layout(&one_tex_pl_ci, None)
                .expect("Failed to create one-texture pipeline layout")
        };

        // Create two-texture pipeline layout with push constants
        let two_tex_layouts = [two_textures_set_layout];
        let two_tex_pl_ci = vk::PipelineLayoutCreateInfo::builder()
            .set_layouts(&two_tex_layouts)
            .push_constant_ranges(std::slice::from_ref(&push_range))
            .build();
        let two_textures_pipeline_layout = unsafe {
            device
                .create_pipeline_layout(&two_tex_pl_ci, None)
                .expect("Failed to create two-textures pipeline layout")
        };

        // Create clear color pipeline layout (no descriptor sets, push constants for color)
        let clear_push_range = vk::PushConstantRange {
            stage_flags: vk::ShaderStageFlags::FRAGMENT,
            offset: 0,
            size: 4 * std::mem::size_of::<f32>() as u32, // 4 floats for color
        };
        let clear_pl_ci = vk::PipelineLayoutCreateInfo::builder()
            .push_constant_ranges(std::slice::from_ref(&clear_push_range))
            .build();
        let clear_color_pipeline_layout = unsafe {
            device
                .create_pipeline_layout(&clear_pl_ci, None)
                .expect("Failed to create clear color pipeline layout")
        };

        let msaa_copy_push_range = vk::PushConstantRange {
            stage_flags: vk::ShaderStageFlags::FRAGMENT,
            offset: 0,
            size: std::mem::size_of::<MsaaCopyPushConstants>() as u32,
        };
        let msaa_copy_pl_ci = vk::PipelineLayoutCreateInfo::builder()
            .set_layouts(&one_tex_layouts)
            .push_constant_ranges(std::slice::from_ref(&msaa_copy_push_range))
            .build();
        let msaa_copy_pipeline_layout = unsafe {
            device
                .create_pipeline_layout(&msaa_copy_pl_ci, None)
                .expect("Failed to create MSAA copy pipeline layout")
        };

        let full_screen_vert = build_shader(&device, FULL_SCREEN_TRIANGLE_VERT_SPV)
            .expect("Failed to build full_screen_triangle.vert");
        let blit_color_to_color_frag = build_shader(&device, BLIT_COLOR_FLOAT_FRAG_SPV)
            .expect("Failed to build blit_color_float.frag");
        let blit_color_msaa_frag = build_shader(&device, BLIT_COLOR_MSAA_FRAG_SPV)
            .expect("Failed to build blit_color_msaa.frag");
        let blit_depth_stencil_frag = if shader_stencil_export_supported {
            build_shader(&device, VULKAN_BLIT_DEPTH_STENCIL_FRAG_SPV)
                .expect("Failed to build vulkan_blit_depth_stencil.frag")
        } else {
            vk::ShaderModule::null()
        };
        let blit_depth_msaa_frag = build_shader(&device, BLIT_DEPTH_MSAA_FRAG_SPV)
            .expect("Failed to build blit_depth_msaa.frag");
        let blit_depth_stencil_msaa_frag = if shader_stencil_export_supported {
            build_shader(&device, BLIT_DEPTH_STENCIL_MSAA_FRAG_SPV)
                .expect("Failed to build blit_depth_stencil_msaa.frag")
        } else {
            vk::ShaderModule::null()
        };
        let clear_color_vert = build_shader(&device, VULKAN_COLOR_CLEAR_VERT_SPV)
            .expect("Failed to build vulkan_color_clear.vert");
        let clear_color_frag = build_shader(&device, VULKAN_COLOR_CLEAR_FRAG_SPV)
            .expect("Failed to build vulkan_color_clear.frag");
        let clear_stencil_frag = build_shader(&device, VULKAN_DEPTHSTENCIL_CLEAR_FRAG_SPV)
            .expect("Failed to build vulkan_depthstencil_clear.frag");
        let convert_depth_to_float_frag = build_shader(&device, CONVERT_DEPTH_TO_FLOAT_FRAG_SPV)
            .expect("Failed to build convert_depth_to_float.frag");
        let convert_float_to_depth_frag = build_shader(&device, CONVERT_FLOAT_TO_DEPTH_FRAG_SPV)
            .expect("Failed to build convert_float_to_depth.frag");
        let convert_abgr8_to_d24s8_frag = if shader_stencil_export_supported {
            build_shader(&device, CONVERT_ABGR8_TO_D24S8_FRAG_SPV)
                .expect("Failed to build convert_abgr8_to_d24s8.frag")
        } else {
            vk::ShaderModule::null()
        };
        let convert_abgr8_to_d32f_frag = build_shader(&device, CONVERT_ABGR8_TO_D32F_FRAG_SPV)
            .expect("Failed to build convert_abgr8_to_d32f.frag");
        let convert_d32f_to_abgr8_frag = build_shader(&device, CONVERT_D32F_TO_ABGR8_FRAG_SPV)
            .expect("Failed to build convert_d32f_to_abgr8.frag");
        let convert_d24s8_to_abgr8_frag = build_shader(&device, CONVERT_D24S8_TO_ABGR8_FRAG_SPV)
            .expect("Failed to build convert_d24s8_to_abgr8.frag");
        let convert_s8d24_to_abgr8_frag = build_shader(&device, CONVERT_S8D24_TO_ABGR8_FRAG_SPV)
            .expect("Failed to build convert_s8d24_to_abgr8.frag");
        let convert_msaa_to_non_msaa_frag =
            build_shader(&device, CONVERT_MSAA_TO_NON_MSAA_FRAG_SPV)
                .expect("Failed to build convert_msaa_to_non_msaa.frag");
        let convert_non_msaa_to_msaa_frag =
            build_shader(&device, CONVERT_NON_MSAA_TO_MSAA_FRAG_SPV)
                .expect("Failed to build convert_non_msaa_to_msaa.frag");

        // Create samplers
        let linear_sampler_ci = vk::SamplerCreateInfo::builder()
            .mag_filter(vk::Filter::LINEAR)
            .min_filter(vk::Filter::LINEAR)
            .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
            .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_BORDER)
            .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_BORDER)
            .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_BORDER)
            .compare_op(vk::CompareOp::NEVER)
            .border_color(vk::BorderColor::FLOAT_OPAQUE_WHITE)
            .unnormalized_coordinates(true)
            .build();
        let linear_sampler = unsafe {
            device
                .create_sampler(&linear_sampler_ci, None)
                .expect("Failed to create linear sampler")
        };

        let nearest_sampler_ci = vk::SamplerCreateInfo::builder()
            .mag_filter(vk::Filter::NEAREST)
            .min_filter(vk::Filter::NEAREST)
            .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
            .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_BORDER)
            .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_BORDER)
            .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_BORDER)
            .compare_op(vk::CompareOp::NEVER)
            .border_color(vk::BorderColor::FLOAT_OPAQUE_WHITE)
            .unnormalized_coordinates(true)
            .build();
        let nearest_sampler = unsafe {
            device
                .create_sampler(&nearest_sampler_ci, None)
                .expect("Failed to create nearest sampler")
        };

        BlitImageHelper {
            device,
            device_owner: NonNull::from(vulkan_device),
            scheduler: NonNull::from(scheduler),
            _state_tracker: NonNull::from(state_tracker),
            shader_stencil_export_supported,
            one_texture_set_layout,
            two_textures_set_layout,
            one_texture_descriptor_allocator,
            two_textures_descriptor_allocator,
            one_texture_pipeline_layout,
            two_textures_pipeline_layout,
            clear_color_pipeline_layout,
            msaa_copy_pipeline_layout,
            full_screen_vert,
            blit_color_to_color_frag,
            blit_color_msaa_frag,
            blit_depth_stencil_frag,
            blit_depth_msaa_frag,
            blit_depth_stencil_msaa_frag,
            clear_color_vert,
            clear_color_frag,
            clear_stencil_frag,
            convert_depth_to_float_frag,
            convert_float_to_depth_frag,
            convert_abgr8_to_d24s8_frag,
            convert_abgr8_to_d32f_frag,
            convert_d32f_to_abgr8_frag,
            convert_d24s8_to_abgr8_frag,
            convert_s8d24_to_abgr8_frag,
            convert_msaa_to_non_msaa_frag,
            convert_non_msaa_to_msaa_frag,
            linear_sampler,
            nearest_sampler,
            blit_color_keys: Vec::new(),
            blit_color_pipelines: Vec::new(),
            blit_depth_stencil_keys: Vec::new(),
            blit_depth_stencil_pipelines: Vec::new(),
            clear_color_keys: Vec::new(),
            clear_color_pipelines: Vec::new(),
            clear_stencil_keys: Vec::new(),
            clear_stencil_pipelines: Vec::new(),
            msaa_copy_keys: Vec::new(),
            msaa_copy_pipelines: Vec::new(),
            blit_msaa_color_keys: Vec::new(),
            blit_msaa_color_pipelines: Vec::new(),
            resolve_depth_keys: Vec::new(),
            resolve_depth_pipelines: Vec::new(),
            resolve_depth_stencil_keys: Vec::new(),
            resolve_depth_stencil_pipelines: Vec::new(),
            msaa_copy_resources: VecDeque::new(),
            convert_d32_to_r32_pipeline: vk::Pipeline::null(),
            convert_r32_to_d32_pipeline: vk::Pipeline::null(),
            convert_d16_to_r16_pipeline: vk::Pipeline::null(),
            convert_r16_to_d16_pipeline: vk::Pipeline::null(),
            convert_abgr8_to_d24s8_pipeline: vk::Pipeline::null(),
            convert_abgr8_to_d32f_pipeline: vk::Pipeline::null(),
            convert_d32f_to_abgr8_pipeline: vk::Pipeline::null(),
            convert_d24s8_to_abgr8_pipeline: vk::Pipeline::null(),
            convert_s8d24_to_abgr8_pipeline: vk::Pipeline::null(),
        }
    }

    pub fn shader_stencil_export_supported(&self) -> bool {
        self.shader_stencil_export_supported
    }

    /// Port of `BlitImageHelper::BlitColor` (sampled blit variant).
    ///
    /// Blits a source image view to a destination framebuffer using the
    /// specified filter and operation.
    pub fn blit_color(
        &mut self,
        dst_framebuffer: BlitFramebufferInfo,
        src_image_view: BlitImageView,
        dst_region: &Region2D,
        src_region: &Region2D,
        filter: Filter,
        operation: Operation,
    ) -> bool {
        let key = BlitImagePipelineKey {
            renderpass: dst_framebuffer.render_pass,
            operation,
        };
        let pipeline = match self.find_or_emplace_color_pipeline(&key) {
            Ok(pipeline) => pipeline,
            Err(err) => {
                log::warn!("BlitImageHelper: failed to create color blit pipeline: {err:?}");
                return false;
            }
        };
        let layout = self.one_texture_pipeline_layout;
        let sampler = if filter == Filter::Bilinear {
            self.linear_sampler
        } else {
            self.nearest_sampler
        };
        let descriptor_allocator = self.one_texture_descriptor_allocator.clone();
        let src_view = src_image_view.color_view;
        let render_area = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: dst_framebuffer.render_area,
        };
        let device = self.device.clone();
        let dst_region = *dst_region;
        let src_region = *src_region;
        let scheduler = unsafe { self.scheduler.as_mut() };
        record_shader_read_barrier(&self.device, scheduler, src_image_view);
        scheduler.request_renderpass_raw(
            dst_framebuffer.framebuffer,
            dst_framebuffer.render_pass,
            render_area,
            &[],
            &dst_framebuffer.images[..dst_framebuffer.num_images],
            &dst_framebuffer.image_ranges[..dst_framebuffer.num_images],
        );
        scheduler.record(move |cmdbuf| unsafe {
            let descriptor_set = descriptor_allocator
                .commit()
                .expect("Failed to allocate color blit descriptor set");
            update_one_texture_descriptor_set(&device, descriptor_set, sampler, src_view);
            device.cmd_bind_pipeline(cmdbuf, vk::PipelineBindPoint::GRAPHICS, pipeline);
            device.cmd_bind_descriptor_sets(
                cmdbuf,
                vk::PipelineBindPoint::GRAPHICS,
                layout,
                0,
                &[descriptor_set],
                &[],
            );
            bind_blit_state(&device, cmdbuf, layout, dst_region, src_region, None);
            device.cmd_draw(cmdbuf, 3, 1, 0, 0);
        });
        scheduler.invalidate_state();
        true
    }

    /// Port of `BlitImageHelper::BlitColorMSAA`.
    pub fn blit_color_msaa(
        &mut self,
        dst_framebuffer: BlitFramebufferInfo,
        src_image_view: BlitImageView,
        dst_region: &Region2D,
        src_region: &Region2D,
    ) -> bool {
        let key = BlitMsaaPipelineKey {
            renderpass: dst_framebuffer.render_pass,
            samples: dst_framebuffer.samples,
        };
        let pipeline = match self.find_or_emplace_blit_color_msaa_pipeline(&key) {
            Ok(pipeline) => pipeline,
            Err(err) => {
                log::warn!("BlitImageHelper: failed to create MSAA color blit pipeline: {err:?}");
                return false;
            }
        };
        let layout = self.one_texture_pipeline_layout;
        let sampler = self.nearest_sampler;
        let descriptor_allocator = self.one_texture_descriptor_allocator.clone();
        let src_view = src_image_view.color_view;
        let render_area = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: dst_framebuffer.render_area,
        };
        let device = self.device.clone();
        let dst_region = *dst_region;
        let src_region = *src_region;
        let scheduler = unsafe { self.scheduler.as_mut() };
        record_shader_read_barrier(&self.device, scheduler, src_image_view);
        scheduler.request_renderpass_raw(
            dst_framebuffer.framebuffer,
            dst_framebuffer.render_pass,
            render_area,
            &[],
            &dst_framebuffer.images[..dst_framebuffer.num_images],
            &dst_framebuffer.image_ranges[..dst_framebuffer.num_images],
        );
        scheduler.record(move |cmdbuf| unsafe {
            let descriptor_set = descriptor_allocator
                .commit()
                .expect("Failed to allocate MSAA color blit descriptor set");
            update_one_texture_descriptor_set(&device, descriptor_set, sampler, src_view);
            device.cmd_bind_pipeline(cmdbuf, vk::PipelineBindPoint::GRAPHICS, pipeline);
            device.cmd_bind_descriptor_sets(
                cmdbuf,
                vk::PipelineBindPoint::GRAPHICS,
                layout,
                0,
                &[descriptor_set],
                &[],
            );
            bind_blit_state(&device, cmdbuf, layout, dst_region, src_region, None);
            device.cmd_draw(cmdbuf, 3, 1, 0, 0);
        });
        scheduler.invalidate_state();
        true
    }

    /// Port of `BlitImageHelper::ResolveDepthStencil`.
    pub fn resolve_depth_stencil(
        &mut self,
        dst_framebuffer: BlitFramebufferInfo,
        src_image_view: BlitImageView,
        dst_region: &Region2D,
        src_region: &Region2D,
    ) -> bool {
        let resolve_stencil = dst_framebuffer.has_stencil && self.shader_stencil_export_supported;
        let pipeline = match self.find_or_emplace_resolve_depth_stencil_pipeline(
            dst_framebuffer.render_pass,
            resolve_stencil,
        ) {
            Ok(pipeline) => pipeline,
            Err(err) => {
                log::warn!(
                    "BlitImageHelper: failed to create depth/stencil resolve pipeline: {err:?}"
                );
                return false;
            }
        };
        let layout = if resolve_stencil {
            self.two_textures_pipeline_layout
        } else {
            self.one_texture_pipeline_layout
        };
        let sampler = self.nearest_sampler;
        let descriptor_allocator = if resolve_stencil {
            self.two_textures_descriptor_allocator.clone()
        } else {
            self.one_texture_descriptor_allocator.clone()
        };
        let src_depth_view = src_image_view.depth_view;
        let src_stencil_view = if resolve_stencil {
            src_image_view.stencil_view
        } else {
            vk::ImageView::null()
        };

        let render_area = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: dst_framebuffer.render_area,
        };
        let device = self.device.clone();
        let dst_region = *dst_region;
        let src_region = *src_region;
        let scheduler = unsafe { self.scheduler.as_mut() };
        record_shader_read_barrier(&self.device, scheduler, src_image_view);
        scheduler.request_renderpass_raw(
            dst_framebuffer.framebuffer,
            dst_framebuffer.render_pass,
            render_area,
            &[],
            &dst_framebuffer.images[..dst_framebuffer.num_images],
            &dst_framebuffer.image_ranges[..dst_framebuffer.num_images],
        );
        scheduler.record(move |cmdbuf| unsafe {
            let descriptor_set = descriptor_allocator
                .commit()
                .expect("Failed to allocate depth/stencil resolve descriptor set");
            if resolve_stencil {
                update_two_textures_descriptor_set(
                    &device,
                    descriptor_set,
                    sampler,
                    src_depth_view,
                    src_stencil_view,
                );
            } else {
                update_one_texture_descriptor_set(&device, descriptor_set, sampler, src_depth_view);
            }
            device.cmd_bind_descriptor_sets(
                cmdbuf,
                vk::PipelineBindPoint::GRAPHICS,
                layout,
                0,
                &[descriptor_set],
                &[],
            );
            device.cmd_bind_pipeline(cmdbuf, vk::PipelineBindPoint::GRAPHICS, pipeline);
            bind_blit_state(&device, cmdbuf, layout, dst_region, src_region, None);
            device.cmd_draw(cmdbuf, 3, 1, 0, 0);
        });
        scheduler.invalidate_state();
        true
    }

    /// Port of `BlitImageHelper::BlitColor` (explicit image + sampler variant).
    pub fn blit_color_with_sampler(
        &mut self,
        dst_framebuffer: BlitFramebufferInfo,
        src_image_view: vk::ImageView,
        src_image: vk::Image,
        src_sampler: vk::Sampler,
        dst_region: &Region2D,
        src_region: &Region2D,
        src_size: &Extent3D,
    ) -> bool {
        let key = BlitImagePipelineKey {
            renderpass: dst_framebuffer.render_pass,
            operation: Operation::SrcCopy,
        };
        let pipeline = match self.find_or_emplace_color_pipeline(&key) {
            Ok(pipeline) => pipeline,
            Err(err) => {
                log::warn!("BlitImageHelper: failed to create draw-texture pipeline: {err:?}");
                return false;
            }
        };
        let descriptor_allocator = self.one_texture_descriptor_allocator.clone();
        let layout = self.one_texture_pipeline_layout;
        let dst_region = *dst_region;
        let src_region = *src_region;
        let src_size = *src_size;
        let device = self.device.clone();
        let scheduler = unsafe { self.scheduler.as_mut() };
        scheduler.request_outside_render_pass_operation_context();
        scheduler.record(move |cmdbuf| unsafe {
            let access = vk::AccessFlags::COLOR_ATTACHMENT_READ
                | vk::AccessFlags::COLOR_ATTACHMENT_WRITE
                | vk::AccessFlags::SHADER_READ;
            let barrier = vk::ImageMemoryBarrier::builder()
                .src_access_mask(access)
                .dst_access_mask(access)
                .old_layout(vk::ImageLayout::GENERAL)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(src_image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .build();
            device.cmd_pipeline_barrier(
                cmdbuf,
                vk::PipelineStageFlags::ALL_GRAPHICS | vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::PipelineStageFlags::ALL_GRAPHICS | vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier],
            );

            let begin = vk::RenderPassBeginInfo::builder()
                .render_pass(dst_framebuffer.render_pass)
                .framebuffer(dst_framebuffer.framebuffer)
                .render_area(vk::Rect2D {
                    offset: vk::Offset2D { x: 0, y: 0 },
                    extent: dst_framebuffer.render_area,
                })
                .build();
            device.cmd_begin_render_pass(cmdbuf, &begin, vk::SubpassContents::INLINE);
            let descriptor_set = descriptor_allocator
                .commit()
                .expect("Failed to allocate draw-texture descriptor set");
            update_one_texture_descriptor_set(&device, descriptor_set, src_sampler, src_image_view);
            device.cmd_bind_pipeline(cmdbuf, vk::PipelineBindPoint::GRAPHICS, pipeline);
            device.cmd_bind_descriptor_sets(
                cmdbuf,
                vk::PipelineBindPoint::GRAPHICS,
                layout,
                0,
                &[descriptor_set],
                &[],
            );
            bind_blit_state(
                &device,
                cmdbuf,
                layout,
                dst_region,
                src_region,
                Some(src_size),
            );
            device.cmd_draw(cmdbuf, 3, 1, 0, 0);
            device.cmd_end_render_pass(cmdbuf);
        });
        true
    }

    /// Port of `BlitImageHelper::BlitDepthStencil`.
    pub fn blit_depth_stencil(
        &mut self,
        dst_framebuffer: BlitFramebufferInfo,
        src_image_view: BlitImageView,
        dst_region: &Region2D,
        src_region: &Region2D,
        filter: Filter,
        operation: Operation,
    ) -> bool {
        if !self.shader_stencil_export_supported {
            return false;
        }
        assert_fail_soft(
            filter == Filter::Point,
            "depth/stencil blit requires point filtering",
        );
        assert_fail_soft(
            operation == Operation::SrcCopy,
            "depth/stencil blit requires SrcCopy",
        );
        let key = BlitImagePipelineKey {
            renderpass: dst_framebuffer.render_pass,
            operation,
        };
        let pipeline = match self.find_or_emplace_depth_stencil_pipeline(&key) {
            Ok(pipeline) => pipeline,
            Err(err) => {
                log::warn!(
                    "BlitImageHelper: failed to create depth/stencil blit pipeline: {err:?}"
                );
                return false;
            }
        };
        let layout = self.two_textures_pipeline_layout;
        let sampler = self.nearest_sampler;
        let descriptor_allocator = self.two_textures_descriptor_allocator.clone();
        let src_depth_view = src_image_view.depth_view;
        let src_stencil_view = src_image_view.stencil_view;
        let render_area = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: dst_framebuffer.render_area,
        };
        let device = self.device.clone();
        let dst_region = *dst_region;
        let src_region = *src_region;
        let scheduler = unsafe { self.scheduler.as_mut() };
        record_shader_read_barrier(&self.device, scheduler, src_image_view);
        scheduler.request_renderpass_raw(
            dst_framebuffer.framebuffer,
            dst_framebuffer.render_pass,
            render_area,
            &[],
            &dst_framebuffer.images[..dst_framebuffer.num_images],
            &dst_framebuffer.image_ranges[..dst_framebuffer.num_images],
        );
        scheduler.record(move |cmdbuf| unsafe {
            let descriptor_set = descriptor_allocator
                .commit()
                .expect("Failed to allocate depth/stencil blit descriptor set");
            update_two_textures_descriptor_set(
                &device,
                descriptor_set,
                sampler,
                src_depth_view,
                src_stencil_view,
            );
            device.cmd_bind_pipeline(cmdbuf, vk::PipelineBindPoint::GRAPHICS, pipeline);
            device.cmd_bind_descriptor_sets(
                cmdbuf,
                vk::PipelineBindPoint::GRAPHICS,
                layout,
                0,
                &[descriptor_set],
                &[],
            );
            bind_blit_state(&device, cmdbuf, layout, dst_region, src_region, None);
            device.cmd_draw(cmdbuf, 3, 1, 0, 0);
        });
        scheduler.invalidate_state();
        true
    }

    /// Port of `BlitImageHelper::ConvertD32ToR32`.
    pub fn convert_d32_to_r32(
        &mut self,
        dst_framebuffer: BlitFramebufferInfo,
        src_image_view: BlitImageView,
    ) -> bool {
        let pipeline = match self.convert_depth_to_color_pipeline(
            self.convert_d32_to_r32_pipeline,
            dst_framebuffer.render_pass,
        ) {
            Ok(pipeline) => pipeline,
            Err(err) => {
                log::warn!("BlitImageHelper: failed to create D32->R32 pipeline: {err:?}");
                return false;
            }
        };
        self.convert_d32_to_r32_pipeline = pipeline;
        self.convert(pipeline, dst_framebuffer, src_image_view)
    }

    /// Port of `BlitImageHelper::ConvertR32ToD32`.
    pub fn convert_r32_to_d32(
        &mut self,
        dst_framebuffer: BlitFramebufferInfo,
        src_image_view: BlitImageView,
    ) -> bool {
        let pipeline = match self.convert_color_to_depth_pipeline(
            self.convert_r32_to_d32_pipeline,
            dst_framebuffer.render_pass,
        ) {
            Ok(pipeline) => pipeline,
            Err(err) => {
                log::warn!("BlitImageHelper: failed to create R32->D32 pipeline: {err:?}");
                return false;
            }
        };
        self.convert_r32_to_d32_pipeline = pipeline;
        self.convert(pipeline, dst_framebuffer, src_image_view)
    }

    /// Port of `BlitImageHelper::ConvertD16ToR16`.
    pub fn convert_d16_to_r16(
        &mut self,
        dst_framebuffer: BlitFramebufferInfo,
        src_image_view: BlitImageView,
    ) -> bool {
        let pipeline = match self.convert_depth_to_color_pipeline(
            self.convert_d16_to_r16_pipeline,
            dst_framebuffer.render_pass,
        ) {
            Ok(pipeline) => pipeline,
            Err(err) => {
                log::warn!("BlitImageHelper: failed to create D16->R16 pipeline: {err:?}");
                return false;
            }
        };
        self.convert_d16_to_r16_pipeline = pipeline;
        self.convert(pipeline, dst_framebuffer, src_image_view)
    }

    /// Port of `BlitImageHelper::ConvertR16ToD16`.
    pub fn convert_r16_to_d16(
        &mut self,
        dst_framebuffer: BlitFramebufferInfo,
        src_image_view: BlitImageView,
    ) -> bool {
        let pipeline = match self.convert_color_to_depth_pipeline(
            self.convert_r16_to_d16_pipeline,
            dst_framebuffer.render_pass,
        ) {
            Ok(pipeline) => pipeline,
            Err(err) => {
                log::warn!("BlitImageHelper: failed to create R16->D16 pipeline: {err:?}");
                return false;
            }
        };
        self.convert_r16_to_d16_pipeline = pipeline;
        self.convert(pipeline, dst_framebuffer, src_image_view)
    }

    /// Port of `BlitImageHelper::ConvertABGR8ToD24S8`.
    pub fn convert_abgr8_to_d24s8(
        &mut self,
        dst_framebuffer: BlitFramebufferInfo,
        src_image_view: BlitImageView,
    ) -> bool {
        if !self.shader_stencil_export_supported {
            log::warn!(
                "BlitImageHelper: ConvertABGR8ToD24S8 requires shader_stencil_export, skipping"
            );
            return false;
        }
        let pipeline = match self.convert_pipeline_depth_target_ex(
            self.convert_abgr8_to_d24s8_pipeline,
            dst_framebuffer.render_pass,
            self.convert_abgr8_to_d24s8_frag,
        ) {
            Ok(pipeline) => pipeline,
            Err(err) => {
                log::warn!("BlitImageHelper: failed to create ABGR8->D24S8 pipeline: {err:?}");
                return false;
            }
        };
        self.convert_abgr8_to_d24s8_pipeline = pipeline;
        self.convert(pipeline, dst_framebuffer, src_image_view)
    }

    /// Port of `BlitImageHelper::ConvertABGR8ToD32F`.
    pub fn convert_abgr8_to_d32f(
        &mut self,
        dst_framebuffer: BlitFramebufferInfo,
        src_image_view: BlitImageView,
    ) -> bool {
        let pipeline = match self.convert_pipeline_depth_target_ex(
            self.convert_abgr8_to_d32f_pipeline,
            dst_framebuffer.render_pass,
            self.convert_abgr8_to_d32f_frag,
        ) {
            Ok(pipeline) => pipeline,
            Err(err) => {
                log::warn!("BlitImageHelper: failed to create ABGR8->D32F pipeline: {err:?}");
                return false;
            }
        };
        self.convert_abgr8_to_d32f_pipeline = pipeline;
        self.convert(pipeline, dst_framebuffer, src_image_view)
    }

    /// Port of `BlitImageHelper::ConvertD32FToABGR8`.
    pub fn convert_d32f_to_abgr8(
        &mut self,
        dst_framebuffer: BlitFramebufferInfo,
        src_image_view: BlitImageView,
    ) -> bool {
        let pipeline = match self.convert_pipeline_color_target_ex(
            self.convert_d32f_to_abgr8_pipeline,
            dst_framebuffer.render_pass,
            self.convert_d32f_to_abgr8_frag,
        ) {
            Ok(pipeline) => pipeline,
            Err(err) => {
                log::warn!("BlitImageHelper: failed to create D32F->ABGR8 pipeline: {err:?}");
                return false;
            }
        };
        self.convert_d32f_to_abgr8_pipeline = pipeline;
        self.convert_depth_stencil(pipeline, dst_framebuffer, src_image_view)
    }

    /// Port of `BlitImageHelper::ConvertD24S8ToABGR8`.
    pub fn convert_d24s8_to_abgr8(
        &mut self,
        dst_framebuffer: BlitFramebufferInfo,
        src_image_view: BlitImageView,
    ) -> bool {
        let pipeline = match self.convert_pipeline_color_target_ex(
            self.convert_d24s8_to_abgr8_pipeline,
            dst_framebuffer.render_pass,
            self.convert_d24s8_to_abgr8_frag,
        ) {
            Ok(pipeline) => pipeline,
            Err(err) => {
                log::warn!("BlitImageHelper: failed to create D24S8->ABGR8 pipeline: {err:?}");
                return false;
            }
        };
        self.convert_d24s8_to_abgr8_pipeline = pipeline;
        self.convert_depth_stencil(pipeline, dst_framebuffer, src_image_view)
    }

    /// Port of `BlitImageHelper::ConvertS8D24ToABGR8`.
    pub fn convert_s8d24_to_abgr8(
        &mut self,
        dst_framebuffer: BlitFramebufferInfo,
        src_image_view: BlitImageView,
    ) -> bool {
        let pipeline = match self.convert_pipeline_color_target_ex(
            self.convert_s8d24_to_abgr8_pipeline,
            dst_framebuffer.render_pass,
            self.convert_s8d24_to_abgr8_frag,
        ) {
            Ok(pipeline) => pipeline,
            Err(err) => {
                log::warn!("BlitImageHelper: failed to create S8D24->ABGR8 pipeline: {err:?}");
                return false;
            }
        };
        self.convert_s8d24_to_abgr8_pipeline = pipeline;
        self.convert_depth_stencil(pipeline, dst_framebuffer, src_image_view)
    }

    /// Port of `BlitImageHelper::ClearColor`.
    ///
    /// Clears a region of the color attachment using a fragment shader that
    /// respects the color write mask.
    pub fn clear_color(
        &mut self,
        dst_framebuffer: BlitFramebufferInfo,
        color_mask: u8,
        clear_color: [f32; 4],
        dst_region: &Region2D,
    ) -> bool {
        let key = BlitImagePipelineKey {
            renderpass: dst_framebuffer.render_pass,
            operation: Operation::BlendPremult,
        };
        let pipeline = match self.find_or_emplace_clear_color_pipeline(&key) {
            Ok(pipeline) => pipeline,
            Err(err) => {
                log::warn!("BlitImageHelper: failed to create color clear pipeline: {err:?}");
                return false;
            }
        };
        let layout = self.clear_color_pipeline_layout;
        let render_area = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: dst_framebuffer.render_area,
        };
        let device = self.device.clone();
        let dst_region = *dst_region;
        let scheduler = unsafe { self.scheduler.as_mut() };
        scheduler.request_renderpass_raw(
            dst_framebuffer.framebuffer,
            dst_framebuffer.render_pass,
            render_area,
            &[],
            &dst_framebuffer.images[..dst_framebuffer.num_images],
            &dst_framebuffer.image_ranges[..dst_framebuffer.num_images],
        );
        scheduler.record(move |cmdbuf| unsafe {
            device.cmd_bind_pipeline(cmdbuf, vk::PipelineBindPoint::GRAPHICS, pipeline);
            let blend_color = [
                if color_mask & 0x1 != 0 { 1.0 } else { 0.0 },
                if color_mask & 0x2 != 0 { 1.0 } else { 0.0 },
                if color_mask & 0x4 != 0 { 1.0 } else { 0.0 },
                if color_mask & 0x8 != 0 { 1.0 } else { 0.0 },
            ];
            device.cmd_set_blend_constants(cmdbuf, &blend_color);
            bind_clear_state(&device, cmdbuf, dst_region);
            let clear_bytes = std::slice::from_raw_parts(
                clear_color.as_ptr().cast::<u8>(),
                std::mem::size_of::<[f32; 4]>(),
            );
            device.cmd_push_constants(
                cmdbuf,
                layout,
                vk::ShaderStageFlags::FRAGMENT,
                0,
                clear_bytes,
            );
            device.cmd_draw(cmdbuf, 3, 1, 0, 0);
        });
        scheduler.invalidate_state();
        true
    }

    /// Port of `BlitImageHelper::ClearDepthStencil`.
    ///
    /// Clears depth and/or stencil attachments using a specialized fragment
    /// shader.
    pub fn clear_depth_stencil(
        &mut self,
        dst_framebuffer: BlitFramebufferInfo,
        depth_clear: bool,
        clear_depth: f32,
        stencil_mask: u8,
        stencil_ref: u32,
        stencil_compare_mask: u32,
        dst_region: &Region2D,
    ) -> bool {
        let key = BlitDepthStencilPipelineKey {
            renderpass: dst_framebuffer.render_pass,
            depth_clear,
            stencil_mask,
            stencil_compare_mask,
            stencil_ref,
        };
        let pipeline = match self.find_or_emplace_clear_stencil_pipeline(&key) {
            Ok(pipeline) => pipeline,
            Err(err) => {
                log::warn!(
                    "BlitImageHelper: failed to create depth/stencil clear pipeline: {err:?}"
                );
                return false;
            }
        };
        let layout = self.clear_color_pipeline_layout;
        let render_area = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: dst_framebuffer.render_area,
        };
        let device = self.device.clone();
        let dst_region = *dst_region;
        let scheduler = unsafe { self.scheduler.as_mut() };
        scheduler.request_renderpass_raw(
            dst_framebuffer.framebuffer,
            dst_framebuffer.render_pass,
            render_area,
            &[],
            &dst_framebuffer.images[..dst_framebuffer.num_images],
            &dst_framebuffer.image_ranges[..dst_framebuffer.num_images],
        );
        scheduler.record(move |cmdbuf| unsafe {
            const BLEND_CONSTANTS: [f32; 4] = [0.0, 0.0, 0.0, 0.0];
            device.cmd_set_blend_constants(cmdbuf, &BLEND_CONSTANTS);
            device.cmd_bind_pipeline(cmdbuf, vk::PipelineBindPoint::GRAPHICS, pipeline);
            bind_clear_state(&device, cmdbuf, dst_region);
            let clear_bytes = std::slice::from_raw_parts(
                (&clear_depth as *const f32).cast::<u8>(),
                std::mem::size_of::<f32>(),
            );
            device.cmd_push_constants(
                cmdbuf,
                layout,
                vk::ShaderStageFlags::FRAGMENT,
                0,
                clear_bytes,
            );
            device.cmd_draw(cmdbuf, 3, 1, 0, 0);
        });
        scheduler.invalidate_state();
        true
    }

    // --- Private helpers ---

    fn convert(
        &mut self,
        pipeline: vk::Pipeline,
        dst_framebuffer: BlitFramebufferInfo,
        src_image_view: BlitImageView,
    ) -> bool {
        let layout = self.one_texture_pipeline_layout;
        let sampler = self.nearest_sampler;
        let src_view = src_image_view.color_view;
        let extent = conversion_extent(src_image_view);
        let descriptor_allocator = self.one_texture_descriptor_allocator.clone();
        let render_area = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: dst_framebuffer.render_area,
        };
        let device = self.device.clone();
        let scheduler = unsafe { self.scheduler.as_mut() };
        record_shader_read_barrier(&self.device, scheduler, src_image_view);
        scheduler.request_renderpass_raw(
            dst_framebuffer.framebuffer,
            dst_framebuffer.render_pass,
            render_area,
            &[],
            &dst_framebuffer.images[..dst_framebuffer.num_images],
            &dst_framebuffer.image_ranges[..dst_framebuffer.num_images],
        );
        scheduler.record(move |cmdbuf| unsafe {
            let viewport = vk::Viewport {
                x: 0.0,
                y: 0.0,
                width: extent.width as f32,
                height: extent.height as f32,
                min_depth: 0.0,
                max_depth: 0.0,
            };
            let scissor = vk::Rect2D {
                offset: vk::Offset2D { x: 0, y: 0 },
                extent,
            };
            let push_constants = PushConstants {
                tex_scale: [viewport.width, viewport.height],
                tex_offset: [0.0, 0.0],
            };
            let push_bytes = std::slice::from_raw_parts(
                (&push_constants as *const PushConstants).cast::<u8>(),
                std::mem::size_of::<PushConstants>(),
            );
            let descriptor_set = descriptor_allocator
                .commit()
                .expect("Failed to allocate convert descriptor set");
            update_one_texture_descriptor_set(&device, descriptor_set, sampler, src_view);
            device.cmd_bind_pipeline(cmdbuf, vk::PipelineBindPoint::GRAPHICS, pipeline);
            device.cmd_bind_descriptor_sets(
                cmdbuf,
                vk::PipelineBindPoint::GRAPHICS,
                layout,
                0,
                &[descriptor_set],
                &[],
            );
            device.cmd_set_viewport(cmdbuf, 0, &[viewport]);
            device.cmd_set_scissor(cmdbuf, 0, &[scissor]);
            device.cmd_push_constants(cmdbuf, layout, vk::ShaderStageFlags::VERTEX, 0, push_bytes);
            device.cmd_draw(cmdbuf, 3, 1, 0, 0);
        });
        scheduler.invalidate_state();
        true
    }

    fn convert_depth_stencil(
        &mut self,
        pipeline: vk::Pipeline,
        dst_framebuffer: BlitFramebufferInfo,
        src_image_view: BlitImageView,
    ) -> bool {
        let layout = self.two_textures_pipeline_layout;
        let sampler = self.nearest_sampler;
        let extent = conversion_extent(src_image_view);
        let descriptor_allocator = self.two_textures_descriptor_allocator.clone();
        let src_depth_view = src_image_view.depth_view;
        let src_stencil_view = src_image_view.stencil_view;
        let render_area = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: dst_framebuffer.render_area,
        };
        let device = self.device.clone();
        let scheduler = unsafe { self.scheduler.as_mut() };
        record_shader_read_barrier(&self.device, scheduler, src_image_view);
        scheduler.request_renderpass_raw(
            dst_framebuffer.framebuffer,
            dst_framebuffer.render_pass,
            render_area,
            &[],
            &dst_framebuffer.images[..dst_framebuffer.num_images],
            &dst_framebuffer.image_ranges[..dst_framebuffer.num_images],
        );
        scheduler.record(move |cmdbuf| unsafe {
            let viewport = vk::Viewport {
                x: 0.0,
                y: 0.0,
                width: extent.width as f32,
                height: extent.height as f32,
                min_depth: 0.0,
                max_depth: 0.0,
            };
            let scissor = vk::Rect2D {
                offset: vk::Offset2D { x: 0, y: 0 },
                extent,
            };
            let push_constants = PushConstants {
                tex_scale: [viewport.width, viewport.height],
                tex_offset: [0.0, 0.0],
            };
            let push_bytes = std::slice::from_raw_parts(
                (&push_constants as *const PushConstants).cast::<u8>(),
                std::mem::size_of::<PushConstants>(),
            );
            let descriptor_set = descriptor_allocator
                .commit()
                .expect("Failed to allocate depth/stencil convert descriptor set");
            update_two_textures_descriptor_set(
                &device,
                descriptor_set,
                sampler,
                src_depth_view,
                src_stencil_view,
            );
            device.cmd_bind_pipeline(cmdbuf, vk::PipelineBindPoint::GRAPHICS, pipeline);
            device.cmd_bind_descriptor_sets(
                cmdbuf,
                vk::PipelineBindPoint::GRAPHICS,
                layout,
                0,
                &[descriptor_set],
                &[],
            );
            device.cmd_set_viewport(cmdbuf, 0, &[viewport]);
            device.cmd_set_scissor(cmdbuf, 0, &[scissor]);
            device.cmd_push_constants(cmdbuf, layout, vk::ShaderStageFlags::VERTEX, 0, push_bytes);
            device.cmd_draw(cmdbuf, 3, 1, 0, 0);
        });
        scheduler.invalidate_state();
        true
    }

    /// Port of `BlitImageHelper::CopyMSAA`.
    pub fn copy_msaa(
        &mut self,
        render_pass_cache: &RenderPassCache,
        dst_image: vk::Image,
        dst_format: PixelFormat,
        src_image: vk::Image,
        src_format: PixelFormat,
        num_samples: u32,
        copies: &[ImageCopy],
        msaa_to_non_msaa: bool,
    ) -> bool {
        while self
            .msaa_copy_resources
            .front()
            .is_some_and(|resource| unsafe { self.scheduler.as_ref() }.is_free(resource.tick))
        {
            let resource = self.msaa_copy_resources.pop_front().unwrap();
            unsafe {
                self.device.destroy_framebuffer(resource.framebuffer, None);
                self.device.destroy_image_view(resource.dst_view, None);
                self.device.destroy_image_view(resource.src_view, None);
            }
        }

        let (samples_x, samples_y) = samples_log2(num_samples as i32);
        let scale_x = 1_i32 << samples_x;
        let scale_y = 1_i32 << samples_y;
        let samples = if msaa_to_non_msaa {
            vk::SampleCountFlags::TYPE_1
        } else {
            sample_count_flag(num_samples)
        };
        let mut renderpass_key = RenderPassKey::default();
        renderpass_key.color_formats[0] = dst_format;
        renderpass_key.samples = samples;
        let renderpass = match render_pass_cache.get(&renderpass_key) {
            Ok(renderpass) => renderpass,
            Err(err) => {
                log::warn!("BlitImageHelper::CopyMSAA render pass creation failed: {err:?}");
                return false;
            }
        };
        let key = MsaaCopyPipelineKey {
            renderpass,
            samples,
            msaa_to_non_msaa,
        };
        let pipeline = match self.find_or_emplace_msaa_copy_pipeline(&key) {
            Ok(pipeline) => pipeline,
            Err(err) => {
                log::warn!("BlitImageHelper::CopyMSAA pipeline creation failed: {err:?}");
                return false;
            }
        };
        // SAFETY: the boxed `Device` owner outlives the rasterizer and this
        // helper, matching upstream's `const Device&` member.
        let vulkan_device = unsafe { self.device_owner.as_ref() };
        let src_vk_format = super::maxwell_to_vk::surface_format(
            vulkan_device,
            FormatType::Optimal,
            true,
            src_format,
        )
        .format;
        let dst_vk_format = super::maxwell_to_vk::surface_format(
            vulkan_device,
            FormatType::Optimal,
            true,
            dst_format,
        )
        .format;

        for copy in copies {
            assert_fail_soft(
                copy.src_subresource.base_layer == 0,
                "MSAA copy source base layer must be zero",
            );
            assert_fail_soft(
                copy.src_subresource.num_layers == 1,
                "MSAA copy source must have one layer",
            );
            assert_fail_soft(
                copy.dst_subresource.base_layer == 0,
                "MSAA copy destination base layer must be zero",
            );
            assert_fail_soft(
                copy.dst_subresource.num_layers == 1,
                "MSAA copy destination must have one layer",
            );

            let src_view = match make_msaa_copy_view(
                &self.device,
                src_image,
                src_vk_format,
                copy.src_subresource.base_level as u32,
            ) {
                Ok(view) => view,
                Err(err) => {
                    log::warn!("BlitImageHelper::CopyMSAA source view failed: {err:?}");
                    return false;
                }
            };
            let dst_view = match make_msaa_copy_view(
                &self.device,
                dst_image,
                dst_vk_format,
                copy.dst_subresource.base_level as u32,
            ) {
                Ok(view) => view,
                Err(err) => {
                    unsafe { self.device.destroy_image_view(src_view, None) };
                    log::warn!("BlitImageHelper::CopyMSAA destination view failed: {err:?}");
                    return false;
                }
            };
            let render_area = vk::Rect2D {
                offset: vk::Offset2D {
                    x: copy.dst_offset.x,
                    y: copy.dst_offset.y,
                },
                extent: vk::Extent2D {
                    width: copy.extent.width,
                    height: copy.extent.height,
                },
            };
            let attachments = [dst_view];
            let framebuffer_info = vk::FramebufferCreateInfo::builder()
                .render_pass(renderpass)
                .attachments(&attachments)
                .width((copy.dst_offset.x as u32).wrapping_add(copy.extent.width))
                .height((copy.dst_offset.y as u32).wrapping_add(copy.extent.height))
                .layers(1)
                .build();
            let framebuffer = match unsafe {
                self.device.create_framebuffer(&framebuffer_info, None)
            } {
                Ok(framebuffer) => framebuffer,
                Err(err) => {
                    unsafe {
                        self.device.destroy_image_view(src_view, None);
                        self.device.destroy_image_view(dst_view, None);
                    }
                    log::warn!("BlitImageHelper::CopyMSAA framebuffer creation failed: {err:?}");
                    return false;
                }
            };
            let push_constants = MsaaCopyPushConstants {
                dst_offset: [copy.dst_offset.x, copy.dst_offset.y],
                src_offset: [copy.src_offset.x, copy.src_offset.y],
                scale: [scale_x, scale_y],
            };
            let device = self.device.clone();
            let layout = self.msaa_copy_pipeline_layout;
            let sampler = self.nearest_sampler;
            let descriptor_allocator = self.one_texture_descriptor_allocator.clone();
            unsafe { self.scheduler.as_mut() }.request_outside_render_pass_operation_context();
            unsafe { self.scheduler.as_mut() }.record(move |cmdbuf| unsafe {
                let color_range = vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: vk::REMAINING_MIP_LEVELS,
                    base_array_layer: 0,
                    layer_count: vk::REMAINING_ARRAY_LAYERS,
                };
                let pre_barriers = [
                    vk::ImageMemoryBarrier::builder()
                        .src_access_mask(
                            vk::AccessFlags::COLOR_ATTACHMENT_WRITE
                                | vk::AccessFlags::SHADER_WRITE
                                | vk::AccessFlags::TRANSFER_WRITE,
                        )
                        .dst_access_mask(vk::AccessFlags::SHADER_READ)
                        .old_layout(vk::ImageLayout::GENERAL)
                        .new_layout(vk::ImageLayout::GENERAL)
                        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .image(src_image)
                        .subresource_range(color_range)
                        .build(),
                    vk::ImageMemoryBarrier::builder()
                        .src_access_mask(
                            vk::AccessFlags::COLOR_ATTACHMENT_WRITE
                                | vk::AccessFlags::SHADER_WRITE
                                | vk::AccessFlags::TRANSFER_WRITE,
                        )
                        .dst_access_mask(
                            vk::AccessFlags::COLOR_ATTACHMENT_READ
                                | vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                        )
                        .old_layout(vk::ImageLayout::GENERAL)
                        .new_layout(vk::ImageLayout::GENERAL)
                        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .image(dst_image)
                        .subresource_range(color_range)
                        .build(),
                ];
                device.cmd_pipeline_barrier(
                    cmdbuf,
                    vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
                        | vk::PipelineStageFlags::COMPUTE_SHADER
                        | vk::PipelineStageFlags::FRAGMENT_SHADER
                        | vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::FRAGMENT_SHADER
                        | vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &pre_barriers,
                );
                let begin_info = vk::RenderPassBeginInfo::builder()
                    .render_pass(renderpass)
                    .framebuffer(framebuffer)
                    .render_area(render_area)
                    .build();
                device.cmd_begin_render_pass(cmdbuf, &begin_info, vk::SubpassContents::INLINE);
                let descriptor_set = descriptor_allocator
                    .commit()
                    .expect("Failed to allocate MSAA copy descriptor set");
                update_one_texture_descriptor_set(&device, descriptor_set, sampler, src_view);
                device.cmd_bind_pipeline(cmdbuf, vk::PipelineBindPoint::GRAPHICS, pipeline);
                device.cmd_bind_descriptor_sets(
                    cmdbuf,
                    vk::PipelineBindPoint::GRAPHICS,
                    layout,
                    0,
                    &[descriptor_set],
                    &[],
                );
                let viewport = vk::Viewport {
                    x: render_area.offset.x as f32,
                    y: render_area.offset.y as f32,
                    width: render_area.extent.width as f32,
                    height: render_area.extent.height as f32,
                    min_depth: 0.0,
                    max_depth: 1.0,
                };
                device.cmd_set_viewport(cmdbuf, 0, &[viewport]);
                device.cmd_set_scissor(cmdbuf, 0, &[render_area]);
                let push_bytes = std::slice::from_raw_parts(
                    (&push_constants as *const MsaaCopyPushConstants).cast::<u8>(),
                    std::mem::size_of::<MsaaCopyPushConstants>(),
                );
                device.cmd_push_constants(
                    cmdbuf,
                    layout,
                    vk::ShaderStageFlags::FRAGMENT,
                    0,
                    push_bytes,
                );
                device.cmd_draw(cmdbuf, 3, 1, 0, 0);
                device.cmd_end_render_pass(cmdbuf);
                let post_barrier = vk::ImageMemoryBarrier::builder()
                    .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
                    .dst_access_mask(vk::AccessFlags::SHADER_READ | vk::AccessFlags::TRANSFER_READ)
                    .old_layout(vk::ImageLayout::GENERAL)
                    .new_layout(vk::ImageLayout::GENERAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(dst_image)
                    .subresource_range(color_range)
                    .build();
                device.cmd_pipeline_barrier(
                    cmdbuf,
                    vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                    vk::PipelineStageFlags::FRAGMENT_SHADER
                        | vk::PipelineStageFlags::COMPUTE_SHADER
                        | vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[post_barrier],
                );
            });
            self.msaa_copy_resources.push_back(MsaaCopyResources {
                tick: unsafe { self.scheduler.as_ref() }.current_tick(),
                src_view,
                dst_view,
                framebuffer,
            });
        }
        unsafe { self.scheduler.as_mut() }.invalidate_state();
        true
    }

    /// Port of `BlitImageHelper::FindOrEmplaceBlitColorMSAAPipeline`.
    fn find_or_emplace_blit_color_msaa_pipeline(
        &mut self,
        key: &BlitMsaaPipelineKey,
    ) -> Result<vk::Pipeline, vk::Result> {
        if let Some(index) = self
            .blit_msaa_color_keys
            .iter()
            .position(|cached| cached == key)
        {
            return Ok(self.blit_msaa_color_pipelines[index]);
        }
        let main = CString::new("main").unwrap();
        let stages = [
            vk::PipelineShaderStageCreateInfo::builder()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(self.full_screen_vert)
                .name(&main)
                .build(),
            vk::PipelineShaderStageCreateInfo::builder()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(self.blit_color_msaa_frag)
                .name(&main)
                .build(),
        ];
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::builder().build();
        let input_assembly = pipeline_input_assembly_state(unsafe { self.device_owner.as_ref() });
        let viewport_state = vk::PipelineViewportStateCreateInfo::builder()
            .viewport_count(1)
            .scissor_count(1)
            .build();
        let rasterization = vk::PipelineRasterizationStateCreateInfo::builder()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::BACK)
            .front_face(vk::FrontFace::CLOCKWISE)
            .line_width(1.0)
            .build();
        let multisample = vk::PipelineMultisampleStateCreateInfo::builder()
            .rasterization_samples(key.samples)
            .sample_shading_enable(true)
            .min_sample_shading(1.0)
            .build();
        let blend_attachment = vk::PipelineColorBlendAttachmentState::builder()
            .color_write_mask(
                vk::ColorComponentFlags::R
                    | vk::ColorComponentFlags::G
                    | vk::ColorComponentFlags::B
                    | vk::ColorComponentFlags::A,
            )
            .build();
        let color_blend = vk::PipelineColorBlendStateCreateInfo::builder()
            .attachments(std::slice::from_ref(&blend_attachment))
            .build();
        let dynamic_states = [
            vk::DynamicState::VIEWPORT,
            vk::DynamicState::SCISSOR,
            vk::DynamicState::BLEND_CONSTANTS,
        ];
        let dynamic_state = vk::PipelineDynamicStateCreateInfo::builder()
            .dynamic_states(&dynamic_states)
            .build();
        let create_info = vk::GraphicsPipelineCreateInfo::builder()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport_state)
            .rasterization_state(&rasterization)
            .multisample_state(&multisample)
            .color_blend_state(&color_blend)
            .dynamic_state(&dynamic_state)
            .layout(self.one_texture_pipeline_layout)
            .render_pass(key.renderpass)
            .subpass(0)
            .build();
        let pipeline = unsafe {
            self.device
                .create_graphics_pipelines(vk::PipelineCache::null(), &[create_info], None)
                .map_err(|(_, err)| err)?[0]
        };
        self.blit_msaa_color_keys.push(*key);
        self.blit_msaa_color_pipelines.push(pipeline);
        Ok(pipeline)
    }

    /// Port of `BlitImageHelper::FindOrEmplaceResolveDepthStencilPipeline`.
    fn find_or_emplace_resolve_depth_stencil_pipeline(
        &mut self,
        renderpass: vk::RenderPass,
        resolve_stencil: bool,
    ) -> Result<vk::Pipeline, vk::Result> {
        let (keys, pipelines) = if resolve_stencil {
            (
                &mut self.resolve_depth_stencil_keys,
                &mut self.resolve_depth_stencil_pipelines,
            )
        } else {
            (
                &mut self.resolve_depth_keys,
                &mut self.resolve_depth_pipelines,
            )
        };
        if let Some(index) = keys.iter().position(|&cached| cached == renderpass) {
            return Ok(pipelines[index]);
        }
        let main = CString::new("main").unwrap();
        let fragment_shader = if resolve_stencil {
            self.blit_depth_stencil_msaa_frag
        } else {
            self.blit_depth_msaa_frag
        };
        let stages = [
            vk::PipelineShaderStageCreateInfo::builder()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(self.full_screen_vert)
                .name(&main)
                .build(),
            vk::PipelineShaderStageCreateInfo::builder()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(fragment_shader)
                .name(&main)
                .build(),
        ];
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::builder().build();
        let input_assembly = pipeline_input_assembly_state(unsafe { self.device_owner.as_ref() });
        let viewport_state = vk::PipelineViewportStateCreateInfo::builder()
            .viewport_count(1)
            .scissor_count(1)
            .build();
        let rasterization = vk::PipelineRasterizationStateCreateInfo::builder()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::BACK)
            .front_face(vk::FrontFace::CLOCKWISE)
            .line_width(1.0)
            .build();
        let multisample = vk::PipelineMultisampleStateCreateInfo::builder()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1)
            .build();
        let depth_stencil = if resolve_stencil {
            pipeline_depth_stencil_state()
        } else {
            pipeline_depth_only_state()
        };
        let color_blend = vk::PipelineColorBlendStateCreateInfo::builder().build();
        let dynamic_states = [
            vk::DynamicState::VIEWPORT,
            vk::DynamicState::SCISSOR,
            vk::DynamicState::BLEND_CONSTANTS,
        ];
        let dynamic_state = vk::PipelineDynamicStateCreateInfo::builder()
            .dynamic_states(&dynamic_states)
            .build();
        let layout = if resolve_stencil {
            self.two_textures_pipeline_layout
        } else {
            self.one_texture_pipeline_layout
        };
        let create_info = vk::GraphicsPipelineCreateInfo::builder()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport_state)
            .rasterization_state(&rasterization)
            .multisample_state(&multisample)
            .depth_stencil_state(&depth_stencil)
            .color_blend_state(&color_blend)
            .dynamic_state(&dynamic_state)
            .layout(layout)
            .render_pass(renderpass)
            .subpass(0)
            .build();
        let pipeline = unsafe {
            self.device
                .create_graphics_pipelines(vk::PipelineCache::null(), &[create_info], None)
                .map_err(|(_, err)| err)?[0]
        };
        keys.push(renderpass);
        pipelines.push(pipeline);
        Ok(pipeline)
    }

    fn find_or_emplace_msaa_copy_pipeline(
        &mut self,
        key: &MsaaCopyPipelineKey,
    ) -> Result<vk::Pipeline, vk::Result> {
        if let Some(index) = self.msaa_copy_keys.iter().position(|cached| cached == key) {
            return Ok(self.msaa_copy_pipelines[index]);
        }
        let main = CString::new("main").unwrap();
        let fragment_shader = if key.msaa_to_non_msaa {
            self.convert_msaa_to_non_msaa_frag
        } else {
            self.convert_non_msaa_to_msaa_frag
        };
        let stages = [
            vk::PipelineShaderStageCreateInfo::builder()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(self.clear_color_vert)
                .name(&main)
                .build(),
            vk::PipelineShaderStageCreateInfo::builder()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(fragment_shader)
                .name(&main)
                .build(),
        ];
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::builder().build();
        let input_assembly = pipeline_input_assembly_state(unsafe { self.device_owner.as_ref() });
        let viewport_state = vk::PipelineViewportStateCreateInfo::builder()
            .viewport_count(1)
            .scissor_count(1)
            .build();
        let rasterization = vk::PipelineRasterizationStateCreateInfo::builder()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::BACK)
            .front_face(vk::FrontFace::CLOCKWISE)
            .line_width(1.0)
            .build();
        let multisample = vk::PipelineMultisampleStateCreateInfo::builder()
            .rasterization_samples(key.samples)
            .sample_shading_enable(!key.msaa_to_non_msaa)
            .min_sample_shading(if key.msaa_to_non_msaa { 0.0 } else { 1.0 })
            .build();
        let blend_attachment = vk::PipelineColorBlendAttachmentState::builder()
            .color_write_mask(
                vk::ColorComponentFlags::R
                    | vk::ColorComponentFlags::G
                    | vk::ColorComponentFlags::B
                    | vk::ColorComponentFlags::A,
            )
            .build();
        let color_blend = vk::PipelineColorBlendStateCreateInfo::builder()
            .attachments(std::slice::from_ref(&blend_attachment))
            .build();
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic_state = vk::PipelineDynamicStateCreateInfo::builder()
            .dynamic_states(&dynamic_states)
            .build();
        let create_info = vk::GraphicsPipelineCreateInfo::builder()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport_state)
            .rasterization_state(&rasterization)
            .multisample_state(&multisample)
            .color_blend_state(&color_blend)
            .dynamic_state(&dynamic_state)
            .layout(self.msaa_copy_pipeline_layout)
            .render_pass(key.renderpass)
            .subpass(0)
            .build();
        let pipeline = unsafe {
            self.device
                .create_graphics_pipelines(vk::PipelineCache::null(), &[create_info], None)
                .map_err(|(_, err)| err)?[0]
        };
        self.msaa_copy_keys.push(*key);
        self.msaa_copy_pipelines.push(pipeline);
        Ok(pipeline)
    }

    /// Port of `BlitImageHelper::FindOrEmplaceColorPipeline`.
    ///
    /// Looks up or creates a graphics pipeline for color blitting with
    /// the given render pass and blend operation.
    fn find_or_emplace_color_pipeline(
        &mut self,
        key: &BlitImagePipelineKey,
    ) -> Result<vk::Pipeline, vk::Result> {
        if let Some(idx) = self.blit_color_keys.iter().position(|k| k == key) {
            return Ok(self.blit_color_pipelines[idx]);
        }
        let main = CString::new("main").unwrap();
        let stages = [
            vk::PipelineShaderStageCreateInfo::builder()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(self.full_screen_vert)
                .name(&main)
                .build(),
            vk::PipelineShaderStageCreateInfo::builder()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(self.blit_color_to_color_frag)
                .name(&main)
                .build(),
        ];
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::builder().build();
        let input_assembly = pipeline_input_assembly_state(unsafe { self.device_owner.as_ref() });
        let viewport_state = vk::PipelineViewportStateCreateInfo::builder()
            .viewport_count(1)
            .scissor_count(1)
            .build();
        let rasterization = vk::PipelineRasterizationStateCreateInfo::builder()
            .depth_clamp_enable(false)
            .rasterizer_discard_enable(false)
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::BACK)
            .front_face(vk::FrontFace::CLOCKWISE)
            .depth_bias_enable(false)
            .line_width(1.0)
            .build();
        let multisample = vk::PipelineMultisampleStateCreateInfo::builder()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1)
            .sample_shading_enable(false)
            .build();
        let blend_attachment = vk::PipelineColorBlendAttachmentState::builder()
            .blend_enable(false)
            .src_color_blend_factor(vk::BlendFactor::ZERO)
            .dst_color_blend_factor(vk::BlendFactor::ZERO)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ZERO)
            .dst_alpha_blend_factor(vk::BlendFactor::ZERO)
            .alpha_blend_op(vk::BlendOp::ADD)
            .color_write_mask(
                vk::ColorComponentFlags::R
                    | vk::ColorComponentFlags::G
                    | vk::ColorComponentFlags::B
                    | vk::ColorComponentFlags::A,
            )
            .build();
        let color_blend = vk::PipelineColorBlendStateCreateInfo::builder()
            .logic_op_enable(false)
            .logic_op(vk::LogicOp::CLEAR)
            .attachments(std::slice::from_ref(&blend_attachment))
            .build();
        let dynamic_states = [
            vk::DynamicState::VIEWPORT,
            vk::DynamicState::SCISSOR,
            vk::DynamicState::BLEND_CONSTANTS,
        ];
        let dynamic_state = vk::PipelineDynamicStateCreateInfo::builder()
            .dynamic_states(&dynamic_states)
            .build();
        let pipeline_info = vk::GraphicsPipelineCreateInfo::builder()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport_state)
            .rasterization_state(&rasterization)
            .multisample_state(&multisample)
            .color_blend_state(&color_blend)
            .dynamic_state(&dynamic_state)
            .layout(self.one_texture_pipeline_layout)
            .render_pass(key.renderpass)
            .subpass(0)
            .build();
        let pipeline = unsafe {
            self.device
                .create_graphics_pipelines(vk::PipelineCache::null(), &[pipeline_info], None)
                .map_err(|(_, err)| err)?[0]
        };
        self.blit_color_keys.push(*key);
        self.blit_color_pipelines.push(pipeline);
        Ok(pipeline)
    }

    /// Port of `BlitImageHelper::FindOrEmplaceDepthStencilPipeline`.
    fn find_or_emplace_depth_stencil_pipeline(
        &mut self,
        key: &BlitImagePipelineKey,
    ) -> Result<vk::Pipeline, vk::Result> {
        if let Some(idx) = self.blit_depth_stencil_keys.iter().position(|k| k == key) {
            return Ok(self.blit_depth_stencil_pipelines[idx]);
        }
        let main = CString::new("main").unwrap();
        let stages = [
            vk::PipelineShaderStageCreateInfo::builder()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(self.full_screen_vert)
                .name(&main)
                .build(),
            vk::PipelineShaderStageCreateInfo::builder()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(self.blit_depth_stencil_frag)
                .name(&main)
                .build(),
        ];
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::builder().build();
        let input_assembly = pipeline_input_assembly_state(unsafe { self.device_owner.as_ref() });
        let viewport_state = vk::PipelineViewportStateCreateInfo::builder()
            .viewport_count(1)
            .scissor_count(1)
            .build();
        let rasterization = vk::PipelineRasterizationStateCreateInfo::builder()
            .depth_clamp_enable(false)
            .rasterizer_discard_enable(false)
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::BACK)
            .front_face(vk::FrontFace::CLOCKWISE)
            .depth_bias_enable(false)
            .line_width(1.0)
            .build();
        let multisample = vk::PipelineMultisampleStateCreateInfo::builder()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1)
            .sample_shading_enable(false)
            .build();
        let depth_stencil = pipeline_depth_stencil_state();
        let color_blend = vk::PipelineColorBlendStateCreateInfo::builder().build();
        let dynamic_states = [
            vk::DynamicState::VIEWPORT,
            vk::DynamicState::SCISSOR,
            vk::DynamicState::BLEND_CONSTANTS,
        ];
        let dynamic_state = vk::PipelineDynamicStateCreateInfo::builder()
            .dynamic_states(&dynamic_states)
            .build();
        let pipeline_info = vk::GraphicsPipelineCreateInfo::builder()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport_state)
            .rasterization_state(&rasterization)
            .multisample_state(&multisample)
            .depth_stencil_state(&depth_stencil)
            .color_blend_state(&color_blend)
            .dynamic_state(&dynamic_state)
            .layout(self.two_textures_pipeline_layout)
            .render_pass(key.renderpass)
            .subpass(0)
            .build();
        let pipeline = unsafe {
            self.device
                .create_graphics_pipelines(vk::PipelineCache::null(), &[pipeline_info], None)
                .map_err(|(_, err)| err)?[0]
        };
        self.blit_depth_stencil_keys.push(*key);
        self.blit_depth_stencil_pipelines.push(pipeline);
        Ok(pipeline)
    }

    /// Port of `BlitImageHelper::FindOrEmplaceClearColorPipeline`.
    fn find_or_emplace_clear_color_pipeline(
        &mut self,
        key: &BlitImagePipelineKey,
    ) -> Result<vk::Pipeline, vk::Result> {
        if let Some(idx) = self.clear_color_keys.iter().position(|k| k == key) {
            return Ok(self.clear_color_pipelines[idx]);
        }
        let main = CString::new("main").unwrap();
        let stages = [
            vk::PipelineShaderStageCreateInfo::builder()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(self.clear_color_vert)
                .name(&main)
                .build(),
            vk::PipelineShaderStageCreateInfo::builder()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(self.clear_color_frag)
                .name(&main)
                .build(),
        ];
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::builder().build();
        let input_assembly = pipeline_input_assembly_state(unsafe { self.device_owner.as_ref() });
        let viewport_state = vk::PipelineViewportStateCreateInfo::builder()
            .viewport_count(1)
            .scissor_count(1)
            .build();
        let rasterization = vk::PipelineRasterizationStateCreateInfo::builder()
            .depth_clamp_enable(false)
            .rasterizer_discard_enable(false)
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::BACK)
            .front_face(vk::FrontFace::CLOCKWISE)
            .depth_bias_enable(false)
            .line_width(1.0)
            .build();
        let multisample = vk::PipelineMultisampleStateCreateInfo::builder()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1)
            .sample_shading_enable(false)
            .build();
        let depth_stencil = pipeline_depth_stencil_state();
        let blend_attachment = vk::PipelineColorBlendAttachmentState::builder()
            .blend_enable(true)
            .src_color_blend_factor(vk::BlendFactor::CONSTANT_COLOR)
            .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_CONSTANT_COLOR)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::CONSTANT_ALPHA)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_CONSTANT_ALPHA)
            .alpha_blend_op(vk::BlendOp::ADD)
            .color_write_mask(
                vk::ColorComponentFlags::R
                    | vk::ColorComponentFlags::G
                    | vk::ColorComponentFlags::B
                    | vk::ColorComponentFlags::A,
            )
            .build();
        let color_blend = vk::PipelineColorBlendStateCreateInfo::builder()
            .logic_op_enable(false)
            .logic_op(vk::LogicOp::CLEAR)
            .attachments(std::slice::from_ref(&blend_attachment))
            .build();
        let dynamic_states = [
            vk::DynamicState::VIEWPORT,
            vk::DynamicState::SCISSOR,
            vk::DynamicState::BLEND_CONSTANTS,
        ];
        let dynamic_state = vk::PipelineDynamicStateCreateInfo::builder()
            .dynamic_states(&dynamic_states)
            .build();
        let pipeline_info = vk::GraphicsPipelineCreateInfo::builder()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport_state)
            .rasterization_state(&rasterization)
            .multisample_state(&multisample)
            .depth_stencil_state(&depth_stencil)
            .color_blend_state(&color_blend)
            .dynamic_state(&dynamic_state)
            .layout(self.clear_color_pipeline_layout)
            .render_pass(key.renderpass)
            .subpass(0)
            .build();
        let pipeline = unsafe {
            self.device
                .create_graphics_pipelines(vk::PipelineCache::null(), &[pipeline_info], None)
                .map_err(|(_, err)| err)?[0]
        };
        self.clear_color_keys.push(*key);
        self.clear_color_pipelines.push(pipeline);
        Ok(pipeline)
    }

    /// Port of `BlitImageHelper::FindOrEmplaceClearStencilPipeline`.
    fn find_or_emplace_clear_stencil_pipeline(
        &mut self,
        key: &BlitDepthStencilPipelineKey,
    ) -> Result<vk::Pipeline, vk::Result> {
        if let Some(idx) = self.clear_stencil_keys.iter().position(|k| k == key) {
            return Ok(self.clear_stencil_pipelines[idx]);
        }
        let main = CString::new("main").unwrap();
        let stages = [
            vk::PipelineShaderStageCreateInfo::builder()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(self.clear_color_vert)
                .name(&main)
                .build(),
            vk::PipelineShaderStageCreateInfo::builder()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(self.clear_stencil_frag)
                .name(&main)
                .build(),
        ];
        let stencil = vk::StencilOpState {
            fail_op: vk::StencilOp::KEEP,
            pass_op: vk::StencilOp::REPLACE,
            depth_fail_op: vk::StencilOp::KEEP,
            compare_op: vk::CompareOp::ALWAYS,
            compare_mask: key.stencil_compare_mask,
            write_mask: key.stencil_mask as u32,
            reference: key.stencil_ref,
        };
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::builder().build();
        let input_assembly = pipeline_input_assembly_state(unsafe { self.device_owner.as_ref() });
        let viewport_state = vk::PipelineViewportStateCreateInfo::builder()
            .viewport_count(1)
            .scissor_count(1)
            .build();
        let rasterization = vk::PipelineRasterizationStateCreateInfo::builder()
            .depth_clamp_enable(false)
            .rasterizer_discard_enable(false)
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::BACK)
            .front_face(vk::FrontFace::CLOCKWISE)
            .depth_bias_enable(false)
            .line_width(1.0)
            .build();
        let multisample = vk::PipelineMultisampleStateCreateInfo::builder()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1)
            .sample_shading_enable(false)
            .build();
        let depth_stencil = vk::PipelineDepthStencilStateCreateInfo::builder()
            .depth_test_enable(key.depth_clear)
            .depth_write_enable(key.depth_clear)
            .depth_compare_op(vk::CompareOp::ALWAYS)
            .depth_bounds_test_enable(false)
            .stencil_test_enable(true)
            .front(stencil)
            .back(stencil)
            .build();
        let blend_attachment = vk::PipelineColorBlendAttachmentState::builder()
            .blend_enable(false)
            .src_color_blend_factor(vk::BlendFactor::ZERO)
            .dst_color_blend_factor(vk::BlendFactor::ZERO)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ZERO)
            .dst_alpha_blend_factor(vk::BlendFactor::ZERO)
            .alpha_blend_op(vk::BlendOp::ADD)
            .color_write_mask(
                vk::ColorComponentFlags::R
                    | vk::ColorComponentFlags::G
                    | vk::ColorComponentFlags::B
                    | vk::ColorComponentFlags::A,
            )
            .build();
        let color_blend = vk::PipelineColorBlendStateCreateInfo::builder()
            .logic_op_enable(false)
            .logic_op(vk::LogicOp::CLEAR)
            .attachments(std::slice::from_ref(&blend_attachment))
            .build();
        let dynamic_states = [
            vk::DynamicState::VIEWPORT,
            vk::DynamicState::SCISSOR,
            vk::DynamicState::BLEND_CONSTANTS,
        ];
        let dynamic_state = vk::PipelineDynamicStateCreateInfo::builder()
            .dynamic_states(&dynamic_states)
            .build();
        let pipeline_info = vk::GraphicsPipelineCreateInfo::builder()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport_state)
            .rasterization_state(&rasterization)
            .multisample_state(&multisample)
            .depth_stencil_state(&depth_stencil)
            .color_blend_state(&color_blend)
            .dynamic_state(&dynamic_state)
            .layout(self.clear_color_pipeline_layout)
            .render_pass(key.renderpass)
            .subpass(0)
            .build();
        let pipeline = unsafe {
            self.device
                .create_graphics_pipelines(vk::PipelineCache::null(), &[pipeline_info], None)
                .map_err(|(_, err)| err)?[0]
        };
        self.clear_stencil_keys.push(*key);
        self.clear_stencil_pipelines.push(pipeline);
        Ok(pipeline)
    }

    /// Port of `BlitImageHelper::ConvertDepthToColorPipeline`.
    fn convert_depth_to_color_pipeline(
        &self,
        pipeline: vk::Pipeline,
        renderpass: vk::RenderPass,
    ) -> Result<vk::Pipeline, vk::Result> {
        self.convert_pipeline(pipeline, renderpass, false)
    }

    /// Port of `BlitImageHelper::ConvertColorToDepthPipeline`.
    fn convert_color_to_depth_pipeline(
        &self,
        pipeline: vk::Pipeline,
        renderpass: vk::RenderPass,
    ) -> Result<vk::Pipeline, vk::Result> {
        self.convert_pipeline(pipeline, renderpass, true)
    }

    /// Port of `BlitImageHelper::ConvertPipelineEx`.
    fn convert_pipeline_ex(
        &self,
        pipeline: vk::Pipeline,
        renderpass: vk::RenderPass,
        module: vk::ShaderModule,
        single_texture: bool,
        is_target_depth: bool,
    ) -> Result<vk::Pipeline, vk::Result> {
        if pipeline != vk::Pipeline::null() {
            return Ok(pipeline);
        }
        let main = CString::new("main").unwrap();
        let stages = [
            vk::PipelineShaderStageCreateInfo::builder()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(self.full_screen_vert)
                .name(&main)
                .build(),
            vk::PipelineShaderStageCreateInfo::builder()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(module)
                .name(&main)
                .build(),
        ];
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::builder().build();
        let input_assembly = pipeline_input_assembly_state(unsafe { self.device_owner.as_ref() });
        let viewport_state = vk::PipelineViewportStateCreateInfo::builder()
            .viewport_count(1)
            .scissor_count(1)
            .build();
        let rasterization = vk::PipelineRasterizationStateCreateInfo::builder()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::BACK)
            .front_face(vk::FrontFace::CLOCKWISE)
            .line_width(1.0)
            .build();
        let multisample = vk::PipelineMultisampleStateCreateInfo::builder()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1)
            .build();
        let depth_stencil = pipeline_depth_stencil_state();
        let blend_attachment = vk::PipelineColorBlendAttachmentState::builder()
            .color_write_mask(
                vk::ColorComponentFlags::R
                    | vk::ColorComponentFlags::G
                    | vk::ColorComponentFlags::B
                    | vk::ColorComponentFlags::A,
            )
            .build();
        let color_blend = if is_target_depth {
            vk::PipelineColorBlendStateCreateInfo::builder().build()
        } else {
            vk::PipelineColorBlendStateCreateInfo::builder()
                .attachments(std::slice::from_ref(&blend_attachment))
                .build()
        };
        let dynamic_states = [
            vk::DynamicState::VIEWPORT,
            vk::DynamicState::SCISSOR,
            vk::DynamicState::BLEND_CONSTANTS,
        ];
        let dynamic_state = vk::PipelineDynamicStateCreateInfo::builder()
            .dynamic_states(&dynamic_states)
            .build();
        let layout = if single_texture {
            self.one_texture_pipeline_layout
        } else {
            self.two_textures_pipeline_layout
        };
        let mut create_info = vk::GraphicsPipelineCreateInfo::builder()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport_state)
            .rasterization_state(&rasterization)
            .multisample_state(&multisample)
            .color_blend_state(&color_blend)
            .dynamic_state(&dynamic_state)
            .layout(layout)
            .render_pass(renderpass)
            .subpass(0);
        if is_target_depth {
            create_info = create_info.depth_stencil_state(&depth_stencil);
        }
        let create_info = create_info.build();
        unsafe {
            self.device
                .create_graphics_pipelines(vk::PipelineCache::null(), &[create_info], None)
                .map_err(|(_, err)| err)
                .map(|pipelines| pipelines[0])
        }
    }

    /// Port of `BlitImageHelper::ConvertPipelineColorTargetEx`.
    fn convert_pipeline_color_target_ex(
        &self,
        pipeline: vk::Pipeline,
        renderpass: vk::RenderPass,
        module: vk::ShaderModule,
    ) -> Result<vk::Pipeline, vk::Result> {
        self.convert_pipeline_ex(pipeline, renderpass, module, false, false)
    }

    /// Port of `BlitImageHelper::ConvertPipelineDepthTargetEx`.
    fn convert_pipeline_depth_target_ex(
        &self,
        pipeline: vk::Pipeline,
        renderpass: vk::RenderPass,
        module: vk::ShaderModule,
    ) -> Result<vk::Pipeline, vk::Result> {
        self.convert_pipeline_ex(pipeline, renderpass, module, true, true)
    }

    /// Port of `BlitImageHelper::ConvertPipeline`.
    fn convert_pipeline(
        &self,
        pipeline: vk::Pipeline,
        renderpass: vk::RenderPass,
        is_target_depth: bool,
    ) -> Result<vk::Pipeline, vk::Result> {
        if pipeline != vk::Pipeline::null() {
            return Ok(pipeline);
        }
        let fragment_shader = if is_target_depth {
            self.convert_float_to_depth_frag
        } else {
            self.convert_depth_to_float_frag
        };
        let main = CString::new("main").unwrap();
        let stages = [
            vk::PipelineShaderStageCreateInfo::builder()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(self.full_screen_vert)
                .name(&main)
                .build(),
            vk::PipelineShaderStageCreateInfo::builder()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(fragment_shader)
                .name(&main)
                .build(),
        ];
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::builder().build();
        let input_assembly = pipeline_input_assembly_state(unsafe { self.device_owner.as_ref() });
        let viewport_state = vk::PipelineViewportStateCreateInfo::builder()
            .viewport_count(1)
            .scissor_count(1)
            .build();
        let rasterization = vk::PipelineRasterizationStateCreateInfo::builder()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::BACK)
            .front_face(vk::FrontFace::CLOCKWISE)
            .line_width(1.0)
            .build();
        let multisample = vk::PipelineMultisampleStateCreateInfo::builder()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1)
            .build();
        let depth_stencil = pipeline_depth_stencil_state();
        let blend_attachment = vk::PipelineColorBlendAttachmentState::builder()
            .color_write_mask(
                vk::ColorComponentFlags::R
                    | vk::ColorComponentFlags::G
                    | vk::ColorComponentFlags::B
                    | vk::ColorComponentFlags::A,
            )
            .build();
        let color_blend = if is_target_depth {
            vk::PipelineColorBlendStateCreateInfo::builder().build()
        } else {
            vk::PipelineColorBlendStateCreateInfo::builder()
                .attachments(std::slice::from_ref(&blend_attachment))
                .build()
        };
        let dynamic_states = [
            vk::DynamicState::VIEWPORT,
            vk::DynamicState::SCISSOR,
            vk::DynamicState::BLEND_CONSTANTS,
        ];
        let dynamic_state = vk::PipelineDynamicStateCreateInfo::builder()
            .dynamic_states(&dynamic_states)
            .build();
        let mut create_info = vk::GraphicsPipelineCreateInfo::builder()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport_state)
            .rasterization_state(&rasterization)
            .multisample_state(&multisample)
            .color_blend_state(&color_blend)
            .dynamic_state(&dynamic_state)
            .layout(self.one_texture_pipeline_layout)
            .render_pass(renderpass)
            .subpass(0);
        if is_target_depth {
            create_info = create_info.depth_stencil_state(&depth_stencil);
        }
        let create_info = create_info.build();
        unsafe {
            self.device
                .create_graphics_pipelines(vk::PipelineCache::null(), &[create_info], None)
                .map_err(|(_, err)| err)
                .map(|pipelines| pipelines[0])
        }
    }
}

impl Drop for BlitImageHelper {
    fn drop(&mut self) {
        unsafe {
            // Match the reverse member-destruction order of Eden's defaulted
            // destructor: conversion pipelines, retained MSAA resources,
            // cached pipelines, samplers, shaders, then layouts.
            for pipeline in [
                &mut self.convert_s8d24_to_abgr8_pipeline,
                &mut self.convert_d24s8_to_abgr8_pipeline,
                &mut self.convert_d32f_to_abgr8_pipeline,
                &mut self.convert_abgr8_to_d32f_pipeline,
                &mut self.convert_abgr8_to_d24s8_pipeline,
                &mut self.convert_r16_to_d16_pipeline,
                &mut self.convert_d16_to_r16_pipeline,
                &mut self.convert_r32_to_d32_pipeline,
                &mut self.convert_d32_to_r32_pipeline,
            ] {
                if *pipeline != vk::Pipeline::null() {
                    self.device.destroy_pipeline(*pipeline, None);
                    *pipeline = vk::Pipeline::null();
                }
            }

            for resource in self.msaa_copy_resources.drain(..) {
                self.device.destroy_framebuffer(resource.framebuffer, None);
                self.device.destroy_image_view(resource.dst_view, None);
                self.device.destroy_image_view(resource.src_view, None);
            }

            for pipeline in self
                .resolve_depth_stencil_pipelines
                .iter_mut()
                .chain(self.resolve_depth_pipelines.iter_mut())
                .chain(self.blit_msaa_color_pipelines.iter_mut())
                .chain(self.msaa_copy_pipelines.iter_mut())
                .chain(self.clear_stencil_pipelines.iter_mut())
                .chain(self.clear_color_pipelines.iter_mut())
                .chain(self.blit_depth_stencil_pipelines.iter_mut())
                .chain(self.blit_color_pipelines.iter_mut())
            {
                if *pipeline != vk::Pipeline::null() {
                    self.device.destroy_pipeline(*pipeline, None);
                    *pipeline = vk::Pipeline::null();
                }
            }

            if self.nearest_sampler != vk::Sampler::null() {
                self.device.destroy_sampler(self.nearest_sampler, None);
                self.nearest_sampler = vk::Sampler::null();
            }
            if self.linear_sampler != vk::Sampler::null() {
                self.device.destroy_sampler(self.linear_sampler, None);
                self.linear_sampler = vk::Sampler::null();
            }

            for shader in [
                &mut self.convert_non_msaa_to_msaa_frag,
                &mut self.convert_msaa_to_non_msaa_frag,
                &mut self.convert_s8d24_to_abgr8_frag,
                &mut self.convert_d24s8_to_abgr8_frag,
                &mut self.convert_d32f_to_abgr8_frag,
                &mut self.convert_abgr8_to_d32f_frag,
                &mut self.convert_abgr8_to_d24s8_frag,
                &mut self.convert_float_to_depth_frag,
                &mut self.convert_depth_to_float_frag,
                &mut self.clear_stencil_frag,
                &mut self.clear_color_frag,
                &mut self.clear_color_vert,
                &mut self.blit_depth_stencil_msaa_frag,
                &mut self.blit_depth_msaa_frag,
                &mut self.blit_depth_stencil_frag,
                &mut self.blit_color_msaa_frag,
                &mut self.blit_color_to_color_frag,
                &mut self.full_screen_vert,
            ] {
                if *shader != vk::ShaderModule::null() {
                    self.device.destroy_shader_module(*shader, None);
                    *shader = vk::ShaderModule::null();
                }
            }

            if self.msaa_copy_pipeline_layout != vk::PipelineLayout::null() {
                self.device
                    .destroy_pipeline_layout(self.msaa_copy_pipeline_layout, None);
                self.msaa_copy_pipeline_layout = vk::PipelineLayout::null();
            }
            if self.clear_color_pipeline_layout != vk::PipelineLayout::null() {
                self.device
                    .destroy_pipeline_layout(self.clear_color_pipeline_layout, None);
                self.clear_color_pipeline_layout = vk::PipelineLayout::null();
            }
            if self.two_textures_pipeline_layout != vk::PipelineLayout::null() {
                self.device
                    .destroy_pipeline_layout(self.two_textures_pipeline_layout, None);
                self.two_textures_pipeline_layout = vk::PipelineLayout::null();
            }
            if self.one_texture_pipeline_layout != vk::PipelineLayout::null() {
                self.device
                    .destroy_pipeline_layout(self.one_texture_pipeline_layout, None);
                self.one_texture_pipeline_layout = vk::PipelineLayout::null();
            }
            if self.two_textures_set_layout != vk::DescriptorSetLayout::null() {
                self.device
                    .destroy_descriptor_set_layout(self.two_textures_set_layout, None);
                self.two_textures_set_layout = vk::DescriptorSetLayout::null();
            }
            if self.one_texture_set_layout != vk::DescriptorSetLayout::null() {
                self.device
                    .destroy_descriptor_set_layout(self.one_texture_set_layout, None);
                self.one_texture_set_layout = vk::DescriptorSetLayout::null();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_constants_match_upstream_layout() {
        assert_eq!(std::mem::size_of::<PushConstants>(), 16);
        assert_eq!(std::mem::align_of::<PushConstants>(), 4);
        assert_eq!(std::mem::offset_of!(PushConstants, tex_scale), 0);
        assert_eq!(std::mem::offset_of!(PushConstants, tex_offset), 8);
    }

    #[test]
    fn msaa_copy_push_constants_match_upstream_layout() {
        assert_eq!(std::mem::size_of::<MsaaCopyPushConstants>(), 24);
        assert_eq!(std::mem::align_of::<MsaaCopyPushConstants>(), 4);
        assert_eq!(std::mem::offset_of!(MsaaCopyPushConstants, dst_offset), 0);
        assert_eq!(std::mem::offset_of!(MsaaCopyPushConstants, src_offset), 8);
        assert_eq!(std::mem::offset_of!(MsaaCopyPushConstants, scale), 16);
    }

    #[test]
    fn subresource_range_matches_upstream_format_aspects_and_slice_rule() {
        let range = SubresourceRange {
            base: crate::texture_cache::types::SubresourceBase { level: 2, layer: 3 },
            extent: crate::texture_cache::types::SubresourceExtent {
                levels: 4,
                layers: 5,
            },
        };
        for (format, aspect_mask) in [
            (PixelFormat::A8B8G8R8Unorm, vk::ImageAspectFlags::COLOR),
            (PixelFormat::D32Float, vk::ImageAspectFlags::DEPTH),
            (PixelFormat::S8Uint, vk::ImageAspectFlags::STENCIL),
            (
                PixelFormat::D24UnormS8Uint,
                vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL,
            ),
            (PixelFormat::Invalid, vk::ImageAspectFlags::COLOR),
        ] {
            let actual = subresource_range_from_view(format, range, false);
            assert_eq!(actual.aspect_mask, aspect_mask);
            assert_eq!(actual.base_mip_level, 2);
            assert_eq!(actual.level_count, 4);
            assert_eq!(actual.base_array_layer, 3);
            assert_eq!(actual.layer_count, 5);
        }

        let slice = subresource_range_from_view(PixelFormat::D32Float, range, true);
        assert_eq!(slice.base_array_layer, 0);
        assert_eq!(slice.layer_count, 1);
    }

    #[test]
    fn depth_stencil_pipeline_state_matches_upstream_stencil_export_contract() {
        let state = pipeline_depth_stencil_state();
        assert_eq!(state.depth_test_enable, vk::TRUE);
        assert_eq!(state.depth_write_enable, vk::TRUE);
        assert_eq!(state.depth_compare_op, vk::CompareOp::ALWAYS);
        assert_eq!(state.stencil_test_enable, vk::TRUE);
        for stencil in [state.front, state.back] {
            assert_eq!(stencil.fail_op, vk::StencilOp::REPLACE);
            assert_eq!(stencil.pass_op, vk::StencilOp::REPLACE);
            assert_eq!(stencil.depth_fail_op, vk::StencilOp::KEEP);
            assert_eq!(stencil.compare_op, vk::CompareOp::ALWAYS);
            assert_eq!(stencil.compare_mask, 0);
            assert_eq!(stencil.write_mask, u32::MAX);
            assert_eq!(stencil.reference, 0);
        }
    }
}
