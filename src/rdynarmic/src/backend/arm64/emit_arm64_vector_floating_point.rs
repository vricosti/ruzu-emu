//! ARM64 floating-point vector emission.
//!
//! Upstream owner: `backend/arm64/emit_arm64_vector_floating_point.cpp`.

use rhazel::{
    CodeGenerator, SystemReg, VReg2D, VReg4S, VRegArranged, D1, SP, V0, V1, X0, X1, X2, X3,
};

use crate::backend::arm64::abi::regs::{WSCRATCH0, XSCRATCH0, XSTATE};
use crate::backend::arm64::abi::{
    emit_pop_registers, emit_push_registers, to_reg_list_vec, ABI_CALLER_SAVE,
};
use crate::backend::arm64::block_of_code::BlockOfCode;
use crate::backend::arm64::emit_context::EmitContext;
use crate::backend::arm64::reg_alloc::RegAlloc;
use crate::common::fp::fpcr::Fpcr as CommonFpcr;
use crate::common::fp::fpsr::Fpsr;
use crate::common::fp::op::fp_round_int::fp_round_int;
use crate::common::fp::rounding_mode::RoundingMode as CommonRoundingMode;
use crate::ir::opcode::Opcode;
use crate::ir::value::InstRef;

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

/// Upstream `MaybeStandardFPSCRValue`.
fn maybe_standard_fpcr(
    code: &mut CodeGenerator<'_>,
    ctx: &EmitContext<'_>,
    fpcr_controlled: bool,
    emit: impl FnOnce(&mut CodeGenerator<'_>) -> Result<(), String>,
) -> Result<(), String> {
    let current_fpcr = ctx.fpcr(true);
    let target_fpcr = ctx.fpcr(fpcr_controlled);
    if target_fpcr != current_fpcr {
        code.mov_imm(WSCRATCH0, u64::from(target_fpcr.value()))?;
        code.msr(SystemReg::FPCR, XSCRATCH0)?;
        emit(code)?;
        code.mov_imm(WSCRATCH0, u64::from(current_fpcr.value()))?;
        code.msr(SystemReg::FPCR, XSCRATCH0)?;
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

/// Upstream `EmitThreeOpArranged<fsize>`: `V` is the arrangement the
/// operands are viewed through (`VReg4S` for 32, `VReg2D` for 64).
fn emit_three_op_arranged<V: VRegArranged>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    emit: impl FnOnce(&mut CodeGenerator<'_>, V, V, V) -> Result<(), String>,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut a = ctx.reg_alloc.read_q(args[0]);
    let mut b = ctx.reg_alloc.read_q(args[1]);
    let fpcr_controlled = args[2].get_immediate_u1();
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut a, &mut b])?;
    ctx.fpsr.load(code)?;

    let (result, a, b) = (
        V::from_vreg(result.v()),
        V::from_vreg(a.v()),
        V::from_vreg(b.v()),
    );
    maybe_standard_fpcr(code, ctx, fpcr_controlled, |code| emit(code, result, a, b))
}

/// Upstream `EmitTwoOpArranged<fsize>`.
fn emit_two_op_arranged<V: VRegArranged>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    emit: impl FnOnce(&mut CodeGenerator<'_>, V, V) -> Result<(), String>,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut a = ctx.reg_alloc.read_q(args[0]);
    let fpcr_controlled = args[1].is_void() || args[1].get_immediate_u1();
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut a])?;
    ctx.fpsr.load(code)?;

    let (result, a) = (V::from_vreg(result.v()), V::from_vreg(a.v()));
    maybe_standard_fpcr(code, ctx, fpcr_controlled, |code| emit(code, result, a))
}

pub fn emit_fp_vector_abs16(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.read_write_q(args[0], inst_ref);
    result.realize(code, ctx.block)?;
    code.bic_imm(result.v().h8(), 0b1000_0000, 8)
}

pub fn emit_fp_vector_abs32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_two_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, result, a| code.fabs(result, a))
}

pub fn emit_fp_vector_abs64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_two_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, result, a| code.fabs(result, a))
}

