// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of the `present/` subdirectory under `renderer_vulkan/`.
//!
//! Contains presentation-related passes: anti-aliasing, scaling filters,
//! layer management, and utility helpers for creating Vulkan objects
//! used during frame presentation.

pub mod anti_alias_pass;
pub mod filters;
pub mod fsr;
pub mod fsr3;
pub mod fxaa;
pub mod layer;
pub mod present_push_constants;
pub mod sgsr;
pub mod smaa;
pub mod util;
pub mod window_adapt_pass;
