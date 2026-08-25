// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `vk_blit_screen.h` / `vk_blit_screen.cpp`.
//!
//! Composites guest framebuffers into a presentable frame using layers
//! and a window adaptation pass.

use ash::vk;
use std::sync::Arc;

use crate::framebuffer_config::{BlendMode as FramebufferBlendMode, FramebufferConfig};
use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
use crate::present::{PresentFilters, ScalingFilter};
use crate::vulkan_common::vulkan_device::Device;
use crate::vulkan_common::vulkan_memory_allocator::MemoryAllocator;

use super::present::filters;
use super::present::layer::Layer;
use super::present::present_push_constants::PresentPushConstants;
use super::present::window_adapt_pass::{BlendMode, WindowAdaptPass};
use super::present_manager::{Frame, PresentManager};
use super::scheduler::Scheduler;
use super::RasterizerVulkan;
use ruzu_core::frontend::framebuffer_layout::FramebufferLayout;

// ---------------------------------------------------------------------------
// FramebufferTextureInfo
// ---------------------------------------------------------------------------

/// Port of `FramebufferTextureInfo` struct.
///
/// Information about a guest framebuffer's backing Vulkan image/view.
#[derive(Debug, Clone, Copy, Default)]
pub struct FramebufferTextureInfo {
    pub image: vk::Image,
    pub image_view: vk::ImageView,
    pub width: u32,
    pub height: u32,
    pub scaled_width: u32,
    pub scaled_height: u32,
}

/// Non-owning draw target matching the fields used from upstream `Frame*`.
#[derive(Debug, Clone, Copy, Default)]
pub struct BlitFrame {
    pub width: u32,
    pub height: u32,
    pub framebuffer: vk::Framebuffer,
}

impl From<&Frame> for BlitFrame {
    fn from(frame: &Frame) -> Self {
        Self {
            width: frame.width,
            height: frame.height,
            framebuffer: frame.framebuffer,
        }
    }
}

// ---------------------------------------------------------------------------
// BlitScreen
// ---------------------------------------------------------------------------

/// Port of `BlitScreen` class.
///
/// Manages layers, anti-aliasing, scaling, and the window adaptation pass
/// to composite guest framebuffers into a presentable output frame.
pub struct BlitScreen {
    device: ash::Device,
    image_count: usize,
    image_index: usize,
    swapchain_view_format: vk::Format,
    scaling_filter: Option<ScalingFilter>,
    window_adapt: Option<WindowAdaptPass>,
    layers: Vec<Layer>,
    filters: &'static PresentFilters,
}

impl BlitScreen {
    /// Port of `BlitScreen::BlitScreen`.
    pub fn new(device: ash::Device, filters: &'static PresentFilters) -> Self {
        BlitScreen {
            device,
            image_count: 1,
            image_index: 0,
            swapchain_view_format: vk::Format::B8G8R8A8_UNORM,
            scaling_filter: None,
            window_adapt: None,
            layers: Vec::new(),
            filters,
        }
    }

    /// Port of `BlitScreen::DrawToFrame` for present-manager frames.
    pub fn draw_to_present_frame(
        &mut self,
        device: &Device,
        rasterizer: &mut RasterizerVulkan,
        scheduler: &mut Scheduler,
        present_manager: &mut PresentManager,
        allocator: &MemoryAllocator,
        device_memory: &Arc<MaxwellDeviceMemoryManager>,
        frame_index: usize,
        framebuffers: &[FramebufferConfig],
        layout: &FramebufferLayout,
        current_swapchain_image_count: usize,
        current_swapchain_view_format: vk::Format,
    ) {
        let mut resource_update_required = false;
        let mut presentation_recreate_required = false;
        let current_scaling_filter = self.current_scaling_filter();

        if self.window_adapt.is_none() || self.scaling_filter != Some(current_scaling_filter) {
            resource_update_required = true;
        }

        let old_count = self.image_count;
        self.image_count = current_swapchain_image_count;
        if old_count != current_swapchain_image_count {
            resource_update_required = true;
        }

        let old_format = self.swapchain_view_format;
        self.swapchain_view_format = current_swapchain_view_format;
        let frame_width = present_manager.frame(frame_index).width;
        let frame_height = present_manager.frame(frame_index).height;
        if old_format != current_swapchain_view_format
            || layout.width != frame_width
            || layout.height != frame_height
        {
            resource_update_required = true;
            presentation_recreate_required = true;
        }

        if resource_update_required {
            self.wait_idle(scheduler, present_manager);
            self.set_window_adapt_pass(device);

            if presentation_recreate_required {
                if let Some(ref window_adapt) = self.window_adapt {
                    present_manager.recreate_frame_by_index(
                        frame_index,
                        layout.width,
                        layout.height,
                        self.swapchain_view_format,
                        window_adapt.get_render_pass(),
                    );
                }
            }
        }

        let frame = BlitFrame::from(present_manager.frame(frame_index));
        self.draw_to_frame(
            device,
            rasterizer,
            scheduler,
            present_manager,
            allocator,
            device_memory,
            frame,
            framebuffers,
            layout,
            current_swapchain_image_count,
            current_swapchain_view_format,
        );
    }

