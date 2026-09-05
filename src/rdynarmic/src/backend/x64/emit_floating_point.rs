use rxbyak::{qword_ptr, JmpType, Reg, RegExp, CL, R15, RSP};

use crate::backend::x64::abi;
use crate::backend::x64::constants::convert_rounding_mode_to_x64_immediate;
use crate::backend::x64::emit_context::{DeferredEmitCtx, EmitContext};
use crate::backend::x64::fp_helpers;
use crate::backend::x64::host_feature::HostFeature;
use crate::backend::x64::hostloc::{HostLoc, HOST_RCX};
use crate::backend::x64::reg_alloc::RegAlloc;
use crate::common::fp::fpcr::Fpcr;
use crate::common::fp::fpsr::Fpsr;
use crate::common::fp::info::FloatFormat;
use crate::common::fp::op::fp_round_int::fp_round_int;
use crate::common::fp::process_nan::process_nan;
use crate::common::fp::rounding_mode::RoundingMode;
use crate::common::fp::unpacked::{fp_unpack, FpType};
use crate::interface::optimization_flags::OptimizationFlag;
use crate::ir::inst::Inst;
use crate::ir::value::InstRef;

// ---------------------------------------------------------------------------
// Helper: emit a host_call to a Rust function with N args, returning result in RAX
// ---------------------------------------------------------------------------

fn emit_host_call_1(ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst, func: usize) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    ra.host_call(Some(inst_ref), &mut [Some(&mut args[0]), None, None, None]);
    ra.asm.mov(rxbyak::RAX, func as i64).unwrap();
    ra.asm.call_reg(rxbyak::RAX).unwrap();
}

fn emit_host_call_2(ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst, func: usize) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let (first, rest) = args.split_at_mut(1);
    ra.host_call(
        Some(inst_ref),
        &mut [Some(&mut first[0]), Some(&mut rest[0]), None, None],
    );
    ra.asm.mov(rxbyak::RAX, func as i64).unwrap();
    ra.asm.call_reg(rxbyak::RAX).unwrap();
}

fn emit_host_call_3(ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst, func: usize) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    emit_host_call_3_with_args(ra, inst_ref, &mut args, func);
}

fn emit_host_call_3_with_args(
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    args: &mut crate::backend::x64::reg_alloc::ArgumentInfo,
    func: usize,
) {
    let (first, rest) = args.split_at_mut(1);
    let (second, rest2) = rest.split_at_mut(1);
    ra.host_call(
        Some(inst_ref),
        &mut [
            Some(&mut first[0]),
            Some(&mut second[0]),
            Some(&mut rest2[0]),
            None,
        ],
    );
    ra.asm.mov(rxbyak::RAX, func as i64).unwrap();
    ra.asm.call_reg(rxbyak::RAX).unwrap();
}

fn emit_fp_estimate_call(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    func: usize,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    ra.host_call(Some(inst_ref), &mut [Some(&mut args[0]), None, None, None]);

    let fpcr_param = abi::ABI_PARAMS[1].to_reg64();
    let fpsr_param = abi::ABI_PARAMS[2].to_reg64();
    ra.asm
        .mov(
            Reg::gpr32(fpcr_param.index()),
            ctx.fpcr(true).value() as i32,
        )
        .unwrap();
    ra.asm
        .lea(
            fpsr_param,
            rxbyak::dword_ptr(RegExp::from(R15) + ctx.jit_state_info.offsetof_fpsr_exc as i32),
        )
        .unwrap();
    ra.asm.mov(rxbyak::RAX, func as i64).unwrap();
    ra.asm.call_reg(rxbyak::RAX).unwrap();
}

fn emit_fp_step_fused_call(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    func: usize,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let (first, rest) = args.split_at_mut(1);
    ra.host_call(
        Some(inst_ref),
        &mut [Some(&mut first[0]), Some(&mut rest[0]), None, None],
    );

    let fpcr_param = abi::ABI_PARAMS[2].to_reg64();
    let fpsr_param = abi::ABI_PARAMS[3].to_reg64();
    ra.asm
        .mov(
            Reg::gpr32(fpcr_param.index()),
            ctx.fpcr(true).value() as i32,
        )
        .unwrap();
    ra.asm
        .lea(
            fpsr_param,
            rxbyak::dword_ptr(RegExp::from(R15) + ctx.jit_state_info.offsetof_fpsr_exc as i32),
        )
        .unwrap();
    ra.asm.mov(rxbyak::RAX, func as i64).unwrap();
    ra.asm.call_reg(rxbyak::RAX).unwrap();
}

fn emit_fp_convert_call(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    func: usize,
) {
    let rounding = inst.args[1].get_u8();
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    ra.host_call(Some(inst_ref), &mut [Some(&mut args[0]), None, None, None]);

    let fpcr_param = abi::ABI_PARAMS[1].to_reg64();
    let rounding_param = abi::ABI_PARAMS[2].to_reg64();
    let fpsr_param = abi::ABI_PARAMS[3].to_reg64();
    ra.asm
        .mov(
            Reg::gpr32(fpcr_param.index()),
            ctx.fpcr(true).value() as i32,
        )
        .unwrap();
    ra.asm
        .mov(Reg::gpr32(rounding_param.index()), rounding as i32)
        .unwrap();
    ra.asm
        .lea(
            fpsr_param,
            rxbyak::dword_ptr(RegExp::from(R15) + ctx.jit_state_info.offsetof_fpsr_exc as i32),
        )
        .unwrap();
    ra.asm.mov(rxbyak::RAX, func as i64).unwrap();
    ra.asm.call_reg(rxbyak::RAX).unwrap();
}

// ---------------------------------------------------------------------------
// Pack2x64To1x128: combine two 64-bit values into one 128-bit XMM
// ---------------------------------------------------------------------------

pub fn emit_pack_2x64_to_1x128(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());

    let lo = ra.use_scratch_xmm(&mut args[0]);
    let hi = ra.use_xmm(&mut args[1]);

    // punpcklqdq lo, hi → lo = [lo_low64, hi_low64]
    ra.asm.punpcklqdq(lo, hi).unwrap();

    ra.define_value(inst_ref, lo);
}

// ---------------------------------------------------------------------------
// FP scalar binary arithmetic (native SSE2)
// ---------------------------------------------------------------------------

fn emit_fp_binary_ss(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    op: fn(&mut rxbyak::CodeAssembler, Reg, Reg) -> rxbyak::Result<()>,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let op2 = ra.use_xmm(&mut args[1]);
    op(&mut *ra.asm, result, op2).unwrap();
    if ctx.fpcr(true).dn() {
        force_to_default_nan(ra, result, false);
    }
    ra.define_value(inst_ref, result);
}

fn emit_fp_binary_sd(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    op: fn(&mut rxbyak::CodeAssembler, Reg, Reg) -> rxbyak::Result<()>,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let op2 = ra.use_xmm(&mut args[1]);
    op(&mut *ra.asm, result, op2).unwrap();
    if ctx.fpcr(true).dn() {
        force_to_default_nan(ra, result, true);
    }
    ra.define_value(inst_ref, result);
}

fn force_to_default_nan(ra: &mut RegAlloc, result: Reg, is_double: bool) {
    let not_nan = ra.asm.create_label();
    if is_double {
        ra.asm.ucomisd(result, result).unwrap();
    } else {
        ra.asm.ucomiss(result, result).unwrap();
    }
    ra.asm.jnp(&not_nan, JmpType::Near).unwrap();

    let default_nan = ra.scratch_gpr();
    if is_double {
        ra.asm.mov(default_nan, 0x7ff8_0000_0000_0000i64).unwrap();
        ra.asm.movq(result, default_nan).unwrap();
    } else {
        ra.asm
            .mov(default_nan.cvt32().unwrap(), 0x7fc0_0000i32)
            .unwrap();
        ra.asm.movd(result, default_nan.cvt32().unwrap()).unwrap();
    }
    ra.release(default_nan);
    ra.asm.bind(&not_nan).unwrap();
}

pub fn emit_fp_add32(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_binary_ss(ctx, ra, inst_ref, inst, rxbyak::CodeAssembler::addss);
}
pub fn emit_fp_add64(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_binary_sd(ctx, ra, inst_ref, inst, rxbyak::CodeAssembler::addsd);
}
pub fn emit_fp_sub32(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_binary_ss(ctx, ra, inst_ref, inst, rxbyak::CodeAssembler::subss);
}
pub fn emit_fp_sub64(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_binary_sd(ctx, ra, inst_ref, inst, rxbyak::CodeAssembler::subsd);
}
pub fn emit_fp_mul32(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_binary_ss(ctx, ra, inst_ref, inst, rxbyak::CodeAssembler::mulss);
}
pub fn emit_fp_mul64(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_binary_sd(ctx, ra, inst_ref, inst, rxbyak::CodeAssembler::mulsd);
}
pub fn emit_fp_div32(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_binary_ss(ctx, ra, inst_ref, inst, rxbyak::CodeAssembler::divss);
}
pub fn emit_fp_div64(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_binary_sd(ctx, ra, inst_ref, inst, rxbyak::CodeAssembler::divsd);
}

