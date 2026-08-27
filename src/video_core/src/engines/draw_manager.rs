// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Port of the `DrawManager` declaration in video_core/engines/maxwell_3d.h
//! and its implementation in video_core/engines/draw_manager.cpp.
//!
//! The DrawManager handles draw call dispatch for the Maxwell 3D engine.
//! It processes method writes for begin/end draws, inline index buffers,
//! instanced draws, and draw textures.

use crate::dirty_flags::flags as Dirty;
use crate::engines::const_buffer_info::ConstBufferInfo;
use crate::engines::maxwell_3d::{
    DrawCall, Maxwell3D, RenderTargetInfo, RtControlInfo, ShaderStageType, CLEAR_SURFACE,
    DRAW_BEGIN, DRAW_END, DRAW_INLINE_INDEX, DRAW_TEXTURE_SRC_Y0, IB_BASE, IB_OFF_COUNT,
    IB_OFF_FIRST, INDEX_BUFFER16_FIRST, INDEX_BUFFER16_SUBSEQUENT, INDEX_BUFFER32_FIRST,
    INDEX_BUFFER32_SUBSEQUENT, INDEX_BUFFER8_FIRST, INDEX_BUFFER8_SUBSEQUENT,
    INLINE_INDEX_2X16_EVEN, INLINE_INDEX_4X8_INDEX0, MAX_CB_SLOTS, NUM_SHADER_PROGRAMS,
    NUM_SHADER_STAGES, TOPOLOGY_OVERRIDE, VB_COUNT, VB_FIRST, VERTEX_ARRAY_INSTANCE_FIRST,
    VERTEX_ARRAY_INSTANCE_SUBSEQUENT,
};
use crate::rasterizer_interface::RasterizerInterface;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;

/// GPU virtual address type.
pub type GPUVAddr = u64;

// Upstream stores these enums in `Maxwell3D::Regs`; re-export them from the
// Rust counterpart so existing draw-manager consumers retain their imports.
pub use crate::engines::maxwell_3d::{
    PrimitiveTopology, PrimitiveTopologyControl, PrimitiveTopologyOverride,
};

/// Port of `DrawManager::UpdateTopology()` as a pure owner-local helper.
///
/// This keeps the topology-resolution logic in the matching upstream owner
/// file even when other owners need the same resolved draw-state topology.
pub fn resolve_draw_topology(
    mut draw_topology: PrimitiveTopology,
    primitive_topology_control: PrimitiveTopologyControl,
    topology_override: PrimitiveTopologyOverride,
    topology_override_raw: u32,
) -> PrimitiveTopology {
    match primitive_topology_control {
        PrimitiveTopologyControl::UseInBeginMethods => {}
        PrimitiveTopologyControl::UseSeparateState => match topology_override {
            PrimitiveTopologyOverride::None if topology_override_raw == 0 => {}
            PrimitiveTopologyOverride::Points => {
                draw_topology = PrimitiveTopology::Points;
            }
            PrimitiveTopologyOverride::Lines => {
                draw_topology = PrimitiveTopology::Lines;
            }
            PrimitiveTopologyOverride::LineStrip => {
                draw_topology = PrimitiveTopology::LineStrip;
            }
            _ => {
                draw_topology = PrimitiveTopology::from_raw(topology_override_raw);
            }
        },
    }

    draw_topology
}

// `IndexFormat` is the upstream-faithful enum from
// `engines::maxwell_3d` (matching `Maxwell3D::Regs::IndexFormat`).
pub use crate::engines::maxwell_3d::IndexFormat;

/// Vertex buffer register state.
#[derive(Debug, Clone, Copy, Default)]
pub struct VertexBuffer {
    pub first: u32,
    pub count: u32,
}

/// Index buffer register state.
#[derive(Debug, Clone, Copy, Default)]
pub struct IndexBuffer {
    pub first: u32,
    pub count: u32,
    pub format: IndexFormat,
}

/// Small (packed) index buffer parameters, decoded from a single u32.
///
/// Upstream `Maxwell3D::Regs::IndexBufferSmall`:
///   bits [0:15]  = first
///   bits [16:27] = count
///   bits [28:31] = topology (PrimitiveTopology)
#[derive(Debug, Clone, Copy)]
pub struct IndexBufferSmall {
    pub first: u32,
    pub count: u32,
    pub topology: PrimitiveTopology,
}

impl IndexBufferSmall {
    /// Decode from a raw u32 argument (upstream `IndexBufferSmall{argument}`).
    ///
    /// Bit layout from upstream maxwell_3d.h:
    ///   BitField<0, 16, u32> first
    ///   BitField<16, 12, u32> count
    ///   BitField<28, 4, PrimitiveTopology> topology
    pub fn from_raw(argument: u32) -> Self {
        Self {
            first: argument & 0xFFFF,
            count: (argument >> 16) & 0xFFF,
            topology: PrimitiveTopology::from_raw((argument >> 28) & 0xF),
        }
    }
}

// ── Maxwell3D register access trait ─────────────────────────────────────────

/// Trait providing access to Maxwell3D state needed by DrawManager.
///
/// Upstream DrawManager methods receive a `Maxwell3D&` and access its
/// registers, dirty flags, rasterizer, and `ShouldExecute()`. Rust uses a
/// trait for that same per-call relationship.
pub trait Maxwell3DAccess {
    /// Whether conditional rendering allows execution.
    /// Upstream: `maxwell3d->ShouldExecute()`.
    fn should_execute(&self) -> bool;

    /// Read the global_base_instance_index register.
    fn global_base_instance_index(&self) -> u32;

    /// Read the global_base_vertex_index register.
    fn global_base_vertex_index(&self) -> u32;

    /// Read the index_buffer register state.
    fn index_buffer(&self) -> IndexBuffer;

    /// Read the vertex_buffer register state.
    fn vertex_buffer(&self) -> VertexBuffer;

    /// Read the primitive_topology_control register.
    fn primitive_topology_control(&self) -> PrimitiveTopologyControl;

    /// Read the topology_override register.
    fn topology_override(&self) -> PrimitiveTopologyOverride;

    /// Read the raw topology_override value for the default match arm.
    fn topology_override_raw(&self) -> u32;

    /// Read the draw.topology register (for DrawBegin).
    fn draw_topology(&self) -> PrimitiveTopology;

    /// Read the draw.instance_id register: returns (is_first, is_subsequent).
    fn draw_instance_id(&self) -> (bool, bool);

    /// Decode the current packed `inline_index_2x16` register into its two
    /// 16-bit values, matching upstream register-field access.
    fn inline_index_2x16_values(&self) -> (u32, u32);

    /// Decode the current packed `inline_index_4x8` register into its four
    /// byte values, matching upstream register-field access.
    fn inline_index_4x8_values(&self) -> [u32; 4];

    /// Decode `vertex_array_instance_first` from the written argument.
    fn vertex_array_instance_first_params(&self, argument: u32) -> (PrimitiveTopology, u32, u32);

    /// Decode `vertex_array_instance_subsequent` from the written argument.
    fn vertex_array_instance_subsequent_params(
        &self,
        argument: u32,
    ) -> (PrimitiveTopology, u32, u32);

    /// Set a dirty flag. Upstream: `maxwell3d->dirty.flags[index] = true`.
    fn set_dirty_flag(&mut self, index: u8);

    /// Borrow dirty flags for draw-time rasterizer state synchronization,
    /// matching upstream rasterizer references to `maxwell3d->dirty.flags`.
    fn dirty_flags(&self) -> &[bool; 256];

    /// Read one dirty flag without materializing the complete flag array.
    fn dirty_flag(&self, index: u8) -> bool;

    /// Clear one dirty flag after the backend consumes it.
    fn clear_dirty_flag(&mut self, index: u8);

    /// Mutate `regs.logic_op.enable`.
    ///
    /// Upstream's AMD workaround writes this register from
    /// `RasterizerVulkan::UpdateDynamicStates`, so the draw-time view must
    /// preserve that mutation on the live Maxwell3D owner.
    fn set_logic_op_enabled(&mut self, _enabled: bool) {}

    /// Return the stable channel-owned dirty flag storage for backends that
    /// mirror upstream's persistent `Maxwell3D*` ownership.
    fn dirty_flags_ptr(&mut self) -> Option<std::ptr::NonNull<[bool; 256]>> {
        None
    }

    /// Run a closure with the bound rasterizer, if present.
    ///
    /// Upstream stores `RasterizerInterface*` on `Maxwell3D` and all draw
    /// paths dispatch through that owner. Keeping the callback on
    /// `Maxwell3DAccess` avoids leaking a mutable rasterizer reference into
    /// `DrawManager` while remaining object-safe for the trait boundary.
    fn with_rasterizer_mut(&mut self, f: &mut dyn FnMut(&mut dyn RasterizerInterface)) -> bool;

    /// Dispatch a draw through the bound rasterizer using the draw-time
    /// Maxwell3D view. Upstream `DrawManager::ProcessDraw` calls
    /// `maxwell3d->rasterizer->Draw(...)`; keeping this as a Maxwell3D-owned
    /// operation avoids spreading rasterizer ownership through DrawManager.
    fn draw_rasterizer(
        &mut self,
        draw_state: &DrawState,
        draw_indexed: bool,
        instance_count: u32,
    ) -> bool {
        let mut dispatched = false;
        self.with_rasterizer_mut(&mut |rasterizer| {
            rasterizer.draw(
                Maxwell3DDrawView::new(draw_state, draw_indexed),
                instance_count,
            );
            dispatched = true;
        });
        dispatched
    }

    /// Dispatch a clear through the bound rasterizer using the clear-time
    /// Maxwell3D view. Upstream `DrawManager::Clear` calls
    /// `maxwell3d->rasterizer->Clear(layer_count)` and the backend reads
    /// Maxwell3D registers directly; the concrete Maxwell3D implementation
    /// passes a live view while the default trait fallback keeps test fakes
    /// working with an empty snapshot.
    fn clear_rasterizer(&mut self, layer_count: u32) -> bool {
        let mut dispatched = false;
        self.with_rasterizer_mut(&mut |rasterizer| {
            rasterizer.clear(Maxwell3DClearView::default(), layer_count);
            dispatched = true;
        });
        dispatched
    }

    /// Dispatch an indirect draw through the bound rasterizer.
    fn draw_indirect_rasterizer(
        &mut self,
        draw_state: &DrawState,
        indirect_params: &IndirectParams,
    ) -> bool {
        let mut dispatched = false;
        self.with_rasterizer_mut(&mut |rasterizer| {
            rasterizer.draw_indirect(Maxwell3DIndirectView::new(draw_state, indirect_params));
            dispatched = true;
        });
        dispatched
    }

    /// Dispatch a draw-texture operation through the bound rasterizer.
    fn draw_texture_rasterizer(
        &mut self,
        draw_state: &DrawState,
        draw_texture_state: DrawTextureState,
    ) -> bool {
        let mut dispatched = false;
        self.with_rasterizer_mut(&mut |rasterizer| {
            rasterizer.draw_texture(Maxwell3DDrawTextureView::new(
                draw_state,
                draw_texture_state,
            ));
            dispatched = true;
        });
        dispatched
    }

    /// Snapshot per-stage shader program GPU virtual addresses.
    /// Disabled stages report `0`. Default impl returns all zeros for tests
    /// and stub access types that do not yet plumb shader-program registers.
    fn shader_program_addresses(&self) -> [u64; 6] {
        [0; 6]
    }

    /// Read the live `regs.draw_texture` payload.
    fn draw_texture_params(&self) -> DrawTextureParams;

    /// Read whether `regs.window_origin.mode != UpperLeft`.
    fn window_origin_lower_left(&self) -> bool;

    /// Read whether `regs.window_origin.flip_y != 0`.
    fn window_origin_flip_y(&self) -> bool;

    /// Read `regs.viewport_transform[index].scale_y`.
    fn viewport_transform_scale_y(&self, index: u32) -> f32;

    /// Read raw `regs.viewport_transform[index]`.
    fn viewport_transform_info(
        &self,
        index: u32,
    ) -> crate::engines::maxwell_3d::ViewportTransformInfo;

    /// Read whether `regs.viewport_scale_offset_enabled != 0`.
    fn viewport_scale_offset_enabled(&self) -> bool;

    /// Read raw `regs.surface_clip`.
    fn surface_clip_info(&self) -> crate::engines::maxwell_3d::SurfaceClipInfo;

    /// Read `regs.framebuffer_srgb`.
    fn framebuffer_srgb(&self) -> bool {
        false
    }

    /// Read the raw `regs.user_clip_enable` mask.
    fn user_clip_enable_raw(&self) -> u32 {
        0
    }

    /// Read `regs.surface_clip.height`.
    fn surface_clip_height(&self) -> u32;

    /// Read the current clear-surface flags register payload.
    fn clear_surface_flags(&self) -> u32;

    /// Read `regs.clear_control.use_scissor`.
    fn clear_control_use_scissor(&self) -> bool {
        false
    }

    /// Read `regs.clear_control.use_viewport_clip0`.
    fn clear_control_use_viewport_clip0(&self) -> bool {
        false
    }

    /// Read render-target address.
    fn rt_address(&self, index: usize) -> u64;

    /// Read render-target width.
    fn rt_width(&self, index: usize) -> u32;

    /// Read render-target height.
    fn rt_height(&self, index: usize) -> u32;

    /// Read render-target format.
    fn rt_format(&self, index: usize) -> u32;

    /// Read the full render-target config snapshot.
    fn rt_info(&self, index: usize) -> crate::engines::maxwell_3d::RenderTargetInfo {
        crate::engines::maxwell_3d::RenderTargetInfo {
            address: self.rt_address(index),
            width: self.rt_width(index),
            height: self.rt_height(index),
            format: self.rt_format(index),
            ..Default::default()
        }
    }

    /// Read clear color as RGBA floats.
    fn clear_color_rgba(&self) -> [f32; 4];

    /// Read clear depth value.
    fn clear_depth(&self) -> f32 {
        0.0
    }

    /// Read clear stencil value.
    fn clear_stencil(&self) -> i32 {
        0
    }

    /// Read index buffer GPU virtual address.
    fn index_buffer_addr(&self) -> u64;

    /// Read index buffer GPU virtual end address.
    fn index_buffer_addr_end(&self) -> u64 {
        let index_buffer = self.index_buffer();
        self.index_buffer_addr()
            + index_buffer.count as u64 * index_buffer.format.size_bytes() as u64
    }

    /// Read vertex stream info for one stream slot.
    fn vertex_stream_info(&self, index: u32) -> crate::engines::maxwell_3d::VertexStreamInfo;

    /// Read vertex stream instancing enable for one stream slot.
    fn vertex_stream_instance(&self, index: u32) -> u32;

    /// Read vertex stream limit info for one stream slot.
    fn vertex_stream_limit(&self, index: u32) -> VertexStreamLimit;

    /// Read viewport info for one viewport slot.
    fn viewport_info(&self, index: u32) -> crate::engines::maxwell_3d::ViewportInfo;

    /// Read scissor info for one scissor slot.
    fn scissor_info(&self, index: u32) -> crate::engines::maxwell_3d::ScissorInfo;

