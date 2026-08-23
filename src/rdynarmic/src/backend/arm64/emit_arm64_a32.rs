//! A32 terminal emission for the ARM64 host backend.
//!
//! Upstream owner: `backend/arm64/emit_arm64_a32.cpp`.

use crate::backend::arm64::abi::{XHALT, XSCRATCH0, XSCRATCH1, XSCRATCH2, XSTATE, XTICKS};
use crate::backend::arm64::block_of_code::BlockOfCode;
use crate::backend::arm64::emit_arm64::{
    emit_block_link_relocation, emit_relocation, BlockRelocationType, LinkTarget,
};
use crate::backend::arm64::emit_context::EmitContext;
use crate::backend::arm64::inst;
use crate::backend::arm64::jit_state::A32JitState;
use crate::backend::arm64::label::Label;
use crate::backend::arm64::reg_alloc::RegAlloc;
use crate::backend::arm64::stack_layout::{RSBEntry, StackLayout, RSB_INDEX_MASK};
use crate::frontend::a32::types::{ExtReg, Reg};
use crate::halt_reason::HaltReason;
use crate::ir::cond::Cond;
use crate::ir::location::{A32LocationDescriptor, LocationDescriptor};
use crate::ir::opcode::Opcode;
use crate::ir::terminal::Terminal;
use crate::ir::value::InstRef;
use crate::jit_config::OptimizationFlag;

const WZR: u8 = 31;

pub fn emit_a32_set_check_bit(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    if args[0].is_immediate() {
        if args[0].get_immediate_u1() {
            emit_mov_w_imm(code, XSCRATCH0, 1)?;
            code.write_u32(inst::strb_w_unsigned(
                XSCRATCH0,
                31,
                StackLayout::check_bit_offset() as u32,
            ))?;
        } else {
            code.write_u32(inst::strb_w_unsigned(
                WZR,
                31,
                StackLayout::check_bit_offset() as u32,
            ))?;
        }
        return Ok(());
    }

    let mut bit = ctx.reg_alloc.read_w(args[0]);
    let bit_reg = bit.realize(code, ctx.block)? as u8;
    code.write_u32(inst::strb_w_unsigned(
        bit_reg,
        31,
        StackLayout::check_bit_offset() as u32,
    ))?;
    Ok(())
}

pub fn emit_a32_get_register(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let reg = ctx.block.get(inst_ref).arg(0).get_a32_reg();
    ensure_valid_reg(reg)?;

    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let result_reg = result.realize(code, ctx.block)? as u8;
    code.write_u32(inst::ldr_w_unsigned(
        result_reg,
        XSTATE,
        a32_reg_offset(reg),
    ))?;
    Ok(())
}

pub fn emit_a32_set_register(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let reg = ctx.block.get(inst_ref).arg(0).get_a32_reg();
    ensure_valid_reg(reg)?;

    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut value = ctx.reg_alloc.read_w(args[1]);
    let value_reg = value.realize(code, ctx.block)? as u8;
    code.write_u32(inst::str_w_unsigned(value_reg, XSTATE, a32_reg_offset(reg)))?;
    Ok(())
}

pub fn emit_a32_get_extended_register32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let reg = ctx.block.get(inst_ref).arg(0).get_a32_ext_reg();
    ensure_single_ext_reg(reg)?;

    let mut result = ctx.reg_alloc.write_s(inst_ref);
    let result_reg = result.realize(code, ctx.block)? as u8;
    code.write_u32(inst::ldr_s_unsigned(
        result_reg,
        XSTATE,
        a32_ext_reg_single_offset(reg),
    ))?;
    Ok(())
}

pub fn emit_a32_get_vector(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let reg = ctx.block.get(inst_ref).arg(0).get_a32_ext_reg();
    ensure_double_or_quad_ext_reg(reg)?;

    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let result_reg = result.realize(code, ctx.block)? as u8;
    if reg.is_double() {
        code.write_u32(inst::ldr_d_unsigned(
            result_reg,
            XSTATE,
            a32_ext_reg_double_offset(reg),
        ))?;
    } else {
        code.write_u32(inst::ldr_q_unsigned(
            result_reg,
            XSTATE,
            a32_ext_reg_quad_offset(reg),
        ))?;
    }
    Ok(())
}

pub fn emit_a32_get_extended_register64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let reg = ctx.block.get(inst_ref).arg(0).get_a32_ext_reg();
    ensure_double_ext_reg(reg)?;

    let mut result = ctx.reg_alloc.write_d(inst_ref);
    let result_reg = result.realize(code, ctx.block)? as u8;
    code.write_u32(inst::ldr_d_unsigned(
        result_reg,
        XSTATE,
        a32_ext_reg_double_offset(reg),
    ))?;
    Ok(())
}

pub fn emit_a32_set_extended_register32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let reg = ctx.block.get(inst_ref).arg(0).get_a32_ext_reg();
    ensure_single_ext_reg(reg)?;

    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut value = ctx.reg_alloc.read_s(args[1]);
    let value_reg = value.realize(code, ctx.block)? as u8;
    code.write_u32(inst::str_s_unsigned(
        value_reg,
        XSTATE,
        a32_ext_reg_single_offset(reg),
    ))?;
    Ok(())
}

pub fn emit_a32_set_extended_register64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let reg = ctx.block.get(inst_ref).arg(0).get_a32_ext_reg();
    ensure_double_ext_reg(reg)?;

    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut value = ctx.reg_alloc.read_d(args[1]);
    let value_reg = value.realize(code, ctx.block)? as u8;
    code.write_u32(inst::str_d_unsigned(
        value_reg,
        XSTATE,
        a32_ext_reg_double_offset(reg),
    ))?;
    Ok(())
}

pub fn emit_a32_set_vector(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let reg = ctx.block.get(inst_ref).arg(0).get_a32_ext_reg();
    ensure_double_or_quad_ext_reg(reg)?;

    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut value = ctx.reg_alloc.read_q(args[1]);
    let value_reg = value.realize(code, ctx.block)? as u8;
    if reg.is_double() {
        code.write_u32(inst::str_d_unsigned(
            value_reg,
            XSTATE,
            a32_ext_reg_double_offset(reg),
        ))?;
    } else {
        code.write_u32(inst::str_q_unsigned(
            value_reg,
            XSTATE,
            a32_ext_reg_quad_offset(reg),
        ))?;
    }
    Ok(())
}

pub fn emit_a32_get_cpsr(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let mut cpsr = ctx.reg_alloc.write_w(inst_ref);
    let cpsr_reg = cpsr.realize(code, ctx.block)? as u8;

    debug_assert_eq!(
        core::mem::offset_of!(A32JitState, cpsr_nzcv) + core::mem::size_of::<u32>(),
        core::mem::offset_of!(A32JitState, cpsr_q)
    );

    code.write_u32(inst::ldp_w_offset(
        XSCRATCH0,
        XSCRATCH1,
        XSTATE,
        a32_cpsr_nzcv_offset() as i32,
    ))?;
    code.write_u32(inst::ldr_w_unsigned(
        cpsr_reg,
        XSTATE,
        a32_cpsr_jaifm_offset(),
    ))?;
    code.write_u32(inst::orr_w(cpsr_reg, cpsr_reg, XSCRATCH0))?;
    code.write_u32(inst::orr_w(cpsr_reg, cpsr_reg, XSCRATCH1))?;

    code.write_u32(inst::ldr_w_unsigned(
        XSCRATCH0,
        XSTATE,
        a32_cpsr_ge_offset(),
    ))?;
    code.write_u32(inst::and_w_imm(XSCRATCH0, XSCRATCH0, 0x8080_8080))?;
    emit_mov_w_imm(code, XSCRATCH1, 0x0020_4081)?;
    code.write_u32(inst::mul_w(XSCRATCH0, XSCRATCH0, XSCRATCH1))?;
    code.write_u32(inst::and_w_imm(XSCRATCH0, XSCRATCH0, 0xf000_0000))?;
    code.write_u32(inst::orr_w_lsr(cpsr_reg, cpsr_reg, XSCRATCH0, 12))?;

    code.write_u32(inst::ldr_w_unsigned(
        XSCRATCH0,
        XSTATE,
        a32_upper_location_descriptor_offset(),
    ))?;
    code.write_u32(inst::and_w_imm(XSCRATCH0, XSCRATCH0, 0b11))?;
    code.write_u32(inst::orr_w_lsl(XSCRATCH0, XSCRATCH0, XSCRATCH0, 3))?;
    code.write_u32(inst::and_w_imm(XSCRATCH0, XSCRATCH0, 0x1111_1111))?;
    code.write_u32(inst::orr_w_lsl(cpsr_reg, cpsr_reg, XSCRATCH0, 5))?;
    Ok(())
}

pub fn emit_a32_set_cpsr(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut cpsr = ctx.reg_alloc.read_w(args[0]);
    let cpsr_reg = cpsr.realize(code, ctx.block)? as u8;

    code.write_u32(inst::and_w_imm(XSCRATCH0, cpsr_reg, 0xf000_0000))?;
    code.write_u32(inst::and_w_imm(XSCRATCH1, cpsr_reg, 1 << 27))?;

    debug_assert_eq!(
        core::mem::offset_of!(A32JitState, cpsr_nzcv) + core::mem::size_of::<u32>(),
        core::mem::offset_of!(A32JitState, cpsr_q)
    );
    code.write_u32(inst::stp_w_offset(
        XSCRATCH0,
        XSCRATCH1,
        XSTATE,
        a32_cpsr_nzcv_offset() as i32,
    ))?;

    code.write_u32(inst::ubfx_w(XSCRATCH0, cpsr_reg, 16, 4))?;
    emit_mov_w_imm(code, XSCRATCH1, 0x0020_4081)?;
    code.write_u32(inst::mul_w(XSCRATCH0, XSCRATCH0, XSCRATCH1))?;
    code.write_u32(inst::and_w_imm(XSCRATCH0, XSCRATCH0, 0x0101_0101))?;
    code.write_u32(inst::lsl_w_imm(XSCRATCH1, XSCRATCH0, 8))?;
    code.write_u32(inst::sub_w_reg(XSCRATCH0, XSCRATCH1, XSCRATCH0))?;

    emit_mov_w_imm(code, XSCRATCH1, 0x0100_01df)?;
    code.write_u32(inst::and_w_reg(XSCRATCH1, cpsr_reg, XSCRATCH1))?;

    debug_assert_eq!(
        core::mem::offset_of!(A32JitState, cpsr_jaifm) + core::mem::size_of::<u32>(),
        core::mem::offset_of!(A32JitState, cpsr_ge)
    );
    code.write_u32(inst::stp_w_offset(
        XSCRATCH1,
        XSCRATCH0,
        XSTATE,
        a32_cpsr_jaifm_offset() as i32,
    ))?;

    code.write_u32(inst::and_w_imm(XSCRATCH0, cpsr_reg, 0xfc00))?;
    code.write_u32(inst::lsr_w_imm(XSCRATCH1, cpsr_reg, 17))?;
    code.write_u32(inst::and_w_imm(XSCRATCH1, XSCRATCH1, 0x300))?;
    code.write_u32(inst::orr_w(XSCRATCH0, XSCRATCH0, XSCRATCH1))?;

    code.write_u32(inst::lsr_w_imm(XSCRATCH1, cpsr_reg, 8))?;
    code.write_u32(inst::and_w_imm(XSCRATCH1, XSCRATCH1, 0x2))?;
    code.write_u32(inst::orr_w(XSCRATCH0, XSCRATCH0, XSCRATCH1))?;
    code.write_u32(inst::ldr_w_unsigned(
        XSCRATCH1,
        XSTATE,
        a32_upper_location_descriptor_offset(),
    ))?;
    code.write_u32(inst::bfxil_w(XSCRATCH0, cpsr_reg, 5, 1))?;
    code.write_u32(inst::and_w_imm(XSCRATCH1, XSCRATCH1, 0xffff_0000))?;
    code.write_u32(inst::orr_w(XSCRATCH0, XSCRATCH0, XSCRATCH1))?;
    code.write_u32(inst::str_w_unsigned(
        XSCRATCH0,
        XSTATE,
        a32_upper_location_descriptor_offset(),
    ))?;
    Ok(())
}

