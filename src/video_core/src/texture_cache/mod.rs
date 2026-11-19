// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of video_core/texture_cache/
//!
//! The texture cache manages GPU image (texture and render-target) resources,
//! tracking their CPU/GPU memory mappings, format conversions, aliasing,
//! rescaling, and lifecycle.

pub mod accelerated_swizzle;
pub mod decode_bc;
pub mod descriptor_table;
pub mod format_lookup_table;
pub mod formatter;
pub mod image_base;
pub mod image_info;
pub mod image_view_base;
pub mod image_view_info;
pub mod render_targets;
pub mod samples_helper;
pub mod texture_cache;
pub mod texture_cache_base;
pub mod types;
pub mod converted_cache;
pub mod util;
