// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! GPU texture cache — images, views, samplers, framebuffers.
//!
//! Ref: zuyu `vk_texture_cache.h` — caches VkImage/VkImageView/VkSampler
//! objects and VkFramebuffer objects by render target configuration.

use std::collections::HashMap;
use std::ptr::NonNull;
use std::sync::Arc;

use ash::vk;
use common::hash::BuildUnorderedDenseHasher;
use smallvec::SmallVec;

use crate::control::channel_state::ChannelState;
use crate::engines::draw_manager::Maxwell3DAccess;
use crate::engines::fermi_2d::{Filter as BlitFilter, Operation as BlitOperation};
use crate::engines::maxwell_3d::Maxwell3D;
use crate::engines::maxwell_dma::dma;
use crate::framebuffer_config::FramebufferConfig;
use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
use crate::surface::{PixelFormat, SurfaceType};
use crate::texture_cache::image_base::{ImageBase, ImageFlagBits};
use crate::texture_cache::image_info::ImageInfo;
use crate::texture_cache::image_view_base::{ImageViewBase, ImageViewFlagBits};
use crate::texture_cache::image_view_info::ImageViewInfo;
use crate::texture_cache::render_targets::RenderTargets;
use crate::texture_cache::texture_cache::RenderTargetDirtyFlagAccess;
use crate::texture_cache::texture_cache_base::{
    BufferDownload, FramebufferImageView, ImageViewInOut, PendingDownload,
    TextureCacheBase as CommonTextureCache,
};
use crate::texture_cache::types::{
    BufferImageCopy, Extent2D, Extent3D, FramebufferId, ImageCopy, ImageId, ImageType, ImageViewId,
    ImageViewType, Offset3D, RelaxedOptions, SamplerId, SubresourceExtent, SubresourceRange,
    NULL_IMAGE_ID, NULL_IMAGE_VIEW_ID, NULL_SAMPLER_ID, NUM_RT,
};
use crate::texture_cache::util::full_download_copies;
#[cfg(test)]
use crate::texture_cache::util::make_shrink_image_copies;
use crate::textures::texture::{
    SamplerReduction, TextureFilter, TextureMipmapFilter, TscEntry, WrapMode,
};
use shader_recompiler::shader_info::{ImageFormat, TextureType};

use super::blit_image::{
    subresource_range_from_view, BlitFramebufferInfo, BlitImageHelper, BlitImageView,
    Extent3D as BlitExtent3D, Offset2D as BlitOffset2D, Region2D as BlitRegion2D,
};
use super::compute_pass::{AstcDecoderPass, BlockLinearUnswizzle3DPass};
use super::descriptor_pool::DescriptorPool;
use super::maxwell_to_vk;
use super::render_pass_cache::{RenderPassCache, RenderPassKey};
use super::scheduler::Scheduler;
use super::staging_buffer_pool::{StagingBufferPool, StagingBufferRef};
use super::update_descriptor::ComputePassDescriptorQueue;
use crate::vulkan_common::vulkan_device::{
    query_device_memory_info, query_device_memory_usage, Device, DeviceMemoryInfo, FormatType,
};
use crate::vulkan_common::vulkan_memory_allocator::{
    AllocatedBuffer, AllocatedImage, MemoryAllocator, MemoryUsage,
};
use crate::vulkan_common::vulkan_wrapper::{
    PIPELINE_STAGE_GRAPHICS_COMPUTE, PIPELINE_STAGE_GRAPHICS_COMPUTE_TRANSFER,
};

const ENABLE_MSAA_RESOLVE_CONSUME: bool = true;
const ENABLE_MSAA_COLOR_DISCARD: bool = true;

fn assert_fail_soft(condition: bool, message: &str) {
    if condition {
        return;
    }
    log::error!("TextureCacheVulkan: {message}");
    if *common::settings::values().use_debug_asserts.get_value() {
        panic!("TextureCacheVulkan: {message}");
    }
}

/// Rust adapter for the draw-scoped dirty copy used by the Vulkan command
/// path. Upstream reads and writes the one live `maxwell3d->dirty.flags`
/// array. Keep the draw copy and that live owner synchronized so mutations
/// made by `DeleteImage`/`InvalidateScale` are visible to the next iteration
/// of `RescaleRenderTargets` in the same call.
struct VulkanRenderTargetDirtyFlags<'a> {
    draw_flags: &'a mut [bool; 256],
    maxwell3d: Option<NonNull<Maxwell3D>>,
}

impl RenderTargetDirtyFlagAccess for VulkanRenderTargetDirtyFlags<'_> {
    fn render_target_dirty_flag(&self, flag: u8) -> bool {
        self.draw_flags[flag as usize]
            || self.maxwell3d.is_some_and(|maxwell3d| {
                // SAFETY: the Vulkan renderer serializes the draw and texture
                // cache on the GPU thread; this is the same stable non-owning
                // engine pointer stored by upstream `ChannelSetupCaches`.
                unsafe { maxwell3d.as_ref().dirty_flag(flag) }
            })
    }

    fn clear_render_target_dirty_flag(&mut self, flag: u8) {
        self.draw_flags[flag as usize] = false;
        if let Some(mut maxwell3d) = self.maxwell3d {
            // SAFETY: see `render_target_dirty_flag`; mutation is serialized
            // for the duration of the render-target update.
            unsafe { maxwell3d.as_mut().clear_dirty_flag(flag) };
        }
    }

    fn set_render_target_dirty_flag(&mut self, flag: u8) {
        self.draw_flags[flag as usize] = true;
        if let Some(mut maxwell3d) = self.maxwell3d {
            // SAFETY: see `render_target_dirty_flag`; mutation is serialized
            // for the duration of the render-target update.
            unsafe { maxwell3d.as_mut().set_dirty_flag(flag) };
        }
    }
}

fn convert_border_color(color: [f32; 4]) -> vk::BorderColor {
    if color == [0.0, 0.0, 0.0, 0.0] {
        vk::BorderColor::FLOAT_TRANSPARENT_BLACK
    } else if color == [0.0, 0.0, 0.0, 1.0] {
        vk::BorderColor::FLOAT_OPAQUE_BLACK
    } else if color == [1.0, 1.0, 1.0, 1.0] {
        vk::BorderColor::FLOAT_OPAQUE_WHITE
    } else if color[0] + color[1] + color[2] > 1.35 {
        vk::BorderColor::FLOAT_OPAQUE_WHITE
    } else if color[3] > 0.5 {
        vk::BorderColor::FLOAT_OPAQUE_BLACK
    } else {
        vk::BorderColor::FLOAT_TRANSPARENT_BLACK
    }
}

fn sampler_reduction_from_raw(value: u32) -> SamplerReduction {
    match value {
        0 => SamplerReduction::WeightedAverage,
        1 => SamplerReduction::Min,
        2 => SamplerReduction::Max,
        value => panic!("invalid Maxwell sampler reduction mode {value}"),
    }
}

/// Backend-owned Vulkan image.
///
/// Port-facing counterpart of upstream `Vulkan::Image`. The common
/// `ImageBase` slot remains the source of truth in `TextureCacheBase`; this
/// backend owner materializes the Vulkan image, memory, current image handle,
/// aspect mask, lazy storage views and initialization/layout state.
pub struct Image {
    runtime: Option<NonNull<TextureCacheRuntime>>,
    base: NonNull<ImageBase>,
    pub original_image: AllocatedImage,
    pub current_image: vk::Image,
    pub scaled_image: Option<AllocatedImage>,
    pub storage_image_views: Vec<vk::ImageView>,
    pub format: vk::Format,
    pub aspect: vk::ImageAspectFlags,
    pub initialized: bool,
    pub layout: vk::ImageLayout,
    pub scale_view: vk::ImageView,
    pub normal_view: vk::ImageView,
    pub scale_depth_view: vk::ImageView,
    pub normal_depth_view: vk::ImageView,
    pub scale_stencil_view: vk::ImageView,
    pub normal_stencil_view: vk::ImageView,
    pub scale_framebuffer: Option<BlitFramebufferInfo>,
    pub normal_framebuffer: Option<BlitFramebufferInfo>,
    pub compute_unswizzle_buffer: Option<AllocatedBuffer>,
    pub compute_unswizzle_buffer_size: vk::DeviceSize,
    pub allocation_tick: u64,
}

impl Image {
    fn new(
        runtime: &mut TextureCacheRuntime,
        _image_id: ImageId,
        mut base: NonNull<ImageBase>,
    ) -> Result<Self, vk::Result> {
        // SAFETY: the generic cache exclusively owns the newly inserted slot.
        let base_mut = unsafe { base.as_mut() };
        TextureCache::apply_backend_image_flags_with_capabilities(
            base_mut,
            runtime.optimal_astc_supported,
            runtime.optimal_bcn_supported,
            *common::settings::values().accelerate_astc.get_value(),
            *common::settings::values().astc_recompression.get_value(),
        );
        let format = runtime.surface_format(base_mut.info.format, false);
        let aspect = image_aspect_mask(base_mut.info.format);
        let image = runtime.create_image_from_info(&base_mut.info)?;
        let image_handle = image.handle();
        Ok(Self {
            runtime: Some(NonNull::from(&mut *runtime)),
            base,
            original_image: image,
            current_image: image_handle,
            scaled_image: None,
            storage_image_views: vec![
                vk::ImageView::null();
                base_mut.info.resources.levels.max(1) as usize
            ],
            format,
            aspect,
            initialized: false,
            layout: vk::ImageLayout::UNDEFINED,
            scale_view: vk::ImageView::null(),
            normal_view: vk::ImageView::null(),
            scale_depth_view: vk::ImageView::null(),
            normal_depth_view: vk::ImageView::null(),
            scale_stencil_view: vk::ImageView::null(),
            normal_stencil_view: vk::ImageView::null(),
            scale_framebuffer: None,
            normal_framebuffer: None,
            compute_unswizzle_buffer: None,
            compute_unswizzle_buffer_size: 0,
            allocation_tick: 0,
        })
    }

    fn base(&self) -> &ImageBase {
        // SAFETY: the pointer targets the boxed base of the typed slot which
        // owns this payload. The allocation remains stable while the slot
        // moves through `SlotVector` and delayed-destruction rings.
        unsafe { self.base.as_ref() }
    }

    fn base_mut(&mut self) -> &mut ImageBase {
        // SAFETY: backend operations take the payload out of its slot before
        // mutating the inherited base, so no competing Rust reference exists.
        unsafe { self.base.as_mut() }
    }

    fn destroy_runtime_resources(&mut self, runtime: &mut TextureCacheRuntime) {
        if ENABLE_MSAA_RESOLVE_CONSUME {
            runtime.erase_resolve_shadow(self.original_image.handle());
            if let Some(scaled_image) = self.scaled_image.as_ref() {
                runtime.erase_resolve_shadow(scaled_image.handle());
            }
        }
        unsafe {
            for view in self.storage_image_views.drain(..) {
                if view != vk::ImageView::null() {
                    runtime.device.destroy_image_view(view, None);
                }
            }
            for view in [
                self.scale_view,
                self.normal_view,
                self.scale_depth_view,
                self.normal_depth_view,
                self.scale_stencil_view,
                self.normal_stencil_view,
            ] {
                if view != vk::ImageView::null() {
                    runtime.device.destroy_image_view(view, None);
                }
            }
            for framebuffer in [
                self.scale_framebuffer.take(),
                self.normal_framebuffer.take(),
            ]
            .into_iter()
            .flatten()
            {
                runtime
                    .device
                    .destroy_framebuffer(framebuffer.framebuffer, None);
            }
        }
        self.runtime = None;
    }

    fn handle(&self) -> vk::Image {
        self.current_image
    }

    fn aspect_mask(&self) -> vk::ImageAspectFlags {
        self.aspect
    }

    fn exchange_initialization(&mut self) -> bool {
        std::mem::replace(&mut self.initialized, true)
    }

    fn is_rescaled(&self) -> bool {
        self.base().flags.contains(ImageFlagBits::RESCALED)
    }

    fn blit_image_view(
        &self,
        color_view: vk::ImageView,
        depth_view: vk::ImageView,
        stencil_view: vk::ImageView,
    ) -> BlitImageView {
        BlitImageView {
            image: self.handle(),
            subresource_range: vk::ImageSubresourceRange {
                aspect_mask: self.aspect,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            },
            color_view,
            depth_view,
            stencil_view,
            size: BlitExtent3D {
                width: self.base().info.size.width,
                height: self.base().info.size.height,
                depth: self.base().info.size.depth,
            },
            is_rescaled: self.is_rescaled(),
        }
    }

    /// Port of `Vulkan::Image::AllocateComputeUnswizzleBuffer`.
    fn allocate_compute_unswizzle_buffer(
        &mut self,
        runtime: &mut TextureCacheRuntime,
        max_slices: u32,
    ) -> bool {
        let block_bytes = crate::surface::bytes_per_block(self.base().info.format) as u64;
        let blocks_x = u64::from(self.base().info.size.width.div_ceil(4));
        let blocks_y = u64::from(self.base().info.size.height.div_ceil(4));
        let blocks_z = u64::from(max_slices.min(self.base().info.size.depth));
        let required_size = blocks_x * blocks_y * blocks_z * block_bytes;
        if self.compute_unswizzle_buffer.is_some()
            && required_size <= self.compute_unswizzle_buffer_size
        {
            return true;
        }
        let create_info = vk::BufferCreateInfo::builder()
            .size(required_size.max(1))
            .usage(vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::TRANSFER_SRC)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .build();
        let buffer = match runtime
            .memory_allocator()
            .create_buffer(&create_info, MemoryUsage::DeviceLocal)
        {
            Ok(buffer) => buffer,
            Err(err) => {
                log::warn!(
                    "Image::allocate_compute_unswizzle_buffer failed size={} err={:?}",
                    required_size,
                    err
                );
                return false;
            }
        };
        self.compute_unswizzle_buffer = Some(buffer);
        self.compute_unswizzle_buffer_size = required_size.max(1);
        true
    }

    fn blit_scale_helper_color(
        &mut self,
        runtime: &mut TextureCacheRuntime,
        scale_up: bool,
    ) -> bool {
        let view = if scale_up {
            self.scale_view
        } else {
            self.normal_view
        };
        if view == vk::ImageView::null() {
            let view = match runtime.create_blit_color_view(self.handle(), self.format) {
                Ok(view) => view,
                Err(err) => {
                    log::warn!(
                        "TextureCacheVulkan: failed to create BlitScaleHelper color view gpu=0x{:X} err={:?}",
                        self.base().gpu_addr,
                        err
                    );
                    return false;
                }
            };
            if scale_up {
                self.scale_view = view;
            } else {
                self.normal_view = view;
            }
        }

        let is_2d = self.base().info.image_type == ImageType::E2D;
        let scaled_width = runtime.resolution.scale_up_u32(self.base().info.size.width);
        let scaled_height = if is_2d {
            runtime
                .resolution
                .scale_up_u32(self.base().info.size.height)
        } else {
            self.base().info.size.height
        };
        let extent = vk::Extent2D {
            width: scaled_width.max(self.base().info.size.width),
            height: scaled_height.max(self.base().info.size.height),
        };
        let (samples_x, samples_y) =
            crate::texture_cache::samples_helper::samples_log2(self.base().info.num_samples as i32);
        let extent = vk::Extent2D {
            width: extent.width >> samples_x,
            height: extent.height >> samples_y,
        };

        let (view, framebuffer) = if scale_up {
            (self.scale_view, self.scale_framebuffer)
        } else {
            (self.normal_view, self.normal_framebuffer)
        };
        if framebuffer.is_none() {
            let framebuffer = match runtime.create_blit_color_framebuffer(
                self.handle(),
                view,
                self.base().info.format,
                extent,
                convert_sample_count(self.base().info.num_samples),
            ) {
                Ok(framebuffer) => framebuffer,
                Err(err) => {
                    log::warn!(
                            "TextureCacheVulkan: failed to create BlitScaleHelper color framebuffer gpu=0x{:X} err={:?}",
                            self.base().gpu_addr,
                            err
                        );
                    return false;
                }
            };
            if scale_up {
                self.scale_framebuffer = Some(framebuffer);
            } else {
                self.normal_framebuffer = Some(framebuffer);
            }
        }

        let (view, framebuffer) = if scale_up {
            (self.scale_view, self.scale_framebuffer)
        } else {
            (self.normal_view, self.normal_framebuffer)
        };
        let Some(framebuffer) = framebuffer else {
            return false;
        };
        let src_width = (if scale_up {
            self.base().info.size.width
        } else {
            scaled_width
        }) >> samples_x;
        let src_height = (if scale_up {
            self.base().info.size.height
        } else {
            scaled_height
        }) >> samples_y;
        let dst_width = (if scale_up {
            scaled_width
        } else {
            self.base().info.size.width
        }) >> samples_x;
        let dst_height = (if scale_up {
            scaled_height
        } else {
            self.base().info.size.height
        }) >> samples_y;
        let src_region = BlitRegion2D {
            start: BlitOffset2D { x: 0, y: 0 },
            end: BlitOffset2D {
                x: src_width as i32,
                y: src_height as i32,
            },
        };
        let dst_region = BlitRegion2D {
            start: BlitOffset2D { x: 0, y: 0 },
            end: BlitOffset2D {
                x: dst_width as i32,
                y: dst_height as i32,
            },
        };
        let filter = if !crate::surface::is_pixel_format_integer(self.base().info.format) {
            BlitFilter::Bilinear
        } else {
            BlitFilter::Point
        };
        let src_image_view =
            self.blit_image_view(view, vk::ImageView::null(), vk::ImageView::null());
        if self.base().info.num_samples > 1 {
            runtime.blit_image_helper().blit_color_msaa(
                framebuffer,
                src_image_view,
                &dst_region,
                &src_region,
            )
        } else {
            runtime.blit_image_helper().blit_color(
                framebuffer,
                src_image_view,
                &dst_region,
                &src_region,
                filter,
                BlitOperation::SrcCopy,
            )
        }
    }

    fn blit_scale_helper_depth_stencil(
        &mut self,
        runtime: &mut TextureCacheRuntime,
        scale_up: bool,
    ) -> bool {
        let view = if scale_up {
            self.scale_view
        } else {
            self.normal_view
        };
        if view == vk::ImageView::null() {
            let render_target_view = match runtime.create_blit_image_view(
                self.handle(),
                self.format,
                vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL,
            ) {
                Ok(view) => view,
                Err(err) => {
                    log::warn!(
                        "TextureCacheVulkan: failed to create BlitScaleHelper depth/stencil target view gpu=0x{:X} err={:?}",
                        self.base().gpu_addr,
                        err
                    );
                    return false;
                }
            };
            let depth_view = match runtime.create_blit_image_view(
                self.handle(),
                self.format,
                vk::ImageAspectFlags::DEPTH,
            ) {
                Ok(view) => view,
                Err(err) => {
                    unsafe {
                        runtime
                            .device()
                            .destroy_image_view(render_target_view, None);
                    }
                    log::warn!(
                        "TextureCacheVulkan: failed to create BlitScaleHelper depth view gpu=0x{:X} err={:?}",
                        self.base().gpu_addr,
                        err
                    );
                    return false;
                }
            };
            let stencil_view = match runtime.create_blit_image_view(
                self.handle(),
                self.format,
                vk::ImageAspectFlags::STENCIL,
            ) {
                Ok(view) => view,
                Err(err) => {
                    unsafe {
                        runtime
                            .device()
                            .destroy_image_view(render_target_view, None);
                        runtime.device().destroy_image_view(depth_view, None);
                    }
                    log::warn!(
                        "TextureCacheVulkan: failed to create BlitScaleHelper stencil view gpu=0x{:X} err={:?}",
                        self.base().gpu_addr,
                        err
                    );
                    return false;
                }
            };
            if scale_up {
                self.scale_view = render_target_view;
                self.scale_depth_view = depth_view;
                self.scale_stencil_view = stencil_view;
            } else {
                self.normal_view = render_target_view;
                self.normal_depth_view = depth_view;
                self.normal_stencil_view = stencil_view;
            }
        }

        let is_2d = self.base().info.image_type == ImageType::E2D;
        let scaled_width = runtime.resolution.scale_up_u32(self.base().info.size.width);
        let scaled_height = if is_2d {
            runtime
                .resolution
                .scale_up_u32(self.base().info.size.height)
        } else {
            self.base().info.size.height
        };
        let extent = vk::Extent2D {
            width: scaled_width.max(self.base().info.size.width),
            height: scaled_height.max(self.base().info.size.height),
        };
        let (view, framebuffer) = if scale_up {
            (self.scale_view, self.scale_framebuffer)
        } else {
            (self.normal_view, self.normal_framebuffer)
        };
        if framebuffer.is_none() {
            let framebuffer = match runtime.create_blit_depth_stencil_framebuffer(
                self.handle(),
                view,
                self.base().info.format,
                extent,
            ) {
                Ok(framebuffer) => framebuffer,
                Err(err) => {
                    log::warn!(
                        "TextureCacheVulkan: failed to create BlitScaleHelper depth/stencil framebuffer gpu=0x{:X} err={:?}",
                        self.base().gpu_addr,
                        err
                    );
                    return false;
                }
            };
            if scale_up {
                self.scale_framebuffer = Some(framebuffer);
            } else {
                self.normal_framebuffer = Some(framebuffer);
            }
        }

        let (depth_view, stencil_view, framebuffer) = if scale_up {
            (
                self.scale_depth_view,
                self.scale_stencil_view,
                self.scale_framebuffer,
            )
        } else {
            (
                self.normal_depth_view,
                self.normal_stencil_view,
                self.normal_framebuffer,
            )
        };
        let Some(framebuffer) = framebuffer else {
            return false;
        };
        let src_width = if scale_up {
            self.base().info.size.width
        } else {
            scaled_width
        };
        let src_height = if scale_up {
            self.base().info.size.height
        } else {
            scaled_height
        };
        let dst_width = if scale_up {
            scaled_width
        } else {
            self.base().info.size.width
        };
        let dst_height = if scale_up {
            scaled_height
        } else {
            self.base().info.size.height
        };
        let src_region = BlitRegion2D {
            start: BlitOffset2D { x: 0, y: 0 },
            end: BlitOffset2D {
                x: src_width as i32,
                y: src_height as i32,
            },
        };
        let dst_region = BlitRegion2D {
            start: BlitOffset2D { x: 0, y: 0 },
            end: BlitOffset2D {
                x: dst_width as i32,
                y: dst_height as i32,
            },
        };
        let src_image_view = self.blit_image_view(vk::ImageView::null(), depth_view, stencil_view);
        runtime.blit_image_helper().blit_depth_stencil(
            framebuffer,
            src_image_view,
            &dst_region,
            &src_region,
            BlitFilter::Point,
            BlitOperation::SrcCopy,
        )
    }

    fn scale_up(&mut self, runtime: &mut TextureCacheRuntime, mut ignore: bool) -> bool {
        if !runtime.resolution.active {
            return false;
        }
        if self.base().flags.contains(ImageFlagBits::RESCALED) {
            return false;
        }
        if self.base().info.image_type == ImageType::Linear {
            return false;
        }
        self.base_mut().flags.insert(ImageFlagBits::RESCALED);
        self.base_mut().has_scaled = true;
        if self.scaled_image.is_none() {
            let scaled_image = match runtime.create_scaled_image(&self.base().info, self.format) {
                Ok(image) => image,
                Err(err) => {
                    log::warn!(
                        "TextureCacheVulkan: failed to create scaled image gpu=0x{:X} err={:?}",
                        self.base().gpu_addr,
                        err
                    );
                    self.base_mut().flags.remove(ImageFlagBits::RESCALED);
                    return false;
                }
            };
            self.scaled_image = Some(scaled_image);
            ignore = false;
        }
        let Some(scaled) = self.scaled_image.as_ref() else {
            self.base_mut().flags.remove(ImageFlagBits::RESCALED);
            return false;
        };
        self.current_image = scaled.handle();
        self.layout = vk::ImageLayout::GENERAL;
        if ignore {
            return true;
        }
        if runtime.needs_scale_helper(&self.base().info, self.format) {
            if self.aspect == vk::ImageAspectFlags::COLOR {
                return self.blit_scale_helper_color(runtime, true);
            }
            if self.aspect == (vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL)
                && self.base().info.num_samples == 1
            {
                return self.blit_scale_helper_depth_stencil(runtime, true);
            }
            log::warn!(
                "TextureCacheVulkan: ScaleUp needs unsupported BlitScaleHelper aspect for image gpu=0x{:X} format={:?}",
                self.base().gpu_addr,
                self.base().info.format
            );
            self.base_mut().flags.remove(ImageFlagBits::RESCALED);
            return false;
        }
        runtime.blit_scale(
            self.original_image.handle(),
            scaled.handle(),
            self.base().info.clone(),
            self.aspect,
            true,
        );
        true
    }

    fn scale_down(&mut self, runtime: &mut TextureCacheRuntime, ignore: bool) -> bool {
        if !runtime.resolution.active {
            return false;
        }
        if !self.base().flags.contains(ImageFlagBits::RESCALED) {
            return false;
        }
        if self.base().info.image_type == ImageType::Linear {
            return false;
        }
        if self.scaled_image.is_none() {
            return false;
        }
        let scaled = self.scaled_image.as_ref().unwrap().handle();
        self.base_mut().flags.remove(ImageFlagBits::RESCALED);
        self.current_image = self.original_image.handle();
        self.layout = vk::ImageLayout::GENERAL;
        if ignore {
            return true;
        }
        if runtime.needs_scale_helper(&self.base().info, self.format) {
            if self.aspect == vk::ImageAspectFlags::COLOR {
                return self.blit_scale_helper_color(runtime, false);
            }
            if self.aspect == (vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL)
                && self.base().info.num_samples == 1
            {
                return self.blit_scale_helper_depth_stencil(runtime, false);
            }
            log::warn!(
                "TextureCacheVulkan: ScaleDown needs unsupported BlitScaleHelper aspect for image gpu=0x{:X} format={:?}",
                self.base().gpu_addr,
                self.base().info.format
            );
            self.base_mut().flags.remove(ImageFlagBits::RESCALED);
            return false;
        }
        runtime.blit_scale(
            scaled,
            self.original_image.handle(),
            self.base().info.clone(),
            self.aspect,
            false,
        );
        true
    }

    /// Port-facing counterpart of upstream `Vulkan::Image::UploadMemory`.
    fn upload_memory(
        &mut self,
        runtime: &mut TextureCacheRuntime,
        staging_buffer: vk::Buffer,
        staging_offset: vk::DeviceSize,
        copies: &[BufferImageCopy],
    ) -> bool {
        if ENABLE_MSAA_RESOLVE_CONSUME {
            runtime.invalidate_resolve_shadow(self.handle());
        }
        if copies.is_empty() {
            return true;
        }
        let is_rescaled = self.is_rescaled();
        if is_rescaled && !self.scale_down(runtime, true) {
            return false;
        }
        let aspect = self.aspect_mask();
        let wants_msaa_upload = wants_msaa_upload(&self.base().info, aspect);

        if wants_msaa_upload {
            let mut temp_info = self.base().info.clone();
            temp_info.num_samples = 1;
            let temp_image = match runtime.create_msaa_upload_image(&temp_info) {
                Ok(image) => image,
                Err(err) => {
                    log::warn!(
                        "Image::upload_memory: failed to create temporary MSAA upload image: {err:?}"
                    );
                    return false;
                }
            };
            let temp_handle = temp_image.handle();
            let vk_copies = transform_buffer_image_copies(copies, staging_offset, aspect);
            let device = runtime.device().clone();
            runtime
                .scheduler()
                .request_outside_render_pass_operation_context();
            runtime.scheduler().record(move |cmd| {
                copy_buffer_to_image(
                    &device,
                    cmd,
                    staging_buffer,
                    temp_handle,
                    aspect,
                    false,
                    &vk_copies,
                );
            });
            let image_copies = make_msaa_upload_copies(copies, self.base().info.num_samples);
            let copied = runtime.copy_msaa_upload(
                self.original_image.handle(),
                self.base().info.format,
                temp_handle,
                self.base().info.format,
                self.base().info.num_samples,
                &image_copies,
            );
            runtime.keep_msaa_upload_image_alive(temp_image);
            if !copied {
                return false;
            }
            self.initialized = true;
            self.layout = vk::ImageLayout::GENERAL;
            if is_rescaled && !self.scale_up(runtime, false) {
                return false;
            }
            return true;
        }

        if self.base().info.num_samples > 1 {
            log::warn!(
                "MSAA upload not implemented for format {:?}",
                self.base().info.format
            );
            if is_rescaled && !self.scale_up(runtime, false) {
                return false;
            }
            return true;
        }

        let is_initialized = self.exchange_initialization();
        let image = self.original_image.handle();
        let vk_copies = transform_buffer_image_copies(copies, staging_offset, aspect);

        let device = runtime.device().clone();
        let scheduler = runtime.scheduler();
        scheduler.request_outside_render_pass_operation_context();
        scheduler.record(move |cmd| {
            copy_buffer_to_image(
                &device,
                cmd,
                staging_buffer,
                image,
                aspect,
                is_initialized,
                &vk_copies,
            );
        });
        self.layout = vk::ImageLayout::GENERAL;
        if is_rescaled && !self.scale_up(runtime, false) {
            return false;
        }
        true
    }

    /// Port-facing counterpart of upstream `Vulkan::Image::DownloadMemory`.
    fn download_memory(
        &mut self,
        runtime: &mut TextureCacheRuntime,
        buffers: &[vk::Buffer],
        offsets: &[vk::DeviceSize],
        copies: &[BufferImageCopy],
    ) -> bool {
        if copies.is_empty() {
            return true;
        }
        if buffers.len() != offsets.len() {
            return false;
        }
        let is_rescaled = self.is_rescaled();
        if is_rescaled && !self.scale_down(runtime, false) {
            return false;
        }

        let image = self.original_image.handle();
        let aspect = self.aspect_mask();
        let buffers = buffers.to_vec();
        let vk_copies = offsets
            .iter()
            .map(|offset| transform_buffer_image_copies(copies, *offset, aspect))
            .collect::<Vec<_>>();

        let device = runtime.device().clone();
        let scheduler = runtime.scheduler();
        scheduler.request_outside_render_pass_operation_context();
        scheduler.record(move |cmd| unsafe {
            let read_barrier = vk::ImageMemoryBarrier::builder()
                .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                .old_layout(vk::ImageLayout::GENERAL)
                .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: aspect,
                    base_mip_level: 0,
                    level_count: vk::REMAINING_MIP_LEVELS,
                    base_array_layer: 0,
                    layer_count: vk::REMAINING_ARRAY_LAYERS,
                })
                .build();
            device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[read_barrier],
            );

            for (buffer, copies) in buffers.iter().zip(vk_copies.iter()) {
                device.cmd_copy_image_to_buffer(
                    cmd,
                    image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    *buffer,
                    copies,
                );
            }

            let memory_write_barrier = vk::MemoryBarrier::builder()
                .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
                .dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE)
                .build();
            let image_write_barrier = vk::ImageMemoryBarrier::builder()
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::MEMORY_WRITE)
                .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: aspect,
                    base_mip_level: 0,
                    level_count: vk::REMAINING_MIP_LEVELS,
                    base_array_layer: 0,
                    layer_count: vk::REMAINING_ARRAY_LAYERS,
                })
                .build();
            device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[memory_write_barrier],
                &[],
                &[image_write_barrier],
            );
        });
        self.layout = vk::ImageLayout::GENERAL;

        if is_rescaled && !self.scale_up(runtime, true) {
            return false;
        }
        true
    }

    fn download_memory_to_staging(
        &mut self,
        runtime: &mut TextureCacheRuntime,
        staging: &StagingBufferRef,
        copies: &[BufferImageCopy],
    ) -> bool {
        self.download_memory(runtime, &[staging.buffer], &[staging.offset], copies)
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        let Some(mut runtime) = self.runtime else {
            return;
        };
        // SAFETY: `TextureCache` stores its runtime in a `Box`, so the address
        // is stable and, as in upstream, the runtime outlives every typed slot
        // and delayed-destruction ring owned by the cache.
        self.destroy_runtime_resources(unsafe { runtime.as_mut() });
    }
}

/// Backend-owned Vulkan view corresponding to upstream `Vulkan::ImageView`.
pub struct ImageView {
    /// Upstream `ImageView::device`; owns creation of all auxiliary views.
    vulkan_device: NonNull<Device>,
    device: ash::Device,
    base: NonNull<ImageViewBase>,
    pub image_handle: vk::Image,
    pub image_views: [vk::ImageView; shader_recompiler::shader_info::NUM_TEXTURE_TYPES as usize],
    pub render_target: vk::ImageView,
    pub typeless_storage_view: vk::ImageView,
    pub depth_view: vk::ImageView,
    pub stencil_view: vk::ImageView,
    pub color_view: vk::ImageView,
    pub storage_signeds:
        [vk::ImageView; shader_recompiler::shader_info::NUM_TEXTURE_TYPES as usize],
    pub storage_unsigneds:
        [vk::ImageView; shader_recompiler::shader_info::NUM_TEXTURE_TYPES as usize],
    /// Dedicated fallback image owned by upstream's null `ImageView` on
    /// devices without `nullDescriptor` support.
    pub null_image: Option<AllocatedImage>,
    pub samples: vk::SampleCountFlags,
    pub buffer_size: u32,
    pub supports_depth_comparison: bool,
}

