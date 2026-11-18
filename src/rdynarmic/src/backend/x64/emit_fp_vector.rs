#![allow(
    clippy::missing_transmute_annotations,
    clippy::useless_transmute,
    unnecessary_transmutes
)]

use rxbyak::XMM0;

use crate::backend::x64::constants::{cmp, fixup_lut, FpFixup};
use crate::backend::x64::emit_context::EmitContext;
use crate::backend::x64::emit_floating_point::fp_min_max;
use crate::backend::x64::emit_fp_vector_convert::maybe_standard_fpscr_value;
use crate::backend::x64::emit_vector_helpers::*;
use crate::backend::x64::host_feature::HostFeature;
use crate::backend::x64::reg_alloc::RegAlloc;
use crate::common::fp::fpcr::Fpcr;
use crate::common::fp::fpsr::Fpsr;
use crate::common::fp::info::FloatFormat;
use crate::common::fp::rounding_mode::RoundingMode;
use crate::ir::inst::Inst;
use crate::ir::value::InstRef;

// ---------------------------------------------------------------------------
// FPVectorAdd/Sub/Mul/Div — native SSE with upstream default-NaN handling
// ---------------------------------------------------------------------------

pub(crate) fn force_to_default_nan_vector(ra: &mut RegAlloc, result: rxbyak::Reg, esize: usize) {
    let nan_mask = ra.scratch_xmm();
    ra.asm.movaps(nan_mask, result).unwrap();
    match esize {
        32 => ra.asm.cmpps(nan_mask, nan_mask, cmp::ORDERED_Q).unwrap(),
        64 => ra.asm.cmppd(nan_mask, nan_mask, cmp::ORDERED_Q).unwrap(),
        _ => unreachable!(),
    }
    ra.asm.andps(result, nan_mask).unwrap();

    let (lo, hi) = match esize {
        32 => (0x7fc0_0000_7fc0_0000, 0x7fc0_0000_7fc0_0000),
        64 => (0x7ff8_0000_0000_0000, 0x7ff8_0000_0000_0000),
        _ => unreachable!(),
    };
    let default_nan = ra
        .constant_pool
        .as_mut()
        .expect("constant pool required for FP default NaN")
        .get_constant(lo, hi);
    ra.asm
        .andnps(nan_mask, rxbyak::xmmword_ptr(default_nan))
        .unwrap();
    ra.asm.orps(result, nan_mask).unwrap();
    ra.release(nan_mask);
}

fn emit_fp_vector_binary(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    esize: usize,
    op: fn(&mut rxbyak::CodeAssembler, rxbyak::Reg, rxbyak::Reg) -> rxbyak::Result<()>,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let fpcr_controlled = args[2].get_immediate_u1();
    let result = ra.use_scratch_xmm(&mut args[0]);
    let operand = ra.use_xmm(&mut args[1]);
    op(&mut *ra.asm, result, operand).unwrap();
    if ctx.fpcr(fpcr_controlled).dn() {
        force_to_default_nan_vector(ra, result, esize);
    }
    ra.define_value(inst_ref, result);
}

pub fn emit_fp_vector_add32(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_vector_binary(ctx, ra, inst_ref, inst, 32, rxbyak::CodeAssembler::addps);
}
pub fn emit_fp_vector_add64(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_vector_binary(ctx, ra, inst_ref, inst, 64, rxbyak::CodeAssembler::addpd);
}

// ---------------------------------------------------------------------------
// FPVectorSub — native SSE: subps/subpd
// ---------------------------------------------------------------------------

pub fn emit_fp_vector_sub32(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_vector_binary(ctx, ra, inst_ref, inst, 32, rxbyak::CodeAssembler::subps);
}
pub fn emit_fp_vector_sub64(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_vector_binary(ctx, ra, inst_ref, inst, 64, rxbyak::CodeAssembler::subpd);
}

// ---------------------------------------------------------------------------
// FPVectorMul — native SSE: mulps/mulpd
// ---------------------------------------------------------------------------

