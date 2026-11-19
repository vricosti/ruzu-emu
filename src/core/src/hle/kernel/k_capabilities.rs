//! Port of zuyu/src/core/hle/kernel/k_capabilities.h / k_capabilities.cpp
//! Status: EN COURS
//! Derniere synchro: 2026-03-17
//!
//! KCapabilities: parses and stores process capability descriptors
//! (core mask, priority mask, SVC permissions, interrupts, etc.)

use crate::hardware_properties;
use crate::hle::kernel::k_memory_block::KMemoryPermission;
use crate::hle::kernel::k_memory_region_type::{
    K_MEMORY_REGION_TYPE_DTB, K_MEMORY_REGION_TYPE_KERNEL_TRACE_BUFFER,
    K_MEMORY_REGION_TYPE_ON_MEMORY_BOOT_IMAGE,
};
use crate::hle::kernel::k_process_page_table::KProcessPageTable;
use crate::hle::kernel::svc::svc_results;

/// Number of possible interrupt IDs.
pub const INTERRUPT_ID_COUNT: usize = 0x400;

/// Number of SVC access flags (matching Svc::SvcAccessFlagSet size).
pub const SVC_ACCESS_FLAG_COUNT: usize = 0xC0;

/// Capability type discriminants.
/// Matches upstream `KCapabilities::CapabilityType` (k_capabilities.h).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityType {
    CorePriority = (1 << 3) - 1,   // 0x07
    SyscallMask = (1 << 4) - 1,    // 0x0F
    MapRange = (1 << 6) - 1,       // 0x3F
    MapIoPage = (1 << 7) - 1,      // 0x7F
    MapRegion = (1 << 10) - 1,     // 0x3FF
    InterruptPair = (1 << 11) - 1, // 0x7FF
    ProgramType = (1 << 13) - 1,   // 0x1FFF
    KernelVersion = (1 << 14) - 1, // 0x3FFF
    HandleTable = (1 << 15) - 1,   // 0x7FFF
    DebugFlags = (1 << 16) - 1,    // 0xFFFF
    Invalid = 0,
    Padding = 0xFFFF_FFFF,
}

/// Region type for MapRegion capability.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionType {
    NoMapping = 0,
    KernelTraceBuffer = 1,
    OnMemoryBootImage = 2,
    Dtb = 3,
}

/// Padding interrupt ID (0x3FF).
pub const PADDING_INTERRUPT_ID: u32 = 0x3FF;

/// Physical map allowed mask.
pub const PHYSICAL_MAP_ALLOWED_MASK: u64 = (1u64 << 36) - 1;

/// Initialize-once flag set.
/// Upstream: CorePriority | ProgramType | KernelVersion | HandleTable | DebugFlags
pub const INITIALIZE_ONCE_FLAGS: u32 = (1 << 3) | (1 << 13) | (1 << 14) | (1 << 15) | (1 << 16);

/// Determine the capability type from a raw value.
/// Matches upstream `GetCapabilityType`.
pub fn get_capability_type(value: u32) -> CapabilityType {
    let flag = (!value) & (value.wrapping_add(1));
    let type_val = flag.wrapping_sub(1);
    match type_val {
        v if v == CapabilityType::CorePriority as u32 => CapabilityType::CorePriority,
        v if v == CapabilityType::SyscallMask as u32 => CapabilityType::SyscallMask,
        v if v == CapabilityType::MapRange as u32 => CapabilityType::MapRange,
        v if v == CapabilityType::MapIoPage as u32 => CapabilityType::MapIoPage,
        v if v == CapabilityType::MapRegion as u32 => CapabilityType::MapRegion,
        v if v == CapabilityType::InterruptPair as u32 => CapabilityType::InterruptPair,
        v if v == CapabilityType::ProgramType as u32 => CapabilityType::ProgramType,
        v if v == CapabilityType::KernelVersion as u32 => CapabilityType::KernelVersion,
        v if v == CapabilityType::HandleTable as u32 => CapabilityType::HandleTable,
        v if v == CapabilityType::DebugFlags as u32 => CapabilityType::DebugFlags,
        0xFFFF_FFFF => CapabilityType::Padding,
        _ => CapabilityType::Invalid,
    }
}