impl ImageView {
    pub(crate) fn base(&self) -> &ImageViewBase {
        // SAFETY: see `Image::base`; the view payload and boxed base share the
        // same typed-slot lifecycle.
        unsafe { self.base.as_ref() }
    }

    /// Port of upstream `ImageView::MakeView`.
    fn make_view(
        &self,
        format: vk::Format,
        aspect_mask: vk::ImageAspectFlags,
        texture_type: Option<TextureType>,
    ) -> Result<vk::ImageView, vk::Result> {
        let (view_type, subresource_range) =
            aux_image_view_params(self.base(), aspect_mask, texture_type);
        let view_info = vk::ImageViewCreateInfo::builder()
            .image(self.image_handle)
            .view_type(view_type)
            .format(format)
            .components(vk::ComponentMapping {
                r: vk::ComponentSwizzle::IDENTITY,
                g: vk::ComponentSwizzle::IDENTITY,
                b: vk::ComponentSwizzle::IDENTITY,
                a: vk::ComponentSwizzle::IDENTITY,
            })
            .subresource_range(subresource_range)
            .build();
        unsafe { self.device.create_image_view(&view_info, None) }
    }

    pub(crate) fn handle(&self, texture_type: TextureType) -> vk::ImageView {
        self.image_views[texture_type as usize]
    }

    /// Port of `Vulkan::ImageView::DepthView`.
    fn depth_view(&mut self) -> Result<vk::ImageView, vk::Result> {
        if self.image_handle == vk::Image::null() {
            return Ok(vk::ImageView::null());
        }
        if self.depth_view == vk::ImageView::null() {
            let format = maxwell_to_vk::surface_format(
                unsafe { self.vulkan_device.as_ref() },
                FormatType::Optimal,
                true,
                self.base().format,
            )
            .format;
            self.depth_view = self.make_view(format, vk::ImageAspectFlags::DEPTH, None)?;
        }
        Ok(self.depth_view)
    }

    /// Port of `Vulkan::ImageView::StencilView`.
    fn stencil_view(&mut self) -> Result<vk::ImageView, vk::Result> {
        if self.image_handle == vk::Image::null() {
            return Ok(vk::ImageView::null());
        }
        if self.stencil_view == vk::ImageView::null() {
            let format = maxwell_to_vk::surface_format(
                unsafe { self.vulkan_device.as_ref() },
                FormatType::Optimal,
                true,
                self.base().format,
            )
            .format;
            self.stencil_view = self.make_view(format, vk::ImageAspectFlags::STENCIL, None)?;
        }
        Ok(self.stencil_view)
    }

    /// Port of `Vulkan::ImageView::ColorView`.
    fn color_view(&mut self) -> Result<vk::ImageView, vk::Result> {
        if self.image_handle == vk::Image::null() {
            return Ok(vk::ImageView::null());
        }
        if self.color_view == vk::ImageView::null() {
            self.color_view = self.make_view(
                vk::Format::R8G8B8A8_UNORM,
                vk::ImageAspectFlags::COLOR,
                None,
            )?;
        }
        Ok(self.color_view)
    }

    fn render_target(&self) -> vk::ImageView {
        self.render_target
    }

    fn image_handle(&self) -> vk::Image {
        self.image_handle
    }

    fn samples(&self) -> vk::SampleCountFlags {
        self.samples
    }
}

fn blit_image_view_from_backend(view: &ImageView, is_rescaled: bool) -> BlitImageView {
    BlitImageView {
        image: view.image_handle(),
        subresource_range: subresource_range_from_view(
            view.base().format,
            view.base().range,
            view.base().flags.contains(ImageViewFlagBits::SLICE),
        ),
        color_view: view.handle(TextureType::Color2D),
        depth_view: view.depth_view,
        stencil_view: view.stencil_view,
        size: BlitExtent3D {
            width: view.base().size.width,
            height: view.base().size.height,
            depth: view.base().size.depth,
        },
        is_rescaled,
    }
}

impl Drop for ImageView {
    fn drop(&mut self) {
        let mut destroyed = Vec::new();
        let mut destroy_once = |handle: vk::ImageView| {
            if handle == vk::ImageView::null() || destroyed.contains(&handle) {
                return;
            }
            destroyed.push(handle);
            unsafe { self.device.destroy_image_view(handle, None) };
        };
        for &handle in &self.image_views {
            destroy_once(handle);
        }
        for &handle in &self.storage_signeds {
            destroy_once(handle);
        }
        for &handle in &self.storage_unsigneds {
            destroy_once(handle);
        }
        for handle in [
            self.typeless_storage_view,
            self.depth_view,
            self.stencil_view,
            self.color_view,
        ] {
            destroy_once(handle);
        }
    }
}

/// Backend-owned framebuffer corresponding to upstream `Vulkan::Framebuffer`.
struct PendingImageViews {
    device: ash::Device,
    handles: Vec<vk::ImageView>,
}

impl Drop for PendingImageViews {
    fn drop(&mut self) {
        unsafe {
            for view in self.handles.drain(..) {
                self.device.destroy_image_view(view, None);
            }
        }
    }
}

pub struct Framebuffer {
    device: Option<ash::Device>,
    framebuffer: vk::Framebuffer,
    render_pass: vk::RenderPass,
    render_area: vk::Extent2D,
    samples: vk::SampleCountFlags,
    num_color_buffers: u32,
    num_images: usize,
    images: [vk::Image; NUM_RT + 1],
    image_ranges: [vk::ImageSubresourceRange; NUM_RT + 1],
    rt_map: [usize; NUM_RT],
    has_depth: bool,
    has_stencil: bool,
    is_rescaled: bool,
    resolve_images: Vec<AllocatedImage>,
    resolve_image_views: Vec<vk::ImageView>,
    render_pass_key: RenderPassKey,
    render_pass_cache: NonNull<RenderPassCache>,
    discard_msaa_color: bool,
}

impl Drop for Framebuffer {
    fn drop(&mut self) {
        let Some(device) = self.device.as_ref() else {
            return;
        };
        unsafe {
            for view in self.resolve_image_views.drain(..) {
                device.destroy_image_view(view, None);
            }
            if self.framebuffer != vk::Framebuffer::null() {
                device.destroy_framebuffer(self.framebuffer, None);
                self.framebuffer = vk::Framebuffer::null();
            }
        }
    }
}

impl Framebuffer {
    /// Port of `Vulkan::Framebuffer::Framebuffer(TextureCacheRuntime&, ...)`
    /// and `Framebuffer::CreateFramebuffer`.
    fn new(
        runtime: &mut TextureCacheRuntime,
        color_buffers: [Option<NonNull<ImageView>>; NUM_RT],
        depth_buffer: Option<NonNull<ImageView>>,
        key: &RenderTargets,
    ) -> Result<Box<Self>, vk::Result> {
        let mut attachments = SmallVec::<[vk::ImageView; NUM_RT + 1]>::new();
        let mut render_pass_key = RenderPassKey::default();
        let mut num_layers = 1i32;
        let mut width = u32::MAX;
        let mut height = u32::MAX;
        let mut samples = vk::SampleCountFlags::TYPE_1;
        let mut num_images = 0usize;
        let mut images = [vk::Image::null(); NUM_RT + 1];
        let mut image_ranges = [vk::ImageSubresourceRange::default(); NUM_RT + 1];
        let mut rt_map = [0usize; NUM_RT];

        for (index, color_buffer) in color_buffers.iter().enumerate() {
            let Some(color_buffer) = color_buffer else {
                render_pass_key.color_formats[index] = PixelFormat::Invalid;
                continue;
            };
            // SAFETY: the typed image-view slots outlive cached framebuffers.
            let color_buffer = unsafe { color_buffer.as_ref() };
            let base = color_buffer.base();
            width = width.min(if key.is_rescaled {
                runtime.resolution.scale_up_u32(base.size.width)
            } else {
                base.size.width
            });
            height = height.min(if key.is_rescaled {
                runtime.resolution.scale_up_u32(base.size.height)
            } else {
                base.size.height
            });
            attachments.push(color_buffer.render_target());
            render_pass_key.color_formats[index] = base.format;
            num_layers = num_layers.max(base.range.extent.layers);
            images[num_images] = color_buffer.image_handle();
            image_ranges[num_images] =
                make_subresource_range(image_aspect_mask(base.format), base.range, base.flags);
            rt_map[index] = num_images;
            samples = color_buffer.samples();
            num_images += 1;
        }
        let num_colors = attachments.len();

        let mut has_depth = false;
        let mut has_stencil = false;
        if let Some(depth_buffer) = depth_buffer {
            // SAFETY: the typed image-view slot outlives cached framebuffers.
            let depth_buffer = unsafe { depth_buffer.as_ref() };
            let base = depth_buffer.base();
            width = width.min(if key.is_rescaled {
                runtime.resolution.scale_up_u32(base.size.width)
            } else {
                base.size.width
            });
            height = height.min(if key.is_rescaled {
                runtime.resolution.scale_up_u32(base.size.height)
            } else {
                base.size.height
            });
            attachments.push(depth_buffer.render_target());
            render_pass_key.depth_format = base.format;
            num_layers = num_layers.max(base.range.extent.layers);
            images[num_images] = depth_buffer.image_handle();
            let subresource_range =
                make_subresource_range(image_aspect_mask(base.format), base.range, base.flags);
            image_ranges[num_images] = subresource_range;
            samples = depth_buffer.samples();
            num_images += 1;
            has_depth = subresource_range
                .aspect_mask
                .contains(vk::ImageAspectFlags::DEPTH);
            has_stencil = subresource_range
                .aspect_mask
                .contains(vk::ImageAspectFlags::STENCIL);
        } else {
            render_pass_key.depth_format = PixelFormat::Invalid;
        }

        render_pass_key.samples = samples;
        let do_resolve_color = samples != vk::SampleCountFlags::TYPE_1
            && num_colors > 0
            && runtime.vulkan_device().is_tiler();
        render_pass_key.resolve_color = do_resolve_color;
        let discard_msaa_color =
            ENABLE_MSAA_RESOLVE_CONSUME && ENABLE_MSAA_COLOR_DISCARD && do_resolve_color;
        let render_pass = runtime.render_pass_cache().get(&render_pass_key)?;
        let render_pass_cache = runtime.render_pass_cache;
        let mut render_area = vk::Extent2D {
            width: key.size.width.min(width),
            height: key.size.height.min(height),
        };
        // With no attachments upstream leaves width/height at UINT_MAX, so
        // the requested default framebuffer extent survives unchanged.
        if width == u32::MAX {
            render_area.width = key.size.width;
        }
        if height == u32::MAX {
            render_area.height = key.size.height;
        }

        let layers = num_layers.max(1) as u32;
        let mut resolve_images = Vec::new();
        let mut pending_resolve_views = PendingImageViews {
            device: runtime.device().clone(),
            handles: Vec::new(),
        };
        if do_resolve_color {
            for index in 0..NUM_RT {
                let format = render_pass_key.color_formats[index];
                if format == PixelFormat::Invalid {
                    continue;
                }
                let vk_format = runtime.surface_format(format, true);
                if ENABLE_MSAA_RESOLVE_CONSUME {
                    let msaa_image = images[rt_map[index]];
                    attachments.push(runtime.get_or_create_resolve_shadow(
                        msaa_image,
                        vk_format,
                        render_area,
                        layers,
                    )?);
                    continue;
                }

                let image_info = vk::ImageCreateInfo::builder()
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(vk_format)
                    .extent(vk::Extent3D {
                        width: render_area.width,
                        height: render_area.height,
                        depth: 1,
                    })
                    .mip_levels(1)
                    .array_layers(layers)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::OPTIMAL)
                    .usage(
                        vk::ImageUsageFlags::COLOR_ATTACHMENT
                            | vk::ImageUsageFlags::SAMPLED
                            | vk::ImageUsageFlags::TRANSFER_SRC,
                    )
                    .sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .initial_layout(vk::ImageLayout::UNDEFINED)
                    .build();
                let resolve_image = runtime
                    .memory_allocator()
                    .create_owned_image(&image_info)
                    .map_err(|error| error.result)?;
                let view_info = vk::ImageViewCreateInfo::builder()
                    .image(resolve_image.handle())
                    .view_type(if layers > 1 {
                        vk::ImageViewType::TYPE_2D_ARRAY
                    } else {
                        vk::ImageViewType::TYPE_2D
                    })
                    .format(vk_format)
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count: layers,
                    })
                    .build();
                let resolve_view = unsafe { runtime.device().create_image_view(&view_info, None) }?;
                attachments.push(resolve_view);
                resolve_images.push(resolve_image);
                pending_resolve_views.handles.push(resolve_view);
            }
        }

        let framebuffer_info = vk::FramebufferCreateInfo::builder()
            .render_pass(render_pass)
            .attachments(&attachments)
            .width(render_area.width)
            .height(render_area.height)
            .layers(layers)
            .build();
        let framebuffer = unsafe { runtime.device().create_framebuffer(&framebuffer_info, None) }?;
        if runtime.vulkan_device().has_debugging_tool_attached() {
            let name = crate::texture_cache::formatter::render_targets_name(key);
            if let Err(error) = crate::vulkan_common::vulkan_wrapper::set_framebuffer_name(
                &runtime.instance,
                runtime.device(),
                framebuffer,
                &name,
            ) {
                unsafe { runtime.device().destroy_framebuffer(framebuffer, None) };
                return Err(error.result);
            }
        }
        let resolve_image_views = std::mem::take(&mut pending_resolve_views.handles);

        Ok(Box::new(Self {
            device: Some(runtime.device().clone()),
            framebuffer,
            render_pass,
            render_area,
            samples,
            num_color_buffers: num_colors as u32,
            num_images,
            images,
            image_ranges,
            rt_map,
            has_depth,
            has_stencil,
            is_rescaled: key.is_rescaled,
            resolve_images,
            resolve_image_views,
            render_pass_key,
            render_pass_cache,
            discard_msaa_color,
        }))
    }

    fn render_target_framebuffer(&self) -> RenderTargetFramebuffer {
        RenderTargetFramebuffer {
            framebuffer_owner: NonNull::from(self),
        }
    }

    /// Port of `Framebuffer::RenderPassVariant`.
    pub fn render_pass_variant(
        &self,
        color_clear_mask: u32,
        depth_stencil_clear: bool,
        color_discard_mask: u32,
    ) -> Result<vk::RenderPass, vk::Result> {
        if color_clear_mask == 0 && !depth_stencil_clear && color_discard_mask == 0 {
            return Ok(self.render_pass);
        }
        let mut key = self.render_pass_key.clone();
        key.color_clear_mask = color_clear_mask;
        key.depth_stencil_clear = depth_stencil_clear;
        key.color_discard_mask = color_discard_mask;
        // SAFETY: the boxed cache outlives all framebuffer owners.
        unsafe { self.render_pass_cache.as_ref() }.get(&key)
    }

    /// Port of `Vulkan::Framebuffer::HasResolveColor`.
    pub fn has_resolve_color(&self) -> bool {
        !self.resolve_images.is_empty()
    }

    /// Port of `Vulkan::Framebuffer::ResolveColorImage`.
    pub fn resolve_color_image(&self, index: usize) -> vk::Image {
        self.resolve_images
            .get(index)
            .map_or(vk::Image::null(), AllocatedImage::handle)
    }

    /// Port of `Vulkan::Framebuffer::DiscardsMsaaColor`.
    pub fn discards_msaa_color(&self) -> bool {
        self.discard_msaa_color
    }
}

/// Backend-owned sampler corresponding to an upstream `TSCEntry`.
pub struct CachedSampler {
    device: Option<ash::Device>,
    sampler: vk::Sampler,
    sampler_default_anisotropy: vk::Sampler,
    sampler_nearest: vk::Sampler,
    sampler_noncompare: vk::Sampler,
}

impl Drop for CachedSampler {
    fn drop(&mut self) {
        let Some(device) = self.device.as_ref() else {
            return;
        };
        unsafe {
            for handle in [
                self.sampler,
                self.sampler_default_anisotropy,
                self.sampler_nearest,
                self.sampler_noncompare,
            ] {
                if handle != vk::Sampler::null() {
                    device.destroy_sampler(handle, None);
                }
            }
        }
    }
}

impl CachedSampler {
    /// Port of `Vulkan::Sampler::Handle`.
    pub fn handle(&self) -> vk::Sampler {
        self.sampler
    }

    /// Port of `Vulkan::Sampler::HandleWithDefaultAnisotropy`.
    pub fn handle_with_default_anisotropy(&self) -> vk::Sampler {
        self.sampler_default_anisotropy
    }

    /// Port of `Vulkan::Sampler::HasAddedAnisotropy`.
    pub fn has_added_anisotropy(&self) -> bool {
        self.sampler_default_anisotropy != vk::Sampler::null()
    }

    /// Port of `Vulkan::Sampler::HandleWithNearestFilter`.
    pub fn handle_with_nearest_filter(&self) -> vk::Sampler {
        self.sampler_nearest
    }

    /// Port of `Vulkan::Sampler::HasLinearFiltering`.
    pub fn has_linear_filtering(&self) -> bool {
        self.sampler_nearest != vk::Sampler::null()
    }

    /// Port of `Vulkan::Sampler::HandleWithoutDepthComparison`.
    pub fn handle_without_depth_comparison(&self) -> vk::Sampler {
        self.sampler_noncompare
    }