pub(crate) fn fp_min_max<F: FloatFormat>(
    operand1: F,
    operand2: F,
    fpcr: Fpcr,
    fpsr: &mut Fpsr,
    is_max: bool,
    numeric: bool,
) -> F {
    // Upstream's DenormalsAreZero masks subnormal operands before FMIN/FMAX;
    // unlike FPUnpack, that path does not raise IDC.
    let input_denormal_cumulative = fpsr.value() & (1 << 7) != 0;
    let (type1, sign1, _) = fp_unpack(operand1, fpcr, fpsr);
    let (type2, sign2, _) = fp_unpack(operand2, fpcr, fpsr);
    fpsr.set_idc(input_denormal_cumulative);

    let is_nan = |fp_type| matches!(fp_type, FpType::QNaN | FpType::SNaN);
    let selected_nan = if type1 == FpType::SNaN {
        Some((type1, operand1))
    } else if type2 == FpType::SNaN {
        Some((type2, operand2))
    } else if !numeric && type1 == FpType::QNaN {
        Some((type1, operand1))
    } else if !numeric && type2 == FpType::QNaN {
        Some((type2, operand2))
    } else if numeric && type1 == FpType::QNaN && type2 == FpType::QNaN {
        Some((type1, operand1))
    } else {
        None
    };
    if let Some((fp_type, operand)) = selected_nan {
        return process_nan(fp_type, operand, fpcr, fpsr);
    }
    if numeric {
        if is_nan(type1) {
            return if type2 == FpType::Zero {
                F::zero(sign2)
            } else {
                operand2
            };
        }
        if is_nan(type2) {
            return if type1 == FpType::Zero {
                F::zero(sign1)
            } else {
                operand1
            };
        }
    }

    if type1 == FpType::Zero && type2 == FpType::Zero {
        return F::zero(if is_max {
            sign1 && sign2
        } else {
            sign1 || sign2
        });
    }

    let operand1 = if type1 == FpType::Zero {
        F::zero(sign1)
    } else {
        operand1
    };
    let operand2 = if type2 == FpType::Zero {
        F::zero(sign2)
    } else {
        operand2
    };
    let bits1 = operand1.to_bits();
    let bits2 = operand2.to_bits();
    let operand1_is_greater = if sign1 != sign2 {
        !sign1
    } else {
        let magnitude1 = bits1 & !F::SIGN_MASK;
        let magnitude2 = bits2 & !F::SIGN_MASK;
        if sign1 {
            magnitude1 < magnitude2
        } else {
            magnitude1 > magnitude2
        }
    };

    if operand1_is_greater == is_max {
        operand1
    } else {
        operand2
    }
}

macro_rules! define_fp_min_max_fallback {
    ($name:ident, $type:ty, $is_max:expr, $numeric:expr) => {
        extern "C" fn $name(
            operand1: $type,
            operand2: $type,
            fpcr: u32,
            fpsr_exc: *mut u32,
        ) -> $type {
            unsafe {
                let mut fpsr = Fpsr::new(fpsr_exc.read());
                let result = fp_min_max(
                    operand1,
                    operand2,
                    Fpcr::new(fpcr),
                    &mut fpsr,
                    $is_max,
                    $numeric,
                );
                fpsr_exc.write(fpsr.value());
                result
            }
        }
    };
}

define_fp_min_max_fallback!(fallback_fp_max32, u32, true, false);
define_fp_min_max_fallback!(fallback_fp_max64, u64, true, false);
define_fp_min_max_fallback!(fallback_fp_min32, u32, false, false);
define_fp_min_max_fallback!(fallback_fp_min64, u64, false, false);
define_fp_min_max_fallback!(fallback_fp_max_numeric32, u32, true, true);
define_fp_min_max_fallback!(fallback_fp_max_numeric64, u64, true, true);
define_fp_min_max_fallback!(fallback_fp_min_numeric32, u32, false, true);
define_fp_min_max_fallback!(fallback_fp_min_numeric64, u64, false, true);

fn emit_fp_min_max(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    fallback: usize,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let (first, rest) = args.split_at_mut(1);
    ra.host_call(
        Some(inst_ref),
        &mut [Some(&mut first[0]), Some(&mut rest[0]), None, None],
    );

    let fpcr_param = abi::ABI_PARAMS[2].to_reg64();
    let fpsr_param = abi::ABI_PARAMS[3].to_reg64();
    ra.asm
        .mov(
            Reg::gpr32(fpcr_param.index()),
            ctx.fpcr(true).value() as i32,
        )
        .unwrap();
    ra.asm
        .lea(
            fpsr_param,
            rxbyak::dword_ptr(RegExp::from(R15) + ctx.jit_state_info.offsetof_fpsr_exc as i32),
        )
        .unwrap();
    ra.asm.mov(rxbyak::RAX, fallback as i64).unwrap();
    ra.asm.call_reg(rxbyak::RAX).unwrap();
}

pub fn emit_fp_max32(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_min_max(
        ctx,
        ra,
        inst_ref,
        inst,
        fallback_fp_max32 as *const () as usize,
    );
}
pub fn emit_fp_max64(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_min_max(
        ctx,
        ra,
        inst_ref,
        inst,
        fallback_fp_max64 as *const () as usize,
    );
}
pub fn emit_fp_min32(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_min_max(
        ctx,
        ra,
        inst_ref,
        inst,
        fallback_fp_min32 as *const () as usize,
    );
}
pub fn emit_fp_min64(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_min_max(
        ctx,
        ra,
        inst_ref,
        inst,
        fallback_fp_min64 as *const () as usize,
    );
}
pub fn emit_fp_max_numeric32(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_min_max(
        ctx,
        ra,
        inst_ref,
        inst,
        fallback_fp_max_numeric32 as *const () as usize,
    );
}
pub fn emit_fp_max_numeric64(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_min_max(
        ctx,
        ra,
        inst_ref,
        inst,
        fallback_fp_max_numeric64 as *const () as usize,
    );
}
pub fn emit_fp_min_numeric32(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_min_max(
        ctx,
        ra,
        inst_ref,
        inst,
        fallback_fp_min_numeric32 as *const () as usize,
    );
}
pub fn emit_fp_min_numeric64(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_min_max(
        ctx,
        ra,
        inst_ref,
        inst,
        fallback_fp_min_numeric64 as *const () as usize,
    );
}

// ---------------------------------------------------------------------------
// FP scalar unary (native SSE2)
// ---------------------------------------------------------------------------

fn emit_fp_unary_ss(
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    op: fn(&mut rxbyak::CodeAssembler, Reg, Reg) -> rxbyak::Result<()>,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    op(&mut *ra.asm, result, result).unwrap();
    ra.define_value(inst_ref, result);
}

pub fn emit_fp_sqrt32(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_unary_ss(ra, inst_ref, inst, rxbyak::CodeAssembler::sqrtss);
}
pub fn emit_fp_sqrt64(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_unary_ss(ra, inst_ref, inst, rxbyak::CodeAssembler::sqrtsd);
}

// ---------------------------------------------------------------------------
// FPAbs: clear sign bit via ANDPS with mask
// ---------------------------------------------------------------------------

pub fn emit_fp_abs32(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let mask = ra.scratch_xmm();
    // Load 0x7FFFFFFF mask
    let temp_gpr = ra.scratch_gpr();
    ra.asm
        .mov(temp_gpr.cvt32().unwrap(), 0x7FFF_FFFFi32)
        .unwrap();
    ra.asm.movd(mask, temp_gpr.cvt32().unwrap()).unwrap();
    ra.asm.andps(result, mask).unwrap();
    ra.release(temp_gpr);
    ra.release(mask);
    ra.define_value(inst_ref, result);
}

