// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of `fixed_pipeline_state.h` / `fixed_pipeline_state.cpp`.
//!
//! Hashable, bit-packed representation of non-dynamic graphics pipeline state.
//! Used as a key in the graphics pipeline cache to avoid re-creating VkPipelines.

use std::hash::{Hash, Hasher};

use crate::engines::draw_manager::Maxwell3DDrawView;
#[cfg(test)]
use crate::engines::draw_manager::{
    DrawMode, DrawState, IndexBuffer, Maxwell3DDrawRegisters, VertexBuffer,
};
use crate::engines::maxwell_3d::StreamOutLayout;
#[cfg(test)]
use crate::engines::maxwell_3d::VertexAttribType;
use crate::engines::maxwell_3d::{
    BlendEquation, BlendFactor, ComparisonOp, CullFace, DepthMode, FrontFace, PolygonMode,
    PrimitiveTopology, StencilOp,
};
use crate::transform_feedback::{TransformFeedbackLayout, TransformFeedbackState};

// ---------------------------------------------------------------------------
// Constants — port of anonymous namespace in fixed_pipeline_state.cpp
// ---------------------------------------------------------------------------

const POINT: usize = 0;
const LINE: usize = 1;
const POLYGON: usize = 2;

/// Lookup table mapping `PrimitiveTopology` to polygon offset mode index.
///
/// Port of `POLYGON_OFFSET_ENABLE_LUT` from `fixed_pipeline_state.cpp`.
const POLYGON_OFFSET_ENABLE_LUT: [usize; 15] = [
    POINT,   // Points
    LINE,    // Lines
    LINE,    // LineLoop
    LINE,    // LineStrip
    POLYGON, // Triangles
    POLYGON, // TriangleStrip
    POLYGON, // TriangleFan
    POLYGON, // Quads
    POLYGON, // QuadStrip
    POLYGON, // Polygon
    LINE,    // LinesAdjacency
    LINE,    // LineStripAdjacency
    POLYGON, // TrianglesAdjacency
    POLYGON, // TriangleStripAdjacency
    POLYGON, // Patches
];

const TOPOLOGY_CLASS_REPRESENTATIVE_LUT: [PrimitiveTopology; 15] = [
    PrimitiveTopology::Points,
    PrimitiveTopology::Lines,
    PrimitiveTopology::LineLoop,
    PrimitiveTopology::LineStrip,
    PrimitiveTopology::Triangles,
    PrimitiveTopology::Triangles,
    PrimitiveTopology::Triangles,
    PrimitiveTopology::Triangles,
    PrimitiveTopology::Triangles,
    PrimitiveTopology::Triangles,
    PrimitiveTopology::LinesAdjacency,
    PrimitiveTopology::LinesAdjacency,
    PrimitiveTopology::TrianglesAdjacency,
    PrimitiveTopology::TrianglesAdjacency,
    PrimitiveTopology::Patches,
];

fn is_dual_source_blend_factor(factor: BlendFactor) -> bool {
    matches!(
        factor,
        BlendFactor::Src1Color
            | BlendFactor::OneMinusSrc1Color
            | BlendFactor::Src1Alpha
            | BlendFactor::OneMinusSrc1Alpha
    )
}

fn attachment0_uses_dual_source_blend(draw: &Maxwell3DDrawView<'_>) -> bool {
    let blend = draw.blend_at(0);
    blend.enabled
        && [
            blend.color_src,
            blend.color_dst,
            blend.alpha_src,
            blend.alpha_dst,
        ]
        .into_iter()
        .any(is_dual_source_blend_factor)
}

/// Number of render targets.
const NUM_RENDER_TARGETS: usize = 8;

/// Number of vertex attributes.
const NUM_VERTEX_ATTRIBUTES: usize = 32;

/// Number of vertex arrays/streams.
const NUM_VERTEX_ARRAYS: usize = 32;

/// Number of viewports.
const NUM_VIEWPORTS: usize = 16;

// ---------------------------------------------------------------------------
// DynamicFeatures — port of DynamicFeatures struct
// ---------------------------------------------------------------------------

/// Dynamic state feature flags from the Vulkan device.
///
/// Port of `DynamicFeatures` struct from `fixed_pipeline_state.h`.
#[derive(Debug, Clone, Copy, Default)]
pub struct DynamicFeatures {
    pub driver_id: u32,
    pub driver_version: u32,
    pub has_extended_dynamic_state: bool,
    pub has_extended_dynamic_state_2: bool,
    pub has_extended_dynamic_state_2_logic_op: bool,
    pub has_extended_dynamic_state_2_patch_control_points: bool,
    pub has_extended_dynamic_state_3_blend: bool,
    pub has_extended_dynamic_state_3_enables: bool,
    pub has_dynamic_state3_depth_clamp_enable: bool,
    pub has_dynamic_state3_logic_op_enable: bool,
    pub has_dynamic_state3_line_stipple_enable: bool,
    pub has_dynamic_vertex_input: bool,
    pub has_color_write_enable: bool,
    pub has_provoking_vertex: bool,
    pub has_provoking_vertex_first_mode: bool,
    pub has_provoking_vertex_last_mode: bool,
    pub has_provoking_vertex_tf_preserve: bool,
}

// ---------------------------------------------------------------------------
// Pack/Unpack functions — port of FixedPipelineState static methods
// ---------------------------------------------------------------------------

/// Port of `FixedPipelineState::PackComparisonOp`.
///
/// OpenGL enums go from 0x200 to 0x207 and the D3D ones from 1 to 8.
/// Subtracting 0x200 from GL enums and 1 from D3D gives a 0-7 range.
pub fn pack_comparison_op(op: ComparisonOp) -> u32 {
    // Our Rust enum already normalizes D3D/GL, so just use ordinal
    op as u32
}

/// Port of `FixedPipelineState::UnpackComparisonOp`.
pub fn unpack_comparison_op(packed: u32) -> ComparisonOp {
    match packed {
        0 => ComparisonOp::Never,
        1 => ComparisonOp::Less,
        2 => ComparisonOp::Equal,
        3 => ComparisonOp::LessEqual,
        4 => ComparisonOp::Greater,
        5 => ComparisonOp::NotEqual,
        6 => ComparisonOp::GreaterEqual,
        7 => ComparisonOp::Always,
        _ => ComparisonOp::Always,
    }
}

/// Port of `FixedPipelineState::PackStencilOp`.
pub fn pack_stencil_op(op: StencilOp) -> u32 {
    match op {
        StencilOp::Keep => 0,
        StencilOp::Zero => 1,
        StencilOp::Replace => 2,
        StencilOp::IncrSat => 3,
        StencilOp::DecrSat => 4,
        StencilOp::Invert => 5,
        StencilOp::Incr => 6,
        StencilOp::Decr => 7,
    }
}

/// Port of `FixedPipelineState::UnpackStencilOp`.
pub fn unpack_stencil_op(packed: u32) -> StencilOp {
    const LUT: [StencilOp; 8] = [
        StencilOp::Keep,
        StencilOp::Zero,
        StencilOp::Replace,
        StencilOp::IncrSat,
        StencilOp::DecrSat,
        StencilOp::Invert,
        StencilOp::Incr,
        StencilOp::Decr,
    ];
    LUT[packed as usize]
}

/// Port of `FixedPipelineState::PackCullFace`.
///
/// FrontAndBack is 0x408, Front is 0x404, Back is 0x405.
pub fn pack_cull_face(cull: CullFace) -> u32 {
    match cull {
        CullFace::Front => 0,
        CullFace::Back => 1,
        CullFace::FrontAndBack => 2,
    }
}

/// Port of `FixedPipelineState::UnpackCullFace`.
pub fn unpack_cull_face(packed: u32) -> CullFace {
    const LUT: [CullFace; 3] = [CullFace::Front, CullFace::Back, CullFace::FrontAndBack];
    LUT[packed as usize]
}

/// Port of `FixedPipelineState::PackFrontFace`.
pub fn pack_front_face(face: FrontFace) -> u32 {
    match face {
        FrontFace::CW => 0,
        FrontFace::CCW => 1,
    }
}

/// Port of `FixedPipelineState::UnpackFrontFace`.
pub fn unpack_front_face(packed: u32) -> FrontFace {
    if packed == 0 {
        FrontFace::CW
    } else {
        FrontFace::CCW
    }
}

/// Port of `FixedPipelineState::PackPolygonMode`.
pub fn pack_polygon_mode(mode: PolygonMode) -> u32 {
    match mode {
        PolygonMode::Point => 0,
        PolygonMode::Line => 1,
        PolygonMode::Fill => 2,
    }
}

/// Port of `FixedPipelineState::UnpackPolygonMode`.
pub fn unpack_polygon_mode(packed: u32) -> PolygonMode {
    match packed {
        0 => PolygonMode::Point,
        1 => PolygonMode::Line,
        2 => PolygonMode::Fill,
        _ => panic!("invalid packed polygon mode {packed}"),
    }
}

/// Port of `FixedPipelineState::PackLogicOp`.
///
/// Logic ops are GL-encoded starting at 0x1500.
pub fn pack_logic_op(op: u32) -> u32 {
    op.wrapping_sub(0x1500)
}

/// Port of `FixedPipelineState::UnpackLogicOp`.
pub fn unpack_logic_op(packed: u32) -> u32 {
    packed + 0x1500
}

/// Port of `FixedPipelineState::PackBlendEquation`.
pub fn pack_blend_equation(eq: BlendEquation) -> u32 {
    match eq {
        BlendEquation::Add => 0,
        BlendEquation::Subtract => 1,
        BlendEquation::ReverseSubtract => 2,
        BlendEquation::Min => 3,
        BlendEquation::Max => 4,
    }
}

/// Port of `FixedPipelineState::UnpackBlendEquation`.
pub fn unpack_blend_equation(packed: u32) -> BlendEquation {
    const LUT: [BlendEquation; 5] = [
        BlendEquation::Add,
        BlendEquation::Subtract,
        BlendEquation::ReverseSubtract,
        BlendEquation::Min,
        BlendEquation::Max,
    ];
    LUT[packed as usize]
}

/// Port of `FixedPipelineState::PackBlendFactor`.
pub fn pack_blend_factor(factor: BlendFactor) -> u32 {
    match factor {
        BlendFactor::Zero => 0,
        BlendFactor::One => 1,
        BlendFactor::SrcColor => 2,
        BlendFactor::OneMinusSrcColor => 3,
        BlendFactor::SrcAlpha => 4,
        BlendFactor::OneMinusSrcAlpha => 5,
        BlendFactor::DstAlpha => 6,
        BlendFactor::OneMinusDstAlpha => 7,
        BlendFactor::DstColor => 8,
        BlendFactor::OneMinusDstColor => 9,
        BlendFactor::SrcAlphaSaturate => 10,
        BlendFactor::Src1Color => 11,
        BlendFactor::OneMinusSrc1Color => 12,
        BlendFactor::Src1Alpha => 13,
        BlendFactor::OneMinusSrc1Alpha => 14,
        BlendFactor::ConstantColor => 15,
        BlendFactor::OneMinusConstantColor => 16,
        BlendFactor::ConstantAlpha => 17,
        BlendFactor::OneMinusConstantAlpha => 18,
    }
}

/// Port of `FixedPipelineState::UnpackBlendFactor`.
pub fn unpack_blend_factor(packed: u32) -> BlendFactor {
    const LUT: [BlendFactor; 19] = [
        BlendFactor::Zero,
        BlendFactor::One,
        BlendFactor::SrcColor,
        BlendFactor::OneMinusSrcColor,
        BlendFactor::SrcAlpha,
        BlendFactor::OneMinusSrcAlpha,
        BlendFactor::DstAlpha,
        BlendFactor::OneMinusDstAlpha,
        BlendFactor::DstColor,
        BlendFactor::OneMinusDstColor,
        BlendFactor::SrcAlphaSaturate,
        BlendFactor::Src1Color,
        BlendFactor::OneMinusSrc1Color,
        BlendFactor::Src1Alpha,
        BlendFactor::OneMinusSrc1Alpha,
        BlendFactor::ConstantColor,
        BlendFactor::OneMinusConstantColor,
        BlendFactor::ConstantAlpha,
        BlendFactor::OneMinusConstantAlpha,
    ];
    assert!((packed as usize) < LUT.len());
    LUT[packed as usize]
}

// ---------------------------------------------------------------------------
// BlendingAttachment — port of FixedPipelineState::BlendingAttachment
// ---------------------------------------------------------------------------

