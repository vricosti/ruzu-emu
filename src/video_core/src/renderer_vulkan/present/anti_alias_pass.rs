// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `present/anti_alias_pass.h`.
//!
//! Abstract anti-aliasing pass interface.

use ash::vk;

use crate::renderer_vulkan::scheduler::Scheduler;
use crate::vulkan_common::vulkan_device::Device;

// ---------------------------------------------------------------------------
// AntiAliasPass trait
// ---------------------------------------------------------------------------

/// Port of `AntiAliasPass` abstract class.
///
/// Interface for anti-aliasing passes that operate on a presentable image
/// in-place (swapping the image/view pointers if needed).
pub trait AntiAliasPass {
    /// Port of `AntiAliasPass::Draw`.
    fn draw(
        &mut self,
        device: &Device,
        scheduler: &mut Scheduler,
        image_index: usize,
        inout_image: &mut vk::Image,
        inout_image_view: &mut vk::ImageView,
    );
}
