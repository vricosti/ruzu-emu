#![allow(
    clippy::missing_transmute_annotations,
    clippy::useless_transmute,
    unnecessary_transmutes
)]

use rxbyak::{qword_ptr, xmmword_ptr, JmpType, RegExp, RSP, XMM0};

use crate::backend::x64::abi;
use crate::backend::x64::constants::{
    cmp, fixup_lut, fp_class, fp_range_lut, FpFixup, FpRangeSelect, FpRangeSign,
};
use crate::backend::x64::emit_context::{DeferredEmitCtx, EmitContext};
use crate::backend::x64::emit_floating_point::fp_min_max;
use crate::backend::x64::emit_fp_vector_convert::maybe_standard_fpscr_value;
use crate::backend::x64::emit_vector_helpers::*;
use crate::backend::x64::host_feature::HostFeature;
use crate::backend::x64::hostloc::HostLoc;
use crate::backend::x64::reg_alloc::RegAlloc;
use crate::common::fp::fpcr::Fpcr;
use crate::common::fp::fpsr::Fpsr;
use crate::common::fp::info::FloatFormat;
use crate::common::fp::rounding_mode::RoundingMode;
use crate::common::fp::util::{is_nan, process_nans2};
use crate::interface::optimization_flags::OptimizationFlag;
use crate::ir::inst::Inst;
use crate::ir::value::InstRef;

// ---------------------------------------------------------------------------
// Upstream NaN handling infrastructure (anonymous namespace of
// emit_x64_vector_floating_point.cpp): ForceToDefaultNaN, NaNHandler,
// HandleNaNs, EmitThreeOpVectorOperation.
// ---------------------------------------------------------------------------

/// Port of upstream vector `ForceToDefaultNaN<fsize>(code, fpcr, result)`.
/// Trashes XMM0 (SSE/AVX paths) or K1 (AVX-512 path).
pub(crate) fn force_to_default_nan(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    fsize: usize,
    fpcr: Fpcr,
    result: rxbyak::Reg,
) {
    if fpcr.dn() {
        if ctx.has_host_feature(HostFeature::AVX512_ORTHO_FLOAT) {
            let nan_mask = rxbyak::K1;
            fcode!(
                ra.asm,
                fsize,
                vfpclassps,
                vfpclasspd,
                nan_mask,
                result,
                fp_class::QNAN | fp_class::SNAN
            );
            let nan_vector = get_nan_vector(ra, fsize);
            fcode!(
                ra.asm,
                fsize,
                vblendmps,
                vblendmpd,
                result.k(1),
                result,
                nan_vector
            );
        } else if ctx.has_host_feature(HostFeature::AVX) {
            let nan_mask = XMM0;
            fcode!(
                ra.asm,
                fsize,
                vcmpps,
                vcmppd,
                nan_mask,
                result,
                result,
                cmp::UNORDERED_Q
            );
            let nan_vector = get_nan_vector(ra, fsize);
            fcode!(ra.asm, fsize, blendvps, blendvpd, result, nan_vector);
        } else {
            let nan_mask = XMM0;
            ra.asm.movaps(nan_mask, result).unwrap();
            fcode!(
                ra.asm,
                fsize,
                cmpps,
                cmppd,
                nan_mask,
                nan_mask,
                cmp::ORDERED_Q
            );
            ra.asm.andps(result, nan_mask).unwrap();
            let nan_vector = get_nan_vector(ra, fsize);
            ra.asm.andnps(nan_mask, nan_vector).unwrap();
            ra.asm.orps(result, nan_mask).unwrap();
        }
    }
}

/// Upstream indexer templates (`DefaultIndexer`, `PairedIndexer`,
/// `PairedLowerIndexer`) used by the NaN handlers.
pub(crate) mod indexer {
    pub(crate) const DEFAULT: u8 = 0;
    pub(crate) const PAIRED: u8 = 1;
    pub(crate) const PAIRED_LOWER: u8 = 2;
}

fn indexed_pair<T: Copy + Default, const N: usize>(
    which: u8,
    i: usize,
    a: &[T; N],
    b: &[T; N],
) -> (T, T) {
    match which {
        indexer::DEFAULT => (a[i], b[i]),
        indexer::PAIRED => {
            let halfway = N / 2;
            let which_array = i / halfway;
            let i = i % halfway;
            match which_array {
                0 => (a[2 * i], a[2 * i + 1]),
                1 => (b[2 * i], b[2 * i + 1]),
                _ => unreachable!(),
            }
        }
        indexer::PAIRED_LOWER => match N {
            4 => match i {
                0 => (a[0], a[1]),
                1 => (b[0], b[1]),
                _ => (T::default(), T::default()),
            },
            2 => {
                if i == 0 {
                    (a[0], b[0])
                } else {
                    (T::default(), T::default())
                }
            }
            _ => unreachable!(),
        },
        _ => unreachable!("invalid indexer {which}"),
    }
}

