// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! SPIR-V warp/subgroup operation emission — maps to zuyu's
//! `backend/spirv/emit_spirv_warp.cpp`.
//!
//! Implements subgroup operations: vote, ballot, shuffle, and derivatives.

use super::spirv_emit_context::SpirvEmitContext;
use crate::ir::{self, Opcode};
use rspirv::spirv::{self, Word};

/// Get the subgroup scope constant.
fn subgroup_scope(ctx: &mut SpirvEmitContext) -> Word {
    ctx.constant_u32(spirv::Scope::Subgroup as u32)
}

fn get_thread_id(ctx: &mut SpirvEmitContext) -> Word {
    ctx.builder
        .load(
            ctx.u32_type,
            None,
            ctx.subgroup_local_invocation_id,
            None,
            [],
        )
        .unwrap()
}

fn warp_extract(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    let thread_id = get_thread_id(ctx);
    let shift = ctx.constant_u32(5);
    let local_index = ctx
        .builder
        .shift_right_arithmetic(ctx.u32_type, None, thread_id, shift)
        .unwrap();
    if ctx
        .profile
        .has_broken_spirv_subgroup_mask_vector_extract_dynamic
    {
        let mut selected = Vec::with_capacity(4);
        for component in 0..4 {
            let index = ctx.constant_u32(component);
            let is_component = ctx
                .builder
                .i_equal(ctx.bool_type, None, local_index, index)
                .unwrap();
            let component = ctx
                .builder
                .composite_extract(ctx.u32_type, None, value, vec![component])
                .unwrap();
            selected.push(
                ctx.builder
                    .select(
                        ctx.u32_type,
                        None,
                        is_component,
                        component,
                        ctx.const_zero_u32,
                    )
                    .unwrap(),
            );
        }
        let first = ctx
            .builder
            .bitwise_or(ctx.u32_type, None, selected[0], selected[1])
            .unwrap();
        let second = ctx
            .builder
            .bitwise_or(ctx.u32_type, None, selected[2], selected[3])
            .unwrap();
        ctx.builder
            .bitwise_or(ctx.u32_type, None, first, second)
            .unwrap()
    } else {
        ctx.builder
            .vector_extract_dynamic(ctx.u32_type, None, value, local_index)
            .unwrap()
    }
}

fn load_mask(ctx: &mut SpirvEmitContext, mask: Word) -> Word {
    let value = ctx
        .builder
        .load(ctx.u32_vec4_type, None, mask, None, [])
        .unwrap();
    if !ctx.profile.warp_size_potentially_larger_than_guest {
        return ctx
            .builder
            .composite_extract(ctx.u32_type, None, value, vec![0])
            .unwrap();
    }
    warp_extract(ctx, value)
}

fn set_in_bounds_flag(ctx: &mut SpirvEmitContext, inst: &ir::Inst, result: Word) {
    let Some(in_bounds) = inst.get_associated_pseudo(Opcode::GetInBoundsFromOp) else {
        return;
    };
    ctx.set_value(in_bounds.block, in_bounds.inst, result);
}

fn compute_min_thread_id(
    ctx: &mut SpirvEmitContext,
    thread_id: Word,
    segmentation_mask: Word,
) -> Word {
    ctx.builder
        .bitwise_and(ctx.u32_type, None, thread_id, segmentation_mask)
        .unwrap()
}

fn compute_max_thread_id(
    ctx: &mut SpirvEmitContext,
    min_thread_id: Word,
    clamp: Word,
    not_seg_mask: Word,
) -> Word {
    let clamped = ctx
        .builder
        .bitwise_and(ctx.u32_type, None, clamp, not_seg_mask)
        .unwrap();
    ctx.builder
        .bitwise_or(ctx.u32_type, None, min_thread_id, clamped)
        .unwrap()
}

fn get_max_thread_id(
    ctx: &mut SpirvEmitContext,
    thread_id: Word,
    clamp: Word,
    segmentation_mask: Word,
) -> Word {
    let not_seg_mask = ctx
        .builder
        .not(ctx.u32_type, None, segmentation_mask)
        .unwrap();
    let min_thread_id = compute_min_thread_id(ctx, thread_id, segmentation_mask);
    compute_max_thread_id(ctx, min_thread_id, clamp, not_seg_mask)
}