/// Get the flag value for a capability type.
/// Matches upstream `GetCapabilityFlag`.
fn get_capability_flag(cap_type: CapabilityType) -> u32 {
    cap_type as u32 + 1
}

// ---------------------------------------------------------------------------
// KCapabilities
// ---------------------------------------------------------------------------

/// Process capabilities.
/// Matches upstream `KCapabilities` class (k_capabilities.h).
pub struct KCapabilities {
    pub(crate) hardware_profile: hardware_properties::HardwareProfile,
    pub svc_access_flags: [bool; SVC_ACCESS_FLAG_COUNT],
    pub irq_access_flags: [bool; INTERRUPT_ID_COUNT],
    pub core_mask: u64,
    pub phys_core_mask: u64,
    pub priority_mask: u64,
    pub debug_capabilities: u32,
    pub handle_table_size: i32,
    pub intended_kernel_version: u32,
    pub program_type: u32,
}

impl KCapabilities {
    pub fn new() -> Self {
        Self {
            hardware_profile: hardware_properties::SWITCH1_PROFILE,
            svc_access_flags: [false; SVC_ACCESS_FLAG_COUNT],
            irq_access_flags: [false; INTERRUPT_ID_COUNT],
            core_mask: 0,
            phys_core_mask: 0,
            priority_mask: 0,
            debug_capabilities: 0,
            handle_table_size: 0,
            intended_kernel_version: 0,
            program_type: 0,
        }
    }

    // -- Getters matching upstream --

    pub fn get_core_mask(&self) -> u64 {
        self.core_mask
    }
    pub fn get_physical_core_mask(&self) -> u64 {
        self.phys_core_mask
    }
    pub fn get_priority_mask(&self) -> u64 {
        self.priority_mask
    }
    pub fn get_handle_table_size(&self) -> i32 {
        self.handle_table_size
    }

    pub fn is_permitted_svc(&self, id: u32) -> bool {
        (id as usize) < SVC_ACCESS_FLAG_COUNT && self.svc_access_flags[id as usize]
    }

    pub fn is_permitted_interrupt(&self, id: u32) -> bool {
        (id as usize) < INTERRUPT_ID_COUNT && self.irq_access_flags[id as usize]
    }

    pub fn is_permitted_debug(&self) -> bool {
        (self.debug_capabilities >> 17) & 1 != 0
    }

    pub fn can_force_debug(&self) -> bool {
        (self.debug_capabilities >> 18) & 1 != 0
    }

    pub fn get_intended_kernel_major_version(&self) -> u32 {
        (self.intended_kernel_version >> 19) & 0x1FFF
    }

    pub fn get_intended_kernel_minor_version(&self) -> u32 {
        (self.intended_kernel_version >> 15) & 0xF
    }

    // -- Initialization --

    /// Initialize capabilities for a KIP (kernel initial process).
    /// Matches upstream `KCapabilities::InitializeForKip`.
    pub fn initialize_for_kip(
        &mut self,
        kern_caps: &[u32],
        page_table: Option<&mut KProcessPageTable>,
    ) -> u32 {
        self.svc_access_flags = [false; SVC_ACCESS_FLAG_COUNT];
        self.irq_access_flags = [false; INTERRUPT_ID_COUNT];
        self.debug_capabilities = 0;
        self.handle_table_size = 0;
        self.intended_kernel_version = 0;
        self.program_type = 0;

        // Initial processes may run on all cores.
        self.core_mask = hardware_properties::VIRTUAL_CORE_MASK;
        self.phys_core_mask = self.hardware_profile.convert_virtual_core_mask_to_physical(self.core_mask);

        // Initial processes may use any user priority (not kernel priorities 0-3).
        self.priority_mask = !0xFu64;

        // Set kernel version to current supported version.
        // major=13, minor=0 (matching upstream SupportedKernelMajorVersion)
        let major: u32 = 13;
        let minor: u32 = 0;
        self.intended_kernel_version = (major << 19) | (minor << 15);

        self.set_capabilities(kern_caps, page_table)
    }