/// Upstream `EmitRoundInt<fsize>`.
fn emit_round_int<V: VRegArranged>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
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

    let (result, operand) = (V::from_vreg(result.v()), V::from_vreg(operand.v()));
    maybe_standard_fpcr(code, ctx, fpcr_controlled, |code| {
        if exact {
            return code.frintx(result, operand);
        }
        match rounding_mode {
            RoundingMode::ToNearestTieEven => code.frintn(result, operand),
            RoundingMode::TowardsPlusInfinity => code.frintp(result, operand),
            RoundingMode::TowardsMinusInfinity => code.frintm(result, operand),
            RoundingMode::TowardsZero => code.frintz(result, operand),
            RoundingMode::ToNearestTieAwayFromZero => code.frinta(result, operand),
            RoundingMode::ToOdd => unreachable!(),
        }
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
    code: &mut CodeGenerator<'_>,
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

    let (input, result) = (input.q(), result.q());
    let saved_registers = ABI_CALLER_SAVE & !to_reg_list_vec(result.index());
    const STACK_SIZE: usize = 2 * 16;
    emit_push_registers(code, saved_registers, STACK_SIZE)?;

    code.mov_imm(XSCRATCH0, fallback as u64)?;
    code.add_imm(X0, SP, 0)?;
    code.add_imm(X1, SP, 16)?;
    code.mov_imm(X2, u64::from(ctx.fpcr(fpcr_controlled).value()))?;
    code.add_imm(
        X3,
        XSTATE,
        u32::try_from(ctx.conf.state_fpsr_offset)
            .map_err(|_| "ARM64 FP vector: FPSR state offset exceeds u32".to_string())?,
    )?;
    code.str(input, X1, 0)?;
    code.blr(XSCRATCH0)?;
    code.ldr(result, SP, 0)?;

    emit_pop_registers(code, saved_registers, STACK_SIZE)
}

/// Upstream `EmitFMA<fsize>`.
fn emit_fma<V: VRegArranged>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    emit: impl FnOnce(&mut CodeGenerator<'_>, V, V, V) -> Result<(), String>,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.read_write_q(args[0], inst_ref);
    let mut m = ctx.reg_alloc.read_q(args[1]);
    let mut n = ctx.reg_alloc.read_q(args[2]);
    let fpcr_controlled = args[3].get_immediate_u1();
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut m, &mut n])?;
    ctx.fpsr.load(code)?;

    let (result, m, n) = (
        V::from_vreg(result.v()),
        V::from_vreg(m.v()),
        V::from_vreg(n.v()),
    );
    maybe_standard_fpcr(code, ctx, fpcr_controlled, |code| emit(code, result, m, n))
}

/// Upstream `EmitFromFixed<fsize, is_signed>`.
fn emit_from_fixed<V: VRegArranged>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
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

    if fbits > V::SIZE {
        return Err(format!(
            "ARM64 FP vector: {}-bit fixed-to-FP has invalid fbits={fbits}",
            V::SIZE
        ));
    }

    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut operand = ctx.reg_alloc.read_q(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;

    let (result, operand) = (V::from_vreg(result.v()), V::from_vreg(operand.v()));
    maybe_standard_fpcr(code, ctx, fpcr_controlled, |code| match (signed, fbits) {
        (true, 0) => code.scvtf(result, operand),
        (false, 0) => code.ucvtf(result, operand),
        (true, _) => code.scvtf_fixed(result, operand, fbits),
        (false, _) => code.ucvtf_fixed(result, operand, fbits),
    })
}

/// Upstream `EmitToFixed<fsize, is_signed>`.
fn emit_to_fixed<V: VRegArranged>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    signed: bool,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let fbits = args[1].get_immediate_u8();
    let rounding_mode = RoundingMode::from_u8(args[2].get_immediate_u8())?;
    let fpcr_controlled = args[3].get_immediate_u1();

    if fbits > V::SIZE {
        return Err(format!(
            "ARM64 FP vector: FP-to-{}-bit fixed has invalid fbits={fbits}",
            V::SIZE
        ));
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

    let (result, operand) = (V::from_vreg(result.v()), V::from_vreg(operand.v()));
    maybe_standard_fpcr(code, ctx, fpcr_controlled, |code| {
        match (signed, rounding_mode, fbits) {
            (true, RoundingMode::TowardsZero, 0) => code.fcvtzs(result, operand),
            (false, RoundingMode::TowardsZero, 0) => code.fcvtzu(result, operand),
            (true, RoundingMode::TowardsZero, _) => code.fcvtzs_fixed(result, operand, fbits),
            (false, RoundingMode::TowardsZero, _) => code.fcvtzu_fixed(result, operand, fbits),
            (true, RoundingMode::ToNearestTieEven, 0) => code.fcvtns(result, operand),
            (true, RoundingMode::TowardsPlusInfinity, 0) => code.fcvtps(result, operand),
            (true, RoundingMode::TowardsMinusInfinity, 0) => code.fcvtms(result, operand),
            (true, RoundingMode::ToNearestTieAwayFromZero, 0) => code.fcvtas(result, operand),
            (false, RoundingMode::ToNearestTieEven, 0) => code.fcvtnu(result, operand),
            (false, RoundingMode::TowardsPlusInfinity, 0) => code.fcvtpu(result, operand),
            (false, RoundingMode::TowardsMinusInfinity, 0) => code.fcvtmu(result, operand),
            (false, RoundingMode::ToNearestTieAwayFromZero, 0) => code.fcvtau(result, operand),
            _ => unreachable!("validated FP vector to-fixed arguments"),
        }
    })
}

