// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `present/fsr.h` / `present/fsr.cpp`.
//!
//! AMD FidelityFX Super Resolution (FSR) 1.0 upscaling pass.

use ash::vk;

use super::util;
use crate::fsr::{fsr_easu_con_offset, fsr_rcas_con};
use crate::host_shaders::spirv_shaders::{
    VULKAN_FIDELITYFX_FSR_EASU_FP16_FRAG_SPV, VULKAN_FIDELITYFX_FSR_EASU_FP32_FRAG_SPV,
    VULKAN_FIDELITYFX_FSR_RCAS_FP16_FRAG_SPV, VULKAN_FIDELITYFX_FSR_RCAS_FP32_FRAG_SPV,
    VULKAN_FIDELITYFX_FSR_VERT_SPV,
};
use crate::renderer_vulkan::scheduler::Scheduler;
use crate::renderer_vulkan::shader_util::build_shader;
use crate::vulkan_common::vulkan_memory_allocator::MemoryAllocator;

// ---------------------------------------------------------------------------
// FSR stage enum
// ---------------------------------------------------------------------------

/// Port of `FSR::FsrStage` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum FsrStage {
    Easu = 0,
    Rcas = 1,
}

/// Total number of FSR stages.
pub const MAX_FSR_STAGE: usize = 2;

/// Port of FSR push constants (4x4 u32 array).
type PushConstants = [u32; 4 * 4];

// ---------------------------------------------------------------------------
// Per-image dynamic resources
// ---------------------------------------------------------------------------

/// Port of `FSR::Images` inner struct.
pub struct FsrImages {
    pub descriptor_sets: Vec<vk::DescriptorSet>,
    pub images: [vk::Image; MAX_FSR_STAGE],
    pub image_views: [vk::ImageView; MAX_FSR_STAGE],
    pub framebuffers: [vk::Framebuffer; MAX_FSR_STAGE],
}

// ---------------------------------------------------------------------------
// FSR
// ---------------------------------------------------------------------------

/// Port of `FSR` class.
///
/// Two-pass (EASU + RCAS) FidelityFX Super Resolution upscaling.
pub struct Fsr {
    device: ash::Device,
    image_count: usize,
    extent: vk::Extent2D,
    images_ready: bool,
    dynamic_images: Vec<FsrImages>,

    descriptor_pool: vk::DescriptorPool,
    descriptor_set_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    vert_shader: vk::ShaderModule,
    easu_shader: vk::ShaderModule,
    rcas_shader: vk::ShaderModule,
    easu_pipeline: vk::Pipeline,
    rcas_pipeline: vk::Pipeline,
    renderpass: vk::RenderPass,
    sampler: vk::Sampler,
}

impl Fsr {
    /// Port of `FSR::FSR`.
    pub fn new(
        device: ash::Device,
        allocator: &MemoryAllocator,
        image_count: usize,
        extent: vk::Extent2D,
        supports_float16: bool,
    ) -> Self {
        let mut fsr = Fsr {
            device,
            image_count,
            extent,
            images_ready: false,
            dynamic_images: Vec::new(),
            descriptor_pool: vk::DescriptorPool::null(),
            descriptor_set_layout: vk::DescriptorSetLayout::null(),
            pipeline_layout: vk::PipelineLayout::null(),
            vert_shader: vk::ShaderModule::null(),
            easu_shader: vk::ShaderModule::null(),
            rcas_shader: vk::ShaderModule::null(),
            easu_pipeline: vk::Pipeline::null(),
            rcas_pipeline: vk::Pipeline::null(),
            renderpass: vk::RenderPass::null(),
            sampler: vk::Sampler::null(),
        };

        fsr.create_images(allocator);
        fsr.create_render_passes();
        fsr.create_sampler();
        fsr.create_shaders(supports_float16);
        fsr.create_descriptor_pool();
        fsr.create_descriptor_set_layout();
        fsr.create_descriptor_sets();
        fsr.create_pipeline_layouts();
        fsr.create_pipelines();
        fsr
    }

    /// Port of `FSR::CreateImages`.
    fn create_images(&mut self, allocator: &MemoryAllocator) {
        self.dynamic_images.reserve_exact(self.image_count);
        for _ in 0..self.image_count {
            let easu_image = util::create_wrapped_image(
                &self.device,
                allocator,
                self.extent,
                vk::Format::R16G16B16A16_SFLOAT,
            );
            let rcas_image = util::create_wrapped_image(
                &self.device,
                allocator,
                self.extent,
                vk::Format::R16G16B16A16_SFLOAT,
            );
            let easu_view = util::create_wrapped_image_view(
                &self.device,
                easu_image,
                vk::Format::R16G16B16A16_SFLOAT,
            );
            let rcas_view = util::create_wrapped_image_view(
                &self.device,
                rcas_image,
                vk::Format::R16G16B16A16_SFLOAT,
            );
            self.dynamic_images.push(FsrImages {
                descriptor_sets: Vec::new(),
                images: [easu_image, rcas_image],
                image_views: [easu_view, rcas_view],
                framebuffers: [vk::Framebuffer::null(); MAX_FSR_STAGE],
            });
        }
    }

