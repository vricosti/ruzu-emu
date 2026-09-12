//! ARM64 data-processing emitters.
//!
//! Upstream owner: `backend/arm64/emit_arm64_data_processing.cpp`.

use rhazel::{CodeGenerator, SystemReg, WReg, XReg, WZR};

use crate::backend::arm64::abi::regs::{WSCRATCH0, WSCRATCH1, XSCRATCH0, XSCRATCH1, XSTATE};
#[cfg(test)]
use crate::backend::arm64::block_of_code::BlockOfCode;
use crate::backend::arm64::emit_context::EmitContext;
use crate::backend::arm64::label::Label;
use crate::backend::arm64::reg_alloc::{Argument, RegAlloc};
use crate::ir::cond::Cond;
use crate::ir::inst::MAX_ARGS;
use crate::ir::opcode::Opcode;
use crate::ir::types::Type;
use crate::ir::value::InstRef;

pub fn emit_is_zero32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut operand = ctx.reg_alloc.read_w(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    ctx.reg_alloc.spill_flags(code)?;

    code.cmp_imm(operand.w(), 0)?;
    code.cinc(result.w(), WZR, Cond::EQ)
}

pub fn emit_is_zero64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut operand = ctx.reg_alloc.read_x(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    ctx.reg_alloc.spill_flags(code)?;

    code.cmp_imm(operand.x(), 0)?;
    code.cinc(result.w(), WZR, Cond::EQ)
}

pub fn emit_pack_2x32_to_1x64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);

    let mut lo = ctx.reg_alloc.read_w(args[0]);
    let mut hi = ctx.reg_alloc.read_w(args[1]);
    let mut result = ctx.reg_alloc.write_x(inst_ref);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut lo, &mut hi, &mut result])?;

    code.mov(result.w(), lo.w())?;
    code.bfi(result.x(), hi.x(), 32, 32)?;
    Ok(())
}

pub fn emit_pack_2x64_to_1x128(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let lo_in_gpr = args[0].is_in_gpr(&ctx.reg_alloc);
    let hi_in_gpr = args[1].is_in_gpr(&ctx.reg_alloc);

    match (lo_in_gpr, hi_in_gpr) {
        (true, true) => {
            let mut lo = ctx.reg_alloc.read_x(args[0]);
            let mut hi = ctx.reg_alloc.read_x(args[1]);
            let mut result = ctx.reg_alloc.write_q(inst_ref);
            RegAlloc::realize_all(code, ctx.block, &mut [&mut lo, &mut hi, &mut result])?;

            code.fmov_from_gp(result.d(), lo.x())?;
            code.mov_to_element(result.v().d2(), 1, hi.x())?;
        }
        (true, false) => {
            let mut lo = ctx.reg_alloc.read_x(args[0]);
            let mut hi = ctx.reg_alloc.read_d(args[1]);
            let mut result = ctx.reg_alloc.write_q(inst_ref);
            RegAlloc::realize_all(code, ctx.block, &mut [&mut lo, &mut hi, &mut result])?;

            code.fmov_from_gp(result.d(), lo.x())?;
            code.mov_d1_from_d0(result.v().d2(), hi.v().d2())?;
        }
        (false, true) => {
            let mut lo = ctx.reg_alloc.read_d(args[0]);
            let mut hi = ctx.reg_alloc.read_x(args[1]);
            let mut result = ctx.reg_alloc.write_q(inst_ref);
            RegAlloc::realize_all(code, ctx.block, &mut [&mut lo, &mut hi, &mut result])?;

            code.fmov(result.d(), lo.d())?;
            code.mov_to_element(result.v().d2(), 1, hi.x())?;
        }
        (false, false) => {
            let mut lo = ctx.reg_alloc.read_d(args[0]);
            let mut hi = ctx.reg_alloc.read_d(args[1]);
            let mut result = ctx.reg_alloc.write_q(inst_ref);
            RegAlloc::realize_all(code, ctx.block, &mut [&mut lo, &mut hi, &mut result])?;

            code.fmov(result.d(), lo.d())?;
            code.mov_d1_from_d0(result.v().d2(), hi.v().d2())?;
        }
    }
    Ok(())
}

pub fn emit_extract_register32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    assert!(
        args[2].is_immediate(),
        "ExtractRegister32 lsb must be immediate"
    );

    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut op1 = ctx.reg_alloc.read_w(args[0]);
    let mut op2 = ctx.reg_alloc.read_w(args[1]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut op1, &mut op2])?;

    code.extr(result.w(), op2.w(), op1.w(), args[2].get_immediate_u8())?;
    Ok(())
}

pub fn emit_extract_register64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    assert!(
        args[2].is_immediate(),
        "ExtractRegister64 lsb must be immediate"
    );

    let mut result = ctx.reg_alloc.write_x(inst_ref);
    let mut op1 = ctx.reg_alloc.read_x(args[0]);
    let mut op2 = ctx.reg_alloc.read_x(args[1]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut op1, &mut op2])?;

    code.extr(result.x(), op2.x(), op1.x(), args[2].get_immediate_u8())?;
    Ok(())
}

pub fn emit_least_significant_word(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut operand = ctx.reg_alloc.read_x(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    code.mov(result.w(), operand.w())?;
    Ok(())
}

pub fn emit_most_significant_word(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let carry_inst = ctx
        .block
        .get_associated_pseudo_operation(inst_ref, Opcode::GetCarryFromOp);
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);

    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut operand = ctx.reg_alloc.read_x(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    code.lsr(result.x(), operand.x(), 32)?;
    if let Some(carry_inst) = carry_inst {
        let mut carry = ctx.reg_alloc.write_w(carry_inst);
        carry.realize(code, ctx.block)?;
        code.lsr(carry.w(), operand.w(), 31 - 29)?;
        code.and_imm(carry.w(), carry.w(), (1 << 29) as u64)?;
    }
    Ok(())
}

pub fn emit_least_significant_half(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut operand = ctx.reg_alloc.read_w(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    code.ubfx(result.w(), operand.w(), 0, 16)?;
    Ok(())
}

pub fn emit_least_significant_byte(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut operand = ctx.reg_alloc.read_w(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    code.ubfx(result.w(), operand.w(), 0, 8)?;
    Ok(())
}

pub fn emit_test_bit(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let bit = args[1].get_immediate_u8();
    debug_assert!(bit < 64);

    let mut result = ctx.reg_alloc.write_x(inst_ref);
    let mut operand = ctx.reg_alloc.read_x(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;

    code.ubfx(result.x(), operand.x(), bit, 1)?;
    Ok(())
}

pub fn emit_conditional_select32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let cond = args[0].get_immediate_cond();
    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut then_value = ctx.reg_alloc.read_w(args[1]);
    let mut else_value = ctx.reg_alloc.read_w(args[2]);
    RegAlloc::realize_all(
        code,
        ctx.block,
        &mut [&mut result, &mut then_value, &mut else_value],
    )?;
    ctx.reg_alloc.spill_flags(code)?;

    code.ldr(WSCRATCH0, XSTATE, ctx.conf.state_nzcv_offset as u32)?;
    code.msr(SystemReg::NZCV, XSCRATCH0)?;
    code.csel(result.w(), then_value.w(), else_value.w(), cond)?;
    Ok(())
}

pub fn emit_conditional_select64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let cond = args[0].get_immediate_cond();
    let mut result = ctx.reg_alloc.write_x(inst_ref);
    let mut then_value = ctx.reg_alloc.read_x(args[1]);
    let mut else_value = ctx.reg_alloc.read_x(args[2]);
    RegAlloc::realize_all(
        code,
        ctx.block,
        &mut [&mut result, &mut then_value, &mut else_value],
    )?;
    ctx.reg_alloc.spill_flags(code)?;

    code.ldr(WSCRATCH0, XSTATE, ctx.conf.state_nzcv_offset as u32)?;
    code.msr(SystemReg::NZCV, XSCRATCH0)?;
    code.csel(result.x(), then_value.x(), else_value.x(), cond)?;
    Ok(())
}

pub fn emit_and32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_bit_op::<32>(code, ctx, inst_ref, BitOp::And)
}

pub fn emit_and64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_bit_op::<64>(code, ctx, inst_ref, BitOp::And)
}

pub fn emit_and_not32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_bit_op::<32>(code, ctx, inst_ref, BitOp::AndNot)
}

pub fn emit_and_not64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_bit_op::<64>(code, ctx, inst_ref, BitOp::AndNot)
}

pub fn emit_eor32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_bit_op::<32>(code, ctx, inst_ref, BitOp::Eor)
}

pub fn emit_eor64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_bit_op::<64>(code, ctx, inst_ref, BitOp::Eor)
}

pub fn emit_or32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_bit_op::<32>(code, ctx, inst_ref, BitOp::Or)
}

pub fn emit_or64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_bit_op::<64>(code, ctx, inst_ref, BitOp::Or)
}

pub fn emit_not32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_not::<32>(code, ctx, inst_ref)
}

pub fn emit_not64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_not::<64>(code, ctx, inst_ref)
}

pub fn emit_sign_extend_byte_to_word(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_sign_extend::<32, 8>(code, ctx, inst_ref)
}

pub fn emit_sign_extend_half_to_word(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_sign_extend::<32, 16>(code, ctx, inst_ref)
}

pub fn emit_sign_extend_byte_to_long(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_sign_extend::<64, 8>(code, ctx, inst_ref)
}

