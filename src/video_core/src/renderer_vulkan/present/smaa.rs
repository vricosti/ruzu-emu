// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `present/smaa.h` / `present/smaa.cpp`.
//!
//! Subpixel Morphological Anti-Aliasing (SMAA) post-processing pass.

use ash::vk;
use std::ptr::NonNull;

use super::anti_alias_pass::AntiAliasPass;
use super::util;
use crate::host_shaders::spirv_shaders::{
    SMAA_BLENDING_WEIGHT_CALCULATION_FRAG_SPV, SMAA_BLENDING_WEIGHT_CALCULATION_VERT_SPV,
    SMAA_EDGE_DETECTION_FRAG_SPV, SMAA_EDGE_DETECTION_VERT_SPV,
    SMAA_NEIGHBORHOOD_BLENDING_FRAG_SPV, SMAA_NEIGHBORHOOD_BLENDING_VERT_SPV,
};
use crate::renderer_vulkan::scheduler::Scheduler;
use crate::renderer_vulkan::shader_util::build_shader;
use crate::smaa_area_tex::{AREATEX_HEIGHT, AREATEX_WIDTH, AREA_TEX_BYTES};
use crate::smaa_search_tex::{SEARCHTEX_HEIGHT, SEARCHTEX_WIDTH, SEARCH_TEX_BYTES};
use crate::vulkan_common::vulkan_device::Device;
use crate::vulkan_common::vulkan_memory_allocator::{AllocatedImage, MemoryAllocator};

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// Port of `SMAA::SMAAStage` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
enum SmaaStage {
    EdgeDetection = 0,
    BlendingWeightCalculation = 1,
    NeighborhoodBlending = 2,
}

const MAX_SMAA_STAGE: usize = 3;

/// Port of `SMAA::StaticImageType` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
enum StaticImageType {
    Area = 0,
    Search = 1,
}

const MAX_STATIC_IMAGE: usize = 2;

/// Port of `SMAA::DynamicImageType` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
enum DynamicImageType {
    Blend = 0,
    Edges = 1,
    Output = 2,
}

const MAX_DYNAMIC_IMAGE: usize = 3;

// ---------------------------------------------------------------------------
// Per-image dynamic resources
// ---------------------------------------------------------------------------

/// Port of `SMAA::Images` inner struct.
struct SmaaImages {
    descriptor_sets: Vec<vk::DescriptorSet>,
    images: [AllocatedImage; MAX_DYNAMIC_IMAGE],
    image_views: [vk::ImageView; MAX_DYNAMIC_IMAGE],
    framebuffers: [vk::Framebuffer; MAX_SMAA_STAGE],
}

// ---------------------------------------------------------------------------
// SMAA
// ---------------------------------------------------------------------------

/// Port of `SMAA` class.
///
/// Three-stage SMAA anti-aliasing: edge detection, blending weight
/// calculation, and neighborhood blending.
pub struct Smaa {
    device: ash::Device,
    allocator: NonNull<MemoryAllocator>,
    extent: vk::Extent2D,
    image_count: u32,
    images_ready: bool,
    dynamic_images: Vec<SmaaImages>,
    static_images: [AllocatedImage; MAX_STATIC_IMAGE],
    static_image_views: [vk::ImageView; MAX_STATIC_IMAGE],

    descriptor_pool: vk::DescriptorPool,
    descriptor_set_layouts: [vk::DescriptorSetLayout; MAX_SMAA_STAGE],
    pipeline_layouts: [vk::PipelineLayout; MAX_SMAA_STAGE],
    vertex_shaders: [vk::ShaderModule; MAX_SMAA_STAGE],
    fragment_shaders: [vk::ShaderModule; MAX_SMAA_STAGE],
    pipelines: [vk::Pipeline; MAX_SMAA_STAGE],
    renderpasses: [vk::RenderPass; MAX_SMAA_STAGE],
    sampler: vk::Sampler,
}

