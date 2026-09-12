//! ARM64 packed-integer emission.
//!
//! Upstream owner: `backend/arm64/emit_arm64_packed.cpp`.

use rhazel::{CodeGenerator, D2, V0, V1, V2};

use crate::backend::arm64::emit_context::EmitContext;
use crate::backend::arm64::reg_alloc::{RAReg, RegAlloc};
use crate::ir::opcode::Opcode;
use crate::ir::value::InstRef;

fn emit_packed_op(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    emit: impl FnOnce(&mut CodeGenerator<'_>, &RAReg, &RAReg, &RAReg) -> Result<(), String>,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);

    let mut result = ctx.reg_alloc.write_d(inst_ref);
    let mut a = ctx.reg_alloc.read_d(args[0]);
    let mut b = ctx.reg_alloc.read_d(args[1]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut a, &mut b])?;

    emit(code, &result, &a, &b)
}

fn emit_saturated_packed_op(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    emit: impl FnOnce(&mut CodeGenerator<'_>, &RAReg, &RAReg, &RAReg) -> Result<(), String>,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);

    let mut result = ctx.reg_alloc.write_d(inst_ref);
    let mut a = ctx.reg_alloc.read_d(args[0]);
    let mut b = ctx.reg_alloc.read_d(args[1]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut a, &mut b])?;
    ctx.fpsr.spill(code)?;

    emit(code, &result, &a, &b)
}

pub fn emit_packed_add_u8(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let ge_inst = ctx
        .block
        .get_associated_pseudo_operation(inst_ref, Opcode::GetGEFromOp);
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_d(inst_ref);
    let mut a = ctx.reg_alloc.read_d(args[0]);
    let mut b = ctx.reg_alloc.read_d(args[1]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut a, &mut b])?;
    let (result, a, b) = (result.v(), a.v(), b.v());

    code.add_v(result.b8(), a.b8(), b.b8())?;

    if let Some(ge_inst) = ge_inst {
        let mut ge = ctx.reg_alloc.write_d(ge_inst);
        ge.realize(code, ctx.block)?;
        let ge = ge.v();
        code.cmhi(ge.b8(), a.b8(), result.b8())?;
    }
    Ok(())
}

pub fn emit_packed_add_s8(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let ge_inst = ctx
        .block
        .get_associated_pseudo_operation(inst_ref, Opcode::GetGEFromOp);
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_d(inst_ref);
    let mut a = ctx.reg_alloc.read_d(args[0]);
    let mut b = ctx.reg_alloc.read_d(args[1]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut a, &mut b])?;
    let (result, a, b) = (result.v(), a.v(), b.v());

    code.add_v(result.b8(), a.b8(), b.b8())?;

    if let Some(ge_inst) = ge_inst {
        let mut ge = ctx.reg_alloc.write_d(ge_inst);
        ge.realize(code, ctx.block)?;
        let ge = ge.v();
        code.shadd(ge.b8(), a.b8(), b.b8())?;
        code.cmge_zero(ge.b8(), ge.b8())?;
    }
    Ok(())
}

pub fn emit_packed_sub_u8(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let ge_inst = ctx
        .block
        .get_associated_pseudo_operation(inst_ref, Opcode::GetGEFromOp);
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_d(inst_ref);
    let mut a = ctx.reg_alloc.read_d(args[0]);
    let mut b = ctx.reg_alloc.read_d(args[1]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut a, &mut b])?;
    let (result, a, b) = (result.v(), a.v(), b.v());

    code.sub_v(result.b8(), a.b8(), b.b8())?;

    if let Some(ge_inst) = ge_inst {
        let mut ge = ctx.reg_alloc.write_d(ge_inst);
        ge.realize(code, ctx.block)?;
        let ge = ge.v();
        code.uhsub(ge.b8(), a.b8(), b.b8())?;
        code.cmge_zero(ge.b8(), ge.b8())?;
    }
    Ok(())
}

