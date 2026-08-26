// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `renderer_vulkan/present/sgsr.h` and `sgsr.cpp`.

use ash::vk;
use std::ptr::NonNull;

use crate::host_shaders::spirv_shaders::{
    SGSR1_SHADER_MOBILE_EDGE_DIRECTION_FRAG_SPV, SGSR1_SHADER_MOBILE_FRAG_SPV,
    SGSR1_SHADER_VERT_SPV,
};
use crate::renderer_vulkan::scheduler::Scheduler;
use crate::renderer_vulkan::shader_util::build_shader;
use crate::vulkan_common::vulkan_device::Device;
use crate::vulkan_common::vulkan_memory_allocator::{AllocatedImage, MemoryAllocator};

use super::util;

pub const SGSR_STAGE_COUNT: usize = 1;
type PushConstants = [u32; 4 + 2 + 1];

struct Images {
    descriptor_sets: Vec<vk::DescriptorSet>,
    image: AllocatedImage,
    image_view: vk::ImageView,
    framebuffer: vk::Framebuffer,
}

pub struct Sgsr {
    device: ash::Device,
    #[allow(dead_code)]
    memory_allocator: NonNull<MemoryAllocator>,
    image_count: usize,
    extent: vk::Extent2D,
    descriptor_pool: vk::DescriptorPool,
    descriptor_set_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    vert_shader: vk::ShaderModule,
    stage_shader: vk::ShaderModule,
    stage_pipeline: vk::Pipeline,
    renderpass: vk::RenderPass,
    sampler: vk::Sampler,
    dynamic_images: Vec<Images>,
    images_ready: bool,
    #[allow(dead_code)]
    edge_dir: bool,
}

impl Sgsr {
    /// Port of `SGSR::SGSR`.
    pub fn new(
        device: &Device,
        allocator: &MemoryAllocator,
        image_count: usize,
        extent: vk::Extent2D,
        edge_dir: bool,
    ) -> Self {
        let logical = device.get_logical();
        let mut dynamic_images = Vec::with_capacity(image_count);
        for _ in 0..image_count {
            let image =
                util::create_wrapped_image(allocator, extent, vk::Format::R16G16B16A16_SFLOAT);
            let image_view = util::create_wrapped_image_view(
                device,
                image.handle(),
                vk::Format::R16G16B16A16_SFLOAT,
            );
            dynamic_images.push(Images {
                descriptor_sets: Vec::new(),
                image,
                image_view,
                framebuffer: vk::Framebuffer::null(),
            });
        }

        let renderpass = util::create_wrapped_render_pass(
            device,
            vk::Format::R16G16B16A16_SFLOAT,
            vk::ImageLayout::GENERAL,
        );
        for images in &mut dynamic_images {
            images.framebuffer =
                util::create_wrapped_framebuffer(device, renderpass, images.image_view, extent);
        }

        let sampler = util::create_bilinear_sampler(device);
        let vert_shader = build_shader(logical, SGSR1_SHADER_VERT_SPV)
            .expect("Failed to build sgsr1_shader.vert");
        let (stage_code, stage_name) = if edge_dir {
            (
                SGSR1_SHADER_MOBILE_EDGE_DIRECTION_FRAG_SPV,
                "sgsr1_shader_mobile_edge_direction.frag",
            )
        } else {
            (SGSR1_SHADER_MOBILE_FRAG_SPV, "sgsr1_shader_mobile.frag")
        };
        let stage_shader = build_shader(logical, stage_code)
            .unwrap_or_else(|_| panic!("Failed to build {stage_name}"));

        let descriptor_pool = util::create_wrapped_descriptor_pool(
            device,
            image_count,
            image_count,
            &[vk::DescriptorType::COMBINED_IMAGE_SAMPLER],
        );
        let descriptor_set_layout = util::create_wrapped_descriptor_set_layout(
            device,
            &[vk::DescriptorType::COMBINED_IMAGE_SAMPLER],
            vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
        );
        for images in &mut dynamic_images {
            images.descriptor_sets = util::create_wrapped_descriptor_sets(
                logical,
                descriptor_pool,
                &[descriptor_set_layout; SGSR_STAGE_COUNT],
            );
        }

        let push_constant_range = vk::PushConstantRange {
            stage_flags: vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
            offset: 0,
            size: std::mem::size_of::<PushConstants>() as u32,
        };
        let set_layouts = [descriptor_set_layout];
        let pipeline_layout_info = vk::PipelineLayoutCreateInfo::builder()
            .set_layouts(&set_layouts)
            .push_constant_ranges(std::slice::from_ref(&push_constant_range))
            .build();
        let pipeline_layout = unsafe {
            logical
                .create_pipeline_layout(&pipeline_layout_info, None)
                .expect("Failed to create SGSR pipeline layout")
        };
        let stage_pipeline = util::create_wrapped_pipeline(
            device,
            renderpass,
            pipeline_layout,
            vert_shader,
            stage_shader,
        );

        Self {
            device: logical.clone(),
            memory_allocator: NonNull::from(allocator),
            image_count,
            extent,
            descriptor_pool,
            descriptor_set_layout,
            pipeline_layout,
            vert_shader,
            stage_shader,
            stage_pipeline,
            renderpass,
            sampler,
            dynamic_images,
            images_ready: false,
            edge_dir,
        }
    }

    /// Port of `SGSR::UpdateDescriptorSets`.
    fn update_descriptor_sets(
        &self,
        device: &Device,
        image_view: vk::ImageView,
        image_index: usize,
    ) {
        let images = &self.dynamic_images[image_index];
        let image_info = vk::DescriptorImageInfo {
            sampler: self.sampler,
            image_view,
            image_layout: vk::ImageLayout::GENERAL,
        };
        let update = vk::WriteDescriptorSet::builder()
            .dst_set(images.descriptor_sets[0])
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(std::slice::from_ref(&image_info))
            .build();
        unsafe {
            device.get_logical().update_descriptor_sets(&[update], &[]);
        }
    }