pub fn emit_fp_vector_mul32(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_vector_binary(ctx, ra, inst_ref, inst, 32, rxbyak::CodeAssembler::mulps);
}
pub fn emit_fp_vector_mul64(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_vector_binary(ctx, ra, inst_ref, inst, 64, rxbyak::CodeAssembler::mulpd);
}

// ---------------------------------------------------------------------------
// FPVectorDiv — native SSE: divps/divpd
// ---------------------------------------------------------------------------

pub fn emit_fp_vector_div32(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_vector_binary(ctx, ra, inst_ref, inst, 32, rxbyak::CodeAssembler::divps);
}
pub fn emit_fp_vector_div64(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_vector_binary(ctx, ra, inst_ref, inst, 64, rxbyak::CodeAssembler::divpd);
}

// ---------------------------------------------------------------------------
// FPVectorSqrt — native SSE: sqrtps/sqrtpd (unary)
// ---------------------------------------------------------------------------

pub fn emit_fp_vector_sqrt32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_unary_op(ra, inst_ref, inst, rxbyak::CodeAssembler::sqrtps);
}
pub fn emit_fp_vector_sqrt64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_vector_unary_op(ra, inst_ref, inst, rxbyak::CodeAssembler::sqrtpd);
}

// ---------------------------------------------------------------------------
// FPVectorAbs — andps with non-sign-bit mask from constant pool
// Upstream: andps(a, GetNonSignMaskVector<fsize>(code))
// ---------------------------------------------------------------------------

fn emit_fp_vector_abs(
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    mask_lo: u64,
    mask_hi: u64,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let pool = ra
        .constant_pool
        .as_mut()
        .expect("constant pool required for FPVectorAbs");
    let mask_addr = pool.get_constant(mask_lo, mask_hi);
    ra.asm
        .andps(result, rxbyak::xmmword_ptr(mask_addr))
        .unwrap();
    ra.define_value(inst_ref, result);
}

pub fn emit_fp_vector_abs16(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    // 16-bit FP: non-sign mask = 0x7FFF per half-word
    emit_fp_vector_abs(
        ra,
        inst_ref,
        inst,
        0x7FFF_7FFF_7FFF_7FFF,
        0x7FFF_7FFF_7FFF_7FFF,
    );
}

pub fn emit_fp_vector_abs32(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    // 32-bit FP: non-sign mask = 0x7FFFFFFF per dword
    emit_fp_vector_abs(
        ra,
        inst_ref,
        inst,
        0x7FFF_FFFF_7FFF_FFFF,
        0x7FFF_FFFF_7FFF_FFFF,
    );
}

pub fn emit_fp_vector_abs64(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    // 64-bit FP: non-sign mask = 0x7FFFFFFFFFFFFFFF per qword
    emit_fp_vector_abs(
        ra,
        inst_ref,
        inst,
        0x7FFF_FFFF_FFFF_FFFF,
        0x7FFF_FFFF_FFFF_FFFF,
    );
}

// ---------------------------------------------------------------------------
// FPVectorNeg — xorps with sign-bit mask from constant pool
// Upstream: xorps(a, GetSignMaskVector<fsize>(code))
// ---------------------------------------------------------------------------

fn emit_fp_vector_neg(
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    mask_lo: u64,
    mask_hi: u64,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let result = ra.use_scratch_xmm(&mut args[0]);
    let pool = ra
        .constant_pool
        .as_mut()
        .expect("constant pool required for FPVectorNeg");
    let mask_addr = pool.get_constant(mask_lo, mask_hi);
    ra.asm
        .xorps(result, rxbyak::xmmword_ptr(mask_addr))
        .unwrap();
    ra.define_value(inst_ref, result);
}

pub fn emit_fp_vector_neg16(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    // 16-bit FP: sign mask = 0x8000 per half-word
    emit_fp_vector_neg(
        ra,
        inst_ref,
        inst,
        0x8000_8000_8000_8000,
        0x8000_8000_8000_8000,
    );
}

