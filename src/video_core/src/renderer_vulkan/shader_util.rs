// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `vk_shader_util.h` / `vk_shader_util.cpp`.
//!
//! Utility for building Vulkan shader modules from SPIR-V code.

use ash::vk;

/// Port of `BuildShader`.
///
/// Creates a `VkShaderModule` from SPIR-V `u32` words.
pub fn build_shader(device: &ash::Device, code: &[u32]) -> Result<vk::ShaderModule, vk::Result> {
    let create_info = vk::ShaderModuleCreateInfo::default().code(code);

    unsafe { device.create_shader_module(&create_info, None) }
}