pub fn emit_sign_extend_half_to_long(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_sign_extend::<64, 16>(code, ctx, inst_ref)
}

pub fn emit_sign_extend_word_to_long(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_sign_extend::<64, 32>(code, ctx, inst_ref)
}

pub fn emit_zero_extend(
    _code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    ctx.reg_alloc
        .define_as_existing(ctx.block, inst_ref, args[0]);
    Ok(())
}

pub fn emit_zero_extend_long_to_quad(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut value = ctx.reg_alloc.read_x(args[0]);
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut value, &mut result])?;

    code.fmov_from_gp(result.d(), value.x())?;
    Ok(())
}

pub fn emit_logical_shift_left32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_shift32(code, ctx, inst_ref, ShiftOp::LogicalLeft)
}

pub fn emit_logical_shift_left64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_shift64(code, ctx, inst_ref, ShiftOp::LogicalLeft)
}

pub fn emit_logical_shift_right32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_shift32(code, ctx, inst_ref, ShiftOp::LogicalRight)
}

pub fn emit_logical_shift_right64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_shift64(code, ctx, inst_ref, ShiftOp::LogicalRight)
}

pub fn emit_arithmetic_shift_right32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_shift32(code, ctx, inst_ref, ShiftOp::ArithmeticRight)
}

pub fn emit_arithmetic_shift_right64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_shift64(code, ctx, inst_ref, ShiftOp::ArithmeticRight)
}

pub fn emit_rotate_right32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_shift32(code, ctx, inst_ref, ShiftOp::RotateRight)
}

pub fn emit_rotate_right_extended(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let carry_inst = ctx
        .block
        .get_associated_pseudo_operation(inst_ref, Opcode::GetCarryFromOp);
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);

    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut operand = ctx.reg_alloc.read_w(args[0]);
    let mut carry_out = carry_inst.map(|carry_inst| ctx.reg_alloc.write_w(carry_inst));

    if args[1].is_immediate() {
        if let Some(carry_out) = carry_out.as_mut() {
            RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand, carry_out])?;
        } else {
            RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
        }

        code.lsr(result.w(), operand.w(), 1)?;
        if args[1].get_immediate_u1() {
            code.orr_imm(result.w(), result.w(), 0x8000_0000)?;
        }
        if let Some(carry_out) = carry_out {
            code.and_imm(carry_out.w(), operand.w(), (1) as u64)?;
            code.lsl(carry_out.w(), carry_out.w(), 29)?;
        }
        return Ok(());
    }

    let mut carry_in = ctx.reg_alloc.read_w(args[1]);
    if let Some(carry_out) = carry_out.as_mut() {
        RegAlloc::realize_all(
            code,
            ctx.block,
            &mut [&mut result, &mut operand, &mut carry_in, carry_out],
        )?;
    } else {
        RegAlloc::realize_all(
            code,
            ctx.block,
            &mut [&mut result, &mut operand, &mut carry_in],
        )?;
    }

    code.lsr(WSCRATCH0, carry_in.w(), 29)?;
    code.extr(result.w(), WSCRATCH0, operand.w(), 1)?;
    if let Some(carry_out) = carry_out {
        code.and_imm(carry_out.w(), operand.w(), (1) as u64)?;
        code.lsl(carry_out.w(), carry_out.w(), 29)?;
    }
    Ok(())
}

pub fn emit_rotate_right64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_shift64(code, ctx, inst_ref, ShiftOp::RotateRight)
}

pub fn emit_add32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_add_sub::<32, false>(code, ctx, inst_ref)
}

pub fn emit_add64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_add_sub::<64, false>(code, ctx, inst_ref)
}

pub fn emit_sub32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_add_sub::<32, true>(code, ctx, inst_ref)
}

pub fn emit_sub64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_add_sub::<64, true>(code, ctx, inst_ref)
}

pub fn emit_mul32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_mul::<32>(code, ctx, inst_ref)
}

pub fn emit_mul64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_mul::<64>(code, ctx, inst_ref)
}

pub fn emit_signed_multiply_high64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_multiply_high64::<true>(code, ctx, inst_ref)
}

pub fn emit_unsigned_multiply_high64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_multiply_high64::<false>(code, ctx, inst_ref)
}

pub fn emit_unsigned_div32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_div::<32, false>(code, ctx, inst_ref)
}

pub fn emit_unsigned_div64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_div::<64, false>(code, ctx, inst_ref)
}

pub fn emit_signed_div32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_div::<32, true>(code, ctx, inst_ref)
}

pub fn emit_signed_div64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_div::<64, true>(code, ctx, inst_ref)
}

fn emit_max_min32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    cond: Cond,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);

    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut op1 = ctx.reg_alloc.read_w(args[0]);
    let mut op2 = ctx.reg_alloc.read_w(args[1]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut op1, &mut op2])?;
    ctx.reg_alloc.spill_flags(code)?;

    code.cmp(op1.w(), op2.w())?;
    code.csel(result.w(), op1.w(), op2.w(), cond)?;
    Ok(())
}

fn emit_max_min64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    cond: Cond,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);

    let mut result = ctx.reg_alloc.write_x(inst_ref);
    let mut op1 = ctx.reg_alloc.read_x(args[0]);
    let mut op2 = ctx.reg_alloc.read_x(args[1]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut op1, &mut op2])?;
    ctx.reg_alloc.spill_flags(code)?;

    code.cmp(op1.x(), op2.x())?;
    code.csel(result.x(), op1.x(), op2.x(), cond)?;
    Ok(())
}

pub fn emit_max_signed32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_max_min32(code, ctx, inst_ref, Cond::GT)
}

pub fn emit_max_signed64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_max_min64(code, ctx, inst_ref, Cond::GT)
}

pub fn emit_max_unsigned32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_max_min32(code, ctx, inst_ref, Cond::HI)
}

pub fn emit_max_unsigned64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_max_min64(code, ctx, inst_ref, Cond::HI)
}

pub fn emit_min_signed32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_max_min32(code, ctx, inst_ref, Cond::LT)
}

pub fn emit_min_signed64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_max_min64(code, ctx, inst_ref, Cond::LT)
}

pub fn emit_min_unsigned32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_max_min32(code, ctx, inst_ref, Cond::LO)
}

pub fn emit_min_unsigned64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_max_min64(code, ctx, inst_ref, Cond::LO)
}

pub fn emit_logical_shift_left_masked32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_shift_masked32(code, ctx, inst_ref, ShiftOp::LogicalLeft)
}

pub fn emit_logical_shift_left_masked64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_shift_masked64(code, ctx, inst_ref, ShiftOp::LogicalLeft)
}

pub fn emit_logical_shift_right_masked32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_shift_masked32(code, ctx, inst_ref, ShiftOp::LogicalRight)
}

pub fn emit_logical_shift_right_masked64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_shift_masked64(code, ctx, inst_ref, ShiftOp::LogicalRight)
}

pub fn emit_arithmetic_shift_right_masked32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_shift_masked32(code, ctx, inst_ref, ShiftOp::ArithmeticRight)
}

pub fn emit_arithmetic_shift_right_masked64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_shift_masked64(code, ctx, inst_ref, ShiftOp::ArithmeticRight)
}

pub fn emit_rotate_right_masked32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_shift_masked32(code, ctx, inst_ref, ShiftOp::RotateRight)
}

pub fn emit_rotate_right_masked64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_shift_masked64(code, ctx, inst_ref, ShiftOp::RotateRight)
}

pub fn emit_count_leading_zeros32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_count_leading_zeros::<32>(code, ctx, inst_ref)
}

pub fn emit_count_leading_zeros64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_count_leading_zeros::<64>(code, ctx, inst_ref)
}

pub fn emit_byte_reverse_word(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut operand = ctx.reg_alloc.read_w(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    code.rev(result.w(), operand.w())?;
    Ok(())
}

pub fn emit_byte_reverse_half(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut operand = ctx.reg_alloc.read_w(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    code.rev16(result.w(), operand.w())?;
    Ok(())
}

pub fn emit_byte_reverse_dual(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_x(inst_ref);
    let mut operand = ctx.reg_alloc.read_x(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    code.rev(result.x(), operand.x())?;
    Ok(())
}

pub fn emit_replicate_bit32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    assert!(
        args[1].is_immediate(),
        "ReplicateBit32 bit must be immediate"
    );

    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut value = ctx.reg_alloc.read_w(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut value])?;

    let bit = args[1].get_immediate_u8();
    code.lsl(result.w(), value.w(), 31 - bit)?;
    code.asr(result.w(), result.w(), 31)?;
    Ok(())
}

pub fn emit_replicate_bit64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    assert!(
        args[1].is_immediate(),
        "ReplicateBit64 bit must be immediate"
    );

    let mut result = ctx.reg_alloc.write_x(inst_ref);
    let mut value = ctx.reg_alloc.read_x(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut value])?;

    let bit = args[1].get_immediate_u8();
    code.lsl(result.x(), value.x(), 63 - bit)?;
    code.asr(result.x(), result.x(), 63)?;
    Ok(())
}

