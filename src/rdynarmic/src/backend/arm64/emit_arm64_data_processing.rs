//! ARM64 data-processing emitters.
//!
//! Upstream owner: `backend/arm64/emit_arm64_data_processing.cpp`.

use crate::backend::arm64::abi::{XSCRATCH0, XSCRATCH1, XSTATE};
use crate::backend::arm64::block_of_code::BlockOfCode;
use crate::backend::arm64::emit_context::EmitContext;
use crate::backend::arm64::inst;
use crate::backend::arm64::label::Label;
use crate::backend::arm64::reg_alloc::{Argument, RegAlloc};
use crate::ir::cond::Cond;
use crate::ir::inst::MAX_ARGS;
use crate::ir::opcode::Opcode;
use crate::ir::types::Type;
use crate::ir::value::InstRef;

pub fn emit_pack_2x32_to_1x64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);

    let mut lo = ctx.reg_alloc.read_w(args[0]);
    let mut hi = ctx.reg_alloc.read_w(args[1]);
    let mut result = ctx.reg_alloc.write_x(inst_ref);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut lo, &mut hi, &mut result])?;

    let lo = lo.index().expect("realized W lo") as u8;
    let hi = hi.index().expect("realized W hi") as u8;
    let result = result.index().expect("realized X result") as u8;
    code.write_u32(inst::mov_w(result, lo))?;
    code.write_u32(inst::bfi_x(result, hi, 32, 32))?;
    Ok(())
}

pub fn emit_pack_2x64_to_1x128(
    code: &mut BlockOfCode,
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

            let lo = lo.index().expect("realized X lo") as u8;
            let hi = hi.index().expect("realized X hi") as u8;
            let result = result.index().expect("realized Q result") as u8;
            code.write_u32(inst::fmov_d_from_x(result, lo))?;
            code.write_u32(inst::fmov_v_d1_from_x(result, hi))?;
        }
        (true, false) => {
            let mut lo = ctx.reg_alloc.read_x(args[0]);
            let mut hi = ctx.reg_alloc.read_d(args[1]);
            let mut result = ctx.reg_alloc.write_q(inst_ref);
            RegAlloc::realize_all(code, ctx.block, &mut [&mut lo, &mut hi, &mut result])?;

            let lo = lo.index().expect("realized X lo") as u8;
            let hi = hi.index().expect("realized D hi") as u8;
            let result = result.index().expect("realized Q result") as u8;
            code.write_u32(inst::fmov_d_from_x(result, lo))?;
            code.write_u32(inst::mov_v_d1_from_v_d0(result, hi))?;
        }
        (false, true) => {
            let mut lo = ctx.reg_alloc.read_d(args[0]);
            let mut hi = ctx.reg_alloc.read_x(args[1]);
            let mut result = ctx.reg_alloc.write_q(inst_ref);
            RegAlloc::realize_all(code, ctx.block, &mut [&mut lo, &mut hi, &mut result])?;

            let lo = lo.index().expect("realized D lo") as u8;
            let hi = hi.index().expect("realized X hi") as u8;
            let result = result.index().expect("realized Q result") as u8;
            code.write_u32(inst::fmov_d(result, lo))?;
            code.write_u32(inst::fmov_v_d1_from_x(result, hi))?;
        }
        (false, false) => {
            let mut lo = ctx.reg_alloc.read_d(args[0]);
            let mut hi = ctx.reg_alloc.read_d(args[1]);
            let mut result = ctx.reg_alloc.write_q(inst_ref);
            RegAlloc::realize_all(code, ctx.block, &mut [&mut lo, &mut hi, &mut result])?;

            let lo = lo.index().expect("realized D lo") as u8;
            let hi = hi.index().expect("realized D hi") as u8;
            let result = result.index().expect("realized Q result") as u8;
            code.write_u32(inst::fmov_d(result, lo))?;
            code.write_u32(inst::mov_v_d1_from_v_d0(result, hi))?;
        }
    }
    Ok(())
}

pub fn emit_extract_register32(
    code: &mut BlockOfCode,
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

    let result = result.index().expect("realized W result") as u8;
    let op1 = op1.index().expect("realized W op1") as u8;
    let op2 = op2.index().expect("realized W op2") as u8;
    code.write_u32(inst::extr_w(result, op2, op1, args[2].get_immediate_u8()))?;
    Ok(())
}

pub fn emit_extract_register64(
    code: &mut BlockOfCode,
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

    let result = result.index().expect("realized X result") as u8;
    let op1 = op1.index().expect("realized X op1") as u8;
    let op2 = op2.index().expect("realized X op2") as u8;
    code.write_u32(inst::extr_x(result, op2, op1, args[2].get_immediate_u8()))?;
    Ok(())
}

pub fn emit_least_significant_word(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut operand = ctx.reg_alloc.read_x(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    code.write_u32(inst::mov_w(
        result.index().expect("realized W result") as u8,
        operand.index().expect("realized X operand") as u8,
    ))?;
    Ok(())
}

pub fn emit_most_significant_word(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let carry_inst = ctx
        .block
        .get_associated_pseudo_operation(inst_ref, Opcode::GetCarryFromOp);
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);

    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut operand = ctx.reg_alloc.read_x(args[0]);
    if let Some(carry_inst) = carry_inst {
        let mut carry = ctx.reg_alloc.write_w(carry_inst);
        RegAlloc::realize_all(
            code,
            ctx.block,
            &mut [&mut result, &mut operand, &mut carry],
        )?;
        let result = result.index().expect("realized W result") as u8;
        let operand = operand.index().expect("realized X operand") as u8;
        let carry = carry.index().expect("realized W carry") as u8;
        code.write_u32(inst::lsr_x_imm(result, operand, 32))?;
        code.write_u32(inst::lsr_w_imm(carry, operand, 31 - 29))?;
        code.write_u32(inst::and_w_imm(carry, carry, 1 << 29))?;
    } else {
        RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
        code.write_u32(inst::lsr_x_imm(
            result.index().expect("realized W result") as u8,
            operand.index().expect("realized X operand") as u8,
            32,
        ))?;
    }
    Ok(())
}

pub fn emit_least_significant_half(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut operand = ctx.reg_alloc.read_w(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    code.write_u32(inst::ubfx_w(
        result.index().expect("realized W result") as u8,
        operand.index().expect("realized W operand") as u8,
        0,
        16,
    ))?;
    Ok(())
}

pub fn emit_least_significant_byte(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut operand = ctx.reg_alloc.read_w(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    code.write_u32(inst::ubfx_w(
        result.index().expect("realized W result") as u8,
        operand.index().expect("realized W operand") as u8,
        0,
        8,
    ))?;
    Ok(())
}

pub fn emit_test_bit(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let bit = args[1].get_immediate_u8();
    debug_assert!(bit < 64);

    let mut result = ctx.reg_alloc.write_x(inst_ref);
    let mut operand = ctx.reg_alloc.read_x(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;

    code.write_u32(inst::ubfx_x(
        result.index().expect("realized X result") as u8,
        operand.index().expect("realized X operand") as u8,
        bit,
        1,
    ))?;
    Ok(())
}

pub fn emit_conditional_select32(
    code: &mut BlockOfCode,
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

    let result = result.index().expect("realized W result") as u8;
    let then_value = then_value.index().expect("realized W then") as u8;
    let else_value = else_value.index().expect("realized W else") as u8;
    code.write_u32(inst::ldr_w_unsigned(
        XSCRATCH0,
        XSTATE,
        ctx.conf.state_nzcv_offset as u32,
    ))?;
    code.write_u32(inst::msr_nzcv(XSCRATCH0))?;
    code.write_u32(inst::csel_w(result, then_value, else_value, cond))?;
    Ok(())
}

pub fn emit_conditional_select64(
    code: &mut BlockOfCode,
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

    let result = result.index().expect("realized X result") as u8;
    let then_value = then_value.index().expect("realized X then") as u8;
    let else_value = else_value.index().expect("realized X else") as u8;
    code.write_u32(inst::ldr_w_unsigned(
        XSCRATCH0,
        XSTATE,
        ctx.conf.state_nzcv_offset as u32,
    ))?;
    code.write_u32(inst::msr_nzcv(XSCRATCH0))?;
    code.write_u32(inst::csel_x(result, then_value, else_value, cond))?;
    Ok(())
}

pub fn emit_and32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_bit_op::<32>(code, ctx, inst_ref, BitOp::And)
}

pub fn emit_and64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_bit_op::<64>(code, ctx, inst_ref, BitOp::And)
}

pub fn emit_and_not32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_bit_op::<32>(code, ctx, inst_ref, BitOp::AndNot)
}

