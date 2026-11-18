//! FPVectorEqual / FPVectorGreater / FPVectorGreaterEqual
//! (Eden emit_x64_vector_floating_point.cpp EmitFPVectorEqual32/64,
//! EmitFPVectorGreater32/64, EmitFPVectorGreaterEqual32/64) against the
//! legacy rdynarmic fallbacks.

use super::*;

/// Legacy rdynarmic fallback (`define_fp_vector_compare!`), verbatim.
macro_rules! define_fp_vector_compare {
    ($name:ident, $ty:ty, $count:expr, $mask_ty:ty, $op:tt) => {
        extern "C" fn $name(result: *mut [u8; 16], a: *const [u8; 16], b: *const [u8; 16]) {
            unsafe {
                let va: [$ty; $count] = std::mem::transmute(*a);
                let vb: [$ty; $count] = std::mem::transmute(*b);
                let mut out = [0 as $mask_ty; $count];
                for i in 0..$count {
                    out[i] = if va[i] $op vb[i] { !0 } else { 0 };
                }
                *result = std::mem::transmute(out);
            }
        }
    };
}

define_fp_vector_compare!(fallback_fp_vector_equal32, f32, 4, u32, ==);
define_fp_vector_compare!(fallback_fp_vector_equal64, f64, 2, u64, ==);
define_fp_vector_compare!(fallback_fp_vector_greater32, f32, 4, u32, >);
define_fp_vector_compare!(fallback_fp_vector_greater64, f64, 2, u64, >);
define_fp_vector_compare!(fallback_fp_vector_greater_equal32, f32, 4, u32, >=);
define_fp_vector_compare!(fallback_fp_vector_greater_equal64, f64, 2, u64, >=);

type CompareFallback = extern "C" fn(*mut [u8; 16], *const [u8; 16], *const [u8; 16]);

/// Compare oracle: result of the legacy fallback under the guest MXCSR;
/// FPSR from the ARM model (FCMEQ: IOC on a signalling NaN operand;
/// FCMGT/FCMGE: IOC on any NaN operand).
fn compare_oracle(
    fallback: CompareFallback,
    fsize: usize,
    signal_on_qnan: bool,
) -> impl Fn(u32, &[Vector]) -> (Vector, u32) {
    move |fpcr, inputs| {
        let a = vec_to_bytes(&inputs[0]);
        let b = vec_to_bytes(&inputs[1]);
        let mut out = [0u8; 16];
        let f = std::hint::black_box(fallback);
        under_guest_mxcsr(fpcr, || f(&mut out, &a, &b));
        let la = lanes(&inputs[0], fsize);
        let lb = lanes(&inputs[1], fsize);
        let invalid = la.iter().zip(&lb).any(|(&x, &y)| {
            if signal_on_qnan {
                is_nan(x, fsize) || is_nan(y, fsize)
            } else {
                is_snan(x, fsize) || is_snan(y, fsize)
            }
        });
        (bytes_to_vec(&out), invalid as u32)
    }
}

// FCMEQ V0.4S, V1.4S, V2.4S / FCMEQ V0.2D, V1.2D, V2.2D
#[test]
fn fp_vector_equal32_matches_fallback() {
    let oracle = compare_oracle(fallback_fp_vector_equal32, 32, false);
    diff_test("FPVectorEqual32", 0x4E22_E420, 32, 2, true, &oracle);
}

#[test]
fn fp_vector_equal64_matches_fallback() {
    let oracle = compare_oracle(fallback_fp_vector_equal64, 64, false);
    diff_test("FPVectorEqual64", 0x4E62_E420, 64, 2, true, &oracle);
}

// FCMGT V0.4S, V1.4S, V2.4S / .2D
#[test]
fn fp_vector_greater32_matches_fallback() {
    let oracle = compare_oracle(fallback_fp_vector_greater32, 32, true);
    diff_test("FPVectorGreater32", 0x6EA2_E420, 32, 2, true, &oracle);
}

#[test]
fn fp_vector_greater64_matches_fallback() {
    let oracle = compare_oracle(fallback_fp_vector_greater64, 64, true);
    diff_test("FPVectorGreater64", 0x6EE2_E420, 64, 2, true, &oracle);
}

// FCMGE V0.4S, V1.4S, V2.4S / .2D
#[test]
fn fp_vector_greater_equal32_matches_fallback() {
    let oracle = compare_oracle(fallback_fp_vector_greater_equal32, 32, true);
    diff_test("FPVectorGreaterEqual32", 0x6E22_E420, 32, 2, true, &oracle);
}

#[test]
fn fp_vector_greater_equal64_matches_fallback() {
    let oracle = compare_oracle(fallback_fp_vector_greater_equal64, 64, true);
    diff_test("FPVectorGreaterEqual64", 0x6E62_E420, 64, 2, true, &oracle);
}
