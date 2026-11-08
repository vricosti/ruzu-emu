//! Port of zuyu/src/core/hardware_properties.h
//! Status: COMPLET
//! Derniere synchro: 2026-03-05
//!
//! Constants for Switch hardware (CPU count, core count, timer frequency, etc.)

/// Default CPU Frequency = 1020 MHz
pub const BASE_CLOCK_RATE: u64 = 1_020_000_000;

/// CNTPCT_EL0 Frequency = 19.2 MHz
pub const CNTFREQ: u64 = 19_200_000;

/// Number of CPU Cores
pub const NUM_CPU_CORES: u32 = 6;

/// Number of virtual cores (bits in a u64)
pub const NUM_VIRTUAL_CORES: usize = 64;

/// Virtual to Physical core map.
/// Maps virtual core indices to physical core indices.
pub const VIRTUAL_TO_PHYSICAL_CORE_MAP: [i32; 64] = [
    0, 1, 2, 3, 4, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3,
];

/// Bitmask covering all virtual cores.
pub const VIRTUAL_CORE_MASK: u64 = u64::MAX;

/// Converts a virtual core mask to a physical core mask.
pub const fn convert_virtual_core_mask_to_physical(mut v_core_mask: u64) -> u64 {
    let mut p_core_mask: u64 = 0;
    while v_core_mask != 0 {
        let next = v_core_mask.trailing_zeros() as usize;
        v_core_mask &= !(1u64 << next);
        p_core_mask |= 1u64 << VIRTUAL_TO_PHYSICAL_CORE_MAP[next] as u64;
    }
    p_core_mask
}

/// Cortex-A57 supports 4 memory watchpoints
pub const NUM_WATCHPOINTS: u64 = 4;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_virtual_to_physical_core_map() {
        assert_eq!(VIRTUAL_TO_PHYSICAL_CORE_MAP[0], 0);
        assert_eq!(VIRTUAL_TO_PHYSICAL_CORE_MAP[1], 1);
        assert_eq!(VIRTUAL_TO_PHYSICAL_CORE_MAP[2], 2);
        assert_eq!(VIRTUAL_TO_PHYSICAL_CORE_MAP[3], 3);
        assert_eq!(VIRTUAL_TO_PHYSICAL_CORE_MAP[63], 3);
    }

    #[test]
    fn test_convert_virtual_core_mask() {
        // Virtual core 0 -> physical core 0
        assert_eq!(convert_virtual_core_mask_to_physical(1), 1);
        // Virtual cores 0-3 -> physical cores 0-3
        assert_eq!(convert_virtual_core_mask_to_physical(0xF), 0xF);
        assert_eq!(convert_virtual_core_mask_to_physical(0x3F), 0x3F);
        assert_eq!(convert_virtual_core_mask_to_physical((1 << 4) | (1 << 5)), 0x30);
    }
}