impl Smaa {
    /// Port of `SMAA::SMAA`.
    pub fn new(
        device: &Device,
        allocator: &MemoryAllocator,
        image_count: usize,
        extent: vk::Extent2D,
    ) -> Self {
        let logical = device.get_logical();
        let mut smaa = Smaa {
            device: logical.clone(),
            allocator: NonNull::from(allocator),
            extent,
            image_count: image_count as u32,
            images_ready: false,
            dynamic_images: Vec::new(),
            static_images: std::array::from_fn(|_| AllocatedImage::null()),
            static_image_views: [vk::ImageView::null(); MAX_STATIC_IMAGE],
            descriptor_pool: vk::DescriptorPool::null(),
            descriptor_set_layouts: [vk::DescriptorSetLayout::null(); MAX_SMAA_STAGE],
            pipeline_layouts: [vk::PipelineLayout::null(); MAX_SMAA_STAGE],
            vertex_shaders: [vk::ShaderModule::null(); MAX_SMAA_STAGE],
            fragment_shaders: [vk::ShaderModule::null(); MAX_SMAA_STAGE],
            pipelines: [vk::Pipeline::null(); MAX_SMAA_STAGE],
            renderpasses: [vk::RenderPass::null(); MAX_SMAA_STAGE],
            sampler: vk::Sampler::null(),
        };

        smaa.create_images(device);
        smaa.create_render_passes(device);
        smaa.create_sampler(device);
        smaa.create_shaders(device);
        smaa.create_descriptor_pool(device);
        smaa.create_descriptor_set_layouts(device);
        smaa.create_descriptor_sets(device);
        smaa.create_pipeline_layouts(device);
        smaa.create_pipelines(device);
        smaa
    }

    /// Port of `SMAA::CreateImages`.
    fn create_images(&mut self, device: &Device) {
        let allocator = unsafe { self.allocator.as_ref() };
        let area_extent = vk::Extent2D {
            width: AREATEX_WIDTH,
            height: AREATEX_HEIGHT,
        };
        let search_extent = vk::Extent2D {
            width: SEARCHTEX_WIDTH,
            height: SEARCHTEX_HEIGHT,
        };

        self.static_images[StaticImageType::Area as usize] =
            util::create_wrapped_image(allocator, area_extent, vk::Format::R8G8_UNORM);
        self.static_images[StaticImageType::Search as usize] =
            util::create_wrapped_image(allocator, search_extent, vk::Format::R8_UNORM);
        self.static_image_views[StaticImageType::Area as usize] = util::create_wrapped_image_view(
            device.get_logical(),
            self.static_images[StaticImageType::Area as usize].handle(),
            vk::Format::R8G8_UNORM,
        );
        self.static_image_views[StaticImageType::Search as usize] = util::create_wrapped_image_view(
            device.get_logical(),
            self.static_images[StaticImageType::Search as usize].handle(),
            vk::Format::R8_UNORM,
        );

        self.dynamic_images.reserve_exact(self.image_count as usize);
        for _ in 0..self.image_count {
            let blend_image =
                util::create_wrapped_image(allocator, self.extent, vk::Format::R16G16B16A16_SFLOAT);
            let edges_image =
                util::create_wrapped_image(allocator, self.extent, vk::Format::R16G16_SFLOAT);
            let output_image =
                util::create_wrapped_image(allocator, self.extent, vk::Format::R16G16B16A16_SFLOAT);
            let blend_view = util::create_wrapped_image_view(
                device.get_logical(),
                blend_image.handle(),
                vk::Format::R16G16B16A16_SFLOAT,
            );
            let edges_view = util::create_wrapped_image_view(
                device.get_logical(),
                edges_image.handle(),
                vk::Format::R16G16_SFLOAT,
            );
            let output_view = util::create_wrapped_image_view(
                device.get_logical(),
                output_image.handle(),
                vk::Format::R16G16B16A16_SFLOAT,
            );
            self.dynamic_images.push(SmaaImages {
                descriptor_sets: Vec::new(),
                images: [blend_image, edges_image, output_image],
                image_views: [blend_view, edges_view, output_view],
                framebuffers: [vk::Framebuffer::null(); MAX_SMAA_STAGE],
            });
        }
    }