pub fn emit_get_nzcv_from_op(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);

    if ctx.reg_alloc.was_value_defined(inst_ref) {
        return Ok(());
    }

    let source_type = if args[0].value.is_immediate() {
        args[0].value.get_type()
    } else {
        ctx.block.inst_real_return_type(args[0].value.inst_ref())
    };

    match source_type {
        Type::U8 | Type::U16 => {
            let mask = if source_type == Type::U8 {
                0xff
            } else {
                0xffff
            };
            let mut value = ctx.reg_alloc.read_w(args[0]);
            let mut flags = ctx.reg_alloc.write_flags(inst_ref);
            RegAlloc::realize_all(code, ctx.block, &mut [&mut value, &mut flags])?;

            code.and_imm(WSCRATCH0, value.w(), (mask) as u64)?;
            code.tst(WSCRATCH0, WSCRATCH0)?;
            Ok(())
        }
        Type::U32 => {
            let mut value = ctx.reg_alloc.read_w(args[0]);
            let mut flags = ctx.reg_alloc.write_flags(inst_ref);
            RegAlloc::realize_all(code, ctx.block, &mut [&mut value, &mut flags])?;

            code.tst(value.w(), value.w())?;
            Ok(())
        }
        Type::U64 => {
            let mut value = ctx.reg_alloc.read_x(args[0]);
            let mut flags = ctx.reg_alloc.write_flags(inst_ref);
            RegAlloc::realize_all(code, ctx.block, &mut [&mut value, &mut flags])?;

            code.tst(value.x(), value.x())?;
            Ok(())
        }
        ty => Err(format!("ARM64 GetNZCVFromOp unsupported input type {ty:?}")),
    }
}

fn emit_mul<const BITSIZE: usize>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    debug_assert!(BITSIZE == 32 || BITSIZE == 64);

    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    if BITSIZE == 32 {
        let mut result = ctx.reg_alloc.write_w(inst_ref);
        let mut lhs = ctx.reg_alloc.read_w(args[0]);
        let mut rhs = ctx.reg_alloc.read_w(args[1]);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut lhs, &mut rhs])?;
        code.mul(result.w(), lhs.w(), rhs.w())?;
    } else {
        let mut result = ctx.reg_alloc.write_x(inst_ref);
        let mut lhs = ctx.reg_alloc.read_x(args[0]);
        let mut rhs = ctx.reg_alloc.read_x(args[1]);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut lhs, &mut rhs])?;
        code.mul(result.x(), lhs.x(), rhs.x())?;
    }
    Ok(())
}

fn emit_multiply_high64<const SIGNED: bool>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_x(inst_ref);
    let mut op1 = ctx.reg_alloc.read_x(args[0]);
    let mut op2 = ctx.reg_alloc.read_x(args[1]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut op1, &mut op2])?;

    let emission = if SIGNED {
        code.smulh(result.x(), op1.x(), op2.x())
    } else {
        code.umulh(result.x(), op1.x(), op2.x())
    };
    emission?;
    Ok(())
}

fn emit_div<const BITSIZE: usize, const SIGNED: bool>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    debug_assert!(BITSIZE == 32 || BITSIZE == 64);

    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    if BITSIZE == 32 {
        let mut result = ctx.reg_alloc.write_w(inst_ref);
        let mut lhs = ctx.reg_alloc.read_w(args[0]);
        let mut rhs = ctx.reg_alloc.read_w(args[1]);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut lhs, &mut rhs])?;

        (if SIGNED {
            code.sdiv(result.w(), lhs.w(), rhs.w())
        } else {
            code.udiv(result.w(), lhs.w(), rhs.w())
        })?;
    } else {
        let mut result = ctx.reg_alloc.write_x(inst_ref);
        let mut lhs = ctx.reg_alloc.read_x(args[0]);
        let mut rhs = ctx.reg_alloc.read_x(args[1]);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut lhs, &mut rhs])?;

        (if SIGNED {
            code.sdiv(result.x(), lhs.x(), rhs.x())
        } else {
            code.udiv(result.x(), lhs.x(), rhs.x())
        })?;
    }
    Ok(())
}

fn emit_count_leading_zeros<const BITSIZE: usize>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    debug_assert!(BITSIZE == 32 || BITSIZE == 64);

    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    if BITSIZE == 32 {
        let mut result = ctx.reg_alloc.write_w(inst_ref);
        let mut operand = ctx.reg_alloc.read_w(args[0]);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
        code.clz(result.w(), operand.w())?;
    } else {
        let mut result = ctx.reg_alloc.write_x(inst_ref);
        let mut operand = ctx.reg_alloc.read_x(args[0]);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
        code.clz(result.x(), operand.x())?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BitOp {
    And,
    AndNot,
    Eor,
    Or,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShiftOp {
    LogicalLeft,
    LogicalRight,
    ArithmeticRight,
    RotateRight,
}

fn emit_shift32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    op: ShiftOp,
) -> Result<(), String> {
    let carry_inst = ctx
        .block
        .get_associated_pseudo_operation(inst_ref, Opcode::GetCarryFromOp);
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let operand_arg = args[0];
    let shift_arg = args[1];
    let carry_arg = args[2];

    if let Some(carry_inst) = carry_inst {
        return emit_shift32_with_carry(
            code,
            ctx,
            inst_ref,
            carry_inst,
            op,
            operand_arg,
            shift_arg,
            carry_arg,
        );
    }

    if shift_arg.is_immediate() {
        let shift = shift_arg.get_immediate_u8();
        let mut result = ctx.reg_alloc.write_w(inst_ref);
        let mut operand = ctx.reg_alloc.read_w(operand_arg);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;

        match op {
            ShiftOp::LogicalLeft if shift <= 31 => {
                code.lsl(result.w(), operand.w(), shift)?;
            }
            ShiftOp::LogicalRight if shift <= 31 => {
                code.lsr(result.w(), operand.w(), shift)?;
            }
            ShiftOp::LogicalLeft | ShiftOp::LogicalRight => {
                code.mov(result.w(), WZR)?;
            }
            ShiftOp::ArithmeticRight => {
                code.asr(result.w(), operand.w(), shift.min(31))?;
            }
            ShiftOp::RotateRight => {
                code.ror(result.w(), operand.w(), shift % 32)?;
            }
        }
        return Ok(());
    }

    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut operand = ctx.reg_alloc.read_w(operand_arg);
    let mut shift = ctx.reg_alloc.read_w(shift_arg);
    RegAlloc::realize_all(
        code,
        ctx.block,
        &mut [&mut result, &mut operand, &mut shift],
    )?;
    ctx.reg_alloc.spill_flags(code)?;

    match op {
        ShiftOp::LogicalLeft | ShiftOp::LogicalRight => {
            code.and_imm(WSCRATCH0, shift.w(), (0xff) as u64)?;
            let shift_reg = XSCRATCH0;
            let emission = match op {
                ShiftOp::LogicalLeft => code.lslv(result.w(), operand.w(), shift_reg.to_w()),
                ShiftOp::LogicalRight => code.lsrv(result.w(), operand.w(), shift_reg.to_w()),
                _ => unreachable!(),
            };
            emission?;
            code.cmp_imm(shift_reg.to_w(), 32)?;
            code.csel(result.w(), result.w(), WZR, Cond::LT)?;
        }
        ShiftOp::ArithmeticRight => {
            code.and_imm(WSCRATCH0, shift.w(), (0xff) as u64)?;
            code.movz(WSCRATCH1, 31, 0)?;
            code.cmp_imm(WSCRATCH0, 31)?;
            code.csel(WSCRATCH0, WSCRATCH0, WSCRATCH1, Cond::LS)?;
            code.asrv(result.w(), operand.w(), WSCRATCH0)?;
        }
        ShiftOp::RotateRight => {
            code.rorv(result.w(), operand.w(), shift.w())?;
        }
    }
    Ok(())
}

fn emit_shift32_with_carry(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    carry_inst: InstRef,
    op: ShiftOp,
    operand_arg: Argument,
    shift_arg: Argument,
    carry_arg: Argument,
) -> Result<(), String> {
    if shift_arg.is_immediate() && shift_arg.get_immediate_u8() == 0 {
        ctx.reg_alloc
            .define_as_existing(ctx.block, carry_inst, carry_arg);
        ctx.reg_alloc
            .define_as_existing(ctx.block, inst_ref, operand_arg);
        return Ok(());
    }

    match op {
        ShiftOp::LogicalLeft => emit_logical_shift_left32_with_carry(
            code,
            ctx,
            inst_ref,
            carry_inst,
            operand_arg,
            shift_arg,
            carry_arg,
        ),
        ShiftOp::LogicalRight => emit_logical_shift_right32_with_carry(
            code,
            ctx,
            inst_ref,
            carry_inst,
            operand_arg,
            shift_arg,
            carry_arg,
        ),
        ShiftOp::ArithmeticRight => emit_arithmetic_shift_right32_with_carry(
            code,
            ctx,
            inst_ref,
            carry_inst,
            operand_arg,
            shift_arg,
            carry_arg,
        ),
        ShiftOp::RotateRight => emit_rotate_right32_with_carry(
            code,
            ctx,
            inst_ref,
            carry_inst,
            operand_arg,
            shift_arg,
            carry_arg,
        ),
    }
}

fn emit_logical_shift_left32_with_carry(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    carry_inst: InstRef,
    operand_arg: Argument,
    shift_arg: Argument,
    carry_arg: Argument,
) -> Result<(), String> {
    if shift_arg.is_immediate() {
        let shift = shift_arg.get_immediate_u8();
        if shift < 32 {
            let mut result = ctx.reg_alloc.write_w(inst_ref);
            let mut carry_out = ctx.reg_alloc.write_w(carry_inst);
            let mut operand = ctx.reg_alloc.read_w(operand_arg);
            RegAlloc::realize_all(
                code,
                ctx.block,
                &mut [&mut result, &mut carry_out, &mut operand],
            )?;

            code.ubfx(carry_out.w(), operand.w(), 32 - shift, 1)?;
            code.lsl(carry_out.w(), carry_out.w(), 29)?;
            code.lsl(result.w(), operand.w(), shift)?;
        } else if shift > 32 {
            let mut result = ctx.reg_alloc.write_w(inst_ref);
            let mut carry_out = ctx.reg_alloc.write_w(carry_inst);
            RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut carry_out])?;
            code.mov(result.w(), WZR)?;
            code.mov(carry_out.w(), WZR)?;
        } else {
            let mut result = ctx.reg_alloc.write_w(inst_ref);
            let mut carry_out = ctx.reg_alloc.write_w(carry_inst);
            let mut operand = ctx.reg_alloc.read_w(operand_arg);
            RegAlloc::realize_all(
                code,
                ctx.block,
                &mut [&mut result, &mut carry_out, &mut operand],
            )?;

            code.ubfiz(carry_out.w(), operand.w(), 29, 1)?;
            code.mov(result.w(), WZR)?;
        }
        return Ok(());
    }

    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut carry_out = ctx.reg_alloc.write_w(carry_inst);
    let mut operand = ctx.reg_alloc.read_w(operand_arg);
    let mut shift = ctx.reg_alloc.read_w(shift_arg);
    let mut carry_in = ctx.reg_alloc.read_w(carry_arg);
    let carry_in_reg;
    if carry_arg.is_immediate() {
        RegAlloc::realize_all(
            code,
            ctx.block,
            &mut [&mut result, &mut carry_out, &mut operand, &mut shift],
        )?;
        carry_in_reg = None;
    } else {
        RegAlloc::realize_all(
            code,
            ctx.block,
            &mut [
                &mut result,
                &mut carry_out,
                &mut operand,
                &mut shift,
                &mut carry_in,
            ],
        )?;
        carry_in_reg = Some(carry_in.w());
    }
    ctx.reg_alloc.spill_flags(code)?;

    let mut zero = Label::new();
    let mut end = Label::new();

    code.ands_imm(WSCRATCH1, shift.w(), 0xff)?;
    code.b_cond(Cond::EQ, &mut zero)?;
    code.neg(WSCRATCH0, shift.w())?;
    code.lsrv(carry_out.w(), operand.w(), WSCRATCH0)?;
    code.lslv(result.w(), operand.w(), shift.w())?;
    code.ubfiz(carry_out.w(), carry_out.w(), 29, 1)?;
    code.cmp_imm(WSCRATCH1, 32)?;
    code.csel(result.w(), result.w(), WZR, Cond::LT)?;
    code.csel(carry_out.w(), carry_out.w(), WZR, Cond::LE)?;
    code.b(&mut end)?;
    code.l(&mut zero)?;
    code.mov(result.w(), operand.w())?;
    emit_carry_input_to_reg(code, carry_arg, carry_in_reg, carry_out.w())?;
    code.l(&mut end)?;
    Ok(())
}

