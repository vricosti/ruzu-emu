// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `vk_swapchain.h` / `vk_swapchain.cpp`.
//!
//! Vulkan swapchain management -- creation, image acquisition, presentation,
//! and recreation.

use ash::vk;
use std::sync::{Arc, Mutex};

use crate::renderer_vulkan::scheduler::Scheduler;
use crate::vulkan_common::vulkan_device::Device;
use crate::vulkan_common::vulkan_wrapper::VulkanError;

// ---------------------------------------------------------------------------
// VSyncMode (matching upstream Settings::VSyncMode)
// ---------------------------------------------------------------------------

/// Port of `Settings::VSyncMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum VSyncMode {
    Immediate = 0,
    Mailbox = 1,
    Fifo = 2,
    FifoRelaxed = 3,
}

// ---------------------------------------------------------------------------
// Anonymous namespace helpers
// ---------------------------------------------------------------------------

/// Port of `ChooseSwapSurfaceFormat`.
fn choose_swap_surface_format(formats: &[vk::SurfaceFormatKHR]) -> vk::SurfaceFormatKHR {
    if formats.len() == 1 && formats[0].format == vk::Format::UNDEFINED {
        return vk::SurfaceFormatKHR {
            format: vk::Format::B8G8R8A8_UNORM,
            color_space: vk::ColorSpaceKHR::SRGB_NONLINEAR,
        };
    }
    formats
        .iter()
        .find(|f| {
            f.format == vk::Format::B8G8R8A8_UNORM
                && f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR
        })
        .copied()
        .unwrap_or(formats[0])
}

/// Port of `ChooseSwapPresentMode`.
fn choose_swap_present_mode(
    has_imm: bool,
    has_mailbox: bool,
    has_fifo_relaxed: bool,
    vsync_mode: VSyncMode,
    use_speed_limit: bool,
    is_turbo: bool,
) -> vk::PresentModeKHR {
    let setting = if !use_speed_limit || is_turbo {
        match vsync_mode {
            VSyncMode::Fifo | VSyncMode::FifoRelaxed => {
                if has_mailbox {
                    VSyncMode::Mailbox
                } else if has_imm {
                    VSyncMode::Immediate
                } else {
                    vsync_mode
                }
            }
            other => other,
        }
    } else {
        vsync_mode
    };

    // Eden falls back from Immediate to Mailbox first, then to FIFO only if
    // Mailbox is unavailable as well.
    let mut validated = setting;
    if validated == VSyncMode::Immediate && !has_imm {
        validated = VSyncMode::Mailbox;
    }
    if (validated == VSyncMode::Mailbox && !has_mailbox)
        || (validated == VSyncMode::FifoRelaxed && !has_fifo_relaxed)
    {
        validated = VSyncMode::Fifo;
    }

    match validated {
        VSyncMode::Immediate => vk::PresentModeKHR::IMMEDIATE,
        VSyncMode::Mailbox => vk::PresentModeKHR::MAILBOX,
        VSyncMode::Fifo => vk::PresentModeKHR::FIFO,
        VSyncMode::FifoRelaxed => vk::PresentModeKHR::FIFO_RELAXED,
    }
}

/// Port of upstream `ChooseSwapPresentMode`'s settings lookup.
fn requested_swap_present_mode(
    has_imm: bool,
    has_mailbox: bool,
    has_fifo_relaxed: bool,
) -> vk::PresentModeKHR {
    let settings = common::settings::values();
    let vsync_mode = match *settings.vsync_mode.get_value() {
        common::settings_enums::VSyncMode::Immediate => VSyncMode::Immediate,
        common::settings_enums::VSyncMode::Mailbox => VSyncMode::Mailbox,
        common::settings_enums::VSyncMode::Fifo => VSyncMode::Fifo,
        common::settings_enums::VSyncMode::FifoRelaxed => VSyncMode::FifoRelaxed,
    };
    choose_swap_present_mode(
        has_imm,
        has_mailbox,
        has_fifo_relaxed,
        vsync_mode,
        *settings.use_speed_limit.get_value(),
        *settings.current_speed_mode.get_value() == common::settings_enums::SpeedMode::Turbo,
    )
}