/// Bit-packed blend state for a single render target attachment.
///
/// Port of `FixedPipelineState::BlendingAttachment`.
///
/// Bit layout (matches upstream):
/// - bits  0..0  : mask_r
/// - bits  1..1  : mask_g
/// - bits  2..2  : mask_b
/// - bits  3..3  : mask_a
/// - bits  4..6  : equation_rgb (3 bits)
/// - bits  7..9  : equation_a   (3 bits)
/// - bits 10..14 : factor_source_rgb (5 bits)
/// - bits 15..19 : factor_dest_rgb   (5 bits)
/// - bits 20..24 : factor_source_a   (5 bits)
/// - bits 25..29 : factor_dest_a     (5 bits)
/// - bit  30     : enable
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct BlendingAttachment {
    pub raw: u32,
}

impl Default for BlendingAttachment {
    fn default() -> Self {
        Self { raw: 0 }
    }
}

impl BlendingAttachment {
    /// Port of `FixedPipelineState::BlendingAttachment::Refresh`.
    #[inline(never)]
    fn refresh(&mut self, draw: &Maxwell3DDrawView<'_>, index: usize) {
        self.raw = 0;
        let mask = draw.color_mask(index);
        self.set_mask(mask.r, mask.g, mask.b, mask.a);

        let blend = draw.blend_at(index);
        if !blend.enabled {
            return;
        }
        if !draw.blend_per_target_enabled()
            && draw.iterated_blend_enabled()
            && common::settings::values().use_squashed_iterated_blend
        {
            self.set_equation_rgb(BlendEquation::Add);
            self.set_equation_alpha(BlendEquation::Add);
            self.set_source_rgb_factor(BlendFactor::One);
            self.set_dest_rgb_factor(BlendFactor::One);
            self.set_source_alpha_factor(BlendFactor::OneMinusSrcColor);
            self.set_dest_alpha_factor(BlendFactor::Zero);
            self.set_enabled(true);
            return;
        }
        self.set_equation_rgb(blend.color_op);
        self.set_equation_alpha(blend.alpha_op);
        self.set_source_rgb_factor(blend.color_src);
        self.set_dest_rgb_factor(blend.color_dst);
        self.set_source_alpha_factor(blend.alpha_src);
        self.set_dest_alpha_factor(blend.alpha_dst);
        self.set_enabled(true);
    }

    /// Port of `BlendingAttachment::Mask`.
    pub fn mask(&self) -> [bool; 4] {
        [
            (self.raw & (1 << 0)) != 0,
            (self.raw & (1 << 1)) != 0,
            (self.raw & (1 << 2)) != 0,
            (self.raw & (1 << 3)) != 0,
        ]
    }

    /// Port of `BlendingAttachment::EquationRGB`.
    pub fn equation_rgb(&self) -> BlendEquation {
        unpack_blend_equation((self.raw >> 4) & 0x7)
    }

    /// Port of `BlendingAttachment::EquationAlpha`.
    pub fn equation_alpha(&self) -> BlendEquation {
        unpack_blend_equation((self.raw >> 7) & 0x7)
    }

    /// Port of `BlendingAttachment::SourceRGBFactor`.
    pub fn source_rgb_factor(&self) -> BlendFactor {
        unpack_blend_factor((self.raw >> 10) & 0x1F)
    }

    /// Port of `BlendingAttachment::DestRGBFactor`.
    pub fn dest_rgb_factor(&self) -> BlendFactor {
        unpack_blend_factor((self.raw >> 15) & 0x1F)
    }

    /// Port of `BlendingAttachment::SourceAlphaFactor`.
    pub fn source_alpha_factor(&self) -> BlendFactor {
        unpack_blend_factor((self.raw >> 20) & 0x1F)
    }

    /// Port of `BlendingAttachment::DestAlphaFactor`.
    pub fn dest_alpha_factor(&self) -> BlendFactor {
        unpack_blend_factor((self.raw >> 25) & 0x1F)
    }

    /// Whether blending is enabled for this attachment.
    pub fn is_enabled(&self) -> bool {
        (self.raw & (1 << 30)) != 0
    }

    /// Set the mask values.
    pub fn set_mask(&mut self, r: bool, g: bool, b: bool, a: bool) {
        self.raw = (self.raw & !0xF)
            | ((r as u32) << 0)
            | ((g as u32) << 1)
            | ((b as u32) << 2)
            | ((a as u32) << 3);
    }

    /// Set equation RGB (3 bits at position 4).
    pub fn set_equation_rgb(&mut self, eq: BlendEquation) {
        let v = pack_blend_equation(eq);
        self.raw = (self.raw & !(0x7 << 4)) | ((v & 0x7) << 4);
    }

    /// Set equation Alpha (3 bits at position 7).
    pub fn set_equation_alpha(&mut self, eq: BlendEquation) {
        let v = pack_blend_equation(eq);
        self.raw = (self.raw & !(0x7 << 7)) | ((v & 0x7) << 7);
    }

    /// Set source RGB factor (5 bits at position 10).
    pub fn set_source_rgb_factor(&mut self, f: BlendFactor) {
        let v = pack_blend_factor(f);
        self.raw = (self.raw & !(0x1F << 10)) | ((v & 0x1F) << 10);
    }

    /// Set dest RGB factor (5 bits at position 15).
    pub fn set_dest_rgb_factor(&mut self, f: BlendFactor) {
        let v = pack_blend_factor(f);
        self.raw = (self.raw & !(0x1F << 15)) | ((v & 0x1F) << 15);
    }

    /// Set source alpha factor (5 bits at position 20).
    pub fn set_source_alpha_factor(&mut self, f: BlendFactor) {
        let v = pack_blend_factor(f);
        self.raw = (self.raw & !(0x1F << 20)) | ((v & 0x1F) << 20);
    }

    /// Set dest alpha factor (5 bits at position 25).
    pub fn set_dest_alpha_factor(&mut self, f: BlendFactor) {
        let v = pack_blend_factor(f);
        self.raw = (self.raw & !(0x1F << 25)) | ((v & 0x1F) << 25);
    }

    /// Set enable bit (bit 30).
    pub fn set_enabled(&mut self, enabled: bool) {
        if enabled {
            self.raw |= 1 << 30;
        } else {
            self.raw &= !(1 << 30);
        }
    }
}

// ---------------------------------------------------------------------------
// VertexAttribute — port of FixedPipelineState::VertexAttribute
// ---------------------------------------------------------------------------

/// Bit-packed vertex attribute descriptor.
///
/// Port of `FixedPipelineState::VertexAttribute`.
///
/// Bit layout:
/// - bit  0      : enabled
/// - bits 1..5   : buffer (5 bits)
/// - bits 6..19  : offset (14 bits)
/// - bits 20..22 : type   (3 bits)
/// - bits 23..28 : size   (6 bits)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct VertexAttribute {
    pub raw: u32,
}

impl Default for VertexAttribute {
    fn default() -> Self {
        Self { raw: 0 }
    }
}

impl VertexAttribute {
    /// Whether this attribute is enabled.
    pub fn is_enabled(&self) -> bool {
        (self.raw & 1) != 0
    }

    /// Buffer index (5 bits).
    pub fn buffer(&self) -> u32 {
        (self.raw >> 1) & 0x1F
    }

    /// Offset within buffer (14 bits).
    pub fn offset(&self) -> u32 {
        (self.raw >> 6) & 0x3FFF
    }

    /// Attribute type (3 bits) — maps to `Maxwell::VertexAttribute::Type`.
    pub fn attrib_type(&self) -> u32 {
        (self.raw >> 20) & 0x7
    }

    /// Attribute size (6 bits) — maps to `Maxwell::VertexAttribute::Size`.
    pub fn attrib_size(&self) -> u32 {
        (self.raw >> 23) & 0x3F
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.raw = (self.raw & !1) | (enabled as u32);
    }

    pub fn set_buffer(&mut self, buffer: u32) {
        self.raw = (self.raw & !(0x1F << 1)) | ((buffer & 0x1F) << 1);
    }

    pub fn set_offset(&mut self, offset: u32) {
        self.raw = (self.raw & !(0x3FFF << 6)) | ((offset & 0x3FFF) << 6);
    }

    pub fn set_type(&mut self, ty: u32) {
        self.raw = (self.raw & !(0x7 << 20)) | ((ty & 0x7) << 20);
    }

    pub fn set_size(&mut self, size: u32) {
        self.raw = (self.raw & !(0x3F << 23)) | ((size & 0x3F) << 23);
    }
}

// ---------------------------------------------------------------------------
// StencilFace — port of FixedPipelineState::StencilFace<Position>
// ---------------------------------------------------------------------------

/// Packed stencil face operations within a u32, at a given bit position.
///
/// Port of `FixedPipelineState::StencilFace<Position>`.
///
/// Layout (relative to position):
/// - bits 0..2  : action_stencil_fail (3 bits)
/// - bits 3..5  : action_depth_fail   (3 bits)
/// - bits 6..8  : action_depth_pass   (3 bits)
/// - bits 9..11 : test_func           (3 bits)
#[derive(Debug, Clone, Copy)]
pub struct StencilFace {
    pub position: u32,
}

impl StencilFace {
    /// Extract action_stencil_fail from the packed u32.
    pub fn action_stencil_fail(&self, raw: u32) -> StencilOp {
        unpack_stencil_op((raw >> self.position) & 0x7)
    }

    /// Extract action_depth_fail.
    pub fn action_depth_fail(&self, raw: u32) -> StencilOp {
        unpack_stencil_op((raw >> (self.position + 3)) & 0x7)
    }

    /// Extract action_depth_pass.
    pub fn action_depth_pass(&self, raw: u32) -> StencilOp {
        unpack_stencil_op((raw >> (self.position + 6)) & 0x7)
    }

    /// Extract test_func.
    pub fn test_func(&self, raw: u32) -> ComparisonOp {
        unpack_comparison_op((raw >> (self.position + 9)) & 0x7)
    }
}

/// Front stencil face (position 0 within raw2).
pub const STENCIL_FRONT: StencilFace = StencilFace { position: 0 };
/// Back stencil face (position 12 within raw2).
pub const STENCIL_BACK: StencilFace = StencilFace { position: 12 };

// ---------------------------------------------------------------------------
// DynamicState — port of FixedPipelineState::DynamicState
// ---------------------------------------------------------------------------

/// Bit-packed dynamic pipeline state.
///
/// Port of `FixedPipelineState::DynamicState`.
///
/// raw1 layout:
/// - bits 0..1   : cull_face (2 bits)
/// - bit  2      : cull_enable
/// - bit  3      : primitive_restart_enable
/// - bit  4      : depth_bias_enable
/// - bit  5      : rasterize_enable
/// - bits 6..9   : logic_op (4 bits)
/// - bit  10     : logic_op_enable
/// - bit  11     : depth_clamp_disabled
/// - bit  12     : line_stipple_enable
///
/// raw2 layout:
/// - bits 0..11  : front stencil face (12 bits)
/// - bits 12..23 : back stencil face  (12 bits)
/// - bit  24     : stencil_enable
/// - bit  25     : depth_write_enable
/// - bit  26     : depth_bounds_enable
/// - bit  27     : depth_test_enable
/// - bit  28     : front_face (1 bit)
/// - bits 29..31 : depth_test_func (3 bits)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DynamicState {
    pub raw1: u32,
    pub raw2: u32,
}

impl Default for DynamicState {
    fn default() -> Self {
        Self { raw1: 0, raw2: 0 }
    }
}

impl DynamicState {
    /// Port of `FixedPipelineState::DynamicState::Refresh`.
    #[inline(never)]
    fn refresh(&mut self, draw: &Maxwell3DDrawView<'_>) {
        let rasterizer = draw.rasterizer();
        let mut front_face = pack_front_face(rasterizer.front_face);
        if draw.window_origin_flip_y() {
            front_face = 1 - front_face;
        }

        let depth_stencil = draw.depth_stencil();
        let front = depth_stencil.front;
        let back = if depth_stencil.stencil_two_side {
            depth_stencil.back
        } else {
            front
        };
        self.set_stencil_face(0, front.fail_op, front.zfail_op, front.zpass_op, front.func);
        self.set_stencil_face(12, back.fail_op, back.zfail_op, back.zpass_op, back.func);
        self.set_stencil_enable(depth_stencil.stencil_enable);
        self.set_depth_write_enable(depth_stencil.depth_write_enable);
        self.set_depth_bounds_enable(draw.depth_bounds_enable());
        self.set_depth_test_enable(depth_stencil.depth_test_enable);
        self.set_front_face(unpack_front_face(front_face));
        self.set_depth_test_func(depth_stencil.depth_func);
        self.set_cull_face(rasterizer.cull_face);
        self.set_cull_enable(rasterizer.cull_enable);
    }

