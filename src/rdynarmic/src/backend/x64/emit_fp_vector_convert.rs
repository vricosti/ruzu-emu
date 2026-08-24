#![allow(
    clippy::missing_transmute_annotations,
    clippy::useless_transmute,
    unnecessary_transmutes
)]

use rxbyak::{dword_ptr, qword_ptr, xmmword_ptr, JmpType, Reg, RegExp, R15, RSP, XMM0};

use crate::backend::x64::abi;
use crate::backend::x64::constants::{cmp, convert_rounding_mode_to_x64_immediate};
use crate::backend::x64::emit_context::{DeferredEmitCtx, EmitContext};
use crate::backend::x64::emit_fp_vector::force_to_default_nan_vector;
use crate::backend::x64::emit_vector_helpers::*;
use crate::backend::x64::fp_helpers;
use crate::backend::x64::host_feature::HostFeature;
use crate::backend::x64::hostloc::HostLoc;
use crate::backend::x64::reg_alloc::RegAlloc;
use crate::common::fp::fpcr::Fpcr;
use crate::common::fp::fpsr::Fpsr;
use crate::common::fp::op::fp_convert::fp_convert;
use crate::common::fp::op::fp_mul_add::fp_mul_add;
use crate::common::fp::op::fp_recip_step_fused::fp_recip_step_fused;
use crate::common::fp::op::fp_round_int::fp_round_int;
use crate::common::fp::op::fp_rsqrt_step_fused::fp_rsqrt_step_fused;
use crate::common::fp::op::fp_to_fixed::fp_to_fixed;
use crate::common::fp::rounding_mode::RoundingMode;
use crate::interface::optimization_flags::OptimizationFlag;
use crate::ir::inst::Inst;
use crate::ir::value::InstRef;

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn host_supports_fma_avx() -> bool {
    std::is_x86_feature_detected!("fma") && std::is_x86_feature_detected!("avx")
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn host_supports_avx() -> bool {
    std::is_x86_feature_detected!("avx")
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
fn host_supports_fma_avx() -> bool {
    false
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
fn host_supports_avx() -> bool {
    false
}

/// Port of upstream `MaybeStandardFPSCRValue` from
/// `emit_x64_vector_floating_point.cpp`.
fn maybe_standard_fpscr_value(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    fpcr_controlled: bool,
    emit: impl FnOnce(&mut RegAlloc),
) {
    let switch_mxcsr = ctx.fpcr(fpcr_controlled) != ctx.fpcr(true);

    if switch_mxcsr && !ctx.has_optimization(OptimizationFlag::UNSAFE_IGNORE_STANDARD_FPCR_VALUE) {
        ra.asm
            .stmxcsr(dword_ptr(
                RegExp::from(R15) + ctx.jit_state_info.offsetof_guest_mxcsr as i32,
            ))
            .unwrap();
        ra.asm
            .ldmxcsr(dword_ptr(
                RegExp::from(R15) + ctx.jit_state_info.offsetof_asimd_mxcsr as i32,
            ))
            .unwrap();
        emit(ra);
        ra.asm
            .stmxcsr(dword_ptr(
                RegExp::from(R15) + ctx.jit_state_info.offsetof_asimd_mxcsr as i32,
            ))
            .unwrap();
        ra.asm
            .ldmxcsr(dword_ptr(
                RegExp::from(R15) + ctx.jit_state_info.offsetof_guest_mxcsr as i32,
            ))
            .unwrap();
    } else {
        emit(ra);
    }
}

// ---------------------------------------------------------------------------
// FPVectorMulAdd — fallback (fused multiply-add: result = a + b*c or a*b+c)
// FPVectorMulAdd16/32/64
// ---------------------------------------------------------------------------

macro_rules! define_fp_muladd_fallback {
    ($name:ident, $type:ty, $count:expr, $exponent_mask:expr, $mantissa_mask:expr, $smallest_normal:expr) => {
        extern "C" fn $name(
            result: *mut [u8; 16],
            addend: *const [u8; 16],
            op1: *const [u8; 16],
            op2: *const [u8; 16],
            fpcr: u32,
            fpsr_exc: *mut u32,
        ) {
            unsafe {
                let addend: [$type; $count] = std::mem::transmute(*addend);
                let op1: [$type; $count] = std::mem::transmute(*op1);
                let op2: [$type; $count] = std::mem::transmute(*op2);
                let mut output = [0 as $type; $count];
                let fpcr = Fpcr::new(fpcr);
                let mut fpsr = Fpsr::new(fpsr_exc.read());
                let had_idc = fpsr.value() & (1 << 7) != 0;
                let mut correction_raises_idc = false;
                for index in 0..$count {
                    output[index] =
                        fp_mul_add(addend[index], op1[index], op2[index], fpcr, &mut fpsr);

                    // Upstream normally executes vector FMA through the host
                    // instruction. With FZ enabled, it invokes the reference
                    // helper only for lanes whose magnitude is exactly the
                    // smallest normal number. DAZ otherwise consumes input
                    // denormals without mapping MXCSR.DE to FPSR.IDC.
                    if fpcr.fz()
                        && (output[index] as u64 & ($exponent_mask | $mantissa_mask))
                            == $smallest_normal
                        && [addend[index], op1[index], op2[index]]
                            .into_iter()
                            .any(|value| {
                                let bits = value as u64;
                                bits & $exponent_mask == 0 && bits & $mantissa_mask != 0
                            })
                    {
                        correction_raises_idc = true;
                    }
                }
                if host_supports_fma_avx() && !had_idc && !correction_raises_idc {
                    fpsr.set_idc(false);
                }
                fpsr_exc.write(fpsr.value());
                *result = std::mem::transmute(output);
            }
        }
    };
}

define_fp_muladd_fallback!(fallback_fp_muladd16, u16, 8, 0x7c00, 0x03ff, 0x0400);
define_fp_muladd_fallback!(
    fallback_fp_muladd32,
    u32,
    4,
    0x7f80_0000,
    0x007f_ffff,
    0x0080_0000
);
define_fp_muladd_fallback!(
    fallback_fp_muladd64,
    u64,
    2,
    0x7ff0_0000_0000_0000,
    0x000f_ffff_ffff_ffff,
    0x0010_0000_0000_0000
);

macro_rules! define_fp_muladd_correction_fallback {
    ($name:ident, $type:ty, $count:expr, $exponent_mask:expr, $mantissa_mask:expr, $smallest_normal:expr, $correct_nan:expr) => {
        extern "C" fn $name(
            result: *mut [u8; 16],
            addend: *const [u8; 16],
            op1: *const [u8; 16],
            op2: *const [u8; 16],
            fpcr: u32,
            fpsr_exc: *mut u32,
        ) {
            unsafe {
                let mut output: [$type; $count] = std::mem::transmute(*result);
                let addend: [$type; $count] = std::mem::transmute(*addend);
                let op1: [$type; $count] = std::mem::transmute(*op1);
                let op2: [$type; $count] = std::mem::transmute(*op2);
                let fpcr = Fpcr::new(fpcr);
                let mut fpsr = Fpsr::new(fpsr_exc.read());
                for index in 0..$count {
                    let bits = output[index] as u64;
                    let is_smallest_normal =
                        bits & ($exponent_mask | $mantissa_mask) == $smallest_normal;
                    let is_nan =
                        bits & $exponent_mask == $exponent_mask && bits & $mantissa_mask != 0;
                    if (fpcr.fz() && is_smallest_normal) || ($correct_nan && is_nan) {
                        output[index] =
                            fp_mul_add(addend[index], op1[index], op2[index], fpcr, &mut fpsr);
                    }
                }
                fpsr_exc.write(fpsr.value());
                *result = std::mem::transmute(output);
            }
        }
    };
}

define_fp_muladd_correction_fallback!(
    fallback_fp_muladd_correction32,
    u32,
    4,
    0x7f80_0000,
    0x007f_ffff,
    0x0080_0000,
    true
);
define_fp_muladd_correction_fallback!(
    fallback_fp_muladd_correction32_inaccurate_nan,
    u32,
    4,
    0x7f80_0000,
    0x007f_ffff,
    0x0080_0000,
    false
);
define_fp_muladd_correction_fallback!(
    fallback_fp_muladd_correction64,
    u64,
    2,
    0x7ff0_0000_0000_0000,
    0x000f_ffff_ffff_ffff,
    0x0010_0000_0000_0000,
    true
);
define_fp_muladd_correction_fallback!(
    fallback_fp_muladd_correction64_inaccurate_nan,
    u64,
    2,
    0x7ff0_0000_0000_0000,
    0x000f_ffff_ffff_ffff,
    0x0010_0000_0000_0000,
    false
);

pub fn emit_fp_vector_muladd16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_four_op_fallback(ctx, ra, inst_ref, inst, fallback_fp_muladd16 as usize);
}
pub fn emit_fp_vector_muladd32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_vector_muladd(ctx, ra, inst_ref, inst, 32, fallback_fp_muladd32 as usize);
}
pub fn emit_fp_vector_muladd64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_vector_muladd(ctx, ra, inst_ref, inst, 64, fallback_fp_muladd64 as usize);
}