pub fn emit_and_not64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_bit_op::<64>(code, ctx, inst_ref, BitOp::AndNot)
}

pub fn emit_eor32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_bit_op::<32>(code, ctx, inst_ref, BitOp::Eor)
}

pub fn emit_eor64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_bit_op::<64>(code, ctx, inst_ref, BitOp::Eor)
}

pub fn emit_or32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_bit_op::<32>(code, ctx, inst_ref, BitOp::Or)
}

pub fn emit_or64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_bit_op::<64>(code, ctx, inst_ref, BitOp::Or)
}

pub fn emit_not32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_not::<32>(code, ctx, inst_ref)
}

pub fn emit_not64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_not::<64>(code, ctx, inst_ref)
}

pub fn emit_sign_extend_byte_to_word(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_sign_extend::<32, 8>(code, ctx, inst_ref)
}

pub fn emit_sign_extend_half_to_word(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_sign_extend::<32, 16>(code, ctx, inst_ref)
}

pub fn emit_sign_extend_byte_to_long(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_sign_extend::<64, 8>(code, ctx, inst_ref)
}

pub fn emit_sign_extend_half_to_long(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_sign_extend::<64, 16>(code, ctx, inst_ref)
}

pub fn emit_sign_extend_word_to_long(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_sign_extend::<64, 32>(code, ctx, inst_ref)
}

pub fn emit_zero_extend(
    _code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    ctx.reg_alloc
        .define_as_existing(ctx.block, inst_ref, args[0]);
    Ok(())
}

pub fn emit_zero_extend_long_to_quad(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut value = ctx.reg_alloc.read_x(args[0]);
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut value, &mut result])?;

    code.write_u32(inst::fmov_d_from_x(
        result.index().expect("realized Q result") as u8,
        value.index().expect("realized X value") as u8,
    ))?;
    Ok(())
}

pub fn emit_logical_shift_left32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_shift32(code, ctx, inst_ref, ShiftOp::LogicalLeft)
}

pub fn emit_logical_shift_left64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_shift64(code, ctx, inst_ref, ShiftOp::LogicalLeft)
}

pub fn emit_logical_shift_right32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_shift32(code, ctx, inst_ref, ShiftOp::LogicalRight)
}

pub fn emit_logical_shift_right64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_shift64(code, ctx, inst_ref, ShiftOp::LogicalRight)
}

pub fn emit_arithmetic_shift_right32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_shift32(code, ctx, inst_ref, ShiftOp::ArithmeticRight)
}

pub fn emit_arithmetic_shift_right64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_shift64(code, ctx, inst_ref, ShiftOp::ArithmeticRight)
}

pub fn emit_rotate_right32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_shift32(code, ctx, inst_ref, ShiftOp::RotateRight)
}

pub fn emit_rotate_right_extended(
    code: &mut BlockOfCode,
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

        let result = result.index().expect("realized W result") as u8;
        let operand = operand.index().expect("realized W operand") as u8;
        code.write_u32(inst::lsr_w_imm(result, operand, 1))?;
        if args[1].get_immediate_u1() {
            code.write_u32(inst::orr_w_imm(result, result, 0x8000_0000))?;
        }
        if let Some(carry_out) = carry_out {
            let carry_out = carry_out.index().expect("realized W carry") as u8;
            code.write_u32(inst::and_w_imm(carry_out, operand, 1))?;
            code.write_u32(inst::lsl_w_imm(carry_out, carry_out, 29))?;
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

    let result = result.index().expect("realized W result") as u8;
    let operand = operand.index().expect("realized W operand") as u8;
    let carry_in = carry_in.index().expect("realized W carry_in") as u8;
    code.write_u32(inst::lsr_w_imm(XSCRATCH0, carry_in, 29))?;
    code.write_u32(inst::extr_w(result, XSCRATCH0, operand, 1))?;
    if let Some(carry_out) = carry_out {
        let carry_out = carry_out.index().expect("realized W carry") as u8;
        code.write_u32(inst::and_w_imm(carry_out, operand, 1))?;
        code.write_u32(inst::lsl_w_imm(carry_out, carry_out, 29))?;
    }
    Ok(())
}

pub fn emit_rotate_right64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_shift64(code, ctx, inst_ref, ShiftOp::RotateRight)
}

pub fn emit_add32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_add_sub::<32, false>(code, ctx, inst_ref)
}

pub fn emit_add64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_add_sub::<64, false>(code, ctx, inst_ref)
}

pub fn emit_sub32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_add_sub::<32, true>(code, ctx, inst_ref)
}

pub fn emit_sub64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_add_sub::<64, true>(code, ctx, inst_ref)
}

pub fn emit_mul32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_mul::<32>(code, ctx, inst_ref)
}

pub fn emit_mul64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_mul::<64>(code, ctx, inst_ref)
}

pub fn emit_signed_multiply_high64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_multiply_high64::<true>(code, ctx, inst_ref)
}

pub fn emit_unsigned_multiply_high64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_multiply_high64::<false>(code, ctx, inst_ref)
}

pub fn emit_unsigned_div32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_div::<32, false>(code, ctx, inst_ref)
}

pub fn emit_unsigned_div64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_div::<64, false>(code, ctx, inst_ref)
}

pub fn emit_signed_div32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_div::<32, true>(code, ctx, inst_ref)
}

pub fn emit_signed_div64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_div::<64, true>(code, ctx, inst_ref)
}

fn emit_max_min32(
    code: &mut BlockOfCode,
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

    let result = result.index().expect("realized W result") as u8;
    let op1 = op1.index().expect("realized W op1") as u8;
    let op2 = op2.index().expect("realized W op2") as u8;
    code.write_u32(inst::cmp_w_reg(op1, op2))?;
    code.write_u32(inst::csel_w(result, op1, op2, cond))?;
    Ok(())
}

fn emit_max_min64(
    code: &mut BlockOfCode,
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

    let result = result.index().expect("realized X result") as u8;
    let op1 = op1.index().expect("realized X op1") as u8;
    let op2 = op2.index().expect("realized X op2") as u8;
    code.write_u32(inst::cmp_x_reg(op1, op2))?;
    code.write_u32(inst::csel_x(result, op1, op2, cond))?;
    Ok(())
}

pub fn emit_max_signed32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_max_min32(code, ctx, inst_ref, Cond::GT)
}

pub fn emit_max_signed64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_max_min64(code, ctx, inst_ref, Cond::GT)
}

pub fn emit_max_unsigned32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_max_min32(code, ctx, inst_ref, Cond::HI)
}

pub fn emit_max_unsigned64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_max_min64(code, ctx, inst_ref, Cond::HI)
}

pub fn emit_min_signed32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_max_min32(code, ctx, inst_ref, Cond::LT)
}

pub fn emit_min_signed64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_max_min64(code, ctx, inst_ref, Cond::LT)
}

pub fn emit_min_unsigned32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_max_min32(code, ctx, inst_ref, Cond::LO)
}

pub fn emit_min_unsigned64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_max_min64(code, ctx, inst_ref, Cond::LO)
}

pub fn emit_logical_shift_left_masked32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_shift_masked32(code, ctx, inst_ref, ShiftOp::LogicalLeft)
}

pub fn emit_logical_shift_left_masked64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_shift_masked64(code, ctx, inst_ref, ShiftOp::LogicalLeft)
}

pub fn emit_logical_shift_right_masked32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_shift_masked32(code, ctx, inst_ref, ShiftOp::LogicalRight)
}

pub fn emit_logical_shift_right_masked64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_shift_masked64(code, ctx, inst_ref, ShiftOp::LogicalRight)
}

pub fn emit_arithmetic_shift_right_masked32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_shift_masked32(code, ctx, inst_ref, ShiftOp::ArithmeticRight)
}