pub fn emit_fp_vector_add32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, result, a, b| {
        code.fadd(result, a, b)
    })
}

pub fn emit_fp_vector_add64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_three_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, result, a, b| {
        code.fadd(result, a, b)
    })
}

pub fn emit_fp_vector_sub32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, result, a, b| {
        code.fsub(result, a, b)
    })
}

pub fn emit_fp_vector_sub64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_three_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, result, a, b| {
        code.fsub(result, a, b)
    })
}

pub fn emit_fp_vector_mul32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, result, a, b| {
        code.fmul(result, a, b)
    })
}

pub fn emit_fp_vector_mul64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_three_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, result, a, b| {
        code.fmul(result, a, b)
    })
}

pub fn emit_fp_vector_mul_x32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, result, a, b| {
        code.fmulx(result, a, b)
    })
}

pub fn emit_fp_vector_mul_x64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_three_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, result, a, b| {
        code.fmulx(result, a, b)
    })
}

pub fn emit_fp_vector_neg32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_two_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, result, a| code.fneg(result, a))
}

pub fn emit_fp_vector_neg64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_two_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, result, a| code.fneg(result, a))
}

pub fn emit_fp_vector_sqrt32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_two_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, result, a| code.fsqrt(result, a))
}

pub fn emit_fp_vector_sqrt64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_two_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, result, a| code.fsqrt(result, a))
}

pub fn emit_fp_vector_recip_estimate32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_two_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, result, a| code.frecpe(result, a))
}

pub fn emit_fp_vector_recip_estimate64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_two_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, result, a| code.frecpe(result, a))
}

pub fn emit_fp_vector_rsqrt_estimate32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_two_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, result, a| code.frsqrte(result, a))
}

pub fn emit_fp_vector_rsqrt_estimate64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_two_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, result, a| code.frsqrte(result, a))
}

pub fn emit_fp_vector_div32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, result, a, b| {
        code.fdiv(result, a, b)
    })
}

pub fn emit_fp_vector_div64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_three_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, result, a, b| {
        code.fdiv(result, a, b)
    })
}

pub fn emit_fp_vector_max32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, result, a, b| {
        code.fmax(result, a, b)
    })
}

pub fn emit_fp_vector_max64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_three_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, result, a, b| {
        code.fmax(result, a, b)
    })
}

pub fn emit_fp_vector_max_numeric32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, result, a, b| {
        code.fmaxnm(result, a, b)
    })
}

pub fn emit_fp_vector_max_numeric64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_three_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, result, a, b| {
        code.fmaxnm(result, a, b)
    })
}

pub fn emit_fp_vector_min32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, result, a, b| {
        code.fmin(result, a, b)
    })
}

pub fn emit_fp_vector_min64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_three_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, result, a, b| {
        code.fmin(result, a, b)
    })
}

pub fn emit_fp_vector_min_numeric32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, result, a, b| {
        code.fminnm(result, a, b)
    })
}

pub fn emit_fp_vector_min_numeric64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_three_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, result, a, b| {
        code.fminnm(result, a, b)
    })
}

pub fn emit_fp_vector_equal32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, result, a, b| {
        code.fcmeq(result, a, b)
    })
}

pub fn emit_fp_vector_equal64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_three_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, result, a, b| {
        code.fcmeq(result, a, b)
    })
}

pub fn emit_fp_vector_greater32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, result, a, b| {
        code.fcmgt(result, a, b)
    })
}

pub fn emit_fp_vector_greater64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_three_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, result, a, b| {
        code.fcmgt(result, a, b)
    })
}

pub fn emit_fp_vector_greater_equal32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, result, a, b| {
        code.fcmge(result, a, b)
    })
}

pub fn emit_fp_vector_greater_equal64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_three_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, result, a, b| {
        code.fcmge(result, a, b)
    })
}

pub fn emit_fp_vector_mul_add32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_fma::<VReg4S>(code, ctx, inst_ref, |code, result, m, n| code.fmla(result, m, n))
}

