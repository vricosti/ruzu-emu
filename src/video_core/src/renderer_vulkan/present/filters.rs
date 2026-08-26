// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `present/filters.h` / `present/filters.cpp`.
//!
//! Factory functions for creating window adaptation passes with different
//! scaling filters. Each filter creates a WindowAdaptPass configured with
//! the appropriate sampler (nearest or bilinear) and fragment shader.

use ash::vk;

use crate::host_shaders::spirv_shaders::{
    PRESENT_AREA_FRAG_SPV, PRESENT_BICUBIC_FRAG_SPV, PRESENT_BSPLINE_FRAG_SPV,
    PRESENT_GAUSSIAN_FRAG_SPV, PRESENT_LANCZOS_FRAG_SPV, PRESENT_MITCHELL_FRAG_SPV,
    PRESENT_MMPX_FRAG_SPV, PRESENT_SPLINE1_FRAG_SPV, PRESENT_ZERO_TANGENT_FRAG_SPV,
    VULKAN_PRESENT_FRAG_SPV, VULKAN_PRESENT_SCALEFORCE_FP16_FRAG_SPV,
    VULKAN_PRESENT_SCALEFORCE_FP32_FRAG_SPV,
};
use crate::renderer_vulkan::shader_util::build_shader;
use crate::vulkan_common::vulkan_device::Device;

use super::util;
use super::window_adapt_pass::WindowAdaptPass;

pub use super::util::CubicFilterWeights;

// ---------------------------------------------------------------------------
// Factory functions
// ---------------------------------------------------------------------------

/// Port of `MakeNearestNeighbor`.
///
/// Creates a window adapt pass using nearest-neighbor sampling and
/// the basic present fragment shader.
pub fn make_nearest_neighbor(device: &Device, frame_format: vk::Format) -> WindowAdaptPass {
    let logical = device.get_logical();
    let sampler = util::create_nearest_neighbor_sampler(logical);
    let fragment_shader = build_shader(logical, VULKAN_PRESENT_FRAG_SPV)
        .expect("Failed to build vulkan_present.frag");
    WindowAdaptPass::new(device, frame_format, sampler, fragment_shader)
}

/// Port of `MakeBilinear`.
///
/// Creates a window adapt pass using bilinear sampling and the basic
/// present fragment shader.
pub fn make_bilinear(device: &Device, frame_format: vk::Format) -> WindowAdaptPass {
    let logical = device.get_logical();
    let sampler = util::create_bilinear_sampler(logical);
    let fragment_shader = build_shader(logical, VULKAN_PRESENT_FRAG_SPV)
        .expect("Failed to build vulkan_present.frag");
    WindowAdaptPass::new(device, frame_format, sampler, fragment_shader)
}

fn uses_hardware_cubic(
    filter_cubic_supported: bool,
    qcom_filter_cubic_weights_supported: bool,
    weights: CubicFilterWeights,
) -> bool {
    filter_cubic_supported
        && (qcom_filter_cubic_weights_supported || weights == CubicFilterWeights::CatmullRom)
}

/// Port of `MakeBicubic`.
pub fn make_bicubic(
    device: &Device,
    frame_format: vk::Format,
    weights: CubicFilterWeights,
) -> WindowAdaptPass {
    let logical = device.get_logical();
    if uses_hardware_cubic(
        device.is_ext_filter_cubic_supported(),
        device.is_qcom_filter_cubic_weights_supported(),
        weights,
    ) {
        let sampler = util::create_cubic_sampler(device, weights);
        let fragment_shader = build_shader(logical, VULKAN_PRESENT_FRAG_SPV)
            .expect("Failed to build vulkan_present.frag");
        return WindowAdaptPass::new(device, frame_format, sampler, fragment_shader);
    }

    let (shader, shader_name) = match weights {
        CubicFilterWeights::CatmullRom => (PRESENT_BICUBIC_FRAG_SPV, "present_bicubic.frag"),
        CubicFilterWeights::ZeroTangentCardinal => {
            (PRESENT_ZERO_TANGENT_FRAG_SPV, "present_zero_tangent.frag")
        }
        CubicFilterWeights::BSpline => (PRESENT_BSPLINE_FRAG_SPV, "present_bspline.frag"),
        CubicFilterWeights::MitchellNetravali => {
            (PRESENT_MITCHELL_FRAG_SPV, "present_mitchell.frag")
        }
    };
    let sampler = util::create_bilinear_sampler(logical);
    let fragment_shader =
        build_shader(logical, shader).unwrap_or_else(|_| panic!("Failed to build {shader_name}"));
    WindowAdaptPass::new(device, frame_format, sampler, fragment_shader)
}