pub fn emit_fp_vector_neg32(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    // 32-bit FP: sign mask = 0x80000000 per dword
    emit_fp_vector_neg(
        ra,
        inst_ref,
        inst,
        0x8000_0000_8000_0000,
        0x8000_0000_8000_0000,
    );
}

pub fn emit_fp_vector_neg64(_ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    // 64-bit FP: sign mask = 0x8000000000000000 per qword
    emit_fp_vector_neg(
        ra,
        inst_ref,
        inst,
        0x8000_0000_0000_0000,
        0x8000_0000_0000_0000,
    );
}

// ---------------------------------------------------------------------------
// FPVectorMax/Min and FPVectorMaxNumeric/MinNumeric
// ---------------------------------------------------------------------------

macro_rules! define_fp_vector_min_max_fallback {
    ($name:ident, $type:ty, $count:expr, $is_max:expr, $numeric:expr) => {
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
                for i in 0..$count {
                    let is_nan = |value: $type| {
                        let bits = <$type as FloatFormat>::to_bits(value);
                        bits & <$type as FloatFormat>::EXPONENT_MASK
                            == <$type as FloatFormat>::EXPONENT_MASK
                            && bits & <$type as FloatFormat>::MANTISSA_MASK != 0
                    };
                    if is_nan(va[i]) || is_nan(vb[i]) {
                        // Upstream's SSE vector min/max sequence records an
                        // invalid operation for both quiet and signaling NaNs.
                        fpsr.set_ioc(true);
                    }
                    out[i] = fp_min_max(va[i], vb[i], fpcr, &mut fpsr, $is_max, $numeric);
                }
                fpsr_exc.write(fpsr.value());
                *result = std::mem::transmute(out);
            }
        }
    };
}

define_fp_vector_min_max_fallback!(fallback_fp_max32, u32, 4, true, false);
define_fp_vector_min_max_fallback!(fallback_fp_max64, u64, 2, true, false);
define_fp_vector_min_max_fallback!(fallback_fp_min32, u32, 4, false, false);
define_fp_vector_min_max_fallback!(fallback_fp_min64, u64, 2, false, false);
define_fp_vector_min_max_fallback!(fallback_fp_maxnm32, u32, 4, true, true);
define_fp_vector_min_max_fallback!(fallback_fp_maxnm64, u64, 2, true, true);
define_fp_vector_min_max_fallback!(fallback_fp_minnm32, u32, 4, false, true);
define_fp_vector_min_max_fallback!(fallback_fp_minnm64, u64, 2, false, true);

fn emit_fp_vector_min_max(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    fallback: usize,
) {
    emit_three_op_fallback(ctx, ra, inst_ref, inst, fallback);
}

pub fn emit_fp_vector_max32(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_vector_min_max(
        ctx,
        ra,
        inst_ref,
        inst,
        fallback_fp_max32 as *const () as usize,
    );
}
pub fn emit_fp_vector_max64(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_vector_min_max(
        ctx,
        ra,
        inst_ref,
        inst,
        fallback_fp_max64 as *const () as usize,
    );
}
pub fn emit_fp_vector_min32(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_vector_min_max(
        ctx,
        ra,
        inst_ref,
        inst,
        fallback_fp_min32 as *const () as usize,
    );
}
pub fn emit_fp_vector_min64(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_vector_min_max(
        ctx,
        ra,
        inst_ref,
        inst,
        fallback_fp_min64 as *const () as usize,
    );
}

pub fn emit_fp_vector_max_numeric32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_vector_min_max(
        ctx,
        ra,
        inst_ref,
        inst,
        fallback_fp_maxnm32 as *const () as usize,
    );
}
pub fn emit_fp_vector_max_numeric64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_vector_min_max(
        ctx,
        ra,
        inst_ref,
        inst,
        fallback_fp_maxnm64 as *const () as usize,
    );
}

