// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `vk_state_tracker.h` / `vk_state_tracker.cpp`.
//!
//! Dirty flag state tracker for selective GPU state updates.
//! Touch*() methods atomically read-and-clear flags to avoid
//! redundant Vulkan dynamic state commands.

use crate::control::channel_state::ChannelState;
use crate::dirty_flags::{fill_block, setup_dirty_flags, DirtyTables};
use crate::engines::maxwell_3d::{
    PrimitiveTopology, ANTI_ALIAS_ALPHA_CONTROL, BLEND_BASE, BLEND_COLOR_BASE,
    BLEND_PER_TARGET_BASE, BLEND_PER_TARGET_ENABLED, BLEND_PER_TARGET_STRIDE, COLOR_MASK_BASE,
    COLOR_MASK_COMMON, CONSERVATIVE_RASTER_ENABLE, CULL_FACE, CULL_TEST_ENABLE, DEPTH_BIAS,
    DEPTH_BIAS_CLAMP, DEPTH_BOUNDS_BASE, DEPTH_BOUNDS_ENABLE, DEPTH_TEST_ENABLE, DEPTH_TEST_FUNC,
    DEPTH_WRITE_ENABLE, FRONT_FACE, LINE_ANTI_ALIAS_ENABLE, LINE_STIPPLE_ENABLE,
    LINE_STIPPLE_PARAMS, LINE_WIDTH_ALIASED, LINE_WIDTH_SMOOTH, LOGIC_OP,
    POLYGON_OFFSET_FILL_ENABLE, POLYGON_OFFSET_LINE_ENABLE, POLYGON_OFFSET_POINT_ENABLE,
    PRIMITIVE_RESTART_BASE, RASTERIZE_ENABLE, SCISSOR_BASE, SCISSOR_STRIDE, SLOPE_SCALE_DEPTH_BIAS,
    STENCIL_BACK_FUNC_MASK, STENCIL_BACK_MASK, STENCIL_BACK_OP_BASE, STENCIL_BACK_REF,
    STENCIL_ENABLE, STENCIL_FRONT_FUNC_MASK, STENCIL_FRONT_MASK, STENCIL_FRONT_OP_BASE,
    STENCIL_FRONT_REF, STENCIL_TWO_SIDE_ENABLE, VERTEX_ATTRIB_BASE, VERTEX_STREAM_BASE,
    VERTEX_STREAM_INSTANCE_BASE, VERTEX_STREAM_STRIDE, VIEWPORT_BASE, VIEWPORT_CLIP_CONTROL,
    VIEWPORT_SCALE_OFFSET_ENABLED, VIEWPORT_STRIDE, VP_TRANSFORM_BASE, VP_TRANSFORM_STRIDE,
    WINDOW_ORIGIN,
};
use std::ptr::NonNull;

// ---------------------------------------------------------------------------
// Dirty flag indices — port of Vulkan::Dirty enum
// ---------------------------------------------------------------------------

/// Dirty flag indices matching upstream `Vulkan::Dirty::` enum.
///
/// The first few indices (0..LastCommonEntry) are shared with the common
/// dirty flag system in `VideoCommon::Dirty`. Backend-specific flags start
/// after `LastCommonEntry`. We keep the same numeric offsets as upstream
/// to preserve behavioral parity with flag table setup.
pub mod dirty {
    // Common entries (from VideoCommon::Dirty)
    pub const RENDER_TARGETS: u8 = crate::dirty_flags::flags::RENDER_TARGETS;
    pub const COLOR_BUFFER_0: u8 = crate::dirty_flags::flags::COLOR_BUFFER0;
    pub const COLOR_BUFFER_7: u8 = crate::dirty_flags::flags::COLOR_BUFFER7;
    pub const ZETA_BUFFER: u8 = crate::dirty_flags::flags::ZETA_BUFFER;
    pub const VERTEX_BUFFERS: u8 = crate::dirty_flags::flags::VERTEX_BUFFERS;
    pub const VERTEX_BUFFER_0: u8 = crate::dirty_flags::flags::VERTEX_BUFFER0;
    pub const VERTEX_BUFFER_31: u8 = crate::dirty_flags::flags::VERTEX_BUFFER31;
    pub const RESCALE_VIEWPORTS: u8 = crate::dirty_flags::flags::RESCALE_VIEWPORTS;
    pub const RESCALE_SCISSORS: u8 = crate::dirty_flags::flags::RESCALE_SCISSORS;
    pub const DEPTH_BIAS_GLOBAL: u8 = crate::dirty_flags::flags::DEPTH_BIAS_GLOBAL;
    pub const LAST_COMMON_ENTRY: u8 = crate::dirty_flags::flags::LAST_COMMON_ENTRY;

    // Vulkan-specific entries
    pub const FIRST: u8 = LAST_COMMON_ENTRY;
    pub const VERTEX_INPUT: u8 = FIRST + 1;
    pub const VERTEX_ATTRIBUTE_0: u8 = VERTEX_INPUT + 1;
    pub const VERTEX_ATTRIBUTE_31: u8 = VERTEX_ATTRIBUTE_0 + 31;
    pub const VERTEX_BINDING_0: u8 = VERTEX_ATTRIBUTE_31 + 1;
    pub const VERTEX_BINDING_31: u8 = VERTEX_BINDING_0 + 31;

