// SPDX-FileCopyrightText: Copyright 2026 Eden Emulator Project
// SPDX-License-Identifier: GPL-3.0-or-later

//! Constants used by x64 vector and floating-point instructions.
//!
//! This is the Rust counterpart of Dynarmic's `backend/x64/constants.h`.

use crate::common::fp::rounding_mode::RoundingMode;

/// Redefinitions of the `_MM_CMP_*` immediates used by `vcmp`.
pub mod cmp {
    pub const EQUAL_OQ: u8 = 0;
    pub const LESS_THAN_OS: u8 = 1;
    pub const LESS_EQUAL_OS: u8 = 2;
    pub const UNORDERED_Q: u8 = 3;
    pub const NOT_EQUAL_UQ: u8 = 4;
    pub const NOT_LESS_THAN_US: u8 = 5;
    pub const NOT_LESS_EQUAL_US: u8 = 6;
    pub const ORDERED_Q: u8 = 7;
    pub const EQUAL_UQ: u8 = 8;
    pub const NOT_GREATER_EQUAL_US: u8 = 9;
    pub const NOT_GREATER_THAN_US: u8 = 10;
    pub const FALSE_OQ: u8 = 11;
    pub const NOT_EQUAL_OQ: u8 = 12;
    pub const GREATER_EQUAL_OS: u8 = 13;
    pub const GREATER_THAN_OS: u8 = 14;
    pub const TRUE_UQ: u8 = 15;
    pub const EQUAL_OS: u8 = 16;
    pub const LESS_THAN_OQ: u8 = 17;
    pub const LESS_EQUAL_OQ: u8 = 18;
    pub const UNORDERED_S: u8 = 19;
    pub const NOT_EQUAL_US: u8 = 20;
    pub const NOT_LESS_THAN_UQ: u8 = 21;
    pub const NOT_LESS_EQUAL_UQ: u8 = 22;
    pub const ORDERED_S: u8 = 23;
    pub const EQUAL_US: u8 = 24;
    pub const NOT_GREATER_EQUAL_UQ: u8 = 25;
    pub const NOT_GREATER_THAN_UQ: u8 = 26;
    pub const FALSE_OS: u8 = 27;
    pub const NOT_EQUAL_OS: u8 = 28;
    pub const GREATER_EQUAL_OQ: u8 = 29;
    pub const GREATER_THAN_OQ: u8 = 30;
    pub const TRUE_US: u8 = 31;
}

/// Redefinitions of the `_MM_CMPINT_*` immediates used by `vpcmp`.
pub mod cmp_int {
    pub const EQUAL: u8 = 0x0;
    pub const LESS_THAN: u8 = 0x1;
    pub const LESS_EQUAL: u8 = 0x2;
    pub const FALSE: u8 = 0x3;
    pub const NOT_EQUAL: u8 = 0x4;
    pub const NOT_LESS_THAN: u8 = 0x5;
    pub const GREATER_EQUAL: u8 = 0x5;
    pub const NOT_LESS_EQUAL: u8 = 0x6;
    pub const GREATER_THAN: u8 = 0x6;
    pub const TRUE: u8 = 0x7;
}

/// Terms used to construct `vpternlog` truth tables.
pub mod tern {
    pub const A: u8 = 0b1111_0000;
    pub const B: u8 = 0b1100_1100;
    pub const C: u8 = 0b1010_1010;
}

/// Bitmask values used by `vfpclass`.
pub mod fp_class {
    pub const QNAN: u8 = 0b0000_0001;
    pub const ZERO_POS: u8 = 0b0000_0010;
    pub const ZERO_NEG: u8 = 0b0000_0100;
    pub const INF_POS: u8 = 0b0000_1000;
    pub const INF_NEG: u8 = 0b0001_0000;
    pub const DENORMAL: u8 = 0b0010_0000;
    pub const NEGATIVE: u8 = 0b0100_0000;
    pub const SNAN: u8 = 0b1000_0000;
}