    /// Port of `BlitScreen::DrawToFrame`.
    ///
    /// Draws the guest framebuffers into the given presentation frame,
    /// recreating resources as needed when the swapchain format/size changes.
    pub fn draw_to_frame(
        &mut self,
        device: &Device,
        rasterizer: &mut RasterizerVulkan,
        scheduler: &mut Scheduler,
        present_manager: &PresentManager,
        allocator: &MemoryAllocator,
        device_memory: &Arc<MaxwellDeviceMemoryManager>,
        frame: BlitFrame,
        framebuffers: &[FramebufferConfig],
        layout: &FramebufferLayout,
        current_swapchain_image_count: usize,
        current_swapchain_view_format: vk::Format,
    ) {
        let mut resource_update_required = false;
        let mut presentation_recreate_required = false;
        let current_scaling_filter = self.current_scaling_filter();

        // Check if scaling filter changed
        if self.window_adapt.is_none() || self.scaling_filter != Some(current_scaling_filter) {
            resource_update_required = true;
        }

        // Check if image count changed
        let old_count = self.image_count;
        self.image_count = current_swapchain_image_count;
        if old_count != current_swapchain_image_count {
            resource_update_required = true;
        }

        // Check if format or dimensions changed
        let old_format = self.swapchain_view_format;
        self.swapchain_view_format = current_swapchain_view_format;
        if old_format != current_swapchain_view_format
            || layout.width != frame.width
            || layout.height != frame.height
        {
            resource_update_required = true;
            presentation_recreate_required = true;
        }

        if resource_update_required {
            self.wait_idle(scheduler, present_manager);

            // Update window adapt pass
            self.set_window_adapt_pass(device);

            // Recreate frame if needed
            if presentation_recreate_required {
                log::debug!(
                    "BlitScreen target frame dimensions changed from {}x{} to {}x{}",
                    frame.width,
                    frame.height,
                    layout.width,
                    layout.height
                );
            }
        }

        if let Some(ref window_adapt) = self.window_adapt {
            let window_size = vk::Extent2D {
                width: layout.screen.get_width(),
                height: layout.screen.get_height(),
            };
            if self.layers.len() != framebuffers.len() {
                self.layers.clear();
            }
            while self.layers.len() < framebuffers.len() {
                self.layers.push(Layer::new(
                    device,
                    allocator,
                    scheduler,
                    device_memory,
                    self.image_count,
                    window_size,
                    window_adapt.get_descriptor_set_layout(),
                    self.filters,
                    device.is_float16_supported(),
                ));
            }
        }

        if let Some(ref window_adapt) = self.window_adapt {
            let render_area = vk::Extent2D {
                width: layout.width,
                height: layout.height,
            };
            let layer_count = framebuffers.len();
            let mut push_constants = vec![PresentPushConstants::default(); layer_count];
            let mut descriptor_sets = vec![vk::DescriptorSet::null(); layer_count];
            let mut blend_modes = Vec::with_capacity(layer_count);
            let sampler = window_adapt.get_sampler();

            for i in 0..layer_count {
                blend_modes.push(to_window_blend_mode(framebuffers[i].blending));
                self.layers[i].configure_draw_from_framebuffer(
                    device,
                    &mut push_constants[i],
                    &mut descriptor_sets[i],
                    rasterizer,
                    sampler,
                    self.image_index,
                    &framebuffers[i],
                    layout,
                );
            }

            window_adapt.draw(
                scheduler,
                &push_constants,
                &descriptor_sets,
                &blend_modes,
                frame.framebuffer,
                render_area,
            );
        }

        // Advance image index
        self.image_index += 1;
        if self.image_index >= self.image_count {
            self.image_index = 0;
        }
    }