    pub const VIEWPORTS: u8 = VERTEX_BINDING_31 + 1;
    pub const SCISSORS: u8 = VIEWPORTS + 1;
    pub const DEPTH_BIAS: u8 = SCISSORS + 1;
    pub const BLEND_CONSTANTS: u8 = DEPTH_BIAS + 1;
    pub const DEPTH_BOUNDS: u8 = BLEND_CONSTANTS + 1;
    pub const STENCIL_PROPERTIES: u8 = DEPTH_BOUNDS + 1;
    pub const STENCIL_REFERENCE: u8 = STENCIL_PROPERTIES + 1;
    pub const STENCIL_WRITE_MASK: u8 = STENCIL_REFERENCE + 1;
    pub const STENCIL_COMPARE: u8 = STENCIL_WRITE_MASK + 1;
    pub const LINE_WIDTH: u8 = STENCIL_COMPARE + 1;

    pub const CULL_MODE: u8 = LINE_WIDTH + 1;
    pub const DEPTH_BOUNDS_ENABLE: u8 = CULL_MODE + 1;
    pub const DEPTH_TEST_ENABLE: u8 = DEPTH_BOUNDS_ENABLE + 1;
    pub const DEPTH_WRITE_ENABLE: u8 = DEPTH_TEST_ENABLE + 1;
    pub const DEPTH_COMPARE_OP: u8 = DEPTH_WRITE_ENABLE + 1;
    pub const FRONT_FACE: u8 = DEPTH_COMPARE_OP + 1;
    pub const STENCIL_OP: u8 = FRONT_FACE + 1;
    pub const STENCIL_TEST_ENABLE: u8 = STENCIL_OP + 1;
    pub const PRIMITIVE_RESTART_ENABLE: u8 = STENCIL_TEST_ENABLE + 1;
    pub const RASTERIZER_DISCARD_ENABLE: u8 = PRIMITIVE_RESTART_ENABLE + 1;
    pub const CONSERVATIVE_RASTERIZATION_MODE: u8 = RASTERIZER_DISCARD_ENABLE + 1;
    pub const LINE_RASTERIZATION_MODE: u8 = CONSERVATIVE_RASTERIZATION_MODE + 1;
    pub const LINE_STIPPLE_ENABLE: u8 = LINE_RASTERIZATION_MODE + 1;
    pub const LINE_STIPPLE_PARAMS: u8 = LINE_STIPPLE_ENABLE + 1;
    pub const DEPTH_BIAS_ENABLE: u8 = LINE_STIPPLE_PARAMS + 1;
    pub const STATE_ENABLE: u8 = DEPTH_BIAS_ENABLE + 1;
    pub const LOGIC_OP: u8 = STATE_ENABLE + 1;
    pub const LOGIC_OP_ENABLE: u8 = LOGIC_OP + 1;
    pub const DEPTH_CLAMP_ENABLE: u8 = LOGIC_OP_ENABLE + 1;
    pub const ALPHA_TO_COVERAGE_ENABLE: u8 = DEPTH_CLAMP_ENABLE + 1;
    pub const ALPHA_TO_ONE_ENABLE: u8 = ALPHA_TO_COVERAGE_ENABLE + 1;

    pub const BLENDING: u8 = ALPHA_TO_ONE_ENABLE + 1;
    pub const BLEND_ENABLE: u8 = BLENDING + 1;
    pub const BLEND_EQUATIONS: u8 = BLEND_ENABLE + 1;
    pub const COLOR_MASK: u8 = BLEND_EQUATIONS + 1;
    pub const VIEWPORT_SWIZZLES: u8 = COLOR_MASK + 1;

    pub const LAST: u8 = VIEWPORT_SWIZZLES + 1;
}

/// Total number of dirty flags.
const NUM_FLAGS: usize = dirty::LAST as usize;

fn set(tables: &mut DirtyTables, table: usize, offset: u32, flag: u8) {
    tables[table][offset as usize] = flag;
}

fn setup_dirty_viewports(tables: &mut DirtyTables) {
    fill_block(
        &mut tables[0],
        VP_TRANSFORM_BASE as usize,
        16 * VP_TRANSFORM_STRIDE as usize,
        dirty::VIEWPORTS,
    );
    fill_block(
        &mut tables[0],
        VIEWPORT_BASE as usize,
        16 * VIEWPORT_STRIDE as usize,
        dirty::VIEWPORTS,
    );
    set(tables, 0, VIEWPORT_SCALE_OFFSET_ENABLED, dirty::VIEWPORTS);
    set(tables, 1, WINDOW_ORIGIN, dirty::VIEWPORTS);
}

fn setup_dirty_scissors(tables: &mut DirtyTables) {
    fill_block(
        &mut tables[0],
        SCISSOR_BASE as usize,
        16 * SCISSOR_STRIDE as usize,
        dirty::SCISSORS,
    );
}

fn setup_dirty_depth_bias(tables: &mut DirtyTables) {
    for offset in [DEPTH_BIAS, DEPTH_BIAS_CLAMP, SLOPE_SCALE_DEPTH_BIAS] {
        set(tables, 0, offset, dirty::DEPTH_BIAS);
    }
}

