//! FPVectorFromSignedFixed / FPVectorFromUnsignedFixed (Eden
//! emit_x64_vector_floating_point.cpp EmitFPVectorFrom{Signed,Unsigned}Fixed{32,64})
//! against the legacy rdynarmic fallbacks.

use super::*;

// ---------------------------------------------------------------------------
// Legacy rdynarmic fallbacks, verbatim (emit_fp_vector_convert.rs before the port).
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

type FromFixedFallback = extern "C" fn(*mut [u8; 16], *const [u8; 16], u8);

/// Oracle: the legacy fallback under the guest MXCSR, with its two known
/// defects corrected (both are fixed by the port, which matches upstream):
/// - fbits == 64 (64-bit lanes): `1u64 << 64` wrapped to a scale of 1; the
///   reference is the fbits == 0 conversion times 2^-64 (exact scaling);
/// - unsigned inputs under FPCR.RMode == RM: LLVM lowers `u32/u64 as f32/f64`
///   with a magic-number subtraction that yields -0.0 for 0; UCVTF gives +0.0
///   (upstream clears the sign bit, `pand 0x7FFF...`). Unsigned results are
///   never negative, so the oracle clears the sign bit.
fn from_fixed_oracle(
    fallback: FromFixedFallback,
    fbits: u8,
    fsize: usize,
    unsigned: bool,
) -> impl Fn(u32, &[Vector]) -> (Vector, u32) {
    move |fpcr, inputs| {
        let a = vec_to_bytes(&inputs[0]);
        let mut out = [0u8; 16];
        let f = std::hint::black_box(fallback);
        let legacy_fbits = if fbits == 64 { 0 } else { fbits };
        let ((), flags) = under_guest_mxcsr(fpcr, || f(&mut out, &a, legacy_fbits));
        let mut v = bytes_to_vec(&out);
        if fbits == 64 {
            for lane in v.iter_mut() {
                *lane = (f64::from_bits(*lane) * 2f64.powi(-64)).to_bits();
            }
        }
        if unsigned {
            let mask = if fsize == 32 {
                0x7fff_ffff_7fff_ffff
            } else {
                0x7fff_ffff_ffff_ffff
            };
            v = [v[0] & mask, v[1] & mask];
        }
        (v, flags)
    }
}

/// SCVTF/UCVTF (vector, fixed-point) Vd, Vn, #fbits: 0 Q U 011110 immh:immb 111001 Rn Rd.
fn cvtf_fixed(unsigned: bool, fsize: usize, fbits: u32) -> u32 {
    let immhb = match fsize {
        32 => 64 - fbits,
        _ => 128 - fbits,
    };
    0x4F00_E400 | ((unsigned as u32) << 29) | (immhb << 16) | (1 << 5)
}

/// SCVTF/UCVTF (vector, integer) Vd.<T>, Vn.<T> (fbits = 0).
fn cvtf_integer(unsigned: bool, fsize: usize) -> u32 {
    0x4E21_D820 | ((unsigned as u32) << 29) | (((fsize == 64) as u32) << 22)
}

fn run_from_fixed(unsigned: bool, fsize: usize, fallback: FromFixedFallback) {
    let name = format!(
        "FPVectorFrom{}Fixed{fsize}",
        if unsigned { "Unsigned" } else { "Signed" }
    );
    let oracle = from_fixed_oracle(fallback, 0, fsize, unsigned);
    diff_test(
        &name,
        cvtf_integer(unsigned, fsize),
        fsize,
        1,
        true,
        &oracle,
    );
    let max_fbits = fsize as u32;
    for fbits in [
        1,
        2,
        7,
        8,
        15,
        16,
        23,
        24,
        31,
        max_fbits / 2 + 1,
        max_fbits - 1,
        max_fbits,
    ] {
        let oracle = from_fixed_oracle(fallback, fbits as u8, fsize, unsigned);
        diff_test(
            &format!("{name}#{fbits}"),
            cvtf_fixed(unsigned, fsize, fbits),
            fsize,
            1,
            true,
            &oracle,
        );
    }
}

#[test]
fn fp_vector_from_signed_fixed32_matches_fallback() {
    run_from_fixed(false, 32, fallback_fp_from_signed_fixed32);
}

#[test]
fn fp_vector_from_signed_fixed64_matches_fallback() {
    run_from_fixed(false, 64, fallback_fp_from_signed_fixed64);
}

#[test]
fn fp_vector_from_unsigned_fixed32_matches_fallback() {
    run_from_fixed(true, 32, fallback_fp_from_unsigned_fixed32);
}

#[test]
fn fp_vector_from_unsigned_fixed64_matches_fallback() {
    run_from_fixed(true, 64, fallback_fp_from_unsigned_fixed64);
}