pub fn emit_arithmetic_shift_right_masked64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_shift_masked64(code, ctx, inst_ref, ShiftOp::ArithmeticRight)
}

pub fn emit_rotate_right_masked32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_shift_masked32(code, ctx, inst_ref, ShiftOp::RotateRight)
}

pub fn emit_rotate_right_masked64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_shift_masked64(code, ctx, inst_ref, ShiftOp::RotateRight)
}

pub fn emit_count_leading_zeros32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_count_leading_zeros::<32>(code, ctx, inst_ref)
}

pub fn emit_count_leading_zeros64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_count_leading_zeros::<64>(code, ctx, inst_ref)
}

pub fn emit_byte_reverse_word(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut operand = ctx.reg_alloc.read_w(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    code.write_u32(inst::rev_w(
        result.index().expect("realized W result") as u8,
        operand.index().expect("realized W operand") as u8,
    ))?;
    Ok(())
}

pub fn emit_byte_reverse_half(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut operand = ctx.reg_alloc.read_w(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    code.write_u32(inst::rev16_w(
        result.index().expect("realized W result") as u8,
        operand.index().expect("realized W operand") as u8,
    ))?;
    Ok(())
}

pub fn emit_byte_reverse_dual(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_x(inst_ref);
    let mut operand = ctx.reg_alloc.read_x(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    code.write_u32(inst::rev_x(
        result.index().expect("realized X result") as u8,
        operand.index().expect("realized X operand") as u8,
    ))?;
    Ok(())
}

pub fn emit_replicate_bit32(
    code: &mut BlockOfCode,
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

    let result = result.index().expect("realized W result") as u8;
    let value = value.index().expect("realized W value") as u8;
    let bit = args[1].get_immediate_u8();
    code.write_u32(inst::lsl_w_imm(result, value, 31 - bit))?;
    code.write_u32(inst::asr_w_imm(result, result, 31))?;
    Ok(())
}

pub fn emit_replicate_bit64(
    code: &mut BlockOfCode,
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

    let result = result.index().expect("realized X result") as u8;
    let value = value.index().expect("realized X value") as u8;
    let bit = args[1].get_immediate_u8();
    code.write_u32(inst::lsl_x_imm(result, value, 63 - bit))?;
    code.write_u32(inst::asr_x_imm(result, result, 63))?;
    Ok(())
}

pub fn emit_get_nzcv_from_op(
    code: &mut BlockOfCode,
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
            let value = value.index().expect("realized W value") as u8;
            code.write_u32(inst::and_w_imm(XSCRATCH0, value, mask))?;
            code.write_u32(inst::tst_w_reg(XSCRATCH0, XSCRATCH0))?;
            Ok(())
        }
        Type::U32 => {
            let mut value = ctx.reg_alloc.read_w(args[0]);
            let mut flags = ctx.reg_alloc.write_flags(inst_ref);
            RegAlloc::realize_all(code, ctx.block, &mut [&mut value, &mut flags])?;
            let value = value.index().expect("realized W value") as u8;
            code.write_u32(inst::tst_w_reg(value, value))?;
            Ok(())
        }
        Type::U64 => {
            let mut value = ctx.reg_alloc.read_x(args[0]);
            let mut flags = ctx.reg_alloc.write_flags(inst_ref);
            RegAlloc::realize_all(code, ctx.block, &mut [&mut value, &mut flags])?;
            let value = value.index().expect("realized X value") as u8;
            code.write_u32(inst::tst_x_reg(value, value))?;
            Ok(())
        }
        ty => Err(format!("ARM64 GetNZCVFromOp unsupported input type {ty:?}")),
    }
}

fn emit_mul<const BITSIZE: usize>(
    code: &mut BlockOfCode,
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
        code.write_u32(inst::mul_w(
            result.index().expect("realized W result") as u8,
            lhs.index().expect("realized W lhs") as u8,
            rhs.index().expect("realized W rhs") as u8,
        ))?;
    } else {
        let mut result = ctx.reg_alloc.write_x(inst_ref);
        let mut lhs = ctx.reg_alloc.read_x(args[0]);
        let mut rhs = ctx.reg_alloc.read_x(args[1]);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut lhs, &mut rhs])?;
        code.write_u32(inst::mul_x(
            result.index().expect("realized X result") as u8,
            lhs.index().expect("realized X lhs") as u8,
            rhs.index().expect("realized X rhs") as u8,
        ))?;
    }
    Ok(())
}

fn emit_multiply_high64<const SIGNED: bool>(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_x(inst_ref);
    let mut op1 = ctx.reg_alloc.read_x(args[0]);
    let mut op2 = ctx.reg_alloc.read_x(args[1]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut op1, &mut op2])?;

    let result = result.index().expect("realized X result") as u8;
    let op1 = op1.index().expect("realized X op1") as u8;
    let op2 = op2.index().expect("realized X op2") as u8;
    let word = if SIGNED {
        inst::smulh_x(result, op1, op2)
    } else {
        inst::umulh_x(result, op1, op2)
    };
    code.write_u32(word)?;
    Ok(())
}

fn emit_div<const BITSIZE: usize, const SIGNED: bool>(
    code: &mut BlockOfCode,
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
        let result = result.index().expect("realized W result") as u8;
        let lhs = lhs.index().expect("realized W lhs") as u8;
        let rhs = rhs.index().expect("realized W rhs") as u8;
        code.write_u32(if SIGNED {
            inst::sdiv_w(result, lhs, rhs)
        } else {
            inst::udiv_w(result, lhs, rhs)
        })?;
    } else {
        let mut result = ctx.reg_alloc.write_x(inst_ref);
        let mut lhs = ctx.reg_alloc.read_x(args[0]);
        let mut rhs = ctx.reg_alloc.read_x(args[1]);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut lhs, &mut rhs])?;
        let result = result.index().expect("realized X result") as u8;
        let lhs = lhs.index().expect("realized X lhs") as u8;
        let rhs = rhs.index().expect("realized X rhs") as u8;
        code.write_u32(if SIGNED {
            inst::sdiv_x(result, lhs, rhs)
        } else {
            inst::udiv_x(result, lhs, rhs)
        })?;
    }
    Ok(())
}