fn setup_dirty_blend_constants(tables: &mut DirtyTables) {
    fill_block(
        &mut tables[0],
        BLEND_COLOR_BASE as usize,
        4,
        dirty::BLEND_CONSTANTS,
    );
}

fn setup_dirty_depth_bounds(tables: &mut DirtyTables) {
    fill_block(
        &mut tables[0],
        DEPTH_BOUNDS_BASE as usize,
        2,
        dirty::DEPTH_BOUNDS,
    );
}

fn setup_dirty_stencil_properties(tables: &mut DirtyTables) {
    set(
        tables,
        0,
        STENCIL_TWO_SIDE_ENABLE,
        dirty::STENCIL_PROPERTIES,
    );
    for (offset, flag) in [
        (STENCIL_FRONT_REF, dirty::STENCIL_REFERENCE),
        (STENCIL_FRONT_MASK, dirty::STENCIL_WRITE_MASK),
        (STENCIL_FRONT_FUNC_MASK, dirty::STENCIL_COMPARE),
        (STENCIL_BACK_REF, dirty::STENCIL_REFERENCE),
        (STENCIL_BACK_MASK, dirty::STENCIL_WRITE_MASK),
        (STENCIL_BACK_FUNC_MASK, dirty::STENCIL_COMPARE),
    ] {
        set(tables, 0, offset, flag);
        set(tables, 1, offset, dirty::STENCIL_PROPERTIES);
    }
}

fn setup_dirty_line_width(tables: &mut DirtyTables) {
    set(tables, 0, LINE_WIDTH_SMOOTH, dirty::LINE_WIDTH);
    set(tables, 0, LINE_WIDTH_ALIASED, dirty::LINE_WIDTH);
}

fn setup_dirty_cull_mode(tables: &mut DirtyTables) {
    set(tables, 0, CULL_FACE, dirty::CULL_MODE);
    set(tables, 0, CULL_TEST_ENABLE, dirty::CULL_MODE);
}

fn setup_dirty_state_enable(tables: &mut DirtyTables) {
    for (offset, flag) in [
        (DEPTH_BOUNDS_ENABLE, dirty::DEPTH_BOUNDS_ENABLE),
        (DEPTH_TEST_ENABLE, dirty::DEPTH_TEST_ENABLE),
        (DEPTH_WRITE_ENABLE, dirty::DEPTH_WRITE_ENABLE),
        (STENCIL_ENABLE, dirty::STENCIL_TEST_ENABLE),
        (PRIMITIVE_RESTART_BASE, dirty::PRIMITIVE_RESTART_ENABLE),
        (RASTERIZE_ENABLE, dirty::RASTERIZER_DISCARD_ENABLE),
        (POLYGON_OFFSET_POINT_ENABLE, dirty::DEPTH_BIAS_ENABLE),
        (POLYGON_OFFSET_LINE_ENABLE, dirty::DEPTH_BIAS_ENABLE),
        (POLYGON_OFFSET_FILL_ENABLE, dirty::DEPTH_BIAS_ENABLE),
        (LOGIC_OP, dirty::LOGIC_OP_ENABLE),
        (VIEWPORT_CLIP_CONTROL, dirty::DEPTH_CLAMP_ENABLE),
        (LINE_STIPPLE_ENABLE, dirty::LINE_STIPPLE_ENABLE),
        (ANTI_ALIAS_ALPHA_CONTROL, dirty::ALPHA_TO_COVERAGE_ENABLE),
        (ANTI_ALIAS_ALPHA_CONTROL, dirty::ALPHA_TO_ONE_ENABLE),
    ] {
        set(tables, 0, offset, flag);
        set(tables, 1, offset, dirty::STATE_ENABLE);
    }
}

fn setup_raster_modes(tables: &mut DirtyTables) {
    set(tables, 0, LINE_STIPPLE_PARAMS, dirty::LINE_STIPPLE_PARAMS);
    set(
        tables,
        0,
        CONSERVATIVE_RASTER_ENABLE,
        dirty::CONSERVATIVE_RASTERIZATION_MODE,
    );
    set(
        tables,
        0,
        LINE_ANTI_ALIAS_ENABLE,
        dirty::LINE_RASTERIZATION_MODE,
    );
}

fn setup_dirty_stencil_op(tables: &mut DirtyTables) {
    for base in [STENCIL_FRONT_OP_BASE, STENCIL_BACK_OP_BASE] {
        fill_block(&mut tables[0], base as usize, 4, dirty::STENCIL_OP);
    }
    set(tables, 1, STENCIL_TWO_SIDE_ENABLE, dirty::STENCIL_OP);
}

