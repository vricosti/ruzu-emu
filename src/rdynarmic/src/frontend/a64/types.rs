use std::fmt;

// Re-export AccType for convenience in translator code
pub use crate::ir::acc_type::AccType;

/// A64 integer register (X0-X30, SP/ZR).
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
    R16,
    R17,
    R18,
    R19,
    R20,
    R21,
    R22,
    R23,
    R24,
    R25,
    R26,
    R27,
    R28,
    R29,
    R30,
    R31,
}

impl Reg {
    pub const LR: Reg = Reg::R30;
    pub const SP: Reg = Reg::R31;
    pub const ZR: Reg = Reg::R31;

    pub fn number(self) -> usize {
        self as usize
    }

    pub fn from_u8(val: u8) -> Self {
        assert!(val <= 31, "Invalid register number: {}", val);
        // SAFETY: val is in 0..=31, matching the repr(u8) layout
        unsafe { std::mem::transmute(val) }
    }

    pub fn from_u32(val: u32) -> Self {
        Self::from_u8(val as u8)
    }
}

impl std::ops::Add<usize> for Reg {
    type Output = Reg;
    fn add(self, rhs: usize) -> Reg {
        let n = self.number() + rhs;
        assert!(n <= 31);
        Reg::from_u8(n as u8)
    }
}

impl fmt::Display for Reg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Reg::R31 => write!(f, "SP/ZR"),
            Reg::R30 => write!(f, "LR"),
            _ => write!(f, "X{}", self.number()),
        }
    }
}

/// A64 vector register (V0-V31).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Vec {
    V0 = 0,
    V1,
    V2,
    V3,
    V4,
    V5,
    V6,
    V7,
    V8,
    V9,
    V10,
    V11,
    V12,
    V13,
    V14,
    V15,
    V16,
    V17,
    V18,
    V19,
    V20,
    V21,
    V22,
    V23,
    V24,
    V25,
    V26,
    V27,
    V28,
    V29,
    V30,
    V31,
}

impl Vec {
    pub fn number(self) -> usize {
        self as usize
    }

    pub fn from_u8(val: u8) -> Self {
        assert!(val <= 31, "Invalid vector register number: {}", val);
        unsafe { std::mem::transmute(val) }
    }

    pub fn from_u32(val: u32) -> Self {
        Self::from_u8(val as u8)
    }
}

impl std::ops::Add<usize> for Vec {
    type Output = Vec;
    fn add(self, rhs: usize) -> Vec {
        let n = self.number() + rhs;
        assert!(n <= 31);
        Vec::from_u8(n as u8)
    }
}

impl fmt::Display for Vec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "V{}", self.number())
    }
}

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
}

pub use crate::interface::a64::config::Exception;