    /// Initialize capabilities for a user process.
    /// Matches upstream `KCapabilities::InitializeForUser`.
    pub fn initialize_for_user(
        &mut self,
        user_caps: &[u32],
        page_table: Option<&mut KProcessPageTable>,
    ) -> u32 {
        // Reset all fields.
        self.svc_access_flags = [false; SVC_ACCESS_FLAG_COUNT];
        self.irq_access_flags = [false; INTERRUPT_ID_COUNT];
        self.debug_capabilities = 0;
        self.handle_table_size = 0;
        self.intended_kernel_version = 0;
        self.program_type = 0;

        // User processes must specify what cores/priorities they can use.
        self.core_mask = 0;
        self.phys_core_mask = 0;
        self.priority_mask = 0;

        self.set_capabilities(user_caps, page_table)
    }

    // -- Capability parsing --

    /// Parse a capabilities array.
    /// Matches upstream `KCapabilities::SetCapabilities`.
    fn set_capabilities(
        &mut self,
        caps: &[u32],
        mut page_table: Option<&mut KProcessPageTable>,
    ) -> u32 {
        let mut set_flags = 0u32;
        let mut set_svc = 0u32;
        let mut i = 0;

        while i < caps.len() {
            let cap = caps[i];

            if get_capability_type(cap) == CapabilityType::MapRange {
                // MapRange is a pair — need the next entry.
                i += 1;
                if i >= caps.len() {
                    return svc_results::RESULT_INVALID_COMBINATION.get_inner_value();
                }
                let size_cap = caps[i];
                if get_capability_type(size_cap) != CapabilityType::MapRange {
                    return svc_results::RESULT_INVALID_COMBINATION.get_inner_value();
                }
                if let Some(ref mut pt) = page_table {
                    let result = Self::map_range(cap, size_cap, pt);
                    if result != 0 {
                        return result;
                    }
                }
                i += 1;
                continue;
            }

            let result = self.set_capability(cap, &mut set_flags, &mut set_svc, &mut page_table);
            if result != 0 {
                return result;
            }
            i += 1;
        }

        0 // success
    }

    /// Process a single capability.
    /// Matches upstream `KCapabilities::SetCapability`.
    fn set_capability(
        &mut self,
        cap: u32,
        set_flags: &mut u32,
        set_svc: &mut u32,
        page_table: &mut Option<&mut KProcessPageTable>,
    ) -> u32 {
        let cap_type = get_capability_type(cap);

        // Invalid type.
        if cap_type == CapabilityType::Invalid {
            return svc_results::RESULT_INVALID_ARGUMENT.get_inner_value();
        }

        // Padding: no work to do.
        if cap_type == CapabilityType::Padding {
            return 0;
        }

        // Check initialize-once flags: some capabilities can only be set once.
        let flag = get_capability_flag(cap_type);
        if (*set_flags & INITIALIZE_ONCE_FLAGS) & flag != 0 {
            return svc_results::RESULT_INVALID_COMBINATION.get_inner_value();
        }
        *set_flags |= flag;

        match cap_type {
            CapabilityType::CorePriority => self.set_core_priority_capability(cap),
            CapabilityType::SyscallMask => self.set_syscall_mask_capability(cap, set_svc),
            CapabilityType::MapIoPage => {
                if let Some(ref mut pt) = page_table {
                    Self::map_io_page(cap, pt)
                } else {
                    0
                }
            }
            CapabilityType::MapRegion => {
                if let Some(ref mut pt) = page_table {
                    Self::map_region(cap, pt)
                } else {
                    0
                }
            }
            CapabilityType::InterruptPair => self.set_interrupt_pair_capability(cap),
            CapabilityType::ProgramType => self.set_program_type_capability(cap),
            CapabilityType::KernelVersion => self.set_kernel_version_capability(cap),
            CapabilityType::HandleTable => self.set_handle_table_capability(cap),
            CapabilityType::DebugFlags => self.set_debug_flags_capability(cap),
            _ => svc_results::RESULT_INVALID_ARGUMENT.get_inner_value(),
        }
    }