fn setup_dirty_blending(tables: &mut DirtyTables) {
    set(tables, 0, COLOR_MASK_COMMON, dirty::BLENDING);
    set(tables, 1, COLOR_MASK_COMMON, dirty::COLOR_MASK);
    set(tables, 0, BLEND_PER_TARGET_ENABLED, dirty::BLENDING);
    set(tables, 1, BLEND_PER_TARGET_ENABLED, dirty::BLEND_EQUATIONS);
    fill_block(&mut tables[0], COLOR_MASK_BASE as usize, 8, dirty::BLENDING);
    fill_block(
        &mut tables[1],
        COLOR_MASK_BASE as usize,
        8,
        dirty::COLOR_MASK,
    );
    fill_block(&mut tables[0], BLEND_BASE as usize, 17, dirty::BLENDING);
    fill_block(
        &mut tables[1],
        BLEND_BASE as usize,
        17,
        dirty::BLEND_EQUATIONS,
    );
    fill_block(
        &mut tables[1],
        BLEND_BASE as usize + 9,
        8,
        dirty::BLEND_ENABLE,
    );
    fill_block(
        &mut tables[0],
        BLEND_PER_TARGET_BASE as usize,
        8 * BLEND_PER_TARGET_STRIDE as usize,
        dirty::BLENDING,
    );
    fill_block(
        &mut tables[1],
        BLEND_PER_TARGET_BASE as usize,
        8 * BLEND_PER_TARGET_STRIDE as usize,
        dirty::BLEND_EQUATIONS,
    );
}

fn setup_dirty_viewport_swizzles(tables: &mut DirtyTables) {
    for index in 0..16u32 {
        set(
            tables,
            1,
            VP_TRANSFORM_BASE + index * VP_TRANSFORM_STRIDE + 6,
            dirty::VIEWPORT_SWIZZLES,
        );
    }
}

fn setup_dirty_vertex_attributes(tables: &mut DirtyTables) {
    for index in 0..32u32 {
        set(
            tables,
            0,
            VERTEX_ATTRIB_BASE + index,
            dirty::VERTEX_ATTRIBUTE_0 + index as u8,
        );
    }
    fill_block(
        &mut tables[1],
        VERTEX_ATTRIB_BASE as usize,
        32,
        dirty::VERTEX_INPUT,
    );
}

fn setup_dirty_vertex_bindings(tables: &mut DirtyTables) {
    for index in 0..32u32 {
        let flag = dirty::VERTEX_BINDING_0 + index as u8;
        set(
            tables,
            0,
            VERTEX_STREAM_INSTANCE_BASE + index,
            dirty::VERTEX_INPUT,
        );
        set(tables, 1, VERTEX_STREAM_INSTANCE_BASE + index, flag);
        let divisor = VERTEX_STREAM_BASE + index * VERTEX_STREAM_STRIDE + 3;
        set(tables, 0, divisor, dirty::VERTEX_INPUT);
        set(tables, 1, divisor, flag);
    }
}

/// Install the dirty-register tables consumed by `FixedPipelineState` and the
/// Vulkan dynamic-state tracker.
///
/// The native Metal backend intentionally reuses `FixedPipelineState`, so it
/// must install the same register-to-flag mapping. Keeping the mapping here
/// preserves the single owner corresponding to Eden's `vk_state_tracker.cpp`.
pub(crate) fn setup_pipeline_state_dirty_tables(tables: &mut DirtyTables) {
    setup_dirty_flags(tables);
    setup_dirty_viewports(tables);
    setup_dirty_scissors(tables);
    setup_dirty_depth_bias(tables);
    setup_dirty_blend_constants(tables);
    setup_dirty_depth_bounds(tables);
    setup_dirty_stencil_properties(tables);
    setup_dirty_line_width(tables);
    setup_dirty_cull_mode(tables);
    setup_dirty_state_enable(tables);
    set(tables, 0, DEPTH_TEST_FUNC, dirty::DEPTH_COMPARE_OP);
    set(tables, 0, FRONT_FACE, dirty::FRONT_FACE);
    set(tables, 0, WINDOW_ORIGIN, dirty::FRONT_FACE);
    setup_dirty_stencil_op(tables);
    setup_dirty_blending(tables);
    setup_dirty_viewport_swizzles(tables);
    setup_dirty_vertex_attributes(tables);
    setup_dirty_vertex_bindings(tables);
    set(tables, 0, LOGIC_OP + 1, dirty::LOGIC_OP);
    setup_raster_modes(tables);
}

/// Backing store for dirty flags — a simple boolean array.
/// Port of `Tegra::Engines::Maxwell3D::DirtyState::Flags`.
pub type DirtyFlags = [bool; 256];

// ---------------------------------------------------------------------------
// StencilProperties
// ---------------------------------------------------------------------------

/// Tracked stencil state for a single face.
/// Port of `StateTracker::StencilProperties`.
#[derive(Debug, Clone, Copy, Default)]
struct StencilProperties {
    reference: u32,
    write_mask: u32,
    compare_mask: u32,
}

// ---------------------------------------------------------------------------
// StateTracker
// ---------------------------------------------------------------------------

/// Tracks GPU state changes via dirty flags.
///
/// Port of `Vulkan::StateTracker` class.
///
/// The Touch*() methods check and clear dirty flags, returning true if the
/// corresponding Vulkan dynamic state command needs to be recorded.
pub struct StateTracker {
    flags: DirtyFlags,
    channel_flags: Option<NonNull<DirtyFlags>>,
    invalidation_flags: DirtyFlags,
    current_topology: Option<PrimitiveTopology>,
    bound_channel_id: Option<i32>,
    two_sided_stencil: bool,
    front: StencilProperties,
    back: StencilProperties,
    stencil_reset: bool,
}

