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
    BLIT_COLOR_FLOAT_FRAG_SPV, BLIT_COLOR_MSAA_FRAG_SPV, BLIT_DEPTH_FRAG_SPV,
    BLIT_DEPTH_MSAA_FRAG_SPV, BLIT_DEPTH_STENCIL_MSAA_FRAG_SPV, CONVERT_ABGR8_TO_D24S8_FRAG_SPV,
    CONVERT_ABGR8_TO_D32F_FRAG_SPV, CONVERT_D24S8_TO_ABGR8_FRAG_SPV,
    CONVERT_D32F_TO_ABGR8_FRAG_SPV, CONVERT_D32S8_TO_RG32_FRAG_SPV,
    CONVERT_DEPTH_TO_FLOAT_FRAG_SPV, CONVERT_FLOAT_TO_DEPTH_FRAG_SPV,
    CONVERT_MSAA_TO_NON_MSAA_DEPTH_FRAG_SPV, CONVERT_MSAA_TO_NON_MSAA_DEPTH_STENCIL_FRAG_SPV,
    CONVERT_MSAA_TO_NON_MSAA_FRAG_SPV, CONVERT_MSAA_TO_NON_MSAA_SINT_FRAG_SPV,
    CONVERT_MSAA_TO_NON_MSAA_UINT_FRAG_SPV, CONVERT_NON_MSAA_TO_MSAA_DEPTH_FRAG_SPV,
    CONVERT_NON_MSAA_TO_MSAA_DEPTH_STENCIL_FRAG_SPV, CONVERT_NON_MSAA_TO_MSAA_FRAG_SPV,
    CONVERT_NON_MSAA_TO_MSAA_SINT_FRAG_SPV, CONVERT_NON_MSAA_TO_MSAA_UINT_FRAG_SPV,
    CONVERT_RG32_TO_D32S8_FRAG_SPV, CONVERT_S8D24_TO_ABGR8_FRAG_SPV, FULL_SCREEN_TRIANGLE_VERT_SPV,
    VULKAN_BLIT_DEPTH_STENCIL_FRAG_SPV, VULKAN_COLOR_CLEAR_FRAG_SPV, VULKAN_COLOR_CLEAR_VERT_SPV,
    VULKAN_DEPTHSTENCIL_CLEAR_FRAG_SPV,
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
use crate::texture_cache::types::{ImageCopy, ImageType, SubresourceRange, NUM_RT};
use crate::vulkan_common::vulkan_device::{Device, FormatType};
use crate::vulkan_common::vulkan_wrapper::PIPELINE_STAGE_GRAPHICS_COMPUTE_TRANSFER;

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

/// Port of `MSAACopyFormatClass`: selects the float, signed or unsigned
/// integer variant of the MSAA conversion shaders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum MsaaCopyFormatClass {
    Float,
    SignedInteger,
    UnsignedInteger,
}

/// Port of the anonymous-namespace `FormatClass`.
fn format_class(format: PixelFormat) -> MsaaCopyFormatClass {
    if !crate::surface::is_pixel_format_integer(format) {
        return MsaaCopyFormatClass::Float;
    }
    if crate::surface::is_pixel_format_signed_integer(format) {
        return MsaaCopyFormatClass::SignedInteger;
    }
    MsaaCopyFormatClass::UnsignedInteger
}

/// Port of `MSAACopyPipelineKey`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct MsaaCopyPipelineKey {
    renderpass: vk::RenderPass,
    samples: vk::SampleCountFlags,
    msaa_to_non_msaa: bool,
    format_class: MsaaCopyFormatClass,
}

/// Port of `BlitImageHelper::MSAACopyAspectInfo`: aspect and barrier masks
/// shared by the color and depth/stencil MSAA copy paths.
#[derive(Debug, Clone, Copy)]
struct MsaaCopyAspectInfo {
    src_view_aspect: vk::ImageAspectFlags,
    attachment_aspect: vk::ImageAspectFlags,
    barrier_aspect: vk::ImageAspectFlags,
    pre_src_access: vk::AccessFlags,
    pre_src_dst_access: vk::AccessFlags,
    pre_dst_dst_access: vk::AccessFlags,
    pre_src_stages: vk::PipelineStageFlags,
    pre_dst_stages: vk::PipelineStageFlags,
    post_src_access: vk::AccessFlags,
    post_dst_access: vk::AccessFlags,
    post_src_stages: vk::PipelineStageFlags,
    post_dst_stages: vk::PipelineStageFlags,
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

/// Transient views and framebuffer retained until the recorded reinterpret
/// draw has completed on the GPU.
struct ReinterpretResources {
    tick: u64,
    views: [vk::ImageView; 3],
    framebuffer: vk::Framebuffer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum D32S8Rg32Direction {
    DepthStencilToColor,
    ColorToDepthStencil,
}

fn d32s8_rg32_direction(
    dst_format: PixelFormat,
    src_format: PixelFormat,
) -> Option<D32S8Rg32Direction> {
    match (dst_format, src_format) {
        (PixelFormat::R32G32Float, PixelFormat::D32FloatS8Uint) => {
            Some(D32S8Rg32Direction::DepthStencilToColor)
        }
        (PixelFormat::D32FloatS8Uint, PixelFormat::R32G32Float) => {
            Some(D32S8Rg32Direction::ColorToDepthStencil)
        }
        _ => None,
    }
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
    /// MSAA images whose resolve shadows must be marked when this pass begins,
    /// matching `Framebuffer::MarkResolveShadowsUpToDate`.
    pub resolve_shadow_images: [vk::Image; NUM_RT + 1],
    pub num_resolve_shadows: usize,
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

/// Depth/stencil state of upstream `FindOrEmplaceMSAACopyDepthPipeline`: depth
/// always written, stencil replaced from `gl_FragStencilRefARB` when copied.
fn msaa_copy_depth_stencil_state(copy_stencil: bool) -> vk::PipelineDepthStencilStateCreateInfo {
    const REPLACE_STENCIL_OP: vk::StencilOpState = vk::StencilOpState {
        fail_op: vk::StencilOp::REPLACE,
        pass_op: vk::StencilOp::REPLACE,
        depth_fail_op: vk::StencilOp::REPLACE,
        compare_op: vk::CompareOp::ALWAYS,
        compare_mask: 0xFF,
        write_mask: 0xFF,
        reference: 0,
    };
    let stencil = if copy_stencil {
        REPLACE_STENCIL_OP
    } else {
        vk::StencilOpState::default()
    };
    vk::PipelineDepthStencilStateCreateInfo::builder()
        .depth_test_enable(true)
        .depth_write_enable(true)
        .depth_compare_op(vk::CompareOp::ALWAYS)
        .depth_bounds_test_enable(false)
        .stencil_test_enable(copy_stencil)
        .front(stencil)
        .back(stencil)
        .min_depth_bounds(0.0)
        .max_depth_bounds(0.0)
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
    base_layer: u32,
    aspect_mask: vk::ImageAspectFlags,
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
            aspect_mask,
            base_mip_level: base_level,
            level_count: 1,
            base_array_layer: base_layer,
            layer_count: 1,
        })
        .build();
    unsafe { device.create_image_view(&create_info, None) }
}

fn make_reinterpret_view(
    device: &ash::Device,
    image: vk::Image,
    format: vk::Format,
    aspect_mask: vk::ImageAspectFlags,
    base_level: u32,
    base_layer: u32,
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
            aspect_mask,
            base_mip_level: base_level,
            level_count: 1,
            base_array_layer: base_layer,
            layer_count: 1,
        })
        .build();
    unsafe { device.create_image_view(&create_info, None) }
}