    /// Read effective blend state for one render target.
    fn effective_blend_info(&self, rt: usize) -> crate::engines::maxwell_3d::BlendInfo;

    /// Read whether blending uses per-render-target state.
    fn blend_per_target_enabled(&self) -> bool {
        false
    }

    /// Read `regs.iterated_blend.enable`.
    fn iterated_blend_enabled(&self) -> bool {
        false
    }

    /// Read global blend state used when per-target blending is disabled.
    fn global_blend_info(&self, rt: usize) -> crate::engines::maxwell_3d::BlendInfo {
        self.effective_blend_info(rt)
    }

    /// Read blend constant color.
    fn blend_color_info(&self) -> crate::engines::maxwell_3d::BlendColorInfo;

    /// Read combined depth/stencil state.
    fn depth_stencil_info(&self) -> crate::engines::maxwell_3d::DepthStencilInfo;

    /// Read rasterizer state.
    fn rasterizer_info(&self) -> crate::engines::maxwell_3d::RasterizerInfo;

    /// Read `regs.rasterize_enable`, matching upstream `SyncRasterizeEnable`.
    fn rasterize_enable(&self) -> bool {
        true
    }

    /// Read `regs.primitive_restart`, matching upstream `SyncPrimitiveRestart`.
    fn primitive_restart_info(&self) -> crate::engines::maxwell_3d::PrimitiveRestartInfo {
        Default::default()
    }

    /// Read `regs.logic_op`, matching upstream `SyncLogicOpState`.
    fn logic_op_info(&self) -> crate::engines::maxwell_3d::LogicOpInfo {
        Default::default()
    }

    /// Read `regs.frag_color_clamp.AnyEnabled()`, matching upstream
    /// `SyncFragmentColorClampState`.
    fn frag_color_clamp_any_enabled(&self) -> bool {
        false
    }

    /// Read `regs.anti_alias_alpha_control`, matching upstream
    /// `SyncMultiSampleState`.
    fn anti_alias_alpha_control_info(
        &self,
    ) -> crate::engines::maxwell_3d::AntiAliasAlphaControlInfo {
        Default::default()
    }

    /// Read point-size state, matching upstream `SyncPointState`.
    fn point_state_info(&self) -> crate::engines::maxwell_3d::PointStateInfo {
        Default::default()
    }

    /// Read line-width state, matching upstream `SyncLineState`.
    fn line_state_info(&self) -> crate::engines::maxwell_3d::LineStateInfo {
        Default::default()
    }

    fn line_stipple_info(&self) -> crate::engines::maxwell_3d::LineStippleInfo {
        Default::default()
    }

    /// Read depth-clamp enable state, matching upstream `SyncDepthClamp`.
    fn depth_clamp_enabled(&self) -> bool {
        true
    }

    /// Read `regs.conservative_raster_enable`, matching upstream
    /// `FixedPipelineState::Refresh`.
    fn conservative_raster_enable(&self) -> bool {
        false
    }

    /// Read upstream owner `Maxwell3D::engine_state`.
    fn engine_state(&self) -> crate::engines::maxwell_3d::EngineHint {
        crate::engines::maxwell_3d::EngineHint::None
    }

    /// Read `regs.provoking_vertex == Last`, matching upstream
    /// `FixedPipelineState::Refresh`.
    fn provoking_vertex_last(&self) -> bool {
        false
    }

    /// Read `regs.depth_bounds_enable`, matching upstream
    /// `FixedPipelineState::DynamicState::Refresh`.
    fn depth_bounds_enable(&self) -> bool {
        false
    }

    /// Read `regs.depth_bounds`, matching `UpdateDepthBounds`.
    fn depth_bounds(&self) -> [f32; 2] {
        [0.0, 1.0]
    }

    /// Read `regs.mandated_early_z`, matching upstream `FixedPipelineState::Refresh`.
    fn mandated_early_z(&self) -> bool {
        false
    }

    /// Read `regs.alpha_test_enabled`, matching upstream `SyncAlphaTest`.
    fn alpha_test_enabled(&self) -> bool {
        false
    }

    /// Read `regs.alpha_test_func`, matching upstream `SyncAlphaTest`.
    fn alpha_test_func(&self) -> crate::engines::maxwell_3d::ComparisonOp {
        crate::engines::maxwell_3d::ComparisonOp::Always
    }

    /// Read `regs.alpha_test_ref`, matching upstream `SyncAlphaTest`.
    fn alpha_test_ref(&self) -> f32 {
        0.0
    }

    /// Read `regs.tessellation.params.domain_type`.
    fn tessellation_domain_type(&self) -> u32 {
        0
    }

    /// Read `regs.tessellation.params.spacing`.
    fn tessellation_spacing(&self) -> u32 {
        0
    }

    /// Read whether tessellation output primitives are clockwise triangles.
    fn tessellation_clockwise(&self) -> bool {
        false
    }

    /// Read `regs.patch_vertices`.
    fn patch_vertices(&self) -> u32 {
        1
    }

    /// Read `regs.transform_feedback_enabled != 0`, matching upstream
    /// `RasterizerOpenGL::BeginTransformFeedback`.
    fn transform_feedback_enabled(&self) -> bool {
        false
    }

    /// Read transform feedback layout/varying state, matching upstream
    /// `RefreshXfbState`.
    fn transform_feedback_state(&self) -> crate::transform_feedback::TransformFeedbackState {
        Default::default()
    }

    /// Read `regs.IsShaderConfigEnabled(stage)`, matching upstream
    /// `RasterizerOpenGL::BeginTransformFeedback`.
    fn shader_config_enabled(&self, stage: ShaderStageType) -> bool {
        matches!(stage, ShaderStageType::VertexB)
    }

    /// Read shader program region base address.
    fn program_base_address(&self) -> u64;

    /// Read one constant-buffer binding.
    fn const_buffer_binding(&self, stage: usize, slot: usize) -> ConstBufferInfo;

    /// Read vertex attribute info for one attribute slot.
    fn vertex_attrib_info(&self, index: u32) -> crate::engines::maxwell_3d::VertexAttribInfo;

    /// Read shader stage info for one pipeline slot.
    fn shader_stage_info(&self, index: u32) -> crate::engines::maxwell_3d::ShaderStageInfo;

    /// Read color write mask for one render target.
    fn color_mask_info(&self, rt: usize) -> crate::engines::maxwell_3d::ColorMaskInfo;

    /// Read whether color_mask[0] is shared by all render targets.
    fn color_mask_common(&self) -> bool {
        false
    }

    /// Read render target control info.
    fn rt_control_info(&self) -> crate::engines::maxwell_3d::RtControlInfo;

    /// Read depth/stencil render target info.
    fn zeta_info(&self) -> crate::engines::maxwell_3d::ZetaInfo;

    /// Read `regs.anti_alias_samples_mode`.
    fn anti_alias_samples_mode(&self) -> u32 {
        0
    }

    /// Read `regs.zpass_pixel_count_enable`.
    fn zpass_pixel_count_enabled(&self) -> bool {
        false
    }

    /// Read texture header pool base address.
    fn tex_header_pool_address(&self) -> u64;

    /// Read texture header pool limit.
    fn tex_header_pool_limit(&self) -> u32;

    /// Read texture sampler pool base address.
    fn tex_sampler_pool_address(&self) -> u64;

    /// Read texture sampler pool limit.
    fn tex_sampler_pool_limit(&self) -> u32;

    /// Read sampler binding mode.
    fn sampler_binding(&self) -> crate::engines::maxwell_3d::SamplerBinding;
}

// ── DrawManager ─────────────────────────────────────────────────────────────

/// Draw mode for the current draw state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum DrawMode {
    #[default]
    General = 0,
    Instance = 1,
    InlineIndex = 2,
}

/// Core draw state tracked across begin/end sequences.
#[derive(Debug, Clone)]
pub struct DrawState {
    pub topology: PrimitiveTopology,
    pub draw_mode: DrawMode,
    pub draw_indexed: bool,
    pub base_index: u32,
    pub vertex_buffer: VertexBuffer,
    pub index_buffer: IndexBuffer,
    pub base_instance: u32,
    pub instance_count: u32,
    pub inline_index_draw_indexes: Vec<u8>,
}

/// Render-target register snapshot used by draw and clear views.
#[derive(Debug, Clone, Copy, Default)]
pub struct Maxwell3DRenderTargets {
    pub rt_control: RtControlInfo,
    pub render_targets: [RenderTargetInfo; 8],
    pub zeta: crate::engines::maxwell_3d::ZetaInfo,
    pub anti_alias_samples_mode: u32,
    pub surface_clip: crate::engines::maxwell_3d::SurfaceClipInfo,
}

#[derive(Debug, Clone)]
pub struct Maxwell3DDrawRegisters {
    pub shader_program_addresses: [u64; 6],
    pub index_buffer_gpu_addr: u64,
    pub index_buffer_gpu_addr_end: u64,
    pub render_targets: Maxwell3DRenderTargets,
    pub cb_bindings: [[ConstBufferInfo; MAX_CB_SLOTS]; NUM_SHADER_STAGES],
    pub vertex_streams: [crate::engines::maxwell_3d::VertexStreamInfo; 32],
    pub vertex_stream_instances: [u32; 32],
    pub vertex_stream_limits: [VertexStreamLimit; 32],
    pub vertex_attribs: [crate::engines::maxwell_3d::VertexAttribInfo; 32],
    pub scissors:
        [crate::engines::maxwell_3d::ScissorInfo; crate::engines::maxwell_3d::NUM_VIEWPORTS],
    pub blend: [crate::engines::maxwell_3d::BlendInfo; 8],
    pub blend_per_target_enabled: bool,
    pub global_blend: crate::engines::maxwell_3d::BlendInfo,
    pub iterated_blend_enabled: bool,
    pub blend_color: crate::engines::maxwell_3d::BlendColorInfo,
    pub depth_stencil: crate::engines::maxwell_3d::DepthStencilInfo,
    pub rasterizer: crate::engines::maxwell_3d::RasterizerInfo,
    pub rasterize_enable: bool,
    pub primitive_restart: crate::engines::maxwell_3d::PrimitiveRestartInfo,
    pub logic_op: crate::engines::maxwell_3d::LogicOpInfo,
    pub frag_color_clamp_any_enabled: bool,
    pub anti_alias_alpha_control: crate::engines::maxwell_3d::AntiAliasAlphaControlInfo,
    pub point_state: crate::engines::maxwell_3d::PointStateInfo,
    pub line_state: crate::engines::maxwell_3d::LineStateInfo,
    pub line_stipple: crate::engines::maxwell_3d::LineStippleInfo,
    pub depth_clamp_enabled: bool,
    pub conservative_raster_enable: bool,
    pub engine_state: crate::engines::maxwell_3d::EngineHint,
    pub provoking_vertex_last: bool,
    pub depth_bounds_enable: bool,
    pub depth_bounds: [f32; 2],
    pub mandated_early_z: bool,
    pub alpha_test_enabled: bool,
    pub alpha_test_func: crate::engines::maxwell_3d::ComparisonOp,
    pub alpha_test_ref: f32,
    pub tessellation_primitive: u32,
    pub tessellation_spacing: u32,
    pub tessellation_clockwise: bool,
    pub patch_vertices: u32,
    pub transform_feedback_enabled: bool,
    pub transform_feedback_state: crate::transform_feedback::TransformFeedbackState,
    pub shader_config_enabled: [bool; NUM_SHADER_PROGRAMS],
    pub descriptor_sync_regs: crate::texture_cache::texture_cache_base::DescriptorSyncRegs,
    pub zpass_pixel_count_enabled: bool,
    pub window_origin_lower_left: bool,
    pub window_origin_flip_y: bool,
    pub viewport0_scale_y: f32,
    pub viewport_transforms: [crate::engines::maxwell_3d::ViewportTransformInfo;
        crate::engines::maxwell_3d::NUM_VIEWPORTS],
    pub viewport_scale_offset_enabled: bool,
    pub surface_clip: crate::engines::maxwell_3d::SurfaceClipInfo,
    pub framebuffer_srgb: bool,
    pub user_clip_enable_raw: u32,
    pub depth_mode: crate::engines::maxwell_3d::DepthMode,
    pub dirty_flags: [bool; 256],
    pub color_masks: [crate::engines::maxwell_3d::ColorMaskInfo; 8],
    pub color_mask_common: bool,
}

impl Default for Maxwell3DDrawRegisters {
    fn default() -> Self {
        Self {
            shader_program_addresses: [0; 6],
            index_buffer_gpu_addr: 0,
            index_buffer_gpu_addr_end: 0,
            render_targets: Default::default(),
            cb_bindings: Default::default(),
            vertex_streams: Default::default(),
            vertex_stream_instances: [0; 32],
            vertex_stream_limits: Default::default(),
            vertex_attribs: Default::default(),
            scissors: Default::default(),
            blend: Default::default(),
            blend_per_target_enabled: false,
            global_blend: Default::default(),
            iterated_blend_enabled: false,
            blend_color: Default::default(),
            depth_stencil: Default::default(),
            rasterizer: Default::default(),
            rasterize_enable: true,
            primitive_restart: Default::default(),
            logic_op: Default::default(),
            frag_color_clamp_any_enabled: false,
            anti_alias_alpha_control: Default::default(),
            point_state: Default::default(),
            line_state: Default::default(),
            line_stipple: Default::default(),
            depth_clamp_enabled: true,
            conservative_raster_enable: false,
            engine_state: crate::engines::maxwell_3d::EngineHint::None,
            provoking_vertex_last: false,
            depth_bounds_enable: false,
            depth_bounds: [0.0, 1.0],
            mandated_early_z: false,
            alpha_test_enabled: false,
            alpha_test_func: crate::engines::maxwell_3d::ComparisonOp::Always,
            alpha_test_ref: 0.0,
            tessellation_primitive: 0,
            tessellation_spacing: 0,
            tessellation_clockwise: false,
            patch_vertices: 1,
            transform_feedback_enabled: false,
            transform_feedback_state: Default::default(),
            shader_config_enabled: [false, true, false, false, false, false],
            descriptor_sync_regs: Default::default(),
            zpass_pixel_count_enabled: false,
            window_origin_lower_left: false,
            window_origin_flip_y: false,
            viewport0_scale_y: 0.0,
            viewport_transforms: Default::default(),
            viewport_scale_offset_enabled: false,
            surface_clip: Default::default(),
            framebuffer_srgb: false,
            user_clip_enable_raw: 0,
            depth_mode: crate::engines::maxwell_3d::DepthMode::ZeroToOne,
            dirty_flags: [false; 256],
            color_masks: Default::default(),
            color_mask_common: false,
        }
    }
}