/// Opcodes used by `vfixupimm`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FpFixup {
    Dest = 0b0000,
    NormSrc = 0b0001,
    QNaNSrc = 0b0010,
    IndefNaN = 0b0011,
    NegInf = 0b0100,
    PosInf = 0b0101,
    InfSrc = 0b0110,
    NegZero = 0b0111,
    PosZero = 0b1000,
    NegOne = 0b1001,
    PosOne = 0b1010,
    Half = 0b1011,
    Ninety = 0b1100,
    HalfPi = 0b1101,
    PosMax = 0b1110,
    NegMax = 0b1111,
}

/// Generates the 32-bit LUT immediate for `vfixupimm`.
#[allow(clippy::too_many_arguments)]
pub const fn fixup_lut(
    src_qnan: FpFixup,
    src_snan: FpFixup,
    src_zero: FpFixup,
    src_posone: FpFixup,
    src_neginf: FpFixup,
    src_posinf: FpFixup,
    src_neg: FpFixup,
    src_pos: FpFixup,
) -> u32 {
    (src_qnan as u32)
        | ((src_snan as u32) << 4)
        | ((src_zero as u32) << 8)
        | ((src_posone as u32) << 12)
        | ((src_neginf as u32) << 16)
        | ((src_posinf as u32) << 20)
        | ((src_neg as u32) << 24)
        | ((src_pos as u32) << 28)
}

/// Value selection used by `vrange*`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FpRangeSelect {
    Min = 0b00,
    Max = 0b01,
    AbsMin = 0b10,
    AbsMax = 0b11,
}

/// Sign selection used by `vrange*`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FpRangeSign {
    A = 0b00,
    Preserve = 0b01,
    Positive = 0b10,
    Negative = 0b11,
}

/// Generates the 8-bit immediate for `vrange*`.
pub const fn fp_range_lut(range_select: FpRangeSelect, range_sign: FpRangeSign) -> u8 {
    range_select as u8 | ((range_sign as u8) << 2)
}

