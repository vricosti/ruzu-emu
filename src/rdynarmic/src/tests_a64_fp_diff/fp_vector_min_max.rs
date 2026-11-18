//! FPVectorMax / FPVectorMin (Eden emit_x64_vector_floating_point.cpp
//! EmitFPVectorMinMax) against the legacy rdynarmic fallbacks.

use super::fp_vector_three_op::three_op_fallback_oracle;
use super::*;
use crate::backend::x64::emit_floating_point::fp_min_max;
use crate::common::fp::fpcr::Fpcr;
use crate::common::fp::fpsr::Fpsr;
use crate::common::fp::info::FloatFormat;

/// Legacy rdynarmic fallback (`define_fp_vector_min_max_fallback!`), verbatim.
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

// FMAX / FMIN V0.4S, V1.4S, V2.4S and .2D
#[test]
fn fp_vector_max32_matches_fallback() {
    let oracle = three_op_fallback_oracle(fallback_fp_max32);
    diff_test("FPVectorMax32", 0x4E22_F420, 32, 2, true, &oracle);
}

#[test]
fn fp_vector_max64_matches_fallback() {
    let oracle = three_op_fallback_oracle(fallback_fp_max64);
    diff_test("FPVectorMax64", 0x4E62_F420, 64, 2, true, &oracle);
}

#[test]
fn fp_vector_min32_matches_fallback() {
    let oracle = three_op_fallback_oracle(fallback_fp_min32);
    diff_test("FPVectorMin32", 0x4EA2_F420, 32, 2, true, &oracle);
}

#[test]
fn fp_vector_min64_matches_fallback() {
    let oracle = three_op_fallback_oracle(fallback_fp_min64);
    diff_test("FPVectorMin64", 0x4EE2_F420, 64, 2, true, &oracle);
}
