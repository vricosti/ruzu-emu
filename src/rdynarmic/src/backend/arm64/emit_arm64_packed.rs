//! ARM64 packed-integer emission.
//!
//! Upstream owner: `backend/arm64/emit_arm64_packed.cpp`.

use crate::backend::arm64::block_of_code::BlockOfCode;
use crate::backend::arm64::emit_context::EmitContext;
use crate::backend::arm64::inst;
use crate::backend::arm64::reg_alloc::RegAlloc;
use crate::ir::opcode::Opcode;
use crate::ir::value::InstRef;

const V0: u8 = 0;
const V1: u8 = 1;
const V2: u8 = 2;

fn emit_packed_op(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    emit: impl FnOnce(&mut BlockOfCode, u8, u8, u8) -> Result<(), String>,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);

    let mut result = ctx.reg_alloc.write_d(inst_ref);
    let mut a = ctx.reg_alloc.read_d(args[0]);
    let mut b = ctx.reg_alloc.read_d(args[1]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut a, &mut b])?;

    emit(
        code,
        result.index().expect("result realized") as u8,
        a.index().expect("a realized") as u8,
        b.index().expect("b realized") as u8,
    )
}

fn emit_saturated_packed_op(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    emit: impl FnOnce(u8, u8, u8) -> u32,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);

    let mut result = ctx.reg_alloc.write_d(inst_ref);
    let mut a = ctx.reg_alloc.read_d(args[0]);
    let mut b = ctx.reg_alloc.read_d(args[1]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut a, &mut b])?;
    ctx.fpsr.spill(code)?;

    code.write_u32(emit(
        result.index().expect("result realized") as u8,
        a.index().expect("a realized") as u8,
        b.index().expect("b realized") as u8,
    ))?;
    Ok(())
}

pub fn emit_packed_add_u8(
    code: &mut BlockOfCode,
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
    let result = result.index().expect("result realized") as u8;
    let a = a.index().expect("a realized") as u8;
    let b = b.index().expect("b realized") as u8;

    code.write_u32(inst::add_v(result, a, b, 8, false))?;

    if let Some(ge_inst) = ge_inst {
        let mut ge = ctx.reg_alloc.write_d(ge_inst);
        let ge = ge.realize(code, ctx.block)? as u8;
        code.write_u32(inst::cmhi_v(ge, a, result, 8, false))?;
    }
    Ok(())
}

pub fn emit_packed_add_s8(
    code: &mut BlockOfCode,
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
    let result = result.index().expect("result realized") as u8;
    let a = a.index().expect("a realized") as u8;
    let b = b.index().expect("b realized") as u8;

    code.write_u32(inst::add_v(result, a, b, 8, false))?;

    if let Some(ge_inst) = ge_inst {
        let mut ge = ctx.reg_alloc.write_d(ge_inst);
        let ge = ge.realize(code, ctx.block)? as u8;
        code.write_u32(inst::shadd_v(ge, a, b, 8, false))?;
        code.write_u32(inst::cmge_v_zero(ge, ge, 8, false))?;
    }
    Ok(())
}

pub fn emit_packed_sub_u8(
    code: &mut BlockOfCode,
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
    let result = result.index().expect("result realized") as u8;
    let a = a.index().expect("a realized") as u8;
    let b = b.index().expect("b realized") as u8;

    code.write_u32(inst::sub_v(result, a, b, 8, false))?;

    if let Some(ge_inst) = ge_inst {
        let mut ge = ctx.reg_alloc.write_d(ge_inst);
        let ge = ge.realize(code, ctx.block)? as u8;
        code.write_u32(inst::uhsub_v(ge, a, b, 8, false))?;
        code.write_u32(inst::cmge_v_zero(ge, ge, 8, false))?;
    }
    Ok(())
}

pub fn emit_packed_sub_s8(
    code: &mut BlockOfCode,
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
    let result = result.index().expect("result realized") as u8;
    let a = a.index().expect("a realized") as u8;
    let b = b.index().expect("b realized") as u8;

    code.write_u32(inst::sub_v(result, a, b, 8, false))?;

    if let Some(ge_inst) = ge_inst {
        let mut ge = ctx.reg_alloc.write_d(ge_inst);
        let ge = ge.realize(code, ctx.block)? as u8;
        code.write_u32(inst::shsub_v(ge, a, b, 8, false))?;
        code.write_u32(inst::cmge_v_zero(ge, ge, 8, false))?;
    }
    Ok(())
}

pub fn emit_packed_add_u16(
    code: &mut BlockOfCode,
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
    let result = result.index().expect("result realized") as u8;
    let a = a.index().expect("a realized") as u8;
    let b = b.index().expect("b realized") as u8;

    code.write_u32(inst::add_v(result, a, b, 16, false))?;

    if let Some(ge_inst) = ge_inst {
        let mut ge = ctx.reg_alloc.write_d(ge_inst);
        let ge = ge.realize(code, ctx.block)? as u8;
        code.write_u32(inst::cmhi_v(ge, a, result, 16, false))?;
    }
    Ok(())
}

