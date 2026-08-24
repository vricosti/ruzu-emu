/// Fine-grained optimization flags matching upstream
/// `interface/optimization_flags.h::OptimizationFlag`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct OptimizationFlag(u32);

impl OptimizationFlag {
    pub const BLOCK_LINKING: Self = Self(0x0000_0001);
    pub const RETURN_STACK_BUFFER: Self = Self(0x0000_0002);
    pub const FAST_DISPATCH: Self = Self(0x0000_0004);
    pub const GET_SET_ELIMINATION: Self = Self(0x0000_0008);
    pub const CONST_PROP: Self = Self(0x0000_0010);
    pub const MISC_IR_OPT: Self = Self(0x0000_0020);
    pub const CODE_SPEED: Self = Self(0x0000_0040);
    pub const DISABLE_VERIFICATION: Self = Self(0x0000_0080);

    pub const UNSAFE_UNFUSE_FMA: Self = Self(0x0001_0000);
    pub const UNSAFE_REDUCED_ERROR_FP: Self = Self(0x0002_0000);
    pub const UNSAFE_INACCURATE_NAN: Self = Self(0x0004_0000);
    pub const UNSAFE_IGNORE_STANDARD_FPCR_VALUE: Self = Self(0x0008_0000);
    pub const UNSAFE_IGNORE_GLOBAL_MONITOR: Self = Self(0x0010_0000);

    pub const NO_OPTIMIZATIONS: Self = Self(0);
    pub const ALL_SAFE_OPTIMIZATIONS: Self = Self(0x0000_ffff);

    #[inline]
    pub fn contains(self, flag: Self) -> bool {
        (self.0 & flag.0) == flag.0 && flag.0 != 0
    }

    #[inline]
    pub fn bits(self) -> u32 {
        self.0
    }
}

impl std::ops::BitOr for OptimizationFlag {
    type Output = Self;

    #[inline]
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for OptimizationFlag {
    #[inline]
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl std::ops::BitAnd for OptimizationFlag {
    type Output = Self;

    #[inline]
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl std::ops::BitAndAssign for OptimizationFlag {
    #[inline]
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

impl std::ops::Not for OptimizationFlag {
    type Output = Self;

    #[inline]
    fn not(self) -> Self {
        Self(!self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_and_layout_match_upstream() {
        let flags = [
            (OptimizationFlag::BLOCK_LINKING, 0x0000_0001),
            (OptimizationFlag::RETURN_STACK_BUFFER, 0x0000_0002),
            (OptimizationFlag::FAST_DISPATCH, 0x0000_0004),
            (OptimizationFlag::GET_SET_ELIMINATION, 0x0000_0008),
            (OptimizationFlag::CONST_PROP, 0x0000_0010),
            (OptimizationFlag::MISC_IR_OPT, 0x0000_0020),
            (OptimizationFlag::CODE_SPEED, 0x0000_0040),
            (OptimizationFlag::DISABLE_VERIFICATION, 0x0000_0080),
            (OptimizationFlag::UNSAFE_UNFUSE_FMA, 0x0001_0000),
            (OptimizationFlag::UNSAFE_REDUCED_ERROR_FP, 0x0002_0000),
            (OptimizationFlag::UNSAFE_INACCURATE_NAN, 0x0004_0000),
            (
                OptimizationFlag::UNSAFE_IGNORE_STANDARD_FPCR_VALUE,
                0x0008_0000,
            ),
            (OptimizationFlag::UNSAFE_IGNORE_GLOBAL_MONITOR, 0x0010_0000),
        ];
        for (flag, expected) in flags {
            assert_eq!(flag.bits(), expected);
        }
        assert_eq!(OptimizationFlag::NO_OPTIMIZATIONS.bits(), 0);
        assert_eq!(OptimizationFlag::ALL_SAFE_OPTIMIZATIONS.bits(), 0x0000_ffff);
        assert_eq!(std::mem::size_of::<OptimizationFlag>(), 4);
        assert_eq!(std::mem::align_of::<OptimizationFlag>(), 4);
    }

    #[test]
    fn bitwise_operators_match_upstream() {
        let mut flags = OptimizationFlag::BLOCK_LINKING | OptimizationFlag::CODE_SPEED;
        assert!(flags.contains(OptimizationFlag::BLOCK_LINKING));
        assert!(flags.contains(OptimizationFlag::CODE_SPEED));
        flags &= !OptimizationFlag::BLOCK_LINKING;
        assert!(!flags.contains(OptimizationFlag::BLOCK_LINKING));
        assert!(flags.contains(OptimizationFlag::CODE_SPEED));
    }
}
