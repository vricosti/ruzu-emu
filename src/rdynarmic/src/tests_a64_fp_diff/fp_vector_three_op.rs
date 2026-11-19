//! Operations routed through the port of upstream
//! `EmitThreeOpVectorOperation` + `HandleNaNs` + `ForceToDefaultNaN`
//! (emit_x64_vector_floating_point.cpp): FPVectorPairedAdd{,Lower},
//! FPVectorAdd/Sub/Mul/Div.
//!
//! Paired adds are compared with the legacy rdynarmic fallbacks (verbatim
//! below). Add/Sub/Mul/Div had no fallback (they were inline without the
//! upstream NaN handling); their oracle is the ARM NaN rule the upstream
//! NaN handler implements: host operation under the guest MXCSR, then the
//! ARM-selected NaN (SNaN quietened, first operand first), else the default
//! NaN when the host produced one; with FPCR.DN every NaN is the default NaN.

use super::*;
use crate::common::fp::fpcr::Fpcr;
use crate::common::fp::fpsr::Fpsr;
use crate::common::fp::info::FloatFormat;

// ---------------------------------------------------------------------------
// Legacy rdynarmic fallbacks, verbatim (emit_fp_vector.rs before the port).
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

pub(super) type ThreeOpFallback =
    extern "C" fn(*mut [u8; 16], *const [u8; 16], *const [u8; 16], u32, *mut u32);

/// Oracle for a legacy `(result, a, b, fpcr, fpsr_exc)` fallback: run under
/// the guest MXCSR; FPSR = fallback FPSR | flags its host arithmetic raised.
pub(super) fn three_op_fallback_oracle(
    fallback: ThreeOpFallback,
) -> impl Fn(u32, &[Vector]) -> (Vector, u32) {
    move |fpcr, inputs| {
        let a = vec_to_bytes(&inputs[0]);
        let b = vec_to_bytes(&inputs[1]);
        let mut out = [0u8; 16];
        let mut fpsr = 0u32;
        let f = std::hint::black_box(fallback);
        let ((), flags) = under_guest_mxcsr(fpcr, || f(&mut out, &a, &b, fpcr, &mut fpsr));
        (bytes_to_vec(&out), fpsr | flags)
    }
}

// ---------------------------------------------------------------------------
// ARM NaN model for FPVectorAdd/Sub/Mul/Div
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Arith {
    Add,
    Sub,
    Mul,
    Div,
}

fn arith32(op: Arith, a: u32, b: u32) -> u32 {
    let (x, y) = (
        std::hint::black_box(f32::from_bits(a)),
        std::hint::black_box(f32::from_bits(b)),
    );
    std::hint::black_box(match op {
        Arith::Add => x + y,
        Arith::Sub => x - y,
        Arith::Mul => x * y,
        Arith::Div => x / y,
    })
    .to_bits()
}

fn arith64(op: Arith, a: u64, b: u64) -> u64 {
    let (x, y) = (
        std::hint::black_box(f64::from_bits(a)),
        std::hint::black_box(f64::from_bits(b)),
    );
    std::hint::black_box(match op {
        Arith::Add => x + y,
        Arith::Sub => x - y,
        Arith::Mul => x * y,
        Arith::Div => x / y,
    })
    .to_bits()
}

fn arm_nan_lane(fsize: usize, dn: bool, a: u64, b: u64, host: u64) -> u64 {
    let quiet = if fsize == 32 {
        0x0040_0000
    } else {
        0x0008_0000_0000_0000
    };
    let default_nan = if fsize == 32 {
        0x7fc0_0000
    } else {
        0x7ff8_0000_0000_0000
    };
    let selected = if is_snan(a, fsize) {
        Some(a | quiet)
    } else if is_snan(b, fsize) {
        Some(b | quiet)
    } else if is_nan(a, fsize) {
        Some(a)
    } else if is_nan(b, fsize) {
        Some(b)
    } else {
        None
    };
    match selected {
        Some(_) if dn => default_nan,
        Some(value) => value,
        None if is_nan(host, fsize) => default_nan,
        None => host,
    }
}