fn emit_count_leading_zeros<const BITSIZE: usize>(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    debug_assert!(BITSIZE == 32 || BITSIZE == 64);

    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    if BITSIZE == 32 {
        let mut result = ctx.reg_alloc.write_w(inst_ref);
        let mut operand = ctx.reg_alloc.read_w(args[0]);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
        code.write_u32(inst::clz_w(
            result.index().expect("realized W result") as u8,
            operand.index().expect("realized W operand") as u8,
        ))?;
    } else {
        let mut result = ctx.reg_alloc.write_x(inst_ref);
        let mut operand = ctx.reg_alloc.read_x(args[0]);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
        code.write_u32(inst::clz_x(
            result.index().expect("realized X result") as u8,
            operand.index().expect("realized X operand") as u8,
        ))?;
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
    code: &mut BlockOfCode,
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
        let result = result.index().expect("realized W result") as u8;
        let operand = operand.index().expect("realized W operand") as u8;
        match op {
            ShiftOp::LogicalLeft if shift <= 31 => {
                code.write_u32(inst::lsl_w_imm(result, operand, shift))?;
            }
            ShiftOp::LogicalRight if shift <= 31 => {
                code.write_u32(inst::lsr_w_imm(result, operand, shift))?;
            }
            ShiftOp::LogicalLeft | ShiftOp::LogicalRight => {
                code.write_u32(inst::mov_w(result, 31))?;
            }
            ShiftOp::ArithmeticRight => {
                code.write_u32(inst::asr_w_imm(result, operand, shift.min(31)))?;
            }
            ShiftOp::RotateRight => {
                code.write_u32(inst::ror_w_imm(result, operand, shift % 32))?;
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

    let result = result.index().expect("realized W result") as u8;
    let operand = operand.index().expect("realized W operand") as u8;
    let shift = shift.index().expect("realized W shift") as u8;
    match op {
        ShiftOp::LogicalLeft | ShiftOp::LogicalRight => {
            code.write_u32(inst::and_w_imm(XSCRATCH0, shift, 0xff))?;
            let shift_reg = XSCRATCH0;
            let word = match op {
                ShiftOp::LogicalLeft => inst::lslv_w(result, operand, shift_reg),
                ShiftOp::LogicalRight => inst::lsrv_w(result, operand, shift_reg),
                _ => unreachable!(),
            };
            code.write_u32(word)?;
            code.write_u32(inst::cmp_w_imm(shift_reg, 32))?;
            code.write_u32(inst::csel_w(result, result, 31, Cond::LT))?;
        }
        ShiftOp::ArithmeticRight => {
            code.write_u32(inst::and_w_imm(XSCRATCH0, shift, 0xff))?;
            code.write_u32(inst::movz_w(XSCRATCH1, 31, 0))?;
            code.write_u32(inst::cmp_w_imm(XSCRATCH0, 31))?;
            code.write_u32(inst::csel_w(XSCRATCH0, XSCRATCH0, XSCRATCH1, Cond::LS))?;
            code.write_u32(inst::asrv_w(result, operand, XSCRATCH0))?;
        }
        ShiftOp::RotateRight => {
            code.write_u32(inst::rorv_w(result, operand, shift))?;
        }
    }
    Ok(())
}

fn emit_shift32_with_carry(
    code: &mut BlockOfCode,
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
    code: &mut BlockOfCode,
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
            let result = result.index().expect("realized W result") as u8;
            let carry_out = carry_out.index().expect("realized W carry") as u8;
            let operand = operand.index().expect("realized W operand") as u8;
            code.write_u32(inst::ubfx_w(carry_out, operand, 32 - shift, 1))?;
            code.write_u32(inst::lsl_w_imm(carry_out, carry_out, 29))?;
            code.write_u32(inst::lsl_w_imm(result, operand, shift))?;
        } else if shift > 32 {
            let mut result = ctx.reg_alloc.write_w(inst_ref);
            let mut carry_out = ctx.reg_alloc.write_w(carry_inst);
            RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut carry_out])?;
            code.write_u32(inst::mov_w(
                result.index().expect("realized W result") as u8,
                31,
            ))?;
            code.write_u32(inst::mov_w(
                carry_out.index().expect("realized W carry") as u8,
                31,
            ))?;
        } else {
            let mut result = ctx.reg_alloc.write_w(inst_ref);
            let mut carry_out = ctx.reg_alloc.write_w(carry_inst);
            let mut operand = ctx.reg_alloc.read_w(operand_arg);
            RegAlloc::realize_all(
                code,
                ctx.block,
                &mut [&mut result, &mut carry_out, &mut operand],
            )?;
            let result = result.index().expect("realized W result") as u8;
            let carry_out = carry_out.index().expect("realized W carry") as u8;
            let operand = operand.index().expect("realized W operand") as u8;
            code.write_u32(inst::ubfiz_w(carry_out, operand, 29, 1))?;
            code.write_u32(inst::mov_w(result, 31))?;
        }
        return Ok(());
    }

    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut carry_out = ctx.reg_alloc.write_w(carry_inst);
    let mut operand = ctx.reg_alloc.read_w(operand_arg);
    let mut shift = ctx.reg_alloc.read_w(shift_arg);
    let carry_in_reg;
    if carry_arg.is_immediate() {
        RegAlloc::realize_all(
            code,
            ctx.block,
            &mut [&mut result, &mut carry_out, &mut operand, &mut shift],
        )?;
        carry_in_reg = None;
    } else {
        let mut carry_in = ctx.reg_alloc.read_w(carry_arg);
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
        carry_in_reg = Some(carry_in.index().expect("realized W carry in") as u8);
    }
    ctx.reg_alloc.spill_flags(code)?;

    let result = result.index().expect("realized W result") as u8;
    let carry_out = carry_out.index().expect("realized W carry") as u8;
    let operand = operand.index().expect("realized W operand") as u8;
    let shift = shift.index().expect("realized W shift") as u8;
    let mut zero = Label::new();
    let mut end = Label::new();

    code.write_u32(inst::ands_w_imm(XSCRATCH1, shift, 0xff))?;
    zero.b_cond(code, Cond::EQ)?;
    code.write_u32(inst::neg_w(XSCRATCH0, shift))?;
    code.write_u32(inst::lsrv_w(carry_out, operand, XSCRATCH0))?;
    code.write_u32(inst::lslv_w(result, operand, shift))?;
    code.write_u32(inst::ubfiz_w(carry_out, carry_out, 29, 1))?;
    code.write_u32(inst::cmp_w_imm(XSCRATCH1, 32))?;
    code.write_u32(inst::csel_w(result, result, 31, Cond::LT))?;
    code.write_u32(inst::csel_w(carry_out, carry_out, 31, Cond::LE))?;
    end.b(code)?;
    zero.bind(code)?;
    code.write_u32(inst::mov_w(result, operand))?;
    emit_carry_input_to_reg(code, carry_arg, carry_in_reg, carry_out)?;
    end.bind(code)?;
    Ok(())
}

fn emit_logical_shift_right32_with_carry(
    code: &mut BlockOfCode,
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
            let result = result.index().expect("realized W result") as u8;
            let carry_out = carry_out.index().expect("realized W carry") as u8;
            let operand = operand.index().expect("realized W operand") as u8;
            code.write_u32(inst::ubfx_w(carry_out, operand, shift - 1, 1))?;
            code.write_u32(inst::lsl_w_imm(carry_out, carry_out, 29))?;
            code.write_u32(inst::lsr_w_imm(result, operand, shift))?;
        } else if shift > 32 {
            let mut result = ctx.reg_alloc.write_w(inst_ref);
            let mut carry_out = ctx.reg_alloc.write_w(carry_inst);
            RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut carry_out])?;
            code.write_u32(inst::mov_w(
                result.index().expect("realized W result") as u8,
                31,
            ))?;
            code.write_u32(inst::mov_w(
                carry_out.index().expect("realized W carry") as u8,
                31,
            ))?;
        } else {
            let mut result = ctx.reg_alloc.write_w(inst_ref);
            let mut carry_out = ctx.reg_alloc.write_w(carry_inst);
            let mut operand = ctx.reg_alloc.read_w(operand_arg);
            RegAlloc::realize_all(
                code,
                ctx.block,
                &mut [&mut result, &mut carry_out, &mut operand],
            )?;
            let result = result.index().expect("realized W result") as u8;
            let carry_out = carry_out.index().expect("realized W carry") as u8;
            let operand = operand.index().expect("realized W operand") as u8;
            code.write_u32(inst::lsr_w_imm(carry_out, operand, 31 - 29))?;
            code.write_u32(inst::and_w_imm(carry_out, carry_out, 1 << 29))?;
            code.write_u32(inst::mov_w(result, 31))?;
        }
        return Ok(());
    }

    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut carry_out = ctx.reg_alloc.write_w(carry_inst);
    let mut operand = ctx.reg_alloc.read_w(operand_arg);
    let mut shift = ctx.reg_alloc.read_w(shift_arg);
    let carry_in_reg;
    if carry_arg.is_immediate() {
        RegAlloc::realize_all(
            code,
            ctx.block,
            &mut [&mut result, &mut carry_out, &mut operand, &mut shift],
        )?;
        carry_in_reg = None;
    } else {
        let mut carry_in = ctx.reg_alloc.read_w(carry_arg);
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
        carry_in_reg = Some(carry_in.index().expect("realized W carry in") as u8);
    }
    ctx.reg_alloc.spill_flags(code)?;

    let result = result.index().expect("realized W result") as u8;
    let carry_out = carry_out.index().expect("realized W carry") as u8;
    let operand = operand.index().expect("realized W operand") as u8;
    let shift = shift.index().expect("realized W shift") as u8;
    let mut zero = Label::new();
    let mut end = Label::new();

    code.write_u32(inst::ands_w_imm(XSCRATCH1, shift, 0xff))?;
    zero.b_cond(code, Cond::EQ)?;
    code.write_u32(inst::sub_w_imm(XSCRATCH0, shift, 1))?;
    code.write_u32(inst::lsrv_w(carry_out, operand, XSCRATCH0))?;
    code.write_u32(inst::lsrv_w(result, operand, shift))?;
    code.write_u32(inst::ubfiz_w(carry_out, carry_out, 29, 1))?;
    code.write_u32(inst::cmp_w_imm(XSCRATCH1, 32))?;
    code.write_u32(inst::csel_w(result, result, 31, Cond::LT))?;
    code.write_u32(inst::csel_w(carry_out, carry_out, 31, Cond::LE))?;
    end.b(code)?;
    zero.bind(code)?;
    code.write_u32(inst::mov_w(result, operand))?;
    emit_carry_input_to_reg(code, carry_arg, carry_in_reg, carry_out)?;
    end.bind(code)?;
    Ok(())
}

