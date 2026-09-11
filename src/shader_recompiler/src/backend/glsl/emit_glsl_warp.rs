// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! GLSL warp/subgroup operation emission.
//!
//! Maps to upstream `backend/glsl/emit_glsl_warp.cpp`.

use super::glsl_emit_context::EmitContext;

pub fn emit_lane_id(ctx: &mut EmitContext) {
    ctx.add_line("u_0=gl_SubgroupInvocationID;");
}
pub fn emit_vote_all(ctx: &mut EmitContext, pred: &str) {
    ctx.add_fmt(format!("b_0=subgroupAll({});", pred));
}
pub fn emit_vote_any(ctx: &mut EmitContext, pred: &str) {
    ctx.add_fmt(format!("b_0=subgroupAny({});", pred));
}
pub fn emit_vote_equal(ctx: &mut EmitContext, pred: &str) {
    ctx.add_fmt(format!("b_0=subgroupAllEqual({});", pred));
}
pub fn emit_subgroup_ballot(ctx: &mut EmitContext, pred: &str) {
    ctx.add_fmt(format!("u4_0=subgroupBallot({});", pred));
}
pub fn emit_subgroup_eq_mask(ctx: &mut EmitContext) {
    ctx.add_line("u4_0=gl_SubgroupEqMask;");
}
pub fn emit_subgroup_lt_mask(ctx: &mut EmitContext) {
    ctx.add_line("u4_0=gl_SubgroupLtMask;");
}
pub fn emit_subgroup_le_mask(ctx: &mut EmitContext) {
    ctx.add_line("u4_0=gl_SubgroupLeMask;");
}
pub fn emit_subgroup_gt_mask(ctx: &mut EmitContext) {
    ctx.add_line("u4_0=gl_SubgroupGtMask;");
}
pub fn emit_subgroup_ge_mask(ctx: &mut EmitContext) {
    ctx.add_line("u4_0=gl_SubgroupGeMask;");
}
pub fn emit_shuffle_index(ctx: &mut EmitContext, v: &str, idx: &str) {
    ctx.add_fmt(format!("u_0=subgroupShuffle({},{});", v, idx));
}
pub fn emit_shuffle_up(ctx: &mut EmitContext, v: &str, delta: &str) {
    ctx.add_fmt(format!("u_0=subgroupShuffleUp({},{});", v, delta));
}
pub fn emit_shuffle_down(ctx: &mut EmitContext, v: &str, delta: &str) {
    ctx.add_fmt(format!("u_0=subgroupShuffleDown({},{});", v, delta));
}
pub fn emit_shuffle_butterfly(ctx: &mut EmitContext, v: &str, mask: &str) {
    ctx.add_fmt(format!("u_0=subgroupShuffleXor({},{});", v, mask));
}
/// Port of `EmitQuadBroadcast` (GLSL).
pub fn emit_quad_broadcast(ctx: &mut EmitContext, v: &str, lane: &str) {
    ctx.add_fmt(format!(
        "u_0=readInvocationARB({},((gl_SubGroupInvocationARB&~3u)|({}&3u)));",
        v, lane
    ));
}
/// Port of `EmitQuadSwap` (GLSL).
pub fn emit_quad_swap(ctx: &mut EmitContext, v: &str, direction: &str) {
    ctx.add_fmt(format!(
        "u_0=readInvocationARB({},(gl_SubGroupInvocationARB^({}+1u)));",
        v, direction
    ));
}
pub fn emit_dpdx_fine(ctx: &mut EmitContext, p: &str) {
    ctx.add_fmt(format!("f_0=dFdxFine({});", p));
}
pub fn emit_dpdy_fine(ctx: &mut EmitContext, p: &str) {
    ctx.add_fmt(format!("f_0=dFdyFine({});", p));
}
pub fn emit_dpdx_coarse(ctx: &mut EmitContext, p: &str) {
    ctx.add_fmt(format!("f_0=dFdxCoarse({});", p));
}
pub fn emit_dpdy_coarse(ctx: &mut EmitContext, p: &str) {
    ctx.add_fmt(format!("f_0=dFdyCoarse({});", p));
}