    /// Port of `SMAA::CreateRenderPasses`.
    fn create_render_passes(&mut self, device: &Device) {
        self.renderpasses[SmaaStage::EdgeDetection as usize] = util::create_wrapped_render_pass(
            device.get_logical(),
            vk::Format::R16G16_SFLOAT,
            vk::ImageLayout::GENERAL,
        );
        self.renderpasses[SmaaStage::BlendingWeightCalculation as usize] =
            util::create_wrapped_render_pass(
                device.get_logical(),
                vk::Format::R16G16B16A16_SFLOAT,
                vk::ImageLayout::GENERAL,
            );
        self.renderpasses[SmaaStage::NeighborhoodBlending as usize] =
            util::create_wrapped_render_pass(
                device.get_logical(),
                vk::Format::R16G16B16A16_SFLOAT,
                vk::ImageLayout::GENERAL,
            );

        for images in &mut self.dynamic_images {
            images.framebuffers[SmaaStage::EdgeDetection as usize] =
                util::create_wrapped_framebuffer(
                    device.get_logical(),
                    self.renderpasses[SmaaStage::EdgeDetection as usize],
                    images.image_views[DynamicImageType::Edges as usize],
                    self.extent,
                );
            images.framebuffers[SmaaStage::BlendingWeightCalculation as usize] =
                util::create_wrapped_framebuffer(
                    device.get_logical(),
                    self.renderpasses[SmaaStage::BlendingWeightCalculation as usize],
                    images.image_views[DynamicImageType::Blend as usize],
                    self.extent,
                );
            images.framebuffers[SmaaStage::NeighborhoodBlending as usize] =
                util::create_wrapped_framebuffer(
                    device.get_logical(),
                    self.renderpasses[SmaaStage::NeighborhoodBlending as usize],
                    images.image_views[DynamicImageType::Output as usize],
                    self.extent,
                );
        }
    }

    /// Port of `SMAA::CreateSampler`.
    fn create_sampler(&mut self, device: &Device) {
        self.sampler = util::create_wrapped_sampler(device.get_logical(), vk::Filter::LINEAR);
    }

    /// Port of `SMAA::CreateShaders`.
    fn create_shaders(&mut self, device: &Device) {
        let vertex_shader_sources = [
            SMAA_EDGE_DETECTION_VERT_SPV,
            SMAA_BLENDING_WEIGHT_CALCULATION_VERT_SPV,
            SMAA_NEIGHBORHOOD_BLENDING_VERT_SPV,
        ];
        let fragment_shader_sources = [
            SMAA_EDGE_DETECTION_FRAG_SPV,
            SMAA_BLENDING_WEIGHT_CALCULATION_FRAG_SPV,
            SMAA_NEIGHBORHOOD_BLENDING_FRAG_SPV,
        ];
        for index in 0..MAX_SMAA_STAGE {
            self.vertex_shaders[index] =
                build_shader(device.get_logical(), vertex_shader_sources[index])
                    .expect("Failed to build SMAA vertex shader");
            self.fragment_shaders[index] =
                build_shader(device.get_logical(), fragment_shader_sources[index])
                    .expect("Failed to build SMAA fragment shader");
        }
    }

    /// Port of `SMAA::CreateDescriptorPool`.
    fn create_descriptor_pool(&mut self, device: &Device) {
        // Edge detection: 1 descriptor
        // Blending weight calculation: 3 descriptors
        // Neighborhood blending: 2 descriptors
        // 6 descriptors, 3 descriptor sets per image
        self.descriptor_pool = util::create_wrapped_descriptor_pool(
            device.get_logical(),
            6 * self.image_count,
            3 * self.image_count,
            &[vk::DescriptorType::COMBINED_IMAGE_SAMPLER],
        );
    }