    /// Port of `FixedPipelineState::DynamicState::Refresh2`.
    #[inline(never)]
    fn refresh2(
        &mut self,
        draw: &Maxwell3DDrawView<'_>,
        topology: PrimitiveTopology,
        base_features_supported: bool,
    ) {
        self.set_logic_op(draw.logic_op().op);
        if base_features_supported {
            return;
        }
        let rasterizer = draw.rasterizer();
        self.set_rasterize_enable(draw.rasterize_enable());
        self.set_primitive_restart_enable(draw.primitive_restart().enabled);
        let depth_bias_enable = match POLYGON_OFFSET_ENABLE_LUT[topology as usize] {
            0 => rasterizer.polygon_offset_point_enable,
            1 => rasterizer.polygon_offset_line_enable,
            _ => rasterizer.polygon_offset_fill_enable,
        };
        self.set_depth_bias_enable(depth_bias_enable);
    }

    /// Port of `FixedPipelineState::DynamicState::Refresh3`.
    #[inline(never)]
    fn refresh3(&mut self, draw: &Maxwell3DDrawView<'_>, features: &DynamicFeatures) {
        if !features.has_dynamic_state3_logic_op_enable {
            self.set_logic_op_enable(draw.logic_op().enabled);
        }
        if !features.has_dynamic_state3_depth_clamp_enable {
            self.set_depth_clamp_disabled(!draw.depth_clamp_enabled());
        }
        if !features.has_dynamic_state3_line_stipple_enable {
            self.set_line_stipple_enable(draw.line_stipple().enabled);
        }
    }

    // --- raw1 field accessors ---

    pub fn cull_face(&self) -> CullFace {
        unpack_cull_face(self.raw1 & 0x3)
    }

    pub fn cull_enable(&self) -> bool {
        (self.raw1 & (1 << 2)) != 0
    }

    pub fn primitive_restart_enable(&self) -> bool {
        (self.raw1 & (1 << 3)) != 0
    }

    pub fn depth_bias_enable(&self) -> bool {
        (self.raw1 & (1 << 4)) != 0
    }

    pub fn rasterize_enable(&self) -> bool {
        (self.raw1 & (1 << 5)) != 0
    }

    pub fn logic_op(&self) -> u32 {
        unpack_logic_op((self.raw1 >> 6) & 0xF)
    }

    pub fn logic_op_enable(&self) -> bool {
        (self.raw1 & (1 << 10)) != 0
    }

    pub fn depth_clamp_disabled(&self) -> bool {
        (self.raw1 & (1 << 11)) != 0
    }

    pub fn line_stipple_enable(&self) -> bool {
        (self.raw1 & (1 << 12)) != 0
    }

    // --- raw1 field setters ---

    pub fn set_cull_face(&mut self, cull: CullFace) {
        let v = pack_cull_face(cull);
        self.raw1 = (self.raw1 & !0x3) | (v & 0x3);
    }

    pub fn set_cull_enable(&mut self, enable: bool) {
        if enable {
            self.raw1 |= 1 << 2;
        } else {
            self.raw1 &= !(1 << 2);
        }
    }

    pub fn set_primitive_restart_enable(&mut self, enable: bool) {
        if enable {
            self.raw1 |= 1 << 3;
        } else {
            self.raw1 &= !(1 << 3);
        }
    }

    pub fn set_depth_bias_enable(&mut self, enable: bool) {
        if enable {
            self.raw1 |= 1 << 4;
        } else {
            self.raw1 &= !(1 << 4);
        }
    }

    pub fn set_rasterize_enable(&mut self, enable: bool) {
        if enable {
            self.raw1 |= 1 << 5;
        } else {
            self.raw1 &= !(1 << 5);
        }
    }

    pub fn set_logic_op(&mut self, op: u32) {
        let v = pack_logic_op(op);
        self.raw1 = (self.raw1 & !(0xF << 6)) | ((v & 0xF) << 6);
    }

    pub fn set_logic_op_enable(&mut self, enable: bool) {
        if enable {
            self.raw1 |= 1 << 10;
        } else {
            self.raw1 &= !(1 << 10);
        }
    }

    pub fn set_depth_clamp_disabled(&mut self, disabled: bool) {
        if disabled {
            self.raw1 |= 1 << 11;
        } else {
            self.raw1 &= !(1 << 11);
        }
    }

    pub fn set_line_stipple_enable(&mut self, enable: bool) {
        if enable {
            self.raw1 |= 1 << 12;
        } else {
            self.raw1 &= !(1 << 12);
        }
    }

    // --- raw2 field accessors ---

    pub fn stencil_enable(&self) -> bool {
        (self.raw2 & (1 << 24)) != 0
    }

    pub fn depth_write_enable(&self) -> bool {
        (self.raw2 & (1 << 25)) != 0
    }

    pub fn depth_bounds_enable(&self) -> bool {
        (self.raw2 & (1 << 26)) != 0
    }

    pub fn depth_test_enable(&self) -> bool {
        (self.raw2 & (1 << 27)) != 0
    }

    pub fn front_face(&self) -> FrontFace {
        unpack_front_face((self.raw2 >> 28) & 0x1)
    }

    pub fn depth_test_func(&self) -> ComparisonOp {
        unpack_comparison_op((self.raw2 >> 29) & 0x7)
    }

    /// Get front stencil face operations from raw2.
    pub fn front_stencil(&self) -> &StencilFace {
        &STENCIL_FRONT
    }

    /// Get back stencil face operations from raw2.
    pub fn back_stencil(&self) -> &StencilFace {
        &STENCIL_BACK
    }

    // --- raw2 field setters ---

    pub fn set_stencil_enable(&mut self, enable: bool) {
        if enable {
            self.raw2 |= 1 << 24;
        } else {
            self.raw2 &= !(1 << 24);
        }
    }

    pub fn set_depth_write_enable(&mut self, enable: bool) {
        if enable {
            self.raw2 |= 1 << 25;
        } else {
            self.raw2 &= !(1 << 25);
        }
    }

    pub fn set_depth_bounds_enable(&mut self, enable: bool) {
        if enable {
            self.raw2 |= 1 << 26;
        } else {
            self.raw2 &= !(1 << 26);
        }
    }

    pub fn set_depth_test_enable(&mut self, enable: bool) {
        if enable {
            self.raw2 |= 1 << 27;
        } else {
            self.raw2 &= !(1 << 27);
        }
    }

    pub fn set_front_face(&mut self, face: FrontFace) {
        let v = pack_front_face(face);
        self.raw2 = (self.raw2 & !(1 << 28)) | ((v & 0x1) << 28);
    }

    pub fn set_depth_test_func(&mut self, func: ComparisonOp) {
        let v = pack_comparison_op(func);
        self.raw2 = (self.raw2 & !(0x7 << 29)) | ((v & 0x7) << 29);
    }

    /// Set a stencil face field (12 bits) at the given position.
    pub fn set_stencil_face(
        &mut self,
        position: u32,
        stencil_fail: StencilOp,
        depth_fail: StencilOp,
        depth_pass: StencilOp,
        test_func: ComparisonOp,
    ) {
        let packed = (pack_stencil_op(stencil_fail) & 0x7)
            | ((pack_stencil_op(depth_fail) & 0x7) << 3)
            | ((pack_stencil_op(depth_pass) & 0x7) << 6)
            | ((pack_comparison_op(test_func) & 0x7) << 9);
        let mask = 0xFFF << position;
        self.raw2 = (self.raw2 & !mask) | ((packed << position) & mask);
    }
}

// ---------------------------------------------------------------------------
// FixedPipelineState — port of the main struct
// ---------------------------------------------------------------------------

/// Hashable representation of all non-dynamic graphics pipeline state.
///
/// Port of `FixedPipelineState` from `fixed_pipeline_state.h`.
///
/// The upstream struct uses anonymous unions with bitfields for compact hashing.
/// We replicate the same bit layout using explicit raw u32 fields.
#[derive(Debug, Clone)]
pub struct FixedPipelineState {
    /// Packed flags word 1: dynamic state features, topology, polygon mode, etc.
    ///
    /// Bit layout (matches upstream raw1):
    /// - bit  0     : extended_dynamic_state
    /// - bit  1     : extended_dynamic_state_2
    /// - bit  2     : extended_dynamic_state_2_logic_op
    /// - bit  3     : extended_dynamic_state_3_blend
    /// - bit  4     : extended_dynamic_state_3_enables
    /// - bit  5     : dynamic_vertex_input
    /// - bit  6     : xfb_enabled
    /// - bit  7     : ndc_minus_one_to_one
    /// - bits 8..9  : polygon_mode (2 bits)
    /// - bits 10..11: tessellation_primitive (2 bits)
    /// - bits 12..13: tessellation_spacing (2 bits)
    /// - bit  14    : tessellation_clockwise
    /// - bits 15..19: patch_control_points_minus_one (5 bits)
    /// - bits 24..27: topology (4 bits)
    /// - bits 28..31: msaa_mode (4 bits)
    pub raw1: u32,

    /// Packed flags word 2: alpha test, depth format, etc.
    ///
    /// Bit layout (matches upstream raw2):
    /// - bits 1..3  : alpha_test_func (3 bits)
    /// - bit  4     : early_z
    /// - bit  5     : depth_enabled
    /// - bits 6..10 : depth_format (5 bits)
    /// - bit  11    : y_negate
    /// - bit  12    : provoking_vertex_last
    /// - bit  13    : conservative_raster_enable
    /// - bit  14    : smooth_lines
    /// - bit  15    : alpha_to_coverage_enabled
    /// - bit  16    : alpha_to_one_enabled
    /// - bits 17..19: app_stage (3 bits)
    pub raw2: u32,

    pub color_formats: [u8; NUM_RENDER_TARGETS],

    pub driver_id: u32,
    pub driver_version: u32,
    pub alpha_test_ref: u32,
    pub point_size: u32,
    pub viewport_swizzles: [u16; NUM_VIEWPORTS],

    /// Used with VK_EXT_vertex_input_dynamic_state as attribute_types,
    /// or as enabled_divisors otherwise (overlapping union).
    pub attribute_types_or_enabled_divisors: u64,

    pub dynamic_state: DynamicState,
    pub attachments: [BlendingAttachment; NUM_RENDER_TARGETS],
    pub attributes: [VertexAttribute; NUM_VERTEX_ATTRIBUTES],
    pub binding_divisors: [u32; NUM_VERTEX_ARRAYS],
    /// Vertex stride is a 12-bit value, we have 4 bits to spare per element.
    pub vertex_strides: [u16; NUM_VERTEX_ARRAYS],
    pub xfb_state: TransformFeedbackState,
    pub depth_bounds_min: u32,
    pub depth_bounds_max: u32,
    pub line_stipple_factor: u32,
    pub line_stipple_pattern: u32,
}

impl Default for FixedPipelineState {
    fn default() -> Self {
        Self {
            raw1: 0,
            raw2: 0,
            color_formats: [0; NUM_RENDER_TARGETS],
            driver_id: 0,
            driver_version: 0,
            alpha_test_ref: 0,
            point_size: 0,
            viewport_swizzles: [0; NUM_VIEWPORTS],
            attribute_types_or_enabled_divisors: 0,
            dynamic_state: DynamicState::default(),
            attachments: [BlendingAttachment::default(); NUM_RENDER_TARGETS],
            attributes: [VertexAttribute::default(); NUM_VERTEX_ARRAYS],
            binding_divisors: [0; NUM_VERTEX_ARRAYS],
            vertex_strides: [0; NUM_VERTEX_ARRAYS],
            xfb_state: TransformFeedbackState::default(),
            depth_bounds_min: 0,
            depth_bounds_max: 0,
            line_stipple_factor: 0,
            line_stipple_pattern: 0,
        }
    }
}