impl Maxwell3DDrawRegisters {
    pub fn from_maxwell3d(maxwell3d: &dyn Maxwell3DAccess) -> Self {
        Self {
            shader_program_addresses: maxwell3d.shader_program_addresses(),
            index_buffer_gpu_addr: maxwell3d.index_buffer_addr(),
            index_buffer_gpu_addr_end: maxwell3d.index_buffer_addr_end(),
            render_targets: Maxwell3DRenderTargets {
                rt_control: maxwell3d.rt_control_info(),
                render_targets: std::array::from_fn(|i| maxwell3d.rt_info(i)),
                zeta: maxwell3d.zeta_info(),
                anti_alias_samples_mode: maxwell3d.anti_alias_samples_mode(),
                surface_clip: maxwell3d.surface_clip_info(),
            },
            cb_bindings: std::array::from_fn(|stage| {
                std::array::from_fn(|slot| maxwell3d.const_buffer_binding(stage, slot))
            }),
            vertex_streams: std::array::from_fn(|i| maxwell3d.vertex_stream_info(i as u32)),
            vertex_stream_instances: std::array::from_fn(|i| {
                maxwell3d.vertex_stream_instance(i as u32)
            }),
            vertex_stream_limits: std::array::from_fn(|i| maxwell3d.vertex_stream_limit(i as u32)),
            vertex_attribs: std::array::from_fn(|i| maxwell3d.vertex_attrib_info(i as u32)),
            scissors: std::array::from_fn(|i| maxwell3d.scissor_info(i as u32)),
            blend: std::array::from_fn(|i| maxwell3d.effective_blend_info(i)),
            blend_per_target_enabled: maxwell3d.blend_per_target_enabled(),
            global_blend: maxwell3d.global_blend_info(0),
            iterated_blend_enabled: maxwell3d.iterated_blend_enabled(),
            blend_color: maxwell3d.blend_color_info(),
            depth_stencil: maxwell3d.depth_stencil_info(),
            rasterizer: maxwell3d.rasterizer_info(),
            rasterize_enable: maxwell3d.rasterize_enable(),
            primitive_restart: maxwell3d.primitive_restart_info(),
            logic_op: maxwell3d.logic_op_info(),
            frag_color_clamp_any_enabled: maxwell3d.frag_color_clamp_any_enabled(),
            anti_alias_alpha_control: maxwell3d.anti_alias_alpha_control_info(),
            point_state: maxwell3d.point_state_info(),
            line_state: maxwell3d.line_state_info(),
            line_stipple: maxwell3d.line_stipple_info(),
            depth_clamp_enabled: maxwell3d.depth_clamp_enabled(),
            conservative_raster_enable: maxwell3d.conservative_raster_enable(),
            engine_state: maxwell3d.engine_state(),
            provoking_vertex_last: maxwell3d.provoking_vertex_last(),
            depth_bounds_enable: maxwell3d.depth_bounds_enable(),
            depth_bounds: maxwell3d.depth_bounds(),
            mandated_early_z: maxwell3d.mandated_early_z(),
            alpha_test_enabled: maxwell3d.alpha_test_enabled(),
            alpha_test_func: maxwell3d.alpha_test_func(),
            alpha_test_ref: maxwell3d.alpha_test_ref(),
            tessellation_primitive: maxwell3d.tessellation_domain_type(),
            tessellation_spacing: maxwell3d.tessellation_spacing(),
            tessellation_clockwise: maxwell3d.tessellation_clockwise(),
            patch_vertices: maxwell3d.patch_vertices(),
            transform_feedback_enabled: maxwell3d.transform_feedback_enabled(),
            transform_feedback_state: maxwell3d.transform_feedback_state(),
            shader_config_enabled: std::array::from_fn(|index| {
                maxwell3d.shader_config_enabled(ShaderStageType::from_raw(index as u32))
            }),
            descriptor_sync_regs: crate::texture_cache::texture_cache_base::DescriptorSyncRegs {
                sampler_binding_via_header: matches!(
                    maxwell3d.sampler_binding(),
                    crate::engines::maxwell_3d::SamplerBinding::ViaHeaderBinding
                ),
                tex_header_addr: maxwell3d.tex_header_pool_address(),
                tex_header_limit: maxwell3d.tex_header_pool_limit(),
                tex_sampler_addr: maxwell3d.tex_sampler_pool_address(),
                tex_sampler_limit: maxwell3d.tex_sampler_pool_limit(),
            },
            zpass_pixel_count_enabled: maxwell3d.zpass_pixel_count_enabled(),
            window_origin_lower_left: maxwell3d.window_origin_lower_left(),
            window_origin_flip_y: maxwell3d.window_origin_flip_y(),
            viewport0_scale_y: maxwell3d.viewport_transform_scale_y(0),
            viewport_transforms: std::array::from_fn(|i| {
                maxwell3d.viewport_transform_info(i as u32)
            }),
            viewport_scale_offset_enabled: maxwell3d.viewport_scale_offset_enabled(),
            surface_clip: maxwell3d.surface_clip_info(),
            framebuffer_srgb: maxwell3d.framebuffer_srgb(),
            user_clip_enable_raw: maxwell3d.user_clip_enable_raw(),
            depth_mode: maxwell3d.depth_stencil_info().depth_mode,
            dirty_flags: *maxwell3d.dirty_flags(),
            color_masks: std::array::from_fn(|i| maxwell3d.color_mask_info(i)),
            color_mask_common: maxwell3d.color_mask_common(),
        }
    }

    /// Rebuild the register snapshot used only by Reden's legacy offscreen
    /// `render_draw_calls` entry point. Runtime Maxwell draws use
    /// `Maxwell3DDrawView::live` and never execute this conversion.
    pub fn from_draw_call(draw: &DrawCall) -> Self {
        Self {
            shader_program_addresses: std::array::from_fn(|index| {
                draw.program_base_address
                    .wrapping_add(draw.shader_stages[index].offset as u64)
            }),
            index_buffer_gpu_addr: draw.index_buffer_addr,
            index_buffer_gpu_addr_end: draw.index_buffer_addr_end,
            render_targets: draw.render_targets(),
            cb_bindings: draw.cb_bindings,
            vertex_streams: draw.vertex_streams,
            vertex_stream_instances: draw.vertex_stream_instances,
            vertex_stream_limits: draw.vertex_stream_limits,
            vertex_attribs: draw.vertex_attribs,
            scissors: draw.scissors,
            blend: draw.blend,
            blend_per_target_enabled: draw.blend_per_target_enabled,
            global_blend: draw.global_blend,
            iterated_blend_enabled: draw.iterated_blend_enabled,
            blend_color: draw.blend_color,
            depth_stencil: draw.depth_stencil.clone(),
            rasterizer: draw.rasterizer.clone(),
            rasterize_enable: draw.rasterize_enable,
            primitive_restart: draw.primitive_restart,
            logic_op: draw.logic_op,
            frag_color_clamp_any_enabled: false,
            anti_alias_alpha_control: draw.anti_alias_alpha_control,
            point_state: crate::engines::maxwell_3d::PointStateInfo {
                point_size: draw.point_size,
                ..Default::default()
            },
            line_state: crate::engines::maxwell_3d::LineStateInfo {
                line_anti_alias_enable: draw.line_anti_alias_enable,
                line_width_smooth: draw.rasterizer.line_width_smooth,
                line_width_aliased: draw.rasterizer.line_width_aliased,
            },
            line_stipple: draw.line_stipple,
            depth_clamp_enabled: draw.depth_clamp_enabled,
            conservative_raster_enable: draw.conservative_raster_enable,
            engine_state: draw.engine_state,
            provoking_vertex_last: draw.provoking_vertex_last,
            depth_bounds_enable: draw.depth_bounds_enable,
            depth_bounds: draw.depth_bounds,
            mandated_early_z: draw.mandated_early_z,
            alpha_test_enabled: draw.alpha_test_enabled,
            alpha_test_func: draw.alpha_test_func,
            alpha_test_ref: draw.alpha_test_ref,
            tessellation_primitive: draw.tessellation_primitive,
            tessellation_spacing: draw.tessellation_spacing,
            tessellation_clockwise: draw.tessellation_clockwise,
            patch_vertices: draw.patch_vertices,
            transform_feedback_enabled: draw.transform_feedback_enabled,
            transform_feedback_state: draw.transform_feedback_state,
            shader_config_enabled: draw.shader_stages.map(|stage| stage.enabled),
            descriptor_sync_regs: crate::texture_cache::texture_cache_base::DescriptorSyncRegs {
                sampler_binding_via_header: matches!(
                    draw.sampler_binding,
                    crate::engines::maxwell_3d::SamplerBinding::ViaHeaderBinding
                ),
                tex_header_addr: draw.tex_header_pool_addr,
                tex_header_limit: draw.tex_header_pool_limit,
                tex_sampler_addr: draw.tex_sampler_pool_addr,
                tex_sampler_limit: draw.tex_sampler_pool_limit,
            },
            zpass_pixel_count_enabled: false,
            window_origin_lower_left: draw.window_origin_lower_left,
            window_origin_flip_y: draw.window_origin_flip_y,
            viewport0_scale_y: draw.viewport_transforms[0].scale_y,
            viewport_transforms: draw.viewport_transforms,
            viewport_scale_offset_enabled: draw.viewport_scale_offset_enabled,
            surface_clip: draw.surface_clip,
            framebuffer_srgb: false,
            user_clip_enable_raw: 0,
            depth_mode: draw.depth_stencil.depth_mode,
            dirty_flags: draw.dirty_flags,
            color_masks: draw.color_masks,
            color_mask_common: false,
        }
    }
}

struct Maxwell3DLiveRef<'a> {
    pointer: NonNull<Maxwell3D>,
    _lifetime: PhantomData<&'a mut Maxwell3D>,
}

impl<'a> Maxwell3DLiveRef<'a> {
    fn new(maxwell3d: &'a mut Maxwell3D) -> Self {
        Self {
            pointer: NonNull::from(maxwell3d),
            _lifetime: PhantomData,
        }
    }
}

impl Deref for Maxwell3DLiveRef<'_> {
    type Target = Maxwell3D;

    fn deref(&self) -> &Self::Target {
        // SAFETY: `_lifetime` prevents the view from outliving the engine;
        // rasterizer callbacks are serialized on the GPU thread and each
        // dereference is scoped to one access, matching upstream's non-owning
        // Maxwell3D*.
        unsafe { self.pointer.as_ref() }
    }
}

impl DerefMut for Maxwell3DLiveRef<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: mutable register accesses are serialized with the cache and
        // state-tracker accesses that retain their own stable raw pointers.
        unsafe { self.pointer.as_mut() }
    }
}

enum Maxwell3DDrawSource<'a> {
    Live(Maxwell3DLiveRef<'a>),
    Snapshot(Maxwell3DDrawRegisters),
}

/// Draw-time Maxwell3D view passed to rasterizers.
///
/// Upstream rasterizers keep a `Maxwell3D*` and read register/dirty state
/// through it while drawing. This wrapper carries the same concrete engine
/// pointer for the live path, while snapshot mode remains available for tests
/// and reduced rasterizer fixtures.
pub struct Maxwell3DDrawView<'a> {
    draw_state: &'a DrawState,
    draw_indexed: bool,
    source: Maxwell3DDrawSource<'a>,
}

impl<'a> Maxwell3DDrawView<'a> {
    pub fn new(draw_state: &'a DrawState, draw_indexed: bool) -> Self {
        Self {
            draw_state,
            draw_indexed,
            source: Maxwell3DDrawSource::Snapshot(Maxwell3DDrawRegisters::default()),
        }
    }

    pub fn live(
        draw_state: &'a DrawState,
        draw_indexed: bool,
        maxwell3d: &'a mut Maxwell3D,
    ) -> Self {
        Self {
            draw_state,
            draw_indexed,
            source: Maxwell3DDrawSource::Live(Maxwell3DLiveRef::new(maxwell3d)),
        }
    }

    pub fn with_register_snapshot(
        draw_state: &'a DrawState,
        draw_indexed: bool,
        registers: Maxwell3DDrawRegisters,
    ) -> Self {
        Self {
            draw_state,
            draw_indexed,
            source: Maxwell3DDrawSource::Snapshot(registers),
        }
    }

