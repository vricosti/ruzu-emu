// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `present/window_adapt_pass.h` / `present/window_adapt_pass.cpp`.
//!
//! Final presentation pass that composites layers into the destination frame
//! using opaque, premultiplied, or coverage blending pipelines.

use ash::vk;
use std::collections::LinkedList;

use crate::framebuffer_config::{BlendMode, FramebufferConfig};
use crate::host_shaders::spirv_shaders::VULKAN_PRESENT_VERT_SPV;
use crate::renderer_vulkan::present::layer::Layer;
use crate::renderer_vulkan::present_manager::Frame;
use crate::renderer_vulkan::scheduler::Scheduler;
use crate::renderer_vulkan::shader_util::build_shader;
use crate::renderer_vulkan::RasterizerVulkan;
use crate::vulkan_common::vulkan_device::Device;
use ruzu_core::frontend::framebuffer_layout::FramebufferLayout;

use super::present_push_constants::PresentPushConstants;
use super::util;

// ---------------------------------------------------------------------------
// WindowAdaptPass
// ---------------------------------------------------------------------------

/// Port of `WindowAdaptPass` class.
///
/// Owns the render pass, pipelines (opaque, premultiplied, coverage),
/// descriptor set layout, pipeline layout, sampler, and shaders for
/// compositing presentation layers into the swapchain frame.
pub struct WindowAdaptPass {
    device: ash::Device,
    descriptor_set_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    sampler: vk::Sampler,
    vertex_shader: vk::ShaderModule,
    fragment_shader: vk::ShaderModule,
    render_pass: vk::RenderPass,
    opaque_pipeline: vk::Pipeline,
    premultiplied_pipeline: vk::Pipeline,
    coverage_pipeline: vk::Pipeline,
}

impl WindowAdaptPass {
    /// Port of `WindowAdaptPass::WindowAdaptPass`.
    pub fn new(
        device: &Device,
        frame_format: vk::Format,
        sampler: vk::Sampler,
        fragment_shader: vk::ShaderModule,
    ) -> Self {
        let descriptor_set_layout = Self::create_descriptor_set_layout(device);
        let pipeline_layout = Self::create_pipeline_layout(device, descriptor_set_layout);
        let vertex_shader = Self::create_vertex_shader(device);
        let render_pass = Self::create_render_pass(device, frame_format);
        let (opaque_pipeline, premultiplied_pipeline, coverage_pipeline) = Self::create_pipelines(
            device,
            render_pass,
            pipeline_layout,
            vertex_shader,
            fragment_shader,
        );

        WindowAdaptPass {
            device: device.get_logical().clone(),
            descriptor_set_layout,
            pipeline_layout,
            sampler,
            vertex_shader,
            fragment_shader,
            render_pass,
            opaque_pipeline,
            premultiplied_pipeline,
            coverage_pipeline,
        }
    }

    /// Port of `WindowAdaptPass::CreateDescriptorSetLayout`.
    fn create_descriptor_set_layout(device: &Device) -> vk::DescriptorSetLayout {
        util::create_wrapped_descriptor_set_layout(
            device,
            &[vk::DescriptorType::COMBINED_IMAGE_SAMPLER],
            vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
        )
    }

    /// Port of `WindowAdaptPass::CreatePipelineLayout`.
    fn create_pipeline_layout(
        device: &Device,
        descriptor_set_layout: vk::DescriptorSetLayout,
    ) -> vk::PipelineLayout {
        let range = vk::PushConstantRange {
            stage_flags: vk::ShaderStageFlags::VERTEX,
            offset: 0,
            size: std::mem::size_of::<PresentPushConstants>() as u32,
        };
        let set_layouts = [descriptor_set_layout];
        let create_info = vk::PipelineLayoutCreateInfo::builder()
            .set_layouts(&set_layouts)
            .push_constant_ranges(std::slice::from_ref(&range))
            .build();
        unsafe {
            device
                .get_logical()
                .create_pipeline_layout(&create_info, None)
                .expect("Failed to create WindowAdaptPass pipeline layout")
        }
    }

    /// Port of `WindowAdaptPass::CreateVertexShader`.
    fn create_vertex_shader(device: &Device) -> vk::ShaderModule {
        build_shader(device.get_logical(), VULKAN_PRESENT_VERT_SPV)
            .expect("Failed to build vulkan_present.vert")
    }

    /// Port of `WindowAdaptPass::CreateRenderPass`.
    fn create_render_pass(device: &Device, frame_format: vk::Format) -> vk::RenderPass {
        util::create_wrapped_render_pass(device, frame_format, vk::ImageLayout::UNDEFINED)
    }

    /// Port of `WindowAdaptPass::CreatePipelines`.
    fn create_pipelines(
        device: &Device,
        render_pass: vk::RenderPass,
        pipeline_layout: vk::PipelineLayout,
        vertex_shader: vk::ShaderModule,
        fragment_shader: vk::ShaderModule,
    ) -> (vk::Pipeline, vk::Pipeline, vk::Pipeline) {
        (
            util::create_wrapped_pipeline(
                device,
                render_pass,
                pipeline_layout,
                vertex_shader,
                fragment_shader,
            ),
            util::create_wrapped_premultiplied_blending_pipeline(
                device,
                render_pass,
                pipeline_layout,
                vertex_shader,
                fragment_shader,
            ),
            util::create_wrapped_coverage_blending_pipeline(
                device,
                render_pass,
                pipeline_layout,
                vertex_shader,
                fragment_shader,
            ),
        )
    }