pub fn emit_fp_vector_min_numeric32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_vector_min_max(
        ctx,
        ra,
        inst_ref,
        inst,
        fallback_fp_minnm32 as *const () as usize,
    );
}
pub fn emit_fp_vector_min_numeric64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_vector_min_max(
        ctx,
        ra,
        inst_ref,
        inst,
        fallback_fp_minnm64 as *const () as usize,
    );
}

// ---------------------------------------------------------------------------
// Shared helpers of upstream `emit_x64_vector_floating_point.cpp`
// ---------------------------------------------------------------------------

/// Upstream `FCODE(NAME)`: selects the `ps` (fsize 32) or `pd` (fsize 64)
/// form of a packed floating-point instruction.
macro_rules! fcode {
    ($asm:expr, $fsize:expr, $ps:ident, $pd:ident, $($arg:expr),+) => {
        match $fsize {
            32 => $asm.$ps($($arg),+).unwrap(),
            64 => $asm.$pd($($arg),+).unwrap(),
            _ => unreachable!("fsize must be either 32 or 64"),
        }
    };
}
pub(crate) use fcode;

/// Upstream `GetVectorOf<fsize>(code, value)` = `code.BConst<fsize>(xword, value)`:
/// a 128-bit constant with `value` broadcast to every `fsize`-bit lane.
pub(crate) fn get_vector_of(ra: &mut RegAlloc, fsize: usize, value: u64) -> rxbyak::Address {
    let lane = match fsize {
        16 => {
            let v = value & 0xffff;
            v | (v << 16) | (v << 32) | (v << 48)
        }
        32 => {
            let v = value & 0xffff_ffff;
            v | (v << 32)
        }
        64 => value,
        _ => unreachable!("invalid fsize {fsize}"),
    };
    let address = ra
        .constant_pool
        .as_mut()
        .expect("constant pool required for vector FP constants")
        .get_constant(lane, lane);
    rxbyak::xmmword_ptr(address)
}

/// Upstream `GetNegativeZeroVector<fsize>`.
pub(crate) fn get_negative_zero_vector(ra: &mut RegAlloc, fsize: usize) -> rxbyak::Address {
    match fsize {
        32 => get_vector_of(ra, 32, u32::SIGN_MASK as u64),
        64 => get_vector_of(ra, 64, u64::SIGN_MASK),
        _ => unreachable!("fsize must be either 32 or 64"),
    }
}

/// Port of upstream vector `DenormalsAreZero<fsize>`
/// (`emit_x64_vector_floating_point.cpp`, anonymous namespace).
pub(crate) fn denormals_are_zero(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    fsize: usize,
    fpcr: Fpcr,
    to_daz: &[rxbyak::Reg],
    tmp: rxbyak::Reg,
) {
    if fpcr.fz() {
        if ctx.has_host_feature(HostFeature::AVX512_ORTHO_FLOAT) {
            let denormal_to_zero = fixup_lut(
                FpFixup::NormSrc,
                FpFixup::NormSrc,
                FpFixup::NormSrc,
                FpFixup::NormSrc,
                FpFixup::NormSrc,
                FpFixup::NormSrc,
                FpFixup::NormSrc,
                FpFixup::NormSrc,
            );

            let lut = get_vector_of(ra, fsize, denormal_to_zero as u64);
            fcode!(ra.asm, fsize, vmovaps, vmovapd, tmp, lut);

            for &xmm in to_daz {
                fcode!(ra.asm, fsize, vfixupimmps, vfixupimmpd, xmm, xmm, tmp, 0u8);
            }
            return;
        }

        if fpcr.rmode() != RoundingMode::TowardsMinusInfinity {
            let negative_zero = get_negative_zero_vector(ra, fsize);
            ra.asm.movaps(tmp, negative_zero).unwrap();
        } else {
            ra.asm.xorps(tmp, tmp).unwrap();
        }
        for &xmm in to_daz {
            fcode!(ra.asm, fsize, addps, addpd, xmm, tmp);
        }
    }
}