    pub fn draw_state(&self) -> &'a DrawState {
        self.draw_state
    }

    pub fn is_indexed(&self) -> bool {
        self.draw_indexed
    }

    /// Expose the concrete engine pointer carried by the production draw
    /// path. OpenGL pipelines retain this through their upstream-equivalent
    /// `SetEngine`; snapshot-only test views intentionally return `None`.
    pub(crate) fn live_maxwell3d_ptr(&mut self) -> Option<NonNull<Maxwell3D>> {
        match &mut self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => Some(maxwell3d.pointer),
            Maxwell3DDrawSource::Snapshot(_) => None,
        }
    }

    pub fn draw_call_snapshot(&self, instance_count: u32) -> DrawCall {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => build_draw_call_snapshot(
                self.draw_state,
                self.draw_indexed,
                instance_count,
                &**maxwell3d,
            ),
            Maxwell3DDrawSource::Snapshot(registers) => {
                let mut shader_stages =
                    [crate::engines::maxwell_3d::ShaderStageInfo::default(); NUM_SHADER_PROGRAMS];
                for (index, stage) in shader_stages.iter_mut().enumerate() {
                    stage.enabled = registers
                        .shader_config_enabled
                        .get(index)
                        .copied()
                        .unwrap_or(false);
                    stage.offset = registers.shader_program_addresses[index] as u32;
                }

                DrawCall {
                    topology: self.draw_state.topology,
                    vertex_first: self.draw_state.vertex_buffer.first,
                    vertex_count: self.draw_state.vertex_buffer.count,
                    indexed: self.draw_indexed,
                    index_buffer_addr: registers.index_buffer_gpu_addr,
                    index_buffer_addr_end: registers.index_buffer_gpu_addr_end,
                    index_buffer_count: self.draw_state.index_buffer.count,
                    index_buffer_first: self.draw_state.index_buffer.first,
                    index_format: self.draw_state.index_buffer.format,
                    vertex_streams: registers.vertex_streams,
                    vertex_stream_instances: registers.vertex_stream_instances,
                    vertex_stream_limits: registers.vertex_stream_limits,
                    viewports: Default::default(),
                    viewport_transforms: registers.viewport_transforms,
                    scissors: registers.scissors,
                    viewport_scale_offset_enabled: registers.viewport_scale_offset_enabled,
                    window_origin_lower_left: registers.window_origin_lower_left,
                    window_origin_flip_y: registers.window_origin_flip_y,
                    surface_clip: registers.surface_clip,
                    blend: registers.blend,
                    blend_per_target_enabled: registers.blend_per_target_enabled,
                    global_blend: registers.global_blend,
                    iterated_blend_enabled: registers.iterated_blend_enabled,
                    blend_color: registers.blend_color,
                    depth_stencil: registers.depth_stencil.clone(),
                    rasterizer: registers.rasterizer.clone(),
                    rasterize_enable: registers.rasterize_enable,
                    primitive_restart: registers.primitive_restart,
                    logic_op: registers.logic_op,
                    depth_clamp_enabled: registers.depth_clamp_enabled,
                    conservative_raster_enable: registers.conservative_raster_enable,
                    engine_state: registers.engine_state,
                    provoking_vertex_last: registers.provoking_vertex_last,
                    depth_bounds_enable: registers.depth_bounds_enable,
                    depth_bounds: registers.depth_bounds,
                    mandated_early_z: registers.mandated_early_z,
                    alpha_test_enabled: registers.alpha_test_enabled,
                    alpha_test_func: registers.alpha_test_func,
                    alpha_test_ref: registers.alpha_test_ref,
                    point_size: registers.point_state.point_size,
                    tessellation_primitive: registers.tessellation_primitive,
                    tessellation_spacing: registers.tessellation_spacing,
                    tessellation_clockwise: registers.tessellation_clockwise,
                    patch_vertices: registers.patch_vertices,
                    anti_alias_samples_mode: registers.render_targets.anti_alias_samples_mode,
                    anti_alias_alpha_control: registers.anti_alias_alpha_control,
                    line_anti_alias_enable: registers.line_state.line_anti_alias_enable,
                    line_stipple: registers.line_stipple,
                    program_base_address: 0,
                    cb_bindings: registers.cb_bindings,
                    vertex_attribs: registers.vertex_attribs,
                    shader_stages,
                    color_masks: registers.color_masks,
                    rt_control: registers.render_targets.rt_control,
                    tex_header_pool_addr: registers.descriptor_sync_regs.tex_header_addr,
                    tex_header_pool_limit: registers.descriptor_sync_regs.tex_header_limit,
                    tex_sampler_pool_addr: registers.descriptor_sync_regs.tex_sampler_addr,
                    tex_sampler_pool_limit: registers.descriptor_sync_regs.tex_sampler_limit,
                    instance_count,
                    base_instance: self.draw_state.base_instance,
                    base_vertex: self.draw_state.base_index as i32,
                    inline_index_data: self.draw_state.inline_index_draw_indexes.clone(),
                    sampler_binding: if registers.descriptor_sync_regs.sampler_binding_via_header {
                        crate::engines::maxwell_3d::SamplerBinding::ViaHeaderBinding
                    } else {
                        crate::engines::maxwell_3d::SamplerBinding::Independently
                    },
                    render_targets: registers.render_targets.render_targets,
                    zeta: registers.render_targets.zeta,
                    transform_feedback_enabled: registers.transform_feedback_enabled,
                    transform_feedback_state: registers.transform_feedback_state,
                    dirty_flags: registers.dirty_flags,
                }
            }
        }
    }

    pub fn registers(&self) -> Maxwell3DDrawRegisters {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => {
                Maxwell3DDrawRegisters::from_maxwell3d(&**maxwell3d)
            }
            Maxwell3DDrawSource::Snapshot(registers) => registers.clone(),
        }
    }

    /// Per-stage shader program GPU virtual addresses.
    ///
    /// Upstream computes these as
    /// `maxwell3d->regs.program_region.Address() + maxwell3d->regs.pipelines[i].offset`.
    /// The live view reads these from Maxwell3D directly; snapshot mode is
    /// retained only for tests and default trait fallbacks.
    pub fn shader_program_addresses(&self) -> [u64; 6] {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.shader_program_addresses(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.shader_program_addresses,
        }
    }

    pub fn index_buffer_gpu_addr(&self) -> u64 {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.index_buffer_addr(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.index_buffer_gpu_addr,
        }
    }

    pub fn index_buffer_gpu_addr_end(&self) -> u64 {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.index_buffer_addr_end(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.index_buffer_gpu_addr_end,
        }
    }

    pub fn render_targets(&self) -> Maxwell3DRenderTargets {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => Maxwell3DRenderTargets {
                rt_control: maxwell3d.rt_control_info(),
                render_targets: std::array::from_fn(|i| maxwell3d.rt_info(i)),
                zeta: maxwell3d.zeta_info(),
                anti_alias_samples_mode: maxwell3d.anti_alias_samples_mode(),
                surface_clip: maxwell3d.surface_clip_info(),
            },
            Maxwell3DDrawSource::Snapshot(registers) => registers.render_targets,
        }
    }

    pub fn render_target(&self, index: usize) -> RenderTargetInfo {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.rt_info(index),
            Maxwell3DDrawSource::Snapshot(registers) => {
                registers.render_targets.render_targets[index]
            }
        }
    }

    pub fn zeta(&self) -> crate::engines::maxwell_3d::ZetaInfo {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.zeta_info(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.render_targets.zeta,
        }
    }

    pub fn anti_alias_samples_mode(&self) -> u32 {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.anti_alias_samples_mode(),
            Maxwell3DDrawSource::Snapshot(registers) => {
                registers.render_targets.anti_alias_samples_mode
            }
        }
    }

    pub fn cb_bindings(&self) -> [[ConstBufferInfo; MAX_CB_SLOTS]; NUM_SHADER_STAGES] {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => std::array::from_fn(|stage| {
                std::array::from_fn(|slot| maxwell3d.const_buffer_binding(stage, slot))
            }),
            Maxwell3DDrawSource::Snapshot(registers) => registers.cb_bindings,
        }
    }

    pub fn const_buffer_binding(&self, stage: usize, slot: usize) -> ConstBufferInfo {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.const_buffer_binding(stage, slot),
            Maxwell3DDrawSource::Snapshot(registers) => registers.cb_bindings[stage][slot],
        }
    }

    pub fn vertex_stream(&self, index: usize) -> crate::engines::maxwell_3d::VertexStreamInfo {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.vertex_stream_info(index as u32),
            Maxwell3DDrawSource::Snapshot(registers) => registers.vertex_streams[index],
        }
    }

    pub fn vertex_stream_instance(&self, index: usize) -> u32 {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.vertex_stream_instance(index as u32),
            Maxwell3DDrawSource::Snapshot(registers) => registers.vertex_stream_instances[index],
        }
    }

    pub fn vertex_attrib(&self, index: usize) -> crate::engines::maxwell_3d::VertexAttribInfo {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.vertex_attrib_info(index as u32),
            Maxwell3DDrawSource::Snapshot(registers) => registers.vertex_attribs[index],
        }
    }

    /// Raw `Maxwell3D::Regs::vertex_attrib_format[index]` word.
    ///
    /// The live path preserves Eden's direct bitfield reads. Snapshot mode
    /// mechanically reconstructs the word for reduced test fixtures.
    pub fn vertex_attrib_raw(&self, index: usize) -> u32 {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.vertex_attrib_raw(index as u32),
            Maxwell3DDrawSource::Snapshot(registers) => {
                let attrib = registers.vertex_attribs[index];
                attrib.buffer_index
                    | (u32::from(attrib.constant) << 6)
                    | ((attrib.offset & 0x3fff) << 7)
                    | ((attrib.size.to_raw() & 0x3f) << 21)
                    | ((attrib.attrib_type.to_raw() & 0x7) << 27)
                    | (u32::from(attrib.bgra) << 31)
            }
        }
    }

    pub fn viewport_transform(
        &self,
        index: usize,
    ) -> crate::engines::maxwell_3d::ViewportTransformInfo {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.viewport_transform_info(index as u32),
            Maxwell3DDrawSource::Snapshot(registers) => registers.viewport_transforms[index],
        }
    }

    pub fn scissor(&self, index: usize) -> crate::engines::maxwell_3d::ScissorInfo {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.scissor_info(index as u32),
            Maxwell3DDrawSource::Snapshot(registers) => registers.scissors[index],
        }
    }

    pub fn blend_at(&self, index: usize) -> crate::engines::maxwell_3d::BlendInfo {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.effective_blend_info(index),
            Maxwell3DDrawSource::Snapshot(registers) => registers.blend[index],
        }
    }

    pub fn vertex_streams(&self) -> [crate::engines::maxwell_3d::VertexStreamInfo; 32] {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => {
                std::array::from_fn(|i| maxwell3d.vertex_stream_info(i as u32))
            }
            Maxwell3DDrawSource::Snapshot(registers) => registers.vertex_streams,
        }
    }

    pub fn vertex_stream_instances(&self) -> [u32; 32] {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => {
                std::array::from_fn(|i| maxwell3d.vertex_stream_instance(i as u32))
            }
            Maxwell3DDrawSource::Snapshot(registers) => registers.vertex_stream_instances,
        }
    }

    pub fn vertex_stream_limits(&self) -> [VertexStreamLimit; 32] {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => {
                std::array::from_fn(|i| maxwell3d.vertex_stream_limit(i as u32))
            }
            Maxwell3DDrawSource::Snapshot(registers) => registers.vertex_stream_limits,
        }
    }

    pub fn vertex_attribs(&self) -> [crate::engines::maxwell_3d::VertexAttribInfo; 32] {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => {
                std::array::from_fn(|i| maxwell3d.vertex_attrib_info(i as u32))
            }
            Maxwell3DDrawSource::Snapshot(registers) => registers.vertex_attribs,
        }
    }

    pub fn scissors(
        &self,
    ) -> [crate::engines::maxwell_3d::ScissorInfo; crate::engines::maxwell_3d::NUM_VIEWPORTS] {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => {
                std::array::from_fn(|i| maxwell3d.scissor_info(i as u32))
            }
            Maxwell3DDrawSource::Snapshot(registers) => registers.scissors,
        }
    }

    pub fn blend(&self) -> [crate::engines::maxwell_3d::BlendInfo; 8] {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => {
                std::array::from_fn(|i| maxwell3d.effective_blend_info(i))
            }
            Maxwell3DDrawSource::Snapshot(registers) => registers.blend,
        }
    }

    pub fn blend_per_target_enabled(&self) -> bool {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.blend_per_target_enabled(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.blend_per_target_enabled,
        }
    }

    pub fn global_blend(&self) -> crate::engines::maxwell_3d::BlendInfo {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.global_blend_info(0),
            Maxwell3DDrawSource::Snapshot(registers) => registers.global_blend,
        }
    }

    pub fn iterated_blend_enabled(&self) -> bool {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.iterated_blend_enabled(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.iterated_blend_enabled,
        }
    }

    pub fn blend_color(&self) -> crate::engines::maxwell_3d::BlendColorInfo {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.blend_color_info(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.blend_color,
        }
    }

    pub fn depth_stencil(&self) -> crate::engines::maxwell_3d::DepthStencilInfo {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.depth_stencil_info(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.depth_stencil.clone(),
        }
    }

    pub fn rasterizer(&self) -> crate::engines::maxwell_3d::RasterizerInfo {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.rasterizer_info(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.rasterizer.clone(),
        }
    }

    pub fn rasterize_enable(&self) -> bool {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.rasterize_enable(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.rasterize_enable,
        }
    }

    pub fn primitive_restart(&self) -> crate::engines::maxwell_3d::PrimitiveRestartInfo {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.primitive_restart_info(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.primitive_restart,
        }
    }

    pub fn logic_op(&self) -> crate::engines::maxwell_3d::LogicOpInfo {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.logic_op_info(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.logic_op,
        }
    }

    pub fn frag_color_clamp_any_enabled(&self) -> bool {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.frag_color_clamp_any_enabled(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.frag_color_clamp_any_enabled,
        }
    }

    pub fn anti_alias_alpha_control(
        &self,
    ) -> crate::engines::maxwell_3d::AntiAliasAlphaControlInfo {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.anti_alias_alpha_control_info(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.anti_alias_alpha_control,
        }
    }

    pub fn point_state(&self) -> crate::engines::maxwell_3d::PointStateInfo {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.point_state_info(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.point_state,
        }
    }

    pub fn line_state(&self) -> crate::engines::maxwell_3d::LineStateInfo {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.line_state_info(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.line_state,
        }
    }

    pub fn depth_clamp_enabled(&self) -> bool {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.depth_clamp_enabled(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.depth_clamp_enabled,
        }
    }

    pub fn conservative_raster_enable(&self) -> bool {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.conservative_raster_enable(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.conservative_raster_enable,
        }
    }

    pub fn engine_state(&self) -> crate::engines::maxwell_3d::EngineHint {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.engine_state(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.engine_state,
        }
    }

    pub fn provoking_vertex_last(&self) -> bool {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.provoking_vertex_last(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.provoking_vertex_last,
        }
    }

    pub fn depth_bounds_enable(&self) -> bool {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.depth_bounds_enable(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.depth_bounds_enable,
        }
    }

    pub fn depth_bounds(&self) -> [f32; 2] {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.depth_bounds(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.depth_bounds,
        }
    }

    pub fn line_stipple(&self) -> crate::engines::maxwell_3d::LineStippleInfo {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.line_stipple_info(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.line_stipple,
        }
    }

    pub fn mandated_early_z(&self) -> bool {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.mandated_early_z(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.mandated_early_z,
        }
    }

    pub fn alpha_test_enabled(&self) -> bool {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.alpha_test_enabled(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.alpha_test_enabled,
        }
    }

    pub fn alpha_test_func(&self) -> crate::engines::maxwell_3d::ComparisonOp {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.alpha_test_func(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.alpha_test_func,
        }
    }

    pub fn alpha_test_ref(&self) -> f32 {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.alpha_test_ref(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.alpha_test_ref,
        }
    }

    pub fn tessellation_domain_type(&self) -> u32 {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.tessellation_domain_type(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.tessellation_primitive,
        }
    }

    pub fn tessellation_spacing(&self) -> u32 {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.tessellation_spacing(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.tessellation_spacing,
        }
    }

    pub fn tessellation_clockwise(&self) -> bool {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.tessellation_clockwise(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.tessellation_clockwise,
        }
    }

    pub fn patch_vertices(&self) -> u32 {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.patch_vertices(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.patch_vertices,
        }
    }

    pub fn transform_feedback_enabled(&self) -> bool {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.transform_feedback_enabled(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.transform_feedback_enabled,
        }
    }

    pub fn transform_feedback_state(&self) -> crate::transform_feedback::TransformFeedbackState {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.transform_feedback_state(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.transform_feedback_state,
        }
    }

    pub fn shader_config_enabled(&self, stage: ShaderStageType) -> bool {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.shader_config_enabled(stage),
            Maxwell3DDrawSource::Snapshot(registers) => stage
                .as_index()
                .and_then(|index| registers.shader_config_enabled.get(index as usize).copied())
                .unwrap_or(false),
        }
    }

    pub fn descriptor_sync_regs(
        &self,
    ) -> crate::texture_cache::texture_cache_base::DescriptorSyncRegs {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => {
                crate::texture_cache::texture_cache_base::DescriptorSyncRegs {
                    sampler_binding_via_header: matches!(
                        maxwell3d.sampler_binding(),
                        crate::engines::maxwell_3d::SamplerBinding::ViaHeaderBinding
                    ),
                    tex_header_addr: maxwell3d.tex_header_pool_address(),
                    tex_header_limit: maxwell3d.tex_header_pool_limit(),
                    tex_sampler_addr: maxwell3d.tex_sampler_pool_address(),
                    tex_sampler_limit: maxwell3d.tex_sampler_pool_limit(),
                }
            }
            Maxwell3DDrawSource::Snapshot(registers) => registers.descriptor_sync_regs,
        }
    }

    pub fn zpass_pixel_count_enabled(&self) -> bool {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.zpass_pixel_count_enabled(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.zpass_pixel_count_enabled,
        }
    }

    pub fn window_origin_lower_left(&self) -> bool {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.window_origin_lower_left(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.window_origin_lower_left,
        }
    }

    pub fn window_origin_flip_y(&self) -> bool {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.window_origin_flip_y(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.window_origin_flip_y,
        }
    }

    pub fn viewport0_scale_y(&self) -> f32 {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.viewport_transform_scale_y(0),
            Maxwell3DDrawSource::Snapshot(registers) => registers.viewport0_scale_y,
        }
    }

    pub fn viewport_transforms(
        &self,
    ) -> [crate::engines::maxwell_3d::ViewportTransformInfo;
           crate::engines::maxwell_3d::NUM_VIEWPORTS] {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => {
                std::array::from_fn(|i| maxwell3d.viewport_transform_info(i as u32))
            }
            Maxwell3DDrawSource::Snapshot(registers) => registers.viewport_transforms,
        }
    }

    pub fn viewport_scale_offset_enabled(&self) -> bool {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.viewport_scale_offset_enabled(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.viewport_scale_offset_enabled,
        }
    }

    pub fn surface_clip(&self) -> crate::engines::maxwell_3d::SurfaceClipInfo {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.surface_clip_info(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.surface_clip,
        }
    }

    pub fn framebuffer_srgb(&self) -> bool {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.framebuffer_srgb(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.framebuffer_srgb,
        }
    }

    pub fn user_clip_enable_raw(&self) -> u32 {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.user_clip_enable_raw(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.user_clip_enable_raw,
        }
    }

    pub fn depth_mode(&self) -> crate::engines::maxwell_3d::DepthMode {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.depth_stencil_info().depth_mode,
            Maxwell3DDrawSource::Snapshot(registers) => registers.depth_mode,
        }
    }

    pub fn dirty_flags(&self) -> &[bool; 256] {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.dirty_flags(),
            Maxwell3DDrawSource::Snapshot(registers) => &registers.dirty_flags,
        }
    }

    pub fn dirty_flag(&self, index: u8) -> bool {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.dirty_flag(index),
            Maxwell3DDrawSource::Snapshot(registers) => registers.dirty_flags[index as usize],
        }
    }

    pub fn dirty_flags_ptr(&mut self) -> Option<std::ptr::NonNull<[bool; 256]>> {
        match &mut self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.dirty_flags_ptr(),
            Maxwell3DDrawSource::Snapshot(_) => None,
        }
    }

    /// Clear a consumed Maxwell dirty flag on the live draw source.
    ///
    /// Upstream OpenGL sync helpers clear `maxwell3d->dirty.flags[...]`
    /// inside each `Sync*` method. Snapshot mode is retained for tests and
    /// cannot mutate the source; clearing is therefore a no-op there.
    pub fn clear_dirty_flag(&mut self, index: u8) {
        match &mut self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.clear_dirty_flag(index),
            Maxwell3DDrawSource::Snapshot(registers) => {
                registers.dirty_flags[index as usize] = false;
            }
        }
    }

    /// Write `regs.logic_op.enable` on the live engine, matching the register
    /// mutation performed by upstream's AMD workaround. Snapshot mode keeps
    /// the same state transition for tests and legacy callers.
    pub fn set_logic_op_enabled(&mut self, enabled: bool) {
        match &mut self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.set_logic_op_enabled(enabled),
            Maxwell3DDrawSource::Snapshot(registers) => {
                registers.logic_op.enabled = enabled;
            }
        }
    }

    /// Mark a Maxwell dirty flag from a backend sync helper.
    ///
    /// Upstream `SyncPolygonModes` sets `PolygonModeFront/Back` dirty again
    /// when `fill_via_triangle_mode` forces `GL_FILL_RECTANGLE_NV`; this keeps
    /// the same lifecycle available through the Rust draw-view boundary.
    pub fn set_dirty_flag(&mut self, index: u8) {
        match &mut self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.set_dirty_flag(index),
            Maxwell3DDrawSource::Snapshot(registers) => {
                registers.dirty_flags[index as usize] = true;
            }
        }
    }

    pub fn color_masks(&self) -> [crate::engines::maxwell_3d::ColorMaskInfo; 8] {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => {
                std::array::from_fn(|i| maxwell3d.color_mask_info(i))
            }
            Maxwell3DDrawSource::Snapshot(registers) => registers.color_masks,
        }
    }

    pub fn color_mask(&self, index: usize) -> crate::engines::maxwell_3d::ColorMaskInfo {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.color_mask_info(index),
            Maxwell3DDrawSource::Snapshot(registers) => registers.color_masks[index],
        }
    }

    pub fn color_mask_common(&self) -> bool {
        match &self.source {
            Maxwell3DDrawSource::Live(maxwell3d) => maxwell3d.color_mask_common(),
            Maxwell3DDrawSource::Snapshot(registers) => registers.color_mask_common,
        }
    }
}

