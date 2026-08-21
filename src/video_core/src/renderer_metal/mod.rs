// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native Metal renderer for macOS.
//!
//! Eden has no Metal backend. The modules here preserve Eden's renderer and
//! rasterizer ownership boundaries while implementing the backend operations
//! with Metal concepts rather than translating Vulkan objects mechanically.

pub mod metal_buffer;
pub mod metal_device;
pub mod metal_format;
pub mod metal_framebuffer;
pub mod metal_image;
pub mod metal_image_view;
pub mod metal_layer;
pub mod metal_pipeline_cache;
pub mod metal_presenter;
pub mod metal_sampler;
pub mod metal_scheduler;
pub mod metal_shader;
pub mod metal_staging_buffer_pool;
pub mod metal_texture_cache;