// ---------------------------------------------------------------------------
// FPVectorEqual
// ---------------------------------------------------------------------------

// FPVectorEqual16 — fp16 compare
extern "C" fn fallback_fp_vector_equal16(
    result: *mut [u8; 16],
    a: *const [u8; 16],
    b: *const [u8; 16],
) {
    unsafe {
        let va: [u16; 8] = std::mem::transmute(*a);
        let vb: [u16; 8] = std::mem::transmute(*b);
        let mut out = [0u16; 8];
        for i in 0..8 {
            // Simple bit equality for fp16 (matching dynarmic behavior)
            out[i] = if va[i] == vb[i] { !0 } else { 0 };
        }
        *result = std::mem::transmute(out);
    }
}

pub fn emit_fp_vector_equal16(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_fp_vector_equal16 as *const () as usize,
    );
}
/// Shared body of upstream `EmitFPVectorEqual32/64`.
fn emit_fp_vector_equal(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    fsize: usize,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let fpcr_controlled = args[2].get_immediate_u1();
    let a = ra.use_scratch_xmm(&mut args[0]);
    let b = if ctx.fpcr(fpcr_controlled).fz() {
        ra.use_scratch_xmm(&mut args[1])
    } else {
        ra.use_xmm(&mut args[1])
    };

    maybe_standard_fpscr_value(ctx, ra, fpcr_controlled, |ra| {
        denormals_are_zero(ctx, ra, fsize, ctx.fpcr(fpcr_controlled), &[a, b], XMM0);
        fcode!(ra.asm, fsize, cmpps, cmppd, a, b, cmp::EQUAL_OQ);
    });

    ra.define_value(inst_ref, a);
}

pub fn emit_fp_vector_equal32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_vector_equal(ctx, ra, inst_ref, inst, 32);
}
pub fn emit_fp_vector_equal64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_vector_equal(ctx, ra, inst_ref, inst, 64);
}

// ---------------------------------------------------------------------------
// FPVectorGreater / FPVectorGreaterEqual
// ---------------------------------------------------------------------------

/// Shared body of upstream `EmitFPVectorGreater{,Equal}32/64`:
/// `b = (b < a)` (`cmpltp`) or `b = (b <= a)` (`cmplep`).
fn emit_fp_vector_greater_impl(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    fsize: usize,
    predicate: u8,
) {
    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let fpcr_controlled = args[2].get_immediate_u1();
    let a = if ctx.fpcr(fpcr_controlled).fz() {
        ra.use_scratch_xmm(&mut args[0])
    } else {
        ra.use_xmm(&mut args[0])
    };
    let b = ra.use_scratch_xmm(&mut args[1]);

    maybe_standard_fpscr_value(ctx, ra, fpcr_controlled, |ra| {
        denormals_are_zero(ctx, ra, fsize, ctx.fpcr(fpcr_controlled), &[a, b], XMM0);
        fcode!(ra.asm, fsize, cmpps, cmppd, b, a, predicate);
    });

    ra.define_value(inst_ref, b);
}

pub fn emit_fp_vector_greater32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_vector_greater_impl(ctx, ra, inst_ref, inst, 32, cmp::LESS_THAN_OS);
}
pub fn emit_fp_vector_greater64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_vector_greater_impl(ctx, ra, inst_ref, inst, 64, cmp::LESS_THAN_OS);
}
pub fn emit_fp_vector_greater_equal32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_vector_greater_impl(ctx, ra, inst_ref, inst, 32, cmp::LESS_EQUAL_OS);
}
pub fn emit_fp_vector_greater_equal64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_vector_greater_impl(ctx, ra, inst_ref, inst, 64, cmp::LESS_EQUAL_OS);
}

// ---------------------------------------------------------------------------
// FPVectorMulX — fallback (mulx handles special cases for 0*inf)
// ---------------------------------------------------------------------------