pub fn emit_fp_vector_mul_add64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_fma::<VReg2D>(code, ctx, inst_ref, |code, result, m, n| code.fmla(result, m, n))
}

pub fn emit_fp_vector_paired_add32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, result, a, b| {
        code.faddp(result, a, b)
    })
}

pub fn emit_fp_vector_paired_add64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_three_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, result, a, b| {
        code.faddp(result, a, b)
    })
}

pub fn emit_fp_vector_paired_add_lower32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut a = ctx.reg_alloc.read_q(args[0]);
    let mut b = ctx.reg_alloc.read_q(args[1]);
    let fpcr_controlled = args[2].get_immediate_u1();
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut a, &mut b])?;
    ctx.fpsr.load(code)?;

    let (result, a, b) = (result.v(), a.v(), b.v());
    maybe_standard_fpcr(code, ctx, fpcr_controlled, |code| {
        code.zip1(V0.d2(), a.d2(), b.d2())?;
        code.movi_zero(D1)?;
        code.faddp(result.s4(), V0.s4(), V1.s4())
    })
}

pub fn emit_fp_vector_paired_add_lower64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = ctx.reg_alloc.write_q(inst_ref);
    let mut a = ctx.reg_alloc.read_q(args[0]);
    let mut b = ctx.reg_alloc.read_q(args[1]);
    let fpcr_controlled = args[2].get_immediate_u1();
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut a, &mut b])?;
    ctx.fpsr.load(code)?;

    let (result, a, b) = (result.v(), a.v(), b.v());
    maybe_standard_fpcr(code, ctx, fpcr_controlled, |code| {
        code.zip1(V0.d2(), a.d2(), b.d2())?;
        code.faddp_scalar(result.d(), V0.d2())
    })
}

pub fn emit_fp_vector_from_half32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
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
    let (result, operand) = (result.v(), operand.v());
    maybe_standard_fpcr(code, ctx, fpcr_controlled, |code| {
        code.fcvtl(result.s4(), operand.h4())
    })
}

pub fn emit_fp_vector_to_half32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
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
    let (result, operand) = (result.v(), operand.v());
    maybe_standard_fpcr(code, ctx, fpcr_controlled, |code| {
        code.fcvtn(result.h4(), operand.s4())
    })
}

pub fn emit_fp_vector_from_signed_fixed32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_from_fixed::<VReg4S>(code, ctx, inst_ref, true)
}

pub fn emit_fp_vector_from_signed_fixed64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_from_fixed::<VReg2D>(code, ctx, inst_ref, true)
}

pub fn emit_fp_vector_from_unsigned_fixed32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_from_fixed::<VReg4S>(code, ctx, inst_ref, false)
}

pub fn emit_fp_vector_from_unsigned_fixed64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_from_fixed::<VReg2D>(code, ctx, inst_ref, false)
}

pub fn emit_fp_vector_to_signed_fixed32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_to_fixed::<VReg4S>(code, ctx, inst_ref, true)
}

pub fn emit_fp_vector_to_signed_fixed64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_to_fixed::<VReg2D>(code, ctx, inst_ref, true)
}

pub fn emit_fp_vector_to_unsigned_fixed32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_to_fixed::<VReg4S>(code, ctx, inst_ref, false)
}

pub fn emit_fp_vector_to_unsigned_fixed64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_to_fixed::<VReg2D>(code, ctx, inst_ref, false)
}

pub fn emit_fp_vector_round_int16(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_round_int16(code, ctx, inst_ref)
}

pub fn emit_fp_vector_round_int32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_round_int::<VReg4S>(code, ctx, inst_ref)
}

pub fn emit_fp_vector_round_int64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_round_int::<VReg2D>(code, ctx, inst_ref)
}

pub fn emit_fp_vector_recip_step_fused32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, result, a, b| {
        code.frecps(result, a, b)
    })
}

pub fn emit_fp_vector_recip_step_fused64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_three_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, result, a, b| {
        code.frecps(result, a, b)
    })
}

pub fn emit_fp_vector_rsqrt_step_fused32(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_three_op_arranged::<VReg4S>(code, ctx, inst_ref, |code, result, a, b| {
        code.frsqrts(result, a, b)
    })
}

pub fn emit_fp_vector_rsqrt_step_fused64(
    code: &mut BlockOfCode,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let code = &mut CodeGenerator::new(code);
    emit_three_op_arranged::<VReg2D>(code, ctx, inst_ref, |code, result, a, b| {
        code.frsqrts(result, a, b)
    })
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
