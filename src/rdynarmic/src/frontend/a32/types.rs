use std::fmt;

/// A32 general-purpose register (R0-R15).
/// R13 = SP, R14 = LR, R15 = PC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Reg {
    R0 = 0,
    R1,
    R2,
    R3,
    R4,
    R5,
    R6,
    R7,
    R8,
    R9,
    R10,
    R11,
    R12,
    R13,
    R14,
    R15,
    InvalidReg = 99,
}

impl Reg {
    pub const SP: Reg = Reg::R13;
    pub const LR: Reg = Reg::R14;
    pub const PC: Reg = Reg::R15;

    pub fn number(self) -> usize {
        self as usize
    }

    pub fn from_u8(val: u8) -> Self {
        match val {
            0 => Reg::R0,
            1 => Reg::R1,
            2 => Reg::R2,
            3 => Reg::R3,
            4 => Reg::R4,
            5 => Reg::R5,
            6 => Reg::R6,
            7 => Reg::R7,
            8 => Reg::R8,
            9 => Reg::R9,
            10 => Reg::R10,
            11 => Reg::R11,
            12 => Reg::R12,
            13 => Reg::R13,
            14 => Reg::R14,
            15 => Reg::R15,
            _ => Reg::InvalidReg,
        }
    }

    pub fn from_u32(val: u32) -> Self {
        Self::from_u8(val as u8)
    }

    pub fn is_valid(self) -> bool {
        (self as u8) <= 15
    }
}

impl fmt::Display for Reg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Reg::R13 => write!(f, "SP"),
            Reg::R14 => write!(f, "LR"),
            Reg::R15 => write!(f, "PC"),
            Reg::InvalidReg => write!(f, "INVALID"),
            r => write!(f, "R{}", r as u8),
        }
    }
}

/// A32 extension register — S (single), D (double), Q (quad).
/// These overlap in hardware storage:
///   S0-S1 maps to D0, D0-D1 maps to Q0, etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExtReg {
    S0 = 0,
    S1,
    S2,
    S3,
    S4,
    S5,
    S6,
    S7,
    S8,
    S9,
    S10,
    S11,
    S12,
    S13,
    S14,
    S15,
    S16,
    S17,
    S18,
    S19,
    S20,
    S21,
    S22,
    S23,
    S24,
    S25,
    S26,
    S27,
    S28,
    S29,
    S30,
    S31,

    D0 = 32,
    D1,
    D2,
    D3,
    D4,
    D5,
    D6,
    D7,
    D8,
    D9,
    D10,
    D11,
    D12,
    D13,
    D14,
    D15,
    D16,
    D17,
    D18,
    D19,
    D20,
    D21,
    D22,
    D23,
    D24,
    D25,
    D26,
    D27,
    D28,
    D29,
    D30,
    D31,

    Q0 = 64,
    Q1,
    Q2,
    Q3,
    Q4,
    Q5,
    Q6,
    Q7,
    Q8,
    Q9,
    Q10,
    Q11,
    Q12,
    Q13,
    Q14,
    Q15,
}

impl ExtReg {
    pub fn is_single(self) -> bool {
        (self as u8) < 32
    }

    pub fn is_double(self) -> bool {
        let v = self as u8;
        (32..64).contains(&v)
    }

    pub fn is_quad(self) -> bool {
        let v = self as u8;
        (64..80).contains(&v)
    }

    /// Get the index within its category (S/D/Q).
    pub fn index(self) -> usize {
        let v = self as u8;
        if v < 32 {
            v as usize
        } else if v < 64 {
            (v - 32) as usize
        } else {
            (v - 64) as usize
        }
    }

    pub fn from_single(n: u8) -> Self {
        assert!(n < 32, "Invalid single register: S{}", n);
        unsafe { std::mem::transmute(n) }
    }

    pub fn from_double(n: u8) -> Self {
        assert!(n < 32, "Invalid double register: D{}", n);
        unsafe { std::mem::transmute(n + 32) }
    }

    pub fn from_quad(n: u8) -> Self {
        assert!(n < 16, "Invalid quad register: Q{}", n);
        unsafe { std::mem::transmute(n + 64) }
    }

    /// Convert to backing ext_reg[] array index for A32JitState.
    /// S registers use indices 0..31 (1 u32 each).
    /// D registers use indices 0..62 step 2 (2 u32 each).
    /// Q registers use indices 0..60 step 4 (4 u32 each).
    pub fn backing_index(self) -> usize {
        let v = self as u8;
        if v < 32 {
            v as usize
        } else if v < 64 {
            ((v - 32) * 2) as usize
        } else {
            ((v - 64) * 4) as usize
        }
    }
}