    /// Matches upstream `KCapabilities::SetCorePriorityCapability`.
    fn set_core_priority_capability(&mut self, cap: u32) -> u32 {
        // Can't set if already set.
        if self.core_mask != 0 {
            return svc_results::RESULT_INVALID_ARGUMENT.get_inner_value();
        }
        if self.priority_mask != 0 {
            return svc_results::RESULT_INVALID_ARGUMENT.get_inner_value();
        }

        // CorePriority bitfield layout (from k_capabilities.h):
        //   bits [3..0]   = type id
        //   bits [9..4]   = lowest_thread_priority (6 bits)
        //   bits [15..10] = highest_thread_priority (6 bits)
        //   bits [23..16] = minimum_core_id (8 bits)
        //   bits [31..24] = maximum_core_id (8 bits)
        let max_prio = ((cap >> 4) & 0x3F) as u32; // lowest_thread_priority
        let min_prio = ((cap >> 10) & 0x3F) as u32; // highest_thread_priority
        let min_core = ((cap >> 16) & 0xFF) as u32;
        let max_core = ((cap >> 24) & 0xFF) as u32;

        // Validate.
        if min_core > max_core {
            return svc_results::RESULT_INVALID_COMBINATION.get_inner_value();
        }
        if min_prio > max_prio {
            return svc_results::RESULT_INVALID_COMBINATION.get_inner_value();
        }
        if max_core >= hardware_properties::NUM_VIRTUAL_CORES as u32 {
            return svc_results::RESULT_INVALID_CORE_ID.get_inner_value();
        }
        debug_assert!(max_prio < 64);

        // Set core mask.
        for core_id in min_core..=max_core {
            self.core_mask |= 1u64 << core_id;
        }
        // Physical core mask = convert virtual to physical.
        // Upstream: Core::Hardware::ConvertVirtualCoreMaskToPhysical
        self.phys_core_mask = self.hardware_profile.convert_virtual_core_mask_to_physical(self.core_mask);

        // Set priority mask.
        for prio in min_prio..=max_prio {
            self.priority_mask |= 1u64 << prio;
        }

        // Must have some core/priority we can use.
        if self.core_mask == 0 {
            return svc_results::RESULT_INVALID_ARGUMENT.get_inner_value();
        }
        if self.priority_mask == 0 {
            return svc_results::RESULT_INVALID_ARGUMENT.get_inner_value();
        }
        // Processes must not have access to kernel thread priorities (0-3).
        if (self.priority_mask & 0xF) != 0 {
            return svc_results::RESULT_INVALID_ARGUMENT.get_inner_value();
        }

        log::debug!(
            "KCapabilities::SetCorePriorityCapability: cores=[{}..{}], priority=[{}..{}], \
             core_mask={:#x}, priority_mask={:#018x}",
            min_core,
            max_core,
            min_prio,
            max_prio,
            self.core_mask,
            self.priority_mask
        );

        0 // success
    }

    /// Matches upstream `KCapabilities::SetSyscallMaskCapability`.
    fn set_syscall_mask_capability(&mut self, cap: u32, set_svc: &mut u32) -> u32 {
        // SyscallMask bitfield:
        //   bits [4..0]   = type id
        //   bits [28..5]  = mask (24 bits)
        //   bits [31..29] = index (3 bits)
        let mask = (cap >> 5) & 0x00FF_FFFF;
        let index = (cap >> 29) & 0x7;

        // Each index can only be set once.
        let index_flag = 1u32 << index;
        if (*set_svc & index_flag) != 0 {
            return svc_results::RESULT_INVALID_COMBINATION.get_inner_value();
        }
        *set_svc |= index_flag;

        // Set SVCs.
        for bit in 0..24u32 {
            if mask & (1 << bit) != 0 {
                let svc_id = 24 * index + bit;
                if !self.set_svc_allowed(svc_id) {
                    return svc_results::RESULT_OUT_OF_RANGE.get_inner_value();
                }
            }
        }

        0 // success
    }

    /// Matches upstream `KCapabilities::SetInterruptPairCapability`.
    fn set_interrupt_pair_capability(&mut self, cap: u32) -> u32 {
        // InterruptPair bitfield:
        //   bits [11..0]  = type id
        //   bits [21..12] = interrupt_id0 (10 bits)
        //   bits [31..22] = interrupt_id1 (10 bits)
        let id0 = (cap >> 12) & 0x3FF;
        let id1 = (cap >> 22) & 0x3FF;

        if id0 != PADDING_INTERRUPT_ID {
            if !self.set_interrupt_permitted(id0) {
                return svc_results::RESULT_OUT_OF_RANGE.get_inner_value();
            }
        }
        if id1 != PADDING_INTERRUPT_ID {
            if !self.set_interrupt_permitted(id1) {
                return svc_results::RESULT_OUT_OF_RANGE.get_inner_value();
            }
        }

        0 // success
    }

