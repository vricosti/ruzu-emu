// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Fragment-shader sources generated as string headers by Eden's
//! `video_core/host_shaders/CMakeLists.txt`.
//!
//! Rust embeds the authoritative shader files directly, keeping the source
//! consumed by OpenGL and the source compiled to SPIR-V identical.

pub const BLIT_COLOR_FLOAT_FRAG: &str = include_str!("blit_color_float.frag");
pub const BLIT_COLOR_MSAA_FRAG: &str = include_str!("blit_color_msaa.frag");
pub const BLIT_DEPTH_MSAA_FRAG: &str = include_str!("blit_depth_msaa.frag");
pub const BLIT_DEPTH_STENCIL_MSAA_FRAG: &str = include_str!("blit_depth_stencil_msaa.frag");
pub const CONVERT_ABGR8_TO_D24S8_FRAG: &str = include_str!("convert_abgr8_to_d24s8.frag");
pub const CONVERT_ABGR8_TO_D32F_FRAG: &str = include_str!("convert_abgr8_to_d32f.frag");
pub const CONVERT_D24S8_TO_ABGR8_FRAG: &str = include_str!("convert_d24s8_to_abgr8.frag");
pub const CONVERT_D32F_TO_ABGR8_FRAG: &str = include_str!("convert_d32f_to_abgr8.frag");
pub const CONVERT_DEPTH_TO_FLOAT_FRAG: &str = include_str!("convert_depth_to_float.frag");
pub const CONVERT_FLOAT_TO_DEPTH_FRAG: &str = include_str!("convert_float_to_depth.frag");
pub const CONVERT_MSAA_TO_NON_MSAA_FRAG: &str = include_str!("convert_msaa_to_non_msaa.frag");
pub const CONVERT_NON_MSAA_TO_MSAA_FRAG: &str = include_str!("convert_non_msaa_to_msaa.frag");
pub const CONVERT_S8D24_TO_ABGR8_FRAG: &str = include_str!("convert_s8d24_to_abgr8.frag");
pub const FIDELITYFX_FSR_FRAG: &str = include_str!("fidelityfx_fsr.frag");
pub const FXAA_FRAG: &str = include_str!("fxaa.frag");
pub const OPENGL_FIDELITYFX_FSR_FRAG: &str = include_str!("opengl_fidelityfx_fsr.frag");
pub const OPENGL_FIDELITYFX_FSR_EASU_FRAG: &str = include_str!("opengl_fidelityfx_fsr_easu.frag");
pub const OPENGL_FIDELITYFX_FSR_RCAS_FRAG: &str = include_str!("opengl_fidelityfx_fsr_rcas.frag");
pub const OPENGL_PRESENT_FRAG: &str = include_str!("opengl_present.frag");
pub const OPENGL_PRESENT_SCALEFORCE_FRAG: &str = include_str!("opengl_present_scaleforce.frag");
pub const PRESENT_AREA_FRAG: &str = include_str!("present_area.frag");
pub const PRESENT_BICUBIC_FRAG: &str = include_str!("present_bicubic.frag");
pub const PRESENT_BSPLINE_FRAG: &str = include_str!("present_bspline.frag");
pub const PRESENT_GAUSSIAN_FRAG: &str = include_str!("present_gaussian.frag");
pub const PRESENT_LANCZOS_FRAG: &str = include_str!("present_lanczos.frag");
pub const PRESENT_MITCHELL_FRAG: &str = include_str!("present_mitchell.frag");
pub const PRESENT_MMPX_FRAG: &str = include_str!("present_mmpx.frag");
pub const PRESENT_SPLINE1_FRAG: &str = include_str!("present_spline1.frag");
pub const PRESENT_ZERO_TANGENT_FRAG: &str = include_str!("present_zero_tangent.frag");
pub const SGSR1_SHADER_MOBILE_FRAG: &str = include_str!("sgsr1_shader_mobile.frag");
pub const SGSR1_SHADER_MOBILE_EDGE_DIRECTION_FRAG: &str =
    include_str!("sgsr1_shader_mobile_edge_direction.frag");
pub const SMAA_BLENDING_WEIGHT_CALCULATION_FRAG: &str =
    include_str!("smaa_blending_weight_calculation.frag");
pub const SMAA_EDGE_DETECTION_FRAG: &str = include_str!("smaa_edge_detection.frag");
pub const SMAA_NEIGHBORHOOD_BLENDING_FRAG: &str = include_str!("smaa_neighborhood_blending.frag");
pub const VULKAN_BLIT_DEPTH_STENCIL_FRAG: &str = include_str!("vulkan_blit_depth_stencil.frag");
pub const VULKAN_COLOR_CLEAR_FRAG: &str = include_str!("vulkan_color_clear.frag");
pub const VULKAN_DEPTHSTENCIL_CLEAR_FRAG: &str = include_str!("vulkan_depthstencil_clear.frag");
pub const VULKAN_FIDELITYFX_FSR_EASU_FP16_FRAG: &str =
    include_str!("vulkan_fidelityfx_fsr_easu_fp16.frag");