    /// Port of `Vulkan::Sampler::HasDepthComparison`.
    pub fn has_depth_comparison(&self) -> bool {
        self.sampler_noncompare != vk::Sampler::null()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderTargetFramebuffer {
    framebuffer_owner: NonNull<Framebuffer>,
}

/// Vulkan resources returned by upstream
/// `TextureCache::GetImageView(draw_texture_state.src_texture)`.
#[derive(Debug, Clone, Copy)]
pub struct DrawTextureSource {
    pub image_view: vk::ImageView,
    pub image: vk::Image,
    pub size: BlitExtent3D,
    pub is_rescaled: bool,
}

impl RenderTargetFramebuffer {
    fn owner(&self) -> &Framebuffer {
        // SAFETY: cached framebuffers use stable `Box<Framebuffer>` storage.
        // Sentenced owners retain that allocation until the scheduler's GPU
        // tick has passed every command that can hold this pointer.
        unsafe { self.framebuffer_owner.as_ref() }
    }

    pub fn handle(&self) -> vk::Framebuffer {
        self.owner().framebuffer
    }

    pub fn render_pass(&self) -> vk::RenderPass {
        self.owner().render_pass
    }

    pub fn render_area(&self) -> vk::Extent2D {
        self.owner().render_area
    }

    pub fn num_color_buffers(&self) -> u32 {
        self.owner().num_color_buffers
    }

    pub fn samples(&self) -> vk::SampleCountFlags {
        self.owner().samples
    }

    pub fn num_images(&self) -> usize {
        self.owner().num_images
    }

    pub fn images(&self) -> &[vk::Image; NUM_RT + 1] {
        &self.owner().images
    }

    pub fn image_ranges(&self) -> &[vk::ImageSubresourceRange; NUM_RT + 1] {
        &self.owner().image_ranges
    }

    pub fn has_aspect_depth_bit(&self) -> bool {
        self.owner().has_depth
    }

    pub fn has_aspect_stencil_bit(&self) -> bool {
        self.owner().has_stencil
    }

    pub fn is_rescaled(&self) -> bool {
        self.owner().is_rescaled
    }

    pub fn render_pass_key_base(&self) -> &RenderPassKey {
        &self.owner().render_pass_key
    }

    pub fn render_pass_variant(
        &self,
        color_clear_mask: u32,
        depth_stencil_clear: bool,
        color_discard_mask: u32,
    ) -> Result<vk::RenderPass, vk::Result> {
        self.owner()
            .render_pass_variant(color_clear_mask, depth_stencil_clear, color_discard_mask)
    }

    pub fn discards_msaa_color(&self) -> bool {
        self.owner().discards_msaa_color()
    }

    /// Port of `Vulkan::Framebuffer::HasAspectColorBit`.
    pub fn has_aspect_color_bit(&self, index: usize) -> bool {
        let mapped = self.owner().rt_map[index];
        self.owner().image_ranges[mapped]
            .aspect_mask
            .contains(vk::ImageAspectFlags::COLOR)
    }

    pub fn blit_framebuffer_info(&self) -> BlitFramebufferInfo {
        BlitFramebufferInfo {
            framebuffer: self.handle(),
            render_pass: self.render_pass(),
            render_area: self.render_area(),
            images: *self.images(),
            image_ranges: *self.image_ranges(),
            num_images: self.num_images(),
            samples: self.samples(),
            has_stencil: self.has_aspect_stencil_bit(),
        }
    }
}

pub struct FramebufferImageViewVulkan {
    pub common: FramebufferImageView,
    pub image: vk::Image,
    pub image_view: vk::ImageView,
    pub width: u32,
    pub height: u32,
}

enum DeferredVkResource {
    Framebuffer(Box<Framebuffer>),
    ImageView(ImageView),
    Image(Image),
}

struct SentencedVkResource {
    retire_tick: u64,
    resource: DeferredVkResource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JoinCopyOperation {
    CopyImage,
    CopyImageMsaa,
    Reinterpret,
    Convert,
}

fn should_reinterpret_join_copy(
    dst: &ImageBase,
    src: &ImageBase,
    shader_stencil_export_supported: bool,
) -> bool {
    if crate::surface::get_format_type(dst.info.format) == SurfaceType::DepthStencil
        && !shader_stencil_export_supported
    {
        return true;
    }
    dst.info.format == PixelFormat::D32FloatS8Uint || src.info.format == PixelFormat::D32FloatS8Uint
}

fn can_convert_join_copy_formats(dst_format: PixelFormat, src_format: PixelFormat) -> bool {
    matches!(
        (dst_format, src_format),
        (PixelFormat::R16Unorm, PixelFormat::D16Unorm)
            | (PixelFormat::A8B8G8R8Srgb, PixelFormat::D32Float)
            | (PixelFormat::A8B8G8R8Unorm, PixelFormat::S8UintD24Unorm)
            | (PixelFormat::A8B8G8R8Unorm, PixelFormat::D24UnormS8Uint)
            | (PixelFormat::A8B8G8R8Unorm, PixelFormat::D32Float)
            | (PixelFormat::B8G8R8A8Srgb, PixelFormat::D32Float)
            | (PixelFormat::B8G8R8A8Unorm, PixelFormat::D32Float)
            | (PixelFormat::R32Float, PixelFormat::D32Float)
            | (PixelFormat::D16Unorm, PixelFormat::R16Unorm)
            | (PixelFormat::S8UintD24Unorm, PixelFormat::A8B8G8R8Unorm)
            | (PixelFormat::S8UintD24Unorm, PixelFormat::B8G8R8A8Unorm)
            | (PixelFormat::D32Float, PixelFormat::A8B8G8R8Unorm)
            | (PixelFormat::D32Float, PixelFormat::B8G8R8A8Unorm)
            | (PixelFormat::D32Float, PixelFormat::A8B8G8R8Srgb)
            | (PixelFormat::D32Float, PixelFormat::B8G8R8A8Srgb)
            | (PixelFormat::D32Float, PixelFormat::R32Float)
    )
}

fn select_join_copy_operation(
    dst: &ImageBase,
    src: &ImageBase,
    shader_stencil_export_supported: bool,
) -> Option<JoinCopyOperation> {
    let dst_format_type = crate::surface::get_format_type(dst.info.format);
    let src_format_type = crate::surface::get_format_type(src.info.format);
    if dst_format_type == src_format_type {
        return Some(if dst.info.num_samples != src.info.num_samples {
            JoinCopyOperation::CopyImageMsaa
        } else {
            JoinCopyOperation::CopyImage
        });
    }
    if dst.info.image_type != ImageType::E2D || src.info.image_type != ImageType::E2D {
        return None;
    }
    if should_reinterpret_join_copy(dst, src, shader_stencil_export_supported) {
        return Some(JoinCopyOperation::Reinterpret);
    }
    can_convert_join_copy_formats(dst.info.format, src.info.format)
        .then_some(JoinCopyOperation::Convert)
}

/// Runtime services used by the Vulkan texture cache backend.
///
/// Port-facing counterpart of upstream `Vulkan::TextureCacheRuntime`. The
/// generic cache owns the complete typed slots; this runtime owns Vulkan
/// resource creation/destruction and the scheduler/staging services used for
/// transfers.
struct ResolveShadow {
    image: AllocatedImage,
    view: vk::ImageView,
    format: vk::Format,
    extent: vk::Extent2D,
    layers: u32,
    up_to_date: bool,
}

pub struct TextureCacheRuntime {
    device_owner: NonNull<Device>,
    device: ash::Device,
    instance: ash::Instance,
    physical_device: vk::PhysicalDevice,
    scheduler: NonNull<Scheduler>,
    memory_allocator: NonNull<MemoryAllocator>,
    staging_buffer_pool: NonNull<StagingBufferPool>,
    blit_image_helper: NonNull<BlitImageHelper>,
    render_pass_cache: NonNull<RenderPassCache>,
    astc_decoder_pass: Option<AstcDecoderPass>,
    bl3d_unswizzle_pass: Option<BlockLinearUnswizzle3DPass>,
    /// Cached `vkGetPhysicalDeviceFormatProperties` results (upstream caches
    /// these in `Device`); queried on hot per-draw paths via `surface_format`.
    format_properties:
        std::cell::RefCell<std::collections::HashMap<vk::Format, vk::FormatProperties>>,
    shader_stencil_export_supported: bool,
    resolution: common::settings::ResolutionScalingInfo,
    view_formats: Vec<Vec<vk::Format>>,
    buffers: [Option<AllocatedBuffer>; Self::INDEXING_SLOTS],
    device_memory_info: DeviceMemoryInfo,
    sentenced_resources: Vec<SentencedVkResource>,
    pending_msaa_images: Vec<(u64, AllocatedImage)>,
    resolve_shadows: HashMap<vk::Image, ResolveShadow, BuildUnorderedDenseHasher>,
    current_tick: u64,
    optimal_bcn_supported: bool,
    optimal_astc_supported: bool,
    cant_blit_msaa: bool,
    image_format_list_supported: bool,
    must_emulate_bgr565: bool,
    ext_4444_formats_supported: bool,
    custom_border_color_supported: bool,
    sampler_filter_minmax_supported: bool,
    sampler_heap_budget: Option<usize>,
    has_null_descriptor: bool,
}

impl TextureCacheRuntime {
    const INDEXING_SLOTS: usize = 8 * std::mem::size_of::<usize>();

    fn vulkan_device(&self) -> &Device {
        // SAFETY: `RendererVulkan` owns stable boxed storage for `Device` and
        // drops the texture cache before the device owner.
        unsafe { self.device_owner.as_ref() }
    }

    pub fn new(
        vulkan_device: &Device,
        device: ash::Device,
        instance: ash::Instance,
        physical_device: vk::PhysicalDevice,
        scheduler: &mut Scheduler,
        memory_allocator: &mut MemoryAllocator,
        staging_buffer_pool: &mut StagingBufferPool,
        blit_image_helper: &mut BlitImageHelper,
        render_pass_cache: &mut RenderPassCache,
        descriptor_pool: &mut DescriptorPool,
        compute_pass_descriptor_queue: &mut ComputePassDescriptorQueue,
        cant_blit_msaa: bool,
        image_format_list_supported: bool,
        optimal_astc_supported: bool,
        must_emulate_bgr565: bool,
        ext_4444_formats_supported: bool,
        custom_border_color_supported: bool,
        sampler_filter_minmax_supported: bool,
        sampler_heap_budget: Option<usize>,
        has_null_descriptor: bool,
    ) -> Self {
        let device_memory_info = query_device_memory_info(&instance, physical_device);
        let optimal_bcn_supported = unsafe {
            instance
                .get_physical_device_features(physical_device)
                .texture_compression_bc
                != 0
        };
        let astc_decoder_pass = if *common::settings::values().accelerate_astc.get_value()
            == common::settings_enums::AstcDecodeMode::Gpu
        {
            match AstcDecoderPass::new(
                vulkan_device,
                scheduler,
                descriptor_pool,
                staging_buffer_pool,
                compute_pass_descriptor_queue,
                memory_allocator,
            ) {
                Ok(pass) => Some(pass),
                Err(err) => {
                    log::warn!(
                        "TextureCacheRuntime: failed to create ASTCDecoderPass: {:?}",
                        err
                    );
                    None
                }
            }
        } else {
            None
        };
        let bl3d_unswizzle_pass = if *common::settings::values().gpu_unswizzle_enabled.get_value() {
            match BlockLinearUnswizzle3DPass::new(
                vulkan_device,
                scheduler,
                descriptor_pool,
                staging_buffer_pool,
                compute_pass_descriptor_queue,
            ) {
                Ok(pass) => Some(pass),
                Err(err) => {
                    log::warn!(
                        "TextureCacheRuntime: failed to create BlockLinearUnswizzle3DPass: {:?}",
                        err
                    );
                    None
                }
            }
        } else {
            None
        };
        let shader_stencil_export_supported = blit_image_helper.shader_stencil_export_supported();
        let mut runtime = Self {
            device_owner: NonNull::from(vulkan_device),
            device,
            instance,
            physical_device,
            scheduler: NonNull::from(scheduler),
            memory_allocator: NonNull::from(memory_allocator),
            staging_buffer_pool: NonNull::from(staging_buffer_pool),
            blit_image_helper: NonNull::from(blit_image_helper),
            render_pass_cache: NonNull::from(render_pass_cache),
            astc_decoder_pass,
            bl3d_unswizzle_pass,
            format_properties: std::cell::RefCell::new(std::collections::HashMap::new()),
            shader_stencil_export_supported,
            resolution: common::settings::values().resolution_info.clone(),
            view_formats: vec![Vec::new(); crate::surface::MAX_PIXEL_FORMAT as usize],
            buffers: std::array::from_fn(|_| None),
            device_memory_info,
            sentenced_resources: Vec::new(),
            pending_msaa_images: Vec::new(),
            resolve_shadows: HashMap::default(),
            current_tick: 0,
            optimal_bcn_supported,
            optimal_astc_supported,
            cant_blit_msaa,
            image_format_list_supported,
            must_emulate_bgr565,
            ext_4444_formats_supported,
            custom_border_color_supported,
            sampler_filter_minmax_supported,
            sampler_heap_budget,
            has_null_descriptor,
        };
        runtime.initialize_view_formats();
        runtime
    }

    /// Port of `TextureCacheRuntime`'s `view_formats` initialization.
    ///
    /// Upstream creates each backend image in its `ImageInfo` format and
    /// advertises every compatible view format through
    /// `VkImageFormatListCreateInfo`. Views may then reinterpret the same
    /// allocation without recreating the image and losing GPU-only contents.
    fn initialize_view_formats(&mut self) {
        if !self.image_format_list_supported {
            return;
        }
        for image_index in 0..crate::surface::MAX_PIXEL_FORMAT {
            // SAFETY: `PixelFormat` is contiguous from zero through
            // `MAX_PIXEL_FORMAT`, matching upstream's enum/table contract.
            let image_format =
                unsafe { std::mem::transmute::<u32, PixelFormat>(image_index as u32) };
            let mut formats = Vec::new();
            if crate::surface::is_pixel_format_astc(image_format) && !self.optimal_astc_supported {
                formats.push(vk::Format::A8B8G8R8_UNORM_PACK32);
            }
            for view_index in 0..crate::surface::MAX_PIXEL_FORMAT {
                // SAFETY: same contiguous enum invariant as above.
                let view_format =
                    unsafe { std::mem::transmute::<u32, PixelFormat>(view_index as u32) };
                if crate::compatible_formats::is_view_compatible(
                    image_format,
                    view_format,
                    false,
                    true,
                ) {
                    let format = self.surface_format_info(view_format, true).format;
                    if format != vk::Format::UNDEFINED && !formats.contains(&format) {
                        formats.push(format);
                    }
                }
            }
            self.view_formats[image_index] = formats;
        }
    }

    fn device(&self) -> &ash::Device {
        &self.device
    }

    fn get_device_local_memory(&self) -> u64 {
        self.device_memory_info.device_local_memory
    }

    fn get_device_memory_usage(&self) -> u64 {
        query_device_memory_usage(
            &self.instance,
            self.physical_device,
            &self.device_memory_info,
        )
    }

    fn can_report_memory_usage(&self) -> bool {
        self.device_memory_info.can_report_memory_usage
    }

    /// Port of `TextureCacheRuntime::GetSamplerHeapBudget`.
    fn get_sampler_heap_budget(&self) -> Option<usize> {
        self.sampler_heap_budget
    }

    fn scheduler(&mut self) -> &mut Scheduler {
        // SAFETY: `TextureCacheRuntime` is constructed with pointers to boxed
        // `RasterizerVulkan` services. The boxes keep stable addresses and the
        // runtime is dropped before those services.
        unsafe { self.scheduler.as_mut() }
    }

    fn staging_buffer_pool(&mut self) -> &mut StagingBufferPool {
        // SAFETY: see `scheduler`.
        unsafe { self.staging_buffer_pool.as_mut() }
    }

    fn memory_allocator(&mut self) -> &mut MemoryAllocator {
        // SAFETY: see `scheduler`.
        unsafe { self.memory_allocator.as_mut() }
    }

    fn render_pass_cache(&self) -> &RenderPassCache {
        // SAFETY: the cache has stable boxed storage and internally
        // synchronizes `get`, so callers only materialize a shared reference.
        unsafe { self.render_pass_cache.as_ref() }
    }

    fn blit_image_helper(&mut self) -> &mut BlitImageHelper {
        // SAFETY: see `scheduler`.
        unsafe { self.blit_image_helper.as_mut() }
    }

    fn upload_staging_buffer(&mut self, size: vk::DeviceSize, deferred: bool) -> StagingBufferRef {
        let staging = if deferred {
            self.staging_buffer_pool()
                .request_deferred_upload_buffer(size)
        } else {
            self.staging_buffer_pool().request_upload_buffer(size)
        };
        staging.expect("Vulkan texture upload staging allocation failed")
    }

    fn download_staging_buffer(
        &mut self,
        size: vk::DeviceSize,
        deferred: bool,
    ) -> Option<StagingBufferRef> {
        self.staging_buffer_pool()
            .request_download_buffer(size, deferred)
    }

    fn free_deferred_staging_buffer(&mut self, buffer: &mut StagingBufferRef) {
        self.staging_buffer_pool().free_deferred(buffer);
    }

    fn finish(&mut self) {
        self.scheduler().finish();
    }

    fn insert_upload_memory_barrier(&mut self) {
        // Upstream Vulkan keeps this empty: `Image::UploadMemory` records
        // `CopyBufferToImage`, including its post-copy barrier, in the same
        // scheduler stream as its consumers.
    }

    fn can_upload_msaa(&self) -> bool {
        true
    }

    fn transition_image_layout(&mut self, image: &mut Image) {
        if image.exchange_initialization() {
            return;
        }
        let image_handle = image.handle();
        let aspect_mask = image.aspect_mask();
        let device = self.device.clone();
        let scheduler = self.scheduler();
        scheduler.request_outside_render_pass_operation_context();
        scheduler.record(move |cmd| unsafe {
            let barrier = vk::ImageMemoryBarrier::builder()
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(image_handle)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask,
                    base_mip_level: 0,
                    level_count: vk::REMAINING_MIP_LEVELS,
                    base_array_layer: 0,
                    layer_count: vk::REMAINING_ARRAY_LAYERS,
                })
                .build();
            device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier],
            );
        });
        image.layout = vk::ImageLayout::GENERAL;
    }

    fn barrier_feedback_loop(&mut self) {
        self.scheduler()
            .request_outside_render_pass_operation_context();
    }

    fn accelerate_image_upload(
        &mut self,
        image: &mut Image,
        map: StagingBufferRef,
        swizzles: &[crate::texture_cache::types::SwizzleParameters],
        z_start: u32,
        z_count: u32,
    ) -> bool {
        if !crate::surface::is_pixel_format_astc(image.base().info.format) {
            if !*common::settings::values().gpu_unswizzle_enabled.get_value()
                || self.bl3d_unswizzle_pass.is_none()
                || !crate::surface::is_pixel_format_bcn(image.base().info.format)
                || image.base().info.image_type != ImageType::E3D
                || image.base().info.resources.levels != 1
                || image.base().info.resources.layers != 1
            {
                log::warn!(
                    "TextureCacheRuntime::accelerate_image_upload unsupported format {:?}",
                    image.base().info.format
                );
                return false;
            }
            let batch_slices = z_count.min(image.base().info.size.depth);
            if !image.allocate_compute_unswizzle_buffer(self, batch_slices) {
                return false;
            }
            let Some(mut pass) = self.bl3d_unswizzle_pass.take() else {
                return false;
            };
            let result = pass.unswizzle(
                image.handle(),
                image.aspect_mask(),
                &image.base().info,
                image.base().guest_size_bytes as usize,
                image
                    .compute_unswizzle_buffer
                    .as_ref()
                    .expect("compute unswizzle buffer was allocated")
                    .handle(),
                image.compute_unswizzle_buffer_size,
                map.buffer,
                map.offset,
                swizzles,
                z_start,
                z_count,
            );
            image.initialized = true;
            image.layout = vk::ImageLayout::GENERAL;
            self.bl3d_unswizzle_pass = Some(pass);
            return result;
        }
        let Some(mut pass) = self.astc_decoder_pass.take() else {
            log::warn!("TextureCacheRuntime::accelerate_image_upload missing ASTCDecoderPass");
            return false;
        };
        let mut storage_views =
            vec![vk::ImageView::null(); image.base().info.resources.levels.max(1) as usize];
        for swizzle in swizzles {
            let view = match self.storage_image_view_with_format(
                image,
                swizzle.level as u32,
                vk::Format::A8B8G8R8_UNORM_PACK32,
            ) {
                Ok(view) => view,
                Err(err) => {
                    log::warn!(
                        "TextureCacheRuntime::accelerate_image_upload failed to create storage view level={} err={:?}",
                        swizzle.level,
                        err
                    );
                    self.astc_decoder_pass = Some(pass);
                    return false;
                }
            };
            let Some(slot) = storage_views.get_mut(swizzle.level as usize) else {
                self.astc_decoder_pass = Some(pass);
                return false;
            };
            *slot = view;
        }
        let is_initialized = image.exchange_initialization();
        let result = pass.assemble(
            image.handle(),
            image.aspect_mask(),
            is_initialized,
            &image.base().info,
            image.base().guest_size_bytes as usize,
            map.buffer,
            map.offset,
            swizzles,
            &storage_views,
        );
        image.layout = vk::ImageLayout::GENERAL;
        self.astc_decoder_pass = Some(pass);
        result
    }

    fn get_temporary_buffer(&mut self, needed_size: usize) -> Option<vk::Buffer> {
        let needed_size = needed_size.max(1);
        let level = (usize::BITS - (needed_size - 1).leading_zeros()) as usize;
        if let Some(buffer) = self.buffers.get(level)?.as_ref() {
            return Some(buffer.handle());
        }

        let new_size = needed_size.checked_next_power_of_two()? as vk::DeviceSize;
        let create_info = vk::BufferCreateInfo::builder()
            .size(new_size)
            .usage(
                vk::BufferUsageFlags::TRANSFER_SRC
                    | vk::BufferUsageFlags::TRANSFER_DST
                    | vk::BufferUsageFlags::UNIFORM_TEXEL_BUFFER
                    | vk::BufferUsageFlags::STORAGE_TEXEL_BUFFER,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .build();
        let buffer = match self
            .memory_allocator()
            .create_buffer(&create_info, MemoryUsage::DeviceLocal)
        {
            Ok(buffer) => buffer,
            Err(err) => {
                log::warn!(
                    "TextureCacheRuntime::get_temporary_buffer: failed to create {} byte buffer: {:?}",
                    new_size,
                    err
                );
                return None;
            }
        };
        let handle = buffer.handle();
        self.buffers[level] = Some(buffer);
        Some(handle)
    }

    /// Port of `TextureCacheRuntime::GetOrCreateResolveShadow`.
    fn get_or_create_resolve_shadow(
        &mut self,
        msaa_image: vk::Image,
        format: vk::Format,
        extent: vk::Extent2D,
        layers: u32,
    ) -> Result<vk::ImageView, vk::Result> {
        let reusable = self.resolve_shadows.get(&msaa_image).is_some_and(|shadow| {
            shadow.format == format
                && shadow.extent.width == extent.width
                && shadow.extent.height == extent.height
                && shadow.layers == layers
        });
        if reusable {
            let shadow = self.resolve_shadows.get_mut(&msaa_image).unwrap();
            shadow.up_to_date = true;
            return Ok(shadow.view);
        }
        if let Some(old) = self.resolve_shadows.remove(&msaa_image) {
            unsafe { self.device.destroy_image_view(old.view, None) };
        }

        let image_info = vk::ImageCreateInfo::builder()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D {
                width: extent.width,
                height: extent.height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(layers)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(
                vk::ImageUsageFlags::COLOR_ATTACHMENT
                    | vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::TRANSFER_SRC,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .build();
        let image = self
            .memory_allocator()
            .create_owned_image(&image_info)
            .map_err(|error| error.result)?;
        let view_info = vk::ImageViewCreateInfo::builder()
            .image(image.handle())
            .view_type(if layers > 1 {
                vk::ImageViewType::TYPE_2D_ARRAY
            } else {
                vk::ImageViewType::TYPE_2D
            })
            .format(format)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: layers,
            })
            .build();
        let view = unsafe { self.device.create_image_view(&view_info, None)? };
        self.resolve_shadows.insert(
            msaa_image,
            ResolveShadow {
                image,
                view,
                format,
                extent,
                layers,
                up_to_date: true,
            },
        );
        Ok(view)
    }

    fn get_valid_resolve_shadow(&self, msaa_image: vk::Image) -> Option<&ResolveShadow> {
        self.resolve_shadows
            .get(&msaa_image)
            .filter(|shadow| shadow.up_to_date)
    }

    fn invalidate_resolve_shadow(&mut self, msaa_image: vk::Image) {
        if let Some(shadow) = self.resolve_shadows.get_mut(&msaa_image) {
            shadow.up_to_date = false;
        }
    }

    fn erase_resolve_shadow(&mut self, msaa_image: vk::Image) {
        if let Some(shadow) = self.resolve_shadows.remove(&msaa_image) {
            unsafe { self.device.destroy_image_view(shadow.view, None) };
        }
    }

    fn reinterpret_image(&mut self, dst: &Image, src: &Image, copies: &[ImageCopy]) -> bool {
        if ENABLE_MSAA_RESOLVE_CONSUME {
            self.invalidate_resolve_shadow(dst.handle());
        }
        if copies.is_empty() {
            return true;
        }

        let src_aspect = src.aspect_mask();
        let dst_aspect = dst.aspect_mask();
        let bpp_in = crate::surface::bytes_per_block(src.base().info.format)
            / crate::surface::default_block_width(src.base().info.format);
        let bpp_out = crate::surface::bytes_per_block(dst.base().info.format)
            / crate::surface::default_block_width(dst.base().info.format);
        if bpp_in == 0 {
            return false;
        }

        let vk_in_copies = copies
            .iter()
            .map(|copy| {
                let mut adjusted = *copy;
                adjusted.src_offset.x = (bpp_out as i32 * copy.src_offset.x) / bpp_in as i32;
                adjusted.extent.width = (bpp_out * copy.extent.width) / bpp_in;
                make_buffer_image_copy(&adjusted, true, src_aspect)
            })
            .collect::<Vec<_>>();
        let vk_out_copies = copies
            .iter()
            .map(|copy| make_buffer_image_copy(copy, false, dst_aspect))
            .collect::<Vec<_>>();

        let img_bpp = crate::surface::bytes_per_block(dst.base().info.format) as u64;
        let mut total_size = 0u64;
        for copy in copies {
            total_size = total_size.saturating_add(
                copy.extent.width as u64
                    * copy.extent.height as u64
                    * copy.extent.depth as u64
                    * img_bpp,
            );
        }
        let Some(copy_buffer) = self.get_temporary_buffer(total_size as usize) else {
            return false;
        };

        let dst_image = dst.handle();
        let src_image = src.handle();
        let device = self.device.clone();
        let scheduler = self.scheduler();
        scheduler.request_outside_render_pass_operation_context();
        scheduler.record(move |cmd| unsafe {
            let mut dst_range = RangedBarrierRange::default();
            let mut src_range = RangedBarrierRange::default();
            for copy in &vk_in_copies {
                src_range.add_layers(copy.image_subresource);
            }
            for copy in &vk_out_copies {
                dst_range.add_layers(copy.image_subresource);
            }

            let read_barrier = vk::MemoryBarrier::builder()
                .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ | vk::AccessFlags::TRANSFER_WRITE)
                .build();
            let write_barrier = vk::MemoryBarrier::builder()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE)
                .build();
            let pre_barriers = [vk::ImageMemoryBarrier::builder()
                .src_access_mask(
                    vk::AccessFlags::SHADER_WRITE
                        | vk::AccessFlags::COLOR_ATTACHMENT_WRITE
                        | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE
                        | vk::AccessFlags::TRANSFER_WRITE,
                )
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                .old_layout(vk::ImageLayout::GENERAL)
                .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(src_image)
                .subresource_range(src_range.subresource_range(src_aspect))
                .build()];
            let middle_in_barriers = [vk::ImageMemoryBarrier::builder()
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::empty())
                .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(src_image)
                .subresource_range(src_range.subresource_range(src_aspect))
                .build()];
            let middle_out_barriers = [vk::ImageMemoryBarrier::builder()
                .src_access_mask(
                    vk::AccessFlags::SHADER_WRITE
                        | vk::AccessFlags::COLOR_ATTACHMENT_WRITE
                        | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE
                        | vk::AccessFlags::TRANSFER_WRITE,
                )
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .old_layout(vk::ImageLayout::GENERAL)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(dst_image)
                .subresource_range(dst_range.subresource_range(dst_aspect))
                .build()];
            let post_barriers = [vk::ImageMemoryBarrier::builder()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(
                    vk::AccessFlags::SHADER_READ
                        | vk::AccessFlags::SHADER_WRITE
                        | vk::AccessFlags::COLOR_ATTACHMENT_READ
                        | vk::AccessFlags::COLOR_ATTACHMENT_WRITE
                        | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_READ
                        | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE
                        | vk::AccessFlags::TRANSFER_READ
                        | vk::AccessFlags::TRANSFER_WRITE,
                )
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(dst_image)
                .subresource_range(dst_range.subresource_range(dst_aspect))
                .build()];

            device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &pre_barriers,
            );
            device.cmd_copy_image_to_buffer(
                cmd,
                src_image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                copy_buffer,
                &vk_in_copies,
            );
            device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[write_barrier],
                &[],
                &middle_in_barriers,
            );
            device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[read_barrier],
                &[],
                &middle_out_barriers,
            );
            device.cmd_copy_buffer_to_image(
                cmd,
                copy_buffer,
                dst_image,
                vk::ImageLayout::GENERAL,
                &vk_out_copies,
            );
            device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &post_barriers,
            );
        });
        true
    }

    fn copy_image(&mut self, dst: &Image, src: &Image, copies: &[ImageCopy]) {
        if ENABLE_MSAA_RESOLVE_CONSUME {
            self.invalidate_resolve_shadow(dst.handle());
        }
        if copies.is_empty() {
            return;
        }
        // Vulkan only permits direct image copies between size-compatible
        // formats. Match upstream's buffer-backed reinterpretation for the
        // remaining cases instead of issuing an invalid vkCmdCopyImage.
        if crate::surface::bytes_per_block(src.base().info.format)
            != crate::surface::bytes_per_block(dst.base().info.format)
        {
            #[cfg(target_os = "windows")]
            if src.base().info.image_type == ImageType::Linear
                || dst.base().info.image_type == ImageType::Linear
            {
                return;
            }
            let copy = ImageCopy {
                extent: dst.base().info.size,
                ..ImageCopy::default()
            };
            let _ = self.reinterpret_image(dst, src, std::slice::from_ref(&copy));
            return;
        }
        let aspect = dst.aspect_mask();
        debug_assert_eq!(aspect, src.aspect_mask());
        let vk_copies = copies
            .iter()
            .map(|copy| make_image_copy(copy, aspect))
            .collect::<Vec<_>>();
        let dst_image = dst.handle();
        let src_image = src.handle();
        let device = self.device.clone();
        let scheduler = self.scheduler();
        scheduler.request_outside_render_pass_operation_context();
        scheduler.record(move |cmd| unsafe {
            let barriers = make_copy_image_barriers(src_image, dst_image, aspect, &vk_copies);
            device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &barriers.pre,
            );
            device.cmd_copy_image(
                cmd,
                src_image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                dst_image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &vk_copies,
            );
            device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &barriers.post,
            );
        });
    }

    fn blit_image(
        &mut self,
        dst_framebuffer: BlitFramebufferInfo,
        dst: &ImageView,
        src: &mut ImageView,
        dst_region: BlitRegion2D,
        src_region: BlitRegion2D,
        filter: BlitFilter,
        operation: BlitOperation,
    ) -> bool {
        let aspect_mask = image_aspect_mask(src.base().format);
        let is_dst_msaa = dst.samples() != vk::SampleCountFlags::TYPE_1;
        let is_src_msaa = src.samples() != vk::SampleCountFlags::TYPE_1;
        if aspect_mask != image_aspect_mask(dst.base().format) {
            log::warn!(
                "TextureCacheRuntime::blit_image: incompatible blit from {:?} to {:?}",
                src.base().format,
                dst.base().format
            );
            return false;
        }

        if aspect_mask == vk::ImageAspectFlags::COLOR && !is_src_msaa && !is_dst_msaa {
            let src_image_view = blit_image_view_from_backend(src, false);
            return self.blit_image_helper().blit_color(
                dst_framebuffer,
                src_image_view,
                &dst_region,
                &src_region,
                filter,
                operation,
            );
        }

        assert_fail_soft(
            src.base().format == dst.base().format,
            "source and destination blit formats must match",
        );

        if is_src_msaa
            && !is_dst_msaa
            && aspect_mask.intersects(vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL)
        {
            if !aspect_mask.contains(vk::ImageAspectFlags::DEPTH) {
                log::warn!(
                    "TextureCacheRuntime::blit_image: stencil-only MSAA resolve is unsupported"
                );
                return false;
            }
            if src.depth_view().is_err() {
                return false;
            }
            let resolve_stencil =
                dst_framebuffer.has_stencil && self.shader_stencil_export_supported;
            if resolve_stencil && src.stencil_view().is_err() {
                return false;
            }
            let src_image_view = blit_image_view_from_backend(src, false);
            return self.blit_image_helper().resolve_depth_stencil(
                dst_framebuffer,
                src_image_view,
                &dst_region,
                &src_region,
            );
        }

        if aspect_mask == (vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL)
            && !self.is_blit_depth_stencil_supported(src.base().format)
        {
            assert_fail_soft(
                !(is_src_msaa || is_dst_msaa),
                "MSAA depth/stencil helper blit is not implemented",
            );
            if src.depth_view().is_err() || src.stencil_view().is_err() {
                return false;
            }
            let src_image_view = blit_image_view_from_backend(src, false);
            return self.blit_image_helper().blit_depth_stencil(
                dst_framebuffer,
                src_image_view,
                &dst_region,
                &src_region,
                filter,
                operation,
            );
        }

        assert_fail_soft(
            !(is_dst_msaa && !is_src_msaa),
            "non-MSAA to MSAA blit is unsupported",
        );
        assert_fail_soft(
            operation == BlitOperation::SrcCopy,
            "non-shader blits require SrcCopy",
        );

        let is_msaa_to_msaa = is_src_msaa && is_dst_msaa;
        if is_msaa_to_msaa && aspect_mask == vk::ImageAspectFlags::COLOR {
            let src_image_view = blit_image_view_from_backend(src, false);
            return self.blit_image_helper().blit_color_msaa(
                dst_framebuffer,
                src_image_view,
                &dst_region,
                &src_region,
            );
        }
        if is_msaa_to_msaa && self.cant_blit_msaa {
            log::warn!(
                "TextureCacheRuntime::blit_image: MSAA depth/stencil blit is unsupported on this driver"
            );
            return false;
        }

        let dst_image = dst.image_handle();
        let src_image = src.image_handle();
        let dst_layers = make_image_subresource_layers_from_view(dst);
        let src_layers = make_image_subresource_layers_from_view(src);
        let is_resolve = is_src_msaa && !is_dst_msaa;
        let device = self.device.clone();
        let scheduler = self.scheduler();
        scheduler.request_outside_render_pass_operation_context();
        scheduler.record(move |cmd| unsafe {
            let full_range = vk::ImageSubresourceRange {
                aspect_mask,
                base_mip_level: 0,
                level_count: vk::REMAINING_MIP_LEVELS,
                base_array_layer: 0,
                layer_count: vk::REMAINING_ARRAY_LAYERS,
            };
            let read_barriers = [
                vk::ImageMemoryBarrier::builder()
                    .src_access_mask(
                        vk::AccessFlags::SHADER_WRITE
                            | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE
                            | vk::AccessFlags::TRANSFER_WRITE,
                    )
                    .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                    .old_layout(vk::ImageLayout::GENERAL)
                    .new_layout(vk::ImageLayout::GENERAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(src_image)
                    .subresource_range(full_range)
                    .build(),
                vk::ImageMemoryBarrier::builder()
                    .src_access_mask(
                        vk::AccessFlags::SHADER_WRITE
                            | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE
                            | vk::AccessFlags::TRANSFER_WRITE,
                    )
                    .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                    .old_layout(vk::ImageLayout::GENERAL)
                    .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(dst_image)
                    .subresource_range(full_range)
                    .build(),
            ];
            let write_barrier = vk::ImageMemoryBarrier::builder()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(
                    vk::AccessFlags::SHADER_READ
                        | vk::AccessFlags::SHADER_WRITE
                        | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_READ
                        | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE
                        | vk::AccessFlags::TRANSFER_READ
                        | vk::AccessFlags::TRANSFER_WRITE,
                )
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(dst_image)
                .subresource_range(full_range)
                .build();
            device.cmd_pipeline_barrier(
                cmd,
                PIPELINE_STAGE_GRAPHICS_COMPUTE_TRANSFER,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &read_barriers,
            );
            if is_resolve {
                device.cmd_resolve_image(
                    cmd,
                    src_image,
                    vk::ImageLayout::GENERAL,
                    dst_image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[make_image_resolve(
                        dst_region, src_region, dst_layers, src_layers,
                    )],
                );
            } else {
                let vk_filter = if filter == BlitFilter::Bilinear {
                    vk::Filter::LINEAR
                } else {
                    vk::Filter::NEAREST
                };
                device.cmd_blit_image(
                    cmd,
                    src_image,
                    vk::ImageLayout::GENERAL,
                    dst_image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[make_image_blit(
                        dst_region, src_region, dst_layers, src_layers,
                    )],
                    vk_filter,
                );
            }
            device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TRANSFER,
                PIPELINE_STAGE_GRAPHICS_COMPUTE,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[write_barrier],
            );
        });
        true
    }

    fn convert_image(
        &mut self,
        dst_framebuffer: BlitFramebufferInfo,
        dst_format: PixelFormat,
        src_format: PixelFormat,
        src_view: BlitImageView,
    ) -> bool {
        if src_format == PixelFormat::D32Float
            && color_blit_from_d32_destination(dst_format)
            && (dst_format == PixelFormat::B5G6R5Unorm
                || *common::settings::values().fix_bloom_effects.get_value())
        {
            let region = BlitRegion2D {
                start: BlitOffset2D { x: 0, y: 0 },
                end: BlitOffset2D {
                    x: dst_framebuffer.render_area.width as i32,
                    y: dst_framebuffer.render_area.height as i32,
                },
            };
            return self.blit_image_helper().blit_color(
                dst_framebuffer,
                src_view,
                &region,
                &region,
                BlitFilter::Point,
                BlitOperation::SrcCopy,
            );
        }
        match dst_format {
            PixelFormat::R16Unorm if src_format == PixelFormat::D16Unorm => self
                .blit_image_helper()
                .convert_d16_to_r16(dst_framebuffer, src_view),
            PixelFormat::A8B8G8R8Srgb if src_format == PixelFormat::D32Float => self
                .blit_image_helper()
                .convert_d32f_to_abgr8(dst_framebuffer, src_view),
            PixelFormat::A8B8G8R8Unorm if src_format == PixelFormat::S8UintD24Unorm => self
                .blit_image_helper()
                .convert_d24s8_to_abgr8(dst_framebuffer, src_view),
            PixelFormat::A8B8G8R8Unorm if src_format == PixelFormat::D24UnormS8Uint => self
                .blit_image_helper()
                .convert_s8d24_to_abgr8(dst_framebuffer, src_view),
            PixelFormat::A8B8G8R8Unorm if src_format == PixelFormat::D32Float => self
                .blit_image_helper()
                .convert_d32f_to_abgr8(dst_framebuffer, src_view),
            PixelFormat::B8G8R8A8Srgb if src_format == PixelFormat::D32Float => self
                .blit_image_helper()
                .convert_d32f_to_abgr8(dst_framebuffer, src_view),
            PixelFormat::B8G8R8A8Unorm if src_format == PixelFormat::D32Float => self
                .blit_image_helper()
                .convert_d32f_to_abgr8(dst_framebuffer, src_view),
            PixelFormat::R32Float if src_format == PixelFormat::D32Float => self
                .blit_image_helper()
                .convert_d32_to_r32(dst_framebuffer, src_view),
            PixelFormat::D16Unorm if src_format == PixelFormat::R16Unorm => self
                .blit_image_helper()
                .convert_r16_to_d16(dst_framebuffer, src_view),
            PixelFormat::S8UintD24Unorm
                if src_format == PixelFormat::A8B8G8R8Unorm
                    || src_format == PixelFormat::B8G8R8A8Unorm =>
            {
                self.blit_image_helper()
                    .convert_abgr8_to_d24s8(dst_framebuffer, src_view)
            }
            PixelFormat::D32Float
                if src_format == PixelFormat::A8B8G8R8Unorm
                    || src_format == PixelFormat::B8G8R8A8Unorm
                    || src_format == PixelFormat::A8B8G8R8Srgb
                    || src_format == PixelFormat::B8G8R8A8Srgb =>
            {
                self.blit_image_helper()
                    .convert_abgr8_to_d32f(dst_framebuffer, src_view)
            }
            PixelFormat::D32Float if src_format == PixelFormat::R32Float => self
                .blit_image_helper()
                .convert_r32_to_d32(dst_framebuffer, src_view),
            _ => {
                log::warn!(
                    "TextureCacheRuntime::convert_image: unimplemented format copy from {:?} to {:?}",
                    src_format,
                    dst_format
                );
                false
            }
        }
    }

    fn is_blit_depth_stencil_supported(&self, format: PixelFormat) -> bool {
        match format {
            PixelFormat::D24UnormS8Uint | PixelFormat::S8UintD24Unorm => self.is_format_supported(
                vk::Format::D24_UNORM_S8_UINT,
                vk::FormatFeatureFlags::BLIT_SRC | vk::FormatFeatureFlags::BLIT_DST,
                true,
            ),
            PixelFormat::D32FloatS8Uint => self.is_format_supported(
                vk::Format::D32_SFLOAT_S8_UINT,
                vk::FormatFeatureFlags::BLIT_SRC | vk::FormatFeatureFlags::BLIT_DST,
                true,
            ),
            _ => true,
        }
    }

    fn is_format_supported(
        &self,
        format: vk::Format,
        usage: vk::FormatFeatureFlags,
        optimal: bool,
    ) -> bool {
        // `vkGetPhysicalDeviceFormatProperties` is a driver call and this
        // runs several times per draw through `surface_format`. Upstream
        // caches format properties in `Device`; do the same here.
        let props = *self
            .format_properties
            .borrow_mut()
            .entry(format)
            .or_insert_with(|| unsafe {
                self.instance
                    .get_physical_device_format_properties(self.physical_device, format)
            });
        let supported = if optimal {
            props.optimal_tiling_features
        } else {
            props.linear_tiling_features
        };
        supported.contains(usage)
    }

    /// Port of `MaxwellToVK::SurfaceFormat(device, FormatType::Optimal, ...)`.
    ///
    /// The static table gives the guest's preferred Vulkan format, but the
    /// actual image/view format must be selected through the device's supported
    /// alternatives. This matters on MoltenVK where D24S8 is not natively
    /// supported and must resolve to D32S8 before image/view creation.
    fn surface_format_info(
        &self,
        format: PixelFormat,
        with_srgb: bool,
    ) -> maxwell_to_vk::FormatInfo {
        maxwell_to_vk::surface_format(self.vulkan_device(), FormatType::Optimal, with_srgb, format)
    }

    fn surface_format(&self, format: PixelFormat, with_srgb: bool) -> vk::Format {
        self.surface_format_info(format, with_srgb).format
    }

    fn needs_scale_helper(&self, info: &ImageInfo, format: vk::Format) -> bool {
        if info.num_samples > 1
            && (self.cant_blit_msaa
                || image_aspect_mask(info.format) == vk::ImageAspectFlags::COLOR)
        {
            return true;
        }
        let blit_usage = vk::FormatFeatureFlags::BLIT_SRC | vk::FormatFeatureFlags::BLIT_DST;
        !self.is_format_supported(format, blit_usage, true)
    }

    fn storage_image_view_with_format(
        &self,
        image: &mut Image,
        level: u32,
        format: vk::Format,
    ) -> Result<vk::ImageView, vk::Result> {
        let index = level as usize;
        if index >= image.storage_image_views.len() {
            return Err(vk::Result::ERROR_INITIALIZATION_FAILED);
        }
        if image.storage_image_views[index] != vk::ImageView::null() {
            return Ok(image.storage_image_views[index]);
        }

        let mut usage_info = vk::ImageViewUsageCreateInfo::builder()
            .usage(vk::ImageUsageFlags::STORAGE)
            .build();
        let view_info = vk::ImageViewCreateInfo::builder()
            .push_next(&mut usage_info)
            .image(image.handle())
            .view_type(vk::ImageViewType::TYPE_2D_ARRAY)
            .format(format)
            .components(vk::ComponentMapping {
                r: vk::ComponentSwizzle::IDENTITY,
                g: vk::ComponentSwizzle::IDENTITY,
                b: vk::ComponentSwizzle::IDENTITY,
                a: vk::ComponentSwizzle::IDENTITY,
            })
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: level,
                level_count: 1,
                base_array_layer: 0,
                layer_count: vk::REMAINING_ARRAY_LAYERS,
            })
            .build();
        let view = unsafe { self.device.create_image_view(&view_info, None)? };
        image.storage_image_views[index] = view;
        Ok(view)
    }

    fn copy_image_msaa(&mut self, dst: &mut Image, src: &mut Image, copies: &[ImageCopy]) -> bool {
        if copies.is_empty() {
            return true;
        }
        let msaa_to_non_msaa = src.base().info.num_samples > 1 && dst.base().info.num_samples == 1;
        let num_samples = if msaa_to_non_msaa {
            src.base().info.num_samples
        } else {
            dst.base().info.num_samples
        };
        if dst.aspect_mask() != vk::ImageAspectFlags::COLOR
            || crate::surface::is_pixel_format_integer(dst.base().info.format)
        {
            log::warn!("Copying images with different samples is not supported");
            return false;
        }
        if ENABLE_MSAA_RESOLVE_CONSUME
            && msaa_to_non_msaa
            && copies.len() == 1
            && src.base().info.format == dst.base().info.format
        {
            let copy = copies[0];
            let shadow_image = self
                .get_valid_resolve_shadow(src.handle())
                .filter(|shadow| {
                    copy.src_offset.x == 0
                        && copy.src_offset.y == 0
                        && copy.src_subresource.base_level == 0
                        && copy.extent.width <= shadow.extent.width
                        && copy.extent.height <= shadow.extent.height
                })
                .map(|shadow| shadow.image.handle());
            if let Some(shadow_image) = shadow_image {
                let dst_image = dst.handle();
                let region = vk::ImageCopy {
                    src_subresource: vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: copy.src_subresource.base_layer as u32,
                        layer_count: copy.src_subresource.num_layers as u32,
                    },
                    src_offset: vk::Offset3D { x: 0, y: 0, z: 0 },
                    dst_subresource: vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: copy.dst_subresource.base_level as u32,
                        base_array_layer: copy.dst_subresource.base_layer as u32,
                        layer_count: copy.dst_subresource.num_layers as u32,
                    },
                    dst_offset: vk::Offset3D {
                        x: copy.dst_offset.x,
                        y: copy.dst_offset.y,
                        z: copy.dst_offset.z,
                    },
                    extent: vk::Extent3D {
                        width: copy.extent.width,
                        height: copy.extent.height,
                        depth: 1,
                    },
                };
                let device = self.device.clone();
                let scheduler = self.scheduler();
                scheduler.request_outside_render_pass_operation_context();
                scheduler.record(move |cmd| unsafe {
                    let full_color_range = vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        base_mip_level: 0,
                        level_count: vk::REMAINING_MIP_LEVELS,
                        base_array_layer: 0,
                        layer_count: vk::REMAINING_ARRAY_LAYERS,
                    };
                    let pre_barriers = [
                        vk::ImageMemoryBarrier::builder()
                            .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
                            .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                            .old_layout(vk::ImageLayout::GENERAL)
                            .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                            .image(shadow_image)
                            .subresource_range(full_color_range)
                            .build(),
                        vk::ImageMemoryBarrier::builder()
                            .src_access_mask(
                                vk::AccessFlags::SHADER_WRITE
                                    | vk::AccessFlags::COLOR_ATTACHMENT_WRITE
                                    | vk::AccessFlags::TRANSFER_WRITE,
                            )
                            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                            .old_layout(vk::ImageLayout::GENERAL)
                            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                            .image(dst_image)
                            .subresource_range(full_color_range)
                            .build(),
                    ];
                    let post_barriers = [
                        vk::ImageMemoryBarrier::builder()
                            .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                            .new_layout(vk::ImageLayout::GENERAL)
                            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                            .image(shadow_image)
                            .subresource_range(full_color_range)
                            .build(),
                        vk::ImageMemoryBarrier::builder()
                            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                            .dst_access_mask(
                                vk::AccessFlags::SHADER_READ
                                    | vk::AccessFlags::COLOR_ATTACHMENT_READ
                                    | vk::AccessFlags::COLOR_ATTACHMENT_WRITE
                                    | vk::AccessFlags::TRANSFER_READ
                                    | vk::AccessFlags::TRANSFER_WRITE,
                            )
                            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                            .new_layout(vk::ImageLayout::GENERAL)
                            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                            .image(dst_image)
                            .subresource_range(full_color_range)
                            .build(),
                    ];
                    device.cmd_pipeline_barrier(
                        cmd,
                        vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
                            | vk::PipelineStageFlags::TRANSFER,
                        vk::PipelineStageFlags::TRANSFER,
                        vk::DependencyFlags::empty(),
                        &[],
                        &[],
                        &pre_barriers,
                    );
                    device.cmd_copy_image(
                        cmd,
                        shadow_image,
                        vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                        dst_image,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        &[region],
                    );
                    device.cmd_pipeline_barrier(
                        cmd,
                        vk::PipelineStageFlags::TRANSFER,
                        vk::PipelineStageFlags::ALL_COMMANDS,
                        vk::DependencyFlags::empty(),
                        &[],
                        &[],
                        &post_barriers,
                    );
                });
                return true;
            }
        }
        unsafe {
            self.blit_image_helper.as_mut().copy_msaa(
                self.render_pass_cache.as_ref(),
                dst.handle(),
                dst.base().info.format,
                src.handle(),
                src.base().info.format,
                num_samples,
                copies,
                msaa_to_non_msaa,
            )
        }
    }

    fn create_scaled_image(
        &mut self,
        info: &ImageInfo,
        _format: vk::Format,
    ) -> Result<AllocatedImage, vk::Result> {
        let is_2d = info.image_type == ImageType::E2D;
        let mut scaled_info = info.clone();
        scaled_info.size.width = self.resolution.scale_up_u32(info.size.width);
        if is_2d {
            scaled_info.size.height = self.resolution.scale_up_u32(info.size.height);
        }
        self.create_image_from_info(&scaled_info)
    }

    fn blit_scale(
        &mut self,
        src_image: vk::Image,
        dst_image: vk::Image,
        info: ImageInfo,
        aspect_mask: vk::ImageAspectFlags,
        up_scaling: bool,
    ) {
        let is_2d = info.image_type == ImageType::E2D;
        let resources = info.resources;
        let extent = vk::Extent2D {
            width: info.size.width,
            height: info.size.height,
        };
        let is_color = aspect_mask == vk::ImageAspectFlags::COLOR;
        let is_bilinear = is_color && !crate::surface::is_pixel_format_integer(info.format);
        let vk_filter = if is_bilinear {
            vk::Filter::LINEAR
        } else {
            vk::Filter::NEAREST
        };
        let resolution = self.resolution.clone();
        let device = self.device.clone();
        let scheduler = self.scheduler();
        scheduler.request_outside_render_pass_operation_context();
        scheduler.record(move |cmd| unsafe {
            let src_size = vk::Offset2D {
                x: if up_scaling {
                    extent.width as i32
                } else {
                    resolution.scale_up_i32(extent.width as i32)
                },
                y: if is_2d && up_scaling {
                    extent.height as i32
                } else {
                    resolution.scale_up_i32(extent.height as i32)
                },
            };
            let dst_size = vk::Offset2D {
                x: if up_scaling {
                    resolution.scale_up_i32(extent.width as i32)
                } else {
                    extent.width as i32
                },
                y: if is_2d && up_scaling {
                    resolution.scale_up_i32(extent.height as i32)
                } else {
                    extent.height as i32
                },
            };
            let mut regions = Vec::with_capacity(resources.levels.max(1) as usize);
            for level in 0..resources.levels.max(1) {
                regions.push(vk::ImageBlit {
                    src_subresource: vk::ImageSubresourceLayers {
                        aspect_mask,
                        mip_level: level as u32,
                        base_array_layer: 0,
                        layer_count: resources.layers.max(1) as u32,
                    },
                    src_offsets: [
                        vk::Offset3D { x: 0, y: 0, z: 0 },
                        vk::Offset3D {
                            x: (src_size.x >> level).max(1),
                            y: (src_size.y >> level).max(1),
                            z: 1,
                        },
                    ],
                    dst_subresource: vk::ImageSubresourceLayers {
                        aspect_mask,
                        mip_level: level as u32,
                        base_array_layer: 0,
                        layer_count: resources.layers.max(1) as u32,
                    },
                    dst_offsets: [
                        vk::Offset3D { x: 0, y: 0, z: 0 },
                        vk::Offset3D {
                            x: (dst_size.x >> level).max(1),
                            y: (dst_size.y >> level).max(1),
                            z: 1,
                        },
                    ],
                });
            }
            let subresource_range = vk::ImageSubresourceRange {
                aspect_mask,
                base_mip_level: 0,
                level_count: vk::REMAINING_MIP_LEVELS,
                base_array_layer: 0,
                layer_count: vk::REMAINING_ARRAY_LAYERS,
            };
            let read_barriers = [
                vk::ImageMemoryBarrier::builder()
                    .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
                    .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                    .old_layout(vk::ImageLayout::GENERAL)
                    .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(src_image)
                    .subresource_range(subresource_range)
                    .build(),
                vk::ImageMemoryBarrier::builder()
                    .src_access_mask(
                        vk::AccessFlags::SHADER_WRITE
                            | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE
                            | vk::AccessFlags::TRANSFER_WRITE,
                    )
                    .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                    .old_layout(vk::ImageLayout::UNDEFINED)
                    .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(dst_image)
                    .subresource_range(subresource_range)
                    .build(),
            ];
            let write_barriers = [
                vk::ImageMemoryBarrier::builder()
                    .src_access_mask(vk::AccessFlags::empty())
                    .dst_access_mask(vk::AccessFlags::MEMORY_WRITE | vk::AccessFlags::MEMORY_READ)
                    .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                    .new_layout(vk::ImageLayout::GENERAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(src_image)
                    .subresource_range(subresource_range)
                    .build(),
                vk::ImageMemoryBarrier::builder()
                    .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                    .dst_access_mask(vk::AccessFlags::MEMORY_WRITE | vk::AccessFlags::MEMORY_READ)
                    .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                    .new_layout(vk::ImageLayout::GENERAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(dst_image)
                    .subresource_range(subresource_range)
                    .build(),
            ];
            device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &read_barriers,
            );
            device.cmd_blit_image(
                cmd,
                src_image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                dst_image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &regions,
                vk_filter,
            );
            device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &write_barriers,
            );
        });
    }

    fn create_image_from_info(&mut self, info: &ImageInfo) -> Result<AllocatedImage, vk::Result> {
        let format_info = self.surface_format_info(info.format, false);
        let mut image_info = make_image_create_info(info, format_info);
        let view_formats = self.view_formats[info.format as usize].clone();
        let mut format_list = vk::ImageFormatListCreateInfo::builder()
            .view_formats(&view_formats)
            .build();
        apply_image_format_list(
            &mut image_info,
            &mut format_list,
            &view_formats,
            self.image_format_list_supported,
        );
        self.memory_allocator()
            .create_owned_image(&image_info)
            .map_err(|err| err.result)
    }

    fn create_msaa_upload_image(&mut self, info: &ImageInfo) -> Result<AllocatedImage, vk::Result> {
        let format_info = self.surface_format_info(info.format, true);
        let mut image_info = make_image_create_info(info, format_info);
        image_info.format = format_info.format;
        image_info.usage = vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED;
        self.memory_allocator()
            .create_owned_image(&image_info)
            .map_err(|err| err.result)
    }

    fn copy_msaa_upload(
        &mut self,
        dst_image: vk::Image,
        dst_format: PixelFormat,
        src_image: vk::Image,
        src_format: PixelFormat,
        num_samples: u32,
        copies: &[ImageCopy],
    ) -> bool {
        // SAFETY: both pointers refer to independently boxed services owned by
        // RasterizerVulkan and remain valid for the runtime lifetime.
        unsafe {
            self.blit_image_helper.as_mut().copy_msaa(
                self.render_pass_cache.as_ref(),
                dst_image,
                dst_format,
                src_image,
                src_format,
                num_samples,
                copies,
                false,
            )
        }
    }

    fn keep_msaa_upload_image_alive(&mut self, image: AllocatedImage) {
        let tick = self.scheduler().current_tick();
        self.pending_msaa_images.push((tick, image));
    }

    /// Port of `Vulkan::ImageView(TextureCacheRuntime&, NullImageViewParams)`.
    fn make_null_image_view(
        &mut self,
        base: NonNull<ImageViewBase>,
    ) -> Result<ImageView, vk::Result> {
        if self.has_null_descriptor {
            return Ok(ImageView {
                vulkan_device: self.device_owner,
                device: self.device.clone(),
                base,
                image_handle: vk::Image::null(),
                image_views: [vk::ImageView::null();
                    shader_recompiler::shader_info::NUM_TEXTURE_TYPES as usize],
                render_target: vk::ImageView::null(),
                typeless_storage_view: vk::ImageView::null(),
                depth_view: vk::ImageView::null(),
                stencil_view: vk::ImageView::null(),
                color_view: vk::ImageView::null(),
                storage_signeds: [vk::ImageView::null();
                    shader_recompiler::shader_info::NUM_TEXTURE_TYPES as usize],
                storage_unsigneds: [vk::ImageView::null();
                    shader_recompiler::shader_info::NUM_TEXTURE_TYPES as usize],
                null_image: None,
                samples: vk::SampleCountFlags::TYPE_1,
                buffer_size: 0,
                supports_depth_comparison: false,
            });
        }

        let info = null_image_info();
        let format_info = self.surface_format_info(info.format, false);
        let image_info = make_image_create_info(&info, format_info);
        // Upstream passes an empty view-format span for the fallback image.
        let null_image = self
            .memory_allocator()
            .create_owned_image(&image_info)
            .map_err(|err| err.result)?;
        let image_handle = null_image.handle();
        let mut view = ImageView {
            vulkan_device: self.device_owner,
            device: self.device.clone(),
            base,
            image_handle,
            image_views: [vk::ImageView::null();
                shader_recompiler::shader_info::NUM_TEXTURE_TYPES as usize],
            render_target: vk::ImageView::null(),
            typeless_storage_view: vk::ImageView::null(),
            depth_view: vk::ImageView::null(),
            stencil_view: vk::ImageView::null(),
            color_view: vk::ImageView::null(),
            storage_signeds: [vk::ImageView::null();
                shader_recompiler::shader_info::NUM_TEXTURE_TYPES as usize],
            storage_unsigneds: [vk::ImageView::null();
                shader_recompiler::shader_info::NUM_TEXTURE_TYPES as usize],
            null_image: Some(null_image),
            samples: vk::SampleCountFlags::TYPE_1,
            buffer_size: 0,
            supports_depth_comparison: false,
        };
        for index in 0..view.image_views.len() {
            match view.make_view(
                vk::Format::A8B8G8R8_UNORM_PACK32,
                vk::ImageAspectFlags::COLOR,
                None,
            ) {
                Ok(image_view) => view.image_views[index] = image_view,
                Err(err) => {
                    unsafe {
                        for &image_view in &view.image_views {
                            if image_view != vk::ImageView::null() {
                                self.device.destroy_image_view(image_view, None);
                            }
                        }
                    }
                    return Err(err);
                }
            }
        }
        Ok(view)
    }

    fn make_image_view(
        &self,
        _view_id: ImageViewId,
        info: &ImageViewInfo,
        view_base: NonNull<ImageViewBase>,
        image: &Image,
    ) -> Result<ImageView, vk::Result> {
        // SAFETY: the pointer is the stable boxed base of the slot receiving
        // the returned payload.
        let view_base = unsafe { view_base.as_ref() };
        let format_info = self.surface_format_info(view_base.format, true);
        let format = format_info.format;
        let aspect_mask = image_view_aspect_mask(info);
        let components = image_view_components(
            info,
            aspect_mask,
            self.must_emulate_bgr565,
            self.ext_4444_formats_supported,
            self.vulkan_device().supports_depth_stencil_swizzle_one(),
        );
        let base_range = make_subresource_range(aspect_mask, view_base.range, view_base.flags);
        let image_format_info = self.surface_format_info(image.base().info.format, false);
        let usage = image_view_usage_flags(
            format_info,
            view_base.format,
            image_format_info,
            image.base().info.format,
        );
        let mut image_views =
            [vk::ImageView::null(); shader_recompiler::shader_info::NUM_TEXTURE_TYPES as usize];

        let create = |texture_type: TextureType,
                      layer_count: Option<u32>|
         -> Result<vk::ImageView, vk::Result> {
            let mut range = base_range;
            if let Some(layer_count) = layer_count {
                range.layer_count = layer_count;
            }
            let mut usage_info = vk::ImageViewUsageCreateInfo::builder().usage(usage).build();
            let view_info = vk::ImageViewCreateInfo::builder()
                .push_next(&mut usage_info)
                .image(image.handle())
                .view_type(image_view_type_from_texture_type(texture_type))
                .format(format)
                .components(components)
                .subresource_range(range)
                .build();
            unsafe { self.device.create_image_view(&view_info, None) }
        };

        let render_target = match view_base.view_type {
            crate::texture_cache::types::ImageViewType::E1D
            | crate::texture_cache::types::ImageViewType::E1DArray => {
                image_views[TextureType::Color1D as usize] = create(TextureType::Color1D, Some(1))?;
                image_views[TextureType::ColorArray1D as usize] =
                    create(TextureType::ColorArray1D, None)?;
                image_views[TextureType::ColorArray1D as usize]
            }
            crate::texture_cache::types::ImageViewType::E2D
            | crate::texture_cache::types::ImageViewType::E2DArray
            | crate::texture_cache::types::ImageViewType::Rect => {
                image_views[TextureType::Color2D as usize] = create(TextureType::Color2D, Some(1))?;
                image_views[TextureType::Color2DRect as usize] =
                    image_views[TextureType::Color2D as usize];
                image_views[TextureType::ColorArray2D as usize] =
                    create(TextureType::ColorArray2D, None)?;
                image_views[TextureType::ColorArray2D as usize]
            }
            crate::texture_cache::types::ImageViewType::E3D => {
                image_views[TextureType::Color3D as usize] = create(TextureType::Color3D, None)?;
                image_views[TextureType::Color3D as usize]
            }
            crate::texture_cache::types::ImageViewType::Cube
            | crate::texture_cache::types::ImageViewType::CubeArray => {
                image_views[TextureType::ColorCube as usize] =
                    create(TextureType::ColorCube, Some(6))?;
                image_views[TextureType::ColorArrayCube as usize] =
                    create(TextureType::ColorArrayCube, None)?;
                image_views[TextureType::ColorArrayCube as usize]
            }
            crate::texture_cache::types::ImageViewType::Buffer => vk::ImageView::null(),
        };

        Ok(ImageView {
            vulkan_device: self.device_owner,
            device: self.device.clone(),
            base: NonNull::from(view_base),
            image_handle: image.handle(),
            image_views,
            render_target,
            typeless_storage_view: vk::ImageView::null(),
            depth_view: vk::ImageView::null(),
            stencil_view: vk::ImageView::null(),
            color_view: vk::ImageView::null(),
            storage_signeds: [vk::ImageView::null();
                shader_recompiler::shader_info::NUM_TEXTURE_TYPES as usize],
            storage_unsigneds: [vk::ImageView::null();
                shader_recompiler::shader_info::NUM_TEXTURE_TYPES as usize],
            null_image: None,
            samples: convert_sample_count(image.base().info.num_samples),
            buffer_size: image.base().guest_size_bytes,
            supports_depth_comparison: self.supports_depth_comparison(format),
        })
    }

    /// Port of the Vulkan `ImageView(runtime, info, view_info, gpu_addr)`
    /// constructor used for texture buffers.
    fn make_buffer_image_view(
        &self,
        _view_id: ImageViewId,
        _info: &ImageViewInfo,
        base: NonNull<ImageViewBase>,
    ) -> ImageView {
        // SAFETY: the pointer is owned by the typed view slot.
        let base_ref = unsafe { base.as_ref() };
        ImageView {
            vulkan_device: self.device_owner,
            device: self.device.clone(),
            base,
            image_handle: vk::Image::null(),
            image_views: [vk::ImageView::null();
                shader_recompiler::shader_info::NUM_TEXTURE_TYPES as usize],
            render_target: vk::ImageView::null(),
            typeless_storage_view: vk::ImageView::null(),
            depth_view: vk::ImageView::null(),
            stencil_view: vk::ImageView::null(),
            color_view: vk::ImageView::null(),
            storage_signeds: [vk::ImageView::null();
                shader_recompiler::shader_info::NUM_TEXTURE_TYPES as usize],
            storage_unsigneds: [vk::ImageView::null();
                shader_recompiler::shader_info::NUM_TEXTURE_TYPES as usize],
            null_image: None,
            samples: vk::SampleCountFlags::TYPE_1,
            buffer_size: base_ref
                .size
                .width
                .wrapping_mul(crate::surface::bytes_per_block(base_ref.format)),
            supports_depth_comparison: false,
        }
    }

    /// Port of the Vulkan 1.3 format-feature check in `Vulkan::ImageView`.
    fn supports_depth_comparison(&self, format: vk::Format) -> bool {
        let properties = unsafe {
            self.instance
                .get_physical_device_properties(self.physical_device)
        };
        if properties.api_version < vk::API_VERSION_1_3 {
            return true;
        }
        let mut properties3 = vk::FormatProperties3::default();
        let mut properties2 = vk::FormatProperties2::builder()
            .push_next(&mut properties3)
            .build();
        unsafe {
            self.instance.get_physical_device_format_properties2(
                self.physical_device,
                format,
                &mut properties2,
            );
        }
        properties3
            .optimal_tiling_features
            .contains(vk::FormatFeatureFlags2::SAMPLED_IMAGE_DEPTH_COMPARISON)
    }

    fn create_framebuffer(
        &self,
        render_pass: vk::RenderPass,
        attachments: &[vk::ImageView],
        extent: vk::Extent2D,
    ) -> Result<vk::Framebuffer, vk::Result> {
        let fb_info = vk::FramebufferCreateInfo::builder()
            .render_pass(render_pass)
            .attachments(attachments)
            .width(extent.width)
            .height(extent.height)
            .layers(1)
            .build();
        unsafe { self.device.create_framebuffer(&fb_info, None) }
    }

    fn create_blit_color_view(
        &self,
        image: vk::Image,
        format: vk::Format,
    ) -> Result<vk::ImageView, vk::Result> {
        self.create_blit_image_view(image, format, vk::ImageAspectFlags::COLOR)
    }

    fn create_blit_image_view(
        &self,
        image: vk::Image,
        format: vk::Format,
        aspect_mask: vk::ImageAspectFlags,
    ) -> Result<vk::ImageView, vk::Result> {
        let view_info = vk::ImageViewCreateInfo::builder()
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
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            })
            .build();
        unsafe { self.device.create_image_view(&view_info, None) }
    }

    fn create_blit_color_framebuffer(
        &mut self,
        image: vk::Image,
        view: vk::ImageView,
        format: PixelFormat,
        extent: vk::Extent2D,
        samples: vk::SampleCountFlags,
    ) -> Result<BlitFramebufferInfo, vk::Result> {
        let mut rp_key = RenderPassKey::default();
        rp_key.color_formats[0] = format;
        rp_key.samples = samples;
        let render_pass = self.render_pass_cache().get(&rp_key)?;
        let framebuffer = self.create_framebuffer(render_pass, &[view], extent)?;
        let mut images = [vk::Image::null(); NUM_RT + 1];
        images[0] = image;
        let mut image_ranges = [vk::ImageSubresourceRange::default(); NUM_RT + 1];
        image_ranges[0] = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };
        Ok(BlitFramebufferInfo {
            framebuffer,
            render_pass,
            render_area: extent,
            images,
            image_ranges,
            num_images: 1,
            samples,
            has_stencil: false,
        })
    }

    fn create_blit_depth_stencil_framebuffer(
        &mut self,
        image: vk::Image,
        view: vk::ImageView,
        format: PixelFormat,
        extent: vk::Extent2D,
    ) -> Result<BlitFramebufferInfo, vk::Result> {
        let rp_key = RenderPassKey {
            depth_format: format,
            samples: vk::SampleCountFlags::TYPE_1,
            ..RenderPassKey::default()
        };
        let render_pass = self.render_pass_cache().get(&rp_key)?;
        let framebuffer = self.create_framebuffer(render_pass, &[view], extent)?;
        let mut images = [vk::Image::null(); NUM_RT + 1];
        images[0] = image;
        let mut image_ranges = [vk::ImageSubresourceRange::default(); NUM_RT + 1];
        image_ranges[0] = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };
        Ok(BlitFramebufferInfo {
            framebuffer,
            render_pass,
            render_area: extent,
            images,
            image_ranges,
            num_images: 1,
            samples: vk::SampleCountFlags::TYPE_1,
            has_stencil: image_aspect_mask(format).contains(vk::ImageAspectFlags::STENCIL),
        })
    }

    fn destroy_image(&mut self, mut image: Image) {
        image.destroy_runtime_resources(self);
    }

    fn destroy_image_view(&self, view: ImageView) {
        drop(view);
    }

    fn destroy_framebuffer_owner(&self, framebuffer: Box<Framebuffer>) {
        drop(framebuffer);
    }

    fn destroy_sampler(&self, sampler: CachedSampler) {
        drop(sampler);
    }

    fn sentence_framebuffer(&mut self, framebuffer: Box<Framebuffer>) {
        self.sentence_resource(DeferredVkResource::Framebuffer(framebuffer));
    }

    fn sentence_image_view(&mut self, view: ImageView) {
        self.sentence_resource(DeferredVkResource::ImageView(view));
    }

    fn sentence_image(&mut self, image: Image) {
        self.sentence_resource(DeferredVkResource::Image(image));
    }

    fn sentence_resource(&mut self, resource: DeferredVkResource) {
        // The last submission that can reference the resource is the pending
        // tick (the flush that will carry the currently recorded chunk).
        // Retire once the GPU (timeline counter) passes it — the submission
        // counter itself runs ahead of the GPU with pipelined submits.
        let retire_tick = self.scheduler().current_tick();
        self.sentenced_resources.push(SentencedVkResource {
            retire_tick,
            resource,
        });
    }

    /// `gpu_tick` is `Scheduler::known_gpu_tick()` — the last tick the GPU
    /// has fully completed.
    fn tick_frame(&mut self, gpu_tick: u64) {
        let scheduler_tick = gpu_tick;
        self.current_tick = scheduler_tick;
        self.pending_msaa_images
            .retain(|(retire_tick, _)| *retire_tick > gpu_tick);
        let mut retained = Vec::with_capacity(self.sentenced_resources.len());
        let mut ready = Vec::new();
        for sentenced in self.sentenced_resources.drain(..) {
            if sentenced.retire_tick <= scheduler_tick {
                ready.push(sentenced.resource);
            } else {
                retained.push(sentenced);
            }
        }
        self.sentenced_resources = retained;
        for resource in ready {
            match resource {
                DeferredVkResource::Framebuffer(framebuffer) => {
                    self.destroy_framebuffer_owner(framebuffer)
                }
                DeferredVkResource::ImageView(view) => self.destroy_image_view(view),
                DeferredVkResource::Image(image) => self.destroy_image(image),
            }
        }
    }
}