impl PartialEq for FixedPipelineState {
    /// Port of `FixedPipelineState`'s byte-prefix equality through
    /// `GraphicsPipelineCacheKey::operator==`.
    ///
    /// Upstream compares only `FixedPipelineState::Size()` bytes. Fields
    /// excluded by active dynamic-state features are deliberately not part of
    /// the key and are also omitted from the disk cache.
    fn eq(&self, rhs: &Self) -> bool {
        if self.raw1 != rhs.raw1
            || self.raw2 != rhs.raw2
            || self.color_formats != rhs.color_formats
            || self.driver_id != rhs.driver_id
            || self.driver_version != rhs.driver_version
            || self.alpha_test_ref != rhs.alpha_test_ref
            || self.point_size != rhs.point_size
            || self.viewport_swizzles != rhs.viewport_swizzles
            || self.attribute_types_or_enabled_divisors != rhs.attribute_types_or_enabled_divisors
        {
            return false;
        }

        // Transform feedback makes upstream Size() cover the complete state,
        // regardless of which dynamic-state extensions are enabled.
        if self.xfb_enabled() {
            return self.dynamic_state == rhs.dynamic_state
                && self.attachments == rhs.attachments
                && self.attributes == rhs.attributes
                && self.binding_divisors == rhs.binding_divisors
                && self.vertex_strides == rhs.vertex_strides
                && self.xfb_state == rhs.xfb_state
                && self.depth_bounds_min == rhs.depth_bounds_min
                && self.depth_bounds_max == rhs.depth_bounds_max
                && self.line_stipple_factor == rhs.line_stipple_factor
                && self.line_stipple_pattern == rhs.line_stipple_pattern;
        }
        if self.dynamic_vertex_input() && self.extended_dynamic_state_3_blend() {
            return true;
        }
        if self.dynamic_state != rhs.dynamic_state || self.attachments != rhs.attachments {
            return false;
        }
        if self.dynamic_vertex_input() {
            return true;
        }
        if self.attributes != rhs.attributes || self.binding_divisors != rhs.binding_divisors {
            return false;
        }
        if self.extended_dynamic_state() {
            return true;
        }
        self.vertex_strides == rhs.vertex_strides
    }
}

impl Eq for FixedPipelineState {}

impl Hash for FixedPipelineState {
    /// Port of `FixedPipelineState::Hash`.
    ///
    /// Upstream uses CityHash64 over a byte range whose size depends on
    /// dynamic state features. We hash the same fields for parity.
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.raw1.hash(state);
        self.raw2.hash(state);
        self.color_formats.hash(state);
        self.driver_id.hash(state);
        self.driver_version.hash(state);
        self.alpha_test_ref.hash(state);
        self.point_size.hash(state);
        self.viewport_swizzles.hash(state);
        self.attribute_types_or_enabled_divisors.hash(state);

        if self.xfb_enabled() {
            self.dynamic_state.hash(state);
            self.attachments.hash(state);
            self.attributes.hash(state);
            self.binding_divisors.hash(state);
            self.vertex_strides.hash(state);
            self.xfb_state.hash(state);
            self.depth_bounds_min.hash(state);
            self.depth_bounds_max.hash(state);
            self.line_stipple_factor.hash(state);
            self.line_stipple_pattern.hash(state);
            return;
        }

        // Match upstream FixedPipelineState::Size(): the hash covers a byte
        // prefix ending at a different field depending on enabled dynamic
        // state. Keep this ordered like the C++ struct declaration.
        if self.dynamic_vertex_input() && self.extended_dynamic_state_3_blend() {
            return;
        }

        self.dynamic_state.hash(state);
        self.attachments.hash(state);
        if self.dynamic_vertex_input() {
            return;
        }

        self.attributes.hash(state);
        self.binding_divisors.hash(state);
        if !self.extended_dynamic_state() {
            self.vertex_strides.hash(state);
        }
    }
}

impl FixedPipelineState {
    pub const XFB_STATE_SIZE: usize = 4 * 3 * 4 + 4 * 32 * 4;
    pub const PREFIX_SIZE: usize = 72;
    pub const DYNAMIC_STATE_OFFSET: usize = Self::PREFIX_SIZE;
    pub const ATTRIBUTES_OFFSET: usize = Self::DYNAMIC_STATE_OFFSET + 8 + NUM_RENDER_TARGETS * 4;
    pub const VERTEX_STRIDES_OFFSET: usize =
        Self::ATTRIBUTES_OFFSET + NUM_VERTEX_ATTRIBUTES * 4 + NUM_VERTEX_ARRAYS * 4;
    pub const XFB_STATE_OFFSET: usize = Self::VERTEX_STRIDES_OFFSET + NUM_VERTEX_ARRAYS * 2;
    pub const FULL_SIZE: usize = Self::XFB_STATE_OFFSET + Self::XFB_STATE_SIZE + 4 * 4;

    /// Port of upstream `FixedPipelineState::Size`.
    pub fn serialized_size(&self) -> usize {
        if self.xfb_enabled() {
            return Self::FULL_SIZE;
        }
        if self.dynamic_vertex_input() && self.extended_dynamic_state_3_blend() {
            return Self::DYNAMIC_STATE_OFFSET;
        }
        if self.dynamic_vertex_input() {
            return Self::ATTRIBUTES_OFFSET;
        }
        if self.extended_dynamic_state() {
            return Self::VERTEX_STRIDES_OFFSET;
        }
        Self::XFB_STATE_OFFSET
    }

    pub fn write_prefix_bytes(&self, out: &mut Vec<u8>) {
        let (all, size) = self.prefix_bytes();
        out.extend_from_slice(&all[..size]);
    }

    /// Materialize the byte prefix used by upstream
    /// `FixedPipelineState::Size()` without a heap allocation.
    ///
    /// The C++ key is a trivially-copyable object and CityHash reads this
    /// prefix directly from its storage. Reden keeps the explicitly packed
    /// Rust representation, so a fixed stack buffer is the equivalent input.
    pub fn prefix_bytes(&self) -> ([u8; Self::FULL_SIZE], usize) {
        let mut all = [0u8; Self::FULL_SIZE];
        let mut offset = 0usize;
        macro_rules! append {
            ($bytes:expr) => {{
                let bytes = $bytes;
                let end = offset + bytes.len();
                all[offset..end].copy_from_slice(&bytes);
                offset = end;
            }};
        }

        append!(self.raw1.to_le_bytes());
        append!(self.raw2.to_le_bytes());
        append!(self.color_formats);
        append!(self.driver_id.to_le_bytes());
        append!(self.driver_version.to_le_bytes());
        append!(self.alpha_test_ref.to_le_bytes());
        append!(self.point_size.to_le_bytes());
        for value in self.viewport_swizzles {
            append!(value.to_le_bytes());
        }
        append!(self.attribute_types_or_enabled_divisors.to_le_bytes());
        append!(self.dynamic_state.raw1.to_le_bytes());
        append!(self.dynamic_state.raw2.to_le_bytes());
        for value in self.attachments {
            append!(value.raw.to_le_bytes());
        }
        for value in self.attributes {
            append!(value.raw.to_le_bytes());
        }
        for value in self.binding_divisors {
            append!(value.to_le_bytes());
        }
        for value in self.vertex_strides {
            append!(value.to_le_bytes());
        }
        for layout in self.xfb_state.layouts {
            append!(layout.stream.to_le_bytes());
            append!(layout.varying_count.to_le_bytes());
            append!(layout.stride.to_le_bytes());
        }
        for buffer in self.xfb_state.varyings {
            for varying in buffer {
                append!(varying.raw().to_le_bytes());
            }
        }
        append!(self.depth_bounds_min.to_le_bytes());
        append!(self.depth_bounds_max.to_le_bytes());
        append!(self.line_stipple_factor.to_le_bytes());
        append!(self.line_stipple_pattern.to_le_bytes());
        debug_assert_eq!(offset, Self::FULL_SIZE);
        (all, self.serialized_size())
    }

    pub fn read_from_file(file: &mut std::fs::File) -> std::io::Result<Self> {
        use std::io::Read;

        let mut state = Self::default();
        state.raw1 = read_u32(file)?;
        state.raw2 = read_u32(file)?;
        file.read_exact(&mut state.color_formats)?;
        state.driver_id = read_u32(file)?;
        state.driver_version = read_u32(file)?;
        state.alpha_test_ref = read_u32(file)?;
        state.point_size = read_u32(file)?;
        for value in &mut state.viewport_swizzles {
            *value = read_u16(file)?;
        }
        state.attribute_types_or_enabled_divisors = read_u64(file)?;

        let size = state.serialized_size();
        if size >= Self::DYNAMIC_STATE_OFFSET + 8 {
            state.dynamic_state.raw1 = read_u32(file)?;
            state.dynamic_state.raw2 = read_u32(file)?;
        }
        if size >= Self::ATTRIBUTES_OFFSET {
            for value in &mut state.attachments {
                value.raw = read_u32(file)?;
            }
        }
        if size >= Self::VERTEX_STRIDES_OFFSET {
            for value in &mut state.attributes {
                value.raw = read_u32(file)?;
            }
            for value in &mut state.binding_divisors {
                *value = read_u32(file)?;
            }
        }
        if size >= Self::XFB_STATE_OFFSET {
            for value in &mut state.vertex_strides {
                *value = read_u16(file)?;
            }
        }
        if size >= Self::FULL_SIZE {
            state.xfb_state = read_xfb_state(file)?;
            state.depth_bounds_min = read_u32(file)?;
            state.depth_bounds_max = read_u32(file)?;
            state.line_stipple_factor = read_u32(file)?;
            state.line_stipple_pattern = read_u32(file)?;
        }
        Ok(state)
    }

    // --- raw1 accessors ---

    pub fn extended_dynamic_state(&self) -> bool {
        (self.raw1 & (1 << 0)) != 0
    }

    pub fn extended_dynamic_state_2(&self) -> bool {
        (self.raw1 & (1 << 1)) != 0
    }

    pub fn extended_dynamic_state_2_logic_op(&self) -> bool {
        (self.raw1 & (1 << 2)) != 0
    }

    pub fn extended_dynamic_state_3_blend(&self) -> bool {
        (self.raw1 & (1 << 3)) != 0
    }

    pub fn extended_dynamic_state_3_enables(&self) -> bool {
        (self.raw1 & (1 << 4)) != 0
    }

    pub fn dynamic_vertex_input(&self) -> bool {
        (self.raw1 & (1 << 5)) != 0
    }

    pub fn color_write_enable_dynamic(&self) -> bool {
        (self.raw1 & (1 << 20)) != 0
    }

    pub fn attachment0_dual_source_blend(&self) -> bool {
        (self.raw1 & (1 << 21)) != 0
    }

    pub fn xfb_enabled(&self) -> bool {
        (self.raw1 & (1 << 6)) != 0
    }

    pub fn ndc_minus_one_to_one(&self) -> bool {
        (self.raw1 & (1 << 7)) != 0
    }

    pub fn polygon_mode(&self) -> PolygonMode {
        unpack_polygon_mode((self.raw1 >> 8) & 0x3)
    }

    pub fn tessellation_primitive(&self) -> u32 {
        (self.raw1 >> 10) & 0x3
    }

    pub fn tessellation_spacing(&self) -> u32 {
        (self.raw1 >> 12) & 0x3
    }

    pub fn tessellation_clockwise(&self) -> bool {
        (self.raw1 & (1 << 14)) != 0
    }

    pub fn topology(&self) -> PrimitiveTopology {
        PrimitiveTopology::from_raw(((self.raw1 >> 24) & 0xF) as u32)
    }

    pub fn msaa_mode_raw(&self) -> u32 {
        (self.raw1 >> 28) & 0xF
    }

    pub fn patch_control_points_minus_one(&self) -> u32 {
        (self.raw1 >> 15) & 0x1F
    }

    pub fn patch_control_points(&self) -> u32 {
        self.patch_control_points_minus_one() + 1
    }

    // --- raw1 setters ---

    pub fn set_extended_dynamic_state(&mut self, v: bool) {
        self.set_bit_raw1(0, v);
    }
    pub fn set_extended_dynamic_state_2(&mut self, v: bool) {
        self.set_bit_raw1(1, v);
    }
    pub fn set_extended_dynamic_state_2_logic_op(&mut self, v: bool) {
        self.set_bit_raw1(2, v);
    }
    pub fn set_extended_dynamic_state_3_blend(&mut self, v: bool) {
        self.set_bit_raw1(3, v);
    }
    pub fn set_extended_dynamic_state_3_enables(&mut self, v: bool) {
        self.set_bit_raw1(4, v);
    }
    pub fn set_dynamic_vertex_input(&mut self, v: bool) {
        self.set_bit_raw1(5, v);
    }
    pub fn set_color_write_enable_dynamic(&mut self, v: bool) {
        self.set_bit_raw1(20, v);
    }
    pub fn set_attachment0_dual_source_blend(&mut self, v: bool) {
        self.set_bit_raw1(21, v);
    }
    pub fn set_xfb_enabled(&mut self, v: bool) {
        self.set_bit_raw1(6, v);
    }
    pub fn set_ndc_minus_one_to_one(&mut self, v: bool) {
        self.set_bit_raw1(7, v);
    }