pub fn emit_fp_abs64(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let mask = ra.scratch_xmm();
    let temp_gpr = ra.scratch_gpr();
    ra.asm.mov(temp_gpr, 0x7FFF_FFFF_FFFF_FFFFi64).unwrap();
    ra.asm.movq(mask, temp_gpr).unwrap();
    ra.asm.andps(result, mask).unwrap();
    ra.release(temp_gpr);
    ra.release(mask);
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// FPNeg: flip sign bit via XORPS with mask
// ---------------------------------------------------------------------------

pub fn emit_fp_neg32(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let mask = ra.scratch_xmm();
    let temp_gpr = ra.scratch_gpr();
    ra.asm
        .mov(temp_gpr.cvt32().unwrap(), -0x80000000i32)
        .unwrap(); // 0x80000000
    ra.asm.movd(mask, temp_gpr.cvt32().unwrap()).unwrap();
    ra.asm.xorps(result, mask).unwrap();
    ra.release(temp_gpr);
    ra.release(mask);
    ra.define_value(inst_ref, result);
}

pub fn emit_fp_neg64(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let mask = ra.scratch_xmm();
    let temp_gpr = ra.scratch_gpr();
    ra.asm.mov(temp_gpr, -0x8000_0000_0000_0000i64).unwrap();
    ra.asm.movq(mask, temp_gpr).unwrap();
    ra.asm.xorps(result, mask).unwrap();
    ra.release(temp_gpr);
    ra.release(mask);
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// FPCompare: ucomiss/ucomisd → NZCV extraction
// Args: (a: U32/U64, b: U32/U64, exc_on_qnan: U1) → NZCV
// ---------------------------------------------------------------------------

pub fn emit_fp_compare32(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_compare(ra, inst_ref, inst, false);
}

pub fn emit_fp_compare64(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_compare(ra, inst_ref, inst, true);
}

fn emit_fp_compare(ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst, is_double: bool) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let a = ra.use_xmm(&mut args[0]);
    let b = ra.use_xmm(&mut args[1]);
    let exc_on_qnan = args[2].is_immediate() && args[2].get_immediate_u1();

    if is_double {
        if exc_on_qnan {
            ra.asm.comisd(a, b).unwrap();
        } else {
            ra.asm.ucomisd(a, b).unwrap();
        }
    } else {
        if exc_on_qnan {
            ra.asm.comiss(a, b).unwrap();
        } else {
            ra.asm.ucomiss(a, b).unwrap();
        }
    }

    // Match upstream SetFpscrNzcvFromFlags(): produce packed x64 NZCV bits.
    // x64 ZF:CF index selects:
    // 0 -> 0x0100 (GT), 1 -> 0x8000 (LT), 2 -> 0x4100 (EQ), 3 -> 0x0101 (unordered)
    let _rcx = ra.scratch_gpr_at(HOST_RCX);
    let result = ra.scratch_gpr();
    ra.asm.mov(result, 0x0101_4100_8000_0100u64 as i64).unwrap();
    ra.asm.sete(CL).unwrap();
    ra.asm.rcl(CL, 5).unwrap();
    ra.asm.shr_cl(result).unwrap();
    ra.define_value(inst_ref, result);
}

// ---------------------------------------------------------------------------
// FP conversions (native SSE2)
// ---------------------------------------------------------------------------

pub fn emit_fp_single_to_double(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let rounding = scalar_rounding_mode(inst.args[1].get_u8());
    if rounding == ctx.fpcr(true).rmode() && rounding != RoundingMode::ToOdd {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let result = ra.use_scratch_xmm(&mut args[0]);
        ra.asm.cvtss2sd(result, result).unwrap();
        if ctx.fpcr(true).dn() {
            force_to_default_nan(ra, result, true);
        }
        ra.define_value(inst_ref, result);
        return;
    }

    emit_fp_convert_call(
        ctx,
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_single_to_double as *const () as usize,
    );
}

pub fn emit_fp_double_to_single(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let rounding = scalar_rounding_mode(inst.args[1].get_u8());
    if rounding == ctx.fpcr(true).rmode() && rounding != RoundingMode::ToOdd {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let result = ra.use_scratch_xmm(&mut args[0]);
        ra.asm.cvtsd2ss(result, result).unwrap();
        if ctx.fpcr(true).dn() {
            force_to_default_nan(ra, result, false);
        }
        ra.define_value(inst_ref, result);
        return;
    }

    emit_fp_convert_call(
        ctx,
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_double_to_single as *const () as usize,
    );
}

// ---------------------------------------------------------------------------
// FPRoundInt
// Args: (value, rounding_mode: U8, exact: U1)
// ---------------------------------------------------------------------------

fn scalar_rounding_mode(rounding: u8) -> RoundingMode {
    match rounding {
        0 => RoundingMode::ToNearestTieEven,
        1 => RoundingMode::TowardsPlusInfinity,
        2 => RoundingMode::TowardsMinusInfinity,
        3 => RoundingMode::TowardsZero,
        4 => RoundingMode::ToNearestTieAwayFromZero,
        _ => unreachable!("invalid FP rounding mode {rounding}"),
    }
}

macro_rules! define_scalar_round_fallback {
    ($name:ident, $type:ty) => {
        extern "C" fn $name<const ROUNDING: u8, const EXACT: bool>(
            input: u64,
            fpsr_exc: *mut u32,
            fpcr: u32,
        ) -> u64 {
            unsafe {
                let mut fpsr = Fpsr::new(fpsr_exc.read());
                let result = fp_round_int(
                    input as $type,
                    Fpcr::new(fpcr),
                    scalar_rounding_mode(ROUNDING),
                    EXACT,
                    &mut fpsr,
                );
                fpsr_exc.write(fpsr.value());
                result as u64
            }
        }
    };
}

define_scalar_round_fallback!(fallback_fp_round_int16, u16);
define_scalar_round_fallback!(fallback_fp_round_int32, u32);
define_scalar_round_fallback!(fallback_fp_round_int64, u64);

macro_rules! scalar_round_fallback {
    ($function:ident, $rounding:expr, $exact:expr) => {
        $function::<$rounding, $exact> as *const () as usize
    };
}

fn scalar_round_fallback_for(esize: usize, rounding: u8, exact: bool) -> usize {
    macro_rules! select_exact {
        ($function:ident, $rounding:expr) => {
            if exact {
                scalar_round_fallback!($function, $rounding, true)
            } else {
                scalar_round_fallback!($function, $rounding, false)
            }
        };
    }
    macro_rules! select_rounding {
        ($function:ident) => {
            match rounding {
                0 => select_exact!($function, 0),
                1 => select_exact!($function, 1),
                2 => select_exact!($function, 2),
                3 => select_exact!($function, 3),
                4 => select_exact!($function, 4),
                _ => unreachable!("invalid FP rounding mode {rounding}"),
            }
        };
    }
    match esize {
        16 => select_rounding!(fallback_fp_round_int16),
        32 => select_rounding!(fallback_fp_round_int32),
        64 => select_rounding!(fallback_fp_round_int64),
        _ => unreachable!("invalid FP element size {esize}"),
    }
}

fn emit_fp_round_int(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    esize: usize,
) {
    let rounding = inst.args[1].get_u8();
    let exact = inst.args[2].get_u1();

    if esize != 16 && ctx.has_host_feature(HostFeature::SSE41) && rounding != 4 && !exact {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let result = ra.use_scratch_xmm(&mut args[0]);
        let sse_rmode = match rounding {
            0 => 0x00u8,
            1 => 0x02u8,
            2 => 0x01u8,
            3 => 0x03u8,
            _ => unreachable!(),
        };
        if esize == 32 {
            ra.asm.roundss(result, result, sse_rmode).unwrap();
        } else {
            ra.asm.roundsd(result, result, sse_rmode).unwrap();
        }
        ra.define_value(inst_ref, result);
        return;
    }

    let fallback = scalar_round_fallback_for(esize, rounding, exact);
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    ra.host_call(Some(inst_ref), &mut [Some(&mut args[0]), None, None, None]);
    let fpsr_param = abi::ABI_PARAMS[1].to_reg64();
    let fpcr_param = abi::ABI_PARAMS[2].to_reg64();
    ra.asm
        .lea(
            fpsr_param,
            rxbyak::dword_ptr(RegExp::from(R15) + ctx.jit_state_info.offsetof_fpsr_exc as i32),
        )
        .unwrap();
    ra.asm
        .mov(
            Reg::gpr32(fpcr_param.index()),
            ctx.fpcr(true).value() as i32,
        )
        .unwrap();
    ra.asm.mov(rxbyak::RAX, fallback as i64).unwrap();
    ra.asm.call_reg(rxbyak::RAX).unwrap();
}

pub fn emit_fp_round_int32(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_round_int(ctx, ra, inst_ref, inst, 32);
}

pub fn emit_fp_round_int64(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_round_int(ctx, ra, inst_ref, inst, 64);
}

// ---------------------------------------------------------------------------
// FPMulAdd/FPMulSub: FMA3 vfmadd231ss/sd, vfmsub/vfnmadd
// Args: (addend, a, b) → addend + a*b / addend - a*b
// ---------------------------------------------------------------------------

pub fn emit_fp_mul_add32(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_mul_add(ctx, ra, inst_ref, inst, 32, false);
}

pub fn emit_fp_mul_add64(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_mul_add(ctx, ra, inst_ref, inst, 64, false);
}

pub fn emit_fp_mul_sub32(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_mul_add(ctx, ra, inst_ref, inst, 32, true);
}

pub fn emit_fp_mul_sub64(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_mul_add(ctx, ra, inst_ref, inst, 64, true);
}

fn emit_fp_mul_add(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    fsize: usize,
    negate_product: bool,
) {
    let fpcr = ctx.fpcr(true);

    // Upstream's correction-free FMA path.
    if ctx.has_host_feature(HostFeature::FMA) && !fpcr.fz() && fpcr.dn() {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let result = ra.use_scratch_xmm(&mut args[0]);
        let operand2 = ra.use_xmm(&mut args[1]);
        let operand3 = ra.use_xmm(&mut args[2]);
        match (fsize, negate_product) {
            (32, false) => ra.asm.vfmadd231ss(result, operand2, operand3).unwrap(),
            (32, true) => ra.asm.vfnmadd231ss(result, operand2, operand3).unwrap(),
            (64, false) => ra.asm.vfmadd231sd(result, operand2, operand3).unwrap(),
            (64, true) => ra.asm.vfnmadd231sd(result, operand2, operand3).unwrap(),
            _ => unreachable!(),
        }
        force_to_default_nan(ra, result, fsize == 64);
        ra.define_value(inst_ref, result);
        return;
    }

    // With DN disabled the native result is exact for the overwhelmingly
    // common non-NaN case. ARM and x64 disagree only on NaN priority here, so
    // branch to the common/fp implementation when the result is unordered.
    // This is the same deferred slow-path boundary as upstream.
    if ctx.has_host_feature(HostFeature::FMA | HostFeature::AVX) {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let operand1 = ra.use_xmm(&mut args[0]);
        let operand2 = ra.use_xmm(&mut args[1]);
        let operand3 = ra.use_xmm(&mut args[2]);
        let result = ra.scratch_xmm();
        let fallback = ra.asm.create_label();
        let end = ra.asm.create_label();

        ra.asm.movaps(result, operand1).unwrap();
        match (fsize, negate_product) {
            (32, false) => ra.asm.vfmadd231ss(result, operand2, operand3).unwrap(),
            (32, true) => ra.asm.vfnmadd231ss(result, operand2, operand3).unwrap(),
            (64, false) => ra.asm.vfmadd231sd(result, operand2, operand3).unwrap(),
            (64, true) => ra.asm.vfnmadd231sd(result, operand2, operand3).unwrap(),
            _ => unreachable!(),
        }
        if fpcr.fz() {
            let magnitude = ra.scratch_xmm();
            ra.asm.movaps(magnitude, result).unwrap();
            let (non_sign_mask, smallest_normal) = if fsize == 32 {
                (0x0000_0000_7fff_ffff, 0x0000_0000_0080_0000)
            } else {
                (0x7fff_ffff_ffff_ffff, 0x0010_0000_0000_0000)
            };
            let non_sign = ra
                .constant_pool
                .as_mut()
                .expect("constant pool required")
                .get_constant(non_sign_mask, 0);
            let smallest = ra
                .constant_pool
                .as_mut()
                .expect("constant pool required")
                .get_constant(smallest_normal, 0);
            ra.asm
                .andps(magnitude, rxbyak::xmmword_ptr(non_sign))
                .unwrap();
            if fsize == 32 {
                ra.asm
                    .ucomiss(magnitude, rxbyak::xmmword_ptr(smallest))
                    .unwrap();
            } else {
                ra.asm
                    .ucomisd(magnitude, rxbyak::xmmword_ptr(smallest))
                    .unwrap();
            }
            ra.release(magnitude);
            // UCOMIS sets ZF both for equality and unordered. The latter also
            // selects the software path when DN is disabled.
            ra.asm.jz(&fallback, JmpType::Near).unwrap();
        } else if fsize == 32 {
            ra.asm.ucomiss(result, result).unwrap();
            ra.asm.jp(&fallback, JmpType::Near).unwrap();
        } else {
            ra.asm.ucomisd(result, result).unwrap();
            ra.asm.jp(&fallback, JmpType::Near).unwrap();
        }
        ra.asm.bind(&end).unwrap();

        let fpcr_value = fpcr.value();
        let fpsr_offset = ctx.jit_state_info.offsetof_fpsr_exc as i32;
        let function = match (fsize, negate_product) {
            (32, false) => fp_helpers::fp_mul_add32 as *const () as usize,
            (32, true) => fp_helpers::fp_mul_sub32 as *const () as usize,
            (64, false) => fp_helpers::fp_mul_add64 as *const () as usize,
            (64, true) => fp_helpers::fp_mul_sub64 as *const () as usize,
            _ => unreachable!(),
        };
        ctx.deferred_emits
            .borrow_mut()
            .push(Box::new(move |dctx: &mut DeferredEmitCtx<'_>| {
                dctx.asm.bind(&fallback).unwrap();
                dctx.asm.sub(RSP, 8).unwrap();
                let frame = abi::push_caller_save_registers_and_adjust_stack_except(
                    dctx.asm,
                    Some(HostLoc::Xmm(result.index())),
                )
                .unwrap();
                dctx.asm
                    .movq(abi::ABI_PARAMS[0].to_reg64(), operand1)
                    .unwrap();
                dctx.asm
                    .movq(abi::ABI_PARAMS[1].to_reg64(), operand2)
                    .unwrap();
                dctx.asm
                    .movq(abi::ABI_PARAMS[2].to_reg64(), operand3)
                    .unwrap();
                dctx.asm
                    .mov(
                        abi::ABI_PARAMS[3].to_reg64().cvt32().unwrap(),
                        fpcr_value as i32,
                    )
                    .unwrap();

                #[cfg(target_os = "windows")]
                {
                    let extra = abi::ABI_SHADOW_SPACE + 16;
                    dctx.asm.sub(RSP, extra as i32).unwrap();
                    dctx.asm
                        .lea(
                            rxbyak::RAX,
                            rxbyak::dword_ptr(RegExp::from(R15) + fpsr_offset),
                        )
                        .unwrap();
                    dctx.asm
                        .mov(
                            rxbyak::qword_ptr(RegExp::from(RSP) + abi::ABI_SHADOW_SPACE as i32),
                            rxbyak::RAX,
                        )
                        .unwrap();
                    dctx.asm.mov(rxbyak::RAX, function as i64).unwrap();
                    dctx.asm.call_reg(rxbyak::RAX).unwrap();
                    dctx.asm.add(RSP, extra as i32).unwrap();
                }

                #[cfg(not(target_os = "windows"))]
                {
                    dctx.asm
                        .lea(
                            abi::ABI_PARAMS[4].to_reg64(),
                            rxbyak::dword_ptr(RegExp::from(R15) + fpsr_offset),
                        )
                        .unwrap();
                    dctx.asm.mov(rxbyak::RAX, function as i64).unwrap();
                    dctx.asm.call_reg(rxbyak::RAX).unwrap();
                }

                dctx.asm.movq(result, rxbyak::RAX).unwrap();
                abi::pop_caller_save_registers_and_adjust_stack(dctx.asm, &frame).unwrap();
                dctx.asm.add(RSP, 8).unwrap();
                dctx.asm.jmp(&end, JmpType::Near).unwrap();
            }));

        ra.define_value(inst_ref, result);
        return;
    }

    if ctx.has_optimization(OptimizationFlag::UNSAFE_UNFUSE_FMA) {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let result = ra.use_scratch_xmm(&mut args[0]);
        let product = ra.use_scratch_xmm(&mut args[1]);
        let operand3 = ra.use_xmm(&mut args[2]);

        if negate_product {
            let sign_mask = if fsize == 32 {
                (0x8000_0000, 0)
            } else {
                (0x8000_0000_0000_0000, 0)
            };
            let mask = ra
                .constant_pool
                .as_mut()
                .expect("constant pool required")
                .get_constant(sign_mask.0, sign_mask.1);
            ra.asm.xorps(product, rxbyak::xmmword_ptr(mask)).unwrap();
        }
        if fsize == 32 {
            ra.asm.mulss(product, operand3).unwrap();
            ra.asm.addss(result, product).unwrap();
        } else {
            ra.asm.mulsd(product, operand3).unwrap();
            ra.asm.addsd(result, product).unwrap();
        }
        ra.release(product);
        ra.define_value(inst_ref, result);
        return;
    }

    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let (first, rest) = args.split_at_mut(1);
    let (second, third) = rest.split_at_mut(1);
    ra.host_call(
        Some(inst_ref),
        &mut [
            Some(&mut first[0]),
            Some(&mut second[0]),
            Some(&mut third[0]),
            None,
        ],
    );
    ra.asm
        .mov(
            Reg::gpr32(abi::ABI_PARAMS[3].to_reg64().index()),
            fpcr.value() as i32,
        )
        .unwrap();

    #[cfg(target_os = "windows")]
    {
        let frame_size = abi::ABI_SHADOW_SPACE + 16;
        ra.alloc_stack_space(frame_size);
        ra.asm
            .lea(
                rxbyak::RAX,
                rxbyak::dword_ptr(RegExp::from(R15) + ctx.jit_state_info.offsetof_fpsr_exc as i32),
            )
            .unwrap();
        ra.asm
            .mov(
                rxbyak::qword_ptr(RegExp::from(rxbyak::RSP) + abi::ABI_SHADOW_SPACE as i32),
                rxbyak::RAX,
            )
            .unwrap();
        let function = match (fsize, negate_product) {
            (32, false) => fp_helpers::fp_mul_add32 as *const () as usize,
            (32, true) => fp_helpers::fp_mul_sub32 as *const () as usize,
            (64, false) => fp_helpers::fp_mul_add64 as *const () as usize,
            (64, true) => fp_helpers::fp_mul_sub64 as *const () as usize,
            _ => unreachable!(),
        };
        ra.asm.mov(rxbyak::RAX, function as i64).unwrap();
        ra.asm.call_reg(rxbyak::RAX).unwrap();
        ra.release_stack_space(frame_size);
    }

    #[cfg(not(target_os = "windows"))]
    {
        ra.asm
            .lea(
                abi::ABI_PARAMS[4].to_reg64(),
                rxbyak::dword_ptr(RegExp::from(R15) + ctx.jit_state_info.offsetof_fpsr_exc as i32),
            )
            .unwrap();
        let function = match (fsize, negate_product) {
            (32, false) => fp_helpers::fp_mul_add32 as usize,
            (32, true) => fp_helpers::fp_mul_sub32 as usize,
            (64, false) => fp_helpers::fp_mul_add64 as usize,
            (64, true) => fp_helpers::fp_mul_sub64 as usize,
            _ => unreachable!(),
        };
        ra.asm.mov(rxbyak::RAX, function as i64).unwrap();
        ra.asm.call_reg(rxbyak::RAX).unwrap();
    }
}