extern "C" fn fallback_fp_vector_mulx32(
    result: *mut [u8; 16],
    a: *const [u8; 16],
    b: *const [u8; 16],
) {
    unsafe {
        let va: [f32; 4] = std::mem::transmute(*a);
        let vb: [f32; 4] = std::mem::transmute(*b);
        let mut out = [0f32; 4];
        for i in 0..4 {
            if (va[i] == 0.0 && vb[i].is_infinite()) || (va[i].is_infinite() && vb[i] == 0.0) {
                out[i] = 2.0f32.copysign(va[i] * vb[i]);
            } else {
                out[i] = va[i] * vb[i];
            }
        }
        *result = std::mem::transmute(out);
    }
}

extern "C" fn fallback_fp_vector_mulx64(
    result: *mut [u8; 16],
    a: *const [u8; 16],
    b: *const [u8; 16],
) {
    unsafe {
        let va: [f64; 2] = std::mem::transmute(*a);
        let vb: [f64; 2] = std::mem::transmute(*b);
        let mut out = [0f64; 2];
        for i in 0..2 {
            if (va[i] == 0.0 && vb[i].is_infinite()) || (va[i].is_infinite() && vb[i] == 0.0) {
                out[i] = 2.0f64.copysign(va[i] * vb[i]);
            } else {
                out[i] = va[i] * vb[i];
            }
        }
        *result = std::mem::transmute(out);
    }
}

pub fn emit_fp_vector_mulx32(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_fp_vector_mulx32 as *const () as usize,
    );
}
pub fn emit_fp_vector_mulx64(
    _ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_two_arg_fallback(
        ra,
        inst_ref,
        inst,
        fallback_fp_vector_mulx64 as *const () as usize,
    );
}

// ---------------------------------------------------------------------------
// FPVectorPairedAdd — fallback
// ---------------------------------------------------------------------------

fn fp_add_lane32(a: u32, b: u32, fpcr: Fpcr, fpsr: &mut Fpsr) -> u32 {
    let is_nan = |value: u32| {
        value & u32::EXPONENT_MASK as u32 == u32::EXPONENT_MASK as u32
            && value & u32::MANTISSA_MASK as u32 != 0
    };
    let is_signaling = |value: u32| is_nan(value) && value & u32::MANTISSA_MSB as u32 == 0;
    let selected_nan = if is_signaling(a) {
        fpsr.set_ioc(true);
        Some(a | u32::MANTISSA_MSB as u32)
    } else if is_signaling(b) {
        fpsr.set_ioc(true);
        Some(b | u32::MANTISSA_MSB as u32)
    } else if is_nan(a) {
        Some(a)
    } else if is_nan(b) {
        Some(b)
    } else {
        None
    };
    if let Some(value) = selected_nan {
        return if fpcr.dn() { u32::default_nan() } else { value };
    }

    let result = (f32::from_bits(a) + f32::from_bits(b)).to_bits();
    if is_nan(result) {
        u32::default_nan()
    } else {
        result
    }
}

fn fp_add_lane64(a: u64, b: u64, fpcr: Fpcr, fpsr: &mut Fpsr) -> u64 {
    let is_nan = |value: u64| {
        value & u64::EXPONENT_MASK == u64::EXPONENT_MASK && value & u64::MANTISSA_MASK != 0
    };
    let is_signaling = |value: u64| is_nan(value) && value & u64::MANTISSA_MSB == 0;
    let selected_nan = if is_signaling(a) {
        fpsr.set_ioc(true);
        Some(a | u64::MANTISSA_MSB)
    } else if is_signaling(b) {
        fpsr.set_ioc(true);
        Some(b | u64::MANTISSA_MSB)
    } else if is_nan(a) {
        Some(a)
    } else if is_nan(b) {
        Some(b)
    } else {
        None
    };
    if let Some(value) = selected_nan {
        return if fpcr.dn() { u64::default_nan() } else { value };
    }

    let result = (f64::from_bits(a) + f64::from_bits(b)).to_bits();
    if is_nan(result) {
        u64::default_nan()
    } else {
        result
    }
}