fn emit_fp_vector_muladd(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    fsize: usize,
    fallback_function: usize,
) {
    let fpcr_controlled = inst.args[3].get_u1();
    let fpcr = ctx.fpcr(fpcr_controlled);
    let inaccurate_nan = ctx.has_optimization(OptimizationFlag::UNSAFE_INACCURATE_NAN);

    if ctx.has_host_feature(HostFeature::FMA) && !fpcr.fz() && (fpcr.dn() || inaccurate_nan) {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let result = ra.use_scratch_xmm(&mut args[0]);
        let operand2 = ra.use_xmm(&mut args[1]);
        let operand3 = ra.use_xmm(&mut args[2]);
        if fsize == 32 {
            ra.asm.vfmadd231ps(result, operand2, operand3).unwrap();
        } else {
            ra.asm.vfmadd231pd(result, operand2, operand3).unwrap();
        }
        if fpcr.dn() {
            force_to_default_nan_vector(ra, result, fsize);
        }
        ra.define_value(inst_ref, result);
        return;
    }

    if ctx.has_host_feature(HostFeature::FMA | HostFeature::AVX) {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let operand1 = ra.use_xmm(&mut args[0]);
        let operand2 = ra.use_xmm(&mut args[1]);
        let operand3 = ra.use_xmm(&mut args[2]);
        let result = ra.scratch_xmm();
        let nan_mask = ra.scratch_xmm();
        let fallback = ra.asm.create_label();
        let end = ra.asm.create_label();

        ra.asm.movaps(result, operand1).unwrap();
        if fsize == 32 {
            ra.asm.vfmadd231ps(result, operand2, operand3).unwrap();
        } else {
            ra.asm.vfmadd231pd(result, operand2, operand3).unwrap();
        }

        let needs_nan_correction = !fpcr.dn() && !inaccurate_nan;
        if fpcr.fz() {
            ra.asm.movaps(nan_mask, result).unwrap();
            let (non_sign_lo, non_sign_hi, smallest_lo, smallest_hi) = if fsize == 32 {
                (
                    0x7fff_ffff_7fff_ffff,
                    0x7fff_ffff_7fff_ffff,
                    0x0080_0000_0080_0000,
                    0x0080_0000_0080_0000,
                )
            } else {
                (
                    0x7fff_ffff_ffff_ffff,
                    0x7fff_ffff_ffff_ffff,
                    0x0010_0000_0000_0000,
                    0x0010_0000_0000_0000,
                )
            };
            let non_sign = ra
                .constant_pool
                .as_mut()
                .expect("constant pool required")
                .get_constant(non_sign_lo, non_sign_hi);
            let smallest = ra
                .constant_pool
                .as_mut()
                .expect("constant pool required")
                .get_constant(smallest_lo, smallest_hi);
            ra.asm.andps(nan_mask, xmmword_ptr(non_sign)).unwrap();
            if fsize == 32 {
                ra.asm
                    .cmpps(nan_mask, xmmword_ptr(smallest), cmp::EQUAL_OQ)
                    .unwrap();
            } else {
                ra.asm
                    .cmppd(nan_mask, xmmword_ptr(smallest), cmp::EQUAL_OQ)
                    .unwrap();
            }
            if needs_nan_correction {
                let unordered = ra.scratch_xmm();
                ra.asm.movaps(unordered, result).unwrap();
                if fsize == 32 {
                    ra.asm.cmpps(unordered, result, cmp::UNORDERED_Q).unwrap();
                } else {
                    ra.asm.cmppd(unordered, result, cmp::UNORDERED_Q).unwrap();
                }
                ra.asm.orps(nan_mask, unordered).unwrap();
                ra.release(unordered);
            }
        } else {
            debug_assert!(needs_nan_correction);
            ra.asm.movaps(nan_mask, result).unwrap();
            if fsize == 32 {
                ra.asm.cmpps(nan_mask, result, cmp::UNORDERED_Q).unwrap();
            } else {
                ra.asm.cmppd(nan_mask, result, cmp::UNORDERED_Q).unwrap();
            }
        }
        ra.asm.vptest(nan_mask, nan_mask).unwrap();
        ra.asm.jnz(&fallback, JmpType::Near).unwrap();
        ra.asm.bind(&end).unwrap();
        if fpcr.dn() {
            force_to_default_nan_vector(ra, result, fsize);
        }

        let fpcr_value = fpcr.value();
        let fpsr_offset = ctx.jit_state_info.offsetof_fpsr_exc as i32;
        let correction_function = match (fsize, inaccurate_nan) {
            (32, false) => fallback_fp_muladd_correction32 as usize,
            (32, true) => fallback_fp_muladd_correction32_inaccurate_nan as usize,
            (64, false) => fallback_fp_muladd_correction64 as usize,
            (64, true) => fallback_fp_muladd_correction64_inaccurate_nan as usize,
            _ => unreachable!(),
        };
        ctx.deferred_emits
            .borrow_mut()
            .push(Box::new(move |dctx: &mut DeferredEmitCtx<'_>| {
                dctx.asm.bind(&fallback).unwrap();
                dctx.asm.sub(RSP, 8).unwrap();

                #[cfg(target_os = "windows")]
                const STACK_ARGS_SIZE: usize = 16;
                #[cfg(not(target_os = "windows"))]
                const STACK_ARGS_SIZE: usize = 0;
                let (frame, local_base) =
                    abi::push_caller_save_registers_and_adjust_stack_except_with_local(
                        dctx.asm,
                        Some(HostLoc::Xmm(result.get_idx())),
                        STACK_ARGS_SIZE + 64,
                    )
                    .unwrap();
                let result_offset = local_base + STACK_ARGS_SIZE;
                let operand1_offset = result_offset + 16;
                let operand2_offset = result_offset + 32;
                let operand3_offset = result_offset + 48;

                for (offset, operand) in [
                    (result_offset, result),
                    (operand1_offset, operand1),
                    (operand2_offset, operand2),
                    (operand3_offset, operand3),
                ] {
                    dctx.asm
                        .movaps(xmmword_ptr(RegExp::from(RSP) + offset as i32), operand)
                        .unwrap();
                }
                for (index, offset) in [
                    result_offset,
                    operand1_offset,
                    operand2_offset,
                    operand3_offset,
                ]
                .into_iter()
                .enumerate()
                {
                    dctx.asm
                        .lea(
                            abi::ABI_PARAMS[index].to_reg64(),
                            xmmword_ptr(RegExp::from(RSP) + offset as i32),
                        )
                        .unwrap();
                }

                #[cfg(target_os = "windows")]
                {
                    dctx.asm
                        .mov(
                            rxbyak::qword_ptr(RegExp::from(RSP) + abi::ABI_SHADOW_SPACE as i32),
                            fpcr_value as i32,
                        )
                        .unwrap();
                    dctx.asm
                        .lea(rxbyak::RAX, dword_ptr(RegExp::from(R15) + fpsr_offset))
                        .unwrap();
                    dctx.asm
                        .mov(
                            rxbyak::qword_ptr(RegExp::from(RSP) + abi::ABI_SHADOW_SPACE as i32 + 8),
                            rxbyak::RAX,
                        )
                        .unwrap();
                }
                #[cfg(not(target_os = "windows"))]
                {
                    dctx.asm
                        .mov(
                            Reg::gpr32(abi::ABI_PARAMS[4].to_reg64().get_idx()),
                            fpcr_value as i32,
                        )
                        .unwrap();
                    dctx.asm
                        .lea(
                            abi::ABI_PARAMS[5].to_reg64(),
                            dword_ptr(RegExp::from(R15) + fpsr_offset),
                        )
                        .unwrap();
                }

                dctx.asm
                    .mov(rxbyak::RAX, correction_function as i64)
                    .unwrap();
                dctx.asm.call_reg(rxbyak::RAX).unwrap();
                dctx.asm
                    .movaps(
                        result,
                        xmmword_ptr(RegExp::from(RSP) + result_offset as i32),
                    )
                    .unwrap();
                abi::pop_caller_save_registers_and_adjust_stack(dctx.asm, &frame).unwrap();
                dctx.asm.add(RSP, 8).unwrap();
                dctx.asm.jmp(&end, JmpType::Near).unwrap();
            }));

        ra.release(nan_mask);
        ra.define_value(inst_ref, result);
        return;
    }

    if ctx.has_optimization(OptimizationFlag::UNSAFE_UNFUSE_FMA) {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let result = ra.use_scratch_xmm(&mut args[0]);
        let product = ra.use_scratch_xmm(&mut args[1]);
        let operand3 = ra.use_xmm(&mut args[2]);
        if fsize == 32 {
            ra.asm.mulps(product, operand3).unwrap();
            ra.asm.addps(result, product).unwrap();
        } else {
            ra.asm.mulpd(product, operand3).unwrap();
            ra.asm.addpd(result, product).unwrap();
        }
        ra.release(product);
        ra.define_value(inst_ref, result);
        return;
    }

    emit_four_op_fallback(ctx, ra, inst_ref, inst, fallback_function);
}