fn mip_extent(size: Extent3D, level: u32) -> vk::Extent2D {
    vk::Extent2D {
        width: (size.width >> level).max(1),
        height: (size.height >> level).max(1),
    }
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
    msaa_copy_depth_stencil_pipeline_layout: vk::PipelineLayout,

    // Shader modules
    full_screen_vert: vk::ShaderModule,
    blit_color_to_color_frag: vk::ShaderModule,
    blit_color_msaa_frag: vk::ShaderModule,
    blit_depth_stencil_frag: vk::ShaderModule,
    blit_depth_frag: vk::ShaderModule,
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
    convert_d32s8_to_rg32_frag: vk::ShaderModule,
    convert_rg32_to_d32s8_frag: vk::ShaderModule,
    convert_msaa_to_non_msaa_frag: vk::ShaderModule,
    convert_msaa_to_non_msaa_sint_frag: vk::ShaderModule,
    convert_msaa_to_non_msaa_uint_frag: vk::ShaderModule,
    convert_msaa_to_non_msaa_depth_frag: vk::ShaderModule,
    convert_msaa_to_non_msaa_depth_stencil_frag: vk::ShaderModule,
    convert_non_msaa_to_msaa_frag: vk::ShaderModule,
    convert_non_msaa_to_msaa_sint_frag: vk::ShaderModule,
    convert_non_msaa_to_msaa_uint_frag: vk::ShaderModule,
    convert_non_msaa_to_msaa_depth_frag: vk::ShaderModule,
    convert_non_msaa_to_msaa_depth_stencil_frag: vk::ShaderModule,

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
    msaa_copy_depth_keys: Vec<MsaaCopyPipelineKey>,
    msaa_copy_depth_pipelines: Vec<vk::Pipeline>,
    msaa_copy_depth_stencil_keys: Vec<MsaaCopyPipelineKey>,
    msaa_copy_depth_stencil_pipelines: Vec<vk::Pipeline>,
    blit_msaa_color_keys: Vec<BlitMsaaPipelineKey>,
    blit_msaa_color_pipelines: Vec<vk::Pipeline>,
    blit_depth_keys: Vec<vk::RenderPass>,
    blit_depth_pipelines: Vec<vk::Pipeline>,
    blit_msaa_depth_keys: Vec<BlitMsaaPipelineKey>,
    blit_msaa_depth_pipelines: Vec<vk::Pipeline>,
    blit_msaa_depth_stencil_keys: Vec<BlitMsaaPipelineKey>,
    blit_msaa_depth_stencil_pipelines: Vec<vk::Pipeline>,
    resolve_depth_keys: Vec<vk::RenderPass>,
    resolve_depth_pipelines: Vec<vk::Pipeline>,
    resolve_depth_stencil_keys: Vec<vk::RenderPass>,
    resolve_depth_stencil_pipelines: Vec<vk::Pipeline>,
    msaa_copy_resources: VecDeque<MsaaCopyResources>,
    reinterpret_resources: VecDeque<ReinterpretResources>,

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
    convert_d32s8_to_rg32_pipeline: vk::Pipeline,
    convert_rg32_to_d32s8_pipeline: vk::Pipeline,

    mark_resolve_shadows_hook: Option<unsafe fn(NonNull<()>, &[vk::Image])>,
    resolve_shadow_runtime: Option<NonNull<()>>,
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
            .allocator(
                vulkan_device,
                scheduler,
                one_texture_set_layout,
                &Self::ONE_TEXTURE_BANK_INFO,
            )
            .expect("Failed to create one-texture descriptor allocator");
        let two_textures_descriptor_allocator = descriptor_pool
            .allocator(
                vulkan_device,
                scheduler,
                two_textures_set_layout,
                &Self::TWO_TEXTURES_BANK_INFO,
            )
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
        let msaa_copy_depth_stencil_pl_ci = vk::PipelineLayoutCreateInfo::builder()
            .set_layouts(&two_tex_layouts)
            .push_constant_ranges(std::slice::from_ref(&msaa_copy_push_range))
            .build();
        let msaa_copy_depth_stencil_pipeline_layout = unsafe {
            device
                .create_pipeline_layout(&msaa_copy_depth_stencil_pl_ci, None)
                .expect("Failed to create MSAA depth/stencil copy pipeline layout")
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
        let blit_depth_frag =
            build_shader(&device, BLIT_DEPTH_FRAG_SPV).expect("Failed to build blit_depth.frag");
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
        let convert_d32s8_to_rg32_frag = build_shader(&device, CONVERT_D32S8_TO_RG32_FRAG_SPV)
            .expect("Failed to build convert_d32s8_to_rg32.frag");
        let convert_rg32_to_d32s8_frag = if shader_stencil_export_supported {
            build_shader(&device, CONVERT_RG32_TO_D32S8_FRAG_SPV)
                .expect("Failed to build convert_rg32_to_d32s8.frag")
        } else {
            vk::ShaderModule::null()
        };
        let convert_msaa_to_non_msaa_frag =
            build_shader(&device, CONVERT_MSAA_TO_NON_MSAA_FRAG_SPV)
                .expect("Failed to build convert_msaa_to_non_msaa.frag");
        let convert_msaa_to_non_msaa_sint_frag =
            build_shader(&device, CONVERT_MSAA_TO_NON_MSAA_SINT_FRAG_SPV)
                .expect("Failed to build convert_msaa_to_non_msaa_sint.frag");
        let convert_msaa_to_non_msaa_uint_frag =
            build_shader(&device, CONVERT_MSAA_TO_NON_MSAA_UINT_FRAG_SPV)
                .expect("Failed to build convert_msaa_to_non_msaa_uint.frag");
        let convert_msaa_to_non_msaa_depth_frag =
            build_shader(&device, CONVERT_MSAA_TO_NON_MSAA_DEPTH_FRAG_SPV)
                .expect("Failed to build convert_msaa_to_non_msaa_depth.frag");
        let convert_msaa_to_non_msaa_depth_stencil_frag =
            build_shader(&device, CONVERT_MSAA_TO_NON_MSAA_DEPTH_STENCIL_FRAG_SPV)
                .expect("Failed to build convert_msaa_to_non_msaa_depth_stencil.frag");
        let convert_non_msaa_to_msaa_frag =
            build_shader(&device, CONVERT_NON_MSAA_TO_MSAA_FRAG_SPV)
                .expect("Failed to build convert_non_msaa_to_msaa.frag");
        let convert_non_msaa_to_msaa_sint_frag =
            build_shader(&device, CONVERT_NON_MSAA_TO_MSAA_SINT_FRAG_SPV)
                .expect("Failed to build convert_non_msaa_to_msaa_sint.frag");
        let convert_non_msaa_to_msaa_uint_frag =
            build_shader(&device, CONVERT_NON_MSAA_TO_MSAA_UINT_FRAG_SPV)
                .expect("Failed to build convert_non_msaa_to_msaa_uint.frag");
        let convert_non_msaa_to_msaa_depth_frag =
            build_shader(&device, CONVERT_NON_MSAA_TO_MSAA_DEPTH_FRAG_SPV)
                .expect("Failed to build convert_non_msaa_to_msaa_depth.frag");
        let convert_non_msaa_to_msaa_depth_stencil_frag = if shader_stencil_export_supported {
            build_shader(&device, CONVERT_NON_MSAA_TO_MSAA_DEPTH_STENCIL_FRAG_SPV)
                .expect("Failed to build convert_non_msaa_to_msaa_depth_stencil.frag")
        } else {
            vk::ShaderModule::null()
        };

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
            msaa_copy_depth_stencil_pipeline_layout,
            full_screen_vert,
            blit_color_to_color_frag,
            blit_color_msaa_frag,
            blit_depth_stencil_frag,
            blit_depth_frag,
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
            convert_d32s8_to_rg32_frag,
            convert_rg32_to_d32s8_frag,
            convert_msaa_to_non_msaa_frag,
            convert_msaa_to_non_msaa_sint_frag,
            convert_msaa_to_non_msaa_uint_frag,
            convert_msaa_to_non_msaa_depth_frag,
            convert_msaa_to_non_msaa_depth_stencil_frag,
            convert_non_msaa_to_msaa_frag,
            convert_non_msaa_to_msaa_sint_frag,
            convert_non_msaa_to_msaa_uint_frag,
            convert_non_msaa_to_msaa_depth_frag,
            convert_non_msaa_to_msaa_depth_stencil_frag,
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
            msaa_copy_depth_keys: Vec::new(),
            msaa_copy_depth_pipelines: Vec::new(),
            msaa_copy_depth_stencil_keys: Vec::new(),
            msaa_copy_depth_stencil_pipelines: Vec::new(),
            blit_msaa_color_keys: Vec::new(),
            blit_msaa_color_pipelines: Vec::new(),
            blit_depth_keys: Vec::new(),
            blit_depth_pipelines: Vec::new(),
            blit_msaa_depth_keys: Vec::new(),
            blit_msaa_depth_pipelines: Vec::new(),
            blit_msaa_depth_stencil_keys: Vec::new(),
            blit_msaa_depth_stencil_pipelines: Vec::new(),
            resolve_depth_keys: Vec::new(),
            resolve_depth_pipelines: Vec::new(),
            resolve_depth_stencil_keys: Vec::new(),
            resolve_depth_stencil_pipelines: Vec::new(),
            msaa_copy_resources: VecDeque::new(),
            reinterpret_resources: VecDeque::new(),
            convert_d32_to_r32_pipeline: vk::Pipeline::null(),
            convert_r32_to_d32_pipeline: vk::Pipeline::null(),
            convert_d16_to_r16_pipeline: vk::Pipeline::null(),
            convert_r16_to_d16_pipeline: vk::Pipeline::null(),
            convert_abgr8_to_d24s8_pipeline: vk::Pipeline::null(),
            convert_abgr8_to_d32f_pipeline: vk::Pipeline::null(),
            convert_d32f_to_abgr8_pipeline: vk::Pipeline::null(),
            convert_d24s8_to_abgr8_pipeline: vk::Pipeline::null(),
            convert_s8d24_to_abgr8_pipeline: vk::Pipeline::null(),
            convert_d32s8_to_rg32_pipeline: vk::Pipeline::null(),
            convert_rg32_to_d32s8_pipeline: vk::Pipeline::null(),
            mark_resolve_shadows_hook: None,
            resolve_shadow_runtime: None,
        }
    }

    /// Installed by `TextureCacheRuntime` so helper blits can mark resolve
    /// shadows the same way `Scheduler::BeginRenderPassImpl` does.
    pub fn set_resolve_shadow_hook(
        &mut self,
        runtime: NonNull<()>,
        hook: unsafe fn(NonNull<()>, &[vk::Image]),
    ) {
        self.resolve_shadow_runtime = Some(runtime);
        self.mark_resolve_shadows_hook = Some(hook);
    }

    fn request_blit_renderpass(&mut self, dst: &BlitFramebufferInfo) {
        let render_area = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: dst.render_area,
        };
        unsafe { self.scheduler.as_mut() }.request_renderpass_raw(
            dst.framebuffer,
            dst.render_pass,
            render_area,
            &[],
            &dst.images[..dst.num_images],
            &dst.image_ranges[..dst.num_images],
        );
        if dst.num_resolve_shadows == 0 {
            return;
        }
        let (Some(hook), Some(runtime)) =
            (self.mark_resolve_shadows_hook, self.resolve_shadow_runtime)
        else {
            return;
        };
        unsafe {
            hook(
                runtime,
                &dst.resolve_shadow_images[..dst.num_resolve_shadows],
            );
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
        let mut sampler = self.nearest_sampler;
        if filter == Filter::Bilinear {
            sampler = self.linear_sampler;
        }
        self.blit_impl(
            dst_framebuffer,
            src_image_view,
            dst_region,
            src_region,
            pipeline,
            sampler,
            src_image_view.color_view,
            vk::ImageView::null(),
            false,
        )
    }

    /// Port of `BlitImageHelper::BlitImpl`: the shared full-screen blit
    /// recording used by every sampled blit variant.
    #[allow(clippy::too_many_arguments)]
    fn blit_impl(
        &mut self,
        dst_framebuffer: BlitFramebufferInfo,
        src_image_view: BlitImageView,
        dst_region: &Region2D,
        src_region: &Region2D,
        pipeline: vk::Pipeline,
        sampler: vk::Sampler,
        src_view: vk::ImageView,
        src_stencil_view: vk::ImageView,
        blit_stencil: bool,
    ) -> bool {
        let mut layout = self.one_texture_pipeline_layout;
        if blit_stencil {
            layout = self.two_textures_pipeline_layout;
        }
        let one_texture_allocator = self.one_texture_descriptor_allocator.reference();
        let two_textures_allocator = self.two_textures_descriptor_allocator.reference();
        let device = self.device.clone();
        let dst_region = *dst_region;
        let src_region = *src_region;
        record_shader_read_barrier(
            &self.device,
            unsafe { self.scheduler.as_mut() },
            src_image_view,
        );
        self.request_blit_renderpass(&dst_framebuffer);
        let scheduler = unsafe { self.scheduler.as_mut() };
        scheduler.record(move |cmdbuf| unsafe {
            let descriptor_set = if blit_stencil {
                let descriptor_set = two_textures_allocator
                    .commit()
                    .expect("Failed to allocate two-texture blit descriptor set");
                update_two_textures_descriptor_set(
                    &device,
                    descriptor_set,
                    sampler,
                    src_view,
                    src_stencil_view,
                );
                descriptor_set
            } else {
                let descriptor_set = one_texture_allocator
                    .commit()
                    .expect("Failed to allocate one-texture blit descriptor set");
                update_one_texture_descriptor_set(&device, descriptor_set, sampler, src_view);
                descriptor_set
            };
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
        let sampler = self.nearest_sampler;
        self.blit_impl(
            dst_framebuffer,
            src_image_view,
            dst_region,
            src_region,
            pipeline,
            sampler,
            src_image_view.color_view,
            vk::ImageView::null(),
            false,
        )
    }

    /// Port of `BlitImageHelper::BlitDepthStencilMSAA`: MSAA->MSAA depth (and
    /// stencil, with `VK_EXT_shader_stencil_export`) blit through per-sample
    /// shaders.
    pub fn blit_depth_stencil_msaa(
        &mut self,
        dst_framebuffer: BlitFramebufferInfo,
        src_image_view: BlitImageView,
        dst_region: &Region2D,
        src_region: &Region2D,
    ) -> bool {
        let blit_stencil = dst_framebuffer.has_stencil && self.shader_stencil_export_supported;
        let key = BlitMsaaPipelineKey {
            renderpass: dst_framebuffer.render_pass,
            samples: dst_framebuffer.samples,
        };
        let pipeline =
            match self.find_or_emplace_blit_depth_stencil_msaa_pipeline(&key, blit_stencil) {
                Ok(pipeline) => pipeline,
                Err(err) => {
                    log::warn!(
                    "BlitImageHelper: failed to create MSAA depth/stencil blit pipeline: {err:?}"
                );
                    return false;
                }
            };
        let mut src_stencil_view = vk::ImageView::null();
        if blit_stencil {
            src_stencil_view = src_image_view.stencil_view;
        }
        let sampler = self.nearest_sampler;
        self.blit_impl(
            dst_framebuffer,
            src_image_view,
            dst_region,
            src_region,
            pipeline,
            sampler,
            src_image_view.depth_view,
            src_stencil_view,
            blit_stencil,
        )
    }

    /// Port of `BlitImageHelper::BlitDepth`: depth-only sampled blit, used
    /// when stencil export is unavailable.
    pub fn blit_depth(
        &mut self,
        dst_framebuffer: BlitFramebufferInfo,
        src_image_view: BlitImageView,
        dst_region: &Region2D,
        src_region: &Region2D,
    ) -> bool {
        let pipeline = match self.find_or_emplace_blit_depth_pipeline(dst_framebuffer.render_pass) {
            Ok(pipeline) => pipeline,
            Err(err) => {
                log::warn!("BlitImageHelper: failed to create depth blit pipeline: {err:?}");
                return false;
            }
        };
        let sampler = self.nearest_sampler;
        self.blit_impl(
            dst_framebuffer,
            src_image_view,
            dst_region,
            src_region,
            pipeline,
            sampler,
            src_image_view.depth_view,
            vk::ImageView::null(),
            false,
        )
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
        let mut src_stencil_view = vk::ImageView::null();
        if resolve_stencil {
            src_stencil_view = src_image_view.stencil_view;
        }
        let sampler = self.nearest_sampler;
        self.blit_impl(
            dst_framebuffer,
            src_image_view,
            dst_region,
            src_region,
            pipeline,
            sampler,
            src_image_view.depth_view,
            src_stencil_view,
            resolve_stencil,
        )
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
        let descriptor_allocator = self.one_texture_descriptor_allocator.reference();
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
        assert_fail_soft(
            filter == Filter::Point,
            "depth/stencil blit requires point filtering",
        );
        assert_fail_soft(
            operation == Operation::SrcCopy,
            "depth/stencil blit requires SrcCopy",
        );
        // Without stencil export the depth aspect is still blitted through
        // `BlitDepth`.
        let blit_stencil = self.shader_stencil_export_supported;
        let key = BlitImagePipelineKey {
            renderpass: dst_framebuffer.render_pass,
            operation,
        };
        let pipeline_result = if blit_stencil {
            self.find_or_emplace_depth_stencil_pipeline(&key)
        } else {
            self.find_or_emplace_blit_depth_pipeline(key.renderpass)
        };
        let pipeline = match pipeline_result {
            Ok(pipeline) => pipeline,
            Err(err) => {
                log::warn!(
                    "BlitImageHelper: failed to create depth/stencil blit pipeline: {err:?}"
                );
                return false;
            }
        };
        let mut src_stencil_view = vk::ImageView::null();
        if blit_stencil {
            src_stencil_view = src_image_view.stencil_view;
        }
        let sampler = self.nearest_sampler;
        self.blit_impl(
            dst_framebuffer,
            src_image_view,
            dst_region,
            src_region,
            pipeline,
            sampler,
            src_image_view.depth_view,
            src_stencil_view,
            blit_stencil,
        )
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

    /// Converts the raw two-word representation of a D32S8 texel into RG32.
    /// The first word preserves the depth float bits and the low byte of the
    /// second word preserves stencil. This replaces a Vulkan-invalid combined
    /// depth/stencil buffer copy while retaining the guest byte layout.
    fn convert_d32s8_to_rg32(
        &mut self,
        dst_framebuffer: BlitFramebufferInfo,
        src_image_view: BlitImageView,
        dst_region: Region2D,
        src_region: Region2D,
    ) -> bool {
        let pipeline = match self.convert_pipeline_ex(
            self.convert_d32s8_to_rg32_pipeline,
            dst_framebuffer.render_pass,
            self.convert_d32s8_to_rg32_frag,
            false,
            false,
        ) {
            Ok(pipeline) => pipeline,
            Err(err) => {
                log::warn!("BlitImageHelper: failed to create D32S8->RG32 pipeline: {err:?}");
                return false;
            }
        };
        self.convert_d32s8_to_rg32_pipeline = pipeline;
        let layout = self.two_textures_pipeline_layout;
        let sampler = self.nearest_sampler;
        let descriptor_allocator = self.two_textures_descriptor_allocator.reference();
        let src_depth_view = src_image_view.depth_view;
        let src_stencil_view = src_image_view.stencil_view;
        let device = self.device.clone();
        record_shader_read_barrier(
            &self.device,
            unsafe { self.scheduler.as_mut() },
            src_image_view,
        );
        self.request_blit_renderpass(&dst_framebuffer);
        let scheduler = unsafe { self.scheduler.as_mut() };
        scheduler.record(move |cmdbuf| unsafe {
            let descriptor_set = descriptor_allocator
                .commit()
                .expect("Failed to allocate D32S8->RG32 descriptor set");
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

    /// Restores D32S8 depth/stencil from the two raw words held by RG32.
    fn convert_rg32_to_d32s8(
        &mut self,
        dst_framebuffer: BlitFramebufferInfo,
        src_image_view: BlitImageView,
        dst_region: Region2D,
        src_region: Region2D,
    ) -> bool {
        if !self.shader_stencil_export_supported {
            log::warn!("BlitImageHelper: RG32->D32S8 requires shader_stencil_export");
            return false;
        }
        let pipeline = match self.convert_pipeline_ex(
            self.convert_rg32_to_d32s8_pipeline,
            dst_framebuffer.render_pass,
            self.convert_rg32_to_d32s8_frag,
            true,
            true,
        ) {
            Ok(pipeline) => pipeline,
            Err(err) => {
                log::warn!("BlitImageHelper: failed to create RG32->D32S8 pipeline: {err:?}");
                return false;
            }
        };
        self.convert_rg32_to_d32s8_pipeline = pipeline;
        let layout = self.one_texture_pipeline_layout;
        let sampler = self.nearest_sampler;
        let descriptor_allocator = self.one_texture_descriptor_allocator.reference();
        let src_view = src_image_view.color_view;
        let device = self.device.clone();
        record_shader_read_barrier(
            &self.device,
            unsafe { self.scheduler.as_mut() },
            src_image_view,
        );
        self.request_blit_renderpass(&dst_framebuffer);
        let scheduler = unsafe { self.scheduler.as_mut() };
        scheduler.record(move |cmdbuf| unsafe {
            let descriptor_set = descriptor_allocator
                .commit()
                .expect("Failed to allocate RG32->D32S8 descriptor set");
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
        let device = self.device.clone();
        let dst_region = *dst_region;
        self.request_blit_renderpass(&dst_framebuffer);
        let scheduler = unsafe { self.scheduler.as_mut() };
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
        let device = self.device.clone();
        let dst_region = *dst_region;
        self.request_blit_renderpass(&dst_framebuffer);
        let scheduler = unsafe { self.scheduler.as_mut() };
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
        let descriptor_allocator = self.one_texture_descriptor_allocator.reference();
        let device = self.device.clone();
        record_shader_read_barrier(
            &self.device,
            unsafe { self.scheduler.as_mut() },
            src_image_view,
        );
        self.request_blit_renderpass(&dst_framebuffer);
        let scheduler = unsafe { self.scheduler.as_mut() };
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
        let descriptor_allocator = self.two_textures_descriptor_allocator.reference();
        let src_depth_view = src_image_view.depth_view;
        let src_stencil_view = src_image_view.stencil_view;
        let device = self.device.clone();
        record_shader_read_barrier(
            &self.device,
            unsafe { self.scheduler.as_mut() },
            src_image_view,
        );
        self.request_blit_renderpass(&dst_framebuffer);
        let scheduler = unsafe { self.scheduler.as_mut() };
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

    /// Reinterprets the raw D32S8/RG32 two-word texel representation through
    /// shader loads and attachment stores. Vulkan buffer-image copies cannot
    /// address depth and stencil together in one region, so the upstream
    /// buffer round trip is invalid for this format pair.
    #[allow(clippy::too_many_arguments)]
    pub fn reinterpret_d32s8_rg32(
        &mut self,
        render_pass_cache: &RenderPassCache,
        dst_image: vk::Image,
        dst_format: PixelFormat,
        dst_type: ImageType,
        dst_size: Extent3D,
        src_image: vk::Image,
        src_format: PixelFormat,
        src_type: ImageType,
        src_size: Extent3D,
        copies: &[ImageCopy],
    ) -> bool {
        let Some(direction) = d32s8_rg32_direction(dst_format, src_format) else {
            return false;
        };
        if direction == D32S8Rg32Direction::ColorToDepthStencil
            && !self.shader_stencil_export_supported
        {
            log::warn!("D32S8 reinterpretation requires shader_stencil_export");
            return false;
        }

        while self
            .reinterpret_resources
            .front()
            .is_some_and(|resource| unsafe { self.scheduler.as_ref() }.is_free(resource.tick))
        {
            let resource = self.reinterpret_resources.pop_front().unwrap();
            unsafe {
                self.device.destroy_framebuffer(resource.framebuffer, None);
                for view in resource.views {
                    if view != vk::ImageView::null() {
                        self.device.destroy_image_view(view, None);
                    }
                }
            }
        }

        let mut render_pass_key = RenderPassKey::default();
        match direction {
            D32S8Rg32Direction::DepthStencilToColor => {
                render_pass_key.color_formats[0] = PixelFormat::R32G32Uint;
            }
            D32S8Rg32Direction::ColorToDepthStencil => {
                render_pass_key.depth_format = PixelFormat::D32FloatS8Uint;
            }
        }
        let render_pass = match render_pass_cache.get(&render_pass_key) {
            Ok(render_pass) => render_pass,
            Err(err) => {
                log::warn!("D32S8 reinterpretation render pass creation failed: {err:?}");
                return false;
            }
        };

        for copy in copies {
            if copy.src_subresource.base_level < 0
                || copy.dst_subresource.base_level < 0
                || copy.src_subresource.base_layer < 0
                || copy.dst_subresource.base_layer < 0
                || copy.src_subresource.num_layers <= 0
                || copy.dst_subresource.num_layers <= 0
                || copy.src_offset.x < 0
                || copy.src_offset.y < 0
                || copy.src_offset.z < 0
                || copy.dst_offset.x < 0
                || copy.dst_offset.y < 0
                || copy.dst_offset.z < 0
                || copy.extent.width == 0
                || copy.extent.height == 0
                || copy.extent.depth == 0
            {
                log::warn!("D32S8 reinterpretation received an invalid copy region");
                return false;
            }
            let src_slices = if src_type == ImageType::E3D {
                copy.extent.depth
            } else {
                copy.src_subresource.num_layers as u32
            };
            let dst_slices = if dst_type == ImageType::E3D {
                copy.extent.depth
            } else {
                copy.dst_subresource.num_layers as u32
            };
            if src_slices != dst_slices {
                log::warn!(
                    "D32S8 reinterpretation layer mismatch: src={} dst={}",
                    src_slices,
                    dst_slices
                );
                return false;
            }

            let src_level = copy.src_subresource.base_level as u32;
            let dst_level = copy.dst_subresource.base_level as u32;
            let src_mip_extent = mip_extent(src_size, src_level);
            let dst_mip_extent = mip_extent(dst_size, dst_level);
            let src_region = Region2D {
                start: Offset2D {
                    x: copy.src_offset.x,
                    y: copy.src_offset.y,
                },
                end: Offset2D {
                    x: copy.src_offset.x + copy.extent.width as i32,
                    y: copy.src_offset.y + copy.extent.height as i32,
                },
            };
            let dst_region = Region2D {
                start: Offset2D {
                    x: copy.dst_offset.x,
                    y: copy.dst_offset.y,
                },
                end: Offset2D {
                    x: copy.dst_offset.x + copy.extent.width as i32,
                    y: copy.dst_offset.y + copy.extent.height as i32,
                },
            };

            for slice in 0..src_slices {
                let src_layer = if src_type == ImageType::E3D {
                    copy.src_offset.z as u32 + slice
                } else {
                    copy.src_subresource.base_layer as u32 + slice
                };
                let dst_layer = if dst_type == ImageType::E3D {
                    copy.dst_offset.z as u32 + slice
                } else {
                    copy.dst_subresource.base_layer as u32 + slice
                };

                let mut views = [vk::ImageView::null(); 3];
                let (src_view, dst_view, dst_aspect) = match direction {
                    D32S8Rg32Direction::DepthStencilToColor => {
                        views[0] = match make_reinterpret_view(
                            &self.device,
                            src_image,
                            vk::Format::D32_SFLOAT_S8_UINT,
                            vk::ImageAspectFlags::DEPTH,
                            src_level,
                            src_layer,
                        ) {
                            Ok(view) => view,
                            Err(err) => {
                                log::warn!("D32S8 depth view creation failed: {err:?}");
                                return false;
                            }
                        };
                        views[1] = match make_reinterpret_view(
                            &self.device,
                            src_image,
                            vk::Format::D32_SFLOAT_S8_UINT,
                            vk::ImageAspectFlags::STENCIL,
                            src_level,
                            src_layer,
                        ) {
                            Ok(view) => view,
                            Err(err) => {
                                unsafe { self.device.destroy_image_view(views[0], None) };
                                log::warn!("D32S8 stencil view creation failed: {err:?}");
                                return false;
                            }
                        };
                        views[2] = match make_reinterpret_view(
                            &self.device,
                            dst_image,
                            vk::Format::R32G32_UINT,
                            vk::ImageAspectFlags::COLOR,
                            dst_level,
                            dst_layer,
                        ) {
                            Ok(view) => view,
                            Err(err) => {
                                unsafe {
                                    self.device.destroy_image_view(views[0], None);
                                    self.device.destroy_image_view(views[1], None);
                                }
                                log::warn!("RG32 target view creation failed: {err:?}");
                                return false;
                            }
                        };
                        (
                            BlitImageView {
                                image: src_image,
                                subresource_range: vk::ImageSubresourceRange {
                                    aspect_mask: vk::ImageAspectFlags::DEPTH
                                        | vk::ImageAspectFlags::STENCIL,
                                    base_mip_level: src_level,
                                    level_count: 1,
                                    base_array_layer: src_layer,
                                    layer_count: 1,
                                },
                                color_view: vk::ImageView::null(),
                                depth_view: views[0],
                                stencil_view: views[1],
                                size: Extent3D {
                                    width: src_mip_extent.width,
                                    height: src_mip_extent.height,
                                    depth: 1,
                                },
                                is_rescaled: false,
                            },
                            views[2],
                            vk::ImageAspectFlags::COLOR,
                        )
                    }
                    D32S8Rg32Direction::ColorToDepthStencil => {
                        views[0] = match make_reinterpret_view(
                            &self.device,
                            src_image,
                            vk::Format::R32G32_UINT,
                            vk::ImageAspectFlags::COLOR,
                            src_level,
                            src_layer,
                        ) {
                            Ok(view) => view,
                            Err(err) => {
                                log::warn!("RG32 source view creation failed: {err:?}");
                                return false;
                            }
                        };
                        views[1] = match make_reinterpret_view(
                            &self.device,
                            dst_image,
                            vk::Format::D32_SFLOAT_S8_UINT,
                            vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL,
                            dst_level,
                            dst_layer,
                        ) {
                            Ok(view) => view,
                            Err(err) => {
                                unsafe { self.device.destroy_image_view(views[0], None) };
                                log::warn!("D32S8 target view creation failed: {err:?}");
                                return false;
                            }
                        };
                        (
                            BlitImageView {
                                image: src_image,
                                subresource_range: vk::ImageSubresourceRange {
                                    aspect_mask: vk::ImageAspectFlags::COLOR,
                                    base_mip_level: src_level,
                                    level_count: 1,
                                    base_array_layer: src_layer,
                                    layer_count: 1,
                                },
                                color_view: views[0],
                                depth_view: vk::ImageView::null(),
                                stencil_view: vk::ImageView::null(),
                                size: Extent3D {
                                    width: src_mip_extent.width,
                                    height: src_mip_extent.height,
                                    depth: 1,
                                },
                                is_rescaled: false,
                            },
                            views[1],
                            vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL,
                        )
                    }
                };

                let attachments = [dst_view];
                let framebuffer_info = vk::FramebufferCreateInfo::builder()
                    .render_pass(render_pass)
                    .attachments(&attachments)
                    .width(dst_mip_extent.width)
                    .height(dst_mip_extent.height)
                    .layers(1)
                    .build();
                let framebuffer = match unsafe {
                    self.device.create_framebuffer(&framebuffer_info, None)
                } {
                    Ok(framebuffer) => framebuffer,
                    Err(err) => {
                        unsafe {
                            for view in views {
                                if view != vk::ImageView::null() {
                                    self.device.destroy_image_view(view, None);
                                }
                            }
                        }
                        log::warn!("D32S8 reinterpretation framebuffer creation failed: {err:?}");
                        return false;
                    }
                };
                let mut images = [vk::Image::null(); NUM_RT + 1];
                images[0] = dst_image;
                let mut image_ranges = [vk::ImageSubresourceRange::default(); NUM_RT + 1];
                image_ranges[0] = vk::ImageSubresourceRange {
                    aspect_mask: dst_aspect,
                    base_mip_level: dst_level,
                    level_count: 1,
                    base_array_layer: dst_layer,
                    layer_count: 1,
                };
                let dst_framebuffer = BlitFramebufferInfo {
                    framebuffer,
                    render_pass,
                    render_area: dst_mip_extent,
                    images,
                    image_ranges,
                    num_images: 1,
                    samples: vk::SampleCountFlags::TYPE_1,
                    has_stencil: dst_aspect.contains(vk::ImageAspectFlags::STENCIL),
                    resolve_shadow_images: [vk::Image::null(); NUM_RT + 1],
                    num_resolve_shadows: 0,
                };
                let converted = match direction {
                    D32S8Rg32Direction::DepthStencilToColor => self.convert_d32s8_to_rg32(
                        dst_framebuffer,
                        src_view,
                        dst_region,
                        src_region,
                    ),
                    D32S8Rg32Direction::ColorToDepthStencil => self.convert_rg32_to_d32s8(
                        dst_framebuffer,
                        src_view,
                        dst_region,
                        src_region,
                    ),
                };
                if !converted {
                    unsafe {
                        self.device.destroy_framebuffer(framebuffer, None);
                        for view in views {
                            if view != vk::ImageView::null() {
                                self.device.destroy_image_view(view, None);
                            }
                        }
                    }
                    return false;
                }
                self.reinterpret_resources.push_back(ReinterpretResources {
                    tick: unsafe { self.scheduler.as_ref() }.current_tick(),
                    views,
                    framebuffer,
                });
            }
        }
        true
    }

    /// Port of `BlitImageHelper::CopyMSAAImpl`: records one full-screen
    /// conversion draw per copy and layer, aliasing the source and destination
    /// subresources through transient views and framebuffers.
    #[allow(clippy::too_many_arguments)]
    fn copy_msaa_impl(
        &mut self,
        renderpass: vk::RenderPass,
        pipeline: vk::Pipeline,
        layout: vk::PipelineLayout,
        dst_image: vk::Image,
        dst_vk_format: vk::Format,
        src_image: vk::Image,
        src_vk_format: vk::Format,
        scale_x: i32,
        scale_y: i32,
        copies: &[ImageCopy],
        aspect_info: &MsaaCopyAspectInfo,
        copy_stencil: bool,
    ) -> bool {
        while self
            .msaa_copy_resources
            .front()
            .is_some_and(|resource| unsafe { self.scheduler.as_ref() }.is_free(resource.tick))
        {
            let resource = self.msaa_copy_resources.pop_front().unwrap();
            self.destroy_msaa_copy_resources(&resource);
        }
        let sampler = self.nearest_sampler;
        for copy in copies {
            let num_layers = copy
                .src_subresource
                .num_layers
                .min(copy.dst_subresource.num_layers);
            for layer in 0..num_layers {
                let src_level = copy.src_subresource.base_level as u32;
                let src_layer = (copy.src_subresource.base_layer + layer) as u32;
                let src_view = match make_msaa_copy_view(
                    &self.device,
                    src_image,
                    src_vk_format,
                    src_level,
                    src_layer,
                    aspect_info.src_view_aspect,
                ) {
                    Ok(view) => view,
                    Err(err) => {
                        log::warn!("BlitImageHelper::CopyMSAA source view failed: {err:?}");
                        return false;
                    }
                };
                let mut src_stencil_view = vk::ImageView::null();
                if copy_stencil {
                    src_stencil_view = match make_msaa_copy_view(
                        &self.device,
                        src_image,
                        src_vk_format,
                        src_level,
                        src_layer,
                        vk::ImageAspectFlags::STENCIL,
                    ) {
                        Ok(view) => view,
                        Err(err) => {
                            unsafe { self.device.destroy_image_view(src_view, None) };
                            log::warn!(
                                "BlitImageHelper::CopyMSAA source stencil view failed: {err:?}"
                            );
                            return false;
                        }
                    };
                }
                let dst_view = match make_msaa_copy_view(
                    &self.device,
                    dst_image,
                    dst_vk_format,
                    copy.dst_subresource.base_level as u32,
                    (copy.dst_subresource.base_layer + layer) as u32,
                    aspect_info.attachment_aspect,
                ) {
                    Ok(view) => view,
                    Err(err) => {
                        unsafe {
                            self.device.destroy_image_view(src_view, None);
                            if src_stencil_view != vk::ImageView::null() {
                                self.device.destroy_image_view(src_stencil_view, None);
                            }
                        }
                        log::warn!("BlitImageHelper::CopyMSAA destination view failed: {err:?}");
                        return false;
                    }
                };
                let dst_offset = vk::Offset2D {
                    x: copy.dst_offset.x,
                    y: copy.dst_offset.y,
                };
                let dst_extent = vk::Extent2D {
                    width: copy.extent.width,
                    height: copy.extent.height,
                };
                let render_area = vk::Rect2D {
                    offset: dst_offset,
                    extent: dst_extent,
                };
                let attachments = [dst_view];
                let framebuffer_info = vk::FramebufferCreateInfo::builder()
                    .render_pass(renderpass)
                    .attachments(&attachments)
                    .width((dst_offset.x as u32).wrapping_add(dst_extent.width))
                    .height((dst_offset.y as u32).wrapping_add(dst_extent.height))
                    .layers(1)
                    .build();
                let framebuffer =
                    match unsafe { self.device.create_framebuffer(&framebuffer_info, None) } {
                        Ok(framebuffer) => framebuffer,
                        Err(err) => {
                            unsafe {
                                self.device.destroy_image_view(src_view, None);
                                if src_stencil_view != vk::ImageView::null() {
                                    self.device.destroy_image_view(src_stencil_view, None);
                                }
                                self.device.destroy_image_view(dst_view, None);
                            }
                            log::warn!(
                                "BlitImageHelper::CopyMSAA framebuffer creation failed: {err:?}"
                            );
                            return false;
                        }
                    };
                let push_constants = MsaaCopyPushConstants {
                    dst_offset: [dst_offset.x, dst_offset.y],
                    src_offset: [copy.src_offset.x, copy.src_offset.y],
                    scale: [scale_x, scale_y],
                };
                let src_stencil_handle = src_stencil_view;
                let device = self.device.clone();
                let one_texture_allocator = self.one_texture_descriptor_allocator.reference();
                let two_textures_allocator = self.two_textures_descriptor_allocator.reference();
                let aspect_info = *aspect_info;
                unsafe { self.scheduler.as_mut() }.request_outside_render_pass_operation_context();
                unsafe { self.scheduler.as_mut() }.record(move |cmdbuf| unsafe {
                    let barrier_range = vk::ImageSubresourceRange {
                        aspect_mask: aspect_info.barrier_aspect,
                        base_mip_level: 0,
                        level_count: vk::REMAINING_MIP_LEVELS,
                        base_array_layer: 0,
                        layer_count: vk::REMAINING_ARRAY_LAYERS,
                    };
                    let pre_barriers = [
                        vk::ImageMemoryBarrier::builder()
                            .src_access_mask(aspect_info.pre_src_access)
                            .dst_access_mask(aspect_info.pre_src_dst_access)
                            .old_layout(vk::ImageLayout::GENERAL)
                            .new_layout(vk::ImageLayout::GENERAL)
                            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                            .image(src_image)
                            .subresource_range(barrier_range)
                            .build(),
                        vk::ImageMemoryBarrier::builder()
                            .src_access_mask(aspect_info.pre_src_access)
                            .dst_access_mask(aspect_info.pre_dst_dst_access)
                            .old_layout(vk::ImageLayout::GENERAL)
                            .new_layout(vk::ImageLayout::GENERAL)
                            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                            .image(dst_image)
                            .subresource_range(barrier_range)
                            .build(),
                    ];
                    device.cmd_pipeline_barrier(
                        cmdbuf,
                        aspect_info.pre_src_stages,
                        aspect_info.pre_dst_stages,
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
                    let descriptor_set = if src_stencil_handle != vk::ImageView::null() {
                        let descriptor_set = two_textures_allocator
                            .commit()
                            .expect("Failed to allocate MSAA copy descriptor set");
                        update_two_textures_descriptor_set(
                            &device,
                            descriptor_set,
                            sampler,
                            src_view,
                            src_stencil_handle,
                        );
                        descriptor_set
                    } else {
                        let descriptor_set = one_texture_allocator
                            .commit()
                            .expect("Failed to allocate MSAA copy descriptor set");
                        update_one_texture_descriptor_set(
                            &device,
                            descriptor_set,
                            sampler,
                            src_view,
                        );
                        descriptor_set
                    };
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
                        .src_access_mask(aspect_info.post_src_access)
                        .dst_access_mask(aspect_info.post_dst_access)
                        .old_layout(vk::ImageLayout::GENERAL)
                        .new_layout(vk::ImageLayout::GENERAL)
                        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .image(dst_image)
                        .subresource_range(barrier_range)
                        .build();
                    device.cmd_pipeline_barrier(
                        cmdbuf,
                        aspect_info.post_src_stages,
                        aspect_info.post_dst_stages,
                        vk::DependencyFlags::empty(),
                        &[],
                        &[],
                        &[post_barrier],
                    );
                });
                let tick = unsafe { self.scheduler.as_ref() }.current_tick();
                self.msaa_copy_resources.push_back(MsaaCopyResources {
                    tick,
                    src_view,
                    dst_view,
                    framebuffer,
                });
                if copy_stencil {
                    self.msaa_copy_resources.push_back(MsaaCopyResources {
                        tick,
                        src_view: src_stencil_view,
                        dst_view: vk::ImageView::null(),
                        framebuffer: vk::Framebuffer::null(),
                    });
                }
            }
        }
        unsafe { self.scheduler.as_mut() }.invalidate_state();
        true
    }

    fn destroy_msaa_copy_resources(&self, resource: &MsaaCopyResources) {
        unsafe {
            if resource.framebuffer != vk::Framebuffer::null() {
                self.device.destroy_framebuffer(resource.framebuffer, None);
            }
            if resource.dst_view != vk::ImageView::null() {
                self.device.destroy_image_view(resource.dst_view, None);
            }
            if resource.src_view != vk::ImageView::null() {
                self.device.destroy_image_view(resource.src_view, None);
            }
        }
    }

    /// Port of `BlitImageHelper::CopyMSAA`: color copies between sample counts
    /// through the MSAA conversion fragment shaders.
    #[allow(clippy::too_many_arguments)]
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
        let (samples_x, samples_y) = samples_log2(num_samples as i32);
        let scale_x = 1_i32 << samples_x;
        let scale_y = 1_i32 << samples_y;
        let mut samples = sample_count_flag(num_samples);
        if msaa_to_non_msaa {
            samples = vk::SampleCountFlags::TYPE_1;
        }
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
            format_class: format_class(dst_format),
        };
        let aspect_info = MsaaCopyAspectInfo {
            src_view_aspect: vk::ImageAspectFlags::COLOR,
            attachment_aspect: vk::ImageAspectFlags::COLOR,
            barrier_aspect: vk::ImageAspectFlags::COLOR,
            pre_src_access: vk::AccessFlags::COLOR_ATTACHMENT_WRITE
                | vk::AccessFlags::SHADER_WRITE
                | vk::AccessFlags::TRANSFER_WRITE,
            pre_src_dst_access: vk::AccessFlags::SHADER_READ,
            pre_dst_dst_access: vk::AccessFlags::COLOR_ATTACHMENT_READ
                | vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            pre_src_stages: vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
                | vk::PipelineStageFlags::COMPUTE_SHADER
                | vk::PipelineStageFlags::FRAGMENT_SHADER
                | vk::PipelineStageFlags::TRANSFER,
            pre_dst_stages: vk::PipelineStageFlags::FRAGMENT_SHADER
                | vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            post_src_access: vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            post_dst_access: vk::AccessFlags::SHADER_READ | vk::AccessFlags::TRANSFER_READ,
            post_src_stages: vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            post_dst_stages: vk::PipelineStageFlags::FRAGMENT_SHADER
                | vk::PipelineStageFlags::COMPUTE_SHADER
                | vk::PipelineStageFlags::TRANSFER,
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
        let layout = self.msaa_copy_pipeline_layout;
        self.copy_msaa_impl(
            renderpass,
            pipeline,
            layout,
            dst_image,
            dst_vk_format,
            src_image,
            src_vk_format,
            scale_x,
            scale_y,
            copies,
            &aspect_info,
            false,
        )
    }

    /// Port of `BlitImageHelper::CopyMSAADepth`: depth (and stencil, with
    /// `VK_EXT_shader_stencil_export`) copies between sample counts through
    /// the depth MSAA conversion fragment shaders.
    #[allow(clippy::too_many_arguments)]
    pub fn copy_msaa_depth(
        &mut self,
        render_pass_cache: &RenderPassCache,
        dst_image: vk::Image,
        dst_format: PixelFormat,
        src_image: vk::Image,
        src_format: PixelFormat,
        num_samples: u32,
        copies: &[ImageCopy],
        copy_stencil: bool,
        msaa_to_non_msaa: bool,
    ) -> bool {
        let (samples_x, samples_y) = samples_log2(num_samples as i32);
        let scale_x = 1_i32 << samples_x;
        let scale_y = 1_i32 << samples_y;
        let mut samples = sample_count_flag(num_samples);
        if msaa_to_non_msaa {
            samples = vk::SampleCountFlags::TYPE_1;
        }
        let renderpass_key = RenderPassKey {
            depth_format: dst_format,
            samples,
            ..RenderPassKey::default()
        };
        let renderpass = match render_pass_cache.get(&renderpass_key) {
            Ok(renderpass) => renderpass,
            Err(err) => {
                log::warn!("BlitImageHelper::CopyMSAADepth render pass creation failed: {err:?}");
                return false;
            }
        };
        let key = MsaaCopyPipelineKey {
            renderpass,
            samples,
            msaa_to_non_msaa,
            format_class: MsaaCopyFormatClass::Float,
        };
        let mut attachment_aspect = vk::ImageAspectFlags::DEPTH;
        if crate::surface::get_format_type(dst_format) == SurfaceType::DepthStencil {
            attachment_aspect |= vk::ImageAspectFlags::STENCIL;
        }
        let mut layout = self.msaa_copy_pipeline_layout;
        if copy_stencil {
            layout = self.msaa_copy_depth_stencil_pipeline_layout;
        }
        let aspect_info = MsaaCopyAspectInfo {
            src_view_aspect: vk::ImageAspectFlags::DEPTH,
            attachment_aspect,
            barrier_aspect: attachment_aspect,
            pre_src_access: vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE
                | vk::AccessFlags::TRANSFER_WRITE,
            pre_src_dst_access: vk::AccessFlags::SHADER_READ,
            pre_dst_dst_access: vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_READ
                | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE,
            pre_src_stages: vk::PipelineStageFlags::EARLY_FRAGMENT_TESTS
                | vk::PipelineStageFlags::LATE_FRAGMENT_TESTS
                | vk::PipelineStageFlags::TRANSFER,
            pre_dst_stages: vk::PipelineStageFlags::FRAGMENT_SHADER
                | vk::PipelineStageFlags::EARLY_FRAGMENT_TESTS,
            post_src_access: vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE,
            post_dst_access: vk::AccessFlags::SHADER_READ
                | vk::AccessFlags::TRANSFER_READ
                | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_READ,
            post_src_stages: vk::PipelineStageFlags::LATE_FRAGMENT_TESTS,
            post_dst_stages: PIPELINE_STAGE_GRAPHICS_COMPUTE_TRANSFER,
        };
        let pipeline = match self.find_or_emplace_msaa_copy_depth_pipeline(&key, copy_stencil) {
            Ok(pipeline) => pipeline,
            Err(err) => {
                log::warn!("BlitImageHelper::CopyMSAADepth pipeline creation failed: {err:?}");
                return false;
            }
        };
        // SAFETY: see `copy_msaa`.
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
        self.copy_msaa_impl(
            renderpass,
            pipeline,
            layout,
            dst_image,
            dst_vk_format,
            src_image,
            src_vk_format,
            scale_x,
            scale_y,
            copies,
            &aspect_info,
            copy_stencil,
        )
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
                .create_graphics_pipelines(
                    unsafe { self.device_owner.as_ref() }.static_pipeline_cache(),
                    &[create_info],
                    None,
                )
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
                .create_graphics_pipelines(
                    unsafe { self.device_owner.as_ref() }.static_pipeline_cache(),
                    &[create_info],
                    None,
                )
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
        let mut fragment_shader = if key.msaa_to_non_msaa {
            self.convert_msaa_to_non_msaa_frag
        } else {
            self.convert_non_msaa_to_msaa_frag
        };
        if key.format_class == MsaaCopyFormatClass::SignedInteger {
            fragment_shader = if key.msaa_to_non_msaa {
                self.convert_msaa_to_non_msaa_sint_frag
            } else {
                self.convert_non_msaa_to_msaa_sint_frag
            };
        } else if key.format_class == MsaaCopyFormatClass::UnsignedInteger {
            fragment_shader = if key.msaa_to_non_msaa {
                self.convert_msaa_to_non_msaa_uint_frag
            } else {
                self.convert_non_msaa_to_msaa_uint_frag
            };
        }
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
                .create_graphics_pipelines(
                    unsafe { self.device_owner.as_ref() }.static_pipeline_cache(),
                    &[create_info],
                    None,
                )
                .map_err(|(_, err)| err)?[0]
        };
        self.msaa_copy_keys.push(*key);
        self.msaa_copy_pipelines.push(pipeline);
        Ok(pipeline)
    }

    /// Port of `BlitImageHelper::FindOrEmplaceMSAACopyDepthPipeline`.
    fn find_or_emplace_msaa_copy_depth_pipeline(
        &mut self,
        key: &MsaaCopyPipelineKey,
        copy_stencil: bool,
    ) -> Result<vk::Pipeline, vk::Result> {
        let (keys, pipelines) = if copy_stencil {
            (
                &mut self.msaa_copy_depth_stencil_keys,
                &mut self.msaa_copy_depth_stencil_pipelines,
            )
        } else {
            (
                &mut self.msaa_copy_depth_keys,
                &mut self.msaa_copy_depth_pipelines,
            )
        };
        if let Some(index) = keys.iter().position(|cached| cached == key) {
            return Ok(pipelines[index]);
        }
        let main = CString::new("main").unwrap();
        let fragment_shader = if key.msaa_to_non_msaa {
            if copy_stencil {
                self.convert_msaa_to_non_msaa_depth_stencil_frag
            } else {
                self.convert_msaa_to_non_msaa_depth_frag
            }
        } else if copy_stencil {
            self.convert_non_msaa_to_msaa_depth_stencil_frag
        } else {
            self.convert_non_msaa_to_msaa_depth_frag
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
        let depth_stencil = msaa_copy_depth_stencil_state(copy_stencil);
        let color_blend = vk::PipelineColorBlendStateCreateInfo::builder().build();
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic_state = vk::PipelineDynamicStateCreateInfo::builder()
            .dynamic_states(&dynamic_states)
            .build();
        let layout = if copy_stencil {
            self.msaa_copy_depth_stencil_pipeline_layout
        } else {
            self.msaa_copy_pipeline_layout
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
            .render_pass(key.renderpass)
            .subpass(0)
            .build();
        let pipeline = unsafe {
            self.device
                .create_graphics_pipelines(
                    self.device_owner.as_ref().static_pipeline_cache(),
                    &[create_info],
                    None,
                )
                .map_err(|(_, err)| err)?[0]
        };
        keys.push(*key);
        pipelines.push(pipeline);
        Ok(pipeline)
    }

    /// Port of `BlitImageHelper::FindOrEmplaceBlitDepthStencilMSAAPipeline`.
    fn find_or_emplace_blit_depth_stencil_msaa_pipeline(
        &mut self,
        key: &BlitMsaaPipelineKey,
        blit_stencil: bool,
    ) -> Result<vk::Pipeline, vk::Result> {
        let (keys, pipelines) = if blit_stencil {
            (
                &mut self.blit_msaa_depth_stencil_keys,
                &mut self.blit_msaa_depth_stencil_pipelines,
            )
        } else {
            (
                &mut self.blit_msaa_depth_keys,
                &mut self.blit_msaa_depth_pipelines,
            )
        };
        if let Some(index) = keys.iter().position(|cached| cached == key) {
            return Ok(pipelines[index]);
        }
        let main = CString::new("main").unwrap();
        let fragment_shader = if blit_stencil {
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
            .rasterization_samples(key.samples)
            .sample_shading_enable(true)
            .min_sample_shading(1.0)
            .build();
        let depth_stencil = if blit_stencil {
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
        let layout = if blit_stencil {
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
            .render_pass(key.renderpass)
            .subpass(0)
            .build();
        let pipeline = unsafe {
            self.device
                .create_graphics_pipelines(
                    self.device_owner.as_ref().static_pipeline_cache(),
                    &[create_info],
                    None,
                )
                .map_err(|(_, err)| err)?[0]
        };
        keys.push(*key);
        pipelines.push(pipeline);
        Ok(pipeline)
    }

    /// Port of `BlitImageHelper::FindOrEmplaceBlitDepthPipeline`.
    fn find_or_emplace_blit_depth_pipeline(
        &mut self,
        renderpass: vk::RenderPass,
    ) -> Result<vk::Pipeline, vk::Result> {
        if let Some(index) = self
            .blit_depth_keys
            .iter()
            .position(|&cached| cached == renderpass)
        {
            return Ok(self.blit_depth_pipelines[index]);
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
                .module(self.blit_depth_frag)
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
        let depth_stencil = pipeline_depth_only_state();
        let color_blend = vk::PipelineColorBlendStateCreateInfo::builder().build();
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
            .depth_stencil_state(&depth_stencil)
            .color_blend_state(&color_blend)
            .dynamic_state(&dynamic_state)
            .layout(self.one_texture_pipeline_layout)
            .render_pass(renderpass)
            .subpass(0)
            .build();
        let pipeline = unsafe {
            self.device
                .create_graphics_pipelines(
                    self.device_owner.as_ref().static_pipeline_cache(),
                    &[create_info],
                    None,
                )
                .map_err(|(_, err)| err)?[0]
        };
        self.blit_depth_keys.push(renderpass);
        self.blit_depth_pipelines.push(pipeline);
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
                .create_graphics_pipelines(
                    unsafe { self.device_owner.as_ref() }.static_pipeline_cache(),
                    &[pipeline_info],
                    None,
                )
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
                .create_graphics_pipelines(
                    unsafe { self.device_owner.as_ref() }.static_pipeline_cache(),
                    &[pipeline_info],
                    None,
                )
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
                .create_graphics_pipelines(
                    unsafe { self.device_owner.as_ref() }.static_pipeline_cache(),
                    &[pipeline_info],
                    None,
                )
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
                .create_graphics_pipelines(
                    unsafe { self.device_owner.as_ref() }.static_pipeline_cache(),
                    &[pipeline_info],
                    None,
                )
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
                .create_graphics_pipelines(
                    unsafe { self.device_owner.as_ref() }.static_pipeline_cache(),
                    &[create_info],
                    None,
                )
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
                .create_graphics_pipelines(
                    unsafe { self.device_owner.as_ref() }.static_pipeline_cache(),
                    &[create_info],
                    None,
                )
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
                &mut self.convert_rg32_to_d32s8_pipeline,
                &mut self.convert_d32s8_to_rg32_pipeline,
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

            for resource in self.reinterpret_resources.drain(..) {
                self.device.destroy_framebuffer(resource.framebuffer, None);
                for view in resource.views {
                    if view != vk::ImageView::null() {
                        self.device.destroy_image_view(view, None);
                    }
                }
            }

            let msaa_copy_resources: Vec<MsaaCopyResources> =
                self.msaa_copy_resources.drain(..).collect();
            for resource in &msaa_copy_resources {
                self.destroy_msaa_copy_resources(resource);
            }

            for pipeline in self
                .resolve_depth_stencil_pipelines
                .iter_mut()
                .chain(self.resolve_depth_pipelines.iter_mut())
                .chain(self.blit_msaa_depth_stencil_pipelines.iter_mut())
                .chain(self.blit_msaa_depth_pipelines.iter_mut())
                .chain(self.blit_depth_pipelines.iter_mut())
                .chain(self.blit_msaa_color_pipelines.iter_mut())
                .chain(self.msaa_copy_depth_stencil_pipelines.iter_mut())
                .chain(self.msaa_copy_depth_pipelines.iter_mut())
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
                &mut self.convert_rg32_to_d32s8_frag,
                &mut self.convert_d32s8_to_rg32_frag,
                &mut self.convert_non_msaa_to_msaa_depth_stencil_frag,
                &mut self.convert_non_msaa_to_msaa_depth_frag,
                &mut self.convert_non_msaa_to_msaa_uint_frag,
                &mut self.convert_non_msaa_to_msaa_sint_frag,
                &mut self.convert_non_msaa_to_msaa_frag,
                &mut self.convert_msaa_to_non_msaa_depth_stencil_frag,
                &mut self.convert_msaa_to_non_msaa_depth_frag,
                &mut self.convert_msaa_to_non_msaa_uint_frag,
                &mut self.convert_msaa_to_non_msaa_sint_frag,
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
                &mut self.blit_depth_frag,
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

            if self.msaa_copy_depth_stencil_pipeline_layout != vk::PipelineLayout::null() {
                self.device
                    .destroy_pipeline_layout(self.msaa_copy_depth_stencil_pipeline_layout, None);
                self.msaa_copy_depth_stencil_pipeline_layout = vk::PipelineLayout::null();
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
    fn msaa_copy_format_class_follows_upstream_integer_classification() {
        assert_eq!(
            format_class(PixelFormat::A8B8G8R8Unorm),
            MsaaCopyFormatClass::Float
        );
        assert_eq!(
            format_class(PixelFormat::R16G16B16A16Float),
            MsaaCopyFormatClass::Float
        );
        assert_eq!(
            format_class(PixelFormat::A8B8G8R8Sint),
            MsaaCopyFormatClass::SignedInteger
        );
        assert_eq!(
            format_class(PixelFormat::R32Sint),
            MsaaCopyFormatClass::SignedInteger
        );
        assert_eq!(
            format_class(PixelFormat::A8B8G8R8Uint),
            MsaaCopyFormatClass::UnsignedInteger
        );
        assert_eq!(
            format_class(PixelFormat::R32G32B32A32Uint),
            MsaaCopyFormatClass::UnsignedInteger
        );
    }

    #[test]
    fn msaa_copy_depth_stencil_state_replaces_stencil_only_when_copied() {
        let with_stencil = msaa_copy_depth_stencil_state(true);
        assert_eq!(with_stencil.depth_test_enable, vk::TRUE);
        assert_eq!(with_stencil.depth_write_enable, vk::TRUE);
        assert_eq!(with_stencil.depth_compare_op, vk::CompareOp::ALWAYS);
        assert_eq!(with_stencil.stencil_test_enable, vk::TRUE);
        assert_eq!(with_stencil.front.fail_op, vk::StencilOp::REPLACE);
        assert_eq!(with_stencil.front.depth_fail_op, vk::StencilOp::REPLACE);
        assert_eq!(with_stencil.front.compare_mask, 0xFF);
        assert_eq!(with_stencil.front.write_mask, 0xFF);
        let depth_only = msaa_copy_depth_stencil_state(false);
        assert_eq!(depth_only.stencil_test_enable, vk::FALSE);
        assert_eq!(depth_only.front.write_mask, 0);
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

    #[test]
    fn d32s8_rg32_reinterpretation_is_selected_only_for_the_raw_64_bit_pair() {
        assert_eq!(
            d32s8_rg32_direction(PixelFormat::R32G32Float, PixelFormat::D32FloatS8Uint),
            Some(D32S8Rg32Direction::DepthStencilToColor)
        );
        assert_eq!(
            d32s8_rg32_direction(PixelFormat::D32FloatS8Uint, PixelFormat::R32G32Float),
            Some(D32S8Rg32Direction::ColorToDepthStencil)
        );
        assert_eq!(
            d32s8_rg32_direction(PixelFormat::R32Float, PixelFormat::D32FloatS8Uint),
            None
        );
        assert!(crate::compatible_formats::is_view_compatible(
            PixelFormat::R32G32Float,
            PixelFormat::R32G32Uint,
            false,
            true,
        ));
    }

    #[test]
    fn d32s8_rg32_raw_words_preserve_depth_bits_and_stencil_byte() {
        for (depth_bits, stencil) in [
            (0x0000_0000_u32, 0_u8),
            (0x3f80_0000, 1),
            (0x3f00_0000, 0x7f),
            (0x3f7f_ffff, 0xff),
        ] {
            // These are the exact integer operations performed by the two
            // conversion shaders around the RG32_UINT attachment view.
            let rg_words = [depth_bits, u32::from(stencil)];
            let restored_depth = rg_words[0];
            let restored_stencil = (rg_words[1] & 0xff) as u8;
            assert_eq!(restored_depth, depth_bits);
            assert_eq!(restored_stencil, stencil);
        }
    }

    #[test]
    fn reinterpretation_mip_extent_never_reaches_zero() {
        let size = Extent3D {
            width: 17,
            height: 9,
            depth: 1,
        };
        assert_eq!(
            mip_extent(size, 0),
            vk::Extent2D {
                width: 17,
                height: 9
            }
        );
        assert_eq!(
            mip_extent(size, 4),
            vk::Extent2D {
                width: 1,
                height: 1
            }
        );
        assert_eq!(
            mip_extent(size, 12),
            vk::Extent2D {
                width: 1,
                height: 1
            }
        );
    }
}
