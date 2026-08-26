// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `present/layer.h` / `present/layer.cpp`.
//!
//! A presentation layer that converts a guest framebuffer into a Vulkan
//! image suitable for composition, applying anti-aliasing and FSR as needed.

use ash::vk;
use common::math_util::Rectangle;
use ruzu_core::hle::service::nvnflinger::pixel_format::PixelFormat as AndroidPixelFormat;
use std::ptr::NonNull;
use std::sync::Arc;

use crate::framebuffer_config::{normalize_crop, FramebufferConfig};
use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
use crate::present::{AntiAliasing, PresentFilters, ScalingFilter};
use crate::renderer_vulkan::scheduler::Scheduler;
use crate::renderer_vulkan::RasterizerVulkan;
use crate::textures::decoders;
use crate::vulkan_common::vulkan_device::Device;
use crate::vulkan_common::vulkan_memory_allocator::{
    AllocatedBuffer, AllocatedImage, MemoryAllocator, MemoryUsage,
};

use super::anti_alias_pass::AntiAliasPass;
use super::fsr::Fsr;
use super::fxaa::Fxaa;
use super::present_push_constants::{
    make_orthographic_matrix, PresentPushConstants, ScreenRectVertex,
};
use super::sgsr::Sgsr;
use super::smaa::Smaa;
use super::util;
use ruzu_core::frontend::framebuffer_layout::FramebufferLayout;

// ---------------------------------------------------------------------------
// Anonymous namespace helpers (port of file-static functions)
// ---------------------------------------------------------------------------

/// Port of anonymous `GetBytesPerPixel` helper.
fn get_bytes_per_pixel(framebuffer: &FramebufferConfig) -> u32 {
    crate::surface::bytes_per_block(crate::surface::pixel_format_from_gpu_pixel_format(
        framebuffer.pixel_format,
    ))
}

/// Port of anonymous `GetSizeInBytes` helper.
fn get_size_in_bytes(framebuffer: &FramebufferConfig) -> u64 {
    (framebuffer.stride as u64)
        .wrapping_mul(framebuffer.height as u64)
        .wrapping_mul(get_bytes_per_pixel(framebuffer) as u64)
}

/// Port of anonymous `GetFormat` helper.
fn get_vk_format(framebuffer: &FramebufferConfig) -> vk::Format {
    match framebuffer.pixel_format {
        AndroidPixelFormat::Rgba8888 | AndroidPixelFormat::Rgbx8888 => {
            vk::Format::A8B8G8R8_UNORM_PACK32
        }
        AndroidPixelFormat::Rgb565 => vk::Format::R5G6B5_UNORM_PACK16,
        AndroidPixelFormat::Bgra8888 => vk::Format::B8G8R8A8_UNORM,
        _ => {
            let message = format!(
                "Unknown framebuffer pixel format: {}",
                framebuffer.pixel_format as u32
            );
            log::error!("{message}");
            if *common::settings::values().use_debug_asserts.get_value() {
                panic!("{message}");
            }
            vk::Format::A8B8G8R8_UNORM_PACK32
        }
    }
}

/// Rust counterpart of upstream's `std::variant<std::monostate, SGSR, FSR>`.
enum SuperResolutionFilter {
    None,
    Sgsr(Sgsr),
    Fsr(Fsr),
}

/// Rust counterpart of upstream's `std::variant<std::monostate, FXAA, SMAA>`.
enum AntiAlias {
    None,
    Fxaa(Fxaa),
    Smaa(Smaa),
}

/// Rust counterpart of the `SCOPE_EXIT` updating `resource_ticks` in
/// `Layer::ConfigureDraw`.
struct ResourceTickGuard {
    scheduler: NonNull<Scheduler>,
    resource_tick: NonNull<u64>,
}

impl Drop for ResourceTickGuard {
    fn drop(&mut self) {
        let tick = unsafe { self.scheduler.as_ref() }.current_tick();
        unsafe {
            *self.resource_tick.as_mut() = tick;
        }
    }
}

/// Port of `Layer` class.
///
/// Owns raw images for framebuffer upload, anti-aliasing state, FSR state,
/// descriptor sets, and a staging buffer. Configures per-draw push constants
/// and descriptor sets for the window adapt pass.
pub struct Layer {
    device: ash::Device,
    memory_allocator: NonNull<MemoryAllocator>,
    scheduler: NonNull<Scheduler>,
    device_memory: Arc<MaxwellDeviceMemoryManager>,
    filters: &'static PresentFilters,
    image_count: usize,