// ---------------------------------------------------------------------------
// FPVectorRecipEstimate — fallback
// ---------------------------------------------------------------------------

extern "C" fn fallback_fp_recip_est16(
    result: *mut [u8; 16],
    a: *const [u8; 16],
    fpcr: u32,
    fpsr_exc: *mut u32,
) {
    unsafe {
        let va: [u16; 8] = std::mem::transmute(*a);
        let mut out = [0u16; 8];
        for i in 0..8 {
            out[i] = fp_helpers::fp_recip_estimate16(va[i] as u64, fpcr, fpsr_exc) as u16;
        }
        *result = std::mem::transmute(out);
    }
}

extern "C" fn fallback_fp_recip_est32(
    result: *mut [u8; 16],
    a: *const [u8; 16],
    fpcr: u32,
    fpsr_exc: *mut u32,
) {
    unsafe {
        let va: [f32; 4] = std::mem::transmute(*a);
        let out: [f32; 4] = [
            f32::from_bits(fp_helpers::fp_recip_estimate32(
                va[0].to_bits(),
                fpcr,
                fpsr_exc,
            )),
            f32::from_bits(fp_helpers::fp_recip_estimate32(
                va[1].to_bits(),
                fpcr,
                fpsr_exc,
            )),
            f32::from_bits(fp_helpers::fp_recip_estimate32(
                va[2].to_bits(),
                fpcr,
                fpsr_exc,
            )),
            f32::from_bits(fp_helpers::fp_recip_estimate32(
                va[3].to_bits(),
                fpcr,
                fpsr_exc,
            )),
        ];
        *result = std::mem::transmute(out);
    }
}

extern "C" fn fallback_fp_recip_est64(
    result: *mut [u8; 16],
    a: *const [u8; 16],
    fpcr: u32,
    fpsr_exc: *mut u32,
) {
    unsafe {
        let va: [f64; 2] = std::mem::transmute(*a);
        let out: [f64; 2] = [
            f64::from_bits(fp_helpers::fp_recip_estimate64(
                va[0].to_bits(),
                fpcr,
                fpsr_exc,
            )),
            f64::from_bits(fp_helpers::fp_recip_estimate64(
                va[1].to_bits(),
                fpcr,
                fpsr_exc,
            )),
        ];
        *result = std::mem::transmute(out);
    }
}

pub fn emit_fp_vector_recip_estimate16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_op_fallback(ctx, ra, inst_ref, inst, fallback_fp_recip_est16 as usize);
}
pub fn emit_fp_vector_recip_estimate32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_op_fallback(ctx, ra, inst_ref, inst, fallback_fp_recip_est32 as usize);
}
pub fn emit_fp_vector_recip_estimate64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_op_fallback(ctx, ra, inst_ref, inst, fallback_fp_recip_est64 as usize);
}

// ---------------------------------------------------------------------------
// FPVectorRecipStepFused / FPVectorRSqrtStepFused — fused Newton-Raphson step.
//
// Per the ARM pseudocode (FPRecipStepFused / FPRSqrtStepFused): when one operand
// is 0 and the other is infinity the result is the constant 2.0 (recip) / 1.5
// (rsqrt); otherwise it is `2 - a*b` / `(3 - a*b)/2` computed with a SINGLE
// rounding (FMA). The previous fallbacks computed the arithmetic non-fused
// (1-ULP error in the common finite case) AND produced a NaN for the 0*inf
// case — both wrong vs hardware.
// ---------------------------------------------------------------------------

macro_rules! define_fp_step_fallback {
    ($name:ident, $type:ty, $count:expr, $operation:ident) => {
        extern "C" fn $name(
            result: *mut [u8; 16],
            a: *const [u8; 16],
            b: *const [u8; 16],
            fpcr: u32,
            fpsr_exc: *mut u32,
        ) {
            unsafe {
                let va: [$type; $count] = std::mem::transmute(*a);
                let vb: [$type; $count] = std::mem::transmute(*b);
                let mut out = [0 as $type; $count];
                let fpcr = Fpcr::new(fpcr);
                let mut fpsr = Fpsr::new(fpsr_exc.read());
                for index in 0..$count {
                    out[index] = $operation(va[index], vb[index], fpcr, &mut fpsr);
                }
                fpsr_exc.write(fpsr.value());
                *result = std::mem::transmute(out);
            }
        }
    };
}

define_fp_step_fallback!(fallback_fp_recip_step16, u16, 8, fp_recip_step_fused);
define_fp_step_fallback!(fallback_fp_recip_step32, u32, 4, fp_recip_step_fused);
define_fp_step_fallback!(fallback_fp_recip_step64, u64, 2, fp_recip_step_fused);

fn emit_fp_vector_recip_step_fused(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    esize: usize,
    fallback_function: usize,
) {
    if esize != 16
        && ctx.has_host_feature(HostFeature::FMA | HostFeature::AVX)
        && ctx.has_optimization(OptimizationFlag::UNSAFE_INACCURATE_NAN)
    {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let fpcr_controlled = args[2].get_immediate_u1();
        let result = ra.scratch_xmm();
        let operand1 = ra.use_xmm(&mut args[0]);
        let operand2 = ra.use_xmm(&mut args[1]);

        maybe_standard_fpscr_value(ctx, ra, fpcr_controlled, |ra| {
            let two = vector_constant(
                ra,
                esize,
                if esize == 32 {
                    2.0f32.to_bits() as u64
                } else {
                    2.0f64.to_bits()
                },
            );
            ra.asm.movaps(result, xmmword_ptr(two)).unwrap();
            if esize == 32 {
                ra.asm.vfnmadd231ps(result, operand1, operand2).unwrap();
            } else {
                ra.asm.vfnmadd231pd(result, operand1, operand2).unwrap();
            }
        });

        ra.define_value(inst_ref, result);
        return;
    }

    if esize != 16 && ctx.has_host_feature(HostFeature::FMA | HostFeature::AVX) {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let fpcr_controlled = args[2].get_immediate_u1();
        let result = ra.scratch_xmm();
        let operand1 = ra.use_xmm(&mut args[0]);
        let operand2 = ra.use_xmm(&mut args[1]);
        let tmp = ra.scratch_xmm();
        let fallback = ra.asm.create_label();
        let end = ra.asm.create_label();

        maybe_standard_fpscr_value(ctx, ra, fpcr_controlled, |ra| {
            let two = vector_constant(
                ra,
                esize,
                if esize == 32 {
                    2.0f32.to_bits() as u64
                } else {
                    2.0f64.to_bits()
                },
            );
            ra.asm.movaps(result, xmmword_ptr(two)).unwrap();
            if esize == 32 {
                ra.asm.vfnmadd231ps(result, operand1, operand2).unwrap();
                ra.asm
                    .vcmpps(tmp, result, result, cmp::UNORDERED_Q)
                    .unwrap();
            } else {
                ra.asm.vfnmadd231pd(result, operand1, operand2).unwrap();
                ra.asm
                    .vcmppd(tmp, result, result, cmp::UNORDERED_Q)
                    .unwrap();
            }
            ra.asm.vptest(tmp, tmp).unwrap();
            ra.asm.jnz(&fallback, JmpType::Near).unwrap();
            ra.asm.bind(&end).unwrap();
        });

        let fpcr_value = ctx.fpcr(fpcr_controlled).value();
        let fpsr_exc_offset = ctx.jit_state_info.offsetof_fpsr_exc as i32;
        ctx.deferred_emits
            .borrow_mut()
            .push(Box::new(move |dctx: &mut DeferredEmitCtx<'_>| {
                dctx.asm.bind(&fallback).unwrap();
                dctx.asm.lea(RSP, qword_ptr(RegExp::from(RSP) - 8)).unwrap();
                let frame = abi::push_caller_save_registers_and_adjust_stack_except(
                    dctx.asm,
                    Some(HostLoc::Xmm(result.get_idx())),
                )
                .unwrap();
                emit_three_op_fallback_without_reg_alloc(
                    dctx.asm,
                    result,
                    operand1,
                    operand2,
                    fallback_function,
                    fpcr_value,
                    fpsr_exc_offset,
                );
                abi::pop_caller_save_registers_and_adjust_stack(dctx.asm, &frame).unwrap();
                dctx.asm.add(RSP, 8).unwrap();
                dctx.asm.jmp(&end, JmpType::Near).unwrap();
            }));

        ra.release(tmp);
        ra.define_value(inst_ref, result);
        return;
    }

    if esize != 16 && ctx.has_optimization(OptimizationFlag::UNSAFE_UNFUSE_FMA) {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let operand1 = ra.use_scratch_xmm(&mut args[0]);
        let operand2 = ra.use_xmm(&mut args[1]);
        let result = ra.scratch_xmm();
        let two = vector_constant(
            ra,
            esize,
            if esize == 32 {
                2.0f32.to_bits() as u64
            } else {
                2.0f64.to_bits()
            },
        );
        ra.asm.movaps(result, xmmword_ptr(two)).unwrap();
        if esize == 32 {
            ra.asm.mulps(operand1, operand2).unwrap();
            ra.asm.subps(result, operand1).unwrap();
        } else {
            ra.asm.mulpd(operand1, operand2).unwrap();
            ra.asm.subpd(result, operand1).unwrap();
        }
        ra.define_value(inst_ref, result);
        return;
    }

    emit_three_op_fallback(ctx, ra, inst_ref, inst, fallback_function);
}

