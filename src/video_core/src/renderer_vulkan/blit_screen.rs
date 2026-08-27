// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `vk_blit_screen.h` / `vk_blit_screen.cpp`.
//!
//! Composites guest framebuffers into a presentable frame using layers
//! and a window adaptation pass.

use ash::vk;
use std::collections::LinkedList;
use std::sync::Arc;

use crate::framebuffer_config::FramebufferConfig;
use crate::host1x::gpu_device_memory_manager::MaxwellDeviceMemoryManager;
use crate::present::{PresentFilters, ScalingFilter};
use crate::vulkan_common::vulkan_device::Device;
use crate::vulkan_common::vulkan_memory_allocator::MemoryAllocator;

use super::present::filters;
use super::present::layer::Layer;
use super::present::window_adapt_pass::WindowAdaptPass;
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

// ---------------------------------------------------------------------------
// BlitScreen
// ---------------------------------------------------------------------------

/// Port of `BlitScreen` class.
///
/// Manages layers, anti-aliasing, scaling, and the window adaptation pass
/// to composite guest framebuffers into a presentable output frame.
pub struct BlitScreen {
    image_count: usize,
    image_index: usize,
    swapchain_view_format: vk::Format,
    scaling_filter: ScalingFilter,
    window_adapt: Option<WindowAdaptPass>,
    layers: LinkedList<Layer>,
    filters: &'static PresentFilters,
}

impl BlitScreen {
    /// Port of `BlitScreen::BlitScreen`.
    pub fn new(filters: &'static PresentFilters) -> Self {
        BlitScreen {
            image_count: 1,
            image_index: 0,
            swapchain_view_format: vk::Format::B8G8R8A8_UNORM,
            scaling_filter: ScalingFilter::NearestNeighbor,
            window_adapt: None,
            layers: LinkedList::new(),
            filters,
        }
    }

    /// Borrow-safe `DrawToFrame` entry point for a frame owned by `PresentManager`.
    #[allow(clippy::too_many_arguments)]
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

        if self.window_adapt.is_none() || self.scaling_filter != current_scaling_filter {
            resource_update_required = true;
        }

        if self.image_count != current_swapchain_image_count {
            resource_update_required = true;
            self.image_count = current_swapchain_image_count;
        }

        let frame_width = present_manager.frame(frame_index).width;
        let frame_height = present_manager.frame(frame_index).height;
        if self.swapchain_view_format != current_swapchain_view_format
            || layout.width != frame_width
            || layout.height != frame_height
        {
            resource_update_required = true;
            presentation_recreate_required = true;
            self.swapchain_view_format = current_swapchain_view_format;
        }

        if resource_update_required {
            self.wait_idle(device, scheduler, present_manager);
            self.set_window_adapt_pass(device);

            if presentation_recreate_required {
                present_manager.recreate_frame_by_index(
                    frame_index,
                    layout.width,
                    layout.height,
                    self.swapchain_view_format,
                    self.window_adapt
                        .as_ref()
                        .expect("window_adapt must be set")
                        .get_render_pass(),
                );
            }

            self.image_index = 0;
        }