pub fn emit_a32_set_cpsr_nzcv(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut nzcv = ctx.reg_alloc.read_w(args[0]);
    let nzcv_reg = nzcv.realize(code, ctx.block)? as u8;
    code.write_u32(inst::str_w_unsigned(
        nzcv_reg,
        XSTATE,
        a32_cpsr_nzcv_offset(),
    ))?;
    Ok(())
}

pub fn emit_a32_set_cpsr_nzcv_raw(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut nzcv = ctx.reg_alloc.read_w(args[0]);
    let nzcv_reg = nzcv.realize(code, ctx.block)? as u8;
    code.write_u32(inst::str_w_unsigned(
        nzcv_reg,
        XSTATE,
        a32_cpsr_nzcv_offset(),
    ))?;
    Ok(())
}

pub fn emit_a32_set_cpsr_nzcvq(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut nzcv = ctx.reg_alloc.read_w(args[0]);
    let nzcv_reg = nzcv.realize(code, ctx.block)? as u8;
    code.write_u32(inst::and_w_imm(XSCRATCH0, nzcv_reg, 0xf000_0000))?;
    code.write_u32(inst::and_w_imm(XSCRATCH1, nzcv_reg, 0x0800_0000))?;
    code.write_u32(inst::stp_w_offset(
        XSCRATCH0,
        XSCRATCH1,
        XSTATE,
        a32_cpsr_nzcv_offset() as i32,
    ))?;
    Ok(())
}

pub fn emit_a32_set_cpsr_nz(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut nz = ctx.reg_alloc.read_w(args[0]);
    let nz_reg = nz.realize(code, ctx.block)? as u8;

    code.write_u32(inst::ldr_w_unsigned(
        XSCRATCH0,
        XSTATE,
        a32_cpsr_nzcv_offset(),
    ))?;
    code.write_u32(inst::and_w_imm(XSCRATCH0, XSCRATCH0, 0x3000_0000))?;
    code.write_u32(inst::orr_w(XSCRATCH0, XSCRATCH0, nz_reg))?;
    code.write_u32(inst::str_w_unsigned(
        XSCRATCH0,
        XSTATE,
        a32_cpsr_nzcv_offset(),
    ))?;
    Ok(())
}

pub fn emit_a32_set_cpsr_nzc(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);

    if args[0].is_immediate() {
        if args[1].is_immediate() {
            let carry = args[1].get_immediate_u1();
            code.write_u32(inst::ldr_w_unsigned(
                XSCRATCH0,
                XSTATE,
                a32_cpsr_nzcv_offset(),
            ))?;
            code.write_u32(inst::and_w_imm(XSCRATCH0, XSCRATCH0, 0x1000_0000))?;
            if carry {
                code.write_u32(inst::orr_w_imm(XSCRATCH0, XSCRATCH0, 0x2000_0000))?;
            }
            code.write_u32(inst::str_w_unsigned(
                XSCRATCH0,
                XSTATE,
                a32_cpsr_nzcv_offset(),
            ))?;
        } else {
            let mut c = ctx.reg_alloc.read_w(args[1]);
            let c_reg = c.realize(code, ctx.block)? as u8;
            code.write_u32(inst::ldr_w_unsigned(
                XSCRATCH0,
                XSTATE,
                a32_cpsr_nzcv_offset(),
            ))?;
            code.write_u32(inst::and_w_imm(XSCRATCH0, XSCRATCH0, 0x1000_0000))?;
            code.write_u32(inst::orr_w(XSCRATCH0, XSCRATCH0, c_reg))?;
            code.write_u32(inst::str_w_unsigned(
                XSCRATCH0,
                XSTATE,
                a32_cpsr_nzcv_offset(),
            ))?;
        }
    } else if args[1].is_immediate() {
        let carry = args[1].get_immediate_u1();
        let mut nz = ctx.reg_alloc.read_w(args[0]);
        let nz_reg = nz.realize(code, ctx.block)? as u8;
        code.write_u32(inst::ldr_w_unsigned(
            XSCRATCH0,
            XSTATE,
            a32_cpsr_nzcv_offset(),
        ))?;
        code.write_u32(inst::and_w_imm(XSCRATCH0, XSCRATCH0, 0x1000_0000))?;
        code.write_u32(inst::orr_w(XSCRATCH0, XSCRATCH0, nz_reg))?;
        if carry {
            code.write_u32(inst::orr_w_imm(XSCRATCH0, XSCRATCH0, 0x2000_0000))?;
        }
        code.write_u32(inst::str_w_unsigned(
            XSCRATCH0,
            XSTATE,
            a32_cpsr_nzcv_offset(),
        ))?;
    } else {
        let mut nz = ctx.reg_alloc.read_w(args[0]);
        let mut c = ctx.reg_alloc.read_w(args[1]);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut nz, &mut c])?;
        let nz_reg = nz.index().expect("realized W NZ") as u8;
        let c_reg = c.index().expect("realized W C") as u8;
        code.write_u32(inst::ldr_w_unsigned(
            XSCRATCH0,
            XSTATE,
            a32_cpsr_nzcv_offset(),
        ))?;
        code.write_u32(inst::and_w_imm(XSCRATCH0, XSCRATCH0, 0x1000_0000))?;
        code.write_u32(inst::orr_w(XSCRATCH0, XSCRATCH0, nz_reg))?;
        code.write_u32(inst::orr_w(XSCRATCH0, XSCRATCH0, c_reg))?;
        code.write_u32(inst::str_w_unsigned(
            XSCRATCH0,
            XSTATE,
            a32_cpsr_nzcv_offset(),
        ))?;
    }

    Ok(())
}

pub fn emit_a32_get_c_flag(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let mut flag = ctx.reg_alloc.write_w(inst_ref);
    let flag_reg = flag.realize(code, ctx.block)? as u8;
    code.write_u32(inst::ldr_w_unsigned(
        flag_reg,
        XSTATE,
        a32_cpsr_nzcv_offset(),
    ))?;
    code.write_u32(inst::and_w_imm(flag_reg, flag_reg, 1 << 29))?;
    Ok(())
}

pub fn emit_a32_or_q_flag(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut flag = ctx.reg_alloc.read_w(args[0]);
    let flag_reg = flag.realize(code, ctx.block)? as u8;

    code.write_u32(inst::ldr_w_unsigned(XSCRATCH0, XSTATE, a32_cpsr_q_offset()))?;
    code.write_u32(inst::orr_w_lsl(XSCRATCH0, XSCRATCH0, flag_reg, 27))?;
    code.write_u32(inst::str_w_unsigned(XSCRATCH0, XSTATE, a32_cpsr_q_offset()))?;
    Ok(())
}

pub fn emit_a32_get_ge_flags(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let mut ge = ctx.reg_alloc.write_s(inst_ref);
    let ge_reg = ge.realize(code, ctx.block)? as u8;
    code.write_u32(inst::ldr_s_unsigned(ge_reg, XSTATE, a32_cpsr_ge_offset()))?;
    Ok(())
}

pub fn emit_a32_set_ge_flags(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut ge = ctx.reg_alloc.read_s(args[0]);
    let ge_reg = ge.realize(code, ctx.block)? as u8;
    code.write_u32(inst::str_s_unsigned(ge_reg, XSTATE, a32_cpsr_ge_offset()))?;
    Ok(())
}

pub fn emit_a32_set_ge_flags_compressed(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut ge = ctx.reg_alloc.read_w(args[0]);
    let ge_reg = ge.realize(code, ctx.block)? as u8;

    code.write_u32(inst::lsr_w_imm(XSCRATCH0, ge_reg, 16))?;
    emit_mov_w_imm(code, XSCRATCH1, 0x0020_4081)?;
    code.write_u32(inst::mul_w(XSCRATCH0, XSCRATCH0, XSCRATCH1))?;
    code.write_u32(inst::and_w_imm(XSCRATCH0, XSCRATCH0, 0x0101_0101))?;
    code.write_u32(inst::lsl_w_imm(XSCRATCH1, XSCRATCH0, 8))?;
    code.write_u32(inst::sub_w_reg(XSCRATCH0, XSCRATCH1, XSCRATCH0))?;
    code.write_u32(inst::str_w_unsigned(
        XSCRATCH0,
        XSTATE,
        a32_cpsr_ge_offset(),
    ))?;
    Ok(())
}

pub fn emit_a32_bx_write_pc(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let upper_without_t = a32_upper_without_t(ctx.block.end_location);

    if args[0].is_immediate() {
        let new_pc = args[0].get_immediate_u32();
        let thumb = (new_pc & 1) != 0;
        let mask = if thumb { 0xffff_fffe } else { 0xffff_fffc };
        let new_upper = upper_without_t | u32::from(thumb);
        let combined = ((new_upper as u64) << 32) | ((new_pc & mask) as u64);
        emit_mov_x_imm(code, XSCRATCH0, combined)?;
        code.write_u32(inst::stur_x(
            XSCRATCH0,
            XSTATE,
            a32_pc_and_upper_offset() as i32,
        ))?;
        return Ok(());
    }

    let mut pc = ctx.reg_alloc.read_w(args[0]);
    let pc_reg = pc.realize(code, ctx.block)? as u8;
    drop(pc);
    ctx.reg_alloc.spill_flags(code)?;

    code.write_u32(inst::ands_w_imm(XSCRATCH0, pc_reg, 1))?;
    emit_mov_w_imm(code, XSCRATCH1, 3)?;
    code.write_u32(inst::csel_w(XSCRATCH1, XSCRATCH0, XSCRATCH1, Cond::NE))?;
    code.write_u32(inst::bic_w(XSCRATCH1, pc_reg, XSCRATCH1))?;
    emit_mov_w_imm(code, XSCRATCH0, upper_without_t)?;
    code.write_u32(inst::cinc_w(XSCRATCH0, XSCRATCH0, Cond::NE))?;
    code.write_u32(inst::stp_w_offset(
        XSCRATCH1,
        XSCRATCH0,
        XSTATE,
        a32_pc_and_upper_offset() as i32,
    ))?;
    Ok(())
}

pub fn emit_a32_update_upper_location_descriptor(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
) -> Result<(), String> {
    if ctx
        .block
        .instructions
        .iter()
        .any(|inst| inst.opcode == Opcode::A32BXWritePC)
    {
        return Ok(());
    }

    emit_set_upper_location_descriptor(code, ctx, ctx.block.end_location, ctx.block.location)
}

pub fn emit_a32_call_supervisor(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    ctx.reg_alloc
        .prepare_for_call(code, ctx.fpsr, [None, None, None, None])?;

    emit_a32_add_ticks_before_call(code, ctx)?;
    emit_mov_w_imm(code, XSCRATCH0, HaltReason::SVC.bits())?;
    code.write_u32(inst::stlr_w(XSCRATCH0, XHALT))?;
    emit_mov_w_imm(code, X1, args[0].get_immediate_u32())?;
    emit_relocation(code, ctx.emitted_block_info, LinkTarget::CallSVC)?;
    emit_a32_get_ticks_remaining_after_call(code, ctx)
}

