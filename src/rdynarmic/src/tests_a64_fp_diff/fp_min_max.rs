//! FPMax / FPMin scalar (Eden emit_x64_floating_point.cpp EmitFPMinMax)
//! against the legacy rdynarmic fallbacks.

use super::*;
use crate::backend::x64::emit_floating_point::fp_min_max;
use crate::common::fp::fpcr::Fpcr;
use crate::common::fp::fpsr::Fpsr;

/// Legacy rdynarmic fallback (`define_fp_min_max_fallback!`), verbatim.
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

/// Scalar oracle: legacy fallback on lane 0, upper bits zero, FPSR =
/// fallback FPSR | flags raised by the fallback's host arithmetic.
fn scalar_oracle32(
    fallback: extern "C" fn(u32, u32, u32, *mut u32) -> u32,
) -> impl Fn(u32, &[Vector]) -> (Vector, u32) {
    move |fpcr, inputs| {
        let mut fpsr = 0u32;
        let f = std::hint::black_box(fallback);
        let (r, flags) = under_guest_mxcsr(fpcr, || {
            f(inputs[0][0] as u32, inputs[1][0] as u32, fpcr, &mut fpsr)
        });
        ([r as u64, 0], fpsr | flags)
    }
}

fn scalar_oracle64(
    fallback: extern "C" fn(u64, u64, u32, *mut u32) -> u64,
) -> impl Fn(u32, &[Vector]) -> (Vector, u32) {
    move |fpcr, inputs| {
        let mut fpsr = 0u32;
        let f = std::hint::black_box(fallback);
        let (r, flags) = under_guest_mxcsr(fpcr, || f(inputs[0][0], inputs[1][0], fpcr, &mut fpsr));
        ([r, 0], fpsr | flags)
    }
}

// FMAX S0, S1, S2 / FMIN S0, S1, S2 / FMAX D0, D1, D2 / FMIN D0, D1, D2
#[test]
fn fp_max32_matches_fallback() {
    let oracle = scalar_oracle32(fallback_fp_max32);
    diff_test("FPMax32", 0x1E22_4820, 32, 2, true, &oracle);
}

#[test]
fn fp_min32_matches_fallback() {
    let oracle = scalar_oracle32(fallback_fp_min32);
    diff_test("FPMin32", 0x1E22_5820, 32, 2, true, &oracle);
}

#[test]
fn fp_max64_matches_fallback() {
    let oracle = scalar_oracle64(fallback_fp_max64);
    diff_test("FPMax64", 0x1E62_4820, 64, 2, true, &oracle);
}

#[test]
fn fp_min64_matches_fallback() {
    let oracle = scalar_oracle64(fallback_fp_min64);
    diff_test("FPMin64", 0x1E62_5820, 64, 2, true, &oracle);
}