    /// Port of `WindowAdaptPass::GetDescriptorSetLayout`.
    pub fn get_descriptor_set_layout(&self) -> vk::DescriptorSetLayout {
        self.descriptor_set_layout
    }

    /// Port of `WindowAdaptPass::GetRenderPass`.
    pub fn get_render_pass(&self) -> vk::RenderPass {
        self.render_pass
    }

    /// Port of `WindowAdaptPass::Draw`.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        device: &Device,
        rasterizer: &mut RasterizerVulkan,
        scheduler: &mut Scheduler,
        image_index: usize,
        layers: &mut LinkedList<Layer>,
        configs: &[FramebufferConfig],
        layout: &FramebufferLayout,
        dst: &Frame,
    ) {
        let host_framebuffer = dst.framebuffer;
        let render_area = vk::Extent2D {
            width: dst.width,
            height: dst.height,
        };
        let layer_count = configs.len();
        let mut push_constants = vec![PresentPushConstants::default(); layer_count];
        let mut descriptor_sets = vec![vk::DescriptorSet::null(); layer_count];
        let mut graphics_pipelines = vec![vk::Pipeline::null(); layer_count];

        let mut layer_it = layers.iter_mut();
        for i in 0..layer_count {
            graphics_pipelines[i] = match configs[i].blending {
                BlendMode::Opaque => self.opaque_pipeline,
                BlendMode::Premultiplied => self.premultiplied_pipeline,
                BlendMode::Coverage => self.coverage_pipeline,
            };
            layer_it
                .next()
                .expect("each framebuffer must have a presentation layer")
                .configure_draw(
                    device,
                    &mut push_constants[i],
                    &mut descriptor_sets[i],
                    rasterizer,
                    self.sampler,
                    image_index,
                    &configs[i],
                    layout,
                );
        }

        let device = self.device.clone();
        let render_pass = self.render_pass;
        let pipeline_layout = self.pipeline_layout;

        scheduler.record(move |cmdbuf| unsafe {
            let values = common::settings::values();
            let bg_color = normalized_background_color(
                *values.bg_red.get_value(),
                *values.bg_green.get_value(),
                *values.bg_blue.get_value(),
            );
            util::begin_render_pass(&device, cmdbuf, render_pass, host_framebuffer, render_area);

            let clear_attachment = vk::ClearAttachment {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                color_attachment: 0,
                clear_value: vk::ClearValue {
                    color: vk::ClearColorValue { float32: bg_color },
                },
            };
            let clear_rect = vk::ClearRect {
                rect: vk::Rect2D {
                    offset: vk::Offset2D { x: 0, y: 0 },
                    extent: render_area,
                },
                base_array_layer: 0,
                layer_count: 1,
            };
            device.cmd_clear_attachments(cmdbuf, &[clear_attachment], &[clear_rect]);

            for i in 0..layer_count {
                device.cmd_bind_pipeline(
                    cmdbuf,
                    vk::PipelineBindPoint::GRAPHICS,
                    graphics_pipelines[i],
                );

                let constants_bytes: &[u8] = std::slice::from_raw_parts(
                    &push_constants[i] as *const PresentPushConstants as *const u8,
                    std::mem::size_of::<PresentPushConstants>(),
                );
                device.cmd_push_constants(
                    cmdbuf,
                    pipeline_layout,
                    vk::ShaderStageFlags::VERTEX,
                    0,
                    constants_bytes,
                );

                device.cmd_bind_descriptor_sets(
                    cmdbuf,
                    vk::PipelineBindPoint::GRAPHICS,
                    pipeline_layout,
                    0,
                    &[descriptor_sets[i]],
                    &[],
                );

                device.cmd_draw(cmdbuf, 4, 1, 0, 0);
            }

            device.cmd_end_render_pass(cmdbuf);
        });
    }
}

fn normalized_background_color(red: u8, green: u8, blue: u8) -> [f32; 4] {
    [
        red as f32 / 255.0,
        green as f32 / 255.0,
        blue as f32 / 255.0,
        1.0,
    ]
}

#[cfg(test)]
mod tests {
    use super::normalized_background_color;

    #[test]
    fn background_color_uses_all_three_configured_channels() {
        assert_eq!(
            normalized_background_color(255, 128, 0),
            [1.0, 128.0 / 255.0, 0.0, 1.0]
        );
    }
}

impl Drop for WindowAdaptPass {
    fn drop(&mut self) {
        unsafe {
            if self.coverage_pipeline != vk::Pipeline::null() {
                self.device.destroy_pipeline(self.coverage_pipeline, None);
                self.coverage_pipeline = vk::Pipeline::null();
            }
            if self.premultiplied_pipeline != vk::Pipeline::null() {
                self.device
                    .destroy_pipeline(self.premultiplied_pipeline, None);
                self.premultiplied_pipeline = vk::Pipeline::null();
            }
            if self.opaque_pipeline != vk::Pipeline::null() {
                self.device.destroy_pipeline(self.opaque_pipeline, None);
                self.opaque_pipeline = vk::Pipeline::null();
            }
            if self.render_pass != vk::RenderPass::null() {
                self.device.destroy_render_pass(self.render_pass, None);
                self.render_pass = vk::RenderPass::null();
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
            if self.sampler != vk::Sampler::null() {
                self.device.destroy_sampler(self.sampler, None);
                self.sampler = vk::Sampler::null();
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
        }
    }
}