fn color_blit_from_d32_destination(format: PixelFormat) -> bool {
    matches!(
        format,
        PixelFormat::A8B8G8R8Unorm
            | PixelFormat::A8B8G8R8Snorm
            | PixelFormat::A8B8G8R8Sint
            | PixelFormat::A8B8G8R8Uint
            | PixelFormat::R5G6B5Unorm
            | PixelFormat::B5G6R5Unorm
            | PixelFormat::A1R5G5B5Unorm
            | PixelFormat::A2B10G10R10Unorm
            | PixelFormat::A2B10G10R10Uint
            | PixelFormat::A2R10G10B10Unorm
            | PixelFormat::A1B5G5R5Unorm
            | PixelFormat::A5B5G5R1Unorm
            | PixelFormat::R8Unorm
            | PixelFormat::R8Snorm
            | PixelFormat::R8Sint
            | PixelFormat::R8Uint
            | PixelFormat::R16G16B16A16Float
            | PixelFormat::R16G16B16A16Unorm
            | PixelFormat::R16G16B16A16Snorm
            | PixelFormat::R16G16B16A16Sint
            | PixelFormat::R16G16B16A16Uint
            | PixelFormat::B10G11R11Float
            | PixelFormat::R32G32B32A32Uint
            | PixelFormat::Bc1RgbaUnorm
            | PixelFormat::Bc2Unorm
            | PixelFormat::Bc3Unorm
            | PixelFormat::Bc4Unorm
            | PixelFormat::Bc4Snorm
            | PixelFormat::Bc5Unorm
            | PixelFormat::Bc5Snorm
            | PixelFormat::Bc7Unorm
            | PixelFormat::Bc6hUfloat
            | PixelFormat::Bc6hSfloat
            | PixelFormat::Astc2d4x4Unorm
            | PixelFormat::B8G8R8A8Unorm
            | PixelFormat::R32G32B32A32Float
            | PixelFormat::R32G32B32A32Sint
            | PixelFormat::R32G32Float
            | PixelFormat::R32G32Sint
            | PixelFormat::R32Float
    )
}

impl Drop for TextureCacheRuntime {
    fn drop(&mut self) {
        for (_, shadow) in self.resolve_shadows.drain() {
            unsafe { self.device.destroy_image_view(shadow.view, None) };
        }
        let resources = self
            .sentenced_resources
            .drain(..)
            .map(|sentenced| sentenced.resource)
            .collect::<Vec<_>>();
        for resource in resources {
            match resource {
                DeferredVkResource::Framebuffer(framebuffer) => {
                    self.destroy_framebuffer_owner(framebuffer)
                }
                DeferredVkResource::ImageView(view) => self.destroy_image_view(view),
                DeferredVkResource::Image(image) => self.destroy_image(image),
            }
        }
    }
}

/// Texture-cache policy matching upstream `Vulkan::TextureCacheParams`.
pub struct TextureCacheParams;

impl crate::texture_cache::texture_cache_base::TextureCacheParams for TextureCacheParams {
    type Runtime = TextureCacheRuntime;
    type Image = Image;
    type ImageAlloc = ();
    type ImageView = ImageView;
    type Sampler = CachedSampler;
    type Framebuffer = Box<Framebuffer>;
    type FramebufferError = vk::Result;
    type AsyncBuffer = StagingBufferRef;
    type BufferType = vk::Buffer;

    const ENABLE_VALIDATION: bool = true;
    const FRAMEBUFFER_BLITS: bool = false;
    const HAS_EMULATED_COPIES: bool = false;
    const HAS_DEVICE_MEMORY_INFO: bool = true;
    const IMPLEMENTS_ASYNC_DOWNLOADS: bool = true;

    fn create_image(
        runtime: Option<&mut TextureCacheRuntime>,
        image_id: ImageId,
        base: NonNull<ImageBase>,
    ) -> Image {
        Image::new(
            runtime.expect("Vulkan TextureCache runtime must be bound"),
            image_id,
            base,
        )
        .unwrap_or_else(|error| panic!("Vulkan Image construction failed: {error:?}"))
    }

    fn set_image_allocation_tick(image: &mut Image, allocation_tick: u64) {
        image.allocation_tick = allocation_tick;
    }

    fn create_image_view(
        runtime: Option<&mut TextureCacheRuntime>,
        view_id: ImageViewId,
        info: &ImageViewInfo,
        base: NonNull<ImageViewBase>,
        image: Option<&Image>,
    ) -> ImageView {
        let runtime = runtime.expect("Vulkan TextureCache runtime must be bound");
        // SAFETY: the pointer is the base of the slot receiving the payload.
        if unsafe { base.as_ref() }.is_buffer() {
            runtime.make_buffer_image_view(view_id, info, base)
        } else {
            runtime
                .make_image_view(
                    view_id,
                    info,
                    base,
                    image.expect("non-buffer Vulkan image view requires its parent image"),
                )
                .unwrap_or_else(|error| panic!("Vulkan ImageView construction failed: {error:?}"))
        }
    }

    fn create_sampler(
        runtime: Option<&mut TextureCacheRuntime>,
        config: &crate::textures::texture::TscEntry,
    ) -> CachedSampler {
        TextureCache::create_sampler_from_tsc(
            runtime.expect("Vulkan TextureCache runtime must be bound"),
            config,
        )
        .unwrap_or_else(|error| panic!("Vulkan Sampler construction failed: {error:?}"))
    }

    fn create_framebuffer(
        runtime: Option<&mut TextureCacheRuntime>,
        color_buffers: [Option<NonNull<ImageView>>; NUM_RT],
        depth_buffer: Option<NonNull<ImageView>>,
        key: &RenderTargets,
    ) -> Result<Box<Framebuffer>, vk::Result> {
        Framebuffer::new(
            runtime.expect("Vulkan TextureCache runtime must be bound"),
            color_buffers,
            depth_buffer,
            key,
        )
    }

    fn prepare_image_view(
        cache: &mut CommonTextureCache<Self>,
        image_view_id: ImageViewId,
        is_modification: bool,
        invalidate: bool,
    ) {
        if !image_view_id.is_valid()
            || image_view_id == NULL_IMAGE_VIEW_ID
            || !cache.slot_image_views.contains(image_view_id)
        {
            return;
        }
        let view = &cache.slot_image_views[image_view_id];
        if view.is_buffer() {
            return;
        }
        cache.prepare_image(view.image_id, is_modification, invalidate);
    }

    fn scale_up_image(
        cache: &mut CommonTextureCache<Self>,
        image_id: ImageId,
        ignore: bool,
    ) -> bool {
        let mut image = cache.slot_images[image_id]
            .backend
            .take()
            .expect("Vulkan image backend must be materialized");
        let scaled = image.scale_up(cache.runtime_mut(), ignore);
        cache.slot_images[image_id].backend = Some(image);
        scaled
    }

    fn scale_down_image(
        cache: &mut CommonTextureCache<Self>,
        image_id: ImageId,
        ignore: bool,
    ) -> bool {
        let mut image = cache.slot_images[image_id]
            .backend
            .take()
            .expect("Vulkan image backend must be materialized");
        let scaled = image.scale_down(cache.runtime_mut(), ignore);
        cache.slot_images[image_id].backend = Some(image);
        scaled
    }

    fn upload_staging_buffer(
        cache: &mut CommonTextureCache<Self>,
        size: usize,
        deferred: bool,
    ) -> StagingBufferRef {
        cache
            .runtime_mut()
            .upload_staging_buffer(size as vk::DeviceSize, deferred)
    }

    fn staging_mapped_span(buffer: &mut StagingBufferRef) -> &mut [u8] {
        crate::buffer_cache::buffer_cache_base::BufferCacheAsyncBuffer::mapped_span_mut(buffer)
    }

    fn free_deferred_staging_buffer(
        cache: &mut CommonTextureCache<Self>,
        buffer: &mut StagingBufferRef,
    ) {
        cache.runtime_mut().free_deferred_staging_buffer(buffer);
    }

    fn can_upload_msaa(cache: &CommonTextureCache<Self>) -> bool {
        cache.runtime().can_upload_msaa()
    }

    fn transition_image_layout(cache: &mut CommonTextureCache<Self>, image_id: ImageId) {
        let mut image = cache.slot_images[image_id]
            .backend
            .take()
            .expect("Vulkan image backend must be materialized");
        cache.runtime_mut().transition_image_layout(&mut image);
        cache.slot_images[image_id].backend = Some(image);
    }

    fn upload_image(
        cache: &mut CommonTextureCache<Self>,
        image_id: ImageId,
        staging: &StagingBufferRef,
        copies: &[BufferImageCopy],
    ) {
        let mut image = cache.slot_images[image_id]
            .backend
            .take()
            .expect("Vulkan image backend must be materialized");
        if !image.upload_memory(cache.runtime_mut(), staging.buffer, staging.offset, copies) {
            log::error!(
                "Vulkan::Image::UploadMemory failed for image {}",
                image_id.index
            );
        }
        cache.slot_images[image_id].backend = Some(image);
    }

    fn accelerate_image_upload(
        cache: &mut CommonTextureCache<Self>,
        image_id: ImageId,
        staging: &StagingBufferRef,
        swizzles: &[crate::texture_cache::types::SwizzleParameters],
        z_start: u32,
        z_count: u32,
    ) {
        let mut image = cache.slot_images[image_id]
            .backend
            .take()
            .expect("Vulkan image backend must be materialized");
        if !cache
            .runtime_mut()
            .accelerate_image_upload(&mut image, *staging, swizzles, z_start, z_count)
        {
            log::error!(
                "Vulkan::TextureCacheRuntime::AccelerateImageUpload failed for image {}",
                image_id.index
            );
        }
        cache.slot_images[image_id].backend = Some(image);
    }

    fn insert_upload_memory_barrier(cache: &mut CommonTextureCache<Self>) {
        cache.runtime_mut().insert_upload_memory_barrier();
    }

    fn copy_image(
        cache: &mut CommonTextureCache<Self>,
        dst_id: ImageId,
        src_id: ImageId,
        copies: &[ImageCopy],
    ) {
        if !texture_cache_from_base(cache).copy_join_image(dst_id, src_id, copies) {
            log::error!(
                "TextureCacheVulkan::JoinImages unsupported copy: dst={} src={}",
                dst_id.index,
                src_id.index,
            );
        }
    }