fn select_value(
    ctx: &mut SpirvEmitContext,
    in_range: Word,
    value: Word,
    src_thread_id: Word,
) -> Word {
    let scope = subgroup_scope(ctx);
    let shuffled = ctx
        .builder
        .group_non_uniform_shuffle(ctx.u32_type, None, scope, value, src_thread_id)
        .unwrap();
    ctx.builder
        .select(ctx.u32_type, None, in_range, shuffled, value)
        .unwrap()
}

fn add_partition_base(ctx: &mut SpirvEmitContext, thread_id: Word) -> Word {
    let host_thread_id = get_thread_id(ctx);
    let five = ctx.constant_u32(5);
    let partition_idx = ctx
        .builder
        .shift_right_logical(ctx.u32_type, None, host_thread_id, five)
        .unwrap();
    let partition_base = ctx
        .builder
        .shift_left_logical(ctx.u32_type, None, partition_idx, five)
        .unwrap();
    ctx.builder
        .i_add(ctx.u32_type, None, thread_id, partition_base)
        .unwrap()
}

/// Emit lane ID (subgroup local invocation ID, masked to 31 for >32 warp).
///
/// Matches upstream `EmitLaneId(EmitContext&)`.
pub fn emit_lane_id(ctx: &mut SpirvEmitContext) -> Word {
    let id = get_thread_id(ctx);
    if !ctx.profile.warp_size_potentially_larger_than_guest {
        return id;
    }
    let mask = ctx.constant_u32(31);
    ctx.builder
        .bitwise_and(ctx.u32_type, None, id, mask)
        .unwrap()
}

/// Emit `OpGroupNonUniformAll` (VOTE.ALL).
///
/// Matches upstream `EmitVoteAll(EmitContext&, Id)`.
pub fn emit_vote_all(ctx: &mut SpirvEmitContext, pred: Word) -> Word {
    if ctx.profile.warp_size_potentially_larger_than_guest {
        let scope = subgroup_scope(ctx);
        let mask_ballot = ctx
            .builder
            .group_non_uniform_ballot(ctx.u32_vec4_type, None, scope, ctx.const_true)
            .unwrap();
        let active_mask = warp_extract(ctx, mask_ballot);
        let ballot = ctx
            .builder
            .group_non_uniform_ballot(ctx.u32_vec4_type, None, scope, pred)
            .unwrap();
        let ballot = warp_extract(ctx, ballot);
        let lhs = ctx
            .builder
            .bitwise_and(ctx.u32_type, None, ballot, active_mask)
            .unwrap();
        return ctx
            .builder
            .i_equal(ctx.bool_type, None, lhs, active_mask)
            .unwrap();
    }
    let scope = subgroup_scope(ctx);
    ctx.builder
        .group_non_uniform_all(ctx.bool_type, None, scope, pred)
        .unwrap()
}

/// Emit `OpGroupNonUniformAny` (VOTE.ANY).
///
/// Matches upstream `EmitVoteAny(EmitContext&, Id)`.
pub fn emit_vote_any(ctx: &mut SpirvEmitContext, pred: Word) -> Word {
    if ctx.profile.warp_size_potentially_larger_than_guest {
        let scope = subgroup_scope(ctx);
        let mask_ballot = ctx
            .builder
            .group_non_uniform_ballot(ctx.u32_vec4_type, None, scope, ctx.const_true)
            .unwrap();
        let active_mask = warp_extract(ctx, mask_ballot);
        let ballot = ctx
            .builder
            .group_non_uniform_ballot(ctx.u32_vec4_type, None, scope, pred)
            .unwrap();
        let ballot = warp_extract(ctx, ballot);
        let lhs = ctx
            .builder
            .bitwise_and(ctx.u32_type, None, ballot, active_mask)
            .unwrap();
        return ctx
            .builder
            .i_not_equal(ctx.bool_type, None, lhs, ctx.const_zero_u32)
            .unwrap();
    }
    let scope = subgroup_scope(ctx);
    ctx.builder
        .group_non_uniform_any(ctx.bool_type, None, scope, pred)
        .unwrap()
}