pub fn emit_fp_vector_recip_step_fused16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_vector_recip_step_fused(
        ctx,
        ra,
        inst_ref,
        inst,
        16,
        fallback_fp_recip_step16 as usize,
    );
}
pub fn emit_fp_vector_recip_step_fused32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_vector_recip_step_fused(
        ctx,
        ra,
        inst_ref,
        inst,
        32,
        fallback_fp_recip_step32 as usize,
    );
}
pub fn emit_fp_vector_recip_step_fused64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_vector_recip_step_fused(
        ctx,
        ra,
        inst_ref,
        inst,
        64,
        fallback_fp_recip_step64 as usize,
    );
}

// ---------------------------------------------------------------------------
// FPVectorRSqrtEstimate — fallback
// ---------------------------------------------------------------------------

extern "C" fn fallback_fp_rsqrt_est16(
    result: *mut [u8; 16],
    a: *const [u8; 16],
    fpcr: u32,
    fpsr_exc: *mut u32,
) {
    unsafe {
        let va: [u16; 8] = std::mem::transmute(*a);
        let mut out = [0u16; 8];
        for i in 0..8 {
            out[i] = fp_helpers::fp_rsqrt_estimate16(va[i] as u64, fpcr, fpsr_exc) as u16;
        }
        *result = std::mem::transmute(out);
    }
}

extern "C" fn fallback_fp_rsqrt_est32(
    result: *mut [u8; 16],
    a: *const [u8; 16],
    fpcr: u32,
    fpsr_exc: *mut u32,
) {
    unsafe {
        let va: [f32; 4] = std::mem::transmute(*a);
        let out: [f32; 4] = [
            f32::from_bits(fp_helpers::fp_rsqrt_estimate32(
                va[0].to_bits(),
                fpcr,
                fpsr_exc,
            )),
            f32::from_bits(fp_helpers::fp_rsqrt_estimate32(
                va[1].to_bits(),
                fpcr,
                fpsr_exc,
            )),
            f32::from_bits(fp_helpers::fp_rsqrt_estimate32(
                va[2].to_bits(),
                fpcr,
                fpsr_exc,
            )),
            f32::from_bits(fp_helpers::fp_rsqrt_estimate32(
                va[3].to_bits(),
                fpcr,
                fpsr_exc,
            )),
        ];
        // Upstream's AVX fast path handles the entire vector only when every
        // lane is positive, normal and finite. Its sqrt/div sequence always
        // raises inexact because it first injects a mantissa bit. One special
        // lane branches the complete vector to the reference fallback.
        if host_supports_avx()
            && va
                .iter()
                .all(|value| value.is_normal() && value.is_sign_positive())
        {
            fpsr_exc.write(fpsr_exc.read() | (1 << 4));
        }
        *result = std::mem::transmute(out);
    }
}

extern "C" fn fallback_fp_rsqrt_est64(
    result: *mut [u8; 16],
    a: *const [u8; 16],
    fpcr: u32,
    fpsr_exc: *mut u32,
) {
    unsafe {
        let va: [f64; 2] = std::mem::transmute(*a);
        let out: [f64; 2] = [
            f64::from_bits(fp_helpers::fp_rsqrt_estimate64(
                va[0].to_bits(),
                fpcr,
                fpsr_exc,
            )),
            f64::from_bits(fp_helpers::fp_rsqrt_estimate64(
                va[1].to_bits(),
                fpcr,
                fpsr_exc,
            )),
        ];
        if host_supports_avx()
            && va
                .iter()
                .all(|value| value.is_normal() && value.is_sign_positive())
        {
            fpsr_exc.write(fpsr_exc.read() | (1 << 4));
        }
        *result = std::mem::transmute(out);
    }
}

pub fn emit_fp_vector_rsqrt_estimate16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_op_fallback(ctx, ra, inst_ref, inst, fallback_fp_rsqrt_est16 as usize);
}
pub fn emit_fp_vector_rsqrt_estimate32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_op_fallback(ctx, ra, inst_ref, inst, fallback_fp_rsqrt_est32 as usize);
}
pub fn emit_fp_vector_rsqrt_estimate64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_op_fallback(ctx, ra, inst_ref, inst, fallback_fp_rsqrt_est64 as usize);
}

// ---------------------------------------------------------------------------
// FPVectorRSqrtStepFused — native FMA with reference fallback
// ---------------------------------------------------------------------------

define_fp_step_fallback!(fallback_fp_rsqrt_step16, u16, 8, fp_rsqrt_step_fused);
define_fp_step_fallback!(fallback_fp_rsqrt_step32, u32, 4, fp_rsqrt_step_fused);
define_fp_step_fallback!(fallback_fp_rsqrt_step64, u64, 2, fp_rsqrt_step_fused);

fn emit_fp_vector_rsqrt_step_fused(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    esize: usize,
    fallback_function: usize,
) {
    if esize != 16
        && ctx.has_host_feature(HostFeature::FMA | HostFeature::AVX)
        && ctx.has_optimization(OptimizationFlag::UNSAFE_INACCURATE_NAN)
    {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let fpcr_controlled = args[2].get_immediate_u1();
        let result = ra.scratch_xmm();
        let operand1 = ra.use_xmm(&mut args[0]);
        let operand2 = ra.use_xmm(&mut args[1]);

        maybe_standard_fpscr_value(ctx, ra, fpcr_controlled, |ra| {
            let three = vector_constant(
                ra,
                esize,
                if esize == 32 {
                    3.0f32.to_bits() as u64
                } else {
                    3.0f64.to_bits()
                },
            );
            let half = vector_constant(
                ra,
                esize,
                if esize == 32 {
                    0.5f32.to_bits() as u64
                } else {
                    0.5f64.to_bits()
                },
            );
            ra.asm.vmovaps(result, xmmword_ptr(three)).unwrap();
            if esize == 32 {
                ra.asm.vfnmadd231ps(result, operand1, operand2).unwrap();
                ra.asm.vmulps(result, result, xmmword_ptr(half)).unwrap();
            } else {
                ra.asm.vfnmadd231pd(result, operand1, operand2).unwrap();
                ra.asm.vmulpd(result, result, xmmword_ptr(half)).unwrap();
            }
        });

        ra.define_value(inst_ref, result);
        return;
    }

    if esize != 16 && ctx.has_host_feature(HostFeature::FMA | HostFeature::AVX) {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let fpcr_controlled = args[2].get_immediate_u1();
        let result = ra.scratch_xmm();
        let operand1 = ra.use_xmm(&mut args[0]);
        let operand2 = ra.use_xmm(&mut args[1]);
        let tmp = ra.scratch_xmm();
        let mask = ra.scratch_xmm();
        let fallback = ra.asm.create_label();
        let end = ra.asm.create_label();

        maybe_standard_fpscr_value(ctx, ra, fpcr_controlled, |ra| {
            let three = vector_constant(
                ra,
                esize,
                if esize == 32 {
                    3.0f32.to_bits() as u64
                } else {
                    3.0f64.to_bits()
                },
            );
            let dangerous_exponent = vector_constant(
                ra,
                esize,
                if esize == 32 {
                    0x7f00_0000
                } else {
                    0x7fe0_0000_0000_0000
                },
            );
            ra.asm.vmovaps(result, xmmword_ptr(three)).unwrap();
            if esize == 32 {
                ra.asm.vfnmadd231ps(result, operand1, operand2).unwrap();
            } else {
                ra.asm.vfnmadd231pd(result, operand1, operand2).unwrap();
            }

            // Upstream tests the fused intermediate before the exact
            // division by two. Infinity, NaN and the adjacent exponent range
            // all use the reference fallback.
            ra.asm
                .vmovaps(mask, xmmword_ptr(dangerous_exponent))
                .unwrap();
            if esize == 32 {
                ra.asm.vandps(tmp, result, mask).unwrap();
                ra.asm.vpcmpeqd(tmp, tmp, mask).unwrap();
            } else {
                ra.asm.vandpd(tmp, result, mask).unwrap();
                ra.asm.vpcmpeqq(tmp, tmp, mask).unwrap();
            }
            ra.asm.ptest(tmp, tmp).unwrap();
            ra.asm.jnz(&fallback, JmpType::Near).unwrap();

            let half = vector_constant(
                ra,
                esize,
                if esize == 32 {
                    0.5f32.to_bits() as u64
                } else {
                    0.5f64.to_bits()
                },
            );
            if esize == 32 {
                ra.asm.vmulps(result, result, xmmword_ptr(half)).unwrap();
            } else {
                ra.asm.vmulpd(result, result, xmmword_ptr(half)).unwrap();
            }
            ra.asm.bind(&end).unwrap();
        });

        let fpcr_value = ctx.fpcr(fpcr_controlled).value();
        let fpsr_exc_offset = ctx.jit_state_info.offsetof_fpsr_exc as i32;
        ctx.deferred_emits
            .borrow_mut()
            .push(Box::new(move |dctx: &mut DeferredEmitCtx<'_>| {
                dctx.asm.bind(&fallback).unwrap();
                dctx.asm.lea(RSP, qword_ptr(RegExp::from(RSP) - 8)).unwrap();
                let frame = abi::push_caller_save_registers_and_adjust_stack_except(
                    dctx.asm,
                    Some(HostLoc::Xmm(result.get_idx())),
                )
                .unwrap();
                emit_three_op_fallback_without_reg_alloc(
                    dctx.asm,
                    result,
                    operand1,
                    operand2,
                    fallback_function,
                    fpcr_value,
                    fpsr_exc_offset,
                );
                abi::pop_caller_save_registers_and_adjust_stack(dctx.asm, &frame).unwrap();
                dctx.asm.add(RSP, 8).unwrap();
                dctx.asm.jmp(&end, JmpType::Near).unwrap();
            }));

        ra.release(mask);
        ra.release(tmp);
        ra.define_value(inst_ref, result);
        return;
    }

    if esize != 16 && ctx.has_optimization(OptimizationFlag::UNSAFE_UNFUSE_FMA) {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let operand1 = ra.use_scratch_xmm(&mut args[0]);
        let operand2 = ra.use_xmm(&mut args[1]);
        let result = ra.scratch_xmm();
        let three = vector_constant(
            ra,
            esize,
            if esize == 32 {
                3.0f32.to_bits() as u64
            } else {
                3.0f64.to_bits()
            },
        );
        let half = vector_constant(
            ra,
            esize,
            if esize == 32 {
                0.5f32.to_bits() as u64
            } else {
                0.5f64.to_bits()
            },
        );
        ra.asm.movaps(result, xmmword_ptr(three)).unwrap();
        if esize == 32 {
            ra.asm.mulps(operand1, operand2).unwrap();
            ra.asm.subps(result, operand1).unwrap();
            ra.asm.mulps(result, xmmword_ptr(half)).unwrap();
        } else {
            ra.asm.mulpd(operand1, operand2).unwrap();
            ra.asm.subpd(result, operand1).unwrap();
            ra.asm.mulpd(result, xmmword_ptr(half)).unwrap();
        }
        ra.define_value(inst_ref, result);
        return;
    }

    emit_three_op_fallback(ctx, ra, inst_ref, inst, fallback_function);
}

