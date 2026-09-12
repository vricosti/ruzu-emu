//! ARM64 vector saturation emission.
//!
//! Upstream owner: `backend/arm64/emit_arm64_vector_saturation.cpp`.

use rhazel::CodeGenerator;

use crate::backend::arm64::emit_context::EmitContext;
use crate::backend::arm64::reg_alloc::{RAReg, RegAlloc};
use crate::ir::opcode::Opcode;
use crate::ir::value::InstRef;

/// Upstream `EmitSaturatedArithmetic`: realize result and operands, load
/// FPSR, then let the opcode-specific closure pick the arrangement.
fn emit(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    emit: fn(&mut CodeGenerator<'_>, &RAReg, &RAReg, &RAReg) -> Result<(), String>,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut a = ctx.reg_alloc.read_q(args[0]);
    let mut b = ctx.reg_alloc.read_q(args[1]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut a, &mut b])?;
    ctx.fpsr.load(code)?;
    emit(code, &result, &a, &b)
}

pub fn emit_vector_saturation_instruction(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    match ctx.block.get(inst_ref).opcode {
        Opcode::VectorSignedSaturatedAdd8 => emit(code, ctx, inst_ref, |code, r, a, b| {
            code.sqadd(r.v().b16(), a.v().b16(), b.v().b16())
        }),
        Opcode::VectorSignedSaturatedAdd16 => emit(code, ctx, inst_ref, |code, r, a, b| {
            code.sqadd(r.v().h8(), a.v().h8(), b.v().h8())
        }),
        Opcode::VectorSignedSaturatedAdd32 => emit(code, ctx, inst_ref, |code, r, a, b| {
            code.sqadd(r.v().s4(), a.v().s4(), b.v().s4())
        }),
        Opcode::VectorSignedSaturatedAdd64 => emit(code, ctx, inst_ref, |code, r, a, b| {
            code.sqadd(r.v().d2(), a.v().d2(), b.v().d2())
        }),
        Opcode::VectorSignedSaturatedSub8 => emit(code, ctx, inst_ref, |code, r, a, b| {
            code.sqsub(r.v().b16(), a.v().b16(), b.v().b16())
        }),
        Opcode::VectorSignedSaturatedSub16 => emit(code, ctx, inst_ref, |code, r, a, b| {
            code.sqsub(r.v().h8(), a.v().h8(), b.v().h8())
        }),
        Opcode::VectorSignedSaturatedSub32 => emit(code, ctx, inst_ref, |code, r, a, b| {
            code.sqsub(r.v().s4(), a.v().s4(), b.v().s4())
        }),
        Opcode::VectorSignedSaturatedSub64 => emit(code, ctx, inst_ref, |code, r, a, b| {
            code.sqsub(r.v().d2(), a.v().d2(), b.v().d2())
        }),
        Opcode::VectorUnsignedSaturatedAdd8 => emit(code, ctx, inst_ref, |code, r, a, b| {
            code.uqadd(r.v().b16(), a.v().b16(), b.v().b16())
        }),
        Opcode::VectorUnsignedSaturatedAdd16 => emit(code, ctx, inst_ref, |code, r, a, b| {
            code.uqadd(r.v().h8(), a.v().h8(), b.v().h8())
        }),
        Opcode::VectorUnsignedSaturatedAdd32 => emit(code, ctx, inst_ref, |code, r, a, b| {
            code.uqadd(r.v().s4(), a.v().s4(), b.v().s4())
        }),
        Opcode::VectorUnsignedSaturatedAdd64 => emit(code, ctx, inst_ref, |code, r, a, b| {
            code.uqadd(r.v().d2(), a.v().d2(), b.v().d2())
        }),
        Opcode::VectorUnsignedSaturatedSub8 => emit(code, ctx, inst_ref, |code, r, a, b| {
            code.uqsub(r.v().b16(), a.v().b16(), b.v().b16())
        }),
        Opcode::VectorUnsignedSaturatedSub16 => emit(code, ctx, inst_ref, |code, r, a, b| {
            code.uqsub(r.v().h8(), a.v().h8(), b.v().h8())
        }),
        Opcode::VectorUnsignedSaturatedSub32 => emit(code, ctx, inst_ref, |code, r, a, b| {
            code.uqsub(r.v().s4(), a.v().s4(), b.v().s4())
        }),
        Opcode::VectorUnsignedSaturatedSub64 => emit(code, ctx, inst_ref, |code, r, a, b| {
            code.uqsub(r.v().d2(), a.v().d2(), b.v().d2())
        }),
        opcode => Err(format!(
            "unimplemented ARM64 vector saturation opcode: {opcode:?}"
        )),
    }
}