impl StateTracker {
    /// Create a new state tracker with all flags dirty.
    pub fn new() -> Self {
        let mut flags = [false; 256];
        let mut invalidation_flags = [false; 256];

        // Start with all Vulkan-specific flags dirty
        for i in 0..NUM_FLAGS {
            flags[i] = true;
            invalidation_flags[i] = true;
        }

        Self {
            flags,
            channel_flags: None,
            invalidation_flags,
            current_topology: None,
            bound_channel_id: None,
            two_sided_stencil: false,
            front: StencilProperties::default(),
            back: StencilProperties::default(),
            stencil_reset: false,
        }
    }

    /// Port of `StateTracker::InvalidateCommandBufferState`.
    fn active_flags_mut(&mut self) -> &mut DirtyFlags {
        match self.channel_flags {
            Some(mut flags) => unsafe { flags.as_mut() },
            None => &mut self.flags,
        }
    }

    pub fn invalidate_command_buffer_state(&mut self) {
        let invalidation_flags = self.invalidation_flags;
        let flags = self.active_flags_mut();
        for i in 0..256 {
            if invalidation_flags[i] {
                flags[i] = true;
            }
        }
        self.current_topology = None;
        self.stencil_reset = true;
    }

    /// Port of `StateTracker::InvalidateState`.
    pub fn invalidate_state(&mut self) {
        for flag in self.active_flags_mut().iter_mut() {
            *flag = true;
        }
        self.current_topology = None;
        self.stencil_reset = true;
    }

    /// Port of `StateTracker::SetupTables`.
    ///
    /// Installs the common and Vulkan-specific lookup tables into the bound
    /// channel's `maxwell_3d->dirty.tables`, in upstream order.
    pub fn setup_tables(&mut self, channel_state: &mut ChannelState) {
        self.bound_channel_id = Some(channel_state.bind_id);
        let Some(maxwell_3d) = channel_state.maxwell_3d.as_mut() else {
            return;
        };
        let tables = maxwell_3d.dirty_tables_mut();
        for table in tables.iter_mut() {
            table.fill(crate::dirty_flags::flags::NULL_ENTRY);
        }
        setup_pipeline_state_dirty_tables(tables);
    }

    /// Port of `StateTracker::ChangeChannel`.
    ///
    /// Upstream switches `flags` to point at the bound channel's live
    /// `maxwell_3d->dirty.flags`. The local array remains only as the upstream
    /// `default_flags` fallback used while no channel is bound.
    pub fn change_channel(&mut self, channel_state: &mut ChannelState) {
        self.bound_channel_id = Some(channel_state.bind_id);
        self.channel_flags = channel_state
            .maxwell_3d
            .as_mut()
            .map(|maxwell| NonNull::from(maxwell.dirty_flags_mut()));
    }

    /// Clear the borrowed channel flag pointer before its owner can be released.
    pub fn release_channel(&mut self, channel_id: i32) {
        if self.bound_channel_id == Some(channel_id) {
            self.channel_flags = None;
            self.bound_channel_id = None;
            self.flags.fill(true);
        }
    }

    #[cfg(test)]
    pub fn bound_channel_id_for_test(&self) -> Option<i32> {
        self.bound_channel_id
    }

    /// Port of `StateTracker::InvalidateViewports`.
    pub fn invalidate_viewports(&mut self) {
        self.active_flags_mut()[dirty::VIEWPORTS as usize] = true;
    }

    /// Port of `StateTracker::InvalidateScissors`.
    pub fn invalidate_scissors(&mut self) {
        self.active_flags_mut()[dirty::SCISSORS as usize] = true;
    }

    // -- Exchange helper (read-and-clear) --

    fn exchange(&mut self, id: u8) -> bool {
        let flags = self.active_flags_mut();
        let is_dirty = flags[id as usize];
        flags[id as usize] = false;
        is_dirty
    }

    fn exchange_check<T: PartialEq + Copy>(old: &mut T, new: T) -> bool {
        let result = *old != new;
        *old = new;
        result
    }

    // -- Touch methods --

    /// Port of `StateTracker::TouchViewports`.
    pub fn touch_viewports(&mut self) -> bool {
        let dirty_viewports = self.exchange(dirty::VIEWPORTS);
        let rescale_viewports = self.exchange(dirty::RESCALE_VIEWPORTS);
        dirty_viewports || rescale_viewports
    }

    /// Port of `StateTracker::TouchScissors`.
    pub fn touch_scissors(&mut self) -> bool {
        let dirty_scissors = self.exchange(dirty::SCISSORS);
        let rescale_scissors = self.exchange(dirty::RESCALE_SCISSORS);
        dirty_scissors || rescale_scissors
    }

    /// Port of `StateTracker::TouchDepthBias`.
    pub fn touch_depth_bias(&mut self) -> bool {
        let a = self.exchange(dirty::DEPTH_BIAS);
        let b = self.exchange(dirty::DEPTH_BIAS_GLOBAL);
        a || b
    }

