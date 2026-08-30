use rxbyak::{Reg, RAX, RBP, RBX, RCX, RDI, RDX, RSI, RSP};
use rxbyak::{R10, R11, R12, R13, R14, R15, R8, R9};
use rxbyak::{XMM0, XMM1, XMM2, XMM3, XMM4, XMM5, XMM6, XMM7};
use rxbyak::{XMM10, XMM11, XMM12, XMM13, XMM14, XMM15, XMM8, XMM9};

/// Host location: abstracts GPRs, XMM registers, and spill slots
/// for the register allocator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostLoc {
    // General-purpose registers (0-15)
    Gpr(u8),
    // XMM registers (0-15)
    Xmm(u8),
    // Spill slot index
    Spill(u8),
}

impl HostLoc {
    pub fn is_gpr(self) -> bool {
        matches!(self, HostLoc::Gpr(_))
    }
    pub fn is_xmm(self) -> bool {
        matches!(self, HostLoc::Xmm(_))
    }
    pub fn is_register(self) -> bool {
        self.is_gpr() || self.is_xmm()
    }
    pub fn is_spill(self) -> bool {
        matches!(self, HostLoc::Spill(_))
    }

    /// Bit width of the location (64 for GPR, 128 for XMM/spill).
    pub fn bit_width(self) -> usize {
        match self {
            HostLoc::Gpr(_) => 64,
            HostLoc::Xmm(_) => 128,
            HostLoc::Spill(_) => 128,
        }
    }

    /// Get the GPR index.
    pub fn gpr_index(self) -> u8 {
        match self {
            HostLoc::Gpr(i) => i,
            _ => panic!("gpr_index called on non-GPR HostLoc"),
        }
    }

    /// Get the XMM index.
    pub fn xmm_index(self) -> u8 {
        match self {
            HostLoc::Xmm(i) => i,
            _ => panic!("xmm_index called on non-XMM HostLoc"),
        }
    }

    /// Convert to rxbyak Reg64.
    pub fn to_reg64(self) -> Reg {
        match self {
            HostLoc::Gpr(i) => gpr_to_reg64(i),
            _ => panic!("to_reg64 called on non-GPR HostLoc"),
        }
    }

    /// Convert to rxbyak Xmm.
    pub fn to_xmm(self) -> Reg {
        match self {
            HostLoc::Xmm(i) => xmm_to_reg(i),
            _ => panic!("to_xmm called on non-XMM HostLoc"),
        }
    }
}

/// Convert GPR index (0-15) to rxbyak Reg.
fn gpr_to_reg64(idx: u8) -> Reg {
    match idx {
        0 => RAX,
        1 => RCX,
        2 => RDX,
        3 => RBX,
        4 => RSP,
        5 => RBP,
        6 => RSI,
        7 => RDI,
        8 => R8,
        9 => R9,
        10 => R10,
        11 => R11,
        12 => R12,
        13 => R13,
        14 => R14,
        15 => R15,
        _ => panic!("Invalid GPR index: {}", idx),
    }
}

/// Convert XMM index (0-15) to rxbyak Reg.
fn xmm_to_reg(idx: u8) -> Reg {
    match idx {
        0 => XMM0,
        1 => XMM1,
        2 => XMM2,
        3 => XMM3,
        4 => XMM4,
        5 => XMM5,
        6 => XMM6,
        7 => XMM7,
        8 => XMM8,
        9 => XMM9,
        10 => XMM10,
        11 => XMM11,
        12 => XMM12,
        13 => XMM13,
        14 => XMM14,
        15 => XMM15,
        _ => panic!("Invalid XMM index: {}", idx),
    }
}

