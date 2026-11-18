//! FCVTXN (scalar and vector): `FPDoubleToSingle(value, RoundingMode::ToOdd)`
//! (Eden simd_scalar_two_register_misc.cpp:167, simd_two_register_misc.cpp:440).
//! Upstream `EmitFPDoubleToSingle` casts the rounding immediate to
//! `FP::RoundingMode` and sends ToOdd to `FP::FPConvert<u32, u64>`; the
//! expectations below follow the ARM FPRound rules for round-to-odd: the
//! truncated result with its lowest bit set when inexact, and the largest
//! finite value (OFC and IXC) on overflow.

use super::{feature_sets, Harness};

const FCVTXN_S0_D1: u32 = 0x7E61_6820;
const FCVTXN_V0_2S_V1_2D: u32 = 0x2E61_6820;

const IXC: u32 = 1 << 4;
const OFC: u32 = 1 << 2;
const IOC: u32 = 1 << 0;

/// (double input, single result, FPSR cumulative bits) with FPCR.DN = 0, FZ = 0.
const CASES: &[(u64, u32, u32)] = &[
    (0x3FF0_0000_0000_0000, 0x3F80_0000, 0), // 1.0: exact
    (0x3FF0_0000_0040_0000, 0x3F80_0001, IXC), // 1 + 2^-30: truncated 1.0, lowest bit set
    (0x3FF0_0000_1000_0000, 0x3F80_0001, IXC), // 1 + 2^-24: halfway, still odd (not to even)
    (0x3FF0_0000_2040_0000, 0x3F80_0001, IXC), // 1 + 2^-23 + 2^-30: already odd
    (0x3FF0_0000_4040_0000, 0x3F80_0003, IXC), // 1 + 2^-22 + 2^-30: 0x...02 -> 0x...03
    (0xBFF0_0000_0040_0000, 0xBF80_0001, IXC), // negative: same magnitude rule
    (0x7FF0_0000_0000_0000, 0x7F80_0000, 0),   // +inf
    (0xFFF0_0000_0000_0000, 0xFF80_0000, 0),   // -inf
    (0x7E37_E43C_8800_759C, 0x7F7F_FFFF, OFC | IXC), // 1e300: overflow, max normal
    (0xFE37_E43C_8800_759C, 0xFF7F_FFFF, OFC | IXC), // -1e300
    (0x7FF8_0000_0000_0001, 0x7FC0_0000, 0),   // qNaN: payload top bits kept
    (0x7FF0_0000_2000_0000, 0x7FC0_0001, IOC), // sNaN: quietened, IOC
];

/// FPCR values: FCVTXN ignores FPCR.RMode, so the conversion must be the same
/// under every rounding mode.
const FPCRS: &[u32] = &[0, 1 << 22, 2 << 22, 3 << 22];

#[test]
fn fcvtxn_scalar_rounds_to_odd() {
    for (features_name, features) in feature_sets() {
        let mut harness = Harness::new(&[FCVTXN_S0_D1, 0xD400_0001], features);
        for &fpcr in FPCRS {
            for &(input, want, want_fpsr) in CASES {
                let v0 = [0xdead_beef_dead_beef, 0x0123_4567_89ab_cdef];
                let (got, fpsr) = harness.run(fpcr, v0, &[[input, 0x5555_5555_5555_5555]]);
                assert_eq!(
                    (got, fpsr & 0x1F),
                    ([want as u64, 0], want_fpsr),
                    "FCVTXN S0, D1 [{features_name}] fpcr={fpcr:#x} input={input:#018x}"
                );
            }
        }
    }
}

#[test]
fn fcvtxn_vector_rounds_to_odd() {
    for (features_name, features) in feature_sets() {
        let mut harness = Harness::new(&[FCVTXN_V0_2S_V1_2D, 0xD400_0001], features);
        for &fpcr in FPCRS {
            for &(lo, want_lo, fpsr_lo) in CASES {
                for &(hi, want_hi, fpsr_hi) in CASES {
                    let v0 = [0xdead_beef_dead_beef, 0x0123_4567_89ab_cdef];
                    let (got, fpsr) = harness.run(fpcr, v0, &[[lo, hi]]);
                    assert_eq!(
                        (got, fpsr & 0x1F),
                        ([(want_hi as u64) << 32 | want_lo as u64, 0], fpsr_lo | fpsr_hi),
                        "FCVTXN V0.2S, V1.2D [{features_name}] fpcr={fpcr:#x} input={lo:#018x},{hi:#018x}"
                    );
                }
            }
        }
    }
}
