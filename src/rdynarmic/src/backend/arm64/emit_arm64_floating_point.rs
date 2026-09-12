//! ARM64 scalar floating-point emission.
//!
//! Upstream owner: `backend/arm64/emit_arm64_floating_point.cpp`.

use crate::backend::arm64::abi::XSCRATCH0;
use crate::backend::arm64::emit_context::EmitContext;
use crate::backend::arm64::reg_alloc::RegAlloc;
use crate::ir::value::InstRef;
use rhazel::code_generator::CodeGenerator;
use rhazel::reg::{DReg, HReg, SReg, WReg, XReg};
use rhazel::SystemReg;

// Typed counterparts of the upstream RAReg conversions after Realize.
trait FpOperand: Copy {
    fn from_index(index: u8) -> Self;
}

impl FpOperand for HReg {
    fn from_index(index: u8) -> Self {
        Self::new(index)
    }
}
impl FpOperand for SReg {
    fn from_index(index: u8) -> Self {
        Self::new(index)
    }
}
impl FpOperand for DReg {
    fn from_index(index: u8) -> Self {
        Self::new(index)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FpSize {
    Single,
    Double,
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
            _ => Err(format!(
                "ARM64 floating point: invalid rounding mode {value}"
            )),
        }
    }

    fn fpcr_bits(self) -> Option<u32> {
        match self {
            Self::ToNearestTieEven => Some(0),
            Self::TowardsPlusInfinity => Some(1),
            Self::TowardsMinusInfinity => Some(2),
            Self::TowardsZero => Some(3),
            Self::ToNearestTieAwayFromZero | Self::ToOdd => None,
        }
    }
}

fn emit_mov_w_imm(code: &mut CodeGenerator<'_>, reg: u8, imm: u32) -> Result<(), String> {
    code.movz(WReg::new(reg), (imm & 0xffff) as u16, 0)?;
    let upper = ((imm >> 16) & 0xffff) as u16;
    if upper != 0 {
        code.movk(WReg::new(reg), upper, 16)?;
    }
    Ok(())
}

fn emit_with_rounding_fpcr(
    code: &mut CodeGenerator<'_>,
    ctx: &EmitContext<'_>,
    rounding_mode: RoundingMode,
    emit: impl FnOnce(&mut CodeGenerator<'_>) -> Result<(), String>,
) -> Result<(), String> {
    let Some(rounding_bits) = rounding_mode.fpcr_bits() else {
        return Err(format!(
            "ARM64 floating point: fixed to FP rounding mode {:?} is not supported by FPCR",
            rounding_mode
        ));
    };

    let current_fpcr = ctx.fpcr(true).value();
    let target_fpcr = (current_fpcr & !(0b11 << 22)) | (rounding_bits << 22);
    if target_fpcr == current_fpcr {
        return emit(code);
    }

    emit_mov_w_imm(code, XSCRATCH0, target_fpcr)?;
    code.msr(SystemReg::FPCR, XReg::new(XSCRATCH0))?;
    emit(code)?;
    emit_mov_w_imm(code, XSCRATCH0, current_fpcr)?;
    code.msr(SystemReg::FPCR, XReg::new(XSCRATCH0))?;
    Ok(())
}

fn emit_two_op<R: FpOperand>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: FpSize,
    emit: impl FnOnce(&mut CodeGenerator<'_>, R, R) -> Result<(), String>,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = match size {
        FpSize::Single => ctx.reg_alloc.write_s(inst_ref),
        FpSize::Double => ctx.reg_alloc.write_d(inst_ref),
    };
    let mut operand = match size {
        FpSize::Single => ctx.reg_alloc.read_s(args[0]),
        FpSize::Double => ctx.reg_alloc.read_d(args[0]),
    };
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    ctx.fpsr.load(code)?;
    emit(
        code,
        R::from_index(result.index().expect("result realized") as u8),
        R::from_index(operand.index().expect("operand realized") as u8),
    )?;
    Ok(())
}

fn emit_three_op<R: FpOperand>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: FpSize,
    emit: impl FnOnce(&mut CodeGenerator<'_>, R, R, R) -> Result<(), String>,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = match size {
        FpSize::Single => ctx.reg_alloc.write_s(inst_ref),
        FpSize::Double => ctx.reg_alloc.write_d(inst_ref),
    };
    let mut a = match size {
        FpSize::Single => ctx.reg_alloc.read_s(args[0]),
        FpSize::Double => ctx.reg_alloc.read_d(args[0]),
    };
    let mut b = match size {
        FpSize::Single => ctx.reg_alloc.read_s(args[1]),
        FpSize::Double => ctx.reg_alloc.read_d(args[1]),
    };
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut a, &mut b])?;
    ctx.fpsr.load(code)?;
    emit(
        code,
        R::from_index(result.index().expect("result realized") as u8),
        R::from_index(a.index().expect("a realized") as u8),
        R::from_index(b.index().expect("b realized") as u8),
    )?;
    Ok(())
}

fn emit_four_op<R: FpOperand>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: FpSize,
    emit: impl FnOnce(&mut CodeGenerator<'_>, R, R, R, R) -> Result<(), String>,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = match size {
        FpSize::Single => ctx.reg_alloc.write_s(inst_ref),
        FpSize::Double => ctx.reg_alloc.write_d(inst_ref),
    };
    let mut a = match size {
        FpSize::Single => ctx.reg_alloc.read_s(args[0]),
        FpSize::Double => ctx.reg_alloc.read_d(args[0]),
    };
    let mut b = match size {
        FpSize::Single => ctx.reg_alloc.read_s(args[1]),
        FpSize::Double => ctx.reg_alloc.read_d(args[1]),
    };
    let mut c = match size {
        FpSize::Single => ctx.reg_alloc.read_s(args[2]),
        FpSize::Double => ctx.reg_alloc.read_d(args[2]),
    };
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut a, &mut b, &mut c])?;
    ctx.fpsr.load(code)?;
    emit(
        code,
        R::from_index(result.index().expect("result realized") as u8),
        R::from_index(a.index().expect("a realized") as u8),
        R::from_index(b.index().expect("b realized") as u8),
        R::from_index(c.index().expect("c realized") as u8),
    )?;
    Ok(())
}