    pub fn set_polygon_mode(&mut self, mode: PolygonMode) {
        let v = pack_polygon_mode(mode);
        self.raw1 = (self.raw1 & !(0x3 << 8)) | ((v & 0x3) << 8);
    }

    pub fn set_tessellation_primitive(&mut self, primitive: u32) {
        self.raw1 = (self.raw1 & !(0x3 << 10)) | ((primitive & 0x3) << 10);
    }

    pub fn set_tessellation_spacing(&mut self, spacing: u32) {
        self.raw1 = (self.raw1 & !(0x3 << 12)) | ((spacing & 0x3) << 12);
    }

    pub fn set_tessellation_clockwise(&mut self, clockwise: bool) {
        self.set_bit_raw1(14, clockwise);
    }

    pub fn set_topology(&mut self, topology: PrimitiveTopology) {
        let v = topology as u32;
        self.raw1 = (self.raw1 & !(0xF << 24)) | ((v & 0xF) << 24);
    }

    pub fn set_patch_control_points_minus_one(&mut self, v: u32) {
        self.raw1 = (self.raw1 & !(0x1F << 15)) | ((v & 0x1F) << 15);
    }

    pub fn set_msaa_mode_raw(&mut self, v: u32) {
        self.raw1 = (self.raw1 & !(0xF << 28)) | ((v & 0xF) << 28);
    }

    fn set_bit_raw1(&mut self, bit: u32, v: bool) {
        if v {
            self.raw1 |= 1 << bit;
        } else {
            self.raw1 &= !(1 << bit);
        }
    }

    // --- raw2 accessors ---

    pub fn alpha_test_func(&self) -> ComparisonOp {
        unpack_comparison_op((self.raw2 >> 1) & 0x7)
    }

    pub fn early_z(&self) -> bool {
        (self.raw2 & (1 << 4)) != 0
    }

    pub fn depth_enabled(&self) -> bool {
        (self.raw2 & (1 << 5)) != 0
    }

    pub fn depth_format(&self) -> u32 {
        (self.raw2 >> 6) & 0x1F
    }

    pub fn y_negate(&self) -> bool {
        (self.raw2 & (1 << 11)) != 0
    }

    pub fn provoking_vertex_last(&self) -> bool {
        (self.raw2 & (1 << 12)) != 0
    }

    pub fn conservative_raster_enable(&self) -> bool {
        (self.raw2 & (1 << 13)) != 0
    }

    pub fn smooth_lines(&self) -> bool {
        (self.raw2 & (1 << 14)) != 0
    }

    pub fn alpha_to_coverage_enabled(&self) -> bool {
        (self.raw2 & (1 << 15)) != 0
    }

    pub fn alpha_to_one_enabled(&self) -> bool {
        (self.raw2 & (1 << 16)) != 0
    }

    pub fn app_stage(&self) -> u32 {
        (self.raw2 >> 17) & 0x7
    }

    // --- raw2 setters ---

    pub fn set_alpha_test_func(&mut self, func: ComparisonOp) {
        let v = pack_comparison_op(func);
        self.raw2 = (self.raw2 & !(0x7 << 1)) | ((v & 0x7) << 1);
    }

    pub fn set_early_z(&mut self, v: bool) {
        self.set_bit_raw2(4, v);
    }
    pub fn set_depth_enabled(&mut self, v: bool) {
        self.set_bit_raw2(5, v);
    }

    pub fn set_depth_format(&mut self, format: u32) {
        self.raw2 = (self.raw2 & !(0x1F << 6)) | ((format & 0x1F) << 6);
    }

    pub fn set_y_negate(&mut self, v: bool) {
        self.set_bit_raw2(11, v);
    }
    pub fn set_provoking_vertex_last(&mut self, v: bool) {
        self.set_bit_raw2(12, v);
    }
    pub fn set_conservative_raster_enable(&mut self, v: bool) {
        self.set_bit_raw2(13, v);
    }
    pub fn set_smooth_lines(&mut self, v: bool) {
        self.set_bit_raw2(14, v);
    }
    pub fn set_alpha_to_coverage_enabled(&mut self, v: bool) {
        self.set_bit_raw2(15, v);
    }
    pub fn set_alpha_to_one_enabled(&mut self, v: bool) {
        self.set_bit_raw2(16, v);
    }

    pub fn set_app_stage(&mut self, v: u32) {
        self.raw2 = (self.raw2 & !(0x7 << 17)) | ((v & 0x7) << 17);
    }

    fn set_bit_raw2(&mut self, bit: u32, v: bool) {
        if v {
            self.raw2 |= 1 << bit;
        } else {
            self.raw2 &= !(1 << bit);
        }
    }

    /// Port of `FixedPipelineState::DynamicAttributeType`.
    pub fn dynamic_attribute_type(&self, index: usize) -> u32 {
        ((self.attribute_types_or_enabled_divisors >> (index * 2)) & 0b11) as u32
    }

    /// Port of `FixedPipelineState::Refresh(Tegra::Engines::Maxwell3D&,
    /// DynamicFeatures&)`.
    pub fn refresh(&mut self, draw: &mut Maxwell3DDrawView<'_>, features: &DynamicFeatures) {
        let topology = draw.draw_state().topology;

        self.driver_id = features.driver_id;
        self.driver_version = features.driver_version;

        self.raw1 = 0;
        self.set_extended_dynamic_state(features.has_extended_dynamic_state);
        self.set_extended_dynamic_state_2(features.has_extended_dynamic_state_2);
        self.set_extended_dynamic_state_2_logic_op(features.has_extended_dynamic_state_2_logic_op);
        self.set_extended_dynamic_state_3_blend(features.has_extended_dynamic_state_3_blend);
        self.set_extended_dynamic_state_3_enables(features.has_extended_dynamic_state_3_enables);
        self.set_color_write_enable_dynamic(features.has_color_write_enable);
        self.set_dynamic_vertex_input(features.has_dynamic_vertex_input);
        self.set_xfb_enabled(draw.transform_feedback_enabled());
        self.set_ndc_minus_one_to_one(draw.depth_stencil().depth_mode == DepthMode::MinusOneToOne);
        self.set_polygon_mode(draw.rasterizer().polygon_mode_front);
        self.set_tessellation_primitive(draw.tessellation_domain_type());
        self.set_tessellation_spacing(draw.tessellation_spacing());
        self.set_tessellation_clockwise(draw.tessellation_clockwise());
        // Upstream subtracts from u32 and assigns the low five bits to the
        // bitfield. Preserve that bit pattern when the reset value is zero.
        self.set_patch_control_points_minus_one(draw.patch_vertices().wrapping_sub(1));
        self.set_topology(
            if features.has_extended_dynamic_state && features.has_extended_dynamic_state_2 {
                TOPOLOGY_CLASS_REPRESENTATIVE_LUT[topology as usize]
            } else {
                topology
            },
        );
        self.set_msaa_mode_raw(draw.anti_alias_samples_mode());
        self.set_attachment0_dual_source_blend(attachment0_uses_dual_source_blend(draw));

        self.raw2 = 0;
        self.set_alpha_test_func(if draw.alpha_test_enabled() {
            draw.alpha_test_func()
        } else {
            ComparisonOp::Always
        });
        self.set_early_z(draw.mandated_early_z());
        let zeta = draw.zeta();
        self.set_depth_enabled(zeta.enabled);
        self.set_depth_format(zeta.format);
        self.set_y_negate(draw.window_origin_lower_left());
        let mut provoking_vertex_last = false;
        if features.has_provoking_vertex
            && (features.has_provoking_vertex_first_mode || features.has_provoking_vertex_last_mode)
        {
            provoking_vertex_last = draw.provoking_vertex_last();
            if draw.transform_feedback_enabled() && !features.has_provoking_vertex_tf_preserve {
                provoking_vertex_last = false;
            }
            if provoking_vertex_last && !features.has_provoking_vertex_last_mode {
                provoking_vertex_last = false;
            } else if !provoking_vertex_last && !features.has_provoking_vertex_first_mode {
                provoking_vertex_last = true;
            }
        }
        self.set_provoking_vertex_last(provoking_vertex_last);
        self.set_conservative_raster_enable(draw.conservative_raster_enable());
        self.set_smooth_lines(draw.line_state().line_anti_alias_enable);
        let alpha_control = draw.anti_alias_alpha_control();
        self.set_alpha_to_coverage_enabled(alpha_control.alpha_to_coverage);
        self.set_alpha_to_one_enabled(alpha_control.alpha_to_one);
        self.set_app_stage(draw.engine_state() as u32);
        let depth_bounds = draw.depth_bounds();
        self.depth_bounds_min = depth_bounds[0] as u32;
        self.depth_bounds_max = depth_bounds[1] as u32;
        let line_stipple = draw.line_stipple();
        self.line_stipple_factor = line_stipple.factor;
        self.line_stipple_pattern = line_stipple.pattern;
        for index in 0..NUM_RENDER_TARGETS {
            self.color_formats[index] = draw.render_target(index).format as u8;
        }
        self.alpha_test_ref = draw.alpha_test_ref().to_bits();
        self.point_size = draw.point_state().point_size.to_bits();
        if draw.dirty_flag(super::state_tracker::dirty::VERTEX_INPUT) {
            if features.has_dynamic_vertex_input {
                // Upstream deliberately leaves VertexInput dirty for the
                // command-buffer dynamic-state update to consume.
                const TYPE_LUT: [u32; 8] = [0, 1, 1, 2, 3, 1, 1, 1];
                self.attribute_types_or_enabled_divisors = 0;
                for index in 0..NUM_VERTEX_ATTRIBUTES {
                    let attrib = draw.vertex_attrib_raw(index);
                    let ty = TYPE_LUT[((attrib >> 27) & 0x7) as usize];
                    let mask: u32 = if attrib & (1 << 6) != 0 { 0 } else { 0b11 };
                    self.attribute_types_or_enabled_divisors |= u64::from(ty & mask) << (index * 2);
                }
            } else {
                draw.clear_dirty_flag(super::state_tracker::dirty::VERTEX_INPUT);
                self.attribute_types_or_enabled_divisors = 0;
                for index in 0..NUM_VERTEX_ARRAYS {
                    let stream = draw.vertex_stream(index);
                    let is_enabled = draw.vertex_stream_instance(index) != 0;
                    self.binding_divisors[index] = if is_enabled { stream.frequency } else { 0 };
                    self.attribute_types_or_enabled_divisors |= u64::from(is_enabled) << index;
                }
                for index in 0..NUM_VERTEX_ATTRIBUTES {
                    let attrib = draw.vertex_attrib_raw(index);
                    self.attributes[index].raw = u32::from(attrib & (1 << 6) == 0)
                        | ((attrib & 0x1f) << 1)
                        | (((attrib >> 7) & 0x3fff) << 6)
                        | (((attrib >> 27) & 0x7) << 20)
                        | (((attrib >> 21) & 0x3f) << 23);
                }
            }
        }
        if draw.dirty_flag(super::state_tracker::dirty::VIEWPORT_SWIZZLES) {
            draw.clear_dirty_flag(super::state_tracker::dirty::VIEWPORT_SWIZZLES);
            for index in 0..NUM_VIEWPORTS {
                self.viewport_swizzles[index] = draw.viewport_transform(index).swizzle as u16;
            }
        }
        // Upstream zeroes dynamic_state then refreshes each group only when
        // the covering extension is NOT supported
        // (fixed_pipeline_state.cpp:142-165):
        //   - !extended_dynamic_state         → DynamicState::Refresh + vertex_strides
        //   - !extended_dynamic_state_2_logic_op → DynamicState::Refresh2 (logic_op always;
        //                                       rasterize/primitive_restart/depth_bias
        //                                       only when !extended_dynamic_state_2)
        //   - !extended_dynamic_state_3_blend → attachments
        //   - Refresh3 always runs and retains only the granular EDS3 fields
        //     that the device cannot make dynamic.
        self.dynamic_state = DynamicState::default();
        if !features.has_extended_dynamic_state {
            self.dynamic_state.refresh(draw);
            for index in 0..NUM_VERTEX_ARRAYS {
                self.vertex_strides[index] = draw.vertex_stream(index).stride as u16;
            }
        }
        if !features.has_extended_dynamic_state_2_logic_op {
            self.dynamic_state
                .refresh2(draw, topology, features.has_extended_dynamic_state_2);
        }
        // Populate blend attachments.
        // Upstream refreshes them only when !extended_dynamic_state_3_blend.
        if !features.has_extended_dynamic_state_3_blend
            && draw.dirty_flag(super::state_tracker::dirty::BLENDING)
        {
            draw.clear_dirty_flag(super::state_tracker::dirty::BLENDING);
            for i in 0..NUM_RENDER_TARGETS {
                let attachment = &mut self.attachments[i];
                attachment.refresh(draw, i);
                let mask = attachment.mask();
                if features.has_color_write_enable && !mask[0] && !mask[1] && !mask[2] && !mask[3] {
                    // Upstream makes fully disabled attachments statically writable;
                    // `VK_EXT_color_write_enable` supplies the actual all-off state.
                    attachment.set_mask(true, true, true, true);
                }
            }
        }

        self.dynamic_state.refresh3(draw, features);

        if self.xfb_enabled() {
            self.xfb_state = draw.transform_feedback_state();
        }
    }