    /// Port of `FSR::CreateRenderPasses`.
    fn create_render_passes(&mut self) {
        self.renderpass = util::create_wrapped_render_pass(
            &self.device,
            vk::Format::R16G16B16A16_SFLOAT,
            vk::ImageLayout::UNDEFINED,
        );

        for images in &mut self.dynamic_images {
            images.framebuffers[FsrStage::Easu as usize] = util::create_wrapped_framebuffer(
                &self.device,
                self.renderpass,
                images.image_views[FsrStage::Easu as usize],
                self.extent,
            );
            images.framebuffers[FsrStage::Rcas as usize] = util::create_wrapped_framebuffer(
                &self.device,
                self.renderpass,
                images.image_views[FsrStage::Rcas as usize],
                self.extent,
            );
        }
    }

    /// Port of `FSR::CreateSampler`.
    fn create_sampler(&mut self) {
        self.sampler = util::create_bilinear_sampler(&self.device);
    }

    /// Port of `FSR::CreateShaders`.
    fn create_shaders(&mut self, supports_float16: bool) {
        self.vert_shader = build_shader(&self.device, VULKAN_FIDELITYFX_FSR_VERT_SPV)
            .expect("Failed to build vulkan_fidelityfx_fsr.vert");
        let (easu_spv, rcas_spv, easu_name, rcas_name) = if supports_float16 {
            (
                VULKAN_FIDELITYFX_FSR_EASU_FP16_FRAG_SPV,
                VULKAN_FIDELITYFX_FSR_RCAS_FP16_FRAG_SPV,
                "vulkan_fidelityfx_fsr_easu_fp16.frag",
                "vulkan_fidelityfx_fsr_rcas_fp16.frag",
            )
        } else {
            (
                VULKAN_FIDELITYFX_FSR_EASU_FP32_FRAG_SPV,
                VULKAN_FIDELITYFX_FSR_RCAS_FP32_FRAG_SPV,
                "vulkan_fidelityfx_fsr_easu_fp32.frag",
                "vulkan_fidelityfx_fsr_rcas_fp32.frag",
            )
        };
        self.easu_shader = build_shader(&self.device, easu_spv)
            .unwrap_or_else(|_| panic!("Failed to build {easu_name}"));
        self.rcas_shader = build_shader(&self.device, rcas_spv)
            .unwrap_or_else(|_| panic!("Failed to build {rcas_name}"));
    }

    /// Port of `FSR::CreateDescriptorPool`.
    fn create_descriptor_pool(&mut self) {
        // EASU: 1 descriptor
        // RCAS: 1 descriptor
        // 2 descriptors, 2 descriptor sets per invocation
        self.descriptor_pool = util::create_wrapped_descriptor_pool(
            &self.device,
            2 * self.image_count as u32,
            2 * self.image_count as u32,
            &[vk::DescriptorType::COMBINED_IMAGE_SAMPLER],
        );
    }

    /// Port of `FSR::CreateDescriptorSetLayout`.
    fn create_descriptor_set_layout(&mut self) {
        self.descriptor_set_layout = util::create_wrapped_descriptor_set_layout(
            &self.device,
            &[vk::DescriptorType::COMBINED_IMAGE_SAMPLER],
        );
    }

    /// Port of `FSR::CreateDescriptorSets`.
    fn create_descriptor_sets(&mut self) {
        let layouts = vec![self.descriptor_set_layout; MAX_FSR_STAGE];
        for images in &mut self.dynamic_images {
            images.descriptor_sets =
                util::create_wrapped_descriptor_sets(&self.device, self.descriptor_pool, &layouts);
        }
    }

    /// Port of `FSR::CreatePipelineLayouts`.
    fn create_pipeline_layouts(&mut self) {
        let push_constant_range = vk::PushConstantRange {
            stage_flags: vk::ShaderStageFlags::FRAGMENT,
            offset: 0,
            size: std::mem::size_of::<PushConstants>() as u32,
        };
        let set_layouts = [self.descriptor_set_layout];
        let pipeline_layout_ci = vk::PipelineLayoutCreateInfo::builder()
            .set_layouts(&set_layouts)
            .push_constant_ranges(std::slice::from_ref(&push_constant_range))
            .build();
        self.pipeline_layout = unsafe {
            self.device
                .create_pipeline_layout(&pipeline_layout_ci, None)
                .expect("Failed to create FSR pipeline layout")
        };
    }

