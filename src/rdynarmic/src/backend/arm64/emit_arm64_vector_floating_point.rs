//! ARM64 floating-point vector emission.
//!
//! Upstream owner: `backend/arm64/emit_arm64_vector_floating_point.cpp`.

use crate::backend::arm64::abi::{
    emit_pop_registers, emit_push_registers, to_reg_list_vec, ABI_CALLER_SAVE, XSCRATCH0, XSTATE,
};
use crate::backend::arm64::block_of_code::BlockOfCode;
use crate::backend::arm64::emit_context::EmitContext;
use crate::backend::arm64::inst;
use crate::backend::arm64::reg_alloc::RegAlloc;
use crate::common::fp::fpcr::Fpcr as CommonFpcr;
use crate::common::fp::fpsr::Fpsr;
use crate::common::fp::op::fp_round_int::fp_round_int;
use crate::common::fp::rounding_mode::RoundingMode as CommonRoundingMode;
use crate::ir::opcode::Opcode;
use crate::ir::value::InstRef;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VectorFpSize {
    F32,
    F64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RoundingMode {
    ToNearestTieEven,
    TowardsPlusInfinity,
    TowardsMinusInfinity,
    TowardsZero,
    ToNearestTieAwayFromZero,
    ToOdd,
}

impl RoundingMode {
    fn from_u8(value: u8) -> Result<Self, String> {
        match value {
            0 => Ok(Self::ToNearestTieEven),
            1 => Ok(Self::TowardsPlusInfinity),
            2 => Ok(Self::TowardsMinusInfinity),
            3 => Ok(Self::TowardsZero),
            4 => Ok(Self::ToNearestTieAwayFromZero),
            5 => Ok(Self::ToOdd),
            _ => Err(format!("ARM64 FP vector: invalid rounding mode {value}")),
        }
    }
}

fn emit_mov_w_imm(code: &mut BlockOfCode, reg: u8, imm: u32) -> Result<(), String> {
    code.write_u32(inst::movz_w(reg, (imm & 0xffff) as u16, 0))?;
    let upper = ((imm >> 16) & 0xffff) as u16;
    if upper != 0 {
        code.write_u32(inst::movk_w(reg, upper, 16))?;
    }
    Ok(())
}

fn emit_mov_x_imm(code: &mut BlockOfCode, reg: u8, imm: u64) -> Result<(), String> {
    code.write_u32(inst::movz_x(reg, (imm & 0xffff) as u16, 0))?;
    for shift in [16, 32, 48] {
        let part = ((imm >> shift) & 0xffff) as u16;
        if part != 0 {
            code.write_u32(inst::movk_x(reg, part, shift))?;
        }
    }
    Ok(())
}

fn maybe_standard_fpcr(
    code: &mut BlockOfCode,
    ctx: &EmitContext<'_>,
    fpcr_controlled: bool,
    emit: impl FnOnce(&mut BlockOfCode) -> Result<(), String>,
) -> Result<(), String> {
    let current_fpcr = ctx.fpcr(true);
    let target_fpcr = ctx.fpcr(fpcr_controlled);
    if target_fpcr != current_fpcr {
        emit_mov_w_imm(code, XSCRATCH0, target_fpcr.value())?;
        code.write_u32(inst::msr_fpcr(XSCRATCH0))?;
        emit(code)?;
        emit_mov_w_imm(code, XSCRATCH0, current_fpcr.value())?;
        code.write_u32(inst::msr_fpcr(XSCRATCH0))?;
        return Ok(());
    }

    emit(code)
}

fn fpcr_rounding_mode(
    ctx: &EmitContext<'_>,
    fpcr_controlled: bool,
) -> Result<RoundingMode, String> {
    RoundingMode::from_u8(((ctx.fpcr(fpcr_controlled).value() >> 22) & 0b11) as u8)
}

fn emit_three_op_arranged(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    emit: impl FnOnce(u8, u8, u8) -> u32,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut a = ctx.reg_alloc.read_q(args[0]);
    let mut b = ctx.reg_alloc.read_q(args[1]);
    let fpcr_controlled = args[2].get_immediate_u1();
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut a, &mut b])?;
    ctx.fpsr.load(code)?;

    let result = result.index().expect("result realized") as u8;
    let a = a.index().expect("a realized") as u8;
    let b = b.index().expect("b realized") as u8;
    maybe_standard_fpcr(code, ctx, fpcr_controlled, |code| {
        code.write_u32(emit(result, a, b))?;
        Ok(())
    })
}

fn emit_two_op_arranged(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    emit: impl FnOnce(u8, u8) -> u32,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut a = ctx.reg_alloc.read_q(args[0]);
    let fpcr_controlled = args[1].is_void() || args[1].get_immediate_u1();
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut a])?;
    ctx.fpsr.load(code)?;

    let result = result.index().expect("result realized") as u8;
    let a = a.index().expect("a realized") as u8;
    maybe_standard_fpcr(code, ctx, fpcr_controlled, |code| {
        code.write_u32(emit(result, a))?;
        Ok(())
    })
}