fn emit_logical_shift_right32_with_carry(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    carry_inst: InstRef,
    operand_arg: Argument,
    shift_arg: Argument,
    carry_arg: Argument,
) -> Result<(), String> {
    if shift_arg.is_immediate() {
        let shift = shift_arg.get_immediate_u8();
        if shift < 32 {
            let mut result = ctx.reg_alloc.write_w(inst_ref);
            let mut carry_out = ctx.reg_alloc.write_w(carry_inst);
            let mut operand = ctx.reg_alloc.read_w(operand_arg);
            RegAlloc::realize_all(
                code,
                ctx.block,
                &mut [&mut result, &mut carry_out, &mut operand],
            )?;

            code.ubfx(carry_out.w(), operand.w(), shift - 1, 1)?;
            code.lsl(carry_out.w(), carry_out.w(), 29)?;
            code.lsr(result.w(), operand.w(), shift)?;
        } else if shift > 32 {
            let mut result = ctx.reg_alloc.write_w(inst_ref);
            let mut carry_out = ctx.reg_alloc.write_w(carry_inst);
            RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut carry_out])?;
            code.mov(result.w(), WZR)?;
            code.mov(carry_out.w(), WZR)?;
        } else {
            let mut result = ctx.reg_alloc.write_w(inst_ref);
            let mut carry_out = ctx.reg_alloc.write_w(carry_inst);
            let mut operand = ctx.reg_alloc.read_w(operand_arg);
            RegAlloc::realize_all(
                code,
                ctx.block,
                &mut [&mut result, &mut carry_out, &mut operand],
            )?;

            code.lsr(carry_out.w(), operand.w(), 31 - 29)?;
            code.and_imm(carry_out.w(), carry_out.w(), (1 << 29) as u64)?;
            code.mov(result.w(), WZR)?;
        }
        return Ok(());
    }

    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut carry_out = ctx.reg_alloc.write_w(carry_inst);
    let mut operand = ctx.reg_alloc.read_w(operand_arg);
    let mut shift = ctx.reg_alloc.read_w(shift_arg);
    let mut carry_in = ctx.reg_alloc.read_w(carry_arg);
    let carry_in_reg;
    if carry_arg.is_immediate() {
        RegAlloc::realize_all(
            code,
            ctx.block,
            &mut [&mut result, &mut carry_out, &mut operand, &mut shift],
        )?;
        carry_in_reg = None;
    } else {
        RegAlloc::realize_all(
            code,
            ctx.block,
            &mut [
                &mut result,
                &mut carry_out,
                &mut operand,
                &mut shift,
                &mut carry_in,
            ],
        )?;
        carry_in_reg = Some(carry_in.w());
    }
    ctx.reg_alloc.spill_flags(code)?;

    let mut zero = Label::new();
    let mut end = Label::new();

    code.ands_imm(WSCRATCH1, shift.w(), 0xff)?;
    code.b_cond(Cond::EQ, &mut zero)?;
    code.sub_imm(WSCRATCH0, shift.w(), 1)?;
    code.lsrv(carry_out.w(), operand.w(), WSCRATCH0)?;
    code.lsrv(result.w(), operand.w(), shift.w())?;
    code.ubfiz(carry_out.w(), carry_out.w(), 29, 1)?;
    code.cmp_imm(WSCRATCH1, 32)?;
    code.csel(result.w(), result.w(), WZR, Cond::LT)?;
    code.csel(carry_out.w(), carry_out.w(), WZR, Cond::LE)?;
    code.b(&mut end)?;
    code.l(&mut zero)?;
    code.mov(result.w(), operand.w())?;
    emit_carry_input_to_reg(code, carry_arg, carry_in_reg, carry_out.w())?;
    code.l(&mut end)?;
    Ok(())
}

fn emit_arithmetic_shift_right32_with_carry(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    carry_inst: InstRef,
    operand_arg: Argument,
    shift_arg: Argument,
    carry_arg: Argument,
) -> Result<(), String> {
    if shift_arg.is_immediate() {
        let shift = shift_arg.get_immediate_u8();
        let mut result = ctx.reg_alloc.write_w(inst_ref);
        let mut carry_out = ctx.reg_alloc.write_w(carry_inst);
        let mut operand = ctx.reg_alloc.read_w(operand_arg);
        RegAlloc::realize_all(
            code,
            ctx.block,
            &mut [&mut result, &mut carry_out, &mut operand],
        )?;

        if shift <= 31 {
            code.ubfx(carry_out.w(), operand.w(), shift - 1, 1)?;
            code.lsl(carry_out.w(), carry_out.w(), 29)?;
            code.asr(result.w(), operand.w(), shift)?;
        } else {
            code.asr(result.w(), operand.w(), 31)?;
            code.and_imm(carry_out.w(), result.w(), (1 << 29) as u64)?;
        }
        return Ok(());
    }

    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut carry_out = ctx.reg_alloc.write_w(carry_inst);
    let mut operand = ctx.reg_alloc.read_w(operand_arg);
    let mut shift = ctx.reg_alloc.read_w(shift_arg);
    let mut carry_in = ctx.reg_alloc.read_w(carry_arg);
    let carry_in_reg;
    if carry_arg.is_immediate() {
        RegAlloc::realize_all(
            code,
            ctx.block,
            &mut [&mut result, &mut carry_out, &mut operand, &mut shift],
        )?;
        carry_in_reg = None;
    } else {
        RegAlloc::realize_all(
            code,
            ctx.block,
            &mut [
                &mut result,
                &mut carry_out,
                &mut operand,
                &mut shift,
                &mut carry_in,
            ],
        )?;
        carry_in_reg = Some(carry_in.w());
    }
    ctx.reg_alloc.spill_flags(code)?;

    let mut zero = Label::new();
    let mut end = Label::new();

    code.ands_imm(WSCRATCH0, shift.w(), 0xff)?;
    code.b_cond(Cond::EQ, &mut zero)?;
    code.movz(WSCRATCH1, 63, 0)?;
    code.cmp_imm(WSCRATCH0, 63)?;
    code.csel(WSCRATCH0, WSCRATCH0, WSCRATCH1, Cond::LS)?;
    code.sxtw(result.x(), operand.w())?;
    code.sub_imm(WSCRATCH1, WSCRATCH0, 1)?;
    code.asrv(carry_out.x(), result.x(), XSCRATCH1)?;
    code.asrv(result.x(), result.x(), XSCRATCH0)?;
    code.ubfiz(carry_out.w(), carry_out.w(), 29, 1)?;
    code.mov(result.w(), result.w())?;
    code.b(&mut end)?;
    code.l(&mut zero)?;
    code.mov(result.w(), operand.w())?;
    emit_carry_input_to_reg(code, carry_arg, carry_in_reg, carry_out.w())?;
    code.l(&mut end)?;
    Ok(())
}