pub fn emit_a32_exception_raised(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    ctx.reg_alloc
        .prepare_for_call(code, ctx.fpsr, [None, None, None, None])?;

    emit_a32_add_ticks_before_call(code, ctx)?;
    emit_mov_w_imm(code, X1, args[0].get_immediate_u32())?;
    emit_mov_w_imm(code, X2, args[1].get_immediate_u64() as u32)?;
    emit_relocation(code, ctx.emitted_block_info, LinkTarget::ExceptionRaised)?;
    emit_a32_get_ticks_remaining_after_call(code, ctx)
}

pub fn emit_a32_data_synchronization_barrier(code: &mut BlockOfCode) -> Result<(), String> {
    code.write_u32(inst::dsb_sy())?;
    Ok(())
}

pub fn emit_a32_data_memory_barrier(code: &mut BlockOfCode) -> Result<(), String> {
    code.write_u32(inst::dmb_sy())?;
    Ok(())
}

pub fn emit_a32_instruction_synchronization_barrier(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
) -> Result<(), String> {
    if !ctx.conf.hook_isb {
        return Ok(());
    }

    ctx.reg_alloc
        .prepare_for_call(code, ctx.fpsr, [None, None, None, None])?;
    emit_relocation(
        code,
        ctx.emitted_block_info,
        LinkTarget::InstructionSynchronizationBarrierRaised,
    )
}

pub fn emit_a32_get_fpscr(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let mut fpscr = ctx.reg_alloc.write_w(inst_ref);
    let fpscr_reg = fpscr.realize(code, ctx.block)? as u8;
    ctx.fpsr.spill(code)?;

    debug_assert_eq!(
        core::mem::offset_of!(A32JitState, fpsr) + core::mem::size_of::<u32>(),
        core::mem::offset_of!(A32JitState, fpsr_nzcv)
    );
    code.write_u32(inst::ldr_w_unsigned(
        fpscr_reg,
        XSTATE,
        a32_upper_location_descriptor_offset(),
    ))?;
    code.write_u32(inst::ldp_w_offset(
        XSCRATCH0,
        XSCRATCH1,
        XSTATE,
        a32_fpsr_offset() as i32,
    ))?;
    emit_mov_w_imm(code, XSCRATCH2, 0xffff_0000)?;
    code.write_u32(inst::and_w_reg(fpscr_reg, fpscr_reg, XSCRATCH2))?;
    code.write_u32(inst::orr_w(XSCRATCH0, XSCRATCH0, XSCRATCH1))?;
    code.write_u32(inst::orr_w(fpscr_reg, fpscr_reg, XSCRATCH0))?;
    Ok(())
}

pub fn emit_a32_set_fpscr(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut fpscr = ctx.reg_alloc.read_w(args[0]);
    let fpscr_reg = fpscr.realize(code, ctx.block)? as u8;
    ctx.fpsr.overwrite();

    debug_assert_eq!(
        core::mem::offset_of!(A32JitState, fpsr) + core::mem::size_of::<u32>(),
        core::mem::offset_of!(A32JitState, fpsr_nzcv)
    );
    code.write_u32(inst::ldr_w_unsigned(
        XSCRATCH0,
        XSTATE,
        a32_upper_location_descriptor_offset(),
    ))?;
    emit_mov_w_imm(code, XSCRATCH1, 0x07f7_0000)?;
    code.write_u32(inst::and_w_reg(XSCRATCH1, fpscr_reg, XSCRATCH1))?;
    emit_mov_w_imm(code, XSCRATCH2, 0x0000_ffff)?;
    code.write_u32(inst::and_w_reg(XSCRATCH0, XSCRATCH0, XSCRATCH2))?;
    code.write_u32(inst::orr_w(XSCRATCH0, XSCRATCH0, XSCRATCH1))?;
    code.write_u32(inst::str_w_unsigned(
        XSCRATCH0,
        XSTATE,
        a32_upper_location_descriptor_offset(),
    ))?;

    emit_mov_w_imm(code, XSCRATCH0, 0x0800_009f)?;
    code.write_u32(inst::and_w_reg(XSCRATCH0, fpscr_reg, XSCRATCH0))?;
    code.write_u32(inst::and_w_imm(XSCRATCH1, fpscr_reg, 0xf000_0000))?;
    code.write_u32(inst::stp_w_offset(
        XSCRATCH0,
        XSCRATCH1,
        XSTATE,
        a32_fpsr_offset() as i32,
    ))?;
    Ok(())
}

pub fn emit_a32_get_fpscr_nzcv(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let mut nzcv = ctx.reg_alloc.write_w(inst_ref);
    let nzcv_reg = nzcv.realize(code, ctx.block)? as u8;
    code.write_u32(inst::ldr_w_unsigned(
        nzcv_reg,
        XSTATE,
        a32_fpsr_nzcv_offset(),
    ))?;
    Ok(())
}

pub fn emit_a32_set_fpscr_nzcv(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut nzcv = ctx.reg_alloc.read_w(args[0]);
    let nzcv_reg = nzcv.realize(code, ctx.block)? as u8;
    code.write_u32(inst::str_w_unsigned(
        nzcv_reg,
        XSTATE,
        a32_fpsr_nzcv_offset(),
    ))?;
    Ok(())
}

fn ensure_valid_reg(reg: Reg) -> Result<(), String> {
    if reg.is_valid() {
        Ok(())
    } else {
        Err(format!("Invalid A32 register: {reg:?}"))
    }
}

fn a32_reg_offset(reg: Reg) -> u32 {
    core::mem::offset_of!(A32JitState, regs) as u32
        + core::mem::size_of::<u32>() as u32 * reg.number() as u32
}

fn a32_pc_and_upper_offset() -> u32 {
    core::mem::offset_of!(A32JitState, regs) as u32 + core::mem::size_of::<u32>() as u32 * 15
}

fn a32_upper_location_descriptor_offset() -> u32 {
    core::mem::offset_of!(A32JitState, upper_location_descriptor) as u32
}

fn a32_upper_without_t(location: LocationDescriptor) -> u32 {
    ((A32LocationDescriptor::from_location(location)
        .set_single_stepping(false)
        .unique_hash()
        >> 32) as u32)
        & 0xffff_fffe
}

fn a32_cpsr_nzcv_offset() -> u32 {
    core::mem::offset_of!(A32JitState, cpsr_nzcv) as u32
}

fn a32_cpsr_q_offset() -> u32 {
    core::mem::offset_of!(A32JitState, cpsr_q) as u32
}

fn a32_cpsr_jaifm_offset() -> u32 {
    core::mem::offset_of!(A32JitState, cpsr_jaifm) as u32
}

fn a32_cpsr_ge_offset() -> u32 {
    core::mem::offset_of!(A32JitState, cpsr_ge) as u32
}

fn a32_fpsr_offset() -> u32 {
    core::mem::offset_of!(A32JitState, fpsr) as u32
}

fn a32_fpsr_nzcv_offset() -> u32 {
    core::mem::offset_of!(A32JitState, fpsr_nzcv) as u32
}

fn ensure_single_ext_reg(reg: ExtReg) -> Result<(), String> {
    if reg.is_single() {
        Ok(())
    } else {
        Err(format!("Expected A32 single extension register: {reg:?}"))
    }
}

fn ensure_double_ext_reg(reg: ExtReg) -> Result<(), String> {
    if reg.is_double() {
        Ok(())
    } else {
        Err(format!("Expected A32 double extension register: {reg:?}"))
    }
}

fn ensure_double_or_quad_ext_reg(reg: ExtReg) -> Result<(), String> {
    if reg.is_double() || reg.is_quad() {
        Ok(())
    } else {
        Err(format!(
            "Expected A32 double or quad extension register: {reg:?}"
        ))
    }
}

fn a32_ext_reg_base_offset() -> u32 {
    core::mem::offset_of!(A32JitState, ext_regs) as u32
}

fn a32_ext_reg_single_offset(reg: ExtReg) -> u32 {
    a32_ext_reg_base_offset() + core::mem::size_of::<u32>() as u32 * reg.index() as u32
}

fn a32_ext_reg_double_offset(reg: ExtReg) -> u32 {
    a32_ext_reg_base_offset() + core::mem::size_of::<u64>() as u32 * reg.index() as u32
}

fn a32_ext_reg_quad_offset(reg: ExtReg) -> u32 {
    a32_ext_reg_base_offset() + 2 * core::mem::size_of::<u64>() as u32 * reg.index() as u32
}

pub fn emit_a32_terminal(code: &mut BlockOfCode, ctx: &mut EmitContext<'_>) -> Result<(), String> {
    let location = A32LocationDescriptor::from_location(ctx.block.location);
    emit_a32_terminal_inner(
        code,
        ctx,
        ctx.block.terminal.clone(),
        location.set_single_stepping(false).to_location(),
        location.single_stepping(),
    )
}

pub fn emit_a32_condition_failed_terminal(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
) -> Result<(), String> {
    let location = A32LocationDescriptor::from_location(ctx.block.location);
    let Some(condition_failed_location) = ctx.block.condition_failed_location else {
        return Err("A32 condition-failed terminal requested without location".to_string());
    };
    emit_a32_terminal_inner(
        code,
        ctx,
        Terminal::LinkBlock {
            next: condition_failed_location,
        },
        location.set_single_stepping(false).to_location(),
        location.single_stepping(),
    )
}

pub(crate) fn emit_a32_check_memory_abort(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    current_location: LocationDescriptor,
    end: &mut Label,
) -> Result<(), String> {
    if !ctx.conf.check_halt_on_memory_access {
        return Ok(());
    }

    let current_location = A32LocationDescriptor::from_location(current_location);
    code.write_u32(inst::ldar_x(XSCRATCH0, XHALT))?;
    code.write_u32(inst::tst_x_imm(
        XSCRATCH0,
        HaltReason::MEMORY_ABORT.bits() as u64,
    ))?;
    end.b_cond(code, Cond::EQ)?;
    emit_set_upper_location_descriptor(
        code,
        ctx,
        current_location.to_location(),
        ctx.block.location,
    )?;
    emit_mov_w_imm(code, XSCRATCH0, current_location.pc())?;
    code.write_u32(inst::str_w_unsigned(
        XSCRATCH0,
        XSTATE,
        core::mem::offset_of!(A32JitState, regs) as u32 + core::mem::size_of::<u32>() as u32 * 15,
    ))?;
    emit_relocation(code, ctx.emitted_block_info, LinkTarget::ReturnFromRunCode)
}