pub fn emit_fp_vector_abs16(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.read_write_q(args[0], inst_ref);
    let result = result.realize(code, ctx.block)? as u8;
    code.write_u32(inst::bic_v8h_sign_bit(result))?;
    Ok(())
}

pub fn emit_fp_vector_abs32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_two_op_arranged(code, ctx, inst_ref, inst::fabs_v4s)
}

pub fn emit_fp_vector_abs64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_two_op_arranged(code, ctx, inst_ref, inst::fabs_v2d)
}

fn emit_round_int(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: VectorFpSize,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let rounding_mode = RoundingMode::from_u8(args[1].get_immediate_u8())?;
    let exact = args[2].get_immediate_u1();
    let fpcr_controlled = args[3].get_immediate_u1();

    if exact && fpcr_rounding_mode(ctx, fpcr_controlled)? != rounding_mode {
        return Err("ARM64 FP vector: exact round mode does not match FPCR".to_string());
    }
    if rounding_mode == RoundingMode::ToOdd {
        return Err("ARM64 FP vector: invalid round-to-odd mode".to_string());
    }

    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut operand = ctx.reg_alloc.read_q(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    ctx.fpsr.load(code)?;

    let result = result.index().expect("result realized") as u8;
    let operand = operand.index().expect("operand realized") as u8;
    maybe_standard_fpcr(code, ctx, fpcr_controlled, |code| {
        let instruction = match (size, exact, rounding_mode) {
            (VectorFpSize::F32, true, _) => inst::frintx_v4s(result, operand),
            (VectorFpSize::F64, true, _) => inst::frintx_v2d(result, operand),
            (VectorFpSize::F32, false, RoundingMode::ToNearestTieEven) => {
                inst::frintn_v4s(result, operand)
            }
            (VectorFpSize::F64, false, RoundingMode::ToNearestTieEven) => {
                inst::frintn_v2d(result, operand)
            }
            (VectorFpSize::F32, false, RoundingMode::TowardsPlusInfinity) => {
                inst::frintp_v4s(result, operand)
            }
            (VectorFpSize::F64, false, RoundingMode::TowardsPlusInfinity) => {
                inst::frintp_v2d(result, operand)
            }
            (VectorFpSize::F32, false, RoundingMode::TowardsMinusInfinity) => {
                inst::frintm_v4s(result, operand)
            }
            (VectorFpSize::F64, false, RoundingMode::TowardsMinusInfinity) => {
                inst::frintm_v2d(result, operand)
            }
            (VectorFpSize::F32, false, RoundingMode::TowardsZero) => {
                inst::frintz_v4s(result, operand)
            }
            (VectorFpSize::F64, false, RoundingMode::TowardsZero) => {
                inst::frintz_v2d(result, operand)
            }
            (VectorFpSize::F32, false, RoundingMode::ToNearestTieAwayFromZero) => {
                inst::frinta_v4s(result, operand)
            }
            (VectorFpSize::F64, false, RoundingMode::ToNearestTieAwayFromZero) => {
                inst::frinta_v2d(result, operand)
            }
            (_, false, RoundingMode::ToOdd) => unreachable!(),
        };
        code.write_u32(instruction)?;
        Ok(())
    })
}

fn common_rounding_mode(rounding: u8) -> CommonRoundingMode {
    match rounding {
        0 => CommonRoundingMode::ToNearestTieEven,
        1 => CommonRoundingMode::TowardsPlusInfinity,
        2 => CommonRoundingMode::TowardsMinusInfinity,
        3 => CommonRoundingMode::TowardsZero,
        4 => CommonRoundingMode::ToNearestTieAwayFromZero,
        _ => unreachable!("invalid FP rounding mode {rounding}"),
    }
}

extern "C" fn fallback_fp_vector_round_int16<const ROUNDING: u8, const EXACT: bool>(
    result: *mut [u8; 16],
    input: *const [u8; 16],
    fpcr: u32,
    fpsr: *mut u32,
) {
    unsafe {
        let input = std::mem::transmute::<[u8; 16], [u16; 8]>(*input);
        let mut output = [0u16; 8];
        let fpcr = CommonFpcr::new(fpcr);
        let mut current_fpsr = Fpsr::new(fpsr.read());
        for (dst, src) in output.iter_mut().zip(input) {
            *dst = fp_round_int(
                src,
                fpcr,
                common_rounding_mode(ROUNDING),
                EXACT,
                &mut current_fpsr,
            );
        }
        fpsr.write(current_fpsr.value());
        result.write(std::mem::transmute::<[u16; 8], [u8; 16]>(output));
    }
}

fn round_int16_fallback(rounding: u8, exact: bool) -> usize {
    macro_rules! select_exact {
        ($rounding:expr) => {
            if exact {
                fallback_fp_vector_round_int16::<$rounding, true> as *const () as usize
            } else {
                fallback_fp_vector_round_int16::<$rounding, false> as *const () as usize
            }
        };
    }

    match rounding {
        0 => select_exact!(0),
        1 => select_exact!(1),
        2 => select_exact!(2),
        3 => select_exact!(3),
        4 => select_exact!(4),
        _ => unreachable!("invalid FP rounding mode {rounding}"),
    }
}

fn emit_round_int16(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let rounding = args[1].get_immediate_u8();
    RoundingMode::from_u8(rounding)?;
    let exact = args[2].get_immediate_u1();
    let fpcr_controlled = args[3].get_immediate_u1();
    let fallback = round_int16_fallback(rounding, exact);

    let mut input = ctx.reg_alloc.read_q(args[0]);
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut input, &mut result])?;
    ctx.reg_alloc.spill_flags(code)?;
    ctx.fpsr.spill(code)?;

    let input = input.index().expect("input realized") as u8;
    let result = result.index().expect("result realized") as u8;
    let saved_registers = ABI_CALLER_SAVE & !to_reg_list_vec(result);
    const STACK_SIZE: usize = 2 * 16;
    emit_push_registers(code, saved_registers, STACK_SIZE)?;

    emit_mov_x_imm(code, XSCRATCH0, fallback as u64)?;
    code.write_u32(inst::add_x_imm(0, 31, 0))?;
    code.write_u32(inst::add_x_imm(1, 31, 16))?;
    emit_mov_w_imm(code, 2, ctx.fpcr(fpcr_controlled).value())?;
    code.write_u32(inst::add_x_imm(
        3,
        XSTATE,
        u32::try_from(ctx.conf.state_fpsr_offset)
            .map_err(|_| "ARM64 FP vector: FPSR state offset exceeds u32".to_string())?,
    ))?;
    code.write_u32(inst::str_q_unsigned_sp(input, 16))?;
    code.write_u32(inst::blr(XSCRATCH0))?;
    code.write_u32(inst::ldr_q_unsigned_sp(result, 0))?;

    emit_pop_registers(code, saved_registers, STACK_SIZE)
}

