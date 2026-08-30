// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `shader_recompiler/runtime_info.h`
//!
//! Runtime information provided to the shader compiler that depends on
//! pipeline state not known at shader translation time.

use std::collections::BTreeMap;

use super::varying_state::VaryingState;

/// Vertex attribute type for input assembly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum AttributeType {
    Float = 0,
    SignedInt = 1,
    UnsignedInt = 2,
    SignedScaled = 3,
    UnsignedScaled = 4,
    Disabled = 5,
}

impl Default for AttributeType {
    fn default() -> Self {
        AttributeType::Float
    }
}

/// Input topology for geometry shaders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputTopology {
    Points,
    Lines,
    LinesAdjacency,
    Triangles,
    TrianglesAdjacency,
}

impl Default for InputTopology {
    fn default() -> Self {
        InputTopology::Points
    }
}

impl InputTopology {
    /// Port of upstream `InputTopologyVertices::vertices`.
    pub const fn vertices(self) -> u32 {
        match self {
            InputTopology::Lines => 2,
            InputTopology::LinesAdjacency => 4,
            InputTopology::Triangles => 3,
            InputTopology::TrianglesAdjacency => 6,
            InputTopology::Points => 1,
        }
    }
}

/// Depth comparison function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompareFunction {
    Never,
    Less,
    Equal,
    LessThanEqual,
    Greater,
    NotEqual,
    GreaterThanEqual,
    Always,
}

/// Tessellation primitive type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TessPrimitive {
    Isolines,
    Triangles,
    Quads,
}

impl Default for TessPrimitive {
    fn default() -> Self {
        TessPrimitive::Isolines
    }
}

/// Tessellation spacing mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TessSpacing {
    Equal,
    FractionalOdd,
    FractionalEven,
}

impl Default for TessSpacing {
    fn default() -> Self {
        TessSpacing::Equal
    }
}

/// Transform feedback varying descriptor.
#[derive(Debug, Clone, Copy, Default)]
pub struct TransformFeedbackVarying {
    pub buffer: u32,
    pub stream: u32,
    pub stride: u32,
    pub offset: u32,
    pub components: u32,
}

/// Runtime information for shader compilation.
///
/// Contains pipeline state that affects shader compilation but is not
/// part of the shader binary itself.
#[derive(Debug, Clone)]
pub struct RuntimeInfo {
    /// Input attribute types for vertex shaders.
    pub generic_input_types: [AttributeType; 32],
    /// Which varyings the previous stage stores.
    pub previous_stage_stores: VaryingState,
    /// Mapping of legacy attributes from previous stage.
    pub previous_stage_legacy_stores_mapping: BTreeMap<u64, u64>,

    /// Whether to convert depth mode (e.g., [-1,1] to [0,1]).
    pub convert_depth_mode: bool,
    /// Force early Z testing.
    pub force_early_z: bool,

    /// Tessellation primitive type.
    pub tess_primitive: TessPrimitive,
    /// Tessellation spacing mode.
    pub tess_spacing: TessSpacing,
    /// Tessellation winding order.
    pub tess_clockwise: bool,

    /// Input topology for geometry shaders.
    pub input_topology: InputTopology,

    /// Fixed-state point size (if set, overrides shader output).
    pub fixed_state_point_size: Option<f32>,
    /// Alpha test comparison function (if enabled).
    pub alpha_test_func: Option<CompareFunction>,
    /// Alpha test reference value.
    pub alpha_test_reference: f32,

    /// Static Y negate value.
    pub y_negate: bool,
    /// Use storage buffers instead of global pointers on GLASM.
    pub glasm_use_storage_buffers: bool,

    /// Transform feedback state for each varying.
    pub xfb_varyings: [TransformFeedbackVarying; 256],
    /// Number of transform feedback varyings.
    pub xfb_count: u32,

    /// Fragment color output type per render target.
    pub frag_color_types: [AttributeType; 8],

    /// Whether attachment 0 uses dual-source blending.
    pub dual_source_blend: bool,
}

impl Default for RuntimeInfo {
    fn default() -> Self {
        Self {
            generic_input_types: [AttributeType::default(); 32],
            previous_stage_stores: VaryingState::default(),
            previous_stage_legacy_stores_mapping: BTreeMap::new(),
            convert_depth_mode: false,
            force_early_z: false,
            tess_primitive: TessPrimitive::default(),
            tess_spacing: TessSpacing::default(),
            tess_clockwise: false,
            input_topology: InputTopology::default(),
            fixed_state_point_size: None,
            alpha_test_func: None,
            alpha_test_reference: 0.0,
            y_negate: false,
            glasm_use_storage_buffers: false,
            xfb_varyings: [TransformFeedbackVarying::default(); 256],
            xfb_count: 0,
            frag_color_types: [AttributeType::default(); 8],
            dual_source_blend: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_topology_vertices_matches_upstream() {
        assert_eq!(InputTopology::Points.vertices(), 1);
        assert_eq!(InputTopology::Lines.vertices(), 2);
        assert_eq!(InputTopology::LinesAdjacency.vertices(), 4);
        assert_eq!(InputTopology::Triangles.vertices(), 3);
        assert_eq!(InputTopology::TrianglesAdjacency.vertices(), 6);
    }

    #[test]
    fn transform_feedback_storage_matches_eden_fixed_extent() {
        let info = RuntimeInfo::default();

        assert_eq!(info.xfb_varyings.len(), 256);
        assert_eq!(info.xfb_count, 0);
        assert!(info.xfb_varyings.iter().all(|varying| varying.buffer == 0
            && varying.stream == 0
            && varying.stride == 0
            && varying.offset == 0
            && varying.components == 0));
    }
}
