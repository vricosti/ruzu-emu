// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `present/fxaa.h` / `present/fxaa.cpp`.
//!
//! Fast Approximate Anti-Aliasing (FXAA) post-processing pass.

use ash::vk;

use super::anti_alias_pass::AntiAliasPass;
use super::util;
use crate::host_shaders::spirv_shaders::{FXAA_FRAG_SPV, FXAA_VERT_SPV};
use crate::renderer_vulkan::scheduler::Scheduler;
use crate::renderer_vulkan::shader_util::build_shader;
use crate::vulkan_common::vulkan_device::Device;
use crate::vulkan_common::vulkan_memory_allocator::MemoryAllocator;

// ---------------------------------------------------------------------------
// Per-image dynamic resources
// ---------------------------------------------------------------------------

/// Port of `FXAA::Image` inner struct.
struct FxaaImage {
    descriptor_sets: Vec<vk::DescriptorSet>,
    framebuffer: vk::Framebuffer,
    image: vk::Image,
    image_view: vk::ImageView,
}

// ---------------------------------------------------------------------------
// FXAA
// ---------------------------------------------------------------------------

/// Port of `FXAA` class.
///
/// Single-pass FXAA anti-aliasing using a fullscreen fragment shader.
pub struct Fxaa {
    device: ash::Device,
    extent: vk::Extent2D,
    image_count: u32,
    images_ready: bool,
    dynamic_images: Vec<FxaaImage>,

    vertex_shader: vk::ShaderModule,
    fragment_shader: vk::ShaderModule,
    descriptor_pool: vk::DescriptorPool,
    descriptor_set_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    renderpass: vk::RenderPass,
    sampler: vk::Sampler,
}

impl Fxaa {
    /// Port of `FXAA::FXAA`.
    pub fn new(
        device: ash::Device,
        allocator: &MemoryAllocator,
        image_count: usize,
        extent: vk::Extent2D,
    ) -> Self {
        let mut fxaa = Fxaa {
            device,
            extent,
            image_count: image_count as u32,
            images_ready: false,
            dynamic_images: Vec::new(),
            vertex_shader: vk::ShaderModule::null(),
            fragment_shader: vk::ShaderModule::null(),
            descriptor_pool: vk::DescriptorPool::null(),
            descriptor_set_layout: vk::DescriptorSetLayout::null(),
            pipeline_layout: vk::PipelineLayout::null(),
            pipeline: vk::Pipeline::null(),
            renderpass: vk::RenderPass::null(),
            sampler: vk::Sampler::null(),
        };

        fxaa.create_images(allocator);
        fxaa.create_render_passes();
        fxaa.create_sampler();
        fxaa.create_shaders();
        fxaa.create_descriptor_pool();
        fxaa.create_descriptor_set_layouts();
        fxaa.create_descriptor_sets();
        fxaa.create_pipeline_layouts();
        fxaa.create_pipelines();
        fxaa
    }

    /// Port of `FXAA::CreateImages`.
    fn create_images(&mut self, allocator: &MemoryAllocator) {
        self.dynamic_images.reserve_exact(self.image_count as usize);
        for _ in 0..self.image_count {
            let image = util::create_wrapped_image(
                &self.device,
                allocator,
                self.extent,
                vk::Format::R16G16B16A16_SFLOAT,
            );
            let image_view = util::create_wrapped_image_view(
                &self.device,
                image,
                vk::Format::R16G16B16A16_SFLOAT,
            );
            self.dynamic_images.push(FxaaImage {
                descriptor_sets: Vec::new(),
                framebuffer: vk::Framebuffer::null(),
                image,
                image_view,
            });
        }
    }

    /// Port of `FXAA::CreateRenderPasses`.
    fn create_render_passes(&mut self) {
        self.renderpass = util::create_wrapped_render_pass(
            &self.device,
            vk::Format::R16G16B16A16_SFLOAT,
            vk::ImageLayout::UNDEFINED,
        );

        for image in &mut self.dynamic_images {
            image.framebuffer = util::create_wrapped_framebuffer(
                &self.device,
                self.renderpass,
                image.image_view,
                self.extent,
            );
        }
    }

    /// Port of `FXAA::CreateSampler`.
    fn create_sampler(&mut self) {
        self.sampler = util::create_wrapped_sampler(&self.device, vk::Filter::LINEAR);
    }

    /// Port of `FXAA::CreateShaders`.
    fn create_shaders(&mut self) {
        self.vertex_shader =
            build_shader(&self.device, FXAA_VERT_SPV).expect("Failed to build fxaa.vert");
        self.fragment_shader =
            build_shader(&self.device, FXAA_FRAG_SPV).expect("Failed to build fxaa.frag");
    }

    /// Port of `FXAA::CreateDescriptorPool`.
    fn create_descriptor_pool(&mut self) {
        // 2 descriptors, 1 descriptor set per image
        self.descriptor_pool = util::create_wrapped_descriptor_pool(
            &self.device,
            2 * self.image_count,
            self.image_count,
            &[vk::DescriptorType::COMBINED_IMAGE_SAMPLER],
        );
    }

    /// Port of `FXAA::CreateDescriptorSetLayouts`.
    fn create_descriptor_set_layouts(&mut self) {
        self.descriptor_set_layout = util::create_wrapped_descriptor_set_layout(
            &self.device,
            &[
                vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
            ],
        );
    }

    /// Port of `FXAA::CreateDescriptorSets`.
    fn create_descriptor_sets(&mut self) {
        for image in &mut self.dynamic_images {
            image.descriptor_sets = util::create_wrapped_descriptor_sets(
                &self.device,
                self.descriptor_pool,
                &[self.descriptor_set_layout],
            );
        }
    }