    /// Port of `StateTracker::TouchBlendConstants`.
    pub fn touch_blend_constants(&mut self) -> bool {
        self.exchange(dirty::BLEND_CONSTANTS)
    }

    /// Port of `StateTracker::TouchDepthBounds`.
    pub fn touch_depth_bounds(&mut self) -> bool {
        self.exchange(dirty::DEPTH_BOUNDS)
    }

    /// Port of `StateTracker::TouchStencilProperties`.
    pub fn touch_stencil_properties(&mut self) -> bool {
        self.exchange(dirty::STENCIL_PROPERTIES)
    }

    /// Port of `StateTracker::TouchStencilReference`.
    pub fn touch_stencil_reference(&mut self) -> bool {
        self.exchange(dirty::STENCIL_REFERENCE)
    }

    /// Port of `StateTracker::TouchStencilWriteMask`.
    pub fn touch_stencil_write_mask(&mut self) -> bool {
        self.exchange(dirty::STENCIL_WRITE_MASK)
    }

    /// Port of `StateTracker::TouchStencilCompare`.
    pub fn touch_stencil_compare(&mut self) -> bool {
        self.exchange(dirty::STENCIL_COMPARE)
    }

    /// Port of `StateTracker::TouchStencilSide`.
    pub fn touch_stencil_side(&mut self, two_sided_stencil_new: bool) -> bool {
        Self::exchange_check(&mut self.two_sided_stencil, two_sided_stencil_new)
            || self.stencil_reset
    }

    /// Port of `StateTracker::CheckStencilReferenceFront`.
    pub fn check_stencil_reference_front(&mut self, new_value: u32) -> bool {
        Self::exchange_check(&mut self.front.reference, new_value) || self.stencil_reset
    }

    /// Port of `StateTracker::CheckStencilReferenceBack`.
    pub fn check_stencil_reference_back(&mut self, new_value: u32) -> bool {
        Self::exchange_check(&mut self.back.reference, new_value) || self.stencil_reset
    }

    /// Port of `StateTracker::CheckStencilWriteMaskFront`.
    pub fn check_stencil_write_mask_front(&mut self, new_value: u32) -> bool {
        Self::exchange_check(&mut self.front.write_mask, new_value) || self.stencil_reset
    }

    /// Port of `StateTracker::CheckStencilWriteMaskBack`.
    pub fn check_stencil_write_mask_back(&mut self, new_value: u32) -> bool {
        Self::exchange_check(&mut self.back.write_mask, new_value) || self.stencil_reset
    }

    /// Port of `StateTracker::CheckStencilCompareMaskFront`.
    pub fn check_stencil_compare_mask_front(&mut self, new_value: u32) -> bool {
        Self::exchange_check(&mut self.front.compare_mask, new_value) || self.stencil_reset
    }

    /// Port of `StateTracker::CheckStencilCompareMaskBack`.
    pub fn check_stencil_compare_mask_back(&mut self, new_value: u32) -> bool {
        Self::exchange_check(&mut self.back.compare_mask, new_value) || self.stencil_reset
    }

    /// Port of `StateTracker::ClearStencilReset`.
    pub fn clear_stencil_reset(&mut self) {
        self.stencil_reset = false;
    }

    /// Port of `StateTracker::TouchLineWidth`.
    pub fn touch_line_width(&mut self) -> bool {
        self.exchange(dirty::LINE_WIDTH)
    }

    /// Port of `StateTracker::TouchCullMode`.
    pub fn touch_cull_mode(&mut self) -> bool {
        self.exchange(dirty::CULL_MODE)
    }

    /// Port of `StateTracker::TouchStateEnable`.
    pub fn touch_state_enable(&mut self) -> bool {
        self.exchange(dirty::STATE_ENABLE)
    }

    /// Port of `StateTracker::TouchDepthBoundsTestEnable`.
    pub fn touch_depth_bounds_test_enable(&mut self) -> bool {
        self.exchange(dirty::DEPTH_BOUNDS_ENABLE)
    }

    /// Port of `StateTracker::TouchDepthTestEnable`.
    pub fn touch_depth_test_enable(&mut self) -> bool {
        self.exchange(dirty::DEPTH_TEST_ENABLE)
    }

    /// Port of `StateTracker::TouchDepthWriteEnable`.
    pub fn touch_depth_write_enable(&mut self) -> bool {
        self.exchange(dirty::DEPTH_WRITE_ENABLE)
    }

    /// Port of `StateTracker::TouchPrimitiveRestartEnable`.
    pub fn touch_primitive_restart_enable(&mut self) -> bool {
        self.exchange(dirty::PRIMITIVE_RESTART_ENABLE)
    }

    /// Port of `StateTracker::TouchRasterizerDiscardEnable`.
    pub fn touch_rasterizer_discard_enable(&mut self) -> bool {
        self.exchange(dirty::RASTERIZER_DISCARD_ENABLE)
    }

    pub fn touch_conservative_rasterization_mode(&mut self) -> bool {
        self.exchange(dirty::CONSERVATIVE_RASTERIZATION_MODE)
    }

    pub fn touch_line_rasterization_mode(&mut self) -> bool {
        self.exchange(dirty::LINE_RASTERIZATION_MODE)
    }