pub fn emit_packed_sub_s8(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let ge_inst = ctx
        .block
        .get_associated_pseudo_operation(inst_ref, Opcode::GetGEFromOp);
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_d(inst_ref);
    let mut a = ctx.reg_alloc.read_d(args[0]);
    let mut b = ctx.reg_alloc.read_d(args[1]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut a, &mut b])?;
    let (result, a, b) = (result.v(), a.v(), b.v());

    code.sub_v(result.b8(), a.b8(), b.b8())?;

    if let Some(ge_inst) = ge_inst {
        let mut ge = ctx.reg_alloc.write_d(ge_inst);
        ge.realize(code, ctx.block)?;
        let ge = ge.v();
        code.shsub(ge.b8(), a.b8(), b.b8())?;
        code.cmge_zero(ge.b8(), ge.b8())?;
    }
    Ok(())
}

pub fn emit_packed_add_u16(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let ge_inst = ctx
        .block
        .get_associated_pseudo_operation(inst_ref, Opcode::GetGEFromOp);
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_d(inst_ref);
    let mut a = ctx.reg_alloc.read_d(args[0]);
    let mut b = ctx.reg_alloc.read_d(args[1]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut a, &mut b])?;
    let (result, a, b) = (result.v(), a.v(), b.v());

    code.add_v(result.h4(), a.h4(), b.h4())?;

    if let Some(ge_inst) = ge_inst {
        let mut ge = ctx.reg_alloc.write_d(ge_inst);
        ge.realize(code, ctx.block)?;
        let ge = ge.v();
        code.cmhi(ge.h4(), a.h4(), result.h4())?;
    }
    Ok(())
}

pub fn emit_packed_add_s16(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let ge_inst = ctx
        .block
        .get_associated_pseudo_operation(inst_ref, Opcode::GetGEFromOp);
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_d(inst_ref);
    let mut a = ctx.reg_alloc.read_d(args[0]);
    let mut b = ctx.reg_alloc.read_d(args[1]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut a, &mut b])?;
    let (result, a, b) = (result.v(), a.v(), b.v());

    code.add_v(result.h4(), a.h4(), b.h4())?;

    if let Some(ge_inst) = ge_inst {
        let mut ge = ctx.reg_alloc.write_d(ge_inst);
        ge.realize(code, ctx.block)?;
        let ge = ge.v();
        code.shadd(ge.h4(), a.h4(), b.h4())?;
        code.cmge_zero(ge.h4(), ge.h4())?;
    }
    Ok(())
}

pub fn emit_packed_sub_u16(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let ge_inst = ctx
        .block
        .get_associated_pseudo_operation(inst_ref, Opcode::GetGEFromOp);
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_d(inst_ref);
    let mut a = ctx.reg_alloc.read_d(args[0]);
    let mut b = ctx.reg_alloc.read_d(args[1]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut a, &mut b])?;
    let (result, a, b) = (result.v(), a.v(), b.v());

    code.sub_v(result.h4(), a.h4(), b.h4())?;

    if let Some(ge_inst) = ge_inst {
        let mut ge = ctx.reg_alloc.write_d(ge_inst);
        ge.realize(code, ctx.block)?;
        let ge = ge.v();
        code.uhsub(ge.h4(), a.h4(), b.h4())?;
        code.cmge_zero(ge.h4(), ge.h4())?;
    }
    Ok(())
}

pub fn emit_packed_sub_s16(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let ge_inst = ctx
        .block
        .get_associated_pseudo_operation(inst_ref, Opcode::GetGEFromOp);
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_d(inst_ref);
    let mut a = ctx.reg_alloc.read_d(args[0]);
    let mut b = ctx.reg_alloc.read_d(args[1]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut a, &mut b])?;
    let (result, a, b) = (result.v(), a.v(), b.v());

    code.sub_v(result.h4(), a.h4(), b.h4())?;

    if let Some(ge_inst) = ge_inst {
        let mut ge = ctx.reg_alloc.write_d(ge_inst);
        ge.realize(code, ctx.block)?;
        let ge = ge.v();
        code.shsub(ge.h4(), a.h4(), b.h4())?;
        code.cmge_zero(ge.h4(), ge.h4())?;
    }
    Ok(())
}