fn emit_a32_terminal_inner(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    terminal: Terminal,
    initial_location: LocationDescriptor,
    is_single_step: bool,
) -> Result<(), String> {
    match terminal {
        Terminal::Invalid => Err("Invalid A32 terminal".to_string()),
        Terminal::ReturnToDispatch => {
            emit_relocation(code, ctx.emitted_block_info, LinkTarget::ReturnToDispatcher)
        }
        Terminal::LinkBlock { next } => {
            emit_set_upper_location_descriptor(code, ctx, next, initial_location)?;
            if ctx.conf.has_optimization(OptimizationFlag::BLOCK_LINKING) && !is_single_step {
                emit_guarded_block_link_relocation(code, ctx, next)?;
            }
            emit_set_pc_and_return_to_dispatcher(code, ctx, next)
        }
        Terminal::LinkBlockFast { next } => {
            emit_set_upper_location_descriptor(code, ctx, next, initial_location)?;
            if ctx.conf.has_optimization(OptimizationFlag::BLOCK_LINKING) && !is_single_step {
                emit_block_link_relocation(
                    code,
                    ctx.emitted_block_info,
                    next,
                    BlockRelocationType::Branch,
                )?;
            }
            emit_set_pc_and_return_to_dispatcher(code, ctx, next)
        }
        Terminal::PopRSBHint => {
            if ctx
                .conf
                .has_optimization(OptimizationFlag::RETURN_STACK_BUFFER)
                && !is_single_step
            {
                emit_pop_rsb_hint(code)?;
            }
            emit_relocation(code, ctx.emitted_block_info, LinkTarget::ReturnToDispatcher)
        }
        Terminal::FastDispatchHint => {
            emit_relocation(code, ctx.emitted_block_info, LinkTarget::ReturnToDispatcher)
        }
        Terminal::If { cond, then_, else_ } => {
            let emit_cond = ctx.conf.emit_cond;
            let pass_branch_offset = emit_cond(code, ctx, cond)?;
            emit_a32_terminal_inner(code, ctx, *else_, initial_location, is_single_step)?;
            patch_branch_to_current(code, pass_branch_offset, |pc_offset| {
                inst::b_cond(cond, pc_offset)
            })?;
            emit_a32_terminal_inner(code, ctx, *then_, initial_location, is_single_step)
        }
        Terminal::CheckBit { then_, else_ } => {
            code.write_u32(inst::ldrb_w_unsigned(
                XSCRATCH0,
                31,
                crate::backend::arm64::stack_layout::StackLayout::check_bit_offset() as u32,
            ))?;
            let fail_branch_offset = code.write_u32(inst::cbz_w(XSCRATCH0, 0))?;
            emit_a32_terminal_inner(code, ctx, *then_, initial_location, is_single_step)?;
            patch_branch_to_current(code, fail_branch_offset, |pc_offset| {
                inst::cbz_w(XSCRATCH0, pc_offset)
            })?;
            emit_a32_terminal_inner(code, ctx, *else_, initial_location, is_single_step)
        }
        Terminal::CheckHalt { else_ } => {
            code.write_u32(inst::ldar_w(XSCRATCH0, XHALT))?;
            let fail_branch_offset = code.write_u32(inst::cbnz_w(XSCRATCH0, 0))?;
            emit_a32_terminal_inner(code, ctx, *else_, initial_location, is_single_step)?;
            patch_branch_to_current(code, fail_branch_offset, |pc_offset| {
                inst::cbnz_w(XSCRATCH0, pc_offset)
            })?;
            emit_relocation(code, ctx.emitted_block_info, LinkTarget::ReturnToDispatcher)
        }
    }
}

fn emit_pop_rsb_hint(code: &mut BlockOfCode) -> Result<(), String> {
    code.write_u32(inst::ldr_w_unsigned(
        XSCRATCH2,
        31,
        StackLayout::rsb_ptr_offset() as u32,
    ))?;
    code.write_u32(inst::and_w_imm(XSCRATCH2, XSCRATCH2, RSB_INDEX_MASK as u32))?;
    code.write_u32(inst::add_x_reg_sp(X2, 31, XSCRATCH2))?;
    code.write_u32(inst::sub_w_imm(
        XSCRATCH2,
        XSCRATCH2,
        core::mem::size_of::<RSBEntry>() as u32,
    ))?;
    code.write_u32(inst::str_w_unsigned(
        XSCRATCH2,
        31,
        StackLayout::rsb_ptr_offset() as u32,
    ))?;
    code.write_u32(inst::ldp_x_offset(
        XSCRATCH0,
        XSCRATCH1,
        X2,
        StackLayout::rsb_offset() as i32,
    ))?;

    debug_assert_eq!(
        core::mem::offset_of!(A32JitState, regs) + 16 * core::mem::size_of::<u32>(),
        core::mem::offset_of!(A32JitState, upper_location_descriptor)
    );
    code.write_u32(inst::ldur_x(
        X0,
        XSTATE,
        core::mem::offset_of!(A32JitState, regs) as i32 + 15 * core::mem::size_of::<u32>() as i32,
    ))?;
    code.write_u32(inst::cmp_x_reg(X0, XSCRATCH0))?;
    let fail_branch_offset = code.write_u32(inst::b_cond(Cond::NE, 0))?;
    code.write_u32(inst::br(XSCRATCH1))?;
    patch_branch_to_current(code, fail_branch_offset, |pc_offset| {
        inst::b_cond(Cond::NE, pc_offset)
    })
}

pub(crate) fn emit_a32_cond(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    cond: Cond,
) -> Result<usize, String> {
    code.write_u32(inst::ldr_w_unsigned(
        XSCRATCH0,
        XSTATE,
        ctx.conf.state_nzcv_offset as u32,
    ))?;
    code.write_u32(inst::msr_nzcv(XSCRATCH0))?;
    code.write_u32(inst::b_cond(cond, 0))
}

fn emit_guarded_block_link_relocation(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    next: LocationDescriptor,
) -> Result<(), String> {
    let branch_offset = if ctx.conf.enable_cycle_counting {
        code.write_u32(inst::cmp_x_imm(XTICKS, 0))?;
        code.write_u32(inst::b_cond(Cond::LE, 0))?
    } else {
        code.write_u32(inst::ldar_w(XSCRATCH0, XHALT))?;
        code.write_u32(inst::cbnz_w(XSCRATCH0, 0))?
    };
    emit_block_link_relocation(
        code,
        ctx.emitted_block_info,
        next,
        BlockRelocationType::Branch,
    )?;

    let fail_offset = code.code_size();
    let pc_offset = i32::try_from(fail_offset as isize - branch_offset as isize)
        .map_err(|_| "A32 LinkBlock guard branch offset overflow".to_string())?;
    let patched_guard = if ctx.conf.enable_cycle_counting {
        inst::b_cond(Cond::LE, pc_offset)
    } else {
        inst::cbnz_w(XSCRATCH0, pc_offset)
    };
    code.patch_u32(branch_offset, patched_guard)?;
    Ok(())
}

fn patch_branch_to_current(
    code: &mut BlockOfCode,
    branch_offset: usize,
    encode: impl FnOnce(i32) -> u32,
) -> Result<(), String> {
    let target_offset = code.code_size();
    let pc_offset = i32::try_from(target_offset as isize - branch_offset as isize)
        .map_err(|_| "A32 terminal branch offset overflow".to_string())?;
    code.patch_u32(branch_offset, encode(pc_offset))
}

fn emit_set_pc_and_return_to_dispatcher(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    next: LocationDescriptor,
) -> Result<(), String> {
    let next = A32LocationDescriptor::from_location(next);
    emit_mov_w_imm(code, XSCRATCH0, next.pc())?;
    code.write_u32(inst::str_w_unsigned(
        XSCRATCH0,
        XSTATE,
        core::mem::offset_of!(A32JitState, regs) as u32 + core::mem::size_of::<u32>() as u32 * 15,
    ))?;
    emit_relocation(code, ctx.emitted_block_info, LinkTarget::ReturnToDispatcher)
}

fn emit_set_upper_location_descriptor(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    new_location: LocationDescriptor,
    old_location: LocationDescriptor,
) -> Result<(), String> {
    let get_upper = |desc: LocationDescriptor| {
        A32LocationDescriptor::from_location(desc)
            .set_single_stepping(false)
            .unique_hash()
            >> 32
    };
    let old_upper = get_upper(old_location) as u32;
    let mut new_upper = get_upper(new_location) as u32;
    if ctx.conf.always_little_endian {
        new_upper &= !0x2;
    }
    if old_upper != new_upper {
        emit_mov_w_imm(code, XSCRATCH0, new_upper)?;
        code.write_u32(inst::str_w_unsigned(
            XSCRATCH0,
            XSTATE,
            core::mem::offset_of!(A32JitState, upper_location_descriptor) as u32,
        ))?;
    }
    Ok(())
}

fn emit_a32_add_ticks_before_call(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
) -> Result<(), String> {
    if !ctx.conf.enable_cycle_counting {
        return Ok(());
    }

    code.write_u32(inst::ldr_x_unsigned(
        X1,
        31,
        StackLayout::cycles_to_run_offset() as u32,
    ))?;
    code.write_u32(inst::sub_x_reg(X1, X1, XTICKS))?;
    emit_relocation(code, ctx.emitted_block_info, LinkTarget::AddTicks)
}

fn emit_a32_get_ticks_remaining_after_call(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
) -> Result<(), String> {
    if !ctx.conf.enable_cycle_counting {
        return Ok(());
    }

    emit_relocation(code, ctx.emitted_block_info, LinkTarget::GetTicksRemaining)?;
    code.write_u32(inst::str_x_unsigned(
        X0,
        31,
        StackLayout::cycles_to_run_offset() as u32,
    ))?;
    code.write_u32(inst::mov_x(XTICKS, X0))?;
    Ok(())
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
        let part = ((imm >> shift) & 0xffff) as u16;
        if part != 0 {
            code.write_u32(inst::movk_x(reg, part, shift as u8))?;
        }
    }
    Ok(())
}

