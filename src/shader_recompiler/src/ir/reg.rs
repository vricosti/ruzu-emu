// SPDX-FileCopyrightText: 2025 ruzu contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! IR Register type — maps to zuyu's `frontend/ir/reg.h`.
//!
//! Maxwell registers R0..R254/RZ plus native lowering shadow banks R256..R767.

use std::fmt;

/// Maxwell GPU register (R0-R254, RZ=255).
///
/// Matches upstream `Shader::IR::Reg` enum with values R0=0 through RZ=255.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Reg(pub u64);

impl Reg {
    pub const R0: Reg = Reg(0);
    pub const RZ: Reg = Reg(255);

    pub const NUM_USER_REGS: usize = 255;
    // Fritchii: two additional banks preserve guest and borrowed registers.
    // Maxwell operands and the zero register retain their original indices.
    pub const NUM_REGS: usize = 768;

    /// Construct from a u8 index.
    pub fn from_index(index: u8) -> Self {
        Reg(index as u64)
    }

    /// Get the register index.
    pub fn index(self) -> usize {
        self.0 as usize
    }

    /// Whether this is the zero register (RZ).
    pub fn is_zero(self) -> bool {
        self.0 == 255
    }

    /// Whether this register is aligned to the given alignment.
    pub fn is_aligned(self, align: usize) -> bool {
        self.is_zero() || (self.index() % align == 0)
    }
}

/// Register addition: reg + offset.
/// Matches upstream `operator+(Reg, int)`.
/// Adding to RZ always yields RZ.
impl std::ops::Add<i32> for Reg {
    type Output = Self;
    fn add(self, rhs: i32) -> Self {
        if self == Reg::RZ {
            return Reg::RZ;
        }
        let result = self.0 as i64 + rhs as i64;
        if result >= Reg::RZ.0 as i64 {
            panic!("Overflow on register arithmetic");
        }
        if result < 0 {
            panic!("Underflow on register arithmetic");
        }
        Reg(result as u64)
    }
}

/// Register subtraction: reg - offset.
impl std::ops::Sub<i32> for Reg {
    type Output = Self;
    fn sub(self, rhs: i32) -> Self {
        self + (-rhs)
    }
}

impl fmt::Debug for Reg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_zero() {
            write!(f, "RZ")
        } else {
            write!(f, "R{}", self.0)
        }
    }
}

impl fmt::Display for Reg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rz_is_zero() {
        assert!(Reg::RZ.is_zero());
        assert_eq!(Reg::RZ.index(), 255);
    }

    #[test]
    fn test_register_arithmetic() {
        let r5 = Reg(5);
        assert_eq!((r5 + 3).0, 8);
        assert_eq!((r5 - 2).0, 3);
    }

    #[test]
    fn test_rz_addition_yields_rz() {
        assert_eq!(Reg::RZ + 5, Reg::RZ);
        assert_eq!(Reg::RZ - 3, Reg::RZ);
    }

    #[test]
    fn test_alignment() {
        assert!(Reg(0).is_aligned(2));
        assert!(!Reg(1).is_aligned(2));
        assert!(Reg(4).is_aligned(4));
        assert!(Reg::RZ.is_aligned(4));
    }

    #[test]
    fn test_display() {
        assert_eq!(format!("{}", Reg(5)), "R5");
        assert_eq!(format!("{}", Reg::RZ), "RZ");
    }
}