pub fn emit_fp_vector_rsqrt_step_fused16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_vector_rsqrt_step_fused(
        ctx,
        ra,
        inst_ref,
        inst,
        16,
        fallback_fp_rsqrt_step16 as usize,
    );
}
pub fn emit_fp_vector_rsqrt_step_fused32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_vector_rsqrt_step_fused(
        ctx,
        ra,
        inst_ref,
        inst,
        32,
        fallback_fp_rsqrt_step32 as usize,
    );
}
pub fn emit_fp_vector_rsqrt_step_fused64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_vector_rsqrt_step_fused(
        ctx,
        ra,
        inst_ref,
        inst,
        64,
        fallback_fp_rsqrt_step64 as usize,
    );
}

// ---------------------------------------------------------------------------
// FPVectorRoundInt
// ---------------------------------------------------------------------------

fn rounding_mode(rounding: u8) -> RoundingMode {
    match rounding {
        0 => RoundingMode::ToNearestTieEven,
        1 => RoundingMode::TowardsPlusInfinity,
        2 => RoundingMode::TowardsMinusInfinity,
        3 => RoundingMode::TowardsZero,
        4 => RoundingMode::ToNearestTieAwayFromZero,
        _ => unreachable!("invalid FP rounding mode {rounding}"),
    }
}

extern "C" fn fallback_fp_round_int16<const ROUNDING: u8, const EXACT: bool>(
    result: *mut [u8; 16],
    a: *const [u8; 16],
    fpcr: u32,
    fpsr_exc: *mut u32,
) {
    unsafe {
        let va: [u16; 8] = std::mem::transmute(*a);
        let mut out = [0u16; 8];
        let fpcr = Fpcr::new(fpcr);
        let mut fpsr = Fpsr::new(fpsr_exc.read());
        for i in 0..8 {
            out[i] = fp_round_int(va[i], fpcr, rounding_mode(ROUNDING), EXACT, &mut fpsr);
        }
        fpsr_exc.write(fpsr.value());
        *result = std::mem::transmute(out);
    }
}

extern "C" fn fallback_fp_round_int32<const ROUNDING: u8, const EXACT: bool>(
    result: *mut [u8; 16],
    a: *const [u8; 16],
    fpcr: u32,
    fpsr_exc: *mut u32,
) {
    unsafe {
        let va: [u32; 4] = std::mem::transmute(*a);
        let mut out = [0u32; 4];
        let fpcr = Fpcr::new(fpcr);
        let mut fpsr = Fpsr::new(fpsr_exc.read());
        for i in 0..4 {
            out[i] = fp_round_int(va[i], fpcr, rounding_mode(ROUNDING), EXACT, &mut fpsr);
        }
        fpsr_exc.write(fpsr.value());
        *result = std::mem::transmute(out);
    }
}

extern "C" fn fallback_fp_round_int64<const ROUNDING: u8, const EXACT: bool>(
    result: *mut [u8; 16],
    a: *const [u8; 16],
    fpcr: u32,
    fpsr_exc: *mut u32,
) {
    unsafe {
        let va: [u64; 2] = std::mem::transmute(*a);
        let mut out = [0u64; 2];
        let fpcr = Fpcr::new(fpcr);
        let mut fpsr = Fpsr::new(fpsr_exc.read());
        for i in 0..2 {
            out[i] = fp_round_int(va[i], fpcr, rounding_mode(ROUNDING), EXACT, &mut fpsr);
        }
        fpsr_exc.write(fpsr.value());
        *result = std::mem::transmute(out);
    }
}

macro_rules! round_fallback {
    ($function:ident, $rounding:expr, $exact:expr) => {
        $function::<$rounding, $exact> as usize
    };
}

fn round_fallback_for(esize: usize, rounding: u8, exact: bool) -> usize {
    macro_rules! select_exact {
        ($function:ident, $rounding:expr) => {
            if exact {
                round_fallback!($function, $rounding, true)
            } else {
                round_fallback!($function, $rounding, false)
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

fn emit_fp_vector_round_int(
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
        let round_imm = convert_rounding_mode_to_x64_immediate(rounding_mode(rounding))
            .expect("hardware FP rounding mode") as u8;
        if esize == 32 {
            ra.asm.roundps(result, result, round_imm).unwrap();
        } else {
            ra.asm.roundpd(result, result, round_imm).unwrap();
        }
        ra.define_value(inst_ref, result);
        return;
    }

    let fallback = round_fallback_for(esize, rounding, exact);
    emit_two_op_fallback_with_fpcr_arg(ctx, ra, inst_ref, inst, 3, fallback);
}

pub fn emit_fp_vector_round_int16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_vector_round_int(ctx, ra, inst_ref, inst, 16);
}
pub fn emit_fp_vector_round_int32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_vector_round_int(ctx, ra, inst_ref, inst, 32);
}
pub fn emit_fp_vector_round_int64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_vector_round_int(ctx, ra, inst_ref, inst, 64);
}

// ---------------------------------------------------------------------------
// FPVectorFromSignedFixed / FPVectorFromUnsignedFixed — fallback (with imm = frac bits)
// ---------------------------------------------------------------------------

extern "C" fn fallback_fp_from_signed_fixed32(
    result: *mut [u8; 16],
    a: *const [u8; 16],
    fbits: u8,
) {
    unsafe {
        let va: [i32; 4] = std::mem::transmute(*a);
        let scale = (1u64 << fbits) as f32;
        let out: [f32; 4] = [
            va[0] as f32 / scale,
            va[1] as f32 / scale,
            va[2] as f32 / scale,
            va[3] as f32 / scale,
        ];
        *result = std::mem::transmute(out);
    }
}

extern "C" fn fallback_fp_from_signed_fixed64(
    result: *mut [u8; 16],
    a: *const [u8; 16],
    fbits: u8,
) {
    unsafe {
        let va: [i64; 2] = std::mem::transmute(*a);
        let scale = (1u64 << fbits) as f64;
        let out: [f64; 2] = [va[0] as f64 / scale, va[1] as f64 / scale];
        *result = std::mem::transmute(out);
    }
}