// Named GPR HostLoc constants
pub const HOST_RAX: HostLoc = HostLoc::Gpr(0);
pub const HOST_RCX: HostLoc = HostLoc::Gpr(1);
pub const HOST_RDX: HostLoc = HostLoc::Gpr(2);
pub const HOST_RBX: HostLoc = HostLoc::Gpr(3);
pub const HOST_RSP: HostLoc = HostLoc::Gpr(4);
pub const HOST_RBP: HostLoc = HostLoc::Gpr(5);
pub const HOST_RSI: HostLoc = HostLoc::Gpr(6);
pub const HOST_RDI: HostLoc = HostLoc::Gpr(7);
pub const HOST_R8: HostLoc = HostLoc::Gpr(8);
pub const HOST_R9: HostLoc = HostLoc::Gpr(9);
pub const HOST_R10: HostLoc = HostLoc::Gpr(10);
pub const HOST_R11: HostLoc = HostLoc::Gpr(11);
pub const HOST_R12: HostLoc = HostLoc::Gpr(12);
pub const HOST_R13: HostLoc = HostLoc::Gpr(13);
pub const HOST_R14: HostLoc = HostLoc::Gpr(14);
pub const HOST_R15: HostLoc = HostLoc::Gpr(15);

/// Available GPRs for register allocation.
/// Matches upstream `any_gpr`: RBP/R13/R14 remain represented here and are
/// rejected by `RegAlloc::select_a_register`, their upstream behavior owner.
pub const ANY_GPR: &[HostLoc] = &[
    HOST_RAX, HOST_RBX, HOST_RCX, HOST_RDX, HOST_RSI, HOST_RDI, HOST_RBP, HOST_R8, HOST_R9,
    HOST_R10, HOST_R11, HOST_R12, HOST_R13, HOST_R14,
];

/// Available XMM registers for register allocation.
/// Excludes XMM0 (reserved as scratch/implicit operand) and XMM1/XMM2
/// (reserved by 128-bit memory accessors), matching upstream `any_xmm`.
pub const ANY_XMM: &[HostLoc] = &[
    HostLoc::Xmm(3),
    HostLoc::Xmm(4),
    HostLoc::Xmm(5),
    HostLoc::Xmm(6),
    HostLoc::Xmm(7),
    HostLoc::Xmm(8),
    HostLoc::Xmm(9),
    HostLoc::Xmm(10),
    HostLoc::Xmm(11),
    HostLoc::Xmm(12),
    HostLoc::Xmm(13),
    HostLoc::Xmm(14),
    HostLoc::Xmm(15),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hostloc_classification() {
        assert!(HOST_RAX.is_gpr());
        assert!(!HOST_RAX.is_xmm());
        assert!(HOST_RAX.is_register());

        let xmm1 = HostLoc::Xmm(1);
        assert!(!xmm1.is_gpr());
        assert!(xmm1.is_xmm());
        assert!(xmm1.is_register());

        let spill0 = HostLoc::Spill(0);
        assert!(!spill0.is_register());
        assert!(spill0.is_spill());
    }

    #[test]
    fn test_any_gpr_matches_upstream_candidate_set() {
        assert!(!ANY_GPR.contains(&HOST_RSP));
        assert!(ANY_GPR.contains(&HOST_RBP));
        assert!(ANY_GPR.contains(&HOST_R13));
        assert!(ANY_GPR.contains(&HOST_R14));
        assert!(!ANY_GPR.contains(&HOST_R15));
        assert_eq!(ANY_GPR.len(), 14);
    }

    #[test]
    fn test_any_xmm_excludes_reserved_registers() {
        assert!(!ANY_XMM.contains(&HostLoc::Xmm(0)));
        assert!(!ANY_XMM.contains(&HostLoc::Xmm(1)));
        assert!(!ANY_XMM.contains(&HostLoc::Xmm(2)));
        assert_eq!(ANY_XMM.len(), 13);
    }

    #[test]
    fn test_bit_width() {
        assert_eq!(HOST_RAX.bit_width(), 64);
        assert_eq!(HostLoc::Xmm(0).bit_width(), 128);
        assert_eq!(HostLoc::Spill(0).bit_width(), 128);
    }
}