/// Port of upstream `NaNHandler<fsize, Indexer, 3>::GetDefault()`:
/// `values = {result, a, b}`; for each element, the ARM NaN of the indexed
/// operands, else the default NaN if the host result is a NaN.
extern "C" fn default_nan_handler3<T, const N: usize, const INDEXER: u8>(
    values: *mut [[T; N]; 3],
    _fpcr: u32,
) where
    T: FloatFormat + Copy + Default,
{
    let values = unsafe { &mut *values };
    let a = values[1];
    let b = values[2];
    let result = &mut values[0];
    for elementi in 0..N {
        let (x, y) = indexed_pair(INDEXER, elementi, &a, &b);
        if let Some(r) = process_nans2(x, y) {
            result[elementi] = r;
        } else if is_nan(result[elementi]) {
            result[elementi] = T::default_nan();
        }
    }
}

pub(crate) fn default_nan_handler3_for(fsize: usize, which: u8) -> usize {
    macro_rules! select {
        ($t:ty, $n:expr) => {
            match which {
                indexer::DEFAULT => {
                    default_nan_handler3::<$t, $n, { indexer::DEFAULT }> as *const () as usize
                }
                indexer::PAIRED => {
                    default_nan_handler3::<$t, $n, { indexer::PAIRED }> as *const () as usize
                }
                indexer::PAIRED_LOWER => {
                    default_nan_handler3::<$t, $n, { indexer::PAIRED_LOWER }> as *const () as usize
                }
                _ => unreachable!(),
            }
        };
    }
    match fsize {
        32 => select!(u32, 4),
        64 => select!(u64, 2),
        _ => unreachable!("fsize must be either 32 or 64"),
    }
}

/// Port of upstream `HandleNaNs<fsize, nargs>`: if any lane of `nan_mask` is
/// set, a deferred slow path stores `xmms` to the stack and calls
/// `nan_handler(values, fpcr)`, which rewrites `xmms[0]` (the result).
fn handle_nans(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    fpcr_controlled: bool,
    xmms: &[rxbyak::Reg],
    nan_mask: rxbyak::Reg,
    nan_handler: usize,
) {
    if ctx.has_host_feature(HostFeature::SSE41) {
        ra.asm.ptest(nan_mask, nan_mask).unwrap();
    } else {
        let bitmask = ra.scratch_gpr().cvt32().unwrap();
        ra.asm.movmskps(bitmask, nan_mask).unwrap();
        ra.asm.cmp(bitmask, 0).unwrap();
    }

    let end = ra.asm.create_label();
    let nan = ra.asm.create_label();

    ra.asm.jnz(&nan, JmpType::Near).unwrap();
    ra.asm.bind(&end).unwrap();

    let xmms = xmms.to_vec();
    let fpcr = ctx.fpcr(fpcr_controlled).value();
    ctx.deferred_emits
        .borrow_mut()
        .push(Box::new(move |dctx: &mut DeferredEmitCtx<'_>| {
            let asm = &mut *dctx.asm;
            asm.bind(&nan).unwrap();

            let result = xmms[0];

            asm.lea(RSP, qword_ptr(RegExp::from(RSP) - 8)).unwrap();
            let frame = abi::push_caller_save_registers_and_adjust_stack_except(
                asm,
                Some(HostLoc::Xmm(result.index())),
            )
            .unwrap();

            let stack_space = xmms.len() * 16;
            asm.lea(
                RSP,
                qword_ptr(RegExp::from(RSP) - (stack_space + abi::ABI_SHADOW_SPACE) as i32),
            )
            .unwrap();
            for (i, &xmm) in xmms.iter().enumerate() {
                asm.movaps(
                    xmmword_ptr(RegExp::from(RSP) + (abi::ABI_SHADOW_SPACE + i * 16) as i32),
                    xmm,
                )
                .unwrap();
            }
            asm.lea(
                abi::ABI_PARAMS[0].to_reg64(),
                qword_ptr(RegExp::from(RSP) + abi::ABI_SHADOW_SPACE as i32),
            )
            .unwrap();
            asm.mov(abi::ABI_PARAMS[1].to_reg64(), fpcr as i64).unwrap();

            asm.mov(rxbyak::RAX, nan_handler as i64).unwrap();
            asm.call_reg(rxbyak::RAX).unwrap();

            asm.movaps(
                result,
                xmmword_ptr(RegExp::from(RSP) + abi::ABI_SHADOW_SPACE as i32),
            )
            .unwrap();
            asm.add(RSP, (stack_space + abi::ABI_SHADOW_SPACE) as i32)
                .unwrap();
            abi::pop_caller_save_registers_and_adjust_stack(asm, &frame).unwrap();
            asm.add(RSP, 8).unwrap();
            asm.jmp(&end, JmpType::Near).unwrap();
        }));
}