    fn copy_image_msaa(
        cache: &mut CommonTextureCache<Self>,
        dst_id: ImageId,
        src_id: ImageId,
        copies: &[ImageCopy],
    ) {
        if !texture_cache_from_base(cache).copy_join_image(dst_id, src_id, copies) {
            log::error!(
                "TextureCacheVulkan::JoinImages unsupported MSAA copy: dst={} src={}",
                dst_id.index,
                src_id.index,
            );
        }
    }
}

/// Manages GPU textures (images, views, samplers, framebuffers).
///
/// Ref: zuyu TextureCacheRuntime — caches textures by TIC index and
/// framebuffers by render target configuration.
#[repr(transparent)]
pub struct TextureCache {
    pub base: CommonTextureCache<TextureCacheParams>,
}

fn texture_cache_from_base(base: &mut CommonTextureCache<TextureCacheParams>) -> &mut TextureCache {
    // SAFETY: `TextureCache` is a transparent newtype with `base` as its only
    // field. The generic policy callback is invoked only through that owner.
    unsafe { &mut *(base as *mut _ as *mut TextureCache) }
}

impl TextureCache {
    pub fn new(
        vulkan_device: &Device,
        device: ash::Device,
        instance: ash::Instance,
        physical_device: vk::PhysicalDevice,
        device_memory: Arc<MaxwellDeviceMemoryManager>,
        scheduler: &mut Scheduler,
        memory_allocator: &mut MemoryAllocator,
        staging_buffer_pool: &mut StagingBufferPool,
        blit_image_helper: &mut BlitImageHelper,
        render_pass_cache: &mut RenderPassCache,
        descriptor_pool: &mut DescriptorPool,
        compute_pass_descriptor_queue: &mut ComputePassDescriptorQueue,
        cant_blit_msaa: bool,
        image_format_list_supported: bool,
        optimal_astc_supported: bool,
        must_emulate_bgr565: bool,
        ext_4444_formats_supported: bool,
        custom_border_color_supported: bool,
        sampler_filter_minmax_supported: bool,
        sampler_heap_budget: Option<usize>,
        has_null_descriptor: bool,
    ) -> Result<Self, vk::Result> {
        let mut base = CommonTextureCache::<TextureCacheParams>::new_for_backend(device_memory);
        let mut runtime = Box::new(TextureCacheRuntime::new(
            vulkan_device,
            device,
            instance,
            physical_device,
            scheduler,
            memory_allocator,
            staging_buffer_pool,
            blit_image_helper,
            render_pass_cache,
            descriptor_pool,
            compute_pass_descriptor_queue,
            cant_blit_msaa,
            image_format_list_supported,
            optimal_astc_supported,
            must_emulate_bgr565,
            ext_4444_formats_supported,
            custom_border_color_supported,
            sampler_filter_minmax_supported,
            sampler_heap_budget,
            has_null_descriptor,
        ));
        base.configure_device_memory_budget(runtime.get_device_local_memory());
        base.set_sampler_heap_budget(runtime.get_sampler_heap_budget());
        let null_view_base = NonNull::from(base.slot_image_views[NULL_IMAGE_VIEW_ID].base.as_mut());
        let null_image_view = runtime.make_null_image_view(null_view_base)?;
        base.slot_image_views[NULL_IMAGE_VIEW_ID].backend = Some(null_image_view);
        let null_sampler_descriptor = **base.slot_samplers.get(NULL_SAMPLER_ID);
        let null_sampler = Self::create_sampler_from_tsc(&runtime, &null_sampler_descriptor)?;
        base.slot_samplers[NULL_SAMPLER_ID].backend = Some(null_sampler);
        base.bind_runtime(runtime);
        Ok(Self { base })
    }

    pub fn set_guest_memory_writer(&mut self, writer: crate::renderer_base::GuestMemoryWriter) {
        self.base.set_guest_memory_writer(writer);
    }

    /// Port of the Vulkan texture-cache owner `CreateChannel` edge.
    pub fn create_channel(&mut self, channel: &ChannelState) {
        self.base.create_channel(channel);
    }

    /// Port of the Vulkan texture-cache owner `BindToChannel` edge.
    pub fn bind_to_channel(&mut self, channel_id: i32) {
        self.base.bind_to_channel(channel_id);
    }

    /// Vulkan-backed port of upstream `TextureCache<P>::DownloadMemory`.
    pub fn download_memory(&mut self, cpu_addr: u64, size: usize) {
        if self.base.channel_gpu_memory.is_none() && self.base.guest_memory_writer.is_none() {
            return;
        }

        let mut images = SmallVec::<[ImageId; 16]>::new();
        self.base
            .for_each_image_in_region(cpu_addr, size, |image_id, image| {
                if !image.is_safe_download() {
                    return false;
                }
                image.flags.remove(ImageFlagBits::GPU_MODIFIED);
                images.push(image_id);
                false
            });
        if images.is_empty() {
            return;
        }
        images.sort_by_key(|&image_id| self.base.slot_images[image_id].modification_tick);

        for image_id in images {
            let Some((image_base, staging)) = self.download_image_to_host_staging(image_id) else {
                continue;
            };
            let copies = full_download_copies(&image_base.info);
            let _ = self
                .base
                .write_downloaded_image(&image_base, &copies, &staging);
        }
    }

    pub fn should_wait_async_flushes(&self) -> bool {
        self.base.should_wait_async_flushes()
    }

    pub fn has_uncommitted_flushes(&self) -> bool {
        self.base.has_uncommitted_flushes()
    }

    pub fn commit_async_flushes(&mut self) {
        let mut download_ids = std::mem::take(&mut self.base.uncommitted_downloads);
        if download_ids.is_empty() {
            self.base.committed_downloads.push_back(download_ids);
            self.base
                .async_buffers
                .push_back(std::mem::take(&mut self.base.uncommitted_async_buffers));
            return;
        }

        let mut total_size_bytes = 0usize;
        let last_async_buffer_id = self.base.uncommitted_async_buffers.len();
        let mut any_swizzle = false;
        for download_info in &mut download_ids {
            if download_info.is_swizzle {
                total_size_bytes = total_size_bytes.saturating_add(common::alignment::align_up(
                    self.base.slot_images[download_info.object_id].unswizzled_size_bytes as u64,
                    64,
                ) as usize);
                any_swizzle = true;
                download_info.async_buffer_id = last_async_buffer_id;
            }
        }

        if any_swizzle && total_size_bytes != 0 {
            let Some(mut download_map) = self
                .base
                .runtime_mut()
                .download_staging_buffer(total_size_bytes as vk::DeviceSize, true)
            else {
                self.base.committed_downloads.push_back(download_ids);
                self.base
                    .async_buffers
                    .push_back(std::mem::take(&mut self.base.uncommitted_async_buffers));
                return;
            };
            for download_info in &download_ids {
                if !download_info.is_swizzle {
                    continue;
                }
                let image_id = download_info.object_id;
                let image_base = self.base.slot_images[image_id].base.as_ref().clone();
                let format = self
                    .base
                    .runtime_mut()
                    .surface_format(image_base.info.format, false);
                let aspect = image_aspect_mask(image_base.info.format);
                if aspect.is_empty()
                    || self
                        .ensure_image(image_id, &image_base, format, aspect)
                        .is_err()
                {
                    continue;
                }
                let copies = full_download_copies(&image_base.info);
                let Some(mut image) = self.take_backend_image(image_id) else {
                    continue;
                };
                let _ = image.download_memory_to_staging(
                    &mut self.base.runtime_mut(),
                    &download_map,
                    &copies,
                );
                self.base.slot_images[image_id].backend = Some(image);
                download_map.offset +=
                    common::alignment::align_up(image_base.unswizzled_size_bytes as u64, 64)
                        as vk::DeviceSize;
            }
            self.base.uncommitted_async_buffers.push(download_map);
        }

        self.base
            .async_buffers
            .push_back(std::mem::take(&mut self.base.uncommitted_async_buffers));
        self.base.committed_downloads.push_back(download_ids);
    }

    pub fn pop_async_flushes(&mut self) {
        let Some(download_ids) = self.base.committed_downloads.pop_front() else {
            return;
        };
        let mut download_map = self.base.async_buffers.pop_front().unwrap_or_default();
        if download_ids.is_empty() {
            return;
        }
        for download_info in download_ids.iter().rev() {
            let Some(download_buffer) = download_map.get_mut(download_info.async_buffer_id) else {
                log::warn!(
                    "TextureCacheVulkan::pop_async_flushes missing async buffer {}",
                    download_info.async_buffer_id
                );
                continue;
            };
            let start = download_buffer.offset as usize;
            let span = unsafe {
                std::slice::from_raw_parts(download_buffer.mapped, download_buffer.size as usize)
            };
            if download_info.is_swizzle {
                let image = self.base.slot_images[download_info.object_id]
                    .base
                    .as_ref()
                    .clone();
                let aligned_size =
                    common::alignment::align_up(image.unswizzled_size_bytes as u64, 64)
                        as vk::DeviceSize;
                download_buffer.offset = download_buffer.offset.saturating_sub(aligned_size);
                let start = download_buffer.offset as usize;
                let end = start.saturating_add(image.unswizzled_size_bytes as usize);
                if end <= span.len() {
                    let copies = full_download_copies(&image.info);
                    let _ = self
                        .base
                        .write_downloaded_image(&image, &copies, &span[start..end]);
                } else {
                    log::warn!(
                        "TextureCacheVulkan::pop_async_flushes swizzle range out of bounds start={} end={} len={}",
                        start,
                        end,
                        span.len()
                    );
                }
            } else {
                let buffer_info = self
                    .base
                    .slot_buffer_downloads
                    .take(download_info.object_id);
                let end = start.saturating_add(buffer_info.size);
                if end <= span.len() {
                    let _ = self
                        .base
                        .write_downloaded_buffer(buffer_info.address, &span[start..end]);
                } else {
                    log::warn!(
                        "TextureCacheVulkan::pop_async_flushes DMA range out of bounds start={} end={} len={}",
                        start,
                        end,
                        span.len()
                    );
                }
            }
        }
        self.base.async_buffers_death_ring.extend(download_map);
    }

    /// Vulkan-backed port of `TextureCache<P>::DmaBufferImageCopy`.
    pub fn dma_buffer_image_copy(
        &mut self,
        copy_info: &dma::ImageCopy,
        buffer_operand: &dma::BufferOperand,
        image_operand: &dma::ImageOperand,
        image_id: ImageId,
        buffer: vk::Buffer,
        buffer_offset: vk::DeviceSize,
        is_upload: bool,
    ) -> bool {
        if buffer == vk::Buffer::null() || image_id == NULL_IMAGE_ID || !image_id.is_valid() {
            return false;
        }
        let Some(copy) = self
            .base
            .dma_buffer_image_copy_descriptor(copy_info, buffer_operand, image_operand, image_id)
            .map(|result| result.copy)
        else {
            return false;
        };

        if is_upload {
            if !self.base_image_exists(image_id) {
                return false;
            }
            self.base.prepare_image(image_id, true, false);
        } else {
            if !self.base_image_exists(image_id) {
                return false;
            }
            self.base.prepare_image(image_id, false, false);
            let bpp = crate::surface::bytes_per_block(self.base.slot_images[image_id].info.format);
            if buffer_offset as usize % bpp as usize != 0 {
                return false;
            }
        }

        let copies = [copy];
        if is_upload {
            let image_base = self.base.slot_images[image_id].base.as_ref().clone();
            let format = self
                .base
                .runtime_mut()
                .surface_format(image_base.info.format, false);
            let aspect = image_aspect_mask(image_base.info.format);
            if aspect.is_empty()
                || self
                    .ensure_image(image_id, &image_base, format, aspect)
                    .is_err()
            {
                return false;
            }
            let Some(mut image) = self.take_backend_image(image_id) else {
                return false;
            };
            let uploaded =
                image.upload_memory(&mut self.base.runtime_mut(), buffer, buffer_offset, &copies);
            self.base.slot_images[image_id].backend = Some(image);
            uploaded
        } else {
            let size = buffer_operand.pitch.wrapping_mul(buffer_operand.height) as usize;
            let downloaded = self.download_image_into_buffer(
                image_id,
                buffer,
                buffer_offset,
                &copies,
                buffer_operand.address,
                size,
            );
            downloaded
        }
    }

    /// Vulkan-backed port of `TextureCache<P>::DownloadImageIntoBuffer`.
    fn download_image_into_buffer(
        &mut self,
        image_id: ImageId,
        buffer: vk::Buffer,
        buffer_offset: vk::DeviceSize,
        copies: &[BufferImageCopy],
        address: u64,
        size: usize,
    ) -> bool {
        if size == 0 || buffer == vk::Buffer::null() || !self.base_image_exists(image_id) {
            return false;
        }

        let mut image_base = self.base.slot_images[image_id].base.as_ref().clone();
        self.apply_backend_image_flags(&mut image_base);
        let format = self
            .base
            .runtime_mut()
            .surface_format(image_base.info.format, false);
        let aspect = image_aspect_mask(image_base.info.format);
        if aspect.is_empty()
            || self
                .ensure_image(image_id, &image_base, format, aspect)
                .is_err()
        {
            return false;
        }

        let slot = self
            .base
            .slot_buffer_downloads
            .insert(BufferDownload { address, size });
        self.base.uncommitted_downloads.push(PendingDownload {
            is_swizzle: false,
            async_buffer_id: self.base.uncommitted_async_buffers.len(),
            object_id: slot,
        });

        let Some(download_map) = self
            .base
            .runtime_mut()
            .download_staging_buffer(size as vk::DeviceSize, true)
        else {
            let _ = self.base.slot_buffer_downloads.take(slot);
            let _ = self.base.uncommitted_downloads.pop();
            return false;
        };

        let async_buffer_id = self.base.uncommitted_async_buffers.len();
        self.base.uncommitted_async_buffers.push(download_map);

        let Some(mut image) = self.take_backend_image(image_id) else {
            let _ = self.base.slot_buffer_downloads.take(slot);
            let _ = self.base.uncommitted_downloads.pop();
            if let Some(mut download_map) = self.base.uncommitted_async_buffers.pop() {
                self.base
                    .runtime_mut()
                    .free_deferred_staging_buffer(&mut download_map);
            }
            return false;
        };
        let download_buffer = self.base.uncommitted_async_buffers[async_buffer_id].buffer;
        let download_offset = self.base.uncommitted_async_buffers[async_buffer_id].offset;
        let downloaded = image.download_memory(
            &mut self.base.runtime_mut(),
            &[buffer, download_buffer],
            &[buffer_offset, download_offset],
            copies,
        );
        self.base.slot_images[image_id].backend = Some(image);

        if !downloaded {
            let _ = self.base.slot_buffer_downloads.take(slot);
            let _ = self.base.uncommitted_downloads.pop();
            if let Some(mut download_map) = self.base.uncommitted_async_buffers.pop() {
                self.base
                    .runtime_mut()
                    .free_deferred_staging_buffer(&mut download_map);
            }
            return false;
        }

        true
    }

    fn download_image_to_host_staging(
        &mut self,
        image_id: ImageId,
    ) -> Option<(ImageBase, Vec<u8>)> {
        if !self.base_image_exists(image_id) {
            return None;
        }
        let image_base = self.base.slot_images[image_id].base.as_ref().clone();
        let staging_size = image_base.unswizzled_size_bytes as usize;
        if staging_size == 0 {
            return None;
        }
        let format = self
            .base
            .runtime_mut()
            .surface_format(image_base.info.format, false);
        let aspect = image_aspect_mask(image_base.info.format);
        if aspect.is_empty()
            || self
                .ensure_image(image_id, &image_base, format, aspect)
                .is_err()
        {
            return None;
        }

        let copies = full_download_copies(&image_base.info);
        let staging = self
            .base
            .runtime_mut()
            .download_staging_buffer(staging_size as vk::DeviceSize, false)?;
        let Some(mut image) = self.take_backend_image(image_id) else {
            return None;
        };
        let downloaded =
            image.download_memory_to_staging(&mut self.base.runtime_mut(), &staging, &copies);
        self.base.slot_images[image_id].backend = Some(image);
        if !downloaded {
            return None;
        }

        self.base.runtime_mut().finish();
        let staging_bytes =
            unsafe { std::slice::from_raw_parts(staging.mapped, staging_size) }.to_vec();
        Some((image_base, staging_bytes))
    }

    /// Port of the Vulkan texture-cache owner `EraseChannel` edge.
    pub fn erase_channel(&mut self, channel_id: i32) {
        self.base.erase_channel(channel_id);
    }

    /// Port of `TextureCache<P>::UpdateRenderTargets` for the Vulkan backend.
    pub fn update_render_targets(
        &mut self,
        render_targets: &crate::engines::draw_manager::Maxwell3DRenderTargets,
        dirty_flags: &mut [bool; 256],
        _read_gpu_unsafe: &dyn Fn(u64, &mut [u8]) -> bool,
        is_clear: bool,
        clear_scissor: Option<(u32, u32, u32, u32)>,
    ) -> bool {
        let Some(gpu_memory) = self.base.channel_gpu_memory.as_ref().cloned() else {
            return false;
        };
        let maxwell3d = NonNull::new(
            self.base.current_channel_state().channel_info.maxwell3d as *mut Maxwell3D,
        );
        let mut dirty_access = VulkanRenderTargetDirtyFlags {
            draw_flags: dirty_flags,
            maxwell3d,
        };
        self.base.update_render_targets_with_snapshot(
            render_targets,
            &mut dirty_access,
            |gpu_addr, guest_size| {
                let gpu_memory = gpu_memory.lock();
                gpu_memory
                    .gpu_to_cpu_address(gpu_addr)
                    .or_else(|| gpu_memory.gpu_to_cpu_address_range(gpu_addr, guest_size))
            },
            is_clear,
            clear_scissor,
        );
        true
    }

    /// Port of `TextureCache<P>::GetFramebuffer`.
    pub fn get_framebuffer(&mut self) -> Result<RenderTargetFramebuffer, vk::Result> {
        Ok(self.base.get_framebuffer()?.render_target_framebuffer())
    }

    fn backend_image(&self, image_id: ImageId) -> Option<&Image> {
        self.base
            .slot_images
            .contains(image_id)
            .then(|| self.base.slot_images[image_id].backend.as_ref())
            .flatten()
    }

    fn backend_image_mut(&mut self, image_id: ImageId) -> Option<&mut Image> {
        if !self.base.slot_images.contains(image_id) {
            return None;
        }
        self.base.slot_images[image_id].backend.as_mut()
    }

    fn take_backend_image(&mut self, image_id: ImageId) -> Option<Image> {
        if !self.base.slot_images.contains(image_id) {
            return None;
        }
        self.base.slot_images[image_id].backend.take()
    }

    fn backend_image_view(&self, view_id: ImageViewId) -> Option<&ImageView> {
        if !self.base.slot_image_views.contains(view_id) {
            return None;
        }
        self.base.slot_image_views[view_id].backend.as_ref()
    }

    fn backend_image_view_mut(&mut self, view_id: ImageViewId) -> Option<&mut ImageView> {
        if !self.base.slot_image_views.contains(view_id) {
            return None;
        }
        self.base.slot_image_views[view_id].backend.as_mut()
    }

    fn take_backend_image_view(&mut self, view_id: ImageViewId) -> Option<ImageView> {
        if !self.base.slot_image_views.contains(view_id) {
            return None;
        }
        self.base.slot_image_views[view_id].backend.take()
    }

    /// Ensure a backend `Image` exists for the common-cache `ImageId`.
    /// Returns `true` when an existing backend image was recreated.
    fn ensure_image(
        &mut self,
        image_id: ImageId,
        image_base: &ImageBase,
        format: vk::Format,
        aspect: vk::ImageAspectFlags,
    ) -> Result<bool, vk::Result> {
        if let Some(existing) = self.base.slot_images[image_id].backend.as_ref() {
            if existing.format == format
                && existing.aspect == aspect
                && existing.base().info.size == image_base.info.size
                && existing.base().info.resources == image_base.info.resources
                && existing.base().info.image_type == image_base.info.image_type
                && existing.base().info.num_samples == image_base.info.num_samples
            {
                return Ok(false);
            }
            // Invalidate dependent framebuffers before replacing their image
            // views, then sentence all stale Vulkan objects until the GPU tick.
            let removed_view_ids = self.base.slot_images[image_id].image_view_ids.clone();
            for &view_id in &removed_view_ids {
                self.remove_framebuffers_for_view(view_id);
            }
            if let Some(old) = self.base.slot_images[image_id].backend.take() {
                self.base.runtime_mut().sentence_image(old);
            }
            for view_id in removed_view_ids {
                if let Some(view) = self.base.slot_image_views[view_id].backend.take() {
                    self.base.runtime_mut().sentence_image_view(view);
                }
            }
            let image = self.make_image(image_id, image_base, format, aspect)?;
            self.base.slot_images[image_id].backend = Some(image);
            return Ok(true);
        }
        unreachable!(
            "a published Vulkan image slot must contain its synchronously-created backend image"
        )
    }

    fn make_image(
        &mut self,
        image_id: ImageId,
        _image_base: &ImageBase,
        _format: vk::Format,
        _aspect: vk::ImageAspectFlags,
    ) -> Result<Image, vk::Result> {
        let base = NonNull::from(self.base.slot_images[image_id].base.as_mut());
        Image::new(self.base.runtime_mut(), image_id, base)
    }

    fn apply_backend_image_flags(&self, image: &mut ImageBase) {
        Self::apply_backend_image_flags_with_capabilities(
            image,
            self.base.runtime().optimal_astc_supported,
            self.base.runtime().optimal_bcn_supported,
            *common::settings::values().accelerate_astc.get_value(),
            *common::settings::values().astc_recompression.get_value(),
        );
    }

    fn apply_backend_image_flags_with_capabilities(
        image: &mut ImageBase,
        optimal_astc_supported: bool,
        optimal_bcn_supported: bool,
        astc_decode_mode: common::settings_enums::AstcDecodeMode,
        astc_recompression: common::settings_enums::AstcRecompression,
    ) {
        if crate::surface::is_pixel_format_astc(image.info.format) && !optimal_astc_supported {
            match astc_decode_mode {
                common::settings_enums::AstcDecodeMode::Gpu => {
                    if astc_recompression == common::settings_enums::AstcRecompression::Uncompressed
                        && image.info.size.depth == 1
                    {
                        image.flags.insert(ImageFlagBits::ACCELERATED_UPLOAD);
                    }
                }
                common::settings_enums::AstcDecodeMode::CpuAsynchronous => {
                    image.flags.insert(ImageFlagBits::ASYNCHRONOUS_DECODE);
                }
                common::settings_enums::AstcDecodeMode::Cpu => {}
            }
            image
                .flags
                .insert(ImageFlagBits::CONVERTED | ImageFlagBits::COSTLY_LOAD);
        }
        if crate::surface::is_pixel_format_bcn(image.info.format) && !optimal_bcn_supported {
            image
                .flags
                .insert(ImageFlagBits::CONVERTED | ImageFlagBits::COSTLY_LOAD);
        }
    }

    fn ensure_image_view(&mut self, view_id: ImageViewId) -> Result<(), vk::Result> {
        if self.backend_image_view(view_id).is_some() {
            return Ok(());
        }
        let view_base = NonNull::from(self.base.slot_image_views[view_id].base.as_mut());
        let info = self.base.slot_image_views[view_id].info;
        // SAFETY: the boxed base allocation remains stable for the complete
        // typed-slot lifetime.
        let view_base_ref = unsafe { view_base.as_ref() };
        if view_base_ref.is_buffer() {
            let view = self
                .base
                .runtime_mut()
                .make_buffer_image_view(view_id, &info, view_base);
            self.base.slot_image_views[view_id].backend = Some(view);
            return Ok(());
        }
        let image = self.base.slot_images[view_base_ref.image_id]
            .backend
            .as_ref()
            .ok_or(vk::Result::ERROR_INITIALIZATION_FAILED)?;
        let view = self
            .base
            .runtime()
            .make_image_view(view_id, &info, view_base, image)?;
        self.base.slot_image_views[view_id].backend = Some(view);
        Ok(())
    }

    fn remove_framebuffers_for_view(&mut self, view_id: ImageViewId) {
        let removed_ids = [view_id];
        let remove_keys = self
            .base
            .framebuffers
            .keys()
            .filter(|key| key.contains(&removed_ids))
            .copied()
            .collect::<Vec<_>>();
        for key in remove_keys {
            if let Some(framebuffer_id) = self.base.framebuffers.remove(&key) {
                if framebuffer_id == self.base.last_framebuffer_id {
                    self.base.last_framebuffer_id = FramebufferId::default();
                    self.base.last_framebuffer_serial = 0;
                }
                let framebuffer = self.base.slot_framebuffers.take(framebuffer_id);
                self.base.runtime_mut().sentence_framebuffer(framebuffer);
            }
        }
    }

    fn base_image_exists(&self, image_id: ImageId) -> bool {
        self.base.slot_images.contains(image_id)
    }

    fn scale_up_image(&mut self, image_id: ImageId, ignore: bool) -> bool {
        if !self.base_image_exists(image_id) {
            return false;
        }
        if self.base.slot_images[image_id]
            .flags
            .contains(ImageFlagBits::RESCALED)
        {
            return false;
        }
        let image_base = self.base.slot_images[image_id].base.as_ref().clone();
        let format = self
            .base
            .runtime_mut()
            .surface_format(image_base.info.format, false);
        let aspect = image_aspect_mask(image_base.info.format);
        if aspect.is_empty()
            || self
                .ensure_image(image_id, &image_base, format, aspect)
                .is_err()
        {
            return false;
        }
        let Some(mut image) = self.take_backend_image(image_id) else {
            return false;
        };
        let had_scaled_copy = image.base().has_scaled;
        let scaled = image.scale_up(&mut self.base.runtime_mut(), ignore);
        if scaled && !had_scaled_copy {
            self.base.total_used_memory = self.base.total_used_memory.wrapping_add(
                CommonTextureCache::<TextureCacheParams>::scaled_image_memory_size(
                    &self.base.slot_images[image_id],
                ),
            );
        }
        if scaled {
            self.base.invalidate_scale(image_id);
        }
        self.base.slot_images[image_id].backend = Some(image);
        scaled
    }

    fn scale_down_image(&mut self, image_id: ImageId, ignore: bool) -> bool {
        if !self.base_image_exists(image_id) {
            return false;
        }
        if !self.base.slot_images[image_id]
            .flags
            .contains(ImageFlagBits::RESCALED)
        {
            return false;
        }
        if self.base.slot_images[image_id].info.image_type == ImageType::Linear {
            return false;
        }
        let Some(mut image) = self.take_backend_image(image_id) else {
            return false;
        };
        let scaled = image.scale_down(&mut self.base.runtime_mut(), ignore);
        if scaled {
            self.base.invalidate_scale(image_id);
        }
        self.base.slot_images[image_id].backend = Some(image);
        scaled
    }

    fn ensure_image_rescale_state(
        &mut self,
        image_id: ImageId,
        should_rescale: bool,
        ignore_copy: bool,
    ) -> bool {
        if !self.base_image_exists(image_id) {
            return false;
        }
        let is_rescaled = self.base.slot_images[image_id]
            .flags
            .contains(ImageFlagBits::RESCALED);
        if should_rescale {
            is_rescaled || self.scale_up_image(image_id, ignore_copy)
        } else {
            !is_rescaled || self.scale_down_image(image_id, ignore_copy)
        }
    }

    fn find_or_insert_image_from_info_with_options_and_finish(
        &mut self,
        info: &ImageInfo,
        gpu_addr: u64,
        cpu_addr: u64,
        options: RelaxedOptions,
        _read_gpu_unsafe: &dyn Fn(u64, &mut [u8]) -> bool,
    ) -> ImageId {
        self.base
            .find_or_insert_image_from_info_with_options(info, gpu_addr, cpu_addr, options)
    }

    fn blit_framebuffer_from_image_view(
        &mut self,
        view_id: ImageViewId,
    ) -> Option<BlitFramebufferInfo> {
        if !view_id.is_valid() || view_id == NULL_IMAGE_VIEW_ID {
            return None;
        }
        let view_base = self.base.slot_image_views[view_id].base.as_ref().clone();
        let image_id = view_base.image_id;
        if !self.base_image_exists(image_id) {
            return None;
        }
        let image_base = self.base.slot_images[image_id].base.as_ref().clone();
        let aspect = image_aspect_mask(image_base.info.format);
        let format = self
            .base
            .runtime_mut()
            .surface_format(image_base.info.format, false);
        if aspect.is_empty()
            || self
                .ensure_image(image_id, &image_base, format, aspect)
                .is_err()
        {
            return None;
        }
        self.ensure_image_view(view_id).ok()?;

        let is_rescaled = self.base.slot_images[image_id]
            .flags
            .contains(ImageFlagBits::RESCALED);
        let mut extent = view_base.size;
        if is_rescaled {
            let resolution = common::settings::values().resolution_info.clone();
            extent.width = resolution.scale_up_i32(extent.width as i32) as u32;
            if image_base.info.image_type == ImageType::E2D {
                extent.height = resolution.scale_up_i32(extent.height as i32) as u32;
            }
        }
        let (samples_x, samples_y) =
            crate::texture_cache::samples_helper::samples_log2(image_base.info.num_samples as i32);
        let fb_extent = vk::Extent2D {
            width: (extent.width >> samples_x).max(1),
            height: (extent.height >> samples_y).max(1),
        };
        let is_color =
            crate::surface::get_format_type(view_base.format) == SurfaceType::ColorTexture;
        let mut key = RenderTargets {
            size: Extent2D {
                width: fb_extent.width,
                height: fb_extent.height,
            },
            is_rescaled,
            ..RenderTargets::default()
        };
        if is_color {
            key.color_buffer_ids[0] = view_id;
        } else {
            key.depth_buffer_id = view_id;
        }

        let framebuffer_id = if let Some(&framebuffer_id) = self.base.framebuffers.get(&key) {
            framebuffer_id
        } else {
            let view = self.backend_image_view(view_id)?;
            let view_handle = view.render_target();
            let mut rp_key = RenderPassKey::default();
            let color_views;
            let depth_view;
            if is_color {
                rp_key.color_formats[0] = view_base.format;
                color_views = vec![view_handle];
                depth_view = None;
            } else {
                rp_key.depth_format = view_base.format;
                color_views = Vec::new();
                depth_view = Some(view_handle);
            }
            rp_key.samples = vk::SampleCountFlags::TYPE_1;
            let render_pass = self
                .base
                .runtime_mut()
                .render_pass_cache()
                .get(&rp_key)
                .ok()?;
            let framebuffer = self
                .base
                .runtime_mut()
                .create_framebuffer(
                    render_pass,
                    &{
                        let mut attachments = color_views.clone();
                        if let Some(depth) = depth_view {
                            attachments.push(depth);
                        }
                        attachments
                    },
                    fb_extent,
                )
                .ok()?;
            let mut images = [vk::Image::null(); NUM_RT + 1];
            let mut image_ranges = [vk::ImageSubresourceRange::default(); NUM_RT + 1];
            images[0] = self.backend_image(image_id)?.handle();
            image_ranges[0] = make_subresource_range(aspect, view_base.range, view_base.flags);
            let owner = Framebuffer {
                device: Some(self.base.runtime_mut().device().clone()),
                framebuffer,
                render_pass,
                render_pass_key: rp_key,
                render_pass_cache: self.base.runtime_mut().render_pass_cache,
                render_area: fb_extent,
                num_color_buffers: if is_color { 1 } else { 0 },
                has_depth: !is_color,
                has_stencil: aspect.contains(vk::ImageAspectFlags::STENCIL),
                is_rescaled,
                samples: vk::SampleCountFlags::TYPE_1,
                rt_map: if is_color {
                    let mut map = [0; NUM_RT];
                    map[0] = 0;
                    map
                } else {
                    [0; NUM_RT]
                },
                images,
                image_ranges,
                num_images: 1,
                resolve_images: Vec::new(),
                resolve_image_views: Vec::new(),
                discard_msaa_color: false,
            };
            let framebuffer_id = self.base.slot_framebuffers.insert(Box::new(owner));
            self.base.framebuffers.insert(key, framebuffer_id);
            framebuffer_id
        };

        let fb = &self.base.slot_framebuffers[framebuffer_id];
        Some(BlitFramebufferInfo {
            framebuffer: fb.framebuffer,
            render_pass: fb.render_pass,
            render_area: fb.render_area,
            images: fb.images,
            image_ranges: fb.image_ranges,
            num_images: fb.num_images,
            samples: fb.samples,
            has_stencil: fb.has_stencil,
        })
    }

    fn conversion_image_view_from_image_view(
        &mut self,
        view_id: ImageViewId,
    ) -> Option<BlitImageView> {
        if !view_id.is_valid() || view_id == NULL_IMAGE_VIEW_ID {
            return None;
        }
        self.ensure_image_view(view_id).ok()?;
        self.image_view_depth_view(view_id)?;
        self.image_view_stencil_view(view_id)?;
        let view = self.backend_image_view(view_id)?;
        let is_rescaled = self
            .base
            .slot_images
            .get(view.base().image_id)
            .flags
            .contains(ImageFlagBits::RESCALED);
        Some(blit_image_view_from_backend(view, is_rescaled))
    }