    descriptor_pool: vk::DescriptorPool,
    descriptor_sets: Vec<vk::DescriptorSet>,

    buffer: Option<AllocatedBuffer>,
    raw_images: Vec<AllocatedImage>,
    raw_image_views: Vec<vk::ImageView>,
    raw_width: u32,
    raw_height: u32,
    pixel_format: AndroidPixelFormat,

    anti_alias_setting: AntiAliasing,
    anti_alias: AntiAlias,

    sr_filter: SuperResolutionFilter,
    resource_ticks: Vec<u64>,
}

impl Layer {
    /// Port of `Layer::Layer`.
    pub fn new(
        device: &Device,
        allocator: &MemoryAllocator,
        scheduler: &mut Scheduler,
        device_memory: &Arc<MaxwellDeviceMemoryManager>,
        image_count: usize,
        output_size: vk::Extent2D,
        layout: vk::DescriptorSetLayout,
        filters: &'static PresentFilters,
    ) -> Self {
        let mut layer = Layer {
            device: device.get_logical().clone(),
            memory_allocator: NonNull::from(allocator),
            scheduler: NonNull::from(&mut *scheduler),
            device_memory: Arc::clone(device_memory),
            filters,
            image_count,
            descriptor_pool: vk::DescriptorPool::null(),
            descriptor_sets: Vec::new(),
            buffer: None,
            raw_images: Vec::new(),
            raw_image_views: Vec::new(),
            raw_width: 0,
            raw_height: 0,
            pixel_format: AndroidPixelFormat::NoFormat,
            anti_alias_setting: AntiAliasing::None,
            anti_alias: AntiAlias::None,
            sr_filter: SuperResolutionFilter::None,
            resource_ticks: Vec::new(),
        };

        layer.create_descriptor_pool(device);
        layer.create_descriptor_sets(device, layout);
        layer.sr_filter = match (filters.get_scaling_filter)() {
            ScalingFilter::Fsr => {
                SuperResolutionFilter::Fsr(Fsr::new(device, allocator, image_count, output_size))
            }
            ScalingFilter::Sgsr => SuperResolutionFilter::Sgsr(Sgsr::new(
                device,
                allocator,
                image_count,
                output_size,
                false,
            )),
            ScalingFilter::SgsrEdge => SuperResolutionFilter::Sgsr(Sgsr::new(
                device,
                allocator,
                image_count,
                output_size,
                true,
            )),
            _ => SuperResolutionFilter::None,
        };
        layer
    }

    /// Port of `Layer::CreateDescriptorPool`.
    fn create_descriptor_pool(&mut self, device: &Device) {
        self.descriptor_pool = util::create_wrapped_descriptor_pool(
            device.get_logical(),
            self.image_count as u32,
            self.image_count as u32,
            &[vk::DescriptorType::COMBINED_IMAGE_SAMPLER],
        );
    }

    /// Port of `Layer::CreateDescriptorSets`.
    fn create_descriptor_sets(&mut self, device: &Device, layout: vk::DescriptorSetLayout) {
        let layouts = vec![layout; self.image_count];
        self.descriptor_sets = util::create_wrapped_descriptor_sets(
            device.get_logical(),
            self.descriptor_pool,
            &layouts,
        );
    }

