// Host shader sources embedded from Eden's video_core/host_shaders/.
//
// Each shader file from the upstream C++ build is embedded as a const &str
// in the appropriate submodule, grouped by shader type.
// The shaders can be compiled to SPIR-V at build time or runtime.

pub mod compute_shaders;
pub mod fragment_shaders;
pub mod glsl_includes;
pub mod spirv_shaders;
pub mod vertex_shaders;

#[cfg(test)]
mod tests {
    use super::{glsl_includes, vertex_shaders};

    #[test]
    fn runtime_shader_exports_use_their_upstream_source_files() {
        assert_eq!(
            vertex_shaders::FULL_SCREEN_TRIANGLE_VERT,
            include_str!("full_screen_triangle.vert")
        );
        assert_eq!(vertex_shaders::FXAA_VERT, include_str!("fxaa.vert"));
        assert_eq!(
            vertex_shaders::OPENGL_PRESENT_VERT,
            include_str!("opengl_present.vert")
        );
        assert_eq!(
            vertex_shaders::SMAA_BLENDING_WEIGHT_CALCULATION_VERT,
            include_str!("smaa_blending_weight_calculation.vert")
        );
        assert_eq!(
            vertex_shaders::SMAA_EDGE_DETECTION_VERT,
            include_str!("smaa_edge_detection.vert")
        );
        assert_eq!(
            vertex_shaders::SMAA_NEIGHBORHOOD_BLENDING_VERT,
            include_str!("smaa_neighborhood_blending.vert")
        );
        assert_eq!(
            glsl_includes::OPENGL_SMAA_GLSL,
            include_str!("opengl_smaa.glsl")
        );
    }
}