    /// Vulkan-backed port of upstream `TextureCache<P>::BlitImage`.
    pub fn blit_image(
        &mut self,
        dst: &crate::engines::fermi_2d::Surface,
        src: &crate::engines::fermi_2d::Surface,
        copy: &crate::engines::fermi_2d::Config,
        mut gpu_to_cpu: impl FnMut(u64) -> Option<u64>,
        read_gpu_unsafe: impl Fn(u64, &mut [u8]) -> bool,
    ) -> bool {
        let dst_addr = dst.address();
        let src_addr = src.address();
        let mut dst_info = ImageInfo::from_fermi2d_surface(dst);
        let mut src_info = ImageInfo::from_fermi2d_surface(src);
        let can_be_depth_blit = dst_info.format == src_info.format
            && copy.filter == crate::engines::fermi_2d::Filter::Point;
        let try_options = if can_be_depth_blit {
            RelaxedOptions::SAMPLES | RelaxedOptions::FORMAT
        } else {
            RelaxedOptions::SAMPLES
        };

        let Some(src_cpu_addr) = gpu_to_cpu(src_addr) else {
            return false;
        };
        let Some(dst_cpu_addr) = gpu_to_cpu(dst_addr) else {
            return false;
        };

        let mut src_id;
        let mut dst_id;
        loop {
            self.base.has_deleted_images = false;
            src_id = self.base.find_image_in_cpu_region_with_caps(
                &src_info,
                src_addr,
                src_cpu_addr,
                try_options,
                self.base.has_broken_texture_view_formats,
                self.base.has_native_bgr,
            );
            dst_id = self.base.find_image_in_cpu_region_with_caps(
                &dst_info,
                dst_addr,
                dst_cpu_addr,
                try_options,
                self.base.has_broken_texture_view_formats,
                self.base.has_native_bgr,
            );
            if !copy.must_accelerate {
                let src_gpu_modified = src_id
                    .map(|id| {
                        self.base.slot_images[id]
                            .flags
                            .contains(ImageFlagBits::GPU_MODIFIED)
                    })
                    .unwrap_or(false);
                let dst_gpu_modified = dst_id
                    .map(|id| {
                        self.base.slot_images[id]
                            .flags
                            .contains(ImageFlagBits::GPU_MODIFIED)
                    })
                    .unwrap_or(false);
                if src_id.is_none() && dst_id.is_none() {
                    return false;
                }
                if !src_gpu_modified && !dst_gpu_modified {
                    return false;
                }
            }

            let src_image = src_id.map(|id| &self.base.slot_images[id]);
            if src_image.is_some_and(|image| image.info.num_samples > 1) {
                let msaa_options = RelaxedOptions::SAMPLES | RelaxedOptions::FORCE_BROKEN_VIEWS;
                src_id = Some(self.find_or_insert_image_from_info_with_options_and_finish(
                    &src_info,
                    src_addr,
                    src_cpu_addr,
                    msaa_options,
                    &read_gpu_unsafe,
                ));
                dst_id = Some(self.find_or_insert_image_from_info_with_options_and_finish(
                    &dst_info,
                    dst_addr,
                    dst_cpu_addr,
                    msaa_options,
                    &read_gpu_unsafe,
                ));
                if self.base.has_deleted_images {
                    continue;
                }
                break;
            }

            if can_be_depth_blit {
                let src_image = src_id.map(|id| &*self.base.slot_images[id]);
                let dst_image = dst_id.map(|id| &*self.base.slot_images[id]);
                crate::texture_cache::util::deduce_blit_images(
                    &mut dst_info,
                    &mut src_info,
                    dst_image,
                    src_image,
                );
                if crate::surface::get_format_type(dst_info.format)
                    != crate::surface::get_format_type(src_info.format)
                {
                    continue;
                }
            }

            if src_id.is_none() {
                src_id = Some(self.find_or_insert_image_from_info_with_options_and_finish(
                    &src_info,
                    src_addr,
                    src_cpu_addr,
                    RelaxedOptions::empty(),
                    &read_gpu_unsafe,
                ));
            }
            if dst_id.is_none() {
                dst_id = Some(self.find_or_insert_image_from_info_with_options_and_finish(
                    &dst_info,
                    dst_addr,
                    dst_cpu_addr,
                    RelaxedOptions::empty(),
                    &read_gpu_unsafe,
                ));
            }
            if !self.base.has_deleted_images {
                break;
            }
        }

        let mut src_id = src_id.unwrap_or(NULL_IMAGE_ID);
        let mut dst_id = dst_id.unwrap_or(NULL_IMAGE_ID);
        if !src_id.is_valid() || !dst_id.is_valid() {
            return false;
        }

        if !self.base_image_exists(src_id) || !self.base_image_exists(dst_id) {
            return false;
        }

        let native_bgr = self.base.has_native_bgr;
        if crate::surface::get_format_type(dst_info.format)
            != crate::surface::get_format_type(self.base.slot_images[dst_id].info.format)
            || crate::surface::get_format_type(src_info.format)
                != crate::surface::get_format_type(self.base.slot_images[src_id].info.format)
            || !crate::compatible_formats::is_view_compatible(
                dst_info.format,
                self.base.slot_images[dst_id].info.format,
                false,
                native_bgr,
            )
            || !crate::compatible_formats::is_view_compatible(
                src_info.format,
                self.base.slot_images[src_id].info.format,
                false,
                native_bgr,
            )
        {
            loop {
                self.base.has_deleted_images = false;
                src_id = self.find_or_insert_image_from_info_with_options_and_finish(
                    &src_info,
                    src_addr,
                    src_cpu_addr,
                    RelaxedOptions::empty(),
                    &read_gpu_unsafe,
                );
                dst_id = self.find_or_insert_image_from_info_with_options_and_finish(
                    &dst_info,
                    dst_addr,
                    dst_cpu_addr,
                    RelaxedOptions::empty(),
                    &read_gpu_unsafe,
                );
                if !self.base.has_deleted_images {
                    break;
                }
            }
            if !self.base_image_exists(src_id) || !self.base_image_exists(dst_id) {
                return false;
            }
        }

        if !self.base_image_exists(src_id) || !self.base_image_exists(dst_id) {
            return false;
        }
        self.base.prepare_image(src_id, false, false);
        self.base.prepare_image(dst_id, true, false);

        let mut is_src_rescaled = self.base.slot_images[src_id]
            .flags
            .contains(ImageFlagBits::RESCALED);
        let mut is_dst_rescaled = self.base.slot_images[dst_id]
            .flags
            .contains(ImageFlagBits::RESCALED);
        let is_resolve = self.base.slot_images[src_id].info.num_samples != 1
            && self.base.slot_images[dst_id].info.num_samples == 1;
        if is_src_rescaled != is_dst_rescaled {
            if self.base.image_can_rescale(src_id) {
                self.ensure_image_rescale_state(src_id, true, false);
                is_src_rescaled = self.base.slot_images[src_id]
                    .flags
                    .contains(ImageFlagBits::RESCALED);
                if is_resolve {
                    self.base.slot_images[dst_id].info.rescaleable = true;
                    let aliases = std::mem::take(&mut self.base.slot_images[dst_id].aliased_images);
                    for alias in &aliases {
                        self.base.slot_images[alias.id].info.rescaleable = true;
                    }
                    self.base.slot_images[dst_id].aliased_images = aliases;
                }
            }
            if self.base.image_can_rescale(dst_id) {
                self.ensure_image_rescale_state(dst_id, true, false);
                is_dst_rescaled = self.base.slot_images[dst_id]
                    .flags
                    .contains(ImageFlagBits::RESCALED);
            }
        }
        if is_resolve && is_src_rescaled != is_dst_rescaled {
            self.ensure_image_rescale_state(src_id, false, false);
            self.ensure_image_rescale_state(dst_id, false, false);
            is_src_rescaled = self.base.slot_images[src_id]
                .flags
                .contains(ImageFlagBits::RESCALED);
            is_dst_rescaled = self.base.slot_images[dst_id]
                .flags
                .contains(ImageFlagBits::RESCALED);
        }

        let Some(src_base) = self.base.slot_images[src_id].try_find_base(src_addr) else {
            return false;
        };
        let src_view_id = self.base.find_or_emplace_image_view(
            src_id,
            ImageViewInfo::for_render_target(
                ImageViewType::E2D,
                src_info.format,
                SubresourceRange {
                    base: src_base,
                    extent: SubresourceExtent {
                        levels: 1,
                        layers: 1,
                    },
                },
            ),
            src_addr,
        );
        let Some(dst_base) = self.base.slot_images[dst_id].try_find_base(dst_addr) else {
            return false;
        };
        let dst_view_id = self.base.find_or_emplace_image_view(
            dst_id,
            ImageViewInfo::for_render_target(
                ImageViewType::E2D,
                dst_info.format,
                SubresourceRange {
                    base: dst_base,
                    extent: SubresourceExtent {
                        levels: 1,
                        layers: 1,
                    },
                },
            ),
            dst_addr,
        );

        self.ensure_image_view(src_view_id).ok();
        self.ensure_image_view(dst_view_id).ok();
        let dst_framebuffer = match self.blit_framebuffer_from_image_view(dst_view_id) {
            Some(framebuffer) => framebuffer,
            None => return false,
        };
        let mut src_view = match self.take_backend_image_view(src_view_id) {
            Some(view) => view,
            None => return false,
        };
        let dst_view = match self.take_backend_image_view(dst_view_id) {
            Some(view) => view,
            None => {
                self.base.slot_image_views[src_view_id].backend = Some(src_view);
                return false;
            }
        };

        let (src_samples_x, src_samples_y) = crate::texture_cache::samples_helper::samples_log2(
            self.base.slot_images[src_id].info.num_samples as i32,
        );
        let (dst_samples_x, dst_samples_y) = crate::texture_cache::samples_helper::samples_log2(
            self.base.slot_images[dst_id].info.num_samples as i32,
        );
        let mut src_region = BlitRegion2D {
            start: BlitOffset2D {
                x: copy.src_x0 >> src_samples_x,
                y: copy.src_y0 >> src_samples_y,
            },
            end: BlitOffset2D {
                x: copy.src_x1 >> src_samples_x,
                y: copy.src_y1 >> src_samples_y,
            },
        };
        let mut dst_region = BlitRegion2D {
            start: BlitOffset2D {
                x: copy.dst_x0 >> dst_samples_x,
                y: copy.dst_y0 >> dst_samples_y,
            },
            end: BlitOffset2D {
                x: copy.dst_x1 >> dst_samples_x,
                y: copy.dst_y1 >> dst_samples_y,
            },
        };
        let resolution = common::settings::values().resolution_info.clone();
        let scale_region = |region: &mut BlitRegion2D| {
            region.start.x = resolution.scale_up_i32(region.start.x);
            region.start.y = resolution.scale_up_i32(region.start.y);
            region.end.x = resolution.scale_up_i32(region.end.x);
            region.end.y = resolution.scale_up_i32(region.end.y);
        };
        if is_src_rescaled {
            scale_region(&mut src_region);
        }
        if is_dst_rescaled {
            scale_region(&mut dst_region);
        }
        let copied = self.base.runtime_mut().blit_image(
            dst_framebuffer,
            &dst_view,
            &mut src_view,
            dst_region,
            src_region,
            copy.filter,
            copy.operation,
        );
        self.base.slot_image_views[src_view_id].backend = Some(src_view);
        self.base.slot_image_views[dst_view_id].backend = Some(dst_view);
        copied
    }

    fn copy_join_image(&mut self, dst_id: ImageId, src_id: ImageId, copies: &[ImageCopy]) -> bool {
        if !dst_id.is_valid() || !src_id.is_valid() || copies.is_empty() {
            return true;
        }
        if !self.base_image_exists(dst_id) || !self.base_image_exists(src_id) {
            return false;
        }
        let dst_base = self.base.slot_images[dst_id].base.as_ref().clone();
        let src_base = self.base.slot_images[src_id].base.as_ref().clone();
        if dst_base.flags.contains(ImageFlagBits::RESCALED)
            != src_base.flags.contains(ImageFlagBits::RESCALED)
        {
            return false;
        }
        let mut copies = copies.to_vec();
        let is_rescaled = src_base.flags.contains(ImageFlagBits::RESCALED);
        if is_rescaled {
            let both_2d = src_base.info.image_type == ImageType::E2D
                && dst_base.info.image_type == ImageType::E2D;
            let resolution = common::settings::values().resolution_info.clone();
            for copy in &mut copies {
                copy.src_offset.x = resolution.scale_up_i32(copy.src_offset.x);
                copy.dst_offset.x = resolution.scale_up_i32(copy.dst_offset.x);
                copy.extent.width = resolution.scale_up_u32(copy.extent.width);
                if both_2d {
                    copy.src_offset.y = resolution.scale_up_i32(copy.src_offset.y);
                    copy.dst_offset.y = resolution.scale_up_i32(copy.dst_offset.y);
                    copy.extent.height = resolution.scale_up_u32(copy.extent.height);
                }
            }
        }
        let Some(operation) = select_join_copy_operation(
            &dst_base,
            &src_base,
            self.base.runtime_mut().shader_stencil_export_supported,
        ) else {
            return false;
        };
        let dst_aspect = image_aspect_mask(dst_base.info.format);
        let src_aspect = image_aspect_mask(src_base.info.format);
        let same_format_type = matches!(
            operation,
            JoinCopyOperation::CopyImage | JoinCopyOperation::CopyImageMsaa
        );
        if dst_aspect.is_empty()
            || src_aspect.is_empty()
            || (same_format_type && dst_aspect != src_aspect)
        {
            return false;
        }
        let dst_format = self
            .base
            .runtime()
            .surface_format(dst_base.info.format, false);
        let src_format = self
            .base
            .runtime()
            .surface_format(src_base.info.format, false);
        if self
            .ensure_image(dst_id, &dst_base, dst_format, dst_aspect)
            .is_err()
            || self
                .ensure_image(src_id, &src_base, src_format, src_aspect)
                .is_err()
        {
            return false;
        }
        if operation == JoinCopyOperation::CopyImageMsaa {
            let Some(mut src) = self.take_backend_image(src_id) else {
                return false;
            };
            let Some(mut dst) = self.take_backend_image(dst_id) else {
                self.base.slot_images[src_id].backend = Some(src);
                return false;
            };
            let copied = self
                .base
                .runtime_mut()
                .copy_image_msaa(&mut dst, &mut src, &copies);
            self.base.slot_images[dst_id].backend = Some(dst);
            self.base.slot_images[src_id].backend = Some(src);
            return copied;
        }

        if operation == JoinCopyOperation::Convert {
            return self.convert_join_image(dst_id, src_id, &dst_base, &src_base, &copies);
        }

        let Some(src) = self.take_backend_image(src_id) else {
            return false;
        };
        let Some(dst) = self.take_backend_image(dst_id) else {
            self.base.slot_images[src_id].backend = Some(src);
            return false;
        };
        let copied = match operation {
            JoinCopyOperation::CopyImage => {
                self.base.runtime_mut().copy_image(&dst, &src, &copies);
                true
            }
            JoinCopyOperation::Reinterpret => self
                .base
                .runtime_mut()
                .reinterpret_image(&dst, &src, &copies),
            JoinCopyOperation::CopyImageMsaa | JoinCopyOperation::Convert => unreachable!(),
        };
        self.base.slot_images[dst_id].backend = Some(dst);
        self.base.slot_images[src_id].backend = Some(src);
        copied
    }

    fn convert_join_image(
        &mut self,
        dst_id: ImageId,
        src_id: ImageId,
        dst_base: &ImageBase,
        src_base: &ImageBase,
        copies: &[ImageCopy],
    ) -> bool {
        for copy in copies {
            if copy.dst_subresource.num_layers != 1
                || copy.src_subresource.num_layers != 1
                || copy.src_offset != crate::texture_cache::types::Offset3D::default()
                || copy.dst_offset != crate::texture_cache::types::Offset3D::default()
            {
                return false;
            }

            let dst_range = SubresourceRange {
                base: crate::texture_cache::types::SubresourceBase {
                    level: copy.dst_subresource.base_level,
                    layer: copy.dst_subresource.base_layer,
                },
                extent: SubresourceExtent {
                    levels: 1,
                    layers: 1,
                },
            };
            let src_range = SubresourceRange {
                base: crate::texture_cache::types::SubresourceBase {
                    level: copy.src_subresource.base_level,
                    layer: copy.src_subresource.base_layer,
                },
                extent: SubresourceExtent {
                    levels: 1,
                    layers: 1,
                },
            };
            let mut dst_format = dst_base.info.format;
            if crate::surface::get_format_type(src_base.info.format) == SurfaceType::DepthStencil
                && crate::surface::get_format_type(dst_format) == SurfaceType::ColorTexture
                && crate::surface::bytes_per_block(dst_format) == 4
            {
                dst_format = PixelFormat::A8B8G8R8Unorm;
            }
            let dst_view_id = self.base.find_or_emplace_image_view(
                dst_id,
                ImageViewInfo::for_render_target(ImageViewType::E2D, dst_format, dst_range),
                dst_base.cpu_addr,
            );
            let src_view_id = self.base.find_or_emplace_image_view(
                src_id,
                ImageViewInfo::for_render_target(
                    ImageViewType::E2D,
                    src_base.info.format,
                    src_range,
                ),
                src_base.cpu_addr,
            );
            let Some(dst_framebuffer) = self.blit_framebuffer_from_image_view(dst_view_id) else {
                return false;
            };
            let Some(src_view) = self.conversion_image_view_from_image_view(src_view_id) else {
                return false;
            };
            let expected_width = self
                .base
                .slot_image_views
                .get(dst_view_id)
                .size
                .width
                .min(self.base.slot_image_views.get(src_view_id).size.width);
            let expected_height = self
                .base
                .slot_image_views
                .get(dst_view_id)
                .size
                .height
                .min(self.base.slot_image_views.get(src_view_id).size.height);
            let expected_depth = self
                .base
                .slot_image_views
                .get(dst_view_id)
                .size
                .depth
                .min(self.base.slot_image_views.get(src_view_id).size.depth);
            let mut scaled_extent = crate::texture_cache::types::Extent3D {
                width: expected_width,
                height: expected_height,
                depth: expected_depth,
            };
            if src_base.flags.contains(ImageFlagBits::RESCALED) {
                let resolution = common::settings::values().resolution_info.clone();
                scaled_extent.width = resolution.scale_up_u32(scaled_extent.width);
                scaled_extent.height = resolution.scale_up_u32(scaled_extent.height);
            }
            if copy.extent != scaled_extent {
                return false;
            }
            if !self.base.runtime_mut().convert_image(
                dst_framebuffer,
                dst_format,
                src_base.info.format,
                src_view,
            ) {
                return false;
            }
        }
        true
    }

    pub fn tick_frame(&mut self, scheduler_tick: u64) {
        if self.base.runtime().can_report_memory_usage() {
            let used_memory = self.base.runtime().get_device_memory_usage();
            self.base.update_total_used_memory_from_runtime(used_memory);
        }
        if self.base.total_used_memory > self.base.minimum_memory {
            let runtime = self.base.runtime_mut() as *mut TextureCacheRuntime;
            self.base.run_garbage_collector_with_downloader(
                |_image_id, base_image, backend, staging| {
                    // SAFETY: texture-cache work is serialized on the GPU
                    // thread and the runtime is stored in a stable Box.
                    let runtime = unsafe { &mut *runtime };
                    if staging.is_empty() {
                        return false;
                    }
                    let Some(image) = backend.as_mut() else {
                        return false;
                    };
                    let copies = full_download_copies(&base_image.info);
                    let Some(staging_buffer) =
                        runtime.download_staging_buffer(staging.len() as vk::DeviceSize, false)
                    else {
                        return false;
                    };
                    if !image.download_memory_to_staging(runtime, &staging_buffer, &copies) {
                        return false;
                    }
                    runtime.finish();
                    let mapped =
                        unsafe { std::slice::from_raw_parts(staging_buffer.mapped, staging.len()) };
                    staging.copy_from_slice(mapped);
                    true
                },
            );
        }
        self.base.tick_delayed_destruction_rings();
        self.base.tick_async_decode();
        self.base.tick_async_unswizzle();
        self.base.runtime_mut().tick_frame(scheduler_tick);
        self.base.tick_frame();
        let runtime = self.base.runtime_mut() as *mut TextureCacheRuntime;
        for buffer in &mut self.base.async_buffers_death_ring {
            // SAFETY: see the runtime ownership invariant above.
            unsafe { &mut *runtime }.free_deferred_staging_buffer(buffer);
        }
        self.base.async_buffers_death_ring.clear();
    }

    /// Port of `TextureCache<P>::CheckFeedbackLoop` with the Vulkan runtime
    /// kept at the backend-owned boundary.
    pub fn check_feedback_loop(&mut self, views: &[ImageViewInOut]) {
        let base = &mut self.base;
        let runtime = base.runtime_mut() as *mut TextureCacheRuntime;
        base.check_feedback_loop(views, || unsafe { &mut *runtime }.barrier_feedback_loop());
    }

    pub fn prepare_framebuffer_for_present(&mut self, image_id: ImageId) {
        let (image, old_layout) = self.base.slot_images[image_id]
            .backend
            .as_ref()
            .map(|target| (target.handle(), target.layout))
            .unwrap_or((vk::Image::null(), vk::ImageLayout::UNDEFINED));
        if image == vk::Image::null() {
            return;
        }
        if old_layout != vk::ImageLayout::GENERAL {
            self.transition_layout(
                image,
                old_layout,
                vk::ImageLayout::GENERAL,
                vk::ImageAspectFlags::COLOR,
            );
            if let Some(target) = self.backend_image_mut(image_id) {
                target.layout = vk::ImageLayout::GENERAL;
                target.exchange_initialization();
            }
        }
    }

    /// Port-facing subset of upstream `TextureCache<P>::TryFindFramebufferImageView`.
    pub fn try_find_framebuffer_image_view(
        &mut self,
        config: &FramebufferConfig,
        cpu_addr: u64,
    ) -> Option<FramebufferImageViewVulkan> {
        let framebuffer_view = self
            .base
            .try_find_framebuffer_image_view(config, cpu_addr)?;
        self.ensure_image_view(framebuffer_view.view_id).ok()?;
        let view = self.backend_image_view(framebuffer_view.view_id)?;
        let target_image = view.image_handle();
        let image_view = view.handle(TextureType::Color2D);
        Some(FramebufferImageViewVulkan {
            width: framebuffer_view.view.size.width,
            height: framebuffer_view.view.size.height,
            common: framebuffer_view,
            image: target_image,
            image_view,
        })
    }

    /// Port-facing equivalent of upstream `TextureCache<P>::GetImageView(id)`.
    pub fn get_image_view(&self, view_id: ImageViewId) -> Option<&ImageView> {
        if !view_id.is_valid() {
            return None;
        }
        self.backend_image_view(view_id)
    }

    /// Accessors used by upstream `GraphicsPipeline::ConfigureImpl` when a
    /// buffer image view is forwarded to `BufferCache`.
    pub(crate) fn image_view_buffer_info(
        &self,
        view_id: ImageViewId,
    ) -> Option<(u64, u32, PixelFormat)> {
        let view = self.backend_image_view(view_id)?;
        Some((view.base().gpu_addr, view.buffer_size, view.base().format))
    }

    /// `slot_image_views[NULL_IMAGE_VIEW_ID].Handle(texture_type)` from
    /// upstream's texture cache.
    pub fn null_image_view_handle(&self, texture_type: TextureType) -> vk::ImageView {
        self.backend_image_view(NULL_IMAGE_VIEW_ID)
            .map(|view| view.handle(texture_type))
            .unwrap_or(vk::ImageView::null())
    }

    pub fn image_view_depth_view(&mut self, view_id: ImageViewId) -> Option<vk::ImageView> {
        self.ensure_image_view(view_id).ok()?;
        self.backend_image_view_mut(view_id)?.depth_view().ok()
    }

    pub fn image_view_stencil_view(&mut self, view_id: ImageViewId) -> Option<vk::ImageView> {
        self.ensure_image_view(view_id).ok()?;
        self.backend_image_view_mut(view_id)?.stencil_view().ok()
    }

    pub fn image_view_color_view(&mut self, view_id: ImageViewId) -> Option<vk::ImageView> {
        self.ensure_image_view(view_id).ok()?;
        self.backend_image_view_mut(view_id)?.color_view().ok()
    }

    pub fn image_view_storage_view(
        &mut self,
        view_id: ImageViewId,
        texture_type: TextureType,
        image_format: ImageFormat,
    ) -> Option<vk::ImageView> {
        if !self.backend_image_view(view_id).is_some() {
            return None;
        }
        self.storage_view(view_id, texture_type, image_format)
    }

    /// `slot_image_views[NULL_IMAGE_VIEW_ID].StorageView(...)` from upstream.
    pub fn null_storage_image_view(
        &mut self,
        texture_type: TextureType,
        image_format: ImageFormat,
    ) -> Option<vk::ImageView> {
        self.storage_view(NULL_IMAGE_VIEW_ID, texture_type, image_format)
    }

    fn storage_view(
        &mut self,
        view_id: ImageViewId,
        texture_type: TextureType,
        image_format: ImageFormat,
    ) -> Option<vk::ImageView> {
        if self.backend_image_view(view_id)?.image_handle() == vk::Image::null() {
            return Some(vk::ImageView::null());
        }
        if image_format == ImageFormat::Typeless {
            let cached = self.backend_image_view(view_id)?.typeless_storage_view;
            if cached != vk::ImageView::null() {
                return Some(cached);
            }
            let format = self
                .base
                .runtime()
                .surface_format(self.backend_image_view(view_id)?.base().format, true);
            let view = self
                .backend_image_view(view_id)?
                .make_view(format, vk::ImageAspectFlags::COLOR, Some(texture_type))
                .ok()?;
            self.backend_image_view_mut(view_id)?.typeless_storage_view = view;
            return Some(view);
        }
        let is_signed = matches!(image_format, ImageFormat::R8Sint | ImageFormat::R16Sint);
        let index = texture_type as usize;
        let cached = if is_signed {
            self.backend_image_view(view_id)?.storage_signeds[index]
        } else {
            self.backend_image_view(view_id)?.storage_unsigneds[index]
        };
        if cached != vk::ImageView::null() {
            return Some(cached);
        }
        let format = image_format_to_vk(image_format);
        let view = self
            .backend_image_view(view_id)?
            .make_view(format, vk::ImageAspectFlags::COLOR, Some(texture_type))
            .ok()?;
        let target = self.backend_image_view_mut(view_id)?;
        if is_signed {
            target.storage_signeds[index] = view;
        } else {
            target.storage_unsigneds[index] = view;
        }
        Some(view)
    }

    /// Port-facing subset of upstream `TextureCache::GetSampler(id).Handle()`.
    pub fn sampler_handle(&self, sampler_id: SamplerId) -> Option<vk::Sampler> {
        self.sampler(sampler_id).map(CachedSampler::handle)
    }

    /// Port-facing equivalent of upstream `TextureCache::GetSampler`.
    pub fn sampler(&self, sampler_id: SamplerId) -> Option<&CachedSampler> {
        self.base.slot_samplers[sampler_id].backend.as_ref()
    }

    /// Vulkan forwarding wrapper for upstream `GetSamplerId(index, compute)`.
    pub fn get_sampler_id(&mut self, index: u32, compute: bool) -> SamplerId {
        self.base.get_sampler_id(index, compute)
    }

    /// Vulkan specialization of upstream `TextureCache<P>::FillImageViews`.
    pub fn fill_image_views(
        &mut self,
        views: &mut [crate::texture_cache::texture_cache_base::ImageViewInOut],
        compute: bool,
        blacklist: bool,
    ) {
        self.base.fill_image_views(views, compute, blacklist);
    }

    /// Resolve and materialize the graphics TIC selected by Maxwell's
    /// draw-texture state.
    ///
    /// This is the backend half of upstream `TextureCache::GetImageView(u32)`:
    /// the common cache visits the graphics descriptor table, then the Vulkan
    /// cache prepares the image and returns its render-target view.
    pub fn draw_texture_source(&mut self, index: u32) -> Option<DrawTextureSource> {
        let mut selected = [crate::texture_cache::texture_cache_base::ImageViewInOut {
            index,
            blacklist: false,
            id: NULL_IMAGE_VIEW_ID,
        }];
        self.fill_image_views(&mut selected, false, false);
        let view_id = selected[0].id;
        if !view_id.is_valid() || view_id == NULL_IMAGE_VIEW_ID {
            return None;
        }
        let view = self.backend_image_view(view_id)?;
        let image_id = view.base().image_id;
        let image = self.backend_image(image_id)?;
        Some(DrawTextureSource {
            image_view: view.render_target(),
            image: view.image_handle(),
            size: BlitExtent3D {
                width: view.base().size.width,
                height: view.base().size.height,
                depth: view.base().size.depth,
            },
            is_rescaled: image.is_rescaled(),
        })
    }

    /// Backend-owned completion of upstream `TextureCache<P>::PrepareImageView`.
    ///
    /// The typed slot constructs the backend payload at insertion time, then
    /// this applies upstream's `PrepareImage` lifecycle before publication.
    fn create_sampler_from_tsc(
        runtime: &TextureCacheRuntime,
        tsc: &TscEntry,
    ) -> Result<CachedSampler, vk::Result> {
        let mag_filter = texture_filter_from_raw(tsc.mag_filter());
        let min_filter = texture_filter_from_raw(tsc.min_filter());
        let mipmap_filter = texture_mipmap_filter_from_raw(tsc.mipmap_filter());
        let wrap_u = wrap_mode_from_raw(tsc.wrap_u());
        let wrap_v = wrap_mode_from_raw(tsc.wrap_v());
        let wrap_p = wrap_mode_from_raw(tsc.wrap_p());
        let max_anisotropy = tsc.computed_max_anisotropy().clamp(1.0, 16.0);
        let border_color = tsc.computed_border_color();
        let reduction = sampler_reduction_from_raw(tsc.reduction_filter());
        let reduction_mode = maxwell_to_vk::sampler_reduction(reduction);
        let create_sampler = |anisotropy: f32, force_nearest: bool, disable_compare: bool| {
            let mut custom_border_color = vk::SamplerCustomBorderColorCreateInfoEXT::builder()
                .custom_border_color(vk::ClearColorValue {
                    float32: border_color,
                })
                .format(vk::Format::UNDEFINED)
                .build();
            let mut reduction_info = vk::SamplerReductionModeCreateInfo::builder()
                .reduction_mode(reduction_mode)
                .build();
            let mut sampler_info = vk::SamplerCreateInfo::builder()
                .mag_filter(if force_nearest {
                    vk::Filter::NEAREST
                } else {
                    maxwell_to_vk::sampler::filter(mag_filter)
                })
                .min_filter(if force_nearest {
                    vk::Filter::NEAREST
                } else {
                    maxwell_to_vk::sampler::filter(min_filter)
                })
                .mipmap_mode(if force_nearest {
                    vk::SamplerMipmapMode::NEAREST
                } else {
                    maxwell_to_vk::sampler::mipmap_mode(mipmap_filter)
                })
                .address_mode_u(maxwell_to_vk::sampler::wrap_mode(
                    runtime.vulkan_device(),
                    wrap_u,
                    mag_filter,
                ))
                .address_mode_v(maxwell_to_vk::sampler::wrap_mode(
                    runtime.vulkan_device(),
                    wrap_v,
                    mag_filter,
                ))
                .address_mode_w(maxwell_to_vk::sampler::wrap_mode(
                    runtime.vulkan_device(),
                    wrap_p,
                    mag_filter,
                ))
                .mip_lod_bias(tsc.lod_bias())
                .anisotropy_enable(!force_nearest && anisotropy > 1.0)
                .max_anisotropy(if force_nearest { 1.0 } else { anisotropy })
                .compare_enable(!disable_compare && tsc.depth_compare_enabled() != 0)
                .compare_op(maxwell_to_vk::sampler::depth_compare_function(
                    crate::textures::texture::DepthCompareFunc::from_raw(tsc.depth_compare_func()),
                ))
                .min_lod(if mipmap_filter == TextureMipmapFilter::None {
                    0.0
                } else {
                    tsc.min_lod()
                })
                .max_lod(if mipmap_filter == TextureMipmapFilter::None {
                    0.25
                } else {
                    tsc.max_lod()
                })
                .border_color(convert_border_color(border_color));
            if runtime.custom_border_color_supported {
                sampler_info = sampler_info
                    .push_next(&mut custom_border_color)
                    .border_color(vk::BorderColor::FLOAT_CUSTOM_EXT);
            }
            if runtime.sampler_filter_minmax_supported {
                sampler_info = sampler_info.push_next(&mut reduction_info);
            } else if reduction_mode != vk::SamplerReductionMode::WEIGHTED_AVERAGE {
                log::warn!("VK_EXT_sampler_filter_minmax is required");
            }
            unsafe { runtime.device().create_sampler(&sampler_info.build(), None) }
        };

        let sampler = create_sampler(max_anisotropy, false, false)?;
        let max_anisotropy_default = (1u32 << tsc.max_anisotropy_raw()) as f32;
        let sampler_default_anisotropy = if max_anisotropy > max_anisotropy_default {
            match create_sampler(max_anisotropy_default, false, false) {
                Ok(handle) => handle,
                Err(error) => {
                    unsafe {
                        runtime.device().destroy_sampler(sampler, None);
                    }
                    return Err(error);
                }
            }
        } else {
            vk::Sampler::null()
        };
        let has_linear_filtering = maxwell_to_vk::sampler::filter(mag_filter) == vk::Filter::LINEAR
            || maxwell_to_vk::sampler::filter(min_filter) == vk::Filter::LINEAR
            || maxwell_to_vk::sampler::mipmap_mode(mipmap_filter) == vk::SamplerMipmapMode::LINEAR;
        let sampler_nearest = if has_linear_filtering {
            match create_sampler(1.0, true, false) {
                Ok(handle) => handle,
                Err(error) => {
                    runtime.destroy_sampler(CachedSampler {
                        device: Some(runtime.device().clone()),
                        sampler,
                        sampler_default_anisotropy,
                        sampler_nearest: vk::Sampler::null(),
                        sampler_noncompare: vk::Sampler::null(),
                    });
                    return Err(error);
                }
            }
        } else {
            vk::Sampler::null()
        };
        let sampler_noncompare = if tsc.depth_compare_enabled() != 0 {
            match create_sampler(max_anisotropy, false, true) {
                Ok(handle) => handle,
                Err(error) => {
                    runtime.destroy_sampler(CachedSampler {
                        device: Some(runtime.device().clone()),
                        sampler,
                        sampler_default_anisotropy,
                        sampler_nearest,
                        sampler_noncompare: vk::Sampler::null(),
                    });
                    return Err(error);
                }
            }
        } else {
            vk::Sampler::null()
        };
        Ok(CachedSampler {
            device: Some(runtime.device().clone()),
            sampler,
            sampler_default_anisotropy,
            sampler_nearest,
            sampler_noncompare,
        })
    }
    /// Get or create a framebuffer for the given render target configuration.
    pub fn get_or_create_framebuffer(
        &mut self,
        render_pass: vk::RenderPass,
        color_views: &[vk::ImageView],
        depth_view: Option<vk::ImageView>,
        width: u32,
        height: u32,
    ) -> Result<vk::Framebuffer, vk::Result> {
        // Build attachment list
        let mut attachments = SmallVec::<[vk::ImageView; NUM_RT + 1]>::from_slice(color_views);
        if let Some(dv) = depth_view {
            attachments.push(dv);
        }

        let fb_info = vk::FramebufferCreateInfo::builder()
            .render_pass(render_pass)
            .attachments(&attachments)
            .width(width)
            .height(height)
            .layers(1)
            .build();

        unsafe {
            self.base
                .runtime_mut()
                .device()
                .create_framebuffer(&fb_info, None)
        }
    }