fn arith_oracle(op: Arith, fsize: usize) -> impl Fn(u32, &[Vector]) -> (Vector, u32) {
    move |fpcr, inputs| {
        let dn = fpcr & (1 << 25) != 0;
        let la = lanes(&inputs[0], fsize);
        let lb = lanes(&inputs[1], fsize);
        let (host, flags) = under_guest_mxcsr(fpcr, || {
            la.iter()
                .zip(&lb)
                .map(|(&x, &y)| match fsize {
                    32 => arith32(op, x as u32, y as u32) as u64,
                    _ => arith64(op, x, y),
                })
                .collect::<Vec<u64>>()
        });
        let out: Vec<u64> = (0..la.len())
            .map(|i| arm_nan_lane(fsize, dn, la[i], lb[i], host[i]))
            .collect();
        let v = match fsize {
            32 => [out[0] | (out[1] << 32), out[2] | (out[3] << 32)],
            _ => [out[0], out[1]],
        };
        (v, flags)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// FADDP V0.4S, V1.4S, V2.4S / FADDP V0.2D, V1.2D, V2.2D
#[test]
fn fp_vector_paired_add32_matches_fallback() {
    let oracle = three_op_fallback_oracle(fallback_fp_paired_add32);
    diff_test("FPVectorPairedAdd32", 0x6E22_D420, 32, 2, true, &oracle);
}

#[test]
fn fp_vector_paired_add64_matches_fallback() {
    let oracle = three_op_fallback_oracle(fallback_fp_paired_add64);
    diff_test("FPVectorPairedAdd64", 0x6E62_D420, 64, 2, true, &oracle);
}

// FADDP V0.2S, V1.2S, V2.2S (FPVectorPairedAddLower32, upper half zeroed)
#[test]
fn fp_vector_paired_add_lower32_matches_fallback() {
    let oracle = three_op_fallback_oracle(fallback_fp_paired_add_lower32);
    diff_test(
        "FPVectorPairedAddLower32",
        0x2E22_D420,
        32,
        2,
        true,
        &oracle,
    );
}

// FADD/FSUB/FMUL/FDIV V0.4S, V1.4S, V2.4S and .2D
#[test]
fn fp_vector_add32_matches_arm_nan_model() {
    diff_test(
        "FPVectorAdd32",
        0x4E22_D420,
        32,
        2,
        true,
        &arith_oracle(Arith::Add, 32),
    );
}

#[test]
fn fp_vector_add64_matches_arm_nan_model() {
    diff_test(
        "FPVectorAdd64",
        0x4E62_D420,
        64,
        2,
        true,
        &arith_oracle(Arith::Add, 64),
    );
}

#[test]
fn fp_vector_sub32_matches_arm_nan_model() {
    diff_test(
        "FPVectorSub32",
        0x4EA2_D420,
        32,
        2,
        true,
        &arith_oracle(Arith::Sub, 32),
    );
}

#[test]
fn fp_vector_sub64_matches_arm_nan_model() {
    diff_test(
        "FPVectorSub64",
        0x4EE2_D420,
        64,
        2,
        true,
        &arith_oracle(Arith::Sub, 64),
    );
}

#[test]
fn fp_vector_mul32_matches_arm_nan_model() {
    diff_test(
        "FPVectorMul32",
        0x6E22_DC20,
        32,
        2,
        true,
        &arith_oracle(Arith::Mul, 32),
    );
}

#[test]
fn fp_vector_mul64_matches_arm_nan_model() {
    diff_test(
        "FPVectorMul64",
        0x6E62_DC20,
        64,
        2,
        true,
        &arith_oracle(Arith::Mul, 64),
    );
}

#[test]
fn fp_vector_div32_matches_arm_nan_model() {
    diff_test(
        "FPVectorDiv32",
        0x6E22_FC20,
        32,
        2,
        true,
        &arith_oracle(Arith::Div, 32),
    );
}

#[test]
fn fp_vector_div64_matches_arm_nan_model() {
    diff_test(
        "FPVectorDiv64",
        0x6E62_FC20,
        64,
        2,
        true,
        &arith_oracle(Arith::Div, 64),
    );
}

// Former unit tests of the legacy paired-add fallback, kept as oracle checks.
#[test]
fn legacy_paired_add_fallback_reference_values() {
    let a: [u8; 16] = unsafe { std::mem::transmute([1.0f32, 2.0f32, 3.0f32, 4.0f32]) };
    let b: [u8; 16] = unsafe { std::mem::transmute([5.0f32, 6.0f32, 7.0f32, 8.0f32]) };
    let mut result = [0u8; 16];
    let mut fpsr = 0;
    fallback_fp_paired_add32(&mut result, &a, &b, 0, &mut fpsr);
    let out: [f32; 4] = unsafe { std::mem::transmute(result) };
    assert_eq!(out, [3.0, 7.0, 11.0, 15.0]);

    let a: [u8; 16] =
        unsafe { std::mem::transmute([0x7f81_7182u32, 0x7ffe_cfe5, 0x3f80_0000, 0x4000_0000]) };
    let mut fpsr = 0;
    fallback_fp_paired_add32(&mut result, &a, &[0u8; 16], 0, &mut fpsr);
    let out: [u32; 4] = unsafe { std::mem::transmute(result) };
    assert_eq!(out[0], 0x7fc1_7182);
    assert_eq!(fpsr & 1, 1);
}