fn fpcr_rounding_mode(ctx: &EmitContext<'_>) -> Result<RoundingMode, String> {
    RoundingMode::from_u8(((ctx.fpcr(true).value() >> 22) & 0b11) as u8)
}

fn emit_convert<R: FpOperand, O: FpOperand>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    from: FpSize,
    to: FpSize,
    emit: impl FnOnce(&mut CodeGenerator<'_>, R, O) -> Result<(), String>,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let rounding_mode = RoundingMode::from_u8(args[1].get_immediate_u8())?;
    let fpcr_rounding_mode = fpcr_rounding_mode(ctx)?;
    if rounding_mode != fpcr_rounding_mode {
        return Err(format!(
            "ARM64 floating point: convert rounding mode {:?} does not match FPCR {:?}",
            rounding_mode, fpcr_rounding_mode
        ));
    }

    let mut result = match to {
        FpSize::Single => ctx.reg_alloc.write_s(inst_ref),
        FpSize::Double => ctx.reg_alloc.write_d(inst_ref),
    };
    let mut operand = match from {
        FpSize::Single => ctx.reg_alloc.read_s(args[0]),
        FpSize::Double => ctx.reg_alloc.read_d(args[0]),
    };
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    ctx.fpsr.load(code)?;
    emit(
        code,
        R::from_index(result.index().expect("result realized") as u8),
        O::from_index(operand.index().expect("operand realized") as u8),
    )?;
    Ok(())
}