extern "C" fn fallback_fp_from_unsigned_fixed32(
    result: *mut [u8; 16],
    a: *const [u8; 16],
    fbits: u8,
) {
    unsafe {
        let va: [u32; 4] = std::mem::transmute(*a);
        let scale = (1u64 << fbits) as f32;
        let out: [f32; 4] = [
            va[0] as f32 / scale,
            va[1] as f32 / scale,
            va[2] as f32 / scale,
            va[3] as f32 / scale,
        ];
        *result = std::mem::transmute(out);
    }
}

extern "C" fn fallback_fp_from_unsigned_fixed64(
    result: *mut [u8; 16],
    a: *const [u8; 16],
    fbits: u8,
) {
    unsafe {
        let va: [u64; 2] = std::mem::transmute(*a);
        let scale = (1u64 << fbits) as f64;
        let out: [f64; 2] = [va[0] as f64 / scale, va[1] as f64 / scale];
        *result = std::mem::transmute(out);
    }
}

pub fn emit_fp_vector_from_signed_fixed32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_one_arg_fallback_with_imm(ra, inst_ref, inst, fallback_fp_from_signed_fixed32 as usize);
}
pub fn emit_fp_vector_from_signed_fixed64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_one_arg_fallback_with_imm(ra, inst_ref, inst, fallback_fp_from_signed_fixed64 as usize);
}
pub fn emit_fp_vector_from_unsigned_fixed32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_one_arg_fallback_with_imm(
        ra,
        inst_ref,
        inst,
        fallback_fp_from_unsigned_fixed32 as usize,
    );
}
pub fn emit_fp_vector_from_unsigned_fixed64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_one_arg_fallback_with_imm(
        ra,
        inst_ref,
        inst,
        fallback_fp_from_unsigned_fixed64 as usize,
    );
}

// ---------------------------------------------------------------------------
// FPVectorToSignedFixed / FPVectorToUnsignedFixed — fallback (with imm = frac bits)
// ---------------------------------------------------------------------------

macro_rules! define_fp_vector_to_fixed_fallback {
    ($name:ident, $type:ty, $count:expr, $bits:expr, $unsigned:expr) => {
        extern "C" fn $name(
            result: *mut [u8; 16],
            a: *const [u8; 16],
            parameters: u64,
            fpcr: u32,
            fpsr_exc: *mut u32,
        ) {
            unsafe {
                let input: [$type; $count] = std::mem::transmute(*a);
                let mut output = [0 as $type; $count];
                let fbits = parameters as u8 as usize;
                let rounding = rounding_mode((parameters >> 8) as u8);
                let fpcr = Fpcr::new(fpcr);
                let mut fpsr = Fpsr::new(fpsr_exc.read());
                for (output, input) in output.iter_mut().zip(input) {
                    *output = fp_to_fixed($bits, input, fbits, $unsigned, fpcr, rounding, &mut fpsr)
                        as $type;
                }
                fpsr_exc.write(fpsr.value());
                *result = std::mem::transmute(output);
            }
        }
    };
}

define_fp_vector_to_fixed_fallback!(fallback_fp_to_signed_fixed16, u16, 8, 16, false);
define_fp_vector_to_fixed_fallback!(fallback_fp_to_signed_fixed32, u32, 4, 32, false);
define_fp_vector_to_fixed_fallback!(fallback_fp_to_signed_fixed64, u64, 2, 64, false);
define_fp_vector_to_fixed_fallback!(fallback_fp_to_unsigned_fixed16, u16, 8, 16, true);
define_fp_vector_to_fixed_fallback!(fallback_fp_to_unsigned_fixed32, u32, 4, 32, true);
define_fp_vector_to_fixed_fallback!(fallback_fp_to_unsigned_fixed64, u64, 2, 64, true);

fn vector_constant(ra: &mut RegAlloc, esize: usize, value: u64) -> RegExp {
    let (lo, hi) = match esize {
        32 => {
            let lanes = value as u32 as u64 * 0x0000_0001_0000_0001;
            (lanes, lanes)
        }
        64 => (value, value),
        _ => unreachable!("invalid FP element size {esize}"),
    };
    ra.constant_pool
        .as_mut()
        .expect("constant pool required for FP vector conversion")
        .get_constant(lo, hi)
}

fn convert_vector_to_signed_host(ra: &mut RegAlloc, src: rxbyak::Reg, esize: usize) {
    match esize {
        32 => ra.asm.cvttps2dq(src, src).unwrap(),
        64 => {
            let hi = ra.scratch_gpr();
            let lo = ra.scratch_gpr();
            ra.asm.cvttsd2si(lo, src).unwrap();
            ra.asm.punpckhqdq(src, src).unwrap();
            ra.asm.cvttsd2si(hi, src).unwrap();
            ra.asm.movq(src, lo).unwrap();
            ra.asm.pinsrq(src, hi, 1).unwrap();
            ra.release(hi);
            ra.release(lo);
        }
        _ => unreachable!("invalid FP element size {esize}"),
    }
}

fn emit_fp_vector_to_fixed_native(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    esize: usize,
    unsigned: bool,
) -> bool {
    let fbits = inst.args[1].get_u8();
    let rounding = inst.args[2].get_u8();
    if esize == 16 || !ctx.has_host_feature(HostFeature::SSE41) || rounding == 4 {
        return false;
    }

    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let fpcr_controlled = args[3].get_immediate_u1();
    let src = ra.use_scratch_xmm(&mut args[0]);
    let switch_mxcsr = ctx.fpcr(fpcr_controlled) != ctx.fpcr(true);
    if switch_mxcsr {
        ra.asm
            .stmxcsr(dword_ptr(
                RegExp::from(R15) + ctx.jit_state_info.offsetof_guest_mxcsr as i32,
            ))
            .unwrap();
        ra.asm
            .ldmxcsr(dword_ptr(
                RegExp::from(R15) + ctx.jit_state_info.offsetof_asimd_mxcsr as i32,
            ))
            .unwrap();
    }

    if fbits != 0 {
        let exponent = match esize {
            32 => (u64::from(fbits) + 127) << 23,
            64 => (u64::from(fbits) + 1023) << 52,
            _ => unreachable!(),
        };
        let scale = vector_constant(ra, esize, exponent);
        if esize == 32 {
            ra.asm.mulps(src, xmmword_ptr(scale)).unwrap();
        } else {
            ra.asm.mulpd(src, xmmword_ptr(scale)).unwrap();
        }
    }

    let round_imm = convert_rounding_mode_to_x64_immediate(rounding_mode(rounding))
        .expect("hardware FP rounding mode") as u8;
    if esize == 32 {
        ra.asm.roundps(src, src, round_imm).unwrap();
        ra.asm.movaps(XMM0, src).unwrap();
        ra.asm.cmpps(XMM0, XMM0, cmp::ORDERED_Q).unwrap();
    } else {
        ra.asm.roundpd(src, src, round_imm).unwrap();
        ra.asm.movaps(XMM0, src).unwrap();
        ra.asm.cmppd(XMM0, XMM0, cmp::ORDERED_Q).unwrap();
    }
    ra.asm.andps(src, XMM0).unwrap();

    let signed_upper = match esize {
        32 => 0x4f00_0000,
        64 => 0x43e0_0000_0000_0000,
        _ => unreachable!(),
    };

    if unsigned {
        let unsigned_upper = match esize {
            32 => 0x4f80_0000,
            64 => 0x43f0_0000_0000_0000,
            _ => unreachable!(),
        };

        ra.asm.xorps(XMM0, XMM0).unwrap();
        if esize == 32 {
            ra.asm.cmpps(XMM0, src, cmp::LESS_EQUAL_OS).unwrap();
        } else {
            ra.asm.cmppd(XMM0, src, cmp::LESS_EQUAL_OS).unwrap();
        }
        ra.asm.andps(src, XMM0).unwrap();

        let exceed_unsigned = ra.scratch_xmm();
        let unsigned_limit = vector_constant(ra, esize, unsigned_upper);
        ra.asm
            .movaps(exceed_unsigned, xmmword_ptr(unsigned_limit))
            .unwrap();
        if esize == 32 {
            ra.asm
                .cmpps(exceed_unsigned, src, cmp::LESS_EQUAL_OS)
                .unwrap();
        } else {
            ra.asm
                .cmppd(exceed_unsigned, src, cmp::LESS_EQUAL_OS)
                .unwrap();
        }

        let tmp = ra.scratch_xmm();
        let signed_limit = vector_constant(ra, esize, signed_upper);
        ra.asm.movaps(tmp, xmmword_ptr(signed_limit)).unwrap();
        ra.asm.movaps(XMM0, tmp).unwrap();
        if esize == 32 {
            ra.asm.cmpps(XMM0, src, cmp::LESS_EQUAL_OS).unwrap();
            ra.asm.andps(tmp, XMM0).unwrap();
            ra.asm.subps(src, tmp).unwrap();
        } else {
            ra.asm.cmppd(XMM0, src, cmp::LESS_EQUAL_OS).unwrap();
            ra.asm.andpd(tmp, XMM0).unwrap();
            ra.asm.subpd(src, tmp).unwrap();
        }
        convert_vector_to_signed_host(ra, src, esize);
        if esize == 32 {
            ra.asm.pslld_imm(XMM0, 31).unwrap();
            ra.asm.orps(src, XMM0).unwrap();
            ra.asm.orps(src, exceed_unsigned).unwrap();
        } else {
            ra.asm.psllq_imm(XMM0, 63).unwrap();
            ra.asm.orpd(src, XMM0).unwrap();
            ra.asm.orpd(src, exceed_unsigned).unwrap();
        }
        ra.release(tmp);
        ra.release(exceed_unsigned);
    } else {
        let signed_limit = vector_constant(ra, esize, signed_upper);
        ra.asm.movaps(XMM0, xmmword_ptr(signed_limit)).unwrap();
        if esize == 32 {
            ra.asm.cmpps(XMM0, src, cmp::LESS_EQUAL_OS).unwrap();
        } else {
            ra.asm.cmppd(XMM0, src, cmp::LESS_EQUAL_OS).unwrap();
        }
        convert_vector_to_signed_host(ra, src, esize);

        let integer_max = match esize {
            32 => i32::MAX as u64,
            64 => i64::MAX as u64,
            _ => unreachable!(),
        };
        let maximum = vector_constant(ra, esize, integer_max);
        if esize == 32 {
            ra.asm.blendvps(src, xmmword_ptr(maximum)).unwrap();
        } else {
            ra.asm.blendvpd(src, xmmword_ptr(maximum)).unwrap();
        }
    }

    if switch_mxcsr {
        ra.asm
            .stmxcsr(dword_ptr(
                RegExp::from(R15) + ctx.jit_state_info.offsetof_asimd_mxcsr as i32,
            ))
            .unwrap();
        ra.asm
            .ldmxcsr(dword_ptr(
                RegExp::from(R15) + ctx.jit_state_info.offsetof_guest_mxcsr as i32,
            ))
            .unwrap();
    }
    ra.define_value(inst_ref, src);
    true
}