    #[cfg(test)]
    fn refresh_draw_call_for_test(
        &mut self,
        draw: &crate::engines::maxwell_3d::DrawCall,
        features: &DynamicFeatures,
    ) {
        let draw_state = DrawState {
            topology: draw.topology,
            draw_mode: DrawMode::General,
            draw_indexed: draw.indexed,
            base_index: draw.base_vertex as u32,
            vertex_buffer: VertexBuffer {
                first: draw.vertex_first,
                count: draw.vertex_count,
            },
            index_buffer: IndexBuffer {
                first: draw.index_buffer_first,
                count: draw.index_buffer_count,
                format: draw.index_format,
            },
            base_instance: draw.base_instance,
            instance_count: draw.instance_count,
            inline_index_draw_indexes: draw.inline_index_data.clone(),
        };
        let registers = Maxwell3DDrawRegisters::from_draw_call(draw);
        let mut draw_view =
            Maxwell3DDrawView::with_register_snapshot(&draw_state, draw.indexed, registers);
        self.refresh(&mut draw_view, features);
    }
}

fn read_xfb_state(file: &mut std::fs::File) -> std::io::Result<TransformFeedbackState> {
    let mut layouts = [TransformFeedbackLayout::default(); 4];
    for layout in &mut layouts {
        layout.stream = read_u32(file)?;
        layout.varying_count = read_u32(file)?;
        layout.stride = read_u32(file)?;
    }
    let mut varyings = [[StreamOutLayout::default(); 32]; 4];
    for buffer in &mut varyings {
        for varying in buffer {
            *varying = StreamOutLayout::from_raw(read_u32(file)?);
        }
    }
    Ok(TransformFeedbackState { layouts, varyings })
}

fn read_u16(file: &mut std::fs::File) -> std::io::Result<u16> {
    use std::io::Read;
    let mut buf = [0u8; 2];
    file.read_exact(&mut buf)?;
    Ok(u16::from_le_bytes(buf))
}

