// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of Eden's video_core/renderer_opengl/present/present_uniforms.h
//!
//! Uniform locations and vertex types for the presentation pipeline.

/// Vertex attribute location for position.
pub const POSITION_LOCATION: i32 = 0;

/// Vertex attribute location for texture coordinates.
pub const TEX_COORD_LOCATION: i32 = 1;

/// Uniform location for the model-view matrix.
pub const MODEL_VIEW_MATRIX_LOCATION: i32 = 0;

/// Uniform location for the screen size.
pub const SCREEN_SIZE_LOCATION: i32 = 1;

/// Screen rectangle vertex for the presentation quad.
///
/// Corresponds to `OpenGL::ScreenRectVertex`.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct ScreenRectVertex {
    pub position: [f32; 2],
    pub tex_coord: [f32; 2],
}

impl ScreenRectVertex {
    pub fn new(x: u32, y: u32, u: f32, v: f32) -> Self {
        Self {
            position: [x as f32, y as f32],
            tex_coord: [u, v],
        }
    }
}

/// Build a 1:1 pixel orthographic projection matrix (3x2, column-major).
///
/// Corresponds to `OpenGL::MakeOrthographicMatrix()`.
pub fn make_orthographic_matrix(width: f32, height: f32) -> [f32; 6] {
    [2.0 / width, 0.0, 0.0, -2.0 / height, -1.0, 1.0]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_rect_vertex_matches_upstream_glfloat_layout() {
        assert_eq!(SCREEN_SIZE_LOCATION, 1);
        assert_eq!(
            std::mem::size_of::<ScreenRectVertex>(),
            4 * std::mem::size_of::<f32>()
        );
        assert_eq!(
            std::mem::align_of::<ScreenRectVertex>(),
            std::mem::align_of::<f32>()
        );

        let vertex = ScreenRectVertex::new(3, 5, 0.25, 0.75);
        assert_eq!(vertex.position, [3.0, 5.0]);
        assert_eq!(vertex.tex_coord, [0.25, 0.75]);
    }
}