fn emit_packed_add_sub<const ADD_IS_HI: bool, const IS_SIGNED: bool, const IS_HALVING: bool>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let ge_inst = ctx
        .block
        .get_associated_pseudo_operation(inst_ref, Opcode::GetGEFromOp);
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_d(inst_ref);
    let mut a = ctx.reg_alloc.read_d(args[0]);
    let mut b = ctx.reg_alloc.read_d(args[1]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut a, &mut b])?;
    let (result, a, b) = (result.v(), a.v(), b.v());

    if IS_SIGNED {
        code.sxtl(V0.s4(), a.h4())?;
        code.sxtl(V1.s4(), b.h4())?;
    } else {
        code.uxtl(V0.s4(), a.h4())?;
        code.uxtl(V1.s4(), b.h4())?;
    }
    code.ext(V1.b8(), V1.b8(), V1.b8(), 4)?;

    code.movi_rep(D2, if ADD_IS_HI { 0b1111_0000 } else { 0b0000_1111 })?;

    code.eor_v(V1.b8(), V1.b8(), V2.b8())?;
    code.sub_v(V1.s2(), V1.s2(), V2.s2())?;
    code.sub_v(result.s2(), V0.s2(), V1.s2())?;

    if IS_HALVING {
        if IS_SIGNED {
            code.sshr(result.s2(), result.s2(), 1)?;
        } else {
            code.ushr(result.s2(), result.s2(), 1)?;
        }
    }

    if let Some(ge_inst) = ge_inst {
        assert!(!IS_HALVING);
        let mut ge = ctx.reg_alloc.write_d(ge_inst);
        ge.realize(code, ctx.block)?;
        let ge = ge.v();

        if IS_SIGNED {
            code.cmge_zero(ge.s2(), result.s2())?;
            code.xtn(ge.h4(), ge.s4())?;
        } else {
            code.cmeq_zero(ge.h4(), result.h4())?;
            code.eor_v(ge.b8(), ge.b8(), V2.b8())?;
            code.shrn(ge.h4(), ge.s4(), 16)?;
        }
    }

    code.xtn(result.h4(), result.s4())?;
    Ok(())
}

pub fn emit_packed_add_sub_u16(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_add_sub::<true, false, false>(code, ctx, inst_ref)
}

pub fn emit_packed_add_sub_s16(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_add_sub::<true, true, false>(code, ctx, inst_ref)
}

pub fn emit_packed_sub_add_u16(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_add_sub::<false, false, false>(code, ctx, inst_ref)
}

pub fn emit_packed_sub_add_s16(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_add_sub::<false, true, false>(code, ctx, inst_ref)
}

pub fn emit_packed_halving_add_u8(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_op(code, ctx, inst_ref, |code, result, a, b| {
        code.uhadd(result.v().b8(), a.v().b8(), b.v().b8())
    })
}

pub fn emit_packed_halving_add_s8(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_op(code, ctx, inst_ref, |code, result, a, b| {
        code.shadd(result.v().b8(), a.v().b8(), b.v().b8())
    })
}

pub fn emit_packed_halving_sub_u8(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_op(code, ctx, inst_ref, |code, result, a, b| {
        code.uhsub(result.v().b8(), a.v().b8(), b.v().b8())
    })
}

pub fn emit_packed_halving_sub_s8(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_op(code, ctx, inst_ref, |code, result, a, b| {
        code.shsub(result.v().b8(), a.v().b8(), b.v().b8())
    })
}

pub fn emit_packed_halving_add_u16(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_op(code, ctx, inst_ref, |code, result, a, b| {
        code.uhadd(result.v().h4(), a.v().h4(), b.v().h4())
    })
}

pub fn emit_packed_halving_add_s16(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_op(code, ctx, inst_ref, |code, result, a, b| {
        code.shadd(result.v().h4(), a.v().h4(), b.v().h4())
    })
}