fn emit_rotate_right32_with_carry(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    carry_inst: InstRef,
    operand_arg: Argument,
    shift_arg: Argument,
    carry_arg: Argument,
) -> Result<(), String> {
    if shift_arg.is_immediate() {
        let shift = shift_arg.get_immediate_u8() % 32;
        let mut result = ctx.reg_alloc.write_w(inst_ref);
        let mut operand = ctx.reg_alloc.read_w(operand_arg);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;

        code.ror(result.w(), operand.w(), shift)?;

        let mut carry_out = ctx.reg_alloc.write_w(carry_inst);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut carry_out])?;

        code.ror(carry_out.w(), operand.w(), ((shift + 31) - 29) % 32)?;
        code.and_imm(carry_out.w(), carry_out.w(), (1 << 29) as u64)?;
        return Ok(());
    }

    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut operand = ctx.reg_alloc.read_w(operand_arg);
    let mut shift = ctx.reg_alloc.read_w(shift_arg);
    RegAlloc::realize_all(
        code,
        ctx.block,
        &mut [&mut result, &mut operand, &mut shift],
    )?;

    code.rorv(result.w(), operand.w(), shift.w())?;

    if carry_arg.is_immediate() {
        let mut carry_out = ctx.reg_alloc.write_w(carry_inst);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut carry_out])?;
        ctx.reg_alloc.spill_flags(code)?;

        code.ands_imm(WZR, shift.w(), 0xff)?;
        code.lsr(carry_out.w(), result.w(), 31 - 29)?;
        code.and_imm(carry_out.w(), carry_out.w(), (1 << 29) as u64)?;
        if carry_arg.get_immediate_u1() {
            emit_mov_w_imm(code, WSCRATCH0, 1 << 29)?;
            code.csel(carry_out.w(), WSCRATCH0, carry_out.w(), Cond::EQ)?;
        } else {
            code.csel(carry_out.w(), WZR, carry_out.w(), Cond::EQ)?;
        }
    } else {
        let mut carry_in = ctx.reg_alloc.read_w(carry_arg);
        let mut carry_out = ctx.reg_alloc.write_w(carry_inst);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut carry_out, &mut carry_in])?;
        ctx.reg_alloc.spill_flags(code)?;

        code.ands_imm(WZR, shift.w(), 0xff)?;
        code.lsr(carry_out.w(), result.w(), 31 - 29)?;
        code.and_imm(carry_out.w(), carry_out.w(), (1 << 29) as u64)?;
        code.csel(carry_out.w(), carry_in.w(), carry_out.w(), Cond::EQ)?;
    }
    Ok(())
}

fn emit_carry_input_to_reg(
    code: &mut CodeGenerator<'_>,
    carry_arg: Argument,
    carry_in_reg: Option<WReg>,
    reg: WReg,
) -> Result<(), String> {
    if let Some(carry_in_reg) = carry_in_reg {
        code.mov(reg, carry_in_reg)?;
        Ok(())
    } else {
        debug_assert!(carry_arg.is_immediate());
        emit_mov_w_imm(code, reg, u32::from(carry_arg.get_immediate_u1()) << 29)
    }
}

fn emit_mov_w_imm(code: &mut CodeGenerator<'_>, reg: WReg, imm: u32) -> Result<(), String> {
    code.movz(reg, (imm & 0xffff) as u16, 0)?;
    let high = (imm >> 16) as u16;
    if high != 0 {
        code.movk(reg, high, 16)?;
    }
    Ok(())
}

fn emit_mov_x_imm(code: &mut CodeGenerator<'_>, reg: XReg, imm: u64) -> Result<(), String> {
    code.movz(reg, (imm & 0xffff) as u16, 0)?;
    for shift in [16, 32, 48] {
        let chunk = ((imm >> shift) & 0xffff) as u16;
        if chunk != 0 {
            code.movk(reg, chunk, shift as u8)?;
        }
    }
    Ok(())
}

fn emit_shift64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    op: ShiftOp,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let operand_arg = args[0];
    let shift_arg = args[1];

    if shift_arg.is_immediate() {
        let shift = shift_arg.get_immediate_u8();
        let mut result = ctx.reg_alloc.write_x(inst_ref);
        let mut operand = ctx.reg_alloc.read_x(operand_arg);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;

        match op {
            ShiftOp::LogicalLeft if shift <= 63 => {
                code.lsl(result.x(), operand.x(), shift)?;
            }
            ShiftOp::LogicalRight if shift <= 63 => {
                code.lsr(result.x(), operand.x(), shift)?;
            }
            ShiftOp::LogicalLeft | ShiftOp::LogicalRight => {
                code.mov(result.x(), rhazel::XZR)?;
            }
            ShiftOp::ArithmeticRight => {
                code.asr(result.x(), operand.x(), shift.min(63))?;
            }
            ShiftOp::RotateRight => {
                code.ror(result.x(), operand.x(), shift % 64)?;
            }
        }
        return Ok(());
    }

    let mut result = ctx.reg_alloc.write_x(inst_ref);
    let mut operand = ctx.reg_alloc.read_x(operand_arg);
    let mut shift = ctx.reg_alloc.read_x(shift_arg);
    RegAlloc::realize_all(
        code,
        ctx.block,
        &mut [&mut result, &mut operand, &mut shift],
    )?;

    match op {
        ShiftOp::LogicalLeft | ShiftOp::LogicalRight => {
            ctx.reg_alloc.spill_flags(code)?;
            code.and_imm(XSCRATCH0, shift.x(), 0xff)?;
            let shift_reg = XSCRATCH0;
            let emission = match op {
                ShiftOp::LogicalLeft => code.lslv(result.x(), operand.x(), shift_reg),
                ShiftOp::LogicalRight => code.lsrv(result.x(), operand.x(), shift_reg),
                _ => unreachable!(),
            };
            emission?;
            code.cmp_imm(shift_reg, 64)?;
            code.csel(result.x(), result.x(), rhazel::XZR, Cond::LT)?;
        }
        ShiftOp::ArithmeticRight => {
            code.asrv(result.x(), operand.x(), shift.x())?;
        }
        ShiftOp::RotateRight => {
            code.rorv(result.x(), operand.x(), shift.x())?;
        }
    }
    Ok(())
}

fn emit_shift_masked32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    op: ShiftOp,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let operand_arg = args[0];
    let shift_arg = args[1];

    if shift_arg.is_immediate() {
        let shift = (shift_arg.get_immediate_u32() & 0x1f) as u8;
        let mut result = ctx.reg_alloc.write_w(inst_ref);
        let mut operand = ctx.reg_alloc.read_w(operand_arg);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;

        let emission = match op {
            ShiftOp::LogicalLeft => code.lsl(result.w(), operand.w(), shift),
            ShiftOp::LogicalRight => code.lsr(result.w(), operand.w(), shift),
            ShiftOp::ArithmeticRight => code.asr(result.w(), operand.w(), shift),
            ShiftOp::RotateRight => code.ror(result.w(), operand.w(), shift),
        };
        emission?;
        return Ok(());
    }

    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut operand = ctx.reg_alloc.read_w(operand_arg);
    let mut shift = ctx.reg_alloc.read_w(shift_arg);
    RegAlloc::realize_all(
        code,
        ctx.block,
        &mut [&mut result, &mut operand, &mut shift],
    )?;

    let emission = match op {
        ShiftOp::LogicalLeft => code.lslv(result.w(), operand.w(), shift.w()),
        ShiftOp::LogicalRight => code.lsrv(result.w(), operand.w(), shift.w()),
        ShiftOp::ArithmeticRight => code.asrv(result.w(), operand.w(), shift.w()),
        ShiftOp::RotateRight => code.rorv(result.w(), operand.w(), shift.w()),
    };
    emission?;
    Ok(())
}

fn emit_shift_masked64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    op: ShiftOp,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let operand_arg = args[0];
    let shift_arg = args[1];

    if shift_arg.is_immediate() {
        let shift = (shift_arg.get_immediate_u64() & 0x3f) as u8;
        let mut result = ctx.reg_alloc.write_x(inst_ref);
        let mut operand = ctx.reg_alloc.read_x(operand_arg);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;

        let emission = match op {
            ShiftOp::LogicalLeft => code.lsl(result.x(), operand.x(), shift),
            ShiftOp::LogicalRight => code.lsr(result.x(), operand.x(), shift),
            ShiftOp::ArithmeticRight => code.asr(result.x(), operand.x(), shift),
            ShiftOp::RotateRight => code.ror(result.x(), operand.x(), shift),
        };
        emission?;
        return Ok(());
    }

    let mut result = ctx.reg_alloc.write_x(inst_ref);
    let mut operand = ctx.reg_alloc.read_x(operand_arg);
    let mut shift = ctx.reg_alloc.read_x(shift_arg);
    RegAlloc::realize_all(
        code,
        ctx.block,
        &mut [&mut result, &mut operand, &mut shift],
    )?;

    let emission = match op {
        ShiftOp::LogicalLeft => code.lslv(result.x(), operand.x(), shift.x()),
        ShiftOp::LogicalRight => code.lsrv(result.x(), operand.x(), shift.x()),
        ShiftOp::ArithmeticRight => code.asrv(result.x(), operand.x(), shift.x()),
        ShiftOp::RotateRight => code.rorv(result.x(), operand.x(), shift.x()),
    };
    emission?;
    Ok(())
}