fn emit_fma(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    emit: impl FnOnce(u8, u8, u8) -> u32,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.read_write_q(args[0], inst_ref);
    let mut m = ctx.reg_alloc.read_q(args[1]);
    let mut n = ctx.reg_alloc.read_q(args[2]);
    let fpcr_controlled = args[3].get_immediate_u1();
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut m, &mut n])?;
    ctx.fpsr.load(code)?;

    let result = result.index().expect("result realized") as u8;
    let m = m.index().expect("m realized") as u8;
    let n = n.index().expect("n realized") as u8;
    maybe_standard_fpcr(code, ctx, fpcr_controlled, |code| {
        code.write_u32(emit(result, m, n))?;
        Ok(())
    })
}

fn emit_from_fixed(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: VectorFpSize,
    signed: bool,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let fbits = args[1].get_immediate_u8();
    let rounding_mode = RoundingMode::from_u8(args[2].get_immediate_u8())?;
    let fpcr_controlled = args[3].get_immediate_u1();
    let fpcr_rounding_mode = fpcr_rounding_mode(ctx, fpcr_controlled)?;
    if rounding_mode != fpcr_rounding_mode {
        return Err(format!(
            "ARM64 FP vector: fixed-to-FP rounding mode {:?} does not match FPCR {:?}",
            rounding_mode, fpcr_rounding_mode
        ));
    }

    match size {
        VectorFpSize::F32 if fbits > 32 => {
            return Err(format!(
                "ARM64 FP vector: 32-bit fixed-to-FP has invalid fbits={fbits}"
            ));
        }
        VectorFpSize::F64 if fbits > 64 => {
            return Err(format!(
                "ARM64 FP vector: 64-bit fixed-to-FP has invalid fbits={fbits}"
            ));
        }
        _ => {}
    }

    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut operand = ctx.reg_alloc.read_q(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;

    let result = result.index().expect("result realized") as u8;
    let operand = operand.index().expect("operand realized") as u8;
    maybe_standard_fpcr(code, ctx, fpcr_controlled, |code| {
        let word = match (size, signed, fbits) {
            (VectorFpSize::F32, true, 0) => inst::scvtf_v4s(result, operand),
            (VectorFpSize::F64, true, 0) => inst::scvtf_v2d(result, operand),
            (VectorFpSize::F32, false, 0) => inst::ucvtf_v4s(result, operand),
            (VectorFpSize::F64, false, 0) => inst::ucvtf_v2d(result, operand),
            (VectorFpSize::F32, true, _) => inst::scvtf_v4s_fixed(result, operand, fbits),
            (VectorFpSize::F64, true, _) => inst::scvtf_v2d_fixed(result, operand, fbits),
            (VectorFpSize::F32, false, _) => inst::ucvtf_v4s_fixed(result, operand, fbits),
            (VectorFpSize::F64, false, _) => inst::ucvtf_v2d_fixed(result, operand, fbits),
        };
        code.write_u32(word)?;
        Ok(())
    })
}

fn emit_to_fixed(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: VectorFpSize,
    signed: bool,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let fbits = args[1].get_immediate_u8();
    let rounding_mode = RoundingMode::from_u8(args[2].get_immediate_u8())?;
    let fpcr_controlled = args[3].get_immediate_u1();

    match size {
        VectorFpSize::F32 if fbits > 32 => {
            return Err(format!(
                "ARM64 FP vector: FP-to-32-bit fixed has invalid fbits={fbits}"
            ));
        }
        VectorFpSize::F64 if fbits > 64 => {
            return Err(format!(
                "ARM64 FP vector: FP-to-64-bit fixed has invalid fbits={fbits}"
            ));
        }
        _ => {}
    }
    if fbits != 0 && rounding_mode != RoundingMode::TowardsZero {
        return Err(format!(
            "ARM64 FP vector: FP-to-fixed with fbits={fbits} and rounding mode {:?} is not ported",
            rounding_mode
        ));
    }
    if rounding_mode == RoundingMode::ToOdd {
        return Err("ARM64 FP vector: ToOdd FP-to-fixed is not ported".to_string());
    }

    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut operand = ctx.reg_alloc.read_q(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    ctx.fpsr.load(code)?;

    let result = result.index().expect("result realized") as u8;
    let operand = operand.index().expect("operand realized") as u8;
    maybe_standard_fpcr(code, ctx, fpcr_controlled, |code| {
        let word = match (size, signed, rounding_mode, fbits) {
            (VectorFpSize::F32, true, RoundingMode::TowardsZero, 0) => {
                inst::fcvtzs_v4s(result, operand)
            }
            (VectorFpSize::F64, true, RoundingMode::TowardsZero, 0) => {
                inst::fcvtzs_v2d(result, operand)
            }
            (VectorFpSize::F32, false, RoundingMode::TowardsZero, 0) => {
                inst::fcvtzu_v4s(result, operand)
            }
            (VectorFpSize::F64, false, RoundingMode::TowardsZero, 0) => {
                inst::fcvtzu_v2d(result, operand)
            }
            (VectorFpSize::F32, true, RoundingMode::TowardsZero, _) => {
                inst::fcvtzs_v4s_fixed(result, operand, fbits)
            }
            (VectorFpSize::F64, true, RoundingMode::TowardsZero, _) => {
                inst::fcvtzs_v2d_fixed(result, operand, fbits)
            }
            (VectorFpSize::F32, false, RoundingMode::TowardsZero, _) => {
                inst::fcvtzu_v4s_fixed(result, operand, fbits)
            }
            (VectorFpSize::F64, false, RoundingMode::TowardsZero, _) => {
                inst::fcvtzu_v2d_fixed(result, operand, fbits)
            }
            (VectorFpSize::F32, true, RoundingMode::ToNearestTieEven, 0) => {
                inst::fcvtns_v4s(result, operand)
            }
            (VectorFpSize::F64, true, RoundingMode::ToNearestTieEven, 0) => {
                inst::fcvtns_v2d(result, operand)
            }
            (VectorFpSize::F32, true, RoundingMode::TowardsPlusInfinity, 0) => {
                inst::fcvtps_v4s(result, operand)
            }
            (VectorFpSize::F64, true, RoundingMode::TowardsPlusInfinity, 0) => {
                inst::fcvtps_v2d(result, operand)
            }
            (VectorFpSize::F32, true, RoundingMode::TowardsMinusInfinity, 0) => {
                inst::fcvtms_v4s(result, operand)
            }
            (VectorFpSize::F64, true, RoundingMode::TowardsMinusInfinity, 0) => {
                inst::fcvtms_v2d(result, operand)
            }
            (VectorFpSize::F32, true, RoundingMode::ToNearestTieAwayFromZero, 0) => {
                inst::fcvtas_v4s(result, operand)
            }
            (VectorFpSize::F64, true, RoundingMode::ToNearestTieAwayFromZero, 0) => {
                inst::fcvtas_v2d(result, operand)
            }
            (VectorFpSize::F32, false, RoundingMode::ToNearestTieEven, 0) => {
                inst::fcvtnu_v4s(result, operand)
            }
            (VectorFpSize::F64, false, RoundingMode::ToNearestTieEven, 0) => {
                inst::fcvtnu_v2d(result, operand)
            }
            (VectorFpSize::F32, false, RoundingMode::TowardsPlusInfinity, 0) => {
                inst::fcvtpu_v4s(result, operand)
            }
            (VectorFpSize::F64, false, RoundingMode::TowardsPlusInfinity, 0) => {
                inst::fcvtpu_v2d(result, operand)
            }
            (VectorFpSize::F32, false, RoundingMode::TowardsMinusInfinity, 0) => {
                inst::fcvtmu_v4s(result, operand)
            }
            (VectorFpSize::F64, false, RoundingMode::TowardsMinusInfinity, 0) => {
                inst::fcvtmu_v2d(result, operand)
            }
            (VectorFpSize::F32, false, RoundingMode::ToNearestTieAwayFromZero, 0) => {
                inst::fcvtau_v4s(result, operand)
            }
            (VectorFpSize::F64, false, RoundingMode::ToNearestTieAwayFromZero, 0) => {
                inst::fcvtau_v2d(result, operand)
            }
            _ => unreachable!("validated FP vector to-fixed arguments"),
        };
        code.write_u32(word)?;
        Ok(())
    })
}

pub fn emit_fp_vector_add32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op_arranged(code, ctx, inst_ref, inst::fadd_v4s)
}

pub fn emit_fp_vector_add64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op_arranged(code, ctx, inst_ref, inst::fadd_v2d)
}