fn emit_convert_half<R: FpOperand, O: FpOperand>(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    from_half: bool,
    other_size: FpSize,
    emit: impl FnOnce(&mut CodeGenerator<'_>, R, O) -> Result<(), String>,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let rounding_mode = RoundingMode::from_u8(args[1].get_immediate_u8())?;
    let fpcr_rounding_mode = fpcr_rounding_mode(ctx)?;
    if rounding_mode != fpcr_rounding_mode {
        return Err(format!(
            "ARM64 floating point: convert rounding mode {:?} does not match FPCR {:?}",
            rounding_mode, fpcr_rounding_mode
        ));
    }

    let mut result = if from_half {
        match other_size {
            FpSize::Single => ctx.reg_alloc.write_s(inst_ref),
            FpSize::Double => ctx.reg_alloc.write_d(inst_ref),
        }
    } else {
        ctx.reg_alloc.write_h(inst_ref)
    };
    let mut operand = if from_half {
        ctx.reg_alloc.read_h(args[0])
    } else {
        match other_size {
            FpSize::Single => ctx.reg_alloc.read_s(args[0]),
            FpSize::Double => ctx.reg_alloc.read_d(args[0]),
        }
    };
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    ctx.fpsr.load(code)?;
    emit(
        code,
        R::from_index(result.index().expect("result realized") as u8),
        O::from_index(operand.index().expect("operand realized") as u8),
    )?;
    Ok(())
}

fn emit_compare(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: FpSize,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut flags = ctx.reg_alloc.write_flags(inst_ref);
    let mut a = match size {
        FpSize::Single => ctx.reg_alloc.read_s(args[0]),
        FpSize::Double => ctx.reg_alloc.read_d(args[0]),
    };
    let exc_on_qnan = args[2].get_immediate_u1();

    if args[1].is_immediate() && args[1].get_immediate_u64() == 0 {
        RegAlloc::realize_all(code, ctx.block, &mut [&mut flags, &mut a])?;
        ctx.fpsr.load(code)?;
        let a = a.index().expect("a realized") as u8;
        let emission = match (size, exc_on_qnan) {
            (FpSize::Single, false) => code.fcmp_zero_fp(SReg::new(a)),
            (FpSize::Single, true) => code.fcmpe_zero_fp(SReg::new(a)),
            (FpSize::Double, false) => code.fcmp_zero_fp(DReg::new(a)),
            (FpSize::Double, true) => code.fcmpe_zero_fp(DReg::new(a)),
        };
        emission?;
        return Ok(());
    }

    let mut b = match size {
        FpSize::Single => ctx.reg_alloc.read_s(args[1]),
        FpSize::Double => ctx.reg_alloc.read_d(args[1]),
    };
    RegAlloc::realize_all(code, ctx.block, &mut [&mut flags, &mut a, &mut b])?;
    ctx.fpsr.load(code)?;
    let a = a.index().expect("a realized") as u8;
    let b = b.index().expect("b realized") as u8;
    let emission = match (size, exc_on_qnan) {
        (FpSize::Single, false) => code.fcmp_fp(SReg::new(a), SReg::new(b)),
        (FpSize::Single, true) => code.fcmpe_fp(SReg::new(a), SReg::new(b)),
        (FpSize::Double, false) => code.fcmp_fp(DReg::new(a), DReg::new(b)),
        (FpSize::Double, true) => code.fcmpe_fp(DReg::new(a), DReg::new(b)),
    };
    emission?;
    Ok(())
}

fn emit_to_fixed32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: FpSize,
    signed: bool,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let fbits = args[1].get_immediate_u8();
    let rounding_mode = RoundingMode::from_u8(args[2].get_immediate_u8())?;
    if fbits > 32 {
        return Err(format!(
            "ARM64 floating point: FP to 32-bit fixed with invalid fbits={fbits}"
        ));
    }
    if fbits != 0 && rounding_mode != RoundingMode::TowardsZero {
        return Err(format!(
            "ARM64 floating point: FP to 32-bit fixed with fbits={fbits} and rounding mode {:?} is not ported",
            rounding_mode
        ));
    }
    if rounding_mode == RoundingMode::ToOdd {
        return Err("ARM64 floating point: ToOdd FP to 32-bit fixed is not ported".to_string());
    }

    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut operand = match size {
        FpSize::Single => ctx.reg_alloc.read_s(args[0]),
        FpSize::Double => ctx.reg_alloc.read_d(args[0]),
    };
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    ctx.fpsr.load(code)?;
    let result = result.index().expect("result realized") as u8;
    let operand = operand.index().expect("operand realized") as u8;
    let emission = match (size, signed, rounding_mode, fbits) {
        (FpSize::Single, false, RoundingMode::TowardsZero, fbits) if fbits != 0 => {
            code.fcvtzu_fixed_fp(WReg::new(result), SReg::new(operand), fbits)
        }
        (FpSize::Double, false, RoundingMode::TowardsZero, fbits) if fbits != 0 => {
            code.fcvtzu_fixed_fp(WReg::new(result), DReg::new(operand), fbits)
        }
        (FpSize::Single, true, RoundingMode::TowardsZero, fbits) if fbits != 0 => {
            code.fcvtzs_fixed_fp(WReg::new(result), SReg::new(operand), fbits)
        }
        (FpSize::Double, true, RoundingMode::TowardsZero, fbits) if fbits != 0 => {
            code.fcvtzs_fixed_fp(WReg::new(result), DReg::new(operand), fbits)
        }
        (FpSize::Single, false, RoundingMode::ToNearestTieEven, _) => {
            code.fcvtnu_fp(WReg::new(result), SReg::new(operand))
        }
        (FpSize::Double, false, RoundingMode::ToNearestTieEven, _) => {
            code.fcvtnu_fp(WReg::new(result), DReg::new(operand))
        }
        (FpSize::Single, false, RoundingMode::TowardsPlusInfinity, _) => {
            code.fcvtpu_fp(WReg::new(result), SReg::new(operand))
        }
        (FpSize::Double, false, RoundingMode::TowardsPlusInfinity, _) => {
            code.fcvtpu_fp(WReg::new(result), DReg::new(operand))
        }
        (FpSize::Single, false, RoundingMode::TowardsMinusInfinity, _) => {
            code.fcvtmu_fp(WReg::new(result), SReg::new(operand))
        }
        (FpSize::Double, false, RoundingMode::TowardsMinusInfinity, _) => {
            code.fcvtmu_fp(WReg::new(result), DReg::new(operand))
        }
        (FpSize::Single, false, RoundingMode::TowardsZero, _) => {
            code.fcvtzu_fp(WReg::new(result), SReg::new(operand))
        }
        (FpSize::Double, false, RoundingMode::TowardsZero, _) => {
            code.fcvtzu_fp(WReg::new(result), DReg::new(operand))
        }
        (FpSize::Single, false, RoundingMode::ToNearestTieAwayFromZero, _) => {
            code.fcvtau_fp(WReg::new(result), SReg::new(operand))
        }
        (FpSize::Double, false, RoundingMode::ToNearestTieAwayFromZero, _) => {
            code.fcvtau_fp(WReg::new(result), DReg::new(operand))
        }
        (FpSize::Single, true, RoundingMode::ToNearestTieEven, _) => {
            code.fcvtns_fp(WReg::new(result), SReg::new(operand))
        }
        (FpSize::Double, true, RoundingMode::ToNearestTieEven, _) => {
            code.fcvtns_fp(WReg::new(result), DReg::new(operand))
        }
        (FpSize::Single, true, RoundingMode::TowardsPlusInfinity, _) => {
            code.fcvtps_fp(WReg::new(result), SReg::new(operand))
        }
        (FpSize::Double, true, RoundingMode::TowardsPlusInfinity, _) => {
            code.fcvtps_fp(WReg::new(result), DReg::new(operand))
        }
        (FpSize::Single, true, RoundingMode::TowardsMinusInfinity, _) => {
            code.fcvtms_fp(WReg::new(result), SReg::new(operand))
        }
        (FpSize::Double, true, RoundingMode::TowardsMinusInfinity, _) => {
            code.fcvtms_fp(WReg::new(result), DReg::new(operand))
        }
        (FpSize::Single, true, RoundingMode::TowardsZero, _) => {
            code.fcvtzs_fp(WReg::new(result), SReg::new(operand))
        }
        (FpSize::Double, true, RoundingMode::TowardsZero, _) => {
            code.fcvtzs_fp(WReg::new(result), DReg::new(operand))
        }
        (FpSize::Single, true, RoundingMode::ToNearestTieAwayFromZero, _) => {
            code.fcvtas_fp(WReg::new(result), SReg::new(operand))
        }
        (FpSize::Double, true, RoundingMode::ToNearestTieAwayFromZero, _) => {
            code.fcvtas_fp(WReg::new(result), DReg::new(operand))
        }
        (_, _, RoundingMode::ToOdd, _) => unreachable!(),
    };
    emission?;
    Ok(())
}

fn emit_to_fixed16(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: FpSize,
    signed: bool,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let fbits = args[1].get_immediate_u8();
    let rounding_mode = RoundingMode::from_u8(args[2].get_immediate_u8())?;
    if fbits > 16 {
        return Err(format!(
            "ARM64 floating point: FP to 16-bit fixed with invalid fbits={fbits}"
        ));
    }
    if rounding_mode != RoundingMode::TowardsZero {
        return Err(format!(
            "ARM64 floating point: FP to 16-bit fixed requires TowardsZero, got {:?}",
            rounding_mode
        ));
    }

    let mut result = ctx.reg_alloc.write_w(inst_ref);
    let mut operand = match size {
        FpSize::Single => ctx.reg_alloc.read_s(args[0]),
        FpSize::Double => ctx.reg_alloc.read_d(args[0]),
    };
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    ctx.fpsr.load(code)?;
    let result = result.index().expect("result realized") as u8;
    let operand = operand.index().expect("operand realized") as u8;
    let scaled_fbits = fbits + 16;
    let emission = match (size, signed) {
        (FpSize::Single, false) => {
            code.fcvtzu_fixed_fp(WReg::new(result), SReg::new(operand), scaled_fbits)
        }
        (FpSize::Double, false) => {
            code.fcvtzu_fixed_fp(WReg::new(result), DReg::new(operand), scaled_fbits)
        }
        (FpSize::Single, true) => {
            code.fcvtzs_fixed_fp(WReg::new(result), SReg::new(operand), scaled_fbits)
        }
        (FpSize::Double, true) => {
            code.fcvtzs_fixed_fp(WReg::new(result), DReg::new(operand), scaled_fbits)
        }
    };
    emission?;
    if signed {
        code.asr(WReg::new(XSCRATCH0), WReg::new(result), 31)?;
        code.add_lsr_fp(
            WReg::new(result),
            WReg::new(result),
            WReg::new(XSCRATCH0),
            16,
        )?;
    }
    code.lsr_fp(WReg::new(result), WReg::new(result), 16)?;
    Ok(())
}