fn emit_arithmetic_shift_right32_with_carry(
    code: &mut BlockOfCode,
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
        let result = result.index().expect("realized W result") as u8;
        let carry_out = carry_out.index().expect("realized W carry") as u8;
        let operand = operand.index().expect("realized W operand") as u8;
        if shift <= 31 {
            code.write_u32(inst::ubfx_w(carry_out, operand, shift - 1, 1))?;
            code.write_u32(inst::lsl_w_imm(carry_out, carry_out, 29))?;
            code.write_u32(inst::asr_w_imm(result, operand, shift))?;
        } else {
            code.write_u32(inst::asr_w_imm(result, operand, 31))?;
            code.write_u32(inst::and_w_imm(carry_out, result, 1 << 29))?;
        }
        return Ok(());
    }

    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut carry_out = ctx.reg_alloc.write_w(carry_inst);
    let mut operand = ctx.reg_alloc.read_w(operand_arg);
    let mut shift = ctx.reg_alloc.read_w(shift_arg);
    let carry_in_reg;
    if carry_arg.is_immediate() {
        RegAlloc::realize_all(
            code,
            ctx.block,
            &mut [&mut result, &mut carry_out, &mut operand, &mut shift],
        )?;
        carry_in_reg = None;
    } else {
        let mut carry_in = ctx.reg_alloc.read_w(carry_arg);
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
        carry_in_reg = Some(carry_in.index().expect("realized W carry in") as u8);
    }
    ctx.reg_alloc.spill_flags(code)?;

    let result = result.index().expect("realized W result") as u8;
    let carry_out = carry_out.index().expect("realized W carry") as u8;
    let operand = operand.index().expect("realized W operand") as u8;
    let shift = shift.index().expect("realized W shift") as u8;
    let mut zero = Label::new();
    let mut end = Label::new();

    code.write_u32(inst::ands_w_imm(XSCRATCH0, shift, 0xff))?;
    zero.b_cond(code, Cond::EQ)?;
    code.write_u32(inst::movz_w(XSCRATCH1, 63, 0))?;
    code.write_u32(inst::cmp_w_imm(XSCRATCH0, 63))?;
    code.write_u32(inst::csel_w(XSCRATCH0, XSCRATCH0, XSCRATCH1, Cond::LS))?;
    code.write_u32(inst::sxtw_x(result, operand))?;
    code.write_u32(inst::sub_w_imm(XSCRATCH1, XSCRATCH0, 1))?;
    code.write_u32(inst::asrv_w(carry_out, result, XSCRATCH1))?;
    code.write_u32(inst::asrv_w(result, result, XSCRATCH0))?;
    code.write_u32(inst::ubfiz_w(carry_out, carry_out, 29, 1))?;
    code.write_u32(inst::mov_w(result, result))?;
    end.b(code)?;
    zero.bind(code)?;
    code.write_u32(inst::mov_w(result, operand))?;
    emit_carry_input_to_reg(code, carry_arg, carry_in_reg, carry_out)?;
    end.bind(code)?;
    Ok(())
}

fn emit_rotate_right32_with_carry(
    code: &mut BlockOfCode,
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
        let result = result.index().expect("realized W result") as u8;
        let operand = operand.index().expect("realized W operand") as u8;
        code.write_u32(inst::ror_w_imm(result, operand, shift))?;

        let mut carry_out = ctx.reg_alloc.write_w(carry_inst);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut carry_out])?;
        let carry_out = carry_out.index().expect("realized W carry") as u8;
        code.write_u32(inst::ror_w_imm(
            carry_out,
            operand,
            ((shift + 31) - 29) % 32,
        ))?;
        code.write_u32(inst::and_w_imm(carry_out, carry_out, 1 << 29))?;
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
    let result = result.index().expect("realized W result") as u8;
    let operand = operand.index().expect("realized W operand") as u8;
    let shift = shift.index().expect("realized W shift") as u8;
    code.write_u32(inst::rorv_w(result, operand, shift))?;

    if carry_arg.is_immediate() {
        let mut carry_out = ctx.reg_alloc.write_w(carry_inst);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut carry_out])?;
        ctx.reg_alloc.spill_flags(code)?;
        let carry_out = carry_out.index().expect("realized W carry") as u8;
        code.write_u32(inst::ands_w_imm(31, shift, 0xff))?;
        code.write_u32(inst::lsr_w_imm(carry_out, result, 31 - 29))?;
        code.write_u32(inst::and_w_imm(carry_out, carry_out, 1 << 29))?;
        if carry_arg.get_immediate_u1() {
            emit_mov_w_imm(code, XSCRATCH0, 1 << 29)?;
            code.write_u32(inst::csel_w(carry_out, XSCRATCH0, carry_out, Cond::EQ))?;
        } else {
            code.write_u32(inst::csel_w(carry_out, 31, carry_out, Cond::EQ))?;
        }
    } else {
        let mut carry_in = ctx.reg_alloc.read_w(carry_arg);
        let mut carry_out = ctx.reg_alloc.write_w(carry_inst);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut carry_out, &mut carry_in])?;
        ctx.reg_alloc.spill_flags(code)?;
        let carry_in = carry_in.index().expect("realized W carry in") as u8;
        let carry_out = carry_out.index().expect("realized W carry") as u8;
        code.write_u32(inst::ands_w_imm(31, shift, 0xff))?;
        code.write_u32(inst::lsr_w_imm(carry_out, result, 31 - 29))?;
        code.write_u32(inst::and_w_imm(carry_out, carry_out, 1 << 29))?;
        code.write_u32(inst::csel_w(carry_out, carry_in, carry_out, Cond::EQ))?;
    }
    Ok(())
}

fn emit_carry_input_to_reg(
    code: &mut BlockOfCode,
    carry_arg: Argument,
    carry_in_reg: Option<u8>,
    reg: u8,
) -> Result<(), String> {
    if let Some(carry_in_reg) = carry_in_reg {
        code.write_u32(inst::mov_w(reg, carry_in_reg))?;
        Ok(())
    } else {
        debug_assert!(carry_arg.is_immediate());
        emit_mov_w_imm(code, reg, u32::from(carry_arg.get_immediate_u1()) << 29)
    }
}

fn emit_mov_w_imm(code: &mut BlockOfCode, reg: u8, imm: u32) -> Result<(), String> {
    code.write_u32(inst::movz_w(reg, (imm & 0xffff) as u16, 0))?;
    let high = (imm >> 16) as u16;
    if high != 0 {
        code.write_u32(inst::movk_w(reg, high, 16))?;
    }
    Ok(())
}

fn emit_mov_x_imm(code: &mut BlockOfCode, reg: u8, imm: u64) -> Result<(), String> {
    code.write_u32(inst::movz_x(reg, (imm & 0xffff) as u16, 0))?;
    for shift in [16, 32, 48] {
        let chunk = ((imm >> shift) & 0xffff) as u16;
        if chunk != 0 {
            code.write_u32(inst::movk_x(reg, chunk, shift as u8))?;
        }
    }
    Ok(())
}