    /// Port of `SMAA::CreateDescriptorSetLayouts`.
    fn create_descriptor_set_layouts(&mut self, device: &Device) {
        self.descriptor_set_layouts[SmaaStage::EdgeDetection as usize] =
            util::create_wrapped_descriptor_set_layout(
                device.get_logical(),
                &[vk::DescriptorType::COMBINED_IMAGE_SAMPLER],
            );
        self.descriptor_set_layouts[SmaaStage::BlendingWeightCalculation as usize] =
            util::create_wrapped_descriptor_set_layout(
                device.get_logical(),
                &[
                    vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                    vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                    vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                ],
            );
        self.descriptor_set_layouts[SmaaStage::NeighborhoodBlending as usize] =
            util::create_wrapped_descriptor_set_layout(
                device.get_logical(),
                &[
                    vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                    vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                ],
            );
    }

    /// Port of `SMAA::CreateDescriptorSets`.
    fn create_descriptor_sets(&mut self, device: &Device) {
        let layouts: Vec<vk::DescriptorSetLayout> =
            self.descriptor_set_layouts.iter().copied().collect();
        for images in &mut self.dynamic_images {
            images.descriptor_sets = util::create_wrapped_descriptor_sets(
                device.get_logical(),
                self.descriptor_pool,
                &layouts,
            );
        }
    }

    /// Port of `SMAA::CreatePipelineLayouts`.
    fn create_pipeline_layouts(&mut self, device: &Device) {
        for index in 0..MAX_SMAA_STAGE {
            self.pipeline_layouts[index] = util::create_wrapped_pipeline_layout(
                device.get_logical(),
                self.descriptor_set_layouts[index],
            );
        }
    }

    /// Port of `SMAA::CreatePipelines`.
    fn create_pipelines(&mut self, device: &Device) {
        for index in 0..MAX_SMAA_STAGE {
            self.pipelines[index] = util::create_wrapped_pipeline(
                device.get_logical(),
                self.renderpasses[index],
                self.pipeline_layouts[index],
                self.vertex_shaders[index],
                self.fragment_shaders[index],
            );
        }
    }

    /// Port of `SMAA::UpdateDescriptorSets`.
    fn update_descriptor_sets(
        &self,
        device: &Device,
        image_view: vk::ImageView,
        image_index: usize,
    ) {
        let images = &self.dynamic_images[image_index];
        let mut image_infos = Vec::with_capacity(6);
        let mut updates = Vec::new();

        // Edge detection: source image
        updates.push(util::create_write_descriptor_set(
            &mut image_infos,
            self.sampler,
            image_view,
            images.descriptor_sets[SmaaStage::EdgeDetection as usize],
            0,
        ));

        // Blending weight calculation: edges, area, search
        updates.push(util::create_write_descriptor_set(
            &mut image_infos,
            self.sampler,
            images.image_views[DynamicImageType::Edges as usize],
            images.descriptor_sets[SmaaStage::BlendingWeightCalculation as usize],
            0,
        ));
        updates.push(util::create_write_descriptor_set(
            &mut image_infos,
            self.sampler,
            self.static_image_views[StaticImageType::Area as usize],
            images.descriptor_sets[SmaaStage::BlendingWeightCalculation as usize],
            1,
        ));
        updates.push(util::create_write_descriptor_set(
            &mut image_infos,
            self.sampler,
            self.static_image_views[StaticImageType::Search as usize],
            images.descriptor_sets[SmaaStage::BlendingWeightCalculation as usize],
            2,
        ));

        // Neighborhood blending: source, blend
        updates.push(util::create_write_descriptor_set(
            &mut image_infos,
            self.sampler,
            image_view,
            images.descriptor_sets[SmaaStage::NeighborhoodBlending as usize],
            0,
        ));
        updates.push(util::create_write_descriptor_set(
            &mut image_infos,
            self.sampler,
            images.image_views[DynamicImageType::Blend as usize],
            images.descriptor_sets[SmaaStage::NeighborhoodBlending as usize],
            1,
        ));

        unsafe {
            device.get_logical().update_descriptor_sets(&updates, &[]);
        }
    }