fn emit_from_fixed32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: FpSize,
    signed: bool,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let fbits = args[1].get_immediate_u8();
    let rounding_mode = RoundingMode::from_u8(args[2].get_immediate_u8())?;
    if fbits > 32 {
        return Err(format!(
            "ARM64 floating point: 32-bit fixed to FP with invalid fbits={fbits}"
        ));
    }

    let mut result = match size {
        FpSize::Single => ctx.reg_alloc.write_s(inst_ref),
        FpSize::Double => ctx.reg_alloc.write_d(inst_ref),
    };
    let mut operand = ctx.reg_alloc.read_w(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    ctx.fpsr.load(code)?;
    let result = result.index().expect("result realized") as u8;
    let operand = operand.index().expect("operand realized") as u8;
    emit_with_rounding_fpcr(code, ctx, rounding_mode, |code| {
        match (size, signed) {
            (FpSize::Single, false) if fbits == 0 => {
                code.ucvtf_fp(SReg::new(result), WReg::new(operand))
            }
            (FpSize::Double, false) if fbits == 0 => {
                code.ucvtf_fp(DReg::new(result), WReg::new(operand))
            }
            (FpSize::Single, true) if fbits == 0 => {
                code.scvtf_fp(SReg::new(result), WReg::new(operand))
            }
            (FpSize::Double, true) if fbits == 0 => {
                code.scvtf_fp(DReg::new(result), WReg::new(operand))
            }
            (FpSize::Single, false) => {
                code.ucvtf_fixed_fp(SReg::new(result), WReg::new(operand), fbits)
            }
            (FpSize::Double, false) => {
                code.ucvtf_fixed_fp(DReg::new(result), WReg::new(operand), fbits)
            }
            (FpSize::Single, true) => {
                code.scvtf_fixed_fp(SReg::new(result), WReg::new(operand), fbits)
            }
            (FpSize::Double, true) => {
                code.scvtf_fixed_fp(DReg::new(result), WReg::new(operand), fbits)
            }
        }?;
        Ok(())
    })
}

