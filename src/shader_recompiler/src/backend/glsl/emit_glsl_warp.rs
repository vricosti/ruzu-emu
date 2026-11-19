// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! GLSL warp/subgroup operation emission.
//!
//! Maps to upstream `backend/glsl/emit_glsl_warp.cpp`.

use super::glsl_emit_context::EmitContext;
use super::var_alloc::GlslVarType;
use crate::ir::instruction::Inst;

const THREAD_ID: &str = "gl_SubGroupInvocationARB";

fn ballot_index(ctx: &EmitContext) -> &'static str {
    if ctx.profile.warp_size_potentially_larger_than_guest {
        "[gl_SubGroupInvocationARB>>5]"
    } else {
        ".x"
    }
}

fn get_mask(ctx: &EmitContext, mask: &str) -> String {
    format!("uint(uvec2({}){})", mask, ballot_index(ctx))
}

pub fn emit_lane_id(ctx: &mut EmitContext, inst: &mut Inst) {
    let dest = ctx.var_alloc.define(inst, GlslVarType::U32);
    ctx.add_fmt(format!("{dest}={THREAD_ID}&31u;"));
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
pub fn emit_subgroup_eq_mask(ctx: &mut EmitContext, inst: &mut Inst) {
    let value = get_mask(ctx, "gl_SubGroupEqMaskARB");
    let dest = ctx.var_alloc.define(inst, GlslVarType::U32);
    ctx.add_fmt(format!("{dest}={value};"));
}
pub fn emit_subgroup_lt_mask(ctx: &mut EmitContext, inst: &mut Inst) {
    let value = get_mask(ctx, "gl_SubGroupLtMaskARB");
    let dest = ctx.var_alloc.define(inst, GlslVarType::U32);
    ctx.add_fmt(format!("{dest}={value};"));
}
pub fn emit_subgroup_le_mask(ctx: &mut EmitContext, inst: &mut Inst) {
    let value = get_mask(ctx, "gl_SubGroupLeMaskARB");
    let dest = ctx.var_alloc.define(inst, GlslVarType::U32);
    ctx.add_fmt(format!("{dest}={value};"));
}
pub fn emit_subgroup_gt_mask(ctx: &mut EmitContext, inst: &mut Inst) {
    let value = get_mask(ctx, "gl_SubGroupGtMaskARB");
    let dest = ctx.var_alloc.define(inst, GlslVarType::U32);
    ctx.add_fmt(format!("{dest}={value};"));
}
pub fn emit_subgroup_ge_mask(ctx: &mut EmitContext, inst: &mut Inst) {
    let value = get_mask(ctx, "gl_SubGroupGeMaskARB");
    let dest = ctx.var_alloc.define(inst, GlslVarType::U32);
    ctx.add_fmt(format!("{dest}={value};"));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{basic_block::Block, opcodes::Opcode, Program};

    #[test]
    fn lane_id_is_relative_to_guest_warp() {
        let mut program = Program::new(crate::stage::Stage::Compute);
        program.blocks.push(Block::new());
        program.block_mut(0).append_inst(Inst::new(Opcode::LaneId, vec![]));
        let source = super::super::emit_glsl_default(&crate::profile::Profile::default(), &program);
        assert!(source.contains("=gl_SubGroupInvocationARB&31u;"));
    }

    #[test]
    fn subgroup_masks_have_no_operand_and_select_guest_word() {
        for large_warp in [false, true] {
            for (opcode, name) in [
                (Opcode::SubgroupEqMask, "Eq"),
                (Opcode::SubgroupLtMask, "Lt"),
                (Opcode::SubgroupLeMask, "Le"),
                (Opcode::SubgroupGtMask, "Gt"),
                (Opcode::SubgroupGeMask, "Ge"),
            ] {
                let mut profile = crate::profile::Profile::default();
                profile.warp_size_potentially_larger_than_guest = large_warp;
                let mut program = Program::new(crate::stage::Stage::Compute);
                program.blocks.push(Block::new());
                program.block_mut(0).append_inst(Inst::new(opcode, vec![]));
                let source = super::super::emit_glsl_default(&profile, &program);
                let index = if large_warp { "[gl_SubGroupInvocationARB>>5]" } else { ".x" };
                assert!(source.contains(&format!("=uint(uvec2(gl_SubGroup{name}MaskARB){index});")), "{source}");
                assert!(!source.contains("=uvec4("), "mask result must be a scalar");
            }
        }
    }
}