/// Emit `OpGroupNonUniformAllEqual` (VOTE.EQ).
///
/// Matches upstream `EmitVoteEqual(EmitContext&, Id)`.
pub fn emit_vote_equal(ctx: &mut SpirvEmitContext, pred: Word) -> Word {
    if ctx.profile.warp_size_potentially_larger_than_guest {
        let scope = subgroup_scope(ctx);
        let mask_ballot = ctx
            .builder
            .group_non_uniform_ballot(ctx.u32_vec4_type, None, scope, ctx.const_true)
            .unwrap();
        let active_mask = warp_extract(ctx, mask_ballot);
        let ballot = ctx
            .builder
            .group_non_uniform_ballot(ctx.u32_vec4_type, None, scope, pred)
            .unwrap();
        let ballot = warp_extract(ctx, ballot);
        let lhs = ctx
            .builder
            .bitwise_xor(ctx.u32_type, None, ballot, active_mask)
            .unwrap();
        let all_false = ctx
            .builder
            .i_equal(ctx.bool_type, None, lhs, ctx.const_zero_u32)
            .unwrap();
        let all_true = ctx
            .builder
            .i_equal(ctx.bool_type, None, lhs, active_mask)
            .unwrap();
        return ctx
            .builder
            .logical_or(ctx.bool_type, None, all_false, all_true)
            .unwrap();
    }
    let scope = subgroup_scope(ctx);
    ctx.builder
        .group_non_uniform_all_equal(ctx.bool_type, None, scope, pred)
        .unwrap()
}

/// Emit `OpGroupNonUniformBallot`.
///
/// Matches upstream `EmitSubgroupBallot(EmitContext&, Id)`.
pub fn emit_subgroup_ballot(ctx: &mut SpirvEmitContext, pred: Word) -> Word {
    let scope = subgroup_scope(ctx);
    let ballot = ctx
        .builder
        .group_non_uniform_ballot(ctx.u32_vec4_type, None, scope, pred)
        .unwrap();
    if ctx.profile.warp_size_potentially_larger_than_guest {
        warp_extract(ctx, ballot)
    } else {
        ctx.builder
            .composite_extract(ctx.u32_type, None, ballot, vec![0])
            .unwrap()
    }
}

/// Emit subgroup eq mask.
///
/// Matches upstream `EmitSubgroupEqMask(EmitContext&)`.
pub fn emit_subgroup_eq_mask(ctx: &mut SpirvEmitContext) -> Word {
    load_mask(ctx, ctx.subgroup_mask_eq)
}

/// Emit subgroup lt mask.
pub fn emit_subgroup_lt_mask(ctx: &mut SpirvEmitContext) -> Word {
    load_mask(ctx, ctx.subgroup_mask_lt)
}

/// Emit subgroup le mask.
pub fn emit_subgroup_le_mask(ctx: &mut SpirvEmitContext) -> Word {
    load_mask(ctx, ctx.subgroup_mask_le)
}

/// Emit subgroup gt mask.
pub fn emit_subgroup_gt_mask(ctx: &mut SpirvEmitContext) -> Word {
    load_mask(ctx, ctx.subgroup_mask_gt)
}

/// Emit subgroup ge mask.
pub fn emit_subgroup_ge_mask(ctx: &mut SpirvEmitContext) -> Word {
    load_mask(ctx, ctx.subgroup_mask_ge)
}