fn emit_from_fixed16(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: FpSize,
    signed: bool,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let fbits = args[1].get_immediate_u8();
    let rounding_mode = RoundingMode::from_u8(args[2].get_immediate_u8())?;
    if fbits > 16 {
        return Err(format!(
            "ARM64 floating point: 16-bit fixed to FP with invalid fbits={fbits}"
        ));
    }

    let mut result = match size {
        FpSize::Single => ctx.reg_alloc.write_s(inst_ref),
        FpSize::Double => ctx.reg_alloc.write_d(inst_ref),
    };
    let mut operand = ctx.reg_alloc.read_w(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    ctx.fpsr.load(code)?;
    let result = result.index().expect("result realized") as u8;
    let operand = operand.index().expect("operand realized") as u8;
    let scaled_fbits = fbits + 16;
    emit_with_rounding_fpcr(code, ctx, rounding_mode, |code| {
        code.lsl(WReg::new(XSCRATCH0), WReg::new(operand), 16)?;
        match (size, signed) {
            (FpSize::Single, false) => {
                code.ucvtf_fixed_fp(SReg::new(result), WReg::new(XSCRATCH0), scaled_fbits)
            }
            (FpSize::Double, false) => {
                code.ucvtf_fixed_fp(DReg::new(result), WReg::new(XSCRATCH0), scaled_fbits)
            }
            (FpSize::Single, true) => {
                code.scvtf_fixed_fp(SReg::new(result), WReg::new(XSCRATCH0), scaled_fbits)
            }
            (FpSize::Double, true) => {
                code.scvtf_fixed_fp(DReg::new(result), WReg::new(XSCRATCH0), scaled_fbits)
            }
        }?;
        Ok(())
    })
}

fn emit_to_fixed64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: FpSize,
    signed: bool,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let fbits = args[1].get_immediate_u8();
    let rounding_mode = RoundingMode::from_u8(args[2].get_immediate_u8())?;
    if fbits > 64 {
        return Err(format!(
            "ARM64 floating point: FP to 64-bit fixed with invalid fbits={fbits}"
        ));
    }
    if rounding_mode == RoundingMode::ToOdd {
        return Err("ARM64 floating point: ToOdd FP to 64-bit fixed is not ported".to_string());
    }
    if fbits != 0 && rounding_mode != RoundingMode::TowardsZero {
        return Err(format!(
            "ARM64 floating point: FP to 64-bit fixed with fbits={fbits} and rounding mode {:?} is not ported",
            rounding_mode
        ));
    }

    let mut result = ctx.reg_alloc.write_x(inst_ref);
    let mut operand = match size {
        FpSize::Single => ctx.reg_alloc.read_s(args[0]),
        FpSize::Double => ctx.reg_alloc.read_d(args[0]),
    };
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    ctx.fpsr.load(code)?;
    let result = result.index().expect("result realized") as u8;
    let operand = operand.index().expect("operand realized") as u8;
    let emission = match (size, signed, rounding_mode, fbits) {
        (FpSize::Single, false, RoundingMode::ToNearestTieEven, 0) => {
            code.fcvtnu_fp(XReg::new(result), SReg::new(operand))
        }
        (FpSize::Double, false, RoundingMode::ToNearestTieEven, 0) => {
            code.fcvtnu_fp(XReg::new(result), DReg::new(operand))
        }
        (FpSize::Single, false, RoundingMode::TowardsPlusInfinity, 0) => {
            code.fcvtpu_fp(XReg::new(result), SReg::new(operand))
        }
        (FpSize::Double, false, RoundingMode::TowardsPlusInfinity, 0) => {
            code.fcvtpu_fp(XReg::new(result), DReg::new(operand))
        }
        (FpSize::Single, false, RoundingMode::TowardsMinusInfinity, 0) => {
            code.fcvtmu_fp(XReg::new(result), SReg::new(operand))
        }
        (FpSize::Double, false, RoundingMode::TowardsMinusInfinity, 0) => {
            code.fcvtmu_fp(XReg::new(result), DReg::new(operand))
        }
        (FpSize::Single, false, RoundingMode::TowardsZero, 0) => {
            code.fcvtzu_fp(XReg::new(result), SReg::new(operand))
        }
        (FpSize::Double, false, RoundingMode::TowardsZero, 0) => {
            code.fcvtzu_fp(XReg::new(result), DReg::new(operand))
        }
        (FpSize::Single, false, RoundingMode::ToNearestTieAwayFromZero, 0) => {
            code.fcvtau_fp(XReg::new(result), SReg::new(operand))
        }
        (FpSize::Double, false, RoundingMode::ToNearestTieAwayFromZero, 0) => {
            code.fcvtau_fp(XReg::new(result), DReg::new(operand))
        }
        (FpSize::Single, true, RoundingMode::ToNearestTieEven, 0) => {
            code.fcvtns_fp(XReg::new(result), SReg::new(operand))
        }
        (FpSize::Double, true, RoundingMode::ToNearestTieEven, 0) => {
            code.fcvtns_fp(XReg::new(result), DReg::new(operand))
        }
        (FpSize::Single, true, RoundingMode::TowardsPlusInfinity, 0) => {
            code.fcvtps_fp(XReg::new(result), SReg::new(operand))
        }
        (FpSize::Double, true, RoundingMode::TowardsPlusInfinity, 0) => {
            code.fcvtps_fp(XReg::new(result), DReg::new(operand))
        }
        (FpSize::Single, true, RoundingMode::TowardsMinusInfinity, 0) => {
            code.fcvtms_fp(XReg::new(result), SReg::new(operand))
        }
        (FpSize::Double, true, RoundingMode::TowardsMinusInfinity, 0) => {
            code.fcvtms_fp(XReg::new(result), DReg::new(operand))
        }
        (FpSize::Single, true, RoundingMode::TowardsZero, 0) => {
            code.fcvtzs_fp(XReg::new(result), SReg::new(operand))
        }
        (FpSize::Double, true, RoundingMode::TowardsZero, 0) => {
            code.fcvtzs_fp(XReg::new(result), DReg::new(operand))
        }
        (FpSize::Single, true, RoundingMode::ToNearestTieAwayFromZero, 0) => {
            code.fcvtas_fp(XReg::new(result), SReg::new(operand))
        }
        (FpSize::Double, true, RoundingMode::ToNearestTieAwayFromZero, 0) => {
            code.fcvtas_fp(XReg::new(result), DReg::new(operand))
        }
        (FpSize::Single, false, RoundingMode::TowardsZero, _) => {
            code.fcvtzu_fixed_fp(XReg::new(result), SReg::new(operand), fbits)
        }
        (FpSize::Double, false, RoundingMode::TowardsZero, _) => {
            code.fcvtzu_fixed_fp(XReg::new(result), DReg::new(operand), fbits)
        }
        (FpSize::Single, true, RoundingMode::TowardsZero, _) => {
            code.fcvtzs_fixed_fp(XReg::new(result), SReg::new(operand), fbits)
        }
        (FpSize::Double, true, RoundingMode::TowardsZero, _) => {
            code.fcvtzs_fixed_fp(XReg::new(result), DReg::new(operand), fbits)
        }
        (_, _, RoundingMode::ToOdd, _) => unreachable!(),
        _ => unreachable!("unsupported fbits/rounding combination checked above"),
    };
    emission?;
    Ok(())
}

fn emit_from_fixed64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: FpSize,
    signed: bool,
) -> Result<(), String> {
    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let fbits = args[1].get_immediate_u8();
    let rounding_mode = RoundingMode::from_u8(args[2].get_immediate_u8())?;
    if fbits > 64 {
        return Err(format!(
            "ARM64 floating point: 64-bit fixed to FP with invalid fbits={fbits}"
        ));
    }

    let mut result = match size {
        FpSize::Single => ctx.reg_alloc.write_s(inst_ref),
        FpSize::Double => ctx.reg_alloc.write_d(inst_ref),
    };
    let mut operand = ctx.reg_alloc.read_x(args[0]);
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    ctx.fpsr.load(code)?;
    let result = result.index().expect("result realized") as u8;
    let operand = operand.index().expect("operand realized") as u8;
    emit_with_rounding_fpcr(code, ctx, rounding_mode, |code| {
        match (size, signed, fbits) {
            (FpSize::Single, false, 0) => code.ucvtf_fp(SReg::new(result), XReg::new(operand)),
            (FpSize::Double, false, 0) => code.ucvtf_fp(DReg::new(result), XReg::new(operand)),
            (FpSize::Single, true, 0) => code.scvtf_fp(SReg::new(result), XReg::new(operand)),
            (FpSize::Double, true, 0) => code.scvtf_fp(DReg::new(result), XReg::new(operand)),
            (FpSize::Single, false, _) => {
                code.ucvtf_fixed_fp(SReg::new(result), XReg::new(operand), fbits)
            }
            (FpSize::Double, false, _) => {
                code.ucvtf_fixed_fp(DReg::new(result), XReg::new(operand), fbits)
            }
            (FpSize::Single, true, _) => {
                code.scvtf_fixed_fp(SReg::new(result), XReg::new(operand), fbits)
            }
            (FpSize::Double, true, _) => {
                code.scvtf_fixed_fp(DReg::new(result), XReg::new(operand), fbits)
            }
        }?;
        Ok(())
    })
}

pub fn emit_fp_compare32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_compare(code, ctx, inst_ref, FpSize::Single)
}

