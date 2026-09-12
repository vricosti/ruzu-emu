//! ARM64 scalar saturation emission.
//!
//! Upstream owner: `backend/arm64/emit_arm64_saturation.cpp`.

use rhazel::{CodeGenerator, WZR};

use crate::backend::arm64::abi::regs::{WSCRATCH0, WSCRATCH1};
use crate::backend::arm64::block_of_code::BlockOfCode;
use crate::backend::arm64::emit_context::EmitContext;
use crate::backend::arm64::reg_alloc::RegAlloc;
use crate::ir::cond::Cond;
use crate::ir::opcode::Opcode;
use crate::ir::value::InstRef;

pub fn emit_signed_saturated_add_with_flag32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    let overflow_inst = ctx
        .block
        .get_associated_pseudo_operation(inst_ref, Opcode::GetOverflowFromOp)
        .expect("SignedSaturatedAddWithFlag32 requires an overflow pseudo-operation");

    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut a = ctx.reg_alloc.read_w(args[0]);
    let mut b = ctx.reg_alloc.read_w(args[1]);
    let mut overflow = ctx.reg_alloc.write_w(overflow_inst);
    RegAlloc::realize_all(
        code,
        ctx.block,
        &mut [&mut result, &mut a, &mut b, &mut overflow],
    )?;
    ctx.reg_alloc.spill_flags(code)?;

    let (result, a, b, overflow) = (result.w(), a.w(), b.w(), overflow.w());
    code.adds(result, a, b)?;
    code.asr(WSCRATCH0, result, 31)?;
    code.mov_imm(WSCRATCH1, 0x8000_0000 as u64)?;
    code.eor(WSCRATCH0, WSCRATCH0, WSCRATCH1)?;
    code.csel(result, result, WSCRATCH0, Cond::VC)?;
    code.cinc(overflow, WZR, Cond::VS)?;
    Ok(())
}

pub fn emit_signed_saturated_sub_with_flag32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    let overflow_inst = ctx
        .block
        .get_associated_pseudo_operation(inst_ref, Opcode::GetOverflowFromOp)
        .expect("SignedSaturatedSubWithFlag32 requires an overflow pseudo-operation");

    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut a = ctx.reg_alloc.read_w(args[0]);
    let mut b = ctx.reg_alloc.read_w(args[1]);
    let mut overflow = ctx.reg_alloc.write_w(overflow_inst);
    RegAlloc::realize_all(
        code,
        ctx.block,
        &mut [&mut result, &mut a, &mut b, &mut overflow],
    )?;
    ctx.reg_alloc.spill_flags(code)?;

    let (result, a, b, overflow) = (result.w(), a.w(), b.w(), overflow.w());
    code.subs(result, a, b)?;
    code.asr(WSCRATCH0, result, 31)?;
    code.mov_imm(WSCRATCH1, 0x8000_0000 as u64)?;
    code.eor(WSCRATCH0, WSCRATCH0, WSCRATCH1)?;
    code.csel(result, result, WSCRATCH0, Cond::VC)?;
    code.cinc(overflow, WZR, Cond::VS)?;
    Ok(())
}

pub fn emit_signed_saturation(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    let overflow_inst = ctx
        .block
        .get_associated_pseudo_operation(inst_ref, Opcode::GetOverflowFromOp);
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let bit_size = args[1].get_immediate_u8() as usize;
    assert!((1..=32).contains(&bit_size));

    if bit_size == 32 {
        ctx.reg_alloc
            .define_as_existing(ctx.block, inst_ref, args[0]);
        if let Some(overflow_inst) = overflow_inst {
            let mut overflow = ctx.reg_alloc.write_w(overflow_inst);
            RegAlloc::realize_all(code, ctx.block, &mut [&mut overflow])?;
            code.mov(overflow.w(), WZR)?;
        }
        return Ok(());
    }

    let positive_saturated_value = (1u32 << (bit_size - 1)) - 1;
    let negative_saturated_value = !0u32 << (bit_size - 1);

    let mut operand = ctx.reg_alloc.read_w(args[0]);
    let mut result = ctx.reg_alloc.write_w(inst_ref);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut operand, &mut result])?;
    ctx.reg_alloc.spill_flags(code)?;

    let (operand, result) = (operand.w(), result.w());
    code.mov_imm(WSCRATCH0, negative_saturated_value as u64)?;
    code.mov_imm(WSCRATCH1, positive_saturated_value as u64)?;
    code.cmp(operand, WSCRATCH0)?;
    code.csel(result, operand, WSCRATCH0, Cond::GT)?;
    code.cmp(operand, WSCRATCH1)?;
    code.csel(result, result, WSCRATCH1, Cond::LT)?;

    if let Some(overflow_inst) = overflow_inst {
        let mut overflow = ctx.reg_alloc.write_w(overflow_inst);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut overflow])?;
        code.cmp(result, operand)?;
        code.cinc(overflow.w(), WZR, Cond::NE)?;
    }
    Ok(())
}

pub fn emit_unsigned_saturation(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    let overflow_inst = ctx
        .block
        .get_associated_pseudo_operation(inst_ref, Opcode::GetOverflowFromOp);
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut operand = ctx.reg_alloc.read_w(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    ctx.reg_alloc.spill_flags(code)?;

    let bit_size = args[1].get_immediate_u8() as usize;
    assert!(bit_size <= 31);
    let saturated_value = (1u32 << bit_size) - 1;

    let (result, operand) = (result.w(), operand.w());
    code.mov_imm(WSCRATCH0, saturated_value as u64)?;
    code.cmp_imm(operand, 0)?;
    code.csel(result, operand, WZR, Cond::GT)?;
    code.cmp(operand, WSCRATCH0)?;
    code.csel(result, result, WSCRATCH0, Cond::LT)?;

    if let Some(overflow_inst) = overflow_inst {
        let mut overflow = ctx.reg_alloc.write_w(overflow_inst);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut overflow])?;
        code.cinc(overflow.w(), WZR, Cond::HI)?;
    }
    Ok(())
}