    /// Record an image layout transition.
    pub fn transition_layout(
        &mut self,
        image: vk::Image,
        old_layout: vk::ImageLayout,
        new_layout: vk::ImageLayout,
        aspect: vk::ImageAspectFlags,
    ) {
        let device = self.base.runtime_mut().device().clone();
        let scheduler = self.base.runtime_mut().scheduler();
        scheduler.request_outside_render_pass_operation_context();
        scheduler.record(move |cmdbuf| unsafe {
            cmd_transition_layout(&device, cmdbuf, image, old_layout, new_layout, aspect);
        });
    }
}

unsafe fn cmd_transition_layout(
    device: &ash::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    old_layout: vk::ImageLayout,
    new_layout: vk::ImageLayout,
    aspect: vk::ImageAspectFlags,
) {
    let (src_access, src_stage, dst_access, dst_stage) = match (old_layout, new_layout) {
        (vk::ImageLayout::UNDEFINED, vk::ImageLayout::GENERAL) => (
            vk::AccessFlags::empty(),
            vk::PipelineStageFlags::TOP_OF_PIPE,
            vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE,
            vk::PipelineStageFlags::ALL_COMMANDS,
        ),
        (vk::ImageLayout::UNDEFINED, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL) => (
            vk::AccessFlags::empty(),
            vk::PipelineStageFlags::TOP_OF_PIPE,
            vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
        ),
        (vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL, vk::ImageLayout::TRANSFER_SRC_OPTIMAL) => (
            vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags::TRANSFER_READ,
            vk::PipelineStageFlags::TRANSFER,
        ),
        (vk::ImageLayout::UNDEFINED, vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL) => (
            vk::AccessFlags::empty(),
            vk::PipelineStageFlags::TOP_OF_PIPE,
            vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE,
            vk::PipelineStageFlags::EARLY_FRAGMENT_TESTS,
        ),
        (vk::ImageLayout::TRANSFER_SRC_OPTIMAL, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL) => (
            vk::AccessFlags::TRANSFER_READ,
            vk::PipelineStageFlags::TRANSFER,
            vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
        ),
        (vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL, vk::ImageLayout::GENERAL) => (
            vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            vk::AccessFlags::SHADER_READ,
            vk::PipelineStageFlags::FRAGMENT_SHADER,
        ),
        (vk::ImageLayout::GENERAL, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL) => (
            vk::AccessFlags::SHADER_READ,
            vk::PipelineStageFlags::FRAGMENT_SHADER,
            vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
        ),
        _ => (
            vk::AccessFlags::empty(),
            vk::PipelineStageFlags::TOP_OF_PIPE,
            vk::AccessFlags::empty(),
            vk::PipelineStageFlags::BOTTOM_OF_PIPE,
        ),
    };

    let barrier = vk::ImageMemoryBarrier::builder()
        .old_layout(old_layout)
        .new_layout(new_layout)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: aspect,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        })
        .src_access_mask(src_access)
        .dst_access_mask(dst_access)
        .build();

    device.cmd_pipeline_barrier(
        cmd,
        src_stage,
        dst_stage,
        vk::DependencyFlags::empty(),
        &[],
        &[],
        &[barrier],
    );
}

struct RangedBarrierRange {
    min_mip: u32,
    max_mip: u32,
    min_layer: u32,
    max_layer: u32,
}

impl Default for RangedBarrierRange {
    fn default() -> Self {
        Self {
            min_mip: u32::MAX,
            max_mip: u32::MIN,
            min_layer: u32::MAX,
            max_layer: u32::MIN,
        }
    }
}

impl RangedBarrierRange {
    fn add_layers(&mut self, layers: vk::ImageSubresourceLayers) {
        self.min_mip = self.min_mip.min(layers.mip_level);
        self.max_mip = self.max_mip.max(layers.mip_level + 1);
        self.min_layer = self.min_layer.min(layers.base_array_layer);
        self.max_layer = self
            .max_layer
            .max(layers.base_array_layer + layers.layer_count);
    }

    fn subresource_range(&self, aspect: vk::ImageAspectFlags) -> vk::ImageSubresourceRange {
        vk::ImageSubresourceRange {
            aspect_mask: aspect,
            base_mip_level: self.min_mip,
            level_count: self.max_mip - self.min_mip,
            base_array_layer: self.min_layer,
            layer_count: self.max_layer - self.min_layer,
        }
    }
}

fn make_image_subresource_layers(
    layers: crate::texture_cache::types::SubresourceLayers,
    aspect: vk::ImageAspectFlags,
) -> vk::ImageSubresourceLayers {
    vk::ImageSubresourceLayers {
        aspect_mask: aspect,
        mip_level: layers.base_level as u32,
        base_array_layer: layers.base_layer as u32,
        layer_count: layers.num_layers as u32,
    }
}

fn make_image_copy(copy: &ImageCopy, aspect: vk::ImageAspectFlags) -> vk::ImageCopy {
    vk::ImageCopy {
        src_subresource: make_image_subresource_layers(copy.src_subresource, aspect),
        src_offset: vk::Offset3D {
            x: copy.src_offset.x,
            y: copy.src_offset.y,
            z: copy.src_offset.z,
        },
        dst_subresource: make_image_subresource_layers(copy.dst_subresource, aspect),
        dst_offset: vk::Offset3D {
            x: copy.dst_offset.x,
            y: copy.dst_offset.y,
            z: copy.dst_offset.z,
        },
        extent: vk::Extent3D {
            width: copy.extent.width,
            height: copy.extent.height,
            depth: copy.extent.depth,
        },
    }
}

struct CopyImageBarriers {
    pre: [vk::ImageMemoryBarrier; 2],
    post: [vk::ImageMemoryBarrier; 2],
}

fn make_copy_image_barriers(
    src_image: vk::Image,
    dst_image: vk::Image,
    aspect: vk::ImageAspectFlags,
    copies: &[vk::ImageCopy],
) -> CopyImageBarriers {
    let mut dst_range = RangedBarrierRange::default();
    let mut src_range = RangedBarrierRange::default();
    for copy in copies {
        dst_range.add_layers(copy.dst_subresource);
        src_range.add_layers(copy.src_subresource);
    }
    let write_access = vk::AccessFlags::SHADER_WRITE
        | vk::AccessFlags::COLOR_ATTACHMENT_WRITE
        | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE
        | vk::AccessFlags::TRANSFER_WRITE;
    CopyImageBarriers {
        pre: [
            vk::ImageMemoryBarrier::builder()
                .src_access_mask(write_access)
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                .old_layout(vk::ImageLayout::GENERAL)
                .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(src_image)
                .subresource_range(src_range.subresource_range(aspect))
                .build(),
            vk::ImageMemoryBarrier::builder()
                .src_access_mask(write_access)
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .old_layout(vk::ImageLayout::GENERAL)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(dst_image)
                .subresource_range(dst_range.subresource_range(aspect))
                .build(),
        ],
        post: [
            vk::ImageMemoryBarrier::builder()
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::empty())
                .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(src_image)
                .subresource_range(src_range.subresource_range(aspect))
                .build(),
            vk::ImageMemoryBarrier::builder()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(
                    vk::AccessFlags::SHADER_READ
                        | vk::AccessFlags::SHADER_WRITE
                        | vk::AccessFlags::COLOR_ATTACHMENT_READ
                        | vk::AccessFlags::COLOR_ATTACHMENT_WRITE
                        | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_READ
                        | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE
                        | vk::AccessFlags::TRANSFER_READ
                        | vk::AccessFlags::TRANSFER_WRITE,
                )
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(dst_image)
                .subresource_range(dst_range.subresource_range(aspect))
                .build(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::texture_cache::image_view_info::SwizzleSource;
    use crate::texture_cache::types::{Extent3D, Offset3D, SubresourceExtent, SubresourceLayers};
    use ash::vk::Handle;

    fn rgba_swizzle() -> [u8; 4] {
        [
            SwizzleSource::R as u8,
            SwizzleSource::G as u8,
            SwizzleSource::B as u8,
            SwizzleSource::A as u8,
        ]
    }

    #[test]
    fn render_target_dirty_adapter_keeps_draw_copy_and_live_engine_in_sync() {
        let mut maxwell3d = Maxwell3D::new();
        let mut draw_flags = [false; 256];
        maxwell3d.set_dirty_flag(crate::dirty_flags::flags::COLOR_BUFFER0);
        let mut access = VulkanRenderTargetDirtyFlags {
            draw_flags: &mut draw_flags,
            maxwell3d: Some(NonNull::from(&mut maxwell3d)),
        };

        assert!(access.render_target_dirty_flag(crate::dirty_flags::flags::COLOR_BUFFER0));
        access.clear_render_target_dirty_flag(crate::dirty_flags::flags::COLOR_BUFFER0);
        assert!(!access.draw_flags[crate::dirty_flags::flags::COLOR_BUFFER0 as usize]);
        assert!(!maxwell3d.dirty_flag(crate::dirty_flags::flags::COLOR_BUFFER0));

        access.set_render_target_dirty_flag(crate::dirty_flags::flags::DEPTH_BIAS_GLOBAL);
        assert!(access.draw_flags[crate::dirty_flags::flags::DEPTH_BIAS_GLOBAL as usize]);
        assert!(maxwell3d.dirty_flag(crate::dirty_flags::flags::DEPTH_BIAS_GLOBAL));
    }

    #[test]
    fn storage_image_views_override_type_and_clamp_non_array_layers() {
        let mut view =
            ImageViewBase::null(crate::texture_cache::image_view_base::NullImageViewParams);
        view.view_type = ImageViewType::E2DArray;
        view.range.extent.layers = 4;

        let (sampled_type, sampled_range) =
            aux_image_view_params(&view, vk::ImageAspectFlags::COLOR, None);
        assert_eq!(sampled_type, vk::ImageViewType::TYPE_2D_ARRAY);
        assert_eq!(sampled_range.layer_count, 4);

        let (storage_type, storage_range) = aux_image_view_params(
            &view,
            vk::ImageAspectFlags::COLOR,
            Some(TextureType::Color2D),
        );
        assert_eq!(storage_type, vk::ImageViewType::TYPE_2D);
        assert_eq!(storage_range.layer_count, 1);

        let (array_type, array_range) = aux_image_view_params(
            &view,
            vk::ImageAspectFlags::COLOR,
            Some(TextureType::ColorArray2D),
        );
        assert_eq!(array_type, vk::ImageViewType::TYPE_2D_ARRAY);
        assert_eq!(array_range.layer_count, 4);
    }

    #[test]
    fn image_view_swizzle_transforms_16_bit_formats_like_upstream() {
        let mut swizzle = rgba_swizzle();
        try_transform_swizzle_if_needed(PixelFormat::A1B5G5R5Unorm, &mut swizzle, false, false);
        assert_eq!(
            swizzle,
            [
                SwizzleSource::B as u8,
                SwizzleSource::G as u8,
                SwizzleSource::R as u8,
                SwizzleSource::A as u8,
            ]
        );

        let mut swizzle = rgba_swizzle();
        try_transform_swizzle_if_needed(PixelFormat::B5G6R5Unorm, &mut swizzle, false, false);
        assert_eq!(swizzle, rgba_swizzle());
        try_transform_swizzle_if_needed(PixelFormat::B5G6R5Unorm, &mut swizzle, true, false);
        assert_eq!(
            swizzle,
            [
                SwizzleSource::B as u8,
                SwizzleSource::G as u8,
                SwizzleSource::R as u8,
                SwizzleSource::A as u8,
            ]
        );

        let mut swizzle = rgba_swizzle();
        try_transform_swizzle_if_needed(PixelFormat::A5B5G5R1Unorm, &mut swizzle, false, false);
        assert_eq!(
            swizzle,
            [
                SwizzleSource::A as u8,
                SwizzleSource::B as u8,
                SwizzleSource::G as u8,
                SwizzleSource::R as u8,
            ]
        );

        let mut swizzle = rgba_swizzle();
        try_transform_swizzle_if_needed(PixelFormat::G4R4Unorm, &mut swizzle, false, false);
        assert_eq!(
            swizzle,
            [
                SwizzleSource::G as u8,
                SwizzleSource::R as u8,
                SwizzleSource::B as u8,
                SwizzleSource::A as u8,
            ]
        );

        let mut swizzle = rgba_swizzle();
        try_transform_swizzle_if_needed(PixelFormat::A4B4G4R4Unorm, &mut swizzle, false, false);
        assert_eq!(swizzle, rgba_swizzle());
        try_transform_swizzle_if_needed(PixelFormat::A4B4G4R4Unorm, &mut swizzle, false, true);
        assert_eq!(
            swizzle,
            [
                SwizzleSource::A as u8,
                SwizzleSource::B as u8,
                SwizzleSource::G as u8,
                SwizzleSource::R as u8,
            ]
        );
    }

    #[test]
    fn image_view_aspect_uses_image_view_info_like_upstream() {
        let mut info = ImageViewInfo {
            format: PixelFormat::D24UnormS8Uint,
            ..ImageViewInfo::default()
        };
        assert_eq!(image_view_aspect_mask(&info), vk::ImageAspectFlags::DEPTH);

        info.x_source = SwizzleSource::G as u8;
        assert_eq!(image_view_aspect_mask(&info), vk::ImageAspectFlags::STENCIL);

        let render_target = ImageViewInfo::for_render_target(
            ImageViewType::E2D,
            PixelFormat::D24UnormS8Uint,
            SubresourceRange::default(),
        );
        assert_eq!(
            image_view_aspect_mask(&render_target),
            vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL
        );
    }

    #[test]
    fn d32_color_blit_destination_list_matches_upstream_boundaries() {
        assert!(color_blit_from_d32_destination(PixelFormat::B5G6R5Unorm));
        assert!(color_blit_from_d32_destination(PixelFormat::Bc1RgbaUnorm));
        assert!(color_blit_from_d32_destination(PixelFormat::R32Float));
        assert!(!color_blit_from_d32_destination(PixelFormat::R16Float));
        assert!(!color_blit_from_d32_destination(PixelFormat::D32Float));
    }

    #[test]
    fn depth_stencil_view_swizzle_converts_green_to_red_like_upstream() {
        let mut swizzle = rgba_swizzle();
        swizzle
            .iter_mut()
            .for_each(|source| *source = convert_green_red(*source));
        assert_eq!(
            swizzle,
            [
                SwizzleSource::R as u8,
                SwizzleSource::R as u8,
                SwizzleSource::B as u8,
                SwizzleSource::A as u8,
            ]
        );

        let mut unsupported_one = [
            SwizzleSource::R as u8,
            SwizzleSource::OneFloat as u8,
            SwizzleSource::OneInt as u8,
            SwizzleSource::A as u8,
        ];
        sanitize_depth_stencil_swizzle(&mut unsupported_one, false);
        assert_eq!(
            unsupported_one,
            [
                SwizzleSource::R as u8,
                SwizzleSource::Zero as u8,
                SwizzleSource::Zero as u8,
                SwizzleSource::A as u8,
            ]
        );

        let mut supported_one = [SwizzleSource::OneFloat as u8; 4];
        sanitize_depth_stencil_swizzle(&mut supported_one, true);
        assert_eq!(supported_one, [SwizzleSource::OneFloat as u8; 4]);
    }

    #[test]
    fn null_image_matches_upstream_fallback_create_info() {
        let info = null_image_info();
        assert_eq!(info.format, PixelFormat::A8B8G8R8Unorm);
        assert_eq!(info.image_type, ImageType::E1D);
        assert_eq!(
            info.size,
            Extent3D {
                width: 1,
                height: 1,
                depth: 1,
            }
        );
        assert_eq!(info.resources, SubresourceExtent::default());
        assert_eq!(info.num_samples, 1);

        let format_info = maxwell_to_vk::surface_format_table(info.format);
        let image_info = make_image_create_info(&info, format_info);
        assert_eq!(image_info.format, vk::Format::A8B8G8R8_UNORM_PACK32);
        assert!(image_info.usage.contains(
            vk::ImageUsageFlags::TRANSFER_SRC
                | vk::ImageUsageFlags::TRANSFER_DST
                | vk::ImageUsageFlags::SAMPLED
                | vk::ImageUsageFlags::STORAGE
                | vk::ImageUsageFlags::COLOR_ATTACHMENT
        ));
    }

    #[test]
    fn astc_backend_flags_match_upstream_vulkan_image_policy() {
        use common::settings_enums::{AstcDecodeMode, AstcRecompression};

        let info = ImageInfo {
            format: PixelFormat::Astc2d4x4Unorm,
            image_type: ImageType::E2D,
            size: Extent3D {
                width: 64,
                height: 64,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        let classify = |decode_mode, recompression, native_astc, depth| {
            let mut image_info = info.clone();
            image_info.size.depth = depth;
            let mut image = ImageBase::new(image_info, 0, 0);
            TextureCache::apply_backend_image_flags_with_capabilities(
                &mut image,
                native_astc,
                true,
                decode_mode,
                recompression,
            );
            image.flags
        };

        let gpu = classify(
            AstcDecodeMode::Gpu,
            AstcRecompression::Uncompressed,
            false,
            1,
        );
        assert!(gpu.contains(ImageFlagBits::ACCELERATED_UPLOAD));
        assert!(!gpu.contains(ImageFlagBits::ASYNCHRONOUS_DECODE));
        assert!(gpu.contains(ImageFlagBits::CONVERTED | ImageFlagBits::COSTLY_LOAD));

        let asynchronous = classify(
            AstcDecodeMode::CpuAsynchronous,
            AstcRecompression::Uncompressed,
            false,
            1,
        );
        assert!(asynchronous.contains(ImageFlagBits::ASYNCHRONOUS_DECODE));
        assert!(!asynchronous.contains(ImageFlagBits::ACCELERATED_UPLOAD));

        let three_dimensional = classify(
            AstcDecodeMode::Gpu,
            AstcRecompression::Uncompressed,
            false,
            2,
        );
        assert!(!three_dimensional.contains(ImageFlagBits::ACCELERATED_UPLOAD));
        assert!(three_dimensional.contains(ImageFlagBits::CONVERTED | ImageFlagBits::COSTLY_LOAD));

        let native = classify(
            AstcDecodeMode::Gpu,
            AstcRecompression::Uncompressed,
            true,
            1,
        );
        assert!(!native.intersects(
            ImageFlagBits::ACCELERATED_UPLOAD
                | ImageFlagBits::ASYNCHRONOUS_DECODE
                | ImageFlagBits::CONVERTED
                | ImageFlagBits::COSTLY_LOAD
        ));
    }

    #[test]
    fn compatible_reinterpretation_uses_mutable_image_format_list() {
        assert!(crate::compatible_formats::is_view_compatible(
            PixelFormat::A2B10G10R10Unorm,
            PixelFormat::A8B8G8R8Unorm,
            false,
            true,
        ));

        let formats = [
            vk::Format::A2B10G10R10_UNORM_PACK32,
            vk::Format::A8B8G8R8_UNORM_PACK32,
        ];
        let mut format_list = vk::ImageFormatListCreateInfo::builder()
            .view_formats(&formats)
            .build();
        let mut image_info = vk::ImageCreateInfo::default();
        apply_image_format_list(&mut image_info, &mut format_list, &formats, true);

        assert!(image_info
            .flags
            .contains(vk::ImageCreateFlags::MUTABLE_FORMAT));
        assert!(!image_info.p_next.is_null());
    }

    #[test]
    fn render_target_framebuffer_matches_upstream_sparse_rt_slot_map() {
        let color_range = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 2,
            level_count: 1,
            base_array_layer: 3,
            layer_count: 1,
        };
        let owner = Framebuffer {
            device: None,
            framebuffer: vk::Framebuffer::null(),
            render_pass: vk::RenderPass::null(),
            render_pass_key: RenderPassKey::default(),
            render_pass_cache: NonNull::dangling(),
            render_area: vk::Extent2D {
                width: 1280,
                height: 720,
            },
            num_color_buffers: 2,
            has_depth: false,
            has_stencil: false,
            is_rescaled: false,
            samples: vk::SampleCountFlags::TYPE_1,
            rt_map: [0, 0, 0, 0, 1, 0, 0, 0],
            num_images: 2,
            images: [
                vk::Image::from_raw(1),
                vk::Image::from_raw(2),
                vk::Image::null(),
                vk::Image::null(),
                vk::Image::null(),
                vk::Image::null(),
                vk::Image::null(),
                vk::Image::null(),
                vk::Image::null(),
            ],
            image_ranges: [color_range; NUM_RT + 1],
            resolve_images: Vec::new(),
            resolve_image_views: Vec::new(),
            discard_msaa_color: false,
        };
        let framebuffer = owner.render_target_framebuffer();

        assert!(framebuffer.has_aspect_color_bit(0));
        assert!(framebuffer.has_aspect_color_bit(1));
        assert!(framebuffer.has_aspect_color_bit(2));
        assert!(framebuffer.has_aspect_color_bit(4));
        let blit = framebuffer.blit_framebuffer_info();
        assert_eq!(blit.num_images, 2);
        assert_eq!(blit.images, owner.images);
        assert!(blit.image_ranges[..blit.num_images]
            .iter()
            .all(|range| range.aspect_mask == color_range.aspect_mask
                && range.base_mip_level == color_range.base_mip_level
                && range.level_count == color_range.level_count
                && range.base_array_layer == color_range.base_array_layer
                && range.layer_count == color_range.layer_count));
    }

    #[test]
    fn convert_border_color_matches_upstream_fallback() {
        assert_eq!(
            convert_border_color([0.0, 0.0, 0.0, 0.0]),
            vk::BorderColor::FLOAT_TRANSPARENT_BLACK
        );
        assert_eq!(
            convert_border_color([0.0, 0.0, 0.0, 1.0]),
            vk::BorderColor::FLOAT_OPAQUE_BLACK
        );
        assert_eq!(
            convert_border_color([1.0, 1.0, 1.0, 1.0]),
            vk::BorderColor::FLOAT_OPAQUE_WHITE
        );
        assert_eq!(
            convert_border_color([0.46, 0.45, 0.45, 0.0]),
            vk::BorderColor::FLOAT_OPAQUE_WHITE
        );
        assert_eq!(
            convert_border_color([0.1, 0.2, 0.3, 0.6]),
            vk::BorderColor::FLOAT_OPAQUE_BLACK
        );
        assert_eq!(
            convert_border_color([0.1, 0.2, 0.3, 0.4]),
            vk::BorderColor::FLOAT_TRANSPARENT_BLACK
        );
    }

    #[test]
    fn sampler_reduction_field_maps_to_upstream_vulkan_modes() {
        assert_eq!(
            maxwell_to_vk::sampler_reduction(sampler_reduction_from_raw(0)),
            vk::SamplerReductionMode::WEIGHTED_AVERAGE
        );
        assert_eq!(
            maxwell_to_vk::sampler_reduction(sampler_reduction_from_raw(1)),
            vk::SamplerReductionMode::MIN
        );
        assert_eq!(
            maxwell_to_vk::sampler_reduction(sampler_reduction_from_raw(2)),
            vk::SamplerReductionMode::MAX
        );
    }

    #[test]
    #[should_panic(expected = "invalid Maxwell sampler reduction mode 3")]
    fn sampler_reduction_rejects_reserved_value() {
        let _ = sampler_reduction_from_raw(3);
    }

    #[test]
    fn image_usage_flags_use_resolved_format_info_storage_bit() {
        let mut format_info = maxwell_to_vk::surface_format_table(PixelFormat::A2B10G10R10Unorm);
        assert!(format_info.storage);
        assert!(
            image_usage_flags(format_info, PixelFormat::A2B10G10R10Unorm)
                .contains(vk::ImageUsageFlags::STORAGE)
        );

        format_info.storage = false;
        assert!(
            !image_usage_flags(format_info, PixelFormat::A2B10G10R10Unorm)
                .contains(vk::ImageUsageFlags::STORAGE)
        );
    }

    #[test]
    fn mutable_image_view_usage_stays_within_base_image_usage() {
        let image_format = PixelFormat::A8B8G8R8Srgb;
        let view_format = PixelFormat::A8B8G8R8Unorm;
        let image_format_info = maxwell_to_vk::surface_format_table(image_format);
        let view_format_info = maxwell_to_vk::surface_format_table(view_format);

        let image_usage = image_usage_flags(image_format_info, image_format);
        let requested_view_usage = image_usage_flags(view_format_info, view_format);
        let view_usage = image_view_usage_flags(
            view_format_info,
            view_format,
            image_format_info,
            image_format,
        );

        assert!(!image_usage.contains(vk::ImageUsageFlags::STORAGE));
        assert!(requested_view_usage.contains(vk::ImageUsageFlags::STORAGE));
        assert!(!view_usage.contains(vk::ImageUsageFlags::STORAGE));
        assert!(image_usage.contains(view_usage));
    }

    #[test]
    fn ranged_barrier_range_matches_upstream_min_max_layers() {
        let mut range = RangedBarrierRange::default();

        range.add_layers(vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            mip_level: 4,
            base_array_layer: 6,
            layer_count: 2,
        });
        range.add_layers(vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            mip_level: 2,
            base_array_layer: 3,
            layer_count: 4,
        });

        let vk_range = range.subresource_range(vk::ImageAspectFlags::COLOR);
        assert_eq!(vk_range.aspect_mask, vk::ImageAspectFlags::COLOR);
        assert_eq!(vk_range.base_mip_level, 2);
        assert_eq!(vk_range.level_count, 3);
        assert_eq!(vk_range.base_array_layer, 3);
        assert_eq!(vk_range.layer_count, 5);
    }

    #[test]
    fn make_image_copy_matches_upstream_field_mapping() {
        let copy = ImageCopy {
            src_subresource: SubresourceLayers {
                base_level: 1,
                base_layer: 2,
                num_layers: 3,
            },
            dst_subresource: SubresourceLayers {
                base_level: 4,
                base_layer: 5,
                num_layers: 6,
            },
            src_offset: Offset3D { x: -1, y: 2, z: 3 },
            dst_offset: Offset3D { x: 4, y: -5, z: 6 },
            extent: Extent3D {
                width: 7,
                height: 8,
                depth: 9,
            },
        };

        let vk_copy = make_image_copy(&copy, vk::ImageAspectFlags::DEPTH);

        assert_eq!(
            vk_copy.src_subresource.aspect_mask,
            vk::ImageAspectFlags::DEPTH
        );
        assert_eq!(vk_copy.src_subresource.mip_level, 1);
        assert_eq!(vk_copy.src_subresource.base_array_layer, 2);
        assert_eq!(vk_copy.src_subresource.layer_count, 3);
        assert_eq!(vk_copy.src_offset, vk::Offset3D { x: -1, y: 2, z: 3 });
        assert_eq!(
            vk_copy.dst_subresource.aspect_mask,
            vk::ImageAspectFlags::DEPTH
        );
        assert_eq!(vk_copy.dst_subresource.mip_level, 4);
        assert_eq!(vk_copy.dst_subresource.base_array_layer, 5);
        assert_eq!(vk_copy.dst_subresource.layer_count, 6);
        assert_eq!(vk_copy.dst_offset, vk::Offset3D { x: 4, y: -5, z: 6 });
        assert_eq!(
            vk_copy.extent,
            vk::Extent3D {
                width: 7,
                height: 8,
                depth: 9,
            }
        );
    }

    #[test]
    fn copy_image_barriers_match_upstream_access_layout_and_ranges() {
        let src_image = vk::Image::from_raw(0x1000);
        let dst_image = vk::Image::from_raw(0x2000);
        let copies = [
            vk::ImageCopy {
                src_subresource: vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 3,
                    base_array_layer: 5,
                    layer_count: 2,
                },
                dst_subresource: vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 1,
                    base_array_layer: 4,
                    layer_count: 3,
                },
                src_offset: vk::Offset3D::default(),
                dst_offset: vk::Offset3D::default(),
                extent: vk::Extent3D {
                    width: 16,
                    height: 16,
                    depth: 1,
                },
            },
            vk::ImageCopy {
                src_subresource: vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 1,
                    base_array_layer: 2,
                    layer_count: 1,
                },
                dst_subresource: vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 4,
                    base_array_layer: 1,
                    layer_count: 2,
                },
                src_offset: vk::Offset3D::default(),
                dst_offset: vk::Offset3D::default(),
                extent: vk::Extent3D {
                    width: 8,
                    height: 8,
                    depth: 1,
                },
            },
        ];

        let barriers =
            make_copy_image_barriers(src_image, dst_image, vk::ImageAspectFlags::COLOR, &copies);
        let write_access = vk::AccessFlags::SHADER_WRITE
            | vk::AccessFlags::COLOR_ATTACHMENT_WRITE
            | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE
            | vk::AccessFlags::TRANSFER_WRITE;
        let final_dst_access = vk::AccessFlags::SHADER_READ
            | vk::AccessFlags::SHADER_WRITE
            | vk::AccessFlags::COLOR_ATTACHMENT_READ
            | vk::AccessFlags::COLOR_ATTACHMENT_WRITE
            | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_READ
            | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE
            | vk::AccessFlags::TRANSFER_READ
            | vk::AccessFlags::TRANSFER_WRITE;

        assert_eq!(barriers.pre[0].src_access_mask, write_access);
        assert_eq!(
            barriers.pre[0].dst_access_mask,
            vk::AccessFlags::TRANSFER_READ
        );
        assert_eq!(barriers.pre[0].old_layout, vk::ImageLayout::GENERAL);
        assert_eq!(
            barriers.pre[0].new_layout,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL
        );
        assert_eq!(barriers.pre[0].image, src_image);
        assert_eq!(barriers.pre[0].subresource_range.base_mip_level, 1);
        assert_eq!(barriers.pre[0].subresource_range.level_count, 3);
        assert_eq!(barriers.pre[0].subresource_range.base_array_layer, 2);
        assert_eq!(barriers.pre[0].subresource_range.layer_count, 5);

        assert_eq!(barriers.pre[1].src_access_mask, write_access);
        assert_eq!(
            barriers.pre[1].dst_access_mask,
            vk::AccessFlags::TRANSFER_WRITE
        );
        assert_eq!(barriers.pre[1].old_layout, vk::ImageLayout::GENERAL);
        assert_eq!(
            barriers.pre[1].new_layout,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL
        );
        assert_eq!(barriers.pre[1].image, dst_image);
        assert_eq!(barriers.pre[1].subresource_range.base_mip_level, 1);
        assert_eq!(barriers.pre[1].subresource_range.level_count, 4);
        assert_eq!(barriers.pre[1].subresource_range.base_array_layer, 1);
        assert_eq!(barriers.pre[1].subresource_range.layer_count, 6);

        assert_eq!(barriers.post[0].src_access_mask, vk::AccessFlags::empty());
        assert_eq!(barriers.post[0].dst_access_mask, vk::AccessFlags::empty());
        assert_eq!(
            barriers.post[0].old_layout,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL
        );
        assert_eq!(barriers.post[0].new_layout, vk::ImageLayout::GENERAL);
        assert_eq!(barriers.post[0].image, src_image);
        assert_eq!(
            barriers.post[0].subresource_range.aspect_mask,
            barriers.pre[0].subresource_range.aspect_mask
        );
        assert_eq!(
            barriers.post[0].subresource_range.base_mip_level,
            barriers.pre[0].subresource_range.base_mip_level
        );
        assert_eq!(
            barriers.post[0].subresource_range.level_count,
            barriers.pre[0].subresource_range.level_count
        );
        assert_eq!(
            barriers.post[0].subresource_range.base_array_layer,
            barriers.pre[0].subresource_range.base_array_layer
        );
        assert_eq!(
            barriers.post[0].subresource_range.layer_count,
            barriers.pre[0].subresource_range.layer_count
        );

        assert_eq!(
            barriers.post[1].src_access_mask,
            vk::AccessFlags::TRANSFER_WRITE
        );
        assert_eq!(barriers.post[1].dst_access_mask, final_dst_access);
        assert_eq!(
            barriers.post[1].old_layout,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL
        );
        assert_eq!(barriers.post[1].new_layout, vk::ImageLayout::GENERAL);
        assert_eq!(barriers.post[1].image, dst_image);
        assert_eq!(
            barriers.post[1].subresource_range.aspect_mask,
            barriers.pre[1].subresource_range.aspect_mask
        );
        assert_eq!(
            barriers.post[1].subresource_range.base_mip_level,
            barriers.pre[1].subresource_range.base_mip_level
        );
        assert_eq!(
            barriers.post[1].subresource_range.level_count,
            barriers.pre[1].subresource_range.level_count
        );
        assert_eq!(
            barriers.post[1].subresource_range.base_array_layer,
            barriers.pre[1].subresource_range.base_array_layer
        );
        assert_eq!(
            barriers.post[1].subresource_range.layer_count,
            barriers.pre[1].subresource_range.layer_count
        );
    }

    #[test]
    fn join_shrink_copy_selects_runtime_copy_image() {
        let mut full = ImageInfo {
            format: PixelFormat::A8B8G8R8Unorm,
            image_type: ImageType::E2D,
            resources: SubresourceExtent {
                levels: 2,
                layers: 1,
            },
            size: Extent3D {
                width: 64,
                height: 64,
                depth: 1,
            },
            ..ImageInfo::default()
        };
        full.layer_stride = crate::texture_cache::util::calculate_layer_stride(&full);
        full.maybe_unaligned_layer_stride = crate::texture_cache::util::calculate_layer_size(&full);
        let sub = ImageInfo {
            resources: SubresourceExtent {
                levels: 1,
                layers: 1,
            },
            size: Extent3D {
                width: 32,
                height: 32,
                depth: 1,
            },
            ..full.clone()
        };

        let full_base = ImageBase::new(full.clone(), 0x5000, 0x9000);
        let mip_offset = full_base.mip_level_offsets[1] as u64;
        let sub_base = ImageBase::new(sub.clone(), 0x5000 + mip_offset, 0x9000 + mip_offset);
        let base = full_base
            .try_find_base(sub_base.gpu_addr)
            .expect("mip-sized overlap must map into the full image");
        let copies = make_shrink_image_copies(&full, &sub, base, 1, 0);

        assert!(!copies.is_empty());
        assert_eq!(
            select_join_copy_operation(&full_base, &sub_base, true),
            Some(JoinCopyOperation::CopyImage)
        );
    }

    #[test]
    fn same_surface_type_with_different_block_sizes_reaches_runtime_copy_image() {
        let dst = ImageBase::new(
            ImageInfo {
                format: PixelFormat::A8B8G8R8Unorm,
                ..ImageInfo::default()
            },
            0x1000,
            0x2000,
        );
        let src = ImageBase::new(
            ImageInfo {
                format: PixelFormat::R8Unorm,
                ..ImageInfo::default()
            },
            0x3000,
            0x4000,
        );

        assert_eq!(
            crate::surface::get_format_type(dst.info.format),
            crate::surface::get_format_type(src.info.format)
        );
        assert_ne!(
            crate::surface::bytes_per_block(dst.info.format),
            crate::surface::bytes_per_block(src.info.format)
        );
        assert_eq!(
            select_join_copy_operation(&dst, &src, true),
            Some(JoinCopyOperation::CopyImage)
        );
    }

    #[test]
    fn transform_buffer_image_copies_splits_depth_stencil_like_upstream() {
        let copies = [
            BufferImageCopy {
                buffer_offset: 0x20,
                buffer_size: 0x100,
                buffer_row_length: 64,
                buffer_image_height: 32,
                image_subresource: SubresourceLayers {
                    base_level: 1,
                    base_layer: 2,
                    num_layers: 3,
                },
                image_offset: Offset3D { x: 4, y: 5, z: 6 },
                image_extent: Extent3D {
                    width: 16,
                    height: 8,
                    depth: 2,
                },
            },
            BufferImageCopy {
                buffer_offset: 0x220,
                buffer_size: 0x80,
                ..BufferImageCopy::default()
            },
        ];

        let transformed = transform_buffer_image_copies(
            &copies,
            0x1000,
            vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL,
        );

        assert_eq!(transformed.len(), copies.len() * 2);
        assert_eq!(
            transformed
                .iter()
                .map(|copy| copy.image_subresource.aspect_mask)
                .collect::<Vec<_>>(),
            vec![
                vk::ImageAspectFlags::DEPTH,
                vk::ImageAspectFlags::DEPTH,
                vk::ImageAspectFlags::STENCIL,
                vk::ImageAspectFlags::STENCIL,
            ]
        );
        assert_eq!(transformed[0].buffer_offset, 0x1020);
        assert_eq!(transformed[2].buffer_offset, transformed[0].buffer_offset);
        assert_eq!(transformed[0].buffer_row_length, 64);
        assert_eq!(transformed[0].buffer_image_height, 32);
        assert_eq!(transformed[0].image_subresource.mip_level, 1);
        assert_eq!(transformed[0].image_subresource.base_array_layer, 2);
        assert_eq!(transformed[0].image_subresource.layer_count, 3);
        assert_eq!(
            transformed[0].image_offset,
            vk::Offset3D { x: 4, y: 5, z: 6 }
        );
        assert_eq!(
            transformed[0].image_extent,
            vk::Extent3D {
                width: 16,
                height: 8,
                depth: 2,
            }
        );
    }

    #[test]
    fn msaa_upload_is_limited_to_non_integer_color_images() {
        let mut info = ImageInfo {
            format: PixelFormat::A8B8G8R8Unorm,
            num_samples: 4,
            ..ImageInfo::default()
        };
        assert!(wants_msaa_upload(&info, vk::ImageAspectFlags::COLOR));
        assert!(!wants_msaa_upload(&info, vk::ImageAspectFlags::DEPTH));
        info.format = PixelFormat::A8B8G8R8Uint;
        assert!(!wants_msaa_upload(&info, vk::ImageAspectFlags::COLOR));
        info.format = PixelFormat::A8B8G8R8Unorm;
        info.num_samples = 1;
        assert!(!wants_msaa_upload(&info, vk::ImageAspectFlags::COLOR));
    }

    #[test]
    fn framebuffer_without_owned_resolve_images_matches_upstream_accessors() {
        let framebuffer = Framebuffer {
            device: None,
            framebuffer: vk::Framebuffer::null(),
            render_pass: vk::RenderPass::null(),
            render_pass_key: RenderPassKey::default(),
            render_pass_cache: NonNull::dangling(),
            render_area: vk::Extent2D::default(),
            num_color_buffers: 0,
            has_depth: false,
            has_stencil: false,
            is_rescaled: false,
            samples: vk::SampleCountFlags::TYPE_1,
            rt_map: [0; NUM_RT],
            images: [vk::Image::null(); NUM_RT + 1],
            image_ranges: [vk::ImageSubresourceRange::default(); NUM_RT + 1],
            num_images: 0,
            resolve_images: Vec::new(),
            resolve_image_views: Vec::new(),
            discard_msaa_color: false,
        };

        assert!(!framebuffer.has_resolve_color());
        assert_eq!(framebuffer.resolve_color_image(0), vk::Image::null());
        let handle = framebuffer.render_target_framebuffer();
        assert_eq!(handle.num_images(), framebuffer.num_images);
        assert_eq!(*handle.images(), framebuffer.images);
        assert_eq!(handle.framebuffer_owner, NonNull::from(&framebuffer));
    }

    #[test]
    fn msaa_upload_copies_scale_guest_sample_grid_like_upstream() {
        let copy = BufferImageCopy {
            image_subresource: SubresourceLayers {
                base_level: 2,
                base_layer: 0,
                num_layers: 1,
            },
            image_offset: Offset3D { x: 12, y: 10, z: 3 },
            image_extent: Extent3D {
                width: 64,
                height: 32,
                depth: 2,
            },
            ..BufferImageCopy::default()
        };
        let copies = make_msaa_upload_copies(&[copy], 8);
        assert_eq!(copies.len(), 1);
        assert_eq!(copies[0].src_offset, Offset3D::default());
        assert_eq!(copies[0].dst_offset, Offset3D { x: 3, y: 5, z: 3 });
        assert_eq!(
            copies[0].extent,
            Extent3D {
                width: 16,
                height: 16,
                depth: 2,
            }
        );
        assert_eq!(copies[0].src_subresource, copy.image_subresource);
        assert_eq!(copies[0].dst_subresource, copy.image_subresource);
    }
}