pub fn emit_fp_compare64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_compare(code, ctx, inst_ref, FpSize::Double)
}

pub fn emit_fp_mul32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op(
        code,
        ctx,
        inst_ref,
        FpSize::Single,
        |code, rd: SReg, rn: SReg, rm: SReg| code.fmul_fp(rd, rn, rm),
    )
}

pub fn emit_fp_mul64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op(
        code,
        ctx,
        inst_ref,
        FpSize::Double,
        |code, rd: DReg, rn: DReg, rm: DReg| code.fmul_fp(rd, rn, rm),
    )
}

pub fn emit_fp_mul_x32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op(
        code,
        ctx,
        inst_ref,
        FpSize::Single,
        |code, rd: SReg, rn: SReg, rm: SReg| code.fmulx_fp(rd, rn, rm),
    )
}

pub fn emit_fp_mul_x64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op(
        code,
        ctx,
        inst_ref,
        FpSize::Double,
        |code, rd: DReg, rn: DReg, rm: DReg| code.fmulx_fp(rd, rn, rm),
    )
}

pub fn emit_fp_add32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op(
        code,
        ctx,
        inst_ref,
        FpSize::Single,
        |code, rd: SReg, rn: SReg, rm: SReg| code.fadd_fp(rd, rn, rm),
    )
}

pub fn emit_fp_add64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op(
        code,
        ctx,
        inst_ref,
        FpSize::Double,
        |code, rd: DReg, rn: DReg, rm: DReg| code.fadd_fp(rd, rn, rm),
    )
}

pub fn emit_fp_sub32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op(
        code,
        ctx,
        inst_ref,
        FpSize::Single,
        |code, rd: SReg, rn: SReg, rm: SReg| code.fsub_fp(rd, rn, rm),
    )
}

pub fn emit_fp_sub64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op(
        code,
        ctx,
        inst_ref,
        FpSize::Double,
        |code, rd: DReg, rn: DReg, rm: DReg| code.fsub_fp(rd, rn, rm),
    )
}

pub fn emit_fp_div32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op(
        code,
        ctx,
        inst_ref,
        FpSize::Single,
        |code, rd: SReg, rn: SReg, rm: SReg| code.fdiv_fp(rd, rn, rm),
    )
}

pub fn emit_fp_div64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op(
        code,
        ctx,
        inst_ref,
        FpSize::Double,
        |code, rd: DReg, rn: DReg, rm: DReg| code.fdiv_fp(rd, rn, rm),
    )
}

pub fn emit_fp_abs32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_two_op(
        code,
        ctx,
        inst_ref,
        FpSize::Single,
        |code, rd: SReg, rn: SReg| code.fabs_fp(rd, rn),
    )
}

pub fn emit_fp_abs64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_two_op(
        code,
        ctx,
        inst_ref,
        FpSize::Double,
        |code, rd: DReg, rn: DReg| code.fabs_fp(rd, rn),
    )
}

pub fn emit_fp_max_numeric32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op(
        code,
        ctx,
        inst_ref,
        FpSize::Single,
        |code, rd: SReg, rn: SReg, rm: SReg| code.fmaxnm_fp(rd, rn, rm),
    )
}

pub fn emit_fp_max32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op(
        code,
        ctx,
        inst_ref,
        FpSize::Single,
        |code, rd: SReg, rn: SReg, rm: SReg| code.fmax_fp(rd, rn, rm),
    )
}

pub fn emit_fp_max_numeric64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op(
        code,
        ctx,
        inst_ref,
        FpSize::Double,
        |code, rd: DReg, rn: DReg, rm: DReg| code.fmaxnm_fp(rd, rn, rm),
    )
}

pub fn emit_fp_max64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op(
        code,
        ctx,
        inst_ref,
        FpSize::Double,
        |code, rd: DReg, rn: DReg, rm: DReg| code.fmax_fp(rd, rn, rm),
    )
}

pub fn emit_fp_mul_add32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_four_op(
        code,
        ctx,
        inst_ref,
        FpSize::Single,
        |code, result: SReg, a, b, c| code.fmadd_fp(result, b, c, a),
    )
}

pub fn emit_fp_mul_add64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_four_op(
        code,
        ctx,
        inst_ref,
        FpSize::Double,
        |code, result: DReg, a, b, c| code.fmadd_fp(result, b, c, a),
    )
}

pub fn emit_fp_mul_sub32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_four_op(
        code,
        ctx,
        inst_ref,
        FpSize::Single,
        |code, result: SReg, a, b, c| code.fmsub_fp(result, b, c, a),
    )
}

pub fn emit_fp_mul_sub64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_four_op(
        code,
        ctx,
        inst_ref,
        FpSize::Double,
        |code, result: DReg, a, b, c| code.fmsub_fp(result, b, c, a),
    )
}

pub fn emit_fp_min_numeric32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op(
        code,
        ctx,
        inst_ref,
        FpSize::Single,
        |code, rd: SReg, rn: SReg, rm: SReg| code.fminnm_fp(rd, rn, rm),
    )
}

pub fn emit_fp_min32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op(
        code,
        ctx,
        inst_ref,
        FpSize::Single,
        |code, rd: SReg, rn: SReg, rm: SReg| code.fmin_fp(rd, rn, rm),
    )
}

pub fn emit_fp_min_numeric64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op(
        code,
        ctx,
        inst_ref,
        FpSize::Double,
        |code, rd: DReg, rn: DReg, rm: DReg| code.fminnm_fp(rd, rn, rm),
    )
}

pub fn emit_fp_min64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op(
        code,
        ctx,
        inst_ref,
        FpSize::Double,
        |code, rd: DReg, rn: DReg, rm: DReg| code.fmin_fp(rd, rn, rm),
    )
}

pub fn emit_fp_neg32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_two_op(
        code,
        ctx,
        inst_ref,
        FpSize::Single,
        |code, rd: SReg, rn: SReg| code.fneg_fp(rd, rn),
    )
}

pub fn emit_fp_neg64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_two_op(
        code,
        ctx,
        inst_ref,
        FpSize::Double,
        |code, rd: DReg, rn: DReg| code.fneg_fp(rd, rn),
    )
}