/// Port of upstream `EmitThreeOpVectorOperation<fsize, Indexer>`.
/// `fn_(ra, result, xmm_b)` emits the operation into `result`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_three_op_vector_operation(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    fsize: usize,
    which_indexer: u8,
    check_input_nan: bool,
    fn_: impl FnOnce(&mut RegAlloc, rxbyak::Reg, rxbyak::Reg),
) {
    let nan_handler = default_nan_handler3_for(fsize, which_indexer);

    let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
    let fpcr_controlled = args[2].get_immediate_u1();

    if ctx.fpcr(fpcr_controlled).dn()
        || ctx.has_optimization(OptimizationFlag::UNSAFE_INACCURATE_NAN)
    {
        let xmm_a = ra.use_scratch_xmm(&mut args[0]);
        let xmm_b = ra.use_xmm(&mut args[1]);

        maybe_standard_fpscr_value(ctx, ra, fpcr_controlled, |ra| {
            fn_(ra, xmm_a, xmm_b);
        });

        if !ctx.has_optimization(OptimizationFlag::UNSAFE_INACCURATE_NAN) {
            force_to_default_nan(ctx, ra, fsize, ctx.fpcr(fpcr_controlled), xmm_a);
        }

        ra.define_value(inst_ref, xmm_a);
        return;
    }

    let result = ra.scratch_xmm();
    let xmm_a = ra.use_xmm(&mut args[0]);
    let xmm_b = ra.use_xmm(&mut args[1]);
    let nan_mask = ra.scratch_xmm();

    ra.asm.movaps(result, xmm_a).unwrap();

    if check_input_nan {
        if ctx.has_host_feature(HostFeature::AVX) {
            fcode!(
                ra.asm,
                fsize,
                vcmpps,
                vcmppd,
                nan_mask,
                xmm_a,
                xmm_b,
                cmp::UNORDERED_Q
            );
        } else {
            ra.asm.movaps(nan_mask, xmm_b).unwrap();
            fcode!(
                ra.asm,
                fsize,
                cmpps,
                cmppd,
                nan_mask,
                xmm_a,
                cmp::UNORDERED_Q
            );
        }
    }

    fn_(ra, result, xmm_b);

    if check_input_nan {
        fcode!(
            ra.asm,
            fsize,
            cmpps,
            cmppd,
            nan_mask,
            result,
            cmp::UNORDERED_Q
        );
    } else if ctx.has_host_feature(HostFeature::AVX) {
        fcode!(
            ra.asm,
            fsize,
            vcmpps,
            vcmppd,
            nan_mask,
            result,
            result,
            cmp::UNORDERED_Q
        );
    } else {
        ra.asm.movaps(nan_mask, result).unwrap();
        fcode!(
            ra.asm,
            fsize,
            cmpps,
            cmppd,
            nan_mask,
            nan_mask,
            cmp::UNORDERED_Q
        );
    }

    handle_nans(
        ctx,
        ra,
        fpcr_controlled,
        &[result, xmm_a, xmm_b],
        nan_mask,
        nan_handler,
    );

    ra.define_value(inst_ref, result);
}

pub fn emit_fp_vector_add32(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_three_op_vector_operation(
        ctx,
        ra,
        inst_ref,
        inst,
        32,
        indexer::DEFAULT,
        false,
        |ra, result, xmm_b| {
            ra.asm.addps(result, xmm_b).unwrap();
        },
    );
}
pub fn emit_fp_vector_add64(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_three_op_vector_operation(
        ctx,
        ra,
        inst_ref,
        inst,
        64,
        indexer::DEFAULT,
        false,
        |ra, result, xmm_b| {
            ra.asm.addpd(result, xmm_b).unwrap();
        },
    );
}