fn emit_bit_op<const BITSIZE: usize>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    op: BitOp,
) -> Result<(), String> {
    debug_assert!(BITSIZE == 32 || BITSIZE == 64);

    let flag_inst = if matches!(op, BitOp::And | BitOp::AndNot) {
        associated_nz_or_nzcv(ctx, inst_ref)?
    } else {
        None
    };

    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    if BITSIZE == 32 {
        let mut result = ctx.reg_alloc.write_w(inst_ref);
        let mut lhs = ctx.reg_alloc.read_w(args[0]);
        let mut rhs = ctx.reg_alloc.read_w(args[1]);
        if let Some(flag_inst) = flag_inst {
            let mut flags = ctx.reg_alloc.write_flags(flag_inst);
            RegAlloc::realize_all(
                code,
                ctx.block,
                &mut [&mut result, &mut lhs, &mut rhs, &mut flags],
            )?;
            emit_bit_op_reg_flags::<32>(code, op, result.x(), lhs.x(), rhs.x())
        } else {
            RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut lhs, &mut rhs])?;
            emit_bit_op_reg::<32>(code, op, result.x(), lhs.x(), rhs.x())
        }
    } else {
        let mut result = ctx.reg_alloc.write_x(inst_ref);
        let mut lhs = ctx.reg_alloc.read_x(args[0]);
        let mut rhs = ctx.reg_alloc.read_x(args[1]);
        if let Some(flag_inst) = flag_inst {
            let mut flags = ctx.reg_alloc.write_flags(flag_inst);
            RegAlloc::realize_all(
                code,
                ctx.block,
                &mut [&mut result, &mut lhs, &mut rhs, &mut flags],
            )?;
            emit_bit_op_reg_flags::<64>(code, op, result.x(), lhs.x(), rhs.x())
        } else {
            RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut lhs, &mut rhs])?;
            emit_bit_op_reg::<64>(code, op, result.x(), lhs.x(), rhs.x())
        }
    }
}

fn emit_bit_op_reg<const BITSIZE: usize>(
    code: &mut CodeGenerator<'_>,
    op: BitOp,
    rd: XReg,
    rn: XReg,
    rm: XReg,
) -> Result<(), String> {
    let emission = match (BITSIZE, op) {
        (32, BitOp::And) => code.and(rd.to_w(), rn.to_w(), rm.to_w()),
        (64, BitOp::And) => code.and(rd, rn, rm),
        (32, BitOp::AndNot) => code.bic(rd.to_w(), rn.to_w(), rm.to_w()),
        (64, BitOp::AndNot) => code.bic(rd, rn, rm),
        (32, BitOp::Eor) => code.eor(rd.to_w(), rn.to_w(), rm.to_w()),
        (64, BitOp::Eor) => code.eor(rd, rn, rm),
        (32, BitOp::Or) => code.orr(rd.to_w(), rn.to_w(), rm.to_w()),
        (64, BitOp::Or) => code.orr(rd, rn, rm),
        _ => unreachable!(),
    };
    emission?;
    Ok(())
}

fn emit_bit_op_reg_flags<const BITSIZE: usize>(
    code: &mut CodeGenerator<'_>,
    op: BitOp,
    rd: XReg,
    rn: XReg,
    rm: XReg,
) -> Result<(), String> {
    let emission = match (BITSIZE, op) {
        (32, BitOp::And) => code.ands(rd.to_w(), rn.to_w(), rm.to_w()),
        (64, BitOp::And) => code.ands(rd, rn, rm),
        (32, BitOp::AndNot) => code.bics(rd.to_w(), rn.to_w(), rm.to_w()),
        (64, BitOp::AndNot) => code.bics(rd, rn, rm),
        _ => unreachable!(),
    };
    emission?;
    Ok(())
}

fn emit_not<const BITSIZE: usize>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    debug_assert!(BITSIZE == 32 || BITSIZE == 64);

    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    if BITSIZE == 32 {
        let mut result = ctx.reg_alloc.write_w(inst_ref);
        let mut operand = ctx.reg_alloc.read_w(args[0]);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
        code.mvn(result.w(), operand.w())?;
    } else {
        let mut result = ctx.reg_alloc.write_x(inst_ref);
        let mut operand = ctx.reg_alloc.read_x(args[0]);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
        code.mvn(result.x(), operand.x())?;
    }
    Ok(())
}

fn emit_sign_extend<const RESULT_BITSIZE: usize, const SOURCE_BITSIZE: usize>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    debug_assert!(RESULT_BITSIZE == 32 || RESULT_BITSIZE == 64);
    debug_assert!(matches!(SOURCE_BITSIZE, 8 | 16 | 32));

    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    if RESULT_BITSIZE == 32 {
        let mut result = ctx.reg_alloc.write_w(inst_ref);
        let mut operand = ctx.reg_alloc.read_w(args[0]);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;

        let emission = match SOURCE_BITSIZE {
            8 => code.sxtb(result.w(), operand.w()),
            16 => code.sxth(result.w(), operand.w()),
            _ => {
                return Err(format!(
                    "ARM64 sign extend {SOURCE_BITSIZE}->32 unsupported"
                ))
            }
        };
        emission?;
    } else {
        let mut result = ctx.reg_alloc.write_x(inst_ref);
        let mut operand = ctx.reg_alloc.read_x(args[0]);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;

        let emission = match SOURCE_BITSIZE {
            8 => code.sxtb(result.x(), operand.w()),
            16 => code.sxth(result.x(), operand.w()),
            32 => code.sxtw(result.x(), operand.w()),
            _ => {
                return Err(format!(
                    "ARM64 sign extend {SOURCE_BITSIZE}->64 unsupported"
                ))
            }
        };
        emission?;
    }
    Ok(())
}