pub fn emit_fp_vector_sub32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op_arranged(code, ctx, inst_ref, inst::fsub_v4s)
}

pub fn emit_fp_vector_sub64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op_arranged(code, ctx, inst_ref, inst::fsub_v2d)
}

pub fn emit_fp_vector_mul32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op_arranged(code, ctx, inst_ref, inst::fmul_v4s)
}

pub fn emit_fp_vector_mul64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op_arranged(code, ctx, inst_ref, inst::fmul_v2d)
}

pub fn emit_fp_vector_mul_x32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op_arranged(code, ctx, inst_ref, inst::fmulx_v4s)
}

pub fn emit_fp_vector_mul_x64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op_arranged(code, ctx, inst_ref, inst::fmulx_v2d)
}

pub fn emit_fp_vector_neg32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_two_op_arranged(code, ctx, inst_ref, inst::fneg_v4s)
}

pub fn emit_fp_vector_neg64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_two_op_arranged(code, ctx, inst_ref, inst::fneg_v2d)
}

pub fn emit_fp_vector_sqrt32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_two_op_arranged(code, ctx, inst_ref, inst::fsqrt_v4s)
}

pub fn emit_fp_vector_sqrt64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_two_op_arranged(code, ctx, inst_ref, inst::fsqrt_v2d)
}