extern "C" fn fallback_fp_paired_add32(
    result: *mut [u8; 16],
    a: *const [u8; 16],
    b: *const [u8; 16],
    fpcr: u32,
    fpsr_exc: *mut u32,
) {
    unsafe {
        let va: [u32; 4] = std::mem::transmute(*a);
        let vb: [u32; 4] = std::mem::transmute(*b);
        let fpcr = Fpcr::new(fpcr);
        let mut fpsr = Fpsr::new(fpsr_exc.read());
        let out = [
            fp_add_lane32(va[0], va[1], fpcr, &mut fpsr),
            fp_add_lane32(va[2], va[3], fpcr, &mut fpsr),
            fp_add_lane32(vb[0], vb[1], fpcr, &mut fpsr),
            fp_add_lane32(vb[2], vb[3], fpcr, &mut fpsr),
        ];
        fpsr_exc.write(fpsr.value());
        *result = std::mem::transmute(out);
    }
}

extern "C" fn fallback_fp_paired_add64(
    result: *mut [u8; 16],
    a: *const [u8; 16],
    b: *const [u8; 16],
    fpcr: u32,
    fpsr_exc: *mut u32,
) {
    unsafe {
        let va: [u64; 2] = std::mem::transmute(*a);
        let vb: [u64; 2] = std::mem::transmute(*b);
        let fpcr = Fpcr::new(fpcr);
        let mut fpsr = Fpsr::new(fpsr_exc.read());
        let out = [
            fp_add_lane64(va[0], va[1], fpcr, &mut fpsr),
            fp_add_lane64(vb[0], vb[1], fpcr, &mut fpsr),
        ];
        fpsr_exc.write(fpsr.value());
        *result = std::mem::transmute(out);
    }
}

pub fn emit_fp_vector_paired_add32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_three_op_fallback(
        ctx,
        ra,
        inst_ref,
        inst,
        fallback_fp_paired_add32 as *const () as usize,
    );
}
pub fn emit_fp_vector_paired_add64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_three_op_fallback(
        ctx,
        ra,
        inst_ref,
        inst,
        fallback_fp_paired_add64 as *const () as usize,
    );
}

// ---------------------------------------------------------------------------
// FPVectorPairedAddLower — fallback (only lower pair, upper zeroed)
// ---------------------------------------------------------------------------

extern "C" fn fallback_fp_paired_add_lower32(
    result: *mut [u8; 16],
    a: *const [u8; 16],
    b: *const [u8; 16],
    fpcr: u32,
    fpsr_exc: *mut u32,
) {
    unsafe {
        let va: [u32; 4] = std::mem::transmute(*a);
        let vb: [u32; 4] = std::mem::transmute(*b);
        let fpcr = Fpcr::new(fpcr);
        let mut fpsr = Fpsr::new(fpsr_exc.read());
        let out = [
            fp_add_lane32(va[0], va[1], fpcr, &mut fpsr),
            fp_add_lane32(vb[0], vb[1], fpcr, &mut fpsr),
            0,
            0,
        ];
        fpsr_exc.write(fpsr.value());
        *result = std::mem::transmute(out);
    }
}

extern "C" fn fallback_fp_paired_add_lower64(
    result: *mut [u8; 16],
    a: *const [u8; 16],
    b: *const [u8; 16],
    fpcr: u32,
    fpsr_exc: *mut u32,
) {
    unsafe {
        let va: [u64; 2] = std::mem::transmute(*a);
        let vb: [u64; 2] = std::mem::transmute(*b);
        let fpcr = Fpcr::new(fpcr);
        let mut fpsr = Fpsr::new(fpsr_exc.read());
        let out = [
            fp_add_lane64(va[0], va[1], fpcr, &mut fpsr),
            fp_add_lane64(vb[0], vb[1], fpcr, &mut fpsr),
        ];
        fpsr_exc.write(fpsr.value());
        *result = std::mem::transmute(out);
    }
}