// ---------------------------------------------------------------------------
// FPVectorSub — native SSE: subps/subpd
// ---------------------------------------------------------------------------

pub fn emit_fp_vector_sub32(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_three_op_vector_operation(
        ctx,
        ra,
        inst_ref,
        inst,
        32,
        indexer::DEFAULT,
        false,
        |ra, result, xmm_b| {
            ra.asm.subps(result, xmm_b).unwrap();
        },
    );
}
pub fn emit_fp_vector_sub64(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_three_op_vector_operation(
        ctx,
        ra,
        inst_ref,
        inst,
        64,
        indexer::DEFAULT,
        false,
        |ra, result, xmm_b| {
            ra.asm.subpd(result, xmm_b).unwrap();
        },
    );
}

// ---------------------------------------------------------------------------
// FPVectorMul — native SSE: mulps/mulpd
// ---------------------------------------------------------------------------

pub fn emit_fp_vector_mul32(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_three_op_vector_operation(
        ctx,
        ra,
        inst_ref,
        inst,
        32,
        indexer::DEFAULT,
        false,
        |ra, result, xmm_b| {
            ra.asm.mulps(result, xmm_b).unwrap();
        },
    );
}
pub fn emit_fp_vector_mul64(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_three_op_vector_operation(
        ctx,
        ra,
        inst_ref,
        inst,
        64,
        indexer::DEFAULT,
        false,
        |ra, result, xmm_b| {
            ra.asm.mulpd(result, xmm_b).unwrap();
        },
    );
}

// ---------------------------------------------------------------------------
// FPVectorDiv — native SSE: divps/divpd
// ---------------------------------------------------------------------------