pub fn emit_fp_vector_recip_estimate32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_two_op_arranged(code, ctx, inst_ref, inst::frecpe_v4s)
}

pub fn emit_fp_vector_recip_estimate64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_two_op_arranged(code, ctx, inst_ref, inst::frecpe_v2d)
}

pub fn emit_fp_vector_rsqrt_estimate32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_two_op_arranged(code, ctx, inst_ref, inst::frsqrte_v4s)
}

pub fn emit_fp_vector_rsqrt_estimate64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_two_op_arranged(code, ctx, inst_ref, inst::frsqrte_v2d)
}

pub fn emit_fp_vector_div32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op_arranged(code, ctx, inst_ref, inst::fdiv_v4s)
}

pub fn emit_fp_vector_div64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op_arranged(code, ctx, inst_ref, inst::fdiv_v2d)
}

pub fn emit_fp_vector_max32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op_arranged(code, ctx, inst_ref, inst::fmax_v4s)
}

pub fn emit_fp_vector_max64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op_arranged(code, ctx, inst_ref, inst::fmax_v2d)
}

pub fn emit_fp_vector_max_numeric32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op_arranged(code, ctx, inst_ref, inst::fmaxnm_v4s)
}

pub fn emit_fp_vector_max_numeric64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op_arranged(code, ctx, inst_ref, inst::fmaxnm_v2d)
}

pub fn emit_fp_vector_min32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op_arranged(code, ctx, inst_ref, inst::fmin_v4s)
}

pub fn emit_fp_vector_min64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op_arranged(code, ctx, inst_ref, inst::fmin_v2d)
}

pub fn emit_fp_vector_min_numeric32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op_arranged(code, ctx, inst_ref, inst::fminnm_v4s)
}

pub fn emit_fp_vector_min_numeric64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op_arranged(code, ctx, inst_ref, inst::fminnm_v2d)
}

pub fn emit_fp_vector_equal32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op_arranged(code, ctx, inst_ref, inst::fcmeq_v4s)
}

pub fn emit_fp_vector_equal64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op_arranged(code, ctx, inst_ref, inst::fcmeq_v2d)
}

pub fn emit_fp_vector_greater32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op_arranged(code, ctx, inst_ref, inst::fcmgt_v4s)
}