pub const VULKAN_FIDELITYFX_FSR_EASU_FP32_FRAG: &str =
    include_str!("vulkan_fidelityfx_fsr_easu_fp32.frag");
pub const VULKAN_FIDELITYFX_FSR_RCAS_FP16_FRAG: &str =
    include_str!("vulkan_fidelityfx_fsr_rcas_fp16.frag");
pub const VULKAN_FIDELITYFX_FSR_RCAS_FP32_FRAG: &str =
    include_str!("vulkan_fidelityfx_fsr_rcas_fp32.frag");
pub const VULKAN_PRESENT_FRAG: &str = include_str!("vulkan_present.frag");
pub const VULKAN_PRESENT_SCALEFORCE_FP16_FRAG: &str =
    include_str!("vulkan_present_scaleforce_fp16.frag");
pub const VULKAN_PRESENT_SCALEFORCE_FP32_FRAG: &str =
    include_str!("vulkan_present_scaleforce_fp32.frag");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_upstream_fragment_shader_sources_are_embedded() {
        let sources = [
            BLIT_COLOR_FLOAT_FRAG,
            BLIT_COLOR_MSAA_FRAG,
            BLIT_DEPTH_MSAA_FRAG,
            BLIT_DEPTH_STENCIL_MSAA_FRAG,
            CONVERT_ABGR8_TO_D24S8_FRAG,
            CONVERT_ABGR8_TO_D32F_FRAG,
            CONVERT_D24S8_TO_ABGR8_FRAG,
            CONVERT_D32F_TO_ABGR8_FRAG,
            CONVERT_DEPTH_TO_FLOAT_FRAG,
            CONVERT_FLOAT_TO_DEPTH_FRAG,
            CONVERT_MSAA_TO_NON_MSAA_FRAG,
            CONVERT_NON_MSAA_TO_MSAA_FRAG,
            CONVERT_S8D24_TO_ABGR8_FRAG,
            FIDELITYFX_FSR_FRAG,
            FXAA_FRAG,
            OPENGL_FIDELITYFX_FSR_FRAG,
            OPENGL_FIDELITYFX_FSR_EASU_FRAG,
            OPENGL_FIDELITYFX_FSR_RCAS_FRAG,
            OPENGL_PRESENT_FRAG,
            OPENGL_PRESENT_SCALEFORCE_FRAG,
            PRESENT_AREA_FRAG,
            PRESENT_BICUBIC_FRAG,
            PRESENT_BSPLINE_FRAG,
            PRESENT_GAUSSIAN_FRAG,
            PRESENT_LANCZOS_FRAG,
            PRESENT_MITCHELL_FRAG,
            PRESENT_MMPX_FRAG,
            PRESENT_SPLINE1_FRAG,
            PRESENT_ZERO_TANGENT_FRAG,
            SGSR1_SHADER_MOBILE_FRAG,
            SGSR1_SHADER_MOBILE_EDGE_DIRECTION_FRAG,
            SMAA_BLENDING_WEIGHT_CALCULATION_FRAG,
            SMAA_EDGE_DETECTION_FRAG,
            SMAA_NEIGHBORHOOD_BLENDING_FRAG,
            VULKAN_BLIT_DEPTH_STENCIL_FRAG,
            VULKAN_COLOR_CLEAR_FRAG,
            VULKAN_DEPTHSTENCIL_CLEAR_FRAG,
            VULKAN_FIDELITYFX_FSR_EASU_FP16_FRAG,
            VULKAN_FIDELITYFX_FSR_EASU_FP32_FRAG,
            VULKAN_FIDELITYFX_FSR_RCAS_FP16_FRAG,
            VULKAN_FIDELITYFX_FSR_RCAS_FP32_FRAG,
            VULKAN_PRESENT_FRAG,
            VULKAN_PRESENT_SCALEFORCE_FP16_FRAG,
            VULKAN_PRESENT_SCALEFORCE_FP32_FRAG,
        ];

        assert_eq!(sources.len(), 44);
        assert!(sources.iter().all(|source| !source.is_empty()));
    }

    #[test]
    fn corrected_shader_sources_match_upstream_semantics() {
        assert!(PRESENT_BICUBIC_FRAG.contains("transpose(mat4x4("));
        assert!(!PRESENT_BICUBIC_FRAG.contains("vec4 n = vec4(1.0, 2.0, 3.0, 4.0)"));
        assert!(VULKAN_BLIT_DEPTH_STENCIL_FRAG.contains("uniform usampler2D stencil_tex"));
        assert!(VULKAN_BLIT_DEPTH_STENCIL_FRAG
            .contains("gl_FragStencilRefARB = int(textureLod(stencil_tex, texcoord, 0).r)"));
    }
}