pub fn emit_fp_recip_estimate32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_two_op(
        code,
        ctx,
        inst_ref,
        FpSize::Single,
        |code, rd: SReg, rn: SReg| code.frecpe_fp(rd, rn),
    )
}

pub fn emit_fp_recip_estimate64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_two_op(
        code,
        ctx,
        inst_ref,
        FpSize::Double,
        |code, rd: DReg, rn: DReg| code.frecpe_fp(rd, rn),
    )
}

pub fn emit_fp_recip_exponent32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_two_op(
        code,
        ctx,
        inst_ref,
        FpSize::Single,
        |code, rd: SReg, rn: SReg| code.frecpx_fp(rd, rn),
    )
}

pub fn emit_fp_recip_exponent64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_two_op(
        code,
        ctx,
        inst_ref,
        FpSize::Double,
        |code, rd: DReg, rn: DReg| code.frecpx_fp(rd, rn),
    )
}

pub fn emit_fp_recip_step_fused32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op(
        code,
        ctx,
        inst_ref,
        FpSize::Single,
        |code, rd: SReg, rn: SReg, rm: SReg| code.frecps_fp(rd, rn, rm),
    )
}

pub fn emit_fp_recip_step_fused64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op(
        code,
        ctx,
        inst_ref,
        FpSize::Double,
        |code, rd: DReg, rn: DReg, rm: DReg| code.frecps_fp(rd, rn, rm),
    )
}

pub fn emit_fp_rsqrt_estimate32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_two_op(
        code,
        ctx,
        inst_ref,
        FpSize::Single,
        |code, rd: SReg, rn: SReg| code.frsqrte_fp(rd, rn),
    )
}

pub fn emit_fp_rsqrt_estimate64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_two_op(
        code,
        ctx,
        inst_ref,
        FpSize::Double,
        |code, rd: DReg, rn: DReg| code.frsqrte_fp(rd, rn),
    )
}

pub fn emit_fp_rsqrt_step_fused32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op(
        code,
        ctx,
        inst_ref,
        FpSize::Single,
        |code, rd: SReg, rn: SReg, rm: SReg| code.frsqrts_fp(rd, rn, rm),
    )
}

pub fn emit_fp_rsqrt_step_fused64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_three_op(
        code,
        ctx,
        inst_ref,
        FpSize::Double,
        |code, rd: DReg, rn: DReg, rm: DReg| code.frsqrts_fp(rd, rn, rm),
    )
}

fn emit_fp_round_int(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
    size: FpSize,
) -> Result<(), String> {
    let rounding_mode = RoundingMode::from_u8(ctx.block.get(inst_ref).arg(1).get_u8())?;
    let exact = ctx.block.get(inst_ref).arg(2).get_u1();

    let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
    let mut result = match size {
        FpSize::Single => ctx.reg_alloc.write_s(inst_ref),
        FpSize::Double => ctx.reg_alloc.write_d(inst_ref),
    };
    let mut operand = match size {
        FpSize::Single => ctx.reg_alloc.read_s(args[0]),
        FpSize::Double => ctx.reg_alloc.read_d(args[0]),
    };
    RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
    ctx.fpsr.load(code)?;

    let result = result.index().expect("result realized") as u8;
    let operand = operand.index().expect("operand realized") as u8;

    let emission = if exact {
        let fpcr_rounding_mode = fpcr_rounding_mode(ctx)?;
        if fpcr_rounding_mode != rounding_mode {
            return Err(format!(
                "ARM64 floating point: exact FPRoundInt rounding mode {:?} does not match FPCR {:?}",
                rounding_mode, fpcr_rounding_mode
            ));
        }
        match size {
            FpSize::Single => code.frintx_fp(SReg::new(result), SReg::new(operand)),
            FpSize::Double => code.frintx_fp(DReg::new(result), DReg::new(operand)),
        }
    } else {
        match (size, rounding_mode) {
            (FpSize::Single, RoundingMode::ToNearestTieEven) => {
                code.frintn_fp(SReg::new(result), SReg::new(operand))
            }
            (FpSize::Double, RoundingMode::ToNearestTieEven) => {
                code.frintn_fp(DReg::new(result), DReg::new(operand))
            }
            (FpSize::Single, RoundingMode::TowardsPlusInfinity) => {
                code.frintp_fp(SReg::new(result), SReg::new(operand))
            }
            (FpSize::Double, RoundingMode::TowardsPlusInfinity) => {
                code.frintp_fp(DReg::new(result), DReg::new(operand))
            }
            (FpSize::Single, RoundingMode::TowardsMinusInfinity) => {
                code.frintm_fp(SReg::new(result), SReg::new(operand))
            }
            (FpSize::Double, RoundingMode::TowardsMinusInfinity) => {
                code.frintm_fp(DReg::new(result), DReg::new(operand))
            }
            (FpSize::Single, RoundingMode::TowardsZero) => {
                code.frintz_fp(SReg::new(result), SReg::new(operand))
            }
            (FpSize::Double, RoundingMode::TowardsZero) => {
                code.frintz_fp(DReg::new(result), DReg::new(operand))
            }
            (FpSize::Single, RoundingMode::ToNearestTieAwayFromZero) => {
                code.frinta_fp(SReg::new(result), SReg::new(operand))
            }
            (FpSize::Double, RoundingMode::ToNearestTieAwayFromZero) => {
                code.frinta_fp(DReg::new(result), DReg::new(operand))
            }
            (_, RoundingMode::ToOdd) => {
                return Err("ARM64 floating point: ToOdd FPRoundInt is not ported".to_string());
            }
        }
    };

    emission?;
    Ok(())
}

pub fn emit_fp_round_int32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_fp_round_int(code, ctx, inst_ref, FpSize::Single)
}

pub fn emit_fp_round_int64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_fp_round_int(code, ctx, inst_ref, FpSize::Double)
}

pub fn emit_fp_sqrt32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_two_op(
        code,
        ctx,
        inst_ref,
        FpSize::Single,
        |code, rd: SReg, rn: SReg| code.fsqrt_fp(rd, rn),
    )
}

