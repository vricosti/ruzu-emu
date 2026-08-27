// Generated Vulkan SPIR-V shader words.
//
// Upstream generates matching `*_spv.h` files from `video_core/host_shaders`
// with glslangValidator. The Rust build script emits this module into OUT_DIR.

include!(concat!(env!("OUT_DIR"), "/vulkan_present_spv.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_vulkan_shaders_are_spirv_modules() {
        const SPIRV_MAGIC: u32 = 0x0723_0203;
        for module in [
            VULKAN_PRESENT_VERT_SPV,
            VULKAN_PRESENT_FRAG_SPV,
            FXAA_VERT_SPV,
            FXAA_FRAG_SPV,
            SMAA_EDGE_DETECTION_VERT_SPV,
            SMAA_EDGE_DETECTION_FRAG_SPV,
            SMAA_BLENDING_WEIGHT_CALCULATION_VERT_SPV,
            SMAA_BLENDING_WEIGHT_CALCULATION_FRAG_SPV,
            SMAA_NEIGHBORHOOD_BLENDING_VERT_SPV,
            SMAA_NEIGHBORHOOD_BLENDING_FRAG_SPV,
            FULL_SCREEN_TRIANGLE_VERT_SPV,
            BLIT_COLOR_FLOAT_FRAG_SPV,
            BLIT_COLOR_MSAA_FRAG_SPV,
            BLIT_DEPTH_MSAA_FRAG_SPV,
            BLIT_DEPTH_STENCIL_MSAA_FRAG_SPV,
            VULKAN_BLIT_DEPTH_STENCIL_FRAG_SPV,
            VULKAN_COLOR_CLEAR_VERT_SPV,
            VULKAN_COLOR_CLEAR_FRAG_SPV,
            VULKAN_DEPTHSTENCIL_CLEAR_FRAG_SPV,
            CONVERT_DEPTH_TO_FLOAT_FRAG_SPV,
            CONVERT_FLOAT_TO_DEPTH_FRAG_SPV,
            CONVERT_ABGR8_TO_D24S8_FRAG_SPV,
            CONVERT_ABGR8_TO_D32F_FRAG_SPV,
            CONVERT_D32F_TO_ABGR8_FRAG_SPV,
            CONVERT_D24S8_TO_ABGR8_FRAG_SPV,
            CONVERT_S8D24_TO_ABGR8_FRAG_SPV,
            CONVERT_MSAA_TO_NON_MSAA_FRAG_SPV,
            CONVERT_NON_MSAA_TO_MSAA_FRAG_SPV,
            CONVERT_MSAA_TO_NON_MSAA_COMP_SPV,
            CONVERT_NON_MSAA_TO_MSAA_COMP_SPV,
            ASTC_DECODER_COMP_SPV,
            BLOCK_LINEAR_UNSWIZZLE_3D_BCN_COMP_SPV,
            QUERIES_PREFIX_SCAN_SUM_COMP_SPV,
            QUERIES_PREFIX_SCAN_SUM_NOSUBGROUPS_COMP_SPV,
            RESOLVE_CONDITIONAL_RENDER_COMP_SPV,
            VULKAN_TURBO_MODE_COMP_SPV,
            VULKAN_FIDELITYFX_FSR_VERT_SPV,
            VULKAN_FIDELITYFX_FSR_EASU_FP32_FRAG_SPV,
            VULKAN_FIDELITYFX_FSR_EASU_FP16_FRAG_SPV,
            VULKAN_FIDELITYFX_FSR_RCAS_FP32_FRAG_SPV,
            VULKAN_FIDELITYFX_FSR_RCAS_FP16_FRAG_SPV,
            PRESENT_BICUBIC_FRAG_SPV,
            PRESENT_GAUSSIAN_FRAG_SPV,
            PRESENT_AREA_FRAG_SPV,
            PRESENT_BSPLINE_FRAG_SPV,
            PRESENT_LANCZOS_FRAG_SPV,
            PRESENT_MITCHELL_FRAG_SPV,
            PRESENT_MMPX_FRAG_SPV,
            PRESENT_SPLINE1_FRAG_SPV,
            PRESENT_ZERO_TANGENT_FRAG_SPV,
            SGSR1_SHADER_VERT_SPV,
            SGSR1_SHADER_MOBILE_FRAG_SPV,
            SGSR1_SHADER_MOBILE_EDGE_DIRECTION_FRAG_SPV,
            VULKAN_PRESENT_SCALEFORCE_FP16_FRAG_SPV,
            VULKAN_PRESENT_SCALEFORCE_FP32_FRAG_SPV,
        ] {
            assert_eq!(module.first().copied(), Some(SPIRV_MAGIC));
        }
    }
}