    /// Port of `SGSR::UploadImages`.
    fn upload_images(&mut self, device: &Device, scheduler: &mut Scheduler) {
        if self.images_ready {
            return;
        }
        let images: Vec<vk::Image> = self
            .dynamic_images
            .iter()
            .map(|image| image.image.handle())
            .collect();
        let device = device.get_logical().clone();
        scheduler.record(move |cmdbuf| {
            for image in images {
                util::clear_color_image(&device, cmdbuf, image);
            }
        });
        scheduler.finish();
        self.images_ready = true;
    }

    /// Port of `SGSR::Draw`.
    pub fn draw(
        &mut self,
        device: &Device,
        scheduler: &mut Scheduler,
        image_index: usize,
        source_image: vk::Image,
        source_image_view: vk::ImageView,
        input_image_extent: vk::Extent2D,
        crop_rect: [f32; 4],
    ) -> vk::ImageView {
        debug_assert!(image_index < self.image_count);
        let images = &self.dynamic_images[image_index];
        let output_image = images.image.handle();
        let output_view = images.image_view;
        let descriptor_set = images.descriptor_sets[0];
        let framebuffer = images.framebuffer;

        let input_width = input_image_extent.width as f32;
        let input_height = input_image_extent.height as f32;
        let viewport_width = (crop_rect[2] - crop_rect[0]) * input_width;
        let viewport_height = (crop_rect[3] - crop_rect[1]) * input_height;
        let sharpening =
            *common::settings::values().fsr_sharpening_slider.get_value() as f32 / 100.0;
        let push_constants: PushConstants = [
            (1.0 / viewport_width).abs().to_bits(),
            (1.0 / viewport_height).abs().to_bits(),
            viewport_width.abs().to_bits(),
            viewport_height.abs().to_bits(),
            (viewport_width / input_width).to_bits(),
            (viewport_height / input_height).to_bits(),
            sharpening.to_bits(),
        ];

        self.upload_images(device, scheduler);
        self.update_descriptor_sets(device, source_image_view, image_index);

        let device = device.get_logical().clone();
        let renderpass = self.renderpass;
        let pipeline_layout = self.pipeline_layout;
        let stage_pipeline = self.stage_pipeline;
        let extent = self.extent;
        scheduler.request_outside_render_pass_operation_context();
        scheduler.record(move |cmdbuf| unsafe {
            util::transition_image_layout(
                &device,
                cmdbuf,
                source_image,
                vk::ImageLayout::GENERAL,
                vk::ImageLayout::GENERAL,
            );
            util::transition_image_layout(
                &device,
                cmdbuf,
                output_image,
                vk::ImageLayout::GENERAL,
                vk::ImageLayout::GENERAL,
            );
            util::begin_render_pass(&device, cmdbuf, renderpass, framebuffer, extent);
            device.cmd_bind_pipeline(cmdbuf, vk::PipelineBindPoint::GRAPHICS, stage_pipeline);
            device.cmd_bind_descriptor_sets(
                cmdbuf,
                vk::PipelineBindPoint::GRAPHICS,
                pipeline_layout,
                0,
                &[descriptor_set],
                &[],
            );
            let push_bytes = std::slice::from_raw_parts(
                push_constants.as_ptr().cast::<u8>(),
                std::mem::size_of::<PushConstants>(),
            );
            device.cmd_push_constants(
                cmdbuf,
                pipeline_layout,
                vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                0,
                push_bytes,
            );
            device.cmd_draw(cmdbuf, 3, 1, 0, 0);
            device.cmd_end_render_pass(cmdbuf);
            util::transition_image_layout(
                &device,
                cmdbuf,
                output_image,
                vk::ImageLayout::GENERAL,
                vk::ImageLayout::GENERAL,
            );
        });
        output_view
    }
}

impl Drop for Sgsr {
    fn drop(&mut self) {
        unsafe {
            for images in &mut self.dynamic_images {
                if images.framebuffer != vk::Framebuffer::null() {
                    self.device.destroy_framebuffer(images.framebuffer, None);
                    images.framebuffer = vk::Framebuffer::null();
                }
                if images.image_view != vk::ImageView::null() {
                    self.device.destroy_image_view(images.image_view, None);
                    images.image_view = vk::ImageView::null();
                }
                drop(std::mem::replace(&mut images.image, AllocatedImage::null()));
                images.descriptor_sets.clear();
            }
            self.dynamic_images.clear();
            if self.sampler != vk::Sampler::null() {
                self.device.destroy_sampler(self.sampler, None);
            }
            if self.renderpass != vk::RenderPass::null() {
                self.device.destroy_render_pass(self.renderpass, None);
            }
            if self.stage_pipeline != vk::Pipeline::null() {
                self.device.destroy_pipeline(self.stage_pipeline, None);
            }
            if self.stage_shader != vk::ShaderModule::null() {
                self.device.destroy_shader_module(self.stage_shader, None);
            }
            if self.vert_shader != vk::ShaderModule::null() {
                self.device.destroy_shader_module(self.vert_shader, None);
            }
            if self.pipeline_layout != vk::PipelineLayout::null() {
                self.device
                    .destroy_pipeline_layout(self.pipeline_layout, None);
            }
            if self.descriptor_set_layout != vk::DescriptorSetLayout::null() {
                self.device
                    .destroy_descriptor_set_layout(self.descriptor_set_layout, None);
            }
            if self.descriptor_pool != vk::DescriptorPool::null() {
                self.device
                    .destroy_descriptor_pool(self.descriptor_pool, None);
            }
        }
    }
}