pub fn emit_fp_vector_to_signed_fixed16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    if emit_fp_vector_to_fixed_native(ctx, ra, inst_ref, inst, 16, false) {
        return;
    }
    emit_fp_one_arg_fallback_with_params(
        ctx,
        ra,
        inst_ref,
        inst,
        fallback_fp_to_signed_fixed16 as usize,
    );
}
pub fn emit_fp_vector_to_signed_fixed32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    if emit_fp_vector_to_fixed_native(ctx, ra, inst_ref, inst, 32, false) {
        return;
    }
    emit_fp_one_arg_fallback_with_params(
        ctx,
        ra,
        inst_ref,
        inst,
        fallback_fp_to_signed_fixed32 as usize,
    );
}
pub fn emit_fp_vector_to_signed_fixed64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    if emit_fp_vector_to_fixed_native(ctx, ra, inst_ref, inst, 64, false) {
        return;
    }
    emit_fp_one_arg_fallback_with_params(
        ctx,
        ra,
        inst_ref,
        inst,
        fallback_fp_to_signed_fixed64 as usize,
    );
}
pub fn emit_fp_vector_to_unsigned_fixed16(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    if emit_fp_vector_to_fixed_native(ctx, ra, inst_ref, inst, 16, true) {
        return;
    }
    emit_fp_one_arg_fallback_with_params(
        ctx,
        ra,
        inst_ref,
        inst,
        fallback_fp_to_unsigned_fixed16 as usize,
    );
}
pub fn emit_fp_vector_to_unsigned_fixed32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    if emit_fp_vector_to_fixed_native(ctx, ra, inst_ref, inst, 32, true) {
        return;
    }
    emit_fp_one_arg_fallback_with_params(
        ctx,
        ra,
        inst_ref,
        inst,
        fallback_fp_to_unsigned_fixed32 as usize,
    );
}
pub fn emit_fp_vector_to_unsigned_fixed64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    if emit_fp_vector_to_fixed_native(ctx, ra, inst_ref, inst, 64, true) {
        return;
    }
    emit_fp_one_arg_fallback_with_params(
        ctx,
        ra,
        inst_ref,
        inst,
        fallback_fp_to_unsigned_fixed64 as usize,
    );
}

// ---------------------------------------------------------------------------
// FPVectorFromHalf32 / FPVectorToHalf32
// ---------------------------------------------------------------------------

macro_rules! define_fp_vector_half_convert_fallback {
    ($from_name:ident, $to_name:ident, $rounding:expr) => {
        extern "C" fn $from_name(
            result: *mut [u8; 16],
            a: *const [u8; 16],
            fpcr: u32,
            fpsr_exc: *mut u32,
        ) {
            unsafe {
                let input: [u16; 8] = std::mem::transmute(*a);
                let fpcr = Fpcr::new(fpcr);
                let mut fpsr = Fpsr::new(fpsr_exc.read());
                let output = [
                    fp_convert::<u32, u16>(input[0], fpcr, $rounding, &mut fpsr),
                    fp_convert::<u32, u16>(input[1], fpcr, $rounding, &mut fpsr),
                    fp_convert::<u32, u16>(input[2], fpcr, $rounding, &mut fpsr),
                    fp_convert::<u32, u16>(input[3], fpcr, $rounding, &mut fpsr),
                ];
                fpsr_exc.write(fpsr.value());
                *result = std::mem::transmute(output);
            }
        }

        extern "C" fn $to_name(
            result: *mut [u8; 16],
            a: *const [u8; 16],
            fpcr: u32,
            fpsr_exc: *mut u32,
        ) {
            unsafe {
                let input: [u32; 4] = std::mem::transmute(*a);
                let fpcr = Fpcr::new(fpcr);
                let mut fpsr = Fpsr::new(fpsr_exc.read());
                let output: [u16; 8] = [
                    fp_convert::<u16, u32>(input[0], fpcr, $rounding, &mut fpsr),
                    fp_convert::<u16, u32>(input[1], fpcr, $rounding, &mut fpsr),
                    fp_convert::<u16, u32>(input[2], fpcr, $rounding, &mut fpsr),
                    fp_convert::<u16, u32>(input[3], fpcr, $rounding, &mut fpsr),
                    0,
                    0,
                    0,
                    0,
                ];
                fpsr_exc.write(fpsr.value());
                *result = std::mem::transmute(output);
            }
        }
    };
}

define_fp_vector_half_convert_fallback!(
    fallback_fp_from_half32_nearest,
    fallback_fp_to_half32_nearest,
    RoundingMode::ToNearestTieEven
);
define_fp_vector_half_convert_fallback!(
    fallback_fp_from_half32_plus,
    fallback_fp_to_half32_plus,
    RoundingMode::TowardsPlusInfinity
);
define_fp_vector_half_convert_fallback!(
    fallback_fp_from_half32_minus,
    fallback_fp_to_half32_minus,
    RoundingMode::TowardsMinusInfinity
);
define_fp_vector_half_convert_fallback!(
    fallback_fp_from_half32_zero,
    fallback_fp_to_half32_zero,
    RoundingMode::TowardsZero
);
define_fp_vector_half_convert_fallback!(
    fallback_fp_from_half32_away,
    fallback_fp_to_half32_away,
    RoundingMode::ToNearestTieAwayFromZero
);

fn half_conversion_fallback(rounding: u8, to_half: bool) -> usize {
    match (rounding, to_half) {
        (0, false) => fallback_fp_from_half32_nearest as usize,
        (1, false) => fallback_fp_from_half32_plus as usize,
        (2, false) => fallback_fp_from_half32_minus as usize,
        (3, false) => fallback_fp_from_half32_zero as usize,
        (4, false) => fallback_fp_from_half32_away as usize,
        (0, true) => fallback_fp_to_half32_nearest as usize,
        (1, true) => fallback_fp_to_half32_plus as usize,
        (2, true) => fallback_fp_to_half32_minus as usize,
        (3, true) => fallback_fp_to_half32_zero as usize,
        (4, true) => fallback_fp_to_half32_away as usize,
        _ => unreachable!("invalid FP half conversion rounding mode {rounding}"),
    }
}