pub fn emit_packed_add_s16(
    code: &mut BlockOfCode,
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
    let result = result.index().expect("result realized") as u8;
    let a = a.index().expect("a realized") as u8;
    let b = b.index().expect("b realized") as u8;

    code.write_u32(inst::add_v(result, a, b, 16, false))?;

    if let Some(ge_inst) = ge_inst {
        let mut ge = ctx.reg_alloc.write_d(ge_inst);
        let ge = ge.realize(code, ctx.block)? as u8;
        code.write_u32(inst::shadd_v(ge, a, b, 16, false))?;
        code.write_u32(inst::cmge_v_zero(ge, ge, 16, false))?;
    }
    Ok(())
}

pub fn emit_packed_sub_u16(
    code: &mut BlockOfCode,
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
    let result = result.index().expect("result realized") as u8;
    let a = a.index().expect("a realized") as u8;
    let b = b.index().expect("b realized") as u8;

    code.write_u32(inst::sub_v(result, a, b, 16, false))?;

    if let Some(ge_inst) = ge_inst {
        let mut ge = ctx.reg_alloc.write_d(ge_inst);
        let ge = ge.realize(code, ctx.block)? as u8;
        code.write_u32(inst::uhsub_v(ge, a, b, 16, false))?;
        code.write_u32(inst::cmge_v_zero(ge, ge, 16, false))?;
    }
    Ok(())
}

pub fn emit_packed_sub_s16(
    code: &mut BlockOfCode,
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
    let result = result.index().expect("result realized") as u8;
    let a = a.index().expect("a realized") as u8;
    let b = b.index().expect("b realized") as u8;

    code.write_u32(inst::sub_v(result, a, b, 16, false))?;

    if let Some(ge_inst) = ge_inst {
        let mut ge = ctx.reg_alloc.write_d(ge_inst);
        let ge = ge.realize(code, ctx.block)? as u8;
        code.write_u32(inst::shsub_v(ge, a, b, 16, false))?;
        code.write_u32(inst::cmge_v_zero(ge, ge, 16, false))?;
    }
    Ok(())
}

fn emit_packed_add_sub<const ADD_IS_HI: bool, const IS_SIGNED: bool, const IS_HALVING: bool>(
    code: &mut BlockOfCode,
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
    let result = result.index().expect("result realized") as u8;
    let a = a.index().expect("a realized") as u8;
    let b = b.index().expect("b realized") as u8;

    code.write_u32(if IS_SIGNED {
        inst::sxtl_v(V0, a, 16)
    } else {
        inst::uxtl_v(V0, a, 16)
    })?;
    code.write_u32(if IS_SIGNED {
        inst::sxtl_v(V1, b, 16)
    } else {
        inst::uxtl_v(V1, b, 16)
    })?;
    code.write_u32(inst::ext_v16b(V1, V1, V1, 4, false))?;

    code.write_u32(inst::movi_v8b_imm(
        V2,
        if ADD_IS_HI { 0b1111_0000 } else { 0b0000_1111 },
    ))?;

    code.write_u32(inst::eor_v8b(V1, V1, V2))?;
    code.write_u32(inst::sub_v(V1, V1, V2, 32, false))?;
    code.write_u32(inst::sub_v(result, V0, V1, 32, false))?;

    if IS_HALVING {
        code.write_u32(if IS_SIGNED {
            inst::sshr_v(result, result, 32, 1, false)
        } else {
            inst::ushr_v(result, result, 32, 1, false)
        })?;
    }

    if let Some(ge_inst) = ge_inst {
        assert!(!IS_HALVING);
        let mut ge = ctx.reg_alloc.write_d(ge_inst);
        let ge = ge.realize(code, ctx.block)? as u8;

        if IS_SIGNED {
            code.write_u32(inst::cmge_v_zero(ge, result, 32, false))?;
            code.write_u32(inst::xtn_v(ge, ge, 32))?;
        } else {
            code.write_u32(inst::cmeq_v_zero(ge, result, 16, false))?;
            code.write_u32(inst::eor_v8b(ge, ge, V2))?;
            code.write_u32(inst::shrn_v(ge, ge, 32, 16))?;
        }
    }

    code.write_u32(inst::xtn_v(result, result, 32))?;
    Ok(())
}

pub fn emit_packed_add_sub_u16(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_add_sub::<true, false, false>(code, ctx, inst_ref)
}

pub fn emit_packed_add_sub_s16(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_add_sub::<true, true, false>(code, ctx, inst_ref)
}

pub fn emit_packed_sub_add_u16(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_add_sub::<false, false, false>(code, ctx, inst_ref)
}

pub fn emit_packed_sub_add_s16(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_add_sub::<false, true, false>(code, ctx, inst_ref)
}

pub fn emit_packed_halving_add_u8(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_op(code, ctx, inst_ref, |code, result, a, b| {
        code.write_u32(inst::uhadd_v(result, a, b, 8, false))?;
        Ok(())
    })
}