    /// Port of `FSR::CreatePipelines`.
    fn create_pipelines(&mut self) {
        self.easu_pipeline = util::create_wrapped_pipeline(
            &self.device,
            self.renderpass,
            self.pipeline_layout,
            self.vert_shader,
            self.easu_shader,
        );
        self.rcas_pipeline = util::create_wrapped_pipeline(
            &self.device,
            self.renderpass,
            self.pipeline_layout,
            self.vert_shader,
            self.rcas_shader,
        );
    }

    /// Port of `FSR::UpdateDescriptorSets`.
    fn update_descriptor_sets(&self, image_view: vk::ImageView, image_index: usize) {
        let images = &self.dynamic_images[image_index];
        let mut image_infos = Vec::with_capacity(2);
        let mut updates = Vec::new();

        // EASU reads from the source image
        updates.push(util::create_write_descriptor_set(
            &mut image_infos,
            self.sampler,
            image_view,
            images.descriptor_sets[FsrStage::Easu as usize],
            0,
        ));
        // RCAS reads from EASU output
        updates.push(util::create_write_descriptor_set(
            &mut image_infos,
            self.sampler,
            images.image_views[FsrStage::Easu as usize],
            images.descriptor_sets[FsrStage::Rcas as usize],
            0,
        ));

        unsafe {
            self.device.update_descriptor_sets(&updates, &[]);
        }
    }

    /// Port of `FSR::UploadImages`.
    fn upload_images(&mut self, scheduler: &mut Scheduler) {
        if self.images_ready {
            return;
        }
        self.images_ready = true;

        let images: Vec<[vk::Image; MAX_FSR_STAGE]> =
            self.dynamic_images.iter().map(|imgs| imgs.images).collect();
        let device = self.device.clone();
        scheduler.record(move |cmdbuf| {
            for image in images {
                util::clear_color_image(&device, cmdbuf, image[FsrStage::Easu as usize]);
                util::clear_color_image(&device, cmdbuf, image[FsrStage::Rcas as usize]);
            }
        });
        scheduler.finish();
    }

    /// Port of `FSR::Draw`.
    ///
    /// Applies EASU upscaling followed by RCAS sharpening, returning the
    /// output image view.
    pub fn draw(
        &mut self,
        scheduler: &mut Scheduler,
        image_index: usize,
        source_image: vk::Image,
        source_image_view: vk::ImageView,
        input_image_extent: vk::Extent2D,
        crop_rect: [f32; 4], // [left, top, right, bottom]
    ) -> vk::ImageView {
        let images = &self.dynamic_images[image_index];

        let easu_image = images.images[FsrStage::Easu as usize];
        let rcas_image = images.images[FsrStage::Rcas as usize];
        let easu_descriptor_set = images.descriptor_sets[FsrStage::Easu as usize];
        let rcas_descriptor_set = images.descriptor_sets[FsrStage::Rcas as usize];
        let easu_framebuffer = images.framebuffers[FsrStage::Easu as usize];
        let rcas_framebuffer = images.framebuffers[FsrStage::Rcas as usize];
        let easu_pipeline = self.easu_pipeline;
        let rcas_pipeline = self.rcas_pipeline;
        let pipeline_layout = self.pipeline_layout;
        let renderpass = self.renderpass;
        let extent = self.extent;
        let output_view = images.image_views[FsrStage::Rcas as usize];

        // Compute push constants
        let input_image_width = input_image_extent.width as f32;
        let input_image_height = input_image_extent.height as f32;
        let output_image_width = self.extent.width as f32;
        let output_image_height = self.extent.height as f32;
        let viewport_width = (crop_rect[2] - crop_rect[0]) * input_image_width;
        let viewport_x = crop_rect[0] * input_image_width;
        let viewport_height = (crop_rect[3] - crop_rect[1]) * input_image_height;
        let viewport_y = crop_rect[1] * input_image_height;

        let mut easu_con: PushConstants = [0u32; 16];
        let mut rcas_con: PushConstants = [0u32; 16];
        let (con0, rest) = easu_con.split_at_mut(4);
        let (con1, rest) = rest.split_at_mut(4);
        let (con2, con3) = rest.split_at_mut(4);
        fsr_easu_con_offset(
            con0.try_into().unwrap(),
            con1.try_into().unwrap(),
            con2.try_into().unwrap(),
            con3.try_into().unwrap(),
            viewport_width,
            viewport_height,
            input_image_width,
            input_image_height,
            output_image_width,
            output_image_height,
            viewport_x,
            viewport_y,
        );
        let sharpening =
            (*common::settings::values().fsr_sharpening_slider.get_value() as f32) / 100.0;
        let (rcas_con0, _) = rcas_con.split_at_mut(4);
        fsr_rcas_con(rcas_con0.try_into().unwrap(), sharpening);

        self.upload_images(scheduler);
        self.update_descriptor_sets(source_image_view, image_index);

        scheduler.request_outside_render_pass_operation_context();
        let device = self.device.clone();
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
                easu_image,
                vk::ImageLayout::GENERAL,
                vk::ImageLayout::GENERAL,
            );
            util::begin_render_pass(&device, cmdbuf, renderpass, easu_framebuffer, extent);
            device.cmd_bind_pipeline(cmdbuf, vk::PipelineBindPoint::GRAPHICS, easu_pipeline);
            device.cmd_bind_descriptor_sets(
                cmdbuf,
                vk::PipelineBindPoint::GRAPHICS,
                pipeline_layout,
                0,
                &[easu_descriptor_set],
                &[],
            );
            let easu_bytes = std::slice::from_raw_parts(
                easu_con.as_ptr() as *const u8,
                std::mem::size_of::<PushConstants>(),
            );
            device.cmd_push_constants(
                cmdbuf,
                pipeline_layout,
                vk::ShaderStageFlags::FRAGMENT,
                0,
                easu_bytes,
            );
            device.cmd_draw(cmdbuf, 3, 1, 0, 0);
            device.cmd_end_render_pass(cmdbuf);

