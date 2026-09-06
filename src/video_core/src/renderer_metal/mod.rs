// SPDX-FileCopyrightText: 2026 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native Metal renderer for macOS.
//!
//! Eden has no Metal backend. The modules here preserve Eden's renderer and
//! rasterizer ownership boundaries while implementing the backend operations
//! with Metal concepts rather than translating Vulkan objects mechanically.

pub mod metal_blit_helper;
pub mod metal_buffer;
pub mod metal_buffer_cache;
pub mod metal_compute_pass;
pub mod metal_compute_pipeline;
pub mod metal_device;
pub mod metal_fence_manager;
pub mod metal_format;
pub mod metal_framebuffer;
pub mod metal_geometry_pipeline;
pub mod metal_geometry_capture;
pub mod metal_graphics_pipeline;
pub mod metal_image;
pub mod metal_image_view;
pub mod metal_layer;
pub mod metal_pipeline_cache;
pub mod metal_presenter;
pub mod metal_primitive_assembler;
pub mod metal_query_cache;
pub mod metal_rasterizer;
pub mod metal_sampler;
pub mod metal_scheduler;
mod metal_gpu_profiler;
pub mod metal_shader;
pub mod metal_staging_buffer_pool;
pub mod metal_state_tracker;
pub mod metal_texture_cache;
pub mod metal_update_descriptor;
pub mod metal_vertex_pulling;
pub mod renderer_metal;