fn wants_msaa_upload(info: &ImageInfo, aspect: vk::ImageAspectFlags) -> bool {
    info.num_samples > 1
        && aspect.contains(vk::ImageAspectFlags::COLOR)
        && !crate::surface::is_pixel_format_integer(info.format)
}

fn make_msaa_upload_copies(copies: &[BufferImageCopy], num_samples: u32) -> Vec<ImageCopy> {
    let (samples_x, samples_y) =
        crate::texture_cache::samples_helper::samples_log2(num_samples as i32);
    copies
        .iter()
        .map(|copy| ImageCopy {
            src_offset: Offset3D::default(),
            dst_offset: Offset3D {
                x: copy.image_offset.x >> samples_x,
                y: copy.image_offset.y >> samples_y,
                z: copy.image_offset.z,
            },
            src_subresource: copy.image_subresource,
            dst_subresource: copy.image_subresource,
            extent: Extent3D {
                width: copy.image_extent.width >> samples_x,
                height: copy.image_extent.height >> samples_y,
                depth: copy.image_extent.depth,
            },
        })
        .collect()
}

fn transform_buffer_image_copies(
    copies: &[BufferImageCopy],
    base_offset: vk::DeviceSize,
    aspect: vk::ImageAspectFlags,
) -> Vec<vk::BufferImageCopy> {
    let make = |copy: &BufferImageCopy, aspect_mask: vk::ImageAspectFlags| {
        vk::BufferImageCopy::builder()
            .buffer_offset(base_offset + copy.buffer_offset as vk::DeviceSize)
            .buffer_row_length(copy.buffer_row_length)
            .buffer_image_height(copy.buffer_image_height)
            .image_subresource(vk::ImageSubresourceLayers {
                aspect_mask,
                mip_level: copy.image_subresource.base_level as u32,
                base_array_layer: copy.image_subresource.base_layer as u32,
                layer_count: copy.image_subresource.num_layers as u32,
            })
            .image_offset(vk::Offset3D {
                x: copy.image_offset.x,
                y: copy.image_offset.y,
                z: copy.image_offset.z,
            })
            .image_extent(vk::Extent3D {
                width: copy.image_extent.width,
                height: copy.image_extent.height,
                depth: copy.image_extent.depth,
            })
            .build()
    };
    if aspect == (vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL) {
        let mut result = Vec::with_capacity(copies.len() * 2);
        result.extend(
            copies
                .iter()
                .map(|copy| make(copy, vk::ImageAspectFlags::DEPTH)),
        );
        result.extend(
            copies
                .iter()
                .map(|copy| make(copy, vk::ImageAspectFlags::STENCIL)),
        );
        result
    } else {
        copies.iter().map(|copy| make(copy, aspect)).collect()
    }
}

fn copy_buffer_to_image(
    device: &ash::Device,
    cmd: vk::CommandBuffer,
    src_buffer: vk::Buffer,
    image: vk::Image,
    aspect_mask: vk::ImageAspectFlags,
    is_initialized: bool,
    copies: &[vk::BufferImageCopy],
) {
    let write_access = vk::AccessFlags::SHADER_WRITE
        | vk::AccessFlags::COLOR_ATTACHMENT_WRITE
        | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE;
    let read_access = vk::AccessFlags::SHADER_READ
        | vk::AccessFlags::COLOR_ATTACHMENT_READ
        | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_READ;
    let mut range = RangedBarrierRange::default();
    for copy in copies {
        range.add_layers(copy.image_subresource);
    }
    let subresource_range = range.subresource_range(aspect_mask);
    let read_barrier = vk::ImageMemoryBarrier::builder()
        .src_access_mask(write_access)
        .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
        .old_layout(if is_initialized {
            vk::ImageLayout::GENERAL
        } else {
            vk::ImageLayout::UNDEFINED
        })
        .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(subresource_range)
        .build();
    let write_barrier = vk::ImageMemoryBarrier::builder()
        .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
        .dst_access_mask(write_access | read_access)
        .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
        .new_layout(vk::ImageLayout::GENERAL)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(subresource_range)
        .build();
    unsafe {
        device.cmd_pipeline_barrier(
            cmd,
            vk::PipelineStageFlags::LATE_FRAGMENT_TESTS
                | vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT
                | vk::PipelineStageFlags::COMPUTE_SHADER,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[read_barrier],
        );
        device.cmd_copy_buffer_to_image(
            cmd,
            src_buffer,
            image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            copies,
        );
        device.cmd_pipeline_barrier(
            cmd,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::LATE_FRAGMENT_TESTS
                | vk::PipelineStageFlags::COMPUTE_SHADER
                | vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[write_barrier],
        );
    }
}

fn make_buffer_image_copy(
    copy: &ImageCopy,
    is_src: bool,
    aspect: vk::ImageAspectFlags,
) -> vk::BufferImageCopy {
    let subresource = if is_src {
        copy.src_subresource
    } else {
        copy.dst_subresource
    };
    let offset = if is_src {
        copy.src_offset
    } else {
        copy.dst_offset
    };
    vk::BufferImageCopy {
        buffer_offset: 0,
        buffer_row_length: 0,
        buffer_image_height: 0,
        image_subresource: make_image_subresource_layers(subresource, aspect),
        image_offset: vk::Offset3D {
            x: offset.x,
            y: offset.y,
            z: offset.z,
        },
        image_extent: vk::Extent3D {
            width: copy.extent.width,
            height: copy.extent.height,
            depth: copy.extent.depth,
        },
    }
}

fn make_image_subresource_layers_from_view(view: &ImageView) -> vk::ImageSubresourceLayers {
    vk::ImageSubresourceLayers {
        aspect_mask: image_aspect_mask(view.base().format),
        mip_level: view.base().range.base.level.max(0) as u32,
        base_array_layer: view.base().range.base.layer.max(0) as u32,
        layer_count: view.base().range.extent.layers.max(1) as u32,
    }
}

fn make_image_blit(
    dst_region: BlitRegion2D,
    src_region: BlitRegion2D,
    dst_layers: vk::ImageSubresourceLayers,
    src_layers: vk::ImageSubresourceLayers,
) -> vk::ImageBlit {
    vk::ImageBlit {
        src_subresource: src_layers,
        src_offsets: [
            vk::Offset3D {
                x: src_region.start.x,
                y: src_region.start.y,
                z: 0,
            },
            vk::Offset3D {
                x: src_region.end.x,
                y: src_region.end.y,
                z: 1,
            },
        ],
        dst_subresource: dst_layers,
        dst_offsets: [
            vk::Offset3D {
                x: dst_region.start.x,
                y: dst_region.start.y,
                z: 0,
            },
            vk::Offset3D {
                x: dst_region.end.x,
                y: dst_region.end.y,
                z: 1,
            },
        ],
    }
}

fn make_image_resolve(
    dst_region: BlitRegion2D,
    src_region: BlitRegion2D,
    dst_layers: vk::ImageSubresourceLayers,
    src_layers: vk::ImageSubresourceLayers,
) -> vk::ImageResolve {
    vk::ImageResolve {
        src_subresource: src_layers,
        src_offset: vk::Offset3D {
            x: src_region.start.x,
            y: src_region.start.y,
            z: 0,
        },
        dst_subresource: dst_layers,
        dst_offset: vk::Offset3D {
            x: dst_region.start.x,
            y: dst_region.start.y,
            z: 0,
        },
        extent: vk::Extent3D {
            width: (dst_region.end.x - dst_region.start.x).max(0) as u32,
            height: (dst_region.end.y - dst_region.start.y).max(0) as u32,
            depth: 1,
        },
    }
}

fn convert_image_type(image_type: ImageType) -> vk::ImageType {
    match image_type {
        ImageType::E1D => vk::ImageType::TYPE_1D,
        ImageType::E2D | ImageType::Linear => vk::ImageType::TYPE_2D,
        ImageType::E3D => vk::ImageType::TYPE_3D,
        ImageType::Buffer => vk::ImageType::TYPE_2D,
    }
}

fn convert_sample_count(num_samples: u32) -> vk::SampleCountFlags {
    match num_samples {
        1 => vk::SampleCountFlags::TYPE_1,
        2 => vk::SampleCountFlags::TYPE_2,
        4 => vk::SampleCountFlags::TYPE_4,
        8 => vk::SampleCountFlags::TYPE_8,
        16 => vk::SampleCountFlags::TYPE_16,
        _ => vk::SampleCountFlags::TYPE_1,
    }
}

/// Exact `ImageInfo` used by upstream's non-`nullDescriptor` image-view
/// fallback.
fn null_image_info() -> ImageInfo {
    ImageInfo {
        format: PixelFormat::A8B8G8R8Unorm,
        ..ImageInfo::default()
    }
}

fn image_usage_flags(
    format_info: maxwell_to_vk::FormatInfo,
    format: PixelFormat,
) -> vk::ImageUsageFlags {
    let mut usage = vk::ImageUsageFlags::TRANSFER_SRC
        | vk::ImageUsageFlags::TRANSFER_DST
        | vk::ImageUsageFlags::SAMPLED;
    if format_info.attachable {
        match crate::surface::get_format_type(format) {
            SurfaceType::ColorTexture => usage |= vk::ImageUsageFlags::COLOR_ATTACHMENT,
            SurfaceType::Depth | SurfaceType::Stencil | SurfaceType::DepthStencil => {
                usage |= vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT;
            }
            SurfaceType::Invalid => {}
        }
    }
    if format_info.storage {
        usage |= vk::ImageUsageFlags::STORAGE;
    }
    usage
}

/// Keeps `VkImageViewUsageCreateInfo::usage` within the usage bits supplied
/// when the underlying image was created. Compatible mutable views can have a
/// broader format-table usage than the base format (notably UNORM views of
/// sRGB images), but Vulkan requires the view usage to remain a subset.
fn image_view_usage_flags(
    view_format_info: maxwell_to_vk::FormatInfo,
    view_format: PixelFormat,
    image_format_info: maxwell_to_vk::FormatInfo,
    image_format: PixelFormat,
) -> vk::ImageUsageFlags {
    image_usage_flags(view_format_info, view_format)
        & image_usage_flags(image_format_info, image_format)
}

fn make_image_create_info(
    info: &ImageInfo,
    format_info: maxwell_to_vk::FormatInfo,
) -> vk::ImageCreateInfo {
    let mut flags = vk::ImageCreateFlags::empty();
    if info.image_type == ImageType::E2D
        && info.resources.layers >= 6
        && info.size.width == info.size.height
    {
        flags |= vk::ImageCreateFlags::CUBE_COMPATIBLE;
    }
    if info.image_type == ImageType::E3D {
        flags |= vk::ImageCreateFlags::TYPE_2D_ARRAY_COMPATIBLE;
    }
    let (samples_x, samples_y) =
        crate::texture_cache::samples_helper::samples_log2(info.num_samples as i32);
    vk::ImageCreateInfo::builder()
        .flags(flags)
        .image_type(convert_image_type(info.image_type))
        .format(format_info.format)
        .extent(vk::Extent3D {
            width: (info.size.width >> samples_x.max(0) as u32).max(1),
            height: (info.size.height >> samples_y.max(0) as u32).max(1),
            depth: info.size.depth.max(1),
        })
        .mip_levels(info.resources.levels.max(1) as u32)
        .array_layers(info.resources.layers.max(1) as u32)
        .samples(convert_sample_count(info.num_samples))
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(image_usage_flags(format_info, info.format))
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .build()
}

fn apply_image_format_list(
    image_info: &mut vk::ImageCreateInfo,
    format_list: &mut vk::ImageFormatListCreateInfo,
    view_formats: &[vk::Format],
    image_format_list_supported: bool,
) {
    if view_formats.len() <= 1 {
        return;
    }
    image_info.flags |= vk::ImageCreateFlags::MUTABLE_FORMAT;
    if image_format_list_supported {
        image_info.p_next = (format_list as *mut vk::ImageFormatListCreateInfo).cast();
    }
}

fn image_aspect_mask(format: PixelFormat) -> vk::ImageAspectFlags {
    match crate::surface::get_format_type(format) {
        SurfaceType::ColorTexture => vk::ImageAspectFlags::COLOR,
        SurfaceType::Depth => vk::ImageAspectFlags::DEPTH,
        SurfaceType::Stencil => vk::ImageAspectFlags::STENCIL,
        SurfaceType::DepthStencil => vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL,
        SurfaceType::Invalid => vk::ImageAspectFlags::empty(),
    }
}

fn image_view_aspect_mask(info: &ImageViewInfo) -> vk::ImageAspectFlags {
    if info.is_render_target() {
        return image_aspect_mask(info.format);
    }
    let any_r = [info.x_source, info.y_source, info.z_source, info.w_source]
        .iter()
        .any(|&source| source == crate::texture_cache::image_view_info::SwizzleSource::R as u8);
    match info.format {
        PixelFormat::D24UnormS8Uint | PixelFormat::D32FloatS8Uint => {
            if any_r {
                vk::ImageAspectFlags::DEPTH
            } else {
                vk::ImageAspectFlags::STENCIL
            }
        }
        PixelFormat::S8UintD24Unorm => {
            if any_r {
                vk::ImageAspectFlags::STENCIL
            } else {
                vk::ImageAspectFlags::DEPTH
            }
        }
        PixelFormat::D16Unorm | PixelFormat::D32Float | PixelFormat::X8D24Unorm => {
            vk::ImageAspectFlags::DEPTH
        }
        PixelFormat::S8Uint => vk::ImageAspectFlags::STENCIL,
        _ => vk::ImageAspectFlags::COLOR,
    }
}

fn convert_green_red(value: u8) -> u8 {
    use crate::texture_cache::image_view_info::SwizzleSource;

    if value == SwizzleSource::G as u8 {
        SwizzleSource::R as u8
    } else {
        value
    }
}

fn swap_blue_red(value: u8) -> u8 {
    use crate::texture_cache::image_view_info::SwizzleSource;

    match value {
        value if value == SwizzleSource::R as u8 => SwizzleSource::B as u8,
        value if value == SwizzleSource::B as u8 => SwizzleSource::R as u8,
        _ => value,
    }
}

fn swap_green_red(value: u8) -> u8 {
    use crate::texture_cache::image_view_info::SwizzleSource;

    match value {
        value if value == SwizzleSource::R as u8 => SwizzleSource::G as u8,
        value if value == SwizzleSource::G as u8 => SwizzleSource::R as u8,
        _ => value,
    }
}

fn swap_special(value: u8) -> u8 {
    use crate::texture_cache::image_view_info::SwizzleSource;

    match value {
        value if value == SwizzleSource::A as u8 => SwizzleSource::R as u8,
        value if value == SwizzleSource::R as u8 => SwizzleSource::A as u8,
        value if value == SwizzleSource::G as u8 => SwizzleSource::B as u8,
        value if value == SwizzleSource::B as u8 => SwizzleSource::G as u8,
        _ => value,
    }
}

/// Port of upstream `TryTransformSwizzleIfNeeded`.
fn try_transform_swizzle_if_needed(
    format: PixelFormat,
    swizzle: &mut [u8; 4],
    emulate_bgr565: bool,
    emulate_a4b4g4r4: bool,
) {
    match format {
        PixelFormat::A1B5G5R5Unorm => swizzle.iter_mut().for_each(|x| *x = swap_blue_red(*x)),
        PixelFormat::B5G6R5Unorm if emulate_bgr565 => {
            swizzle.iter_mut().for_each(|x| *x = swap_blue_red(*x));
        }
        PixelFormat::A5B5G5R1Unorm => swizzle.iter_mut().for_each(|x| *x = swap_special(*x)),
        PixelFormat::G4R4Unorm => swizzle.iter_mut().for_each(|x| *x = swap_green_red(*x)),
        PixelFormat::A4B4G4R4Unorm if emulate_a4b4g4r4 => swizzle.reverse(),
        _ => {}
    }
}

fn sanitize_depth_stencil_swizzle(swizzle: &mut [u8; 4], supports_depth_stencil_swizzle_one: bool) {
    if supports_depth_stencil_swizzle_one {
        return;
    }
    swizzle.iter_mut().for_each(|source| {
        if *source == crate::texture_cache::image_view_info::SwizzleSource::OneFloat as u8
            || *source == crate::texture_cache::image_view_info::SwizzleSource::OneInt as u8
        {
            *source = crate::texture_cache::image_view_info::SwizzleSource::Zero as u8;
        }
    });
}

fn image_view_components(
    info: &ImageViewInfo,
    aspect_mask: vk::ImageAspectFlags,
    emulate_bgr565: bool,
    ext_4444_formats_supported: bool,
    supports_depth_stencil_swizzle_one: bool,
) -> vk::ComponentMapping {
    let mut swizzle = if info.is_render_target() {
        [
            crate::texture_cache::image_view_info::SwizzleSource::R as u8,
            crate::texture_cache::image_view_info::SwizzleSource::G as u8,
            crate::texture_cache::image_view_info::SwizzleSource::B as u8,
            crate::texture_cache::image_view_info::SwizzleSource::A as u8,
        ]
    } else {
        [info.x_source, info.y_source, info.z_source, info.w_source]
    };
    if !info.is_render_target() {
        try_transform_swizzle_if_needed(
            info.format,
            &mut swizzle,
            emulate_bgr565,
            !ext_4444_formats_supported,
        );
        if aspect_mask.intersects(vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL) {
            swizzle
                .iter_mut()
                .for_each(|source| *source = convert_green_red(*source));
            sanitize_depth_stencil_swizzle(&mut swizzle, supports_depth_stencil_swizzle_one);
        }
    }
    vk::ComponentMapping {
        r: component_swizzle(swizzle[0]),
        g: component_swizzle(swizzle[1]),
        b: component_swizzle(swizzle[2]),
        a: component_swizzle(swizzle[3]),
    }
}

fn component_swizzle(source: u8) -> vk::ComponentSwizzle {
    match source {
        0 => vk::ComponentSwizzle::ZERO,
        2 => vk::ComponentSwizzle::R,
        3 => vk::ComponentSwizzle::G,
        4 => vk::ComponentSwizzle::B,
        5 => vk::ComponentSwizzle::A,
        6 | 7 => vk::ComponentSwizzle::ONE,
        _ => vk::ComponentSwizzle::IDENTITY,
    }
}

fn make_subresource_range(
    aspect_mask: vk::ImageAspectFlags,
    range: SubresourceRange,
    flags: ImageViewFlagBits,
) -> vk::ImageSubresourceRange {
    let base_layer = if flags.contains(ImageViewFlagBits::SLICE) {
        0
    } else {
        range.base.layer.max(0) as u32
    };
    let layer_count = if flags.contains(ImageViewFlagBits::SLICE) {
        1
    } else {
        range.extent.layers.max(1) as u32
    };
    vk::ImageSubresourceRange {
        aspect_mask,
        base_mip_level: range.base.level.max(0) as u32,
        level_count: range.extent.levels.max(1) as u32,
        base_array_layer: base_layer,
        layer_count,
    }
}

/// Parameter selection performed by upstream `Vulkan::ImageView::MakeView`.
/// Storage images pass a texture type and therefore override the base view
/// type; non-array storage views expose exactly one layer.
fn aux_image_view_params(
    view: &ImageViewBase,
    aspect_mask: vk::ImageAspectFlags,
    texture_type: Option<TextureType>,
) -> (vk::ImageViewType, vk::ImageSubresourceRange) {
    let mut range = make_subresource_range(aspect_mask, view.range, view.flags);
    let view_type = if let Some(texture_type) = texture_type {
        let view_type = image_view_type_from_texture_type(texture_type);
        if !matches!(
            view_type,
            vk::ImageViewType::TYPE_1D_ARRAY
                | vk::ImageViewType::TYPE_2D_ARRAY
                | vk::ImageViewType::CUBE_ARRAY
        ) {
            range.layer_count = 1;
        }
        view_type
    } else {
        image_view_type_from_view_type(view.view_type)
    };
    (view_type, range)
}

fn image_view_type_from_texture_type(texture_type: TextureType) -> vk::ImageViewType {
    match texture_type {
        TextureType::Color1D => vk::ImageViewType::TYPE_1D,
        TextureType::ColorArray1D => vk::ImageViewType::TYPE_1D_ARRAY,
        TextureType::Color2D | TextureType::Color2DRect => vk::ImageViewType::TYPE_2D,
        TextureType::ColorArray2D => vk::ImageViewType::TYPE_2D_ARRAY,
        TextureType::Color3D => vk::ImageViewType::TYPE_3D,
        TextureType::ColorCube => vk::ImageViewType::CUBE,
        TextureType::ColorArrayCube => vk::ImageViewType::CUBE_ARRAY,
        TextureType::Buffer => vk::ImageViewType::TYPE_1D,
    }
}

fn image_view_type_from_view_type(
    view_type: crate::texture_cache::types::ImageViewType,
) -> vk::ImageViewType {
    match view_type {
        crate::texture_cache::types::ImageViewType::E1D => vk::ImageViewType::TYPE_1D,
        crate::texture_cache::types::ImageViewType::E2D
        | crate::texture_cache::types::ImageViewType::Rect => vk::ImageViewType::TYPE_2D,
        crate::texture_cache::types::ImageViewType::Cube => vk::ImageViewType::CUBE,
        crate::texture_cache::types::ImageViewType::E3D => vk::ImageViewType::TYPE_3D,
        crate::texture_cache::types::ImageViewType::E1DArray => vk::ImageViewType::TYPE_1D_ARRAY,
        crate::texture_cache::types::ImageViewType::E2DArray => vk::ImageViewType::TYPE_2D_ARRAY,
        crate::texture_cache::types::ImageViewType::CubeArray => vk::ImageViewType::CUBE_ARRAY,
        crate::texture_cache::types::ImageViewType::Buffer => vk::ImageViewType::TYPE_1D,
    }
}

fn image_format_to_vk(format: ImageFormat) -> vk::Format {
    match format {
        ImageFormat::Typeless => vk::Format::UNDEFINED,
        ImageFormat::R8Sint => vk::Format::R8_SINT,
        ImageFormat::R8Uint => vk::Format::R8_UINT,
        ImageFormat::R16Uint => vk::Format::R16_UINT,
        ImageFormat::R16Sint => vk::Format::R16_SINT,
        ImageFormat::R32Uint => vk::Format::R32_UINT,
        ImageFormat::R32G32Uint => vk::Format::R32G32_UINT,
        ImageFormat::R32G32B32A32Uint => vk::Format::R32G32B32A32_UINT,
    }
}

impl Drop for TextureCache {
    fn drop(&mut self) {
        let runtime = self.base.runtime_mut() as *mut TextureCacheRuntime;
        for (_, framebuffer_id) in self.base.framebuffers.drain() {
            let fb = self.base.slot_framebuffers.take(framebuffer_id);
            unsafe { &mut *runtime }.destroy_framebuffer_owner(fb);
        }
        for (_, view) in self.base.slot_image_views.iter_mut() {
            if let Some(view) = view.backend.take() {
                unsafe { &mut *runtime }.destroy_image_view(view);
            }
        }
        for (_, image) in self.base.slot_images.iter_mut() {
            if let Some(image) = image.backend.take() {
                unsafe { &mut *runtime }.destroy_image(image);
            }
        }
        for (_, sampler) in self.base.slot_samplers.iter_mut() {
            if let Some(sampler) = sampler.backend.take() {
                unsafe { &mut *runtime }.destroy_sampler(sampler);
            }
        }
    }
}

fn texture_filter_from_raw(raw: u32) -> TextureFilter {
    match raw {
        2 => TextureFilter::Linear,
        _ => TextureFilter::Nearest,
    }
}

fn texture_mipmap_filter_from_raw(raw: u32) -> TextureMipmapFilter {
    match raw {
        2 => TextureMipmapFilter::Nearest,
        3 => TextureMipmapFilter::Linear,
        _ => TextureMipmapFilter::None,
    }
}

fn wrap_mode_from_raw(raw: u32) -> WrapMode {
    match raw {
        0 => WrapMode::Wrap,
        1 => WrapMode::Mirror,
        2 => WrapMode::ClampToEdge,
        3 => WrapMode::Border,
        4 => WrapMode::Clamp,
        5 => WrapMode::MirrorOnceClampToEdge,
        6 => WrapMode::MirrorOnceBorder,
        7 => WrapMode::MirrorOnceClampOgl,
        _ => WrapMode::ClampToEdge,
    }
}
