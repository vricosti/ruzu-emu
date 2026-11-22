// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! GLASM floating-point operation emission.
//!
//! Maps to upstream `backend/glasm/emit_glasm_floating_point.cpp`.

use super::glasm_emit_context::EmitContext;

pub fn emit_fp_abs32(ctx: &mut EmitContext) {
    ctx.add_line("MOV.F RC.x,|RC.x|;");
}
pub fn emit_fp_abs64(ctx: &mut EmitContext) {
    ctx.add_line("MOV.F64 DC.x,|DC.x|;");
}
pub fn emit_fp_add32(ctx: &mut EmitContext) {
    ctx.add_line("ADD.F RC.x,RC.x,RC.y;");
}
pub fn emit_fp_add64(ctx: &mut EmitContext) {
    ctx.add_line("ADD.F64 DC.x,DC.x,DC.y;");
}
pub fn emit_fp_fma32(ctx: &mut EmitContext) {
    ctx.add_line("MAD.F RC.x,RC.x,RC.y,RC.z;");
}
pub fn emit_fp_fma64(ctx: &mut EmitContext) {
    ctx.add_line("MAD.F64 DC.x,DC.x,DC.y,DC.z;");
}
pub fn emit_fp_max32(ctx: &mut EmitContext) {
    ctx.add_line("MAX.F RC.x,RC.x,RC.y;");
}
pub fn emit_fp_max64(ctx: &mut EmitContext) {
    ctx.add_line("MAX.F64 DC.x,DC.x,DC.y;");
}
pub fn emit_fp_min32(ctx: &mut EmitContext) {
    ctx.add_line("MIN.F RC.x,RC.x,RC.y;");
}
pub fn emit_fp_min64(ctx: &mut EmitContext) {
    ctx.add_line("MIN.F64 DC.x,DC.x,DC.y;");
}
pub fn emit_fp_mul32(ctx: &mut EmitContext) {
    ctx.add_line("MUL.F RC.x,RC.x,RC.y;");
}
pub fn emit_fp_mul64(ctx: &mut EmitContext) {
    ctx.add_line("MUL.F64 DC.x,DC.x,DC.y;");
}
pub fn emit_fp_neg32(ctx: &mut EmitContext) {
    ctx.add_line("MOV.F RC.x,-RC.x;");
}
pub fn emit_fp_neg64(ctx: &mut EmitContext) {
    ctx.add_line("MOV.F64 DC.x,-DC.x;");
}
pub fn emit_fp_sin(ctx: &mut EmitContext) {
    ctx.add_line("SIN RC.x,RC.x;");
}
pub fn emit_fp_cos(ctx: &mut EmitContext) {
    ctx.add_line("COS RC.x,RC.x;");
}
pub fn emit_fp_exp2(ctx: &mut EmitContext) {
    ctx.add_line("EX2 RC.x,RC.x;");
}
/// Upstream `EmitFPTanh` (Lichii emit_glasm_floating_point.cpp, eaeb09b098): a series below
/// 2^-4, (1 - e^-2x) / (1 + e^-2x) above, the input itself below 2^-12, the sign put back.
/// Upstream reads its operand from its own register; this backend has the operand and the
/// result in RC.x, so the same steps keep RC.x until the end and use RC.y-RC.w as scratch.
pub fn emit_fp_tanh(ctx: &mut EmitContext) {
    ctx.add_line("AND.U RC.y,RC.x,0x7fffffff;");
    ctx.add_line("MUL.F RC.w,RC.y,RC.y;");
    ctx.add_line("MUL.F RC.z,RC.w,-0.05396825397;");
    ctx.add_line("ADD.F RC.z,RC.z,0.1333333333;");
    ctx.add_line("MAD.F RC.z,RC.z,RC.w,-0.3333333333;");
    ctx.add_line("MUL.F RC.w,RC.w,RC.y;");
    ctx.add_line("MAD.F RC.y,RC.w,RC.z,RC.y;");
    ctx.add_line("AND.U RC.z,RC.x,0x7fffffff;");
    ctx.add_line("MUL.F RC.z,RC.z,-2.885390082;");
    ctx.add_line("EX2 RC.z,RC.z;");
    ctx.add_line("ADD.F RC.w,RC.z,1.0;");
    ctx.add_line("RCP RC.w,RC.w;");
    ctx.add_line("SUB.F RC.z,1.0,RC.z;");
    ctx.add_line("MUL.F RC.z,RC.z,RC.w;");
    ctx.add_line("AND.U RC.w,RC.x,0x7fffffff;");
    ctx.add_line("SLT.U RC.w,RC.w,0x3d800000;");
    ctx.add_line("CMP.S RC.y,RC.w,RC.y,RC.z;");
    ctx.add_line("AND.U RC.z,RC.x,0x80000000;");
    ctx.add_line("OR.U RC.y,RC.y,RC.z;");
    ctx.add_line("AND.U RC.w,RC.x,0x7fffffff;");
    ctx.add_line("SLT.U RC.w,RC.w,0x39800000;");
    ctx.add_line("CMP.S RC.x,RC.w,RC.x,RC.y;");
}
pub fn emit_fp_log2(ctx: &mut EmitContext) {
    ctx.add_line("LG2 RC.x,RC.x;");
}
pub fn emit_fp_recip32(ctx: &mut EmitContext) {
    ctx.add_line("RCP RC.x,RC.x;");
}
pub fn emit_fp_recip_sqrt32(ctx: &mut EmitContext) {
    ctx.add_line("RSQ RC.x,RC.x;");
}
pub fn emit_fp_sqrt(ctx: &mut EmitContext) {
    ctx.add_line("RSQ RC.x,RC.x;");
    ctx.add_line("RCP RC.x,RC.x;");
}
pub fn emit_fp_saturate32(ctx: &mut EmitContext) {
    ctx.add_line("MOV.F.SAT RC.x,RC.x;");
}
pub fn emit_fp_clamp32(ctx: &mut EmitContext) {
    ctx.add_line("MAX.F RC.x,RC.y,RC.x;");
    ctx.add_line("MIN.F RC.x,RC.x,RC.z;");
}
pub fn emit_fp_clamp64(ctx: &mut EmitContext) {
    ctx.add_line("MAX.F64 DC.x,DC.y,DC.x;");
    ctx.add_line("MIN.F64 DC.x,DC.x,DC.z;");
}
pub fn emit_fp_round_even32(ctx: &mut EmitContext) {
    ctx.add_line("ROUND.F RC.x,RC.x;");
}
pub fn emit_fp_round_even64(ctx: &mut EmitContext) {
    ctx.add_line("ROUND.F64 DC.x,DC.x;");
}
pub fn emit_fp_floor32(ctx: &mut EmitContext) {
    ctx.add_line("FLR.F RC.x,RC.x;");
}
pub fn emit_fp_floor64(ctx: &mut EmitContext) {
    ctx.add_line("FLR.F64 DC.x,DC.x;");
}
pub fn emit_fp_ceil32(ctx: &mut EmitContext) {
    ctx.add_line("CEIL.F RC.x,RC.x;");
}
pub fn emit_fp_ceil64(ctx: &mut EmitContext) {
    ctx.add_line("CEIL.F64 DC.x,DC.x;");
}
pub fn emit_fp_trunc32(ctx: &mut EmitContext) {
    ctx.add_line("TRUNC.F RC.x,RC.x;");
}
pub fn emit_fp_trunc64(ctx: &mut EmitContext) {
    ctx.add_line("TRUNC.F64 DC.x,DC.x;");
}