fn build_draw_call_snapshot(
    draw_state: &DrawState,
    draw_indexed: bool,
    instance_count: u32,
    maxwell3d: &dyn Maxwell3DAccess,
) -> DrawCall {
    let vertex_streams = std::array::from_fn(|i| maxwell3d.vertex_stream_info(i as u32));

    let vertex_attribs = std::array::from_fn(|i| maxwell3d.vertex_attrib_info(i as u32));

    let mut shader_stages =
        [crate::engines::maxwell_3d::ShaderStageInfo::default(); NUM_SHADER_PROGRAMS];
    for (i, stage) in shader_stages.iter_mut().enumerate() {
        *stage = maxwell3d.shader_stage_info(i as u32);
    }

    let mut color_masks = [crate::engines::maxwell_3d::ColorMaskInfo::default(); 8];
    for (i, mask) in color_masks.iter_mut().enumerate() {
        *mask = maxwell3d.color_mask_info(i);
    }

    let render_targets = std::array::from_fn(|i| maxwell3d.rt_info(i));

    let mut blend = [crate::engines::maxwell_3d::BlendInfo::default(); 8];
    for (i, item) in blend.iter_mut().enumerate() {
        *item = maxwell3d.effective_blend_info(i);
    }

    let cb_bindings = std::array::from_fn(|stage| {
        std::array::from_fn(|slot| maxwell3d.const_buffer_binding(stage, slot))
    });

    DrawCall {
        topology: draw_state.topology,
        vertex_first: draw_state.vertex_buffer.first,
        vertex_count: draw_state.vertex_buffer.count,
        indexed: draw_indexed,
        index_buffer_addr: maxwell3d.index_buffer_addr(),
        index_buffer_addr_end: maxwell3d.index_buffer_addr_end(),
        index_buffer_count: draw_state.index_buffer.count,
        index_buffer_first: draw_state.index_buffer.first,
        index_format: draw_state.index_buffer.format,
        vertex_streams,
        vertex_stream_instances: std::array::from_fn(|i| {
            maxwell3d.vertex_stream_instance(i as u32)
        }),
        vertex_stream_limits: std::array::from_fn(|i| maxwell3d.vertex_stream_limit(i as u32)),
        viewports: std::array::from_fn(|i| maxwell3d.viewport_info(i as u32)),
        viewport_transforms: std::array::from_fn(|i| maxwell3d.viewport_transform_info(i as u32)),
        scissors: std::array::from_fn(|i| maxwell3d.scissor_info(i as u32)),
        viewport_scale_offset_enabled: maxwell3d.viewport_scale_offset_enabled(),
        window_origin_lower_left: maxwell3d.window_origin_lower_left(),
        window_origin_flip_y: maxwell3d.window_origin_flip_y(),
        surface_clip: maxwell3d.surface_clip_info(),
        blend,
        blend_per_target_enabled: maxwell3d.blend_per_target_enabled(),
        global_blend: maxwell3d.global_blend_info(0),
        iterated_blend_enabled: maxwell3d.iterated_blend_enabled(),
        blend_color: maxwell3d.blend_color_info(),
        depth_stencil: maxwell3d.depth_stencil_info(),
        rasterizer: maxwell3d.rasterizer_info(),
        rasterize_enable: maxwell3d.rasterize_enable(),
        primitive_restart: maxwell3d.primitive_restart_info(),
        logic_op: maxwell3d.logic_op_info(),
        depth_clamp_enabled: maxwell3d.depth_clamp_enabled(),
        conservative_raster_enable: maxwell3d.conservative_raster_enable(),
        engine_state: maxwell3d.engine_state(),
        provoking_vertex_last: maxwell3d.provoking_vertex_last(),
        depth_bounds_enable: maxwell3d.depth_bounds_enable(),
        depth_bounds: maxwell3d.depth_bounds(),
        mandated_early_z: maxwell3d.mandated_early_z(),
        alpha_test_enabled: maxwell3d.alpha_test_enabled(),
        alpha_test_func: maxwell3d.alpha_test_func(),
        alpha_test_ref: maxwell3d.alpha_test_ref(),
        point_size: maxwell3d.point_state_info().point_size,
        tessellation_primitive: maxwell3d.tessellation_domain_type(),
        tessellation_spacing: maxwell3d.tessellation_spacing(),
        tessellation_clockwise: maxwell3d.tessellation_clockwise(),
        patch_vertices: maxwell3d.patch_vertices(),
        anti_alias_samples_mode: maxwell3d.anti_alias_samples_mode(),
        anti_alias_alpha_control: maxwell3d.anti_alias_alpha_control_info(),
        line_anti_alias_enable: maxwell3d.line_state_info().line_anti_alias_enable,
        line_stipple: maxwell3d.line_stipple_info(),
        program_base_address: maxwell3d.program_base_address(),
        cb_bindings,
        vertex_attribs,
        shader_stages,
        color_masks,
        rt_control: maxwell3d.rt_control_info(),
        tex_header_pool_addr: maxwell3d.tex_header_pool_address(),
        tex_header_pool_limit: maxwell3d.tex_header_pool_limit(),
        tex_sampler_pool_addr: maxwell3d.tex_sampler_pool_address(),
        tex_sampler_pool_limit: maxwell3d.tex_sampler_pool_limit(),
        instance_count,
        base_instance: draw_state.base_instance,
        base_vertex: draw_state.base_index as i32,
        inline_index_data: draw_state.inline_index_draw_indexes.clone(),
        sampler_binding: maxwell3d.sampler_binding(),
        render_targets,
        zeta: maxwell3d.zeta_info(),
        transform_feedback_enabled: maxwell3d.transform_feedback_enabled(),
        transform_feedback_state: maxwell3d.transform_feedback_state(),
        dirty_flags: *maxwell3d.dirty_flags(),
    }
}

enum Maxwell3DClearSource<'a> {
    Live(Maxwell3DLiveRef<'a>),
    Snapshot {
        clear_state: ClearState,
        render_targets: Maxwell3DRenderTargets,
        dirty_flags: [bool; 256],
    },
}

pub struct Maxwell3DClearView<'a> {
    source: Maxwell3DClearSource<'a>,
}

impl<'a> Maxwell3DClearView<'a> {
    pub fn new(clear_state: ClearState, render_targets: Maxwell3DRenderTargets) -> Self {
        Self {
            source: Maxwell3DClearSource::Snapshot {
                clear_state,
                render_targets,
                dirty_flags: [false; 256],
            },
        }
    }

    #[cfg(test)]
    pub fn with_dirty_snapshot(
        clear_state: ClearState,
        render_targets: Maxwell3DRenderTargets,
        dirty_flags: [bool; 256],
    ) -> Self {
        Self {
            source: Maxwell3DClearSource::Snapshot {
                clear_state,
                render_targets,
                dirty_flags,
            },
        }
    }

    pub fn live(maxwell3d: &'a mut Maxwell3D) -> Self {
        Self {
            source: Maxwell3DClearSource::Live(Maxwell3DLiveRef::new(maxwell3d)),
        }
    }

    pub fn clear_state(&self) -> ClearState {
        match &self.source {
            Maxwell3DClearSource::Live(maxwell3d) => ClearState {
                flags: maxwell3d.clear_surface_flags(),
                color: maxwell3d.clear_color_rgba(),
                depth: maxwell3d.clear_depth(),
                stencil: maxwell3d.clear_stencil(),
            },
            Maxwell3DClearSource::Snapshot { clear_state, .. } => *clear_state,
        }
    }

    pub fn render_targets(&self) -> Maxwell3DRenderTargets {
        match &self.source {
            Maxwell3DClearSource::Live(maxwell3d) => Maxwell3DRenderTargets {
                rt_control: maxwell3d.rt_control_info(),
                render_targets: std::array::from_fn(|i| maxwell3d.rt_info(i)),
                zeta: maxwell3d.zeta_info(),
                anti_alias_samples_mode: maxwell3d.anti_alias_samples_mode(),
                surface_clip: maxwell3d.surface_clip_info(),
            },
            Maxwell3DClearSource::Snapshot { render_targets, .. } => *render_targets,
        }
    }

    pub fn dirty_flags(&self) -> &[bool; 256] {
        match &self.source {
            Maxwell3DClearSource::Live(maxwell3d) => maxwell3d.dirty_flags(),
            Maxwell3DClearSource::Snapshot { dirty_flags, .. } => dirty_flags,
        }
    }

    pub fn clear_dirty_flag(&mut self, index: u8) {
        match &mut self.source {
            Maxwell3DClearSource::Live(maxwell3d) => maxwell3d.clear_dirty_flag(index),
            Maxwell3DClearSource::Snapshot { dirty_flags, .. } => {
                dirty_flags[index as usize] = false;
            }
        }
    }

    pub fn set_dirty_flag(&mut self, index: u8) {
        match &mut self.source {
            Maxwell3DClearSource::Live(maxwell3d) => maxwell3d.set_dirty_flag(index),
            Maxwell3DClearSource::Snapshot { dirty_flags, .. } => {
                dirty_flags[index as usize] = true;
            }
        }
    }

    pub fn use_scissor(&self) -> bool {
        match &self.source {
            Maxwell3DClearSource::Live(maxwell3d) => maxwell3d.clear_control_use_scissor(),
            Maxwell3DClearSource::Snapshot { .. } => false,
        }
    }

    pub fn use_viewport_clip0(&self) -> bool {
        match &self.source {
            Maxwell3DClearSource::Live(maxwell3d) => maxwell3d.clear_control_use_viewport_clip0(),
            Maxwell3DClearSource::Snapshot { .. } => false,
        }
    }

    pub fn scissor(&self, index: u32) -> crate::engines::maxwell_3d::ScissorInfo {
        match &self.source {
            Maxwell3DClearSource::Live(maxwell3d) => maxwell3d.scissor_info(index),
            Maxwell3DClearSource::Snapshot { .. } => Default::default(),
        }
    }

    pub fn scissors(
        &self,
    ) -> [crate::engines::maxwell_3d::ScissorInfo; crate::engines::maxwell_3d::NUM_VIEWPORTS] {
        match &self.source {
            Maxwell3DClearSource::Live(maxwell3d) => {
                std::array::from_fn(|index| maxwell3d.scissor_info(index as u32))
            }
            Maxwell3DClearSource::Snapshot { .. } => Default::default(),
        }
    }

    /// Read whether `regs.window_origin.mode != UpperLeft` for clear-time
    /// scissor conversion. Upstream `RasterizerVulkan::Clear` passes the live
    /// Maxwell register file to `GetScissorState`.
    pub fn window_origin_lower_left(&self) -> bool {
        match &self.source {
            Maxwell3DClearSource::Live(maxwell3d) => maxwell3d.window_origin_lower_left(),
            Maxwell3DClearSource::Snapshot { .. } => false,
        }
    }