fn associated_nz_or_nzcv(
    ctx: &EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<Option<InstRef>, String> {
    let nz_inst = ctx
        .block
        .get_associated_pseudo_operation(inst_ref, Opcode::GetNZFromOp);
    let nzcv_inst = ctx
        .block
        .get_associated_pseudo_operation(inst_ref, Opcode::GetNZCVFromOp);
    match (nz_inst, nzcv_inst) {
        (Some(_), Some(_)) => Err("ARM64 bit operation cannot have both NZ and NZCV".to_string()),
        (Some(inst), None) | (None, Some(inst)) => Ok(Some(inst)),
        (None, None) => Ok(None),
    }
}

fn emit_add_sub<const BITSIZE: usize, const SUB: bool>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    debug_assert!(BITSIZE == 32 || BITSIZE == 64);

    let overflow_inst = ctx
        .block
        .get_associated_pseudo_operation(inst_ref, Opcode::GetOverflowFromOp);

    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let nzcv_inst = ctx
        .block
        .get_associated_pseudo_operation(inst_ref, Opcode::GetNZCVFromOp);

    if let Some(overflow_inst) = overflow_inst {
        if SUB || nzcv_inst.is_some() || !args[2].is_immediate() || args[2].get_immediate_u1() {
            return Err(
                "ARM64 Add/Sub GetOverflowFromOp only supports upstream Add-without-carry form"
                    .to_string(),
            );
        }

        if BITSIZE == 32 {
            let mut result = ctx.reg_alloc.write_w(inst_ref);
            let mut lhs = ctx.reg_alloc.read_w(args[0]);
            let mut rhs = ctx.reg_alloc.read_w(args[1]);
            let mut overflow = ctx.reg_alloc.write_w(overflow_inst);
            ctx.reg_alloc.spill_flags(code)?;
            RegAlloc::realize_all(
                code,
                ctx.block,
                &mut [&mut result, &mut lhs, &mut rhs, &mut overflow],
            )?;
            code.adds(result.w(), lhs.w(), rhs.w())?;
            code.cinc(overflow.w(), WZR, Cond::VS)?;
        } else {
            let mut result = ctx.reg_alloc.write_x(inst_ref);
            let mut lhs = ctx.reg_alloc.read_x(args[0]);
            let mut rhs = ctx.reg_alloc.read_x(args[1]);
            let mut overflow = ctx.reg_alloc.write_w(overflow_inst);
            ctx.reg_alloc.spill_flags(code)?;
            RegAlloc::realize_all(
                code,
                ctx.block,
                &mut [&mut result, &mut lhs, &mut rhs, &mut overflow],
            )?;
            code.adds(result.x(), lhs.x(), rhs.x())?;
            code.cinc(overflow.w(), WZR, Cond::VS)?;
        }

        return Ok(());
    }

    if !args[2].is_immediate() {
        return emit_add_sub_dynamic_carry::<BITSIZE, SUB>(code, ctx, inst_ref, args, nzcv_inst);
    }

    let carry = args[2].get_immediate_u1();

    if args[1].is_immediate() {
        let imm = mask_add_sub_imm::<BITSIZE>(args[1].get_immediate_u64());
        if let Some(nzcv_inst) = nzcv_inst {
            if BITSIZE == 32 {
                let mut result = ctx.reg_alloc.write_w(inst_ref);
                let mut lhs = ctx.reg_alloc.read_w(args[0]);
                let mut flags = ctx.reg_alloc.write_flags(nzcv_inst);
                RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut lhs, &mut flags])?;
                emit_add_sub_imm_flags::<32, SUB>(code, result.x(), lhs.x(), imm, carry)
            } else {
                let mut result = ctx.reg_alloc.write_x(inst_ref);
                let mut lhs = ctx.reg_alloc.read_x(args[0]);
                let mut flags = ctx.reg_alloc.write_flags(nzcv_inst);
                RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut lhs, &mut flags])?;
                emit_add_sub_imm_flags::<64, SUB>(code, result.x(), lhs.x(), imm, carry)
            }
        } else if BITSIZE == 32 {
            let mut result = ctx.reg_alloc.write_w(inst_ref);
            let mut lhs = ctx.reg_alloc.read_w(args[0]);
            RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut lhs])?;
            emit_add_sub_imm::<32, SUB>(code, result.x(), lhs.x(), imm, carry)
        } else {
            let mut result = ctx.reg_alloc.write_x(inst_ref);
            let mut lhs = ctx.reg_alloc.read_x(args[0]);
            RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut lhs])?;
            emit_add_sub_imm::<64, SUB>(code, result.x(), lhs.x(), imm, carry)
        }
    } else if let Some(nzcv_inst) = nzcv_inst {
        if BITSIZE == 32 {
            let mut result = ctx.reg_alloc.write_w(inst_ref);
            let mut lhs = ctx.reg_alloc.read_w(args[0]);
            let mut rhs = ctx.reg_alloc.read_w(args[1]);
            let mut flags = ctx.reg_alloc.write_flags(nzcv_inst);
            RegAlloc::realize_all(
                code,
                ctx.block,
                &mut [&mut result, &mut lhs, &mut rhs, &mut flags],
            )?;
            emit_add_sub_reg_flags::<32, SUB>(code, result.x(), lhs.x(), rhs.x(), carry)
        } else {
            let mut result = ctx.reg_alloc.write_x(inst_ref);
            let mut lhs = ctx.reg_alloc.read_x(args[0]);
            let mut rhs = ctx.reg_alloc.read_x(args[1]);
            let mut flags = ctx.reg_alloc.write_flags(nzcv_inst);
            RegAlloc::realize_all(
                code,
                ctx.block,
                &mut [&mut result, &mut lhs, &mut rhs, &mut flags],
            )?;
            emit_add_sub_reg_flags::<64, SUB>(code, result.x(), lhs.x(), rhs.x(), carry)
        }
    } else if BITSIZE == 32 {
        let mut result = ctx.reg_alloc.write_w(inst_ref);
        let mut lhs = ctx.reg_alloc.read_w(args[0]);
        let mut rhs = ctx.reg_alloc.read_w(args[1]);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut lhs, &mut rhs])?;
        emit_add_sub_reg::<32, SUB>(code, result.x(), lhs.x(), rhs.x(), carry)
    } else {
        let mut result = ctx.reg_alloc.write_x(inst_ref);
        let mut lhs = ctx.reg_alloc.read_x(args[0]);
        let mut rhs = ctx.reg_alloc.read_x(args[1]);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut lhs, &mut rhs])?;
        emit_add_sub_reg::<64, SUB>(code, result.x(), lhs.x(), rhs.x(), carry)
    }
}

fn mask_add_sub_imm<const BITSIZE: usize>(imm: u64) -> u64 {
    if BITSIZE == 32 {
        u32::try_from(imm & u32::MAX as u64).unwrap() as u64
    } else {
        imm
    }
}

fn encode_add_sub_imm(imm: u64) -> Option<(u32, bool)> {
    if imm < 4096 {
        Some((imm as u32, false))
    } else if (imm & 0xfff) == 0 && (imm >> 12) < 4096 {
        Some(((imm >> 12) as u32, true))
    } else {
        None
    }
}

fn emit_add_sub_imm<const BITSIZE: usize, const SUB: bool>(
    code: &mut CodeGenerator<'_>,
    rd: XReg,
    rn: XReg,
    imm: u64,
    carry: bool,
) -> Result<(), String> {
    let adjusted = mask_add_sub_imm::<BITSIZE>(if carry {
        if SUB {
            imm
        } else {
            !imm
        }
    } else if SUB {
        !imm
    } else {
        imm
    });

    if let Some((imm12, shift12)) = encode_add_sub_imm(adjusted) {
        let emission = match (BITSIZE, SUB, carry) {
            (32, false, false) => {
                code.add_imm_shift(rd.to_w(), rn.to_w(), imm12, if shift12 { 12 } else { 0 })
            }
            (64, false, false) => code.add_imm_shift(rd, rn, imm12, if shift12 { 12 } else { 0 }),
            (32, true, true) => {
                code.sub_imm_shift(rd.to_w(), rn.to_w(), imm12, if shift12 { 12 } else { 0 })
            }
            (64, true, true) => code.sub_imm_shift(rd, rn, imm12, if shift12 { 12 } else { 0 }),
            (32, false, true) => {
                code.sub_imm_shift(rd.to_w(), rn.to_w(), imm12, if shift12 { 12 } else { 0 })
            }
            (64, false, true) => code.sub_imm_shift(rd, rn, imm12, if shift12 { 12 } else { 0 }),
            (32, true, false) => {
                code.add_imm_shift(rd.to_w(), rn.to_w(), imm12, if shift12 { 12 } else { 0 })
            }
            (64, true, false) => code.add_imm_shift(rd, rn, imm12, if shift12 { 12 } else { 0 }),
            _ => unreachable!(),
        };
        emission?;
    } else {
        if BITSIZE == 32 {
            emit_mov_w_imm(code, WSCRATCH0, adjusted as u32)?;
        } else {
            emit_mov_x_imm(code, XSCRATCH0, adjusted)?;
        }
        // MaybeAddSubImm materializes an already-adjusted operand; do not
        // complement it again through the unadjusted register-operand path.
        (match (BITSIZE, carry) {
            (32, false) => code.add(rd.to_w(), rn.to_w(), WSCRATCH0),
            (64, false) => code.add(rd, rn, XSCRATCH0),
            (32, true) => code.sub(rd.to_w(), rn.to_w(), WSCRATCH0),
            (64, true) => code.sub(rd, rn, XSCRATCH0),
            _ => unreachable!(),
        })?;
    }
    Ok(())
}

fn emit_add_sub_imm_flags<const BITSIZE: usize, const SUB: bool>(
    code: &mut CodeGenerator<'_>,
    rd: XReg,
    rn: XReg,
    imm: u64,
    carry: bool,
) -> Result<(), String> {
    let adjusted = mask_add_sub_imm::<BITSIZE>(if carry {
        if SUB {
            imm
        } else {
            !imm
        }
    } else if SUB {
        !imm
    } else {
        imm
    });

    if let Some((imm12, shift12)) = encode_add_sub_imm(adjusted) {
        let emission = match (BITSIZE, SUB, carry) {
            (32, false, false) => {
                code.adds_imm_shift(rd.to_w(), rn.to_w(), imm12, if shift12 { 12 } else { 0 })
            }
            (64, false, false) => code.adds_imm_shift(rd, rn, imm12, if shift12 { 12 } else { 0 }),
            (32, true, true) => {
                code.subs_imm_shift(rd.to_w(), rn.to_w(), imm12, if shift12 { 12 } else { 0 })
            }
            (64, true, true) => code.subs_imm_shift(rd, rn, imm12, if shift12 { 12 } else { 0 }),
            (32, false, true) => {
                code.subs_imm_shift(rd.to_w(), rn.to_w(), imm12, if shift12 { 12 } else { 0 })
            }
            (64, false, true) => code.subs_imm_shift(rd, rn, imm12, if shift12 { 12 } else { 0 }),
            (32, true, false) => {
                code.adds_imm_shift(rd.to_w(), rn.to_w(), imm12, if shift12 { 12 } else { 0 })
            }
            (64, true, false) => code.adds_imm_shift(rd, rn, imm12, if shift12 { 12 } else { 0 }),
            _ => unreachable!(),
        };
        emission?;
    } else {
        if BITSIZE == 32 {
            emit_mov_w_imm(code, WSCRATCH0, adjusted as u32)?;
        } else {
            emit_mov_x_imm(code, XSCRATCH0, adjusted)?;
        }
        (match (BITSIZE, carry) {
            (32, false) => code.adds(rd.to_w(), rn.to_w(), WSCRATCH0),
            (64, false) => code.adds(rd, rn, XSCRATCH0),
            (32, true) => code.subs(rd.to_w(), rn.to_w(), WSCRATCH0),
            (64, true) => code.subs(rd, rn, XSCRATCH0),
            _ => unreachable!(),
        })?;
    }
    Ok(())
}