pub fn emit_fp_vector_greater64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op_arranged(code, ctx, inst_ref, inst::fcmgt_v2d)
}

pub fn emit_fp_vector_greater_equal32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op_arranged(code, ctx, inst_ref, inst::fcmge_v4s)
}

pub fn emit_fp_vector_greater_equal64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op_arranged(code, ctx, inst_ref, inst::fcmge_v2d)
}

pub fn emit_fp_vector_mul_add32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_fma(code, ctx, inst_ref, inst::fmla_v4s)
}

pub fn emit_fp_vector_mul_add64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_fma(code, ctx, inst_ref, inst::fmla_v2d)
}

pub fn emit_fp_vector_paired_add32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op_arranged(code, ctx, inst_ref, inst::faddp_v4s)
}

pub fn emit_fp_vector_paired_add64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op_arranged(code, ctx, inst_ref, inst::faddp_v2d)
}

pub fn emit_fp_vector_paired_add_lower32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut a = ctx.reg_alloc.read_q(args[0]);
    let mut b = ctx.reg_alloc.read_q(args[1]);
    let fpcr_controlled = args[2].get_immediate_u1();
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut a, &mut b])?;
    ctx.fpsr.load(code)?;

    let result = result.index().expect("result realized") as u8;
    let a = a.index().expect("a realized") as u8;
    let b = b.index().expect("b realized") as u8;
    maybe_standard_fpcr(code, ctx, fpcr_controlled, |code| {
        code.write_u32(inst::zip1_v(0, a, b, 64, true))?;
        code.write_u32(inst::movi_d_imm0(1))?;
        code.write_u32(inst::faddp_v4s(result, 0, 1))?;
        Ok(())
    })
}

pub fn emit_fp_vector_paired_add_lower64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut a = ctx.reg_alloc.read_q(args[0]);
    let mut b = ctx.reg_alloc.read_q(args[1]);
    let fpcr_controlled = args[2].get_immediate_u1();
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut a, &mut b])?;
    ctx.fpsr.load(code)?;

    let result = result.index().expect("result realized") as u8;
    let a = a.index().expect("a realized") as u8;
    let b = b.index().expect("b realized") as u8;
    maybe_standard_fpcr(code, ctx, fpcr_controlled, |code| {
        code.write_u32(inst::zip1_v(0, a, b, 64, true))?;
        code.write_u32(inst::faddp_d_from_v2d(result, 0))?;
        Ok(())
    })
}

pub fn emit_fp_vector_from_half32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let rounding_mode = RoundingMode::from_u8(args[1].get_immediate_u8())?;
    if rounding_mode != RoundingMode::ToNearestTieEven {
        return Err(format!(
            "ARM64 FP vector: half-to-single requires nearest-even, got {:?}",
            rounding_mode
        ));
    }
    let fpcr_controlled = args[2].get_immediate_u1();
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut operand = ctx.reg_alloc.read_d(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    ctx.fpsr.load(code)?;
    let result = result.index().expect("result realized") as u8;
    let operand = operand.index().expect("operand realized") as u8;
    maybe_standard_fpcr(code, ctx, fpcr_controlled, |code| {
        code.write_u32(inst::fcvtl_v4s_from_v4h(result, operand))?;
        Ok(())
    })
}

pub fn emit_fp_vector_to_half32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let rounding_mode = RoundingMode::from_u8(args[1].get_immediate_u8())?;
    if rounding_mode != RoundingMode::ToNearestTieEven {
        return Err(format!(
            "ARM64 FP vector: single-to-half requires nearest-even, got {:?}",
            rounding_mode
        ));
    }
    let fpcr_controlled = args[2].get_immediate_u1();
    let mut result = ctx.reg_alloc.write_d(inst_ref);
    let mut operand = ctx.reg_alloc.read_q(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    ctx.fpsr.load(code)?;
    let result = result.index().expect("result realized") as u8;
    let operand = operand.index().expect("operand realized") as u8;
    maybe_standard_fpcr(code, ctx, fpcr_controlled, |code| {
        code.write_u32(inst::fcvtn_v4h_from_v4s(result, operand))?;
        Ok(())
    })
}

pub fn emit_fp_vector_from_signed_fixed32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_from_fixed(code, ctx, inst_ref, VectorFpSize::F32, true)
}

pub fn emit_fp_vector_from_signed_fixed64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_from_fixed(code, ctx, inst_ref, VectorFpSize::F64, true)
}

pub fn emit_fp_vector_from_unsigned_fixed32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_from_fixed(code, ctx, inst_ref, VectorFpSize::F32, false)
}

pub fn emit_fp_vector_from_unsigned_fixed64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_from_fixed(code, ctx, inst_ref, VectorFpSize::F64, false)
}