pub fn emit_packed_halving_sub_u16(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_op(code, ctx, inst_ref, |code, result, a, b| {
        code.uhsub(result.v().h4(), a.v().h4(), b.v().h4())
    })
}

pub fn emit_packed_halving_sub_s16(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_op(code, ctx, inst_ref, |code, result, a, b| {
        code.shsub(result.v().h4(), a.v().h4(), b.v().h4())
    })
}

pub fn emit_packed_halving_add_sub_u16(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_add_sub::<true, false, true>(code, ctx, inst_ref)
}

pub fn emit_packed_halving_add_sub_s16(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_add_sub::<true, true, true>(code, ctx, inst_ref)
}

pub fn emit_packed_halving_sub_add_u16(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_add_sub::<false, false, true>(code, ctx, inst_ref)
}

pub fn emit_packed_halving_sub_add_s16(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_add_sub::<false, true, true>(code, ctx, inst_ref)
}

pub fn emit_packed_saturated_add_u8(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_saturated_packed_op(code, ctx, inst_ref, |code, result, a, b| {
        code.uqadd(result.v().b8(), a.v().b8(), b.v().b8())
    })
}

pub fn emit_packed_saturated_add_s8(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_saturated_packed_op(code, ctx, inst_ref, |code, result, a, b| {
        code.sqadd(result.v().b8(), a.v().b8(), b.v().b8())
    })
}

pub fn emit_packed_saturated_sub_u8(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_saturated_packed_op(code, ctx, inst_ref, |code, result, a, b| {
        code.uqsub(result.v().b8(), a.v().b8(), b.v().b8())
    })
}

pub fn emit_packed_saturated_sub_s8(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_saturated_packed_op(code, ctx, inst_ref, |code, result, a, b| {
        code.sqsub(result.v().b8(), a.v().b8(), b.v().b8())
    })
}

pub fn emit_packed_saturated_add_u16(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_saturated_packed_op(code, ctx, inst_ref, |code, result, a, b| {
        code.uqadd(result.v().h4(), a.v().h4(), b.v().h4())
    })
}

pub fn emit_packed_saturated_add_s16(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_saturated_packed_op(code, ctx, inst_ref, |code, result, a, b| {
        code.sqadd(result.v().h4(), a.v().h4(), b.v().h4())
    })
}

pub fn emit_packed_saturated_sub_u16(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_saturated_packed_op(code, ctx, inst_ref, |code, result, a, b| {
        code.uqsub(result.v().h4(), a.v().h4(), b.v().h4())
    })
}

pub fn emit_packed_saturated_sub_s16(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_saturated_packed_op(code, ctx, inst_ref, |code, result, a, b| {
        code.sqsub(result.v().h4(), a.v().h4(), b.v().h4())
    })
}

pub fn emit_packed_abs_diff_sum_u8(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_op(code, ctx, inst_ref, |code, result, a, b| {
        let (result, a, b) = (result.v(), a.v(), b.v());
        code.movi_rep(D2, 0b0000_1111)?;
        code.uabd(result.b8(), a.b8(), b.b8())?;
        code.and_v(result.b8(), result.b8(), V2.b8())?;
        code.uaddlv(result.h(), result.b8())?;
        Ok(())
    })
}

pub fn emit_packed_select(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);

    let mut result = ctx.reg_alloc.write_d(inst_ref);
    let mut ge = ctx.reg_alloc.read_d(args[0]);
    let mut a = ctx.reg_alloc.read_d(args[1]);
    let mut b = ctx.reg_alloc.read_d(args[2]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut ge, &mut a, &mut b])?;
    let (result, ge, a, b) = (result.v(), ge.v(), a.v(), b.v());

    code.fmov(result.d(), ge.d())?;
    code.bsl(result.b8(), b.b8(), a.b8())?;
    Ok(())
}