    /// Port of `Layer::ConfigureDraw`.
    pub fn configure_draw(
        &mut self,
        device: &Device,
        out_push_constants: &mut PresentPushConstants,
        out_descriptor_set: &mut vk::DescriptorSet,
        rasterizer: &mut RasterizerVulkan,
        sampler: vk::Sampler,
        image_index: usize,
        framebuffer: &FramebufferConfig,
        layout: &FramebufferLayout,
    ) {
        let framebuffer_addr = framebuffer.address.wrapping_add(framebuffer.offset as u64);
        let texture_info =
            rasterizer.accelerate_display(framebuffer, framebuffer_addr, framebuffer.stride);
        let texture_width = texture_info
            .as_ref()
            .map_or(framebuffer.width, |info| info.width);
        let texture_height = texture_info
            .as_ref()
            .map_or(framebuffer.height, |info| info.height);
        let scaled_width = texture_info
            .as_ref()
            .map_or(texture_width, |info| info.scaled_width);
        let scaled_height = texture_info
            .as_ref()
            .map_or(texture_height, |info| info.scaled_height);
        let use_accelerated = texture_info.is_some();

        self.refresh_resources(device, framebuffer);
        self.set_anti_alias_pass(device);

        {
            let scheduler = unsafe { self.scheduler.as_mut() };
            scheduler.request_outside_render_pass_operation_context();
            scheduler.wait(self.resource_ticks[image_index]);
        }
        let _resource_tick_guard = ResourceTickGuard {
            scheduler: self.scheduler,
            resource_tick: NonNull::from(&mut self.resource_ticks[image_index]),
        };
        if !use_accelerated {
            self.update_raw_image(framebuffer, image_index);
        }

        let (mut source_image, mut source_image_view) = texture_info.as_ref().map_or_else(
            || {
                (
                    self.raw_images[image_index].handle(),
                    self.raw_image_views[image_index],
                )
            },
            |info| (info.image, info.image_view),
        );

        let scheduler = unsafe { self.scheduler.as_mut() };

        match &mut self.anti_alias {
            AntiAlias::Fxaa(fxaa) => fxaa.draw(
                device,
                scheduler,
                image_index,
                &mut source_image,
                &mut source_image_view,
            ),
            AntiAlias::Smaa(smaa) => smaa.draw(
                device,
                scheduler,
                image_index,
                &mut source_image,
                &mut source_image_view,
            ),
            AntiAlias::None => {}
        }

        let mut crop_rect = normalize_crop(framebuffer, texture_width, texture_height);
        let render_extent = vk::Extent2D {
            width: scaled_width,
            height: scaled_height,
        };
        let crop = [
            crop_rect.left,
            crop_rect.top,
            crop_rect.right,
            crop_rect.bottom,
        ];
        let filtered_view = match &mut self.sr_filter {
            SuperResolutionFilter::None => None,
            SuperResolutionFilter::Fsr(fsr) => Some(fsr.draw(
                device,
                scheduler,
                image_index,
                source_image,
                source_image_view,
                render_extent,
                crop,
            )),
            SuperResolutionFilter::Sgsr(sgsr) => Some(sgsr.draw(
                device,
                scheduler,
                image_index,
                source_image,
                source_image_view,
                render_extent,
                crop,
            )),
        };
        if let Some(filtered_view) = filtered_view {
            source_image_view = filtered_view;
            crop_rect = Rectangle::new(0.0, 0.0, 1.0, 1.0);
        }

        self.set_matrix_data(device, out_push_constants, layout);
        self.set_vertex_data(device, out_push_constants, layout, &crop_rect);

        self.update_descriptor_set(device, source_image_view, sampler, image_index);
        *out_descriptor_set = self.descriptor_sets[image_index];
    }