pub const fn convert_rounding_mode_to_x64_immediate(rounding_mode: RoundingMode) -> Option<i32> {
    match rounding_mode {
        RoundingMode::ToNearestTieEven => Some(0b00),
        RoundingMode::TowardsPlusInfinity => Some(0b10),
        RoundingMode::TowardsMinusInfinity => Some(0b01),
        RoundingMode::TowardsZero => Some(0b11),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predicate_inventory_matches_upstream_encodings() {
        assert_eq!(
            [
                cmp::EQUAL_OQ,
                cmp::LESS_THAN_OS,
                cmp::LESS_EQUAL_OS,
                cmp::UNORDERED_Q,
                cmp::NOT_EQUAL_UQ,
                cmp::NOT_LESS_THAN_US,
                cmp::NOT_LESS_EQUAL_US,
                cmp::ORDERED_Q,
                cmp::EQUAL_UQ,
                cmp::NOT_GREATER_EQUAL_US,
                cmp::NOT_GREATER_THAN_US,
                cmp::FALSE_OQ,
                cmp::NOT_EQUAL_OQ,
                cmp::GREATER_EQUAL_OS,
                cmp::GREATER_THAN_OS,
                cmp::TRUE_UQ,
                cmp::EQUAL_OS,
                cmp::LESS_THAN_OQ,
                cmp::LESS_EQUAL_OQ,
                cmp::UNORDERED_S,
                cmp::NOT_EQUAL_US,
                cmp::NOT_LESS_THAN_UQ,
                cmp::NOT_LESS_EQUAL_UQ,
                cmp::ORDERED_S,
                cmp::EQUAL_US,
                cmp::NOT_GREATER_EQUAL_UQ,
                cmp::NOT_GREATER_THAN_UQ,
                cmp::FALSE_OS,
                cmp::NOT_EQUAL_OS,
                cmp::GREATER_EQUAL_OQ,
                cmp::GREATER_THAN_OQ,
                cmp::TRUE_US,
            ],
            std::array::from_fn::<_, 32, _>(|value| value as u8)
        );
        assert_eq!(
            [
                cmp_int::EQUAL,
                cmp_int::LESS_THAN,
                cmp_int::LESS_EQUAL,
                cmp_int::FALSE,
                cmp_int::NOT_EQUAL,
                cmp_int::NOT_LESS_THAN,
                cmp_int::GREATER_EQUAL,
                cmp_int::NOT_LESS_EQUAL,
                cmp_int::GREATER_THAN,
                cmp_int::TRUE,
            ],
            [0, 1, 2, 3, 4, 5, 5, 6, 6, 7]
        );
    }

    #[test]
    fn truth_terms_classes_and_fixup_opcodes_match_upstream() {
        assert_eq!([tern::A, tern::B, tern::C], [0xf0, 0xcc, 0xaa]);
        assert_eq!(
            [
                fp_class::QNAN,
                fp_class::ZERO_POS,
                fp_class::ZERO_NEG,
                fp_class::INF_POS,
                fp_class::INF_NEG,
                fp_class::DENORMAL,
                fp_class::NEGATIVE,
                fp_class::SNAN,
            ],
            [1, 2, 4, 8, 16, 32, 64, 128]
        );
        assert_eq!(
            [
                FpFixup::Dest as u8,
                FpFixup::NormSrc as u8,
                FpFixup::QNaNSrc as u8,
                FpFixup::IndefNaN as u8,
                FpFixup::NegInf as u8,
                FpFixup::PosInf as u8,
                FpFixup::InfSrc as u8,
                FpFixup::NegZero as u8,
                FpFixup::PosZero as u8,
                FpFixup::NegOne as u8,
                FpFixup::PosOne as u8,
                FpFixup::Half as u8,
                FpFixup::Ninety as u8,
                FpFixup::HalfPi as u8,
                FpFixup::PosMax as u8,
                FpFixup::NegMax as u8,
            ],
            std::array::from_fn::<_, 16, _>(|value| value as u8)
        );
    }

    #[test]
    fn fixup_lut_places_each_opcode_in_its_upstream_nibble() {
        assert_eq!(
            fixup_lut(
                FpFixup::Dest,
                FpFixup::NormSrc,
                FpFixup::QNaNSrc,
                FpFixup::IndefNaN,
                FpFixup::NegInf,
                FpFixup::PosInf,
                FpFixup::InfSrc,
                FpFixup::NegZero,
            ),
            0x7654_3210
        );
    }

    #[test]
    fn range_lut_matches_upstream_bit_placement() {
        assert_eq!(
            fp_range_lut(FpRangeSelect::AbsMax, FpRangeSign::Negative),
            0b1111
        );
        assert_eq!(
            fp_range_lut(FpRangeSelect::Max, FpRangeSign::Preserve),
            0b0101
        );
    }

    #[test]
    fn x64_rounding_immediates_match_mxcsr_encoding() {
        assert_eq!(
            convert_rounding_mode_to_x64_immediate(RoundingMode::ToNearestTieEven),
            Some(0b00)
        );
        assert_eq!(
            convert_rounding_mode_to_x64_immediate(RoundingMode::TowardsPlusInfinity),
            Some(0b10)
        );
        assert_eq!(
            convert_rounding_mode_to_x64_immediate(RoundingMode::TowardsMinusInfinity),
            Some(0b01)
        );
        assert_eq!(
            convert_rounding_mode_to_x64_immediate(RoundingMode::TowardsZero),
            Some(0b11)
        );
        assert_eq!(
            convert_rounding_mode_to_x64_immediate(RoundingMode::ToNearestTieAwayFromZero),
            None
        );
        assert_eq!(
            convert_rounding_mode_to_x64_immediate(RoundingMode::ToOdd),
            None
        );
    }
}