    /// Matches upstream `KCapabilities::SetProgramTypeCapability`.
    fn set_program_type_capability(&mut self, cap: u32) -> u32 {
        // ProgramType bitfield:
        //   bits [13..0]  = type id
        //   bits [16..14] = type (3 bits)
        //   bits [31..17] = reserved (must be 0)
        let ptype = (cap >> 14) & 0x7;
        let reserved = (cap >> 17) & 0x7FFF;
        if reserved != 0 {
            return svc_results::RESULT_RESERVED_USED.get_inner_value();
        }
        self.program_type = ptype;
        0
    }

    /// Matches upstream `KCapabilities::SetKernelVersionCapability`.
    fn set_kernel_version_capability(&mut self, cap: u32) -> u32 {
        // Ensure we haven't already set the version.
        let current_major = (self.intended_kernel_version >> 19) & 0x1FFF;
        if current_major != 0 {
            return svc_results::RESULT_INVALID_ARGUMENT.get_inner_value();
        }

        self.intended_kernel_version = cap;

        // Ensure the version is valid (major != 0).
        let new_major = (self.intended_kernel_version >> 19) & 0x1FFF;
        if new_major == 0 {
            return svc_results::RESULT_INVALID_ARGUMENT.get_inner_value();
        }

        0
    }

    /// Matches upstream `KCapabilities::SetHandleTableCapability`.
    fn set_handle_table_capability(&mut self, cap: u32) -> u32 {
        // HandleTable bitfield:
        //   bits [15..0]  = type id
        //   bits [25..16] = size (10 bits)
        //   bits [31..26] = reserved (must be 0)
        let size = ((cap >> 16) & 0x3FF) as i32;
        let reserved = (cap >> 26) & 0x3F;
        if reserved != 0 {
            return svc_results::RESULT_RESERVED_USED.get_inner_value();
        }
        self.handle_table_size = size;
        0
    }

    /// Matches upstream `KCapabilities::SetDebugFlagsCapability`.
    fn set_debug_flags_capability(&mut self, cap: u32) -> u32 {
        // DebugFlags bitfield:
        //   bits [16..0]  = type id
        //   bit  17       = allow_debug
        //   bit  18       = force_debug
        //   bits [31..19] = reserved (must be 0)
        let reserved = (cap >> 19) & 0x1FFF;
        if reserved != 0 {
            return svc_results::RESULT_RESERVED_USED.get_inner_value();
        }
        // Store the raw cap; allow_debug and force_debug are extracted by getters.
        self.debug_capabilities = cap;
        0
    }

    // -- Mapping methods --

    /// Matches upstream `KCapabilities::MapRange_`.
    fn map_range(cap: u32, size_cap: u32, page_table: &mut KProcessPageTable) -> u32 {
        const PAGE_SIZE: u64 = 0x1000;

        // MapRange bitfield: bits [7..31] = address (24 bits), bit 31 = read_only
        let phys_addr = (((cap >> 7) & 0x00FF_FFFF) as u64) * PAGE_SIZE;
        let read_only = (cap >> 31) & 1;

        // MapRangeSize bitfield: bits [7..27] = pages (20 bits), bits [27..31] = reserved, bit 31 = normal
        let num_pages = ((size_cap >> 7) & 0x000F_FFFF) as usize;
        let reserved = (size_cap >> 27) & 0xF;
        let normal = (size_cap >> 31) & 1;

        if reserved != 0 {
            return svc_results::RESULT_OUT_OF_RANGE.get_inner_value();
        }
        let size = num_pages * PAGE_SIZE as usize;
        if num_pages == 0 {
            return svc_results::RESULT_INVALID_SIZE.get_inner_value();
        }
        if phys_addr >= phys_addr + size as u64 {
            return svc_results::RESULT_INVALID_ADDRESS.get_inner_value();
        }
        if ((phys_addr + size as u64 - 1) & !PHYSICAL_MAP_ALLOWED_MASK) != 0 {
            return svc_results::RESULT_INVALID_ADDRESS.get_inner_value();
        }

        let perm = if read_only != 0 {
            KMemoryPermission::USER_READ
        } else {
            KMemoryPermission::USER_READ_WRITE
        };

        if normal != 0 {
            page_table.map_static(phys_addr, size, perm)
        } else {
            page_table.map_io(phys_addr, size, perm)
        }
    }