/// Port of `ChooseSwapExtent`.
fn choose_swap_extent(
    capabilities: &vk::SurfaceCapabilitiesKHR,
    width: u32,
    height: u32,
) -> vk::Extent2D {
    if capabilities.current_extent.width != u32::MAX {
        return capabilities.current_extent;
    }
    vk::Extent2D {
        width: width
            .max(capabilities.min_image_extent.width)
            .min(capabilities.max_image_extent.width),
        height: height
            .max(capabilities.min_image_extent.height)
            .min(capabilities.max_image_extent.height),
    }
}

/// Port of `ChooseAlphaFlags`.
fn choose_alpha_flags(capabilities: &vk::SurfaceCapabilitiesKHR) -> vk::CompositeAlphaFlagsKHR {
    if capabilities
        .supported_composite_alpha
        .contains(vk::CompositeAlphaFlagsKHR::OPAQUE)
    {
        vk::CompositeAlphaFlagsKHR::OPAQUE
    } else if capabilities
        .supported_composite_alpha
        .contains(vk::CompositeAlphaFlagsKHR::INHERIT)
    {
        vk::CompositeAlphaFlagsKHR::INHERIT
    } else {
        log::error!(
            "Unknown composite alpha flags value {:#x}",
            capabilities.supported_composite_alpha.as_raw()
        );
        vk::CompositeAlphaFlagsKHR::OPAQUE
    }
}

// ---------------------------------------------------------------------------
// Swapchain
// ---------------------------------------------------------------------------

/// Port of `Swapchain` class.
pub struct Swapchain {
    surface: vk::SurfaceKHR,
    surface_loader: ash::extensions::khr::Surface,
    swapchain_loader: ash::extensions::khr::Swapchain,
    physical_device: vk::PhysicalDevice,
    device: ash::Device,
    present_queue: vk::Queue,
    submit_mutex: Arc<Mutex<()>>,
    graphics_family: u32,
    present_family: u32,
    mutable_format_enabled: bool,
    swapchain: vk::SwapchainKHR,

    image_count: usize,
    images: Vec<vk::Image>,
    resource_ticks: Vec<u64>,
    present_semaphores: Vec<vk::Semaphore>,
    render_semaphores: Vec<vk::Semaphore>,

    width: u32,
    height: u32,
    image_index: u32,
    frame_index: u32,

    image_view_format: vk::Format,
    extent: vk::Extent2D,
    present_mode: vk::PresentModeKHR,
    surface_format: vk::SurfaceFormatKHR,

    has_imm: bool,
    has_mailbox: bool,
    has_fifo_relaxed: bool,

    is_outdated: bool,
    is_suboptimal: bool,
}

impl Swapchain {
    /// Port of `Swapchain::Swapchain`.
    pub fn new(
        instance: &ash::Instance,
        surface_loader: ash::extensions::khr::Surface,
        surface: vk::SurfaceKHR,
        device: &Device,
        submit_mutex: Arc<Mutex<()>>,
        width: u32,
        height: u32,
    ) -> Result<Self, VulkanError> {
        let swapchain_loader = ash::extensions::khr::Swapchain::new(instance, device.get_logical());
        let mut swapchain = Swapchain {
            surface,
            surface_loader,
            swapchain_loader,
            physical_device: device.get_physical(),
            device: device.get_logical().clone(),
            present_queue: device.get_present_queue(),
            submit_mutex,
            graphics_family: device.get_graphics_family(),
            present_family: device.get_present_family(),
            mutable_format_enabled: device.is_khr_swapchain_mutable_format_enabled(),
            swapchain: vk::SwapchainKHR::null(),
            image_count: 0,
            images: Vec::new(),
            resource_ticks: Vec::new(),
            present_semaphores: Vec::new(),
            render_semaphores: Vec::new(),
            width,
            height,
            image_index: 0,
            frame_index: 0,
            image_view_format: vk::Format::UNDEFINED,
            extent: vk::Extent2D::default(),
            present_mode: vk::PresentModeKHR::FIFO,
            surface_format: vk::SurfaceFormatKHR::default(),
            has_imm: false,
            has_mailbox: false,
            has_fifo_relaxed: false,
            is_outdated: false,
            is_suboptimal: false,
        };
        swapchain.create(surface, width, height)?;
        Ok(swapchain)
    }

