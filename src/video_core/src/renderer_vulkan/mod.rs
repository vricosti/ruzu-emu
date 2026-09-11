// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Vulkan renderer modules.
//!
//! Upstream `vk_rasterizer.h/.cpp` ownership lives in [`vk_rasterizer`].

pub mod blit_image;
pub mod blit_screen;
pub mod buffer_cache;
pub mod buffer_cache_base;
pub mod command_pool;
pub mod compute_pass;
pub mod compute_pipeline;
pub mod descriptor_buffer;
pub mod descriptor_pool;
pub mod fence_manager;
pub mod fixed_pipeline_state;
pub mod graphics_pipeline;
pub mod master_semaphore;
pub mod multi_range_buffer;
pub mod maxwell_to_vk;
pub mod pipeline_cache;
pub mod pipeline_helper;
pub mod pipeline_statistics;
pub mod present;
pub mod present_manager;
pub mod query_cache;
pub mod render_pass_cache;
pub mod renderer_vulkan;
pub mod resource_pool;
pub mod scheduler;
pub mod shader_util;
pub mod staging_buffer_pool;
pub mod state_tracker;
pub mod swapchain;
pub mod texture_cache;
pub mod texture_cache_base;
pub mod turbo_mode;
pub mod update_descriptor;
pub mod vk_rasterizer;

pub use vk_rasterizer::{RasterizerVulkan, RendererError};