// ---------------------------------------------------------------------------
// FP fixed-point conversions (native SSE2)
// Args: (value, fbits: U8, rounding: U8)
// ---------------------------------------------------------------------------

pub fn emit_fp_fixed_s32_to_single(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let src = ra.use_gpr(&mut args[0]);
    let fbits = args[1].get_immediate_u8();
    let result = ra.scratch_xmm();
    ra.asm.cvtsi2ss(result, src.cvt32().unwrap()).unwrap();
    if fbits > 0 {
        // Divide by 2^fbits
        let scale = ra.scratch_xmm();
        let temp = ra.scratch_gpr();
        let divisor = (1u64 << fbits) as f32;
        ra.asm
            .mov(temp.cvt32().unwrap(), divisor.to_bits() as i32)
            .unwrap();
        ra.asm.movd(scale, temp.cvt32().unwrap()).unwrap();
        ra.asm.divss(result, scale).unwrap();
        ra.release(scale);
        ra.release(temp);
    }
    ra.define_value(inst_ref, result);
}

pub fn emit_fp_fixed_s32_to_double(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let src = ra.use_gpr(&mut args[0]);
    let fbits = args[1].get_immediate_u8();
    let result = ra.scratch_xmm();
    ra.asm.cvtsi2sd(result, src.cvt32().unwrap()).unwrap();
    if fbits > 0 {
        let scale = ra.scratch_xmm();
        let temp = ra.scratch_gpr();
        let divisor = (1u64 << fbits) as f64;
        ra.asm.mov(temp, divisor.to_bits() as i64).unwrap();
        ra.asm.movq(scale, temp).unwrap();
        ra.asm.divsd(result, scale).unwrap();
        ra.release(scale);
        ra.release(temp);
    }
    ra.define_value(inst_ref, result);
}