/// Emit shuffle index (SHFL.IDX).
///
/// Matches upstream `EmitShuffleIndex(EmitContext&, ...)`.
pub fn emit_shuffle_index(
    ctx: &mut SpirvEmitContext,
    inst: &ir::Inst,
    value: Word,
    index: Word,
    clamp: Word,
    segmentation_mask: Word,
) -> Word {
    let not_seg_mask = ctx
        .builder
        .not(ctx.u32_type, None, segmentation_mask)
        .unwrap();
    let thread_id = emit_lane_id(ctx);
    let min_thread_id = compute_min_thread_id(ctx, thread_id, segmentation_mask);
    let max_thread_id = compute_max_thread_id(ctx, min_thread_id, clamp, not_seg_mask);
    let lhs = ctx
        .builder
        .bitwise_and(ctx.u32_type, None, index, not_seg_mask)
        .unwrap();
    let mut src_thread_id = ctx
        .builder
        .bitwise_or(ctx.u32_type, None, lhs, min_thread_id)
        .unwrap();
    let in_range = ctx
        .builder
        .s_less_than_equal(ctx.bool_type, None, src_thread_id, max_thread_id)
        .unwrap();
    if ctx.profile.warp_size_potentially_larger_than_guest {
        src_thread_id = add_partition_base(ctx, src_thread_id);
    }
    set_in_bounds_flag(ctx, inst, in_range);
    select_value(ctx, in_range, value, src_thread_id)
}

/// Emit shuffle up (SHFL.UP).
pub fn emit_shuffle_up(
    ctx: &mut SpirvEmitContext,
    inst: &ir::Inst,
    value: Word,
    delta: Word,
    clamp: Word,
    segmentation_mask: Word,
) -> Word {
    let thread_id = emit_lane_id(ctx);
    let max_thread_id = get_max_thread_id(ctx, thread_id, clamp, segmentation_mask);
    let mut src_thread_id = ctx
        .builder
        .i_sub(ctx.u32_type, None, thread_id, delta)
        .unwrap();
    let in_range = ctx
        .builder
        .s_greater_than_equal(ctx.bool_type, None, src_thread_id, max_thread_id)
        .unwrap();
    if ctx.profile.warp_size_potentially_larger_than_guest {
        src_thread_id = add_partition_base(ctx, src_thread_id);
    }
    set_in_bounds_flag(ctx, inst, in_range);
    select_value(ctx, in_range, value, src_thread_id)
}

/// Emit shuffle down (SHFL.DOWN).
pub fn emit_shuffle_down(
    ctx: &mut SpirvEmitContext,
    inst: &ir::Inst,
    value: Word,
    delta: Word,
    clamp: Word,
    segmentation_mask: Word,
) -> Word {
    let thread_id = emit_lane_id(ctx);
    let max_thread_id = get_max_thread_id(ctx, thread_id, clamp, segmentation_mask);
    let mut src_thread_id = ctx
        .builder
        .i_add(ctx.u32_type, None, thread_id, delta)
        .unwrap();
    let in_range = ctx
        .builder
        .s_less_than_equal(ctx.bool_type, None, src_thread_id, max_thread_id)
        .unwrap();
    if ctx.profile.warp_size_potentially_larger_than_guest {
        src_thread_id = add_partition_base(ctx, src_thread_id);
    }
    set_in_bounds_flag(ctx, inst, in_range);
    select_value(ctx, in_range, value, src_thread_id)
}

/// Emit shuffle butterfly (SHFL.BFLY).
pub fn emit_shuffle_butterfly(
    ctx: &mut SpirvEmitContext,
    inst: &ir::Inst,
    value: Word,
    index: Word,
    clamp: Word,
    segmentation_mask: Word,
) -> Word {
    let thread_id = emit_lane_id(ctx);
    let max_thread_id = get_max_thread_id(ctx, thread_id, clamp, segmentation_mask);
    let mut src_thread_id = ctx
        .builder
        .bitwise_xor(ctx.u32_type, None, thread_id, index)
        .unwrap();
    let in_range = ctx
        .builder
        .s_less_than_equal(ctx.bool_type, None, src_thread_id, max_thread_id)
        .unwrap();
    if ctx.profile.warp_size_potentially_larger_than_guest {
        src_thread_id = add_partition_base(ctx, src_thread_id);
    }
    set_in_bounds_flag(ctx, inst, in_range);
    select_value(ctx, in_range, value, src_thread_id)
}