fn emit_shift64(
    code: &mut BlockOfCode,
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
        let result = result.index().expect("realized X result") as u8;
        let operand = operand.index().expect("realized X operand") as u8;
        match op {
            ShiftOp::LogicalLeft if shift <= 63 => {
                code.write_u32(inst::lsl_x_imm(result, operand, shift))?;
            }
            ShiftOp::LogicalRight if shift <= 63 => {
                code.write_u32(inst::lsr_x_imm(result, operand, shift))?;
            }
            ShiftOp::LogicalLeft | ShiftOp::LogicalRight => {
                code.write_u32(inst::mov_x(result, 31))?;
            }
            ShiftOp::ArithmeticRight => {
                code.write_u32(inst::asr_x_imm(result, operand, shift.min(63)))?;
            }
            ShiftOp::RotateRight => {
                code.write_u32(inst::ror_x_imm(result, operand, shift % 64))?;
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

    let result = result.index().expect("realized X result") as u8;
    let operand = operand.index().expect("realized X operand") as u8;
    let shift = shift.index().expect("realized X shift") as u8;
    match op {
        ShiftOp::LogicalLeft | ShiftOp::LogicalRight => {
            ctx.reg_alloc.spill_flags(code)?;
            code.write_u32(inst::and_x_imm(XSCRATCH0, shift, 0xff))?;
            let shift_reg = XSCRATCH0;
            let word = match op {
                ShiftOp::LogicalLeft => inst::lslv_x(result, operand, shift_reg),
                ShiftOp::LogicalRight => inst::lsrv_x(result, operand, shift_reg),
                _ => unreachable!(),
            };
            code.write_u32(word)?;
            code.write_u32(inst::cmp_x_imm(shift_reg, 64))?;
            code.write_u32(inst::csel_x(result, result, 31, Cond::LT))?;
        }
        ShiftOp::ArithmeticRight => {
            code.write_u32(inst::asrv_x(result, operand, shift))?;
        }
        ShiftOp::RotateRight => {
            code.write_u32(inst::rorv_x(result, operand, shift))?;
        }
    }
    Ok(())
}

fn emit_shift_masked32(
    code: &mut BlockOfCode,
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
        let result = result.index().expect("realized W result") as u8;
        let operand = operand.index().expect("realized W operand") as u8;
        let word = match op {
            ShiftOp::LogicalLeft => inst::lsl_w_imm(result, operand, shift),
            ShiftOp::LogicalRight => inst::lsr_w_imm(result, operand, shift),
            ShiftOp::ArithmeticRight => inst::asr_w_imm(result, operand, shift),
            ShiftOp::RotateRight => inst::ror_w_imm(result, operand, shift),
        };
        code.write_u32(word)?;
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
    let result = result.index().expect("realized W result") as u8;
    let operand = operand.index().expect("realized W operand") as u8;
    let shift = shift.index().expect("realized W shift") as u8;
    let word = match op {
        ShiftOp::LogicalLeft => inst::lslv_w(result, operand, shift),
        ShiftOp::LogicalRight => inst::lsrv_w(result, operand, shift),
        ShiftOp::ArithmeticRight => inst::asrv_w(result, operand, shift),
        ShiftOp::RotateRight => inst::rorv_w(result, operand, shift),
    };
    code.write_u32(word)?;
    Ok(())
}

fn emit_shift_masked64(
    code: &mut BlockOfCode,
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
        let result = result.index().expect("realized X result") as u8;
        let operand = operand.index().expect("realized X operand") as u8;
        let word = match op {
            ShiftOp::LogicalLeft => inst::lsl_x_imm(result, operand, shift),
            ShiftOp::LogicalRight => inst::lsr_x_imm(result, operand, shift),
            ShiftOp::ArithmeticRight => inst::asr_x_imm(result, operand, shift),
            ShiftOp::RotateRight => inst::ror_x_imm(result, operand, shift),
        };
        code.write_u32(word)?;
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
    let result = result.index().expect("realized X result") as u8;
    let operand = operand.index().expect("realized X operand") as u8;
    let shift = shift.index().expect("realized X shift") as u8;
    let word = match op {
        ShiftOp::LogicalLeft => inst::lslv_x(result, operand, shift),
        ShiftOp::LogicalRight => inst::lsrv_x(result, operand, shift),
        ShiftOp::ArithmeticRight => inst::asrv_x(result, operand, shift),
        ShiftOp::RotateRight => inst::rorv_x(result, operand, shift),
    };
    code.write_u32(word)?;
    Ok(())
}

fn emit_bit_op<const BITSIZE: usize>(
    code: &mut BlockOfCode,
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
            emit_bit_op_reg_flags::<32>(
                code,
                op,
                result.index().expect("realized W result") as u8,
                lhs.index().expect("realized W lhs") as u8,
                rhs.index().expect("realized W rhs") as u8,
            )
        } else {
            RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut lhs, &mut rhs])?;
            emit_bit_op_reg::<32>(
                code,
                op,
                result.index().expect("realized W result") as u8,
                lhs.index().expect("realized W lhs") as u8,
                rhs.index().expect("realized W rhs") as u8,
            )
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
            emit_bit_op_reg_flags::<64>(
                code,
                op,
                result.index().expect("realized X result") as u8,
                lhs.index().expect("realized X lhs") as u8,
                rhs.index().expect("realized X rhs") as u8,
            )
        } else {
            RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut lhs, &mut rhs])?;
            emit_bit_op_reg::<64>(
                code,
                op,
                result.index().expect("realized X result") as u8,
                lhs.index().expect("realized X lhs") as u8,
                rhs.index().expect("realized X rhs") as u8,
            )
        }
    }
}

fn emit_bit_op_reg<const BITSIZE: usize>(
    code: &mut BlockOfCode,
    op: BitOp,
    rd: u8,
    rn: u8,
    rm: u8,
) -> Result<(), String> {
    let word = match (BITSIZE, op) {
        (32, BitOp::And) => inst::and_w_reg(rd, rn, rm),
        (64, BitOp::And) => inst::and_x_reg(rd, rn, rm),
        (32, BitOp::AndNot) => inst::bic_w(rd, rn, rm),
        (64, BitOp::AndNot) => inst::bic_x(rd, rn, rm),
        (32, BitOp::Eor) => inst::eor_w_reg(rd, rn, rm),
        (64, BitOp::Eor) => inst::eor_x_reg(rd, rn, rm),
        (32, BitOp::Or) => inst::orr_w(rd, rn, rm),
        (64, BitOp::Or) => inst::orr_x(rd, rn, rm),
        _ => unreachable!(),
    };
    code.write_u32(word)?;
    Ok(())
}

fn emit_bit_op_reg_flags<const BITSIZE: usize>(
    code: &mut BlockOfCode,
    op: BitOp,
    rd: u8,
    rn: u8,
    rm: u8,
) -> Result<(), String> {
    let word = match (BITSIZE, op) {
        (32, BitOp::And) => inst::ands_w_reg(rd, rn, rm),
        (64, BitOp::And) => inst::ands_x_reg(rd, rn, rm),
        (32, BitOp::AndNot) => inst::bics_w(rd, rn, rm),
        (64, BitOp::AndNot) => inst::bics_x(rd, rn, rm),
        _ => unreachable!(),
    };
    code.write_u32(word)?;
    Ok(())
}

fn emit_not<const BITSIZE: usize>(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    debug_assert!(BITSIZE == 32 || BITSIZE == 64);

    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    if BITSIZE == 32 {
        let mut result = ctx.reg_alloc.write_w(inst_ref);
        let mut operand = ctx.reg_alloc.read_w(args[0]);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
        code.write_u32(inst::mvn_w(
            result.index().expect("realized W result") as u8,
            operand.index().expect("realized W operand") as u8,
        ))?;
    } else {
        let mut result = ctx.reg_alloc.write_x(inst_ref);
        let mut operand = ctx.reg_alloc.read_x(args[0]);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
        code.write_u32(inst::mvn_x(
            result.index().expect("realized X result") as u8,
            operand.index().expect("realized X operand") as u8,
        ))?;
    }
    Ok(())
}