    fn update_raw_image(&mut self, framebuffer: &FramebufferConfig, image_index: usize) {
        let image_offset = self.get_raw_image_offset(framebuffer, image_index);
        let linear_size = get_size_in_bytes(framebuffer);
        let buffer = self
            .buffer
            .as_mut()
            .expect("Layer staging buffer must exist after RefreshResources");
        let end = image_offset.wrapping_add(linear_size) as usize;
        let mapped = buffer.mapped_slice_mut();

        let bytes_per_pixel = get_bytes_per_pixel(framebuffer);
        const BLOCK_HEIGHT_LOG2: u32 = 4;
        let tiled_size = decoders::calculate_size(
            true,
            bytes_per_pixel,
            framebuffer.stride,
            framebuffer.height,
            1,
            BLOCK_HEIGHT_LOG2,
            0,
        );
        let framebuffer_addr = framebuffer.address.wrapping_add(framebuffer.offset as u64);
        let host_ptr = self.device_memory.get_pointer(framebuffer_addr);
        if !host_ptr.is_null() {
            let input = unsafe { std::slice::from_raw_parts(host_ptr, tiled_size) };
            decoders::unswizzle_texture(
                &mut mapped[image_offset as usize..end],
                input,
                bytes_per_pixel,
                framebuffer.width,
                framebuffer.height,
                1,
                BLOCK_HEIGHT_LOG2,
                0,
                0,
            );
            buffer.flush();
        }

        let image = self.raw_images[image_index].handle();
        let staging_buffer = buffer.handle();
        let image_width = framebuffer.width;
        let image_height = framebuffer.height;
        let device = self.device.clone();
        let scheduler = unsafe { self.scheduler.as_mut() };
        scheduler.record(move |cmdbuf| unsafe {
            let upload_barrier = vk::ImageMemoryBarrier::builder()
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(image)
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
                vk::PipelineStageFlags::HOST,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[upload_barrier],
            );

            let copy = vk::BufferImageCopy::builder()
                .buffer_offset(image_offset)
                .buffer_row_length(0)
                .buffer_image_height(0)
                .image_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .image_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
                .image_extent(vk::Extent3D {
                    width: image_width,
                    height: image_height,
                    depth: 1,
                })
                .build();
            device.cmd_copy_buffer_to_image(
                cmdbuf,
                staging_buffer,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[copy],
            );

            let shader_barrier = vk::ImageMemoryBarrier::builder()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::SHADER_READ)
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(image)
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
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::FRAGMENT_SHADER | vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[shader_barrier],
            );
        });
    }

    /// Port of `Layer::SetMatrixData`.
    fn set_matrix_data(
        &self,
        _device: &Device,
        data: &mut PresentPushConstants,
        layout: &FramebufferLayout,
    ) {
        data.modelview_matrix = make_orthographic_matrix(layout.width as f32, layout.height as f32);
    }

    /// Port of `Layer::SetVertexData`.
    fn set_vertex_data(
        &self,
        _device: &Device,
        data: &mut PresentPushConstants,
        layout: &FramebufferLayout,
        crop: &Rectangle<f32>,
    ) {
        let x = layout.screen.left as f32;
        let y = layout.screen.top as f32;
        let w = layout.screen.get_width() as f32;
        let h = layout.screen.get_height() as f32;

        data.vertices[0] = ScreenRectVertex::new(x, y, crop.left, crop.top);
        data.vertices[1] = ScreenRectVertex::new(x + w, y, crop.right, crop.top);
        data.vertices[2] = ScreenRectVertex::new(x, y + h, crop.left, crop.bottom);
        data.vertices[3] = ScreenRectVertex::new(x + w, y + h, crop.right, crop.bottom);
    }

    /// Port of `Layer::UpdateDescriptorSet`.
    fn update_descriptor_set(
        &self,
        device: &Device,
        image_view: vk::ImageView,
        sampler: vk::Sampler,
        image_index: usize,
    ) {
        let image_info = vk::DescriptorImageInfo {
            sampler,
            image_view,
            image_layout: vk::ImageLayout::GENERAL,
        };

        let sampler_write = vk::WriteDescriptorSet {
            s_type: vk::StructureType::WRITE_DESCRIPTOR_SET,
            p_next: std::ptr::null(),
            dst_set: self.descriptor_sets[image_index],
            dst_binding: 0,
            dst_array_element: 0,
            descriptor_count: 1,
            descriptor_type: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
            p_image_info: &image_info,
            p_buffer_info: std::ptr::null(),
            p_texel_buffer_view: std::ptr::null(),
        };

        unsafe {
            device
                .get_logical()
                .update_descriptor_sets(&[sampler_write], &[]);
        }
    }

    /// Port of `Layer::CalculateBufferSize`.
    fn calculate_buffer_size(&self, framebuffer: &FramebufferConfig) -> u64 {
        get_size_in_bytes(framebuffer).wrapping_mul(self.image_count as u64)
    }

    /// Port of `Layer::GetRawImageOffset`.
    fn get_raw_image_offset(&self, framebuffer: &FramebufferConfig, image_index: usize) -> u64 {
        get_size_in_bytes(framebuffer).wrapping_mul(image_index as u64)
    }

    /// Port of `Layer::ReleaseRawImages`.
    fn release_raw_images(&mut self) {
        let scheduler = unsafe { self.scheduler.as_mut() };
        for tick in self.resource_ticks.iter().copied() {
            scheduler.wait(tick);
        }
        self.raw_images.clear();
        self.buffer = None;
    }

    /// Port of `Layer::RefreshResources`.
    ///
    /// Recreates raw images and staging buffer if the framebuffer dimensions
    /// or pixel format have changed.
    pub fn refresh_resources(&mut self, device: &Device, framebuffer: &FramebufferConfig) {
        if framebuffer.width == self.raw_width
            && framebuffer.height == self.raw_height
            && framebuffer.pixel_format == self.pixel_format
            && !self.raw_images.is_empty()
        {
            return;
        }

        self.raw_width = framebuffer.width;
        self.raw_height = framebuffer.height;
        self.pixel_format = framebuffer.pixel_format;
        self.anti_alias = AntiAlias::None;

        self.release_raw_images();
        self.create_staging_buffer(device, framebuffer);
        self.create_raw_images(device, framebuffer);
    }

    /// Port of `Layer::CreateStagingBuffer`.
    fn create_staging_buffer(&mut self, _device: &Device, framebuffer: &FramebufferConfig) {
        let size = self.calculate_buffer_size(framebuffer);
        let ci = vk::BufferCreateInfo::builder()
            .size(size)
            .usage(
                vk::BufferUsageFlags::TRANSFER_SRC
                    | vk::BufferUsageFlags::TRANSFER_DST
                    | vk::BufferUsageFlags::VERTEX_BUFFER
                    | vk::BufferUsageFlags::UNIFORM_BUFFER,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .build();
        let allocator = unsafe { self.memory_allocator.as_ref() };
        self.buffer = Some(
            allocator
                .create_owned_buffer(&ci, MemoryUsage::Upload)
                .expect("Failed to create Vulkan layer staging buffer"),
        );
    }

    /// Port of `Layer::CreateRawImages`.
    fn create_raw_images(&mut self, device: &Device, framebuffer: &FramebufferConfig) {
        let format = get_vk_format(framebuffer);
        let extent = vk::Extent2D {
            width: framebuffer.width,
            height: framebuffer.height,
        };
        self.resource_ticks.resize(self.image_count, 0);
        self.raw_image_views
            .resize(self.image_count, vk::ImageView::null());

        let allocator = unsafe { self.memory_allocator.as_ref() };
        self.raw_images.reserve_exact(self.image_count);
        for image_index in 0..self.image_count {
            let image = util::create_wrapped_image(allocator, extent, format);
            if self.raw_image_views[image_index] != vk::ImageView::null() {
                unsafe {
                    device
                        .get_logical()
                        .destroy_image_view(self.raw_image_views[image_index], None);
                }
            }
            self.raw_image_views[image_index] =
                util::create_wrapped_image_view(device.get_logical(), image.handle(), format);
            self.raw_images.push(image);
        }
    }

    /// Port of `Layer::SetAntiAliasPass`.
    fn set_anti_alias_pass(&mut self, device: &Device) {
        let requested = (self.filters.get_anti_aliasing)();
        if !matches!(self.anti_alias, AntiAlias::None) && self.anti_alias_setting == requested {
            return;
        }

        let allocator = unsafe { self.memory_allocator.as_ref() };
        self.anti_alias_setting = requested;
        let resolution = common::settings::values().resolution_info.clone();
        let extent = vk::Extent2D {
            width: resolution.scale_up_u32(self.raw_width),
            height: resolution.scale_up_u32(self.raw_height),
        };
        self.anti_alias = match requested {
            AntiAliasing::Fxaa => {
                AntiAlias::Fxaa(Fxaa::new(device, allocator, self.image_count, extent))
            }
            AntiAliasing::Smaa => {
                AntiAlias::Smaa(Smaa::new(device, allocator, self.image_count, extent))
            }
            AntiAliasing::None => AntiAlias::None,
        };
    }
}