/// Matches upstream `EmitFSwizzleAdd`.
pub fn emit_fswizzle_add(
    ctx: &mut SpirvEmitContext,
    op_a: Word,
    op_b: Word,
    swizzle: Word,
) -> Word {
    let three = ctx.constant_u32(3);
    let mut mask = get_thread_id(ctx);
    mask = ctx
        .builder
        .bitwise_and(ctx.u32_type, None, mask, three)
        .unwrap();
    let one = ctx.constant_u32(1);
    mask = ctx
        .builder
        .shift_left_logical(ctx.u32_type, None, mask, one)
        .unwrap();
    mask = ctx
        .builder
        .shift_right_logical(ctx.u32_type, None, swizzle, mask)
        .unwrap();
    mask = ctx
        .builder
        .bitwise_and(ctx.u32_type, None, mask, three)
        .unwrap();
    let modifier_a = ctx
        .builder
        .vector_extract_dynamic(ctx.f32_type, None, ctx.fswzadd_lut_a, mask)
        .unwrap();
    let modifier_b = ctx
        .builder
        .vector_extract_dynamic(ctx.f32_type, None, ctx.fswzadd_lut_b, mask)
        .unwrap();
    let result_a = ctx
        .builder
        .f_mul(ctx.f32_type, None, op_a, modifier_a)
        .unwrap();
    let result_b = ctx
        .builder
        .f_mul(ctx.f32_type, None, op_b, modifier_b)
        .unwrap();
    ctx.builder
        .f_add(ctx.f32_type, None, result_a, result_b)
        .unwrap()
}

/// Emit DPdxFine: `OpDPdxFine`.
pub fn emit_dpdx_fine(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    ctx.builder.d_pdx_fine(ctx.f32_type, None, value).unwrap()
}

/// Emit DPdyFine: `OpDPdyFine`.
pub fn emit_dpdy_fine(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    ctx.builder.d_pdy_fine(ctx.f32_type, None, value).unwrap()
}

/// Emit DPdxCoarse: `OpDPdxCoarse`.
pub fn emit_dpdx_coarse(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    ctx.builder.d_pdx_coarse(ctx.f32_type, None, value).unwrap()
}

/// Emit DPdyCoarse: `OpDPdyCoarse`.
pub fn emit_dpdy_coarse(ctx: &mut SpirvEmitContext, value: Word) -> Word {
    ctx.builder.d_pdy_coarse(ctx.f32_type, None, value).unwrap()
}

/// Port of `EmitQuadBroadcast`: broadcast `value` from lane `lane` of the
/// invocation's quad (OpGroupNonUniformQuadBroadcast when supported, else a
/// shuffle from `(thread_id & ~3) | (lane & 3)`).
pub fn emit_quad_broadcast(ctx: &mut SpirvEmitContext, value: Word, lane: Word) -> Word {
    let scope = subgroup_scope(ctx);
    if ctx.profile.support_quad_shuffles {
        return ctx
            .builder
            .group_non_uniform_quad_broadcast(ctx.u32_type, None, scope, value, lane)
            .unwrap();
    }
    let thread_id = get_thread_id(ctx);
    let not_three = ctx.constant_u32(!3u32);
    let three = ctx.constant_u32(3);
    let base = ctx
        .builder
        .bitwise_and(ctx.u32_type, None, thread_id, not_three)
        .unwrap();
    let local_lane = ctx
        .builder
        .bitwise_and(ctx.u32_type, None, lane, three)
        .unwrap();
    let src_thread_id = ctx
        .builder
        .bitwise_or(ctx.u32_type, None, base, local_lane)
        .unwrap();
    ctx.builder
        .group_non_uniform_shuffle(ctx.u32_type, None, scope, value, src_thread_id)
        .unwrap()
}

/// Port of `EmitQuadSwap`: swap within the quad along `direction`
/// (0 = horizontal, 1 = vertical, 2 = diagonal) via a shuffle-xor of
/// `direction + 1`.
pub fn emit_quad_swap(ctx: &mut SpirvEmitContext, value: Word, direction: Word) -> Word {
    let scope = subgroup_scope(ctx);
    let one = ctx.constant_u32(1);
    let xor_mask = ctx
        .builder
        .i_add(ctx.u32_type, None, direction, one)
        .unwrap();
    ctx.builder
        .group_non_uniform_shuffle_xor(ctx.u32_type, None, scope, value, xor_mask)
        .unwrap()
}
