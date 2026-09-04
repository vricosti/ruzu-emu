// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Port of `frontend/ir/flow_test.h` and `frontend/ir/flow_test.cpp`
//!
//! Flow test conditions used in Maxwell branch instructions.

use std::fmt;

/// Flow test conditions for Maxwell branch instructions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u64)]
pub enum FlowTest {
    F = 0,
    LT = 1,
    EQ = 2,
    LE = 3,
    GT = 4,
    NE = 5,
    GE = 6,
    NUM = 7,
    NaN = 8,
    LTU = 9,
    EQU = 10,
    LEU = 11,
    GTU = 12,
    NEU = 13,
    GEU = 14,
    T = 15,
    OFF = 16,
    LO = 17,
    SFF = 18,
    LS = 19,
    HI = 20,
    SFT = 21,
    HS = 22,
    OFT = 23,
    CsmTa = 24,
    CsmTr = 25,
    CsmMx = 26,
    FcsmTa = 27,
    FcsmTr = 28,
    FcsmMx = 29,
    RLE = 30,
    RGT = 31,
}

impl FlowTest {
    pub fn from_u64(val: u64) -> Option<Self> {
        match val {
            0 => Some(FlowTest::F),
            1 => Some(FlowTest::LT),
            2 => Some(FlowTest::EQ),
            3 => Some(FlowTest::LE),
            4 => Some(FlowTest::GT),
            5 => Some(FlowTest::NE),
            6 => Some(FlowTest::GE),
            7 => Some(FlowTest::NUM),
            8 => Some(FlowTest::NaN),
            9 => Some(FlowTest::LTU),
            10 => Some(FlowTest::EQU),
            11 => Some(FlowTest::LEU),
            12 => Some(FlowTest::GTU),
            13 => Some(FlowTest::NEU),
            14 => Some(FlowTest::GEU),
            15 => Some(FlowTest::T),
            16 => Some(FlowTest::OFF),
            17 => Some(FlowTest::LO),
            18 => Some(FlowTest::SFF),
            19 => Some(FlowTest::LS),
            20 => Some(FlowTest::HI),
            21 => Some(FlowTest::SFT),
            22 => Some(FlowTest::HS),
            23 => Some(FlowTest::OFT),
            24 => Some(FlowTest::CsmTa),
            25 => Some(FlowTest::CsmTr),
            26 => Some(FlowTest::CsmMx),
            27 => Some(FlowTest::FcsmTa),
            28 => Some(FlowTest::FcsmTr),
            29 => Some(FlowTest::FcsmMx),
            30 => Some(FlowTest::RLE),
            31 => Some(FlowTest::RGT),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            FlowTest::F => "F",
            FlowTest::LT => "LT",
            FlowTest::EQ => "EQ",
            FlowTest::LE => "LE",
            FlowTest::GT => "GT",
            FlowTest::NE => "NE",
            FlowTest::GE => "GE",
            FlowTest::NUM => "NUM",
            FlowTest::NaN => "NAN",
            FlowTest::LTU => "LTU",
            FlowTest::EQU => "EQU",
            FlowTest::LEU => "LEU",
            FlowTest::GTU => "GTU",
            FlowTest::NEU => "NEU",
            FlowTest::GEU => "GEU",
            FlowTest::T => "T",
            FlowTest::OFF => "OFF",
            FlowTest::LO => "LO",
            FlowTest::SFF => "SFF",
            FlowTest::LS => "LS",
            FlowTest::HI => "HI",
            FlowTest::SFT => "SFT",
            FlowTest::HS => "HS",
            FlowTest::OFT => "OFT",
            FlowTest::CsmTa => "CSM_TA",
            FlowTest::CsmTr => "CSM_TR",
            FlowTest::CsmMx => "CSM_MX",
            FlowTest::FcsmTa => "FCSM_TA",
            FlowTest::FcsmTr => "FCSM_TR",
            FlowTest::FcsmMx => "FCSM_MX",
            FlowTest::RLE => "RLE",
            FlowTest::RGT => "RGT",
        }
    }
}

impl fmt::Display for FlowTest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::FlowTest;

    #[test]
    fn rust_names_preserve_maxwell_values_and_display_names() {
        let expected = [
            (FlowTest::CsmTa, 24, "CSM_TA"),
            (FlowTest::CsmTr, 25, "CSM_TR"),
            (FlowTest::CsmMx, 26, "CSM_MX"),
            (FlowTest::FcsmTa, 27, "FCSM_TA"),
            (FlowTest::FcsmTr, 28, "FCSM_TR"),
            (FlowTest::FcsmMx, 29, "FCSM_MX"),
        ];
        for (flow_test, raw, name) in expected {
            assert_eq!(flow_test as u64, raw);
            assert_eq!(FlowTest::from_u64(raw), Some(flow_test));
            assert_eq!(flow_test.name(), name);
        }
    }
}
