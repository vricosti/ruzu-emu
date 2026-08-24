//! Public JIT halt reasons from upstream `interface/halt_reason.h`.

use bitflags::bitflags;

bitflags! {
    /// Reasons the JIT execution loop stopped.
    ///
    /// Matches upstream `dynarmic/interface/halt_reason.h`.
    ///
    /// Multiple reasons can be active simultaneously (OR'd together).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct HaltReason: u32 {
        /// Single-step completed.
        const STEP               = 0x0000_0001;
        /// Cache invalidation requested.
        const CACHE_INVALIDATION = 0x0000_0002;
        /// Data/memory abort requested by the host.
        const MEMORY_ABORT       = 0x0000_0004;

        const USER_DEFINED1      = 0x0100_0000;
        const USER_DEFINED2      = 0x0200_0000;
        const USER_DEFINED3      = 0x0400_0000;
        const USER_DEFINED4      = 0x0800_0000;
        const USER_DEFINED5      = 0x1000_0000;
        const USER_DEFINED6      = 0x2000_0000;
        const USER_DEFINED7      = 0x4000_0000;
        const USER_DEFINED8      = 0x8000_0000;

        /// Local compatibility alias for older Rust callers.
        const EXCEPTION_RAISED   = Self::USER_DEFINED1.bits();
        /// Local compatibility alias for the scheduler break-loop path.
        const EXTERNAL_HALT      = Self::USER_DEFINED2.bits();
        /// Local compatibility alias for SVC halt.
        const SVC                = Self::USER_DEFINED3.bits();
        /// Local compatibility alias for instruction breakpoint halt.
        const BREAKPOINT         = Self::USER_DEFINED4.bits();
        /// Local compatibility alias for prefetch abort halt.
        const PREFETCH_ABORT     = Self::USER_DEFINED6.bits();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_halt_reason_bitflags() {
        let reason = HaltReason::SVC | HaltReason::STEP;
        assert!(reason.contains(HaltReason::SVC));
        assert!(reason.contains(HaltReason::STEP));
        assert!(!reason.contains(HaltReason::BREAKPOINT));
    }

    #[test]
    fn test_halt_reason_empty() {
        let reason = HaltReason::empty();
        assert!(reason.is_empty());
        assert_eq!(reason.bits(), 0);
    }

    #[test]
    fn test_halt_reason_from_bits() {
        let reason = HaltReason::from_bits_truncate(
            HaltReason::MEMORY_ABORT.bits() | HaltReason::SVC.bits(),
        );
        assert!(reason.contains(HaltReason::MEMORY_ABORT));
        assert!(reason.contains(HaltReason::SVC));
    }

    #[test]
    fn test_halt_reason_values_match_upstream_dynarmic() {
        assert_eq!(std::mem::size_of::<HaltReason>(), 4);
        assert_eq!(HaltReason::STEP.bits(), 0x0000_0001);
        assert_eq!(HaltReason::CACHE_INVALIDATION.bits(), 0x0000_0002);
        assert_eq!(HaltReason::MEMORY_ABORT.bits(), 0x0000_0004);
        assert_eq!(HaltReason::USER_DEFINED2.bits(), 0x0200_0000);
        assert_eq!(HaltReason::USER_DEFINED3.bits(), 0x0400_0000);
        assert_eq!(HaltReason::USER_DEFINED4.bits(), 0x0800_0000);
        assert_eq!(HaltReason::USER_DEFINED6.bits(), 0x2000_0000);
    }

    #[test]
    fn test_compatibility_aliases_match_upstream_mapped_bits() {
        assert_eq!(
            HaltReason::EXTERNAL_HALT.bits(),
            HaltReason::USER_DEFINED2.bits()
        );
        assert_eq!(HaltReason::SVC.bits(), HaltReason::USER_DEFINED3.bits());
        assert_eq!(
            HaltReason::BREAKPOINT.bits(),
            HaltReason::USER_DEFINED4.bits()
        );
        assert_eq!(
            HaltReason::PREFETCH_ABORT.bits(),
            HaltReason::USER_DEFINED6.bits()
        );
    }
}