pub fn emit_fp_sqrt64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_two_op(
        code,
        ctx,
        inst_ref,
        FpSize::Double,
        |code, rd: DReg, rn: DReg| code.fsqrt_fp(rd, rn),
    )
}

pub fn emit_fp_single_to_double(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_convert(
        code,
        ctx,
        inst_ref,
        FpSize::Single,
        FpSize::Double,
        |code, rd: DReg, rn: SReg| code.fcvt_d_from_s_fp(rd, rn),
    )
}

pub fn emit_fp_half_to_single(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_convert_half(
        code,
        ctx,
        inst_ref,
        true,
        FpSize::Single,
        |code, rd: SReg, rn: HReg| code.fcvt_s_from_h_fp(rd, rn),
    )
}

pub fn emit_fp_half_to_double(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_convert_half(
        code,
        ctx,
        inst_ref,
        true,
        FpSize::Double,
        |code, rd: DReg, rn: HReg| code.fcvt_d_from_h_fp(rd, rn),
    )
}

pub fn emit_fp_single_to_half(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_convert_half(
        code,
        ctx,
        inst_ref,
        false,
        FpSize::Single,
        |code, rd: HReg, rn: SReg| code.fcvt_h_from_s_fp(rd, rn),
    )
}

pub fn emit_fp_double_to_half(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_convert_half(
        code,
        ctx,
        inst_ref,
        false,
        FpSize::Double,
        |code, rd: HReg, rn: DReg| code.fcvt_h_from_d_fp(rd, rn),
    )
}

pub fn emit_fp_double_to_single(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    let rounding_mode = RoundingMode::from_u8(ctx.block.get(inst_ref).arg(1).get_u8())?;

    if rounding_mode == RoundingMode::ToOdd {
        let args = ctx.reg_alloc.get_argument_info(ctx.block, inst_ref);
        let mut result = ctx.reg_alloc.write_s(inst_ref);
        let mut operand = ctx.reg_alloc.read_d(args[0]);
        RegAlloc::realize_all(code, ctx.block, &mut [&mut result, &mut operand])?;
        ctx.fpsr.load(code)?;
        code.fcvtxn_s_from_d_fp(
            SReg::new(result.index().expect("result realized") as u8),
            DReg::new(operand.index().expect("operand realized") as u8),
        )?;
        return Ok(());
    }

    emit_convert(
        code,
        ctx,
        inst_ref,
        FpSize::Double,
        FpSize::Single,
        |code, rd: SReg, rn: DReg| code.fcvt_s_from_d_fp(rd, rn),
    )
}

pub fn emit_fp_single_to_fixed_u32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_to_fixed32(code, ctx, inst_ref, FpSize::Single, false)
}

pub fn emit_fp_single_to_fixed_u16(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_to_fixed16(code, ctx, inst_ref, FpSize::Single, false)
}

pub fn emit_fp_double_to_fixed_u16(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_to_fixed16(code, ctx, inst_ref, FpSize::Double, false)
}

pub fn emit_fp_single_to_fixed_s16(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_to_fixed16(code, ctx, inst_ref, FpSize::Single, true)
}

pub fn emit_fp_double_to_fixed_s16(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_to_fixed16(code, ctx, inst_ref, FpSize::Double, true)
}

pub fn emit_fp_double_to_fixed_u32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_to_fixed32(code, ctx, inst_ref, FpSize::Double, false)
}

pub fn emit_fp_single_to_fixed_s32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_to_fixed32(code, ctx, inst_ref, FpSize::Single, true)
}

pub fn emit_fp_double_to_fixed_s32(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_to_fixed32(code, ctx, inst_ref, FpSize::Double, true)
}

pub fn emit_fp_single_to_fixed_u64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_to_fixed64(code, ctx, inst_ref, FpSize::Single, false)
}

pub fn emit_fp_double_to_fixed_u64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_to_fixed64(code, ctx, inst_ref, FpSize::Double, false)
}

pub fn emit_fp_single_to_fixed_s64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_to_fixed64(code, ctx, inst_ref, FpSize::Single, true)
}

pub fn emit_fp_double_to_fixed_s64(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_to_fixed64(code, ctx, inst_ref, FpSize::Double, true)
}

pub fn emit_fp_fixed_u16_to_single(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_from_fixed16(code, ctx, inst_ref, FpSize::Single, false)
}

pub fn emit_fp_fixed_u16_to_double(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_from_fixed16(code, ctx, inst_ref, FpSize::Double, false)
}

pub fn emit_fp_fixed_s16_to_single(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_from_fixed16(code, ctx, inst_ref, FpSize::Single, true)
}

pub fn emit_fp_fixed_s16_to_double(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_from_fixed16(code, ctx, inst_ref, FpSize::Double, true)
}

pub fn emit_fp_fixed_u32_to_single(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_from_fixed32(code, ctx, inst_ref, FpSize::Single, false)
}

pub fn emit_fp_fixed_u32_to_double(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_from_fixed32(code, ctx, inst_ref, FpSize::Double, false)
}

pub fn emit_fp_fixed_s32_to_single(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_from_fixed32(code, ctx, inst_ref, FpSize::Single, true)
}

pub fn emit_fp_fixed_s32_to_double(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_from_fixed32(code, ctx, inst_ref, FpSize::Double, true)
}

pub fn emit_fp_fixed_u64_to_single(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_from_fixed64(code, ctx, inst_ref, FpSize::Single, false)
}

pub fn emit_fp_fixed_u64_to_double(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_from_fixed64(code, ctx, inst_ref, FpSize::Double, false)
}

pub fn emit_fp_fixed_s64_to_single(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_from_fixed64(code, ctx, inst_ref, FpSize::Single, true)
}

pub fn emit_fp_fixed_s64_to_double(
    code: &mut CodeGenerator<'_>,
    ctx: &mut EmitContext<'_>,
    inst_ref: InstRef,
) -> Result<(), String> {
    emit_from_fixed64(code, ctx, inst_ref, FpSize::Double, true)
}