fn emit_sign_extend<const RESULT_BITSIZE: usize, const SOURCE_BITSIZE: usize>(
    code: &mut BlockOfCode,
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
        let result = result.index().expect("realized W result") as u8;
        let operand = operand.index().expect("realized W operand") as u8;
        let word = match SOURCE_BITSIZE {
            8 => inst::sxtb_w(result, operand),
            16 => inst::sxth_w(result, operand),
            _ => {
                return Err(format!(
                    "ARM64 sign extend {SOURCE_BITSIZE}->32 unsupported"
                ))
            }
        };
        code.write_u32(word)?;
    } else {
        let mut result = ctx.reg_alloc.write_x(inst_ref);
        let mut operand = ctx.reg_alloc.read_w(args[0]);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
        let result = result.index().expect("realized X result") as u8;
        let operand = operand.index().expect("realized W operand") as u8;
        let word = match SOURCE_BITSIZE {
            8 => inst::sxtb_x(result, operand),
            16 => inst::sxth_x(result, operand),
            32 => inst::sxtw_x(result, operand),
            _ => {
                return Err(format!(
                    "ARM64 sign extend {SOURCE_BITSIZE}->64 unsupported"
                ))
            }
        };
        code.write_u32(word)?;
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
    code: &mut BlockOfCode,
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
            code.write_u32(inst::adds_w_reg(
                result.index().expect("realized W result") as u8,
                lhs.index().expect("realized W lhs") as u8,
                rhs.index().expect("realized W rhs") as u8,
            ))?;
            code.write_u32(inst::cinc_w(
                overflow.index().expect("realized W overflow") as u8,
                31,
                Cond::VS,
            ))?;
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
            code.write_u32(inst::adds_x_reg(
                result.index().expect("realized X result") as u8,
                lhs.index().expect("realized X lhs") as u8,
                rhs.index().expect("realized X rhs") as u8,
            ))?;
            code.write_u32(inst::cinc_w(
                overflow.index().expect("realized W overflow") as u8,
                31,
                Cond::VS,
            ))?;
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
                emit_add_sub_imm_flags::<32, SUB>(
                    code,
                    result.index().expect("realized W result") as u8,
                    lhs.index().expect("realized W lhs") as u8,
                    imm,
                    carry,
                )
            } else {
                let mut result = ctx.reg_alloc.write_x(inst_ref);
                let mut lhs = ctx.reg_alloc.read_x(args[0]);
                let mut flags = ctx.reg_alloc.write_flags(nzcv_inst);
                RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut lhs, &mut flags])?;
                emit_add_sub_imm_flags::<64, SUB>(
                    code,
                    result.index().expect("realized X result") as u8,
                    lhs.index().expect("realized X lhs") as u8,
                    imm,
                    carry,
                )
            }
        } else if BITSIZE == 32 {
            let mut result = ctx.reg_alloc.write_w(inst_ref);
            let mut lhs = ctx.reg_alloc.read_w(args[0]);
            RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut lhs])?;
            emit_add_sub_imm::<32, SUB>(
                code,
                result.index().expect("realized W result") as u8,
                lhs.index().expect("realized W lhs") as u8,
                imm,
                carry,
            )
        } else {
            let mut result = ctx.reg_alloc.write_x(inst_ref);
            let mut lhs = ctx.reg_alloc.read_x(args[0]);
            RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut lhs])?;
            emit_add_sub_imm::<64, SUB>(
                code,
                result.index().expect("realized X result") as u8,
                lhs.index().expect("realized X lhs") as u8,
                imm,
                carry,
            )
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
            emit_add_sub_reg_flags::<32, SUB>(
                code,
                result.index().expect("realized W result") as u8,
                lhs.index().expect("realized W lhs") as u8,
                rhs.index().expect("realized W rhs") as u8,
                carry,
            )
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
            emit_add_sub_reg_flags::<64, SUB>(
                code,
                result.index().expect("realized X result") as u8,
                lhs.index().expect("realized X lhs") as u8,
                rhs.index().expect("realized X rhs") as u8,
                carry,
            )
        }
    } else if BITSIZE == 32 {
        let mut result = ctx.reg_alloc.write_w(inst_ref);
        let mut lhs = ctx.reg_alloc.read_w(args[0]);
        let mut rhs = ctx.reg_alloc.read_w(args[1]);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut lhs, &mut rhs])?;
        emit_add_sub_reg::<32, SUB>(
            code,
            result.index().expect("realized W result") as u8,
            lhs.index().expect("realized W lhs") as u8,
            rhs.index().expect("realized W rhs") as u8,
            carry,
        )
    } else {
        let mut result = ctx.reg_alloc.write_x(inst_ref);
        let mut lhs = ctx.reg_alloc.read_x(args[0]);
        let mut rhs = ctx.reg_alloc.read_x(args[1]);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut lhs, &mut rhs])?;
        emit_add_sub_reg::<64, SUB>(
            code,
            result.index().expect("realized X result") as u8,
            lhs.index().expect("realized X lhs") as u8,
            rhs.index().expect("realized X rhs") as u8,
            carry,
        )
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
    code: &mut BlockOfCode,
    rd: u8,
    rn: u8,
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
        let word = match (BITSIZE, SUB, carry) {
            (32, false, false) => inst::add_w_imm_shift(rd, rn, imm12, shift12),
            (64, false, false) => inst::add_x_imm_shift(rd, rn, imm12, shift12),
            (32, true, true) => inst::sub_w_imm_shift(rd, rn, imm12, shift12),
            (64, true, true) => inst::sub_x_imm_shift(rd, rn, imm12, shift12),
            (32, false, true) => inst::sub_w_imm_shift(rd, rn, imm12, shift12),
            (64, false, true) => inst::sub_x_imm_shift(rd, rn, imm12, shift12),
            (32, true, false) => inst::add_w_imm_shift(rd, rn, imm12, shift12),
            (64, true, false) => inst::add_x_imm_shift(rd, rn, imm12, shift12),
            _ => unreachable!(),
        };
        code.write_u32(word)?;
    } else {
        if BITSIZE == 32 {
            emit_mov_w_imm(code, XSCRATCH0, adjusted as u32)?;
        } else {
            emit_mov_x_imm(code, XSCRATCH0, adjusted)?;
        }
        // MaybeAddSubImm materializes an already-adjusted operand; do not
        // complement it again through the unadjusted register-operand path.
        code.write_u32(match (BITSIZE, carry) {
            (32, false) => inst::add_w_reg(rd, rn, XSCRATCH0),
            (64, false) => inst::add_x_reg(rd, rn, XSCRATCH0),
            (32, true) => inst::sub_w_reg(rd, rn, XSCRATCH0),
            (64, true) => inst::sub_x_reg(rd, rn, XSCRATCH0),
            _ => unreachable!(),
        })?;
    }
    Ok(())
}

fn emit_add_sub_imm_flags<const BITSIZE: usize, const SUB: bool>(
    code: &mut BlockOfCode,
    rd: u8,
    rn: u8,
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
        let word = match (BITSIZE, SUB, carry) {
            (32, false, false) => inst::adds_w_imm_shift(rd, rn, imm12, shift12),
            (64, false, false) => inst::adds_x_imm_shift(rd, rn, imm12, shift12),
            (32, true, true) => inst::subs_w_imm_shift(rd, rn, imm12, shift12),
            (64, true, true) => inst::subs_x_imm_shift(rd, rn, imm12, shift12),
            (32, false, true) => inst::subs_w_imm_shift(rd, rn, imm12, shift12),
            (64, false, true) => inst::subs_x_imm_shift(rd, rn, imm12, shift12),
            (32, true, false) => inst::adds_w_imm_shift(rd, rn, imm12, shift12),
            (64, true, false) => inst::adds_x_imm_shift(rd, rn, imm12, shift12),
            _ => unreachable!(),
        };
        code.write_u32(word)?;
    } else {
        if BITSIZE == 32 {
            emit_mov_w_imm(code, XSCRATCH0, adjusted as u32)?;
        } else {
            emit_mov_x_imm(code, XSCRATCH0, adjusted)?;
        }
        code.write_u32(match (BITSIZE, carry) {
            (32, false) => inst::adds_w_reg(rd, rn, XSCRATCH0),
            (64, false) => inst::adds_x_reg(rd, rn, XSCRATCH0),
            (32, true) => inst::subs_w_reg(rd, rn, XSCRATCH0),
            (64, true) => inst::subs_x_reg(rd, rn, XSCRATCH0),
            _ => unreachable!(),
        })?;
    }
    Ok(())
}