fn emit_add_sub_dynamic_carry<const BITSIZE: usize, const SUB: bool>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    args: [Argument; MAX_ARGS],
    nzcv_inst: Option<InstRef>,
) -> Result<(), String> {
    if args[1].is_immediate() {
        let imm = mask_add_sub_imm::<BITSIZE>(args[1].get_immediate_u64());
        if BITSIZE == 32 {
            let mut result = ctx.reg_alloc.write_w(inst_ref);
            let mut lhs = ctx.reg_alloc.read_w(args[0]);
            RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut lhs])?;
            ctx.reg_alloc
                .read_write_flags(code, ctx.block, args[2], nzcv_inst)?;
            let rhs = if imm == 0 {
                WZR
            } else {
                emit_mov_w_imm(code, WSCRATCH0, imm as u32)?;
                WSCRATCH0
            };
            let emission = match (SUB, nzcv_inst.is_some()) {
                (false, false) => code.adc(result.w(), lhs.w(), rhs),
                (true, false) => code.sbc(result.w(), lhs.w(), rhs),
                (false, true) => code.adcs(result.w(), lhs.w(), rhs),
                (true, true) => code.sbcs(result.w(), lhs.w(), rhs),
            };
            emission?;
        } else {
            let mut result = ctx.reg_alloc.write_x(inst_ref);
            let mut lhs = ctx.reg_alloc.read_x(args[0]);
            RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut lhs])?;
            ctx.reg_alloc
                .read_write_flags(code, ctx.block, args[2], nzcv_inst)?;
            let rhs = if imm == 0 {
                rhazel::XZR
            } else {
                emit_mov_x_imm(code, XSCRATCH0, imm)?;
                XSCRATCH0
            };
            let emission = match (SUB, nzcv_inst.is_some()) {
                (false, false) => code.adc(result.x(), lhs.x(), rhs),
                (true, false) => code.sbc(result.x(), lhs.x(), rhs),
                (false, true) => code.adcs(result.x(), lhs.x(), rhs),
                (true, true) => code.sbcs(result.x(), lhs.x(), rhs),
            };
            emission?;
        }
        return Ok(());
    }

    if BITSIZE == 32 {
        let mut result = ctx.reg_alloc.write_w(inst_ref);
        let mut lhs = ctx.reg_alloc.read_w(args[0]);
        let mut rhs = ctx.reg_alloc.read_w(args[1]);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut lhs, &mut rhs])?;
        ctx.reg_alloc
            .read_write_flags(code, ctx.block, args[2], nzcv_inst)?;
        let emission = match (SUB, nzcv_inst.is_some()) {
            (false, false) => code.adc(result.w(), lhs.w(), rhs.w()),
            (true, false) => code.sbc(result.w(), lhs.w(), rhs.w()),
            (false, true) => code.adcs(result.w(), lhs.w(), rhs.w()),
            (true, true) => code.sbcs(result.w(), lhs.w(), rhs.w()),
        };
        emission?;
    } else {
        let mut result = ctx.reg_alloc.write_x(inst_ref);
        let mut lhs = ctx.reg_alloc.read_x(args[0]);
        let mut rhs = ctx.reg_alloc.read_x(args[1]);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut lhs, &mut rhs])?;
        ctx.reg_alloc
            .read_write_flags(code, ctx.block, args[2], nzcv_inst)?;
        let emission = match (SUB, nzcv_inst.is_some()) {
            (false, false) => code.adc(result.x(), lhs.x(), rhs.x()),
            (true, false) => code.sbc(result.x(), lhs.x(), rhs.x()),
            (false, true) => code.adcs(result.x(), lhs.x(), rhs.x()),
            (true, true) => code.sbcs(result.x(), lhs.x(), rhs.x()),
        };
        emission?;
    }
    Ok(())
}

fn emit_add_sub_reg<const BITSIZE: usize, const SUB: bool>(
    code: &mut CodeGenerator<'_>,
    rd: XReg,
    rn: XReg,
    rm: XReg,
    carry: bool,
) -> Result<(), String> {
    match (BITSIZE, SUB, carry) {
        (32, false, false) => {
            code.add(rd.to_w(), rn.to_w(), rm.to_w())?;
            Ok(())
        }
        (64, false, false) => {
            code.add(rd, rn, rm)?;
            Ok(())
        }
        (32, true, true) => {
            code.sub(rd.to_w(), rn.to_w(), rm.to_w())?;
            Ok(())
        }
        (64, true, true) => {
            code.sub(rd, rn, rm)?;
            Ok(())
        }
        (32, false, true) => {
            code.mvn(WSCRATCH0, rm.to_w())?;
            code.sub(rd.to_w(), rn.to_w(), WSCRATCH0)?;
            Ok(())
        }
        (64, false, true) => {
            code.mvn(XSCRATCH0, rm)?;
            code.sub(rd, rn, XSCRATCH0)?;
            Ok(())
        }
        (32, true, false) => {
            code.mvn(WSCRATCH0, rm.to_w())?;
            code.add(rd.to_w(), rn.to_w(), WSCRATCH0)?;
            Ok(())
        }
        (64, true, false) => {
            code.mvn(XSCRATCH0, rm)?;
            code.add(rd, rn, XSCRATCH0)?;
            Ok(())
        }
        _ => unreachable!(),
    }
}

fn emit_add_sub_reg_flags<const BITSIZE: usize, const SUB: bool>(
    code: &mut CodeGenerator<'_>,
    rd: XReg,
    rn: XReg,
    rm: XReg,
    carry: bool,
) -> Result<(), String> {
    match (BITSIZE, SUB, carry) {
        (32, false, false) => {
            code.adds(rd.to_w(), rn.to_w(), rm.to_w())?;
            Ok(())
        }
        (64, false, false) => {
            code.adds(rd, rn, rm)?;
            Ok(())
        }
        (32, true, true) => {
            code.subs(rd.to_w(), rn.to_w(), rm.to_w())?;
            Ok(())
        }
        (64, true, true) => {
            code.subs(rd, rn, rm)?;
            Ok(())
        }
        (32, false, true) => {
            code.mvn(WSCRATCH0, rm.to_w())?;
            code.subs(rd.to_w(), rn.to_w(), WSCRATCH0)?;
            Ok(())
        }
        (64, false, true) => {
            code.mvn(XSCRATCH0, rm)?;
            code.subs(rd, rn, XSCRATCH0)?;
            Ok(())
        }
        (32, true, false) => {
            code.mvn(WSCRATCH0, rm.to_w())?;
            code.adds(rd.to_w(), rn.to_w(), WSCRATCH0)?;
            Ok(())
        }
        (64, true, false) => {
            code.mvn(XSCRATCH0, rm)?;
            code.adds(rd, rn, XSCRATCH0)?;
            Ok(())
        }
        _ => unreachable!(),
    }
}

#[cfg(all(test, target_arch = "aarch64"))]
mod tests {
    use super::*;
    use crate::backend::arm64::inst;

    fn check_immediate_arithmetic<const BITS: usize, const SUB: bool>() {
        for carry in [false, true] {
            for flags in [false, true] {
                for imm in [0, 1, 16, 0x12345, 0x1234_5678, u64::MAX] {
                    let mut code_storage = BlockOfCode::with_size(4096).unwrap();
                    let mut code = rhazel::CodeGenerator::new(&mut code_storage);
                    if flags {
                        emit_add_sub_imm_flags::<BITS, SUB>(
                            &mut code,
                            rhazel::X0,
                            rhazel::X0,
                            imm,
                            carry,
                        )
                        .unwrap();
                    } else {
                        emit_add_sub_imm::<BITS, SUB>(
                            &mut code,
                            rhazel::X0,
                            rhazel::X0,
                            imm,
                            carry,
                        )
                        .unwrap();
                    }
                    code.write_u32(inst::mrs_nzcv(2)).unwrap();
                    code.write_u32(inst::str_w_unsigned(2, 1, 0)).unwrap();
                    code.write_u32(inst::ret_lr()).unwrap();
                    code.seal();
                    // Only caller-saved registers and NZCV are modified.
                    let run: unsafe extern "C" fn(u64, *mut u32) -> u64 =
                        unsafe { std::mem::transmute(code.code_base_ptr()) };
                    for lhs in [
                        0u64,
                        1,
                        0x7fff_ffff,
                        0x8000_0000,
                        0x7fff_ffff_ffff_ffff,
                        0x8000_0000_0000_0000,
                        u64::MAX,
                    ] {
                        let mut actual_flags = 0;
                        let expected = if SUB {
                            lhs.wrapping_sub(imm).wrapping_sub(u64::from(!carry))
                        } else {
                            lhs.wrapping_add(imm).wrapping_add(u64::from(carry))
                        };
                        assert_eq!(
                            unsafe { run(lhs, &mut actual_flags) },
                            mask_add_sub_imm::<BITS>(expected),
                            "bits={BITS} sub={SUB} carry={carry} flags={flags} imm={imm:#x} lhs={lhs:#x}"
                        );
                        if flags {
                            let a = mask_add_sub_imm::<BITS>(lhs);
                            let b = mask_add_sub_imm::<BITS>(if SUB { !imm } else { imm });
                            let sum = a as u128 + b as u128 + u128::from(carry);
                            let result = mask_add_sub_imm::<BITS>(expected);
                            let sign = 1u64 << (BITS - 1);
                            let expected_flags = (u32::from(result & sign != 0) << 31)
                                | (u32::from(result == 0) << 30)
                                | (u32::from(sum >> BITS != 0) << 29)
                                | (u32::from((!(a ^ b) & (a ^ result) & sign) != 0) << 28);
                            assert_eq!(actual_flags, expected_flags);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn immediate_arithmetic_preserves_carry_when_materializing_constant() {
        check_immediate_arithmetic::<32, false>();
        check_immediate_arithmetic::<32, true>();
        check_immediate_arithmetic::<64, false>();
        check_immediate_arithmetic::<64, true>();
    }
}
