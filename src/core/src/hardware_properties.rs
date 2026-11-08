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
pub const NUM_CPU_CORES: u32 = 4;

/// Host storage capacity; unlike NUM_CPU_CORES this is not guest hardware behavior.
pub const MAX_NUM_CPU_CORES: usize = 6;

/// Immutable per-session selection. NX2 values preserve the experimental port;
/// they are not a claim about undocumented Switch 2 hardware.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HardwareProfile {
    #[default]
    Switch1,
    Nx2,
}

pub const SWITCH1_PROFILE: HardwareProfile = HardwareProfile::Switch1;
pub const NX2_PROFILE: HardwareProfile = HardwareProfile::Nx2;

impl HardwareProfile {
    pub const fn num_cpu_cores(self) -> u32 {
        match self { Self::Switch1 => NUM_CPU_CORES, Self::Nx2 => 6 }
    }

    pub const fn virtual_to_physical_core_map(self) -> &'static [i32; 64] {
        match self { Self::Switch1 => &VIRTUAL_TO_PHYSICAL_CORE_MAP,
            Self::Nx2 => &NX2_VIRTUAL_TO_PHYSICAL_CORE_MAP }
    }

    pub const fn convert_virtual_core_mask_to_physical(self, mut mask: u64) -> u64 {
        let mut physical = 0;
        while mask != 0 {
            let next = mask.trailing_zeros() as usize;
            mask &= !(1u64 << next);
            physical |= 1u64 << self.virtual_to_physical_core_map()[next];
        }
        physical
    }
}


/// Number of virtual cores (bits in a u64)
pub const NUM_VIRTUAL_CORES: usize = 64;

/// Virtual to Physical core map.
/// Maps virtual core indices to physical core indices.
pub const VIRTUAL_TO_PHYSICAL_CORE_MAP: [i32; 64] = [
    0, 1, 2, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3,
];

/// Existing NX2 application-core mapping, kept separate from Eden's map.
const NX2_VIRTUAL_TO_PHYSICAL_CORE_MAP: [i32; 64] = {
    let mut map = VIRTUAL_TO_PHYSICAL_CORE_MAP;
    map[4] = 4;
    map[5] = 5;
    map
};

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
    fn hardware_profiles_preserve_all_virtual_core_mappings() {
        for core in 0..64 {
            let switch1 = if core < 4 { core } else if core == 63 { 3 } else { 0 };
            let nx2 = if core < 6 { core } else if core == 63 { 3 } else { 0 };
            assert_eq!(SWITCH1_PROFILE.convert_virtual_core_mask_to_physical(1 << core), 1 << switch1);
            assert_eq!(NX2_PROFILE.convert_virtual_core_mask_to_physical(1 << core), 1 << nx2);
        }
        assert_eq!(SWITCH1_PROFILE.num_cpu_cores(), 4);
        assert_eq!(NX2_PROFILE.num_cpu_cores(), 6);
        assert_eq!(SWITCH1_PROFILE.convert_virtual_core_mask_to_physical(0x3f), 0xf);
        assert_eq!(NX2_PROFILE.convert_virtual_core_mask_to_physical(0x3f), 0x3f);
    }

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
        assert_eq!(convert_virtual_core_mask_to_physical(0x3F), 0xF);
        assert_eq!(convert_virtual_core_mask_to_physical((1 << 4) | (1 << 5)), 1);
    }
}