pub fn emit_fp_vector_to_signed_fixed32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_to_fixed(code, ctx, inst_ref, VectorFpSize::F32, true)
}

pub fn emit_fp_vector_to_signed_fixed64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_to_fixed(code, ctx, inst_ref, VectorFpSize::F64, true)
}

pub fn emit_fp_vector_to_unsigned_fixed32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_to_fixed(code, ctx, inst_ref, VectorFpSize::F32, false)
}

pub fn emit_fp_vector_to_unsigned_fixed64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_to_fixed(code, ctx, inst_ref, VectorFpSize::F64, false)
}

pub fn emit_fp_vector_round_int16(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_round_int16(code, ctx, inst_ref)
}

pub fn emit_fp_vector_round_int32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_round_int(code, ctx, inst_ref, VectorFpSize::F32)
}

pub fn emit_fp_vector_round_int64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_round_int(code, ctx, inst_ref, VectorFpSize::F64)
}

pub fn emit_fp_vector_recip_step_fused32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op_arranged(code, ctx, inst_ref, inst::frecps_v4s)
}

pub fn emit_fp_vector_recip_step_fused64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op_arranged(code, ctx, inst_ref, inst::frecps_v2d)
}

pub fn emit_fp_vector_rsqrt_step_fused32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op_arranged(code, ctx, inst_ref, inst::frsqrts_v4s)
}

pub fn emit_fp_vector_rsqrt_step_fused64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op_arranged(code, ctx, inst_ref, inst::frsqrts_v2d)
}