    pub fn touch_line_stipple_enable(&mut self) -> bool {
        self.exchange(dirty::LINE_STIPPLE_ENABLE)
    }

    pub fn touch_line_stipple(&mut self) -> bool {
        self.exchange(dirty::LINE_STIPPLE_PARAMS)
    }

    /// Port of `StateTracker::TouchDepthBiasEnable`.
    pub fn touch_depth_bias_enable(&mut self) -> bool {
        self.exchange(dirty::DEPTH_BIAS_ENABLE)
    }

    /// Port of `StateTracker::TouchLogicOpEnable`.
    pub fn touch_logic_op_enable(&mut self) -> bool {
        self.exchange(dirty::LOGIC_OP_ENABLE)
    }

    /// Port of `StateTracker::TouchDepthClampEnable`.
    pub fn touch_depth_clamp_enable(&mut self) -> bool {
        self.exchange(dirty::DEPTH_CLAMP_ENABLE)
    }

    pub fn touch_alpha_to_coverage_enable(&mut self) -> bool {
        self.exchange(dirty::ALPHA_TO_COVERAGE_ENABLE)
    }

    pub fn touch_alpha_to_one_enable(&mut self) -> bool {
        self.exchange(dirty::ALPHA_TO_ONE_ENABLE)
    }

    /// Port of `StateTracker::TouchDepthCompareOp`.
    pub fn touch_depth_compare_op(&mut self) -> bool {
        self.exchange(dirty::DEPTH_COMPARE_OP)
    }

    /// Port of `StateTracker::TouchFrontFace`.
    pub fn touch_front_face(&mut self) -> bool {
        self.exchange(dirty::FRONT_FACE)
    }

    /// Port of `StateTracker::TouchStencilOp`.
    pub fn touch_stencil_op(&mut self) -> bool {
        self.exchange(dirty::STENCIL_OP)
    }

    /// Port of `StateTracker::TouchBlending`.
    pub fn touch_blending(&mut self) -> bool {
        self.exchange(dirty::BLENDING)
    }

    /// Port of `StateTracker::TouchBlendEnable`.
    pub fn touch_blend_enable(&mut self) -> bool {
        self.exchange(dirty::BLEND_ENABLE)
    }

    /// Port of `StateTracker::TouchBlendEquations`.
    pub fn touch_blend_equations(&mut self) -> bool {
        self.exchange(dirty::BLEND_EQUATIONS)
    }

    /// Port of `StateTracker::TouchColorMask`.
    pub fn touch_color_mask(&mut self) -> bool {
        self.exchange(dirty::COLOR_MASK)
    }

    /// Port of `StateTracker::TouchStencilTestEnable`.
    pub fn touch_stencil_test_enable(&mut self) -> bool {
        self.exchange(dirty::STENCIL_TEST_ENABLE)
    }

    /// Port of `StateTracker::TouchLogicOp`.
    pub fn touch_logic_op(&mut self) -> bool {
        self.exchange(dirty::LOGIC_OP)
    }

    /// Port of `StateTracker::ChangePrimitiveTopology`.
    pub fn change_primitive_topology(&mut self, new_topology: PrimitiveTopology) -> bool {
        let has_changed = self.current_topology != Some(new_topology);
        self.current_topology = Some(new_topology);
        has_changed
    }

    /// Access the dirty flags array directly (for table setup).
    pub fn flags_mut(&mut self) -> &mut DirtyFlags {
        &mut self.flags
    }

    /// Read-only access to dirty flags.
    pub fn flags(&self) -> &DirtyFlags {
        &self.flags
    }
}