impl fmt::Display for ExtReg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let v = *self as u8;
        if v < 32 {
            write!(f, "S{}", v)
        } else if v < 64 {
            write!(f, "D{}", v - 32)
        } else {
            write!(f, "Q{}", v - 64)
        }
    }
}

/// Bitmask of registers (bits 0-15 correspond to R0-R15).
pub type RegList = u16;

/// Shift type for data processing instructions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ShiftType {
    LSL = 0,
    LSR = 1,
    ASR = 2,
    ROR = 3,
}

impl ShiftType {
    pub fn from_u8(val: u8) -> Self {
        match val & 3 {
            0 => ShiftType::LSL,
            1 => ShiftType::LSR,
            2 => ShiftType::ASR,
            3 => ShiftType::ROR,
            _ => unreachable!(),
        }
    }

    pub fn from_u32(val: u32) -> Self {
        Self::from_u8(val as u8)
    }
}

/// Sign-extend rotation for SXTB/UXTB etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SignExtendRotation {
    ROR0 = 0,
    ROR8 = 1,
    ROR16 = 2,
    ROR24 = 3,
}

impl SignExtendRotation {
    pub fn from_u8(val: u8) -> Self {
        match val & 3 {
            0 => SignExtendRotation::ROR0,
            1 => SignExtendRotation::ROR8,
            2 => SignExtendRotation::ROR16,
            3 => SignExtendRotation::ROR24,
            _ => unreachable!(),
        }
    }

    pub fn amount(self) -> u32 {
        (self as u32) * 8
    }
}

pub use crate::interface::a32::coprocessor_util::CoprocReg;

/// A32 exception type, passed to the `exception_raised` callback.
///
/// Matches upstream `dynarmic::A32::Exception` enum in `interface/A32/config.h`.
/// Values are the discriminant values (0-based).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum Exception {
    UndefinedInstruction = 0,
    UnpredictableInstruction = 1,
    DecodeError = 2,
    SendEvent = 3,
    SendEventLocal = 4,
    WaitForInterrupt = 5,
    WaitForEvent = 6,
    Yield = 7,
    Breakpoint = 8,
    PreloadData = 9,
    PreloadDataWithIntentToWrite = 10,
    PreloadInstruction = 11,
    NoExecuteFault = 12,
}

impl Exception {
    pub fn as_u32(self) -> u32 {
        self as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reg_basics() {
        assert_eq!(Reg::SP, Reg::R13);
        assert_eq!(Reg::LR, Reg::R14);
        assert_eq!(Reg::PC, Reg::R15);
        assert_eq!(Reg::R0.number(), 0);
        assert_eq!(Reg::R15.number(), 15);
        assert!(Reg::R0.is_valid());
        assert!(!Reg::InvalidReg.is_valid());
    }

    #[test]
    fn test_reg_from_u8() {
        for i in 0..16u8 {
            let r = Reg::from_u8(i);
            assert_eq!(r.number(), i as usize);
        }
        assert_eq!(Reg::from_u8(16), Reg::InvalidReg);
    }

    #[test]
    fn test_ext_reg() {
        assert!(ExtReg::S0.is_single());
        assert!(!ExtReg::S0.is_double());
        assert!(ExtReg::D0.is_double());
        assert!(ExtReg::Q0.is_quad());
        assert_eq!(ExtReg::S0.index(), 0);
        assert_eq!(ExtReg::S31.index(), 31);
        assert_eq!(ExtReg::D0.index(), 0);
        assert_eq!(ExtReg::D31.index(), 31);
        assert_eq!(ExtReg::Q0.index(), 0);
        assert_eq!(ExtReg::Q15.index(), 15);
    }

    #[test]
    fn test_ext_reg_backing() {
        assert_eq!(ExtReg::S0.backing_index(), 0);
        assert_eq!(ExtReg::S1.backing_index(), 1);
        assert_eq!(ExtReg::D0.backing_index(), 0);
        assert_eq!(ExtReg::D1.backing_index(), 2);
        assert_eq!(ExtReg::Q0.backing_index(), 0);
        assert_eq!(ExtReg::Q1.backing_index(), 4);
    }

    #[test]
    fn test_shift_type() {
        assert_eq!(ShiftType::from_u8(0), ShiftType::LSL);
        assert_eq!(ShiftType::from_u8(3), ShiftType::ROR);
    }

    #[test]
    fn test_sign_extend_rotation() {
        assert_eq!(SignExtendRotation::ROR0.amount(), 0);
        assert_eq!(SignExtendRotation::ROR8.amount(), 8);
        assert_eq!(SignExtendRotation::ROR16.amount(), 16);
        assert_eq!(SignExtendRotation::ROR24.amount(), 24);
    }
}