    /// Port of `Swapchain::Create`.
    pub fn create(
        &mut self,
        surface: vk::SurfaceKHR,
        width: u32,
        height: u32,
    ) -> Result<(), VulkanError> {
        self.is_outdated = false;
        self.is_suboptimal = false;
        self.width = width;
        self.height = height;
        self.surface = surface;

        let capabilities = unsafe {
            self.surface_loader
                .get_physical_device_surface_capabilities(self.physical_device, self.surface)
                .map_err(VulkanError::new)?
        };
        if capabilities.max_image_extent.width == 0 || capabilities.max_image_extent.height == 0 {
            return Ok(());
        }

        // MoltenVK can complete presentation asynchronously on an IOGPU thread
        // after the present call returns. Keep the macOS workaround that drains
        // those callbacks before destroying the old swapchain. Other platforms
        // follow upstream `Swapchain::Create` and recreate directly; a global
        // device wait here makes consecutive resize/out-of-date recreations
        // visibly stall presentation.
        #[cfg(target_os = "macos")]
        unsafe {
            let _ = self.device.device_wait_idle();
        }

        self.destroy();
        self.create_swapchain(&capabilities)?;
        self.create_semaphores()?;
        self.resource_ticks.clear();
        self.resource_ticks.resize(self.image_count, 0);
        Ok(())
    }