fn make_shader_filter(
    device: &Device,
    frame_format: vk::Format,
    shader: &[u32],
    shader_name: &str,
) -> WindowAdaptPass {
    let logical = device.get_logical();
    let sampler = util::create_bilinear_sampler(logical);
    let fragment_shader =
        build_shader(logical, shader).unwrap_or_else(|_| panic!("Failed to build {shader_name}"));
    WindowAdaptPass::new(device, frame_format, sampler, fragment_shader)
}

/// Port of `MakeSpline1`.
pub fn make_spline1(device: &Device, frame_format: vk::Format) -> WindowAdaptPass {
    make_shader_filter(
        device,
        frame_format,
        PRESENT_SPLINE1_FRAG_SPV,
        "present_spline1.frag",
    )
}

/// Port of `MakeGaussian`.
///
/// Creates a window adapt pass using bilinear sampling with the
/// Gaussian blur fragment shader.
pub fn make_gaussian(device: &Device, frame_format: vk::Format) -> WindowAdaptPass {
    let logical = device.get_logical();
    let sampler = util::create_bilinear_sampler(logical);
    let fragment_shader = build_shader(logical, PRESENT_GAUSSIAN_FRAG_SPV)
        .expect("Failed to build present_gaussian.frag");
    WindowAdaptPass::new(device, frame_format, sampler, fragment_shader)
}

/// Port of `MakeLanczos`.
pub fn make_lanczos(device: &Device, frame_format: vk::Format) -> WindowAdaptPass {
    make_shader_filter(
        device,
        frame_format,
        PRESENT_LANCZOS_FRAG_SPV,
        "present_lanczos.frag",
    )
}

/// Port of `MakeScaleForce`.
///
/// Creates a window adapt pass using bilinear sampling with the
/// ScaleForce shader (fp16 preferred, fp32 fallback).
pub fn make_scale_force(device: &Device, frame_format: vk::Format) -> WindowAdaptPass {
    let logical = device.get_logical();
    let sampler = util::create_bilinear_sampler(logical);
    let (shader_spv, shader_name) = if device.is_float16_supported() {
        (
            VULKAN_PRESENT_SCALEFORCE_FP16_FRAG_SPV,
            "vulkan_present_scaleforce_fp16.frag",
        )
    } else {
        (
            VULKAN_PRESENT_SCALEFORCE_FP32_FRAG_SPV,
            "vulkan_present_scaleforce_fp32.frag",
        )
    };
    let fragment_shader = build_shader(logical, shader_spv)
        .unwrap_or_else(|_| panic!("Failed to build {shader_name}"));
    WindowAdaptPass::new(device, frame_format, sampler, fragment_shader)
}

/// Port of `MakeArea`.
pub fn make_area(device: &Device, frame_format: vk::Format) -> WindowAdaptPass {
    make_shader_filter(
        device,
        frame_format,
        PRESENT_AREA_FRAG_SPV,
        "present_area.frag",
    )
}

/// Port of `MakeMmpx`; upstream selects a nearest-neighbour sampler for MMPX.
pub fn make_mmpx(device: &Device, frame_format: vk::Format) -> WindowAdaptPass {
    let logical = device.get_logical();
    let sampler = util::create_nearest_neighbor_sampler(logical);
    let fragment_shader =
        build_shader(logical, PRESENT_MMPX_FRAG_SPV).expect("Failed to build present_mmpx.frag");
    WindowAdaptPass::new(device, frame_format, sampler, fragment_shader)
}

#[cfg(test)]
mod tests {
    use super::{uses_hardware_cubic, CubicFilterWeights};

    #[test]
    fn hardware_cubic_matches_upstream_extension_selection() {
        assert!(uses_hardware_cubic(
            true,
            false,
            CubicFilterWeights::CatmullRom
        ));
        assert!(!uses_hardware_cubic(
            false,
            true,
            CubicFilterWeights::CatmullRom
        ));
        assert!(!uses_hardware_cubic(
            true,
            false,
            CubicFilterWeights::ZeroTangentCardinal
        ));
        assert!(!uses_hardware_cubic(
            true,
            false,
            CubicFilterWeights::BSpline
        ));
        assert!(!uses_hardware_cubic(
            true,
            false,
            CubicFilterWeights::MitchellNetravali
        ));
        for weights in [
            CubicFilterWeights::CatmullRom,
            CubicFilterWeights::ZeroTangentCardinal,
            CubicFilterWeights::BSpline,
            CubicFilterWeights::MitchellNetravali,
        ] {
            assert!(uses_hardware_cubic(true, true, weights));
        }
    }
}