    pub fn window_origin_flip_y(&self) -> bool {
        match &self.source {
            Maxwell3DClearSource::Live(maxwell3d) => maxwell3d.window_origin_flip_y(),
            Maxwell3DClearSource::Snapshot { .. } => false,
        }
    }

    pub fn viewport0_scale_y(&self) -> f32 {
        match &self.source {
            Maxwell3DClearSource::Live(maxwell3d) => maxwell3d.viewport_transform_scale_y(0),
            Maxwell3DClearSource::Snapshot { .. } => 0.0,
        }
    }

    pub fn viewport_transforms(
        &self,
    ) -> [crate::engines::maxwell_3d::ViewportTransformInfo;
           crate::engines::maxwell_3d::NUM_VIEWPORTS] {
        match &self.source {
            Maxwell3DClearSource::Live(maxwell3d) => {
                std::array::from_fn(|index| maxwell3d.viewport_transform_info(index as u32))
            }
            Maxwell3DClearSource::Snapshot { .. } => Default::default(),
        }
    }

    pub fn viewport_scale_offset_enabled(&self) -> bool {
        match &self.source {
            Maxwell3DClearSource::Live(maxwell3d) => maxwell3d.viewport_scale_offset_enabled(),
            Maxwell3DClearSource::Snapshot { .. } => false,
        }
    }

    pub fn surface_clip(&self) -> crate::engines::maxwell_3d::SurfaceClipInfo {
        match &self.source {
            Maxwell3DClearSource::Live(maxwell3d) => maxwell3d.surface_clip_info(),
            Maxwell3DClearSource::Snapshot { render_targets, .. } => render_targets.surface_clip,
        }
    }

    pub fn rasterizer(&self) -> crate::engines::maxwell_3d::RasterizerInfo {
        match &self.source {
            Maxwell3DClearSource::Live(maxwell3d) => maxwell3d.rasterizer_info(),
            Maxwell3DClearSource::Snapshot { .. } => Default::default(),
        }
    }

    pub fn depth_mode(&self) -> crate::engines::maxwell_3d::DepthMode {
        match &self.source {
            Maxwell3DClearSource::Live(maxwell3d) => maxwell3d.depth_stencil_info().depth_mode,
            Maxwell3DClearSource::Snapshot { .. } => {
                crate::engines::maxwell_3d::DepthMode::ZeroToOne
            }
        }
    }

    pub fn zpass_pixel_count_enabled(&self) -> bool {
        match &self.source {
            Maxwell3DClearSource::Live(maxwell3d) => maxwell3d.zpass_pixel_count_enabled(),
            Maxwell3DClearSource::Snapshot { .. } => false,
        }
    }

    pub fn depth_stencil(&self) -> crate::engines::maxwell_3d::DepthStencilInfo {
        match &self.source {
            Maxwell3DClearSource::Live(maxwell3d) => maxwell3d.depth_stencil_info(),
            Maxwell3DClearSource::Snapshot { .. } => Default::default(),
        }
    }

    pub fn framebuffer_srgb(&self) -> bool {
        match &self.source {
            Maxwell3DClearSource::Live(maxwell3d) => maxwell3d.framebuffer_srgb(),
            Maxwell3DClearSource::Snapshot { .. } => false,
        }
    }

    pub fn rasterize_enable(&self) -> bool {
        match &self.source {
            Maxwell3DClearSource::Live(maxwell3d) => maxwell3d.rasterize_enable(),
            Maxwell3DClearSource::Snapshot { .. } => true,
        }
    }

    pub fn frag_color_clamp_any_enabled(&self) -> bool {
        match &self.source {
            Maxwell3DClearSource::Live(maxwell3d) => maxwell3d.frag_color_clamp_any_enabled(),
            Maxwell3DClearSource::Snapshot { .. } => false,
        }
    }
}

impl Default for Maxwell3DClearView<'static> {
    fn default() -> Self {
        Self::new(ClearState::default(), Maxwell3DRenderTargets::default())
    }
}

/// View passed to `RasterizerOpenGL::DrawIndirect`.
///
/// Upstream reaches this through `maxwell3d->draw_manager->GetIndirectParams()`
/// and `maxwell3d->regs` from the rasterizer's `Maxwell3D*`.
pub struct Maxwell3DIndirectView<'a> {
    draw_view: Maxwell3DDrawView<'a>,
    indirect_params: &'a IndirectParams,
}

impl<'a> Maxwell3DIndirectView<'a> {
    pub fn new(draw_state: &'a DrawState, indirect_params: &'a IndirectParams) -> Self {
        Self {
            draw_view: Maxwell3DDrawView::new(draw_state, indirect_params.is_indexed),
            indirect_params,
        }
    }

    pub fn live(
        draw_state: &'a DrawState,
        indirect_params: &'a IndirectParams,
        maxwell3d: &'a mut Maxwell3D,
    ) -> Self {
        Self {
            draw_view: Maxwell3DDrawView::live(draw_state, indirect_params.is_indexed, maxwell3d),
            indirect_params,
        }
    }

    pub fn draw_state(&self) -> &'a DrawState {
        self.draw_view.draw_state()
    }

    pub fn params(&self) -> &'a IndirectParams {
        self.indirect_params
    }

    pub fn into_draw_view(self) -> Maxwell3DDrawView<'a> {
        self.draw_view
    }

    pub fn draw_call_snapshot(&self) -> DrawCall {
        self.draw_view.draw_call_snapshot(1)
    }

    pub fn dirty_flags_ptr(&mut self) -> Option<std::ptr::NonNull<[bool; 256]>> {
        self.draw_view.dirty_flags_ptr()
    }

    pub fn clear_dirty_flag(&mut self, index: u8) {
        self.draw_view.clear_dirty_flag(index);
    }

    pub fn set_logic_op_enabled(&mut self, enabled: bool) {
        self.draw_view.set_logic_op_enabled(enabled);
    }

    pub fn draw_view_mut(&mut self) -> &mut Maxwell3DDrawView<'a> {
        &mut self.draw_view
    }
}

impl Default for DrawState {
    fn default() -> Self {
        Self {
            topology: PrimitiveTopology::default(),
            draw_mode: DrawMode::default(),
            draw_indexed: false,
            base_index: 0,
            vertex_buffer: VertexBuffer::default(),
            index_buffer: IndexBuffer::default(),
            base_instance: 0,
            instance_count: 0,
            inline_index_draw_indexes: Vec::new(),
        }
    }
}

/// Vertex stream limit state captured from Maxwell3D registers.
#[derive(Debug, Clone, Copy, Default)]
pub struct VertexStreamLimit {
    pub address: u64,
}

/// Clear state captured from Maxwell3D registers.
#[derive(Debug, Clone, Copy, Default)]
pub struct ClearState {
    pub flags: u32,
    pub color: [f32; 4],
    pub depth: f32,
    pub stencil: i32,
}

/// State for draw-texture operations.
#[derive(Debug, Clone, Copy, Default)]
pub struct DrawTextureState {
    pub dst_x0: f32,
    pub dst_y0: f32,
    pub dst_x1: f32,
    pub dst_y1: f32,
    pub src_x0: f32,
    pub src_y0: f32,
    pub src_x1: f32,
    pub src_y1: f32,
    pub src_sampler: u32,
    pub src_texture: u32,
}

/// Draw-texture-time Maxwell3D view passed to rasterizers.
///
/// Upstream rasterizers read both `DrawManager::GetDrawTextureState()` and
/// live Maxwell3D registers through their persistent `Maxwell3D*`. Rust
/// carries the same data through the existing draw-view boundary instead of
/// storing a movable engine pointer in the backend.
pub struct Maxwell3DDrawTextureView<'a> {
    draw_texture_state: DrawTextureState,
    draw_view: Maxwell3DDrawView<'a>,
}

impl<'a> Maxwell3DDrawTextureView<'a> {
    pub fn new(draw_state: &'a DrawState, draw_texture_state: DrawTextureState) -> Self {
        Self {
            draw_texture_state,
            draw_view: Maxwell3DDrawView::new(draw_state, false),
        }
    }

    pub fn live(
        draw_state: &'a DrawState,
        draw_texture_state: DrawTextureState,
        maxwell3d: &'a mut Maxwell3D,
    ) -> Self {
        Self {
            draw_texture_state,
            draw_view: Maxwell3DDrawView::live(draw_state, false, maxwell3d),
        }
    }

    pub fn draw_texture_state(&self) -> DrawTextureState {
        self.draw_texture_state
    }

    pub fn draw_call_snapshot(&self) -> DrawCall {
        self.draw_view.draw_call_snapshot(1)
    }

    pub fn render_targets(&self) -> Maxwell3DRenderTargets {
        self.draw_view.render_targets()
    }

    pub fn descriptor_sync_regs(
        &self,
    ) -> crate::texture_cache::texture_cache_base::DescriptorSyncRegs {
        self.draw_view.descriptor_sync_regs()
    }

    pub fn dirty_flags(&self) -> &[bool; 256] {
        self.draw_view.dirty_flags()
    }

    pub fn draw_view_mut(&mut self) -> &mut Maxwell3DDrawView<'a> {
        &mut self.draw_view
    }

    pub fn dirty_flags_ptr(&mut self) -> Option<std::ptr::NonNull<[bool; 256]>> {
        self.draw_view.dirty_flags_ptr()
    }

    pub fn clear_dirty_flag(&mut self, index: u8) {
        self.draw_view.clear_dirty_flag(index);
    }

    pub fn set_logic_op_enabled(&mut self, enabled: bool) {
        self.draw_view.set_logic_op_enabled(enabled);
    }

    pub fn zpass_pixel_count_enabled(&self) -> bool {
        self.draw_view.zpass_pixel_count_enabled()
    }
}

/// Raw draw-texture register payload from `Maxwell3D`.
///
/// Corresponds to `Maxwell3D::Regs::DrawTexture`.
#[derive(Debug, Clone, Copy, Default)]
pub struct DrawTextureParams {
    pub dst_x0: i32,
    pub dst_y0: i32,
    pub dst_width: i32,
    pub dst_height: i32,
    pub dx_du: i64,
    pub dy_dv: i64,
    pub src_sampler: u32,
    pub src_texture: u32,
    pub src_x0: i32,
    pub src_y0: i32,
}

/// Parameters for indirect draw calls.
#[derive(Debug, Clone, Copy, Default)]
pub struct IndirectParams {
    pub is_byte_count: bool,
    pub is_indexed: bool,
    pub include_count: bool,
    pub count_start_address: GPUVAddr,
    pub indirect_start_address: GPUVAddr,
    pub buffer_size: usize,
    pub max_draw_counts: usize,
    pub stride: usize,
}

/// Manages draw call processing for the Maxwell 3D engine.
///
/// Corresponds to the C++ `DrawManager` class. Rust uses the
/// `Maxwell3DAccess` trait where Eden passes `Maxwell3D&`, while keeping the
/// same draw state in this owner.
pub struct DrawManager {
    pub draw_state: DrawState,
    pub draw_texture_state: DrawTextureState,
    pub indirect_state: IndirectParams,
    #[cfg(test)]
    compat_draw_calls: Vec<DrawCall>,
}

impl DrawManager {
    /// Create a new DrawManager.
    pub fn new() -> Self {
        Self {
            draw_state: DrawState::default(),
            draw_texture_state: DrawTextureState::default(),
            indirect_state: IndirectParams::default(),
            #[cfg(test)]
            compat_draw_calls: Vec::new(),
        }
    }

    /// Get a reference to the current draw state.
    pub fn get_draw_state(&self) -> &DrawState {
        &self.draw_state
    }

    /// Get a reference to the draw texture state.
    pub fn get_draw_texture_state(&self) -> &DrawTextureState {
        &self.draw_texture_state
    }

    /// Get a mutable reference to indirect draw parameters.
    pub fn get_indirect_params_mut(&mut self) -> &mut IndirectParams {
        &mut self.indirect_state
    }

    /// Get an immutable reference to indirect draw parameters.
    pub fn get_indirect_params(&self) -> &IndirectParams {
        &self.indirect_state
    }

    /// Drain the temporary software-facing compatibility queue.
    #[cfg(test)]
    pub fn take_compat_draw_calls(&mut self) -> Vec<DrawCall> {
        std::mem::take(&mut self.compat_draw_calls)
    }

    /// Push a temporary software-facing compatibility draw snapshot.
    #[cfg(test)]
    pub fn push_compat_draw_call(&mut self, draw_call: DrawCall) {
        self.compat_draw_calls.push(draw_call);
    }