pub fn emit_fp_vector_instruction(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    match ctx.block.get(inst_ref).opcode {
        Opcode::FPVectorAbs16 => emit_fp_vector_abs16(code, ctx, inst_ref),
        Opcode::FPVectorAbs32 => emit_fp_vector_abs32(code, ctx, inst_ref),
        Opcode::FPVectorAbs64 => emit_fp_vector_abs64(code, ctx, inst_ref),
        Opcode::FPVectorAdd32 => emit_fp_vector_add32(code, ctx, inst_ref),
        Opcode::FPVectorAdd64 => emit_fp_vector_add64(code, ctx, inst_ref),
        Opcode::FPVectorSub32 => emit_fp_vector_sub32(code, ctx, inst_ref),
        Opcode::FPVectorSub64 => emit_fp_vector_sub64(code, ctx, inst_ref),
        Opcode::FPVectorMul32 => emit_fp_vector_mul32(code, ctx, inst_ref),
        Opcode::FPVectorMul64 => emit_fp_vector_mul64(code, ctx, inst_ref),
        Opcode::FPVectorMulX32 => emit_fp_vector_mul_x32(code, ctx, inst_ref),
        Opcode::FPVectorMulX64 => emit_fp_vector_mul_x64(code, ctx, inst_ref),
        Opcode::FPVectorNeg32 => emit_fp_vector_neg32(code, ctx, inst_ref),
        Opcode::FPVectorNeg64 => emit_fp_vector_neg64(code, ctx, inst_ref),
        Opcode::FPVectorSqrt32 => emit_fp_vector_sqrt32(code, ctx, inst_ref),
        Opcode::FPVectorSqrt64 => emit_fp_vector_sqrt64(code, ctx, inst_ref),
        Opcode::FPVectorRecipEstimate32 => emit_fp_vector_recip_estimate32(code, ctx, inst_ref),
        Opcode::FPVectorRecipEstimate64 => emit_fp_vector_recip_estimate64(code, ctx, inst_ref),
        Opcode::FPVectorRSqrtEstimate32 => emit_fp_vector_rsqrt_estimate32(code, ctx, inst_ref),
        Opcode::FPVectorRSqrtEstimate64 => emit_fp_vector_rsqrt_estimate64(code, ctx, inst_ref),
        Opcode::FPVectorDiv32 => emit_fp_vector_div32(code, ctx, inst_ref),
        Opcode::FPVectorDiv64 => emit_fp_vector_div64(code, ctx, inst_ref),
        Opcode::FPVectorMax32 => emit_fp_vector_max32(code, ctx, inst_ref),
        Opcode::FPVectorMax64 => emit_fp_vector_max64(code, ctx, inst_ref),
        Opcode::FPVectorMaxNumeric32 => emit_fp_vector_max_numeric32(code, ctx, inst_ref),
        Opcode::FPVectorMaxNumeric64 => emit_fp_vector_max_numeric64(code, ctx, inst_ref),
        Opcode::FPVectorMin32 => emit_fp_vector_min32(code, ctx, inst_ref),
        Opcode::FPVectorMin64 => emit_fp_vector_min64(code, ctx, inst_ref),
        Opcode::FPVectorMinNumeric32 => emit_fp_vector_min_numeric32(code, ctx, inst_ref),
        Opcode::FPVectorMinNumeric64 => emit_fp_vector_min_numeric64(code, ctx, inst_ref),
        Opcode::FPVectorEqual32 => emit_fp_vector_equal32(code, ctx, inst_ref),
        Opcode::FPVectorEqual64 => emit_fp_vector_equal64(code, ctx, inst_ref),
        Opcode::FPVectorGreater32 => emit_fp_vector_greater32(code, ctx, inst_ref),
        Opcode::FPVectorGreater64 => emit_fp_vector_greater64(code, ctx, inst_ref),
        Opcode::FPVectorGreaterEqual32 => emit_fp_vector_greater_equal32(code, ctx, inst_ref),
        Opcode::FPVectorGreaterEqual64 => emit_fp_vector_greater_equal64(code, ctx, inst_ref),
        Opcode::FPVectorMulAdd32 => emit_fp_vector_mul_add32(code, ctx, inst_ref),
        Opcode::FPVectorMulAdd64 => emit_fp_vector_mul_add64(code, ctx, inst_ref),
        Opcode::FPVectorPairedAdd32 => emit_fp_vector_paired_add32(code, ctx, inst_ref),
        Opcode::FPVectorPairedAdd64 => emit_fp_vector_paired_add64(code, ctx, inst_ref),
        Opcode::FPVectorPairedAddLower32 => emit_fp_vector_paired_add_lower32(code, ctx, inst_ref),
        Opcode::FPVectorPairedAddLower64 => emit_fp_vector_paired_add_lower64(code, ctx, inst_ref),
        Opcode::FPVectorFromHalf32 => emit_fp_vector_from_half32(code, ctx, inst_ref),
        Opcode::FPVectorToHalf32 => emit_fp_vector_to_half32(code, ctx, inst_ref),
        Opcode::FPVectorFromSignedFixed32 => {
            emit_fp_vector_from_signed_fixed32(code, ctx, inst_ref)
        }
        Opcode::FPVectorFromSignedFixed64 => {
            emit_fp_vector_from_signed_fixed64(code, ctx, inst_ref)
        }
        Opcode::FPVectorFromUnsignedFixed32 => {
            emit_fp_vector_from_unsigned_fixed32(code, ctx, inst_ref)
        }
        Opcode::FPVectorFromUnsignedFixed64 => {
            emit_fp_vector_from_unsigned_fixed64(code, ctx, inst_ref)
        }
        Opcode::FPVectorToSignedFixed32 => emit_fp_vector_to_signed_fixed32(code, ctx, inst_ref),
        Opcode::FPVectorToSignedFixed64 => emit_fp_vector_to_signed_fixed64(code, ctx, inst_ref),
        Opcode::FPVectorToUnsignedFixed32 => {
            emit_fp_vector_to_unsigned_fixed32(code, ctx, inst_ref)
        }
        Opcode::FPVectorToUnsignedFixed64 => {
            emit_fp_vector_to_unsigned_fixed64(code, ctx, inst_ref)
        }
        Opcode::FPVectorRoundInt16 => emit_fp_vector_round_int16(code, ctx, inst_ref),
        Opcode::FPVectorRoundInt32 => emit_fp_vector_round_int32(code, ctx, inst_ref),
        Opcode::FPVectorRoundInt64 => emit_fp_vector_round_int64(code, ctx, inst_ref),
        Opcode::FPVectorRecipStepFused32 => emit_fp_vector_recip_step_fused32(code, ctx, inst_ref),
        Opcode::FPVectorRecipStepFused64 => emit_fp_vector_recip_step_fused64(code, ctx, inst_ref),
        Opcode::FPVectorRSqrtStepFused32 => emit_fp_vector_rsqrt_step_fused32(code, ctx, inst_ref),
        Opcode::FPVectorRSqrtStepFused64 => emit_fp_vector_rsqrt_step_fused64(code, ctx, inst_ref),
        opcode => Err(format!("unimplemented ARM64 FP vector opcode: {opcode:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_int16_fallback_matches_tie_even_and_updates_fpsr() {
        let input: [u16; 8] = [
            0x3e00, 0x4100, 0xbe00, 0xc100, 0x3c00, 0xbc00, 0x0000, 0x8000,
        ];
        let input = unsafe { std::mem::transmute::<[u16; 8], [u8; 16]>(input) };
        let mut output = [0u8; 16];
        let mut fpsr = 0u32;

        fallback_fp_vector_round_int16::<0, true>(
            &mut output,
            &input,
            CommonFpcr::new(0).value(),
            &mut fpsr,
        );

        let output = unsafe { std::mem::transmute::<[u8; 16], [u16; 8]>(output) };
        assert_eq!(
            output,
            [0x4000, 0x4000, 0xc000, 0xc000, 0x3c00, 0xbc00, 0x0000, 0x8000]
        );
        assert_ne!(fpsr & (1 << 4), 0);
    }
}