fn emit_add_sub_dynamic_carry<const BITSIZE: usize, const SUB: bool>(
    code: &mut BlockOfCode,
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
                31
            } else {
                emit_mov_w_imm(code, XSCRATCH0, imm as u32)?;
                XSCRATCH0
            };
            let word = match (SUB, nzcv_inst.is_some()) {
                (false, false) => inst::adc_w(
                    result.index().expect("realized W result") as u8,
                    lhs.index().expect("realized W lhs") as u8,
                    rhs,
                ),
                (true, false) => inst::sbc_w(
                    result.index().expect("realized W result") as u8,
                    lhs.index().expect("realized W lhs") as u8,
                    rhs,
                ),
                (false, true) => inst::adcs_w(
                    result.index().expect("realized W result") as u8,
                    lhs.index().expect("realized W lhs") as u8,
                    rhs,
                ),
                (true, true) => inst::sbcs_w(
                    result.index().expect("realized W result") as u8,
                    lhs.index().expect("realized W lhs") as u8,
                    rhs,
                ),
            };
            code.write_u32(word)?;
        } else {
            let mut result = ctx.reg_alloc.write_x(inst_ref);
            let mut lhs = ctx.reg_alloc.read_x(args[0]);
            RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut lhs])?;
            ctx.reg_alloc
                .read_write_flags(code, ctx.block, args[2], nzcv_inst)?;
            let rhs = if imm == 0 {
                31
            } else {
                emit_mov_x_imm(code, XSCRATCH0, imm)?;
                XSCRATCH0
            };
            let word = match (SUB, nzcv_inst.is_some()) {
                (false, false) => inst::adc_x(
                    result.index().expect("realized X result") as u8,
                    lhs.index().expect("realized X lhs") as u8,
                    rhs,
                ),
                (true, false) => inst::sbc_x(
                    result.index().expect("realized X result") as u8,
                    lhs.index().expect("realized X lhs") as u8,
                    rhs,
                ),
                (false, true) => inst::adcs_x(
                    result.index().expect("realized X result") as u8,
                    lhs.index().expect("realized X lhs") as u8,
                    rhs,
                ),
                (true, true) => inst::sbcs_x(
                    result.index().expect("realized X result") as u8,
                    lhs.index().expect("realized X lhs") as u8,
                    rhs,
                ),
            };
            code.write_u32(word)?;
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
        let word = match (SUB, nzcv_inst.is_some()) {
            (false, false) => inst::adc_w(
                result.index().expect("realized W result") as u8,
                lhs.index().expect("realized W lhs") as u8,
                rhs.index().expect("realized W rhs") as u8,
            ),
            (true, false) => inst::sbc_w(
                result.index().expect("realized W result") as u8,
                lhs.index().expect("realized W lhs") as u8,
                rhs.index().expect("realized W rhs") as u8,
            ),
            (false, true) => inst::adcs_w(
                result.index().expect("realized W result") as u8,
                lhs.index().expect("realized W lhs") as u8,
                rhs.index().expect("realized W rhs") as u8,
            ),
            (true, true) => inst::sbcs_w(
                result.index().expect("realized W result") as u8,
                lhs.index().expect("realized W lhs") as u8,
                rhs.index().expect("realized W rhs") as u8,
            ),
        };
        code.write_u32(word)?;
    } else {
        let mut result = ctx.reg_alloc.write_x(inst_ref);
        let mut lhs = ctx.reg_alloc.read_x(args[0]);
        let mut rhs = ctx.reg_alloc.read_x(args[1]);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut lhs, &mut rhs])?;
        ctx.reg_alloc
            .read_write_flags(code, ctx.block, args[2], nzcv_inst)?;
        let word = match (SUB, nzcv_inst.is_some()) {
            (false, false) => inst::adc_x(
                result.index().expect("realized X result") as u8,
                lhs.index().expect("realized X lhs") as u8,
                rhs.index().expect("realized X rhs") as u8,
            ),
            (true, false) => inst::sbc_x(
                result.index().expect("realized X result") as u8,
                lhs.index().expect("realized X lhs") as u8,
                rhs.index().expect("realized X rhs") as u8,
            ),
            (false, true) => inst::adcs_x(
                result.index().expect("realized X result") as u8,
                lhs.index().expect("realized X lhs") as u8,
                rhs.index().expect("realized X rhs") as u8,
            ),
            (true, true) => inst::sbcs_x(
                result.index().expect("realized X result") as u8,
                lhs.index().expect("realized X lhs") as u8,
                rhs.index().expect("realized X rhs") as u8,
            ),
        };
        code.write_u32(word)?;
    }
    Ok(())
}

fn emit_add_sub_reg<const BITSIZE: usize, const SUB: bool>(
    code: &mut BlockOfCode,
    rd: u8,
    rn: u8,
    rm: u8,
    carry: bool,
) -> Result<(), String> {
    match (BITSIZE, SUB, carry) {
        (32, false, false) => {
            code.write_u32(inst::add_w_reg(rd, rn, rm))?;
            Ok(())
        }
        (64, false, false) => {
            code.write_u32(inst::add_x_reg(rd, rn, rm))?;
            Ok(())
        }
        (32, true, true) => {
            code.write_u32(inst::sub_w_reg(rd, rn, rm))?;
            Ok(())
        }
        (64, true, true) => {
            code.write_u32(inst::sub_x_reg(rd, rn, rm))?;
            Ok(())
        }
        (32, false, true) => {
            code.write_u32(inst::mvn_w(XSCRATCH0, rm))?;
            code.write_u32(inst::sub_w_reg(rd, rn, XSCRATCH0))?;
            Ok(())
        }
        (64, false, true) => {
            code.write_u32(inst::mvn_x(XSCRATCH0, rm))?;
            code.write_u32(inst::sub_x_reg(rd, rn, XSCRATCH0))?;
            Ok(())
        }
        (32, true, false) => {
            code.write_u32(inst::mvn_w(XSCRATCH0, rm))?;
            code.write_u32(inst::add_w_reg(rd, rn, XSCRATCH0))?;
            Ok(())
        }
        (64, true, false) => {
            code.write_u32(inst::mvn_x(XSCRATCH0, rm))?;
            code.write_u32(inst::add_x_reg(rd, rn, XSCRATCH0))?;
            Ok(())
        }
        _ => unreachable!(),
    }
}

fn emit_add_sub_reg_flags<const BITSIZE: usize, const SUB: bool>(
    code: &mut BlockOfCode,
    rd: u8,
    rn: u8,
    rm: u8,
    carry: bool,
) -> Result<(), String> {
    match (BITSIZE, SUB, carry) {
        (32, false, false) => {
            code.write_u32(inst::adds_w_reg(rd, rn, rm))?;
            Ok(())
        }
        (64, false, false) => {
            code.write_u32(inst::adds_x_reg(rd, rn, rm))?;
            Ok(())
        }
        (32, true, true) => {
            code.write_u32(inst::subs_w_reg(rd, rn, rm))?;
            Ok(())
        }
        (64, true, true) => {
            code.write_u32(inst::subs_x_reg(rd, rn, rm))?;
            Ok(())
        }
        (32, false, true) => {
            code.write_u32(inst::mvn_w(XSCRATCH0, rm))?;
            code.write_u32(inst::subs_w_reg(rd, rn, XSCRATCH0))?;
            Ok(())
        }
        (64, false, true) => {
            code.write_u32(inst::mvn_x(XSCRATCH0, rm))?;
            code.write_u32(inst::subs_x_reg(rd, rn, XSCRATCH0))?;
            Ok(())
        }
        (32, true, false) => {
            code.write_u32(inst::mvn_w(XSCRATCH0, rm))?;
            code.write_u32(inst::adds_w_reg(rd, rn, XSCRATCH0))?;
            Ok(())
        }
        (64, true, false) => {
            code.write_u32(inst::mvn_x(XSCRATCH0, rm))?;
            code.write_u32(inst::adds_x_reg(rd, rn, XSCRATCH0))?;
            Ok(())
        }
        _ => unreachable!(),
    }
}

#[cfg(all(test, target_arch = "aarch64"))]
mod tests {
    use super::*;

    fn check_immediate_arithmetic<const BITS: usize, const SUB: bool>() {
        for carry in [false, true] {
            for flags in [false, true] {
                for imm in [0, 1, 16, 0x12345, 0x1234_5678, u64::MAX] {
                    let mut code = BlockOfCode::with_size(4096).unwrap();
                    if flags {
                        emit_add_sub_imm_flags::<BITS, SUB>(&mut code, 0, 0, imm, carry).unwrap();
                    } else {
                        emit_add_sub_imm::<BITS, SUB>(&mut code, 0, 0, imm, carry).unwrap();
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