    #[cfg(test)]
    fn build_compat_draw_call(
        &self,
        draw_state: &DrawState,
        draw_indexed: bool,
        instance_count: u32,
        maxwell3d: &dyn Maxwell3DAccess,
    ) -> DrawCall {
        let vertex_streams = std::array::from_fn(|i| maxwell3d.vertex_stream_info(i as u32));

        let vertex_attribs = std::array::from_fn(|i| maxwell3d.vertex_attrib_info(i as u32));

        let mut shader_stages =
            [crate::engines::maxwell_3d::ShaderStageInfo::default(); NUM_SHADER_PROGRAMS];
        for (i, stage) in shader_stages.iter_mut().enumerate() {
            *stage = maxwell3d.shader_stage_info(i as u32);
        }

        let mut color_masks = [crate::engines::maxwell_3d::ColorMaskInfo::default(); 8];
        for (i, mask) in color_masks.iter_mut().enumerate() {
            *mask = maxwell3d.color_mask_info(i);
        }

        let render_targets = std::array::from_fn(|i| maxwell3d.rt_info(i));

        let mut blend = [crate::engines::maxwell_3d::BlendInfo::default(); 8];
        for (i, item) in blend.iter_mut().enumerate() {
            *item = maxwell3d.effective_blend_info(i);
        }

        let cb_bindings = std::array::from_fn(|stage| {
            std::array::from_fn(|slot| maxwell3d.const_buffer_binding(stage, slot))
        });

        DrawCall {
            topology: draw_state.topology,
            vertex_first: draw_state.vertex_buffer.first,
            vertex_count: draw_state.vertex_buffer.count,
            indexed: draw_indexed,
            index_buffer_addr: maxwell3d.index_buffer_addr(),
            index_buffer_addr_end: maxwell3d.index_buffer_addr_end(),
            index_buffer_count: draw_state.index_buffer.count,
            index_buffer_first: draw_state.index_buffer.first,
            index_format: draw_state.index_buffer.format,
            vertex_streams,
            vertex_stream_instances: std::array::from_fn(|i| {
                maxwell3d.vertex_stream_instance(i as u32)
            }),
            vertex_stream_limits: std::array::from_fn(|i| maxwell3d.vertex_stream_limit(i as u32)),
            viewports: std::array::from_fn(|i| maxwell3d.viewport_info(i as u32)),
            viewport_transforms: std::array::from_fn(|i| {
                maxwell3d.viewport_transform_info(i as u32)
            }),
            scissors: std::array::from_fn(|i| maxwell3d.scissor_info(i as u32)),
            viewport_scale_offset_enabled: maxwell3d.viewport_scale_offset_enabled(),
            window_origin_lower_left: maxwell3d.window_origin_lower_left(),
            window_origin_flip_y: maxwell3d.window_origin_flip_y(),
            surface_clip: maxwell3d.surface_clip_info(),
            blend,
            blend_per_target_enabled: maxwell3d.blend_per_target_enabled(),
            global_blend: maxwell3d.global_blend_info(0),
            iterated_blend_enabled: maxwell3d.iterated_blend_enabled(),
            blend_color: maxwell3d.blend_color_info(),
            depth_stencil: maxwell3d.depth_stencil_info(),
            rasterizer: maxwell3d.rasterizer_info(),
            rasterize_enable: maxwell3d.rasterize_enable(),
            primitive_restart: maxwell3d.primitive_restart_info(),
            logic_op: maxwell3d.logic_op_info(),
            depth_clamp_enabled: maxwell3d.depth_clamp_enabled(),
            conservative_raster_enable: maxwell3d.conservative_raster_enable(),
            engine_state: maxwell3d.engine_state(),
            provoking_vertex_last: maxwell3d.provoking_vertex_last(),
            depth_bounds_enable: maxwell3d.depth_bounds_enable(),
            depth_bounds: maxwell3d.depth_bounds(),
            mandated_early_z: maxwell3d.mandated_early_z(),
            alpha_test_enabled: maxwell3d.alpha_test_enabled(),
            alpha_test_func: maxwell3d.alpha_test_func(),
            alpha_test_ref: maxwell3d.alpha_test_ref(),
            point_size: maxwell3d.point_state_info().point_size,
            tessellation_primitive: maxwell3d.tessellation_domain_type(),
            tessellation_spacing: maxwell3d.tessellation_spacing(),
            tessellation_clockwise: maxwell3d.tessellation_clockwise(),
            patch_vertices: maxwell3d.patch_vertices(),
            anti_alias_samples_mode: maxwell3d.anti_alias_samples_mode(),
            anti_alias_alpha_control: maxwell3d.anti_alias_alpha_control_info(),
            line_anti_alias_enable: maxwell3d.line_state_info().line_anti_alias_enable,
            line_stipple: maxwell3d.line_stipple_info(),
            program_base_address: maxwell3d.program_base_address(),
            cb_bindings,
            vertex_attribs,
            shader_stages,
            color_masks,
            rt_control: maxwell3d.rt_control_info(),
            tex_header_pool_addr: maxwell3d.tex_header_pool_address(),
            tex_header_pool_limit: maxwell3d.tex_header_pool_limit(),
            tex_sampler_pool_addr: maxwell3d.tex_sampler_pool_address(),
            tex_sampler_pool_limit: maxwell3d.tex_sampler_pool_limit(),
            instance_count,
            base_instance: draw_state.base_instance,
            base_vertex: draw_state.base_index as i32,
            inline_index_data: draw_state.inline_index_draw_indexes.clone(),
            sampler_binding: maxwell3d.sampler_binding(),
            render_targets,
            zeta: maxwell3d.zeta_info(),
            transform_feedback_enabled: maxwell3d.transform_feedback_enabled(),
            transform_feedback_state: maxwell3d.transform_feedback_state(),
            dirty_flags: *maxwell3d.dirty_flags(),
        }
    }

    /// Process a method call that may trigger draw operations.
    ///
    /// Corresponds to `DrawManager::ProcessMethodCall`.
    pub fn process_method_call(
        &mut self,
        method: u32,
        argument: u32,
        maxwell3d: &mut dyn Maxwell3DAccess,
    ) {
        match method {
            CLEAR_SURFACE => self.clear(1, maxwell3d),
            DRAW_BEGIN => self.draw_begin(maxwell3d),
            DRAW_END => self.draw_end(1, false, maxwell3d),
            VB_FIRST | VB_COUNT => {}
            m if m == IB_BASE + IB_OFF_FIRST => {}
            m if m == IB_BASE + IB_OFF_COUNT => {
                self.draw_state.draw_indexed = true;
            }
            INDEX_BUFFER32_SUBSEQUENT | INDEX_BUFFER16_SUBSEQUENT | INDEX_BUFFER8_SUBSEQUENT => {
                self.draw_state.instance_count = self.draw_state.instance_count.wrapping_add(1);
                self.draw_index_small(argument, maxwell3d);
            }
            INDEX_BUFFER32_FIRST | INDEX_BUFFER16_FIRST | INDEX_BUFFER8_FIRST => {
                self.draw_index_small(argument, maxwell3d);
            }
            DRAW_INLINE_INDEX => {
                self.set_inline_index_buffer(argument);
            }
            INLINE_INDEX_2X16_EVEN => {
                let (even, odd) = maxwell3d.inline_index_2x16_values();
                self.set_inline_index_buffer(even);
                self.set_inline_index_buffer(odd);
            }
            INLINE_INDEX_4X8_INDEX0 => {
                for index in maxwell3d.inline_index_4x8_values() {
                    self.set_inline_index_buffer(index);
                }
            }
            VERTEX_ARRAY_INSTANCE_FIRST => {
                let (topology, start, count) =
                    maxwell3d.vertex_array_instance_first_params(argument);
                self.draw_array_instanced(topology, start, count, false, maxwell3d);
            }
            VERTEX_ARRAY_INSTANCE_SUBSEQUENT => {
                let (topology, start, count) =
                    maxwell3d.vertex_array_instance_subsequent_params(argument);
                self.draw_array_instanced(topology, start, count, true, maxwell3d);
            }
            DRAW_TEXTURE_SRC_Y0 => {
                self.draw_texture(maxwell3d);
            }
            TOPOLOGY_OVERRIDE => {}
            _ => {}
        }
    }

    /// Execute a clear operation.
    ///
    /// Corresponds to `DrawManager::Clear`.
    /// Upstream: `maxwell3d->rasterizer->Clear(layer_count)` if ShouldExecute.
    pub fn clear(&mut self, layer_count: u32, maxwell3d: &mut dyn Maxwell3DAccess) {
        if maxwell3d.should_execute() {
            maxwell3d.clear_rasterizer(layer_count);
        }
    }

    /// Flush any deferred instanced draw calls.
    ///
    /// Corresponds to `DrawManager::DrawDeferred`.
    pub fn draw_deferred(&mut self, maxwell3d: &mut dyn Maxwell3DAccess) {
        if self.draw_state.draw_mode != DrawMode::Instance || self.draw_state.instance_count == 0 {
            return;
        }
        let instance_count = self.draw_state.instance_count.wrapping_add(1);
        self.draw_end(instance_count, true, maxwell3d);
        self.draw_state.instance_count = 0;
    }

    /// Issue a non-indexed draw call.
    ///
    /// Corresponds to `DrawManager::DrawArray`.
    pub fn draw_array(
        &mut self,
        topology: PrimitiveTopology,
        vertex_first: u32,
        vertex_count: u32,
        base_instance: u32,
        num_instances: u32,
        maxwell3d: &mut dyn Maxwell3DAccess,
    ) {
        self.draw_state.topology = topology;
        self.draw_state.vertex_buffer.first = vertex_first;
        self.draw_state.vertex_buffer.count = vertex_count;
        self.draw_state.base_instance = base_instance;
        self.process_draw(false, num_instances, maxwell3d);
    }

    /// Issue an instanced non-indexed draw call.
    ///
    /// Corresponds to `DrawManager::DrawArrayInstanced`.
    pub fn draw_array_instanced(
        &mut self,
        topology: PrimitiveTopology,
        vertex_first: u32,
        vertex_count: u32,
        subsequent: bool,
        maxwell3d: &mut dyn Maxwell3DAccess,
    ) {
        self.draw_state.topology = topology;
        self.draw_state.vertex_buffer.first = vertex_first;
        self.draw_state.vertex_buffer.count = vertex_count;

        if !subsequent {
            self.draw_state.instance_count = 1;
        }

        self.draw_state.base_instance = self.draw_state.instance_count.wrapping_sub(1);
        self.draw_state.draw_mode = DrawMode::Instance;
        self.draw_state.instance_count = self.draw_state.instance_count.wrapping_add(1);
        self.process_draw(false, 1, maxwell3d);
    }

    /// Issue an indexed draw call.
    ///
    /// Corresponds to `DrawManager::DrawIndex`.
    pub fn draw_index(
        &mut self,
        topology: PrimitiveTopology,
        index_first: u32,
        index_count: u32,
        base_index: u32,
        base_instance: u32,
        num_instances: u32,
        maxwell3d: &mut dyn Maxwell3DAccess,
    ) {
        self.draw_state.topology = topology;
        // Upstream: draw_state.index_buffer = regs.index_buffer;
        self.draw_state.index_buffer = maxwell3d.index_buffer();
        self.draw_state.index_buffer.first = index_first;
        self.draw_state.index_buffer.count = index_count;
        self.draw_state.base_index = base_index;
        self.draw_state.base_instance = base_instance;
        self.process_draw(true, num_instances, maxwell3d);
    }

    /// Issue an indirect non-indexed draw call.
    ///
    /// Corresponds to `DrawManager::DrawArrayIndirect`.
    pub fn draw_array_indirect(
        &mut self,
        topology: PrimitiveTopology,
        maxwell3d: &mut dyn Maxwell3DAccess,
    ) {
        self.draw_state.topology = topology;
        self.process_draw_indirect(maxwell3d);
    }

    /// Issue an indirect indexed draw call.
    ///
    /// Corresponds to `DrawManager::DrawIndexedIndirect`.
    pub fn draw_indexed_indirect(
        &mut self,
        topology: PrimitiveTopology,
        index_first: u32,
        index_count: u32,
        maxwell3d: &mut dyn Maxwell3DAccess,
    ) {
        self.draw_state.topology = topology;
        // Upstream: draw_state.index_buffer = regs.index_buffer;
        self.draw_state.index_buffer = maxwell3d.index_buffer();
        self.draw_state.index_buffer.first = index_first;
        self.draw_state.index_buffer.count = index_count;
        self.process_draw_indirect(maxwell3d);
    }

    // ── Private helpers ─────────────────────────────────────────────────

    /// Push 4 bytes (one u32 in LE) into the inline index buffer.
    ///
    /// Corresponds to `DrawManager::SetInlineIndexBuffer`.
    pub fn set_inline_index_buffer(&mut self, index: u32) {
        self.draw_state
            .inline_index_draw_indexes
            .push((index & 0x000000FF) as u8);
        self.draw_state
            .inline_index_draw_indexes
            .push(((index & 0x0000FF00) >> 8) as u8);
        self.draw_state
            .inline_index_draw_indexes
            .push(((index & 0x00FF0000) >> 16) as u8);
        self.draw_state
            .inline_index_draw_indexes
            .push(((index & 0xFF000000) >> 24) as u8);
        self.draw_state.draw_mode = DrawMode::InlineIndex;
    }

    /// Append a batch of packed inline indices.
    ///
    /// Corresponds to the bulk `DrawManager::SetInlineIndexBuffer` overload.
    pub fn set_inline_index_buffer_multi(&mut self, method: u32, base_start: &[u32]) {
        let index_buffer = &mut self.draw_state.inline_index_draw_indexes;
        match method {
            DRAW_INLINE_INDEX => {
                index_buffer.extend_from_slice(bytemuck::cast_slice(base_start));
            }
            INLINE_INDEX_2X16_EVEN => {
                index_buffer.reserve(base_start.len() * 2 * std::mem::size_of::<u32>());
                for &word in base_start {
                    let indexes = [word & 0xFFFF, word >> 16];
                    index_buffer.extend_from_slice(bytemuck::cast_slice(&indexes));
                }
            }
            INLINE_INDEX_4X8_INDEX0 => {
                index_buffer.reserve(base_start.len() * 4 * std::mem::size_of::<u32>());
                for &word in base_start {
                    let indexes = [
                        word & 0xFF,
                        (word >> 8) & 0xFF,
                        (word >> 16) & 0xFF,
                        word >> 24,
                    ];
                    index_buffer.extend_from_slice(bytemuck::cast_slice(&indexes));
                }
            }
            _ => {}
        }
        self.draw_state.draw_mode = DrawMode::InlineIndex;
    }

    /// Handle draw-begin register write.
    ///
    /// Corresponds to `DrawManager::DrawBegin`.
    /// Upstream reads `regs.draw.instance_id` and `regs.draw.topology` from Maxwell3D.
    pub fn draw_begin(&mut self, maxwell3d: &mut dyn Maxwell3DAccess) {
        let (is_first, is_subsequent) = maxwell3d.draw_instance_id();
        if is_first {
            self.draw_deferred(maxwell3d);
            self.draw_state.instance_count = 0;
            self.draw_state.draw_mode = DrawMode::General;
        } else if is_subsequent {
            self.draw_state.instance_count = self.draw_state.instance_count.wrapping_add(1);
            self.draw_state.draw_mode = DrawMode::Instance;
        }

        self.draw_state.topology = maxwell3d.draw_topology();
    }

    /// Handle draw-end, flushing the current draw call.
    ///
    /// Corresponds to `DrawManager::DrawEnd`.
    /// Upstream reads `regs.global_base_instance_index`, `regs.global_base_vertex_index`,
    /// `regs.index_buffer`, `regs.vertex_buffer` from Maxwell3D.
    pub(crate) fn draw_end(
        &mut self,
        instance_count: u32,
        force_draw: bool,
        maxwell3d: &mut dyn Maxwell3DAccess,
    ) {
        match self.draw_state.draw_mode {
            DrawMode::Instance => {
                if !force_draw {
                    return;
                }
                // fallthrough to General (matching upstream [[fallthrough]])
                self.draw_state.base_instance = maxwell3d.global_base_instance_index();
                self.draw_state.base_index = maxwell3d.global_base_vertex_index();
                if self.draw_state.draw_indexed {
                    self.draw_state.index_buffer = maxwell3d.index_buffer();
                    self.process_draw(true, instance_count, maxwell3d);
                } else {
                    self.draw_state.vertex_buffer = maxwell3d.vertex_buffer();
                    self.process_draw(false, instance_count, maxwell3d);
                }
                self.draw_state.draw_indexed = false;
            }
            DrawMode::General => {
                self.draw_state.base_instance = maxwell3d.global_base_instance_index();
                self.draw_state.base_index = maxwell3d.global_base_vertex_index();
                if self.draw_state.draw_indexed {
                    self.draw_state.index_buffer = maxwell3d.index_buffer();
                    self.process_draw(true, instance_count, maxwell3d);
                } else {
                    self.draw_state.vertex_buffer = maxwell3d.vertex_buffer();
                    self.process_draw(false, instance_count, maxwell3d);
                }
                self.draw_state.draw_indexed = false;
            }
            DrawMode::InlineIndex => {
                self.draw_state.base_instance = maxwell3d.global_base_instance_index();
                self.draw_state.base_index = maxwell3d.global_base_vertex_index();
                self.draw_state.index_buffer = maxwell3d.index_buffer();
                self.draw_state.index_buffer.count =
                    (self.draw_state.inline_index_draw_indexes.len() / 4) as u32;
                self.draw_state.index_buffer.format = IndexFormat::UnsignedInt;
                // Upstream: maxwell3d->dirty.flags[VideoCommon::Dirty::IndexBuffer] = true;
                maxwell3d.set_dirty_flag(Dirty::INDEX_BUFFER);
                self.process_draw(true, instance_count, maxwell3d);
                self.draw_state.inline_index_draw_indexes.clear();
            }
        }
    }

