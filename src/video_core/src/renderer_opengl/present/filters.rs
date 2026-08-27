// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden's video_core/renderer_opengl/present/filters.h and filters.cpp
//!
//! Factory functions for creating window adapt passes with different scaling filters.
//! Each filter creates a WindowAdaptPass configured with the appropriate sampler
//! (nearest or bilinear) and fragment shader source.

use super::util;
use super::window_adapt_pass::WindowAdaptPass;
use crate::host_shaders::fragment_shaders::{
    OPENGL_PRESENT_FRAG, OPENGL_PRESENT_SCALEFORCE_FRAG, PRESENT_AREA_FRAG, PRESENT_BICUBIC_FRAG,
    PRESENT_BSPLINE_FRAG, PRESENT_GAUSSIAN_FRAG, PRESENT_LANCZOS_FRAG, PRESENT_MITCHELL_FRAG,
    PRESENT_MMPX_FRAG, PRESENT_SPLINE1_FRAG, PRESENT_ZERO_TANGENT_FRAG,
};
use crate::renderer_opengl::Device;

// ---------------------------------------------------------------------------
// Factory functions
// ---------------------------------------------------------------------------

/// Create a nearest-neighbor scaling filter pass.
///
/// Port of `OpenGL::MakeNearestNeighbor()`.
pub fn make_nearest_neighbor(device: *const Device) -> WindowAdaptPass {
    let sampler = util::create_nearest_neighbor_sampler();
    WindowAdaptPass::new(device, sampler, OPENGL_PRESENT_FRAG)
}

/// Create a bilinear scaling filter pass.
///
/// Port of `OpenGL::MakeBilinear()`.
pub fn make_bilinear(device: *const Device) -> WindowAdaptPass {
    let sampler = util::create_bilinear_sampler();
    WindowAdaptPass::new(device, sampler, OPENGL_PRESENT_FRAG)
}

/// Port of `OpenGL::MakeSpline1()`.
pub fn make_spline1(device: *const Device) -> WindowAdaptPass {
    let sampler = util::create_bilinear_sampler();
    WindowAdaptPass::new(device, sampler, PRESENT_SPLINE1_FRAG)
}

/// Create a bicubic scaling filter pass.
///
/// Port of `OpenGL::MakeBicubic()`.
pub fn make_bicubic(device: *const Device) -> WindowAdaptPass {
    let sampler = util::create_bilinear_sampler();
    WindowAdaptPass::new(device, sampler, PRESENT_BICUBIC_FRAG)
}

/// Port of `OpenGL::MakeMitchell()`.
pub fn make_mitchell(device: *const Device) -> WindowAdaptPass {
    let sampler = util::create_bilinear_sampler();
    WindowAdaptPass::new(device, sampler, PRESENT_MITCHELL_FRAG)
}

/// Port of `OpenGL::MakeZeroTangent()`.
pub fn make_zero_tangent(device: *const Device) -> WindowAdaptPass {
    let sampler = util::create_bilinear_sampler();
    WindowAdaptPass::new(device, sampler, PRESENT_ZERO_TANGENT_FRAG)
}

/// Port of `OpenGL::MakeBSpline()`.
pub fn make_b_spline(device: *const Device) -> WindowAdaptPass {
    let sampler = util::create_bilinear_sampler();
    WindowAdaptPass::new(device, sampler, PRESENT_BSPLINE_FRAG)
}

/// Create a Gaussian scaling filter pass.
///
/// Port of `OpenGL::MakeGaussian()`.
pub fn make_gaussian(device: *const Device) -> WindowAdaptPass {
    let sampler = util::create_bilinear_sampler();
    WindowAdaptPass::new(device, sampler, PRESENT_GAUSSIAN_FRAG)
}

/// Port of `OpenGL::MakeLanczos()`.
pub fn make_lanczos(device: *const Device) -> WindowAdaptPass {
    let sampler = util::create_bilinear_sampler();
    WindowAdaptPass::new(device, sampler, PRESENT_LANCZOS_FRAG)
}

/// Create a ScaleForce scaling filter pass.
///
/// Port of `OpenGL::MakeScaleForce()`.
/// Upstream prepends `#version 460\n` to the scaleforce shader source.
pub fn make_scale_force(device: *const Device) -> WindowAdaptPass {
    let sampler = util::create_bilinear_sampler();
    let source = format!("#version 460\n{}", OPENGL_PRESENT_SCALEFORCE_FRAG);
    WindowAdaptPass::new(device, sampler, &source)
}

/// Port of `OpenGL::MakeArea()`.
pub fn make_area(device: *const Device) -> WindowAdaptPass {
    let sampler = util::create_bilinear_sampler();
    WindowAdaptPass::new(device, sampler, PRESENT_AREA_FRAG)
}

/// Port of `OpenGL::MakeMmpx()`.
pub fn make_mmpx(device: *const Device) -> WindowAdaptPass {
    let sampler = util::create_nearest_neighbor_sampler();
    WindowAdaptPass::new(device, sampler, PRESENT_MMPX_FRAG)
}