    /// Port of `BlitScreen::CreateFramebuffer`.
    pub fn create_framebuffer(
        &mut self,
        device: &Device,
        scheduler: &mut Scheduler,
        present_manager: &PresentManager,
        image_view: vk::ImageView,
        current_view_format: vk::Format,
        layout_width: u32,
        layout_height: u32,
    ) -> vk::Framebuffer {
        let format_updated = self.swapchain_view_format != current_view_format;
        self.swapchain_view_format = current_view_format;

        if self.window_adapt.is_none()
            || self.scaling_filter != Some(self.current_scaling_filter())
            || format_updated
        {
            self.wait_idle(scheduler, present_manager);
            self.set_window_adapt_pass(device);
        }

        let render_pass = self
            .window_adapt
            .as_ref()
            .expect("window_adapt must be set")
            .get_render_pass();

        let extent = vk::Extent2D {
            width: layout_width,
            height: layout_height,
        };

        self.create_framebuffer_impl(image_view, extent, render_pass)
    }

    // --- Private ---

    /// Port of `BlitScreen::SetWindowAdaptPass`.
    fn set_window_adapt_pass(&mut self, device: &Device) {
        self.layers.clear();
        let filter = self.current_scaling_filter();
        self.scaling_filter = Some(filter);

        self.window_adapt = Some(match filter {
            ScalingFilter::NearestNeighbor => {
                filters::make_nearest_neighbor(device, self.swapchain_view_format)
            }
            ScalingFilter::Bicubic => filters::make_bicubic(
                device,
                self.swapchain_view_format,
                filters::CubicFilterWeights::CatmullRom,
            ),
            ScalingFilter::ZeroTangent => filters::make_bicubic(
                device,
                self.swapchain_view_format,
                filters::CubicFilterWeights::ZeroTangentCardinal,
            ),
            ScalingFilter::BSpline => filters::make_bicubic(
                device,
                self.swapchain_view_format,
                filters::CubicFilterWeights::BSpline,
            ),
            ScalingFilter::Mitchell => filters::make_bicubic(
                device,
                self.swapchain_view_format,
                filters::CubicFilterWeights::MitchellNetravali,
            ),
            ScalingFilter::Spline1 => filters::make_spline1(device, self.swapchain_view_format),
            ScalingFilter::Gaussian => filters::make_gaussian(device, self.swapchain_view_format),
            ScalingFilter::Lanczos => filters::make_lanczos(device, self.swapchain_view_format),
            ScalingFilter::ScaleForce => {
                filters::make_scale_force(device, self.swapchain_view_format)
            }
            ScalingFilter::Area => filters::make_area(device, self.swapchain_view_format),
            ScalingFilter::Mmpx => filters::make_mmpx(device, self.swapchain_view_format),
            ScalingFilter::Fsr
            | ScalingFilter::Sgsr
            | ScalingFilter::SgsrEdge
            | ScalingFilter::Bilinear => filters::make_bilinear(device, self.swapchain_view_format),
        });
    }

    fn current_scaling_filter(&self) -> ScalingFilter {
        (self.filters.get_scaling_filter)()
    }

    /// Port of `BlitScreen::WaitIdle`.
    fn wait_idle(&self, scheduler: &mut Scheduler, present_manager: &PresentManager) {
        present_manager.wait_present();
        scheduler.finish();
        unsafe {
            self.device
                .device_wait_idle()
                .expect("Failed to wait for Vulkan device idle");
        };
    }

    /// Port of `BlitScreen::CreateFramebuffer` (private overload).
    fn create_framebuffer_impl(
        &self,
        image_view: vk::ImageView,
        extent: vk::Extent2D,
        render_pass: vk::RenderPass,
    ) -> vk::Framebuffer {
        let attachments = [image_view];
        let fb_ci = vk::FramebufferCreateInfo::builder()
            .render_pass(render_pass)
            .attachments(&attachments)
            .width(extent.width)
            .height(extent.height)
            .layers(1)
            .build();

        unsafe {
            self.device
                .create_framebuffer(&fb_ci, None)
                .expect("Failed to create blit screen framebuffer")
        }
    }
}

fn to_window_blend_mode(mode: FramebufferBlendMode) -> BlendMode {
    match mode {
        FramebufferBlendMode::Opaque => BlendMode::Opaque,
        FramebufferBlendMode::Premultiplied => BlendMode::Premultiplied,
        FramebufferBlendMode::Coverage => BlendMode::Coverage,
    }
}