fn read_u32(file: &mut std::fs::File) -> std::io::Result<u32> {
    use std::io::Read;
    let mut buf = [0u8; 4];
    file.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_u64(file: &mut std::fs::File) -> std::io::Result<u64> {
    use std::io::Read;
    let mut buf = [0u8; 8];
    file.read_exact(&mut buf)?;
    Ok(u64::from_le_bytes(buf))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::const_buffer_info::ConstBufferInfo;
    use crate::engines::maxwell_3d::{
        AntiAliasAlphaControlInfo, BlendColorInfo, BlendInfo, ColorMaskInfo, DepthMode,
        DepthStencilInfo, DrawCall, IndexFormat, LogicOpInfo, RasterizerInfo, RenderTargetInfo,
        RtControlInfo, SamplerBinding, ScissorInfo, ShaderStageInfo, StencilFaceInfo,
        VertexAttribInfo, VertexAttribSize, VertexStreamInfo, ViewportInfo, ZetaInfo,
    };
    use std::collections::hash_map::DefaultHasher;

    fn hash_state(state: &FixedPipelineState) -> u64 {
        let mut hasher = DefaultHasher::new();
        state.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn fixed_pipeline_state_serialized_offsets_match_upstream() {
        assert_eq!(FixedPipelineState::DYNAMIC_STATE_OFFSET, 72);
        assert_eq!(FixedPipelineState::ATTRIBUTES_OFFSET, 112);
        assert_eq!(FixedPipelineState::VERTEX_STRIDES_OFFSET, 368);
        assert_eq!(FixedPipelineState::XFB_STATE_OFFSET, 432);
        assert_eq!(FixedPipelineState::XFB_STATE_SIZE, 560);
        assert_eq!(FixedPipelineState::FULL_SIZE, 1008);

        let mut state = FixedPipelineState::default();
        assert_eq!(
            state.serialized_size(),
            FixedPipelineState::XFB_STATE_OFFSET
        );
        state.set_extended_dynamic_state(true);
        assert_eq!(
            state.serialized_size(),
            FixedPipelineState::VERTEX_STRIDES_OFFSET
        );
        state.set_dynamic_vertex_input(true);
        assert_eq!(
            state.serialized_size(),
            FixedPipelineState::ATTRIBUTES_OFFSET
        );
        state.set_extended_dynamic_state_3_blend(true);
        assert_eq!(
            state.serialized_size(),
            FixedPipelineState::DYNAMIC_STATE_OFFSET
        );
        state.set_xfb_enabled(true);
        assert_eq!(state.serialized_size(), FixedPipelineState::FULL_SIZE);
    }

    #[test]
    fn fixed_pipeline_state_round_trips_transform_feedback_state() {
        let mut state = FixedPipelineState::default();
        state.set_xfb_enabled(true);
        state.xfb_state.layouts[2] = TransformFeedbackLayout {
            stream: 7,
            varying_count: 9,
            stride: 11,
        };
        state.xfb_state.varyings[2][3] = StreamOutLayout::from_raw(0xAABB_CCDD);

        let mut bytes = Vec::new();
        state.write_prefix_bytes(&mut bytes);
        let path = std::env::temp_dir().join(format!(
            "ruzu-fixed-pipeline-state-xfb-{}.bin",
            std::process::id()
        ));
        std::fs::write(&path, &bytes).unwrap();
        let mut file = std::fs::File::open(&path).unwrap();
        let decoded = FixedPipelineState::read_from_file(&mut file).unwrap();
        let _ = std::fs::remove_file(&path);

        assert!(decoded.xfb_enabled());
        assert_eq!(decoded.xfb_state.layouts[2].stream, 7);
        assert_eq!(decoded.xfb_state.layouts[2].varying_count, 9);
        assert_eq!(decoded.xfb_state.layouts[2].stride, 11);
        assert_eq!(decoded.xfb_state.varyings[2][3].raw(), 0xAABB_CCDD);
    }

    #[test]
    fn equality_ignores_state_excluded_by_upstream_size() {
        let mut live = FixedPipelineState::default();
        live.set_dynamic_vertex_input(true);
        live.set_extended_dynamic_state_3_blend(true);
        live.dynamic_state.raw1 = 0x1122_3344;
        live.dynamic_state.raw2 = 0x5566_7788;
        live.attachments[0].raw = 0xAABB_CCDD;
        live.attributes[0].raw = 0x1234_5678;
        live.binding_divisors[0] = 9;
        live.vertex_strides[0] = 32;

        let mut loaded = live.clone();
        loaded.dynamic_state = DynamicState::default();
        loaded.attachments = [BlendingAttachment::default(); NUM_RENDER_TARGETS];
        loaded.attributes = [VertexAttribute::default(); NUM_VERTEX_ATTRIBUTES];
        loaded.binding_divisors = [0; NUM_VERTEX_ARRAYS];
        loaded.vertex_strides = [0; NUM_VERTEX_ARRAYS];

        assert_eq!(
            live.serialized_size(),
            FixedPipelineState::DYNAMIC_STATE_OFFSET
        );
        assert_eq!(live, loaded);
        assert_eq!(hash_state(&live), hash_state(&loaded));
    }

    #[test]
    fn transform_feedback_forces_full_state_equality_and_hashing() {
        let mut a = FixedPipelineState::default();
        a.set_dynamic_vertex_input(true);
        a.set_extended_dynamic_state_3_blend(true);
        a.set_xfb_enabled(true);
        let mut b = a.clone();
        b.attachments[0].raw = 1;

        assert_eq!(a.serialized_size(), FixedPipelineState::FULL_SIZE);
        assert_ne!(a, b);
        assert_ne!(hash_state(&a), hash_state(&b));
    }

    fn make_test_draw_call() -> DrawCall {
        DrawCall {
            topology: PrimitiveTopology::Triangles,
            vertex_first: 0,
            vertex_count: 0,
            indexed: false,
            index_buffer_addr: 0,
            index_buffer_addr_end: 0,
            index_buffer_count: 0,
            index_buffer_first: 0,
            index_format: IndexFormat::UnsignedInt,
            vertex_streams: Default::default(),
            vertex_stream_instances: Default::default(),
            vertex_stream_limits: Default::default(),
            viewports: [ViewportInfo::default(); NUM_VIEWPORTS],
            viewport_transforms: Default::default(),
            scissors: [ScissorInfo::default(); NUM_VIEWPORTS],
            viewport_scale_offset_enabled: false,
            window_origin_lower_left: false,
            window_origin_flip_y: false,
            surface_clip: Default::default(),
            blend: [BlendInfo::default(); NUM_RENDER_TARGETS],
            blend_per_target_enabled: false,
            global_blend: BlendInfo::default(),
            iterated_blend_enabled: false,
            blend_color: BlendColorInfo {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            },
            depth_stencil: DepthStencilInfo {
                depth_test_enable: false,
                depth_write_enable: false,
                depth_func: ComparisonOp::Always,
                depth_mode: DepthMode::MinusOneToOne,
                stencil_enable: false,
                stencil_two_side: false,
                front: StencilFaceInfo::default(),
                back: StencilFaceInfo::default(),
            },
            rasterizer: RasterizerInfo {
                cull_enable: false,
                front_face: FrontFace::CCW,
                cull_face: CullFace::Back,
                polygon_mode_front: PolygonMode::Fill,
                polygon_mode_back: PolygonMode::Fill,
                line_width_smooth: 1.0,
                line_width_aliased: 1.0,
                depth_bias: 0.0,
                slope_scale_depth_bias: 0.0,
                depth_bias_clamp: 0.0,
                ..RasterizerInfo::default()
            },
            rasterize_enable: true,
            primitive_restart: Default::default(),
            logic_op: LogicOpInfo::default(),
            depth_clamp_enabled: true,
            conservative_raster_enable: false,
            engine_state: crate::engines::maxwell_3d::EngineHint::None,
            provoking_vertex_last: false,
            depth_bounds_enable: false,
            depth_bounds: [0.0, 1.0],
            mandated_early_z: false,
            alpha_test_enabled: false,
            alpha_test_func: ComparisonOp::Always,
            alpha_test_ref: 0.0,
            point_size: 1.0,
            tessellation_primitive: 0,
            tessellation_spacing: 0,
            tessellation_clockwise: false,
            patch_vertices: 1,
            anti_alias_samples_mode: 0,
            anti_alias_alpha_control: AntiAliasAlphaControlInfo::default(),
            line_anti_alias_enable: false,
            line_stipple: Default::default(),
            program_base_address: 0,
            cb_bindings: [[ConstBufferInfo::default(); 18]; 5],
            vertex_attribs: Default::default(),
            shader_stages: [ShaderStageInfo::default(); 6],
            color_masks: [ColorMaskInfo::default(); NUM_RENDER_TARGETS],
            rt_control: RtControlInfo::default(),
            tex_header_pool_addr: 0,
            tex_header_pool_limit: 0,
            tex_sampler_pool_addr: 0,
            tex_sampler_pool_limit: 0,
            instance_count: 1,
            base_instance: 0,
            base_vertex: 0,
            inline_index_data: Vec::new(),
            sampler_binding: SamplerBinding::Independently,
            render_targets: [RenderTargetInfo::default(); NUM_RENDER_TARGETS],
            zeta: ZetaInfo::default(),
            transform_feedback_enabled: false,
            transform_feedback_state: Default::default(),
            dirty_flags: [true; 256],
        }
    }

    #[test]
    fn test_default_state_is_consistent() {
        let a = FixedPipelineState::default();
        let b = FixedPipelineState::default();
        assert_eq!(a, b);
        assert_eq!(hash_state(&a), hash_state(&b));
    }

    #[test]
    fn refresh_preserves_sparse_attribute_location_and_instance_divisor() {
        let mut draw = make_test_draw_call();
        draw.vertex_streams[5] = VertexStreamInfo {
            // Array position owns the Maxwell binding. Keep the redundant
            // snapshot field at its default to catch accidental renumbering.
            index: 0,
            address: 0x1000,
            stride: 32,
            frequency: 5,
            enabled: true,
        };
        draw.vertex_stream_instances[5] = 1;
        draw.vertex_attribs[7] = VertexAttribInfo {
            buffer_index: 5,
            constant: false,
            offset: 12,
            size: VertexAttribSize::R32G32,
            attrib_type: VertexAttribType::Float,
            bgra: false,
        };

        let mut state = FixedPipelineState::default();
        state.refresh_draw_call_for_test(&draw, &DynamicFeatures::default());

        assert_eq!(state.binding_divisors[5], 5);
        assert_ne!(state.attribute_types_or_enabled_divisors & (1 << 5), 0);
        assert_eq!(state.vertex_strides[5], 32);
        assert_eq!(
            state.attributes[0].attrib_type(),
            VertexAttribType::Invalid.to_raw()
        );
        assert!(state.attributes[7].is_enabled());
        assert_eq!(
            state.attributes[7].attrib_type(),
            VertexAttribType::Float.to_raw()
        );
        assert_eq!(state.attributes[7].buffer(), 5);
        assert_eq!(state.attributes[7].offset(), 12);
    }

    #[test]
    fn refresh_consumes_and_reuses_upstream_dirty_gated_state() {
        let draw = make_test_draw_call();
        let draw_state = DrawState {
            topology: draw.topology,
            draw_mode: DrawMode::General,
            draw_indexed: draw.indexed,
            base_index: draw.base_vertex as u32,
            vertex_buffer: VertexBuffer {
                first: draw.vertex_first,
                count: draw.vertex_count,
            },
            index_buffer: IndexBuffer {
                first: draw.index_buffer_first,
                count: draw.index_buffer_count,
                format: draw.index_format,
            },
            base_instance: draw.base_instance,
            instance_count: draw.instance_count,
            inline_index_draw_indexes: draw.inline_index_data.clone(),
        };
        let registers = Maxwell3DDrawRegisters::from_draw_call(&draw);
        let mut draw_view =
            Maxwell3DDrawView::with_register_snapshot(&draw_state, draw.indexed, registers);
        let mut state = FixedPipelineState::default();

        state.refresh(&mut draw_view, &DynamicFeatures::default());
        let flags = draw_view.dirty_flags();
        assert!(!flags[super::super::state_tracker::dirty::VERTEX_INPUT as usize]);
        assert!(!flags[super::super::state_tracker::dirty::VIEWPORT_SWIZZLES as usize]);
        assert!(!flags[super::super::state_tracker::dirty::BLENDING as usize]);

        state.attributes[0].raw = 0x1234_5678;
        state.attachments[0].raw = 0x8765_4321;
        state.viewport_swizzles[0] = 0xCAFE;
        state.refresh(&mut draw_view, &DynamicFeatures::default());

        assert_eq!(state.attributes[0].raw, 0x1234_5678);
        assert_eq!(state.attachments[0].raw, 0x8765_4321);
        assert_eq!(state.viewport_swizzles[0], 0xCAFE);
    }

    #[test]
    fn test_different_topology_gives_different_hash() {
        let mut a = FixedPipelineState::default();
        let mut b = FixedPipelineState::default();
        a.set_topology(PrimitiveTopology::Triangles);
        b.set_topology(PrimitiveTopology::Lines);
        assert_ne!(a, b);
        assert_ne!(hash_state(&a), hash_state(&b));
    }

    #[test]
    fn refresh_packs_raw_maxwell_attribute_type_and_size() {
        use crate::engines::maxwell_3d::{VertexAttribSize, VertexAttribType};

        let mut draw = make_test_draw_call();
        draw.vertex_attribs[0].constant = false;
        draw.vertex_attribs[0].attrib_type = VertexAttribType::Float;
        draw.vertex_attribs[0].size = VertexAttribSize::R32G32B32A32;

        let mut state = FixedPipelineState::default();
        state.refresh_draw_call_for_test(&draw, &DynamicFeatures::default());

        // The packed bits must hold the raw Maxwell encodings (upstream
        // `attribute.type.Assign(input.type.Value())`), so `from_raw` on
        // read-back returns the original enum. Rust enum ordinals here read
        // Float back as SScaled and R32G32B32A32 back as Invalid.
        assert_eq!(state.attributes[0].attrib_type(), 7);
        assert_eq!(state.attributes[0].attrib_size(), 0x01);
        assert_eq!(
            VertexAttribType::from_raw(state.attributes[0].attrib_type()),
            VertexAttribType::Float
        );
        assert_eq!(
            VertexAttribSize::from_raw(state.attributes[0].attrib_size()),
            VertexAttribSize::R32G32B32A32
        );
    }

    #[test]
    fn refresh_preserves_color_mask_when_blending_is_disabled() {
        let mut draw = make_test_draw_call();
        draw.blend[0].enabled = false;
        draw.color_masks[0] = ColorMaskInfo {
            r: true,
            g: false,
            b: true,
            a: false,
        };

        let mut state = FixedPipelineState::default();
        state.refresh_draw_call_for_test(&draw, &DynamicFeatures::default());

        assert!(!state.attachments[0].is_enabled());
        assert_eq!(state.attachments[0].mask(), [true, false, true, false]);

        let mut bytes = Vec::new();
        state.write_prefix_bytes(&mut bytes);
        let path = std::env::temp_dir().join(format!(
            "ruzu-fixed-pipeline-state-color-mask-{}.bin",
            std::process::id()
        ));
        std::fs::write(&path, bytes).unwrap();
        let mut file = std::fs::File::open(&path).unwrap();
        let decoded = FixedPipelineState::read_from_file(&mut file).unwrap();
        let _ = std::fs::remove_file(path);

        assert!(!decoded.attachments[0].is_enabled());
        assert_eq!(decoded.attachments[0].mask(), [true, false, true, false]);
    }

    #[test]
    fn refresh_applies_upstream_squashed_iterated_blend_override() {
        let previous = common::settings::values().use_squashed_iterated_blend;
        common::settings::values_mut().use_squashed_iterated_blend = true;

        let mut draw = make_test_draw_call();
        draw.blend[0] = BlendInfo {
            enabled: true,
            separate_alpha: true,
            color_op: BlendEquation::Subtract,
            color_src: BlendFactor::SrcAlpha,
            color_dst: BlendFactor::OneMinusSrcAlpha,
            alpha_op: BlendEquation::Max,
            alpha_src: BlendFactor::DstAlpha,
            alpha_dst: BlendFactor::OneMinusDstAlpha,
        };
        draw.global_blend = draw.blend[0];
        draw.blend_per_target_enabled = false;
        draw.iterated_blend_enabled = true;

        let mut state = FixedPipelineState::default();
        state.refresh_draw_call_for_test(&draw, &DynamicFeatures::default());
        let attachment = state.attachments[0];

        common::settings::values_mut().use_squashed_iterated_blend = previous;

        assert!(attachment.is_enabled());
        assert_eq!(attachment.equation_rgb(), BlendEquation::Add);
        assert_eq!(attachment.equation_alpha(), BlendEquation::Add);
        assert_eq!(attachment.source_rgb_factor(), BlendFactor::One);
        assert_eq!(attachment.dest_rgb_factor(), BlendFactor::One);
        assert_eq!(
            attachment.source_alpha_factor(),
            BlendFactor::OneMinusSrcColor
        );
        assert_eq!(attachment.dest_alpha_factor(), BlendFactor::Zero);
    }

    #[test]
    fn refresh_leaves_extension_covered_fields_zero_like_upstream() {
        // Upstream fixed_pipeline_state.cpp:142-165: fields covered by a
        // supported dynamic-state extension are never refreshed into the
        // key. Two draws differing only in dynamic state must produce the
        // SAME FixedPipelineState under the extension (one pipeline, not
        // hundreds of variants).
        let mut draw = make_test_draw_call();
        draw.rasterizer.cull_enable = true;
        draw.depth_stencil.depth_test_enable = true;
        draw.depth_stencil.depth_write_enable = true;
        draw.vertex_streams[0].stride = 32;

        let features = DynamicFeatures {
            has_extended_dynamic_state: true,
            ..Default::default()
        };
        let mut state = FixedPipelineState::default();
        state.refresh_draw_call_for_test(&draw, &features);

        // EDS1-covered dynamic_state fields stay zero…
        assert!(!state.dynamic_state.cull_enable());
        assert!(!state.dynamic_state.depth_test_enable());
        assert!(!state.dynamic_state.depth_write_enable());
        // …the extension flag is recorded in the key…
        assert!(state.extended_dynamic_state());
        // …and vertex strides stay out of the key because they are supplied
        // dynamically by vkCmdBindVertexBuffers2.
        assert_eq!(state.vertex_strides[0], 0);

        // Non-covered groups are still captured (eds2/eds3 unsupported here).
        let mut logic_draw = make_test_draw_call();
        logic_draw.logic_op = LogicOpInfo {
            enabled: true,
            op: 0x1503,
        };
        let mut logic_state = FixedPipelineState::default();
        logic_state.refresh_draw_call_for_test(&logic_draw, &features);
        assert!(logic_state.dynamic_state.logic_op_enable());

        // Two draws differing only in EDS1-covered dynamic state →
        // identical keys.
        let mut other_draw = make_test_draw_call();
        other_draw.rasterizer.cull_enable = false;
        other_draw.depth_stencil.depth_test_enable = false;
        other_draw.vertex_streams[0].stride = 64;
        let mut other_state = FixedPipelineState::default();
        other_state.refresh_draw_call_for_test(&other_draw, &features);
        assert_eq!(state, other_state);
        assert_eq!(hash_state(&state), hash_state(&other_state));
    }

    #[test]
    fn radv_dynamic_feature_key_matches_upstream_capability_mask() {
        let features = DynamicFeatures {
            has_extended_dynamic_state: true,
            has_extended_dynamic_state_2: true,
            has_extended_dynamic_state_2_logic_op: true,
            has_extended_dynamic_state_3_blend: false,
            has_extended_dynamic_state_3_enables: true,
            has_color_write_enable: false,
            has_dynamic_vertex_input: true,
            ..Default::default()
        };
        let mut state = FixedPipelineState::default();
        state.refresh_draw_call_for_test(&make_test_draw_call(), &features);

        assert_eq!(state.raw1 & 0x3f, 0x37);
        assert_eq!(
            state.serialized_size(),
            FixedPipelineState::ATTRIBUTES_OFFSET
        );
    }

    #[test]
    fn refresh_captures_upstream_dynamic_state_refresh2_refresh3_fields() {
        let mut draw = make_test_draw_call();
        draw.rasterize_enable = false;
        draw.logic_op = LogicOpInfo {
            enabled: true,
            op: 0x1503,
        };
        draw.depth_clamp_enabled = false;
        draw.conservative_raster_enable = true;
        draw.engine_state = crate::engines::maxwell_3d::EngineHint::OnHleMacro;
        draw.provoking_vertex_last = true;
        draw.depth_bounds_enable = true;
        draw.viewport_transforms[0].swizzle = 0x3210;
        draw.topology = PrimitiveTopology::Lines;
        draw.rasterizer.depth_bias = 1.0;
        draw.rasterizer.polygon_offset_point_enable = true;
        draw.rasterizer.polygon_offset_line_enable = false;
        draw.rasterizer.polygon_offset_fill_enable = true;

        let features = DynamicFeatures {
            has_provoking_vertex: true,
            has_provoking_vertex_first_mode: true,
            has_provoking_vertex_last_mode: true,
            ..Default::default()
        };
        let mut state = FixedPipelineState::default();
        state.refresh_draw_call_for_test(&draw, &features);

        assert!(!state.dynamic_state.rasterize_enable());
        assert!(!state.dynamic_state.depth_bias_enable());
        assert!(state.dynamic_state.logic_op_enable());
        assert_eq!(state.dynamic_state.logic_op(), 0x1503);
        assert!(state.dynamic_state.depth_clamp_disabled());
        assert!(state.conservative_raster_enable());
        assert!(state.provoking_vertex_last());
        assert!(state.dynamic_state.depth_bounds_enable());
        assert_eq!(
            state.app_stage(),
            crate::engines::maxwell_3d::EngineHint::OnHleMacro as u32
        );
        assert_eq!(state.viewport_swizzles[0], 0x3210);

        draw.rasterizer.polygon_offset_line_enable = true;
        state.refresh_draw_call_for_test(&draw, &features);
        assert!(state.dynamic_state.depth_bias_enable());
    }

    #[test]
    fn refresh_collapses_topology_only_when_eds1_and_eds2_are_available() {
        let mut draw = make_test_draw_call();
        draw.topology = PrimitiveTopology::TriangleStrip;

        let mut state = FixedPipelineState::default();
        state.refresh_draw_call_for_test(&draw, &DynamicFeatures::default());
        assert_eq!(state.topology(), PrimitiveTopology::TriangleStrip);

        state.refresh_draw_call_for_test(
            &draw,
            &DynamicFeatures {
                has_extended_dynamic_state: true,
                has_extended_dynamic_state_2: true,
                ..Default::default()
            },
        );
        assert_eq!(state.topology(), PrimitiveTopology::Triangles);
    }

    #[test]
    fn refresh3_keeps_only_unsupported_granular_state_in_the_key() {
        let mut draw = make_test_draw_call();
        draw.logic_op.enabled = true;
        draw.depth_clamp_enabled = false;
        draw.line_stipple.enabled = true;

        let mut state = FixedPipelineState::default();
        state.refresh_draw_call_for_test(
            &draw,
            &DynamicFeatures {
                has_dynamic_state3_logic_op_enable: true,
                has_dynamic_state3_depth_clamp_enable: false,
                has_dynamic_state3_line_stipple_enable: true,
                ..Default::default()
            },
        );

        assert!(!state.dynamic_state.logic_op_enable());
        assert!(state.dynamic_state.depth_clamp_disabled());
        assert!(!state.dynamic_state.line_stipple_enable());
    }

    #[test]
    fn refresh_preserves_upstream_full_state_tail_and_dual_source_bit() {
        let mut draw = make_test_draw_call();
        draw.transform_feedback_enabled = true;
        draw.depth_bounds = [3.75, 9.5];
        draw.line_stipple.factor = 7;
        draw.line_stipple.pattern = 0xa55a;
        draw.blend[0].enabled = true;
        draw.blend[0].color_src = BlendFactor::Src1Color;

        let mut state = FixedPipelineState::default();
        state.refresh_draw_call_for_test(
            &draw,
            &DynamicFeatures {
                driver_id: 42,
                driver_version: 73,
                ..Default::default()
            },
        );

        assert_eq!(state.driver_id, 42);
        assert_eq!(state.driver_version, 73);
        assert_eq!(state.depth_bounds_min, 3);
        assert_eq!(state.depth_bounds_max, 9);
        assert_eq!(state.line_stipple_factor, 7);
        assert_eq!(state.line_stipple_pattern, 0xa55a);
        assert!(state.attachment0_dual_source_blend());
        assert_eq!(state.serialized_size(), FixedPipelineState::FULL_SIZE);
    }

    #[test]
    fn provoking_vertex_falls_back_like_upstream_with_transform_feedback() {
        let mut draw = make_test_draw_call();
        draw.provoking_vertex_last = true;
        draw.transform_feedback_enabled = true;

        let mut state = FixedPipelineState::default();
        state.refresh_draw_call_for_test(
            &draw,
            &DynamicFeatures {
                has_provoking_vertex: true,
                has_provoking_vertex_first_mode: true,
                has_provoking_vertex_last_mode: true,
                has_provoking_vertex_tf_preserve: false,
                ..Default::default()
            },
        );
        assert!(!state.provoking_vertex_last());

        state.refresh_draw_call_for_test(
            &draw,
            &DynamicFeatures {
                has_provoking_vertex: true,
                has_provoking_vertex_first_mode: true,
                has_provoking_vertex_last_mode: true,
                has_provoking_vertex_tf_preserve: true,
                ..Default::default()
            },
        );
        assert!(state.provoking_vertex_last());
    }

    #[test]
    fn refresh_preserves_zero_patch_vertices_wrapping_bit_pattern() {
        let mut draw = make_test_draw_call();
        draw.patch_vertices = 0;

        let mut state = FixedPipelineState::default();
        state.refresh_draw_call_for_test(&draw, &DynamicFeatures::default());

        // Upstream assigns `(u32{0} - 1)` to a five-bit bitfield.
        assert_eq!(state.patch_control_points_minus_one(), 0x1f);
        assert_eq!(state.patch_control_points(), 32);
    }

    #[test]
    fn test_pack_unpack_comparison_op() {
        for i in 0..8u32 {
            let op = unpack_comparison_op(i);
            assert_eq!(pack_comparison_op(op), i);
        }
    }

    #[test]
    fn test_pack_unpack_stencil_op() {
        for i in 0..8u32 {
            let op = unpack_stencil_op(i);
            assert_eq!(pack_stencil_op(op), i);
        }
    }

    #[test]
    fn test_pack_unpack_blend_equation() {
        for i in 0..5u32 {
            let eq = unpack_blend_equation(i);
            assert_eq!(pack_blend_equation(eq), i);
        }
    }

    #[test]
    fn test_pack_unpack_blend_factor() {
        for i in 0..19u32 {
            let f = unpack_blend_factor(i);
            assert_eq!(pack_blend_factor(f), i);
        }
    }

    #[test]
    fn test_pack_unpack_cull_face() {
        for i in 0..3u32 {
            let c = unpack_cull_face(i);
            assert_eq!(pack_cull_face(c), i);
        }
    }

    #[test]
    fn test_pack_unpack_front_face() {
        assert_eq!(pack_front_face(unpack_front_face(0)), 0);
        assert_eq!(pack_front_face(unpack_front_face(1)), 1);
    }

    #[test]
    fn test_pack_unpack_polygon_mode() {
        for i in 0..3u32 {
            let m = unpack_polygon_mode(i);
            assert_eq!(pack_polygon_mode(m), i);
        }
    }

    #[test]
    fn test_blending_attachment_bitfields() {
        let mut att = BlendingAttachment::default();
        att.set_mask(true, false, true, false);
        att.set_equation_rgb(BlendEquation::Subtract);
        att.set_source_rgb_factor(BlendFactor::SrcAlpha);
        att.set_dest_rgb_factor(BlendFactor::OneMinusSrcAlpha);
        att.set_enabled(true);

        let mask = att.mask();
        assert!(mask[0]);
        assert!(!mask[1]);
        assert!(mask[2]);
        assert!(!mask[3]);
        assert_eq!(att.equation_rgb(), BlendEquation::Subtract);
        assert_eq!(att.source_rgb_factor(), BlendFactor::SrcAlpha);
        assert_eq!(att.dest_rgb_factor(), BlendFactor::OneMinusSrcAlpha);
        assert!(att.is_enabled());
    }

    #[test]
    fn test_vertex_attribute_bitfields() {
        let mut attr = VertexAttribute::default();
        attr.set_enabled(true);
        attr.set_buffer(5);
        attr.set_offset(128);
        attr.set_type(3);
        attr.set_size(12);

        assert!(attr.is_enabled());
        assert_eq!(attr.buffer(), 5);
        assert_eq!(attr.offset(), 128);
        assert_eq!(attr.attrib_type(), 3);
        assert_eq!(attr.attrib_size(), 12);
    }

    #[test]
    fn test_dynamic_state_bitfields() {
        let mut ds = DynamicState::default();
        ds.set_cull_enable(true);
        ds.set_cull_face(CullFace::Back);
        ds.set_depth_test_enable(true);
        ds.set_depth_test_func(ComparisonOp::Less);
        ds.set_front_face(FrontFace::CCW);
        ds.set_stencil_enable(true);

        assert!(ds.cull_enable());
        assert_eq!(ds.cull_face(), CullFace::Back);
        assert!(ds.depth_test_enable());
        assert_eq!(ds.depth_test_func(), ComparisonOp::Less);
        assert_eq!(ds.front_face(), FrontFace::CCW);
        assert!(ds.stencil_enable());
    }

    #[test]
    fn refresh_flips_front_face_when_window_origin_flip_y_matches_upstream() {
        let mut draw = make_test_draw_call();
        draw.rasterizer.front_face = FrontFace::CCW;
        draw.window_origin_flip_y = false;

        let mut state = FixedPipelineState::default();
        state.refresh_draw_call_for_test(&draw, &DynamicFeatures::default());
        assert_eq!(state.dynamic_state.front_face(), FrontFace::CCW);

        draw.window_origin_flip_y = true;
        state.refresh_draw_call_for_test(&draw, &DynamicFeatures::default());
        assert_eq!(state.dynamic_state.front_face(), FrontFace::CW);
    }

    #[test]
    fn test_refresh_preserves_zeta_pipeline_state() {
        let no_zeta_draw = make_test_draw_call();
        let mut zeta_draw = no_zeta_draw.clone();
        zeta_draw.zeta.enabled = true;
        zeta_draw.zeta.format = 7;

        let mut no_zeta = FixedPipelineState::default();
        no_zeta.refresh_draw_call_for_test(&no_zeta_draw, &DynamicFeatures::default());
        let mut with_zeta = FixedPipelineState::default();
        with_zeta.refresh_draw_call_for_test(&zeta_draw, &DynamicFeatures::default());

        assert!(!no_zeta.depth_enabled());
        assert_eq!(no_zeta.depth_format(), 0);
        assert!(with_zeta.depth_enabled());
        assert_eq!(with_zeta.depth_format(), 7);
        assert_ne!(hash_state(&no_zeta), hash_state(&with_zeta));
    }

    #[test]
    fn hash_includes_blend_when_dynamic_vertex_input_without_dynamic_blend() {
        let mut a = FixedPipelineState::default();
        let mut b = FixedPipelineState::default();
        a.set_dynamic_vertex_input(true);
        b.set_dynamic_vertex_input(true);
        b.attachments[0].set_enabled(true);
        b.attachments[0].set_source_rgb_factor(BlendFactor::SrcAlpha);

        assert_ne!(hash_state(&a), hash_state(&b));
    }

    #[test]
    fn hash_excludes_blend_when_dynamic_vertex_input_and_dynamic_blend() {
        let mut a = FixedPipelineState::default();
        let mut b = FixedPipelineState::default();
        a.set_dynamic_vertex_input(true);
        b.set_dynamic_vertex_input(true);
        a.set_extended_dynamic_state_3_blend(true);
        b.set_extended_dynamic_state_3_blend(true);
        b.attachments[0].set_enabled(true);
        b.attachments[0].set_source_rgb_factor(BlendFactor::SrcAlpha);

        assert_eq!(hash_state(&a), hash_state(&b));
    }

    #[test]
    fn hash_excludes_attributes_when_dynamic_vertex_input() {
        let mut a = FixedPipelineState::default();
        let mut b = FixedPipelineState::default();
        a.set_dynamic_vertex_input(true);
        b.set_dynamic_vertex_input(true);
        b.attributes[0].set_enabled(true);
        b.binding_divisors[0] = 7;
        b.vertex_strides[0] = 16;

        assert_eq!(hash_state(&a), hash_state(&b));
    }

    #[test]
    fn hash_excludes_vertex_strides_when_extended_dynamic_state() {
        let mut a = FixedPipelineState::default();
        let mut b = FixedPipelineState::default();
        a.set_extended_dynamic_state(true);
        b.set_extended_dynamic_state(true);
        b.vertex_strides[0] = 16;

        assert_eq!(hash_state(&a), hash_state(&b));
    }

    #[test]
    fn hash_includes_vertex_strides_without_extended_dynamic_state() {
        let a = FixedPipelineState::default();
        let mut b = FixedPipelineState::default();
        b.vertex_strides[0] = 16;

        assert_ne!(hash_state(&a), hash_state(&b));
    }

    #[test]
    fn test_polygon_offset_lut() {
        assert_eq!(
            POLYGON_OFFSET_ENABLE_LUT[PrimitiveTopology::Points as usize],
            POINT
        );
        assert_eq!(
            POLYGON_OFFSET_ENABLE_LUT[PrimitiveTopology::Lines as usize],
            LINE
        );
        assert_eq!(
            POLYGON_OFFSET_ENABLE_LUT[PrimitiveTopology::Triangles as usize],
            POLYGON
        );
        assert_eq!(
            POLYGON_OFFSET_ENABLE_LUT[PrimitiveTopology::Patches as usize],
            POLYGON
        );
    }
}