pub fn emit_fp_fixed_u32_to_single(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let src = ra.use_scratch_gpr(&mut args[0]);
    let fbits = args[1].get_immediate_u8();
    // Zero-extend to 64-bit to handle full u32 range
    ra.asm
        .mov(src.cvt32().unwrap(), src.cvt32().unwrap())
        .unwrap(); // zero-extend 32→64
    let result = ra.scratch_xmm();
    ra.asm.cvtsi2ss(result, src).unwrap(); // 64-bit signed → covers full u32
    if fbits > 0 {
        let scale = ra.scratch_xmm();
        let temp = ra.scratch_gpr();
        let divisor = (1u64 << fbits) as f32;
        ra.asm
            .mov(temp.cvt32().unwrap(), divisor.to_bits() as i32)
            .unwrap();
        ra.asm.movd(scale, temp.cvt32().unwrap()).unwrap();
        ra.asm.divss(result, scale).unwrap();
        ra.release(scale);
        ra.release(temp);
    }
    ra.define_value(inst_ref, result);
}

pub fn emit_fp_fixed_u32_to_double(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let src = ra.use_scratch_gpr(&mut args[0]);
    let fbits = args[1].get_immediate_u8();
    ra.asm
        .mov(src.cvt32().unwrap(), src.cvt32().unwrap())
        .unwrap();
    let result = ra.scratch_xmm();
    ra.asm.cvtsi2sd(result, src).unwrap();
    if fbits > 0 {
        let scale = ra.scratch_xmm();
        let temp = ra.scratch_gpr();
        let divisor = (1u64 << fbits) as f64;
        ra.asm.mov(temp, divisor.to_bits() as i64).unwrap();
        ra.asm.movq(scale, temp).unwrap();
        ra.asm.divsd(result, scale).unwrap();
        ra.release(scale);
        ra.release(temp);
    }
    ra.define_value(inst_ref, result);
}

pub fn emit_fp_fixed_s64_to_single(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let src = ra.use_gpr(&mut args[0]);
    let fbits = args[1].get_immediate_u8();
    let result = ra.scratch_xmm();
    ra.asm.cvtsi2ss(result, src).unwrap();
    if fbits > 0 {
        let scale = ra.scratch_xmm();
        let temp = ra.scratch_gpr();
        let divisor = (1u64 << fbits) as f32;
        ra.asm
            .mov(temp.cvt32().unwrap(), divisor.to_bits() as i32)
            .unwrap();
        ra.asm.movd(scale, temp.cvt32().unwrap()).unwrap();
        ra.asm.divss(result, scale).unwrap();
        ra.release(scale);
        ra.release(temp);
    }
    ra.define_value(inst_ref, result);
}

pub fn emit_fp_fixed_s64_to_double(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let src = ra.use_gpr(&mut args[0]);
    let fbits = args[1].get_immediate_u8();
    let result = ra.scratch_xmm();
    ra.asm.cvtsi2sd(result, src).unwrap();
    if fbits > 0 {
        let scale = ra.scratch_xmm();
        let temp = ra.scratch_gpr();
        let divisor = (1u64 << fbits) as f64;
        ra.asm.mov(temp, divisor.to_bits() as i64).unwrap();
        ra.asm.movq(scale, temp).unwrap();
        ra.asm.divsd(result, scale).unwrap();
        ra.release(scale);
        ra.release(temp);
    }
    ra.define_value(inst_ref, result);
}

pub fn emit_fp_fixed_u64_to_single(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    // u64→f32: host_call for simplicity (u64 may exceed i64 range)
    emit_host_call_2(
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_fixed_u64_to_single as *const () as usize,
    );
}

pub fn emit_fp_fixed_u64_to_double(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_host_call_2(
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_fixed_u64_to_double as *const () as usize,
    );
}

// FP to fixed-point
fn emit_fp_to_fixed_s32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    is_double: bool,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let fbits = args[1].get_immediate_u8();
    let rounding = args[2].get_immediate_u8();

    if rounding == 4 || (rounding != 3 && !ctx.has_host_feature(HostFeature::SSE41)) {
        let helper = if is_double {
            fp_helpers::fp_double_to_fixed_s32 as *const () as usize
        } else {
            fp_helpers::fp_single_to_fixed_s32 as *const () as usize
        };
        ra.host_call(Some(inst_ref), &mut [Some(&mut args[0]), None, None, None]);
        let parameters = fbits as u64 | ((rounding as u64) << 8);
        ra.asm
            .mov(abi::ABI_PARAMS[1].to_reg64(), parameters as i64)
            .unwrap();
        ra.asm
            .mov(
                Reg::gpr32(abi::ABI_PARAMS[2].to_reg64().index()),
                ctx.fpcr(true).value() as i32,
            )
            .unwrap();
        ra.asm
            .lea(
                abi::ABI_PARAMS[3].to_reg64(),
                rxbyak::dword_ptr(RegExp::from(R15) + ctx.jit_state_info.offsetof_fpsr_exc as i32),
            )
            .unwrap();
        ra.asm.mov(rxbyak::RAX, helper as i64).unwrap();
        ra.asm.call_reg(rxbyak::RAX).unwrap();
        return;
    }

    let src = ra.use_scratch_xmm(&mut args[0]);
    if fbits != 0 {
        let scale = ra.scratch_xmm();
        let temp = ra.scratch_gpr();
        if is_double {
            let scale_bits = ((fbits as u64 + 1023) << 52) as i64;
            ra.asm.mov(temp, scale_bits).unwrap();
            ra.asm.movq(scale, temp).unwrap();
            ra.asm.mulsd(src, scale).unwrap();
        } else {
            let scale_bits = ((fbits as u32 + 127) << 23) as i32;
            ra.asm.mov(temp.cvt32().unwrap(), scale_bits).unwrap();
            ra.asm.movd(scale, temp.cvt32().unwrap()).unwrap();
            ra.asm.mulss(src, scale).unwrap();
        }
        ra.release(scale);
        ra.release(temp);
    }

    if rounding != 3 {
        let sse_rounding = match rounding {
            0 => 0x00,
            1 => 0x02,
            2 => 0x01,
            _ => unreachable!(),
        };
        if is_double {
            ra.asm.roundsd(src, src, sse_rounding).unwrap();
        } else {
            ra.asm.roundss(src, src, sse_rounding).unwrap();
        }
    }

    if !is_double {
        ra.asm.cvtss2sd(src, src).unwrap();
    }

    // Upstream ZeroIfNaN<64> makes all NaNs convert to zero before applying
    // the signed upper clamp. Values below INT_MIN can use cvttsd2si's
    // architectural 0x80000000 result directly.
    let not_nan = ra.asm.create_label();
    ra.asm.ucomisd(src, src).unwrap();
    ra.asm.jnp(&not_nan, JmpType::Near).unwrap();
    ra.asm.pxor(src, src).unwrap();
    ra.asm.bind(&not_nan).unwrap();

    let max = ra.scratch_xmm();
    let temp = ra.scratch_gpr();
    ra.asm
        .mov(temp, (i32::MAX as f64).to_bits() as i64)
        .unwrap();
    ra.asm.movq(max, temp).unwrap();
    ra.asm.minsd(src, max).unwrap();

    let result = ra.scratch_gpr();
    ra.asm.cvttsd2si(result.cvt32().unwrap(), src).unwrap();
    ra.release(max);
    ra.release(temp);
    ra.define_value(inst_ref, result);
}

pub fn emit_fp_single_to_fixed_s32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_to_fixed_s32(ctx, ra, inst_ref, inst, false);
}

fn emit_fp_to_fixed_s64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    is_double: bool,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let fbits = args[1].get_immediate_u8();
    let rounding = args[2].get_immediate_u8();

    if rounding == 4 || (rounding != 3 && !ctx.has_host_feature(HostFeature::SSE41)) {
        let helper = if is_double {
            fp_helpers::fp_double_to_fixed_s64 as *const () as usize
        } else {
            fp_helpers::fp_single_to_fixed_s64 as *const () as usize
        };
        ra.host_call(Some(inst_ref), &mut [Some(&mut args[0]), None, None, None]);
        let parameters = fbits as u64 | ((rounding as u64) << 8);
        ra.asm
            .mov(abi::ABI_PARAMS[1].to_reg64(), parameters as i64)
            .unwrap();
        ra.asm
            .mov(
                Reg::gpr32(abi::ABI_PARAMS[2].to_reg64().index()),
                ctx.fpcr(true).value() as i32,
            )
            .unwrap();
        ra.asm
            .lea(
                abi::ABI_PARAMS[3].to_reg64(),
                rxbyak::dword_ptr(RegExp::from(R15) + ctx.jit_state_info.offsetof_fpsr_exc as i32),
            )
            .unwrap();
        ra.asm.mov(rxbyak::RAX, helper as i64).unwrap();
        ra.asm.call_reg(rxbyak::RAX).unwrap();
        return;
    }

    let src = ra.use_scratch_xmm(&mut args[0]);
    if fbits != 0 {
        let scale = ra.scratch_xmm();
        let temp = ra.scratch_gpr();
        if is_double {
            let scale_bits = ((fbits as u64 + 1023) << 52) as i64;
            ra.asm.mov(temp, scale_bits).unwrap();
            ra.asm.movq(scale, temp).unwrap();
            ra.asm.mulsd(src, scale).unwrap();
        } else {
            let scale_bits = ((fbits as u32 + 127) << 23) as i32;
            ra.asm.mov(temp.cvt32().unwrap(), scale_bits).unwrap();
            ra.asm.movd(scale, temp.cvt32().unwrap()).unwrap();
            ra.asm.mulss(src, scale).unwrap();
        }
        ra.release(scale);
        ra.release(temp);
    }

    if rounding != 3 {
        let sse_rounding = match rounding {
            0 => 0x00,
            1 => 0x02,
            2 => 0x01,
            _ => unreachable!(),
        };
        if is_double {
            ra.asm.roundsd(src, src, sse_rounding).unwrap();
        } else {
            ra.asm.roundss(src, src, sse_rounding).unwrap();
        }
    }

    if !is_double {
        ra.asm.cvtss2sd(src, src).unwrap();
    }

    let not_nan = ra.asm.create_label();
    ra.asm.ucomisd(src, src).unwrap();
    ra.asm.jnp(&not_nan, JmpType::Near).unwrap();
    ra.asm.pxor(src, src).unwrap();
    ra.asm.bind(&not_nan).unwrap();

    let limit = ra.scratch_xmm();
    let temp = ra.scratch_gpr();
    ra.asm.mov(temp, 0x43E0_0000_0000_0000i64).unwrap();
    ra.asm.movq(limit, temp).unwrap();
    ra.asm.comisd(limit, src).unwrap();

    let saturate_max = ra.asm.create_label();
    let end = ra.asm.create_label();
    ra.asm.jbe(&saturate_max, JmpType::Near).unwrap();

    let result = ra.scratch_gpr();
    ra.asm.cvttsd2si(result, src).unwrap();
    ra.asm.jmp(&end, JmpType::Near).unwrap();

    ra.asm.bind(&saturate_max).unwrap();
    ra.asm.mov(result, i64::MAX).unwrap();
    ra.asm.bind(&end).unwrap();

    ra.release(limit);
    ra.release(temp);
    ra.define_value(inst_ref, result);
}