pub fn emit_fp_vector_div32(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_three_op_vector_operation(
        ctx,
        ra,
        inst_ref,
        inst,
        32,
        indexer::DEFAULT,
        false,
        |ra, result, xmm_b| {
            ra.asm.divps(result, xmm_b).unwrap();
        },
    );
}
pub fn emit_fp_vector_div64(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_three_op_vector_operation(
        ctx,
        ra,
        inst_ref,
        inst,
        64,
        indexer::DEFAULT,
        false,
        |ra, result, xmm_b| {
            ra.asm.divpd(result, xmm_b).unwrap();
        },
    );
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

define_fp_vector_min_max_fallback!(fallback_fp_maxnm32, u32, 4, true, true);
define_fp_vector_min_max_fallback!(fallback_fp_maxnm64, u64, 2, true, true);
define_fp_vector_min_max_fallback!(fallback_fp_minnm32, u32, 4, false, true);
define_fp_vector_min_max_fallback!(fallback_fp_minnm64, u64, 2, false, true);

/// Fallback for FPVectorMaxNumeric/MinNumeric (upstream emits
/// `EmitFPVectorMinMaxNumeric` inline; not ported here).
fn emit_fp_vector_min_max_numeric(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    fallback: usize,
) {
    emit_three_op_fallback(ctx, ra, inst_ref, inst, fallback);
}

/// Port of upstream `EmitFPVectorMinMax<fsize, is_max>`.
fn emit_fp_vector_min_max(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
    fsize: usize,
    is_max: bool,
) {
    let fpcr_controlled = inst.args[2].get_u1();
    let fpcr = ctx.fpcr(fpcr_controlled);

    if fpcr.dn() {
        let mut args = ra.get_argument_info(inst_ref, &inst.args, inst.num_args());
        let result = ra.use_scratch_xmm(&mut args[0]);
        let xmm_b = if fpcr.fz() {
            ra.use_scratch_xmm(&mut args[1])
        } else {
            ra.use_xmm(&mut args[1])
        };

        let mask = XMM0;
        let eq = ra.scratch_xmm();
        let nan_mask = ra.scratch_xmm();

        maybe_standard_fpscr_value(ctx, ra, fpcr_controlled, |ra| {
            denormals_are_zero(ctx, ra, fsize, fpcr, &[result, xmm_b], mask);

            if ctx.has_host_feature(HostFeature::AVX512_ORTHO_FLOAT) {
                let range_select = if is_max {
                    FpRangeSelect::Max
                } else {
                    FpRangeSelect::Min
                };
                fcode!(
                    ra.asm,
                    fsize,
                    vcmpps,
                    vcmppd,
                    rxbyak::K1,
                    result,
                    xmm_b,
                    cmp::UNORDERED_Q
                );
                fcode!(
                    ra.asm,
                    fsize,
                    vrangeps,
                    vrangepd,
                    result,
                    result,
                    xmm_b,
                    fp_range_lut(range_select, FpRangeSign::Preserve)
                );
                let nan_vector = get_nan_vector(ra, fsize);
                fcode!(
                    ra.asm,
                    fsize,
                    vblendmps,
                    vblendmpd,
                    result.k(1),
                    result,
                    nan_vector
                );
            } else if ctx.has_host_feature(HostFeature::AVX) {
                fcode!(
                    ra.asm,
                    fsize,
                    vcmpps,
                    vcmppd,
                    mask,
                    result,
                    xmm_b,
                    cmp::EQUAL_OQ
                );
                fcode!(
                    ra.asm,
                    fsize,
                    vcmpps,
                    vcmppd,
                    nan_mask,
                    result,
                    xmm_b,
                    cmp::UNORDERED_Q
                );
                if is_max {
                    fcode!(ra.asm, fsize, vandps, vandpd, eq, result, xmm_b);
                    fcode!(ra.asm, fsize, vmaxps, vmaxpd, result, result, xmm_b);
                } else {
                    fcode!(ra.asm, fsize, vorps, vorpd, eq, result, xmm_b);
                    fcode!(ra.asm, fsize, vminps, vminpd, result, result, xmm_b);
                }
                fcode!(ra.asm, fsize, blendvps, blendvpd, result, eq);
                let nan_vector = get_nan_vector(ra, fsize);
                fcode!(ra.asm, fsize, vblendvps, vblendvpd, result, result, nan_vector, nan_mask);
            } else {
                ra.asm.movaps(mask, result).unwrap();
                ra.asm.movaps(eq, result).unwrap();
                ra.asm.movaps(nan_mask, result).unwrap();
                fcode!(ra.asm, fsize, cmpps, cmppd, mask, xmm_b, cmp::NOT_EQUAL_UQ);
                fcode!(ra.asm, fsize, cmpps, cmppd, nan_mask, xmm_b, cmp::ORDERED_Q);

                if is_max {
                    ra.asm.andps(eq, xmm_b).unwrap();
                    fcode!(ra.asm, fsize, maxps, maxpd, result, xmm_b);
                } else {
                    ra.asm.orps(eq, xmm_b).unwrap();
                    fcode!(ra.asm, fsize, minps, minpd, result, xmm_b);
                }

                ra.asm.andps(result, mask).unwrap();
                ra.asm.andnps(mask, eq).unwrap();
                ra.asm.orps(result, mask).unwrap();

                ra.asm.andps(result, nan_mask).unwrap();
                let nan_vector = get_nan_vector(ra, fsize);
                ra.asm.andnps(nan_mask, nan_vector).unwrap();
                ra.asm.orps(result, nan_mask).unwrap();
            }
        });

        ra.define_value(inst_ref, result);
        return;
    }

    emit_three_op_vector_operation(
        ctx,
        ra,
        inst_ref,
        inst,
        fsize,
        indexer::DEFAULT,
        true,
        |ra, result, xmm_b| {
            let mask = XMM0;
            let eq = ra.scratch_xmm();
            let mut xmm_b = xmm_b;

            if fpcr.fz() {
                let prev_xmm_b = xmm_b;
                xmm_b = ra.scratch_xmm();
                ra.asm.movaps(xmm_b, prev_xmm_b).unwrap();
                denormals_are_zero(ctx, ra, fsize, fpcr, &[result, xmm_b], mask);
            }

            // What we are doing here is handling the case when the inputs are differently signed zeros.
            // x86-64 treats differently signed zeros as equal while ARM does not.
            // Thus if we AND together things that x86-64 thinks are equal we'll get the positive zero.

            // vrangep{s,d} here ends up not being significantly shorter than the AVX implementation

            if ctx.has_host_feature(HostFeature::AVX) {
                fcode!(
                    ra.asm,
                    fsize,
                    vcmpps,
                    vcmppd,
                    mask,
                    result,
                    xmm_b,
                    cmp::EQUAL_OQ
                );
                if is_max {
                    fcode!(ra.asm, fsize, vandps, vandpd, eq, result, xmm_b);
                    fcode!(ra.asm, fsize, vmaxps, vmaxpd, result, result, xmm_b);
                } else {
                    fcode!(ra.asm, fsize, vorps, vorpd, eq, result, xmm_b);
                    fcode!(ra.asm, fsize, vminps, vminpd, result, result, xmm_b);
                }
                fcode!(ra.asm, fsize, blendvps, blendvpd, result, eq);
            } else {
                ra.asm.movaps(mask, result).unwrap();
                ra.asm.movaps(eq, result).unwrap();
                fcode!(ra.asm, fsize, cmpps, cmppd, mask, xmm_b, cmp::NOT_EQUAL_UQ);

                if is_max {
                    ra.asm.andps(eq, xmm_b).unwrap();
                    fcode!(ra.asm, fsize, maxps, maxpd, result, xmm_b);
                } else {
                    ra.asm.orps(eq, xmm_b).unwrap();
                    fcode!(ra.asm, fsize, minps, minpd, result, xmm_b);
                }

                ra.asm.andps(result, mask).unwrap();
                ra.asm.andnps(mask, eq).unwrap();
                ra.asm.orps(result, mask).unwrap();
            }
        },
    );
}

pub fn emit_fp_vector_max32(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_vector_min_max(ctx, ra, inst_ref, inst, 32, true);
}
pub fn emit_fp_vector_max64(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_vector_min_max(ctx, ra, inst_ref, inst, 64, true);
}
pub fn emit_fp_vector_min32(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_vector_min_max(ctx, ra, inst_ref, inst, 32, false);
}
pub fn emit_fp_vector_min64(ctx: &EmitContext, ra: &mut RegAlloc, inst_ref: InstRef, inst: &Inst) {
    emit_fp_vector_min_max(ctx, ra, inst_ref, inst, 64, false);
}

pub fn emit_fp_vector_max_numeric32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_fp_vector_min_max_numeric(
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
    emit_fp_vector_min_max_numeric(
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
    emit_fp_vector_min_max_numeric(
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
    emit_fp_vector_min_max_numeric(
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

/// Upstream `GetNaNVector<fsize>`.
pub(crate) fn get_nan_vector(ra: &mut RegAlloc, fsize: usize) -> rxbyak::Address {
    match fsize {
        32 => get_vector_of(ra, 32, u32::default_nan() as u64),
        64 => get_vector_of(ra, 64, u64::default_nan()),
        _ => unreachable!("fsize must be either 32 or 64"),
    }
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
// FPVectorPairedAdd / FPVectorPairedAddLower
// Port of upstream EmitFPVectorPairedAdd{32,64} and
// EmitFPVectorPairedAddLower{32,64} (emit_x64_vector_floating_point.cpp).
// ---------------------------------------------------------------------------

pub fn emit_fp_vector_paired_add32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_three_op_vector_operation(
        ctx,
        ra,
        inst_ref,
        inst,
        32,
        indexer::PAIRED,
        false,
        |ra, result, xmm_b| {
            ra.asm.haddps(result, xmm_b).unwrap();
        },
    );
}
pub fn emit_fp_vector_paired_add64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_three_op_vector_operation(
        ctx,
        ra,
        inst_ref,
        inst,
        64,
        indexer::PAIRED,
        false,
        |ra, result, xmm_b| {
            ra.asm.haddpd(result, xmm_b).unwrap();
        },
    );
}

pub fn emit_fp_vector_paired_add_lower32(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_three_op_vector_operation(
        ctx,
        ra,
        inst_ref,
        inst,
        32,
        indexer::PAIRED_LOWER,
        false,
        |ra, result, xmm_b| {
            let zero = ra.scratch_xmm();
            ra.asm.xorps(zero, zero).unwrap();
            ra.asm.punpcklqdq(result, xmm_b).unwrap();
            ra.asm.haddps(result, zero).unwrap();
        },
    );
}
pub fn emit_fp_vector_paired_add_lower64(
    ctx: &EmitContext,
    ra: &mut RegAlloc,
    inst_ref: InstRef,
    inst: &Inst,
) {
    emit_three_op_vector_operation(
        ctx,
        ra,
        inst_ref,
        inst,
        64,
        indexer::PAIRED_LOWER,
        false,
        |ra, result, xmm_b| {
            let zero = ra.scratch_xmm();
            ra.asm.xorps(zero, zero).unwrap();
            ra.asm.punpcklqdq(result, xmm_b).unwrap();
            ra.asm.haddpd(result, zero).unwrap();
        },
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

    // Test removed: fallback_fp_vector_abs32 replaced with inline SSE (andps)
    // Correctness verified via a32_diff fuzzing
}