    /// Matches upstream `KCapabilities::MapIoPage_`.
    fn map_io_page(cap: u32, page_table: &mut KProcessPageTable) -> u32 {
        const PAGE_SIZE: u64 = 0x1000;

        // MapIoPage bitfield: bits [8..32] = address (24 bits)
        let phys_addr = (((cap >> 8) & 0x00FF_FFFF) as u64) * PAGE_SIZE;
        let size = PAGE_SIZE as usize;

        if phys_addr >= phys_addr + size as u64 {
            return svc_results::RESULT_INVALID_ADDRESS.get_inner_value();
        }
        if ((phys_addr + size as u64 - 1) & !PHYSICAL_MAP_ALLOWED_MASK) != 0 {
            return svc_results::RESULT_INVALID_ADDRESS.get_inner_value();
        }

        page_table.map_io(phys_addr, size, KMemoryPermission::USER_READ_WRITE)
    }

    /// Matches upstream `KCapabilities::MapRegion_`.
    fn map_region(cap: u32, page_table: &mut KProcessPageTable) -> u32 {
        const IS_KTRACE_ENABLED: bool = false;

        // MapRegion bitfield:
        //   bits [16..11] = region0 (6 bits, RegionType)
        //   bit  17       = read_only0
        //   bits [23..18] = region1
        //   bit  24       = read_only1
        //   bits [30..25] = region2
        //   bit  31       = read_only2
        let types = [
            ((cap >> 11) & 0x3F, (cap >> 17) & 1),
            ((cap >> 18) & 0x3F, (cap >> 24) & 1),
            ((cap >> 25) & 0x3F, (cap >> 31) & 1),
        ];

        for &(region_type, read_only) in &types {
            let perm = if read_only != 0 {
                KMemoryPermission::USER_READ
            } else {
                KMemoryPermission::USER_READ_WRITE
            };
            let mapped_region_type = match region_type {
                value if value == RegionType::NoMapping as u32 => continue,
                value if value == RegionType::KernelTraceBuffer as u32 => {
                    if !IS_KTRACE_ENABLED {
                        continue;
                    }
                    K_MEMORY_REGION_TYPE_KERNEL_TRACE_BUFFER.get_value()
                }
                value if value == RegionType::OnMemoryBootImage as u32 => {
                    K_MEMORY_REGION_TYPE_ON_MEMORY_BOOT_IMAGE.get_value()
                }
                value if value == RegionType::Dtb as u32 => K_MEMORY_REGION_TYPE_DTB.get_value(),
                _ => return svc_results::RESULT_NOT_FOUND.get_inner_value(),
            };
            let result = page_table.map_region(mapped_region_type, perm);
            if result != 0 {
                return result;
            }
        }

        0
    }

    // -- Private helpers --

    fn set_svc_allowed(&mut self, id: u32) -> bool {
        if (id as usize) < SVC_ACCESS_FLAG_COUNT {
            self.svc_access_flags[id as usize] = true;
            true
        } else {
            false
        }
    }

    fn set_interrupt_permitted(&mut self, id: u32) -> bool {
        if (id as usize) < INTERRUPT_ID_COUNT {
            self.irq_access_flags[id as usize] = true;
            true
        } else {
            false
        }
    }
}

impl Default for KCapabilities {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware_properties::HardwareProfile;

    #[test]
    fn test_capability_type_values() {
        assert_eq!(CapabilityType::CorePriority as u32, 0x07);
        assert_eq!(CapabilityType::SyscallMask as u32, 0x0F);
        assert_eq!(CapabilityType::HandleTable as u32, 0x7FFF);
        assert_eq!(CapabilityType::DebugFlags as u32, 0xFFFF);
    }

    #[test]
    fn test_get_capability_type() {
        assert_eq!(get_capability_type(0xFFFF_FFFF), CapabilityType::Padding);
    }

