// Vertex shaders from Eden's `video_core/host_shaders`.

pub const FULL_SCREEN_TRIANGLE_VERT: &str = include_str!("full_screen_triangle.vert");
pub const FXAA_VERT: &str = include_str!("fxaa.vert");
pub const OPENGL_PRESENT_VERT: &str = include_str!("opengl_present.vert");
pub const SGSR1_SHADER_VERT: &str = include_str!("sgsr1_shader.vert");
pub const SMAA_BLENDING_WEIGHT_CALCULATION_VERT: &str =
    include_str!("smaa_blending_weight_calculation.vert");
pub const SMAA_EDGE_DETECTION_VERT: &str = include_str!("smaa_edge_detection.vert");
pub const SMAA_NEIGHBORHOOD_BLENDING_VERT: &str = include_str!("smaa_neighborhood_blending.vert");
pub const VULKAN_COLOR_CLEAR_VERT: &str = include_str!("vulkan_color_clear.vert");
pub const VULKAN_FIDELITYFX_FSR_VERT: &str = include_str!("vulkan_fidelityfx_fsr.vert");
pub const VULKAN_PRESENT_VERT: &str = include_str!("vulkan_present.vert");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_upstream_vertex_shader_sources_are_embedded() {
        let sources = [
            FULL_SCREEN_TRIANGLE_VERT,
            FXAA_VERT,
            OPENGL_PRESENT_VERT,
            SGSR1_SHADER_VERT,
            SMAA_BLENDING_WEIGHT_CALCULATION_VERT,
            SMAA_EDGE_DETECTION_VERT,
            SMAA_NEIGHBORHOOD_BLENDING_VERT,
            VULKAN_COLOR_CLEAR_VERT,
            VULKAN_FIDELITYFX_FSR_VERT,
            VULKAN_PRESENT_VERT,
        ];

        assert_eq!(sources.len(), 10);
        assert!(sources.iter().all(|source| !source.is_empty()));
    }
}