pub fn emit_fp_vector_from_half32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let rounding = inst.args[1].get_u8();
    let fpcr_controlled = inst.args[2].get_u1();
    if ctx.has_host_feature(HostFeature::F16C) && !ctx.fpcr(true).ahp() && !ctx.fpcr(true).fz16() {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let result = ra.scratch_xmm();
        let value = ra.use_xmm(&mut args[0]);
        ra.asm.vcvtph2ps(result, value).unwrap();
        if ctx.fpcr(fpcr_controlled).dn() {
            force_to_default_nan_vector(ra, result, 32);
        }
        ra.define_value(inst_ref, result);
        return;
    }
    emit_two_op_fallback_with_fpcr_arg(
        ctx,
        ra,
        inst_ref,
        inst,
        2,
        half_conversion_fallback(rounding, false),
    );
}
pub fn emit_fp_vector_to_half32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    let rounding = inst.args[1].get_u8();
    let fpcr_controlled = inst.args[2].get_u1();
    if ctx.has_host_feature(HostFeature::F16C)
        && rounding <= 3
        && !ctx.fpcr(true).ahp()
        && !ctx.fpcr(true).fz16()
    {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let result = ra.use_scratch_xmm(&mut args[0]);
        if ctx.fpcr(fpcr_controlled).dn() {
            force_to_default_nan_vector(ra, result, 32);
        }
        let round_imm = convert_rounding_mode_to_x64_immediate(rounding_mode(rounding))
            .expect("hardware FP conversion rounding mode") as u8;
        ra.asm.vcvtps2ph(result, result, round_imm).unwrap();
        ra.define_value(inst_ref, result);
        return;
    }
    emit_two_op_fallback_with_fpcr_arg(
        ctx,
        ra,
        inst_ref,
        inst,
        2,
        half_conversion_fallback(rounding, true),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fn_signatures() {
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_muladd16;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_muladd32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_muladd64;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_recip_estimate16;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_recip_estimate32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_recip_estimate64;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_recip_step_fused16;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_recip_step_fused32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_rsqrt_estimate16;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_rsqrt_estimate32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_rsqrt_step_fused16;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_rsqrt_step_fused64;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_round_int16;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_round_int32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_round_int64;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_from_signed_fixed32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) =
            emit_fp_vector_from_unsigned_fixed64;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_to_signed_fixed16;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_to_unsigned_fixed64;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_from_half32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_to_half32;
    }

    #[test]
    fn test_fallback_fp_muladd32() {
        let addend: [u8; 16] = unsafe { std::mem::transmute([1.0f32, 2.0f32, 3.0f32, 4.0f32]) };
        let op1: [u8; 16] = unsafe { std::mem::transmute([2.0f32, 3.0f32, 4.0f32, 5.0f32]) };
        let op2: [u8; 16] = unsafe { std::mem::transmute([3.0f32, 4.0f32, 5.0f32, 6.0f32]) };
        let mut result = [0u8; 16];
        let mut fpsr = 0;
        fallback_fp_muladd32(&mut result, &addend, &op1, &op2, 0, &mut fpsr);
        let out: [f32; 4] = unsafe { std::mem::transmute(result) };
        assert_eq!(out[0], 7.0); // 1 + 2*3
        assert_eq!(out[1], 14.0); // 2 + 3*4
        assert_eq!(out[2], 23.0); // 3 + 4*5
        assert_eq!(out[3], 34.0); // 4 + 5*6
        assert_eq!(fpsr, 0);
    }

    #[test]
    fn muladd_correction_only_recomputes_selected_lanes() {
        let original = [
            42.0f32.to_bits(),
            0x0080_0000,
            0x7fc1_2345,
            (-7.0f32).to_bits(),
        ];
        let mut result: [u8; 16] = unsafe { std::mem::transmute(original) };
        let addend: [u8; 16] = unsafe {
            std::mem::transmute([
                100.0f32.to_bits(),
                0x0080_0000,
                0x7fc5_4321,
                100.0f32.to_bits(),
            ])
        };
        let op1: [u8; 16] = unsafe { std::mem::transmute([2.0f32.to_bits(); 4]) };
        let op2: [u8; 16] = unsafe { std::mem::transmute([3.0f32.to_bits(); 4]) };
        let mut fpsr = 0;

        fallback_fp_muladd_correction32(&mut result, &addend, &op1, &op2, 1 << 24, &mut fpsr);

        let corrected: [u32; 4] = unsafe { std::mem::transmute(result) };
        assert_eq!(corrected[0], original[0]);
        assert_eq!(corrected[3], original[3]);
        assert_eq!(corrected[2], 0x7fc5_4321);

        let mut inaccurate_result: [u8; 16] = unsafe { std::mem::transmute(original) };
        fallback_fp_muladd_correction32_inaccurate_nan(
            &mut inaccurate_result,
            &addend,
            &op1,
            &op2,
            1 << 24,
            &mut fpsr,
        );
        let inaccurate: [u32; 4] = unsafe { std::mem::transmute(inaccurate_result) };
        assert_eq!(inaccurate[2], original[2]);
    }

    #[test]
    fn muladd_fallback_applies_default_nan_mode() {
        let addend = [0u8; 16];
        let op1: [u8; 16] = unsafe { std::mem::transmute([f32::INFINITY.to_bits(), 0, 0, 0]) };
        let op2 = [0u8; 16];
        let mut result = [0u8; 16];
        let mut fpsr = 0;
        fallback_fp_muladd32(&mut result, &addend, &op1, &op2, 1 << 25, &mut fpsr);
        let out: [u32; 4] = unsafe { std::mem::transmute(result) };
        assert_eq!(out[0], 0x7fc0_0000);
        assert_ne!(fpsr & 1, 0);
    }

    #[test]
    fn muladd_fallback_matches_native_input_denormal_exception_behavior() {
        if !host_supports_fma_avx() {
            return;
        }

        let addend: [u8; 16] = unsafe { std::mem::transmute([1.0f32.to_bits(); 4]) };
        let op1: [u8; 16] = unsafe { std::mem::transmute([1u32; 4]) };
        let op2: [u8; 16] = unsafe { std::mem::transmute([1.0f32.to_bits(); 4]) };
        let mut result = [0u8; 16];
        let mut fpsr = 0;
        fallback_fp_muladd32(&mut result, &addend, &op1, &op2, 1 << 24, &mut fpsr);
        assert_eq!(fpsr & (1 << 7), 0);

        fpsr = 1 << 7;
        fallback_fp_muladd32(&mut result, &addend, &op1, &op2, 1 << 24, &mut fpsr);
        assert_ne!(fpsr & (1 << 7), 0);
    }

    #[test]
    fn fp_estimate_fallbacks_match_arm_values_and_accumulate_fpsr() {
        let input: [u8; 16] = unsafe {
            std::mem::transmute([
                1.0f32.to_bits(),
                0.0f32.to_bits(),
                f32::INFINITY.to_bits(),
                0x7f80_0001u32,
            ])
        };
        let mut result = [0u8; 16];
        let mut fpsr_exc = 0;

        fallback_fp_recip_est32(&mut result, &input, 0, &mut fpsr_exc);

        let output: [u32; 4] = unsafe { std::mem::transmute(result) };
        assert_eq!(
            output,
            [0x3f7f_8000, f32::INFINITY.to_bits(), 0, 0x7fc0_0001]
        );
        assert_eq!(fpsr_exc, (1 << 1) | 1);
    }

    #[test]
    fn rsqrt_estimate_matches_native_vector_inexact_behavior() {
        if !host_supports_avx() {
            return;
        }

        let normal: [u8; 16] = unsafe { std::mem::transmute([1.0f32, 2.0, 3.0, 4.0]) };
        let mut result = [0u8; 16];
        let mut fpsr = 0;
        fallback_fp_rsqrt_est32(&mut result, &normal, 0, &mut fpsr);
        assert_ne!(fpsr & (1 << 4), 0);

        let with_zero: [u8; 16] = unsafe { std::mem::transmute([1.0f32, 2.0, 3.0, 0.0]) };
        fpsr = 0;
        fallback_fp_rsqrt_est32(&mut result, &with_zero, 0, &mut fpsr);
        assert_eq!(fpsr & (1 << 4), 0);
        assert_ne!(fpsr & (1 << 1), 0);
    }

    #[test]
    fn test_fallback_fp_to_signed_fixed32() {
        let a: [u8; 16] = unsafe { std::mem::transmute([1.5f32, -2.5f32, 0.0f32, 100.0f32]) };
        let mut result = [0u8; 16];
        let mut fpsr = 0;
        fallback_fp_to_signed_fixed32(&mut result, &a, 4 << 8, 0, &mut fpsr);
        let out: [i32; 4] = unsafe { std::mem::transmute(result) };
        assert_eq!(out[0], 2);
        assert_eq!(out[1], -3);
        assert_eq!(out[2], 0);
        assert_eq!(out[3], 100);
        assert_ne!(fpsr & (1 << 4), 0);
    }

    #[test]
    fn vector_to_fixed_fallback_accumulates_fpsr_exceptions() {
        let a: [u8; 16] = unsafe {
            std::mem::transmute([
                f32::NAN.to_bits(),
                1.5f32.to_bits(),
                f32::INFINITY.to_bits(),
                (-1.5f32).to_bits(),
            ])
        };
        let mut result = [0u8; 16];
        let mut fpsr = 0x0e;
        fallback_fp_to_signed_fixed32(&mut result, &a, 3 << 8, 0, &mut fpsr);

        let out: [u32; 4] = unsafe { std::mem::transmute(result) };
        assert_eq!(out, [0, 1, i32::MAX as u32, (-1i32) as u32]);
        assert_eq!(fpsr, 0x1f);
    }
}