    /// Handle small (packed) index draw.
    ///
    /// Corresponds to `DrawManager::DrawIndexSmall`.
    pub fn draw_index_small(&mut self, argument: u32, maxwell3d: &mut dyn Maxwell3DAccess) {
        let index_small = IndexBufferSmall::from_raw(argument);
        self.draw_state.base_instance = maxwell3d.global_base_instance_index();
        self.draw_state.base_index = maxwell3d.global_base_vertex_index();
        self.draw_state.index_buffer = maxwell3d.index_buffer();
        self.draw_state.index_buffer.first = index_small.first;
        self.draw_state.index_buffer.count = index_small.count;
        self.draw_state.topology = index_small.topology;
        // Upstream: maxwell3d->dirty.flags[VideoCommon::Dirty::IndexBuffer] = true;
        maxwell3d.set_dirty_flag(Dirty::INDEX_BUFFER);
        self.process_draw(true, 1, maxwell3d);
    }

    /// Handle draw-texture trigger.
    ///
    /// Corresponds to `DrawManager::DrawTexture`.
    /// Upstream: DrawManager::DrawTexture() in video_core/engines/draw_manager.cpp
    pub fn draw_texture(&mut self, maxwell3d: &mut dyn Maxwell3DAccess) {
        let regs = maxwell3d.draw_texture_params();
        self.draw_texture_state.dst_x0 = regs.dst_x0 as f32 / 4096.0;
        self.draw_texture_state.dst_y0 = regs.dst_y0 as f32 / 4096.0;
        let dst_width = regs.dst_width as f32 / 4096.0;
        let dst_height = regs.dst_height as f32 / 4096.0;
        if maxwell3d.window_origin_lower_left() {
            self.draw_texture_state.dst_y0 =
                maxwell3d.surface_clip_height() as f32 - self.draw_texture_state.dst_y0;
        }
        self.draw_texture_state.dst_x1 = self.draw_texture_state.dst_x0 + dst_width;
        self.draw_texture_state.dst_y1 = self.draw_texture_state.dst_y0 + dst_height;
        self.draw_texture_state.src_x0 = regs.src_x0 as f32 / 4096.0;
        self.draw_texture_state.src_y0 = regs.src_y0 as f32 / 4096.0;
        self.draw_texture_state.src_x1 =
            (regs.dx_du as f32 / 4_294_967_296.0) * dst_width + self.draw_texture_state.src_x0;
        self.draw_texture_state.src_y1 =
            (regs.dy_dv as f32 / 4_294_967_296.0) * dst_height + self.draw_texture_state.src_y0;
        self.draw_texture_state.src_sampler = regs.src_sampler;
        self.draw_texture_state.src_texture = regs.src_texture;
        maxwell3d.draw_texture_rasterizer(&self.draw_state, self.draw_texture_state);
    }

    /// Update topology based on topology control mode and override.
    ///
    /// Corresponds to `DrawManager::UpdateTopology`.
    /// Upstream reads `regs.primitive_topology_control` and `regs.topology_override`.
    fn update_topology(&mut self, maxwell3d: &dyn Maxwell3DAccess) {
        self.draw_state.topology = resolve_draw_topology(
            self.draw_state.topology,
            maxwell3d.primitive_topology_control(),
            maxwell3d.topology_override(),
            maxwell3d.topology_override_raw(),
        );
    }

    /// Core draw dispatch.
    ///
    /// Corresponds to `DrawManager::ProcessDraw`.
    /// Calls UpdateTopology, then rasterizer->Draw if ShouldExecute.
    fn process_draw(
        &mut self,
        draw_indexed: bool,
        instance_count: u32,
        maxwell3d: &mut dyn Maxwell3DAccess,
    ) {
        log::trace!(
            "DrawManager::process_draw: topology={:?}, count={}",
            self.draw_state.topology,
            if draw_indexed {
                self.draw_state.index_buffer.count
            } else {
                self.draw_state.vertex_buffer.count
            }
        );

        self.update_topology(maxwell3d);

        #[cfg(test)]
        {
            let draw_call = self.build_compat_draw_call(
                &self.draw_state,
                draw_indexed,
                instance_count,
                maxwell3d,
            );
            self.compat_draw_calls.push(draw_call);
        }

        let should_execute = maxwell3d.should_execute();

        if should_execute {
            maxwell3d.draw_rasterizer(&self.draw_state, draw_indexed, instance_count);
        }
    }

    /// Core indirect draw dispatch.
    ///
    /// Corresponds to `DrawManager::ProcessDrawIndirect`.
    /// Calls UpdateTopology, then rasterizer->DrawIndirect if ShouldExecute.
    fn process_draw_indirect(&mut self, maxwell3d: &mut dyn Maxwell3DAccess) {
        log::trace!(
            "DrawManager::process_draw_indirect: topology={:?}, is_indexed={}, buffer_size={}",
            self.draw_state.topology,
            self.indirect_state.is_indexed,
            self.indirect_state.buffer_size,
        );

        self.update_topology(maxwell3d);

        if maxwell3d.should_execute() {
            maxwell3d.draw_indirect_rasterizer(&self.draw_state, &self.indirect_state);
        }
    }
}

impl Default for DrawManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_maxwell_view_uses_a_thin_concrete_engine_pointer() {
        assert_eq!(
            std::mem::size_of::<Maxwell3DLiveRef<'static>>(),
            std::mem::size_of::<*mut Maxwell3D>()
        );
    }

    #[test]
    fn test_index_buffer_small_from_raw() {
        // Topology = Triangles (0x4), count = 100, first = 0
        let raw = (0x4u32 << 28) | (100u32 << 16) | 0;
        let ibs = IndexBufferSmall::from_raw(raw);
        assert_eq!(ibs.first, 0);
        assert_eq!(ibs.count, 100);
        assert_eq!(ibs.topology, PrimitiveTopology::Triangles);

        // Topology = Lines (0x1), count = 50, first = 1234
        let raw2 = (0x1u32 << 28) | (50u32 << 16) | 1234;
        let ibs2 = IndexBufferSmall::from_raw(raw2);
        assert_eq!(ibs2.first, 1234);
        assert_eq!(ibs2.count, 50);
        assert_eq!(ibs2.topology, PrimitiveTopology::Lines);
    }

    #[test]
    fn test_index_buffer_small_max_values() {
        // Max first/count and highest valid PrimitiveTopology value (Patches = 0xE).
        let raw = 0xEFFF_FFFF;
        let ibs = IndexBufferSmall::from_raw(raw);
        assert_eq!(ibs.first, 0xFFFF);
        assert_eq!(ibs.count, 0xFFF);
        assert_eq!(ibs.topology, PrimitiveTopology::Patches);
    }

    #[test]
    fn bulk_inline_indices_match_upstream_native_word_layout() {
        let mut manager = DrawManager::new();
        manager.set_inline_index_buffer_multi(DRAW_INLINE_INDEX, &[0x4433_2211, 0x8877_6655]);
        assert_eq!(
            manager.draw_state.inline_index_draw_indexes,
            bytemuck::cast_slice::<u32, u8>(&[0x4433_2211, 0x8877_6655])
        );

        manager.draw_state.inline_index_draw_indexes.clear();
        manager.set_inline_index_buffer_multi(INLINE_INDEX_2X16_EVEN, &[0x4433_2211]);
        assert_eq!(
            manager.draw_state.inline_index_draw_indexes,
            bytemuck::cast_slice::<u32, u8>(&[0x2211, 0x4433])
        );

        manager.draw_state.inline_index_draw_indexes.clear();
        manager.set_inline_index_buffer_multi(INLINE_INDEX_4X8_INDEX0, &[0x4433_2211]);
        assert_eq!(
            manager.draw_state.inline_index_draw_indexes,
            bytemuck::cast_slice::<u32, u8>(&[0x11, 0x22, 0x33, 0x44])
        );
    }

    #[test]
    fn resolve_draw_topology_uses_separate_state_override() {
        let resolved = resolve_draw_topology(
            PrimitiveTopology::TriangleStrip,
            PrimitiveTopologyControl::UseSeparateState,
            PrimitiveTopologyOverride::Lines,
            PrimitiveTopologyOverride::Lines as u32,
        );
        assert_eq!(resolved, PrimitiveTopology::Lines);
    }

    #[test]
    fn resolve_draw_topology_preserves_all_upstream_override_values() {
        let cases = [
            (
                PrimitiveTopologyOverride::Triangles,
                PrimitiveTopology::Triangles,
            ),
            (
                PrimitiveTopologyOverride::TriangleStrip,
                PrimitiveTopology::TriangleStrip,
            ),
            (
                PrimitiveTopologyOverride::LinesAdjacency,
                PrimitiveTopology::LinesAdjacency,
            ),
            (
                PrimitiveTopologyOverride::LineStripAdjacency,
                PrimitiveTopology::LineStripAdjacency,
            ),
            (
                PrimitiveTopologyOverride::TrianglesAdjacency,
                PrimitiveTopology::TrianglesAdjacency,
            ),
            (
                PrimitiveTopologyOverride::TriangleStripAdjacency,
                PrimitiveTopology::TriangleStripAdjacency,
            ),
            (
                PrimitiveTopologyOverride::Patches,
                PrimitiveTopology::Patches,
            ),
        ];

        for (topology_override, expected) in cases {
            assert_eq!(
                resolve_draw_topology(
                    PrimitiveTopology::Points,
                    PrimitiveTopologyControl::UseSeparateState,
                    topology_override,
                    topology_override as u32,
                ),
                expected
            );
        }

        assert_eq!(
            resolve_draw_topology(
                PrimitiveTopology::Points,
                PrimitiveTopologyControl::UseSeparateState,
                PrimitiveTopologyOverride::from_raw(6),
                6,
            ),
            PrimitiveTopology::TriangleFan
        );

        let legacy_cases = [
            PrimitiveTopologyOverride::LegacyPoints,
            PrimitiveTopologyOverride::LegacyIndexedLines,
            PrimitiveTopologyOverride::LegacyIndexedTriangles,
            PrimitiveTopologyOverride::LegacyLines,
            PrimitiveTopologyOverride::LegacyLineStrip,
            PrimitiveTopologyOverride::LegacyIndexedLineStrip,
            PrimitiveTopologyOverride::LegacyTriangles,
            PrimitiveTopologyOverride::LegacyTriangleStrip,
            PrimitiveTopologyOverride::LegacyIndexedTriangleStrip,
            PrimitiveTopologyOverride::LegacyTriangleFan,
            PrimitiveTopologyOverride::LegacyIndexedTriangleFan,
            PrimitiveTopologyOverride::LegacyTriangleFanImm,
            PrimitiveTopologyOverride::LegacyLinesImm,
            PrimitiveTopologyOverride::LegacyIndexedTriangles2,
            PrimitiveTopologyOverride::LegacyIndexedLines2,
        ];
        for topology_override in legacy_cases {
            let resolved = resolve_draw_topology(
                PrimitiveTopology::Points,
                PrimitiveTopologyControl::UseSeparateState,
                topology_override,
                topology_override as u32,
            );
            assert_eq!(resolved as u32, topology_override as u32);
        }
    }

    #[test]
    fn maxwell_draw_view_exposes_draw_boundary_state() {
        let draw_state = DrawState::default();
        let mut registers = Maxwell3DDrawRegisters::default();
        registers.shader_program_addresses = [0x1000, 0x2000, 0, 0, 0, 0];
        registers.index_buffer_gpu_addr = 0x4000;
        registers.index_buffer_gpu_addr_end = 0x4fff;
        registers.descriptor_sync_regs.tex_header_addr = 0x3000;
        registers.window_origin_lower_left = true;
        registers.window_origin_flip_y = true;
        registers.viewport0_scale_y = -1.0;
        registers.dirty_flags
            [crate::renderer_opengl::gl_state_tracker::dirty::VIEWPORTS as usize] = true;

        let view = Maxwell3DDrawView::with_register_snapshot(&draw_state, true, registers);

        assert!(std::ptr::eq(view.draw_state(), &draw_state));
        assert_eq!(
            view.shader_program_addresses(),
            [0x1000, 0x2000, 0, 0, 0, 0]
        );
        let draw = view.draw_call_snapshot(1);
        assert_eq!(draw.index_buffer_addr, 0x4000);
        assert_eq!(draw.index_buffer_addr_end, 0x4fff);
        assert_eq!(view.descriptor_sync_regs().tex_header_addr, 0x3000);
        assert!(view.window_origin_lower_left());
        assert!(view.window_origin_flip_y());
        assert_eq!(view.viewport0_scale_y(), -1.0);
        fn assert_borrowed_dirty_flags(_: &[bool; 256]) {}
        assert_borrowed_dirty_flags(view.dirty_flags());
        assert!(
            view.dirty_flags()[crate::renderer_opengl::gl_state_tracker::dirty::VIEWPORTS as usize]
        );
    }

    #[test]
    fn maxwell_draw_view_shader_config_enabled_uses_snapshot() {
        let draw_state = DrawState::default();
        let mut registers = Maxwell3DDrawRegisters::default();
        registers.shader_config_enabled[ShaderStageType::TessInit.as_index().unwrap() as usize] =
            true;

        let view = Maxwell3DDrawView::with_register_snapshot(&draw_state, false, registers);

        assert!(view.shader_config_enabled(ShaderStageType::VertexB));
        assert!(view.shader_config_enabled(ShaderStageType::TessInit));
        assert!(!view.shader_config_enabled(ShaderStageType::Tessellation));
        assert!(!view.shader_config_enabled(ShaderStageType::Invalid));
    }

    #[test]
    fn maxwell_draw_view_exposes_raw_viewport_register_snapshot() {
        let draw_state = DrawState::default();
        let mut registers = Maxwell3DDrawRegisters::default();
        registers.viewport_transforms[0] = crate::engines::maxwell_3d::ViewportTransformInfo {
            scale_x: 10.0,
            scale_y: -20.0,
            scale_z: 0.25,
            translate_x: 30.0,
            translate_y: 40.0,
            translate_z: 0.5,
            swizzle: 0x3210,
            snap_grid_precision: 0x0504,
        };
        registers.viewport_scale_offset_enabled = true;
        registers.surface_clip = crate::engines::maxwell_3d::SurfaceClipInfo {
            x: 1,
            y: 2,
            width: 1280,
            height: 720,
        };
        registers.depth_mode = crate::engines::maxwell_3d::DepthMode::MinusOneToOne;

        let view = Maxwell3DDrawView::with_register_snapshot(&draw_state, false, registers);

        assert!(view.viewport_scale_offset_enabled());
        assert_eq!(view.viewport_transforms()[0].scale_y, -20.0);
        assert_eq!(view.viewport_transforms()[0].swizzle, 0x3210);
        assert_eq!(view.surface_clip().width, 1280);
        assert_eq!(
            view.depth_mode(),
            crate::engines::maxwell_3d::DepthMode::MinusOneToOne
        );
    }
}