pub fn emit_fp_single_to_fixed_s64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_to_fixed_s64(ctx, ra, inst_ref, inst, false);
}

pub fn emit_fp_double_to_fixed_s32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_to_fixed_s32(ctx, ra, inst_ref, inst, true);
}

pub fn emit_fp_double_to_fixed_s64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_to_fixed_s64(ctx, ra, inst_ref, inst, true);
}

fn emit_fp_to_fixed_unsigned(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    is_double: bool,
    integer_size: usize,
    helper: usize,
) {
    let fbits = inst.args[1].get_u8();
    let rounding = inst.args[2].get_u8();
    let truncating = rounding == 3;
    let native_rounding = rounding <= 3 && (truncating || ctx.has_host_feature(HostFeature::SSE41));

    if native_rounding {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let src = ra.use_scratch_xmm(&mut args[0]);

        if fbits != 0 {
            let scale = ra.scratch_xmm();
            let temp = ra.scratch_gpr();
            if is_double {
                let scale_bits = ((u64::from(fbits) + 1023) << 52) as i64;
                ra.asm.mov(temp, scale_bits).unwrap();
                ra.asm.movq(scale, temp).unwrap();
                ra.asm.mulsd(src, scale).unwrap();
            } else {
                let scale_bits = ((u32::from(fbits) + 127) << 23) as i32;
                ra.asm.mov(temp.cvt32().unwrap(), scale_bits).unwrap();
                ra.asm.movd(scale, temp.cvt32().unwrap()).unwrap();
                ra.asm.mulss(src, scale).unwrap();
            }
            ra.release(scale);
            ra.release(temp);
        }

        if !truncating {
            let sse_rounding = match rounding {
                0 => 0x00,
                1 => 0x02,
                2 => 0x01,
                _ => unreachable!(),
            };
            if is_double {
                ra.asm.roundsd(src, src, sse_rounding).unwrap();
            } else {
                ra.asm.roundss(src, src, sse_rounding).unwrap();
            }
        }

        if !is_double {
            ra.asm.cvtss2sd(src, src).unwrap();
        }

        let zero = ra.scratch_xmm();
        ra.asm.pxor(zero, zero).unwrap();
        let result = ra.scratch_gpr();

        if integer_size == 64 {
            let upper_half = ra.scratch_xmm();
            let limit = ra.scratch_xmm();
            let temp = ra.scratch_gpr();
            let result_upper = ra.scratch_gpr();

            ra.asm.movaps(upper_half, src).unwrap();
            ra.asm.mov(temp, 0x43e0_0000_0000_0000i64).unwrap();
            ra.asm.movq(limit, temp).unwrap();
            ra.asm.subsd(upper_half, limit).unwrap();

            // MAXSD selects its second operand for NaN, so both values become
            // zero before conversion exactly like upstream's xmm0 sequence.
            ra.asm.maxsd(src, zero).unwrap();
            ra.asm.maxsd(upper_half, zero).unwrap();
            ra.asm.cvttsd2si(result, src).unwrap();
            ra.asm.cvttsd2si(result_upper, upper_half).unwrap();
            ra.asm.or_(result, result_upper).unwrap();
            ra.asm.sar(result_upper, 63).unwrap();
            ra.asm.or_(result, result_upper).unwrap();

            ra.release(upper_half);
            ra.release(limit);
            ra.release(temp);
            ra.release(result_upper);
        } else {
            debug_assert_eq!(integer_size, 32);
            let maximum = ra.scratch_xmm();
            let temp = ra.scratch_gpr();

            ra.asm.maxsd(src, zero).unwrap();
            ra.asm
                .mov(temp, 4_294_967_295.0f64.to_bits() as i64)
                .unwrap();
            ra.asm.movq(maximum, temp).unwrap();
            ra.asm.minsd(src, maximum).unwrap();
            ra.asm.cvttsd2si(result, src).unwrap();

            ra.release(maximum);
            ra.release(temp);
        }

        ra.release(zero);
        ra.define_value(inst_ref, result);
        return;
    }

    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    ra.host_call(Some(inst_ref), &mut [Some(&mut args[0]), None, None, None]);

    let parameters = fbits as u64 | ((rounding as u64) << 8);
    ra.asm
        .mov(abi::ABI_PARAMS[1].to_reg64(), parameters as i64)
        .unwrap();
    ra.asm
        .mov(
            Reg::gpr32(abi::ABI_PARAMS[2].to_reg64().index()),
            ctx.fpcr(true).value() as i32,
        )
        .unwrap();
    ra.asm
        .lea(
            abi::ABI_PARAMS[3].to_reg64(),
            rxbyak::dword_ptr(RegExp::from(R15) + ctx.jit_state_info.offsetof_fpsr_exc as i32),
        )
        .unwrap();
    ra.asm.mov(rxbyak::RAX, helper as i64).unwrap();
    ra.asm.call_reg(rxbyak::RAX).unwrap();
}

pub fn emit_fp_single_to_fixed_u32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_to_fixed_unsigned(
        ctx,
        ra,
        inst_ref,
        inst,
        false,
        32,
        fp_helpers::fp_single_to_fixed_u32 as *const () as usize,
    );
}
pub fn emit_fp_single_to_fixed_u64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_to_fixed_unsigned(
        ctx,
        ra,
        inst_ref,
        inst,
        false,
        64,
        fp_helpers::fp_single_to_fixed_u64 as *const () as usize,
    );
}
pub fn emit_fp_double_to_fixed_u32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_to_fixed_unsigned(
        ctx,
        ra,
        inst_ref,
        inst,
        true,
        32,
        fp_helpers::fp_double_to_fixed_u32 as *const () as usize,
    );
}
pub fn emit_fp_double_to_fixed_u64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_to_fixed_unsigned(
        ctx,
        ra,
        inst_ref,
        inst,
        true,
        64,
        fp_helpers::fp_double_to_fixed_u64 as *const () as usize,
    );
}

// Half-precision and 16-bit fixed-point — all host_call fallback
pub fn emit_fp_abs16(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_host_call_1(
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_abs16 as *const () as usize,
    );
}
pub fn emit_fp_neg16(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_host_call_1(
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_neg16 as *const () as usize,
    );
}
pub fn emit_fp_round_int16(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_round_int(ctx, ra, inst_ref, inst, 16);
}
pub fn emit_fp_half_to_single(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    if ctx.has_host_feature(HostFeature::F16C) && !ctx.fpcr(true).ahp() && !ctx.fpcr(true).fz16() {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let result = ra.scratch_xmm();
        let value = ra.use_xmm(&mut args[0]);

        ra.asm.vcvtph2ps(result, value).unwrap();
        if ctx.fpcr(true).dn() {
            force_to_default_nan(ra, result, false);
        }

        ra.define_value(inst_ref, result);
        return;
    }

    emit_fp_convert_call(
        ctx,
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_half_to_single as *const () as usize,
    );
}
pub fn emit_fp_half_to_double(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    if ctx.has_host_feature(HostFeature::F16C) && !ctx.fpcr(true).ahp() && !ctx.fpcr(true).fz16() {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let result = ra.scratch_xmm();
        let value = ra.use_xmm(&mut args[0]);

        // Expanding through single precision is exact for every half value.
        ra.asm.vcvtph2ps(result, value).unwrap();
        ra.asm.vcvtps2pd(result, result).unwrap();
        if ctx.fpcr(true).dn() {
            force_to_default_nan(ra, result, true);
        }

        ra.define_value(inst_ref, result);
        return;
    }

    emit_fp_convert_call(
        ctx,
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_half_to_double as *const () as usize,
    );
}
pub fn emit_fp_single_to_half(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let rounding = inst.args[1].get_u8();
    if ctx.has_host_feature(HostFeature::F16C) && !ctx.fpcr(true).ahp() && !ctx.fpcr(true).fz16() {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let result = ra.use_scratch_xmm(&mut args[0]);

        if ctx.fpcr(true).dn() {
            let not_nan = ra.asm.create_label();
            ra.asm.ucomiss(result, result).unwrap();
            ra.asm.jnp(&not_nan, JmpType::Near).unwrap();
            let default_nan = ra.scratch_gpr();
            ra.asm
                .mov(default_nan.cvt32().unwrap(), 0x7fc0_0000i32)
                .unwrap();
            ra.asm.movd(result, default_nan.cvt32().unwrap()).unwrap();
            ra.release(default_nan);
            ra.asm.bind(&not_nan).unwrap();
        }

        let round_imm = convert_rounding_mode_to_x64_immediate(scalar_rounding_mode(rounding))
            .expect("hardware FP conversion rounding mode") as u8;
        ra.asm.vcvtps2ph(result, result, round_imm).unwrap();
        ra.define_value(inst_ref, result);
        return;
    }

    emit_fp_convert_call(
        ctx,
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_single_to_half as *const () as usize,
    );
}
pub fn emit_fp_double_to_half(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_convert_call(
        ctx,
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_double_to_half as *const () as usize,
    );
}