    /// Port of `FXAA::CreatePipelineLayouts`.
    fn create_pipeline_layouts(&mut self) {
        self.pipeline_layout =
            util::create_wrapped_pipeline_layout(&self.device, self.descriptor_set_layout);
    }

    /// Port of `FXAA::CreatePipelines`.
    fn create_pipelines(&mut self) {
        self.pipeline = util::create_wrapped_pipeline(
            &self.device,
            self.renderpass,
            self.pipeline_layout,
            self.vertex_shader,
            self.fragment_shader,
        );
    }

    /// Port of `FXAA::UpdateDescriptorSets`.
    fn update_descriptor_sets(
        &self,
        device: &Device,
        image_view: vk::ImageView,
        image_index: usize,
    ) {
        let image = &self.dynamic_images[image_index];
        let mut image_infos = Vec::with_capacity(2);
        let mut updates = Vec::new();

        updates.push(util::create_write_descriptor_set(
            &mut image_infos,
            self.sampler,
            image_view,
            image.descriptor_sets[0],
            0,
        ));
        updates.push(util::create_write_descriptor_set(
            &mut image_infos,
            self.sampler,
            image_view,
            image.descriptor_sets[0],
            1,
        ));

        unsafe {
            device.get_logical().update_descriptor_sets(&updates, &[]);
        }
    }

    /// Port of `FXAA::UploadImages`.
    fn upload_images(&mut self, device: &Device, scheduler: &mut Scheduler) {
        if self.images_ready {
            return;
        }

        let images: Vec<vk::Image> = self.dynamic_images.iter().map(|img| img.image).collect();
        let device = device.get_logical().clone();
        scheduler.record(move |cmdbuf| {
            for image in images {
                util::clear_color_image(&device, cmdbuf, image);
            }
        });
        scheduler.finish();

        self.images_ready = true;
    }
}

impl AntiAliasPass for Fxaa {
    /// Port of `FXAA::Draw`.
    ///
    /// Records commands to apply FXAA to the input image and swaps the
    /// image/view pointers to the output.
    fn draw(
        &mut self,
        device: &Device,
        scheduler: &mut Scheduler,
        image_index: usize,
        inout_image: &mut vk::Image,
        inout_image_view: &mut vk::ImageView,
    ) {
        let input_image = *inout_image;
        let output_image = self.dynamic_images[image_index].image;
        let output_image_view = self.dynamic_images[image_index].image_view;
        let descriptor_set = self.dynamic_images[image_index].descriptor_sets[0];
        let framebuffer = self.dynamic_images[image_index].framebuffer;
        let renderpass = self.renderpass;
        let pipeline = self.pipeline;
        let layout = self.pipeline_layout;
        let extent = self.extent;

        self.upload_images(device, scheduler);
        self.update_descriptor_sets(device, *inout_image_view, image_index);

        scheduler.request_outside_render_pass_operation_context();
        let device = device.get_logical().clone();
        scheduler.record(move |cmdbuf| unsafe {
            util::transition_image_layout(
                &device,
                cmdbuf,
                input_image,
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
            device.cmd_bind_pipeline(cmdbuf, vk::PipelineBindPoint::GRAPHICS, pipeline);
            device.cmd_bind_descriptor_sets(
                cmdbuf,
                vk::PipelineBindPoint::GRAPHICS,
                layout,
                0,
                &[descriptor_set],
                &[],
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

        *inout_image = output_image;
        *inout_image_view = output_image_view;
    }
}

impl Drop for Fxaa {
    fn drop(&mut self) {
        unsafe {
            for image in &mut self.dynamic_images {
                if image.framebuffer != vk::Framebuffer::null() {
                    self.device.destroy_framebuffer(image.framebuffer, None);
                    image.framebuffer = vk::Framebuffer::null();
                }
                if image.image_view != vk::ImageView::null() {
                    self.device.destroy_image_view(image.image_view, None);
                    image.image_view = vk::ImageView::null();
                }
            }
            if self.sampler != vk::Sampler::null() {
                self.device.destroy_sampler(self.sampler, None);
                self.sampler = vk::Sampler::null();
            }
            if self.renderpass != vk::RenderPass::null() {
                self.device.destroy_render_pass(self.renderpass, None);
                self.renderpass = vk::RenderPass::null();
            }
            if self.pipeline != vk::Pipeline::null() {
                self.device.destroy_pipeline(self.pipeline, None);
                self.pipeline = vk::Pipeline::null();
            }
            if self.pipeline_layout != vk::PipelineLayout::null() {
                self.device
                    .destroy_pipeline_layout(self.pipeline_layout, None);
                self.pipeline_layout = vk::PipelineLayout::null();
            }
            if self.descriptor_set_layout != vk::DescriptorSetLayout::null() {
                self.device
                    .destroy_descriptor_set_layout(self.descriptor_set_layout, None);
                self.descriptor_set_layout = vk::DescriptorSetLayout::null();
            }
            if self.descriptor_pool != vk::DescriptorPool::null() {
                self.device
                    .destroy_descriptor_pool(self.descriptor_pool, None);
                self.descriptor_pool = vk::DescriptorPool::null();
            }
            if self.fragment_shader != vk::ShaderModule::null() {
                self.device
                    .destroy_shader_module(self.fragment_shader, None);
                self.fragment_shader = vk::ShaderModule::null();
            }
            if self.vertex_shader != vk::ShaderModule::null() {
                self.device.destroy_shader_module(self.vertex_shader, None);
                self.vertex_shader = vk::ShaderModule::null();
            }
        }
    }
}