            util::transition_image_layout(
                &device,
                cmdbuf,
                easu_image,
                vk::ImageLayout::GENERAL,
                vk::ImageLayout::GENERAL,
            );
            util::transition_image_layout(
                &device,
                cmdbuf,
                rcas_image,
                vk::ImageLayout::GENERAL,
                vk::ImageLayout::GENERAL,
            );
            util::begin_render_pass(&device, cmdbuf, renderpass, rcas_framebuffer, extent);
            device.cmd_bind_pipeline(cmdbuf, vk::PipelineBindPoint::GRAPHICS, rcas_pipeline);
            device.cmd_bind_descriptor_sets(
                cmdbuf,
                vk::PipelineBindPoint::GRAPHICS,
                pipeline_layout,
                0,
                &[rcas_descriptor_set],
                &[],
            );
            let rcas_bytes = std::slice::from_raw_parts(
                rcas_con.as_ptr() as *const u8,
                std::mem::size_of::<PushConstants>(),
            );
            device.cmd_push_constants(
                cmdbuf,
                pipeline_layout,
                vk::ShaderStageFlags::FRAGMENT,
                0,
                rcas_bytes,
            );
            device.cmd_draw(cmdbuf, 3, 1, 0, 0);
            device.cmd_end_render_pass(cmdbuf);

            util::transition_image_layout(
                &device,
                cmdbuf,
                rcas_image,
                vk::ImageLayout::GENERAL,
                vk::ImageLayout::GENERAL,
            );
        });

        output_view
    }
}

impl Drop for Fsr {
    fn drop(&mut self) {
        unsafe {
            for images in &mut self.dynamic_images {
                for framebuffer in &mut images.framebuffers {
                    if *framebuffer != vk::Framebuffer::null() {
                        self.device.destroy_framebuffer(*framebuffer, None);
                        *framebuffer = vk::Framebuffer::null();
                    }
                }
                for image_view in &mut images.image_views {
                    if *image_view != vk::ImageView::null() {
                        self.device.destroy_image_view(*image_view, None);
                        *image_view = vk::ImageView::null();
                    }
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
            if self.rcas_pipeline != vk::Pipeline::null() {
                self.device.destroy_pipeline(self.rcas_pipeline, None);
                self.rcas_pipeline = vk::Pipeline::null();
            }
            if self.easu_pipeline != vk::Pipeline::null() {
                self.device.destroy_pipeline(self.easu_pipeline, None);
                self.easu_pipeline = vk::Pipeline::null();
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
            if self.rcas_shader != vk::ShaderModule::null() {
                self.device.destroy_shader_module(self.rcas_shader, None);
                self.rcas_shader = vk::ShaderModule::null();
            }
            if self.easu_shader != vk::ShaderModule::null() {
                self.device.destroy_shader_module(self.easu_shader, None);
                self.easu_shader = vk::ShaderModule::null();
            }
            if self.vert_shader != vk::ShaderModule::null() {
                self.device.destroy_shader_module(self.vert_shader, None);
                self.vert_shader = vk::ShaderModule::null();
            }
        }
    }
}