pub fn emit_packed_instruction(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    match ctx.block.get(inst_ref).opcode {
        Opcode::PackedAddU8 => emit_packed_add_u8(code, ctx, inst_ref),
        Opcode::PackedAddS8 => emit_packed_add_s8(code, ctx, inst_ref),
        Opcode::PackedSubU8 => emit_packed_sub_u8(code, ctx, inst_ref),
        Opcode::PackedSubS8 => emit_packed_sub_s8(code, ctx, inst_ref),
        Opcode::PackedAddU16 => emit_packed_add_u16(code, ctx, inst_ref),
        Opcode::PackedAddS16 => emit_packed_add_s16(code, ctx, inst_ref),
        Opcode::PackedSubU16 => emit_packed_sub_u16(code, ctx, inst_ref),
        Opcode::PackedSubS16 => emit_packed_sub_s16(code, ctx, inst_ref),
        Opcode::PackedAddSubU16 => emit_packed_add_sub_u16(code, ctx, inst_ref),
        Opcode::PackedAddSubS16 => emit_packed_add_sub_s16(code, ctx, inst_ref),
        Opcode::PackedSubAddU16 => emit_packed_sub_add_u16(code, ctx, inst_ref),
        Opcode::PackedSubAddS16 => emit_packed_sub_add_s16(code, ctx, inst_ref),
        Opcode::PackedHalvingAddU8 => emit_packed_halving_add_u8(code, ctx, inst_ref),
        Opcode::PackedHalvingAddS8 => emit_packed_halving_add_s8(code, ctx, inst_ref),
        Opcode::PackedHalvingSubU8 => emit_packed_halving_sub_u8(code, ctx, inst_ref),
        Opcode::PackedHalvingSubS8 => emit_packed_halving_sub_s8(code, ctx, inst_ref),
        Opcode::PackedHalvingAddU16 => emit_packed_halving_add_u16(code, ctx, inst_ref),
        Opcode::PackedHalvingAddS16 => emit_packed_halving_add_s16(code, ctx, inst_ref),
        Opcode::PackedHalvingSubU16 => emit_packed_halving_sub_u16(code, ctx, inst_ref),
        Opcode::PackedHalvingSubS16 => emit_packed_halving_sub_s16(code, ctx, inst_ref),
        Opcode::PackedHalvingAddSubU16 => emit_packed_halving_add_sub_u16(code, ctx, inst_ref),
        Opcode::PackedHalvingAddSubS16 => emit_packed_halving_add_sub_s16(code, ctx, inst_ref),
        Opcode::PackedHalvingSubAddU16 => emit_packed_halving_sub_add_u16(code, ctx, inst_ref),
        Opcode::PackedHalvingSubAddS16 => emit_packed_halving_sub_add_s16(code, ctx, inst_ref),
        Opcode::PackedSaturatedAddU8 => emit_packed_saturated_add_u8(code, ctx, inst_ref),
        Opcode::PackedSaturatedAddS8 => emit_packed_saturated_add_s8(code, ctx, inst_ref),
        Opcode::PackedSaturatedSubU8 => emit_packed_saturated_sub_u8(code, ctx, inst_ref),
        Opcode::PackedSaturatedSubS8 => emit_packed_saturated_sub_s8(code, ctx, inst_ref),
        Opcode::PackedSaturatedAddU16 => emit_packed_saturated_add_u16(code, ctx, inst_ref),
        Opcode::PackedSaturatedAddS16 => emit_packed_saturated_add_s16(code, ctx, inst_ref),
        Opcode::PackedSaturatedSubU16 => emit_packed_saturated_sub_u16(code, ctx, inst_ref),
        Opcode::PackedSaturatedSubS16 => emit_packed_saturated_sub_s16(code, ctx, inst_ref),
        Opcode::PackedAbsDiffSumU8 => emit_packed_abs_diff_sum_u8(code, ctx, inst_ref),
        Opcode::PackedSelect => emit_packed_select(code, ctx, inst_ref),
        opcode => Err(format!("unimplemented ARM64 packed opcode: {opcode:?}")),
    }
}