// Comparisons
pub fn emit_fp_ord_equal32(ctx: &mut EmitContext) {
    ctx.add_line("SEQ.F RC.x,RC.x,RC.y;");
}
pub fn emit_fp_ord_equal64(ctx: &mut EmitContext) {
    ctx.add_line("SEQ.F64 RC.x,DC.x,DC.y;");
}
pub fn emit_fp_unord_equal32(ctx: &mut EmitContext) {
    ctx.add_line("SEQ.F RC.x,RC.x,RC.y;");
}
pub fn emit_fp_unord_equal64(ctx: &mut EmitContext) {
    ctx.add_line("SEQ.F64 RC.x,DC.x,DC.y;");
}
pub fn emit_fp_ord_not_equal32(ctx: &mut EmitContext) {
    ctx.add_line("SNE.F RC.x,RC.x,RC.y;");
}
pub fn emit_fp_ord_not_equal64(ctx: &mut EmitContext) {
    ctx.add_line("SNE.F64 RC.x,DC.x,DC.y;");
}
pub fn emit_fp_unord_not_equal32(ctx: &mut EmitContext) {
    ctx.add_line("SNE.F RC.x,RC.x,RC.y;");
}
pub fn emit_fp_unord_not_equal64(ctx: &mut EmitContext) {
    ctx.add_line("SNE.F64 RC.x,DC.x,DC.y;");
}
pub fn emit_fp_ord_less_than32(ctx: &mut EmitContext) {
    ctx.add_line("SLT.F RC.x,RC.x,RC.y;");
}
pub fn emit_fp_ord_less_than64(ctx: &mut EmitContext) {
    ctx.add_line("SLT.F64 RC.x,DC.x,DC.y;");
}
pub fn emit_fp_unord_less_than32(ctx: &mut EmitContext) {
    ctx.add_line("SLT.F RC.x,RC.x,RC.y;");
}
pub fn emit_fp_unord_less_than64(ctx: &mut EmitContext) {
    ctx.add_line("SLT.F64 RC.x,DC.x,DC.y;");
}
pub fn emit_fp_ord_greater_than32(ctx: &mut EmitContext) {
    ctx.add_line("SGT.F RC.x,RC.x,RC.y;");
}
pub fn emit_fp_ord_greater_than64(ctx: &mut EmitContext) {
    ctx.add_line("SGT.F64 RC.x,DC.x,DC.y;");
}
pub fn emit_fp_unord_greater_than32(ctx: &mut EmitContext) {
    ctx.add_line("SGT.F RC.x,RC.x,RC.y;");
}
pub fn emit_fp_unord_greater_than64(ctx: &mut EmitContext) {
    ctx.add_line("SGT.F64 RC.x,DC.x,DC.y;");
}
pub fn emit_fp_ord_less_than_equal32(ctx: &mut EmitContext) {
    ctx.add_line("SLE.F RC.x,RC.x,RC.y;");
}
pub fn emit_fp_ord_less_than_equal64(ctx: &mut EmitContext) {
    ctx.add_line("SLE.F64 RC.x,DC.x,DC.y;");
}
pub fn emit_fp_unord_less_than_equal32(ctx: &mut EmitContext) {
    ctx.add_line("SLE.F RC.x,RC.x,RC.y;");
}
pub fn emit_fp_unord_less_than_equal64(ctx: &mut EmitContext) {
    ctx.add_line("SLE.F64 RC.x,DC.x,DC.y;");
}
pub fn emit_fp_ord_greater_than_equal32(ctx: &mut EmitContext) {
    ctx.add_line("SGE.F RC.x,RC.x,RC.y;");
}
pub fn emit_fp_ord_greater_than_equal64(ctx: &mut EmitContext) {
    ctx.add_line("SGE.F64 RC.x,DC.x,DC.y;");
}
pub fn emit_fp_unord_greater_than_equal32(ctx: &mut EmitContext) {
    ctx.add_line("SGE.F RC.x,RC.x,RC.y;");
}
pub fn emit_fp_unord_greater_than_equal64(ctx: &mut EmitContext) {
    ctx.add_line("SGE.F64 RC.x,DC.x,DC.y;");
}
pub fn emit_fp_is_nan32(ctx: &mut EmitContext) {
    ctx.add_line("SNE.F RC.x,RC.x,RC.x;");
}
pub fn emit_fp_is_nan64(ctx: &mut EmitContext) {
    ctx.add_line("SNE.F64 RC.x,DC.x,DC.x;");
}