    /// Port of `Swapchain::AcquireNextImage`.
    ///
    /// Returns `true` if the swapchain is suboptimal or outdated.
    /// Surface loss is returned to `PresentManager`, which owns the upstream
    /// surface-recreation lifecycle.
    pub fn acquire_next_image(
        &mut self,
        scheduler: Option<&mut Scheduler>,
    ) -> Result<bool, vk::Result> {
        if self.swapchain == vk::SwapchainKHR::null() || self.present_semaphores.is_empty() {
            self.is_outdated = true;
            return Ok(true);
        }
        let semaphore = self.present_semaphores[self.frame_index as usize];
        match unsafe {
            self.swapchain_loader.acquire_next_image(
                self.swapchain,
                u64::MAX,
                semaphore,
                vk::Fence::null(),
            )
        } {
            Ok((index, suboptimal)) => {
                self.image_index = index;
                self.is_suboptimal |= suboptimal;
            }
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                self.is_outdated = true;
            }
            Err(vk::Result::ERROR_SURFACE_LOST_KHR) => {
                return Err(vk::Result::ERROR_SURFACE_LOST_KHR);
            }
            Err(result) => {
                log::error!("vkAcquireNextImageKHR returned {:?}", result);
            }
        }
        if let Some(tick) = self.resource_ticks.get_mut(self.image_index as usize) {
            match scheduler {
                Some(scheduler) => {
                    use common::settings_enums::FramePacingMode;
                    let target_fps = match *common::settings::values().frame_pacing_mode.get_value()
                    {
                        FramePacingMode::Target_Auto => 0.0,
                        FramePacingMode::Target_30 => 30.0,
                        FramePacingMode::Target_60 => 60.0,
                        FramePacingMode::Target_90 => 90.0,
                        FramePacingMode::Target_120 => 120.0,
                    };
                    scheduler.wait_with_frame_pacing(*tick, target_fps);
                    *tick = scheduler.current_tick();
                }
                // Present-thread path: the scheduler belongs to the GPU thread
                // and `Scheduler::wait` may record/submit, so it cannot be
                // called here. Image reuse is already ordered by the
                // presentation engine (an image is only re-acquired after its
                // previous present completed, which waited on the blit's
                // semaphore), and frame readiness by `render_ready`.
                None => {
                    *tick = tick.saturating_add(1);
                }
            }
        }
        Ok(self.is_suboptimal || self.is_outdated)
    }

    /// Port of `Swapchain::Present`.
    pub fn present(&mut self, render_semaphore: vk::Semaphore) -> Result<(), vk::Result> {
        if self.swapchain == vk::SwapchainKHR::null() {
            self.is_outdated = true;
            return Ok(());
        }
        let wait_semaphores = [render_semaphore];
        let swapchains = [self.swapchain];
        let image_indices = [self.image_index];
        let present_info = vk::PresentInfoKHR::builder()
            .wait_semaphores(if render_semaphore == vk::Semaphore::null() {
                &[]
            } else {
                &wait_semaphores
            })
            .swapchains(&swapchains)
            .image_indices(&image_indices)
            .build();
        let _submit_lock = self.submit_mutex.lock().unwrap();
        match unsafe {
            self.swapchain_loader
                .queue_present(self.present_queue, &present_info)
        } {
            Ok(suboptimal) => {
                self.is_suboptimal |= suboptimal;
                if suboptimal {
                    log::debug!("Suboptimal swapchain");
                }
            }
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                self.is_outdated = true;
            }
            Err(vk::Result::ERROR_SURFACE_LOST_KHR) => {
                return Err(vk::Result::ERROR_SURFACE_LOST_KHR);
            }
            Err(result) => {
                log::error!("Failed to present with error {:?}", result);
            }
        }
        self.frame_index += 1;
        if self.frame_index as usize >= self.image_count {
            self.frame_index = 0;
        }
        Ok(())
    }

    /// Port of `Swapchain::NeedsRecreation`.
    pub fn needs_recreation(&self) -> bool {
        self.is_suboptimal() || self.needs_present_mode_update()
    }

    /// Port of `Swapchain::IsOutDated`.
    pub fn is_outdated(&self) -> bool {
        self.is_outdated
    }

    /// Port of `Swapchain::IsSubOptimal`.
    pub fn is_suboptimal(&self) -> bool {
        self.is_suboptimal
    }

    /// Port of `Swapchain::GetSize`.
    pub fn get_size(&self) -> vk::Extent2D {
        self.extent
    }

    /// Port of `Swapchain::GetImageCount`.
    pub fn get_image_count(&self) -> usize {
        self.image_count
    }

    /// Port of `Swapchain::GetImageIndex`.
    pub fn get_image_index(&self) -> usize {
        self.image_index as usize
    }

    /// Port of `Swapchain::GetFrameIndex`.
    pub fn get_frame_index(&self) -> usize {
        self.frame_index as usize
    }

    /// Port of `Swapchain::GetImageIndex(index)`.
    pub fn get_image_at(&self, index: usize) -> vk::Image {
        self.images[index]
    }

    /// Port of `Swapchain::CurrentImage`.
    pub fn current_image(&self) -> vk::Image {
        self.images[self.image_index as usize]
    }

    /// Port of `Swapchain::GetImageViewFormat`.
    pub fn get_image_view_format(&self) -> vk::Format {
        self.image_view_format
    }

    /// Port of `Swapchain::GetImageFormat`.
    pub fn get_image_format(&self) -> vk::Format {
        self.surface_format.format
    }

    /// Port of `Swapchain::CurrentPresentSemaphore`.
    pub fn current_present_semaphore(&self) -> vk::Semaphore {
        self.present_semaphores[self.frame_index as usize]
    }

    /// Port of `Swapchain::CurrentRenderSemaphore`.
    pub fn current_render_semaphore(&self) -> vk::Semaphore {
        self.render_semaphores[self.frame_index as usize]
    }

    /// Port of `Swapchain::GetWidth`.
    pub fn get_width(&self) -> u32 {
        self.width
    }

    /// Port of `Swapchain::GetHeight`.
    pub fn get_height(&self) -> u32 {
        self.height
    }

    /// Port of `Swapchain::GetExtent`.
    pub fn get_extent(&self) -> vk::Extent2D {
        self.extent
    }

    /// Returns the platform-reported current surface extent when it is fixed.
    ///
    /// Upstream obtains this through `ChooseSwapExtent` during swapchain
    /// creation. ruzu also needs the value before drawing on macOS, where SDL
    /// resize events can lag behind MoltenVK's surface extent during live
    /// resize/maximize.
    pub fn current_surface_extent(&self) -> Option<vk::Extent2D> {
        let capabilities = unsafe {
            self.surface_loader
                .get_physical_device_surface_capabilities(self.physical_device, self.surface)
                .ok()?
        };
        if capabilities.current_extent.width == u32::MAX
            || capabilities.current_extent.height == u32::MAX
            || capabilities.current_extent.width == 0
            || capabilities.current_extent.height == 0
        {
            return None;
        }
        Some(capabilities.current_extent)
    }

    // --- Private ---

    /// Port of `Swapchain::CreateSwapchain`.
    ///
    /// Creates the Vulkan swapchain with the given surface capabilities.
    fn create_swapchain(
        &mut self,
        capabilities: &vk::SurfaceCapabilitiesKHR,
    ) -> Result<(), VulkanError> {
        let available_formats = unsafe {
            self.surface_loader
                .get_physical_device_surface_formats(self.physical_device, self.surface)
                .map_err(VulkanError::new)?
        };
        let available_present_modes = unsafe {
            self.surface_loader
                .get_physical_device_surface_present_modes(self.physical_device, self.surface)
                .map_err(VulkanError::new)?
        };
        self.has_mailbox = available_present_modes.contains(&vk::PresentModeKHR::MAILBOX);
        self.has_imm = available_present_modes.contains(&vk::PresentModeKHR::IMMEDIATE);
        self.has_fifo_relaxed = available_present_modes.contains(&vk::PresentModeKHR::FIFO_RELAXED);

        let alpha_flags = choose_alpha_flags(capabilities);
        self.surface_format = choose_swap_surface_format(&available_formats);
        self.present_mode =
            requested_swap_present_mode(self.has_imm, self.has_mailbox, self.has_fifo_relaxed);

        // Request triple buffering if possible
        let mut requested_image_count = capabilities.min_image_count + 1;
        if capabilities.max_image_count > 0 {
            if requested_image_count > capabilities.max_image_count {
                requested_image_count = capabilities.max_image_count;
            } else {
                requested_image_count =
                    requested_image_count.max(3u32.min(capabilities.max_image_count));
            }
        } else {
            requested_image_count = requested_image_count.max(3);
        }

        let updated_capabilities = unsafe {
            self.surface_loader
                .get_physical_device_surface_capabilities(self.physical_device, self.surface)
                .map_err(VulkanError::new)?
        };
        self.extent = choose_swap_extent(&updated_capabilities, self.width, self.height);

        let queue_indices = [self.graphics_family, self.present_family];
        let mut create_info = vk::SwapchainCreateInfoKHR::builder()
            .surface(self.surface)
            .min_image_count(requested_image_count)
            .image_format(self.surface_format.format)
            .image_color_space(self.surface_format.color_space)
            .image_extent(self.extent)
            .image_array_layers(1)
            .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_DST)
            .pre_transform(updated_capabilities.current_transform)
            .composite_alpha(alpha_flags)
            .present_mode(self.present_mode)
            .clipped(false);
        if self.graphics_family != self.present_family {
            create_info = create_info
                .image_sharing_mode(vk::SharingMode::CONCURRENT)
                .queue_family_indices(&queue_indices);
        } else {
            create_info = create_info.image_sharing_mode(vk::SharingMode::EXCLUSIVE);
        }

        #[cfg(not(target_os = "android"))]
        let view_formats = [
            self.surface_format.format,
            vk::Format::B8G8R8A8_UNORM,
            vk::Format::B8G8R8A8_SRGB,
        ];
        #[cfg(target_os = "android")]
        let view_formats = [
            self.surface_format.format,
            vk::Format::B8G8R8A8_UNORM,
            vk::Format::B8G8R8A8_SRGB,
            vk::Format::R8G8B8A8_UNORM,
            vk::Format::R8G8B8A8_SRGB,
        ];
        let mut format_list = vk::ImageFormatListCreateInfo::builder()
            .view_formats(&view_formats)
            .build();
        let mut create_info = create_info.build();
        if self.mutable_format_enabled {
            create_info.flags |= vk::SwapchainCreateFlagsKHR::MUTABLE_FORMAT;
            create_info.p_next = (&mut format_list as *mut vk::ImageFormatListCreateInfo).cast();
        }

        self.swapchain = unsafe {
            self.swapchain_loader
                .create_swapchain(&create_info, None)
                .map_err(VulkanError::new)?
        };
        self.images = unsafe {
            self.swapchain_loader
                .get_swapchain_images(self.swapchain)
                .map_err(VulkanError::new)?
        };
        self.image_count = self.images.len();
        self.image_view_format = vk::Format::B8G8R8A8_UNORM;
        log::info!(
            "Vulkan swapchain created: {} images, {}x{}, format {:?}, present mode {:?}",
            self.image_count,
            self.extent.width,
            self.extent.height,
            self.surface_format.format,
            self.present_mode
        );
        Ok(())
    }

    /// Port of `Swapchain::CreateSemaphores`.
    fn create_semaphores(&mut self) -> Result<(), VulkanError> {
        self.present_semaphores.clear();
        self.render_semaphores.clear();
        let semaphore_ci = vk::SemaphoreCreateInfo::builder().build();
        for _i in 0..self.image_count {
            let present = unsafe {
                self.device
                    .create_semaphore(&semaphore_ci, None)
                    .map_err(VulkanError::new)?
            };
            let render = unsafe {
                self.device
                    .create_semaphore(&semaphore_ci, None)
                    .map_err(VulkanError::new)?
            };
            self.present_semaphores.push(present);
            self.render_semaphores.push(render);
        }
        Ok(())
    }

    /// Port of `Swapchain::Destroy`.
    fn destroy(&mut self) {
        self.frame_index = 0;
        unsafe {
            for semaphore in self.present_semaphores.drain(..) {
                self.device.destroy_semaphore(semaphore, None);
            }
            for semaphore in self.render_semaphores.drain(..) {
                self.device.destroy_semaphore(semaphore, None);
            }
            if self.swapchain != vk::SwapchainKHR::null() {
                self.swapchain_loader
                    .destroy_swapchain(self.swapchain, None);
                self.swapchain = vk::SwapchainKHR::null();
            }
        }
    }

    /// Port of `Swapchain::NeedsPresentModeUpdate`.
    fn needs_present_mode_update(&self) -> bool {
        let requested =
            requested_swap_present_mode(self.has_imm, self.has_mailbox, self.has_fifo_relaxed);
        self.present_mode != requested
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turbo_unlocks_fifo_present_mode_like_eden() {
        assert_eq!(
            choose_swap_present_mode(false, true, false, VSyncMode::Fifo, true, true),
            vk::PresentModeKHR::MAILBOX
        );
        assert_eq!(
            choose_swap_present_mode(false, true, false, VSyncMode::Fifo, true, false),
            vk::PresentModeKHR::FIFO
        );
    }

    #[test]
    fn unsupported_immediate_falls_back_through_mailbox() {
        assert_eq!(
            choose_swap_present_mode(false, true, false, VSyncMode::Immediate, false, false),
            vk::PresentModeKHR::MAILBOX
        );
        assert_eq!(
            choose_swap_present_mode(false, false, false, VSyncMode::Immediate, false, false),
            vk::PresentModeKHR::FIFO
        );
    }
}

impl Drop for Swapchain {
    fn drop(&mut self) {
        self.destroy();
    }
}