    /// Port of `SMAA::UploadImages`.
    fn upload_images(&mut self, device: &Device, scheduler: &mut Scheduler) {
        if self.images_ready {
            return;
        }

        let area_image = self.static_images[StaticImageType::Area as usize].handle();
        let search_image = self.static_images[StaticImageType::Search as usize].handle();
        let allocator = unsafe { self.allocator.as_ref() };

        util::upload_image(
            device.get_logical(),
            allocator,
            scheduler,
            area_image,
            vk::Extent2D {
                width: AREATEX_WIDTH,
                height: AREATEX_HEIGHT,
            },
            vk::Format::R8G8_UNORM,
            AREA_TEX_BYTES,
        );
        util::upload_image(
            device.get_logical(),
            allocator,
            scheduler,
            search_image,
            vk::Extent2D {
                width: SEARCHTEX_WIDTH,
                height: SEARCHTEX_HEIGHT,
            },
            vk::Format::R8_UNORM,
            SEARCH_TEX_BYTES,
        );

        let dynamic_images: Vec<[vk::Image; MAX_DYNAMIC_IMAGE]> = self
            .dynamic_images
            .iter()
            .map(|images| {
                [
                    images.images[0].handle(),
                    images.images[1].handle(),
                    images.images[2].handle(),
                ]
            })
            .collect();

        let device = device.get_logical().clone();
        scheduler.record(move |cmdbuf| {
            for images in dynamic_images {
                for image in images {
                    util::clear_color_image(&device, cmdbuf, image);
                }
            }
        });
        scheduler.finish();

        self.images_ready = true;
    }
}

impl AntiAliasPass for Smaa {
    /// Port of `SMAA::Draw`.
    ///
    /// Records three-pass SMAA: edge detection, blending weight calculation,
    /// and neighborhood blending. Swaps the image/view pointers to the output.
    fn draw(
        &mut self,
        device: &Device,
        scheduler: &mut Scheduler,
        image_index: usize,
        inout_image: &mut vk::Image,
        inout_image_view: &mut vk::ImageView,
    ) {
        let images = &self.dynamic_images[image_index];

        let input_image = *inout_image;
        let output_image = images.images[DynamicImageType::Output as usize].handle();
        let output_image_view = images.image_views[DynamicImageType::Output as usize];
        let edges_image = images.images[DynamicImageType::Edges as usize].handle();
        let blend_image = images.images[DynamicImageType::Blend as usize].handle();

        let edge_detection_ds = images.descriptor_sets[SmaaStage::EdgeDetection as usize];
        let blending_weight_ds =
            images.descriptor_sets[SmaaStage::BlendingWeightCalculation as usize];
        let neighborhood_ds = images.descriptor_sets[SmaaStage::NeighborhoodBlending as usize];

        let edge_fb = images.framebuffers[SmaaStage::EdgeDetection as usize];
        let blend_fb = images.framebuffers[SmaaStage::BlendingWeightCalculation as usize];
        let neighborhood_fb = images.framebuffers[SmaaStage::NeighborhoodBlending as usize];
        let renderpasses = self.renderpasses;
        let pipelines = self.pipelines;
        let pipeline_layouts = self.pipeline_layouts;
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
                edges_image,
                vk::ImageLayout::GENERAL,
                vk::ImageLayout::GENERAL,
            );
            util::begin_render_pass(
                &device,
                cmdbuf,
                renderpasses[SmaaStage::EdgeDetection as usize],
                edge_fb,
                extent,
            );
            device.cmd_bind_pipeline(
                cmdbuf,
                vk::PipelineBindPoint::GRAPHICS,
                pipelines[SmaaStage::EdgeDetection as usize],
            );
            device.cmd_bind_descriptor_sets(
                cmdbuf,
                vk::PipelineBindPoint::GRAPHICS,
                pipeline_layouts[SmaaStage::EdgeDetection as usize],
                0,
                &[edge_detection_ds],
                &[],
            );
            device.cmd_draw(cmdbuf, 3, 1, 0, 0);
            device.cmd_end_render_pass(cmdbuf);