// FP multiply extended
pub fn emit_fp_mul_x32(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_host_call_2(
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_mul_x32 as *const () as usize,
    );
}
pub fn emit_fp_mul_x64(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_host_call_2(
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_mul_x64 as *const () as usize,
    );
}

// Reciprocal/sqrt estimates
pub fn emit_fp_recip_estimate16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_estimate_call(
        ctx,
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_recip_estimate16 as *const () as usize,
    );
}
pub fn emit_fp_recip_estimate32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_estimate_call(
        ctx,
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_recip_estimate32 as *const () as usize,
    );
}
pub fn emit_fp_recip_estimate64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_estimate_call(
        ctx,
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_recip_estimate64 as *const () as usize,
    );
}
pub fn emit_fp_recip_exponent16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_estimate_call(
        ctx,
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_recip_exponent16 as *const () as usize,
    );
}
pub fn emit_fp_recip_exponent32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_estimate_call(
        ctx,
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_recip_exponent32 as *const () as usize,
    );
}
pub fn emit_fp_recip_exponent64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_estimate_call(
        ctx,
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_recip_exponent64 as *const () as usize,
    );
}

fn emit_fp_recip_step_fused(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    fsize: usize,
    fallback_function: usize,
) {
    if fsize != 16
        && ctx.has_host_feature(HostFeature::FMA)
        && ctx.has_optimization(OptimizationFlag::UNSAFE_INACCURATE_NAN)
    {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let operand1 = ra.use_xmm(&mut args[0]);
        let operand2 = ra.use_xmm(&mut args[1]);
        let result = ra.scratch_xmm();
        let two = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required for FP reciprocal step")
            .get_constant(
                if fsize == 32 {
                    2.0f32.to_bits() as u64
                } else {
                    2.0f64.to_bits()
                },
                0,
            );
        ra.asm.movaps(result, rxbyak::xmmword_ptr(two)).unwrap();
        if fsize == 32 {
            ra.asm.vfnmadd231ss(result, operand1, operand2).unwrap();
        } else {
            ra.asm.vfnmadd231sd(result, operand1, operand2).unwrap();
        }
        ra.define_value(inst_ref, result);
        return;
    }

    if fsize != 16 && ctx.has_host_feature(HostFeature::FMA) {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let operand1 = ra.use_xmm(&mut args[0]);
        let operand2 = ra.use_xmm(&mut args[1]);
        let result = ra.scratch_xmm();
        let fallback = ra.asm.create_label();
        let end = ra.asm.create_label();
        let two = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required for FP reciprocal step")
            .get_constant(
                if fsize == 32 {
                    2.0f32.to_bits() as u64
                } else {
                    2.0f64.to_bits()
                },
                0,
            );
        ra.asm.movaps(result, rxbyak::xmmword_ptr(two)).unwrap();
        if fsize == 32 {
            ra.asm.vfnmadd231ss(result, operand1, operand2).unwrap();
            ra.asm.ucomiss(result, result).unwrap();
        } else {
            ra.asm.vfnmadd231sd(result, operand1, operand2).unwrap();
            ra.asm.ucomisd(result, result).unwrap();
        }
        ra.asm.jp(&fallback, JmpType::Near).unwrap();
        ra.asm.bind(&end).unwrap();

        let fpcr_value = ctx.fpcr(true).value();
        let fpsr_exc_offset = ctx.jit_state_info.offsetof_fpsr_exc as i32;
        ctx.deferred_emits
            .borrow_mut()
            .push(Box::new(move |dctx: &mut DeferredEmitCtx<'_>| {
                dctx.asm.bind(&fallback).unwrap();
                dctx.asm.lea(RSP, qword_ptr(RegExp::from(RSP) - 8)).unwrap();
                let frame = abi::push_caller_save_registers_and_adjust_stack_except(
                    dctx.asm,
                    Some(HostLoc::Xmm(result.index())),
                )
                .unwrap();
                dctx.asm
                    .movq(abi::ABI_PARAMS[0].to_reg64(), operand1)
                    .unwrap();
                dctx.asm
                    .movq(abi::ABI_PARAMS[1].to_reg64(), operand2)
                    .unwrap();
                dctx.asm
                    .mov(
                        Reg::gpr32(abi::ABI_PARAMS[2].to_reg64().index()),
                        fpcr_value as i32,
                    )
                    .unwrap();
                dctx.asm
                    .lea(
                        abi::ABI_PARAMS[3].to_reg64(),
                        rxbyak::dword_ptr(RegExp::from(R15) + fpsr_exc_offset),
                    )
                    .unwrap();
                dctx.asm.mov(rxbyak::RAX, fallback_function as i64).unwrap();
                dctx.asm.call_reg(rxbyak::RAX).unwrap();
                dctx.asm.movq(result, rxbyak::RAX).unwrap();
                abi::pop_caller_save_registers_and_adjust_stack(dctx.asm, &frame).unwrap();
                dctx.asm.add(RSP, 8).unwrap();
                dctx.asm.jmp(&end, JmpType::Near).unwrap();
            }));

        ra.define_value(inst_ref, result);
        return;
    }

    if fsize != 16 && ctx.has_optimization(OptimizationFlag::UNSAFE_UNFUSE_FMA) {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let operand1 = ra.use_scratch_xmm(&mut args[0]);
        let operand2 = ra.use_xmm(&mut args[1]);
        let result = ra.scratch_xmm();
        let two = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required for FP reciprocal step")
            .get_constant(
                if fsize == 32 {
                    2.0f32.to_bits() as u64
                } else {
                    2.0f64.to_bits()
                },
                0,
            );
        ra.asm.movaps(result, rxbyak::xmmword_ptr(two)).unwrap();
        if fsize == 32 {
            ra.asm.mulss(operand1, operand2).unwrap();
            ra.asm.subss(result, operand1).unwrap();
        } else {
            ra.asm.mulsd(operand1, operand2).unwrap();
            ra.asm.subsd(result, operand1).unwrap();
        }
        ra.define_value(inst_ref, result);
        return;
    }

    emit_fp_step_fused_call(ctx, ra, inst_ref, inst, fallback_function);
}

pub fn emit_fp_recip_step_fused16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_recip_step_fused(
        ctx,
        ra,
        inst_ref,
        inst,
        16,
        fp_helpers::fp_recip_step_fused16 as *const () as usize,
    );
}
pub fn emit_fp_recip_step_fused32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_recip_step_fused(
        ctx,
        ra,
        inst_ref,
        inst,
        32,
        fp_helpers::fp_recip_step_fused32 as *const () as usize,
    );
}
pub fn emit_fp_recip_step_fused64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_recip_step_fused(
        ctx,
        ra,
        inst_ref,
        inst,
        64,
        fp_helpers::fp_recip_step_fused64 as *const () as usize,
    );
}
pub fn emit_fp_rsqrt_estimate16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_estimate_call(
        ctx,
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_rsqrt_estimate16 as *const () as usize,
    );
}
pub fn emit_fp_rsqrt_estimate32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_estimate_call(
        ctx,
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_rsqrt_estimate32 as *const () as usize,
    );
}
pub fn emit_fp_rsqrt_estimate64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_estimate_call(
        ctx,
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_rsqrt_estimate64 as *const () as usize,
    );
}

