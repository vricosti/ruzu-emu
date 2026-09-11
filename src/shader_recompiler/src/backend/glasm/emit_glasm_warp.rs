// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! GLASM warp/subgroup operation emission.
//!
//! Maps to upstream `backend/glasm/emit_glasm_warp.cpp`.

use super::glasm_emit_context::EmitContext;

pub fn emit_lane_id(ctx: &mut EmitContext) {
    let sn = ctx.stage_name;
    ctx.add_fmt(format!("MOV.S RC.x,{}.threadid;", sn));
}

pub fn emit_vote_all(ctx: &mut EmitContext) {
    ctx.add_line("TGALL.S RC.x,RC.x;");
}

pub fn emit_vote_any(ctx: &mut EmitContext) {
    ctx.add_line("TGANY.S RC.x,RC.x;");
}

pub fn emit_vote_equal(ctx: &mut EmitContext) {
    ctx.add_line("TGEQ.S RC.x,RC.x;");
}

pub fn emit_subgroup_ballot(ctx: &mut EmitContext) {
    ctx.add_line("TGBALLOT RC.x,RC.x;");
}

pub fn emit_subgroup_eq_mask(ctx: &mut EmitContext) {
    let sn = ctx.stage_name;
    ctx.add_fmt(format!("MOV.U RC,{}.threadeqmask;", sn));
}

pub fn emit_subgroup_lt_mask(ctx: &mut EmitContext) {
    let sn = ctx.stage_name;
    ctx.add_fmt(format!("MOV.U RC,{}.threadltmask;", sn));
}

pub fn emit_subgroup_le_mask(ctx: &mut EmitContext) {
    let sn = ctx.stage_name;
    ctx.add_fmt(format!("MOV.U RC,{}.threadlemask;", sn));
}

pub fn emit_subgroup_gt_mask(ctx: &mut EmitContext) {
    let sn = ctx.stage_name;
    ctx.add_fmt(format!("MOV.U RC,{}.threadgtmask;", sn));
}

pub fn emit_subgroup_ge_mask(ctx: &mut EmitContext) {
    let sn = ctx.stage_name;
    ctx.add_fmt(format!("MOV.U RC,{}.threadgemask;", sn));
}

pub fn emit_shuffle_index(ctx: &mut EmitContext) {
    ctx.add_line("SHFIDX.U RC,RC.x,RC.y,RC.z;");
}

pub fn emit_shuffle_up(ctx: &mut EmitContext) {
    ctx.add_line("SHFUP.U RC,RC.x,RC.y,RC.z;");
}

pub fn emit_shuffle_down(ctx: &mut EmitContext) {
    ctx.add_line("SHFDOWN.U RC,RC.x,RC.y,RC.z;");
}

pub fn emit_shuffle_butterfly(ctx: &mut EmitContext) {
    ctx.add_line("SHFXOR.U RC,RC.x,RC.y,RC.z;");
}

/// Port of `EmitQuadBroadcast` (GLASM): value in RC.x, lane in RC.y.
pub fn emit_quad_broadcast(ctx: &mut EmitContext) {
    let sn = ctx.stage_name;
    ctx.add_fmt(format!(
        "AND.U RC.z,{}.threadid,~3;\n\
         AND.U RC.y,RC.y,3;\n\
         OR.U RC.z,RC.z,RC.y;\n\
         SHFIDX.U RC,RC.x,RC.z,0x1C03;\n\
         MOV.U RC.x,RC.y;",
        sn
    ));
}

/// Port of `EmitQuadSwap` (GLASM): value in RC.x, direction in RC.y.
pub fn emit_quad_swap(ctx: &mut EmitContext) {
    ctx.add_line("ADD.U RC.y,RC.y,1;\nSHFXOR.U RC,RC.x,RC.y,0x1C03;\nMOV.U RC.x,RC.y;");
}

pub fn emit_f_swizzle_add(ctx: &mut EmitContext) {
    let sn = ctx.stage_name;
    ctx.add_fmt(format!(
        "AND.U RC.z,{}.threadid,3;\n\
         SHL.U RC.z,RC.z,1;\n\
         SHR.U RC.z,RC.w,RC.z;\n\
         AND.U RC.z,RC.z,3;\n\
         MUL.F RC.x,RC.x,FSWZA[RC.z];\n\
         MUL.F RC.y,RC.y,FSWZB[RC.z];\n\
         ADD.F RC.x,RC.x,RC.y;",
        sn
    ));
}

pub fn emit_dpdx_fine(ctx: &mut EmitContext) {
    ctx.add_line("DDX.FINE RC.x,RC.x;");
}

pub fn emit_dpdy_fine(ctx: &mut EmitContext) {
    ctx.add_line("DDY.FINE RC.x,RC.x;");
}

pub fn emit_dpdx_coarse(ctx: &mut EmitContext) {
    ctx.add_line("DDX.COARSE RC.x,RC.x;");
}

pub fn emit_dpdy_coarse(ctx: &mut EmitContext) {
    ctx.add_line("DDY.COARSE RC.x,RC.x;");
}