            util::transition_image_layout(
                &device,
                cmdbuf,
                edges_image,
                vk::ImageLayout::GENERAL,
                vk::ImageLayout::GENERAL,
            );
            util::transition_image_layout(
                &device,
                cmdbuf,
                blend_image,
                vk::ImageLayout::GENERAL,
                vk::ImageLayout::GENERAL,
            );
            util::begin_render_pass(
                &device,
                cmdbuf,
                renderpasses[SmaaStage::BlendingWeightCalculation as usize],
                blend_fb,
                extent,
            );
            device.cmd_bind_pipeline(
                cmdbuf,
                vk::PipelineBindPoint::GRAPHICS,
                pipelines[SmaaStage::BlendingWeightCalculation as usize],
            );
            device.cmd_bind_descriptor_sets(
                cmdbuf,
                vk::PipelineBindPoint::GRAPHICS,
                pipeline_layouts[SmaaStage::BlendingWeightCalculation as usize],
                0,
                &[blending_weight_ds],
                &[],
            );
            device.cmd_draw(cmdbuf, 3, 1, 0, 0);
            device.cmd_end_render_pass(cmdbuf);

            util::transition_image_layout(
                &device,
                cmdbuf,
                blend_image,
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
            util::begin_render_pass(
                &device,
                cmdbuf,
                renderpasses[SmaaStage::NeighborhoodBlending as usize],
                neighborhood_fb,
                extent,
            );
            device.cmd_bind_pipeline(
                cmdbuf,
                vk::PipelineBindPoint::GRAPHICS,
                pipelines[SmaaStage::NeighborhoodBlending as usize],
            );
            device.cmd_bind_descriptor_sets(
                cmdbuf,
                vk::PipelineBindPoint::GRAPHICS,
                pipeline_layouts[SmaaStage::NeighborhoodBlending as usize],
                0,
                &[neighborhood_ds],
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

impl Drop for Smaa {
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
            for image_view in &mut self.static_image_views {
                if *image_view != vk::ImageView::null() {
                    self.device.destroy_image_view(*image_view, None);
                    *image_view = vk::ImageView::null();
                }
            }
            if self.sampler != vk::Sampler::null() {
                self.device.destroy_sampler(self.sampler, None);
                self.sampler = vk::Sampler::null();
            }
            for renderpass in &mut self.renderpasses {
                if *renderpass != vk::RenderPass::null() {
                    self.device.destroy_render_pass(*renderpass, None);
                    *renderpass = vk::RenderPass::null();
                }
            }
            for pipeline in &mut self.pipelines {
                if *pipeline != vk::Pipeline::null() {
                    self.device.destroy_pipeline(*pipeline, None);
                    *pipeline = vk::Pipeline::null();
                }
            }
            for layout in &mut self.pipeline_layouts {
                if *layout != vk::PipelineLayout::null() {
                    self.device.destroy_pipeline_layout(*layout, None);
                    *layout = vk::PipelineLayout::null();
                }
            }
            for layout in &mut self.descriptor_set_layouts {
                if *layout != vk::DescriptorSetLayout::null() {
                    self.device.destroy_descriptor_set_layout(*layout, None);
                    *layout = vk::DescriptorSetLayout::null();
                }
            }
            if self.descriptor_pool != vk::DescriptorPool::null() {
                self.device
                    .destroy_descriptor_pool(self.descriptor_pool, None);
                self.descriptor_pool = vk::DescriptorPool::null();
            }
            for shader in &mut self.fragment_shaders {
                if *shader != vk::ShaderModule::null() {
                    self.device.destroy_shader_module(*shader, None);
                    *shader = vk::ShaderModule::null();
                }
            }
            for shader in &mut self.vertex_shaders {
                if *shader != vk::ShaderModule::null() {
                    self.device.destroy_shader_module(*shader, None);
                    *shader = vk::ShaderModule::null();
                }
            }
        }
    }
}