        self.draw_layers(
            device,
            rasterizer,
            scheduler,
            allocator,
            device_memory,
            present_manager.frame(frame_index),
            framebuffers,
            layout,
        );
    }

    /// Port of `BlitScreen::DrawToFrame`.
    ///
    /// Draws the guest framebuffers into the given presentation frame,
    /// recreating resources as needed when the swapchain format/size changes.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_to_frame(
        &mut self,
        device: &Device,
        rasterizer: &mut RasterizerVulkan,
        scheduler: &mut Scheduler,
        present_manager: &PresentManager,
        allocator: &MemoryAllocator,
        device_memory: &Arc<MaxwellDeviceMemoryManager>,
        frame: &mut Frame,
        framebuffers: &[FramebufferConfig],
        layout: &FramebufferLayout,
        current_swapchain_image_count: usize,
        current_swapchain_view_format: vk::Format,
    ) {
        let mut resource_update_required = false;
        let mut presentation_recreate_required = false;
        let current_scaling_filter = self.current_scaling_filter();

        if self.window_adapt.is_none() || self.scaling_filter != current_scaling_filter {
            resource_update_required = true;
        }

        if self.image_count != current_swapchain_image_count {
            resource_update_required = true;
            self.image_count = current_swapchain_image_count;
        }

        if self.swapchain_view_format != current_swapchain_view_format
            || layout.width != frame.width
            || layout.height != frame.height
        {
            resource_update_required = true;
            presentation_recreate_required = true;
            self.swapchain_view_format = current_swapchain_view_format;
        }

        if resource_update_required {
            self.wait_idle(device, scheduler, present_manager);
            self.set_window_adapt_pass(device);

            if presentation_recreate_required {
                present_manager.recreate_frame(
                    frame,
                    layout.width,
                    layout.height,
                    self.swapchain_view_format,
                    self.window_adapt
                        .as_ref()
                        .expect("window_adapt must be set")
                        .get_render_pass(),
                );
            }

            self.image_index = 0;
        }

        self.draw_layers(
            device,
            rasterizer,
            scheduler,
            allocator,
            device_memory,
            frame,
            framebuffers,
            layout,
        );
    }

    /// Port of `BlitScreen::PrepareFrame`.
    pub fn prepare_frame(
        &mut self,
        device: &Device,
        scheduler: &mut Scheduler,
        present_manager: &PresentManager,
        frame: &mut Frame,
        layout: &FramebufferLayout,
    ) {
        let Some(window_adapt) = self.window_adapt.as_ref() else {
            return;
        };
        if frame.width == layout.width && frame.height == layout.height {
            return;
        }

        let render_pass = window_adapt.get_render_pass();
        self.wait_idle(device, scheduler, present_manager);
        present_manager.recreate_frame(
            frame,
            layout.width,
            layout.height,
            self.swapchain_view_format,
            render_pass,
        );
    }

    /// Port of `BlitScreen::CreateFramebuffer`.
    pub fn create_framebuffer(
        &mut self,
        device: &Device,
        scheduler: &mut Scheduler,
        present_manager: &PresentManager,
        layout: &FramebufferLayout,
        image_view: vk::ImageView,
        current_view_format: vk::Format,
    ) -> vk::Framebuffer {
        let format_updated = self.swapchain_view_format != current_view_format;
        self.swapchain_view_format = current_view_format;

        if self.window_adapt.is_none()
            || self.scaling_filter != self.current_scaling_filter()
            || format_updated
        {
            self.wait_idle(device, scheduler, present_manager);
            self.set_window_adapt_pass(device);
            self.image_index = 0;
        }

        let render_pass = self
            .window_adapt
            .as_ref()
            .expect("window_adapt must be set")
            .get_render_pass();

        let extent = vk::Extent2D {
            width: layout.width,
            height: layout.height,
        };

        self.create_framebuffer_impl(device, image_view, extent, render_pass)
    }

    // --- Private ---

    /// Mechanical split of the post-resource-update portion of Eden's
    /// `DrawToFrame`, required for frames stored inside `PresentManager`.
    #[allow(clippy::too_many_arguments)]
    fn draw_layers(
        &mut self,
        device: &Device,
        rasterizer: &mut RasterizerVulkan,
        scheduler: &mut Scheduler,
        allocator: &MemoryAllocator,
        device_memory: &Arc<MaxwellDeviceMemoryManager>,
        frame: &Frame,
        framebuffers: &[FramebufferConfig],
        layout: &FramebufferLayout,
    ) {
        let window_adapt = self
            .window_adapt
            .as_ref()
            .expect("window_adapt must be set before drawing");
        let window_size = vk::Extent2D {
            width: layout.screen.get_width(),
            height: layout.screen.get_height(),
        };

        if self.layers.len() != framebuffers.len() {
            self.layers.clear();
            for _ in 0..framebuffers.len() {
                self.layers.push_back(Layer::new(
                    device,
                    allocator,
                    scheduler,
                    device_memory,
                    self.image_count,
                    window_size,
                    window_adapt.get_descriptor_set_layout(),
                    self.filters,
                ));
            }
        }

        window_adapt.draw(
            device,
            rasterizer,
            scheduler,
            self.image_index,
            &mut self.layers,
            framebuffers,
            layout,
            frame,
        );

        self.image_index += 1;
        if self.image_index >= self.image_count {
            self.image_index = 0;
        }
    }

    /// Port of `BlitScreen::SetWindowAdaptPass`.
    fn set_window_adapt_pass(&mut self, device: &Device) {
        self.layers.clear();
        let filter = self.current_scaling_filter();
        self.scaling_filter = filter;

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
    fn wait_idle(
        &self,
        device: &Device,
        scheduler: &mut Scheduler,
        present_manager: &PresentManager,
    ) {
        present_manager.wait_present();
        scheduler.finish();
        unsafe {
            device
                .get_logical()
                .device_wait_idle()
                .expect("Failed to wait for Vulkan device idle");
        };
    }

    /// Port of `BlitScreen::CreateFramebuffer` (private overload).
    fn create_framebuffer_impl(
        &self,
        device: &Device,
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
            device
                .get_logical()
                .create_framebuffer(&fb_ci, None)
                .expect("Failed to create blit screen framebuffer")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::present::AntiAliasing;

    fn nearest_neighbor() -> ScalingFilter {
        ScalingFilter::NearestNeighbor
    }

    fn no_anti_aliasing() -> AntiAliasing {
        AntiAliasing::None
    }

    static FILTERS: PresentFilters = PresentFilters {
        get_scaling_filter: nearest_neighbor,
        get_anti_aliasing: no_anti_aliasing,
    };

    #[test]
    fn constructor_matches_upstream_presentation_defaults() {
        let screen = BlitScreen::new(&FILTERS);
        assert_eq!(screen.image_count, 1);
        assert_eq!(screen.image_index, 0);
        assert_eq!(screen.swapchain_view_format, vk::Format::B8G8R8A8_UNORM);
        assert_eq!(screen.scaling_filter, ScalingFilter::NearestNeighbor);
        assert!(screen.window_adapt.is_none());
        assert!(screen.layers.is_empty());
    }
}