pub fn emit_fp_vector_paired_add_lower32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_three_op_fallback(
        ctx,
        ra,
        inst_ref,
        inst,
        fallback_fp_paired_add_lower32 as *const () as usize,
    );
}
pub fn emit_fp_vector_paired_add_lower64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_three_op_fallback(
        ctx,
        ra,
        inst_ref,
        inst,
        fallback_fp_paired_add_lower64 as *const () as usize,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fn_signatures() {
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_add32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_add64;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_sub32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_mul64;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_div32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_sqrt32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_abs16;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_neg16;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_max32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_min64;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_max_numeric32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_min_numeric64;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_equal16;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_greater32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_greater_equal64;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_mulx32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_paired_add32;
        let _: fn(&EmitContext, &mut RegAlloc, InstRef, &Inst) = emit_fp_vector_paired_add_lower64;
    }

    #[test]
    fn test_fallback_fp_paired_add32() {
        let a: [u8; 16] = unsafe { std::mem::transmute([1.0f32, 2.0f32, 3.0f32, 4.0f32]) };
        let b: [u8; 16] = unsafe { std::mem::transmute([5.0f32, 6.0f32, 7.0f32, 8.0f32]) };
        let mut result = [0u8; 16];
        let mut fpsr = 0;
        fallback_fp_paired_add32(&mut result, &a, &b, 0, &mut fpsr);
        let out: [f32; 4] = unsafe { std::mem::transmute(result) };
        assert_eq!(out[0], 3.0); // 1+2
        assert_eq!(out[1], 7.0); // 3+4
        assert_eq!(out[2], 11.0); // 5+6
        assert_eq!(out[3], 15.0); // 7+8
    }

    #[test]
    fn paired_add_default_nan_mode_canonicalizes_nan_lanes() {
        let a: [u8; 16] =
            unsafe { std::mem::transmute([f32::INFINITY, f32::NEG_INFINITY, 1.0f32, 2.0f32]) };
        let b = [0u8; 16];
        let mut result = [0u8; 16];
        let mut fpsr = 0;

        fallback_fp_paired_add32(&mut result, &a, &b, 1 << 25, &mut fpsr);

        let out: [u32; 4] = unsafe { std::mem::transmute(result) };
        assert_eq!(out[0], 0x7fc0_0000);
        assert_eq!(out[1], 3.0f32.to_bits());
    }

    #[test]
    fn paired_add_prioritizes_and_quiets_signaling_nan() {
        let a: [u8; 16] =
            unsafe { std::mem::transmute([0x7f81_7182u32, 0x7ffe_cfe5, 0x3f80_0000, 0x4000_0000]) };
        let b = [0u8; 16];
        let mut result = [0u8; 16];
        let mut fpsr = 0;

        fallback_fp_paired_add32(&mut result, &a, &b, 0, &mut fpsr);

        let out: [u32; 4] = unsafe { std::mem::transmute(result) };
        assert_eq!(out[0], 0x7fc1_7182);
        assert_eq!(fpsr & 1, 1);
    }

    #[test]
    fn vector_min_max_records_nan_invalid_operation() {
        let a: [u8; 16] =
            unsafe { std::mem::transmute([0x3f80_0000u32, 0x7fc1_2345, 0x4040_0000, 0x4080_0000]) };
        let b: [u8; 16] =
            unsafe { std::mem::transmute([0x4000_0000u32, 0x4040_0000, 0x40a0_0000, 0x40c0_0000]) };
        let mut result = [0u8; 16];
        let mut fpsr = 0;

        fallback_fp_max32(&mut result, &a, &b, 0, &mut fpsr);

        assert_eq!(fpsr & 1, 1);
    }

    // Test removed: fallback_fp_vector_abs32 replaced with inline SSE (andps)
    // Correctness verified via a32_diff fuzzing
}