impl Default for StateTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::channel_state::ChannelState;

    #[test]
    fn test_new_all_dirty() {
        let tracker = StateTracker::new();
        assert_eq!(dirty::FIRST, crate::dirty_flags::flags::LAST_COMMON_ENTRY);
        assert_eq!(dirty::VERTEX_INPUT, dirty::FIRST + 1);
        assert!(tracker.flags[dirty::VIEWPORTS as usize]);
        assert!(tracker.flags[dirty::SCISSORS as usize]);
        assert!(tracker.flags[dirty::VERTEX_INPUT as usize]);
    }

    #[test]
    fn test_touch_clears_flag() {
        let mut tracker = StateTracker::new();
        assert!(tracker.touch_viewports());
        assert!(!tracker.touch_viewports());
    }

    #[test]
    fn test_invalidate_command_buffer_state() {
        let mut tracker = StateTracker::new();
        tracker.touch_viewports();
        tracker.touch_scissors();
        tracker.touch_depth_bias();
        tracker.invalidate_command_buffer_state();
        assert!(tracker.touch_viewports());
        assert!(tracker.touch_scissors());
        assert!(tracker.touch_depth_bias());
    }

    #[test]
    fn test_touch_does_not_affect_others() {
        let mut tracker = StateTracker::new();
        tracker.touch_viewports();
        assert!(!tracker.flags[dirty::VIEWPORTS as usize]);
        assert!(tracker.flags[dirty::SCISSORS as usize]);
    }

    #[test]
    fn test_change_primitive_topology() {
        let mut tracker = StateTracker::new();
        assert!(tracker.change_primitive_topology(PrimitiveTopology::Triangles));
        assert!(!tracker.change_primitive_topology(PrimitiveTopology::Triangles));
        assert!(tracker.change_primitive_topology(PrimitiveTopology::Lines));
    }

    #[test]
    fn test_stencil_reference_front() {
        let mut tracker = StateTracker::new();
        // First time: stencil_reset is false, but value changes from 0 to 42
        assert!(tracker.check_stencil_reference_front(42));
        // Same value, no stencil_reset
        assert!(!tracker.check_stencil_reference_front(42));
        // Different value
        assert!(tracker.check_stencil_reference_front(100));
    }

    #[test]
    fn test_stencil_reset_forces_update() {
        let mut tracker = StateTracker::new();
        tracker.check_stencil_reference_front(42);
        // Force stencil reset
        tracker.invalidate_command_buffer_state();
        // Even same value returns true due to stencil_reset
        assert!(tracker.check_stencil_reference_front(42));
    }

    #[test]
    fn test_touch_blend_constants() {
        let mut tracker = StateTracker::new();
        assert!(tracker.touch_blend_constants());
        assert!(!tracker.touch_blend_constants());
    }

    #[test]
    fn setup_and_change_channel_track_bound_channel_owner() {
        let mut tracker = StateTracker::new();
        let mut channel = ChannelState::new(9);
        channel.maxwell_3d = Some(Box::new(crate::engines::maxwell_3d::Maxwell3D::new()));
        channel
            .maxwell_3d
            .as_mut()
            .unwrap()
            .dirty_flags_mut()
            .fill(false);

        tracker.setup_tables(&mut channel);
        assert_eq!(tracker.bound_channel_id_for_test(), Some(9));

        let tables = channel.maxwell_3d.as_ref().unwrap().dirty_tables();
        assert_eq!(tables[0][VP_TRANSFORM_BASE as usize], dirty::VIEWPORTS);
        assert_eq!(tables[1][WINDOW_ORIGIN as usize], dirty::VIEWPORTS);
        assert_eq!(tables[0][SCISSOR_BASE as usize], dirty::SCISSORS);
        assert_eq!(tables[0][DEPTH_BIAS as usize], dirty::DEPTH_BIAS);
        assert_eq!(tables[0][BLEND_COLOR_BASE as usize], dirty::BLEND_CONSTANTS);
        assert_eq!(tables[0][DEPTH_BOUNDS_BASE as usize], dirty::DEPTH_BOUNDS);
        assert_eq!(
            tables[1][STENCIL_FRONT_REF as usize],
            dirty::STENCIL_PROPERTIES
        );
        assert_eq!(tables[0][LINE_WIDTH_SMOOTH as usize], dirty::LINE_WIDTH);
        assert_eq!(tables[0][CULL_FACE as usize], dirty::CULL_MODE);
        assert_eq!(tables[1][DEPTH_TEST_ENABLE as usize], dirty::STATE_ENABLE);
        assert_eq!(tables[0][DEPTH_TEST_FUNC as usize], dirty::DEPTH_COMPARE_OP);
        assert_eq!(tables[0][WINDOW_ORIGIN as usize], dirty::FRONT_FACE);
        assert_eq!(tables[0][STENCIL_FRONT_OP_BASE as usize], dirty::STENCIL_OP);
        assert_eq!(tables[1][COLOR_MASK_BASE as usize], dirty::COLOR_MASK);
        assert_eq!(tables[1][BLEND_BASE as usize + 9], dirty::BLEND_ENABLE);
        assert_eq!(
            tables[1][VP_TRANSFORM_BASE as usize + 6],
            dirty::VIEWPORT_SWIZZLES
        );
        assert_eq!(
            tables[0][VERTEX_ATTRIB_BASE as usize],
            dirty::VERTEX_ATTRIBUTE_0
        );
        assert_eq!(
            tables[1][VERTEX_STREAM_BASE as usize + 3],
            dirty::VERTEX_BINDING_0
        );
        assert_eq!(tables[0][LOGIC_OP as usize + 1], dirty::LOGIC_OP);

        tracker.change_channel(&mut channel);
        tracker.invalidate_state();
        assert_eq!(tracker.bound_channel_id_for_test(), Some(9));
        let flags = channel.maxwell_3d.as_mut().unwrap().dirty_flags_mut();
        assert!(flags[crate::dirty_flags::flags::VERTEX_BUFFERS as usize]);
        assert!(flags[crate::dirty_flags::flags::VERTEX_BUFFER0 as usize]);
        flags[dirty::VIEWPORTS as usize] = false;
        flags[dirty::SCISSORS as usize] = false;
        tracker.invalidate_viewports();
        tracker.invalidate_scissors();
        let flags = channel.maxwell_3d.as_mut().unwrap().dirty_flags_mut();
        assert!(flags[dirty::VIEWPORTS as usize]);
        assert!(flags[dirty::SCISSORS as usize]);

        tracker.release_channel(9);
        assert_eq!(tracker.bound_channel_id_for_test(), None);
        channel.maxwell_3d = None;
        assert!(tracker.touch_viewports());
    }
}