impl Drop for Layer {
    /// Port of `Layer::~Layer` plus explicit destruction for raw Vulkan handles.
    fn drop(&mut self) {
        self.release_raw_images();
        unsafe {
            for image_view in self.raw_image_views.drain(..) {
                if image_view != vk::ImageView::null() {
                    self.device.destroy_image_view(image_view, None);
                }
            }
        }
        if self.descriptor_pool != vk::DescriptorPool::null() {
            unsafe {
                self.device
                    .destroy_descriptor_pool(self.descriptor_pool, None);
            }
            self.descriptor_pool = vk::DescriptorPool::null();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn framebuffer(pixel_format: AndroidPixelFormat) -> FramebufferConfig {
        FramebufferConfig {
            pixel_format,
            ..Default::default()
        }
    }

    #[test]
    fn framebuffer_formats_match_eden() {
        assert_eq!(
            get_vk_format(&framebuffer(AndroidPixelFormat::Rgba8888)),
            vk::Format::A8B8G8R8_UNORM_PACK32
        );
        assert_eq!(
            get_vk_format(&framebuffer(AndroidPixelFormat::Rgbx8888)),
            vk::Format::A8B8G8R8_UNORM_PACK32
        );
        assert_eq!(
            get_vk_format(&framebuffer(AndroidPixelFormat::Rgb565)),
            vk::Format::R5G6B5_UNORM_PACK16
        );
        assert_eq!(
            get_vk_format(&framebuffer(AndroidPixelFormat::Bgra8888)),
            vk::Format::B8G8R8A8_UNORM
        );
    }

    #[test]
    fn framebuffer_byte_size_preserves_unsigned_wrapping() {
        let framebuffer = FramebufferConfig {
            stride: u32::MAX,
            height: u32::MAX,
            pixel_format: AndroidPixelFormat::Rgba8888,
            ..Default::default()
        };
        let expected = (u32::MAX as u64)
            .wrapping_mul(u32::MAX as u64)
            .wrapping_mul(4);
        assert_eq!(get_size_in_bytes(&framebuffer), expected);
    }
}