const X0: u8 = 0;
const X1: u8 = 1;
const X2: u8 = 2;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::arm64::emit_arm64::{
        BlockRelocation, EmitConfig, EmittedBlockInfo, Relocation,
    };
    use crate::backend::arm64::fastmem::FastmemManager;
    use crate::backend::arm64::fpsr_manager::FpsrManager;
    use crate::backend::arm64::reg_alloc::RegAlloc;
    use crate::frontend::a32::fpscr::FPSCR;
    use crate::frontend::a32::psr::PSR;
    use crate::frontend::a32::types::{ExtReg, Reg};
    use crate::ir::block::Block;
    use crate::ir::opcode::Opcode;
    use crate::ir::value::Value;
    use crate::jit_config::{JitConfig, UserCallbacks};
    use std::collections::HashMap;

    struct DummyCallbacks;

    impl UserCallbacks for DummyCallbacks {
        fn memory_read_code(&self, _vaddr: u64) -> Option<u32> {
            None
        }
        fn memory_read_8(&self, _vaddr: u64) -> u8 {
            0
        }
        fn memory_read_16(&self, _vaddr: u64) -> u16 {
            0
        }
        fn memory_read_32(&self, _vaddr: u64) -> u32 {
            0
        }
        fn memory_read_64(&self, _vaddr: u64) -> u64 {
            0
        }
        fn memory_read_128(&self, _vaddr: u64) -> (u64, u64) {
            (0, 0)
        }
        fn memory_write_8(&mut self, _vaddr: u64, _value: u8) {}
        fn memory_write_16(&mut self, _vaddr: u64, _value: u16) {}
        fn memory_write_32(&mut self, _vaddr: u64, _value: u32) {}
        fn memory_write_64(&mut self, _vaddr: u64, _value: u64) {}
        fn memory_write_128(&mut self, _vaddr: u64, _value_lo: u64, _value_hi: u64) {}
        fn exclusive_write_8(&mut self, _vaddr: u64, _value: u8, _expected: u8) -> bool {
            false
        }
        fn exclusive_write_16(&mut self, _vaddr: u64, _value: u16, _expected: u16) -> bool {
            false
        }
        fn exclusive_write_32(&mut self, _vaddr: u64, _value: u32, _expected: u32) -> bool {
            false
        }
        fn exclusive_write_64(&mut self, _vaddr: u64, _value: u64, _expected: u64) -> bool {
            false
        }
        fn exclusive_write_128(
            &mut self,
            _vaddr: u64,
            _value_lo: u64,
            _value_hi: u64,
            _expected_lo: u64,
            _expected_hi: u64,
        ) -> bool {
            false
        }
        fn call_supervisor(&mut self, _svc_num: u32) {}
        fn exception_raised(&mut self, _pc: u64, _exception: u64) {}
        fn add_ticks(&mut self, _ticks: u64) {}
        fn get_ticks_remaining(&self) -> u64 {
            0
        }
    }

    fn config_with(optimizations: OptimizationFlag, enable_cycle_counting: bool) -> JitConfig {
        JitConfig {
            coprocessors: JitConfig::default_coprocessors(),
            callbacks: Box::new(DummyCallbacks),
            enable_cycle_counting,
            code_cache_size: 0,
            optimizations,
            unsafe_optimizations: false,
            global_monitor: None,
            fastmem_pointer: None,
            page_table_pointer: None,
            define_unpredictable_behaviour: false,
            arch_version: crate::interface::a32::arch_version::ArchVersion::V8,
            hook_hint_instructions: false,
            processor_id: 0,
            wall_clock_cntpct: false,
            cntfrq_el0: 600_000_000,
            ctr_el0: 0x8444_c004,
            dczid_el0: 4,
            hook_data_cache_operations: false,
            hook_isb: false,
            tpidrro_el0: None,
            tpidr_el0: None,
            memory: Default::default(),
        }
    }

    fn config() -> JitConfig {
        config_with(OptimizationFlag::NO_OPTIMIZATIONS, false)
    }

    fn config_with_memory_abort_check() -> JitConfig {
        let mut config = config();
        config.memory.check_halt_on_memory_access = true;
        config
    }

    fn emitted_words(code: &BlockOfCode) -> Vec<u32> {
        (0..code.code_size() / 4)
            .map(|index| unsafe {
                code.code_base_ptr()
                    .add(index * 4)
                    .cast::<u32>()
                    .read_unaligned()
            })
            .collect()
    }

    fn test_gpr(index: usize) -> u8 {
        crate::backend::arm64::abi::GPR_ORDER[index] as u8
    }

    fn test_fpr(index: usize) -> u8 {
        crate::backend::arm64::abi::FPR_ORDER[index] as u8
    }

    fn with_context(
        block: &mut Block,
        code: &mut BlockOfCode,
        f: impl FnOnce(&mut BlockOfCode, &mut EmitContext<'_>),
    ) -> EmittedBlockInfo {
        with_context_config(block, code, config(), f)
    }

    fn with_context_config(
        block: &mut Block,
        code: &mut BlockOfCode,
        config: JitConfig,
        f: impl FnOnce(&mut BlockOfCode, &mut EmitContext<'_>),
    ) -> EmittedBlockInfo {
        with_context_config_mut(block, code, config, |_| {}, f)
    }

    fn with_context_config_mut(
        block: &mut Block,
        code: &mut BlockOfCode,
        config: JitConfig,
        mutate_conf: impl FnOnce(&mut EmitConfig),
        f: impl FnOnce(&mut BlockOfCode, &mut EmitContext<'_>),
    ) -> EmittedBlockInfo {
        let mut conf = EmitConfig::from_a32_config(&config);
        mutate_conf(&mut conf);
        let mut reg_alloc = RegAlloc::default();
        let mut info = EmittedBlockInfo {
            entry_point: code.code_base_ptr(),
            size: 0,
            relocations: Vec::new(),
            block_relocations: crate::backend::arm64::fast_hash::FastHashMap::default(),
            fastmem_patch_info: crate::backend::arm64::fast_hash::FastHashMap::default(),
        };
        let mut fpsr = FpsrManager::default();
        let mut fastmem = FastmemManager::default();
        {
            let mut ctx = EmitContext {
                block,
                reg_alloc: &mut reg_alloc,
                conf: &conf,
                emitted_block_info: &mut info,
                fpsr: &mut fpsr,
                fastmem: &mut fastmem,
                deferred_emits: Vec::new(),
            };
            f(code, &mut ctx);
        }
        info
    }

    #[test]
    fn return_to_dispatch_terminal_emits_relocation_placeholder() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x1000).to_location());
        block.terminal = Terminal::ReturnToDispatch;

        let info = with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_terminal(code, ctx).unwrap();
        });

        assert_eq!(emitted_words(&code), vec![inst::nop()]);
        assert_eq!(
            info.relocations,
            vec![Relocation {
                code_offset: 0,
                target: LinkTarget::ReturnToDispatcher,
            }]
        );
    }

    #[test]
    fn check_memory_abort_emits_upstream_abort_path_when_enabled() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x1000).to_location());
        let current_location = A32LocationDescriptor::at(0x2004).to_location();

        let info = with_context_config(
            &mut block,
            &mut code,
            config_with_memory_abort_check(),
            |code, ctx| {
                let mut end = Label::new();
                emit_a32_check_memory_abort(code, ctx, current_location, &mut end).unwrap();
                end.bind(code).unwrap();
            },
        );

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::ldar_x(XSCRATCH0, XHALT),
                inst::tst_x_imm(XSCRATCH0, HaltReason::MEMORY_ABORT.bits() as u64),
                inst::b_cond(Cond::EQ, 16),
                inst::movz_w(XSCRATCH0, 0x2004, 0),
                inst::str_w_unsigned(
                    XSCRATCH0,
                    XSTATE,
                    core::mem::offset_of!(A32JitState, regs) as u32
                        + core::mem::size_of::<u32>() as u32 * 15
                ),
                inst::nop(),
            ]
        );
        assert_eq!(info.relocations[0].code_offset, 20);
        assert_eq!(info.relocations[0].target, LinkTarget::ReturnFromRunCode);
    }

    #[test]
    fn check_memory_abort_emits_nothing_when_disabled() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x1000).to_location());
        let current_location = A32LocationDescriptor::at(0x2004).to_location();

        let info = with_context(&mut block, &mut code, |code, ctx| {
            let mut end = Label::new();
            emit_a32_check_memory_abort(code, ctx, current_location, &mut end).unwrap();
            end.bind(code).unwrap();
        });

        assert_eq!(emitted_words(&code), Vec::<u32>::new());
        assert!(info.relocations.is_empty());
    }

    #[test]
    fn link_block_fast_updates_pc_then_returns_to_dispatcher_without_block_linking() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(
            A32LocationDescriptor::new(0x1000, PSR::default(), FPSCR::default(), true)
                .to_location(),
        );
        let next = A32LocationDescriptor::at(0x1234_5678).to_location();
        block.terminal = Terminal::LinkBlockFast { next };

        let info = with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_terminal(code, ctx).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::movz_w(XSCRATCH0, 0x5678, 0),
                inst::movk_w(XSCRATCH0, 0x1234, 16),
                inst::str_w_unsigned(
                    XSCRATCH0,
                    XSTATE,
                    core::mem::offset_of!(A32JitState, regs) as u32
                        + core::mem::size_of::<u32>() as u32 * 15
                ),
                inst::nop(),
            ]
        );
        assert_eq!(info.relocations[0].code_offset, 12);
        assert_eq!(info.block_relocations.len(), 0);
    }

    #[test]
    fn link_block_updates_pc_then_returns_to_dispatcher_without_block_linking() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(
            A32LocationDescriptor::new(0x1000, PSR::default(), FPSCR::default(), true)
                .to_location(),
        );
        let next = A32LocationDescriptor::at(0x2004).to_location();
        block.terminal = Terminal::LinkBlock { next };

        let info = with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_terminal(code, ctx).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::movz_w(XSCRATCH0, 0x2004, 0),
                inst::str_w_unsigned(
                    XSCRATCH0,
                    XSTATE,
                    core::mem::offset_of!(A32JitState, regs) as u32
                        + core::mem::size_of::<u32>() as u32 * 15
                ),
                inst::nop(),
            ]
        );
        assert_eq!(info.relocations[0].code_offset, 8);
        assert_eq!(info.block_relocations.len(), 0);
    }

    #[test]
    fn link_block_with_block_linking_checks_halt_then_links_or_falls_back() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(
            A32LocationDescriptor::new(0x1000, PSR::default(), FPSCR::default(), false)
                .to_location(),
        );
        let next = A32LocationDescriptor::at(0x2004).to_location();
        block.terminal = Terminal::LinkBlock { next };

        let info = with_context_config(
            &mut block,
            &mut code,
            config_with(OptimizationFlag::BLOCK_LINKING, false),
            |code, ctx| {
                emit_a32_terminal(code, ctx).unwrap();
            },
        );

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::ldar_w(XSCRATCH0, XHALT),
                inst::cbnz_w(XSCRATCH0, 8),
                inst::nop(),
                inst::movz_w(XSCRATCH0, 0x2004, 0),
                inst::str_w_unsigned(
                    XSCRATCH0,
                    XSTATE,
                    core::mem::offset_of!(A32JitState, regs) as u32
                        + core::mem::size_of::<u32>() as u32 * 15
                ),
                inst::nop(),
            ]
        );
        assert_eq!(
            info.block_relocations[&next],
            vec![BlockRelocation {
                code_offset: 8,
                relocation_type: BlockRelocationType::Branch,
            }]
        );
        assert_eq!(info.relocations[0].code_offset, 20);
    }

    #[test]
    fn link_block_with_cycle_counting_checks_ticks_then_links_or_falls_back() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(
            A32LocationDescriptor::new(0x1000, PSR::default(), FPSCR::default(), false)
                .to_location(),
        );
        let next = A32LocationDescriptor::at(0x2004).to_location();
        block.terminal = Terminal::LinkBlock { next };

        let info = with_context_config(
            &mut block,
            &mut code,
            config_with(OptimizationFlag::BLOCK_LINKING, true),
            |code, ctx| {
                emit_a32_terminal(code, ctx).unwrap();
            },
        );

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::cmp_x_imm(XTICKS, 0),
                inst::b_cond(Cond::LE, 8),
                inst::nop(),
                inst::movz_w(XSCRATCH0, 0x2004, 0),
                inst::str_w_unsigned(
                    XSCRATCH0,
                    XSTATE,
                    core::mem::offset_of!(A32JitState, regs) as u32
                        + core::mem::size_of::<u32>() as u32 * 15
                ),
                inst::nop(),
            ]
        );
        assert_eq!(
            info.block_relocations[&next],
            vec![BlockRelocation {
                code_offset: 8,
                relocation_type: BlockRelocationType::Branch,
            }]
        );
        assert_eq!(info.relocations[0].code_offset, 20);
    }

    #[test]
    fn fast_dispatch_hint_returns_to_dispatcher_like_upstream_todo_path() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x1000).to_location());
        block.terminal = Terminal::FastDispatchHint;

        let info = with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_terminal(code, ctx).unwrap();
        });

        assert_eq!(emitted_words(&code), vec![inst::nop()]);
        assert_eq!(info.relocations[0].code_offset, 0);
        assert_eq!(info.relocations[0].target, LinkTarget::ReturnToDispatcher);
    }

    #[test]
    fn pop_rsb_hint_with_rsb_optimization_emits_upstream_prediction_path() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x1000).to_location());
        block.terminal = Terminal::PopRSBHint;

        let info = with_context_config(
            &mut block,
            &mut code,
            config_with(OptimizationFlag::RETURN_STACK_BUFFER, false),
            |code, ctx| {
                emit_a32_terminal(code, ctx).unwrap();
            },
        );

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::ldr_w_unsigned(XSCRATCH2, 31, StackLayout::rsb_ptr_offset() as u32),
                inst::and_w_imm(XSCRATCH2, XSCRATCH2, RSB_INDEX_MASK as u32),
                inst::add_x_reg_sp(X2, 31, XSCRATCH2),
                inst::sub_w_imm(
                    XSCRATCH2,
                    XSCRATCH2,
                    core::mem::size_of::<RSBEntry>() as u32
                ),
                inst::str_w_unsigned(XSCRATCH2, 31, StackLayout::rsb_ptr_offset() as u32),
                inst::ldp_x_offset(XSCRATCH0, XSCRATCH1, X2, StackLayout::rsb_offset() as i32),
                inst::ldur_x(
                    X0,
                    XSTATE,
                    core::mem::offset_of!(A32JitState, regs) as i32
                        + 15 * core::mem::size_of::<u32>() as i32
                ),
                inst::cmp_x_reg(X0, XSCRATCH0),
                inst::b_cond(Cond::NE, 8),
                inst::br(XSCRATCH1),
                inst::nop(),
            ]
        );
        assert_eq!(info.relocations[0].code_offset, 40);
        assert_eq!(info.relocations[0].target, LinkTarget::ReturnToDispatcher);
    }

    #[test]
    fn check_halt_branches_to_dispatcher_when_halted() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x1000).to_location());
        block.terminal = Terminal::CheckHalt {
            else_: Box::new(Terminal::ReturnToDispatch),
        };

        let info = with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_terminal(code, ctx).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::ldar_w(XSCRATCH0, XHALT),
                inst::cbnz_w(XSCRATCH0, 8),
                inst::nop(),
                inst::nop(),
            ]
        );
        assert_eq!(info.relocations[0].code_offset, 8);
        assert_eq!(info.relocations[1].code_offset, 12);
    }

    #[test]
    fn check_bit_branches_between_then_and_else_terminals() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x1000).to_location());
        block.terminal = Terminal::CheckBit {
            then_: Box::new(Terminal::ReturnToDispatch),
            else_: Box::new(Terminal::ReturnToDispatch),
        };

        let info = with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_terminal(code, ctx).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::ldrb_w_unsigned(
                    XSCRATCH0,
                    31,
                    crate::backend::arm64::stack_layout::StackLayout::check_bit_offset() as u32,
                ),
                inst::cbz_w(XSCRATCH0, 8),
                inst::nop(),
                inst::nop(),
            ]
        );
        assert_eq!(info.relocations[0].code_offset, 8);
        assert_eq!(info.relocations[1].code_offset, 12);
    }

    #[test]
    fn if_terminal_restores_nzcv_then_branches_to_then_terminal() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x1000).to_location());
        block.terminal = Terminal::If {
            cond: Cond::NE,
            then_: Box::new(Terminal::ReturnToDispatch),
            else_: Box::new(Terminal::ReturnToDispatch),
        };

        let info = with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_terminal(code, ctx).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::ldr_w_unsigned(
                    XSCRATCH0,
                    XSTATE,
                    core::mem::offset_of!(A32JitState, cpsr_nzcv) as u32,
                ),
                inst::msr_nzcv(XSCRATCH0),
                inst::b_cond(Cond::NE, 8),
                inst::nop(),
                inst::nop(),
            ]
        );
        assert_eq!(info.relocations[0].code_offset, 12);
        assert_eq!(info.relocations[1].code_offset, 16);
    }

    #[test]
    fn set_check_bit_immediate_true_sets_stack_check_bit() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x1000).to_location());
        let inst_ref = block.append(Opcode::A32SetCheckBit, &[Value::ImmU1(true)]);

        with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_set_check_bit(code, ctx, inst_ref).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::movz_w(XSCRATCH0, 1, 0),
                inst::strb_w_unsigned(
                    XSCRATCH0,
                    31,
                    crate::backend::arm64::stack_layout::StackLayout::check_bit_offset() as u32,
                ),
            ]
        );
    }

    #[test]
    fn set_check_bit_immediate_false_clears_stack_check_bit() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x1000).to_location());
        let inst_ref = block.append(Opcode::A32SetCheckBit, &[Value::ImmU1(false)]);

        with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_set_check_bit(code, ctx, inst_ref).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![inst::strb_w_unsigned(
                WZR,
                31,
                crate::backend::arm64::stack_layout::StackLayout::check_bit_offset() as u32,
            )]
        );
    }

    #[test]
    fn get_register_loads_from_a32_jitstate_regs() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x1000).to_location());
        let inst_ref = block.append(Opcode::A32GetRegister, &[Reg::R3.into()]);

        with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_get_register(code, ctx, inst_ref).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![inst::ldr_w_unsigned(
                test_gpr(0),
                XSTATE,
                a32_reg_offset(Reg::R3)
            )]
        );
    }

    #[test]
    fn set_register_stores_to_a32_jitstate_regs() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x1000).to_location());
        let inst_ref = block.append(
            Opcode::A32SetRegister,
            &[Reg::R4.into(), Value::ImmU32(0x1234)],
        );

        with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_set_register(code, ctx, inst_ref).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::movz_x(test_gpr(0), 0x1234, 0),
                inst::str_w_unsigned(test_gpr(0), XSTATE, a32_reg_offset(Reg::R4)),
            ]
        );
    }

    #[test]
    fn get_extended_register32_loads_s_from_a32_ext_regs() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x1000).to_location());
        let inst_ref = block.append(Opcode::A32GetExtendedRegister32, &[ExtReg::S3.into()]);

        with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_get_extended_register32(code, ctx, inst_ref).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![inst::ldr_s_unsigned(
                test_fpr(0),
                XSTATE,
                a32_ext_reg_single_offset(ExtReg::S3)
            )]
        );
    }

    #[test]
    fn get_extended_register64_loads_d_from_a32_ext_regs() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x1000).to_location());
        let inst_ref = block.append(Opcode::A32GetExtendedRegister64, &[ExtReg::D5.into()]);

        with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_get_extended_register64(code, ctx, inst_ref).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![inst::ldr_d_unsigned(
                test_fpr(0),
                XSTATE,
                a32_ext_reg_double_offset(ExtReg::D5)
            )]
        );
    }

    #[test]
    fn get_vector_loads_d_or_q_from_a32_ext_regs() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x1000).to_location());
        let get_d = block.append(Opcode::A32GetVector, &[ExtReg::D6.into()]);
        let get_q = block.append(Opcode::A32GetVector, &[ExtReg::Q2.into()]);

        with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_get_vector(code, ctx, get_d).unwrap();
            emit_a32_get_vector(code, ctx, get_q).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::ldr_d_unsigned(test_fpr(0), XSTATE, a32_ext_reg_double_offset(ExtReg::D6)),
                inst::ldr_q_unsigned(test_fpr(1), XSTATE, a32_ext_reg_quad_offset(ExtReg::Q2)),
            ]
        );
    }

    #[test]
    fn set_extended_register32_stores_s_to_a32_ext_regs() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x1000).to_location());
        let src = block.append(Opcode::A32GetExtendedRegister32, &[ExtReg::S0.into()]);
        let set = block.append(
            Opcode::A32SetExtendedRegister32,
            &[ExtReg::S7.into(), Value::Inst(src)],
        );

        with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_get_extended_register32(code, ctx, src).unwrap();
            emit_a32_set_extended_register32(code, ctx, set).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::ldr_s_unsigned(test_fpr(0), XSTATE, a32_ext_reg_single_offset(ExtReg::S0)),
                inst::str_s_unsigned(test_fpr(0), XSTATE, a32_ext_reg_single_offset(ExtReg::S7)),
            ]
        );
    }

    #[test]
    fn set_extended_register64_stores_d_to_a32_ext_regs() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x1000).to_location());
        let src = block.append(Opcode::A32GetExtendedRegister64, &[ExtReg::D1.into()]);
        let set = block.append(
            Opcode::A32SetExtendedRegister64,
            &[ExtReg::D8.into(), Value::Inst(src)],
        );

        with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_get_extended_register64(code, ctx, src).unwrap();
            emit_a32_set_extended_register64(code, ctx, set).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::ldr_d_unsigned(test_fpr(0), XSTATE, a32_ext_reg_double_offset(ExtReg::D1)),
                inst::str_d_unsigned(test_fpr(0), XSTATE, a32_ext_reg_double_offset(ExtReg::D8)),
            ]
        );
    }

    #[test]
    fn set_vector_stores_d_or_q_to_a32_ext_regs() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x1000).to_location());
        let src_d = block.append(Opcode::A32GetVector, &[ExtReg::D1.into()]);
        let set_d = block.append(
            Opcode::A32SetVector,
            &[ExtReg::D9.into(), Value::Inst(src_d)],
        );
        let src_q = block.append(Opcode::A32GetVector, &[ExtReg::Q1.into()]);
        let set_q = block.append(
            Opcode::A32SetVector,
            &[ExtReg::Q3.into(), Value::Inst(src_q)],
        );

        with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_get_vector(code, ctx, src_d).unwrap();
            emit_a32_set_vector(code, ctx, set_d).unwrap();
            emit_a32_get_vector(code, ctx, src_q).unwrap();
            emit_a32_set_vector(code, ctx, set_q).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::ldr_d_unsigned(test_fpr(0), XSTATE, a32_ext_reg_double_offset(ExtReg::D1)),
                inst::str_d_unsigned(test_fpr(0), XSTATE, a32_ext_reg_double_offset(ExtReg::D9)),
                inst::ldr_q_unsigned(test_fpr(1), XSTATE, a32_ext_reg_quad_offset(ExtReg::Q1)),
                inst::str_q_unsigned(test_fpr(1), XSTATE, a32_ext_reg_quad_offset(ExtReg::Q3)),
            ]
        );
    }

    #[test]
    fn get_cpsr_reconstructs_full_cpsr_from_split_state_like_upstream() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x1000).to_location());
        let get = block.append(Opcode::A32GetCpsr, &[]);

        with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_get_cpsr(code, ctx, get).unwrap();
        });

        let mut expected = vec![
            inst::ldp_w_offset(XSCRATCH0, XSCRATCH1, XSTATE, a32_cpsr_nzcv_offset() as i32),
            inst::ldr_w_unsigned(test_gpr(0), XSTATE, a32_cpsr_jaifm_offset()),
            inst::orr_w(test_gpr(0), test_gpr(0), XSCRATCH0),
            inst::orr_w(test_gpr(0), test_gpr(0), XSCRATCH1),
            inst::ldr_w_unsigned(XSCRATCH0, XSTATE, a32_cpsr_ge_offset()),
            inst::and_w_imm(XSCRATCH0, XSCRATCH0, 0x8080_8080),
        ];
        emit_expected_mov_w(&mut expected, XSCRATCH1, 0x0020_4081);
        expected.extend([
            inst::mul_w(XSCRATCH0, XSCRATCH0, XSCRATCH1),
            inst::and_w_imm(XSCRATCH0, XSCRATCH0, 0xf000_0000),
            inst::orr_w_lsr(test_gpr(0), test_gpr(0), XSCRATCH0, 12),
            inst::ldr_w_unsigned(XSCRATCH0, XSTATE, a32_upper_location_descriptor_offset()),
            inst::and_w_imm(XSCRATCH0, XSCRATCH0, 0b11),
            inst::orr_w_lsl(XSCRATCH0, XSCRATCH0, XSCRATCH0, 3),
            inst::and_w_imm(XSCRATCH0, XSCRATCH0, 0x1111_1111),
            inst::orr_w_lsl(test_gpr(0), test_gpr(0), XSCRATCH0, 5),
        ]);
        assert_eq!(emitted_words(&code), expected);
    }

    #[test]
    fn set_cpsr_decomposes_full_cpsr_into_split_state_like_upstream() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x1000).to_location());
        let src = block.append(Opcode::A32GetRegister, &[Reg::R0.into()]);
        let set = block.append(Opcode::A32SetCpsr, &[Value::Inst(src)]);

        with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_get_register(code, ctx, src).unwrap();
            emit_a32_set_cpsr(code, ctx, set).unwrap();
        });

        let mut expected = vec![
            inst::ldr_w_unsigned(test_gpr(0), XSTATE, a32_reg_offset(Reg::R0)),
            inst::and_w_imm(XSCRATCH0, test_gpr(0), 0xf000_0000),
            inst::and_w_imm(XSCRATCH1, test_gpr(0), 1 << 27),
            inst::stp_w_offset(XSCRATCH0, XSCRATCH1, XSTATE, a32_cpsr_nzcv_offset() as i32),
            inst::ubfx_w(XSCRATCH0, test_gpr(0), 16, 4),
        ];
        emit_expected_mov_w(&mut expected, XSCRATCH1, 0x0020_4081);
        expected.extend([
            inst::mul_w(XSCRATCH0, XSCRATCH0, XSCRATCH1),
            inst::and_w_imm(XSCRATCH0, XSCRATCH0, 0x0101_0101),
            inst::lsl_w_imm(XSCRATCH1, XSCRATCH0, 8),
            inst::sub_w_reg(XSCRATCH0, XSCRATCH1, XSCRATCH0),
        ]);
        emit_expected_mov_w(&mut expected, XSCRATCH1, 0x0100_01df);
        expected.extend([
            inst::and_w_reg(XSCRATCH1, test_gpr(0), XSCRATCH1),
            inst::stp_w_offset(XSCRATCH1, XSCRATCH0, XSTATE, a32_cpsr_jaifm_offset() as i32),
            inst::and_w_imm(XSCRATCH0, test_gpr(0), 0xfc00),
            inst::lsr_w_imm(XSCRATCH1, test_gpr(0), 17),
            inst::and_w_imm(XSCRATCH1, XSCRATCH1, 0x300),
            inst::orr_w(XSCRATCH0, XSCRATCH0, XSCRATCH1),
            inst::lsr_w_imm(XSCRATCH1, test_gpr(0), 8),
            inst::and_w_imm(XSCRATCH1, XSCRATCH1, 0x2),
            inst::orr_w(XSCRATCH0, XSCRATCH0, XSCRATCH1),
            inst::ldr_w_unsigned(XSCRATCH1, XSTATE, a32_upper_location_descriptor_offset()),
            inst::bfxil_w(XSCRATCH0, test_gpr(0), 5, 1),
            inst::and_w_imm(XSCRATCH1, XSCRATCH1, 0xffff_0000),
            inst::orr_w(XSCRATCH0, XSCRATCH0, XSCRATCH1),
            inst::str_w_unsigned(XSCRATCH0, XSTATE, a32_upper_location_descriptor_offset()),
        ]);
        assert_eq!(emitted_words(&code), expected);
    }

    #[test]
    fn set_cpsr_nzcv_stores_nzcv_to_a32_jitstate() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x1000).to_location());
        let src = block.append(Opcode::A32GetRegister, &[Reg::R0.into()]);
        let set = block.append(Opcode::A32SetCpsrNZCV, &[Value::Inst(src)]);

        with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_get_register(code, ctx, src).unwrap();
            emit_a32_set_cpsr_nzcv(code, ctx, set).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::ldr_w_unsigned(test_gpr(0), XSTATE, a32_reg_offset(Reg::R0)),
                inst::str_w_unsigned(test_gpr(0), XSTATE, a32_cpsr_nzcv_offset()),
            ]
        );
    }

    #[test]
    fn set_cpsr_nzcv_raw_stores_raw_nzcv_to_a32_jitstate() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x1000).to_location());
        let src = block.append(Opcode::A32GetRegister, &[Reg::R1.into()]);
        let set = block.append(Opcode::A32SetCpsrNZCVRaw, &[Value::Inst(src)]);

        with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_get_register(code, ctx, src).unwrap();
            emit_a32_set_cpsr_nzcv_raw(code, ctx, set).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::ldr_w_unsigned(test_gpr(0), XSTATE, a32_reg_offset(Reg::R1)),
                inst::str_w_unsigned(test_gpr(0), XSTATE, a32_cpsr_nzcv_offset()),
            ]
        );
    }

    #[test]
    fn set_cpsr_nzcvq_splits_nzcv_and_q_into_adjacent_state_words() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x1000).to_location());
        let src = block.append(Opcode::A32GetRegister, &[Reg::R2.into()]);
        let set = block.append(Opcode::A32SetCpsrNZCVQ, &[Value::Inst(src)]);

        with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_get_register(code, ctx, src).unwrap();
            emit_a32_set_cpsr_nzcvq(code, ctx, set).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::ldr_w_unsigned(test_gpr(0), XSTATE, a32_reg_offset(Reg::R2)),
                inst::and_w_imm(XSCRATCH0, test_gpr(0), 0xf000_0000),
                inst::and_w_imm(XSCRATCH1, test_gpr(0), 0x0800_0000),
                inst::stp_w_offset(XSCRATCH0, XSCRATCH1, XSTATE, a32_cpsr_nzcv_offset() as i32,),
            ]
        );
    }

    #[test]
    fn get_c_flag_loads_and_masks_cpsr_nzcv() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x1000).to_location());
        let inst_ref = block.append(Opcode::A32GetCFlag, &[]);

        with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_get_c_flag(code, ctx, inst_ref).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::ldr_w_unsigned(test_gpr(0), XSTATE, a32_cpsr_nzcv_offset()),
                inst::and_w_imm(test_gpr(0), test_gpr(0), 1 << 29),
            ]
        );
    }

    #[test]
    fn set_cpsr_nz_preserves_cv_and_ors_in_nz() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x1000).to_location());
        let src = block.append(Opcode::A32GetRegister, &[Reg::R3.into()]);
        let set = block.append(Opcode::A32SetCpsrNZ, &[Value::Inst(src)]);

        with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_get_register(code, ctx, src).unwrap();
            emit_a32_set_cpsr_nz(code, ctx, set).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::ldr_w_unsigned(test_gpr(0), XSTATE, a32_reg_offset(Reg::R3)),
                inst::ldr_w_unsigned(XSCRATCH0, XSTATE, a32_cpsr_nzcv_offset()),
                inst::and_w_imm(XSCRATCH0, XSCRATCH0, 0x3000_0000),
                inst::orr_w(XSCRATCH0, XSCRATCH0, test_gpr(0)),
                inst::str_w_unsigned(XSCRATCH0, XSTATE, a32_cpsr_nzcv_offset()),
            ]
        );
    }

    #[test]
    fn set_cpsr_nzc_empty_nz_immediate_carry_sets_carry_only() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x1000).to_location());
        let set = block.append(
            Opcode::A32SetCpsrNZC,
            &[Value::EmptyNZCVImmediateMarker, Value::ImmU1(true)],
        );

        with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_set_cpsr_nzc(code, ctx, set).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::ldr_w_unsigned(XSCRATCH0, XSTATE, a32_cpsr_nzcv_offset()),
                inst::and_w_imm(XSCRATCH0, XSCRATCH0, 0x1000_0000),
                inst::orr_w_imm(XSCRATCH0, XSCRATCH0, 0x2000_0000),
                inst::str_w_unsigned(XSCRATCH0, XSTATE, a32_cpsr_nzcv_offset()),
            ]
        );
    }

    #[test]
    fn set_cpsr_nzc_non_immediate_nz_preserves_v_and_optional_carry() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x1000).to_location());
        let nz = block.append(Opcode::A32GetRegister, &[Reg::R4.into()]);
        let set = block.append(
            Opcode::A32SetCpsrNZC,
            &[Value::Inst(nz), Value::ImmU1(false)],
        );

        with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_get_register(code, ctx, nz).unwrap();
            emit_a32_set_cpsr_nzc(code, ctx, set).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::ldr_w_unsigned(test_gpr(0), XSTATE, a32_reg_offset(Reg::R4)),
                inst::ldr_w_unsigned(XSCRATCH0, XSTATE, a32_cpsr_nzcv_offset()),
                inst::and_w_imm(XSCRATCH0, XSCRATCH0, 0x1000_0000),
                inst::orr_w(XSCRATCH0, XSCRATCH0, test_gpr(0)),
                inst::str_w_unsigned(XSCRATCH0, XSTATE, a32_cpsr_nzcv_offset()),
            ]
        );
    }

    #[test]
    fn or_q_flag_loads_shifts_and_stores_q() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x1000).to_location());
        let flag = block.append(Opcode::A32GetCFlag, &[]);
        let set = block.append(Opcode::A32OrQFlag, &[Value::Inst(flag)]);

        with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_get_c_flag(code, ctx, flag).unwrap();
            emit_a32_or_q_flag(code, ctx, set).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::ldr_w_unsigned(test_gpr(0), XSTATE, a32_cpsr_nzcv_offset()),
                inst::and_w_imm(test_gpr(0), test_gpr(0), 1 << 29),
                inst::ldr_w_unsigned(XSCRATCH0, XSTATE, a32_cpsr_q_offset()),
                inst::orr_w_lsl(XSCRATCH0, XSCRATCH0, test_gpr(0), 27),
                inst::str_w_unsigned(XSCRATCH0, XSTATE, a32_cpsr_q_offset()),
            ]
        );
    }

    #[test]
    fn ge_flags_load_and_store_s_register_state() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x1000).to_location());
        let ge = block.append(Opcode::A32GetGEFlags, &[]);
        let set = block.append(Opcode::A32SetGEFlags, &[Value::Inst(ge)]);

        with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_get_ge_flags(code, ctx, ge).unwrap();
            emit_a32_set_ge_flags(code, ctx, set).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::ldr_s_unsigned(test_fpr(0), XSTATE, a32_cpsr_ge_offset()),
                inst::str_s_unsigned(test_fpr(0), XSTATE, a32_cpsr_ge_offset()),
            ]
        );
    }

    #[test]
    fn set_ge_flags_compressed_expands_bits_to_byte_lanes() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x1000).to_location());
        let src = block.append(Opcode::A32GetRegister, &[Reg::R5.into()]);
        let set = block.append(Opcode::A32SetGEFlagsCompressed, &[Value::Inst(src)]);

        with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_get_register(code, ctx, src).unwrap();
            emit_a32_set_ge_flags_compressed(code, ctx, set).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::ldr_w_unsigned(test_gpr(0), XSTATE, a32_reg_offset(Reg::R5)),
                inst::lsr_w_imm(XSCRATCH0, test_gpr(0), 16),
                inst::movz_w(XSCRATCH1, 0x4081, 0),
                inst::movk_w(XSCRATCH1, 0x0020, 16),
                inst::mul_w(XSCRATCH0, XSCRATCH0, XSCRATCH1),
                inst::and_w_imm(XSCRATCH0, XSCRATCH0, 0x0101_0101),
                inst::lsl_w_imm(XSCRATCH1, XSCRATCH0, 8),
                inst::sub_w_reg(XSCRATCH0, XSCRATCH1, XSCRATCH0),
                inst::str_w_unsigned(XSCRATCH0, XSTATE, a32_cpsr_ge_offset()),
            ]
        );
    }

    #[test]
    fn bx_write_pc_immediate_stores_pc_and_upper_descriptor_together() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x2000).to_location());
        block.end_location = A32LocationDescriptor::at(0x2004)
            .set_t_flag(false)
            .set_single_stepping(true)
            .to_location();
        let bx = block.append(Opcode::A32BXWritePC, &[Value::ImmU32(0x1235)]);
        let upper_without_t = a32_upper_without_t(block.end_location);
        let combined = (((upper_without_t | 1) as u64) << 32) | 0x1234;

        with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_bx_write_pc(code, ctx, bx).unwrap();
        });

        let mut expected = Vec::new();
        emit_expected_mov_x(&mut expected, XSCRATCH0, combined);
        expected.push(inst::stur_x(
            XSCRATCH0,
            XSTATE,
            a32_pc_and_upper_offset() as i32,
        ));
        assert_eq!(emitted_words(&code), expected);
    }

    #[test]
    fn bx_write_pc_register_updates_pc_alignment_and_t_flag_like_upstream() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x2000).to_location());
        block.end_location = A32LocationDescriptor::at(0x2004)
            .set_t_flag(true)
            .to_location();
        let pc = block.append(Opcode::A32GetRegister, &[Reg::R6.into()]);
        let bx = block.append(Opcode::A32BXWritePC, &[Value::Inst(pc)]);
        let upper_without_t = a32_upper_without_t(block.end_location);

        with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_get_register(code, ctx, pc).unwrap();
            emit_a32_bx_write_pc(code, ctx, bx).unwrap();
        });

        let mut expected = vec![
            inst::ldr_w_unsigned(test_gpr(0), XSTATE, a32_reg_offset(Reg::R6)),
            inst::ands_w_imm(XSCRATCH0, test_gpr(0), 1),
            inst::movz_w(XSCRATCH1, 3, 0),
            inst::csel_w(XSCRATCH1, XSCRATCH0, XSCRATCH1, Cond::NE),
            inst::bic_w(XSCRATCH1, test_gpr(0), XSCRATCH1),
        ];
        emit_expected_mov_w(&mut expected, XSCRATCH0, upper_without_t);
        expected.extend([
            inst::cinc_w(XSCRATCH0, XSCRATCH0, Cond::NE),
            inst::stp_w_offset(
                XSCRATCH1,
                XSCRATCH0,
                XSTATE,
                a32_pc_and_upper_offset() as i32,
            ),
        ]);
        assert_eq!(emitted_words(&code), expected);
    }

    #[test]
    fn update_upper_location_descriptor_skips_when_bx_write_pc_exists() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(
            A32LocationDescriptor::at(0x2000)
                .set_t_flag(false)
                .to_location(),
        );
        block.end_location = A32LocationDescriptor::at(0x2004)
            .set_t_flag(true)
            .to_location();
        block.append(Opcode::A32BXWritePC, &[Value::ImmU32(0x3001)]);
        let update = block.append(Opcode::A32UpdateUpperLocationDescriptor, &[]);

        with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_update_upper_location_descriptor(code, ctx).unwrap();
            assert_eq!(
                ctx.block.get(update).opcode,
                Opcode::A32UpdateUpperLocationDescriptor
            );
        });

        assert!(emitted_words(&code).is_empty());
    }

    #[test]
    fn update_upper_location_descriptor_emits_when_end_upper_changes() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(
            A32LocationDescriptor::at(0x2000)
                .set_t_flag(false)
                .to_location(),
        );
        block.end_location = A32LocationDescriptor::at(0x2004)
            .set_t_flag(true)
            .to_location();

        with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_update_upper_location_descriptor(code, ctx).unwrap();
        });

        let new_upper = A32LocationDescriptor::from_location(block.end_location)
            .set_single_stepping(false)
            .unique_hash() as u64
            >> 32;
        let mut expected = Vec::new();
        emit_expected_mov_w(&mut expected, XSCRATCH0, new_upper as u32);
        expected.push(inst::str_w_unsigned(
            XSCRATCH0,
            XSTATE,
            core::mem::offset_of!(A32JitState, upper_location_descriptor) as u32,
        ));
        assert_eq!(emitted_words(&code), expected);
    }

    #[test]
    fn call_supervisor_emits_svc_relocation_with_immediate_in_w1() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x2000).to_location());
        let svc = block.append(Opcode::A32CallSupervisor, &[Value::ImmU32(0x42)]);

        let info = with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_call_supervisor(code, ctx, svc).unwrap();
        });

        let mut expected = Vec::new();
        emit_expected_mov_w(&mut expected, XSCRATCH0, HaltReason::SVC.bits());
        expected.push(inst::stlr_w(XSCRATCH0, XHALT));
        emit_expected_mov_w(&mut expected, X1, 0x42);
        expected.push(inst::nop());
        assert_eq!(emitted_words(&code), expected);
        assert_eq!(
            info.relocations,
            vec![Relocation {
                code_offset: 16,
                target: LinkTarget::CallSVC,
            }]
        );
    }

    #[test]
    fn call_supervisor_with_cycle_counting_wraps_callback_like_upstream() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x2000).to_location());
        let svc = block.append(Opcode::A32CallSupervisor, &[Value::ImmU32(0x42)]);

        let info = with_context_config(
            &mut block,
            &mut code,
            config_with(OptimizationFlag::NO_OPTIMIZATIONS, true),
            |code, ctx| {
                emit_a32_call_supervisor(code, ctx, svc).unwrap();
            },
        );

        let mut expected = vec![
            inst::ldr_x_unsigned(X1, 31, StackLayout::cycles_to_run_offset() as u32),
            inst::sub_x_reg(X1, X1, XTICKS),
            inst::nop(),
        ];
        emit_expected_mov_w(&mut expected, XSCRATCH0, HaltReason::SVC.bits());
        expected.push(inst::stlr_w(XSCRATCH0, XHALT));
        emit_expected_mov_w(&mut expected, X1, 0x42);
        expected.extend([
            inst::nop(),
            inst::nop(),
            inst::str_x_unsigned(X0, 31, StackLayout::cycles_to_run_offset() as u32),
            inst::mov_x(XTICKS, X0),
        ]);
        assert_eq!(emitted_words(&code), expected);
        assert_eq!(
            info.relocations,
            vec![
                Relocation {
                    code_offset: 8,
                    target: LinkTarget::AddTicks,
                },
                Relocation {
                    code_offset: 28,
                    target: LinkTarget::CallSVC,
                },
                Relocation {
                    code_offset: 32,
                    target: LinkTarget::GetTicksRemaining,
                },
            ]
        );
    }

    #[test]
    fn exception_raised_emits_pc_exception_args_and_relocation() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x2000).to_location());
        let exception = block.append(
            Opcode::A32ExceptionRaised,
            &[Value::ImmU32(0x1234), Value::ImmU64(0x20)],
        );

        let info = with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_exception_raised(code, ctx, exception).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::movz_w(X1, 0x1234, 0),
                inst::movz_w(X2, 0x20, 0),
                inst::nop(),
            ]
        );
        assert_eq!(
            info.relocations,
            vec![Relocation {
                code_offset: 8,
                target: LinkTarget::ExceptionRaised,
            }]
        );
    }

    #[test]
    fn barriers_emit_upstream_sy_barriers() {
        let mut code = BlockOfCode::with_size(4096).unwrap();

        emit_a32_data_synchronization_barrier(&mut code).unwrap();
        emit_a32_data_memory_barrier(&mut code).unwrap();

        assert_eq!(emitted_words(&code), vec![inst::dsb_sy(), inst::dmb_sy()]);
    }

    #[test]
    fn instruction_synchronization_barrier_respects_hook_flag() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x2000).to_location());
        let isb = block.append(Opcode::A32InstructionSynchronizationBarrier, &[]);

        let info = with_context(&mut block, &mut code, |code, ctx| {
            assert_eq!(
                ctx.block.get(isb).opcode,
                Opcode::A32InstructionSynchronizationBarrier
            );
            emit_a32_instruction_synchronization_barrier(code, ctx).unwrap();
        });
        assert!(emitted_words(&code).is_empty());
        assert!(info.relocations.is_empty());

        let info = with_context_config_mut(
            &mut block,
            &mut code,
            config(),
            |conf| conf.hook_isb = true,
            |code, ctx| {
                emit_a32_instruction_synchronization_barrier(code, ctx).unwrap();
            },
        );
        assert_eq!(emitted_words(&code), vec![inst::nop()]);
        assert_eq!(
            info.relocations,
            vec![Relocation {
                code_offset: 0,
                target: LinkTarget::InstructionSynchronizationBarrierRaised,
            }]
        );
    }

    #[test]
    fn fpscr_get_and_set_preserve_upstream_state_split() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x2000).to_location());
        let get = block.append(Opcode::A32GetFpscr, &[]);
        let set = block.append(Opcode::A32SetFpscr, &[Value::Inst(get)]);

        with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_get_fpscr(code, ctx, get).unwrap();
            emit_a32_set_fpscr(code, ctx, set).unwrap();
        });

        let mut expected = vec![
            inst::ldr_w_unsigned(test_gpr(0), XSTATE, a32_upper_location_descriptor_offset()),
            inst::ldp_w_offset(XSCRATCH0, XSCRATCH1, XSTATE, a32_fpsr_offset() as i32),
        ];
        emit_expected_mov_w(&mut expected, XSCRATCH2, 0xffff_0000);
        expected.extend([
            inst::and_w_reg(test_gpr(0), test_gpr(0), XSCRATCH2),
            inst::orr_w(XSCRATCH0, XSCRATCH0, XSCRATCH1),
            inst::orr_w(test_gpr(0), test_gpr(0), XSCRATCH0),
            inst::ldr_w_unsigned(XSCRATCH0, XSTATE, a32_upper_location_descriptor_offset()),
        ]);
        emit_expected_mov_w(&mut expected, XSCRATCH1, 0x07f7_0000);
        expected.push(inst::and_w_reg(XSCRATCH1, test_gpr(0), XSCRATCH1));
        emit_expected_mov_w(&mut expected, XSCRATCH2, 0x0000_ffff);
        expected.extend([
            inst::and_w_reg(XSCRATCH0, XSCRATCH0, XSCRATCH2),
            inst::orr_w(XSCRATCH0, XSCRATCH0, XSCRATCH1),
            inst::str_w_unsigned(XSCRATCH0, XSTATE, a32_upper_location_descriptor_offset()),
        ]);
        emit_expected_mov_w(&mut expected, XSCRATCH0, 0x0800_009f);
        expected.extend([
            inst::and_w_reg(XSCRATCH0, test_gpr(0), XSCRATCH0),
            inst::and_w_imm(XSCRATCH1, test_gpr(0), 0xf000_0000),
            inst::stp_w_offset(XSCRATCH0, XSCRATCH1, XSTATE, a32_fpsr_offset() as i32),
        ]);
        assert_eq!(emitted_words(&code), expected);
    }

    #[test]
    fn fpscr_nzcv_get_and_set_use_fpsr_nzcv_word() {
        let mut code = BlockOfCode::with_size(4096).unwrap();
        let mut block = Block::new(A32LocationDescriptor::at(0x2000).to_location());
        let get = block.append(Opcode::A32GetFpscrNZCV, &[]);
        let set = block.append(Opcode::A32SetFpscrNZCV, &[Value::Inst(get)]);

        with_context(&mut block, &mut code, |code, ctx| {
            emit_a32_get_fpscr_nzcv(code, ctx, get).unwrap();
            emit_a32_set_fpscr_nzcv(code, ctx, set).unwrap();
        });

        assert_eq!(
            emitted_words(&code),
            vec![
                inst::ldr_w_unsigned(test_gpr(0), XSTATE, a32_fpsr_nzcv_offset()),
                inst::str_w_unsigned(test_gpr(0), XSTATE, a32_fpsr_nzcv_offset()),
            ]
        );
    }

    fn emit_expected_mov_w(out: &mut Vec<u32>, reg: u8, imm: u32) {
        out.push(inst::movz_w(reg, (imm & 0xffff) as u16, 0));
        let high = (imm >> 16) as u16;
        if high != 0 {
            out.push(inst::movk_w(reg, high, 16));
        }
    }

    fn emit_expected_mov_x(out: &mut Vec<u32>, reg: u8, imm: u64) {
        out.push(inst::movz_x(reg, (imm & 0xffff) as u16, 0));
        for shift in [16, 32, 48] {
            let part = ((imm >> shift) & 0xffff) as u16;
            if part != 0 {
                out.push(inst::movk_x(reg, part, shift as u8));
            }
        }
    }
}