fn emit_fp_rsqrt_step_fused(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    fsize: usize,
    fallback_function: usize,
) {
    if fsize != 16
        && ctx.has_host_feature(HostFeature::FMA | HostFeature::AVX)
        && ctx.has_optimization(OptimizationFlag::UNSAFE_INACCURATE_NAN)
    {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let operand1 = ra.use_xmm(&mut args[0]);
        let operand2 = ra.use_xmm(&mut args[1]);
        let result = ra.scratch_xmm();
        let three = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required for FP reciprocal square-root step")
            .get_constant(
                if fsize == 32 {
                    3.0f32.to_bits() as u64
                } else {
                    3.0f64.to_bits()
                },
                0,
            );
        let half = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required for FP reciprocal square-root step")
            .get_constant(
                if fsize == 32 {
                    0.5f32.to_bits() as u64
                } else {
                    0.5f64.to_bits()
                },
                0,
            );
        ra.asm.vmovaps(result, rxbyak::xmmword_ptr(three)).unwrap();
        if fsize == 32 {
            ra.asm.vfnmadd231ss(result, operand1, operand2).unwrap();
            ra.asm
                .vmulss(result, result, rxbyak::xmmword_ptr(half))
                .unwrap();
        } else {
            ra.asm.vfnmadd231sd(result, operand1, operand2).unwrap();
            ra.asm
                .vmulsd(result, result, rxbyak::xmmword_ptr(half))
                .unwrap();
        }
        ra.define_value(inst_ref, result);
        return;
    }

    if fsize != 16 && ctx.has_host_feature(HostFeature::FMA | HostFeature::AVX) {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let operand1 = ra.use_xmm(&mut args[0]);
        let operand2 = ra.use_xmm(&mut args[1]);
        let result = ra.scratch_xmm();
        let fallback = ra.asm.create_label();
        let end = ra.asm.create_label();
        let three = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required for FP reciprocal square-root step")
            .get_constant(
                if fsize == 32 {
                    3.0f32.to_bits() as u64
                } else {
                    3.0f64.to_bits()
                },
                0,
            );
        ra.asm.vmovaps(result, rxbyak::xmmword_ptr(three)).unwrap();
        if fsize == 32 {
            ra.asm.vfnmadd231ss(result, operand1, operand2).unwrap();
        } else {
            ra.asm.vfnmadd231sd(result, operand1, operand2).unwrap();
        }

        let tmp = ra.scratch_gpr();
        let tmp32 = tmp.cvt32().unwrap();
        let tmp16 = tmp.cvt16().unwrap();
        ra.asm
            .vpextrw(tmp32, result, if fsize == 32 { 1 } else { 3 })
            .unwrap();
        ra.asm
            .and_(tmp16, if fsize == 32 { 0x7f80 } else { 0x7ff0 })
            .unwrap();
        ra.asm
            .cmp(tmp16, if fsize == 32 { 0x7f00 } else { 0x7fe0 })
            .unwrap();
        ra.release(tmp);
        ra.asm.jae(&fallback, JmpType::Near).unwrap();

        let half = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required for FP reciprocal square-root step")
            .get_constant(
                if fsize == 32 {
                    0.5f32.to_bits() as u64
                } else {
                    0.5f64.to_bits()
                },
                0,
            );
        if fsize == 32 {
            ra.asm
                .vmulss(result, result, rxbyak::xmmword_ptr(half))
                .unwrap();
        } else {
            ra.asm
                .vmulsd(result, result, rxbyak::xmmword_ptr(half))
                .unwrap();
        }
        ra.asm.bind(&end).unwrap();

        let fpcr_value = ctx.fpcr(true).value();
        let fpsr_exc_offset = ctx.jit_state_info.offsetof_fpsr_exc as i32;
        ctx.deferred_emits
            .borrow_mut()
            .push(Box::new(move |dctx: &mut DeferredEmitCtx<'_>| {
                dctx.asm.bind(&fallback).unwrap();
                dctx.asm.lea(RSP, qword_ptr(RegExp::from(RSP) - 8)).unwrap();
                let frame = abi::push_caller_save_registers_and_adjust_stack_except(
                    dctx.asm,
                    Some(HostLoc::Xmm(result.index())),
                )
                .unwrap();
                dctx.asm
                    .movq(abi::ABI_PARAMS[0].to_reg64(), operand1)
                    .unwrap();
                dctx.asm
                    .movq(abi::ABI_PARAMS[1].to_reg64(), operand2)
                    .unwrap();
                dctx.asm
                    .mov(
                        Reg::gpr32(abi::ABI_PARAMS[2].to_reg64().index()),
                        fpcr_value as i32,
                    )
                    .unwrap();
                dctx.asm
                    .lea(
                        abi::ABI_PARAMS[3].to_reg64(),
                        rxbyak::dword_ptr(RegExp::from(R15) + fpsr_exc_offset),
                    )
                    .unwrap();
                dctx.asm.mov(rxbyak::RAX, fallback_function as i64).unwrap();
                dctx.asm.call_reg(rxbyak::RAX).unwrap();
                dctx.asm.movq(result, rxbyak::RAX).unwrap();
                abi::pop_caller_save_registers_and_adjust_stack(dctx.asm, &frame).unwrap();
                dctx.asm.add(RSP, 8).unwrap();
                dctx.asm.jmp(&end, JmpType::Near).unwrap();
            }));

        ra.define_value(inst_ref, result);
        return;
    }

    if fsize != 16 && ctx.has_optimization(OptimizationFlag::UNSAFE_UNFUSE_FMA) {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let operand1 = ra.use_scratch_xmm(&mut args[0]);
        let operand2 = ra.use_xmm(&mut args[1]);
        let result = ra.scratch_xmm();
        let three = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required for FP reciprocal square-root step")
            .get_constant(
                if fsize == 32 {
                    3.0f32.to_bits() as u64
                } else {
                    3.0f64.to_bits()
                },
                0,
            );
        let half = ra
            .constant_pool
            .as_mut()
            .expect("constant pool required for FP reciprocal square-root step")
            .get_constant(
                if fsize == 32 {
                    0.5f32.to_bits() as u64
                } else {
                    0.5f64.to_bits()
                },
                0,
            );
        ra.asm.movaps(result, rxbyak::xmmword_ptr(three)).unwrap();
        if fsize == 32 {
            ra.asm.mulss(operand1, operand2).unwrap();
            ra.asm.subss(result, operand1).unwrap();
            ra.asm.mulss(result, rxbyak::xmmword_ptr(half)).unwrap();
        } else {
            ra.asm.mulsd(operand1, operand2).unwrap();
            ra.asm.subsd(result, operand1).unwrap();
            ra.asm.mulsd(result, rxbyak::xmmword_ptr(half)).unwrap();
        }
        // Preserve current upstream's selected register in this unsafe path.
        ra.define_value(inst_ref, operand1);
        return;
    }

    emit_fp_step_fused_call(ctx, ra, inst_ref, inst, fallback_function);
}

pub fn emit_fp_rsqrt_step_fused16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_rsqrt_step_fused(
        ctx,
        ra,
        inst_ref,
        inst,
        16,
        fp_helpers::fp_rsqrt_step_fused16 as *const () as usize,
    );
}
pub fn emit_fp_rsqrt_step_fused32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_rsqrt_step_fused(
        ctx,
        ra,
        inst_ref,
        inst,
        32,
        fp_helpers::fp_rsqrt_step_fused32 as *const () as usize,
    );
}
pub fn emit_fp_rsqrt_step_fused64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_rsqrt_step_fused(
        ctx,
        ra,
        inst_ref,
        inst,
        64,
        fp_helpers::fp_rsqrt_step_fused64 as *const () as usize,
    );
}

// FPMulAdd/Sub 16 — host_call fallback
pub fn emit_fp_mul_add16(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_host_call_3(
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_mul_add16 as *const () as usize,
    );
}
pub fn emit_fp_mul_sub16(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_host_call_3(
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_mul_sub16 as *const () as usize,
    );
}

// Half-precision fixed-point conversions — host_call fallback
pub fn emit_fp_half_to_fixed_s16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_host_call_3(
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_half_to_fixed_s as *const () as usize,
    );
}
pub fn emit_fp_half_to_fixed_s32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_host_call_3(
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_half_to_fixed_s as *const () as usize,
    );
}
pub fn emit_fp_half_to_fixed_s64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_host_call_3(
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_half_to_fixed_s as *const () as usize,
    );
}
pub fn emit_fp_half_to_fixed_u16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_host_call_3(
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_half_to_fixed_u as *const () as usize,
    );
}
pub fn emit_fp_half_to_fixed_u32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_host_call_3(
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_half_to_fixed_u as *const () as usize,
    );
}
pub fn emit_fp_half_to_fixed_u64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_host_call_3(
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_half_to_fixed_u as *const () as usize,
    );
}

pub fn emit_fp_double_to_fixed_u16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_host_call_3(
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_to_fixed_u16 as *const () as usize,
    );
}
pub fn emit_fp_single_to_fixed_u16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_host_call_3(
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_to_fixed_u16 as *const () as usize,
    );
}
pub fn emit_fp_single_to_fixed_s16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_host_call_3(
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_half_to_fixed_s as *const () as usize,
    );
}
pub fn emit_fp_double_to_fixed_s16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_host_call_3(
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_half_to_fixed_s as *const () as usize,
    );
}

// Fixed 16-bit to FP — host_call fallback
pub fn emit_fp_fixed_u16_to_single(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_host_call_2(
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_fixed_u16_to_single as *const () as usize,
    );
}
pub fn emit_fp_fixed_s16_to_single(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_host_call_2(
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_fixed_s16_to_single as *const () as usize,
    );
}
pub fn emit_fp_fixed_u16_to_double(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_host_call_2(
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_fixed_u16_to_double as *const () as usize,
    );
}
pub fn emit_fp_fixed_s16_to_double(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_host_call_2(
        ra,
        inst_ref,
        inst,
        fp_helpers::fp_fixed_s16_to_double as *const () as usize,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fp_fn_signatures() {
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_add32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_add64;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_compare32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_mul_add32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_round_int32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_pack_2x64_to_1x128;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_abs32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_neg64;
    }

    #[test]
    fn fp_min_max_matches_arm_nan_zero_and_flush_rules() {
        let mut fpsr = Fpsr::default();
        assert_eq!(
            fp_min_max(
                1.0f32.to_bits(),
                2.0f32.to_bits(),
                Fpcr::default(),
                &mut fpsr,
                true,
                false,
            ),
            2.0f32.to_bits()
        );

        assert_eq!(
            fp_min_max(0, 0x8000_0000u32, Fpcr::default(), &mut fpsr, true, false),
            0
        );
        assert_eq!(
            fp_min_max(0, 0x8000_0000u32, Fpcr::default(), &mut fpsr, false, false),
            0x8000_0000
        );

        let qnan = 0x7fc1_2345u32;
        let snan = 0x7f81_2345u32;
        fpsr = Fpsr::default();
        assert_eq!(
            fp_min_max(qnan, snan, Fpcr::default(), &mut fpsr, true, false),
            snan | u32::MANTISSA_MSB as u32
        );
        assert_eq!(fpsr.value() & 1, 1);

        fpsr = Fpsr::default();
        assert_eq!(
            fp_min_max(
                qnan,
                1.0f32.to_bits(),
                Fpcr::default(),
                &mut fpsr,
                true,
                true,
            ),
            1.0f32.to_bits()
        );

        let fpcr_dn = Fpcr::new(1 << 25);
        assert_eq!(
            fp_min_max(qnan, 1.0f32.to_bits(), fpcr_dn, &mut fpsr, true, false),
            u32::default_nan()
        );

        fpsr = Fpsr::default();
        let fpcr_fz = Fpcr::new(1 << 24);
        assert_eq!(
            fp_min_max(1u32, 0x8000_0000, fpcr_fz, &mut fpsr, true, false),
            0
        );
        assert_eq!(fpsr.value() & (1 << 7), 0);
    }
}