pub fn emit_packed_halving_add_s8(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_op(code, ctx, inst_ref, |code, result, a, b| {
        code.write_u32(inst::shadd_v(result, a, b, 8, false))?;
        Ok(())
    })
}

pub fn emit_packed_halving_sub_u8(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_op(code, ctx, inst_ref, |code, result, a, b| {
        code.write_u32(inst::uhsub_v(result, a, b, 8, false))?;
        Ok(())
    })
}

pub fn emit_packed_halving_sub_s8(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_op(code, ctx, inst_ref, |code, result, a, b| {
        code.write_u32(inst::shsub_v(result, a, b, 8, false))?;
        Ok(())
    })
}

pub fn emit_packed_halving_add_u16(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_op(code, ctx, inst_ref, |code, result, a, b| {
        code.write_u32(inst::uhadd_v(result, a, b, 16, false))?;
        Ok(())
    })
}

pub fn emit_packed_halving_add_s16(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_op(code, ctx, inst_ref, |code, result, a, b| {
        code.write_u32(inst::shadd_v(result, a, b, 16, false))?;
        Ok(())
    })
}

pub fn emit_packed_halving_sub_u16(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_op(code, ctx, inst_ref, |code, result, a, b| {
        code.write_u32(inst::uhsub_v(result, a, b, 16, false))?;
        Ok(())
    })
}

pub fn emit_packed_halving_sub_s16(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_op(code, ctx, inst_ref, |code, result, a, b| {
        code.write_u32(inst::shsub_v(result, a, b, 16, false))?;
        Ok(())
    })
}

pub fn emit_packed_halving_add_sub_u16(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_add_sub::<true, false, true>(code, ctx, inst_ref)
}

pub fn emit_packed_halving_add_sub_s16(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_add_sub::<true, true, true>(code, ctx, inst_ref)
}

pub fn emit_packed_halving_sub_add_u16(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_add_sub::<false, false, true>(code, ctx, inst_ref)
}

pub fn emit_packed_halving_sub_add_s16(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_add_sub::<false, true, true>(code, ctx, inst_ref)
}

pub fn emit_packed_saturated_add_u8(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_saturated_packed_op(code, ctx, inst_ref, |result, a, b| {
        inst::uqadd_v(result, a, b, 8, false)
    })
}

pub fn emit_packed_saturated_add_s8(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_saturated_packed_op(code, ctx, inst_ref, |result, a, b| {
        inst::sqadd_v(result, a, b, 8, false)
    })
}

pub fn emit_packed_saturated_sub_u8(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_saturated_packed_op(code, ctx, inst_ref, |result, a, b| {
        inst::uqsub_v(result, a, b, 8, false)
    })
}

pub fn emit_packed_saturated_sub_s8(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_saturated_packed_op(code, ctx, inst_ref, |result, a, b| {
        inst::sqsub_v(result, a, b, 8, false)
    })
}

pub fn emit_packed_saturated_add_u16(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_saturated_packed_op(code, ctx, inst_ref, |result, a, b| {
        inst::uqadd_v(result, a, b, 16, false)
    })
}

pub fn emit_packed_saturated_add_s16(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_saturated_packed_op(code, ctx, inst_ref, |result, a, b| {
        inst::sqadd_v(result, a, b, 16, false)
    })
}

pub fn emit_packed_saturated_sub_u16(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_saturated_packed_op(code, ctx, inst_ref, |result, a, b| {
        inst::uqsub_v(result, a, b, 16, false)
    })
}

pub fn emit_packed_saturated_sub_s16(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_saturated_packed_op(code, ctx, inst_ref, |result, a, b| {
        inst::sqsub_v(result, a, b, 16, false)
    })
}

pub fn emit_packed_abs_diff_sum_u8(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_packed_op(code, ctx, inst_ref, |code, result, a, b| {
        code.write_u32(inst::movi_v8b_imm(V2, 0b0000_1111))?;
        code.write_u32(inst::uabd_v(result, a, b, 8, false))?;
        code.write_u32(inst::and_v8b(result, result, V2))?;
        code.write_u32(inst::uaddlv_from_v(result, result, 8, false))?;
        Ok(())
    })
}

pub fn emit_packed_select(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);

    let mut result = ctx.reg_alloc.write_d(inst_ref);
    let mut ge = ctx.reg_alloc.read_d(args[0]);
    let mut a = ctx.reg_alloc.read_d(args[1]);
    let mut b = ctx.reg_alloc.read_d(args[2]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut ge, &mut a, &mut b])?;
    let result = result.index().expect("result realized") as u8;
    let ge = ge.index().expect("ge realized") as u8;
    let a = a.index().expect("a realized") as u8;
    let b = b.index().expect("b realized") as u8;

    code.write_u32(inst::fmov_d(result, ge))?;
    code.write_u32(inst::bsl_v8b(result, b, a))?;
    Ok(())
}

pub fn emit_packed_instruction(
    code: &mut BlockOfCode,
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