    #[test]
    fn virtual_core_affinity_uses_hardware_mapping() {
        for (profile, minimum, maximum, virtual_mask, physical_mask) in [
            (HardwareProfile::Switch1, 0u32, 5u32, 0x3fu64, 0x0fu64),
            (HardwareProfile::Nx2, 0, 5, 0x3f, 0x3f),
            (HardwareProfile::Switch1, 63, 63, 1u64 << 63, 1u64 << 3),
            (HardwareProfile::Nx2, 63, 63, 1u64 << 63, 1u64 << 3),
        ] {
            let mut caps = KCapabilities::new();
            caps.hardware_profile = profile;
            let cap = 7 | (59 << 4) | (28 << 10) | (minimum << 16) | (maximum << 24);
            assert_eq!(caps.set_core_priority_capability(cap), 0);
            assert_eq!(caps.core_mask, virtual_mask);
            assert_eq!(caps.phys_core_mask, physical_mask);
        }
        let mut caps = KCapabilities::new();
        assert_eq!(caps.set_core_priority_capability(7 | (59 << 4) | (28 << 10) | (64 << 24)),
            svc_results::RESULT_INVALID_CORE_ID.get_inner_value());
    }

    #[test]
    fn test_core_priority_parsing() {
        let mut caps = KCapabilities::new();
        // Build a CorePriority cap: cores [0..2], priority [28..59]
        // lowest_thread_priority (max_prio) = 59, highest_thread_priority (min_prio) = 28
        // minimum_core_id = 0, maximum_core_id = 2
        let cap: u32 = 0x07 // type id (CorePriority)
            | (59 << 4)      // lowest_thread_priority
            | (28 << 10)     // highest_thread_priority
            | (0 << 16)      // minimum_core_id
            | (2 << 24); // maximum_core_id
        let result = caps.set_core_priority_capability(cap);
        assert_eq!(result, 0);
        assert_eq!(caps.core_mask, 0b111); // cores 0,1,2
                                           // Priority mask should have bits 28..59 set.
        for prio in 0..64 {
            if prio >= 28 && prio <= 59 {
                assert!(
                    caps.priority_mask & (1u64 << prio) != 0,
                    "prio {} should be set",
                    prio
                );
            } else {
                assert!(
                    caps.priority_mask & (1u64 << prio) == 0,
                    "prio {} should NOT be set",
                    prio
                );
            }
        }
    }

    #[test]
    fn test_syscall_mask_parsing() {
        let mut caps = KCapabilities::new();
        // Build a SyscallMask cap: index=0, mask has bits 1,6 set
        // (SVCs 1 and 6 = SetHeapSize and QueryMemory)
        let cap: u32 = 0x0F // type id
            | ((1 << 1 | 1 << 6) << 5) // mask bits for SVCs 1 and 6
            | (0 << 29); // index 0
        let mut set_svc = 0;
        let result = caps.set_syscall_mask_capability(cap, &mut set_svc);
        assert_eq!(result, 0);
        assert!(caps.is_permitted_svc(1));
        assert!(caps.is_permitted_svc(6));
        assert!(!caps.is_permitted_svc(0));
        assert!(!caps.is_permitted_svc(2));
    }

    #[test]
    fn test_initialize_for_user() {
        let mut caps = KCapabilities::new();
        // CorePriority + HandleTable caps
        let core_prio: u32 = 0x07 | (59 << 4) | (28 << 10) | (0 << 16) | (2 << 24);
        let handle_tbl: u32 = 0x7FFF | (512 << 16); // size=512
        let user_caps = [core_prio, handle_tbl];
        let result = caps.initialize_for_user(&user_caps, None);
        assert_eq!(result, 0);
        assert_eq!(caps.core_mask, 0b111);
        assert_ne!(caps.priority_mask, 0);
        assert_eq!(caps.handle_table_size, 512);
    }

    #[test]
    fn test_initialize_for_kip() {
        let mut caps = KCapabilities::new();
        let result = caps.initialize_for_kip(&[], None);
        assert_eq!(result, 0);
        // KIP: all cores, all user priorities
        assert_ne!(caps.core_mask, 0);
        assert_eq!(caps.priority_mask & 0xF, 0); // no kernel priorities
        assert_ne!(caps.priority_mask, 0);
    }
}
